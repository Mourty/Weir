//! The settings window of a strip or bus: its equalizer with an editable
//! curve in the middle, and down the side everything else about it, each in
//! a section that folds away. For strips: noise suppression, the noise
//! gate, the compressor, the fader, ducking and upmixing. For buses:
//! downmixing, the fader and the safety limiter. Both have external
//! effects, whose section sits wherever they are in the chain.
//!
//! A row along the top shows the signal chain, the order things happen in,
//! with the ones that are on lit up. Clicking one opens its section, and
//! external effects are dragged along it to move them.
//!
//! Each window is its own native window (an egui viewport), so it can sit on
//! another screen while the mixer stays where it is.
//!
//! Controls that are dragged keep what they are set to here, as a [`Local`],
//! and send it at most every [`SEND_INTERVAL`]. Until the daemon has had
//! time to echo a change back, the local copy wins, so a slider never jumps
//! back to an older value under the pointer. Controls that change in one
//! click send their request straight away.
//!
//! The parts:
//!
//! * `chain`: the signal chain along the top.
//! * `section`: the folding header every side section has.
//! * `fader`, `dynamics`, `ducking`, `channels` and `external`: the side
//!   sections.
//! * `eq` and `curve`: the equalizer, and the graph it is edited on.

mod chain;
mod channels;
mod curve;
mod delay;
mod ducking;
mod dynamics;
mod eq;
mod external;
mod fader;
mod section;

use crate::patch::CommonPatch;
use crate::prefs::SpectrumView;
use crate::theme;
use egui::{vec2, Align, Layout, Rect, Ui};
use std::time::{Duration, Instant};
use weir_protocol::*;

/// Minimum interval between two updates of the same setting.
const SEND_INTERVAL: Duration = Duration::from_millis(40);
/// How long a value edited here wins over the daemon's copy after the last
/// edit, so an echo of an older value cannot yank a control back.
const LOCAL_TTL: Duration = Duration::from_millis(400);
/// Width of the column of sections down the side.
const SIDE_W: f32 = 270.0;

/// The strip or bus a window belongs to.
pub type FxTarget = StripOrBus;

/// A value being edited in this window.
struct Local<T> {
    value: T,
    edited: Instant,
    /// Changed since it was last sent.
    unsent: bool,
}

impl<T> Local<T> {
    fn new(value: T) -> Self {
        Self {
            value,
            edited: Instant::now(),
            unsent: true,
        }
    }
}

/// The value in `slot` if it has changed since it was last sent, marking it
/// sent.
fn take_unsent<T: Clone>(slot: &mut Option<Local<T>>) -> Option<T> {
    let local = slot.as_mut().filter(|l| l.unsent)?;
    local.unsent = false;
    Some(local.value.clone())
}

/// Forget a local copy once it has been sent and the daemon has had time to
/// echo it.
fn expire<T>(slot: &mut Option<Local<T>>) {
    if slot
        .as_ref()
        .is_some_and(|l| !l.unsent && l.edited.elapsed() > LOCAL_TTL)
    {
        *slot = None;
    }
}

/// What the mixer window knows about a strip's gate, for the level display.
#[derive(Clone, Copy, Default)]
pub struct GateView {
    /// Input level as shown, in dBFS, falling smoothly.
    pub level: f32,
    /// How far the gate is turning the strip down, in dB.
    pub reduction: f32,
}

/// What the mixer window knows about a strip's compressor, for its display.
#[derive(Clone, Copy, Default)]
pub struct CompView {
    /// Input level as shown, in dBFS, falling smoothly.
    pub level: f32,
    /// How far it is turning the strip down, in dB (0 or less), recovering
    /// smoothly.
    pub reduction: f32,
}

/// The live readings a settings window shows, handed in by the mixer
/// window, which receives the meters.
#[derive(Clone, Copy, Default)]
pub struct WindowMeters {
    pub gate: Option<GateView>,
    pub comp: Option<CompView>,
    /// How far ducking is turning the strip down, in dB.
    pub duck: Option<f32>,
    /// How far a bus's limiter is turning it down, in dB (0 or less).
    pub limiter: Option<f32>,
}

/// The folding sections of a settings window, strips' and buses' alike.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Denoise,
    Gate,
    Comp,
    Fader,
    Duck,
    Upmix,
    Downmix,
    Limiter,
    Delay,
    /// External effects.
    Insert,
}

