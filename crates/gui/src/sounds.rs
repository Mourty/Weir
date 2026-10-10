//! Picking the sounds hotkeys play, shared by the hotkey editor and
//! Preferences: a list of Weir's sounds and the person's own, with a way
//! to hear one and to add another from a file.

use crate::theme;
use egui::{RichText, Ui};
use std::path::Path;
use weir_protocol::*;

/// Which of a hotkey's sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundSlot {
    Press,
    Release,
    Repeat,
}

impl SoundSlot {
    /// That sound of `sounds`.
    pub fn of(self, sounds: &mut HotkeySounds) -> &mut Option<String> {
        match self {
            SoundSlot::Press => &mut sounds.press,
            SoundSlot::Release => &mut sounds.release,
            SoundSlot::Repeat => &mut sounds.repeat,
        }
    }
}

/// The file kinds Weir plays, for the file dialog.
pub const FILTERS: &[(&str, &[&str])] = &[("Sounds", &["*.wav", "*.ogg", "*.flac"])];

/// A name for a sound from the file at `path`: its name without the
/// extension, kept to what sound names allow, and numbered if `taken`.
pub fn name_from_file(path: &Path, taken: impl Fn(&str) -> bool) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-' | '.') {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let clean = clean.trim_start_matches('.').trim();
    let clean: String = clean.chars().take(LIBRARY_NAME_MAX - 3).collect();
    let base = if clean.is_empty() {
        "Sound".to_string()
    } else {
        clean
    };
    let builtin = |n: &str| BUILTIN_SOUNDS.iter().any(|b| b.eq_ignore_ascii_case(n));
    free_name(&base, LIBRARY_NAME_MAX, |n| taken(n) || builtin(n))
}

/// What the person did with a sound picker.
pub enum Picked {
    /// Nothing, or picked a sound.
    Nothing,
    /// Asked to add a sound from a file.
    Add,
}

/// A list to pick one of the sounds in `sounds`, or none, with a button to
/// hear it, which sends `play_sound` into `actions`.
pub fn picker(
    ui: &mut Ui,
    salt: &str,
    current: &mut Option<String>,
    sounds: &[SoundInfo],
    actions: &mut Vec<Request>,
) -> Picked {
    let mut asked = Picked::Nothing;
    let known = current
        .as_deref()
        .is_none_or(|c| sounds.iter().any(|s| s.name == c));
    let text = match current.as_deref() {
        None => RichText::new("None"),
        Some(c) if known => RichText::new(c),
        Some(c) => RichText::new(format!("{c} (gone)")).color(theme::p().warning),
    };
    egui::ComboBox::from_id_salt(("sound", salt))
        .selected_text(text)
        .width(170.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(current, None, "None");
            let (own, weirs): (Vec<&SoundInfo>, Vec<&SoundInfo>) =
                sounds.iter().partition(|s| !s.builtin);
            for s in weirs.into_iter().chain(own) {
                ui.selectable_value(current, Some(s.name.clone()), &s.name);
            }
            ui.separator();
            if ui
                .selectable_label(false, "Add a sound…")
                .on_hover_text("A .wav, .ogg or .flac file of your own, 10 seconds at most")
                .clicked()
            {
                asked = Picked::Add;
            }
        });
    if let Some(name) = current.as_ref().filter(|_| known) {
        if ui
            .small_button("Play")
            .on_hover_text("Hear it where hotkeys' sounds play")
            .clicked()
        {
            actions.push(Request::PlaySound(NameParams { name: name.clone() }));
        }
    }
    asked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_name_makes_a_sound_name() {
        let none = |_: &str| false;
        assert_eq!(
            name_from_file(Path::new("/x/air horn!.wav"), none),
            "air horn"
        );
        assert_eq!(name_from_file(Path::new("/x/.hidden.ogg"), none), "hidden");
        assert_eq!(name_from_file(Path::new("/x/click.wav"), none), "click 2");
        assert_eq!(
            name_from_file(Path::new("/x/Zap.flac"), |n| n == "Zap"),
            "Zap 2"
        );
        assert_eq!(name_from_file(Path::new("/x/★.wav"), none), "Sound");
    }
}
