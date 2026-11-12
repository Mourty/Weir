//! Running from an AppImage. Nothing is installed, so Weir is not in the
//! application menu until something puts it there. The window offers once
//! to add it, and Preferences can add or remove it later. The entry is
//! also what the desktop's hotkeys service needs to know Weir by, off
//! Plasma. The entry names the AppImage file, and the window keeps it
//! naming the right one when the AppImage moves or a newer one replaces it.

use super::App;
use crate::theme;
use egui::{Frame, RichText, Ui};
use std::path::{Path, PathBuf};
use weir_protocol::appimage::AppImage;

/// The line that marks a menu entry as the one Weir wrote for an AppImage.
/// Weir only ever changes or removes an entry that has it.
const MARK: &str = "X-Weir-AppImage=true";

/// The installed entry, the model for this one.
const TEMPLATE: &str = include_str!("../../../../packaging/weir.desktop.in");
const ICON: &[u8] = include_bytes!("../../../../packaging/weir.svg");
const SYMBOLIC_ICON: &[u8] = include_bytes!("../../../../packaging/weir-symbolic.svg");

/// Whether the application menu has Weir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Menu {
    /// It does not.
    Missing,
    /// It has the entry Weir wrote for an AppImage.
    Ours,
    /// It has another: an installed Weir's, or one made by hand or by an
    /// AppImage manager. Weir leaves that alone.
    Other,
}

/// Where the entry and its icons go, under `~/.local/share`.
struct Places {
    entry: PathBuf,
    icon: PathBuf,
    symbolic_icon: PathBuf,
}

impl Places {
    fn here() -> Option<Places> {
        let data = dirs::data_dir()?;
        let icons = data.join("icons/hicolor");
        Some(Places {
            entry: data.join("applications/weir.desktop"),
            icon: icons.join("scalable/apps/weir.svg"),
            symbolic_icon: icons.join("symbolic/apps/weir-symbolic.svg"),
        })
    }
}

/// Every folder the desktop reads menu entries from, the person's own
/// first.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = dirs::data_dir().into_iter().collect();
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(
        system
            .split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
    );
    dirs
}

/// Whether the application menu has Weir now.
pub(super) fn menu_state() -> Menu {
    let own = Places::here().map(|p| p.entry);
    for dir in data_dirs() {
        let entry = dir.join("applications/weir.desktop");
        let Ok(text) = std::fs::read_to_string(&entry) else {
            continue;
        };
        let ours = Some(&entry) == own.as_ref() && text.lines().any(|l| l.trim() == MARK);
        return if ours { Menu::Ours } else { Menu::Other };
    }
    Menu::Missing
}

