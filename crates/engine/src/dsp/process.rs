//! The per-cycle mixing routine.
//!
//! One cycle, for `n` samples:
//!
//! 1. Every bus output, and every output to external effects, is zeroed.
//! 2. Each strip ramps its level, runs its effects, and adds each of its
//!    channels into every bus through that bus's send matrix, with its
//!    subwoofer and surround feeds when it has them. Its meter reads here,
//!    after the fader.
//! 3. Each bus folds to mono if asked, runs its equalizer, applies its
//!    fader, then its limiter and its delay, and meters what comes out.
//! 4. Sounds hotkeys play go to the engine's own `hotkey_sounds` output.
//!
//! External effects go out and come back wherever they sit in a strip's or
//! bus's chain.

use super::fx::{self, FxScratch, StripFx};
use super::params::{RtBus, RtInsert, RtParams, RtSend, RtStrip, MAX_QUANTUM};
use super::sounds::Voices;
use nnnoiseless::DenoiseState;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use weir_protocol::InsertPoint;

/// Scratch buffers used inside one process call.
struct Scratch {
    /// A strip's level per sample.
    level: Vec<f32>,
    /// A bus's level per sample.
    bus_level: Vec<f32>,
    /// A bus's mono fold per sample.
    mono: Vec<f32>,
    /// The average of a bus's channels, for the mono fold.
    avg: Vec<f32>,
    /// A strip's subwoofer feed.
    sub: Vec<f32>,
    /// A strip's passive surround feed.
    surround: Vec<f32>,
    /// A strip's level with its ducking applied, for the mixes it is ducked
    /// in.
    ducked: Vec<f32>,
    /// All ones: the level of a strip whose fader has been applied already.
    unity: Vec<f32>,
}

/// Real-time thread state: remembers the last snapshot it ran so ramp state
/// can be carried over when a new snapshot arrives, and owns the scratch
/// space every cycle works in.
pub struct Processor {
    last: Option<Arc<RtParams>>,
    scratch: Scratch,
    fx_scratch: FxScratch,
    /// A suppressor run once on the real-time thread's first cycle and then
    /// kept, never freed there. See [`fx::warm_up_denoise`].
    warm_up: Box<DenoiseState<'static>>,
    warmed_up: bool,
    /// The sounds playing.
    voices: Voices,
}

impl Default for Processor {
    fn default() -> Self {
        Self::new()
    }
}

impl Processor {
    /// Allocates everything the real-time thread will need, so create it
    /// on another thread.
    pub fn new() -> Self {
        Self {
            last: None,
            scratch: Scratch {
                level: vec![0.0; MAX_QUANTUM],
                bus_level: vec![0.0; MAX_QUANTUM],
                mono: vec![0.0; MAX_QUANTUM],
                avg: vec![0.0; MAX_QUANTUM],
                sub: vec![0.0; MAX_QUANTUM],
                surround: vec![0.0; MAX_QUANTUM],
                ducked: vec![0.0; MAX_QUANTUM],
                unity: vec![1.0; MAX_QUANTUM],
            },
            fx_scratch: FxScratch::new(),
            warm_up: fx::new_warm_up_state(),
            warmed_up: false,
            voices: Voices::default(),
        }
    }

    /// Mix one cycle of `n_samples`.
    ///
    /// * `params` is the snapshot to use (typically freshly loaded from an
    ///   `ArcSwap`).
    /// * `input(strip_index, channel)` returns the strip channel's input
    ///   buffer of at least `n_samples` floats, or null when unavailable.
    /// * `output(bus_index, channel)` returns the bus channel's output buffer
    ///   of at least `n_samples` floats, or null when unavailable.
    /// * `ramp_samples` is the length of every parameter ramp.
    /// * `rate` is the sample rate, which the effects need.
    ///
    /// # Safety
    /// The pointers returned by `input`/`output` must be valid for
    /// `n_samples` floats for the duration of the call, and no output buffer
    /// may alias an input buffer or another output buffer.
    pub unsafe fn process(
        &mut self,
        params: &Arc<RtParams>,
        n_samples: usize,
        input: &dyn Fn(usize, usize) -> *const f32,
        output: &dyn Fn(usize, usize) -> *mut f32,
        ramp_samples: usize,
        rate: u32,
    ) {
        if !self.warmed_up {
            self.warmed_up = true;
            fx::warm_up_denoise(&mut self.warm_up);
        }
        let changed = match &self.last {
            Some(last) => !Arc::ptr_eq(last, params),
            None => true,
        };
        if changed {
            if let Some(old) = &self.last {
                migrate(params, old);
            }
            self.last = Some(Arc::clone(params));
        }
        let p: &RtParams = params;
        let n = n_samples.min(MAX_QUANTUM);
        if n == 0 {
            return;
        }
        let ramp = ramp_samples.max(1);

        self.voices.take_requests(&p.sounds, &p.sound_queue);

        for (bi, bus) in p.buses.iter().enumerate() {
            for c in 0..bus.ports.len() {
                let out = output(bi, c);
                if !out.is_null() {
                    std::ptr::write_bytes(out, 0, n);
                }
            }
        }
        // PipeWire's output buffers hold whatever was in them last, so an
        // external effects output nothing writes to this cycle must not
        // send that again.
        let inserts = p.strips.iter().map(|s| &s.insert);
        for ins in inserts.chain(p.buses.iter().map(|b| &b.insert)) {
            clear_sends(ins, n);
        }
        for (si, strip) in p.strips.iter().enumerate() {
            self.mix_strip(p, strip, n, &|c| input(si, c), output, ramp, rate);
        }
        for (bi, bus) in p.buses.iter().enumerate() {
            self.finish_bus(bus, n, &|c| output(bi, c), ramp, rate);
        }
        let [left, right] = [p.sound_bufs[0].get(), p.sound_bufs[1].get()];
        for out in [left, right] {
            if !out.is_null() {
                std::ptr::write_bytes(out, 0, n);
            }
        }
        if self.voices.playing() {
            self.voices.mix_into(&p.sounds, n, rate, left, right);
        }
    }

