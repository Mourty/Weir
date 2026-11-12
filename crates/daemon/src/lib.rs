//! `weir-daemon`'s parts, as a library: the program (`main.rs`) puts them
//! together, and the fuzz targets in `fuzz/` reach the code that reads
//! what comes from outside, such as imported settings and sound files.
//!
//! * [`config`]: the files under `~/.config/weir`.
//! * [`controller`]: the mixer state and every request handler.
//! * [`server`]: the control socket.
//! * [`history`]: undo and redo.
//! * [`tray`]: the system tray icon.
//! * [`defaults`]: the mixer a first run starts with.
//! * [`display`]: finding the desktop to show the window on.
//! * [`hotkeys`]: doing what hotkeys say, and getting their keys.
//! * [`transfer`]: exported settings files.
//! * [`sounds`]: hotkeys' sound files.

pub mod config;
pub mod controller;
pub mod defaults;
pub mod diagnostics;
pub mod display;
pub mod history;
pub mod hotkeys;
pub mod login;
pub mod logins;
pub mod recorder;
pub mod seen_apps;
pub mod server;
pub mod sounds;
pub mod transfer;
pub mod tray;
pub mod updates;

/// Set to `service`, `part`, `audio` or `request`, the service crashes on
/// purpose, to try crash reports: the whole service, a part of its own, the
/// audio thread (each a few seconds after it starts), or a `ping`.
pub const CRASH_TEST_ENV: &str = "WEIR_CRASH_TEST";

/// What `WEIR_CRASH_TEST` asks for, if anything.
pub fn crash_test() -> Option<String> {
    std::env::var(CRASH_TEST_ENV).ok().filter(|t| !t.is_empty())
}
