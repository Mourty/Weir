//! Real-time mixing core, with no PipeWire in it.
//!
//! Design in one paragraph: the control thread builds an immutable
//! [`RtParams`] snapshot from the user-facing mixer state and publishes it
//! through an `ArcSwap`. The real-time thread loads the snapshot at the start
//! of every cycle, migrates its ramp state from the previous snapshot when the
//! pointer changed, and then mixes every strip into every routed bus with
//! per-sample linear ramps on all coefficients so that fader moves, mutes,
//! route toggles and pan changes never click. Nothing in the process path
//! allocates, locks or blocks.
//!
//! * [`params`]: the snapshot and its builder.
//! * [`mapping`]: how a strip's channels land on a bus's speakers.
//! * [`fx`]: the effects, and the state they keep between snapshots.
//! * [`handoff`]: what comes back from external effects, on its way to
//!   the engine.
//! * [`process`]: one cycle of mixing.
//! * [`sounds`]: the clicks and beeps hotkeys play.
//! * [`analyzer`]: the equalizer's spectrum, computed off the real-time
//!   thread.

pub mod analyzer;
pub mod fx;
pub mod handoff;
pub mod mapping;
pub mod params;
pub mod process;
pub mod sounds;

pub use params::{build_rt_params, PortPtr, PortResolver, RtParams};
pub use process::Processor;
