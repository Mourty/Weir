//! Adding sounds of the person's own for hotkeys: a file dialog, then
//! `add_sound` under a name made from the file's, and the new sound handed
//! to the editor that asked for it.

use super::App;
use crate::file_dialog::{Dialog, Picked};
use crate::sounds::{name_from_file, SoundSlot, FILTERS};
use std::time::{Duration, Instant};
use weir_protocol::*;

/// How long to wait for a sound being added before giving up on handing it
/// to the editor.
const ADD_WAIT: Duration = Duration::from_secs(10);

impl App {
    /// Ask for a sound file to add, for the editor's `slot` when it asks.
    pub(super) fn open_add_sound(&mut self, ctx: &egui::Context, slot: Option<SoundSlot>) {
        if self.sound_dialog.is_none() {
            let dialog = Dialog::open(ctx, "Add a sound for hotkeys", FILTERS);
            self.sound_dialog = Some((dialog, slot));
        }
    }

    /// Each frame: the sound file dialog's answer, and a sound being added
    /// arriving.
    pub(super) fn poll_sounds(&mut self, state: &FullState) {
        if let Some(picked) = self.sound_dialog.as_ref().and_then(|(d, _)| d.poll()) {
            let (_, slot) = self.sound_dialog.take().expect("polled");
            match picked {
                Picked::File(path) => {
                    let sounds = &state.hotkeys.sounds;
                    let name = name_from_file(&path, |n| {
                        sounds.iter().any(|s| s.name.eq_ignore_ascii_case(n))
                    });
                    self.actions.push(Request::AddSound(AddSoundParams {
                        name: name.clone(),
                        path: path.display().to_string(),
                    }));
                    self.sound_wanted = Some((name, slot, Instant::now()));
                }
                Picked::Cancelled => {}
                Picked::NoDialog(_) => self.show_toast(
                    "Your desktop has no file dialog Weir can use: add sounds with \
                     weirctl sound add",
                    false,
                ),
            }
        }
        let Some((name, slot, at)) = &self.sound_wanted else {
            return;
        };
        if state.hotkeys.sounds.iter().any(|s| s.name == *name) {
            let text = format!("Added the sound '{name}'");
            if let (Some(slot), Some(editor)) = (slot, &mut self.hotkey_editor) {
                editor.set_sound(*slot, name.clone());
            }
            self.sound_wanted = None;
            self.show_toast(&text, false);
        } else if at.elapsed() > ADD_WAIT {
            self.sound_wanted = None;
        }
    }
}
