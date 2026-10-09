//! The authoritative mixer state, and everything that changes it.
//!
//! [`Controller`] holds the mixer, what the engine reports about the world,
//! the settings and the undo history, behind one lock. Every request from
//! every client comes through [`Controller::handle`] (in [`handlers`]);
//! every change to the mixer goes through [`Controller::mutate`], which
//! normalizes the result, records it for undo, hands it to the engine,
//! marks the configuration for saving, and tells every client.
//!
//! * [`handlers`]: what each request does.
//! * [`hotkeys`]: keeping hotkeys, and passing presses to the runner.
//! * [`names`]: looking strips and buses up by name in a request.
//! * [`undo_labels`]: what each change is called in the undo history.
//! * [`rules`]: putting applications where their rules say.

mod handlers;
pub mod hotkeys;
mod names;
mod rules;
mod undo_labels;

#[cfg(test)]
mod tests;

use crate::config::{self, Paths, Settings};
use crate::history::{History, Step};
use crate::login::{self, LoginStart};
use crate::tray::TrayCommand;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};
use weir_engine::{EngineEvent, EngineHandle, EngineOptions};
use weir_protocol::*;

/// What one connection asked to be told about.
#[derive(Default)]
pub struct Subscriptions {
    /// The topics it subscribed to.
    pub topics: BTreeSet<Topic>,
    /// Strips and buses this connection wants `spectrum` notifications for.
    pub spectrum: BTreeSet<StripOrBus>,
    /// The most meter messages a second this connection wants, when fewer
    /// than the daemon makes.
    pub meter_rate_hz: Option<u32>,
}

/// Most spectra one connection may watch at once. Each costs an FFT pair 30
/// times a second, so this is a guard against a runaway client, not a limit
/// anyone should meet.
const MAX_SPECTRUM_TARGETS: usize = 32;

/// Everything behind the controller's lock.
struct Inner {
    mixer: MixerState,
    devices: Vec<DeviceInfo>,
    apps: Vec<AppStream>,
    engine: EngineStatus,
    settings: Settings,
    /// The user's own equalizer presets. `Err` holds why the presets file
    /// could not be read, in which case it is left alone rather than
    /// overwritten.
    user_eq_presets: Result<Vec<EqPreset>, String>,
    /// What can be undone and redone.
    history: History,
    app_rules: Vec<AppRule>,
    /// Applications the rules are done with: put where their rule says,
    /// or given up on. Anything moved by hand later stays where it is put.
    /// Keyed by id and name, since PipeWire hands a finished stream's id to
    /// the next one.
    rules_done: HashSet<(u32, String)>,
    /// How many times each application was moved by its rule so far.
    rules_tried: HashMap<(u32, String), u8>,
    /// The scene last loaded or saved.
    scene: Option<String>,
    /// The setup last loaded or saved.
    setup: Option<String>,
    /// The system volumes of Weir's own devices.
    system_volumes: SystemVolumes,
    /// Whether each strip's and bus's external effects are connected.
    inserts: Vec<InsertStatus>,
    /// The hotkeys. `Err` holds why the hotkeys file could not be read, in
    /// which case it is left alone rather than overwritten.
    hotkeys: Result<config::HotkeyList, String>,
    /// How keys reach Weir, as the runner last reported.
    keys_status: KeysStatus,
    /// Hotkeys whose keys do not work, and why, as the runner reported.
    key_problems: BTreeMap<HotkeyId, String>,
    /// Hotkeys that work on strips or buses that are gone, as last told.
    target_problems: Vec<HotkeyProblem>,
}

