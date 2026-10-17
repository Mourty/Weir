//! The mixer itself: the strips, then the buses, in one row that scrolls
//! sideways, with every fader lined up. Strips and buses are put in order by
//! dragging them by their caption.

use super::App;
use crate::{theme, widgets};
use egui::{RichText, Ui};
use weir_protocol::*;

/// Sizes worked out once per frame so every strip and bus agrees on them and
/// all the faders line up.
pub(super) struct Metrics {
    /// Height of a whole strip or bus.
    pub strip_h: f32,
    pub fader_h: f32,
    /// Routing buttons in each column beside a strip's fader.
    pub per_col: usize,
    /// The dB scale and its gap, when there is one.
    scale_w: f32,
    /// The routing buttons beside a strip's fader.
    routes_w: f32,
    /// A dB scale beside each meter.
    pub scale: bool,
    /// The highest peak as a number above each meter.
    pub peak: bool,
}

impl Metrics {
    /// The sizes for a mixer area `avail_h` tall showing `mixer`, with a
    /// dB `scale` beside each meter and the `peak` number above it, as the
    /// preferences say.
    pub(super) fn compute(avail_h: f32, mixer: &MixerState, scale: bool, peak: bool) -> Self {
        let strip_h = (avail_h - 8.0).max(300.0);
        // Everything in a strip except the fader row itself.
        let chrome = 14.0            // caption
            + 24.0                   // name editor
            + theme::BAND_H          // source picker and pan
            + 26.0                   // gain readout
            + theme::FX_ROW_H        // effect buttons
            + 26.0                   // mute and solo row
            + 6.0 * 6.0              // spacing between those
            + 12.0 // frame margin
            + if peak { theme::PEAK_H + theme::PEAK_GAP } else { 0.0 };
        let fader_h = (strip_h - chrome).max(80.0);
        let scale_w = if scale {
            theme::METER_SCALE_W + theme::SCALE_GAP
        } else {
            0.0
        };
        let step = theme::ROUTE_BTN_H + theme::ROUTE_GAP;
        let fit = (((fader_h + theme::ROUTE_GAP) / step).floor() as usize).max(1);
        let n_buses = mixer.buses.len();
        let route_cols = n_buses.div_ceil(fit);
        // Spread the buttons evenly rather than filling each column before
        // starting the next, so eight buses in two columns read 4 and 4
        // instead of 7 and 1.
        let per_col = if route_cols > 0 {
            n_buses.div_ceil(route_cols)
        } else {
            fit
        };
        let routes_w = routes_width(route_cols);
        Self {
            strip_h,
            fader_h,
            per_col,
            scale_w,
            routes_w,
            scale,
            peak,
        }
    }

    /// How wide a strip with `channels` channels is. Each strip is as wide
    /// as its own meter needs, rather than all as wide as the widest, so
    /// the scale, meter and fader sit the same distance apart on a mono
    /// strip as on a 7.1 one.
    pub(super) fn strip_w(&self, channels: usize) -> f32 {
        (self.strip_row_w(channels) + FRAME_MARGINS).max(theme::STRIP_MIN_WIDTH)
    }

    /// How wide a bus with `channels` channels is, likewise.
    pub(super) fn bus_w(&self, channels: usize) -> f32 {
        (self.bus_row_w(channels) + FRAME_MARGINS).max(theme::BUS_MIN_WIDTH)
    }

    /// The width of a strip's fader row: scale, meter, fader and routing
    /// buttons, with the gaps between them.
    pub(super) fn strip_row_w(&self, channels: usize) -> f32 {
        self.scale_w
            + widgets::meter_width(channels)
            + theme::METER_GAP
            + theme::FADER_W
            + theme::METER_GAP
            + self.routes_w
    }

    /// The width of a bus's fader row: scale, ceiling handle, meter and
    /// fader.
    pub(super) fn bus_row_w(&self, channels: usize) -> f32 {
        self.scale_w
            + theme::CEILING_W
            + theme::CEILING_GAP
            + widgets::meter_width(channels)
            + theme::METER_GAP
            + theme::FADER_W
    }
}

/// The frame's margins either side of a strip or bus.
const FRAME_MARGINS: f32 = 12.0;

/// The width of `cols` columns of routing buttons.
fn routes_width(cols: usize) -> f32 {
    cols as f32 * theme::ROUTE_BTN_W + cols.saturating_sub(1) as f32 * theme::ROUTE_GAP
}

