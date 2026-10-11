//! What each request does. [`Controller::handle`] sends every request to a
//! method of its own; changes to the mixer go through
//! [`Controller::mutate`], which checks nothing broke, records undo and
//! tells everyone.

use super::hotkeys::Action;
use super::names::{self, Mixers};
use super::rules::rule_moves;
use super::undo_labels::undo_step;
use super::{Controller, Subscriptions, MAX_SPECTRUM_TARGETS};
use crate::config;
use crate::history::Step;
use crate::tray::TrayCommand;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use tracing::info;
use weir_protocol::*;

/// Longest a strip's, bus's or equalizer preset's name may be.
pub(super) const NAME_MAX: usize = 40;
/// Most channels a strip or bus may have.
const CHANNELS_MAX: usize = 16;

/// What the daemon can do, for clients that want to work with older ones.
const CAPABILITIES: &[&str] = &[
    "meters",
    "apps",
    "eq",
    "gate",
    "denoise",
    "compressor",
    "ducking",
    "limiter",
    "bus_delay",
    "sends",
    "upmix",
    "downmix",
    "spectrum",
    "history",
    "scenes",
    "setups",
    "app_rules",
    "system_volumes",
    "external_effects",
    "hotkeys",
    "toggle",
    "deltas",
    "names",
    "batch",
    "describe",
];

impl Controller {
    /// Carry out one request for the connection whose subscriptions are
    /// `subs`, and return its result.
    pub fn handle(&self, req: Request, subs: &mut Subscriptions) -> Result<Value, RpcError> {
        self.handle_as(req, subs, true)
    }

    /// Carry out one request, with its own step in the undo history when
    /// `record` is true. Hotkeys run their steps without, and record each
    /// press as a whole.
    pub(super) fn handle_as(
        &self,
        req: Request,
        subs: &mut Subscriptions,
        record: bool,
    ) -> Result<Value, RpcError> {
        let step = if record {
            undo_step(&req, &self.inner.lock().unwrap().mixer)
        } else {
            None
        };
        match req {
            Request::Hello => Ok(to_json(&HelloResult {
                protocol_version: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
            })),
            Request::Ping => Ok(json!("pong")),
            Request::GetState => Ok(to_json(&self.full_state())),
            Request::ListDevices => Ok(to_json(&self.inner.lock().unwrap().devices)),
            Request::ListApps => Ok(to_json(&self.inner.lock().unwrap().apps)),
            Request::SetStrip(p) => self.mutate(step, |m| set_strip(m, p)),
            Request::SetBus(p) => self.mutate(step, |m| set_bus(m, p)),
            Request::SetRoute(p) => self.mutate(step, |m| set_route(m, p)),
            Request::AddStrip(p) => self.mutate(step, |m| add_strip(m, p)),
            Request::RemoveStrip(p) => self.mutate(step, |m| remove_strip(m, p.id)),
            Request::AddBus(p) => self.mutate(step, |m| add_bus(m, p)),
            Request::RemoveBus(p) => self.mutate(step, |m| remove_bus(m, p.id)),
            Request::MoveStrip(p) => self.mutate(step, |m| {
                move_in(&mut m.strips, |s| s.id, p, "strip")?;
                Ok(m.strips.iter().map(|s| s.id).collect::<Vec<_>>())
            }),
            Request::MoveBus(p) => self.mutate(step, |m| {
                move_in(&mut m.buses, |b| b.id, p, "bus")?;
                Ok(m.buses.iter().map(|b| b.id).collect::<Vec<_>>())
            }),
            Request::SetAppRules(p) => self.set_app_rules(p),
            Request::MoveApp(p) => self.move_app(p),
            Request::SetAppVolume(p) => self.set_app_volume(p),
            Request::SetSettings(p) => self.set_settings(p),
            Request::ShowWindow => self.show_window_request(),
            Request::ListSetups => Ok(to_json(&config::list_saved(&self.paths.setups_dir))),
            Request::ListScenes => Ok(to_json(&config::list_saved(&self.paths.scenes_dir))),
            Request::SaveSetup(p) => self.save_setup(p),
            Request::SaveScene(p) => self.save_scene(p),
            Request::LoadSetup(p) => self.load_setup(p, step),
            Request::LoadScene(p) => self.load_scene(p, step),
            Request::DeleteSetup(p) => self.delete_setup(p),
            Request::DeleteScene(p) => self.delete_scene(p),
            Request::ListEqPresets => Ok(to_json(&self.eq_presets())),
            Request::SaveEqPreset(p) => self.save_eq_preset(p),
            Request::DeleteEqPreset(p) => self.delete_eq_preset(p),
            Request::ApplyEqPreset(p) => self.apply_eq_preset(p, step),
            Request::ListHotkeys => Ok(to_json(&self.hotkeys_info())),
            Request::SetHotkey(h) => self.set_hotkey(h),
            Request::RemoveHotkey(r) => self.remove_hotkey(r),
            Request::SwitchHotkey(p) => self.switch_hotkey(p),
            Request::MoveHotkey(p) => self.move_hotkey(p),
            Request::AddHotkeyGroup(p) => self.add_hotkey_group(p),
            Request::SetHotkeyGroup(p) => self.set_hotkey_group(p),
            Request::RemoveHotkeyGroup(r) => self.remove_hotkey_group(r),
            Request::MoveHotkeyGroup(p) => self.move_hotkey_group(p),
            Request::PressHotkey(r) => self.hotkey_action(r, Action::Press),
            Request::ReleaseHotkey(r) => self.hotkey_action(r, Action::Release),
            Request::RunHotkey(r) => self.hotkey_action(r, Action::Run),
            Request::OpenShortcutSettings => self.open_shortcut_settings(),
            Request::AddSound(p) => self.add_sound(p),
            Request::RemoveSound(p) => self.remove_sound(p),
            Request::PlaySound(p) => self.play_sound(&p.name),
            Request::ExportSettings(p) => self.export_settings(p),
            Request::InspectImport(p) => self.inspect_import(p),
            Request::ImportSettings(p) => self.import_settings(p),
            Request::WatchSpectrum(p) => watch_spectrum(subs, p),
            Request::Undo(p) => self.step_history(p.steps, true),
            Request::Redo(p) => self.step_history(p.steps, false),
            Request::History => Ok(to_json(&self.inner.lock().unwrap().history.info())),
            Request::Describe => Ok(weir_protocol::describe()),
            Request::Subscribe(p) => Ok(subscribe(subs, p)),
            Request::Unsubscribe(p) => Ok(unsubscribe(subs, p)),
        }
    }

