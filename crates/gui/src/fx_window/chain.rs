//! The signal chain along the top of the settings window: every stage the
//! sound goes through, in order, lit where it is on, with external effects
//! in the gap they sit in, to be dragged to another.

use super::{external, FxTarget, FxWindow, Section};
use crate::effects::Status;
use crate::theme;
use egui::{vec2, Align2, Color32, FontId, Rect, RichText, Sense, Ui};
use weir_protocol::{FullState, InsertPoint, Request, Upmix};

/// One stage of the chain.
struct Link {
    label: &'static str,
    on: bool,
    color: Color32,
    /// The section with its settings; the equalizer has none, being the
    /// middle of the window.
    section: Option<Section>,
    what: String,
    /// Always doing something, so it has no on or off to report.
    always: bool,
}

impl Link {
    /// A stage that can be switched on and off.
    fn switch(
        label: &'static str,
        on: bool,
        color: Color32,
        section: Option<Section>,
        what: impl Into<String>,
    ) -> Self {
        Self {
            label,
            on,
            color,
            section,
            what: what.into(),
            always: false,
        }
    }

    /// A stage that is always at work.
    fn always(label: &'static str, section: Section, what: impl Into<String>) -> Self {
        Self {
            label,
            on: true,
            color: theme::p().fader_handle,
            section: Some(section),
            what: what.into(),
            always: true,
        }
    }
}

/// What is dragged when external effects are moved along the chain.
struct MoveEffects;

/// Width of the cell between two stages, which shows "›", and while
/// external effects are being dragged is where they can be dropped.
const GAP_W: f32 = 14.0;
/// Height of a stage.
const LINK_H: f32 = 20.0;

/// Where external effects can go in the chain of `target`: before which
/// stage of [`FxWindow::links`] (their number for the very end), and the
/// place that is.
fn gaps(target: FxTarget) -> &'static [(usize, InsertPoint)] {
    use InsertPoint::*;
    match target {
        FxTarget::Strip(_) => &[
            (0, BeforeDenoise),
            (1, BeforeGate),
            (2, BeforeEq),
            (3, BeforeCompressor),
            (4, BeforeFader),
            (5, AfterFader),
        ],
        FxTarget::Bus(_) => &[
            (1, BeforeEq),
            (2, BeforeFader),
            (3, BeforeLimiter),
            (4, AfterLimiter),
        ],
    }
}

impl FxWindow {
    /// The signal chain, lit where it is on. Clicking a link opens its
    /// section down the side and scrolls to it. External effects have a
    /// stage of their own in the gap they sit in, which can be dragged to
    /// another.
    pub(super) fn chain_row(&mut self, ui: &mut Ui, state: &FullState, actions: &mut Vec<Request>) {
        let Some(links) = self.links(state) else {
            return;
        };
        let Some((name, insert)) = external::settings(state, self.target) else {
            return;
        };
        let status = Status::of(state, self.target);
        let gaps = gaps(self.target);
        let here = gaps
            .iter()
            .find(|(_, at)| *at == insert.position)
            .map(|(g, _)| *g);
        let dragging = egui::DragAndDrop::has_payload_of_type::<MoveEffects>(ui.ctx());
        let mut dropped = None;
        let mut focus = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(
                RichText::new("SIGNAL CHAIN")
                    .size(10.0)
                    .color(theme::p().text_dim),
            );
            ui.add_space(4.0);
            let n = links.len();
            for g in 0..=n {
                let place = gaps.iter().find(|(k, _)| *k == g).map(|(_, at)| *at);
                // A gap external effects can be dropped into, while they
                // are being dragged from another.
                let target = place.filter(|_| dragging && here != Some(g));
                if here == Some(g) {
                    gap_cell(ui, g > 0, None);
                    if ext_link(ui, insert.enabled, status, name).clicked() {
                        focus = Some(Section::Insert);
                    }
                    gap_cell(ui, g < n, None);
                } else if let Some(at) = gap_cell(ui, g > 0 && g < n, target) {
                    dropped = Some(at);
                }
                let Some(link) = links.get(g) else {
                    continue;
                };
                let w = 18.0 + 7.5 * link.label.len() as f32;
                let state = if link.always {
                    ""
                } else if link.on {
                    ": on"
                } else {
                    ": off"
                };
                let hint = if link.section.is_some() {
                    "\nClick to open its settings"
                } else {
                    ""
                };
                let r =
                    crate::widgets::toggle(ui, link.label, link.on, link.color, vec2(w, LINK_H))
                        .on_hover_text(format!("{}{state}{hint}", link.what));
                if r.clicked() {
                    focus = focus.or(link.section);
                }
            }
        });
        if let Some(section) = focus {
            self.focus = Some(section);
        }
        if let Some(at) = dropped {
            self.move_effects(at, actions);
        }
    }

    /// The stages of this window's strip or bus, with what is being edited
    /// here taking the place of the daemon's copy.
    fn links(&self, state: &FullState) -> Option<Vec<Link>> {
        let p = theme::p();
        Some(match self.target {
            FxTarget::Strip(id) => {
                let s = state.mixer.strip(id)?;
                let d = self.denoise.as_ref().map_or(s.denoise, |l| l.value);
                let g = self.gate.as_ref().map_or(s.gate, |l| l.value);
                let c = self.comp.as_ref().map_or(s.compressor, |l| l.value);
                let ducking = self
                    .duck
                    .as_ref()
                    .map_or(s.ducking.enabled, |l| l.value.enabled);
                let upmix = s.upmix != Upmix::Off || s.subwoofer;
                vec![
                    Link::switch(
                        "NS",
                        d.enabled,
                        p.ns_on,
                        Some(Section::Denoise),
                        "Noise suppression",
                    ),
                    Link::switch(
                        "Gate",
                        g.enabled,
                        p.gate_on,
                        Some(Section::Gate),
                        "Noise gate",
                    ),
                    Link::switch("EQ", s.eq.enabled, p.eq_on, None, "Equalizer"),
                    Link::switch(
                        "Comp",
                        c.enabled,
                        p.comp_on,
                        Some(Section::Comp),
                        "Compressor",
                    ),
                    Link::always("Fader", Section::Fader, "The fader, pan and mute"),
                    Link::switch(
                        "Duck",
                        ducking,
                        p.duck,
                        Some(Section::Duck),
                        "Ducking, in the mixes it applies to",
                    ),
                    Link::switch(
                        "Upmix",
                        upmix,
                        p.accent,
                        Some(Section::Upmix),
                        format!(
                            "Upmix, for buses with more speakers than it has channels: {}",
                            s.upmix.label().to_lowercase()
                        ),
                    ),
                ]
            }
            FxTarget::Bus(id) => {
                let b = state.mixer.bus(id)?;
                let limiting = self.limiter.as_ref().map_or(b.limiter, |l| l.value);
                vec![
                    Link::always(
                        "Downmix",
                        Section::Downmix,
                        format!(
                            "Downmix, for strips with channels this bus has no speaker for: {}",
                            b.downmix.method.label().to_lowercase()
                        ),
                    ),
                    Link::switch("EQ", b.eq.enabled, p.eq_on, None, "Equalizer"),
                    Link::always("Fader", Section::Fader, "The fader, mute and mono"),
                    Link::switch(
                        "Limiter",
                        limiting.enabled,
                        p.limit,
                        Some(Section::Limiter),
                        "Safety limiter",
                    ),
                ]
            }
        })
    }
}

