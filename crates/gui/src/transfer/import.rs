//! The Import window: what a file holds, in a section for each kind, with
//! what each thing would meet here (a name already taken, keys another
//! hotkey has, a strip this mixer lacks, a damaged file) and the choices
//! that deal with it, then what the import did.

use super::{count, item_row, section, window, Pane};
use crate::client::Transfer;
use crate::file_dialog::{Dialog, Picked};
use crate::theme;
use egui::{RichText, Ui};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use weir_protocol::*;

/// What to do with an item whose name is taken here.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Taken {
    Replace,
    KeepBoth(String),
    Skip,
}

/// A file read, and the choices made about it so far.
struct Shown {
    seen: ImportInspection,
    /// The items ticked, by id.
    picked: BTreeSet<String>,
    /// For items whose name is taken.
    taken: BTreeMap<String, Taken>,
    hotkeys: HotkeyImport,
    /// For each strip and bus the file names that this mixer lacks, the
    /// one of this mixer's to use, if any.
    maps: BTreeMap<MissingTarget, Option<String>>,
    /// Why the import failed as a whole, if it did.
    error: Option<String>,
}

/// Where the window is.
enum Stage {
    /// Waiting for a file to be picked.
    Choosing,
    /// Reading the file picked.
    Reading(String),
    /// The file could not be read, and why.
    Failed(String, String),
    Shown(Box<Shown>),
    Importing(Box<Shown>),
    Done(ImportResult),
}

/// The Import window.
pub struct ImportWindow {
    pub raise: bool,
    pub closed: bool,
    stage: Stage,
    dialog: Option<Dialog>,
    /// The path being typed, where there is no file dialog.
    typed: Option<String>,
}

impl ImportWindow {
    /// Open the window, asking for a file straight away.
    pub fn new(ctx: &egui::Context) -> Self {
        Self {
            raise: false,
            closed: false,
            stage: Stage::Choosing,
            dialog: Some(open_dialog(ctx)),
            typed: None,
        }
    }

    /// The daemon's answer to reading or importing the file.
    pub fn reply(&mut self, kind: Transfer, answer: Result<Value, String>) {
        let stage = std::mem::replace(&mut self.stage, Stage::Choosing);
        self.stage = match (kind, stage, answer) {
            (Transfer::Inspect, Stage::Reading(path), Ok(v)) => {
                match serde_json::from_value::<ImportInspection>(v) {
                    Ok(seen) => Stage::Shown(Box::new(Shown::new(seen))),
                    Err(e) => Stage::Failed(path, format!("the answer made no sense: {e}")),
                }
            }
            (Transfer::Inspect, Stage::Reading(path), Err(e)) => Stage::Failed(path, e),
            (Transfer::Import, Stage::Importing(_), Ok(v)) => match serde_json::from_value(v) {
                Ok(result) => Stage::Done(result),
                Err(e) => Stage::Failed(String::new(), format!("the answer made no sense: {e}")),
            },
            (Transfer::Import, Stage::Importing(mut shown), Err(e)) => {
                shown.error = Some(e);
                Stage::Shown(shown)
            }
            (_, stage, _) => stage,
        };
    }

    /// Draw the window. Requests for the daemon go into `actions`.
    pub fn show(&mut self, ctx: &egui::Context, state: &FullState, actions: &mut Vec<Request>) {
        if let Some(picked) = self.dialog.as_ref().and_then(Dialog::poll) {
            self.dialog = None;
            match picked {
                Picked::File(path) => self.read(path, actions),
                // Closing the dialog before any file was read closes the
                // window: there is nothing in it.
                Picked::Cancelled => {
                    if matches!(self.stage, Stage::Choosing) {
                        self.closed = true;
                    }
                }
                Picked::NoDialog(_) => {
                    self.typed = Some(std::env::var("HOME").unwrap_or_default() + "/")
                }
            }
        }
        let (mut raise, mut closed) = (self.raise, self.closed);
        window(
            ctx,
            "import",
            "Import settings",
            &mut raise,
            &mut closed,
            |ui, pane| match pane {
                Pane::Body => self.body(ui, state),
                Pane::Footer => self.footer(ui, ctx, state, actions),
            },
        );
        (self.raise, self.closed) = (raise, closed);
    }

    /// Read the file at `path`.
    fn read(&mut self, path: PathBuf, actions: &mut Vec<Request>) {
        let path = path.display().to_string();
        actions.push(Request::InspectImport(InspectImportParams {
            path: path.clone(),
        }));
        self.stage = Stage::Reading(path);
    }

