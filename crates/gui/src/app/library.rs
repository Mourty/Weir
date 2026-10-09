//! Scenes and setups: the menus that load, save over and delete them, and
//! the dialogs that name a new one or ask before replacing or deleting one.
//!
//! A scene keeps levels, routes and effects; a setup keeps which strips and
//! buses there are and their devices. The two menus and dialogs are the same
//! apart from the words, which [`LibraryKind`] supplies.

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
            LibraryKind::Setup => "the strips and buses, their devices, names, layouts and colors",
        }
    }

    fn load(self, name: String) -> Request {
        let params = NameParams { name };
        match self {
            LibraryKind::Scene => Request::LoadScene(params),
            LibraryKind::Setup => Request::LoadSetup(params),
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

impl App {
    /// The Scenes or Setups menu: what it keeps, the saved ones with the
    /// current one ticked, and saving.
    pub(super) fn library_menu(&mut self, ui: &mut Ui, kind: LibraryKind, library: &Library) {
        let (names, current) = (kind.names(library), kind.current(library));
        let what = match kind {
            LibraryKind::Scene => {
                "Levels, routes and effects. Loading one never changes which devices are used."
            }
            LibraryKind::Setup => {
                "Devices, strips and buses. Switching keeps the levels and effects of the \
                 strips and buses that stay."
            }
        };
        ui.set_max_width(280.0);
        ui.label(RichText::new(what).size(11.0).color(theme::p().text_dim));
        ui.separator();
        if names.is_empty() {
            ui.label(RichText::new(format!("no {}s yet", kind.word())).color(theme::p().text_dim));
        }
        for n in names {
            ui.horizontal(|ui| self.library_row(ui, kind, n, current == Some(n.as_str())));
        }
        ui.separator();
        if ui.button(format!("Save {} as…", kind.word())).clicked() {
            self.save_as = Some((kind, current.unwrap_or_default().to_string()));
            self.focus_save_as = true;
            ui.close();
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
        let tip = if is_current {
            format!("{n}\n\nThe {word} last loaded or saved. Click to load it again.")
        } else {
            format!("{n}\n\nClick to load this {word}.")
        };
        if load.on_hover_text(tip).clicked() {
            self.actions.push(kind.load(n.to_string()));
            self.show_toast(&format!("Loaded {word} {n}"), false);
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
