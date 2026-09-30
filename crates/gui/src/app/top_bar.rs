//! The bars along the top of the window: the engine's state and the menus,
//! and under them the choice of which mix the strip faders show.

use super::dialogs::{AddBusDialog, AddStripDialog};
use super::history::{ago, HistoryKey};
use super::library::LibraryKind;
use super::{bus_title, fmt_rate, frames_ms, App};
use crate::{theme, widgets};
use egui::{vec2, Align, Frame, Layout, RichText, Ui};
use weir_protocol::*;

impl App {
    pub(super) fn top_bar(
        &mut self,
        ctx: &egui::Context,
        connected: bool,
        state: &FullState,
        last_error: Option<&str>,
    ) {
        egui::TopBottomPanel::top("bar")
            .frame(
                Frame::new()
                    .fill(theme::p().top_bar)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Weir")
                            .size(18.0)
                            .strong()
                            .color(theme::p().accent),
                    );
                    ui.add_space(8.0);
                    let (color, text) = engine_status(connected, state);
                    ui.label(RichText::new("•").size(18.0).color(color));
                    ui.label(RichText::new(text).size(12.0).color(theme::p().text_dim));
                    if let Some(err) = last_error {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(format!("Error: {err}"))
                                .size(12.0)
                                .color(theme::p().meter_red),
                        );
                    }
                    // Right to left, so the first here is the rightmost.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.menu_button("…", |ui| self.main_menu(ui));
                        ui.menu_button("Setups", |ui| {
                            self.library_menu(ui, LibraryKind::Setup, &state.library);
                        });
                        ui.menu_button("Scenes", |ui| {
                            self.library_menu(ui, LibraryKind::Scene, &state.library);
                        });
                        ui.menu_button(format!("Apps ({})", state.apps.len()), |ui| {
                            self.apps_menu(ui, state);
                        });
                        if ui
                            .add_enabled(connected, egui::Button::new("+ Bus"))
                            .clicked()
                        {
                            self.add_bus = Some(AddBusDialog::new());
                        }
                        if ui
                            .add_enabled(connected, egui::Button::new("+ Strip"))
                            .clicked()
                        {
                            self.add_strip = Some(AddStripDialog::new(&state.mixer));
                        }
                    });
                });
            });
    }

    /// The "…" menu: undo and redo, the list of recent changes, and the
    /// window's own things.
    fn main_menu(&mut self, ui: &mut Ui) {
        let undo = self.history.undo.first().map(|e| e.label.clone());
        let redo = self.history.redo.first().map(|e| e.label.clone());
        let undo_text = undo.map_or_else(|| "Undo".to_string(), |l| format!("Undo {l}"));
        if ui
            .add_enabled(
                !self.history.undo.is_empty(),
                egui::Button::new(undo_text).shortcut_text("Ctrl+Z"),
            )
            .clicked()
        {
            self.step_history(HistoryKey::Undo);
            ui.close();
        }
        let redo_text = redo.map_or_else(|| "Redo".to_string(), |l| format!("Redo {l}"));
        if ui
            .add_enabled(
                !self.history.redo.is_empty(),
                egui::Button::new(redo_text).shortcut_text("Ctrl+Shift+Z"),
            )
            .clicked()
        {
            self.step_history(HistoryKey::Redo);
            ui.close();
        }
        ui.add_enabled_ui(!self.history.undo.is_empty(), |ui| {
            ui.menu_button("Recent changes", |ui| {
                ui.label(
                    RichText::new("Click one to undo back to before it")
                        .size(11.0)
                        .color(theme::p().text_dim),
                );
                let entries = self.history.undo.clone();
                for (k, e) in entries.iter().enumerate() {
                    let r = ui.add(egui::Button::new(&e.label).shortcut_text(ago(e.at_ms)));
                    if r.clicked() {
                        self.undo_steps(k + 1);
                        ui.close();
                    }
                }
            });
        });
        ui.separator();
        if ui.button("Preferences…").clicked() {
            self.show_prefs = true;
            ui.close();
        }
        if ui.button("About").clicked() {
            self.show_about = true;
            ui.close();
        }
        if ui.button("Quit window").clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Which mix the strip faders show: their own levels, or one bus's.
    pub(super) fn mix_bar(&mut self, ctx: &egui::Context, state: &FullState) {
        if self
            .mix_view
            .is_some_and(|id| state.mixer.bus(id).is_none())
        {
            self.mix_view = None;
        }
        if state.mixer.buses.is_empty() {
            return;
        }
        egui::TopBottomPanel::top("mixbar")
            .frame(
                Frame::new()
                    .fill(theme::p().bg)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(
                        RichText::new("Faders show")
                            .size(12.0)
                            .color(theme::p().text_dim),
                    );
                    let main = self.mix_view.is_none();
                    if widgets::toggle(ui, "Main levels", main, theme::p().accent, vec2(0.0, 24.0))
                        .on_hover_text("Each fader sets its strip's level in every mix it feeds")
                        .clicked()
                    {
                        self.mix_view = None;
                    }
                    for b in &state.mixer.buses {
                        let on = self.mix_view == Some(b.id);
                        let label = bus_title(&state.mixer, b);
                        if widgets::toggle(ui, &label, on, theme::bus_color(b), vec2(0.0, 24.0))
                            .on_hover_text(format!(
                                "Make the faders set how loud each strip is in {}",
                                b.name
                            ))
                            .clicked()
                        {
                            self.mix_view = if on { None } else { Some(b.id) };
                        }
                    }
                    ui.add_space(8.0);
                    let (text, color) = match self.mix_view.and_then(|id| state.mixer.bus(id)) {
                        Some(b) => (
                            format!(
                                "Faders set each strip's level in {} only, on top of its main \
                                 fader.",
                                b.name
                            ),
                            theme::bus_color(b),
                        ),
                        None => (
                            "A level on a route button is that mix's own trim.".to_string(),
                            theme::p().text_dim,
                        ),
                    };
                    ui.add(
                        egui::Label::new(RichText::new(text).size(11.5).color(color)).truncate(),
                    );
                });
            });
    }
}

/// The engine's state in a few words, and the color of its light.
fn engine_status(connected: bool, state: &FullState) -> (egui::Color32, String) {
    let p = theme::p();
    if !connected {
        return (p.text_dim, "not connected".to_string());
    }
    let e = &state.engine;
    let held = |on: bool| if on { ", held" } else { "" };
    match e.state.as_str() {
        "streaming" => (
            p.meter_green,
            format!(
                "engine running · {}{} · {} frames{} · {:.1} ms",
                fmt_rate(e.sample_rate),
                held(state.settings.sample_rate.is_some()),
                e.quantum,
                held(state.settings.quantum.is_some()),
                frames_ms(e.quantum, e.sample_rate)
            ),
        ),
        "paused" => (p.meter_yellow, "engine idle (nothing linked)".to_string()),
        "error" => (
            p.meter_red,
            format!("engine error: {}", e.error.clone().unwrap_or_default()),
        ),
        "disabled" => (p.text_dim, "engine disabled".to_string()),
        other => (p.meter_yellow, other.to_string()),
    }
}