/// The daemon's state and its request handlers, shared by every
/// connection.
pub struct Controller {
    inner: Mutex<Inner>,
    /// The audio engine; `None` when the daemon runs without one.
    engine: Option<EngineHandle>,
    /// Every notification, to every connection; each picks what it
    /// subscribed to.
    notify: broadcast::Sender<Notification>,
    paths: Paths,
    /// Set when the configuration changed since it was last saved.
    dirty: AtomicBool,
    /// How many connections have claimed to be the mixer window. The tray
    /// uses this to decide between raising a window and starting one.
    window_clients: AtomicUsize,
    /// Set once the main loop is ready to act on window requests it cannot
    /// serve itself, such as starting the window when none is open.
    window_tx: Mutex<Option<UnboundedSender<TrayCommand>>>,
    /// How many connections watch each strip's or bus's spectrum.
    spectrum_watchers: Mutex<BTreeMap<StripOrBus, usize>>,
    /// Whether Weir starts at login is kept here, not in the settings file.
    login: Box<dyn LoginStart>,
    /// The hotkey runner, once it runs.
    hotkey_runner: Mutex<Option<Arc<Mutex<crate::hotkeys::Runner>>>>,
    /// Asks the desktop's shortcut service to open its settings at Weir's
    /// hotkeys; the keys task listens.
    shortcut_settings: tokio::sync::Notify,
}

impl Controller {
    /// A controller for `mixer`, driving `engine` (if any), keeping its
    /// files where `paths` says.
    pub fn new(
        mixer: MixerState,
        engine: Option<EngineHandle>,
        paths: Paths,
        settings: Settings,
        app_rules: Vec<AppRule>,
    ) -> Self {
        let (notify, _) = broadcast::channel(512);
        let hotkeys = config::load_hotkeys(&paths.hotkeys_file, &paths.backups_dir, &mixer)
            .map_err(|e| {
                warn!("{e:#}");
                format!("{e:#}")
            });
        Self {
            inner: Mutex::new(Inner {
                mixer,
                devices: Vec::new(),
                apps: Vec::new(),
                engine: EngineStatus {
                    state: if engine.is_some() {
                        "connecting".into()
                    } else {
                        "disabled".into()
                    },
                    ..Default::default()
                },
                settings,
                user_eq_presets: config::load_eq_presets(&paths.eq_presets_file).map_err(|e| {
                    warn!("{e:#}");
                    format!("{e:#}")
                }),
                history: History::default(),
                app_rules,
                rules_done: HashSet::new(),
                rules_tried: HashMap::new(),
                scene: None,
                setup: None,
                system_volumes: SystemVolumes::default(),
                inserts: Vec::new(),
                hotkeys,
                keys_status: KeysStatus {
                    method: KeysMethod::Starting,
                    message: "Weir is getting hotkeys ready.".into(),
                    ..Default::default()
                },
                key_problems: BTreeMap::new(),
                target_problems: Vec::new(),
            }),
            engine,
            notify,
            paths,
            dirty: AtomicBool::new(false),
            window_clients: AtomicUsize::new(0),
            window_tx: Mutex::new(None),
            spectrum_watchers: Mutex::new(BTreeMap::new()),
            login: Box::new(login::Systemd),
            hotkey_runner: Mutex::new(None),
            shortcut_settings: tokio::sync::Notify::new(),
        }
    }

    /// Read whether Weir starts at login back from where it is kept, into
    /// the settings. Done at startup rather than in [`Controller::new`], so
    /// tests do not ask the machine they run on.
    pub fn refresh_start_at_login(&self) {
        let now = self.login.get();
        self.inner.lock().unwrap().settings.start_at_login = now;
    }

    /// Tell every connection that subscribed to this kind of news.
    fn announce(&self, n: Notification) {
        // No receivers just means no client is connected.
        let _ = self.notify.send(n);
    }

    /// Tell the engine how to run, from the settings.
    pub fn apply_engine_options(&self) {
        let Some(engine) = &self.engine else {
            return;
        };
        let s = self.settings();
        let options = EngineOptions {
            solo: s.solo,
            sample_rate: s.sample_rate,
            quantum: s.quantum,
        };
        if let Err(e) = engine.set_options(options) {
            warn!("engine rejected options: {e}");
        }
    }

    /// One connection started watching `added` and stopped watching
    /// `removed`.
    pub fn adjust_spectrum_watch(&self, added: &[StripOrBus], removed: &[StripOrBus]) {
        let mut w = self.spectrum_watchers.lock().unwrap();
        for t in added {
            *w.entry(*t).or_default() += 1;
        }
        for t in removed {
            if let Some(n) = w.get_mut(t) {
                *n -= 1;
                if *n == 0 {
                    w.remove(t);
                }
            }
        }
        debug!("spectra watched: {:?}", w.keys().collect::<Vec<_>>());
    }

