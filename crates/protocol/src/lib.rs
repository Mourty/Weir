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
//! * [`meter_colors`]: the colors meters are drawn in.
//! * [`recorder`]: recording strips and buses to files.
//! * [`hotkeys`]: keys that do things in the mixer, and their descriptions.
//! * [`keys`]: key combinations such as `Ctrl+Alt+M`.
//! * [`sounds`]: what hotkeys show and play when pressed.
//! * [`targets`]: where requests name strips and buses, and finding them.
//! * [`transfer`]: exporting settings to files, and importing them.
//! * [`report`]: what a bug report says, worded alike by every client.
//! * [`crash`]: crash reports, and locks a panic cannot spoil.
//! * [`safe`]: safe mode, which holds parts of Weir back after a crash.
//! * [`updates`]: whether a newer Weir is out.
//! * [`rpc`]: every request, response and notification.
//!
//! The doc comments here are also the protocol's documentation: the
//! daemon's `describe` method returns JSON Schemas generated from these
//! types, descriptions included.

#![warn(missing_docs)]

pub mod appimage;
pub mod crash;
pub mod fx;
pub mod hotkeys;
pub mod keys;
pub mod library;
pub mod meter_colors;
pub mod model;
pub mod recorder;
pub mod report;
pub mod rpc;
pub mod safe;
pub mod sounds;
pub mod targets;
pub mod transfer;
pub mod updates;

pub use fx::*;
pub use hotkeys::*;
pub use keys::*;
pub use library::*;
pub use meter_colors::*;
pub use model::*;
pub use recorder::*;
pub use report::*;
pub use rpc::*;
pub use safe::*;
pub use sounds::*;
pub use targets::*;
pub use transfer::*;
pub use updates::*;

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

/// This program's own file, to start it or its partner again.
///
/// An update replaces the file while the old copy keeps running, and Linux
/// then names the running one's file `PATH (deleted)`. Starting that fails,
/// so this gives `PATH` instead, where the new version now is.
pub fn own_exe() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe().map(|exe| without_deleted(exe, |p| p.exists()))
}

/// `exe` without the ` (deleted)` Linux adds to a replaced file's name,
/// when the file at the plain name `exists`.
fn without_deleted(
    exe: std::path::PathBuf,
    exists: impl Fn(&std::path::Path) -> bool,
) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let plain = exe
        .as_os_str()
        .as_bytes()
        .strip_suffix(b" (deleted)")
        .map(|b| std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b)));
    match plain {
        Some(plain) if exists(&plain) => plain,
        _ => exe,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_replaced_program_is_found_at_its_plain_name() {
        let replaced = PathBuf::from("/usr/bin/weir-daemon (deleted)");
        assert_eq!(
            without_deleted(replaced.clone(), |_| true),
            Path::new("/usr/bin/weir-daemon")
        );
        // Removed rather than replaced: nothing better to start.
        assert_eq!(without_deleted(replaced.clone(), |_| false), replaced);
        let plain = PathBuf::from("/usr/bin/weir-daemon");
        assert_eq!(without_deleted(plain.clone(), |_| true), plain);
    }
}
