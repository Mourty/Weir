//! The files settings are exported to and imported from (the requests are
//! in `weir_protocol::transfer`): a `.zip` holding a manifest and one JSON
//! file per item, in a folder for each kind, or one item's file on its
//! own.
//!
//! Every file starts with what it holds (`weir`), the format it is in and
//! the Weir that wrote it, so that it can be read alone and a later Weir
//! can still read it. Reading looks at the format before anything else
//! and refuses a later one rather than half read it.

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Cursor, Read, Write};
use std::path::Path;
use weir_protocol::{
    EqBand, ExportKind, Hotkey, MixerState, Scene, Startup, TrayIcon, EXPORT_FORMAT,
};

/// The manifest's name in a `.zip`: what is in it, and who wrote it.
pub const MANIFEST: &str = "weir-export.json";
/// The hotkeys' groups and the list's order, beside the hotkeys.
pub const HOTKEY_LIST: &str = "hotkeys/groups.json";
/// The largest file read from a `.zip`, and the most files: far more than
/// any settings need, and a guard against a damaged or hostile one.
const FILE_MAX: u64 = 16 << 20;
const FILES_MAX: usize = 10_000;

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

/// The folder in a `.zip` each kind goes in.
pub fn folder(kind: ExportKind) -> &'static str {
    match kind {
        ExportKind::Scene => "scenes",
        ExportKind::Setup => "setups",
        ExportKind::Hotkey => "hotkeys",
        ExportKind::EqPreset => "eq-presets",
        ExportKind::AppRules => "app-rules",
        ExportKind::Preferences => "preferences",
    }
}

/// The kind whose folder `path` is in, if any.
pub fn kind_of_path(path: &str) -> Option<ExportKind> {
    let dir = path.split('/').next()?;
    ExportKind::ALL.into_iter().find(|k| folder(*k) == dir)
}

/// A scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneFile {
    pub name: String,
    pub scene: Scene,
}

/// A setup: a whole mixer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupFile {
    pub name: String,
    pub setup: MixerState,
}

/// A hotkey, its steps naming strips and buses rather than numbering them,
/// and the name of its group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotkeyFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub hotkey: Hotkey,
}

/// The hotkeys' groups, and the order of the list, by names.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HotkeyListFile {
    #[serde(default)]
    pub groups: Vec<GroupFile>,
    #[serde(default)]
    pub order: Vec<PlaceFile>,
}

/// A group of hotkeys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupFile {
    pub name: String,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

/// A place in the list of hotkeys: `{"hotkey": "Mute mic"}` or `{"group":
/// "Streaming"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceFile {
    Hotkey(String),
    Group(String),
}

/// An equalizer preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqPresetFile {
    pub name: String,
    pub bands: Vec<EqBand>,
}

/// The app rules, their strips by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppRulesFile {
    pub rules: Vec<RuleFile>,
}

/// One app rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleFile {
    pub app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip: Option<String>,
}

/// The preferences: each part there only when exported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PreferencesFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_look: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mixer: Option<MixerPrefs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_timing: Option<AudioTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_at_login: Option<bool>,
}

/// How the mixer behaves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MixerPrefs {
    pub meter_rate_hz: u32,
    pub startup: Startup,
    pub tray: bool,
    pub tray_icon: TrayIcon,
    pub solo: SoloFile,
}

/// The solo mode, its bus by name: `"exclusive"` or `{"cue": "Headset"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoloFile {
    Exclusive,
    Cue(String),
}

/// The sample rate and latency; left out, PipeWire decides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioTiming {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantum: Option<u32>,
}

/// What a `.zip` holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ManifestFile {
    #[serde(default)]
    pub files: Vec<String>,
}

/// What one file holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Manifest(ManifestFile),
    Scene(SceneFile),
    Setup(SetupFile),
    Hotkey(HotkeyFile),
    HotkeyList(HotkeyListFile),
    EqPreset(EqPresetFile),
    AppRules(AppRulesFile),
    Preferences(PreferencesFile),
}

