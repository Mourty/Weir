//! The Export window: every scene, setup, hotkey, sound and equalizer
//! preset of the user's own, the app rules and the parts of the
//! preferences, in a section each, to tick what goes in the `.zip`.

use super::{count, item_row, section, today, window, Pane};
use crate::file_dialog::{Dialog, Picked};
use crate::theme;
use egui::{RichText, Ui};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;
use weir_protocol::*;

/// One thing that can go in an export.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Item {
    Scene(String),
    Setup(String),
    Hotkey(HotkeyId),
    Sound(String),
    EqPreset(String),
    AppRules,
    Part(PreferencePart),
}

/// One line of the list: the item, its name and what it is.
struct Line {
    item: Item,
    name: String,
    detail: String,
}

/// Everything there is to export now, by kind.
fn lines(state: &FullState) -> Vec<(ExportKind, Vec<Line>)> {
    let named = |make: fn(String) -> Item, names: &[String]| -> Vec<Line> {
        names
            .iter()
            .map(|n| Line {
                item: make(n.clone()),
                name: n.clone(),
                detail: String::new(),
            })
            .collect()
    };
    let info = &state.hotkeys;
    let hotkeys = info
        .hotkeys
        .iter()
        .map(|h| {
            let mut detail = h.keys.join(", ");
            if let Some(g) = info.groups.iter().find(|g| g.id == h.group) {
                detail = format!("{detail}  · in {}", g.name);
            }
            Line {
                item: Item::Hotkey(h.id),
                name: h.name.clone(),
                detail,
            }
        })
        .collect();
    let sounds = info
        .sounds
        .iter()
        .filter(|s| !s.builtin)
        .map(|s| Line {
            item: Item::Sound(s.name.clone()),
            name: s.name.clone(),
            detail: s.length(),
        })
        .collect();
    let presets = state
        .eq_presets
        .iter()
        .filter(|p| !p.builtin)
        .map(|p| Line {
            item: Item::EqPreset(p.name.clone()),
            name: p.name.clone(),
            detail: count(p.bands.len(), "band", "bands"),
        })
        .collect();
    let rules = if state.app_rules.is_empty() {
        Vec::new()
    } else {
        vec![Line {
            item: Item::AppRules,
            name: "App rules".into(),
            detail: count(state.app_rules.len(), "rule", "rules"),
        }]
    };
    let parts = PreferencePart::ALL
        .into_iter()
        // Whether Weir starts at login is not known everywhere.
        .filter(|p| *p != PreferencePart::StartAtLogin || state.settings.start_at_login.is_some())
        .map(|p| Line {
            item: Item::Part(p),
            name: p.label().into(),
            detail: p.summary().into(),
        })
        .collect();
    vec![
        (ExportKind::Scene, named(Item::Scene, &state.library.scenes)),
        (ExportKind::Setup, named(Item::Setup, &state.library.setups)),
        (ExportKind::Hotkey, hotkeys),
        (ExportKind::Sound, sounds),
        (ExportKind::EqPreset, presets),
        (ExportKind::AppRules, rules),
        (ExportKind::Preferences, parts),
    ]
}

/// The Export window.
pub struct ExportWindow {
    pub raise: bool,
    pub closed: bool,
    picked: BTreeSet<Item>,
    /// Whether everything has been ticked at the start yet.
    started: bool,
    dialog: Option<Dialog>,
    /// The path being typed, where there is no file dialog.
    typed: Option<String>,
    /// Waiting for the daemon to write the file.
    pub waiting: bool,
    outcome: Option<Result<ExportResult, String>>,
}

impl ExportWindow {
    pub fn new() -> Self {
        Self {
            raise: false,
            closed: false,
            picked: BTreeSet::new(),
            started: false,
            dialog: None,
            typed: None,
            waiting: false,
            outcome: None,
        }
    }

    /// The daemon's answer to the export.
    pub fn reply(&mut self, answer: Result<Value, String>) {
        self.waiting = false;
        self.outcome = Some(answer.and_then(|v| {
            serde_json::from_value(v).map_err(|e| format!("the answer made no sense: {e}"))
        }));
    }

