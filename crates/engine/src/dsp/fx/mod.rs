//! Per-strip and per-bus effects on the real-time thread: noise suppression,
//! noise gate, equalizer and compressor on strips; equalizer and limiter on
//! buses; and the feeds a strip sends to subwoofers and surround speakers.
//!
//! Unlike the ramps in [`super::params`], effect state (filter memories, the
//! gate's envelope, the noise suppressor's frames) must carry on smoothly
//! across parameter snapshots, and some of it is too large to copy. So it
//! lives in objects the snapshot builder hands from one snapshot to the next
//! by `Arc`: every snapshot of the same strip points at the same state, and
//! only the real-time thread ever touches what is inside.
//!
//! Each effect has a module of its own with its state and its per-sample
//! work. This one ties them together: [`FxState`] holds one strip's or
//! bus's effects, [`StripFx::process`] runs a strip's chain, and
//! [`process_bus_eq`] and [`process_bus_limiter`] run a bus's.

mod compressor;
mod denoise;
mod ducking;
mod eq;
mod feeds;
mod gate;
mod limiter;
mod tap;

pub use denoise::{new_warm_up_state, warm_up_denoise, DenoiseBank};
pub use ducking::RtDuck;
pub use eq::EqParams;
pub use tap::Tap;

use super::params::{RtCell, MAX_QUANTUM};
use compressor::CompRt;
use ducking::DuckRt;
use eq::EqRt;
use feeds::{SubFilter, SurroundFeed, SURROUND_DELAY_LEN};
use gate::GateRt;
use limiter::LimiterRt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use weir_protocol::{linear_to_db, Compressor, CompressorMeter, Denoise, Gate, GateMeter, Limiter};

/// Most channels a strip's gate and compressor look at, and a bus's limiter
/// works on. Channels past this pass through unlimited.
const MAX_LINKED_CHANNELS: usize = 16;

/// Move `v` one `step` towards `target`, stopping there.
fn step_towards(v: f32, target: f32, step: f32) -> f32 {
    if v == target {
        return v;
    }
    let next = v + step;
    if (step > 0.0 && next >= target) || (step < 0.0 && next <= target) || step == 0.0 {
        target
    } else {
        next
    }
}

/// The per-sample coefficient of a one-pole smoother that gets 99% of the
/// way to a new value in `ms` milliseconds at `rate` Hz: 99% takes 4.6 time
/// constants. The effects' attack and release times all mean this.
fn reach_99(ms: f32, rate: f32) -> f32 {
    1.0 - (-4.6 / (ms * 0.001 * rate)).exp()
}

/// Flush values that have decayed into the denormal range to zero. Filters
/// ringing down in silence end up there, where arithmetic gets very slow on
/// x86.
fn flush_denormals<'a>(values: impl IntoIterator<Item = &'a mut f32>) {
    for v in values {
        if v.abs() < 1e-18 {
            *v = 0.0;
        }
    }
}

/// The first `n` samples of up to [`MAX_LINKED_CHANNELS`] of `bufs`, for the
/// detectors that look at every channel at once. Returns them and how many
/// are in use.
fn channel_views(bufs: &[Vec<f32>], n: usize) -> ([&[f32]; MAX_LINKED_CHANNELS], usize) {
    let mut views: [&[f32]; MAX_LINKED_CHANNELS] = [&[]; MAX_LINKED_CHANNELS];
    for (v, b) in views.iter_mut().zip(bufs) {
        *v = &b[..n];
    }
    (views, bufs.len().min(MAX_LINKED_CHANNELS))
}

/// Multiply the first `n` samples of every channel by `gains`.
fn apply_gains(bufs: &mut [Vec<f32>], n: usize, gains: &[f32]) {
    for buf in bufs {
        for (x, g) in buf[..n].iter_mut().zip(gains) {
            *x *= g;
        }
    }
}

