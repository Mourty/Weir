//! Scenes and setups: the menus that load, save over and delete them, and
//! the dialogs that name a new one or ask before replacing or deleting one.
//!
//! A scene keeps how it sounds: levels, routes and effects. A setup keeps
//! what is there: which strips and buses, their devices and external
//! effects. The two menus and dialogs are the same apart from the words,
//! which [`LibraryKind`] supplies, and the setups' list of scenes to load
//! with one.

use super::App;
use crate::theme;
use egui::{vec2, RichText, Ui};
use weir_protocol::*;

/// Width of a name in the Scenes and Setups menus, so every row lines up
/// whatever the names' lengths.
const LIBRARY_NAME_W: f32 = 160.0;

/// The two kinds of saved mixer state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LibraryKind {
    /// Levels, routes and effects.
    Scene,
    /// Devices, strips and buses.
    Setup,
}

impl LibraryKind {
    fn word(self) -> &'static str {
        match self {
            LibraryKind::Scene => "scene",
            LibraryKind::Setup => "setup",
        }
    }

    /// What saving one keeps, for the dialogs.
    fn keeps(self) -> &'static str {
        match self {
            LibraryKind::Scene => "the levels, mutes, routes and effects",
            LibraryKind::Setup => {
                "the strips and buses, their devices, names, layouts, colors and external \
                 effects"
            }
        }
    }

    fn load(self, name: String) -> Request {
        match self {
            LibraryKind::Scene => Request::LoadScene(NameParams { name }),
            LibraryKind::Setup => Request::LoadSetup(LoadSetupParams { name, scene: None }),
        }
    }

    fn save(self, name: String) -> Request {
        let params = NameParams { name };
        match self {
            LibraryKind::Scene => Request::SaveScene(params),
            LibraryKind::Setup => Request::SaveSetup(params),
        }
    }

    fn delete(self, name: String) -> Request {
        let params = NameParams { name };
        match self {
            LibraryKind::Scene => Request::DeleteScene(params),
            LibraryKind::Setup => Request::DeleteSetup(params),
        }
    }

    /// The saved names of this kind.
    fn names(self, library: &Library) -> &[String] {
        match self {
            LibraryKind::Scene => &library.scenes,
            LibraryKind::Setup => &library.setups,
        }
    }

    /// The one last loaded or saved.
    fn current(self, library: &Library) -> Option<&str> {
        match self {
            LibraryKind::Scene => library.scene.as_deref(),
            LibraryKind::Setup => library.setup.as_deref(),
        }
    }
}

/// Saving over or deleting a saved scene or setup, waiting for a yes.
#[derive(Clone)]
pub(super) struct LibraryConfirm {
    kind: LibraryKind,
    name: String,
    delete: bool,
}

/// What a scene would miss in the strips and buses `there`, by name.
fn scene_misses(library: &Library, scene: &str, there: &Members) -> Vec<(TargetKind, String)> {
    library
        .scene_members
        .get(scene)
        .map(|m| m.missing_in(there))
        .unwrap_or_default()
}

