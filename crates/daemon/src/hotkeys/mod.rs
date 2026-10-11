//! Hotkeys at work: the runner that does what they say, and the keys that
//! press them. The controller keeps the hotkeys themselves (see
//! [`crate::controller::hotkeys`]).
//!
//! * [`runner`]: steps, holding, repeating, fading, putting back, and one
//!   undo step per press.
//! * [`keys`]: getting key presses from the desktop or X11.
//! * [`restore`]: finding and setting back only what a hotkey changed.
//! * [`popup`]: showing what a press did.

pub mod keys;
pub mod popup;
mod restore;
mod runner;

pub use runner::{Command, Runner};

use crate::controller::Controller;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

/// Start the runner and the keys, for as long as the daemon runs.
pub fn start(controller: &Arc<Controller>) {
    controller.set_popups(popup::Popups::start());
    let tx = start_runner(controller);
    tokio::spawn(keys::run(controller.clone(), tx));
}

/// Start the runner, and hand it to the controller. Returns its mailbox.
///
/// The runner is shared rather than reached only through its mailbox so
/// that a client pressing a hotkey gets its answer once the steps are done,
/// and a script reading the mixer next sees what they did.
pub fn start_runner(controller: &Arc<Controller>) -> UnboundedSender<Command> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let runner = Arc::new(Mutex::new(Runner::new(controller.clone(), tx.clone())));
    controller.set_hotkey_runner(runner.clone());
    tokio::spawn(async move {
        while let Some(command) = rx.recv().await {
            runner.lock().unwrap().handle(command);
        }
    });
    tx
}
