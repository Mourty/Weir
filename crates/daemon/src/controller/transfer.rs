//! Exporting settings to files and importing them again: the requests of
//! `weir_protocol::transfer`, on top of the files in `crate::transfer`.
//!
//! Importing reads and checks the file twice, once to say what it holds
//! (`inspect_import`) and again to import it, so that nothing rests on the
//! file staying the same in between. Strips and buses travel by name:
//! hotkeys' steps, app rules and a solo cue are numbered here and named in
//! files, and named again in this mixer's numbers when imported.

use super::handlers::{check_bands, is_builtin_eq_preset, NAME_MAX};
use super::{names, Controller};
use crate::config::{self, HotkeyList};
use crate::transfer::{self as files, *};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use tracing::info;
use weir_protocol::*;

/// A failure, as the client is told it.
fn failed(e: anyhow::Error) -> RpcError {
    RpcError::application(format!("{e:#}"))
}

/// `path` as a path the daemon can use: whole, since the daemon's working
/// folder is not the client's.
fn whole_path(path: &str) -> Result<PathBuf, RpcError> {
    let p = PathBuf::from(path.trim());
    if !p.is_absolute() {
        return Err(RpcError::invalid_params(format!(
            "give the whole path, starting with /, not '{path}'"
        )));
    }
    Ok(p)
}

/// Whether `path` names a bare `.json` rather than a `.zip`.
fn is_json(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

/// What an item holds, once read and checked.
#[derive(Debug, Clone)]
enum Content {
    Scene(Scene),
    Setup(MixerState),
    /// With strips and buses by name, and its group's name.
    Hotkey(Hotkey, Option<String>),
    EqPreset(Vec<EqBand>),
    AppRules(Vec<RuleFile>),
    WindowLook(Value),
    Mixer(MixerPrefs),
    AudioTiming(AudioTiming),
    StartAtLogin(bool),
    /// Nothing that can be imported: see the item's `broken`.
    Broken,
}

/// One item of a file to import.
#[derive(Debug, Clone)]
struct Found {
    item: ImportItem,
    content: Content,
}

/// A file to import, read and checked against what is here.
struct Examined {
    inspection: ImportInspection,
    found: Vec<Found>,
    list: Option<HotkeyListFile>,
}

/// What there is here already, for telling which names are taken.
struct Here {
    mixer: MixerState,
    scenes: Vec<String>,
    setups: Vec<String>,
    presets: Vec<EqPreset>,
    hotkeys: HotkeyList,
    rules: Vec<AppRule>,
}

impl Here {
    /// Whether something of `kind` is called `name` here.
    fn has(&self, kind: ExportKind, name: &str) -> bool {
        match kind {
            ExportKind::Scene => self.scenes.iter().any(|n| n == name),
            ExportKind::Setup => self.setups.iter().any(|n| n == name),
            ExportKind::Hotkey => self
                .hotkeys
                .hotkeys
                .iter()
                .any(|h| h.name.eq_ignore_ascii_case(name)),
            ExportKind::EqPreset => self
                .presets
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(name)),
            ExportKind::AppRules | ExportKind::Preferences => false,
        }
    }
}

/// The longest name things of `kind` may have.
fn name_max(kind: ExportKind) -> usize {
    match kind {
        ExportKind::Scene | ExportKind::Setup => LIBRARY_NAME_MAX,
        ExportKind::Hotkey => HOTKEY_NAME_MAX,
        _ => NAME_MAX,
    }
}

/// What is wrong with `name` as the name of something of `kind`.
fn name_problem(kind: ExportKind, name: &str) -> Option<String> {
    match kind {
        ExportKind::Scene | ExportKind::Setup => library_name_problem(name).map(str::to_string),
        _ if name.trim().is_empty() => Some("it has no name".into()),
        _ if name.chars().count() > name_max(kind) => Some(format!(
            "its name is longer than {} characters",
            name_max(kind)
        )),
        _ => None,
    }
}

/// An item as the lists show it before checks.
fn item(id: &str, kind: ExportKind, name: &str) -> ImportItem {
    ImportItem {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        summary: String::new(),
        group: None,
        taken: false,
        free_name: None,
        keys_taken: Vec::new(),
        missing: Vec::new(),
        setups: Vec::new(),
        setup_items: Vec::new(),
        broken: None,
        note: None,
    }
}

/// How an item is spoken of in results: "scene 'Gaming'", "app rules".
fn spoken(item: &ImportItem) -> String {
    match item.kind {
        ExportKind::AppRules => "app rules".into(),
        ExportKind::Preferences => format!("preferences: {}", item.name.to_lowercase()),
        kind => format!("{} '{}'", kind.word(), item.name),
    }
}

/// The strips and buses `params` names that `m` has none called. Numbers
/// are strips or buses that were gone when the file was written.
fn missing_in(
    method: &str,
    params: &Value,
    m: &MixerState,
    missing: &mut BTreeSet<MissingTarget>,
    gone: &mut bool,
) {
    let mut params = params.clone();
    let _ = visit_targets::<()>(method, &mut params, &mut |kind, v| {
        match v {
            Value::String(name) if find_named(m, kind, name).is_none() => {
                missing.insert(MissingTarget {
                    kind,
                    name: name.clone(),
                });
            }
            Value::Number(_) => *gone = true,
            _ => {}
        }
        Ok(())
    });
}

/// `params` with the names `map_strips` and `map_buses` give other names
/// for replaced.
fn mapped(method: &str, params: &mut Value, p: &ImportParams) {
    let _ = visit_targets::<()>(method, params, &mut |kind, v| {
        if let Value::String(name) = v {
            let map = match kind {
                TargetKind::Strip => &p.map_strips,
                TargetKind::Bus => &p.map_buses,
            };
            if let Some(to) = map.get(name.as_str()) {
                *name = to.clone();
            }
        }
        Ok(())
    });
}

