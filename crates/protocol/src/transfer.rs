//! Settings to share, or to take to another computer: scenes, setups,
//! hotkeys, equalizer presets, app rules and preferences, exported to a
//! `.zip` of JSON files, or one of them to a bare `.json`, and imported
//! again.
//!
//! The files are described in `docs/API.md`; here are the requests that
//! write and read them. Importing is two steps: `inspect_import` says what
//! a file holds and what importing each item would meet (a name already
//! taken, a strip this mixer does not have), and `import_settings` imports
//! it with the choices made.

use crate::hotkeys::HotkeyKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The format exported files are written in. A file in a later format is
/// refused rather than half read, and a later Weir reads this one.
pub const EXPORT_FORMAT: u32 = 1;

/// The window's own preferences that "window look" carries, as the
/// window's `gui.toml` names them. Window sizes and the names of devices
/// once plugged in belong to one computer and stay on it.
pub const WINDOW_LOOK_KEYS: &[&str] = &[
    "appearance",
    "system_accent",
    "system_font",
    "bus_sources",
    "show_app_volume",
    "spectrum",
];

fn is_false(b: &bool) -> bool {
    !*b
}

/// What sort of thing an exported item is.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExportKind {
    /// A scene.
    Scene,
    /// A setup.
    Setup,
    /// A hotkey.
    Hotkey,
    /// An equalizer preset of the user's own.
    EqPreset,
    /// The app rules, all together.
    AppRules,
    /// One part of the preferences.
    Preferences,
}

impl ExportKind {
    /// Every kind, in the order lists show them.
    pub const ALL: [ExportKind; 6] = [
        ExportKind::Scene,
        ExportKind::Setup,
        ExportKind::Hotkey,
        ExportKind::EqPreset,
        ExportKind::AppRules,
        ExportKind::Preferences,
    ];

    /// What one of them is called, such as "scene".
    pub fn word(self) -> &'static str {
        match self {
            ExportKind::Scene => "scene",
            ExportKind::Setup => "setup",
            ExportKind::Hotkey => "hotkey",
            ExportKind::EqPreset => "equalizer preset",
            ExportKind::AppRules => "app rules",
            ExportKind::Preferences => "preferences",
        }
    }

    /// A heading for all of them, such as "Scenes".
    pub fn heading(self) -> &'static str {
        match self {
            ExportKind::Scene => "Scenes",
            ExportKind::Setup => "Setups",
            ExportKind::Hotkey => "Hotkeys",
            ExportKind::EqPreset => "Equalizer presets",
            ExportKind::AppRules => "App rules",
            ExportKind::Preferences => "Preferences",
        }
    }
}

/// A part of the preferences, exported and imported on its own.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PreferencePart {
    /// The window's look: light or dark, the desktop's accent and font,
    /// what each bus lists, app volume sliders and the spectrum.
    WindowLook,
    /// How the mixer behaves: solo, meter speed, the tray icon, and what
    /// opens when Weir starts.
    Mixer,
    /// The sample rate and latency. They suit the sound hardware of the
    /// computer they were set on.
    AudioTiming,
    /// Whether Weir starts when you log in.
    StartAtLogin,
}

impl PreferencePart {
    /// Every part, in the order lists show them.
    pub const ALL: [PreferencePart; 4] = [
        PreferencePart::WindowLook,
        PreferencePart::Mixer,
        PreferencePart::AudioTiming,
        PreferencePart::StartAtLogin,
    ];

    /// Its name for people.
    pub fn label(self) -> &'static str {
        match self {
            PreferencePart::WindowLook => "Window look",
            PreferencePart::Mixer => "Mixer behavior",
            PreferencePart::AudioTiming => "Audio timing",
            PreferencePart::StartAtLogin => "Start at login",
        }
    }

    /// What it holds, in a line.
    pub fn summary(self) -> &'static str {
        match self {
            PreferencePart::WindowLook => {
                "Light or dark, accent and font, what buses list, app volume sliders, spectrum"
            }
            PreferencePart::Mixer => "Solo, meter speed, tray icon, what opens when Weir starts",
            PreferencePart::AudioTiming => "Sample rate and latency",
            PreferencePart::StartAtLogin => "Whether Weir starts when you log in",
        }
    }

    /// Its name in files and in the ids of `inspect_import`'s items.
    pub fn key(self) -> &'static str {
        match self {
            PreferencePart::WindowLook => "window_look",
            PreferencePart::Mixer => "mixer",
            PreferencePart::AudioTiming => "audio_timing",
            PreferencePart::StartAtLogin => "start_at_login",
        }
    }
}