/// Who wrote an export, and when.
#[derive(Debug, Clone, PartialEq)]
pub struct Stamp {
    /// Weir's version, such as `1.2.0`.
    pub weir_version: String,
    /// When, as `config::utc_stamp` writes it.
    pub exported: String,
}

/// `body` as the text of a file: what it holds and who wrote it first,
/// then the thing itself.
pub fn write_file(body: &Body, stamp: &Stamp) -> String {
    #[derive(Serialize)]
    struct Out<'a, T> {
        weir: &'a str,
        format: u32,
        weir_version: &'a str,
        exported: &'a str,
        #[serde(flatten)]
        body: &'a T,
    }
    fn out<T: Serialize>(weir: &str, body: &T, stamp: &Stamp) -> String {
        let out = Out {
            weir,
            format: EXPORT_FORMAT,
            weir_version: &stamp.weir_version,
            exported: &stamp.exported,
            body,
        };
        let mut text = serde_json::to_string_pretty(&out).expect("settings serialize");
        text.push('\n');
        text
    }
    match body {
        Body::Manifest(b) => out("export", b, stamp),
        Body::Scene(b) => out("scene", b, stamp),
        Body::Setup(b) => out("setup", b, stamp),
        Body::Hotkey(b) => out("hotkey", b, stamp),
        Body::HotkeyList(b) => out("hotkey_list", b, stamp),
        Body::EqPreset(b) => out("eq_preset", b, stamp),
        Body::AppRules(b) => out("app_rules", b, stamp),
        Body::Preferences(b) => out("preferences", b, stamp),
    }
}

/// One file read back.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadFile {
    /// Its path in the `.zip`, or its own name.
    pub path: String,
    /// The Weir that wrote it, if it says.
    pub weir_version: String,
    /// When, if it says.
    pub exported: Option<String>,
    /// What it holds, or why it cannot be read, as a sentence's end ("it is
    /// damaged: ...").
    pub body: std::result::Result<Body, String>,
    /// Whether it is in a later format than this Weir reads.
    pub newer: bool,
}

/// Why a file from a later Weir is not read.
pub fn newer_reason(weir_version: &str) -> String {
    if weir_version.is_empty() {
        "it was made by a newer version of Weir; update Weir to import it".into()
    } else {
        format!("it was made by a newer version of Weir ({weir_version}); update Weir to import it")
    }
}

/// Read one file's text, found at `path`.
pub fn read_file(path: &str, text: &str) -> ReadFile {
    let mut file = ReadFile {
        path: path.to_string(),
        weir_version: String::new(),
        exported: None,
        body: Err(String::new()),
        newer: false,
    };
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            file.body = Err(format!("it is not a file Weir can read: {e}"));
            return file;
        }
    };
    let text_of = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    file.weir_version = text_of("weir_version").unwrap_or_default();
    file.exported = text_of("exported");
    let Some(kind) = text_of("weir") else {
        file.body = Err("it is not one of Weir's files: it does not say what it holds".into());
        return file;
    };
    match value.get("format").and_then(Value::as_u64) {
        None | Some(0) => {
            file.body = Err("it does not say which format it is in".into());
            return file;
        }
        Some(f) if f > u64::from(EXPORT_FORMAT) => {
            file.body = Err(newer_reason(&file.weir_version));
            file.newer = true;
            return file;
        }
        Some(_) => {}
    }
    fn parse<T: DeserializeOwned>(v: Value) -> std::result::Result<T, String> {
        serde_json::from_value(v).map_err(|e| format!("it is damaged: {e}"))
    }
    file.body = match kind.as_str() {
        "export" => parse(value).map(Body::Manifest),
        "scene" => parse(value).map(Body::Scene),
        "setup" => parse(value).map(Body::Setup),
        "hotkey" => parse(value).map(Body::Hotkey),
        "hotkey_list" => parse(value).map(Body::HotkeyList),
        "eq_preset" => parse(value).map(Body::EqPreset),
        "app_rules" => parse(value).map(Body::AppRules),
        "preferences" => parse(value).map(Body::Preferences),
        other => Err(format!(
            "it holds something this version of Weir does not know: '{other}'"
        )),
    };
    file
}