    /// Everything a client needs to draw itself.
    fn full_state(&self) -> FullState {
        let eq_presets = self.eq_presets();
        let library = self.library();
        let hotkeys = self.hotkeys_info();
        let inner = self.inner.lock().unwrap();
        FullState {
            mixer: inner.mixer.clone(),
            devices: inner.devices.clone(),
            apps: inner.apps.clone(),
            engine: inner.engine.clone(),
            settings: inner.settings.clone(),
            eq_presets,
            app_rules: inner.app_rules.clone(),
            library,
            system_volumes: inner.system_volumes.clone(),
            inserts: inner.inserts.clone(),
            hotkeys,
        }
    }

    pub(super) fn set_app_rules(&self, p: AppRulesParams) -> Result<Value, RpcError> {
        // A rule's strip may be one only a saved setup has: the rule waits
        // for it.
        let mixers = Mixers::new(self.mixer(), &self.paths.setups_dir, Vec::new());
        let mut rules: Vec<AppRule> = Vec::new();
        for r in p.rules {
            let app = r.app.trim().to_string();
            if app.is_empty() {
                return Err(RpcError::invalid_params("a rule needs an application"));
            }
            let strip = match r.strip.as_deref().map(str::trim) {
                None | Some("") => None,
                Some(key) => {
                    let (m, id) = mixers
                        .locate(TargetKind::Strip, key)
                        .ok_or_else(|| names::no_such(TargetKind::Strip, key))?;
                    let s = m.strip(id).expect("located");
                    if s.kind != StripKind::Virtual {
                        return Err(RpcError::application(format!(
                            "'{}' is not a virtual strip, so applications cannot play into it",
                            s.name
                        )));
                    }
                    Some(s.name.clone())
                }
            };
            // One rule per application: the first one wins anyway, so a
            // second would only confuse.
            if !rules.iter().any(|x| x.app.eq_ignore_ascii_case(&app)) {
                rules.push(AppRule { app, strip });
            }
        }
        let (rules, moves) = {
            let mut inner = self.inner.lock().unwrap();
            inner.app_rules = rules.clone();
            // A new rule applies to what is already playing too.
            inner.rules_done.clear();
            inner.rules_tried.clear();
            let moves = rule_moves(&mut inner);
            (rules, moves)
        };
        self.dirty.store(true, Ordering::Release);
        self.announce(Notification::AppRulesChanged(rules.clone()));
        self.make_moves(moves);
        Ok(to_json(&rules))
    }

