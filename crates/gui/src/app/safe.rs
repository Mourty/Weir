//! Safe mode, as the window shows it: what the window holds back by itself
//! after it closed because of a bug soon after opening, the bar under the
//! top bar saying why, the Safe mode window with a way to bring each thing
//! back, and the slim cards that stand in for strips and buses held back.
//!
//! The service keeps what safe mode holds back (`FullState::safe_mode`);
//! the window only asks. Before the service has answered, the window holds
//! back its own parts and every strip and bus by itself, since drawing
//! them may be what crashed it.

use super::mixer::Metrics;
use super::App;
use crate::theme;
use crate::transfer::{Pane, WindowSize};
use egui::{Frame, RichText, Ui};
use std::time::{Duration, Instant};
use tracing::warn;
use weir_protocol::*;

/// The width of the card standing in for a strip or bus held back.
const HELD_W: f32 = 124.0;

/// How long the window holds things back by itself after asking the
/// service to: an older service that does not know safe mode never says
/// it is in it.
const ASK_WAIT: Duration = Duration::from_secs(3);

/// What the window knows and does about safe mode.
#[derive(Default)]
pub(super) struct SafeUi {
    /// Safe mode to ask the service for, once it can be asked: after this
    /// window closed because of a bug soon after opening.
    ask: Option<EnterSafeModeParams>,
    /// Hold back the window's parts and every strip and bus here until the
    /// service says what it holds back; and when the service was asked.
    local: bool,
    asked_at: Option<Instant>,
    /// Restart the service in safe mode once connected, unless it is in it
    /// already: `weir --safe`.
    restart: bool,
    /// What safe mode holds back, as of this frame.
    now: SafeMode,
    /// What crash reports were last told.
    noted: Option<SafeMode>,
    /// The Safe mode window is open, and to be brought forward.
    open: bool,
    raise: bool,
}

impl SafeUi {
    /// Safe mode as the window starts: asked for with `weir --safe`
    /// (`asked`), or after the window closed because of a bug soon after
    /// opening last time.
    pub(super) fn at_start(asked: bool) -> Self {
        let ask = weir_protocol::crash::safe_mode_wanted("window").map(|r| EnterSafeModeParams {
            why: SafeWhy::Window,
            suspect: r.brought_back,
        });
        if ask.is_some() {
            warn!("the window closed because of a bug last time, so it starts in safe mode");
        }
        Self {
            local: ask.is_some(),
            ask,
            restart: asked,
            ..Default::default()
        }
    }

    /// Whether `item` is held back now.
    pub(super) fn holds(&self, item: &SafeItem) -> bool {
        if self.now.on {
            self.now.holds(item)
        } else {
            self.local && !item.of_service()
        }
    }

    /// Whether Weir is in safe mode, as far as the window knows.
    pub(super) fn on(&self) -> bool {
        self.now.on || self.local
    }

    /// Safe mode as crash reports are to tell it.
    fn as_known(&self) -> SafeMode {
        if self.local && !self.now.on {
            SafeMode {
                on: true,
                why: SafeWhy::Window,
                ..Default::default()
            }
        } else {
            self.now.clone()
        }
    }
}

/// What `safe` holds back that `mixer` has: strips and buses held back by
/// a name no strip or bus has now are left out of lists.
fn shown_held(safe: &SafeMode, mixer: &MixerState) -> Vec<SafeItem> {
    safe.held
        .iter()
        .filter(|i| here(i, mixer))
        .cloned()
        .collect()
}

/// Whether `item` is in `mixer`, or is not a strip or bus.
fn here(item: &SafeItem, mixer: &MixerState) -> bool {
    match item {
        SafeItem::Strip(n) => mixer.strips.iter().any(|s| &s.name == n),
        SafeItem::Bus(n) => mixer.buses.iter().any(|b| &b.name == n),
        _ => true,
    }
}

/// "3 things", "1 thing".
fn things(n: usize) -> String {
    if n == 1 {
        "1 thing".into()
    } else {
        format!("{n} things")
    }
}

impl App {
    /// Whether safe mode holds `item` back now.
    pub(super) fn held(&self, item: &SafeItem) -> bool {
        self.safe.holds(item)
    }

    /// Follow the service's safe mode, and ask it for what the window
    /// wants once it can.
    pub(super) fn safe_tick(&mut self, state: &FullState, has_state: bool) {
        let s = &mut self.safe;
        s.now = state.safe_mode.clone();
        if s.now.on {
            s.local = false;
        }
        if has_state {
            if let Some(p) = s.ask.take() {
                self.actions.push(Request::EnterSafeMode(p));
                s.asked_at = Some(Instant::now());
            }
            if std::mem::take(&mut s.restart) && !s.now.on {
                // The service starts again in safe mode and opens a new
                // window; this one is told to close.
                self.actions.push(Request::Restart(RestartParams {
                    window: true,
                    safe: true,
                }));
            }
        }
        if s.local && s.asked_at.is_some_and(|t| t.elapsed() > ASK_WAIT) {
            warn!("the service did not go into safe mode; showing everything");
            s.local = false;
        }
        let known = s.as_known();
        if s.noted.as_ref() != Some(&known) {
            weir_protocol::crash::note_safe_mode(&known);
            s.noted = Some(known);
        }
    }