/// How many [`Section`]s there are.
const SECTIONS: usize = 10;

/// One settings window, and what is being edited in it.
pub struct FxWindow {
    pub target: FxTarget,
    bands: Option<Local<Vec<EqBand>>>,
    gate: Option<Local<Gate>>,
    denoise: Option<Local<Denoise>>,
    comp: Option<Local<Compressor>>,
    duck: Option<Local<Ducking>>,
    gain: Option<Local<f32>>,
    pan: Option<Local<f32>>,
    limiter: Option<Local<Limiter>>,
    delay: Option<Local<f32>>,
    /// Which sections are unfolded, by [`Section`]. Decided on the first
    /// frame: the effects that are on start unfolded.
    open: Option<[bool; SECTIONS]>,
    /// A section to scroll into view on the next frame, after a click on
    /// the signal chain.
    focus: Option<Section>,
    last_send: Instant,
    /// The equalizer band picked on the curve or in the table.
    selected: Option<usize>,
    /// The band whose handle is being dragged. Its local copy is kept for
    /// as long as the drag lasts.
    dragging: Option<usize>,
    /// Vertical range of the curve, +/- this many dB.
    range_db: f32,
    /// The name being typed for a new equalizer preset, while the field
    /// for it is open.
    save_name: Option<String>,
    /// Put the cursor in the preset name field on its first frame. Asking on
    /// every frame would take focus straight back when Enter releases it,
    /// and Enter would never save.
    focus_save_name: bool,
    /// Bring the window to the front on the next frame.
    pub raise: bool,
    /// The window's size: what it opens at, and then what it has been
    /// resized to, so the mixer can remember it for next time.
    pub size: [f32; 2],
    pub closed: bool,
    /// The newest spectrum of this window's equalizer, handed in by the
    /// mixer each frame; `None` when there is nothing recent.
    pub spectrum: Option<Spectrum>,
    /// What the analyzer shows. Shared by every settings window, so the
    /// mixer hands it in and reads it back.
    pub view: SpectrumView,
    /// Undo or redo pressed in this window, for the mixer to act on.
    pub history_key: Option<crate::app::HistoryKey>,
    /// One of the user's presets to export, asked for in the preset menu,
    /// for the mixer to ask where.
    pub export_preset: Option<String>,
}

impl FxWindow {
    pub fn new(target: FxTarget) -> Self {
        Self {
            target,
            bands: None,
            gate: None,
            denoise: None,
            comp: None,
            duck: None,
            gain: None,
            pan: None,
            limiter: None,
            delay: None,
            open: None,
            focus: None,
            last_send: Instant::now(),
            selected: None,
            dragging: None,
            range_db: 12.0,
            save_name: None,
            focus_save_name: false,
            raise: false,
            size: [1000.0, 660.0],
            closed: false,
            spectrum: None,
            view: SpectrumView::default(),
            history_key: None,
            export_preset: None,
        }
    }

    /// Unfold `section` and scroll to it on the next frame.
    pub fn open_section(&mut self, section: Section) {
        self.focus = Some(section);
    }