    fn move_app(&self, p: MoveAppParams) -> Result<Value, RpcError> {
        {
            let inner = self.inner.lock().unwrap();
            match inner.mixer.strip(p.strip) {
                None => return Err(unknown("strip", p.strip)),
                Some(s) if s.kind != StripKind::Virtual => {
                    return Err(RpcError::application(
                        "apps can only be moved to virtual strips",
                    ))
                }
                _ => {}
            }
            if !inner.apps.iter().any(|a| a.id == p.app) {
                return Err(unknown("app", p.app));
            }
        }
        self.engine()
            .ok_or_else(|| RpcError::engine("engine disabled"))?
            .move_app(p.app, p.strip)
            .map_err(|e| RpcError::engine(e.to_string()))?;
        Ok(Value::Null)
    }

    fn set_app_volume(&self, p: AppVolumeParams) -> Result<Value, RpcError> {
        // Toggles and steps start from what the stream reports now.
        let (now_db, now_mute) = {
            let inner = self.inner.lock().unwrap();
            let app = inner
                .apps
                .iter()
                .find(|a| a.id == p.app)
                .ok_or_else(|| unknown("app", p.app))?;
            (app.volume_db.unwrap_or(0.0), app.mute)
        };
        if p.volume_db.is_none() && p.volume_delta_db.is_none() && p.mute.is_none() {
            return Err(RpcError::invalid_params(
                "set volume_db, volume_delta_db or mute",
            ));
        }

        let volume = match (p.volume_db, p.volume_delta_db) {
            (None, None) => None,
            (v, d) => {
                Some((v.unwrap_or(now_db) + d.unwrap_or(0.0)).clamp(GAIN_MIN_DB, GAIN_MAX_DB))
            }
        };
        self.engine()
            .ok_or_else(|| RpcError::engine("engine disabled"))?
            .set_app_volume(p.app, volume, p.mute.map(|f| f.apply(now_mute)))
            .map_err(|e| RpcError::engine(e.to_string()))?;
        Ok(Value::Null)
    }

