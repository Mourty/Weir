//! What the window shows in place of the mixer while it has no daemon to
//! talk to: waiting for one, or why the one it started stopped.

use super::App;
use crate::client::DaemonExit;
use crate::theme;
use egui::{Align, Frame, Layout, RichText, Ui};

impl App {
    /// Waiting to connect.
    pub(super) fn disconnected_view(&self, ui: &mut Ui, error: Option<&str>, spawned: bool) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            ui.label(RichText::new("Connecting to the Weir daemon…").size(18.0));
            ui.add_space(6.0);
            let hint = if spawned {
                "The daemon was started automatically, waiting for it."
            } else {
                "Start it with `weir-daemon` or `systemctl --user start weir`."
            };
            ui.label(RichText::new(hint).color(theme::p().text_dim));
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("socket: {}", self.socket.display()))
                    .size(11.0)
                    .color(theme::p().text_dim),
            );
            if let Some(e) = error {
                ui.label(RichText::new(e).size(11.0).color(theme::p().text_dim));
            }
        });
    }

    /// The daemon this window started has stopped: say so, show what it
    /// printed about why, and offer to start it again.
    pub(super) fn daemon_stopped_view(&self, ui: &mut Ui, exit: &DaemonExit) {
        ui.vertical_centered(|ui| {
            ui.set_max_width(640.0);
            ui.add_space(ui.available_height() * 0.2);
            ui.label(RichText::new("Weir's daemon stopped").size(18.0));
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "The daemon is the part of Weir that does the mixing. It stopped before \
                     this window could reach it. This is what it said:",
                )
                .color(theme::p().text_dim),
            );
            ui.add_space(6.0);
            Frame::new()
                .fill(theme::p().track)
                .corner_radius(6)
                .inner_margin(10)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&exit.reason).monospace().size(12.0))
                                .selectable(true),
                        );
                    });
                });
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("The full log is in {}", exit.log.display()))
                    .size(11.0)
                    .color(theme::p().text_dim),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                // Centered by hand: a horizontal row inside a centered column
                // still starts at the left.
                let w = 200.0;
                ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                if ui
                    .button("Copy the message")
                    .on_hover_text("To paste into a bug report or a message asking for help")
                    .clicked()
                {
                    ui.ctx().copy_text(exit.reason.clone());
                }
                if ui
                    .button("Try again")
                    .on_hover_text("Start the daemon again")
                    .clicked()
                {
                    let mut sh = self.client.shared.lock().unwrap();
                    sh.daemon_exit = None;
                    sh.retry_spawn = true;
                }
            });
        });
    }
}