/// Average `channels` into `out`. Missing channels are skipped.
fn mix_to_mono<'a>(out: &mut [f32], channels: impl Iterator<Item = &'a [f32]>) {
    out.fill(0.0);
    let mut count = 0;
    for ch in channels {
        for (o, x) in out.iter_mut().zip(ch) {
            *o += x;
        }
        count += 1;
    }
    if count > 1 {
        let inv = 1.0 / count as f32;
        out.iter_mut().for_each(|o| *o *= inv);
    }
}

/// Effect state for one strip or bus that lasts across snapshots.
///
/// The meters are atomics the real-time thread writes and the control thread
/// reads and resets. They hold `f32` bits: the levels are positive, and
/// positive floats order like their bits, so an integer `fetch_max` or
/// `fetch_min` is a float one.
pub struct FxState {
    /// How many channels the strip or bus has.
    pub channels: usize,
    inner: RtCell<FxInner>,
    /// The gate's current gain.
    gate_gain: AtomicU32,
    /// Peak level going into the gate since the meters last read it.
    gate_level: AtomicU32,
    /// Whether anyone is looking at this equalizer's spectrum. The taps are
    /// only fed while it is set.
    analyze: AtomicBool,
    /// What goes into the equalizer.
    pub tap_in: Tap,
    /// What comes out of the equalizer.
    pub tap_out: Tap,
    /// The limiter's lowest gain since the meters last read it.
    limiter_gain: AtomicU32,
    /// Peak level going into the compressor since the meters last read it.
    comp_level: AtomicU32,
    /// The most the compressor turned down since then, in dB, positive.
    comp_reduction: AtomicU32,
    /// Strips: the loudest sample after the fader in the last cycle, for
    /// strips that duck others when heard.
    out_peak: AtomicU32,
    /// Strips: the lowest ducking gain since the meters last read it.
    duck_gain: AtomicU32,
}

/// The part of [`FxState`] only the real-time thread touches.
struct FxInner {
    /// Per channel: the strip's audio after its effects. Empty for buses,
    /// which process their output buffers in place.
    bufs: Vec<Vec<f32>>,
    eq: EqRt,
    gate: GateRt,
    /// Buses only; strips have an empty one.
    limiter: LimiterRt,
    /// Strips only: the low-pass on what they send to subwoofers.
    sub: SubFilter,
    /// Strips only: the low-pass and delay on a passive surround feed.
    surround: SurroundFeed,
    /// Strips only.
    comp: CompRt,
    /// Strips only.
    duck: DuckRt,
}

impl FxState {
    /// State for a strip, with room for its processed audio.
    pub fn for_strip(channels: usize) -> Self {
        Self::new(
            channels,
            vec![vec![0.0; MAX_QUANTUM]; channels],
            0,
            SURROUND_DELAY_LEN,
        )
    }

    /// State for a bus: its equalizer and its limiter.
    pub fn for_bus(channels: usize) -> Self {
        Self::new(channels, Vec::new(), channels, 0)
    }

    fn new(
        channels: usize,
        bufs: Vec<Vec<f32>>,
        limiter_channels: usize,
        surround_delay: usize,
    ) -> Self {
        Self {
            channels,
            inner: RtCell::new(FxInner {
                bufs,
                eq: EqRt::new(channels),
                gate: GateRt::new(),
                limiter: LimiterRt::new(limiter_channels),
                sub: SubFilter::new(),
                surround: SurroundFeed::new(surround_delay),
                comp: CompRt::new(),
                duck: DuckRt::new(),
            }),
            gate_gain: AtomicU32::new(1.0f32.to_bits()),
            gate_level: AtomicU32::new(0),
            analyze: AtomicBool::new(false),
            tap_in: Tap::new(),
            tap_out: Tap::new(),
            limiter_gain: AtomicU32::new(1.0f32.to_bits()),
            comp_level: AtomicU32::new(0),
            comp_reduction: AtomicU32::new(0),
            out_peak: AtomicU32::new(0),
            duck_gain: AtomicU32::new(1.0f32.to_bits()),
        }
    }

    /// Low-pass a strip's subwoofer feed in place.
    ///
    /// # Safety
    /// Real-time thread only.
    pub unsafe fn low_pass_sub(&self, rate: u32, buf: &mut [f32]) {
        self.inner.get_mut().sub.run(rate, buf);
    }