    pub(super) fn set_settings(&self, p: SettingsPatch) -> Result<Value, RpcError> {
        // Check every field before changing anything: starting at login is
        // changed outside the daemon, and a later field failing could not
        // take that back.
        if let Some(SoloMode::Cue(bus)) = p.solo {
            if self.inner.lock().unwrap().mixer.bus(bus).is_none() {
                return Err(unknown("bus", bus));
            }
        }
        // 0 means "let PipeWire decide".
        let hold = |v: u32, range: std::ops::RangeInclusive<u32>, what: &str| {
            if v != 0 && !range.contains(&v) {
                return Err(RpcError::invalid_params(format!(
                    "{what} must be 0 (PipeWire decides) or {} to {}",
                    range.start(),
                    range.end()
                )));
            }
            Ok((v != 0).then_some(v))
        };
        let sample_rate = p
            .sample_rate
            .map(|v| hold(v, 8_000..=384_000, "sample_rate"))
            .transpose()?;
        let quantum = p
            .quantum
            .map(|v| hold(v, 16..=8192, "quantum"))
            .transpose()?;
        if let Some(v) = p.sounds_volume_db {
            let (lo, hi) = SOUNDS_VOLUME_DB;
            if !(lo..=hi).contains(&v) {
                return Err(RpcError::invalid_params(format!(
                    "sounds_volume_db must be from {lo} to {hi}"
                )));
            }
        }
        let sounds_device = p
            .sounds_device
            .map(|d| d.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()));
        let start_at_login = p
            .start_at_login
            .map(|flag| self.set_start_at_login(flag))
            .transpose()?;
        let settings = {
            let mut inner = self.inner.lock().unwrap();
            let s = &mut inner.settings;
            if let Some(v) = p.meter_rate_hz {
                s.meter_rate_hz = v.clamp(1, 120);
            }
            if let Some(v) = p.startup {
                s.startup = v;
            }
            if let Some(v) = p.tray {
                s.tray = v;
            }
            if let Some(v) = p.tray_icon {
                s.tray_icon = v;
            }
            if let Some(v) = p.solo {
                s.solo = v;
            }
            if let Some(v) = sample_rate {
                s.sample_rate = v;
            }
            if let Some(v) = quantum {
                s.quantum = v;
            }
            if let Some(v) = start_at_login {
                s.start_at_login = v;
            }
            if let Some(v) = p.hotkey_popup {
                s.hotkey_popup = v;
            }
            if let Some(v) = sounds_device {
                s.sounds_device = v;
            }
            if let Some(v) = p.sounds_volume_db {
                s.sounds_volume_db = v;
            }
            s.clone()
        };
        self.apply_engine_options();
        self.update_sounds_device();
        self.dirty.store(true, Ordering::Release);
        self.announce(Notification::SettingsChanged(settings.clone()));
        Ok(to_json(&settings))
    }

    /// Start Weir at login or stop doing so, and return what systemd says
    /// afterwards. Runs `systemctl`, so the lock is not held meanwhile.
    fn set_start_at_login(&self, flag: Flag) -> Result<Option<bool>, RpcError> {
        let Some(now) = self.inner.lock().unwrap().settings.start_at_login else {
            return Err(RpcError::application(
                "Weir cannot start at login here: its service is not installed, or \
                 systemd is not running",
            ));
        };
        let wanted = flag.apply(now);
        if wanted != now {
            self.login.set(wanted).map_err(RpcError::application)?;
        }
        Ok(self.login.get())
    }

    fn show_window_request(&self) -> Result<Value, RpcError> {
        if self.window_attached() {
            self.show_window();
        } else if let Some(tx) = self.window_tx.lock().unwrap().as_ref() {
            let _ = tx.send(TrayCommand::Show);
        } else {
            return Err(RpcError::application(
                "no window is open and this daemon cannot start one",
            ));
        }
        Ok(Value::Null)
    }

    fn save_setup(&self, p: NameParams) -> Result<Value, RpcError> {
        let setup = Setup::capture(&self.mixer());
        config::save_saved(&self.paths.setups_dir, &p.name, "setup", &setup).map_err(app_error)?;
        Ok(to_json(&self.library_changed(None, Some(Some(p.name)))))
    }

    fn save_scene(&self, p: NameParams) -> Result<Value, RpcError> {
        let scene = Scene::capture(&self.mixer());
        config::save_saved(&self.paths.scenes_dir, &p.name, "scene", &scene).map_err(app_error)?;
        Ok(to_json(&self.library_changed(Some(Some(p.name)), None)))
    }

    fn load_setup(&self, p: LoadSetupParams, step: Option<Step>) -> Result<Value, RpcError> {
        let setup: Setup =
            config::load_saved(&self.paths.setups_dir, &p.name, "setup").map_err(app_error)?;
        let scene: Option<Scene> = match &p.scene {
            Some(name) => {
                Some(config::load_saved(&self.paths.scenes_dir, name, "scene").map_err(app_error)?)
            }
            None => None,
        };
        // Without a scene, every strip and bus starts at its default mix,
        // nothing routed: as a scene that mentions nothing would leave it.
        let mut loaded = setup.mixer();
        if let Some(scene) = &scene {
            scene.apply(&mut loaded);
        }
        let result = self.replace_mixer(step, |m| {
            *m = loaded;
            Ok(m.clone())
        })?;
        {
            // Another setup: app rules apply afresh, to strips of their
            // names here, when the applications show up on its devices.
            let mut inner = self.inner.lock().unwrap();
            inner.rules_done.clear();
            inner.rules_tried.clear();
        }
        self.library_changed(Some(p.scene), Some(Some(p.name)));
        Ok(result)
    }

    fn load_scene(&self, p: NameParams, step: Option<Step>) -> Result<Value, RpcError> {
        let scene: Scene =
            config::load_saved(&self.paths.scenes_dir, &p.name, "scene").map_err(app_error)?;
        let result = self.mutate(step, |m| {
            scene.apply(m);
            Ok(m.clone())
        })?;
        self.library_changed(Some(Some(p.name)), None);
        Ok(result)
    }

    fn delete_setup(&self, p: NameParams) -> Result<Value, RpcError> {
        config::delete_saved(&self.paths.setups_dir, &p.name, "setup").map_err(app_error)?;
        // Deleting the current setup leaves none current.
        let current = self.inner.lock().unwrap().setup.clone();
        let clear = (current.as_deref() == Some(p.name.as_str())).then_some(None);
        Ok(to_json(&self.library_changed(None, clear)))
    }

    fn delete_scene(&self, p: NameParams) -> Result<Value, RpcError> {
        config::delete_saved(&self.paths.scenes_dir, &p.name, "scene").map_err(app_error)?;
        let current = self.inner.lock().unwrap().scene.clone();
        let clear = (current.as_deref() == Some(p.name.as_str())).then_some(None);
        Ok(to_json(&self.library_changed(clear, None)))
    }

    fn save_eq_preset(&self, p: SaveEqPresetParams) -> Result<Value, RpcError> {
        let name = p.name.trim().to_string();
        if name.is_empty() || name.chars().count() > NAME_MAX {
            return Err(RpcError::invalid_params(format!(
                "a preset name needs 1 to {NAME_MAX} characters"
            )));
        }
        if is_builtin_eq_preset(&name) {
            return Err(RpcError::application(format!(
                "'{name}' is a built-in preset; choose another name"
            )));
        }
        check_bands(&p.bands)?;
        let bands: Vec<EqBand> = p.bands.iter().map(|b| b.clamped()).collect();
        let result = self.edit_eq_presets(|user| {
            match user.iter_mut().find(|u| u.name.eq_ignore_ascii_case(&name)) {
                Some(existing) => {
                    existing.name = name.clone();
                    existing.bands = bands;
                }
                None => user.push(EqPreset {
                    name: name.clone(),
                    bands,
                    builtin: false,
                }),
            }
            user.sort_by_key(|u| u.name.to_lowercase());
            Ok(())
        })?;
        info!("saved equalizer preset '{name}'");
        Ok(result)
    }

    fn delete_eq_preset(&self, p: NameParams) -> Result<Value, RpcError> {
        let name = p.name.trim().to_string();
        if is_builtin_eq_preset(&name) {
            return Err(RpcError::application(format!(
                "'{name}' is a built-in preset and cannot be deleted"
            )));
        }
        let result = self.edit_eq_presets(|user| {
            let before = user.len();
            user.retain(|u| !u.name.eq_ignore_ascii_case(&name));
            if user.len() == before {
                return Err(RpcError::application(format!(
                    "no equalizer preset called '{name}'"
                )));
            }
            Ok(())
        })?;
        info!("deleted equalizer preset '{name}'");
        Ok(result)
    }

    fn apply_eq_preset(
        &self,
        p: ApplyEqPresetParams,
        step: Option<Step>,
    ) -> Result<Value, RpcError> {
        let preset = self.find_eq_preset(&p.name)?;
        let eq = Equalizer {
            enabled: true,
            bands: preset.bands,
        };
        match (p.strip, p.bus) {
            (Some(id), None) => self.mutate(step, |m| {
                let s = m.strip_mut(id).ok_or_else(|| unknown("strip", id))?;
                s.eq = eq;
                Ok(s.clone())
            }),
            (None, Some(id)) => self.mutate(step, |m| {
                let b = m.bus_mut(id).ok_or_else(|| unknown("bus", id))?;
                b.eq = eq;
                Ok(b.clone())
            }),
            _ => Err(RpcError::invalid_params("give exactly one of strip or bus")),
        }
    }
}