    fn body(&mut self, ui: &mut Ui, state: &FullState) {
        if let Some(typed) = &mut self.typed {
            ui.label("Your desktop has no file dialog Weir can use. Type the file's path:");
            ui.add(egui::TextEdit::singleline(typed).desired_width(f32::INFINITY));
            ui.add_space(8.0);
        }
        match &mut self.stage {
            Stage::Choosing => {
                ui.label("Pick a .zip or .json that Weir exported.");
            }
            Stage::Reading(path) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Reading {path}…"));
                });
            }
            Stage::Failed(path, reason) => {
                if !path.is_empty() {
                    ui.label(RichText::new(path.as_str()).strong());
                }
                ui.label(
                    RichText::new(format!("This cannot be imported: {reason}"))
                        .color(theme::p().meter_red),
                );
            }
            Stage::Shown(shown) => shown.body(ui, state),
            Stage::Importing(shown) => {
                ui.add_enabled_ui(false, |ui| shown.body(ui, state));
            }
            Stage::Done(result) => done(ui, result),
        }
    }

    fn footer(
        &mut self,
        ui: &mut Ui,
        ctx: &egui::Context,
        state: &FullState,
        actions: &mut Vec<Request>,
    ) {
        ui.horizontal(|ui| {
            if let Some(typed) = &self.typed {
                if ui.button("Open").clicked() {
                    let path = PathBuf::from(typed.trim());
                    self.typed = None;
                    self.read(path, actions);
                }
            }
            let mut start = None;
            match &self.stage {
                Stage::Shown(shown) => {
                    let n = shown.importable(state).len();
                    let label = format!("Import {}", count(n, "thing", "things"));
                    let button =
                        egui::Button::new(RichText::new(label).strong().color(theme::p().on_text))
                            .fill(theme::p().accent);
                    if ui.add_enabled(n > 0, button).clicked() {
                        start = Some(shown.params(state));
                    }
                    if ui.button("Cancel").clicked() {
                        self.closed = true;
                    }
                    if let Some(e) = &shown.error {
                        ui.label(
                            RichText::new(format!("Nothing was imported: {e}"))
                                .color(theme::p().meter_red),
                        );
                    }
                }
                Stage::Importing(_) => {
                    ui.spinner();
                    ui.label("Importing…");
                }
                Stage::Done(_) => {
                    if ui.button("Done").clicked() {
                        self.closed = true;
                    }
                }
                _ => {
                    if ui.button("Cancel").clicked() {
                        self.closed = true;
                    }
                }
            }
            // Somewhere to pick another file, whatever happened to this one.
            let busy = matches!(
                self.stage,
                Stage::Done(_) | Stage::Reading(_) | Stage::Importing(_)
            );
            if !busy && self.dialog.is_none() && self.typed.is_none() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Choose another file…").clicked() {
                        self.dialog = Some(open_dialog(ctx));
                    }
                });
            }
            if let Some(params) = start {
                actions.push(Request::ImportSettings(params));
                if let Stage::Shown(shown) = std::mem::replace(&mut self.stage, Stage::Choosing) {
                    self.stage = Stage::Importing(shown);
                }
            }
        });
    }
}

/// The dialog for a file to import.
fn open_dialog(ctx: &egui::Context) -> Dialog {
    Dialog::open(
        ctx,
        "Import settings",
        &[("Weir settings", &["*.zip", "*.json"])],
    )
}

/// What the import did.
fn done(ui: &mut Ui, r: &ImportResult) {
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            if r.imported.is_empty() {
                ui.label(RichText::new("Nothing was imported.").strong());
            } else {
                ui.label(RichText::new("Imported").strong().size(15.0));
                for i in &r.imported {
                    ui.label(format!("• {}", capital(i)));
                }
            }
            if !r.skipped.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new("Left out").strong().size(15.0));
                for s in &r.skipped {
                    ui.label(RichText::new(format!("• {}", capital(s))).color(theme::p().warning));
                }
            }
            if !r.notes.is_empty() {
                ui.add_space(8.0);
                for n in &r.notes {
                    ui.label(n.as_str());
                }
            }
            if let Some(dir) = &r.backup {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("Copies of what was replaced are in {dir}"))
                        .size(12.0)
                        .color(theme::p().text_dim),
                );
            }
        });
}