/// "Podcast and the bus Stream", for saying what a scene misses.
fn missing_words(missing: &[(TargetKind, String)]) -> String {
    let words: Vec<String> = missing
        .iter()
        .map(|(kind, name)| match kind {
            TargetKind::Strip => name.clone(),
            TargetKind::Bus => format!("the bus {name}"),
        })
        .collect();
    match words.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A yellow "2 missing" with what is missing on hover, after a scene that
/// mentions strips or buses `where_` lacks.
fn missing_note(ui: &mut Ui, missing: &[(TargetKind, String)], where_: &str) {
    if missing.is_empty() {
        return;
    }
    ui.label(
        RichText::new(format!("{} missing", missing.len()))
            .size(11.0)
            .color(theme::p().warning),
    )
    .on_hover_text(format!(
        "It has a mix for {}, which {where_} does not have. It loads anyway, without {}.",
        missing_words(missing),
        if missing.len() == 1 { "it" } else { "them" }
    ));
}

impl App {
    /// The Scenes or Setups menu: what it keeps, the saved ones with the
    /// current one ticked, and saving. Resting on a setup lists the scenes
    /// to load with it.
    pub(super) fn library_menu(&mut self, ui: &mut Ui, kind: LibraryKind, state: &FullState) {
        let library = &state.library;
        let (names, current) = (kind.names(library), kind.current(library));
        let what = match kind {
            LibraryKind::Scene => {
                "How it sounds: levels, routes and effects. Loading one never changes which \
                 devices are used; strips and buses it has no mix for start at their default, \
                 with nothing routed."
            }
            LibraryKind::Setup => {
                "What is there: strips, buses, their devices and external effects. Click one \
                 to load it with nothing routed, or rest on it to pick a scene to load with it."
            }
        };
        ui.set_max_width(300.0);
        ui.label(RichText::new(what).size(11.0).color(theme::p().text_dim));
        ui.separator();
        if names.is_empty() {
            ui.label(RichText::new(format!("no {}s yet", kind.word())).color(theme::p().text_dim));
        }
        let here = Members::of_mixer(&state.mixer);
        for n in names {
            let is_current = current == Some(n.as_str());
            match kind {
                LibraryKind::Scene => {
                    ui.horizontal(|ui| {
                        self.library_row(ui, kind, n, is_current);
                        missing_note(ui, &scene_misses(library, n, &here), "this mixer");
                    });
                }
                LibraryKind::Setup => {
                    let row = ui
                        .horizontal(|ui| {
                            self.library_row(ui, kind, n, is_current);
                            ui.label(RichText::new("›").color(theme::p().text_dim));
                        })
                        .response;
                    egui::containers::menu::SubMenu::new()
                        .show(ui, &row, |ui| self.scenes_for_setup(ui, n, library));
                }
            }
        }
        ui.separator();
        if ui.button(format!("Save {} as…", kind.word())).clicked() {
            self.save_as = Some((kind, current.unwrap_or_default().to_string()));
            self.focus_save_as = true;
            ui.close();
        }
    }

    /// The scenes to load with setup `setup`, each with what it would miss
    /// there.
    fn scenes_for_setup(&mut self, ui: &mut Ui, setup: &str, library: &Library) {
        ui.set_min_width(200.0);
        ui.label(
            RichText::new(format!("Load {setup} with a scene"))
                .size(11.0)
                .color(theme::p().text_dim),
        );
        if library.scenes.is_empty() {
            ui.label(
                RichText::new("No scenes yet. Save one from the Scenes menu.")
                    .color(theme::p().text_dim),
            );
        }
        let there = library
            .setup_members
            .get(setup)
            .cloned()
            .unwrap_or_default();
        for scene in &library.scenes {
            let missing = scene_misses(library, scene, &there);
            ui.horizontal(|ui| {
                let load = ui.add(egui::Button::new(scene.as_str()).truncate());
                missing_note(ui, &missing, "this setup");
                if load.clicked() {
                    self.actions.push(Request::LoadSetup(LoadSetupParams {
                        name: setup.to_string(),
                        scene: Some(scene.clone()),
                    }));
                    let without = if missing.is_empty() {
                        String::new()
                    } else {
                        format!(", without {}", missing_words(&missing))
                    };
                    self.show_toast(
                        &format!("Loaded setup {setup} with scene {scene}{without}"),
                        true,
                    );
                    ui.close();
                }
            });
        }
    }

    /// One saved scene or setup: its name, which loads it, then buttons to
    /// save over it and delete it.
    fn library_row(&mut self, ui: &mut Ui, kind: LibraryKind, n: &str, is_current: bool) {
        // Every name gets the same width, cut short with "…" when it is
        // longer, so the buttons after it line up. The filler after the name
        // keeps it at the left.
        let size = vec2(LIBRARY_NAME_W, ui.spacing().interact_size.y);
        let load = ui
            .allocate_ui(size, |ui| {
                ui.add(
                    egui::Button::selectable(is_current, (n, egui::Atom::grow()))
                        .truncate()
                        .min_size(size),
                )
            })
            .inner;
        let word = kind.word();
        let click = match kind {
            LibraryKind::Scene => format!("Click to load this {word}."),
            LibraryKind::Setup => {
                "Click to load it with nothing routed, or rest here to pick a scene to load with \
                 it."
                .into()
            }
        };
        let tip = if is_current {
            format!("{n}\n\nThe {word} last loaded or saved. {click}")
        } else {
            format!("{n}\n\n{click}")
        };
        if load.on_hover_text(tip).clicked() {
            self.actions.push(kind.load(n.to_string()));
            match kind {
                LibraryKind::Scene => self.show_toast(&format!("Loaded scene {n}"), false),
                // Easy to regret: the mix goes.
                LibraryKind::Setup => {
                    self.show_toast(&format!("Loaded setup {n} with nothing routed"), true)
                }
            }
            ui.close();
        }
        let confirm = |delete| LibraryConfirm {
            kind,
            name: n.to_string(),
            delete,
        };
        if ui
            .small_button("save")
            .on_hover_text(format!("Replace this {word} with how things are now"))
            .clicked()
        {
            self.confirm_library = Some(confirm(false));
            ui.close();
        }
        if ui
            .small_button("export")
            .on_hover_text(format!(
                "Save this {word} to a file, to share it or keep it safe"
            ))
            .clicked()
        {
            let one = match kind {
                LibraryKind::Scene => super::transfer::One::Scene(n.to_string()),
                LibraryKind::Setup => super::transfer::One::Setup(n.to_string()),
            };
            self.export_one(ui.ctx(), one);
            ui.close();
        }
        if ui
            .small_button("delete")
            .on_hover_text(format!("Delete this {word}"))
            .clicked()
        {
            self.confirm_library = Some(confirm(true));
            ui.close();
        }
    }

    /// The "save as" dialog and the confirmation, when either is open.
    pub(super) fn library_dialogs(&mut self, ctx: &egui::Context, state: &FullState) {
        self.save_as_dialog(ctx, state);
        self.confirm_library_dialog(ctx);
    }

    /// Naming a new scene or setup. The name is checked as it is typed,
    /// with the same rule the daemon uses, so a name that cannot be a file
    /// is caught before saving.
    fn save_as_dialog(&mut self, ctx: &egui::Context, state: &FullState) {
        let Some((kind, name)) = self.save_as.as_mut() else {
            return;
        };
        let kind = *kind;
        let mut open = true;
        let mut submit = false;
        let mut close = false;
        let problem = library_name_problem(name.trim());
        let replaces = kind.names(&state.library).iter().any(|n| n == name.trim());
        egui::Window::new(format!("Save {}", kind.word()))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_max_width(320.0);
                ui.label(format!("A {} keeps {}.", kind.word(), kind.keeps()));
                ui.add_space(4.0);
                let r = ui.add(
                    egui::TextEdit::singleline(name)
                        .char_limit(LIBRARY_NAME_MAX)
                        .hint_text("Name")
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut self.focus_save_as) {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                // Reserve the line either way, so the buttons do not jump
                // as the note comes and goes.
                let (note, color) = match problem {
                    // An empty field needs no scolding; the Save button
                    // being gray says enough.
                    Some(_) if name.trim().is_empty() => (String::new(), theme::p().text_dim),
                    Some(p) => (p.to_string(), theme::p().meter_red),
                    None if replaces => (
                        format!("Replaces the {} saved under this name.", kind.word()),
                        theme::p().warning,
                    ),
                    None => (String::new(), theme::p().text_dim),
                };
                ui.label(
                    RichText::new(if note.is_empty() { " " } else { &note })
                        .size(11.0)
                        .color(color),
                );
                ui.horizontal(|ui| {
                    let label = if replaces { "Replace" } else { "Save" };
                    if ui
                        .add_enabled(problem.is_none(), egui::Button::new(label))
                        .clicked()
                    {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if submit && problem.is_none() {
            self.actions.push(kind.save(name.trim().to_string()));
            close = true;
        }
        if !open || close {
            self.save_as = None;
        }
    }

    /// Asking before saving over, or deleting, a scene or setup.
    fn confirm_library_dialog(&mut self, ctx: &egui::Context) {
        let Some(c) = self.confirm_library.clone() else {
            return;
        };
        let mut open = true;
        let mut done = false;
        let word = c.kind.word();
        let title = if c.delete {
            format!("Delete {word}?")
        } else {
            format!("Save over {word}?")
        };
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_max_width(320.0);
                if c.delete {
                    ui.label(format!("Delete the {word} \"{}\"?", c.name));
                    ui.label(
                        RichText::new("It cannot be brought back.").color(theme::p().text_dim),
                    );
                } else {
                    ui.label(format!(
                        "Replace the {word} \"{}\" with how things are now?",
                        c.name
                    ));
                    ui.label(
                        RichText::new(format!(
                            "It will keep {} as they are at the moment.",
                            c.kind.keeps()
                        ))
                        .color(theme::p().text_dim),
                    );
                }
                ui.horizontal(|ui| {
                    let yes = if c.delete {
                        RichText::new("Delete").color(theme::p().meter_red)
                    } else {
                        RichText::new("Save over")
                    };
                    if ui.button(yes).clicked() {
                        if c.delete {
                            self.actions.push(c.kind.delete(c.name.clone()));
                            self.show_toast(&format!("Deleted {word} {}", c.name), false);
                        } else {
                            self.actions.push(c.kind.save(c.name.clone()));
                            self.show_toast(&format!("Saved {word} {}", c.name), false);
                        }
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        done = true;
                    }
                });
            });
        if !open || done {
            self.confirm_library = None;
        }
    }
}
