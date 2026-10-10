//! Sounds the mixer plays itself: the clicks and beeps hotkeys make.
//!
//! The daemon decodes every sound once, into a [`SoundBank`] that rides in
//! each snapshot. To play one it puts a [`PlayRequest`] in the
//! [`SoundQueue`], which the real-time thread empties at the start of each
//! cycle into a few [`Voices`]. They play on the engine's own stereo output,
//! `hotkey_sounds`, which the engine links to the device chosen for them,
//! past every bus, so nothing recording a bus hears them. Sounds keep their
//! own sample rate and are read at the engine's by interpolating between
//! samples, so a change of rate needs nothing rebuilt.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

/// How many sounds can play at once. More are passed over.
pub const MAX_VOICES: usize = 8;
/// How many requests can wait for the real-time thread.
const QUEUE_LEN: usize = 16;

/// One sound, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundData {
    /// Its sample rate.
    pub rate: u32,
    /// Its channels, one or two, all as long.
    pub channels: Vec<Vec<f32>>,
}

impl SoundData {
    /// How many frames it has.
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// How long it plays, in seconds.
    pub fn seconds(&self) -> f32 {
        self.frames() as f32 / self.rate.max(1) as f32
    }

    /// Sample `i` of channel `c`, the last channel standing in for any it
    /// lacks, or silence past the end.
    fn at(&self, c: usize, i: usize) -> f32 {
        self.channels
            .get(c.min(self.channels.len().saturating_sub(1)))
            .and_then(|ch| ch.get(i))
            .copied()
            .unwrap_or(0.0)
    }
}

/// Every sound the engine can play, by place in the list. Built off the
/// real-time thread and never changed; a new bank replaces it.
#[derive(Debug, Default)]
pub struct SoundBank {
    /// The sounds.
    pub sounds: Vec<SoundData>,
}

/// A sound to play, and how loud, as a linear gain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayRequest {
    /// Its place in the bank.
    pub sound: u32,
    /// Its gain, linear.
    pub gain: f32,
}

/// Requests on their way to the real-time thread: a ring that any thread
/// fills, one at a time under a lock, and the real-time thread empties
/// without one.
#[derive(Debug)]
pub struct SoundQueue {
    /// Per slot, the sound.
    sound: [AtomicU32; QUEUE_LEN],
    /// Per slot, the gain as `f32` bits.
    gain: [AtomicU32; QUEUE_LEN],
    /// Requests taken, in all. Only the real-time thread moves it.
    head: AtomicUsize,
    /// Requests put in, in all. Moved under `lock`.
    tail: AtomicUsize,
    lock: Mutex<()>,
}

impl Default for SoundQueue {
    fn default() -> Self {
        Self {
            sound: std::array::from_fn(|_| AtomicU32::new(0)),
            gain: std::array::from_fn(|_| AtomicU32::new(0)),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            lock: Mutex::new(()),
        }
    }
}

impl SoundQueue {
    /// Put `r` in for the real-time thread. Returns false when the queue is
    /// full, which only happens if it is not running.
    pub fn push(&self, r: PlayRequest) -> bool {
        let Ok(_guard) = self.lock.lock() else {
            return false;
        };
        let tail = self.tail.load(Ordering::Relaxed);
        if tail - self.head.load(Ordering::Acquire) >= QUEUE_LEN {
            return false;
        }
        let slot = tail % QUEUE_LEN;
        self.sound[slot].store(r.sound, Ordering::Relaxed);
        self.gain[slot].store(r.gain.to_bits(), Ordering::Relaxed);
        self.tail.store(tail + 1, Ordering::Release);
        true
    }

    /// Take the oldest request. Real-time thread only.
    pub fn pop(&self) -> Option<PlayRequest> {
        let head = self.head.load(Ordering::Relaxed);
        if head == self.tail.load(Ordering::Acquire) {
            return None;
        }
        let slot = head % QUEUE_LEN;
        let r = PlayRequest {
            sound: self.sound[slot].load(Ordering::Relaxed),
            gain: f32::from_bits(self.gain[slot].load(Ordering::Relaxed)),
        };
        self.head.store(head + 1, Ordering::Release);
        Some(r)
    }
}

/// A sound playing.
#[derive(Debug, Clone, Copy)]
struct Voice {
    request: PlayRequest,
    /// Where in the sound it has got to, in the sound's own frames.
    pos: f64,
}

