//! On-disk configuration and presets (TOML under `~/.config/weir`).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use weir_protocol::{
    hotkey_order, EqBand, EqPreset, Hotkey, HotkeyGroup, HotkeyListItem, MixerState,
};

/// Daemon settings live in the protocol crate so clients can read and change
/// them over the control socket.
pub use weir_protocol::Settings;

/// The configuration format this build writes. Bump it whenever an older
/// file needs changing to mean the same thing under the new code, and add a
/// step to [`migrate`].
pub const CONFIG_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Which format the file is in. Files from before versioning have none,
    /// which reads as 1.
    pub version: u32,
    /// The daemon's settings.
    pub settings: Settings,
    /// The strips and buses, as they were when last saved.
    pub mixer: MixerState,
    /// Where applications go when they start playing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub app_rules: Vec<weir_protocol::AppRule>,
    /// The scene and setup last loaded or saved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_scene: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_setup: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            settings: Settings::default(),
            mixer: MixerState::default(),
            app_rules: Vec::new(),
            current_scene: None,
            current_setup: None,
        }
    }
}

/// What reading a configuration found beyond its contents.
#[derive(Debug, Default)]
pub struct LoadReport {
    /// Human readable descriptions of what was upgraded.
    pub migrated: Vec<String>,
    /// The file came from a newer Weir than this one.
    pub from_newer: Option<u32>,
}

/// Bring `cfg` up to [`CONFIG_VERSION`], one version at a time.
pub fn migrate(cfg: &mut Config) -> LoadReport {
    let mut report = LoadReport::default();
    if cfg.version > CONFIG_VERSION {
        report.from_newer = Some(cfg.version);
        return report;
    }
    // Each step turns version N into N + 1.
    while cfg.version < CONFIG_VERSION {
        let v = cfg.version;
        match v {
            1 => {
                // Buses gained a safety limiter. Virtual ones start with it
                // on, as a new one would.
                let mut named = Vec::new();
                for b in cfg.mixer.buses.iter_mut() {
                    if b.kind == weir_protocol::BusKind::Virtual && !b.limiter.enabled {
                        b.limiter = weir_protocol::Limiter::on();
                        named.push(b.name.clone());
                    }
                }
                if !named.is_empty() {
                    report.migrated.push(format!(
                        "switched the new safety limiter on for {}",
                        named.join(", ")
                    ));
                }
            }
            2 => {
                // The window used to show hardware strips and buses before
                // virtual ones, whatever order the file had; now it shows
                // the file's order, which can be changed. Sort the file the
                // way it used to look.
                use weir_protocol::{BusKind, StripKind};
                let m = &mut cfg.mixer;
                let before: Vec<u32> = m.strips.iter().map(|s| s.id).collect();
                m.strips.sort_by_key(|s| s.kind != StripKind::Hardware);
                let bus_before: Vec<u32> = m.buses.iter().map(|b| b.id).collect();
                m.buses.sort_by_key(|b| b.kind != BusKind::Hardware);
                if before != m.strips.iter().map(|s| s.id).collect::<Vec<_>>()
                    || bus_before != m.buses.iter().map(|b| b.id).collect::<Vec<_>>()
                {
                    report
                        .migrated
                        .push("kept strips and buses in the order the window showed them".into());
                }
            }
            _ => report.migrated.push(format!("format {v} to {}", v + 1)),
        }
        cfg.version = v + 1;
    }
    report
}

/// Where the daemon keeps its files.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The configuration itself.
    pub config_file: PathBuf,
    /// Saved setups: whole mixers.
    pub setups_dir: PathBuf,
    /// Saved scenes: mixes without devices.
    pub scenes_dir: PathBuf,
    /// Equalizer presets of the user's own, all in one file.
    pub eq_presets_file: PathBuf,
    /// Copies of the config file from recent starts.
    pub backups_dir: PathBuf,
    /// Hotkeys, as JSON: their steps are requests, which may hold `null`,
    /// and TOML has no null.
    pub hotkeys_file: PathBuf,
}

impl Paths {
    /// The files under `~/.config/weir`, or, with `--config`, next to the
    /// file given.
    pub fn resolve(config_override: Option<PathBuf>) -> Self {
        let base = match &config_override {
            Some(p) => p
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
            None => dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("weir"),
        };
        Self {
            config_file: config_override.unwrap_or_else(|| base.join("config.toml")),
            setups_dir: base.join("setups"),
            scenes_dir: base.join("scenes"),
            eq_presets_file: base.join("eq-presets.toml"),
            backups_dir: base.join("backups"),
            hotkeys_file: base.join("hotkeys.json"),
        }
    }
}

