//! Shared types for Weir: the mixer data model and the JSON-RPC control
//! protocol spoken between `weir-daemon` and its clients (the window,
//! `weirctl`, Stream Deck plugins, scripts).
//!
//! Everything in this crate is plain data with `serde` derives, plus the
//! maths clients must agree with the engine on: the equalizer's filters and
//! the compressor's curve, so that every client draws exactly what the
//! engine applies. It has no knowledge of PipeWire or of any UI toolkit.
//!
//! * [`model`]: strips, buses, devices, meters and settings.
//! * [`fx`]: the settings of every effect.
//! * [`library`]: scenes and setups, the two kinds of saved mix.
//! * [`hotkeys`]: keys that do things in the mixer, and their descriptions.
//! * [`keys`]: key combinations such as `Ctrl+Alt+M`.
//! * [`rpc`]: every request, response and notification.
//!
//! The doc comments here are also the protocol's documentation: the
//! daemon's `describe` method returns JSON Schemas generated from these
//! types, descriptions included.

#![warn(missing_docs)]

pub mod fx;
pub mod hotkeys;
pub mod keys;
pub mod library;
pub mod model;
pub mod rpc;

pub use fx::*;
pub use hotkeys::*;
pub use keys::*;
pub use library::*;
pub use model::*;
pub use rpc::*;

/// Version of the control protocol. Bumped on breaking changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Default location of the control socket.
///
/// `$XDG_RUNTIME_DIR/weir/control.sock`, falling back to
/// `/tmp/weir-<uid>/control.sock` when the runtime dir is unset.
pub fn default_socket_path() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let dir = std::path::PathBuf::from(dir);
        if dir.is_dir() {
            return dir.join("weir").join("control.sock");
        }
    }
    let uid = std::fs::metadata("/proc/self")
        .map(|m| {
            use std::os::unix::fs::MetadataExt;
            m.uid()
        })
        .unwrap_or(0);
    std::path::PathBuf::from(format!("/tmp/weir-{uid}")).join("control.sock")
}
