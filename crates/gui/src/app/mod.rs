//! The mixer window: a strip for each input and a bus for each output, side
//! by side, under a bar of menus.
//!
//! The window is a thin client. What it shows comes from the daemon's
//! state, which the [`Client`] keeps a copy of, and every change goes back
//! to the daemon as a request. Two things make that feel immediate:
//!
//! * A control being dragged, such as a fader, keeps its own value for a
//!   moment ([`App::value`]), so an echo of an older value cannot pull it
//!   back, and sends at most every [`SEND_INTERVAL`].
//! * Meters move smoothly here between the daemon's readings, falling at a
//!   steady rate and holding their peaks.
//!
//! The parts:
//!
//! * `top_bar`: the engine's state, the menus, and which mix the faders
//!   show.
//! * `mixer`: the row of strips and buses, and dragging them into order.
//! * `strip` and `bus`: one of each.
//! * `controls`: what strips and buses both have, such as the name field.
//! * `meters`: turning the daemon's readings into moving meters.
//! * `apps`: the Apps menu and the App rules window.
//! * `library`: scenes and setups.
//! * `dialogs`: adding and removing strips and buses.
//! * `settings`: the Preferences and About windows.
//! * `history`: undo, redo, and the messages that report them.
//! * `status`: what shows while there is no daemon to talk to.

mod apps;
mod bus;
mod controls;
mod dialogs;
mod history;
mod library;
mod meters;
mod mixer;
mod settings;
mod status;
mod strip;
mod top_bar;

pub(crate) use history::{history_shortcut, HistoryKey};

use crate::appearance;
use crate::client::{Client, DaemonExit, Shared};
use crate::fx_window::{CompView, FxTarget, FxWindow, GateView, Section, WindowMeters};
use crate::patch::CommonPatch;
use crate::prefs::{Appearance, Prefs, SpectrumView};
use crate::theme;
use dialogs::{AddBusDialog, AddStripDialog};
use egui::Frame;
use history::Toast;
use library::{LibraryConfirm, LibraryKind};
use meters::MeterDisplay;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use weir_protocol::*;

/// Minimum interval between two updates sent for the same control.
const SEND_INTERVAL: Duration = Duration::from_millis(40);
/// How long a locally edited value wins over the daemon's echo.
const OVERRIDE_TTL: Duration = Duration::from_millis(300);

/// A control whose value is dragged, and so kept here while it moves.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    /// A strip's or bus's fader.
    Gain(StripOrBus),
    StripPan(StripId),
    /// A strip's level in one bus's mix.
    StripSend(StripId, BusId),
    BusCeiling(BusId),
    BusRelease(BusId),
    AppVolume(u32),
}

/// What the window reads from the client at the start of a frame, so the
/// lock is held only for that long.
struct Snapshot {
    connected: bool,
    connect_error: Option<String>,
    last_error: Option<String>,
    state: FullState,
    /// New meter readings and their sequence number, if any came.
    meters: Option<(Meters, u64)>,
    spawned: bool,
    /// Why the daemon this window started stopped, if it did.
    daemon_exit: Option<DaemonExit>,
    show_seq: u64,
    quit_requested: bool,
    /// Recent spectra, by strip or bus.
    spectra: HashMap<StripOrBus, Spectrum>,
    history: HistoryInfo,
}

