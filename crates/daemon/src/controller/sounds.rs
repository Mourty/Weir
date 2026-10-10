//! What hotkeys show and play: their sounds, the device those play on, and
//! the popup that says what a press did.

use super::Controller;
use crate::hotkeys::popup::{popup_for, Popups};
use crate::sounds::{decode, sound_extension, sound_file, Library};
use serde_json::Value;
use std::path::Path;
use tracing::{debug, info, warn};
use weir_protocol::*;

impl Controller {
    /// Hand the controller what shows popups.
    pub fn set_popups(&self, popups: Popups) {
        *self.popups.lock().unwrap() = Some(popups);
    }

    /// Give the engine every sound there is now.
    pub fn push_sounds(&self) {
        let Some(engine) = &self.engine else {
            return;
        };
        let bank = self.inner.lock().unwrap().sounds.bank();
        if let Err(e) = engine.set_sounds(bank) {
            warn!("engine rejected the hotkey sounds: {e}");
        }
    }

    /// Tell the engine where sounds play, when that changed: the device
    /// chosen, or the first bus's while it is not plugged in. Called when
    /// the settings, the mixer or the devices change.
    pub(super) fn update_sounds_device(&self) {
        let device = {
            let mut inner = self.inner.lock().unwrap();
            let now = sounds_device(
                inner.settings.sounds_device.as_deref(),
                &inner.mixer,
                &inner.devices,
            );
            if inner.sounds_device.as_ref() == Some(&now) {
                return;
            }
            inner.sounds_device = Some(now.clone());
            now
        };
        if let Some(engine) = &self.engine {
            if let Err(e) = engine.set_sounds_device(device) {
                warn!("engine rejected the hotkey sounds' device: {e}");
            }
        }
    }

    /// Play the sound called `name` where hotkeys' sounds play.
    pub(super) fn play_sound(&self, name: &str) -> Result<Value, RpcError> {
        let (index, gain, device) = {
            let inner = self.inner.lock().unwrap();
            let index = inner
                .sounds
                .index(name)
                .ok_or_else(|| RpcError::application(format!("no sound called '{name}'")))?;
            let gain = 10f32.powf(inner.settings.sounds_volume_db / 20.0);
            (index, gain, inner.sounds_device.clone().flatten())
        };
        #[cfg(test)]
        self.played.lock().unwrap().push(
            self.inner.lock().unwrap().sounds.infos()[index]
                .name
                .clone(),
        );
        if device.is_none() {
            return Err(RpcError::application(
                "sounds have nowhere to play: pick a device for them in Preferences, \
                 or give a bus a device",
            ));
        }
        if let Some(engine) = &self.engine {
            if !engine.play_sound(index, gain) {
                return Err(RpcError::application("the audio engine is not running"));
            }
        }
        Ok(Value::Null)
    }

    /// Play a hotkey's sound, if it has one there: never in the way of the
    /// hotkey itself.
    pub fn hotkey_sound(&self, name: Option<&str>) {
        if let Some(name) = name {
            if let Err(e) = self.play_sound(name) {
                debug!("a hotkey's sound did not play: {}", e.message);
            }
        }
    }

    /// Show what `h` did with `steps`, its strips and buses by id, as the
    /// settings say, unless the hotkey says no popup.
    pub fn hotkey_popup(&self, h: &Hotkey, steps: &[HotkeyStep]) {
        if !h.popup {
            return;
        }
        let Some(popups) = self.popups.lock().unwrap().clone() else {
            return;
        };
        let (mixer, how) = {
            let inner = self.inner.lock().unwrap();
            (inner.mixer.clone(), inner.settings.hotkey_popup)
        };
        if let Some(popup) = popup_for(steps, &mixer) {
            popups.show(popup, how);
        }
    }

    /// The name of the sound called `name` as the library has it, ignoring
    /// case, or `None` when there is none.
    pub(super) fn sound_named(&self, name: &str) -> Option<String> {
        self.inner.lock().unwrap().sounds.info(name).map(|s| s.name)
    }

    /// Read the sound files again, give the engine the sounds, and tell
    /// clients.
    pub(super) fn sounds_changed(&self) {
        let library = Library::load(&self.paths.sounds_dir);
        self.inner.lock().unwrap().sounds = library;
        self.push_sounds();
        self.announce(Notification::HotkeysChanged(self.hotkeys_info()));
    }