/// The version of a file that has no `version` key: everything written
/// before versioning existed.
const UNVERSIONED: u32 = 1;

/// Read the configuration at `path` and bring it up to date. `None` when
/// there is no file.
pub fn load(path: &Path) -> Result<Option<(Config, LoadReport)>> {
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut table: toml::Table =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if !table.contains_key("version") {
        table.insert("version".into(), toml::Value::Integer(UNVERSIONED as i64));
    }
    let mut cfg: Config = table
        .try_into()
        .with_context(|| format!("parsing {}", path.display()))?;
    let report = migrate(&mut cfg);
    if let Some(v) = report.from_newer {
        // Keep the newer file as it is, since saving over it would drop
        // whatever this version does not understand.
        let keep = path.with_extension(format!("toml.v{v}.bak"));
        if !keep.exists() {
            std::fs::copy(path, &keep)
                .with_context(|| format!("keeping a copy as {}", keep.display()))?;
        }
    }
    Ok(Some((cfg, report)))
}

/// Write atomically (temp file + rename).
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    write_atomic_bytes(path, text.as_bytes())
}

/// Write `bytes` to `path` through a temporary file, so that a crash never
/// leaves half a file.
pub fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let tmp = path.with_extension(format!("{ext}.tmp"));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
    Ok(())
}

/// Write `cfg` to `path`, as the current format.
pub fn save(path: &Path, cfg: &Config) -> Result<()> {
    let cfg = Config {
        version: CONFIG_VERSION,
        // systemd keeps this; a copy here could only disagree with it.
        settings: Settings {
            start_at_login: None,
            ..cfg.settings.clone()
        },
        ..cfg.clone()
    };
    let text = toml::to_string_pretty(&cfg).context("serializing config")?;
    write_atomic(
        path,
        &format!("# Weir configuration. Edited live by weir-daemon.\n{text}"),
    )
}

/// How many copies of the config file [`backup`] keeps.
pub const BACKUPS_KEPT: usize = 10;

/// Copy the config file into `dir`, named after the time (UTC), and keep
/// only the newest [`BACKUPS_KEPT`]. Nothing is written when the file is the
/// same as the newest copy, so restarting does not push older copies out.
/// Called at startup, so there is always a copy from before this run changed
/// anything. Returns the copy written, if any.
pub fn backup(config: &Path, dir: &Path, now_secs: u64) -> Result<Option<PathBuf>> {
    if !config.exists() {
        return Ok(None);
    }
    let text = std::fs::read(config).with_context(|| format!("reading {}", config.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut copies: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("config-") && n.ends_with(".toml"))
        })
        .collect();
    // The names sort by time.
    copies.sort();
    if let Some(newest) = copies.last() {
        if std::fs::read(newest).ok().as_deref() == Some(&text[..]) {
            return Ok(None);
        }
    }
    let stamp = utc_stamp(now_secs);
    let mut path = dir.join(format!("config-{stamp}.toml"));
    let mut n = 1;
    while path.exists() {
        n += 1;
        path = dir.join(format!("config-{stamp}-{n}.toml"));
    }
    std::fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
    copies.push(path.clone());
    while copies.len() > BACKUPS_KEPT {
        let old = copies.remove(0);
        if let Err(e) = std::fs::remove_file(&old) {
            tracing::warn!("could not remove old backup {}: {e}", old.display());
        }
    }
    Ok(Some(path))
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

/// The file for the scene or setup (`what`) called `name` in `dir`. Names
/// are checked first, since they become file names.
fn saved_path(dir: &Path, name: &str, what: &str) -> Result<PathBuf> {
    if let Some(problem) = weir_protocol::library_name_problem(name) {
        bail!("invalid {what} name '{name}': {problem}");
    }
    Ok(dir.join(format!("{name}.toml")))
}

/// The names of the scenes or setups saved in `dir`.
pub fn list_saved(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("toml") {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

/// Read the scene or setup (`what`) called `name` from `dir`.
pub fn load_saved<T: serde::de::DeserializeOwned>(dir: &Path, name: &str, what: &str) -> Result<T> {
    let path = saved_path(dir, name, what)?;
    let text = std::fs::read_to_string(&path).map_err(|e| missing(e, what, name))?;
    toml::from_str(&text).with_context(|| format!("parsing {what} '{name}'"))
}

/// Write the scene or setup (`what`) called `name` to `dir`, replacing one
/// of the same name.
pub fn save_saved<T: Serialize>(dir: &Path, name: &str, what: &str, value: &T) -> Result<()> {
    let path = saved_path(dir, name, what)?;
    let text = toml::to_string_pretty(value).with_context(|| format!("serializing {what}"))?;
    write_atomic(&path, &text)
}

/// Delete the scene or setup (`what`) called `name` from `dir`.
pub fn delete_saved(dir: &Path, name: &str, what: &str) -> Result<()> {
    let path = saved_path(dir, name, what)?;
    std::fs::remove_file(&path).map_err(|e| missing(e, what, name))
}

/// The error for a saved file that could not be read or removed: plainly
/// "no scene called ..." when it does not exist, the system's reason
/// otherwise.
fn missing(e: std::io::Error, what: &str, name: &str) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow::anyhow!("no {what} called '{name}'")
    } else {
        anyhow::Error::new(e).context(format!("{what} '{name}'"))
    }
}