fn set_strip(m: &mut MixerState, p: StripPatch) -> Result<Strip, RpcError> {
    if let Some(name) = &p.name {
        let others = m.strips.iter().filter(|s| s.id != p.id);
        check_name(name, others.map(|s| s.name.as_str()))?;
    }
    let s = m.strip_mut(p.id).ok_or_else(|| unknown("strip", p.id))?;
    if let Some(v) = p.name {
        s.name = v.trim().to_string();
    }
    if let Some(v) = p.gain_db {
        s.gain_db = v.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    }
    if let Some(d) = p.gain_delta_db {
        s.gain_db = (s.gain_db + d).clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    }
    if let Some(v) = p.mute {
        s.mute = v.apply(s.mute);
    }
    if let Some(v) = p.solo {
        s.solo = v.apply(s.solo);
    }
    if let Some(v) = p.pan {
        s.pan = v.clamp(-1.0, 1.0);
    }
    if let Some(d) = p.pan_delta {
        s.pan = (s.pan + d).clamp(-1.0, 1.0);
    }
    if let Some(v) = p.layout {
        check_layout(&v)?;
        s.layout = v;
    }
    if let Some(v) = p.device {
        if s.kind != StripKind::Hardware && v.is_some() {
            return Err(RpcError::application("only hardware strips take a device"));
        }
        s.device = v.filter(|d| !d.is_empty());
    }
    if let Some(v) = p.color {
        s.color = v;
    }
    if let Some(v) = &p.sends {
        for (bus, db) in v {
            s.sends.insert(*bus, db.clamp(GAIN_MIN_DB, GAIN_MAX_DB));
        }
    }
    if let Some(v) = p.upmix {
        s.upmix = v;
    }
    if let Some(v) = p.subwoofer {
        s.subwoofer = v.apply(s.subwoofer);
    }
    if let Some(v) = &p.eq {
        if let Some(bands) = &v.bands {
            check_bands(bands)?;
        }
        v.apply(&mut s.eq);
    }
    if let Some(v) = &p.gate {
        v.apply(&mut s.gate);
    }
    if let Some(v) = &p.denoise {
        v.apply(&mut s.denoise);
    }
    if let Some(v) = &p.compressor {
        v.apply(&mut s.compressor);
    }
    if let Some(v) = &p.ducking {
        v.apply(&mut s.ducking);
    }
    if let Some(v) = &p.insert {
        check_insert_point(v, &InsertPoint::STRIP, "strip")?;
        v.apply(&mut s.insert);
    }
    Ok(s.clone())
}