    /// Low-pass and delay a strip's passive surround feed in place.
    ///
    /// # Safety
    /// Real-time thread only.
    pub unsafe fn surround_feed(&self, rate: u32, buf: &mut [f32]) {
        self.inner.get_mut().surround.run(rate, buf);
    }

    /// Start or stop feeding the taps. Starting clears them first.
    pub fn set_analyzing(&self, on: bool) {
        let was = self.analyze.load(Ordering::Acquire);
        if on && !was {
            self.tap_in.clear();
            self.tap_out.clear();
        }
        if on != was {
            self.analyze.store(on, Ordering::Release);
        }
    }

    fn analyzing(&self) -> bool {
        self.analyze.load(Ordering::Relaxed)
    }

    /// Record how loud the strip was after its fader this cycle.
    pub fn set_out_peak(&self, peak: f32) {
        self.out_peak.store(peak.to_bits(), Ordering::Relaxed);
    }

    /// Whether this strip counts as heard for ducking: louder than
    /// `threshold` (linear) after its fader in the last cycle, and with its
    /// gate, if `gate_on`, open.
    pub fn heard(&self, threshold: f32, gate_on: bool) -> bool {
        let loud = f32::from_bits(self.out_peak.load(Ordering::Relaxed)) > threshold;
        let open = !gate_on || f32::from_bits(self.gate_gain.load(Ordering::Relaxed)) > 0.5;
        loud && open
    }

    /// Fill `out` with `level` times the ducking gain, moving that gain as
    /// `duck` says given whether a trigger is `heard`. Returns false,
    /// leaving `out` alone, when there is nothing to duck: off, and all the
    /// way back up.
    ///
    /// # Safety
    /// Real-time thread only.
    pub unsafe fn duck(
        &self,
        duck: &RtDuck,
        heard: bool,
        rate: u32,
        level: &[f32],
        out: &mut [f32],
    ) -> bool {
        let st = &mut self.inner.get_mut().duck;
        if st.is_idle(duck) {
            return false;
        }
        st.run(duck, heard, rate, level, out);
        self.duck_gain
            .fetch_min(st.gain.to_bits(), Ordering::Relaxed);
        true
    }

    /// The lowest ducking gain since the last read, in dB.
    pub fn take_duck_db(&self) -> f32 {
        let g = f32::from_bits(self.duck_gain.swap(1.0f32.to_bits(), Ordering::Relaxed));
        linear_to_db(g).min(0.0)
    }

    /// How far the limiter turned things down since the last read, in dB.
    pub fn take_limiter_db(&self) -> f32 {
        let g = f32::from_bits(self.limiter_gain.swap(1.0f32.to_bits(), Ordering::Relaxed));
        linear_to_db(g).min(0.0)
    }

    /// Read the compressor's meter, resetting it.
    pub fn take_compressor_meter(&self) -> CompressorMeter {
        CompressorMeter {
            level_db: linear_to_db(f32::from_bits(self.comp_level.swap(0, Ordering::Relaxed))),
            // Subtracted from 0 rather than negated, so no reduction reads
            // 0, not -0.
            reduction_db: 0.0 - f32::from_bits(self.comp_reduction.swap(0, Ordering::Relaxed)),
        }
    }

    /// Read the gate's meter, resetting its peak level.
    pub fn take_gate_meter(&self) -> GateMeter {
        GateMeter {
            level_db: linear_to_db(f32::from_bits(self.gate_level.swap(0, Ordering::Relaxed))),
            reduction_db: linear_to_db(f32::from_bits(self.gate_gain.load(Ordering::Relaxed))),
        }
    }

    /// Feed `tap` the mono mix of `channels`, using `mono` as scratch space.
    fn feed_tap<'a>(&self, tap: &Tap, channels: impl Iterator<Item = &'a [f32]>, mono: &mut [f32]) {
        mix_to_mono(mono, channels);
        tap.write(mono);
    }
}