/// The sounds playing now. Lives on the real-time thread, allocated once.
#[derive(Debug)]
pub struct Voices {
    voices: [Option<Voice>; MAX_VOICES],
    /// The bank they play from, to notice a new one. Only compared.
    bank: usize,
}

impl Default for Voices {
    fn default() -> Self {
        Self {
            voices: [None; MAX_VOICES],
            bank: 0,
        }
    }
}

impl Voices {
    /// Start the sounds asked for since the last cycle. A new bank stops
    /// whatever was playing from the old one, whose places mean other sounds
    /// now.
    pub fn take_requests(&mut self, bank: &SoundBank, queue: &SoundQueue) {
        let id = bank as *const SoundBank as usize;
        if id != self.bank {
            self.bank = id;
            self.voices = [None; MAX_VOICES];
        }
        while let Some(request) = queue.pop() {
            if request.sound as usize >= bank.sounds.len() {
                continue;
            }
            if let Some(free) = self.voices.iter_mut().find(|v| v.is_none()) {
                *free = Some(Voice { request, pos: 0.0 });
            }
        }
    }

    /// Whether anything is playing.
    pub fn playing(&self) -> bool {
        self.voices.iter().any(Option::is_some)
    }

    /// Add `n` samples of every sound playing into `left` and `right`, and
    /// move them on: a stereo sound's channels to each side, a mono one to
    /// both. A side may be null, when its port has no buffer this cycle; the
    /// sounds move on all the same.
    ///
    /// # Safety
    /// `left` and `right` must each be null or valid for `n` floats, and
    /// must not be the same buffer.
    pub unsafe fn mix_into(
        &mut self,
        bank: &SoundBank,
        n: usize,
        rate: u32,
        left: *mut f32,
        right: *mut f32,
    ) {
        for slot in &mut self.voices {
            let Some(mut v) = *slot else { continue };
            let Some(sound) = bank.sounds.get(v.request.sound as usize) else {
                *slot = None;
                continue;
            };
            let step = f64::from(sound.rate) / f64::from(rate.max(1));
            for (c, out) in [left, right].into_iter().enumerate() {
                if out.is_null() {
                    continue;
                }
                let out = std::slice::from_raw_parts_mut(out, n);
                let mut at = v.pos;
                for x in out.iter_mut() {
                    let i = at as usize;
                    let f = (at - i as f64) as f32;
                    let s = sound.at(c, i) * (1.0 - f) + sound.at(c, i + 1) * f;
                    *x += s * v.request.gain;
                    at += step;
                }
            }
            v.pos += step * n as f64;
            *slot = (v.pos < sound.frames() as f64).then_some(v);
        }
    }
}

/// The sample rate Weir's own sounds are made at.
const BUILTIN_RATE: u32 = 48_000;

/// Tones, each a frequency in Hz and a length in ms, one after another,
/// each faded in and out over a few milliseconds so it does not click.
fn tones(parts: &[(f32, f32)], amplitude: f32) -> Vec<f32> {
    let rate = BUILTIN_RATE as f32;
    let fade = (0.004 * rate) as usize;
    let mut out = Vec::new();
    for &(hz, ms) in parts {
        let len = (ms / 1000.0 * rate) as usize;
        for i in 0..len {
            let edge = i.min(len - 1 - i);
            let env = if edge < fade {
                0.5 - 0.5 * (std::f32::consts::PI * edge as f32 / fade as f32).cos()
            } else {
                1.0
            };
            let t = i as f32 / rate;
            out.push((std::f32::consts::TAU * hz * t).sin() * env * amplitude);
        }
    }
    out
}

/// A short knock: a tone that dies away within milliseconds.
fn knock(hz: f32, decay_ms: f32, amplitude: f32) -> Vec<f32> {
    let rate = BUILTIN_RATE as f32;
    let len = (decay_ms * 6.0 / 1000.0 * rate) as usize;
    let attack = (0.0005 * rate) as usize;
    (0..len)
        .map(|i| {
            let t = i as f32 / rate;
            let rise = (i as f32 / attack as f32).min(1.0);
            let env = rise * (-t * 1000.0 / decay_ms).exp();
            let s = (std::f32::consts::TAU * hz * t).sin()
                + 0.4 * (std::f32::consts::TAU * hz * 2.03 * t).sin();
            s * env * amplitude / 1.4
        })
        .collect()
}