fn set_bus(m: &mut MixerState, p: BusPatch) -> Result<Bus, RpcError> {
    if let Some(name) = &p.name {
        let others = m.buses.iter().filter(|b| b.id != p.id);
        check_name(name, others.map(|b| b.name.as_str()))?;
    }
    let b = m.bus_mut(p.id).ok_or_else(|| unknown("bus", p.id))?;
    if let Some(v) = p.name {
        b.name = v.trim().to_string();
    }
    if let Some(v) = p.gain_db {
        b.gain_db = v.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    }
    if let Some(d) = p.gain_delta_db {
        b.gain_db = (b.gain_db + d).clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    }
    if let Some(v) = p.mute {
        b.mute = v.apply(b.mute);
    }
    if let Some(v) = p.mono {
        b.mono = v.apply(b.mono);
    }
    if let Some(v) = p.delay_ms {
        b.delay_ms = v.clamp(0.0, BUS_DELAY_MAX_MS);
    }
    if let Some(d) = p.delay_delta_ms {
        b.delay_ms = (b.delay_ms + d).clamp(0.0, BUS_DELAY_MAX_MS);
    }
    if let Some(v) = p.layout {
        check_layout(&v)?;
        b.layout = v;
    }
    if let Some(v) = p.device {
        if b.kind != BusKind::Hardware && v.is_some() {
            return Err(RpcError::application("only hardware buses take a device"));
        }
        b.device = v.filter(|d| !d.is_empty());
    }
    if let Some(v) = p.color {
        b.color = v;
    }
    if let Some(v) = &p.eq {
        if let Some(bands) = &v.bands {
            check_bands(bands)?;
        }
        v.apply(&mut b.eq);
    }
    if let Some(v) = &p.downmix {
        v.apply(&mut b.downmix);
    }
    if let Some(v) = &p.limiter {
        v.apply(&mut b.limiter);
    }
    if let Some(v) = &p.insert {
        check_insert_point(v, &InsertPoint::BUS, "bus")?;
        v.apply(&mut b.insert);
    }
    Ok(b.clone())
}