/// Everything the processor needs to run one strip's effects.
pub struct StripFx<'a> {
    /// The strip's state.
    pub state: &'a FxState,
    /// Its equalizer.
    pub eq: &'a EqParams,
    /// Its gate.
    pub gate: &'a Gate,
    /// Its noise suppression settings.
    pub denoise: &'a Denoise,
    /// Its noise suppressor, once suppression has been switched on.
    pub denoise_bank: Option<&'a DenoiseBank>,
    /// Its compressor.
    pub compressor: &'a Compressor,
}

/// Scratch space for effect processing, owned by the processor.
pub struct FxScratch {
    /// The equalizer's crossfade position per sample.
    weights: Vec<f32>,
    /// A copy of a channel, for the equalizer's crossfade.
    tmp: Vec<f32>,
    /// The gate's or compressor's gain per sample.
    gains: Vec<f32>,
    /// A mono mix, for the taps.
    mono: Vec<f32>,
}

impl FxScratch {
    /// Scratch space for the longest cycle, allocated here and never on the
    /// real-time thread.
    pub fn new() -> Self {
        Self {
            weights: vec![0.0; MAX_QUANTUM],
            tmp: vec![0.0; MAX_QUANTUM],
            gains: vec![0.0; MAX_QUANTUM],
            mono: vec![0.0; MAX_QUANTUM],
        }
    }
}

impl Default for FxScratch {
    fn default() -> Self {
        Self::new()
    }
}

impl StripFx<'_> {
    /// Run this strip's effects on `n` samples, reading each channel from
    /// `input(c)` (null is silence): noise suppression, gate, equalizer and
    /// compressor, in that order. Returns the processed channels, or `None`
    /// when every effect is off and settled, in which case the caller
    /// should use the input as it is.
    ///
    /// # Safety
    /// Real-time thread only. Input pointers must be valid for `n` samples.
    pub unsafe fn process(
        &self,
        n: usize,
        rate: u32,
        ramp: usize,
        input: &dyn Fn(usize) -> *const f32,
        scratch: &mut FxScratch,
    ) -> Option<&[Vec<f32>]> {
        let state = self.state;
        let fx = state.inner.get_mut();
        fx.eq.update(self.eq, rate);
        let denoise_wanted = self.denoise.enabled && rate == Denoise::SAMPLE_RATE;
        let denoise_busy = self.denoise_bank.is_some_and(|b| b.is_busy(denoise_wanted));
        let gate_busy = !fx.gate.is_idle(self.gate);
        let comp_busy = !fx.comp.is_idle(self.compressor);
        let analyzing = state.analyzing();
        let mono = &mut scratch.mono[..n];

        if !denoise_busy && !gate_busy && fx.eq.is_idle() && !comp_busy {
            state.gate_gain.store(1.0f32.to_bits(), Ordering::Relaxed);
            if analyzing {
                // Nothing changes the audio, so it goes in and out as is.
                let channels = (0..state.channels)
                    .map(input)
                    .filter(|p| !p.is_null())
                    .map(|p| std::slice::from_raw_parts(p, n));
                state.feed_tap(&state.tap_in, channels, mono);
                state.tap_out.write(mono);
            }
            return None;
        }

        let channels = state.channels;
        for (c, buf) in fx.bufs[..channels].iter_mut().enumerate() {
            let buf = &mut buf[..n];
            let inp = input(c);
            if inp.is_null() {
                buf.fill(0.0);
            } else {
                buf.copy_from_slice(std::slice::from_raw_parts(inp, n));
            }
        }
        let bufs = &mut fx.bufs[..channels];

        if let (Some(bank), true) = (self.denoise_bank, denoise_busy) {
            bank.run(bufs, n, denoise_wanted, self.denoise.amount, ramp);
        }

        if gate_busy {
            let gains = &mut scratch.gains[..n];
            let (views, count) = channel_views(bufs, n);
            let peak = fx.gate.gains(self.gate, rate, &views[..count], gains);
            state
                .gate_level
                .fetch_max(peak.to_bits(), Ordering::Relaxed);
            apply_gains(bufs, n, gains);
            state
                .gate_gain
                .store(fx.gate.gain.to_bits(), Ordering::Relaxed);
        } else {
            state.gate_gain.store(1.0f32.to_bits(), Ordering::Relaxed);
        }

        if analyzing {
            state.feed_tap(&state.tap_in, bufs.iter().map(|b| &b[..n]), mono);
        }
        if !fx.eq.is_idle() {
            let weights = &mut scratch.weights[..n];
            fx.eq.weights(weights, ramp);
            for (c, buf) in bufs.iter_mut().enumerate() {
                fx.eq.run(c, &mut buf[..n], weights, &mut scratch.tmp[..n]);
            }
            fx.eq.advance(n, ramp);
        }
        if analyzing {
            state.feed_tap(&state.tap_out, bufs.iter().map(|b| &b[..n]), mono);
        }

        if comp_busy {
            let gains = &mut scratch.gains[..n];
            let (views, count) = channel_views(bufs, n);
            let (peak, most) = fx
                .comp
                .gains(self.compressor, rate, ramp, &views[..count], gains);
            apply_gains(bufs, n, gains);
            state
                .comp_level
                .fetch_max(peak.to_bits(), Ordering::Relaxed);
            state
                .comp_reduction
                .fetch_max(most.to_bits(), Ordering::Relaxed);
        }
        Some(&fx.bufs[..channels])
    }
}