    /// Open the Safe mode window, or bring it forward.
    pub(super) fn open_safe_mode(&mut self) {
        self.safe.open = true;
        self.safe.raise = true;
    }

    /// The main menu's way into safe mode, or to what it holds back.
    /// Returns whether it was used.
    pub(super) fn safe_menu_item(&mut self, ui: &mut Ui) -> bool {
        if self.safe.now.on {
            if ui.button("Safe mode…").clicked() {
                self.open_safe_mode();
                return true;
            }
        } else if ui
            .button("Restart in safe mode")
            .on_hover_text(
                "Start Weir again with strips, buses and the parts most likely to make it \
                 crash held back, to bring back one at a time",
            )
            .clicked()
        {
            self.actions.push(Request::Restart(RestartParams {
                window: true,
                safe: true,
            }));
            return true;
        }
        false
    }

    /// Ask the service to bring `items` back.
    fn bring_back(&mut self, items: Vec<SafeItem>) {
        self.actions
            .push(Request::BringBack(BringBackParams { items, all: false }));
    }

    /// The bar under the top bar while in safe mode: why, and the quickest
    /// ways out of it.
    pub(super) fn safe_bar(&mut self, root: &mut Ui, state: &FullState) {
        if !self.safe.on() {
            return;
        }
        let p = theme::p();
        let safe = self.safe.now.clone();
        let held = shown_held(&safe, &state.mixer);
        egui::Panel::top("safe_bar")
            .frame(
                Frame::new()
                    .fill(p.section_fill)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(root, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Safe mode").strong().color(p.warning));
                    if !safe.on {
                        ui.label(
                            "Weir's window closed unexpectedly last time, so it holds some \
                             things back.",
                        );
                        return;
                    }
                    ui.label(safe.reason());
                    let count = if held.is_empty() {
                        "Everything is back.".to_string()
                    } else {
                        format!("{} held back.", things(held.len()))
                    };
                    ui.label(RichText::new(count).color(p.text_dim));
                    if let Some(suspect) = safe.suspect.as_ref().filter(|_| held.len() > 1) {
                        let label = format!("Bring back all but {}", suspect.in_sentence());
                        if ui.button(label).clicked() {
                            let rest = safe.held.iter().filter(|i| *i != suspect).cloned();
                            self.bring_back(rest.collect());
                        }
                    } else if !held.is_empty() && ui.button("Bring back all").clicked() {
                        self.actions.push(Request::BringBack(BringBackParams {
                            items: Vec::new(),
                            all: true,
                        }));
                    }
                    if ui.button("What's held back…").clicked() {
                        self.open_safe_mode();
                    }
                    if ui.button("Leave safe mode").clicked() {
                        self.actions.push(Request::LeaveSafeMode);
                    }
                });
            });
    }

    /// The Safe mode window: everything safe mode holds back, and what it
    /// brought back, with a way to bring each back.
    pub(super) fn safe_window(&mut self, ctx: &egui::Context, state: &FullState) {
        if !self.safe.open {
            return;
        }
        if !self.safe.now.on {
            // Left, from here or elsewhere: nothing to show.
            self.safe.open = false;
            return;
        }
        let safe = self.safe.now.clone();
        let mut asked: Vec<Request> = Vec::new();
        let (mut raise, mut closed) = (std::mem::take(&mut self.safe.raise), false);
        let mut done = false;
        let size = WindowSize {
            size: [640.0, 620.0],
            min: [480.0, 360.0],
        };
        crate::transfer::window_sized(
            ctx,
            "safe-mode",
            "Safe mode",
            size,
            &mut raise,
            &mut closed,
            |ui, pane| match pane {
                Pane::Body => {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| safe_body(ui, &safe, &state.mixer, &mut asked));
                }
                Pane::Footer => {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            done = true;
                        }
                        if ui
                            .button("Leave safe mode")
                            .on_hover_text("Bring everything back, and stop being in safe mode")
                            .clicked()
                        {
                            asked.push(Request::LeaveSafeMode);
                            done = true;
                        }
                        if !safe.held.is_empty() && ui.button("Bring back all").clicked() {
                            asked.push(Request::BringBack(BringBackParams {
                                items: Vec::new(),
                                all: true,
                            }));
                        }
                    });
                }
            },
        );
        self.actions.extend(asked);
        if closed || done {
            self.safe.open = false;
        }
    }

    /// The card standing in for a strip or bus held back: its name, why it
    /// is not shown, and a way to bring it back.
    pub(super) fn held_view(
        &mut self,
        ui: &mut Ui,
        item: SafeItem,
        name: &str,
        caption: &str,
        lay: &Metrics,
    ) -> egui::Response {
        let p = theme::p();
        let audio = self.safe.now.audio;
        let suspect = self.safe.now.suspect.as_ref() == Some(&item);
        Frame::new()
            .fill(p.strip_bg)
            .stroke(egui::Stroke::new(1.0_f32, p.grid_major))
            .corner_radius(8)
            .inner_margin(6)
            .show(ui, |ui| {
                ui.set_width(HELD_W - 12.0);
                ui.set_min_height(lay.strip_h - 12.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(caption).size(10.0).color(p.text_dim));
                    ui.add(egui::Label::new(RichText::new(name).strong()).truncate())
                        .on_hover_text(name);
                    ui.add_space(8.0);
                    ui.label(RichText::new("Held back by safe mode").color(p.warning));
                    let what = if audio {
                        "Not set up, so silent."
                    } else {
                        "Not drawn. It still plays."
                    };
                    ui.label(RichText::new(what).size(11.0).color(p.text_dim));
                    if suspect {
                        ui.label(
                            RichText::new("Weir crashed after this came back last time.")
                                .size(11.0)
                                .color(p.warning),
                        );
                    }
                    ui.add_space(6.0);
                    if ui.button("Bring back").clicked() {
                        self.bring_back(vec![item.clone()]);
                    }
                });
            })
            .response
    }

    /// In Preferences, over settings safe mode holds back: that they are,
    /// and a way to bring them back. Changes made meanwhile are kept.
    pub(super) fn held_prefs_note(&mut self, ui: &mut Ui, item: SafeItem) {
        if !self.held(&item) {
            return;
        }
        let p = theme::p();
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(
                    "Safe mode holds these back. Changes are kept, and show once they come back.",
                )
                .size(11.0)
                .color(p.warning),
            );
            if self.safe.now.on && ui.small_button("Bring back").clicked() {
                self.bring_back(vec![item.clone()]);
            }
        });
        ui.add_space(4.0);
    }
}