/// The equalizer presets file: a list of `[[preset]]` tables.
#[derive(Debug, Default, Serialize, Deserialize)]
struct EqPresetFile {
    #[serde(default, rename = "preset")]
    presets: Vec<StoredEqPreset>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredEqPreset {
    name: String,
    #[serde(default, rename = "band")]
    bands: Vec<EqBand>,
}

/// Read the user's equalizer presets. A missing file is no presets.
pub fn load_eq_presets(path: &Path) -> Result<Vec<EqPreset>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: EqPresetFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(file
        .presets
        .into_iter()
        .map(|p| EqPreset {
            name: p.name,
            bands: p.bands,
            builtin: false,
        })
        .collect())
}

/// The hotkeys file: a version, for changes to come, the hotkeys, their
/// groups, and the order of the list, which files from before groups do
/// not have.
#[derive(Debug, Serialize, Deserialize)]
struct HotkeysFile {
    version: u32,
    hotkeys: Vec<Hotkey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    groups: Vec<HotkeyGroup>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    order: Vec<HotkeyListItem>,
}

/// Every hotkey and every group of them, and the list they make: see
/// [`HotkeysInfo`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HotkeyList {
    pub hotkeys: Vec<Hotkey>,
    pub groups: Vec<HotkeyGroup>,
    pub order: Vec<HotkeyListItem>,
}

impl HotkeyList {
    /// Make everything agree after a change: hotkeys in a group that is
    /// gone are in none, every hotkey in no group and every group has one
    /// place in `order`, and `hotkeys` and `groups` are in the list's order.
    pub fn tidy(&mut self) {
        let groups = self.groups.clone();
        for h in &mut self.hotkeys {
            if !groups.iter().any(|g| g.id == h.group) {
                h.group = 0;
            }
        }
        self.order = hotkey_order(&self.order, &self.hotkeys, &self.groups);
        let mut hotkeys = Vec::with_capacity(self.hotkeys.len());
        let mut groups = Vec::with_capacity(self.groups.len());
        for item in &self.order {
            match *item {
                HotkeyListItem::Hotkey(id) => {
                    hotkeys.extend(self.hotkeys.iter().filter(|h| h.id == id).cloned());
                }
                HotkeyListItem::Group(id) => {
                    groups.extend(self.groups.iter().filter(|g| g.id == id).cloned());
                    hotkeys.extend(self.hotkeys.iter().filter(|h| h.group == id).cloned());
                }
            }
        }
        self.hotkeys = hotkeys;
        self.groups = groups;
    }
}

/// What [`HotkeysFile::version`] is now.
const HOTKEYS_VERSION: u32 = 1;