/// Run a bus's equalizer in place on its output channels.
///
/// # Safety
/// Real-time thread only. Output pointers must be valid for `n` samples.
pub unsafe fn process_bus_eq(
    state: &FxState,
    eq: &EqParams,
    n: usize,
    rate: u32,
    ramp: usize,
    output: &dyn Fn(usize) -> *mut f32,
    scratch: &mut FxScratch,
) {
    let fx = state.inner.get_mut();
    fx.eq.update(eq, rate);
    let analyzing = state.analyzing();
    let tap = |tap: &Tap, mono: &mut [f32]| {
        let channels = (0..state.channels)
            .map(output)
            .filter(|p| !p.is_null())
            .map(|p| std::slice::from_raw_parts(p as *const f32, n));
        state.feed_tap(tap, channels, mono);
    };
    if analyzing {
        tap(&state.tap_in, &mut scratch.mono[..n]);
    }
    if !fx.eq.is_idle() {
        let weights = &mut scratch.weights[..n];
        fx.eq.weights(weights, ramp);
        for c in 0..state.channels {
            let out = output(c);
            if out.is_null() {
                continue;
            }
            let buf = std::slice::from_raw_parts_mut(out, n);
            fx.eq.run(c, buf, weights, &mut scratch.tmp[..n]);
        }
        fx.eq.advance(n, ramp);
    }
    if analyzing {
        tap(&state.tap_out, &mut scratch.mono[..n]);
    }
}

/// Run a bus's limiter in place on its output channels, after its fader.
///
/// # Safety
/// Real-time thread only. Output pointers must be valid for `n` samples and
/// must not alias each other.
pub unsafe fn process_bus_limiter(
    state: &FxState,
    limiter: &Limiter,
    n: usize,
    rate: u32,
    ramp: usize,
    output: &dyn Fn(usize) -> *mut f32,
) {
    let fx = state.inner.get_mut();
    if fx.limiter.is_idle(limiter) {
        return;
    }
    let mut chans: [&mut [f32]; MAX_LINKED_CHANNELS] = Default::default();
    let mut count = 0;
    for c in 0..state.channels.min(MAX_LINKED_CHANNELS) {
        let out = output(c);
        if !out.is_null() {
            chans[count] = std::slice::from_raw_parts_mut(out, n);
            count += 1;
        }
    }
    if count == 0 {
        return;
    }
    let lowest = fx.limiter.run(limiter, rate, ramp, &mut chans[..count], n);
    state
        .limiter_gain
        .fetch_min(lowest.to_bits(), Ordering::Relaxed);
}
