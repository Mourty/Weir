//! What hotkeys show and play when they are pressed: a popup, and sounds.
//!
//! Sounds are Weir's own few, made by the engine, and sounds of your own,
//! kept in Weir's settings folder so that they go along when settings are
//! exported. A hotkey names the sounds it plays; Weir plays them straight to
//! one device, past every bus, so nothing recording a bus hears them.

use crate::library::library_name_problem;
use crate::model::{BusKind, DeviceInfo, DeviceKind, MixerState};
use serde::{Deserialize, Serialize};

/// Weir's own sounds, by name. Sounds of your own cannot take these names.
pub const BUILTIN_SOUNDS: [&str; 4] = ["Click", "Beep up", "Beep down", "Tick"];
/// The longest a sound of your own may play, in seconds.
pub const SOUND_SECONDS_MAX: f32 = 10.0;
/// The largest sound file Weir takes, in bytes.
pub const SOUND_FILE_MAX: u64 = 10 * 1024 * 1024;
/// The kinds of sound file Weir reads, by extension.
pub const SOUND_EXTENSIONS: [&str; 3] = ["wav", "ogg", "flac"];
/// The quietest and loudest hotkey sounds play, in dB.
pub const SOUNDS_VOLUME_DB: (f32, f32) = (-40.0, 0.0);
/// How loud hotkey sounds play unless set otherwise, in dB.
pub const SOUNDS_VOLUME_DEFAULT_DB: f32 = -12.0;

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// A sound hotkeys can play.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SoundInfo {
    /// Its name, which hotkeys use.
    pub name: String,
    /// Whether it is one of Weir's own.
    #[serde(default, skip_serializing_if = "is_default")]
    pub builtin: bool,
    /// How long it plays, in seconds.
    pub seconds: f32,
}

impl SoundInfo {
    /// How long it plays, in words: `15 ms`, `2.4 s`.
    pub fn length(&self) -> String {
        sound_length(self.seconds)
    }
}

/// A sound's length in words: milliseconds under a second, `15 ms`, and
/// seconds from there, `2.4 s`.
pub fn sound_length(seconds: f32) -> String {
    if seconds < 1.0 {
        format!("{} ms", (seconds * 1000.0).round())
    } else {
        format!("{seconds:.1} s")
    }
}

/// The sounds a hotkey plays, each by name, or left out for none.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HotkeySounds {
    /// When its keys go down, or it is pressed by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub press: Option<String>,
    /// When its keys come up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// Each time a hotkey that repeats while held does its steps again: a
    /// tick for each step of a volume going up, say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<String>,
}

impl HotkeySounds {
    /// Whether it plays nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Every sound it names, with where: `press`, `release` or `repeat`.
    pub fn named(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("press", &self.press),
            ("release", &self.release),
            ("repeat", &self.repeat),
        ]
        .into_iter()
        .filter_map(|(at, s)| s.as_deref().map(|s| (at, s)))
    }

    /// Every sound it names, to change.
    pub fn names_mut(&mut self) -> impl Iterator<Item = &mut Option<String>> {
        [&mut self.press, &mut self.release, &mut self.repeat].into_iter()
    }
}

/// What pressing a hotkey shows.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyPopup {
    /// The desktop's own small popup, the one its volume keys show, which
    /// goes over full-screen games and takes no focus (KDE Plasma); a
    /// notification where the desktop has none.
    #[default]
    Popup,
    /// A notification, which does not stay in the desktop's list of them.
    Notification,
    /// Nothing.
    Nothing,
}

/// Parameters of `add_sound`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddSoundParams {
    /// Its name: letters, digits, spaces and `_ - .`, 64 characters at
    /// most, and none of Weir's own sounds' names.
    pub name: String,
    /// The sound file, as a whole path: `.wav`, `.ogg` or `.flac`, 10
    /// seconds and 10 MB at most. Weir keeps a copy, so the file can be
    /// moved or deleted afterwards.
    pub path: String,
}

/// The device hotkeys' sounds play on now, by `node.name`: `chosen` while
/// it is plugged in, otherwise the device of the first bus that plays to
/// one that is, otherwise none. `devices` is what `list_devices` returns.
pub fn sounds_device(
    chosen: Option<&str>,
    mixer: &MixerState,
    devices: &[DeviceInfo],
) -> Option<String> {
    let present = |name: &str| {
        devices
            .iter()
            .any(|d| d.kind == DeviceKind::Sink && d.name == name)
    };
    chosen
        .filter(|d| present(d))
        .map(str::to_string)
        .or_else(|| {
            mixer
                .buses
                .iter()
                .filter(|b| b.kind == BusKind::Hardware)
                .filter_map(|b| b.device.as_deref())
                .find(|d| present(d))
                .map(str::to_string)
        })
}

/// What is wrong with `name` as the name of a sound of your own, in words
/// for the person typing it, or `None` when it will do.
pub fn sound_name_problem(name: &str) -> Option<&'static str> {
    if let Some(problem) = library_name_problem(name) {
        return Some(problem);
    }
    if BUILTIN_SOUNDS
        .iter()
        .any(|b| b.eq_ignore_ascii_case(name.trim()))
    {
        return Some("Weir has a sound of its own by that name");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_hotkey_without_sounds_says_nothing_about_them() {
        let s = HotkeySounds::default();
        assert!(s.is_empty());
        assert_eq!(serde_json::to_value(&s).unwrap(), json!({}));
        let s: HotkeySounds = serde_json::from_value(json!({"press": "Click"})).unwrap();
        assert_eq!(s.named().collect::<Vec<_>>(), [("press", "Click")]);
    }

    #[test]
    fn sounds_play_on_the_chosen_device_or_the_first_buss() {
        use crate::model::{Bus, ChannelLayout};
        let sink = |name: &str| DeviceInfo {
            id: 1,
            name: name.into(),
            description: name.into(),
            kind: DeviceKind::Sink,
            channels: Vec::new(),
        };
        let mut headset = Bus::new(1, "Headset", BusKind::Hardware, ChannelLayout::Stereo);
        headset.device = Some("headset".into());
        let mut speakers = Bus::new(2, "Speakers", BusKind::Hardware, ChannelLayout::Stereo);
        speakers.device = Some("speakers".into());
        let stream = Bus::new(3, "Stream", BusKind::Virtual, ChannelLayout::Stereo);
        let m = MixerState {
            strips: Vec::new(),
            buses: vec![stream, headset, speakers],
        };
        let both = [sink("headset"), sink("speakers")];
        assert_eq!(
            sounds_device(Some("speakers"), &m, &both).unwrap(),
            "speakers"
        );
        assert_eq!(sounds_device(None, &m, &both).unwrap(), "headset");
        // Unplugged: the first bus's device that is plugged in.
        assert_eq!(sounds_device(Some("usb"), &m, &both).unwrap(), "headset");
        assert_eq!(sounds_device(None, &m, &both[1..]).unwrap(), "speakers");
        assert_eq!(sounds_device(None, &m, &[]), None);
    }

    #[test]
    fn lengths_read_in_the_unit_that_suits_them() {
        assert_eq!(sound_length(0.015), "15 ms");
        assert_eq!(sound_length(0.999), "999 ms");
        assert_eq!(sound_length(2.44), "2.4 s");
    }

    #[test]
    fn own_sounds_cannot_take_weirs_names() {
        assert_eq!(sound_name_problem("Airhorn"), None);
        assert!(sound_name_problem("click").is_some());
        assert!(sound_name_problem("a/b").is_some());
        assert!(sound_name_problem("").is_some());
    }
}
