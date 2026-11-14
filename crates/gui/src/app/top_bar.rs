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
        root: &mut Ui,
        connected: bool,
        state: &FullState,
        last_error: Option<&str>,
    ) {
        egui::Panel::top("bar")
            .frame(
                Frame::new()
                    .fill(theme::p().top_bar)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(root, |ui| {
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
                        // Clicking "Recent changes" unfolds it: only a
                        // click outside, or an item, closes this menu.
                        widgets::field_menu(ui, "…", |ui| self.main_menu(ui));
                        if ui
                            .add_enabled(connected, egui::Button::new("Hotkeys"))
                            .on_hover_text(
                                "Keys that do things in the mixer from anywhere, such as push \
                                 to talk",
                            )
                            .clicked()
                        {
                            self.open_hotkeys();
                        }
                        ui.menu_button("Setups", |ui| {
                            self.library_menu(ui, LibraryKind::Setup, state);
                        });
                        ui.menu_button("Scenes", |ui| {
                            self.library_menu(ui, LibraryKind::Scene, state);
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
                        ui.add_enabled_ui(connected, |ui| self.recorder_button(ui, state));
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
        // The list unfolds in place. As a side menu it opened on hover, and
        // at the window's right edge it covered the items below it while
        // the pointer passed on its way down. A long list scrolls, about
        // ten changes tall, so the menu never grows past that.
        // Each time the menu opens, the list starts folded and at its top,
        // the newest change.
        let pass = ui.ctx().cumulative_pass_nr();
        let just_opened = pass > self.main_menu_pass + 1;
        self.main_menu_pass = pass;
        if just_opened {
            self.main_menu_opens += 1;
        }
        let opens = self.main_menu_opens;
        let any = !self.history.undo.is_empty();
        ui.add_enabled_ui(any, |ui| {
            egui::CollapsingHeader::new("Recent changes")
                .id_salt("recent_changes")
                .open((!any || just_opened).then_some(false))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Click one to undo back to before it")
                            .size(11.0)
                            .color(theme::p().text_dim),
                    );
                    let row = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
                    let max_h = (10.0 * row).min(ui.content_rect().height() * 0.4);
                    let entries = self.history.undo.clone();
                    // A width of its own: sized by the text, a long name
                    // would widen the whole menu. Long names are cut with
                    // "…" and shown whole on hover.
                    let list_w = 240.0;
                    // A new list each opening, so it starts at the top.
                    egui::ScrollArea::vertical()
                        .id_salt(("recent_changes_list", opens))
                        .max_height(max_h)
                        .show(ui, |ui| {
                            ui.set_width(list_w);
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                            for (k, e) in entries.iter().enumerate() {
                                let button = egui::Button::new(&e.label)
                                    .shortcut_text(ago(e.at_ms))
                                    .min_size(vec2(list_w, 0.0));
                                let r = ui.add(button).on_hover_text(&e.label);
                                if r.clicked() {
                                    self.undo_steps(k + 1);
                                    // The menu, not the fold this sits in.
                                    ui.close_kind(egui::UiKind::Menu);
                                }
                            }
                        });
                });
        });
        ui.separator();
        if ui
            .button("Export settings…")
            .on_hover_text("Save scenes, setups, hotkeys and more to a file")
            .clicked()
        {
            self.open_export();
            ui.close();
        }
        if ui
            .button("Import settings…")
            .on_hover_text("Bring in settings from a file Weir exported")
            .clicked()
        {
            self.open_import(ui.ctx());
            ui.close();
        }
        ui.separator();
        if ui.button("Preferences…").clicked() {
            self.open_prefs(None);
            ui.close();
        }
        if ui.button("About").clicked() {
            self.open_about();
            ui.close();
        }
        if self.safe_menu_item(ui) {
            ui.close();
        }
        if ui.button("Quit window").clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            ui.close();
        }
    }

    /// Which mix the strip faders show: their own levels, or one bus's.
    pub(super) fn mix_bar(&mut self, root: &mut Ui, state: &FullState) {
        if self
            .mix_view
            .is_some_and(|id| state.mixer.bus(id).is_none())
        {
            self.mix_view = None;
        }
        if state.mixer.buses.is_empty() {
            return;
        }
        egui::Panel::top("mixbar")
            .frame(
                Frame::new()
                    .fill(theme::p().bg)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(root, |ui| {
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
        "waiting" => (
            p.meter_yellow,
            "waiting for PipeWire to come back".to_string(),
        ),
        "disabled" => (p.text_dim, "engine disabled".to_string()),
        other => (p.meter_yellow, other.to_string()),
    }
}