    /// Hand the controller a way to reach the main loop, which is what can
    /// actually start a window process.
    pub fn set_window_sender(&self, tx: UnboundedSender<TrayCommand>) {
        *self.window_tx.lock().unwrap() = Some(tx);
    }

    /// The daemon's settings now.
    pub fn settings(&self) -> Settings {
        self.inner.lock().unwrap().settings.clone()
    }

    /// True when a mixer window is connected right now.
    pub fn window_attached(&self) -> bool {
        self.window_clients.load(Ordering::Acquire) > 0
    }

    /// A connection claims to be the mixer window. There is only ever one:
    /// the claim fails, returning false, while another window is attached.
    /// Checking and claiming in one step means two windows starting at the
    /// same moment cannot both win.
    pub fn claim_window(&self) -> bool {
        self.window_clients
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// A connection that claimed to be the mixer window stopped being one.
    pub fn remove_window_client(&self) {
        // Count down, never below zero. Written out rather than with
        // `fetch_update`, which newer compilers flag as renamed to
        // `try_update`, a name Rust 1.88 does not have yet.
        let mut n = self.window_clients.load(Ordering::Acquire);
        while n > 0 {
            match self.window_clients.compare_exchange_weak(
                n,
                n - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(now) => n = now,
            }
        }
    }

    /// Ask any attached window to come to the front.
    pub fn show_window(&self) {
        self.announce(Notification::ShowWindow);
    }

    /// Ask any attached window to close, before the daemon goes away.
    pub fn quit_windows(&self) {
        self.announce(Notification::Quit);
    }

    /// A new receiver of every notification, for a connection or the tray.
    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.notify.subscribe()
    }

    /// The mixer now.
    pub fn mixer(&self) -> MixerState {
        self.inner.lock().unwrap().mixer.clone()
    }

    /// The audio engine, when there is one.
    pub fn engine(&self) -> Option<&EngineHandle> {
        self.engine.as_ref()
    }

    /// Persist the configuration if anything changed since the last save.
    pub fn save_if_dirty(&self) {
        if self.dirty.swap(false, Ordering::AcqRel) {
            let cfg = {
                let inner = self.inner.lock().unwrap();
                config::Config {
                    settings: inner.settings.clone(),
                    mixer: inner.mixer.clone(),
                    app_rules: inner.app_rules.clone(),
                    current_scene: inner.scene.clone(),
                    current_setup: inner.setup.clone(),
                    ..Default::default()
                }
            };
            if let Err(e) = config::save(&self.paths.config_file, &cfg) {
                warn!("could not save config: {e:#}");
                self.dirty.store(true, Ordering::Release);
            }
        }
    }

    /// Take in something the engine reports, and pass it on to clients.
    pub fn on_engine_event(&self, ev: EngineEvent) {
        let (n, moves) = {
            let mut inner = self.inner.lock().unwrap();
            match ev {
                EngineEvent::Status(s) => {
                    inner.engine = s.clone();
                    (Notification::EngineChanged(s), Vec::new())
                }
                EngineEvent::Devices(d) => {
                    inner.devices = d.clone();
                    (Notification::DevicesChanged(d), Vec::new())
                }
                EngineEvent::Apps(a) => {
                    inner.apps = a.clone();
                    let moves = rules::rule_moves(&mut inner);
                    (Notification::AppsChanged(a), moves)
                }
                EngineEvent::SystemVolumes(v) => {
                    inner.system_volumes = v.clone();
                    (Notification::SystemVolumesChanged(v), Vec::new())
                }
                EngineEvent::Inserts(v) => {
                    inner.inserts = v.clone();
                    (Notification::InsertsChanged(v), Vec::new())
                }
            }
        };
        self.announce(n);
        self.make_moves(moves);
    }

    /// Remember which scene and setup were in use, from the config file.
    pub fn restore_current(&self, scene: Option<String>, setup: Option<String>) {
        let mut inner = self.inner.lock().unwrap();
        inner.scene = scene;
        inner.setup = setup;
    }