    /// Draw the window. The export request goes into `actions`; `look` is
    /// this window's look, which only it knows.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        state: &FullState,
        look: &Value,
        actions: &mut Vec<Request>,
    ) {
        let all = lines(state);
        // Everything starts ticked.
        if !self.started {
            self.started = true;
            self.picked = all
                .iter()
                .flat_map(|(_, l)| l)
                .map(|l| l.item.clone())
                .collect();
        }
        if let Some(picked) = self.dialog.as_ref().and_then(Dialog::poll) {
            self.dialog = None;
            match picked {
                Picked::File(path) => self.export(path, look, actions),
                Picked::Cancelled => {}
                Picked::NoDialog(_) => {
                    let home = std::env::var("HOME").unwrap_or_default();
                    self.typed = Some(format!("{home}/weir-settings-{}.zip", today()));
                }
            }
        }
        let (mut raise, mut closed) = (self.raise, self.closed);
        window(
            ctx,
            "export",
            "Export settings",
            &mut raise,
            &mut closed,
            |ui, pane| match pane {
                Pane::Body => self.body(ui, &all),
                Pane::Footer => self.footer(ui, ctx, look, actions),
            },
        );
        // Buttons drawn in the window close it too.
        self.raise = raise;
        self.closed |= closed;
    }

    fn body(&mut self, ui: &mut Ui, all: &[(ExportKind, Vec<Line>)]) {
        ui.label(
            "Pick what to put in the file. Take it to another computer, keep it safe, or \
             share some of it, then bring it in with Import settings.",
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Select all").clicked() {
                self.picked = all
                    .iter()
                    .flat_map(|(_, l)| l)
                    .map(|l| l.item.clone())
                    .collect();
            }
            if ui.button("Select none").clicked() {
                self.picked.clear();
            }
            ui.label(
                RichText::new(self.summary())
                    .size(12.0)
                    .color(theme::p().text_dim),
            );
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for (kind, lines) in all {
                    let ticked = lines
                        .iter()
                        .filter(|l| self.picked.contains(&l.item))
                        .count();
                    let id = egui::Id::new(("export-section", *kind));
                    let asked = section(ui, id, kind.heading(), ticked, lines.len(), |ui| {
                        if *kind == ExportKind::Sound {
                            ui.label(
                                RichText::new(
                                    "Your own sounds. A hotkey takes the ones it plays with it.",
                                )
                                .size(12.0)
                                .color(theme::p().text_dim),
                            );
                        }
                        if lines.is_empty() {
                            ui.label(
                                RichText::new(format!("No {} yet.", kind.heading().to_lowercase()))
                                    .color(theme::p().text_dim),
                            );
                        }
                        for l in lines {
                            let mut on = self.picked.contains(&l.item);
                            if item_row(ui, &mut on, true, &l.name, &l.detail) {
                                if on {
                                    self.picked.insert(l.item.clone());
                                } else {
                                    self.picked.remove(&l.item);
                                }
                            }
                        }
                    });
                    match asked {
                        Some(true) => self.picked.extend(lines.iter().map(|l| l.item.clone())),
                        Some(false) => {
                            for l in lines {
                                self.picked.remove(&l.item);
                            }
                        }
                        None => {}
                    }
                }
            });
    }

    fn footer(
        &mut self,
        ui: &mut Ui,
        ctx: &egui::Context,
        look: &Value,
        actions: &mut Vec<Request>,
    ) {
        if let Some(typed) = &mut self.typed {
            ui.label("Your desktop has no file dialog Weir can use. Type where to save it:");
            let mut go = None;
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(typed).desired_width(420.0));
                if ui.button("Save").clicked() {
                    go = Some(PathBuf::from(typed.trim()));
                }
            });
            if let Some(path) = go {
                self.typed = None;
                self.export(path, look, actions);
            }
            ui.add_space(6.0);
        }
        ui.horizontal(|ui| {
            let ready = !self.picked.is_empty() && !self.waiting && self.dialog.is_none();
            let button =
                egui::Button::new(RichText::new("Export…").strong().color(theme::p().on_text))
                    .fill(theme::p().accent);
            if ui.add_enabled(ready, button).clicked() {
                let name = format!("weir-settings-{}.zip", today());
                self.dialog = Some(Dialog::save(
                    ctx,
                    "Export settings",
                    &name,
                    &[("Weir settings", &["*.zip"])],
                ));
                self.outcome = None;
            }
            if ui.button("Close").clicked() {
                self.closed = true;
            }
            if self.waiting {
                ui.spinner();
                ui.label("Saving…");
            }
            match &self.outcome {
                Some(Ok(done)) => {
                    ui.add(
                        egui::Label::new(format!(
                            "Saved {} in {}",
                            count(done.files.len(), "file", "files"),
                            done.path
                        ))
                        .truncate(),
                    )
                    .on_hover_text(done.path.as_str());
                }
                Some(Err(e)) => {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("Not saved: {e}")).color(theme::p().meter_red),
                        )
                        .truncate(),
                    )
                    .on_hover_text(e.as_str());
                }
                None => {}
            }
        });
    }

    /// What is ticked, in words.
    fn summary(&self) -> String {
        if self.picked.is_empty() {
            return "Nothing ticked".into();
        }
        let n = |f: fn(&Item) -> bool| self.picked.iter().filter(|i| f(i)).count();
        let mut parts = Vec::new();
        for (k, one, many) in [
            (n(|i| matches!(i, Item::Scene(_))), "scene", "scenes"),
            (n(|i| matches!(i, Item::Setup(_))), "setup", "setups"),
            (n(|i| matches!(i, Item::Hotkey(_))), "hotkey", "hotkeys"),
            (n(|i| matches!(i, Item::Sound(_))), "sound", "sounds"),
            (n(|i| matches!(i, Item::EqPreset(_))), "preset", "presets"),
            (n(|i| matches!(i, Item::AppRules)), "app rules", "app rules"),
            (
                n(|i| matches!(i, Item::Part(_))),
                "part of the preferences",
                "parts of the preferences",
            ),
        ] {
            if k > 0 {
                parts.push(if one == "app rules" {
                    one.to_string()
                } else {
                    count(k, one, many)
                });
            }
        }
        parts.join(", ")
    }

    /// Ask the daemon to write what is ticked to `path`, a `.zip`.
    fn export(&mut self, mut path: PathBuf, look: &Value, actions: &mut Vec<Request>) {
        if path
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("zip"))
        {
            path.as_mut_os_string().push(".zip");
        }
        let mut p = ExportParams {
            path: path.display().to_string(),
            ..Default::default()
        };
        for item in &self.picked {
            match item {
                Item::Scene(n) => p.scenes.push(n.clone()),
                Item::Setup(n) => p.setups.push(n.clone()),
                Item::Hotkey(id) => p.hotkeys.push(HotkeyKey::Id(*id)),
                Item::Sound(n) => p.sounds.push(n.clone()),
                Item::EqPreset(n) => p.eq_presets.push(n.clone()),
                Item::AppRules => p.app_rules = true,
                Item::Part(part) => p.preferences.push(*part),
            }
        }
        if p.preferences.contains(&PreferencePart::WindowLook) {
            p.window_look = Some(look.clone());
        }
        actions.push(Request::ExportSettings(p));
        self.waiting = true;
        self.outcome = None;
    }
}
