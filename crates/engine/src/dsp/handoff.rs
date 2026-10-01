//! What comes back from external effects, handed from the node that
//! receives it to the engine.
//!
//! The sound goes out from the engine, through the effects program, and
//! back. If it came back into the engine itself, PipeWire would see a loop,
//! and it only handles loops it can see: through an effects program made of
//! two unlinked nodes, such as a filter chain, every node in the loop would
//! wait for the one before it, the engine included, forever. So it comes
//! back into a second node of Weir's, "Weir effects return", which runs
//! after the effects program and leaves each cycle here. The engine takes it
//! on its next cycle, so the trip out and back costs one cycle.
//!
//! One [`Handoff`] per strip or bus with external effects on. The return
//! node writes, the engine reads, possibly on different threads: every
//! sample is an atomic, and the count of frames written is published after
//! them.

use super::params::{RtCell, MAX_QUANTUM};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Frames each channel's ring holds: enough that what the engine reads is
/// never what the return node is writing at the same time.
const RING: usize = 4 * MAX_QUANTUM;

/// The return node's cycles, waiting for the engine.
pub struct Handoff {
    /// Per channel, the ring of samples, as `f32` bits.
    rings: Vec<Box<[AtomicU32]>>,
    /// Frames the return node has written, in all.
    written: AtomicU64,
    /// Frames the engine has read, in all. Only the engine touches it.
    read: AtomicU64,
    /// Per channel, the engine's copy of the cycle it read last.
    taken: RtCell<Vec<Vec<f32>>>,
}

impl Handoff {
    /// A hand-off for `channels` channels. Allocates everything, so make it
    /// off the real-time threads.
    pub fn new(channels: usize) -> Self {
        Self {
            rings: (0..channels)
                .map(|_| (0..RING).map(|_| AtomicU32::new(0)).collect())
                .collect(),
            written: AtomicU64::new(0),
            read: AtomicU64::new(0),
            taken: RtCell::new(vec![vec![0.0; MAX_QUANTUM]; channels]),
        }
    }

    /// How many channels it carries.
    pub fn channels(&self) -> usize {
        self.rings.len()
    }

    /// The return node's side: add `n` frames, each channel's from
    /// `input(c)`, null being silence.
    ///
    /// # Safety
    /// Only ever called from one thread at a time; the pointers must be
    /// valid for `n` samples.
    pub unsafe fn push(&self, n: usize, input: &dyn Fn(usize) -> *const f32) {
        let n = n.min(MAX_QUANTUM);
        let start = self.written.load(Ordering::Relaxed);
        for (c, ring) in self.rings.iter().enumerate() {
            let src = input(c);
            for k in 0..n {
                let v = if src.is_null() { 0.0 } else { *src.add(k) };
                ring[(start as usize + k) % RING].store(v.to_bits(), Ordering::Relaxed);
            }
        }
        self.written.store(start + n as u64, Ordering::Release);
    }

    /// The engine's side: the `n` frames to use this cycle, per channel.
    /// That is what the return node wrote since the last call, normally its
    /// previous cycle. Anything older than the latest `n` frames is passed
    /// over, so the delay never grows past a cycle, and what is missing is
    /// silence.
    ///
    /// # Safety
    /// Real-time thread of the engine only, and the result must be done
    /// with before the next call.
    pub unsafe fn pull(&self, n: usize) -> &[Vec<f32>] {
        let n = n.min(MAX_QUANTUM);
        let written = self.written.load(Ordering::Acquire);
        let mut read = self.read.load(Ordering::Relaxed);
        if written - read > n as u64 {
            read = written - n as u64;
        }
        let avail = (written - read) as usize;
        let taken = self.taken.get_mut();
        for (ring, out) in self.rings.iter().zip(taken.iter_mut()) {
            for (k, v) in out[..avail].iter_mut().enumerate() {
                *v = f32::from_bits(ring[(read as usize + k) % RING].load(Ordering::Relaxed));
            }
            out[avail..n].fill(0.0);
        }
        self.read.store(read + avail as u64, Ordering::Relaxed);
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(h: &Handoff, values: &[f32]) {
        let left = values.to_vec();
        let right: Vec<f32> = values.iter().map(|v| -v).collect();
        let input = |c: usize| {
            if c == 0 {
                left.as_ptr()
            } else {
                right.as_ptr()
            }
        };
        unsafe { h.push(values.len(), &input) };
    }

    fn pull(h: &Handoff, n: usize) -> (Vec<f32>, Vec<f32>) {
        let got = unsafe { h.pull(n) };
        (got[0][..n].to_vec(), got[1][..n].to_vec())
    }

    #[test]
    fn each_cycle_reaches_the_engine_on_its_next() {
        let h = Handoff::new(2);
        // Nothing has come back yet: silence.
        assert_eq!(pull(&h, 4).0, [0.0; 4]);
        push(&h, &[1.0, 2.0, 3.0, 4.0]);
        let (l, r) = pull(&h, 4);
        assert_eq!(l, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(r, [-1.0, -2.0, -3.0, -4.0]);
        // Read once only.
        assert_eq!(pull(&h, 4).0, [0.0; 4]);
    }

    #[test]
    fn the_delay_never_grows_past_a_cycle() {
        let h = Handoff::new(2);
        // Two cycles came back before the engine read: only the latest
        // counts.
        push(&h, &[1.0; 4]);
        push(&h, &[2.0; 4]);
        assert_eq!(pull(&h, 4).0, [2.0; 4]);
        // A short cycle is made up with silence.
        push(&h, &[3.0; 2]);
        assert_eq!(pull(&h, 4).0, [3.0, 3.0, 0.0, 0.0]);
    }

    #[test]
    fn the_ring_wraps_around() {
        let h = Handoff::new(2);
        let block: Vec<f32> = (0..MAX_QUANTUM).map(|k| k as f32).collect();
        for _ in 0..9 {
            push(&h, &block);
            assert_eq!(pull(&h, MAX_QUANTUM).0, block);
        }
    }
}
