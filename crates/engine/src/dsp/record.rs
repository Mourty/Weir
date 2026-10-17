//! Recording: a copy of what a strip or bus sounds like, handed from the
//! real-time thread to the recorder.
//!
//! One [`RecordTap`] per strip or bus being recorded. The real-time thread
//! writes every cycle, the recorder reads every so often, possibly on
//! another thread, so every sample is an atomic and each side publishes how
//! far it got after its samples. The real-time thread never waits: if the
//! recorder falls behind and the ring fills, a cycle's samples are dropped
//! and counted, and the recorder puts the same length of silence in their
//! place, so the recording keeps time.
//!
//! Every tap gets exactly one write each cycle, silence when its strip or
//! bus is gone or quiet, so all the tracks of a recording have the same
//! length and line up sample for sample.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use weir_protocol::TrackSource;

/// How long a tap holds, in seconds of audio at 48 kHz: how long the
/// recorder may stall, on a slow disk say, before anything is lost.
pub const RECORD_RING_SECONDS: usize = 8;

/// A ring of one strip's or bus's audio, interleaved.
pub struct RecordTap {
    /// What it records, by name: loading another setup puts it on the
    /// strip of that name there, not on whichever has the same id. Read
    /// when a snapshot is built, never on the real-time thread.
    source: Mutex<TrackSource>,
    /// Its channels. The strip or bus may change its layout while it is
    /// recorded: extra channels are left out and missing ones are silent.
    pub channels: usize,
    /// The samples, as `f32` bits: `frames` frames of `channels`.
    ring: Box<[AtomicU32]>,
    frames: usize,
    /// Frames written, in all. Published after their samples.
    written: AtomicU64,
    /// Frames read, in all. Published after they are copied out.
    read: AtomicU64,
    /// Frames dropped because the ring was full, not yet made up.
    lost: AtomicU64,
    /// Frames dropped, in all.
    lost_total: AtomicU64,
    /// The sample rate of the last cycle written.
    rate: AtomicU32,
    /// The real-time thread's count of the cycle it last wrote in.
    cycle: AtomicU64,
}

impl RecordTap {
    /// A tap for `source`, of `channels` channels. Allocates everything,
    /// so make it off the real-time thread.
    pub fn new(source: TrackSource, channels: usize) -> Self {
        let channels = channels.max(1);
        let frames = RECORD_RING_SECONDS * 48_000;
        Self {
            source: Mutex::new(source),
            channels,
            ring: (0..frames * channels).map(|_| AtomicU32::new(0)).collect(),
            frames,
            written: AtomicU64::new(0),
            read: AtomicU64::new(0),
            lost: AtomicU64::new(0),
            lost_total: AtomicU64::new(0),
            rate: AtomicU32::new(0),
            cycle: AtomicU64::new(u64::MAX),
        }
    }

    /// What it records.
    pub fn source(&self) -> TrackSource {
        self.source.lock().unwrap().clone()
    }

    /// Whether it records `source`.
    pub fn records(&self, source: &TrackSource) -> bool {
        self.source.lock().unwrap().same(source)
    }

    /// Keep recording its strip or bus under the name it was given.
    pub fn rename(&self, name: &str) {
        let mut source = self.source.lock().unwrap();
        *source = match &*source {
            TrackSource::Strip(_) => TrackSource::Strip(name.into()),
            TrackSource::Bus(_) => TrackSource::Bus(name.into()),
        };
    }