/// Switch a route, set its level, or both. Without `enabled`, a request
/// that sets a level leaves the route as it is, and one that does not
/// toggles it: a route button's click.
fn set_route(m: &mut MixerState, p: RouteParams) -> Result<Strip, RpcError> {
    if m.bus(p.bus).is_none() {
        return Err(unknown("bus", p.bus));
    }
    let s = m
        .strip_mut(p.strip)
        .ok_or_else(|| unknown("strip", p.strip))?;
    let routed = s.routes.contains(&p.bus);
    let level_only = p.level_db.is_some() || p.level_delta_db.is_some();
    let enable = match p.enabled {
        Some(v) => v.apply(routed),
        None if level_only => routed,
        None => !routed,
    };
    if enable {
        s.routes.insert(p.bus);
    } else {
        s.routes.remove(&p.bus);
    }
    if let Some(db) = p.level_db {
        s.sends.insert(p.bus, db.clamp(GAIN_MIN_DB, GAIN_MAX_DB));
    }
    if let Some(d) = p.level_delta_db {
        let db = s.send_db(p.bus) + d;
        s.sends.insert(p.bus, db.clamp(GAIN_MIN_DB, GAIN_MAX_DB));
    }
    Ok(s.clone())
}

fn add_strip(m: &mut MixerState, p: AddStripParams) -> Result<Strip, RpcError> {
    check_name(&p.name, m.strips.iter().map(|s| s.name.as_str()))?;
    check_layout(&p.layout)?;
    if let Some(b) = p.routes.iter().find(|b| m.bus(**b).is_none()) {
        return Err(unknown("bus", *b));
    }
    if p.kind != StripKind::Hardware && p.device.is_some() {
        return Err(RpcError::application("only hardware strips take a device"));
    }
    let strip = Strip {
        routes: p.routes.into_iter().collect(),
        device: p.device,
        ..Strip::new(m.next_strip_id(), p.name.trim(), p.kind, p.layout)
    };
    m.strips.push(strip.clone());
    info!("added strip {} '{}'", strip.id, strip.name);
    Ok(strip)
}

fn remove_strip(m: &mut MixerState, id: StripId) -> Result<Value, RpcError> {
    let before = m.strips.len();
    m.strips.retain(|s| s.id != id);
    if m.strips.len() == before {
        return Err(unknown("strip", id));
    }
    info!("removed strip {id}");
    Ok(Value::Null)
}

fn add_bus(m: &mut MixerState, p: AddBusParams) -> Result<Bus, RpcError> {
    check_name(&p.name, m.buses.iter().map(|b| b.name.as_str()))?;
    check_layout(&p.layout)?;
    if p.kind != BusKind::Hardware && p.device.is_some() {
        return Err(RpcError::application("only hardware buses take a device"));
    }
    let bus = Bus {
        device: p.device,
        ..Bus::new(m.next_bus_id(), p.name.trim(), p.kind, p.layout)
    };
    m.buses.push(bus.clone());
    info!("added bus {} '{}'", bus.id, bus.name);
    Ok(bus)
}

/// Remove a bus. Routes to it go with it; everything else that mentions
/// it is tidied by `normalize` afterwards.
fn remove_bus(m: &mut MixerState, id: BusId) -> Result<Value, RpcError> {
    let before = m.buses.len();
    m.buses.retain(|b| b.id != id);
    if m.buses.len() == before {
        return Err(unknown("bus", id));
    }
    for s in &mut m.strips {
        s.routes.remove(&id);
    }
    info!("removed bus {id}");
    Ok(Value::Null)
}