    /// Ramp `strip`'s level, run its effects, meter it, and add it into
    /// every bus's output.
    ///
    /// # Safety
    /// As for [`Processor::process`].
    #[allow(clippy::too_many_arguments)]
    unsafe fn mix_strip(
        &mut self,
        p: &RtParams,
        strip: &RtStrip,
        n: usize,
        input: &dyn Fn(usize) -> *const f32,
        output: &dyn Fn(usize, usize) -> *mut f32,
        ramp: usize,
        rate: u32,
    ) {
        let scratch = &mut self.scratch;
        let lvl_buf = &mut scratch.level[..n];
        let mut lvl = strip.level.get();
        fill_ramp(lvl_buf, &mut lvl, strip.level_target, ramp);
        strip.level.set(lvl);
        let steady_level = lvl == strip.level_target;
        // External effects always hear the strip, even when it is silent
        // in every mix.
        let insert_busy = strip.fx.insert_busy(&strip.insert);
        if steady_level
            && lvl == 0.0
            && !insert_busy
            && strip.sends.iter().all(|s| is_all_zero(s.current.get_mut()))
        {
            // Fully silent and settled: nothing to mix, meters read zero.
            strip.fx.set_out_peak(0.0);
            return;
        }

        // Ducking: the level again, turned down while a trigger strip is
        // heard, for the mixes it applies to. Triggers are judged on their
        // previous cycle when they come later in the list, which is a few
        // milliseconds and well inside the attack time.
        let duck = &strip.ducking;
        let heard = duck.enabled
            && duck.triggers.iter().any(|&t| {
                let trigger = &p.strips[t];
                trigger.fx.heard(duck.threshold, trigger.gate.enabled)
            });
        // External effects after the fader get the strip with its fader
        // applied, and what comes back goes into the mixes as it is.
        let faded = insert_busy && strip.insert.position == InsertPoint::AfterFader;
        let fader: &[f32] = lvl_buf;
        let lvl_buf: &[f32] = if faded { &scratch.unity[..n] } else { fader };
        let ducked_buf = &mut scratch.ducked[..n];
        let ducking = strip.fx.duck(duck, heard, rate, lvl_buf, ducked_buf);
        let ducked_buf: &[f32] = ducked_buf;
        let level_for = |bi: usize| {
            if ducking && strip.duck_mask[bi] {
                ducked_buf
            } else {
                lvl_buf
            }
        };
        // Add one row of every send, times the level, into every bus.
        let into_buses = |row: usize, buf: &[f32]| {
            for (bi, bus) in p.buses.iter().enumerate() {
                mix_row(
                    &strip.sends[bi],
                    row,
                    bus.ports.len(),
                    &|o| output(bi, o),
                    level_for(bi),
                    buf,
                    ramp,
                );
            }
        };

        // Noise suppression, gate, equalizer and compressor, when any is on.
        let effects = StripFx {
            state: &strip.fx,
            eq: &strip.eq,
            gate: &strip.gate,
            denoise: &strip.denoise,
            denoise_bank: strip.denoise_bank.as_deref(),
            compressor: &strip.compressor,
            insert: &strip.insert,
            insert_busy,
            level: fader,
        };
        let processed = effects.process(n, rate, ramp, input, &mut self.fx_scratch);
        debug_assert_eq!(processed.as_ref().is_some_and(|p| p.faded), faded);
        let channel = |c: usize| -> Option<&[f32]> {
            match &processed {
                Some(p) => Some(&p.channels[c][..n]),
                None => {
                    let inp = input(c);
                    (!inp.is_null()).then(|| std::slice::from_raw_parts(inp, n))
                }
            }
        };

        let mut out_peak = 0.0f32;
        for (c, meter) in strip.peaks.iter().enumerate() {
            let Some(inp) = channel(c) else {
                continue;
            };
            // The strip's meter reads after the fader.
            let peak = inp
                .iter()
                .zip(lvl_buf)
                .fold(0.0f32, |m, (x, l)| m.max((x * l).abs()));
            meter.fetch_max(peak.to_bits(), Ordering::Relaxed);
            out_peak = out_peak.max(peak);
            into_buses(c, inp);
        }
        strip.fx.set_out_peak(out_peak);

        // The subwoofer feed: the strip's channels mixed together and
        // low-passed, into its row of every send.
        if let Some(row) = strip.sub_row() {
            let sub = &mut scratch.sub[..n];
            sub.fill(0.0);
            let mut count = 0usize;
            for inp in (0..strip.ports.len()).filter_map(channel) {
                sub.iter_mut().zip(inp).for_each(|(s, x)| *s += x);
                count += 1;
            }
            if count > 1 {
                let inv = 1.0 / count as f32;
                sub.iter_mut().for_each(|v| *v *= inv);
            }
            strip.fx.low_pass_sub(rate, sub);
            into_buses(row, sub);
        }

        // Passive surround: what differs between left and right, kept
        // behind the fronts by a low-pass and a short delay, into its row of
        // every send.
        if let (Some((l, r)), Some(row)) = (strip.surround_feed, strip.surround_row()) {
            let feed = &mut scratch.surround[..n];
            feed.fill(0.0);
            if let Some(l) = channel(l) {
                feed.iter_mut().zip(l).for_each(|(f, v)| *f += v);
            }
            if let Some(r) = channel(r) {
                feed.iter_mut().zip(r).for_each(|(f, v)| *f -= v);
            }
            feed.iter_mut()
                .for_each(|f| *f *= std::f32::consts::FRAC_1_SQRT_2);
            strip.fx.surround_feed(rate, feed);
            into_buses(row, feed);
        }
    }

    /// Everything a bus does to its mix once every strip is in: the mono
    /// fold, the equalizer, the fader, the limiter, the delay, and the meter.
    ///
    /// # Safety
    /// As for [`Processor::process`].
    unsafe fn finish_bus(
        &mut self,
        bus: &RtBus,
        n: usize,
        output: &dyn Fn(usize) -> *mut f32,
        ramp: usize,
        rate: u32,
    ) {
        let scratch = &mut self.scratch;
        let lvl_buf = &mut scratch.bus_level[..n];
        let mut lvl = bus.level.get();
        fill_ramp(lvl_buf, &mut lvl, bus.level_target, ramp);
        bus.level.set(lvl);

        let mono_buf = &mut scratch.mono[..n];
        let mut mono = bus.mono.get();
        fill_ramp(mono_buf, &mut mono, bus.mono_target, ramp);
        bus.mono.set(mono);
        if mono != 0.0 || bus.mono_target != 0.0 {
            fold_to_mono(bus, n, output, mono_buf, &mut scratch.avg[..n]);
        }

        let insert = |at, fx_scratch: &mut FxScratch| {
            fx::process_bus_insert(&bus.fx, &bus.insert, at, n, ramp, output, fx_scratch);
        };
        insert(InsertPoint::BeforeEq, &mut self.fx_scratch);
        fx::process_bus_eq(
            &bus.fx,
            &bus.eq,
            n,
            rate,
            ramp,
            output,
            &mut self.fx_scratch,
        );

        insert(InsertPoint::BeforeFader, &mut self.fx_scratch);
        for o in 0..bus.ports.len() {
            let out = output(o);
            if out.is_null() {
                continue;
            }
            let out = std::slice::from_raw_parts_mut(out, n);
            for (x, l) in out.iter_mut().zip(lvl_buf.iter()) {
                *x *= l;
            }
        }

        // The limiter goes last, after the fader, so nothing can push the
        // output past its ceiling, unless external effects come after it.
        insert(InsertPoint::BeforeLimiter, &mut self.fx_scratch);
        fx::process_bus_limiter(&bus.fx, &bus.limiter, n, rate, ramp, output);
        insert(InsertPoint::AfterLimiter, &mut self.fx_scratch);

        // The delay is the very last thing, so the meter shows what plays.
        fx::process_bus_delay(&bus.fx, bus.delay_ms, n, rate, ramp, output);

        for (o, meter) in bus.peaks.iter().enumerate() {
            let out = output(o);
            if out.is_null() {
                continue;
            }
            let out = std::slice::from_raw_parts(out, n);
            let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            meter.fetch_max(peak.to_bits(), Ordering::Relaxed);
        }
    }
}

/// Blend each of a bus's channels (the subwoofer aside) towards the average
/// of them all, by `mono_buf` per sample: 0 leaves it, 1 is full mono.
/// `avg` is scratch space.
///
/// # Safety
/// As for [`Processor::process`].
unsafe fn fold_to_mono(
    bus: &RtBus,
    n: usize,
    output: &dyn Fn(usize) -> *mut f32,
    mono_buf: &[f32],
    avg: &mut [f32],
) {
    avg.fill(0.0);
    let mut count = 0usize;
    for (o, &folds) in bus.fold_mask.iter().enumerate() {
        let out = output(o);
        if !folds || out.is_null() {
            continue;
        }
        let out = std::slice::from_raw_parts(out, n);
        avg.iter_mut().zip(out).for_each(|(a, x)| *a += x);
        count += 1;
    }
    // One channel is mono already.
    if count < 2 {
        return;
    }
    let inv = 1.0 / count as f32;
    avg.iter_mut().for_each(|v| *v *= inv);
    for (o, &folds) in bus.fold_mask.iter().enumerate() {
        let out = output(o);
        if !folds || out.is_null() {
            continue;
        }
        let out = std::slice::from_raw_parts_mut(out, n);
        for k in 0..n {
            let m = mono_buf[k];
            out[k] = out[k] * (1.0 - m) + avg[k] * m;
        }
    }
}

/// Silence `ins`'s outputs to external effects for `n` samples.
///
/// # Safety
/// As for [`Processor::process`]: its send buffers must be valid for `n`
/// floats, or null.
unsafe fn clear_sends(ins: &RtInsert, n: usize) {
    for buf in &ins.send_bufs {
        let out = buf.get();
        if !out.is_null() {
            std::ptr::write_bytes(out, 0, n);
        }
    }
}

fn is_all_zero(v: &[f32]) -> bool {
    v.iter().all(|&x| x == 0.0)
}

/// Fill `buf` with a linear ramp from `*value` towards `target`, advancing
/// `*value` to where the ramp ended.
fn fill_ramp(buf: &mut [f32], value: &mut f32, target: f32, ramp: usize) {
    if *value == target {
        buf.iter_mut().for_each(|v| *v = target);
        return;
    }
    let step = (target - *value) / ramp as f32;
    let mut v = *value;
    for slot in buf.iter_mut() {
        if v != target {
            v += step;
            if (step > 0.0 && v >= target) || (step < 0.0 && v <= target) {
                v = target;
            }
        }
        *slot = v;
    }
    *value = v;
}

