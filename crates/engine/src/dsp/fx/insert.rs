//! External effects: a strip's or bus's sound out to another program and
//! back in.
//!
//! Where the insert sits in the chain, each channel is copied to its "to
//! effects" port, and what comes back through "from effects" takes its
//! place. What comes back left one PipeWire cycle earlier: it reaches the
//! engine through the return node's hand-off (see [`crate::dsp::handoff`]).
//! While nothing plays into "from effects" the sound as it went out carries
//! on, or silence, as the fallback says. Every change
//! between those crossfades over one ramp, so nothing clicks when a program
//! connects or goes away.

use crate::dsp::params::RtInsert;
use weir_protocol::InsertPoint;

/// How much of the sound as it went out ("dry") and of what came back
/// ("wet") carries on, as the real-time thread last left them.
pub(super) struct InsertRt {
    dry: f32,
    wet: f32,
}

impl InsertRt {
    /// Off: everything as it went out.
    pub(super) fn new() -> Self {
        Self { dry: 1.0, wet: 0.0 }
    }

    /// Whether the insert has nothing to do: off, and done fading out.
    pub(super) fn is_idle(&self, ins: &RtInsert) -> bool {
        !ins.enabled && self.dry == 1.0 && self.wet == 0.0
    }

    /// Fill `dry` and `wet` with this cycle's mix, moving each towards
    /// `ins`'s by at most `1 / ramp` a sample.
    fn weights(&mut self, ins: &RtInsert, ramp: usize, dry: &mut [f32], wet: &mut [f32]) {
        let step = 1.0 / ramp as f32;
        ramp_into(dry, &mut self.dry, ins.dry, step);
        ramp_into(wet, &mut self.wet, ins.wet, step);
    }
}

/// Fill `buf` with `value` moving towards `target` by `step` a sample, and
/// leave `value` where it got to.
fn ramp_into(buf: &mut [f32], value: &mut f32, target: f32, step: f32) {
    if *value == target {
        buf.fill(target);
        return;
    }
    let step = if target > *value { step } else { -step };
    for slot in buf {
        *value = super::step_towards(*value, target, step);
        *slot = *value;
    }
}

/// Send channel `c`, `buf`, out through `ins` and replace it with what
/// comes back, mixed as `dry` and `wet` say.
///
/// # Safety
/// Real-time thread only; `ins`'s buffers must be valid for `buf.len()`
/// samples or null.
unsafe fn exchange(ins: &RtInsert, c: usize, buf: &mut [f32], dry: &[f32], wet: &[f32]) {
    let n = buf.len();
    let send = ins
        .send_bufs
        .get(c)
        .map_or(std::ptr::null_mut(), |b| b.get());
    if !send.is_null() {
        std::slice::from_raw_parts_mut(send, n).copy_from_slice(buf);
    }
    let back = ins.return_bufs.get(c).map_or(std::ptr::null(), |b| b.get());
    if back.is_null() {
        buf.iter_mut().zip(dry).for_each(|(x, d)| *x *= d);
        return;
    }
    let back = std::slice::from_raw_parts(back, n);
    for k in 0..n {
        buf[k] = dry[k] * buf[k] + wet[k] * back[k];
    }
}

impl super::FxState {
    /// Whether this strip's or bus's external effects have anything to do
    /// this cycle.
    ///
    /// # Safety
    /// Real-time thread only, and not while [`super::StripFx::process`]'s
    /// result is in use.
    pub unsafe fn insert_busy(&self, ins: &RtInsert) -> bool {
        !self.inner.get_mut().insert.is_idle(ins)
    }
}

/// Run a strip's external effects on its processed channels, in place.
/// `dry` and `wet` are scratch space of `n` samples.
///
/// # Safety
/// As for [`exchange`].
pub(super) unsafe fn run_on_bufs(
    rt: &mut InsertRt,
    ins: &RtInsert,
    bufs: &mut [Vec<f32>],
    n: usize,
    ramp: usize,
    dry: &mut [f32],
    wet: &mut [f32],
) {
    rt.weights(ins, ramp, dry, wet);
    for (c, buf) in bufs.iter_mut().enumerate() {
        exchange(ins, c, &mut buf[..n], dry, wet);
    }
}

/// Run a bus's external effects in place on its output channels, if they
/// sit at `at` and have anything to do.
///
/// # Safety
/// Real-time thread only. Output pointers must be valid for `n` samples.
pub unsafe fn process_bus_insert(
    state: &super::FxState,
    ins: &RtInsert,
    at: InsertPoint,
    n: usize,
    ramp: usize,
    output: &dyn Fn(usize) -> *mut f32,
    scratch: &mut super::FxScratch,
) {
    let rt = &mut state.inner.get_mut().insert;
    if ins.position != at || rt.is_idle(ins) {
        return;
    }
    let (dry, wet) = (&mut scratch.weights[..n], &mut scratch.gains[..n]);
    rt.weights(ins, ramp, dry, wet);
    for c in 0..state.channels {
        let out = output(c);
        if !out.is_null() {
            exchange(ins, c, std::slice::from_raw_parts_mut(out, n), dry, wet);
        }
    }
}
