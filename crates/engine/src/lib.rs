//! Weir engine: the real-time mixer core and its PipeWire integration.
//!
//! * `dsp` is pure Rust and has no PipeWire dependency. It defines the
//!   real-time parameter snapshot, the channel mapping rules, the effects
//!   and the per-cycle processor.
//! * `pw` owns the PipeWire connection: the single `pw_filter` engine node,
//!   the virtual devices, the registry mirror and the link manager.
//!
//! The daemon sees only [`Engine`], which starts the PipeWire thread, and
//! the [`EngineHandle`] it returns, and hands it sounds to play as
//! [`SoundBank`]s, Weir's own made by [`builtin_sound`].

#![warn(missing_docs)]

mod dsp;
mod pw;

pub use dsp::record::RecordTap;
pub use dsp::sounds::{builtin_sound, SoundBank, SoundData};
pub use pw::{
    crash_on_purpose, library_version, Engine, EngineError, EngineEvent, EngineHandle,
    EngineOptions,
};

/// For the fuzz targets (`fuzz/` in the repository): the mixing code, run
/// as the process callback runs it, on made-up mixers.
#[cfg(feature = "fuzzing")]
pub mod fuzzing {
    use crate::dsp::harness::Rig;
    use crate::dsp::params::{build_rt_params, NoPorts, MAX_QUANTUM};
    use weir_protocol::{MixerState, SoloMode};

    /// Play `before`, then `after` as a change made while it plays, each
    /// for `cycles` blocks of `frames` samples, every input a steady
    /// `level` and each strip's first channel a click. Returns whether
    /// every output sample stayed finite.
    pub fn play(
        before: &MixerState,
        after: &MixerState,
        solo: SoloMode,
        frames: usize,
        cycles: usize,
        level: f32,
    ) -> bool {
        let frames = frames.clamp(1, MAX_QUANTUM);
        let mut finite = true;
        let mut prev = None;
        for state in [before, after] {
            let params = build_rt_params(state, solo, &NoPorts, prev.as_deref());
            let mut rig = Rig::new(state, frames);
            for (s, strip) in rig.inputs.iter_mut().enumerate() {
                for (c, channel) in strip.iter_mut().enumerate() {
                    channel.fill(level);
                    if c == 0 {
                        channel[s % frames] = 1.0;
                    }
                }
            }
            for _ in 0..cycles {
                rig.run(&params, 64);
                finite &= rig
                    .outputs
                    .iter()
                    .flatten()
                    .flatten()
                    .all(|v| v.is_finite());
            }
            prev = Some(params);
        }
        finite
    }
}