    /// The saved scenes and setups, and which of each is current.
    fn library(&self) -> Library {
        let (scene, setup) = {
            let inner = self.inner.lock().unwrap();
            (inner.scene.clone(), inner.setup.clone())
        };
        Library {
            scenes: config::list_saved(&self.paths.scenes_dir),
            setups: config::list_saved(&self.paths.setups_dir),
            scene,
            setup,
        }
    }

    /// Note which scene or setup is current, save that, and tell clients.
    /// `None` leaves one as it is; `Some(None)` clears it.
    fn library_changed(
        &self,
        scene: Option<Option<String>>,
        setup: Option<Option<String>>,
    ) -> Library {
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(v) = scene {
                inner.scene = v;
            }
            if let Some(v) = setup {
                inner.setup = v;
            }
        }
        self.dirty.store(true, Ordering::Release);
        let library = self.library();
        self.announce(Notification::LibraryChanged(library.clone()));
        library
    }

    /// Look strips and buses named in `params` up by name; see
    /// [`names::resolve_names`].
    pub fn resolve_names(&self, method: &str, params: &mut Option<Value>) -> Result<(), RpcError> {
        match params {
            Some(p) => names::resolve_names(method, p, &self.inner.lock().unwrap().mixer),
            None => Ok(()),
        }
    }

    /// Called at the meter rate: publish meters and spectra, and pick up
    /// sample rate or quantum changes that the real-time thread reported.
    pub fn tick(&self) {
        let Some(engine) = &self.engine else {
            return;
        };
        if self.notify.receiver_count() > 0 {
            self.announce(Notification::Meters(engine.take_meters()));
        }
        // Called even when nobody watches, since that is also what tells
        // the engine to stop feeding the analyzer.
        let watched: BTreeSet<StripOrBus> = self
            .spectrum_watchers
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect();
        for spectrum in engine.take_spectra(&watched) {
            self.announce(Notification::Spectrum(spectrum));
        }
        let status = engine.status();
        let changed = {
            let mut inner = self.inner.lock().unwrap();
            let prev = &inner.engine;
            let changed = prev.state != status.state
                || prev.sample_rate != status.sample_rate
                || prev.quantum != status.quantum
                || prev.error != status.error;
            if changed {
                inner.engine = EngineStatus {
                    node_id: status.node_id.or(prev.node_id),
                    ..status
                };
            }
            changed.then(|| inner.engine.clone())
        };
        if let Some(status) = changed {
            self.announce(Notification::EngineChanged(status));
        }
    }

    /// Apply a change to the mixer, recording it in the undo history as
    /// `step` when given. `f` changes a copy; if it fails nothing changes.
    /// Returns what `f` returned, as JSON. Hotkeys follow the strips and
    /// buses it renames.
    fn mutate<T: serde::Serialize>(
        &self,
        step: Option<Step>,
        f: impl FnOnce(&mut MixerState) -> Result<T, RpcError>,
    ) -> Result<Value, RpcError> {
        self.change(step, false, f)
    }

    /// Like [`Self::mutate`], for putting another mixer in place of this
    /// one, as loading a setup does: a strip that keeps its id may be
    /// another strip there, so none is taken as renamed.
    fn replace_mixer<T: serde::Serialize>(
        &self,
        step: Option<Step>,
        f: impl FnOnce(&mut MixerState) -> Result<T, RpcError>,
    ) -> Result<Value, RpcError> {
        self.change(step, true, f)
    }

    fn change<T: serde::Serialize>(
        &self,
        step: Option<Step>,
        whole: bool,
        f: impl FnOnce(&mut MixerState) -> Result<T, RpcError>,
    ) -> Result<Value, RpcError> {
        let (result, state, history, renamed) = {
            let mut inner = self.inner.lock().unwrap();
            let mut candidate = inner.mixer.clone();
            let result = f(&mut candidate)?;
            candidate.normalize();
            let before = std::mem::replace(&mut inner.mixer, candidate.clone());
            let step = step.map(|s| if whole { s.whole_mixer() } else { s });
            let changed = step.is_some_and(|s| inner.history.record(s, &before, &candidate));
            let renamed = if whole {
                Vec::new()
            } else {
                names::renames(&before, &candidate)
            };
            (
                result,
                candidate,
                changed.then(|| inner.history.info()),
                renamed,
            )
        };
        self.follow_renames(&renamed);
        self.push_state(state);
        if let Some(info) = history {
            self.announce(Notification::HistoryChanged(info));
        }
        Ok(to_json(&result))
    }

    /// Undo (`back`) or redo up to `steps` steps.
    fn step_history(&self, steps: Option<u32>, back: bool) -> Result<Value, RpcError> {
        let steps = steps.unwrap_or(1).clamp(1, 100);
        let (state, info, labels, renamed) = {
            let mut inner = self.inner.lock().unwrap();
            let start = inner.mixer.clone();
            let mut whole = false;
            let mut labels = Vec::new();
            for _ in 0..steps {
                whole |= inner.history.next_is_whole(back);
                let label = {
                    let info = inner.history.info();
                    let list = if back { info.undo } else { info.redo };
                    list.first().map(|e| e.label.clone())
                };
                let current = inner.mixer.clone();
                let next = if back {
                    inner.history.undo(&current)
                } else {
                    inner.history.redo(&current)
                };
                match next {
                    Some(s) => {
                        inner.mixer = s;
                        labels.extend(label);
                    }
                    None => break,
                }
            }
            if labels.is_empty() {
                return Err(RpcError::application(if back {
                    "nothing to undo"
                } else {
                    "nothing to redo"
                }));
            }
            // Undoing a rename takes the old name back, in hotkeys too;
            // not undoing a setup loaded, which only looks like renames.
            let renamed = if whole {
                Vec::new()
            } else {
                names::renames(&start, &inner.mixer)
            };
            (inner.mixer.clone(), inner.history.info(), labels, renamed)
        };
        self.follow_renames(&renamed);
        info!(
            "{} {}",
            if back { "undid" } else { "redid" },
            labels.join(", ")
        );
        self.push_state(state);
        self.announce(Notification::HistoryChanged(info.clone()));
        Ok(to_json(&info))
    }

    /// A new mixer state: to the engine, to the config file, to clients.
    fn push_state(&self, state: MixerState) {
        if let Some(engine) = &self.engine {
            if let Err(e) = engine.set_state(state.clone()) {
                warn!("engine rejected state: {e}");
            }
        }
        self.dirty.store(true, Ordering::Release);
        self.check_hotkey_targets(&state);
        self.announce(Notification::StateChanged(state));
    }

    /// Built-in presets first, then the user's own.
    fn eq_presets(&self) -> Vec<EqPreset> {
        let mut all = builtin_eq_presets();
        if let Ok(user) = &self.inner.lock().unwrap().user_eq_presets {
            all.extend(user.iter().cloned());
        }
        all
    }

    /// The preset called `name`, ignoring case.
    fn find_eq_preset(&self, name: &str) -> Result<EqPreset, RpcError> {
        self.eq_presets()
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| RpcError::application(format!("no equalizer preset called '{name}'")))
    }

    /// Change the user's presets with `f`, write them out and tell clients.
    fn edit_eq_presets(
        &self,
        f: impl FnOnce(&mut Vec<EqPreset>) -> Result<(), RpcError>,
    ) -> Result<Value, RpcError> {
        {
            let mut inner = self.inner.lock().unwrap();
            let user = inner.user_eq_presets.as_mut().map_err(|e| {
                RpcError::application(format!(
                    "equalizer presets are read-only until this is fixed: {e}"
                ))
            })?;
            let mut candidate = user.clone();
            f(&mut candidate)?;
            config::save_eq_presets(&self.paths.eq_presets_file, &candidate)
                .map_err(|e| RpcError::application(format!("{e:#}")))?;
            *user = candidate;
        }
        let all = self.eq_presets();
        self.announce(Notification::EqPresetsChanged(all.clone()));
        Ok(to_json(&all))
    }
}