/// One of Weir's own sounds, made fresh, by its name in
/// [`weir_protocol::BUILTIN_SOUNDS`].
pub fn builtin_sound(name: &str) -> Option<SoundData> {
    let samples = match name {
        "Click" => knock(1800.0, 2.5, 0.7),
        "Tick" => knock(1200.0, 1.5, 0.35),
        "Beep up" => tones(&[(660.0, 70.0), (880.0, 90.0)], 0.35),
        "Beep down" => tones(&[(880.0, 70.0), (660.0, 90.0)], 0.35),
        _ => return None,
    };
    Some(SoundData {
        rate: BUILTIN_RATE,
        channels: vec![samples],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(sound: u32) -> PlayRequest {
        PlayRequest { sound, gain: 0.5 }
    }

    #[test]
    fn the_queue_keeps_order_and_says_when_full() {
        let q = SoundQueue::default();
        for i in 0..QUEUE_LEN as u32 {
            assert!(q.push(request(i)));
        }
        assert!(!q.push(request(99)), "full");
        assert_eq!(q.pop(), Some(request(0)));
        assert!(q.push(request(42)), "room again");
        let rest: Vec<u32> = std::iter::from_fn(|| q.pop()).map(|r| r.sound).collect();
        assert_eq!(rest.len(), QUEUE_LEN);
        assert_eq!(rest.last(), Some(&42));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn weirs_own_sounds_are_made_and_stay_quiet_at_the_edges() {
        for name in weir_protocol::BUILTIN_SOUNDS {
            let s = builtin_sound(name).unwrap_or_else(|| panic!("{name}"));
            let ch = &s.channels[0];
            assert!(s.seconds() > 0.005 && s.seconds() < 0.5, "{name}");
            let peak = ch.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            assert!(peak > 0.1 && peak <= 1.0, "{name} peaks at {peak}");
            assert!(ch.last().unwrap().abs() < 0.01, "{name} ends in silence");
        }
        assert!(builtin_sound("Airhorn").is_none());
    }

    /// Play `sound` through `Voices` in cycles of `n` at `rate`, and return
    /// what the left and right got.
    fn play(sound: SoundData, n: usize, rate: u32) -> [Vec<f32>; 2] {
        let bank = SoundBank {
            sounds: vec![sound],
        };
        let queue = SoundQueue::default();
        let mut voices = Voices::default();
        queue.push(request(0));
        let mut out = [Vec::new(), Vec::new()];
        for _ in 0..4 {
            voices.take_requests(&bank, &queue);
            let mut l = vec![0.0f32; n];
            let mut r = vec![0.0f32; n];
            unsafe { voices.mix_into(&bank, n, rate, l.as_mut_ptr(), r.as_mut_ptr()) };
            out[0].extend(l);
            out[1].extend(r);
        }
        assert!(!voices.playing(), "finished");
        out
    }

    #[test]
    fn a_stereo_sound_plays_left_and_right_and_a_mono_one_on_both() {
        let stereo = SoundData {
            rate: 48_000,
            channels: vec![vec![1.0; 10], vec![-1.0; 10]],
        };
        let [l, r] = play(stereo, 16, 48_000);
        assert_eq!((l[0], r[0]), (0.5, -0.5));
        assert_eq!(l[12], 0.0, "over after 10 frames");
        let mono = SoundData {
            rate: 48_000,
            channels: vec![vec![1.0; 10]],
        };
        let [l, r] = play(mono, 16, 48_000);
        assert_eq!((l[0], r[0]), (0.5, 0.5));
    }

    #[test]
    fn a_sound_at_another_rate_plays_for_as_long() {
        // A tenth of a second at 24 kHz is 4800 frames at 48 kHz.
        let sound = SoundData {
            rate: 24_000,
            channels: vec![vec![1.0; 2400]],
        };
        let [l, _] = play(sound, 2048, 48_000);
        let heard = l.iter().filter(|&&x| x > 0.25).count();
        assert!((4790..=4802).contains(&heard), "{heard} frames");
    }

    #[test]
    fn a_new_bank_stops_what_plays_and_unknown_sounds_play_nothing() {
        let bank = SoundBank {
            sounds: vec![builtin_sound("Beep up").unwrap()],
        };
        let queue = SoundQueue::default();
        let mut voices = Voices::default();
        queue.push(request(5));
        voices.take_requests(&bank, &queue);
        assert!(!voices.playing(), "no such sound");
        queue.push(request(0));
        voices.take_requests(&bank, &queue);
        assert!(voices.playing());
        let other = SoundBank {
            sounds: vec![builtin_sound("Tick").unwrap()],
        };
        voices.take_requests(&other, &queue);
        assert!(!voices.playing(), "the old bank's sound stops");
    }
}