/// `s` with a capital first letter.
fn capital(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

impl Shown {
    fn new(seen: ImportInspection) -> Self {
        let picked = seen
            .items
            .iter()
            .filter(|i| i.broken.is_none())
            .map(|i| i.id.clone())
            .collect();
        // Keeping both is the choice that loses nothing.
        let taken = seen
            .items
            .iter()
            .filter(|i| i.taken)
            .map(|i| {
                let name = i.free_name.clone().unwrap_or_else(|| i.name.clone());
                (i.id.clone(), Taken::KeepBoth(name))
            })
            .collect();
        let maps = seen.missing.iter().map(|m| (m.clone(), None)).collect();
        Self {
            seen,
            picked,
            taken,
            hotkeys: HotkeyImport::Add,
            maps,
            error: None,
        }
    }

    /// Whether `item`'s name being taken matters: not for hotkeys that
    /// replace all of these.
    fn taken_matters(&self, item: &ImportItem) -> bool {
        item.taken && !(item.kind == ExportKind::Hotkey && self.hotkeys == HotkeyImport::ReplaceAll)
    }

    /// Why `item` cannot come in as things stand, if it cannot.
    fn blocked(&self, item: &ImportItem, state: &FullState) -> Option<String> {
        if let Some(reason) = &item.broken {
            return Some(format!("Cannot be imported: {reason}"));
        }
        let unmapped: Vec<String> = item
            .missing
            .iter()
            .filter(|m| self.maps.get(*m).is_none_or(Option::is_none))
            .map(|m| format!("{} '{}'", word(m.kind), m.name))
            .collect();
        if !unmapped.is_empty() {
            return Some(format!(
                "Uses the {}, which this mixer does not have: pick one of yours above.",
                unmapped.join(" and the ")
            ));
        }
        if self.taken_matters(item) {
            if let Some(Taken::KeepBoth(name)) = self.taken.get(&item.id) {
                return name_problem(item.kind, name, state);
            }
        }
        None
    }

    /// The items that would be imported, by id.
    fn importable(&self, state: &FullState) -> Vec<String> {
        self.seen
            .items
            .iter()
            .filter(|i| self.picked.contains(&i.id))
            .filter(|i| self.blocked(i, state).is_none())
            .filter(|i| !(self.taken_matters(i) && self.taken.get(&i.id) == Some(&Taken::Skip)))
            .map(|i| i.id.clone())
            .collect()
    }

    /// The request that imports what is ticked, as chosen.
    fn params(&self, state: &FullState) -> ImportParams {
        let items = self.importable(state);
        let choices = items
            .iter()
            .filter_map(|id| {
                let item = self.seen.items.iter().find(|i| &i.id == id)?;
                if !self.taken_matters(item) {
                    return None;
                }
                Some((
                    id.clone(),
                    match self.taken.get(id)? {
                        Taken::Replace => ImportChoice::Replace,
                        Taken::KeepBoth(name) => ImportChoice::Rename(name.trim().to_string()),
                        Taken::Skip => ImportChoice::Skip,
                    },
                ))
            })
            .collect();
        let map = |kind: TargetKind| {
            self.maps
                .iter()
                .filter(|(m, to)| m.kind == kind && to.is_some())
                .map(|(m, to)| (m.name.clone(), to.clone().unwrap_or_default()))
                .collect()
        };
        ImportParams {
            path: self.seen.path.clone(),
            items: Some(items),
            choices,
            when_taken: WhenTaken::Skip,
            hotkeys: self.hotkeys,
            map_strips: map(TargetKind::Strip),
            map_buses: map(TargetKind::Bus),
        }
    }

    fn body(&mut self, ui: &mut Ui, state: &FullState) {
        let file = std::path::Path::new(&self.seen.path)
            .file_name()
            .map_or_else(
                || self.seen.path.clone(),
                |n| n.to_string_lossy().into_owned(),
            );
        ui.label(RichText::new(file).strong().size(16.0))
            .on_hover_text(self.seen.path.as_str());
        let mut from = String::from("Exported");
        if !self.seen.weir_version.is_empty() {
            from.push_str(&format!(" by Weir {}", self.seen.weir_version));
        }
        if let Some(when) = &self.seen.exported {
            from.push_str(&format!(" on {}", when_words(when)));
        }
        ui.label(RichText::new(from).size(12.0).color(theme::p().text_dim));
        for p in &self.seen.problems {
            ui.label(RichText::new(format!("Not read: {p}")).color(theme::p().warning));
        }
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                self.missing(ui, state);
                ui.horizontal(|ui| {
                    if ui.button("Select all").clicked() {
                        self.picked = self.seen.items.iter().map(|i| i.id.clone()).collect();
                    }
                    if ui.button("Select none").clicked() {
                        self.picked.clear();
                    }
                });
                ui.add_space(6.0);
                for kind in ExportKind::ALL {
                    let ids: Vec<String> = self
                        .seen
                        .items
                        .iter()
                        .filter(|i| i.kind == kind)
                        .map(|i| i.id.clone())
                        .collect();
                    if ids.is_empty() {
                        continue;
                    }
                    let ticked = ids.iter().filter(|id| self.picked.contains(*id)).count();
                    let id = egui::Id::new(("import-section", kind));
                    let asked = section(ui, id, kind.heading(), ticked, ids.len(), |ui| {
                        if kind == ExportKind::Hotkey {
                            self.hotkey_mode(ui, state);
                        }
                        for id in &ids {
                            self.item(ui, id, state);
                        }
                    });
                    match asked {
                        Some(true) => self.picked.extend(ids.iter().cloned()),
                        Some(false) => {
                            for id in &ids {
                                self.picked.remove(id);
                            }
                        }
                        None => {}
                    }
                }
            });
    }

    /// The strips and buses the file names that this mixer lacks, each with
    /// a pick of this mixer's own to use instead.
    fn missing(&mut self, ui: &mut Ui, state: &FullState) {
        if self.maps.is_empty() {
            return;
        }
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0_f32, theme::p().warning))
            .corner_radius(6)
            .inner_margin(10)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(
                        "Some of this uses strips or buses this mixer does not have. Pick one \
                         of yours to use for each, or leave what uses it out.",
                    )
                    .color(theme::p().warning),
                );
                ui.add_space(4.0);
                egui::Grid::new("import-maps")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        for (m, to) in self.maps.iter_mut() {
                            ui.label(format!("The {} '{}'", word(m.kind), m.name));
                            let names: Vec<&str> = match m.kind {
                                TargetKind::Strip => {
                                    state.mixer.strips.iter().map(|s| s.name.as_str()).collect()
                                }
                                TargetKind::Bus => {
                                    state.mixer.buses.iter().map(|b| b.name.as_str()).collect()
                                }
                            };
                            let shown = to.clone().unwrap_or_else(|| "Leave out".into());
                            egui::ComboBox::from_id_salt(("map", m.kind, m.name.as_str()))
                                .selected_text(shown)
                                .width(200.0)
                                .truncate()
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(to, None, "Leave out");
                                    for n in names {
                                        ui.selectable_value(to, Some(n.to_string()), n);
                                    }
                                });
                            ui.end_row();
                        }
                    });
            });
        ui.add_space(8.0);
    }

    /// Add the hotkeys to these, or replace all of these.
    fn hotkey_mode(&mut self, ui: &mut Ui, state: &FullState) {
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.hotkeys, HotkeyImport::Add, "Add to my hotkeys");
            ui.radio_value(
                &mut self.hotkeys,
                HotkeyImport::ReplaceAll,
                "Replace all my hotkeys",
            );
        });
        if self.hotkeys == HotkeyImport::ReplaceAll {
            let mine = state.hotkeys.hotkeys.len();
            ui.label(
                RichText::new(format!(
                    "Your {} and their groups are removed first, and the ticked ones take \
                     their place, in their order.",
                    count(mine, "hotkey", "hotkeys")
                ))
                .size(12.0)
                .color(theme::p().warning),
            );
        }
        ui.add_space(4.0);
    }

    /// One item: its box, name and what it is, and below them anything in
    /// the way of importing it and the choices that deal with it.
    fn item(&mut self, ui: &mut Ui, id: &str, state: &FullState) {
        let Some(item) = self.seen.items.iter().find(|i| i.id == id).cloned() else {
            return;
        };
        let mut detail = item.summary.clone();
        if let Some(g) = &item.group {
            detail = format!("{detail}  · in {g}");
        }
        let mut on = self.picked.contains(id);
        if item_row(ui, &mut on, item.broken.is_none(), &item.name, &detail) {
            if on {
                self.picked.insert(id.to_string());
            } else {
                self.picked.remove(id);
            }
        }
        // Why it cannot come in shows whether ticked or not: it cannot be.
        if let Some(reason) = &item.broken {
            ui.indent(("import-item", id), |ui| {
                ui.label(
                    RichText::new(format!("Cannot be imported: {reason}"))
                        .size(12.0)
                        .color(theme::p().meter_red),
                );
            });
            return;
        }
        if !on {
            return;
        }
        ui.indent(("import-item", id), |ui| {
            let amber = theme::p().warning;
            if self.taken_matters(&item) {
                self.taken_choice(ui, &item, state);
            }
            let replacing = self.taken.get(id) == Some(&Taken::Replace);
            if item.kind == ExportKind::Hotkey && self.hotkeys == HotkeyImport::Add {
                for k in &item.keys_taken {
                    // Replacing the hotkey that has them gives them back.
                    if replacing && k.by.eq_ignore_ascii_case(&item.name) {
                        continue;
                    }
                    ui.label(
                        RichText::new(format!(
                            "{}: your hotkey '{}' has these keys, so this one comes in \
                             without them.",
                            k.keys, k.by
                        ))
                        .size(12.0)
                        .color(amber),
                    );
                }
            }
            if let Some(why) = self.blocked(&item, state) {
                if item
                    .missing
                    .iter()
                    .any(|m| self.maps.get(m).is_none_or(Option::is_none))
                {
                    ui.label(RichText::new(why).size(12.0).color(amber));
                }
            }
            if let Some(note) = &item.note {
                ui.label(
                    RichText::new(note.as_str())
                        .size(12.0)
                        .color(theme::p().text_dim),
                );
            }
        });
    }

    /// The choice for an item whose name is taken: replace the one here,
    /// keep both under another name, or skip it.
    fn taken_choice(&mut self, ui: &mut Ui, item: &ImportItem, state: &FullState) {
        ui.label(
            RichText::new(format!(
                "You have a {} called '{}'.",
                item.kind.word(),
                item.name
            ))
            .color(theme::p().warning),
        );
        let free = item.free_name.clone().unwrap_or_else(|| item.name.clone());
        let choice = self
            .taken
            .entry(item.id.clone())
            .or_insert_with(|| Taken::KeepBoth(free.clone()));
        let builtin = item.kind == ExportKind::EqPreset
            && state
                .eq_presets
                .iter()
                .any(|p| p.builtin && p.name.eq_ignore_ascii_case(&item.name));
        ui.horizontal(|ui| {
            let keeping = matches!(choice, Taken::KeepBoth(_));
            if ui.radio(keeping, "Keep both, this one as").clicked() && !keeping {
                *choice = Taken::KeepBoth(free.clone());
            }
            let mut name = match choice {
                Taken::KeepBoth(n) => n.clone(),
                _ => free.clone(),
            };
            let field = ui.add_enabled(
                keeping,
                egui::TextEdit::singleline(&mut name).desired_width(180.0),
            );
            if field.changed() {
                *choice = Taken::KeepBoth(name);
            }
            // A preset that comes with Weir cannot be replaced.
            if !builtin
                && ui
                    .radio(*choice == Taken::Replace, "Replace mine")
                    .clicked()
            {
                *choice = Taken::Replace;
            }
            if ui.radio(*choice == Taken::Skip, "Skip").clicked() {
                *choice = Taken::Skip;
            }
        });
        if let Taken::KeepBoth(name) = choice.clone() {
            if let Some(problem) = name_problem(item.kind, &name, state) {
                ui.label(
                    RichText::new(problem)
                        .size(12.0)
                        .color(theme::p().meter_red),
                );
            }
        }
    }
}