/// The mixer window.
pub struct App {
    client: Client,
    socket: PathBuf,
    /// Requests made this frame, sent at the end of it.
    actions: Vec<Request>,
    /// Values of controls being dragged, and when they last changed.
    overrides: HashMap<Key, (f32, Instant)>,
    /// Values of controls that changed since they were last sent.
    pending: HashMap<Key, f32>,
    last_flush: Instant,
    meters: HashMap<StripOrBus, MeterDisplay>,
    last_meter_seq: u64,
    last_frame: Instant,
    /// Names being typed into strips' and buses' name fields.
    name_edits: HashMap<StripOrBus, String>,
    add_strip: Option<AddStripDialog>,
    add_bus: Option<AddBusDialog>,
    /// The "save as" dialog of a scene or setup, and the name typed so far.
    save_as: Option<(LibraryKind, String)>,
    /// Put the cursor in the "save as" name field on its first frame only:
    /// asking every frame would take focus back as Enter releases it, and
    /// Enter would never save.
    focus_save_as: bool,
    confirm_library: Option<LibraryConfirm>,
    /// A strip or bus waiting for a yes before it is removed, and its name.
    confirm_remove: Option<(StripOrBus, String)>,
    /// A rename waiting for a yes, because it renames external effects'
    /// devices too.
    confirm_rename: Option<dialogs::RenameConfirm>,
    show_about: bool,
    show_prefs: bool,
    prefs: Prefs,
    /// Minimize on the first frame, for `--minimized`.
    start_minimized: bool,
    /// Last "come forward" request acted on.
    last_show_seq: u64,
    /// Measured height of one application entry, including the gap after it.
    /// A menu button's height depends on the theme's padding and the font, so
    /// this is measured while drawing and used on the next frame rather than
    /// predicted from constants.
    app_pitch: f32,
    /// The bus whose mix the strip faders show, or `None` for their own
    /// levels.
    mix_view: Option<BusId>,
    /// Open settings windows, one per strip or bus at most.
    fx_windows: Vec<FxWindow>,
    /// What each gated strip's gate is doing, from the meters.
    gate_views: HashMap<StripId, GateView>,
    /// What each compressed strip's compressor is doing, from the meters.
    comp_views: HashMap<StripId, CompView>,
    /// How far ducking is turning each strip down, in dB, from the meters.
    duck_views: HashMap<StripId, f32>,
    /// The spectra last asked for, to ask again only when that changes.
    spectrum_watch: Vec<StripOrBus>,
    toast: Option<Toast>,
    /// The App rules window, and the name typed into its "by name" field.
    show_rules: bool,
    rule_name: String,
    /// What can be undone and redone, as of this frame.
    history: HistoryInfo,
    /// Strips and buses that have clipped since their CLIP light was last
    /// cleared.
    clips: HashSet<StripOrBus>,
    /// How far each bus's limiter is turning it down, as displayed: it
    /// jumps to a new reduction and recovers slowly, so short bursts are
    /// still readable.
    reductions: HashMap<BusId, f32>,
    /// Whether the desktop's font is in use, once that has been decided.
    desktop_font: Option<bool>,
    /// Weir's icon for the About window, made into a texture the first time
    /// that window opens.
    about_icon: Option<egui::TextureHandle>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        socket: PathBuf,
        allow_spawn: bool,
        start_minimized: bool,
    ) -> Self {
        // The first frame puts the right palette in; see `palette`.
        theme::apply(&cc.egui_ctx, theme::DARK);
        appearance::set_context(&cc.egui_ctx);
        let client = Client::spawn(socket.clone(), cc.egui_ctx.clone(), allow_spawn);
        Self {
            client,
            socket,
            actions: Vec::new(),
            overrides: HashMap::new(),
            pending: HashMap::new(),
            last_flush: Instant::now(),
            meters: HashMap::new(),
            last_meter_seq: 0,
            last_frame: Instant::now(),
            name_edits: HashMap::new(),
            add_strip: None,
            add_bus: None,
            save_as: None,
            focus_save_as: false,
            confirm_library: None,
            confirm_remove: None,
            confirm_rename: None,
            show_about: false,
            show_prefs: false,
            prefs: Prefs::load(),
            start_minimized,
            last_show_seq: 0,
            app_pitch: 45.0,
            mix_view: None,
            fx_windows: Vec::new(),
            gate_views: HashMap::new(),
            comp_views: HashMap::new(),
            duck_views: HashMap::new(),
            clips: HashSet::new(),
            toast: None,
            show_rules: false,
            rule_name: String::new(),
            history: HistoryInfo::default(),
            reductions: HashMap::new(),
            desktop_font: None,
            about_icon: None,
            spectrum_watch: Vec::new(),
        }
    }

    fn snapshot(&self) -> Snapshot {
        let sh: std::sync::MutexGuard<'_, Shared> = self.client.shared.lock().unwrap();
        let err = sh
            .last_error
            .as_ref()
            .filter(|(_, t)| t.is_none_or(|t| t.elapsed() < Duration::from_secs(6)))
            .map(|(m, _)| m.clone());
        let meters = if sh.meters_seq != self.last_meter_seq {
            Some((sh.meters.clone(), sh.meters_seq))
        } else {
            None
        };
        Snapshot {
            connected: sh.connected,
            connect_error: sh.connect_error.clone(),
            last_error: err,
            state: sh.state.clone(),
            meters,
            spawned: sh.spawned_daemon,
            daemon_exit: sh.daemon_exit.clone(),
            show_seq: sh.show_seq,
            quit_requested: sh.quit_requested,
            // Anything older than this stopped coming; do not draw it frozen.
            spectra: sh
                .spectra
                .iter()
                .filter(|(_, (_, at))| at.elapsed() < Duration::from_millis(600))
                .map(|(t, (s, _))| (*t, s.clone()))
                .collect(),
            history: sh.history.clone(),
        }
    }

    /// The palette the preferences and the desktop ask for. Dark unless the
    /// desktop says it prefers light, since that is how Weir looks without
    /// being told.
    fn palette(&self) -> theme::Palette {
        let dark = match self.prefs.appearance {
            Appearance::Dark => true,
            Appearance::Light => false,
            Appearance::System => appearance::scheme() != appearance::Scheme::Light,
        };
        let palette = if dark { theme::DARK } else { theme::LIGHT };
        match appearance::accent().filter(|_| self.prefs.system_accent) {
            Some(accent) => theme::with_accent(palette, accent),
            None => palette,
        }
    }

    /// Follow the desktop's colors and font. Checked every frame: the
    /// desktop can change its mind at any time, and the watcher asks for a
    /// frame when it does.
    fn follow_desktop(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        if palette != theme::p() {
            theme::apply(ctx, palette);
        }
        if self.desktop_font != Some(self.prefs.system_font) {
            appearance::use_desktop_font(ctx, self.prefs.system_font);
            self.desktop_font = Some(self.prefs.system_font);
        }
    }

    /// Act on what the daemon asked of the window: to close with it, or to
    /// come forward from the tray.
    fn window_requests(&mut self, ctx: &egui::Context, quit_requested: bool, show_seq: u64) {
        if quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if show_seq != self.last_show_seq {
            self.last_show_seq = show_seq;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if self.start_minimized {
            self.start_minimized = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
    }

    /// Open the settings window of a strip or bus, or bring it forward if
    /// it is already open.
    fn open_fx(&mut self, target: FxTarget) {
        match self.fx_windows.iter_mut().find(|w| w.target == target) {
            Some(w) => w.raise = true,
            None => {
                let mut w = FxWindow::new(target);
                let remembered = match target {
                    FxTarget::Strip(_) => self.prefs.strip_fx_size,
                    FxTarget::Bus(_) => self.prefs.bus_settings_size,
                };
                if let Some(size) = remembered {
                    w.size = size;
                }
                self.fx_windows.push(w);
            }
        }
    }

    /// Open a settings window straight at one of its sections.
    fn open_fx_at(&mut self, target: FxTarget, section: Section) {
        self.open_fx(target);
        if let Some(w) = self.fx_windows.iter_mut().find(|w| w.target == target) {
            w.open_section(section);
        }
    }

    /// Draw the open settings windows, handing each the readings it shows,
    /// and remember their sizes as they close.
    fn show_fx_windows(
        &mut self,
        ctx: &egui::Context,
        state: &FullState,
        spectra: &HashMap<StripOrBus, Spectrum>,
    ) {
        let mut windows = std::mem::take(&mut self.fx_windows);
        for w in windows.iter_mut() {
            let meters = match w.target {
                FxTarget::Strip(id) => WindowMeters {
                    gate: self.gate_views.get(&id).copied(),
                    comp: self.comp_views.get(&id).copied(),
                    duck: self.duck_views.get(&id).copied(),
                    limiter: None,
                },
                FxTarget::Bus(id) => WindowMeters {
                    limiter: self.reductions.get(&id).copied(),
                    ..Default::default()
                },
            };
            w.spectrum = spectra.get(&w.target).cloned();
            w.view = self.prefs.spectrum;
            w.show(ctx, state, meters, &mut self.actions);
            if let Some(key) = w.history_key.take() {
                self.step_history(key);
            }
            if w.view != self.prefs.spectrum {
                self.prefs.spectrum = w.view;
                self.prefs.save();
            }
        }
        let mut resized = false;
        for w in windows.iter().filter(|w| w.closed) {
            let slot = match w.target {
                FxTarget::Strip(_) => &mut self.prefs.strip_fx_size,
                FxTarget::Bus(_) => &mut self.prefs.bus_settings_size,
            };
            if *slot != Some(w.size) {
                *slot = Some(w.size);
                resized = true;
            }
        }
        if resized {
            self.prefs.save();
        }
        windows.retain(|w| !w.closed);
        self.fx_windows = windows;

        // Spectra cost the daemon an FFT each, so only ask for those of open
        // windows, and none at all with the analyzer switched off.
        let wanted: Vec<StripOrBus> = if self.prefs.spectrum == SpectrumView::Off {
            Vec::new()
        } else {
            self.fx_windows.iter().map(|w| w.target).collect()
        };
        if wanted != self.spectrum_watch {
            self.client.watch_spectrum(wanted.clone());
            self.spectrum_watch = wanted;
        }
    }

    /// The value a dragged control shows: its own while it is being
    /// dragged, the daemon's otherwise.
    fn value(&self, key: Key, from_state: f32) -> f32 {
        self.overrides.get(&key).map_or(from_state, |(v, _)| *v)
    }

    /// Set a dragged control, to be sent by the next [`App::flush`].
    fn set_value(&mut self, key: Key, v: f32) {
        self.overrides.insert(key, (v, Instant::now()));
        self.pending.insert(key, v);
    }

    /// Send what changed: dragged controls at most every [`SEND_INTERVAL`]
    /// unless `force`, everything else straight away.
    fn flush(&mut self, force: bool) {
        let now = Instant::now();
        if !self.pending.is_empty()
            && (force || now.duration_since(self.last_flush) >= SEND_INTERVAL)
        {
            for (key, v) in self.pending.drain() {
                self.actions.push(key_request(key, v));
            }
            self.last_flush = now;
        }
        let pending = &self.pending;
        self.overrides
            .retain(|k, (_, t)| pending.contains_key(k) || t.elapsed() < OVERRIDE_TTL);
        for a in self.actions.drain(..) {
            self.client.send(a);
        }
    }
}

/// The request setting the control `key` to `v`.
fn key_request(key: Key, v: f32) -> Request {
    match key {
        Key::Gain(target) => CommonPatch {
            gain_db: Some(v),
            ..Default::default()
        }
        .to(target),
        Key::StripPan(id) => Request::SetStrip(StripPatch {
            id,
            pan: Some(v),
            ..Default::default()
        }),
        Key::BusCeiling(id) => Request::SetBus(BusPatch {
            id,
            limiter: Some(LimiterPatch {
                ceiling_db: Some(v),
                ..Default::default()
            }),
            ..Default::default()
        }),
        Key::BusRelease(id) => Request::SetBus(BusPatch {
            id,
            limiter: Some(LimiterPatch {
                release_ms: Some(v),
                ..Default::default()
            }),
            ..Default::default()
        }),
        Key::AppVolume(app) => Request::SetAppVolume(AppVolumeParams {
            app,
            volume_db: Some(v),
            volume_delta_db: None,
            mute: None,
        }),
        Key::StripSend(strip, bus) => Request::SetRoute(RouteParams {
            strip,
            bus,
            enabled: None,
            level_db: Some(v),
            level_delta_db: None,
        }),
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.follow_desktop(ctx);
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.25);
        self.last_frame = now;
        let Snapshot {
            connected,
            connect_error,
            last_error,
            state,
            meters,
            spawned,
            daemon_exit,
            show_seq,
            quit_requested,
            spectra,
            history,
        } = self.snapshot();
        self.history = history;
        if connected && self.prefs.learn_device_names(&state) {
            self.prefs.save();
        }
        if connected {
            if let Some(key) = history_shortcut(ctx) {
                self.step_history(key);
            }
        }
        self.window_requests(ctx, quit_requested, show_seq);
        if let Some((m, seq)) = meters {
            self.last_meter_seq = seq;
            self.take_meters(&m);
        }
        self.animate_meters(dt, now);

        self.top_bar(ctx, connected, &state, last_error.as_deref());
        if connected {
            self.mix_bar(ctx, &state);
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::p().bg).inner_margin(8))
            .show(ctx, |ui| {
                if !connected {
                    match &daemon_exit {
                        Some(exit) => self.daemon_stopped_view(ui, exit),
                        None => self.disconnected_view(ui, connect_error.as_deref(), spawned),
                    }
                    return;
                }
                self.mixer_view(ui, &state);
            });
        self.dialogs(ctx, &state);
        self.rules_window(ctx, &state);
        self.library_dialogs(ctx, &state);
        self.settings_windows(ctx, &state);
        self.toast_view(ctx);
        self.show_fx_windows(ctx, &state, &spectra);
        self.flush(false);
        ctx.request_repaint_after(Duration::from_millis(if connected { 33 } else { 500 }));
    }
}