/// Add `inp`, times the strip's level, times one row of a send's matrix, into
/// the bus's outputs, ramping any coefficient that is on its way somewhere.
///
/// # Safety
/// As for [`Processor::process`]: `output` must return valid, unaliased
/// buffers of `inp.len()` floats, or null.
unsafe fn mix_row(
    send: &RtSend,
    row: usize,
    n_out: usize,
    output: &dyn Fn(usize) -> *mut f32,
    lvl_buf: &[f32],
    inp: &[f32],
    ramp: usize,
) {
    let n = inp.len();
    let cur = send.current.get_mut();
    for o in 0..n_out {
        let idx = row * n_out + o;
        let tgt = send.target[idx];
        let mut coef = cur[idx];
        if coef == 0.0 && tgt == 0.0 {
            continue;
        }
        let out = output(o);
        if out.is_null() {
            continue;
        }
        let out = std::slice::from_raw_parts_mut(out, n);
        if coef == tgt {
            for k in 0..n {
                out[k] += coef * lvl_buf[k] * inp[k];
            }
        } else {
            let step = (tgt - coef) / ramp as f32;
            for k in 0..n {
                if coef != tgt {
                    coef += step;
                    if (step > 0.0 && coef >= tgt) || (step < 0.0 && coef <= tgt) {
                        coef = tgt;
                    }
                }
                out[k] += coef * lvl_buf[k] * inp[k];
            }
            cur[idx] = coef;
        }
    }
}