impl Controller {
    /// When and by which Weir an export is written.
    fn stamp(&self) -> Stamp {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Stamp {
            weir_version: env!("CARGO_PKG_VERSION").to_string(),
            exported: utc_stamp(secs),
        }
    }

    /// Write what `p` asks for to a `.zip`, or one item to a `.json`.
    pub(super) fn export_settings(&self, p: ExportParams) -> Result<Value, RpcError> {
        let path = whole_path(&p.path)?;
        let bare = is_json(&path);
        let mixer = self.mixer();
        let mut out: Vec<(ExportKind, String, Body)> = Vec::new();

        let saved = |dir: &Path, asked: &[String], all: bool| -> Vec<String> {
            if all {
                config::list_saved(dir)
            } else {
                asked.to_vec()
            }
        };
        for name in saved(&self.paths.scenes_dir, &p.scenes, p.all) {
            let scene: Scene =
                config::load_saved(&self.paths.scenes_dir, &name, "scene").map_err(failed)?;
            out.push((
                ExportKind::Scene,
                name.clone(),
                Body::Scene(SceneFile { name, scene }),
            ));
        }
        for name in saved(&self.paths.setups_dir, &p.setups, p.all) {
            let setup: MixerState =
                config::load_saved(&self.paths.setups_dir, &name, "setup").map_err(failed)?;
            out.push((
                ExportKind::Setup,
                name.clone(),
                Body::Setup(SetupFile { name, setup }),
            ));
        }

        // Hotkeys go in the list's order, whatever order they were asked
        // for in.
        let list = self.hotkey_list();
        let mut wanted: BTreeSet<HotkeyId> = BTreeSet::new();
        if p.all {
            wanted.extend(list.hotkeys.iter().map(|h| h.id));
        }
        for key in &p.hotkeys {
            wanted.insert(self.find_hotkey(key)?);
        }
        let group_name = |id: HotkeyGroupId| {
            list.groups
                .iter()
                .find(|g| g.id == id)
                .map(|g| g.name.clone())
        };
        for h in list.hotkeys.iter().filter(|h| wanted.contains(&h.id)) {
            let mut hotkey = h.clone();
            hotkey.id = 0;
            hotkey.group = 0;
            // Hotkeys keep names; only one saved before they did may still
            // have an id.
            hotkey.targets_by_name(&mixer);
            out.push((
                ExportKind::Hotkey,
                h.name.clone(),
                Body::Hotkey(HotkeyFile {
                    group: group_name(h.group),
                    hotkey,
                }),
            ));
        }

        let presets = self.eq_presets();
        let preset_names: Vec<String> = if p.all {
            presets
                .iter()
                .filter(|x| !x.builtin)
                .map(|x| x.name.clone())
                .collect()
        } else {
            p.eq_presets.clone()
        };
        for name in preset_names {
            let Some(preset) = presets
                .iter()
                .find(|x| x.name.eq_ignore_ascii_case(name.trim()))
            else {
                return Err(RpcError::application(format!(
                    "no equalizer preset called '{name}'"
                )));
            };
            if preset.builtin {
                return Err(RpcError::application(format!(
                    "'{}' comes with Weir, so it is not exported",
                    preset.name
                )));
            }
            out.push((
                ExportKind::EqPreset,
                preset.name.clone(),
                Body::EqPreset(EqPresetFile {
                    name: preset.name.clone(),
                    bands: preset.bands.clone(),
                }),
            ));
        }

        let rules = self.inner.lock().unwrap().app_rules.clone();
        if p.app_rules || (p.all && !rules.is_empty()) {
            let rules = rules
                .iter()
                .map(|r| RuleFile {
                    app: r.app.clone(),
                    strip: r
                        .strip
                        .and_then(|id| mixer.strip(id))
                        .map(|s| s.name.clone()),
                })
                .collect();
            out.push((
                ExportKind::AppRules,
                "app-rules".into(),
                Body::AppRules(AppRulesFile { rules }),
            ));
        }

        if let Some(prefs) = self.preferences_to_export(&p, &mixer)? {
            out.push((
                ExportKind::Preferences,
                "preferences".into(),
                Body::Preferences(prefs),
            ));
        }

        if out.is_empty() {
            return Err(RpcError::application(
                "nothing to export: name what to export, or ask for all",
            ));
        }
        let stamp = self.stamp();
        let written: Vec<String> = if bare {
            if out.len() > 1 {
                return Err(RpcError::application(
                    "a .json holds one thing; export to a .zip for more",
                ));
            }
            config::write_atomic(&path, &files::write_file(&out[0].2, &stamp)).map_err(failed)?;
            vec![path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())]
        } else {
            let mut zipped = self.zip_entries(out, &list, &wanted, &stamp);
            let names: Vec<String> = zipped.iter().map(|(p, _)| p.clone()).collect();
            let manifest = Body::Manifest(ManifestFile {
                files: names.clone(),
            });
            zipped.insert(
                0,
                (MANIFEST.to_string(), files::write_file(&manifest, &stamp)),
            );
            let bytes = files::zip_files(&zipped, &stamp.exported).map_err(failed)?;
            config::write_atomic_bytes(&path, &bytes).map_err(failed)?;
            names
        };
        info!("exported {} settings to {}", written.len(), path.display());
        Ok(to_json(&ExportResult {
            path: path.display().to_string(),
            files: written,
        }))
    }

    /// The files of a `.zip` for `out`, each in its kind's folder, with the
    /// hotkeys' groups and order beside the hotkeys.
    fn zip_entries(
        &self,
        out: Vec<(ExportKind, String, Body)>,
        list: &HotkeyList,
        wanted: &BTreeSet<HotkeyId>,
        stamp: &Stamp,
    ) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        for kind in ExportKind::ALL {
            let mine: Vec<&(ExportKind, String, Body)> =
                out.iter().filter(|(k, ..)| *k == kind).collect();
            if mine.is_empty() {
                continue;
            }
            let item_names: Vec<String> = mine.iter().map(|(_, n, _)| n.clone()).collect();
            let reserved: &[&str] = if kind == ExportKind::Hotkey {
                &[HOTKEY_LIST]
            } else {
                &[]
            };
            let paths = files::paths_for(kind, &item_names, reserved);
            for (path, (_, _, body)) in paths.into_iter().zip(mine) {
                entries.push((path, files::write_file(body, stamp)));
            }
            if kind == ExportKind::Hotkey {
                let by_id = |id: HotkeyId| list.hotkeys.iter().find(|h| h.id == id);
                let used: BTreeSet<HotkeyGroupId> = list
                    .hotkeys
                    .iter()
                    .filter(|h| wanted.contains(&h.id))
                    .map(|h| h.group)
                    .collect();
                let groups: Vec<&HotkeyGroup> = list
                    .groups
                    .iter()
                    .filter(|g| used.contains(&g.id))
                    .collect();
                let order = list
                    .order
                    .iter()
                    .filter_map(|place| match *place {
                        HotkeyListItem::Hotkey(id) if wanted.contains(&id) => {
                            by_id(id).map(|h| PlaceFile::Hotkey(h.name.clone()))
                        }
                        HotkeyListItem::Group(id) if used.contains(&id) => groups
                            .iter()
                            .find(|g| g.id == id)
                            .map(|g| PlaceFile::Group(g.name.clone())),
                        _ => None,
                    })
                    .collect();
                let body = Body::HotkeyList(HotkeyListFile {
                    groups: groups
                        .iter()
                        .map(|g| GroupFile {
                            name: g.name.clone(),
                            enabled: g.enabled,
                        })
                        .collect(),
                    order,
                });
                entries.push((HOTKEY_LIST.to_string(), files::write_file(&body, stamp)));
            }
        }
        entries
    }

    /// The parts of the preferences `p` asks for, if any.
    fn preferences_to_export(
        &self,
        p: &ExportParams,
        mixer: &MixerState,
    ) -> Result<Option<PreferencesFile>, RpcError> {
        let parts: Vec<PreferencePart> = if p.all {
            PreferencePart::ALL.to_vec()
        } else {
            p.preferences.clone()
        };
        if parts.is_empty() {
            return Ok(None);
        }
        let s = self.settings();
        let mut prefs = PreferencesFile::default();
        for part in parts {
            match part {
                PreferencePart::WindowLook => match &p.window_look {
                    Some(Value::Object(look)) => {
                        let kept: serde_json::Map<String, Value> = look
                            .iter()
                            .filter(|(k, _)| WINDOW_LOOK_KEYS.contains(&k.as_str()))
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect();
                        prefs.window_look = Some(Value::Object(kept));
                    }
                    // Everything there is, and the window's look is not
                    // here to give.
                    None if p.all => {}
                    _ => {
                        return Err(RpcError::invalid_params(
                            "the window look comes from the window: give it as window_look",
                        ))
                    }
                },
                PreferencePart::Mixer => {
                    prefs.mixer = Some(MixerPrefs {
                        meter_rate_hz: s.meter_rate_hz,
                        startup: s.startup,
                        tray: s.tray,
                        tray_icon: s.tray_icon,
                        solo: match s.solo {
                            SoloMode::Cue(b) => mixer
                                .bus(b)
                                .map_or(SoloFile::Exclusive, |b| SoloFile::Cue(b.name.clone())),
                            SoloMode::Exclusive => SoloFile::Exclusive,
                        },
                    });
                }
                PreferencePart::AudioTiming => {
                    prefs.audio_timing = Some(AudioTiming {
                        sample_rate: s.sample_rate,
                        quantum: s.quantum,
                    });
                }
                PreferencePart::StartAtLogin => match s.start_at_login {
                    Some(on) => prefs.start_at_login = Some(on),
                    None if p.all => {}
                    None => {
                        return Err(RpcError::application(
                            "whether Weir starts at login is not known here",
                        ))
                    }
                },
            }
        }
        let empty = prefs == PreferencesFile::default();
        Ok((!empty).then_some(prefs))
    }

    /// Say what the file at `p.path` holds, and what importing it would
    /// meet. Changes nothing.
    pub(super) fn inspect_import(&self, p: InspectImportParams) -> Result<Value, RpcError> {
        Ok(to_json(&self.examine(&p.path)?.inspection))
    }

    /// What is here now, for checks.
    fn here(&self) -> Here {
        Here {
            mixer: self.mixer(),
            scenes: config::list_saved(&self.paths.scenes_dir),
            setups: config::list_saved(&self.paths.setups_dir),
            presets: self.eq_presets(),
            hotkeys: self.hotkey_list(),
            rules: self.inner.lock().unwrap().app_rules.clone(),
        }
    }

    /// Read the file at `path` and check each item in it against what is
    /// here.
    fn examine(&self, path: &str) -> Result<Examined, RpcError> {
        let file = whole_path(path)?;
        let read = files::read_export(&file).map_err(failed)?;
        let here = self.here();
        let mut ex = Examined {
            inspection: ImportInspection {
                path: file.display().to_string(),
                ..Default::default()
            },
            found: Vec::new(),
            list: None,
        };
        for f in read {
            // Who wrote it: the manifest says for a .zip, the file itself
            // for a bare one.
            if f.path == MANIFEST || ex.inspection.weir_version.is_empty() {
                ex.inspection.weir_version = f.weir_version.clone();
                ex.inspection.exported = f.exported.clone();
            }
            let body = match f.body {
                Ok(body) => body,
                Err(reason) => {
                    match kind_of_path(&f.path).filter(|_| f.path != HOTKEY_LIST) {
                        Some(kind) => {
                            let name = Path::new(&f.path)
                                .file_stem()
                                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                            let mut it = item(&f.path, kind, &name);
                            it.broken = Some(reason);
                            ex.found.push(Found {
                                item: it,
                                content: Content::Broken,
                            });
                        }
                        None => ex.inspection.problems.push(format!("{}: {reason}", f.path)),
                    }
                    continue;
                }
            };
            match body {
                Body::Manifest(_) => {}
                Body::HotkeyList(list) => ex.list = Some(list),
                Body::Preferences(prefs) => ex
                    .found
                    .extend(self.check_preferences(&f.path, prefs, &here)),
                body => ex.found.push(self.check(&f.path, body, &here)),
            }
        }
        ex.found.sort_by_key(|f| f.item.kind);
        self.place_in_setups(&mut ex.found, &here);
        // A free name must be free of the other imported items too.
        let names: Vec<(ExportKind, String)> = ex
            .found
            .iter()
            .map(|f| (f.item.kind, f.item.name.to_lowercase()))
            .collect();
        for f in &mut ex.found {
            if f.item.taken {
                let kind = f.item.kind;
                let taken =
                    |n: &str| here.has(kind, n) || names.contains(&(kind, n.to_lowercase()));
                f.item.free_name = Some(free_name(&f.item.name, name_max(kind), taken));
            }
        }
        let missing: BTreeSet<MissingTarget> = ex
            .found
            .iter()
            .flat_map(|f| f.item.missing.iter().cloned())
            .collect();
        ex.inspection.missing = missing.into_iter().collect();
        ex.inspection.items = ex.found.iter().map(|f| f.item.clone()).collect();
        Ok(ex)
    }

    /// A hotkey naming strips or buses this mixer lacks may be meant for
    /// another setup: what a setup saved here or one in the file has is
    /// not missing, and the item says which setups have it. Once nothing
    /// is missing, the hotkey is checked as `set_hotkey` would.
    fn place_in_setups(&self, found: &mut [Found], here: &Here) {
        if !found
            .iter()
            .any(|f| f.item.kind == ExportKind::Hotkey && !f.item.missing.is_empty())
        {
            return;
        }
        let saved: Vec<(String, MixerState)> = here
            .setups
            .iter()
            .filter_map(|name| {
                let m = config::load_saved(&self.paths.setups_dir, name, "setup").ok()?;
                Some((name.clone(), m))
            })
            .collect();
        // By item id, and name.
        let in_file: Vec<(String, String, MixerState)> = found
            .iter()
            .filter_map(|f| match &f.content {
                Content::Setup(m) => Some((f.item.id.clone(), f.item.name.clone(), m.clone())),
                _ => None,
            })
            .collect();
        let has = |m: &MixerState, t: &MissingTarget| find_named(m, t.kind, &t.name).is_some();
        for Found { item, content } in found.iter_mut() {
            let Content::Hotkey(h, _) = content else {
                continue;
            };
            let elsewhere: Vec<MissingTarget> = item
                .missing
                .iter()
                .filter(|t| {
                    saved.iter().any(|(_, m)| has(m, t))
                        || in_file.iter().any(|(_, _, m)| has(m, t))
                })
                .cloned()
                .collect();
            if elsewhere.is_empty() {
                continue;
            }
            item.missing.retain(|t| !elsewhere.contains(t));
            let saved_with: Vec<&String> = saved
                .iter()
                .filter(|(_, m)| elsewhere.iter().any(|t| has(m, t)))
                .map(|(name, _)| name)
                .collect();
            let file_with: Vec<&(String, String, MixerState)> = in_file
                .iter()
                .filter(|(_, _, m)| elsewhere.iter().any(|t| has(m, t)))
                .collect();
            item.setups = saved_with.iter().map(|n| n.to_string()).collect();
            item.setup_items = file_with.iter().map(|(id, _, _)| id.clone()).collect();
            let mut which: Vec<String> = saved_with.iter().map(|n| format!("'{n}'")).collect();
            which.extend(
                file_with
                    .iter()
                    .map(|(_, name, _)| format!("'{name}' (in this file)")),
            );
            let what: Vec<String> = elsewhere
                .iter()
                .map(|t| format!("{} called '{}'", t.kind.word(), t.name))
                .collect();
            let (setups, have, while_) = if which.len() == 1 {
                ("the setup", "has", "it")
            } else {
                ("the setups", "have", "one of them")
            };
            item.note = Some(format!(
                "This mixer has no {}; {setups} {} {have} {}, and the hotkey works while \
                 {while_} is loaded.",
                what.join(" or "),
                which.join(", "),
                if what.len() == 1 { "one" } else { "them" },
            ));
            if item.missing.is_empty() {
                let extra = in_file.iter().map(|(_, _, m)| m.clone()).collect();
                if let Err(e) = self.checked_hotkey_with(h.clone(), extra) {
                    item.broken = Some(e.message);
                }
            }
        }
    }

    /// Check one scene, setup, hotkey, preset or set of app rules.
    fn check(&self, id: &str, body: Body, here: &Here) -> Found {
        let (kind, name) = match &body {
            Body::Scene(b) => (ExportKind::Scene, b.name.trim().to_string()),
            Body::Setup(b) => (ExportKind::Setup, b.name.trim().to_string()),
            Body::Hotkey(b) => (ExportKind::Hotkey, b.hotkey.name.trim().to_string()),
            Body::EqPreset(b) => (ExportKind::EqPreset, b.name.trim().to_string()),
            _ => (ExportKind::AppRules, "App rules".to_string()),
        };
        let mut it = item(id, kind, &name);
        if kind != ExportKind::AppRules {
            if let Some(problem) = name_problem(kind, &name) {
                it.broken = Some(problem);
                return Found {
                    item: it,
                    content: Content::Broken,
                };
            }
            it.taken = here.has(kind, &name);
        }
        let content = match body {
            Body::Scene(b) => {
                it.summary = format!(
                    "{} strips, {} buses",
                    b.scene.strips.len(),
                    b.scene.buses.len()
                );
                Content::Scene(b.scene)
            }
            Body::Setup(b) => {
                let mut setup = b.setup;
                setup.normalize();
                it.summary = format!("{} strips, {} buses", setup.strips.len(), setup.buses.len());
                Content::Setup(setup)
            }
            Body::Hotkey(b) => {
                self.check_hotkey(&mut it, &b, here);
                Content::Hotkey(b.hotkey, b.group)
            }
            Body::EqPreset(b) => {
                if let Err(e) = check_bands(&b.bands) {
                    it.broken = Some(e.message);
                }
                if is_builtin_eq_preset(&name) {
                    it.note = Some(
                        "A preset that comes with Weir has this name, so this one can only \
                         come in under another."
                            .into(),
                    );
                }
                it.summary = format!("{} bands", b.bands.len());
                Content::EqPreset(b.bands)
            }
            Body::AppRules(b) => {
                let mut missing = BTreeSet::new();
                let mut hardware = Vec::new();
                for r in &b.rules {
                    let Some(strip) = &r.strip else { continue };
                    match here.mixer.find_strip(strip) {
                        None => {
                            missing.insert(MissingTarget {
                                kind: TargetKind::Strip,
                                name: strip.clone(),
                            });
                        }
                        Some(s) if s.kind != StripKind::Virtual => hardware.push(s.name.clone()),
                        Some(_) => {}
                    }
                }
                it.missing = missing.into_iter().collect();
                it.summary = match b.rules.len() {
                    1 => "1 rule".into(),
                    n => format!("{n} rules"),
                };
                let replaced: Vec<&str> = b
                    .rules
                    .iter()
                    .filter(|r| {
                        here.rules
                            .iter()
                            .any(|x| x.app.eq_ignore_ascii_case(r.app.trim()))
                    })
                    .map(|r| r.app.as_str())
                    .collect();
                let mut notes = Vec::new();
                if !replaced.is_empty() {
                    notes.push(format!("Replaces your rules for {}.", replaced.join(", ")));
                }
                if !hardware.is_empty() {
                    notes.push(format!(
                        "{} is not a virtual strip here, so its rules are left out.",
                        hardware.join(", ")
                    ));
                }
                it.note = (!notes.is_empty()).then(|| notes.join(" "));
                Content::AppRules(b.rules)
            }
            _ => Content::Broken,
        };
        if it.broken.is_some() {
            return Found {
                item: it,
                content: Content::Broken,
            };
        }
        Found { item: it, content }
    }

    /// Check an imported hotkey: the strips and buses it names, whether it
    /// would be taken as it is, and the keys hotkeys here have already.
    fn check_hotkey(&self, it: &mut ImportItem, b: &HotkeyFile, here: &Here) {
        let h = &b.hotkey;
        it.group = b.group.clone();
        it.summary = describe_hotkey(h, &here.mixer);
        let mut missing = BTreeSet::new();
        let mut gone = false;
        for step in h.steps.iter().chain(&h.release_steps) {
            missing_in(
                &step.method,
                &step.params,
                &here.mixer,
                &mut missing,
                &mut gone,
            );
        }
        if gone {
            it.broken =
                Some("it uses a strip or bus that was removed before it was exported".into());
            return;
        }
        it.missing = missing.into_iter().collect();
        // Its steps can be checked only once their strips and buses are
        // here; its keys always.
        if it.missing.is_empty() {
            if let Err(e) = self.checked_hotkey(h.clone()) {
                it.broken = Some(e.message);
                return;
            }
        }
        let mut keys = Vec::new();
        for text in &h.keys {
            match KeyCombo::parse(text) {
                Ok(combo) => keys.push(combo.to_string()),
                Err(e) => {
                    it.broken = Some(e);
                    return;
                }
            }
        }
        for keys in &keys {
            if let Some(other) = here.hotkeys.hotkeys.iter().find(|o| o.keys.contains(keys)) {
                it.keys_taken.push(KeysTaken {
                    keys: keys.clone(),
                    by: other.name.clone(),
                });
            }
        }
    }

    /// One item for each part of the preferences in a file.
    fn check_preferences(&self, id: &str, prefs: PreferencesFile, here: &Here) -> Vec<Found> {
        let mut found = Vec::new();
        let mut part = |part: PreferencePart, content: Content, note: Option<String>| {
            let mut it = item(
                &format!("{id}#{}", part.key()),
                ExportKind::Preferences,
                part.label(),
            );
            it.summary = part.summary().into();
            it.note = note;
            if let Content::Mixer(MixerPrefs {
                solo: SoloFile::Cue(bus),
                ..
            }) = &content
            {
                if here.mixer.find_bus(bus).is_none() {
                    it.missing.push(MissingTarget {
                        kind: TargetKind::Bus,
                        name: bus.clone(),
                    });
                }
            }
            found.push(Found { item: it, content });
        };
        if let Some(look) = prefs.window_look {
            part(PreferencePart::WindowLook, Content::WindowLook(look), None);
        }
        if let Some(mixer) = prefs.mixer {
            part(PreferencePart::Mixer, Content::Mixer(mixer), None);
        }
        if let Some(timing) = prefs.audio_timing {
            part(
                PreferencePart::AudioTiming,
                Content::AudioTiming(timing),
                Some("These suit the sound hardware of the computer they came from.".into()),
            );
        }
        if let Some(on) = prefs.start_at_login {
            let note = if here_can_start_at_login(self) {
                if on {
                    "Weir will start when you log in to this computer."
                } else {
                    "Weir will not start by itself when you log in to this computer."
                }
            } else {
                "Weir cannot start at login on this computer, so this is left out."
            };
            part(
                PreferencePart::StartAtLogin,
                Content::StartAtLogin(on),
                Some(note.into()),
            );
        }
        found
    }

    /// Import what `p` asks for from the file at `p.path`.
    pub(super) fn import_settings(&self, p: ImportParams) -> Result<Value, RpcError> {
        let ex = self.examine(&p.path)?;
        let here = self.here();
        for (map, kind) in [
            (&p.map_strips, TargetKind::Strip),
            (&p.map_buses, TargetKind::Bus),
        ] {
            for to in map.values() {
                if names::find(&here.mixer, kind, to).is_none() {
                    return Err(RpcError::application(format!(
                        "no {} called '{to}'",
                        kind.word()
                    )));
                }
            }
        }
        let replace_all = p.hotkeys == HotkeyImport::ReplaceAll;
        let mut result = ImportResult::default();
        let mut plan: Vec<Planned> = Vec::new();
        let mut used: BTreeSet<(ExportKind, String)> = BTreeSet::new();
        for f in ex.found {
            let id = f.item.id.clone();
            if p.items.as_ref().is_some_and(|ids| !ids.contains(&id)) {
                continue;
            }
            let said = spoken(&f.item);
            if let Some(reason) = &f.item.broken {
                result.skipped.push(format!("{said}: {reason}"));
                continue;
            }
            let unmapped: Vec<String> = f
                .item
                .missing
                .iter()
                .filter(|m| {
                    let map = match m.kind {
                        TargetKind::Strip => &p.map_strips,
                        TargetKind::Bus => &p.map_buses,
                    };
                    !map.contains_key(&m.name)
                })
                .map(|m| format!("{} '{}'", m.kind.word(), m.name))
                .collect();
            if !unmapped.is_empty() {
                result.skipped.push(format!(
                    "{said}: this mixer has no {}",
                    unmapped.join(" or ")
                ));
                continue;
            }
            let kind = f.item.kind;
            let taken = f.item.taken && !(replace_all && kind == ExportKind::Hotkey);
            let choice = p.choices.get(&id).cloned().unwrap_or(if taken {
                match p.when_taken {
                    WhenTaken::Skip => ImportChoice::Skip,
                    WhenTaken::Replace => ImportChoice::Replace,
                    WhenTaken::KeepBoth => ImportChoice::Rename(
                        f.item.free_name.clone().unwrap_or(f.item.name.clone()),
                    ),
                }
            } else {
                ImportChoice::Replace
            });
            let (name, replace) = match choice {
                ImportChoice::Skip if taken => {
                    result
                        .skipped
                        .push(format!("{said}: there is one called that already"));
                    continue;
                }
                ImportChoice::Skip => {
                    result.skipped.push(format!("{said}: left out"));
                    continue;
                }
                ImportChoice::Replace => {
                    if taken && kind == ExportKind::EqPreset && is_builtin_eq_preset(&f.item.name) {
                        result.skipped.push(format!(
                            "{said}: a preset that comes with Weir has this name, and it \
                             cannot be replaced"
                        ));
                        continue;
                    }
                    (f.item.name.clone(), taken)
                }
                ImportChoice::Rename(new) => {
                    let new = new.trim().to_string();
                    let clash = (here.has(kind, &new)
                        && !(replace_all && kind == ExportKind::Hotkey))
                        || (kind == ExportKind::EqPreset && is_builtin_eq_preset(&new));
                    if let Some(problem) = name_problem(kind, &new) {
                        result
                            .skipped
                            .push(format!("{said}: '{new}' will not do: {problem}"));
                        continue;
                    }
                    if clash {
                        result
                            .skipped
                            .push(format!("{said}: there is one called '{new}' already"));
                        continue;
                    }
                    (new, false)
                }
            };
            if matches!(
                kind,
                ExportKind::Scene | ExportKind::Setup | ExportKind::Hotkey | ExportKind::EqPreset
            ) && !used.insert((kind, name.to_lowercase()))
            {
                result.skipped.push(format!(
                    "{said}: another {} imported now is called '{name}'",
                    kind.word()
                ));
                continue;
            }
            plan.push(Planned {
                item: f.item,
                content: f.content,
                name,
                replace,
            });
        }
        result.backup = self.back_up_for_import(&plan)?;
        self.import_library(&plan, &mut result);
        self.import_presets(&plan, &mut result);
        self.import_rules(&plan, &p, &here, &mut result);
        self.import_hotkeys(&plan, &p, ex.list.as_ref(), &mut result);
        self.import_preferences(&plan, &p, &here, &mut result);
        info!(
            "imported {} settings from {}",
            result.imported.len(),
            ex.inspection.path
        );
        Ok(to_json(&result))
    }

    /// Copies of what the import replaces, in a folder of their own under
    /// `backups`: the scenes and setups it writes over, and the files of
    /// presets, hotkeys and settings it changes.
    fn back_up_for_import(&self, plan: &[Planned]) -> Result<Option<String>, RpcError> {
        let mut copies: Vec<(PathBuf, String)> = Vec::new();
        let has = |kind: ExportKind| plan.iter().any(|x| x.item.kind == kind);
        for x in plan.iter().filter(|x| x.replace) {
            let (dir, folder) = match x.item.kind {
                ExportKind::Scene => (&self.paths.scenes_dir, "scenes"),
                ExportKind::Setup => (&self.paths.setups_dir, "setups"),
                _ => continue,
            };
            copies.push((
                dir.join(format!("{}.toml", x.name)),
                format!("{folder}/{}.toml", x.name),
            ));
        }
        if has(ExportKind::Hotkey) {
            copies.push((self.paths.hotkeys_file.clone(), "hotkeys.json".into()));
        }
        if has(ExportKind::EqPreset) {
            copies.push((self.paths.eq_presets_file.clone(), "eq-presets.toml".into()));
        }
        // The window's look is the window's, not in the daemon's files.
        let settings = plan.iter().any(|x| {
            matches!(
                x.content,
                Content::Mixer(_) | Content::AudioTiming(_) | Content::StartAtLogin(_)
            )
        });
        if has(ExportKind::AppRules) || settings {
            copies.push((self.paths.config_file.clone(), "config.toml".into()));
        }
        copies.retain(|(from, _)| from.exists());
        if copies.is_empty() {
            return Ok(None);
        }
        let dir = self
            .paths
            .backups_dir
            .join(format!("import-{}", self.stamp().exported));
        for (from, to) in &copies {
            let to = dir.join(to);
            let copied = to
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|_| std::fs::copy(from, &to));
            if let Err(e) = copied {
                return Err(RpcError::application(format!(
                    "could not keep a copy of {} before importing, so nothing was \
                     imported: {e}",
                    from.display()
                )));
            }
        }
        Ok(Some(dir.display().to_string()))
    }

    /// Scenes and setups, added to the library.
    fn import_library(&self, plan: &[Planned], result: &mut ImportResult) {
        let mut any = false;
        for x in plan {
            let saved = match &x.content {
                Content::Scene(scene) => {
                    config::save_saved(&self.paths.scenes_dir, &x.name, "scene", scene)
                }
                Content::Setup(setup) => {
                    config::save_saved(&self.paths.setups_dir, &x.name, "setup", setup)
                }
                _ => continue,
            };
            let said = format!("{} '{}'", x.item.kind.word(), x.name);
            match saved {
                Ok(()) => {
                    any = true;
                    result.imported.push(said);
                }
                Err(e) => result.skipped.push(format!("{said}: {e:#}")),
            }
        }
        if any {
            self.library_changed(None, None);
        }
    }

    /// Equalizer presets, added to the user's own.
    fn import_presets(&self, plan: &[Planned], result: &mut ImportResult) {
        let presets: Vec<(&Planned, &Vec<EqBand>)> = plan
            .iter()
            .filter_map(|x| match &x.content {
                Content::EqPreset(bands) => Some((x, bands)),
                _ => None,
            })
            .collect();
        if presets.is_empty() {
            return;
        }
        let edited = self.edit_eq_presets(|user| {
            for (x, bands) in &presets {
                let bands: Vec<EqBand> = bands.iter().map(|b| b.clamped()).collect();
                match user
                    .iter_mut()
                    .find(|u| u.name.eq_ignore_ascii_case(&x.name))
                {
                    Some(existing) => existing.bands = bands,
                    None => user.push(EqPreset {
                        name: x.name.clone(),
                        bands,
                        builtin: false,
                    }),
                }
            }
            user.sort_by_key(|u| u.name.to_lowercase());
            Ok(())
        });
        for (x, _) in presets {
            let said = format!("equalizer preset '{}'", x.name);
            match &edited {
                Ok(_) => result.imported.push(said),
                Err(e) => result.skipped.push(format!("{said}: {}", e.message)),
            }
        }
    }

    /// App rules: each replaces the rule here for the same application.
    fn import_rules(
        &self,
        plan: &[Planned],
        p: &ImportParams,
        here: &Here,
        result: &mut ImportResult,
    ) {
        let Some(rules) = plan.iter().find_map(|x| match &x.content {
            Content::AppRules(rules) => Some(rules),
            _ => None,
        }) else {
            return;
        };
        let mut merged = here.rules.clone();
        let mut left_out = Vec::new();
        for r in rules {
            let app = r.app.trim().to_string();
            if app.is_empty() {
                continue;
            }
            let strip = match &r.strip {
                None => None,
                Some(name) => {
                    let name = p.map_strips.get(name).unwrap_or(name);
                    match here.mixer.find_strip(name) {
                        Some(s) if s.kind == StripKind::Virtual => Some(s.id),
                        _ => {
                            left_out.push(app);
                            continue;
                        }
                    }
                }
            };
            merged.retain(|x| !x.app.eq_ignore_ascii_case(&app));
            merged.push(AppRule { app, strip });
        }
        match self.set_app_rules(AppRulesParams { rules: merged }) {
            Ok(_) => result.imported.push("app rules".into()),
            Err(e) => result.skipped.push(format!("app rules: {}", e.message)),
        }
        if !left_out.is_empty() {
            result.notes.push(format!(
                "Rules left out, as their strip is not a virtual strip here: {}.",
                left_out.join(", ")
            ));
        }
    }

    /// Hotkeys, added to the ones here or replacing them all, with their
    /// groups.
    fn import_hotkeys(
        &self,
        plan: &[Planned],
        p: &ImportParams,
        list_file: Option<&HotkeyListFile>,
        result: &mut ImportResult,
    ) {
        let replace_all = p.hotkeys == HotkeyImport::ReplaceAll;
        // Each checked here, with this mixer's ids.
        let mut incoming: Vec<(Hotkey, Option<String>, bool)> = Vec::new();
        for x in plan {
            let Content::Hotkey(h, group) = &x.content else {
                continue;
            };
            let mut h = h.clone();
            h.id = 0;
            h.group = 0;
            h.name = x.name.clone();
            for step in h.steps.iter_mut().chain(&mut h.release_steps) {
                mapped(&step.method, &mut step.params, p);
            }
            match self.checked_hotkey(h) {
                Ok(h) => incoming.push((h, group.clone(), x.replace)),
                Err(e) => result
                    .skipped
                    .push(format!("hotkey '{}': {}", x.name, e.message)),
            }
        }
        // Replacing all of them takes at least one to replace them with:
        // importing only a scene never clears the hotkeys.
        if incoming.is_empty() {
            return;
        }
        let groups_file: Vec<GroupFile> = list_file.map(|l| l.groups.clone()).unwrap_or_default();
        let mut notes = Vec::new();
        let mut imported = Vec::new();
        let edited = self.edit_hotkeys(|list| {
            if replace_all {
                *list = HotkeyList::default();
            }
            for (mut h, group, replace) in incoming.clone() {
                // Into its group, made if there is none of its name.
                if let Some(name) = &group {
                    let found = list
                        .groups
                        .iter()
                        .find(|g| g.name.eq_ignore_ascii_case(name))
                        .map(|g| g.id);
                    h.group = match found {
                        Some(id) => id,
                        None => {
                            let id = list.groups.iter().map(|g| g.id).max().unwrap_or(0) + 1;
                            let enabled = groups_file
                                .iter()
                                .find(|g| g.name.eq_ignore_ascii_case(name))
                                .is_none_or(|g| g.enabled);
                            list.groups.push(HotkeyGroup {
                                id,
                                name: name.clone(),
                                enabled,
                            });
                            id
                        }
                    };
                }
                let existing = list
                    .hotkeys
                    .iter()
                    .position(|o| o.name.eq_ignore_ascii_case(&h.name));
                if let (true, Some(at)) = (replace, existing) {
                    h.id = list.hotkeys[at].id;
                }
                // Keys another hotkey has stay with it.
                let mut dropped = Vec::new();
                h.keys.retain(|k| {
                    let other = list
                        .hotkeys
                        .iter()
                        .find(|o| o.id != h.id && o.keys.contains(k));
                    if let Some(other) = other {
                        dropped.push(format!("{k} (the hotkey '{}' has it)", other.name));
                    }
                    other.is_none()
                });
                if !dropped.is_empty() {
                    notes.push(format!(
                        "The hotkey '{}' came in without {}.",
                        h.name,
                        dropped.join(", ")
                    ));
                }
                match (replace, existing) {
                    (true, Some(at)) => list.hotkeys[at] = h.clone(),
                    _ => {
                        h.id = list.hotkeys.iter().map(|o| o.id).max().unwrap_or(0) + 1;
                        list.hotkeys.push(h.clone());
                    }
                }
                imported.push(h.name.clone());
            }
            // Replacing them all takes the order they had too.
            if replace_all {
                if let Some(file) = list_file {
                    list.order = file
                        .order
                        .iter()
                        .filter_map(|place| match place {
                            PlaceFile::Hotkey(name) => list
                                .hotkeys
                                .iter()
                                .find(|h| h.name.eq_ignore_ascii_case(name))
                                .map(|h| HotkeyListItem::Hotkey(h.id)),
                            PlaceFile::Group(name) => list
                                .groups
                                .iter()
                                .find(|g| g.name.eq_ignore_ascii_case(name))
                                .map(|g| HotkeyListItem::Group(g.id)),
                        })
                        .collect();
                }
            }
            Ok(())
        });
        match edited {
            Ok(()) => {
                if replace_all {
                    result
                        .notes
                        .push("Your hotkeys were replaced by the imported ones.".into());
                }
                result
                    .imported
                    .extend(imported.iter().map(|n| format!("hotkey '{n}'")));
                result.notes.extend(notes);
            }
            Err(e) => result.skipped.push(format!("hotkeys: {}", e.message)),
        }
    }

    /// The parts of the preferences: the daemon's settings, and the
    /// window's look, which goes to the window.
    fn import_preferences(
        &self,
        plan: &[Planned],
        p: &ImportParams,
        here: &Here,
        result: &mut ImportResult,
    ) {
        let mut patch = SettingsPatch::default();
        let mut parts = Vec::new();
        for x in plan {
            match &x.content {
                Content::WindowLook(look) => {
                    result.window_look = Some(look.clone());
                    if self.window_attached() {
                        self.announce(Notification::WindowLook(look.clone()));
                        result.window_told = true;
                    }
                    result.imported.push(spoken(&x.item));
                }
                Content::Mixer(m) => {
                    patch.meter_rate_hz = Some(m.meter_rate_hz);
                    patch.startup = Some(m.startup);
                    patch.tray = Some(m.tray);
                    patch.tray_icon = Some(m.tray_icon);
                    patch.solo = Some(match &m.solo {
                        SoloFile::Exclusive => SoloMode::Exclusive,
                        SoloFile::Cue(name) => {
                            let name = p.map_buses.get(name).unwrap_or(name);
                            match names::find(&here.mixer, TargetKind::Bus, name) {
                                Some(id) => SoloMode::Cue(id),
                                None => SoloMode::Exclusive,
                            }
                        }
                    });
                    parts.push(&x.item);
                }
                Content::AudioTiming(t) => {
                    // 0 asks for PipeWire to decide.
                    patch.sample_rate = Some(t.sample_rate.unwrap_or(0));
                    patch.quantum = Some(t.quantum.unwrap_or(0));
                    parts.push(&x.item);
                }
                Content::StartAtLogin(on) => {
                    if here_can_start_at_login(self) {
                        patch.start_at_login = Some(Flag::Set(*on));
                        parts.push(&x.item);
                    } else {
                        result.skipped.push(format!(
                            "{}: Weir cannot start at login on this computer",
                            spoken(&x.item)
                        ));
                    }
                }
                _ => {}
            }
        }
        if parts.is_empty() {
            return;
        }
        match self.set_settings(patch) {
            Ok(_) => result.imported.extend(parts.iter().map(|i| spoken(i))),
            Err(e) => {
                for i in parts {
                    result.skipped.push(format!("{}: {}", spoken(i), e.message));
                }
            }
        }
    }
}

/// Whether Weir can be made to start at login here.
fn here_can_start_at_login(c: &Controller) -> bool {
    c.settings().start_at_login.is_some()
}

/// An item to import, and how: under which name, and whether it replaces
/// the one here of that name.
struct Planned {
    item: ImportItem,
    content: Content,
    name: String,
    replace: bool,
}