/// "strip" or "bus".
fn word(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Strip => "strip",
        TargetKind::Bus => "bus",
    }
}

/// What is wrong with `name` for a new thing of `kind` here, if anything.
fn name_problem(kind: ExportKind, name: &str, state: &FullState) -> Option<String> {
    let name = name.trim();
    let problem = match kind {
        ExportKind::Scene | ExportKind::Setup => library_name_problem(name).map(str::to_string),
        _ if name.is_empty() => Some("Type a name".into()),
        _ => None,
    };
    if problem.is_some() {
        return problem;
    }
    let taken = match kind {
        ExportKind::Scene => state.library.scenes.iter().any(|n| n == name),
        ExportKind::Setup => state.library.setups.iter().any(|n| n == name),
        ExportKind::Hotkey => state
            .hotkeys
            .hotkeys
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case(name)),
        ExportKind::EqPreset => state
            .eq_presets
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(name)),
        _ => false,
    };
    taken.then(|| format!("You have a {} called that too.", kind.word()))
}

/// A stamp such as `2026-10-10T14-30-05Z` as `2026-10-10 14:30 UTC`.
fn when_words(stamp: &str) -> String {
    match stamp.split_once('T') {
        Some((day, time)) if time.len() >= 5 => {
            format!("{day} {}:{} UTC", &time[0..2], &time[3..5])
        }
        _ => stamp.to_string(),
    }
}