/// The cell for one gap between stages: "›" when `arrow`, and, while
/// external effects are being dragged, a place to drop them, `drop_at`.
/// Returns the place when they are dropped on it.
fn gap_cell(ui: &mut Ui, arrow: bool, drop_at: Option<InsertPoint>) -> Option<InsertPoint> {
    let (rect, resp) = ui.allocate_exact_size(vec2(GAP_W, LINK_H), Sense::hover());
    let p = theme::p();
    if drop_at.is_some() {
        let over = resp.dnd_hover_payload::<MoveEffects>().is_some();
        let slot = Rect::from_center_size(rect.center(), vec2(GAP_W - 4.0, LINK_H));
        if over {
            ui.painter().rect_filled(slot, 3, p.ext_on);
        } else {
            ui.painter().rect_stroke(
                slot,
                3,
                egui::Stroke::new(1.5_f32, p.ext_on),
                egui::StrokeKind::Inside,
            );
        }
    } else if arrow {
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "›",
            FontId::proportional(14.0),
            p.text_dim,
        );
    }
    drop_at.filter(|_| resp.dnd_release_payload::<MoveEffects>().is_some())
}

/// The external effects stage: lit in their color while they are on, and
/// dragged to move them. Clicked, it should open their section.
fn ext_link(ui: &mut Ui, on: bool, status: Option<Status>, name: &str) -> egui::Response {
    let label = "Ext FX";
    let size = vec2(18.0 + 7.5 * label.len() as f32, LINK_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    resp.dnd_set_drag_payload(MoveEffects);
    let p = theme::p();
    let (fill, text) = match status {
        Some(s) => (s.color(), p.on_text),
        None if resp.hovered() => (p.hover_fill, p.text),
        None => (p.button_off, p.text),
    };
    let paint = |painter: &egui::Painter, rect: Rect| {
        painter.rect_filled(rect, 4, fill);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(11.0),
            text,
        );
    };
    if resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        // Where it was stays dimmed, and a copy follows the pointer, beside
        // it rather than under it, so the gaps can tell they are pointed at.
        paint(ui.painter(), rect);
        ui.painter().rect_filled(rect, 4, p.bg.gamma_multiply(0.6));
        if let Some(pos) = ui.ctx().pointer_interact_pos() {
            let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("ext_fx_drag"));
            paint(
                &ui.ctx().layer_painter(layer),
                Rect::from_min_size(pos + vec2(10.0, 10.0), size),
            );
        }
    } else {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        paint(ui.painter(), rect);
    }
    let what = match status {
        Some(s) => s.explain(name),
        None if on => "External effects: on".to_string(),
        None => "External effects: off".to_string(),
    };
    resp.on_hover_text(format!(
        "{what}\nDrag along the chain to move them, click to open their settings"
    ))
}