    /// The real-time thread's side: add `n` frames, each channel's from
    /// `channel(c)` times `gain` (when not empty), silence where it gives
    /// none. `cycle` is the real-time thread's count of cycles. Only the
    /// engine writes, and tests standing in for it.
    pub fn write<'a>(
        &self,
        cycle: u64,
        n: usize,
        rate: u32,
        channel: impl Fn(usize) -> Option<&'a [f32]>,
        gain: &[f32],
    ) {
        self.cycle.store(cycle, Ordering::Relaxed);
        self.rate.store(rate, Ordering::Relaxed);
        let start = self.written.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        if start - read + n as u64 > self.frames as u64 {
            self.lost.fetch_add(n as u64, Ordering::Relaxed);
            self.lost_total.fetch_add(n as u64, Ordering::Relaxed);
            return;
        }
        let ch = self.channels;
        let first = start as usize % self.frames;
        for c in 0..ch {
            let src = channel(c);
            for k in 0..n {
                let mut v = src.map_or(0.0, |s| s[k]);
                if let Some(g) = gain.get(k) {
                    v *= g;
                }
                let at = ((first + k) % self.frames) * ch + c;
                self.ring[at].store(v.to_bits(), Ordering::Relaxed);
            }
        }
        self.written.store(start + n as u64, Ordering::Release);
    }

    /// Whether it was written in cycle `cycle` yet.
    pub(crate) fn written_in(&self, cycle: u64) -> bool {
        self.cycle.load(Ordering::Relaxed) == cycle
    }

    /// The recorder's side: append to `out` every frame written since the
    /// last call, interleaved, then as much silence as was lost. Returns how
    /// many frames it appended.
    pub fn take(&self, out: &mut Vec<f32>) -> usize {
        let written = self.written.load(Ordering::Acquire);
        let read = self.read.load(Ordering::Relaxed);
        let ch = self.channels;
        let n = (written - read) as usize;
        out.reserve(n * ch);
        for k in 0..n {
            let at = ((read as usize + k) % self.frames) * ch;
            out.extend(
                self.ring[at..at + ch]
                    .iter()
                    .map(|v| f32::from_bits(v.load(Ordering::Relaxed))),
            );
        }
        self.read.store(written, Ordering::Release);
        let lost = self.lost.swap(0, Ordering::Relaxed) as usize;
        out.resize(out.len() + lost * ch, 0.0);
        n + lost
    }

    /// Frames lost so far because the recorder fell behind.
    pub fn lost_frames(&self) -> u64 {
        self.lost_total.load(Ordering::Relaxed)
    }

    /// The sample rate of what was written last, 0 before anything was.
    pub fn rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_come_out_in_order_and_a_full_ring_loses_samples_not_time() {
        let tap = RecordTap::new(TrackSource::Strip("Mic".into()), 2);
        let (l, r) = ([0.0, 10.0, 20.0], [1.0, 11.0, 21.0]);
        tap.write(
            0,
            3,
            48_000,
            |c| Some(if c == 0 { &l[..] } else { &r[..] }),
            &[],
        );
        // A mono source at half level: the second channel is silent.
        let mono = [200.0, 202.0];
        tap.write(1, 2, 48_000, |c| (c == 0).then_some(&mono[..]), &[0.5, 0.5]);
        let mut out = Vec::new();
        assert_eq!(tap.take(&mut out), 5);
        assert_eq!(
            out,
            [0.0, 1.0, 10.0, 11.0, 20.0, 21.0, 100.0, 0.0, 101.0, 0.0]
        );
        assert!(tap.written_in(1) && !tap.written_in(2));
        assert_eq!(tap.rate(), 48_000);

        // Fill the ring, then one more cycle: its frames are lost, and come
        // out as silence after what was there.
        let frames = tap.frames;
        let half = vec![0.5; frames];
        tap.write(2, frames, 48_000, |_| Some(&half[..]), &[]);
        tap.write(3, 4, 48_000, |_| Some(&[0.9; 4][..]), &[]);
        out.clear();
        assert_eq!(tap.take(&mut out), frames + 4);
        assert!(out[..frames * 2].iter().all(|v| *v == 0.5));
        assert!(out[frames * 2..].iter().all(|v| *v == 0.0));
        assert_eq!(tap.lost_frames(), 4);
        // And it wraps around the ring.
        let (l, r) = (
            [0.0, 2.0, 4.0, 6.0, 8.0, 10.0],
            [1.0, 3.0, 5.0, 7.0, 9.0, 11.0],
        );
        tap.write(
            4,
            6,
            44_100,
            |c| Some(if c == 0 { &l[..] } else { &r[..] }),
            &[],
        );
        out.clear();
        assert_eq!(tap.take(&mut out), 6);
        assert_eq!(out, (0..12).map(|v| v as f32).collect::<Vec<_>>());
        assert_eq!(tap.rate(), 44_100);
    }
}