    fn viewport_id(&self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("fx", self.target))
    }

    /// Draw the window. Requests for the daemon are appended to `actions`.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        state: &FullState,
        meters: WindowMeters,
        actions: &mut Vec<Request>,
    ) {
        let Some(name) = self.target_name(state) else {
            self.closed = true;
            return;
        };
        let title = format!("{name} settings · Weir");
        let id = self.viewport_id();
        if self.raise {
            self.raise = false;
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
        }
        let size = self.size;
        let builder = egui::ViewportBuilder::default()
            .with_title(title.clone())
            .with_app_id("weir")
            .with_icon(crate::icon())
            .with_inner_size(size)
            .with_min_inner_size([620.0, 440.0]);
        ctx.show_viewport_immediate(id, builder, |ctx, class| {
            if class == egui::ViewportClass::Embedded {
                // No separate windows on this backend: fall back to one
                // floating inside the mixer.
                let mut open = true;
                egui::Window::new(title.clone())
                    .id(egui::Id::new(("fx_embedded", self.target)))
                    .open(&mut open)
                    .default_size(size)
                    .show(ctx, |ui| self.contents(ui, state, meters, actions));
                if !open {
                    self.closed = true;
                }
            } else {
                // Undo and redo work from here too; the mixer window has the
                // labels, so it is only told which way to go.
                if let Some(key) = crate::app::history_shortcut(ctx) {
                    self.history_key = Some(key);
                }
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::p().bg).inner_margin(10))
                    .show(ctx, |ui| self.contents(ui, state, meters, actions));
                if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
                    self.size = [rect.width().round(), rect.height().round()];
                }
                if ctx.input(|i| i.viewport().close_requested()) {
                    self.closed = true;
                }
                // Keep the gate's and compressor's displays moving.
                ctx.request_repaint_after(Duration::from_millis(33));
            }
        });
        self.flush(actions, self.closed);
    }

    fn target_name(&self, state: &FullState) -> Option<String> {
        match self.target {
            FxTarget::Strip(id) => state.mixer.strip(id).map(|s| s.name.clone()),
            FxTarget::Bus(id) => state.mixer.bus(id).map(|b| b.name.clone()),
        }
    }

    /// Send whatever changed, at most every [`SEND_INTERVAL`] unless `force`
    /// says the window is closing, and forget local copies once the daemon
    /// has had time to echo them.
    fn flush(&mut self, actions: &mut Vec<Request>, force: bool) {
        if force || self.last_send.elapsed() >= SEND_INTERVAL {
            let before = actions.len();
            if let Some(bands) = take_unsent(&mut self.bands) {
                actions.push(self.eq_request(EqPatch {
                    enabled: None,
                    bands: Some(bands),
                }));
            }
            if let Some(gain) = take_unsent(&mut self.gain) {
                let patch = CommonPatch {
                    gain_db: Some(gain),
                    ..Default::default()
                };
                actions.push(patch.to(self.target));
            }
            // One request per control, so each change is undone under its
            // own name.
            match self.target {
                FxTarget::Strip(id) => {
                    let patches = [
                        take_unsent(&mut self.gate).map(|g| StripPatch {
                            gate: Some(g.into()),
                            ..Default::default()
                        }),
                        take_unsent(&mut self.denoise).map(|d| StripPatch {
                            denoise: Some(d.into()),
                            ..Default::default()
                        }),
                        take_unsent(&mut self.comp).map(|c| StripPatch {
                            compressor: Some(c.into()),
                            ..Default::default()
                        }),
                        take_unsent(&mut self.duck).map(|d| StripPatch {
                            ducking: Some(d.into()),
                            ..Default::default()
                        }),
                        take_unsent(&mut self.pan).map(|p| StripPatch {
                            pan: Some(p),
                            ..Default::default()
                        }),
                    ];
                    actions.extend(
                        patches
                            .into_iter()
                            .flatten()
                            .map(|p| Request::SetStrip(StripPatch { id, ..p })),
                    );
                }
                FxTarget::Bus(id) => {
                    if let Some(l) = take_unsent(&mut self.limiter) {
                        actions.push(Request::SetBus(BusPatch {
                            id,
                            limiter: Some(l.into()),
                            ..Default::default()
                        }));
                    }
                    if let Some(ms) = take_unsent(&mut self.delay) {
                        actions.push(Request::SetBus(BusPatch {
                            id,
                            delay_ms: Some(ms),
                            ..Default::default()
                        }));
                    }
                }
            }
            if actions.len() > before {
                self.last_send = Instant::now();
            }
        }
        // The bands being dragged stay local until the drag ends, however
        // long that takes.
        if self.dragging.is_none() {
            expire(&mut self.bands);
        }
        expire(&mut self.gate);
        expire(&mut self.denoise);
        expire(&mut self.comp);
        expire(&mut self.duck);
        expire(&mut self.gain);
        expire(&mut self.delay);
        expire(&mut self.pan);
        expire(&mut self.limiter);
    }

    /// The equalizer on the left, and the side sections on the right, under
    /// the signal chain.
    fn contents(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        meters: WindowMeters,
        actions: &mut Vec<Request>,
    ) {
        self.chain_row(ui, state, actions);
        ui.add_space(4.0);
        let avail = ui.available_size();
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(vec2(SIDE_W, avail.y), Layout::top_down(Align::Min), |ui| {
                ui.set_width(SIDE_W);
                egui::ScrollArea::vertical()
                    .id_salt("fx_side")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Leave room for the scroll bar, which floats
                        // over the right edge.
                        ui.set_max_width(SIDE_W - 14.0);
                        self.side(ui, state, meters, actions);
                    });
            });
            ui.separator();
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), avail.y),
                Layout::top_down(Align::Min),
                |ui| self.eq_section(ui, state, actions),
            );
        });
    }

    /// The folding sections down the side, in the order things happen to
    /// the sound, external effects included wherever they are.
    fn side(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        meters: WindowMeters,
        actions: &mut Vec<Request>,
    ) {
        use Section::*;
        let target = self.target;
        let mut open = *self
            .open
            .get_or_insert_with(|| first_open(target, &state.mixer));
        let focus = self.focus.take();
        if let Some(section) = focus {
            open[section as usize] = true;
        }
        let at = external::settings(state, target).map(|(_, i)| i.position);
        let order = side_order(target, at.unwrap_or_default());
        for (k, &section) in order.iter().enumerate() {
            if k > 0 {
                ui.add_space(6.0);
            }
            let top = ui.cursor().min;
            let o = &mut open[section as usize];
            match (section, target) {
                (Fader, _) => self.fader_section(ui, state, o, actions),
                (Insert, _) => self.insert_section(ui, state, o, actions),
                (_, FxTarget::Strip(id)) => {
                    let Some(strip) = state.mixer.strip(id) else {
                        return;
                    };
                    match section {
                        Denoise => self.denoise_section(ui, strip, state, o),
                        Gate => self.gate_section(ui, strip, meters.gate, o),
                        Comp => self.comp_section(ui, strip, meters.comp, o),
                        Duck => {
                            self.duck_section(ui, strip, state, meters.duck, o);
                            ducking::ducks_others(ui, strip, state);
                        }
                        Upmix => channels::upmix_section(ui, strip, &state.mixer, o, actions),
                        _ => {}
                    }
                }
                (_, FxTarget::Bus(id)) => {
                    let Some(bus) = state.mixer.bus(id) else {
                        return;
                    };
                    match section {
                        Downmix => channels::downmix_section(ui, bus, &state.mixer, o, actions),
                        Limiter => self.limiter_section(ui, bus, meters.limiter, o),
                        Delay => self.delay_section(ui, bus, o),
                        _ => {}
                    }
                }
            }
            if focus == Some(section) {
                ui.scroll_to_rect(
                    Rect::from_min_size(top, vec2(SIDE_W, 42.0)),
                    Some(Align::TOP),
                );
            }
        }
        self.open = Some(open);
    }
}

