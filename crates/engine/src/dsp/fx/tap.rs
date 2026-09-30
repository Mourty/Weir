//! Taps: the recent audio going into and out of an equalizer, for the
//! analyzer to read.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// Samples a tap keeps: enough for one analysis frame and some slack.
const TAP_LEN: usize = 16384;

/// The most recent [`TAP_LEN`] samples of a signal, mixed to mono. The
/// real-time thread writes, the analyzer reads.
///
/// Each sample is its own atomic, so reading while the real-time thread
/// writes is safe. At worst a read straddles a write and mixes two cycles'
/// worth of samples, which an analyzer drawn 30 times a second cannot show.
pub struct Tap {
    data: Box<[AtomicU32]>,
    /// Samples written since creation. The newest sample is at
    /// `(written - 1) % TAP_LEN`.
    written: AtomicUsize,
}

impl Tap {
    pub(crate) fn new() -> Self {
        Self {
            data: (0..TAP_LEN).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicUsize::new(0),
        }
    }

    /// Add `samples` after the ones written before.
    pub(crate) fn write(&self, samples: &[f32]) {
        let start = self.written.load(Ordering::Relaxed);
        for (k, v) in samples.iter().enumerate() {
            self.data[(start + k) % TAP_LEN].store(v.to_bits(), Ordering::Relaxed);
        }
        self.written
            .store(start.wrapping_add(samples.len()), Ordering::Release);
    }

    /// How many samples have been written so far. Unchanged between two
    /// reads means the signal stopped arriving.
    pub fn written(&self) -> usize {
        self.written.load(Ordering::Acquire)
    }

    /// Copy the newest `out.len()` samples, oldest first.
    pub fn read_latest(&self, out: &mut [f32]) {
        let end = self.written();
        let n = out.len().min(TAP_LEN);
        let start = end.wrapping_sub(n);
        for (k, o) in out.iter_mut().take(n).enumerate() {
            *o = f32::from_bits(self.data[start.wrapping_add(k) % TAP_LEN].load(Ordering::Relaxed));
        }
    }

    /// Forget old audio, so an analyzer starting up does not show it.
    pub(super) fn clear(&self) {
        for v in self.data.iter() {
            v.store(0, Ordering::Relaxed);
        }
    }
}