/// Every file in the `.zip` or `.json` at `path`, read. Fails when the
/// file itself cannot be read or is not one Weir exported.
pub fn read_export(path: &Path) -> Result<Vec<ReadFile>> {
    let shown = path.display();
    let size = std::fs::metadata(path)
        .with_context(|| format!("could not read {shown}"))?
        .len();
    if size > FILE_MAX * 4 {
        bail!("{shown} is far too large to be settings Weir exported");
    }
    let bytes = std::fs::read(path).with_context(|| format!("could not read {shown}"))?;
    let files = if bytes.starts_with(b"PK") {
        read_zip(&bytes).with_context(|| format!("could not read {shown}"))?
    } else {
        let Ok(text) = String::from_utf8(bytes) else {
            bail!("{shown} is neither a .zip nor a .json Weir exported");
        };
        let name = path
            .file_name()
            .map_or_else(|| shown.to_string(), |n| n.to_string_lossy().into_owned());
        vec![read_file(&name, &text)]
    };
    // A manifest from a later Weir means the rest may not mean what this
    // one thinks, so none of it is read.
    if let Some(m) = files.iter().find(|f| f.newer && f.path == MANIFEST) {
        bail!("{shown}: {}", newer_reason(&m.weir_version));
    }
    if let [only] = &files[..] {
        if let Err(reason) = &only.body {
            bail!("{shown} cannot be imported: {reason}");
        }
    }
    Ok(files)
}

/// Read every `.json` in a `.zip`. Anything else in it is not Weir's and
/// is passed over.
fn read_zip(bytes: &[u8]) -> Result<Vec<ReadFile>> {
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).context("it is not a .zip Weir can read")?;
    if zip.len() > FILES_MAX {
        bail!("it holds too many files to be settings Weir exported");
    }
    let mut files = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).context("it is damaged")?;
        let name = entry.name().to_string();
        if entry.is_dir() || !name.ends_with(".json") {
            continue;
        }
        let mut text = String::new();
        let read = (&mut entry).take(FILE_MAX + 1).read_to_string(&mut text);
        files.push(match read {
            Ok(n) if n as u64 <= FILE_MAX => read_file(&name, &text),
            Ok(_) => broken(&name, "it is too large".into()),
            Err(e) => broken(&name, format!("it is damaged: {e}")),
        });
    }
    let weirs = files.iter().filter(|f| f.body.is_ok() || f.newer).count();
    if weirs == 0 {
        bail!("it holds none of Weir's settings");
    }
    Ok(files)
}

fn broken(path: &str, reason: String) -> ReadFile {
    ReadFile {
        path: path.to_string(),
        weir_version: String::new(),
        exported: None,
        body: Err(reason),
        newer: false,
    }
}

/// `files`, each a path and its text, as a `.zip`, every file dated
/// `stamp` (`2026-10-10T14-30-05Z`).
pub fn zip_files(files: &[(String, String)], stamp: &str) -> Result<Vec<u8>> {
    let mut options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    if let Some(time) = zip_time(stamp) {
        options = options.last_modified_time(time);
    }
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, text) in files {
        zip.start_file(path, options)
            .with_context(|| format!("adding {path}"))?;
        zip.write_all(text.as_bytes())
            .with_context(|| format!("adding {path}"))?;
    }
    Ok(zip.finish().context("finishing the .zip")?.into_inner())
}

/// A stamp such as `2026-10-10T14-30-05Z` as a time in a `.zip`.
fn zip_time(stamp: &str) -> Option<zip::DateTime> {
    let n: Vec<u16> = stamp
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().ok())
        .collect::<Option<_>>()?;
    let [y, mo, d, h, mi, s] = n[..] else {
        return None;
    };
    zip::DateTime::from_date_and_time(y, mo as u8, d as u8, h as u8, mi as u8, s as u8).ok()
}

/// A file name for something called `name`: the characters file systems
/// refuse, and a leading dot that would hide it, replaced.
pub fn file_name(name: &str) -> String {
    let clean: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let clean = match clean.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None => clean,
    };
    if clean.is_empty() {
        "_".into()
    } else {
        clean
    }
}