/// The menu entry that starts the AppImage `file`, or `None` for a file
/// name with a line break.
fn entry_for(file: &Path) -> Option<String> {
    let exec = exec_quote(file)?;
    let try_exec = string_value(file)?;
    let mut out = String::new();
    for line in TEMPLATE.lines() {
        if line.starts_with("Exec=") {
            out.push_str(&format!("Exec={exec}\n"));
            // Hides the entry if the AppImage is deleted.
            out.push_str(&format!("TryExec={try_exec}\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(MARK);
    out.push('\n');
    Some(out)
}

/// `path` as one word on a menu entry's `Exec` line: in double quotes,
/// with the characters the specification reserves inside them escaped,
/// `%` doubled, and then written as a string value, which doubles each
/// backslash again.
fn exec_quote(path: &Path) -> Option<String> {
    let text = path.to_str().filter(|t| !t.contains(['\n', '\r']))?;
    let mut inner = String::from("\"");
    for c in text.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                inner.push('\\');
                inner.push(c);
            }
            '%' => inner.push_str("%%"),
            c => inner.push(c),
        }
    }
    inner.push('"');
    Some(inner.replace('\\', "\\\\"))
}

/// `path` as a plain string value in a menu entry.
fn string_value(path: &Path) -> Option<String> {
    let text = path.to_str().filter(|t| !t.contains(['\n', '\r']))?;
    Some(text.replace('\\', "\\\\"))
}

/// Put Weir in the application menu, starting the AppImage `file`.
fn add(file: &Path) -> Result<(), String> {
    let places = Places::here().ok_or("there is no folder for menu entries")?;
    let entry = entry_for(file).ok_or_else(|| {
        format!(
            "{} has a line break in its name; rename the AppImage first",
            file.display()
        )
    })?;
    for (path, bytes) in [
        (&places.icon, ICON),
        (&places.symbolic_icon, SYMBOLIC_ICON),
        (&places.entry, entry.as_bytes()),
    ] {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not make {}: {e}", dir.display()))?;
        }
        std::fs::write(path, bytes)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Take Weir out of the application menu: the entry Weir wrote, and its
/// icons when they are Weir's own and nothing else in the menu needs them.
fn remove() -> Result<(), String> {
    let places = Places::here().ok_or("there is no folder for menu entries")?;
    if menu_state() != Menu::Ours {
        return Ok(());
    }
    std::fs::remove_file(&places.entry)
        .map_err(|e| format!("could not remove {}: {e}", places.entry.display()))?;
    if menu_state() == Menu::Missing {
        for (path, bytes) in [(&places.icon, ICON), (&places.symbolic_icon, SYMBOLIC_ICON)] {
            if std::fs::read(path).is_ok_and(|b| b == bytes) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    Ok(())
}

/// Point the entry Weir wrote at the AppImage `file`, if it names another:
/// the AppImage was moved, or a newer one with another name replaced it.
pub(super) fn follow(file: &Path) {
    let Some(places) = Places::here() else {
        return;
    };
    if menu_state() != Menu::Ours {
        return;
    }
    let current = std::fs::read_to_string(&places.entry).ok();
    if entry_for(file).is_some_and(|wanted| current.as_deref() != Some(wanted.as_str())) {
        match add(file) {
            Ok(()) => tracing::info!("the menu entry now starts {}", file.display()),
            Err(e) => tracing::warn!("{e}"),
        }
    }
}

impl App {
    /// The question asked once, under the top bar, when Weir runs from an
    /// AppImage that is not in the application menu.
    pub(super) fn appimage_question(&mut self, root: &mut egui::Ui) {
        let Some(appimage) = self.appimage.clone() else {
            return;
        };
        if self.menu != Menu::Missing || self.prefs.appimage_menu_asked {
            return;
        }
        egui::Panel::top("appimage_question")
            .frame(
                Frame::new()
                    .fill(theme::p().section_fill)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(root, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Add Weir to your application menu?");
                    ui.label(
                        RichText::new(
                            "It runs from an AppImage, so it isn't there yet. On desktops \
                             other than Plasma, hotkeys need it too.",
                        )
                        .color(theme::p().text_dim),
                    );
                    if ui.button("Add to the menu").clicked() {
                        self.add_to_menu(&appimage);
                        self.answer_menu_question();
                    }
                    if ui.button("No thanks").clicked() {
                        self.answer_menu_question();
                    }
                });
            });
    }

    fn answer_menu_question(&mut self) {
        self.prefs.appimage_menu_asked = true;
        self.prefs.save();
    }

    fn add_to_menu(&mut self, appimage: &AppImage) {
        match add(&appimage.file) {
            Ok(()) => self.show_toast("Weir is in your application menu", false),
            Err(e) => self.show_toast(&format!("Weir could not be added to the menu: {e}"), false),
        }
        self.menu = menu_state();
    }

    /// Preferences' AppImage box: which file Weir runs from, the menu entry,
    /// and weirctl.
    pub(super) fn appimage_prefs(&mut self, ui: &mut Ui) {
        let Some(appimage) = self.appimage.clone() else {
            return;
        };
        ui.label(format!("Weir runs from {}.", appimage.file.display()));
        ui.add_space(4.0);
        match self.menu {
            Menu::Ours => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("It's in your application menu.");
                    if ui.button("Remove from the menu").clicked() {
                        match remove() {
                            Ok(()) => {
                                self.show_toast("Weir is out of your application menu", false)
                            }
                            Err(e) => self.show_toast(&format!("Could not remove it: {e}"), false),
                        }
                        self.menu = menu_state();
                    }
                });
            }
            Menu::Missing => {
                if ui.button("Add to the application menu").clicked() {
                    self.add_to_menu(&appimage);
                }
            }
            Menu::Other => {
                ui.label(
                    RichText::new(
                        "Your application menu already has Weir, from another copy, so Weir \
                         leaves it as it is.",
                    )
                    .color(theme::p().text_dim),
                );
            }
        }
        ui.add_space(6.0);
        ui.label("To use weirctl in a terminal, link it to the AppImage once:");
        let link = format!(
            "mkdir -p ~/.local/bin && ln -sf '{}' ~/.local/bin/weirctl",
            appimage.file.display().to_string().replace('\'', r"'\''")
        );
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&link).monospace());
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(link.clone());
            }
        });
        ui.label(
            RichText::new("If you move the AppImage, run it again and make the link again.")
                .color(theme::p().text_dim),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_appimage_path_is_one_word_on_the_exec_line() {
        assert_eq!(
            exec_quote(Path::new("/home/me/Apps/Weir.AppImage")).as_deref(),
            Some(r#""/home/me/Apps/Weir.AppImage""#)
        );
        // Inside the quotes: \" \` \$ \\ and %% ; then each backslash
        // doubled for the string value.
        assert_eq!(
            exec_quote(Path::new(r#"/home/me/My "Apps"/100% $x`y`\Weir.AppImage"#)).as_deref(),
            Some(r#""/home/me/My \\"Apps\\"/100%% \\$x\\`y\\`\\\\Weir.AppImage""#)
        );
        assert_eq!(exec_quote(Path::new("/home/me/a\nb.AppImage")), None);
    }

    #[test]
    fn the_entry_starts_the_appimage_and_says_who_wrote_it() {
        let entry = entry_for(Path::new("/home/me/Apps/Weir-1.5.0-x86_64.AppImage")).unwrap();
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nExec=\"/home/me/Apps/Weir-1.5.0-x86_64.AppImage\"\n"));
        assert!(entry.contains("\nTryExec=/home/me/Apps/Weir-1.5.0-x86_64.AppImage\n"));
        assert!(entry.ends_with(&format!("\n{MARK}\n")));
        assert!(!entry.contains("@BINDIR@"));
        assert_eq!(entry.matches("\nExec=").count(), 1);
    }
}