/// Move the item of `items` with id `p.id` to position `p.index`, or last
/// when that is past the end. `what` names the kind of item for the error.
fn move_in<T>(
    items: &mut Vec<T>,
    id: impl Fn(&T) -> u32,
    p: MoveParams,
    what: &str,
) -> Result<(), RpcError> {
    let from = items
        .iter()
        .position(|x| id(x) == p.id)
        .ok_or_else(|| unknown(what, p.id))?;
    let item = items.remove(from);
    items.insert(p.index.min(items.len()), item);
    Ok(())
}

fn watch_spectrum(subs: &mut Subscriptions, p: WatchSpectrumParams) -> Result<Value, RpcError> {
    let targets: BTreeSet<StripOrBus> = p.targets.into_iter().collect();
    if targets.len() > MAX_SPECTRUM_TARGETS {
        return Err(RpcError::invalid_params(format!(
            "watch at most {MAX_SPECTRUM_TARGETS} spectra at once"
        )));
    }
    // The connection's guard notices the change and updates the counts,
    // including when the connection drops.
    subs.spectrum = targets;
    Ok(to_json(&subs.spectrum))
}

fn subscribe(subs: &mut Subscriptions, p: SubscribeParams) -> Value {
    let topics = if p.topics.is_empty() {
        Topic::ALL.to_vec()
    } else {
        p.topics
    };
    subs.topics.extend(topics);
    if let Some(hz) = p.meter_rate_hz {
        subs.meter_rate_hz = Some(hz.clamp(1, 120));
    }
    to_json(&subs.topics)
}

fn unsubscribe(subs: &mut Subscriptions, p: SubscribeParams) -> Value {
    if p.topics.is_empty() {
        subs.topics.clear();
    } else {
        for t in p.topics {
            subs.topics.remove(&t);
        }
    }
    to_json(&subs.topics)
}

/// The error for a strip, bus or app that does not exist.
fn unknown(what: &str, id: u32) -> RpcError {
    RpcError::application(format!("no {what} with id {id}"))
}

/// A file problem, as an application error with its whole story.
fn app_error(e: anyhow::Error) -> RpcError {
    RpcError::application(format!("{e:#}"))
}

pub(super) fn is_builtin_eq_preset(name: &str) -> bool {
    builtin_eq_presets()
        .iter()
        .any(|b| b.name.eq_ignore_ascii_case(name))
}

/// Check a new name for a strip or bus against the names of the `others`.
fn check_name<'a>(name: &str, mut others: impl Iterator<Item = &'a str>) -> Result<(), RpcError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(RpcError::invalid_params("name must not be empty"));
    }
    if name.len() > NAME_MAX {
        return Err(RpcError::invalid_params(format!(
            "name is too long ({NAME_MAX} characters max)"
        )));
    }
    if others.any(|o| o.eq_ignore_ascii_case(name)) {
        return Err(RpcError::application(format!(
            "the name '{name}' is already in use"
        )));
    }
    Ok(())
}

pub(super) fn check_bands(bands: &[EqBand]) -> Result<(), RpcError> {
    if bands.len() > EQ_MAX_BANDS {
        return Err(RpcError::invalid_params(format!(
            "an equalizer has at most {EQ_MAX_BANDS} bands"
        )));
    }
    Ok(())
}

/// Refuse to put external effects where a strip or bus (`what`) has no
/// place for them: `places` are the ones it has.
fn check_insert_point(
    patch: &InsertPatch,
    places: &[InsertPoint],
    what: &str,
) -> Result<(), RpcError> {
    match patch.position {
        Some(at) if !places.contains(&at) => {
            let names: Vec<String> = places.iter().map(|p| to_json(p).to_string()).collect();
            Err(RpcError::invalid_params(format!(
                "a {what}'s external effects go {}",
                names.join(", ").replace('"', "")
            )))
        }
        _ => Ok(()),
    }
}

fn check_layout(layout: &ChannelLayout) -> Result<(), RpcError> {
    let n = layout.channel_count();
    if n == 0 || n > CHANNELS_MAX {
        return Err(RpcError::invalid_params(format!(
            "layout must have 1 to {CHANNELS_MAX} channels"
        )));
    }
    Ok(())
}