/// Paths in `kind`'s folder for things called `names`, in order: each
/// `FOLDER/NAME.json`, with a number added where two would be the same, or
/// one would be `reserved`.
pub fn paths_for(kind: ExportKind, names: &[String], reserved: &[&str]) -> Vec<String> {
    let mut used: Vec<String> = reserved.iter().map(|r| r.to_lowercase()).collect();
    names
        .iter()
        .map(|name| {
            let base = file_name(name);
            let mut path = format!("{}/{base}.json", folder(kind));
            let mut n = 1;
            while used.contains(&path.to_lowercase()) {
                n += 1;
                path = format!("{}/{base} {n}.json", folder(kind));
            }
            used.push(path.to_lowercase());
            path
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stamp() -> Stamp {
        Stamp {
            weir_version: "1.2.0".into(),
            exported: "2026-10-10T14-30-05Z".into(),
        }
    }

    #[test]
    fn files_say_what_they_are_first_and_read_back() {
        let body = Body::EqPreset(EqPresetFile {
            name: "Warm".into(),
            bands: Vec::new(),
        });
        let text = write_file(&body, &stamp());
        assert!(
            text.starts_with("{\n  \"weir\": \"eq_preset\",\n  \"format\": 1,"),
            "{text}"
        );
        let back = read_file("eq-presets/Warm.json", &text);
        assert_eq!(back.body, Ok(body));
        assert_eq!(back.weir_version, "1.2.0");
        assert_eq!(back.exported.as_deref(), Some("2026-10-10T14-30-05Z"));
    }

    #[test]
    fn later_formats_and_strangers_are_refused() {
        let later = json!({"weir": "scene", "format": 2, "weir_version": "3.0.0"});
        let f = read_file("x.json", &later.to_string());
        assert!(f.newer);
        assert!(f
            .body
            .unwrap_err()
            .contains("newer version of Weir (3.0.0)"));
        let f = read_file("x.json", &json!({"weir": "song", "format": 1}).to_string());
        assert!(f.body.unwrap_err().contains("'song'"));
        let f = read_file("x.json", &json!({"name": "x"}).to_string());
        assert!(f.body.unwrap_err().contains("does not say what it holds"));
        let f = read_file("x.json", "{not json");
        assert!(f
            .body
            .unwrap_err()
            .starts_with("it is not a file Weir can read"));
        let f = read_file("x.json", &json!({"weir": "scene", "format": 1}).to_string());
        assert!(f.body.unwrap_err().starts_with("it is damaged"));
    }

    #[test]
    fn a_zip_reads_back_and_its_names_are_safe() {
        let paths = paths_for(
            ExportKind::Hotkey,
            &["A/B".into(), "A_B".into(), "groups".into(), ".x".into()],
            &[HOTKEY_LIST],
        );
        assert_eq!(
            paths,
            [
                "hotkeys/A_B.json",
                "hotkeys/A_B 2.json",
                "hotkeys/groups 2.json",
                "hotkeys/_x.json"
            ]
        );
        let scene = Body::Scene(SceneFile {
            name: "Gaming".into(),
            scene: Scene::default(),
        });
        let files = vec![
            (
                MANIFEST.to_string(),
                write_file(
                    &Body::Manifest(ManifestFile {
                        files: vec!["scenes/Gaming.json".into()],
                    }),
                    &stamp(),
                ),
            ),
            (
                "scenes/Gaming.json".to_string(),
                write_file(&scene, &stamp()),
            ),
            ("README.txt".to_string(), "not Weir's".to_string()),
        ];
        let bytes = zip_files(&files, "2026-10-10T14-30-05Z").unwrap();
        let dir = std::env::temp_dir().join(format!("weir-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.zip");
        std::fs::write(&path, &bytes).unwrap();
        let read = read_export(&path).unwrap();
        assert_eq!(read.len(), 2, "the README is passed over");
        assert_eq!(read[1].body, Ok(scene));
        assert_eq!(kind_of_path(&read[1].path), Some(ExportKind::Scene));
        // Damaged: not a zip after all.
        std::fs::write(&path, b"PK\x03\x04 broken").unwrap();
        assert!(read_export(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