impl App {
    pub(super) fn mixer_view(&mut self, ui: &mut Ui, state: &FullState) {
        let lay = Metrics::compute(
            ui.available_height(),
            &state.mixer,
            self.prefs.meter_scale,
            self.prefs.meter_peak,
        );
        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.set_min_height(lay.strip_h);
                    let strips = &state.mixer.strips;
                    if strips.is_empty() {
                        ui.label(
                            RichText::new("No strips. Use + Strip to add one.")
                                .color(theme::p().text_dim),
                        );
                    }
                    let order: Vec<StripOrBus> =
                        strips.iter().map(|s| StripOrBus::Strip(s.id)).collect();
                    for (i, s) in strips.iter().enumerate() {
                        let resp = self.strip_view(ui, state, s, &lay);
                        self.drop_target(ui, &resp, i, &order);
                    }
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                    let buses = &state.mixer.buses;
                    if buses.is_empty() {
                        ui.label(
                            RichText::new("No buses. Use + Bus to add one.")
                                .color(theme::p().text_dim),
                        );
                    }
                    let order: Vec<StripOrBus> =
                        buses.iter().map(|b| StripOrBus::Bus(b.id)).collect();
                    for (i, b) in buses.iter().enumerate() {
                        let resp = self.bus_view(ui, state, b, &lay);
                        self.drop_target(ui, &resp, i, &order);
                    }
                });
            });
    }

    /// While a strip or bus from `order` is dragged over the one at `index`,
    /// show where it would land, and move it there when it is dropped.
    /// Strips only go among strips and buses among buses.
    fn drop_target(&mut self, ui: &Ui, resp: &egui::Response, index: usize, order: &[StripOrBus]) {
        let Some(dragged) = resp.dnd_hover_payload::<StripOrBus>() else {
            return;
        };
        let Some(from) = order.iter().position(|x| *x == *dragged) else {
            return;
        };
        let Some(pointer) = ui.ctx().pointer_interact_pos() else {
            return;
        };
        let after = pointer.x > resp.rect.center().x;
        let x = if after {
            resp.rect.right() + 2.0
        } else {
            resp.rect.left() - 2.0
        };
        ui.painter().vline(
            x,
            resp.rect.y_range(),
            egui::Stroke::new(3.0_f32, theme::p().accent),
        );
        if resp.dnd_release_payload::<StripOrBus>().is_none() {
            return;
        }
        if let Some(to) = crate::widgets::drop_index(from, index, after) {
            self.actions.push(move_request(*dragged, to));
        }
    }
}

/// The request moving a strip or bus to `index`.
pub(super) fn move_request(item: StripOrBus, index: usize) -> Request {
    match item {
        StripOrBus::Strip(id) => Request::MoveStrip(MoveParams { id, index }),
        StripOrBus::Bus(id) => Request::MoveBus(MoveParams { id, index }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faders_get_what_the_rest_of_a_strip_leaves() {
        let none = MixerState::default();
        let tall = Metrics::compute(900.0, &none, false, false);
        let short = Metrics::compute(500.0, &none, false, false);
        assert!(tall.fader_h > short.fader_h);
        assert_eq!(tall.routes_w, 0.0, "no buses, no buttons");
        let tiny = Metrics::compute(10.0, &none, false, false);
        assert!(tiny.fader_h >= 80.0, "a fader never gets too short to use");
        // The peak number takes its height from the fader.
        let peaked = Metrics::compute(900.0, &none, true, true);
        assert_eq!(
            peaked.fader_h,
            tall.fader_h - theme::PEAK_H - theme::PEAK_GAP
        );
    }

    #[test]
    fn each_strip_is_as_wide_as_its_own_meter_needs() {
        let mixer = MixerState {
            buses: (1..=3)
                .map(|id| Bus::new(id, "Bus", BusKind::Virtual, ChannelLayout::Stereo))
                .collect(),
            ..Default::default()
        };
        let lay = Metrics::compute(900.0, &mixer, true, true);
        // A row with more channels is wider by just its meter: the gaps
        // either side of it stay the same.
        let (stereo, surround) = (lay.strip_w(2), lay.strip_w(6));
        assert_eq!(
            surround - stereo,
            widgets::meter_width(6) - widgets::meter_width(2)
        );
        assert_eq!(surround, lay.strip_row_w(6) + FRAME_MARGINS);
        assert_eq!(
            lay.bus_w(8) - lay.bus_w(6),
            widgets::meter_width(8) - widgets::meter_width(6)
        );
        // Narrow ones keep room for the name and buttons.
        let bare = Metrics::compute(900.0, &mixer, false, false);
        assert_eq!(bare.strip_w(1), theme::STRIP_MIN_WIDTH);
        assert_eq!(bare.bus_w(2), theme::BUS_MIN_WIDTH);
    }
}
