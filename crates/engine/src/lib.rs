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

pub use dsp::sounds::{builtin_sound, SoundBank, SoundData};
pub use pw::{Engine, EngineError, EngineEvent, EngineHandle, EngineOptions};