/// Parameters of `export_settings`: where to write, and what. Name each
/// item, or give `all`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExportParams {
    /// The file to write: a `.zip`, or a `.json` for exactly one item. A
    /// file already there is replaced.
    pub path: String,
    /// Everything there is: every scene, setup, hotkey and preset of your
    /// own, the app rules and every part of the preferences.
    #[serde(default, skip_serializing_if = "is_false")]
    pub all: bool,
    /// Scenes, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenes: Vec<String>,
    /// Setups, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setups: Vec<String>,
    /// Hotkeys, by id or name. Their groups and the list's order go with
    /// them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hotkeys: Vec<HotkeyKey>,
    /// Equalizer presets of your own, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub eq_presets: Vec<String>,
    /// The app rules.
    #[serde(default, skip_serializing_if = "is_false")]
    pub app_rules: bool,
    /// Parts of the preferences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferences: Vec<PreferencePart>,
    /// The window's look, which only the window knows: an object with its
    /// settings named in `WINDOW_LOOK_KEYS`, as the window or `weirctl`
    /// read them. Without it, `window_look` is left out of the export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_look: Option<Value>,
}

/// What `export_settings` returns.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExportResult {
    /// The file written.
    pub path: String,
    /// The files in it, such as `scenes/Gaming.json`; for a `.json`, its
    /// own name.
    pub files: Vec<String>,
}

/// Parameters of `inspect_import`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InspectImportParams {
    /// The file to read: a `.zip` Weir exported, or one of its `.json`
    /// files on its own.
    pub path: String,
}

/// Strip or bus.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// A strip.
    Strip,
    /// A bus.
    Bus,
}

/// A strip or bus that something imported names and the mixer has none
/// called.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct MissingTarget {
    /// Strip or bus.
    pub kind: TargetKind,
    /// Its name in the file.
    pub name: String,
}

/// Keys an imported hotkey has that a hotkey here has too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct KeysTaken {
    /// The keys, such as `Ctrl+Alt+M`.
    pub keys: String,
    /// The name of the hotkey here that has them.
    pub by: String,
}

/// One thing a file to import holds, and what importing it would meet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImportItem {
    /// Names the item in `import_settings`: its file in the export, such
    /// as `scenes/Gaming.json`, and for a part of the preferences the part
    /// after `#`, such as `preferences/preferences.json#audio_timing`.
    pub id: String,
    /// What sort of thing it is.
    pub kind: ExportKind,
    /// Its name; for app rules and the preferences, what it is.
    pub name: String,
    /// What it is or does, in a line: a hotkey's keys and steps, how many
    /// rules.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// For a hotkey, the group it was in. One here of the same name is
    /// joined; otherwise the group is made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Something of its kind here already has its name: choose to replace
    /// it, to keep both with this one under another name, or to skip it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub taken: bool,
    /// When `taken`, a name nothing has, to keep both under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_name: Option<String>,
    /// For a hotkey being added to the hotkeys here, keys one of them has
    /// already. It is imported without them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys_taken: Vec<KeysTaken>,
    /// Strips and buses it names that the mixer has none called. It can
    /// be imported only once each is given one the mixer has, in
    /// `map_strips` or `map_buses`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<MissingTarget>,
    /// Why it cannot be imported, such as a damaged file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broken: Option<String>,
    /// Anything else worth knowing before importing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What a file to import holds. What `inspect_import` returns.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImportInspection {
    /// The file.
    pub path: String,
    /// The version of Weir that exported it, such as `1.2.0`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub weir_version: String,
    /// When it was exported, as `2026-10-10T14-30-05Z`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported: Option<String>,
    /// Every item, scenes first, in the order of [`ExportKind::ALL`].
    pub items: Vec<ImportItem>,
    /// Every strip and bus the items name that the mixer has none called.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<MissingTarget>,
    /// Files in it that could not be read and are not any one item, each
    /// with why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

/// What to do with an item whose name is taken here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportChoice {
    /// Replace the one here with it.
    Replace,
    /// Keep both, importing this one under the name given:
    /// `{"rename": "Gaming 2"}`.
    Rename(String),
    /// Leave it out.
    Skip,
}