/// A bus's short name: A1, A2... for hardware, B1, B2... for virtual.
pub(crate) fn bus_label(state: &MixerState, bus: &Bus) -> String {
    state.bus_label(bus.id).unwrap_or_default()
}

/// A bus's short name and its name, "A1 Headset", for lists of buses.
fn bus_title(state: &MixerState, bus: &Bus) -> String {
    format!("{} {}", bus_label(state, bus), bus.name)
}

/// A sample rate as people say it: "48 kHz", "44.1 kHz".
fn fmt_rate(hz: u32) -> String {
    if hz.is_multiple_of(1000) {
        format!("{} kHz", hz / 1000)
    } else {
        format!("{:.1} kHz", hz as f32 / 1000.0)
    }
}

/// How long `frames` last at `rate`, in milliseconds.
fn frames_ms(frames: u32, rate: u32) -> f32 {
    frames as f32 * 1000.0 / rate.max(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_buffers_read_like_people_say_them() {
        assert_eq!(fmt_rate(48_000), "48 kHz");
        assert_eq!(fmt_rate(44_100), "44.1 kHz");
        assert_eq!(frames_ms(480, 48_000), 10.0);
        assert!(
            frames_ms(256, 0) > 0.0,
            "an unknown rate does not divide by zero"
        );
    }

    #[test]
    fn a_dragged_fader_goes_to_its_strip_or_bus() {
        let Request::SetBus(p) = key_request(Key::Gain(StripOrBus::Bus(2)), -6.0) else {
            panic!("expected set_bus");
        };
        assert_eq!((p.id, p.gain_db), (2, Some(-6.0)));
        let Request::SetRoute(r) = key_request(Key::StripSend(1, 2), -3.0) else {
            panic!("expected set_route");
        };
        assert_eq!(
            (r.strip, r.bus, r.level_db, r.enabled),
            (1, 2, Some(-3.0), None)
        );
    }
}