/// The side sections of `target`, in chain order with its external effects
/// `at` their place. The equalizer has no section, being the middle of the
/// window, so external effects just before it sit with the compressor's.
fn side_order(target: FxTarget, at: InsertPoint) -> Vec<Section> {
    use Section::*;
    let (mut order, before) = match target {
        FxTarget::Strip(_) => (
            vec![Denoise, Gate, Comp, Fader, Duck, Upmix],
            match at.for_strip() {
                InsertPoint::BeforeDenoise => Denoise,
                InsertPoint::BeforeGate => Gate,
                InsertPoint::BeforeFader => Fader,
                InsertPoint::AfterFader => Duck,
                _ => Comp,
            },
        ),
        FxTarget::Bus(_) => (
            vec![Downmix, Fader, Limiter, Delay],
            match at.for_bus() {
                InsertPoint::BeforeLimiter => Limiter,
                InsertPoint::AfterLimiter => {
                    return vec![Downmix, Fader, Limiter, Insert, Delay];
                }
                _ => Fader,
            },
        ),
    };
    let k = order
        .iter()
        .position(|&s| s == before)
        .unwrap_or(order.len());
    order.insert(k, Insert);
    order
}

/// Which sections start unfolded: those of the effects that are on, so
/// what is at work is in view.
fn first_open(target: FxTarget, mixer: &MixerState) -> [bool; SECTIONS] {
    let mut open = [false; SECTIONS];
    match target {
        FxTarget::Strip(id) => {
            if let Some(s) = mixer.strip(id) {
                open[Section::Denoise as usize] = s.denoise.enabled;
                open[Section::Gate as usize] = s.gate.enabled;
                open[Section::Comp as usize] = s.compressor.enabled;
                open[Section::Duck as usize] = s.ducking.enabled;
                open[Section::Upmix as usize] = s.upmix != Upmix::Off;
                open[Section::Insert as usize] = s.insert.enabled;
            }
        }
        FxTarget::Bus(id) => {
            if let Some(b) = mixer.bus(id) {
                open[Section::Limiter as usize] = b.limiter.enabled;
                open[Section::Delay as usize] = b.delay_ms > 0.0;
                open[Section::Downmix as usize] = !b.downmix.is_default();
                open[Section::Insert as usize] = b.insert.enabled;
            }
        }
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_are_sent_once_and_forgotten_after_the_echo() {
        let mut slot = Some(Local::new(3.0_f32));
        assert_eq!(take_unsent(&mut slot), Some(3.0));
        assert_eq!(take_unsent(&mut slot), None);
        // Sent, but too recent to forget: the daemon may not have echoed it.
        expire(&mut slot);
        assert!(slot.is_some());
        if let Some(l) = slot.as_mut() {
            l.edited = Instant::now()
                .checked_sub(LOCAL_TTL * 2)
                .unwrap_or(l.edited);
        }
        expire(&mut slot);
        assert!(slot.is_none());
    }

    #[test]
    fn a_window_sends_one_request_per_control() {
        let mut w = FxWindow::new(FxTarget::Strip(4));
        w.gain = Some(Local::new(-6.0));
        w.gate = Some(Local::new(Gate::default()));
        let mut actions = Vec::new();
        w.flush(&mut actions, true);
        assert_eq!(actions.len(), 2);
        assert!(actions
            .iter()
            .all(|a| matches!(a, Request::SetStrip(p) if p.id == 4)));
        actions.clear();
        w.flush(&mut actions, true);
        assert!(actions.is_empty(), "nothing changed since");
    }

    #[test]
    fn external_effects_sit_down_the_side_where_they_are_in_the_chain() {
        use Section::*;
        let strip = FxTarget::Strip(1);
        assert_eq!(
            side_order(strip, InsertPoint::BeforeFader),
            [Denoise, Gate, Comp, Insert, Fader, Duck, Upmix]
        );
        assert_eq!(
            side_order(strip, InsertPoint::BeforeEq),
            [Denoise, Gate, Insert, Comp, Fader, Duck, Upmix]
        );
        assert_eq!(
            side_order(strip, InsertPoint::AfterFader),
            [Denoise, Gate, Comp, Fader, Insert, Duck, Upmix]
        );
        let bus = FxTarget::Bus(1);
        assert_eq!(
            side_order(bus, InsertPoint::BeforeEq),
            [Downmix, Insert, Fader, Limiter, Delay]
        );
        assert_eq!(
            side_order(bus, InsertPoint::AfterLimiter),
            [Downmix, Fader, Limiter, Insert, Delay]
        );
    }

    #[test]
    fn a_bus_delay_is_sent_once_and_starts_open_when_there_is_one() {
        let mut w = FxWindow::new(FxTarget::Bus(2));
        w.delay = Some(Local::new(191.0));
        let mut actions = Vec::new();
        w.flush(&mut actions, true);
        assert!(matches!(
            actions.as_slice(),
            [Request::SetBus(p)] if p.id == 2 && p.delay_ms == Some(191.0)
        ));
        actions.clear();
        w.flush(&mut actions, true);
        assert!(actions.is_empty(), "nothing changed since");

        let mut bus = Bus::new(2, "Speakers", BusKind::Hardware, ChannelLayout::Stereo);
        let mixer = |bus: &Bus| MixerState {
            strips: Vec::new(),
            buses: vec![bus.clone()],
        };
        assert!(!first_open(FxTarget::Bus(2), &mixer(&bus))[Section::Delay as usize]);
        bus.delay_ms = 191.0;
        assert!(first_open(FxTarget::Bus(2), &mixer(&bus))[Section::Delay as usize]);
    }

    #[test]
    fn sections_of_effects_that_are_on_start_open() {
        let mut strip = Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono);
        strip.gate.enabled = true;
        let mixer = MixerState {
            strips: vec![strip],
            buses: Vec::new(),
        };
        let open = first_open(FxTarget::Strip(1), &mixer);
        assert!(open[Section::Gate as usize]);
        assert!(!open[Section::Comp as usize]);
    }
}