/// Read the hotkeys at `path`; none when there is no file.
pub fn load_hotkeys(path: &Path) -> Result<HotkeyList> {
    if !path.exists() {
        return Ok(HotkeyList::default());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: HotkeysFile =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let mut list = HotkeyList {
        hotkeys: file.hotkeys,
        groups: file.groups,
        order: file.order,
    };
    list.tidy();
    Ok(list)
}

/// Write `list` to `path`.
pub fn save_hotkeys(path: &Path, list: &HotkeyList) -> Result<()> {
    let file = HotkeysFile {
        version: HOTKEYS_VERSION,
        hotkeys: list.hotkeys.clone(),
        groups: list.groups.clone(),
        order: list.order.clone(),
    };
    let text = serde_json::to_string_pretty(&file).context("serializing hotkeys")?;
    write_atomic(path, &format!("{text}\n"))
}

/// Write the user's own equalizer presets to `path`.
pub fn save_eq_presets(path: &Path, presets: &[EqPreset]) -> Result<()> {
    let file = EqPresetFile {
        presets: presets
            .iter()
            .filter(|p| !p.builtin)
            .map(|p| StoredEqPreset {
                name: p.name.clone(),
                bands: p.bands.clone(),
            })
            .collect(),
    };
    let text = toml::to_string_pretty(&file).context("serializing equalizer presets")?;
    write_atomic(
        path,
        &format!("# Weir equalizer presets. Edited by weir-daemon.\n{text}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use weir_protocol::{ChannelLayout, EqBandKind, Gate, Strip, StripKind};

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("weir-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn times_turn_into_sortable_names() {
        assert_eq!(utc_stamp(0), "1970-01-01T00-00-00Z");
        assert_eq!(utc_stamp(1_700_000_000), "2023-11-14T22-13-20Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00-00-00Z");
    }

    #[test]
    fn backups_skip_repeats_and_keep_the_newest() {
        let dir = scratch_dir("backups");
        let cfg = dir.join("config.toml");
        let backups = dir.join("backups");
        assert!(backup(&cfg, &backups, 0).unwrap().is_none(), "no file yet");
        std::fs::write(&cfg, "a").unwrap();
        assert!(backup(&cfg, &backups, 100).unwrap().is_some());
        assert!(backup(&cfg, &backups, 200).unwrap().is_none(), "unchanged");
        for k in 0..15u64 {
            std::fs::write(&cfg, format!("v{k}")).unwrap();
            backup(&cfg, &backups, 1000 + k * 3600).unwrap();
        }
        let mut names: Vec<String> = std::fs::read_dir(&backups)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names.len(), BACKUPS_KEPT);
        let newest = std::fs::read_to_string(backups.join(names.last().unwrap())).unwrap();
        assert_eq!(newest, "v14");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_scene_is_named_plainly() {
        let dir = scratch_dir("missing");
        let e = load_saved::<weir_protocol::Scene>(&dir, "Nope", "scene").unwrap_err();
        assert_eq!(format!("{e:#}"), "no scene called 'Nope'");
        let e = delete_saved(&dir, "Nope", "setup").unwrap_err();
        assert_eq!(format!("{e:#}"), "no setup called 'Nope'");
    }

    #[test]
    fn eq_presets_round_trip() {
        let dir = scratch_dir("eq");
        let path = dir.join("eq-presets.toml");
        assert!(load_eq_presets(&path).unwrap().is_empty());
        let presets = vec![EqPreset {
            name: "My mic".into(),
            bands: vec![
                EqBand::new(EqBandKind::HighPass, 90.0, 0.0, 0.707),
                EqBand::new(EqBandKind::Peak, 3000.0, 2.5, 1.2),
            ],
            builtin: false,
        }];
        save_eq_presets(&path, &presets).unwrap();
        assert_eq!(load_eq_presets(&path).unwrap(), presets);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn effects_survive_the_config_file() {
        let mut strip = Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono);
        strip.eq.enabled = true;
        strip.eq.bands = vec![EqBand::new(EqBandKind::LowShelf, 120.0, -3.0, 0.707)];
        strip.gate = Gate {
            enabled: true,
            threshold_db: -38.0,
            ..Gate::default()
        };
        strip.denoise.enabled = true;
        // Send levels are keyed by bus, and TOML writes those keys as text.
        strip.routes.insert(1);
        strip.sends.insert(1, -6.0);
        let cfg = Config {
            version: CONFIG_VERSION,
            settings: Settings::default(),
            mixer: MixerState {
                strips: vec![
                    strip,
                    Strip::new(2, "Music", StripKind::Virtual, ChannelLayout::Stereo),
                ],
                buses: Vec::new(),
            },
            app_rules: vec![
                weir_protocol::AppRule {
                    app: "Spotify".into(),
                    strip: Some(2),
                },
                weir_protocol::AppRule {
                    app: "Discord".into(),
                    strip: None,
                },
            ],
            current_scene: Some("Streaming".into()),
            current_setup: None,
        };
        let dir = scratch_dir("cfg");
        let path = dir.join("config.toml");
        save(&path, &cfg).unwrap();
        let (back, report) = load(&path).unwrap().unwrap();
        assert_eq!(back.mixer, cfg.mixer);
        assert_eq!(back.app_rules, cfg.app_rules);
        assert_eq!(back.current_scene.as_deref(), Some("Streaming"));
        assert_eq!(back.current_setup, None);
        assert!(report.migrated.is_empty() && report.from_newer.is_none());
        // Strips without effects do not grow empty effect sections.
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("[mixer.strips.gate]").count(), 1, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_levels_survive_scenes_and_setups() {
        let mut mixer = MixerState {
            strips: vec![Strip::new(
                1,
                "Music",
                StripKind::Virtual,
                ChannelLayout::Stereo,
            )],
            buses: Vec::new(),
        };
        mixer.strips[0].routes.insert(2);
        mixer.strips[0].sends.insert(2, -4.5);
        let dir = scratch_dir("sends");
        save_saved(&dir, "Late night", "setup", &mixer).unwrap();
        let back: MixerState = load_saved(&dir, "Late night", "setup").unwrap();
        assert_eq!(back, mixer);
        let scene = weir_protocol::Scene::capture(&mixer);
        save_saved(&dir, "Streaming", "scene", &scene).unwrap();
        let back: weir_protocol::Scene = load_saved(&dir, "Streaming", "scene").unwrap();
        assert_eq!(back, scene);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod version_tests {
    use super::*;

    fn write(dir: &Path, text: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn files_from_before_versioning_read_as_version_1() {
        let dir = std::env::temp_dir().join(format!("weir-ver-old-{}", std::process::id()));
        let path = write(&dir, "[settings]\nmeter_rate_hz = 20\n");
        let (cfg, report) = load(&path).unwrap().unwrap();
        assert_eq!(cfg.version, CONFIG_VERSION);
        assert_eq!(cfg.settings.meter_rate_hz, 20);
        assert!(report.from_newer.is_none());
        save(&path, &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(&format!("version = {CONFIG_VERSION}")),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_levels_read_back_from_files_already_written() {
        // As 0.1 wrote them, which it then could not read: a bare key and,
        // as a hand edit might have it, a quoted one.
        let dir = std::env::temp_dir().join(format!("weir-ver-sends-{}", std::process::id()));
        let path = write(
            &dir,
            "version = 3\n\n[[mixer.strips]]\nid = 1\nname = \"Music\"\nkind = \"virtual\"\n\
             routes = [1, 2]\n\n[mixer.strips.sends]\n1 = -6.0\n\"2\" = 3.5\n",
        );
        let (cfg, _) = load(&path).unwrap().unwrap();
        let sends: Vec<_> = cfg.mixer.strips[0].sends.clone().into_iter().collect();
        assert_eq!(sends, vec![(1, -6.0), (2, 3.5)]);
        let path = write(
            &dir,
            "[[mixer.strips]]\nid = 1\nname = \"Music\"\nkind = \"virtual\"\n\n\
             [mixer.strips.sends]\nmain = -6.0\n",
        );
        let err = format!("{:#}", load(&path).unwrap_err());
        assert!(err.contains("strip or bus number"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_1_files_get_the_limiter_on_their_virtual_buses() {
        let dir = std::env::temp_dir().join(format!("weir-ver-lim-{}", std::process::id()));
        let path = write(
            &dir,
            "[[mixer.buses]]\nid = 1\nname = \"Headset\"\nkind = \"hardware\"\n\n\
             [[mixer.buses]]\nid = 2\nname = \"Stream\"\nkind = \"virtual\"\n",
        );
        let (cfg, report) = load(&path).unwrap().unwrap();
        assert!(!cfg.mixer.buses[0].limiter.enabled);
        assert!(cfg.mixer.buses[1].limiter.enabled);
        assert_eq!(report.migrated.len(), 1, "{:?}", report.migrated);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_2_files_keep_the_order_the_window_showed() {
        let dir = std::env::temp_dir().join(format!("weir-ver-order-{}", std::process::id()));
        let path = write(
            &dir,
            "version = 2\n\
             [[mixer.strips]]\nid = 1\nname = \"Music\"\nkind = \"virtual\"\n\n\
             [[mixer.strips]]\nid = 2\nname = \"Mic\"\nkind = \"hardware\"\n\n\
             [[mixer.strips]]\nid = 3\nname = \"Game\"\nkind = \"virtual\"\n",
        );
        let (cfg, report) = load(&path).unwrap().unwrap();
        let order: Vec<u32> = cfg.mixer.strips.iter().map(|s| s.id).collect();
        assert_eq!(order, [2, 1, 3]);
        assert_eq!(report.migrated.len(), 1, "{:?}", report.migrated);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn starting_at_login_is_left_to_systemd() {
        let dir = std::env::temp_dir().join(format!("weir-login-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.settings.start_at_login = Some(true);
        save(&path, &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("start_at_login"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_file_is_kept_before_anything_overwrites_it() {
        let dir = std::env::temp_dir().join(format!("weir-ver-new-{}", std::process::id()));
        let newer = CONFIG_VERSION + 5;
        let path = write(
            &dir,
            &format!("version = {newer}\n[settings]\ntray = false\n"),
        );
        let (cfg, report) = load(&path).unwrap().unwrap();
        assert_eq!(report.from_newer, Some(newer));
        assert!(!cfg.settings.tray);
        assert!(dir.join(format!("config.toml.v{newer}.bak")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
