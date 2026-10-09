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
    /// Columns of routing buttons.
    pub route_cols: usize,
    pub strip_w: f32,
    pub bus_w: f32,
}

impl Metrics {
    pub(super) fn compute(avail_h: f32, mixer: &MixerState) -> Self {
        let strip_h = (avail_h - 8.0).max(300.0);
        // Everything in a strip except the fader row itself.
        let chrome = 14.0            // caption
            + 24.0                   // name editor
            + theme::BAND_H          // source picker and pan
            + 26.0                   // gain readout
            + theme::FX_ROW_H        // effect buttons
            + 26.0                   // mute and solo row
            + 6.0 * 6.0              // spacing between those
            + 12.0; // frame margin
        let fader_h = (strip_h - chrome).max(80.0);
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
        let widest = |counts: &mut dyn Iterator<Item = usize>| counts.max().unwrap_or(2);
        let strip_ch = widest(&mut mixer.strips.iter().map(|s| s.layout.channel_count()));
        let bus_ch = widest(&mut mixer.buses.iter().map(|b| b.layout.channel_count()));
        let routes_w = route_cols as f32 * (theme::ROUTE_BTN_W + theme::ROUTE_GAP);
        let strip_w = (widgets::meter_width(strip_ch) + 8.0 + theme::FADER_W + routes_w + 12.0)
            .max(theme::STRIP_MIN_WIDTH);
        let bus_w =
            (theme::CEILING_W + 4.0 + widgets::meter_width(bus_ch) + 8.0 + theme::FADER_W + 12.0)
                .max(theme::BUS_MIN_WIDTH);
        Self {
            strip_h,
            fader_h,
            per_col,
            route_cols,
            strip_w,
            bus_w,
        }
    }
}

impl App {
    pub(super) fn mixer_view(&mut self, ui: &mut Ui, state: &FullState) {
        let lay = Metrics::compute(ui.available_height(), &state.mixer);
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
        let tall = Metrics::compute(900.0, &MixerState::default());
        let short = Metrics::compute(500.0, &MixerState::default());
        assert!(tall.fader_h > short.fader_h);
        assert_eq!(tall.route_cols, 0, "no buses, no buttons");
        let tiny = Metrics::compute(10.0, &MixerState::default());
        assert!(tiny.fader_h >= 80.0, "a fader never gets too short to use");
    }
}