/// What to do with items whose name is taken and that have no choice of
/// their own.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WhenTaken {
    /// Leave them out.
    #[default]
    Skip,
    /// Replace the ones here.
    Replace,
    /// Keep both, under the item's `free_name`.
    KeepBoth,
}

/// How imported hotkeys meet the hotkeys here.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyImport {
    /// Add them to the hotkeys here.
    #[default]
    Add,
    /// Remove every hotkey and group here first, and take the imported
    /// ones' groups and order.
    ReplaceAll,
}

/// Parameters of `import_settings`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImportParams {
    /// The file, as given to `inspect_import`.
    pub path: String,
    /// The items to import, by `id`. Left out, every item that can be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<String>>,
    /// What to do with items whose name is taken, by `id`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub choices: BTreeMap<String, ImportChoice>,
    /// What to do with items whose name is taken and that are not in
    /// `choices`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub when_taken: WhenTaken,
    /// Add the hotkeys to the ones here, or replace them all.
    #[serde(default, skip_serializing_if = "is_default")]
    pub hotkeys: HotkeyImport,
    /// Strips the file names that the mixer has none called, each to a
    /// strip it has, by name: `{"Music": "Media"}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub map_strips: BTreeMap<String, String>,
    /// The same for buses.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub map_buses: BTreeMap<String, String>,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// What `import_settings` returns.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImportResult {
    /// What was imported, a line each, such as `scene 'Gaming'`.
    pub imported: Vec<String>,
    /// What was left out, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    /// Anything else worth knowing, such as keys left out because a
    /// hotkey here has them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The folder holding copies of what the import replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
    /// The window look imported, for a client to save in `gui.toml` when
    /// no window took it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_look: Option<Value>,
    /// Whether an open window was sent the window look to take.
    #[serde(default, skip_serializing_if = "is_false")]
    pub window_told: bool,
}

/// `name`, or with a number after it, " 2", " 3" and so on, whichever
/// `taken` says is free first, kept to `max` characters.
pub fn free_name(name: &str, max: usize, taken: impl Fn(&str) -> bool) -> String {
    let name = name.trim();
    if !taken(name) {
        return name.to_string();
    }
    (2..)
        .map(|n| {
            let tail = format!(" {n}");
            let room = max.saturating_sub(tail.chars().count());
            let base: String = name.chars().take(room).collect();
            format!("{}{tail}", base.trim_end())
        })
        .find(|candidate| !taken(candidate))
        .expect("some number is free")
}

/// A Unix time as `2026-09-27T14-30-05Z`: sortable, and fine in a file name.
pub fn utc_stamp(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's days-to-civil-date algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn times_turn_into_sortable_names() {
        assert_eq!(utc_stamp(0), "1970-01-01T00-00-00Z");
        assert_eq!(utc_stamp(1_700_000_000), "2023-11-14T22-13-20Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00-00-00Z");
        assert_eq!(utc_stamp(1_791_642_605), "2026-10-10T14-30-05Z");
    }

    #[test]
    fn free_names_count_up_and_stay_short() {
        let have = ["Gaming", "Gaming 2"];
        assert_eq!(free_name("Gaming", 64, |n| have.contains(&n)), "Gaming 3");
        assert_eq!(free_name("Music", 64, |n| have.contains(&n)), "Music");
        assert_eq!(free_name("abcdef", 6, |n| n == "abcdef"), "abcd 2");
    }

    #[test]
    fn choices_read_as_written_in_the_docs() {
        let p: ImportParams = serde_json::from_value(json!({
            "path": "x.zip",
            "choices": {"scenes/Gaming.json": {"rename": "Gaming 2"},
                        "setups/Home.json": "replace"},
            "when_taken": "keep_both",
            "hotkeys": "replace_all",
            "map_strips": {"Music": "Media"}
        }))
        .unwrap();
        assert_eq!(
            p.choices["scenes/Gaming.json"],
            ImportChoice::Rename("Gaming 2".into())
        );
        assert_eq!(p.choices["setups/Home.json"], ImportChoice::Replace);
        assert_eq!(p.when_taken, WhenTaken::KeepBoth);
        assert_eq!(p.hotkeys, HotkeyImport::ReplaceAll);
        let e: ExportParams =
            serde_json::from_value(json!({"path": "a.zip", "all": true})).unwrap();
        assert!(e.all && e.scenes.is_empty());
    }
}