/// Which items a group of the Safe mode window lists.
type Kind = fn(&SafeItem) -> bool;

/// The Safe mode window's contents.
fn safe_body(ui: &mut Ui, safe: &SafeMode, mixer: &MixerState, asked: &mut Vec<Request>) {
    let p = theme::p();
    ui.label(RichText::new(safe.reason()).size(15.0).strong());
    ui.add_space(4.0);
    ui.label(
        "Bring things back one at a time, and use Weir a little after each. If it crashes \
         again, its crash report names what came back last, and safe mode then keeps that \
         one back.",
    );
    ui.label(
        RichText::new(
            "Nothing held back is lost or changed: it comes back as it was saved. Weir starts \
             normally again the next time, unless it crashes first.",
        )
        .color(p.text_dim),
    );
    ui.add_space(10.0);
    let groups: [(&str, Kind); 4] = [
        ("In the window", |i| i.of_window()),
        ("Strips", |i| matches!(i, SafeItem::Strip(_))),
        ("Buses", |i| matches!(i, SafeItem::Bus(_))),
        ("In the service", |i| i.of_service()),
    ];
    for (title, of) in groups {
        let held: Vec<&SafeItem> = safe
            .held
            .iter()
            .filter(|i| of(i) && here(i, mixer))
            .collect();
        let back: Vec<&SafeItem> = safe
            .brought_back
            .iter()
            .filter(|i| of(i) && here(i, mixer))
            .collect();
        if held.is_empty() && back.is_empty() {
            continue;
        }
        Frame::new()
            .fill(p.strip_bg)
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(title).size(15.0).strong());
                ui.add_space(4.0);
                for item in held {
                    item_row(ui, safe, item, true, asked);
                }
                for item in back {
                    item_row(ui, safe, item, false, asked);
                }
            });
        ui.add_space(10.0);
    }
}

/// One thing safe mode holds back (`held`) or brought back.
fn item_row(ui: &mut Ui, safe: &SafeMode, item: &SafeItem, held: bool, asked: &mut Vec<Request>) {
    let p = theme::p();
    ui.horizontal(|ui| {
        let button_w = 96.0;
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - button_w).max(120.0));
            let name = RichText::new(item.label());
            let name = if held {
                name.strong()
            } else {
                name.color(p.text_dim)
            };
            ui.add(egui::Label::new(name).truncate());
            let note = if held {
                item.held_note(safe.audio)
            } else if safe.last_brought_back() == Some(item) {
                "Back, most recently."
            } else {
                "Back."
            };
            ui.label(RichText::new(note).size(11.0).color(p.text_dim));
            if held && safe.suspect.as_ref() == Some(item) {
                ui.label(
                    RichText::new("Weir crashed after this came back last time.")
                        .size(11.0)
                        .color(p.warning),
                );
            }
        });
        if held {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Bring back").clicked() {
                    asked.push(Request::BringBack(BringBackParams {
                        items: vec![item.clone()],
                        all: false,
                    }));
                }
            });
        }
    });
    ui.add_space(4.0);
}