    /// Add the sound file at `p.path` as `p.name`, keeping a copy.
    pub(super) fn add_sound(&self, p: AddSoundParams) -> Result<Value, RpcError> {
        let name = p.name.trim().to_string();
        if let Some(problem) = sound_name_problem(&name) {
            return Err(RpcError::invalid_params(problem));
        }
        if let Some(taken) = self.sound_named(&name) {
            return Err(RpcError::application(format!(
                "there is already a sound called '{taken}'"
            )));
        }
        let from = Path::new(&p.path);
        if !from.is_absolute() {
            return Err(RpcError::invalid_params(
                "give the sound file as a whole path, starting with /",
            ));
        }
        let ext = sound_extension(from)
            .ok_or_else(|| RpcError::invalid_params("Weir plays .wav, .ogg and .flac files"))?;
        decode(from).map_err(|e| RpcError::application(format!("{e:#}")))?;
        let dir = &self.paths.sounds_dir;
        let to = dir.join(format!("{name}.{ext}"));
        std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::copy(from, &to))
            .map_err(|e| {
                RpcError::application(format!("could not keep a copy of the sound: {e}"))
            })?;
        info!("added the sound '{name}'");
        self.sounds_changed();
        self.inner
            .lock()
            .unwrap()
            .sounds
            .info(&name)
            .map(|s| to_json(&s))
            .ok_or_else(|| RpcError::application("the sound could not be read back"))
    }

    /// Remove the sound of the person's own called `name`, and take it out
    /// of every hotkey that plays it.
    pub(super) fn remove_sound(&self, p: NameParams) -> Result<Value, RpcError> {
        let name = {
            let inner = self.inner.lock().unwrap();
            let info = inner
                .sounds
                .info(&p.name)
                .ok_or_else(|| RpcError::application(format!("no sound called '{}'", p.name)))?;
            if info.builtin {
                return Err(RpcError::application(format!(
                    "'{}' is one of Weir's own sounds, which stay",
                    info.name
                )));
            }
            info.name
        };
        if let Some(file) = sound_file(&self.paths.sounds_dir, &name) {
            std::fs::remove_file(&file).map_err(|e| {
                RpcError::application(format!("could not remove {}: {e}", file.display()))
            })?;
        }
        info!("removed the sound '{name}'");
        let edited = self.edit_hotkeys(|list| {
            for h in &mut list.hotkeys {
                for s in h.sounds.names_mut() {
                    if s.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(&name)) {
                        *s = None;
                    }
                }
            }
            Ok(())
        });
        if let Err(e) = edited {
            warn!(
                "hotkeys still name the removed sound '{name}': {}",
                e.message
            );
        }
        self.sounds_changed();
        Ok(to_json(&self.hotkeys_info()))
    }

    /// `h`'s sounds, each named as the library has it, or, where it has no
    /// such sound and `also` does not name one, why not.
    pub(super) fn check_sounds(&self, h: &mut Hotkey, also: &[String]) -> Result<(), RpcError> {
        for s in h.sounds.names_mut() {
            let Some(name) = s.as_deref().map(str::trim) else {
                continue;
            };
            if name.is_empty() {
                *s = None;
                continue;
            }
            *s = Some(match self.sound_named(name) {
                Some(known) => known,
                None => also
                    .iter()
                    .find(|a| a.eq_ignore_ascii_case(name))
                    .cloned()
                    .ok_or_else(|| {
                        RpcError::invalid_params(format!("there is no sound called '{name}'"))
                    })?,
            });
        }
        Ok(())
    }

    /// Hotkeys that play sounds Weir no longer has, such as a file removed
    /// by hand.
    pub(super) fn sound_problems(&self, hotkeys: &[Hotkey]) -> Vec<HotkeyProblem> {
        let inner = self.inner.lock().unwrap();
        hotkeys
            .iter()
            .filter_map(|h| {
                let gone: Vec<&str> = h
                    .sounds
                    .named()
                    .map(|(_, s)| s)
                    .filter(|s| inner.sounds.index(s).is_none())
                    .collect();
                let first = gone.first()?;
                Some(HotkeyProblem {
                    hotkey: h.id,
                    problem: format!(
                        "It plays the sound '{first}', which is gone, so it plays nothing \
                         there. Pick another sound, or add one by that name."
                    ),
                })
            })
            .collect()
    }
}