/// Carry ramp state from the previously running snapshot into a new one so
/// that unchanged parameters do not restart from zero.
///
/// # Safety
/// Must be called from the real-time thread only, before `new` is processed
/// for the first time.
unsafe fn migrate(new: &RtParams, old: &RtParams) {
    for ns in &new.strips {
        let Some(os) = old.strips.iter().find(|s| s.id == ns.id) else {
            continue;
        };
        ns.level.set(os.level.get());
        for (bi, nsend) in ns.sends.iter().enumerate() {
            let bus_id = new.buses[bi].id;
            let Some(obi) = old.buses.iter().position(|b| b.id == bus_id) else {
                continue;
            };
            let osend = &os.sends[obi];
            let ncur = nsend.current.get_mut();
            let ocur = osend.current.get_mut();
            // The channel rows carry over, and so do the subwoofer and
            // surround feeds' rows when both snapshots have them. A feed that
            // was not there before fades in from zero.
            let n_out = new.buses[bi].positions.len();
            if ns.positions == os.positions && new.buses[bi].positions == old.buses[obi].positions {
                let channels = ns.positions.len() * n_out;
                ncur[..channels].copy_from_slice(&ocur[..channels]);
                for (to, from) in [
                    (ns.sub_row(), os.sub_row()),
                    (ns.surround_row(), os.surround_row()),
                ] {
                    if let (Some(to), Some(from)) = (to, from) {
                        ncur[to * n_out..(to + 1) * n_out]
                            .copy_from_slice(&ocur[from * n_out..(from + 1) * n_out]);
                    }
                }
            }
        }
    }
    for nb in &new.buses {
        let Some(ob) = old.buses.iter().find(|b| b.id == nb.id) else {
            continue;
        };
        nb.level.set(ob.level.get());
        nb.mono.set(ob.mono.get());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::params::{build_rt_params, NoPorts, PortPtr, PortResolver};
    use weir_protocol::*;

    /// Test harness: owns buffers and drives the processor.
    struct Rig {
        inputs: Vec<Vec<Vec<f32>>>,  // [strip][ch][k]
        outputs: Vec<Vec<Vec<f32>>>, // [bus][ch][k]
        /// What each strip's and bus's external effects are sent, and what
        /// they give back, like `inputs` and `outputs`.
        strip_sends: Vec<Vec<Vec<f32>>>,
        strip_returns: Vec<Vec<Vec<f32>>>,
        bus_sends: Vec<Vec<Vec<f32>>>,
        bus_returns: Vec<Vec<Vec<f32>>>,
        n: usize,
        proc_: Processor,
    }

    impl Rig {
        fn new(state: &MixerState, n: usize) -> Self {
            let strips = || {
                state
                    .strips
                    .iter()
                    .map(|s| vec![vec![0.0; n]; s.layout.channel_count()])
                    .collect()
            };
            let buses = || {
                state
                    .buses
                    .iter()
                    .map(|b| vec![vec![0.0; n]; b.layout.channel_count()])
                    .collect()
            };
            Self {
                inputs: strips(),
                outputs: buses(),
                strip_sends: strips(),
                strip_returns: strips(),
                bus_sends: buses(),
                bus_returns: buses(),
                n,
                proc_: Processor::new(),
            }
        }

        fn run(&mut self, params: &Arc<RtParams>, ramp: usize) {
            // As the process callback does with PipeWire's buffers.
            let wire = |ins: &RtInsert, sends: &[Vec<f32>], returns: &[Vec<f32>]| unsafe {
                for (cell, buf) in ins.send_bufs.iter().zip(sends) {
                    cell.set(buf.as_ptr() as *mut f32);
                }
                for (cell, buf) in ins.return_bufs.iter().zip(returns) {
                    cell.set(buf.as_ptr());
                }
            };
            for (i, s) in params.strips.iter().enumerate() {
                wire(&s.insert, &self.strip_sends[i], &self.strip_returns[i]);
            }
            for (i, b) in params.buses.iter().enumerate() {
                wire(&b.insert, &self.bus_sends[i], &self.bus_returns[i]);
            }
            let inputs = &self.inputs;
            let outputs = &self.outputs;
            let inp = |s: usize, c: usize| inputs[s][c].as_ptr();
            let out = |b: usize, c: usize| outputs[b][c].as_ptr() as *mut f32;
            unsafe { self.proc_.process(params, self.n, &inp, &out, ramp, 48_000) };
        }

        fn settle(&mut self, params: &Arc<RtParams>) {
            // ramp of 8 samples, so 4 blocks settle everything.
            for _ in 0..4 {
                self.run(params, 8);
            }
        }
    }

    fn state() -> MixerState {
        MixerState {
            strips: vec![
                Strip {
                    routes: [2].into_iter().collect(),
                    ..Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono)
                },
                Strip {
                    gain_db: -6.0,
                    routes: [1, 2].into_iter().collect(),
                    ..Strip::new(2, "Music", StripKind::Virtual, ChannelLayout::Stereo)
                },
            ],
            buses: vec![
                Bus::new(1, "Headset", BusKind::Hardware, ChannelLayout::Stereo),
                // No limiter, whose lookahead would shift every timing test.
                Bus {
                    limiter: Limiter::default(),
                    ..Bus::new(2, "Stream", BusKind::Virtual, ChannelLayout::Stereo)
                },
            ],
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn routing_and_gain() {
        let st = state();
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 1.0); // mic
        rig.inputs[1][0].iter_mut().for_each(|v| *v = 0.5); // music L
        rig.inputs[1][1].iter_mut().for_each(|v| *v = 0.25); // music R
        rig.settle(&params);
        let g = db_to_linear(-6.0);
        // Headset gets music only.
        assert!(close(rig.outputs[0][0][63], 0.5 * g));
        assert!(close(rig.outputs[0][1][63], 0.25 * g));
        // Stream gets mic (to both) + music.
        assert!(close(rig.outputs[1][0][63], 1.0 + 0.5 * g));
        assert!(close(rig.outputs[1][1][63], 1.0 + 0.25 * g));
        // Meters: post-fader peaks.
        let (sp, bp) = params.take_peaks();
        assert!(close(sp[0].1[0], 0.0)); // mic at 0 dB
        assert!(close(sp[1].1[0], linear_to_db(0.5 * g)));
        assert!(close(bp[0].1[0], linear_to_db(0.5 * g)));
    }

    #[test]
    fn fade_in_ramps_from_zero() {
        let st = state();
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 1.0);
        rig.run(&params, 32);
        let out = &rig.outputs[1][0];
        assert!(
            out[0] < 0.01,
            "first sample should be near zero, got {}",
            out[0]
        );
        assert!(out[16] > out[0] && out[16] < 1.0);
        assert!(close(out[63], 1.0));
    }

    #[test]
    fn mute_and_solo() {
        let mut st = state();
        let mut rig = Rig::new(&st, 32);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 1.0);
        rig.inputs[1][0].iter_mut().for_each(|v| *v = 1.0);
        rig.inputs[1][1].iter_mut().for_each(|v| *v = 1.0);

        st.strips[0].solo = true; // mic solo silences music everywhere
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        rig.settle(&params);
        assert!(close(rig.outputs[0][0][31], 0.0)); // headset: music silenced
        assert!(close(rig.outputs[1][0][31], 1.0)); // stream: mic only

        st.strips[0].solo = false;
        st.strips[0].mute = true;
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        rig.settle(&params);
        assert!(close(rig.outputs[1][0][31], db_to_linear(-6.0)));
    }

    #[test]
    fn snapshot_change_keeps_continuity() {
        let mut st = state();
        let p1 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 1.0);
        rig.settle(&p1);
        assert!(close(rig.outputs[1][0][63], 1.0));

        // Raise mic gain by 6 dB; the first sample after the swap must still
        // be about 1.0 (no restart from zero), and the ramp must reach the
        // new value.
        st.strips[0].gain_db = 6.0;
        let p2 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        rig.run(&p2, 32);
        let out = &rig.outputs[1][0];
        assert!(
            out[0] > 0.9 && out[0] < 1.3,
            "continuity broken: {}",
            out[0]
        );
        assert!(close(out[63], db_to_linear(6.0)));
    }

    #[test]
    fn route_toggle_ramps_and_mono_fold() {
        let mut st = state();
        let mut rig = Rig::new(&st, 64);
        rig.inputs[1][0].iter_mut().for_each(|v| *v = 1.0); // music L only
        let p1 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        rig.settle(&p1);
        let g = db_to_linear(-6.0);
        assert!(close(rig.outputs[0][0][63], g));
        assert!(close(rig.outputs[0][1][63], 0.0));

        // Mono fold on the headset bus: both channels become the average.
        st.buses[0].mono = true;
        let p2 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        rig.settle(&p2);
        assert!(close(rig.outputs[0][0][63], g / 2.0));
        assert!(close(rig.outputs[0][1][63], g / 2.0));

        // Route off: output ramps down to zero, no hard cut.
        st.buses[0].mono = false;
        st.strips[1].routes.remove(&1);
        let p3 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        rig.run(&p3, 32);
        let out = &rig.outputs[0][0];
        assert!(out[0] > 0.0 && out[0] < g);
        assert!(close(out[63], 0.0));
    }

    fn sine(buf: &mut [f32], start: usize, freq: f32, amp: f32) {
        for (k, v) in buf.iter_mut().enumerate() {
            let t = (start + k) as f32 / 48_000.0;
            *v = amp * (2.0 * std::f32::consts::PI * freq * t).sin();
        }
    }

    /// Deterministic white noise in -amp..amp.
    fn noise(buf: &mut [f32], seed: &mut u32, amp: f32) {
        for v in buf.iter_mut() {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *v = amp * ((*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0);
        }
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    /// Mono mic into a mono bus, nothing else, so outputs are easy to read.
    fn mic_only() -> MixerState {
        MixerState {
            strips: vec![Strip {
                routes: [1].into_iter().collect(),
                ..Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono)
            }],
            buses: vec![Bus::new(1, "Out", BusKind::Hardware, ChannelLayout::Mono)],
        }
    }

    #[test]
    fn strip_eq_boosts_what_the_curve_says() {
        let mut st = mic_only();
        st.strips[0].eq = Equalizer {
            enabled: true,
            bands: vec![EqBand::new(EqBandKind::Peak, 1000.0, 12.0, 1.0)],
        };
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        for block in 0..40 {
            sine(&mut rig.inputs[0][0], block * 480, 1000.0, 0.1);
            rig.run(&params, 480);
        }
        let expected = 0.1 * db_to_linear(12.0);
        let got = peak(&rig.outputs[0][0]);
        assert!((got - expected).abs() < 0.005, "{got} vs {expected}");
    }

    #[test]
    fn switching_the_eq_on_does_not_click() {
        let mut st = mic_only();
        let mut rig = Rig::new(&st, 480);
        let p1 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut pos = 0;
        let mut out = Vec::new();
        for _ in 0..20 {
            sine(&mut rig.inputs[0][0], pos, 200.0, 0.2);
            pos += 480;
            rig.run(&p1, 480);
        }
        st.strips[0].eq = Equalizer {
            enabled: true,
            bands: vec![EqBand::new(EqBandKind::LowShelf, 400.0, 18.0, 0.707)],
        };
        let p2 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, Some(&p1));
        for _ in 0..20 {
            sine(&mut rig.inputs[0][0], pos, 200.0, 0.2);
            pos += 480;
            rig.run(&p2, 480);
            out.extend_from_slice(&rig.outputs[0][0]);
        }
        // The steepest a 200 Hz sine can move between samples, at the level
        // it ends up at once the boost is in.
        let final_amp = peak(&out[out.len() - 480..]);
        let max_step = final_amp * 2.0 * std::f32::consts::PI * 200.0 / 48_000.0;
        let worst = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(final_amp > 1.0, "boost not applied: {final_amp}");
        assert!(worst < max_step * 1.1, "click: {worst} > {max_step}");
    }

    #[test]
    fn effect_state_carries_across_snapshots() {
        let mut st = mic_only();
        st.strips[0].eq.enabled = true;
        st.strips[0].denoise.enabled = true;
        let p1 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        st.strips[0].gain_db = -3.0;
        let p2 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, Some(&p1));
        assert!(Arc::ptr_eq(&p1.strips[0].fx, &p2.strips[0].fx));
        assert!(Arc::ptr_eq(&p1.buses[0].fx, &p2.buses[0].fx));
        let (a, b) = (&p1.strips[0].denoise_bank, &p2.strips[0].denoise_bank);
        assert!(Arc::ptr_eq(a.as_ref().unwrap(), b.as_ref().unwrap()));
        // Switching suppression off keeps the suppressor, so it can fade out.
        st.strips[0].denoise.enabled = false;
        let p3 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, Some(&p2));
        assert!(p3.strips[0].denoise_bank.is_some());
        // A different channel count needs new state.
        st.strips[0].layout = ChannelLayout::Stereo;
        let p4 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, Some(&p3));
        assert!(!Arc::ptr_eq(&p3.strips[0].fx, &p4.strips[0].fx));
        assert!(p4.strips[0].denoise_bank.is_none());
    }

    #[test]
    fn gate_closes_on_noise_and_opens_for_speech() {
        let mut st = mic_only();
        st.strips[0].gate = Gate {
            enabled: true,
            threshold_db: -40.0,
            range_db: -90.0,
            attack_ms: 1.0,
            hold_ms: 50.0,
            release_ms: 50.0,
        };
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        let mut seed = 1;
        // Room noise at -60 dB: after hold and release the gate is shut.
        for _ in 0..40 {
            noise(&mut rig.inputs[0][0], &mut seed, 0.001);
            rig.run(&params, 480);
        }
        assert!(peak(&rig.outputs[0][0]) < 1e-6);
        assert!(params.take_gate_meters()[0].1.reduction_db < -80.0);
        // Someone talks at -12 dB: it opens within a couple of milliseconds.
        let mut pos = 0;
        sine(&mut rig.inputs[0][0], pos, 300.0, 0.25);
        pos += 480;
        rig.run(&params, 480);
        assert!(rig.outputs[0][0][..96].iter().all(|v| v.abs() <= 0.25));
        let settled = &rig.outputs[0][0][240..];
        assert!((peak(settled) - 0.25).abs() < 0.01);
        let meter = params.take_gate_meters()[0].1;
        assert_eq!(meter.reduction_db, 0.0);
        assert!((meter.level_db - linear_to_db(0.25)).abs() < 0.1);
        // Hold keeps it open through a short pause.
        rig.inputs[0][0].fill(0.0);
        rig.run(&params, 480);
        sine(&mut rig.inputs[0][0], pos, 300.0, 0.25);
        rig.run(&params, 480);
        assert!((peak(&rig.outputs[0][0][..48]) - 0.25).abs() < 0.05);
    }

    #[test]
    fn gate_switched_off_lets_everything_through() {
        let mut st = mic_only();
        st.strips[0].gate.enabled = true;
        st.strips[0].gate.threshold_db = -20.0;
        let p1 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        let mut seed = 7;
        for _ in 0..40 {
            noise(&mut rig.inputs[0][0], &mut seed, 0.01);
            rig.run(&p1, 480);
        }
        assert!(peak(&rig.outputs[0][0]) < 1e-4);
        st.strips[0].gate.enabled = false;
        let p2 = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, Some(&p1));
        for _ in 0..4 {
            noise(&mut rig.inputs[0][0], &mut seed, 0.01);
            rig.run(&p2, 480);
        }
        let (i, o) = (&rig.inputs[0][0], &rig.outputs[0][0]);
        assert!(i.iter().zip(o).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn denoise_output_lines_up_with_its_input() {
        // With the amount at zero the suppressor outputs its delayed copy of
        // the original, which must be exactly the input 20 ms (960 samples)
        // late. If it were not, mixing the two would comb filter.
        let mut st = mic_only();
        st.strips[0].denoise = Denoise {
            enabled: true,
            amount: 0.0,
        };
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 256);
        let mut seed = 3;
        let mut input = Vec::new();
        let mut output = Vec::new();
        for _ in 0..40 {
            noise(&mut rig.inputs[0][0], &mut seed, 0.3);
            input.extend_from_slice(&rig.inputs[0][0]);
            rig.run(&params, 256);
            output.extend_from_slice(&rig.outputs[0][0]);
        }
        for k in 4000..output.len() {
            assert!(
                (output[k] - input[k - 960]).abs() < 1e-5,
                "sample {k}: {} vs {}",
                output[k],
                input[k - 960]
            );
        }
    }

    #[test]
    fn denoise_removes_steady_noise() {
        // Room and fan noise is loudest in the low end, which a low passed
        // noise imitates. (Flat white noise is a poor test: RNNoise leaves
        // much of it alone, as it does the hiss of an "s".)
        let mut st = mic_only();
        st.strips[0].denoise.enabled = true;
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        let mut seed = 11;
        let mut lp = 0.0f32;
        let (mut e_in, mut e_out) = (0.0f64, 0.0f64);
        for block in 0..300 {
            noise(&mut rig.inputs[0][0], &mut seed, 0.2);
            for v in rig.inputs[0][0].iter_mut() {
                lp = 0.95 * lp + 0.05 * *v;
                *v = lp;
            }
            rig.run(&params, 480);
            if block >= 100 {
                e_in += rig.inputs[0][0].iter().map(|v| (v * v) as f64).sum::<f64>();
                e_out += rig.outputs[0][0]
                    .iter()
                    .map(|v| (v * v) as f64)
                    .sum::<f64>();
            }
        }
        let reduction_db = 10.0 * (e_out / e_in).log10();
        assert!(reduction_db < -25.0, "only {reduction_db:.1} dB");
    }

    #[test]
    fn denoise_stays_out_of_the_way_at_other_rates() {
        let mut st = mic_only();
        st.strips[0].denoise.enabled = true;
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 441);
        let mut seed = 5;
        for _ in 0..20 {
            noise(&mut rig.inputs[0][0], &mut seed, 0.1);
            let inputs = &rig.inputs;
            let outputs = &rig.outputs;
            let inp = |s: usize, c: usize| inputs[s][c].as_ptr();
            let out = |b: usize, c: usize| outputs[b][c].as_ptr() as *mut f32;
            unsafe { rig.proc_.process(&params, 441, &inp, &out, 8, 44_100) };
        }
        let (i, o) = (&rig.inputs[0][0], &rig.outputs[0][0]);
        assert!(i.iter().zip(o).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn bus_eq_filters_the_mix() {
        let mut st = mic_only();
        st.buses[0].eq = Equalizer {
            enabled: true,
            bands: vec![EqBand::new(EqBandKind::HighPass, 2000.0, 0.0, 0.707)],
        };
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        for block in 0..40 {
            sine(&mut rig.inputs[0][0], block * 480, 100.0, 0.5);
            rig.run(&params, 480);
            if block == 30 {
                // Forget the filter's start-up transient.
                params.take_peaks();
            }
        }
        let expected = 0.5
            * db_to_linear(
                EqBand::new(EqBandKind::HighPass, 2000.0, 0.0, 0.707).response_db(100.0, 48_000.0),
            );
        let got = peak(&rig.outputs[0][0]);
        assert!(
            got < 0.01 && (got - expected).abs() < 0.001,
            "{got} vs {expected}"
        );
        // The bus meter reads after the equalizer.
        let (_, buses) = params.take_peaks();
        assert!(buses[0].1[0] < -30.0);
    }

    #[test]
    fn taps_see_the_equalizer_input_and_output_only_when_watched() {
        let mut st = mic_only();
        st.strips[0].eq = Equalizer {
            enabled: true,
            bands: vec![EqBand::new(EqBandKind::HighPass, 2000.0, 0.0, 0.707)],
        };
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        let fx = &params.strips[0].fx;
        sine(&mut rig.inputs[0][0], 0, 100.0, 0.5);
        rig.run(&params, 480);
        assert_eq!(fx.tap_in.written(), 0, "fed while nobody watched");

        fx.set_analyzing(true);
        for block in 1..20 {
            sine(&mut rig.inputs[0][0], block * 480, 100.0, 0.5);
            rig.run(&params, 480);
        }
        let mut seen = vec![0.0; 2048];
        fx.tap_in.read_latest(&mut seen);
        assert!((peak(&seen) - 0.5).abs() < 0.01, "input {}", peak(&seen));
        fx.tap_out.read_latest(&mut seen);
        assert!(peak(&seen) < 0.01, "output {}", peak(&seen));

        // The bus has no equalizer, so what goes in comes out, and both
        // taps match the output.
        let bus = &params.buses[0].fx;
        bus.set_analyzing(true);
        for block in 20..24 {
            sine(&mut rig.inputs[0][0], block * 480, 100.0, 0.5);
            rig.run(&params, 480);
        }
        let (mut a, mut b) = (vec![0.0; 480], vec![0.0; 480]);
        bus.tap_in.read_latest(&mut a);
        bus.tap_out.read_latest(&mut b);
        assert_eq!(a, b);
        assert_eq!(b, rig.outputs[0][0]);
    }

    #[test]
    fn send_levels_trim_one_mix_only() {
        let mut st = state();
        // Music is 10 dB quieter in the stream than in the headset.
        st.strips[1].sends.insert(2, -10.0);
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[1][0].iter_mut().for_each(|v| *v = 0.5);
        rig.settle(&params);
        let g = db_to_linear(-6.0);
        assert!(close(rig.outputs[0][0][63], 0.5 * g));
        assert!(close(rig.outputs[1][0][63], 0.5 * g * db_to_linear(-10.0)));
    }

    #[test]
    fn the_limiter_holds_the_ceiling_without_touching_quieter_audio() {
        let mut st = mic_only();
        st.buses[0].limiter = Limiter {
            enabled: true,
            ceiling_db: -6.0,
            ..Limiter::default()
        };
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        let ceiling = db_to_linear(-6.0);
        // A burst at +6 dB, with sharp edges, after a quiet stretch.
        let mut worst = 0.0f32;
        for block in 0..60 {
            let amp = if (20..30).contains(&block) { 2.0 } else { 0.1 };
            sine(&mut rig.inputs[0][0], block * 480, 440.0, amp);
            rig.run(&params, 480);
            if block > 2 {
                worst = worst.max(peak(&rig.outputs[0][0]));
            }
        }
        assert!(worst <= ceiling * 1.0001, "went over: {worst} > {ceiling}");
        let meters = params.take_limiter_meters();
        assert!(meters[0].1 < -10.0, "{:?}", meters);
        // Long after the burst, quiet audio passes at its own level (one
        // lookahead late).
        let quiet = peak(&rig.outputs[0][0]);
        assert!((quiet - 0.1).abs() < 0.002, "{quiet}");
    }

    #[test]
    fn a_bus_without_a_limiter_is_not_delayed() {
        let st = mic_only();
        assert!(!st.buses[0].limiter.enabled);
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 0.5);
        rig.settle(&params);
        assert!(rig.outputs[0][0].iter().all(|&v| close(v, 0.5)));
    }

    /// A mono mic into a mono bus that delays by `ms`.
    fn delayed(ms: f32) -> MixerState {
        let mut st = mic_only();
        st.buses[0].delay_ms = ms;
        st
    }

    /// A snapshot of [`delayed`], taking over the effect state of `prev`.
    /// A function rather than a closure, whose one inferred lifetime for
    /// `prev` would keep every earlier snapshot borrowed.
    fn delayed_params(ms: f32, prev: Option<&RtParams>) -> Arc<RtParams> {
        build_rt_params(&delayed(ms), SoloMode::Exclusive, &NoPorts, prev)
    }

    #[test]
    fn a_bus_delay_holds_the_output_back_by_that_many_milliseconds() {
        // 25 ms is 1200 samples at 48 kHz: two and a half blocks of 480.
        let st = delayed(25.0);
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        rig.settle(&params);
        let mut heard = Vec::new();
        for block in 0..6 {
            rig.inputs[0][0].fill(0.0);
            if block == 0 {
                rig.inputs[0][0][100] = 0.5;
            }
            rig.run(&params, 8);
            heard.extend_from_slice(&rig.outputs[0][0]);
        }
        let at = heard.iter().position(|v| v.abs() > 0.01).unwrap();
        assert_eq!(at, 100 + 1200);
        assert!(close(heard[at], 0.5), "{}", heard[at]);
        assert!(heard
            .iter()
            .enumerate()
            .all(|(i, v)| i == at || close(*v, 0.0)));
    }

    #[test]
    fn hotkey_sounds_play_on_their_own_output_and_no_bus() {
        use crate::dsp::sounds::{PlayRequest, SoundBank, SoundData};
        let st = mic_only();
        let mut params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        Arc::get_mut(&mut params).unwrap().sounds = Arc::new(SoundBank {
            sounds: vec![SoundData {
                rate: 48_000,
                channels: vec![vec![1.0; 100]],
            }],
        });
        let mut rig = Rig::new(&st, 480);
        rig.settle(&params);
        // PipeWire's buffers hold whatever was in them last.
        let mut left = vec![9.0f32; 480];
        let mut right = vec![9.0f32; 480];
        unsafe {
            params.sound_bufs[0].set(left.as_mut_ptr());
            params.sound_bufs[1].set(right.as_mut_ptr());
        }
        assert!(params.sound_queue.push(PlayRequest {
            sound: 0,
            gain: 0.25,
        }));
        rig.run(&params, 8);
        for side in [&left, &right] {
            assert!(
                side[..100].iter().all(|&v| close(v, 0.25)),
                "{:?}",
                &side[..4]
            );
            assert!(side[100..].iter().all(|&v| v == 0.0), "silence after it");
        }
        assert!(
            rig.outputs[0][0].iter().all(|&v| close(v, 0.0)),
            "not in the bus"
        );
        rig.run(&params, 8);
        assert!(left.iter().all(|&v| v == 0.0), "over, and cleared");
    }

    #[test]
    fn a_bus_delay_of_nothing_leaves_the_output_alone() {
        let st = delayed(0.0);
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 0.5);
        rig.settle(&params);
        assert!(rig.outputs[0][0].iter().all(|&v| close(v, 0.5)));
    }

    #[test]
    fn changing_a_bus_delay_does_not_click() {
        let mut rig = Rig::new(&delayed(0.0), 480);
        let mut last = 0.0f32;
        let mut worst = 0.0f32;
        let mut current = delayed_params(0.0, None);
        for block in 0..60 {
            // 0 -> 20 ms -> 7 ms -> 0, a change every 15 blocks.
            if block % 15 == 0 && block > 0 {
                let ms = [0.0, 20.0, 7.0, 0.0][block / 15];
                let next = delayed_params(ms, Some(&current));
                current = next;
            }
            sine(&mut rig.inputs[0][0], block * 480, 440.0, 0.5);
            // Ramp of 10 ms, as the engine uses.
            rig.run(&current, 480);
            if block > 2 {
                for v in &rig.outputs[0][0] {
                    worst = worst.max((v - last).abs());
                    last = *v;
                }
            } else {
                last = *rig.outputs[0][0].last().unwrap();
            }
        }
        // A 440 Hz sine of 0.5 moves 0.03 per sample at most; a crossfade
        // between two points of it a little more. A hard switch would jump
        // by up to 1.0.
        assert!(worst < 0.05, "clicked: {worst}");
    }

    #[test]
    fn a_bus_delay_switched_off_and_on_again_starts_from_silence() {
        let on = delayed_params(20.0, None);
        let mut rig = Rig::new(&delayed(20.0), 480);
        for block in 0..10 {
            sine(&mut rig.inputs[0][0], block * 480, 440.0, 0.5);
            rig.run(&on, 8);
        }
        // Off, long enough to fade out and go idle.
        let off = delayed_params(0.0, Some(&on));
        for block in 10..14 {
            sine(&mut rig.inputs[0][0], block * 480, 440.0, 0.5);
            rig.run(&off, 8);
        }
        // On again, with nothing playing: none of the old sound may come out.
        let again = delayed_params(20.0, Some(&off));
        rig.inputs[0][0].fill(0.0);
        for i in 0..4 {
            rig.run(&again, 8);
            let bad = rig.outputs[0][0].iter().position(|&v| !close(v, 0.0));
            assert!(bad.is_none(), "block {i}: sound at {bad:?}");
        }
    }

    #[test]
    fn missing_ports_are_silence() {
        let st = state();
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 16);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 1.0);
        let inp = |_: usize, _: usize| std::ptr::null::<f32>();
        let outputs = &rig.outputs;
        let out = |b: usize, c: usize| outputs[b][c].as_ptr() as *mut f32;
        unsafe { rig.proc_.process(&params, 16, &inp, &out, 4, 48_000) };
        assert!(rig.outputs[1][0].iter().all(|&v| v == 0.0));
    }

    /// A stereo strip on one 5.1 bus.
    fn stereo_on_surround(upmix: Upmix, subwoofer: bool) -> MixerState {
        MixerState {
            strips: vec![Strip {
                routes: [1].into_iter().collect(),
                upmix,
                subwoofer,
                ..Strip::new(1, "Music", StripKind::Virtual, ChannelLayout::Stereo)
            }],
            buses: vec![Bus::new(
                1,
                "Speakers",
                BusKind::Hardware,
                ChannelLayout::Surround51,
            )],
        }
    }

    #[test]
    fn all_channel_stereo_plays_a_stereo_strip_on_the_rears_and_center() {
        let st = stereo_on_surround(Upmix::AllChannelStereo, false);
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 0.4);
        rig.inputs[0][1].iter_mut().for_each(|v| *v = 0.2);
        rig.settle(&params);
        // FL FR FC LFE RL RR
        let got: Vec<f32> = rig.outputs[0].iter().map(|c| c[63]).collect();
        let want = [0.4, 0.2, 0.3, 0.0, 0.4, 0.2];
        for (g, w) in got.iter().zip(want) {
            assert!(close(*g, w), "{got:?}");
        }
    }

    #[test]
    fn the_subwoofer_gets_the_bass_and_not_the_rest() {
        let st = stereo_on_surround(Upmix::Off, true);
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        let lfe = 3;
        let level = |freq: f32, rig: &mut Rig| {
            let mut worst = 0.0f32;
            for block in 0..40 {
                sine(&mut rig.inputs[0][0], block * 480, freq, 0.5);
                sine(&mut rig.inputs[0][1], block * 480, freq, 0.5);
                rig.run(&params, 480);
                if block >= 30 {
                    worst = worst.max(peak(&rig.outputs[0][lfe]));
                }
            }
            worst
        };
        let bass = level(40.0, &mut rig);
        let voice = level(1000.0, &mut rig);
        assert!(bass > 0.45 && bass < 0.52, "40 Hz on the subwoofer: {bass}");
        assert!(voice < 0.5 * 0.001, "1 kHz on the subwoofer: {voice}");
        // The front speakers still get everything.
        assert!((peak(&rig.outputs[0][0]) - 0.5).abs() < 0.01);
    }

    /// Run `blocks` blocks of 480 samples of a sine on the strip's two
    /// channels, the right one scaled by `right`, and return the peak of
    /// each bus channel over the last ten.
    fn sine_through(st: &MixerState, freq: f32, right: f32, blocks: usize) -> Vec<f32> {
        let params = build_rt_params(st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(st, 480);
        let mut peaks = vec![0.0f32; rig.outputs[0].len()];
        for block in 0..blocks {
            sine(&mut rig.inputs[0][0], block * 480, freq, 0.5);
            sine(&mut rig.inputs[0][1], block * 480, freq, 0.5 * right);
            rig.run(&params, 480);
            if block + 10 >= blocks {
                for (p, ch) in peaks.iter_mut().zip(&rig.outputs[0]) {
                    *p = p.max(peak(ch));
                }
            }
        }
        peaks
    }

    #[test]
    fn passive_surround_puts_the_difference_on_the_rears() {
        let st = stereo_on_surround(Upmix::PassiveSurround, false);
        // FL FR FC LFE RL RR. Left and right in opposite phase: all
        // difference, so the rears get 0.707 (L - R) and the center nothing.
        let p = sine_through(&st, 1000.0, -1.0, 30);
        assert!(
            (p[4] - 0.707).abs() < 0.03 && (p[5] - 0.707).abs() < 0.03,
            "{p:?}"
        );
        assert!(p[2] < 1e-3, "{p:?}");
        // In phase: nothing differs, so the rears stay silent and the
        // center gets what the two share.
        let p = sine_through(&st, 1000.0, 1.0, 30);
        assert!(p[4] < 1e-3 && p[5] < 1e-3, "{p:?}");
        assert!((p[2] - 0.5).abs() < 0.01, "{p:?}");
        // Above the cutoff the feed fades.
        let p = sine_through(&st, 15_000.0, -1.0, 30);
        assert!(p[4] < 0.707 * 0.4, "{p:?}");
    }

    #[test]
    fn passive_surround_reaches_the_rears_later_and_inverted_on_the_right() {
        let st = stereo_on_surround(Upmix::PassiveSurround, false);
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        rig.settle(&params);
        // One click on the left, then silence.
        let mut rl = Vec::new();
        let mut rr = Vec::new();
        for block in 0..4 {
            rig.inputs[0][0].fill(0.0);
            if block == 0 {
                rig.inputs[0][0][0] = 1.0;
            }
            rig.run(&params, 8);
            rl.extend_from_slice(&rig.outputs[0][4]);
            rr.extend_from_slice(&rig.outputs[0][5]);
        }
        let loudest = |v: &[f32]| {
            (0..v.len())
                .max_by(|&a, &b| v[a].abs().total_cmp(&v[b].abs()))
                .unwrap()
        };
        let at = loudest(&rl);
        // 12 ms at 48 kHz, plus a sample or two through the low-pass.
        assert!((576..=580).contains(&at), "arrived at sample {at}");
        assert!(rl[at] > 0.0 && rr[at] < 0.0, "{} {}", rl[at], rr[at]);
        assert!((rl[at] + rr[at]).abs() < 1e-6);
    }

    #[test]
    fn a_bus_playing_only_the_center_gets_only_the_center() {
        let st = MixerState {
            strips: vec![Strip {
                routes: [1].into_iter().collect(),
                ..Strip::new(1, "Film", StripKind::Virtual, ChannelLayout::Surround51)
            }],
            buses: vec![Bus {
                downmix: Downmix {
                    method: DownmixMethod::CenterOnly,
                    ..Downmix::default()
                },
                ..Bus::new(1, "Center amp", BusKind::Hardware, ChannelLayout::Stereo)
            }],
        };
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 64);
        for (c, ch) in rig.inputs[0].iter_mut().enumerate() {
            ch.fill(0.1 * (c + 1) as f32);
        }
        rig.settle(&params);
        // FC is the third channel, at 0.3, on both speakers.
        assert!(close(rig.outputs[0][0][63], 0.3) && close(rig.outputs[0][1][63], 0.3));
    }

    #[test]
    fn the_subwoofer_feed_follows_the_route_and_send_level() {
        let mut st = stereo_on_surround(Upmix::Off, true);
        st.strips[0].sends.insert(1, -20.0);
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        for block in 0..40 {
            sine(&mut rig.inputs[0][0], block * 480, 40.0, 0.5);
            sine(&mut rig.inputs[0][1], block * 480, 40.0, 0.5);
            rig.run(&params, 480);
        }
        let got = peak(&rig.outputs[0][3]);
        assert!((got - 0.05).abs() < 0.005, "{got}");

        st.strips[0].routes.clear();
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        for _ in 0..4 {
            rig.run(&params, 480);
        }
        assert!(peak(&rig.outputs[0][3]) < 1e-6);
    }

    /// Play a steady 1 kHz sine at `amp` into `mic_only` with `comp` for
    /// half a second, and return the output's peak over the last block.
    fn compressed(comp: Compressor, amp: f32) -> (f32, Arc<RtParams>) {
        let mut st = mic_only();
        st.strips[0].compressor = comp;
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        for block in 0..50 {
            sine(&mut rig.inputs[0][0], block * 480, 1000.0, amp);
            rig.run(&params, 480);
        }
        (peak(&rig.outputs[0][0]), params)
    }

    #[test]
    fn the_compressor_follows_its_curve() {
        let comp = Compressor {
            enabled: true,
            threshold_db: -30.0,
            ratio: 4.0,
            auto_makeup: false,
            ..Compressor::default()
        };
        // A sine peaking at -6 dB is 24 dB over: it comes out 18 dB down.
        let (got, params) = compressed(comp, db_to_linear(-6.0));
        let want = db_to_linear(-6.0 + comp.reduction_db(-6.0));
        assert!(
            (linear_to_db(got) - linear_to_db(want)).abs() < 0.5,
            "{got} vs {want}"
        );
        let meters = params.take_compressor_meters();
        assert!((meters[0].1.reduction_db + 18.0).abs() < 0.5, "{meters:?}");
        assert!((meters[0].1.level_db + 6.0).abs() < 0.2, "{meters:?}");
        // Well under the threshold nothing happens.
        let (got, _) = compressed(comp, db_to_linear(-50.0));
        assert!((linear_to_db(got) + 50.0).abs() < 0.05, "{got}");
    }

    #[test]
    fn the_compressor_lift_raises_everything() {
        let comp = Compressor {
            enabled: true,
            threshold_db: -30.0,
            ratio: 4.0,
            makeup_db: 6.0,
            auto_makeup: false,
            ..Compressor::default()
        };
        let (got, _) = compressed(comp, db_to_linear(-50.0));
        assert!((linear_to_db(got) + 44.0).abs() < 0.05, "{got}");
    }

    #[test]
    fn switching_the_compressor_off_fades_rather_than_jumps() {
        let mut st = mic_only();
        st.strips[0].compressor = Compressor {
            enabled: true,
            threshold_db: -40.0,
            ratio: 10.0,
            auto_makeup: false,
            ..Compressor::default()
        };
        let on = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        st.strips[0].compressor.enabled = false;
        let off = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 0.5);
        for _ in 0..50 {
            rig.run(&on, 480);
        }
        let before = rig.outputs[0][0][479];
        rig.run(&off, 480);
        let out = &rig.outputs[0][0];
        // The ramp takes the whole block, so no step between samples is
        // much bigger than an even share of the change.
        let even = (out[479] - before).abs() / 480.0;
        let worst = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < even * 3.0 + 1e-6, "{worst} vs {even}");
        assert!((out[479] - 0.5).abs() < 1e-3, "{}", out[479]);
    }

    /// `state()` with Music ducked 20 dB in the Stream bus while Mic is
    /// heard, with quick timing.
    fn ducking_state() -> MixerState {
        let mut st = state();
        st.strips[1].ducking = Ducking {
            enabled: true,
            triggers: [1].into_iter().collect(),
            amount_db: 20.0,
            buses: [2].into_iter().collect(),
            threshold_db: -40.0,
            attack_ms: 5.0,
            hold_ms: 50.0,
            release_ms: 50.0,
        };
        st
    }

    #[test]
    fn ducking_turns_a_strip_down_in_its_mixes_while_the_trigger_is_heard() {
        let st = ducking_state();
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        rig.inputs[1][0].iter_mut().for_each(|v| *v = 0.5);
        rig.inputs[1][1].iter_mut().for_each(|v| *v = 0.5);
        let music = 0.5 * db_to_linear(-6.0);
        // Mic quiet: Music at its own level everywhere.
        for _ in 0..10 {
            rig.run(&params, 8);
        }
        assert!(close(rig.outputs[0][0][479], music));
        assert!(close(rig.outputs[1][0][479], music));
        // Mic talking: Music 20 dB down in the stream, untouched in the
        // headset. The mic itself only goes to the stream.
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 0.1);
        for _ in 0..10 {
            rig.run(&params, 8);
        }
        assert!(close(rig.outputs[0][0][479], music));
        let stream = rig.outputs[1][0][479] - 0.1;
        assert!(
            (stream - music * db_to_linear(-20.0)).abs() < 1e-3,
            "{stream}"
        );
        let meters = params.take_duck_meters();
        assert!((meters[0].1 + 20.0).abs() < 0.1, "{meters:?}");
        // Mic quiet again: still down through the hold, then back up.
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 0.0);
        rig.run(&params, 8);
        assert!(rig.outputs[1][0][479] < music * 0.2);
        for _ in 0..20 {
            rig.run(&params, 8);
        }
        assert!(close(rig.outputs[1][0][479], music));
    }

    #[test]
    fn a_trigger_with_its_gate_closed_does_not_duck() {
        let mut st = ducking_state();
        // Loud enough to pass the ducking threshold, but under the gate's,
        // which only turns it down 20 dB.
        st.strips[0].gate = Gate {
            enabled: true,
            threshold_db: -6.0,
            range_db: -20.0,
            ..Gate::default()
        };
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 480);
        rig.inputs[1][0].iter_mut().for_each(|v| *v = 0.5);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 0.1);
        for _ in 0..20 {
            rig.run(&params, 8);
        }
        let music = 0.5 * db_to_linear(-6.0);
        let mic = 0.1 * db_to_linear(-20.0);
        assert!(
            (rig.outputs[1][0][479] - (music + mic)).abs() < 1e-3,
            "{}",
            rig.outputs[1][0][479]
        );
    }

    #[test]
    fn cue_solo_only_changes_the_cue_mix() {
        let mut st = state();
        // Mic soloed, cueing on the Headset: the headset loses Music, and
        // the stream carries on with both.
        st.strips[0].solo = true;
        let params = build_rt_params(&st, SoloMode::Cue(1), &NoPorts, None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 0.1);
        rig.inputs[1][0].iter_mut().for_each(|v| *v = 0.5);
        rig.settle(&params);
        let music = 0.5 * db_to_linear(-6.0);
        assert!(close(rig.outputs[0][0][63], 0.0));
        assert!(close(rig.outputs[1][0][63], 0.1 + music));

        // The same solo in the usual mode silences Music everywhere.
        let params = build_rt_params(&st, SoloMode::Exclusive, &NoPorts, None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].iter_mut().for_each(|v| *v = 0.1);
        rig.inputs[1][0].iter_mut().for_each(|v| *v = 0.5);
        rig.settle(&params);
        assert!(close(rig.outputs[1][0][63], 0.1));
    }

    /// Says whether external effects are connected, everywhere. It has no
    /// ports: the rig wires the buffers itself.
    struct Effects(bool);

    impl PortResolver for Effects {
        fn strip_port(&self, _: StripId, _: usize) -> PortPtr {
            std::ptr::null_mut()
        }
        fn bus_port(&self, _: BusId, _: usize) -> PortPtr {
            std::ptr::null_mut()
        }
        fn insert_connected(&self, _: StripOrBus) -> bool {
            self.0
        }
    }

    /// `mic_only`, its fader at -6 dB, with external effects at `position`.
    fn mic_with_effects(position: InsertPoint, fallback: InsertFallback) -> MixerState {
        let mut st = mic_only();
        st.strips[0].gain_db = -6.0;
        st.strips[0].insert = Insert {
            enabled: true,
            position,
            fallback,
        };
        st
    }

    #[test]
    fn external_effects_before_the_fader_replace_the_sound() {
        let st = mic_with_effects(InsertPoint::BeforeFader, InsertFallback::PassThrough);
        let params = build_rt_params(&st, SoloMode::Exclusive, &Effects(true), None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].fill(1.0);
        rig.strip_returns[0][0].fill(0.5);
        rig.settle(&params);
        let g = db_to_linear(-6.0);
        // The effects hear the strip before its fader, and what they give
        // back goes on through it into the mix and the meter.
        assert!(close(rig.strip_sends[0][0][63], 1.0));
        assert!(close(rig.outputs[0][0][63], 0.5 * g));
        let (strips, _) = params.take_peaks();
        assert!(close(strips[0].1[0], linear_to_db(0.5 * g)));
    }

    #[test]
    fn external_effects_after_the_fader_hear_it() {
        let st = mic_with_effects(InsertPoint::AfterFader, InsertFallback::PassThrough);
        let params = build_rt_params(&st, SoloMode::Exclusive, &Effects(true), None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].fill(1.0);
        rig.strip_returns[0][0].fill(0.5);
        rig.settle(&params);
        assert!(close(rig.strip_sends[0][0][63], db_to_linear(-6.0)));
        assert!(close(rig.outputs[0][0][63], 0.5));
        let (strips, _) = params.take_peaks();
        assert!(close(strips[0].1[0], linear_to_db(0.5)));
    }

    #[test]
    fn unconnected_external_effects_fall_back() {
        let g = db_to_linear(-6.0);
        for (fallback, expected) in [
            (InsertFallback::PassThrough, g),
            (InsertFallback::Silence, 0.0),
        ] {
            let st = mic_with_effects(InsertPoint::BeforeFader, fallback);
            let params = build_rt_params(&st, SoloMode::Exclusive, &Effects(false), None);
            let mut rig = Rig::new(&st, 64);
            rig.inputs[0][0].fill(1.0);
            // Nothing is connected, so whatever the buffer holds is unused.
            rig.strip_returns[0][0].fill(0.5);
            rig.settle(&params);
            assert!(close(rig.outputs[0][0][63], expected), "{fallback:?}");
            // The sound still goes out, for a program about to connect.
            assert!(close(rig.strip_sends[0][0][63], 1.0));
        }
    }

    #[test]
    fn external_effects_connecting_crossfade() {
        let mut st = mic_with_effects(InsertPoint::BeforeFader, InsertFallback::PassThrough);
        st.strips[0].gain_db = 0.0;
        let p1 = build_rt_params(&st, SoloMode::Exclusive, &Effects(false), None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].fill(1.0);
        rig.settle(&p1);
        assert!(close(rig.outputs[0][0][63], 1.0));
        // A program connects and gives back silence: the strip fades from
        // its own sound to that over the ramp, rather than cutting out.
        let p2 = build_rt_params(&st, SoloMode::Exclusive, &Effects(true), Some(&p1));
        rig.run(&p2, 32);
        let out = &rig.outputs[0][0];
        assert!(out[0] > 0.95 && out[0] < 1.0, "{}", out[0]);
        assert!(out.windows(2).all(|w| w[1] <= w[0]));
        assert!(out[16] > 0.3 && out[16] < 0.7, "{}", out[16]);
        assert!(close(out[63], 0.0));
    }

    #[test]
    fn external_effects_hear_a_strip_heard_nowhere() {
        let mut st = mic_with_effects(InsertPoint::BeforeFader, InsertFallback::PassThrough);
        st.strips[0].mute = true;
        st.strips[0].routes.clear();
        let params = build_rt_params(&st, SoloMode::Exclusive, &Effects(true), None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].fill(1.0);
        rig.settle(&params);
        assert!(close(rig.strip_sends[0][0][63], 1.0));
    }

    #[test]
    fn what_comes_back_goes_through_the_rest_of_the_chain() {
        let gated = |position| {
            let mut st = mic_with_effects(position, InsertFallback::PassThrough);
            st.strips[0].gain_db = 0.0;
            st.strips[0].gate = Gate {
                enabled: true,
                threshold_db: -40.0,
                range_db: -90.0,
                attack_ms: 1.0,
                hold_ms: 10.0,
                release_ms: 10.0,
            };
            let params = build_rt_params(&st, SoloMode::Exclusive, &Effects(true), None);
            let mut rig = Rig::new(&st, 480);
            let mut seed = 3;
            // Someone talks, and the effects give back only quiet noise.
            for block in 0..40 {
                sine(&mut rig.inputs[0][0], block * 480, 300.0, 0.25);
                noise(&mut rig.strip_returns[0][0], &mut seed, 0.001);
                rig.run(&params, 480);
            }
            peak(&rig.outputs[0][0])
        };
        // Before the gate, the gate hears the noise and shuts it out.
        assert!(gated(InsertPoint::BeforeGate) < 1e-5);
        // After it, the gate heard the talking and stays open for the noise.
        let after = gated(InsertPoint::BeforeEq);
        assert!(after > 5e-4 && after <= 0.001, "{after}");
    }

    #[test]
    fn bus_external_effects_sit_where_they_are_put() {
        let g = db_to_linear(-6.0);
        // (where, what the effects hear, what comes out)
        for (position, sent, out) in [
            (InsertPoint::BeforeFader, 1.0, 0.5 * g),
            (InsertPoint::AfterLimiter, g, 0.5),
        ] {
            let mut st = mic_only();
            st.buses[0].gain_db = -6.0;
            st.buses[0].limiter = Limiter::default();
            st.buses[0].insert = Insert {
                enabled: true,
                position,
                fallback: InsertFallback::PassThrough,
            };
            let params = build_rt_params(&st, SoloMode::Exclusive, &Effects(true), None);
            let mut rig = Rig::new(&st, 64);
            rig.inputs[0][0].fill(1.0);
            rig.bus_returns[0][0].fill(0.5);
            rig.settle(&params);
            assert!(close(rig.bus_sends[0][0][63], sent), "{position:?}");
            assert!(close(rig.outputs[0][0][63], out), "{position:?}");
        }
    }

    #[test]
    fn external_effects_switched_off_stop_sending() {
        let mut st = mic_with_effects(InsertPoint::BeforeFader, InsertFallback::PassThrough);
        let p1 = build_rt_params(&st, SoloMode::Exclusive, &Effects(true), None);
        let mut rig = Rig::new(&st, 64);
        rig.inputs[0][0].fill(1.0);
        rig.strip_returns[0][0].fill(0.5);
        rig.settle(&p1);
        st.strips[0].insert.enabled = false;
        let p2 = build_rt_params(&st, SoloMode::Exclusive, &Effects(true), Some(&p1));
        rig.settle(&p2);
        assert!(close(rig.outputs[0][0][63], db_to_linear(-6.0)));
        assert!(rig.strip_sends[0][0].iter().all(|&v| v == 0.0));
    }
}
