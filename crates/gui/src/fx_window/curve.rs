//! The equalizer's graph: the frequency response of every band and of all
//! of them together, a handle per band to drag, and behind it all a live
//! spectrum of the audio going in and coming out, so you can see what a
//! change does as well as hear it.
//!
//! Frequency runs along the bottom on a log scale from 20 Hz to 20 kHz.
//! The curve's vertical scale is a change in level, ± the range picked
//! above it; the spectrum has a scale of its own, a level, on the right.

use super::eq::{band_color, fmt_freq, round_freq};
use super::FxWindow;
use crate::prefs::SpectrumView;
use crate::theme;
use egui::{pos2, vec2, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui};
use weir_protocol::*;

/// The frequencies at the left and right edges.
const F_LO: f32 = 20.0;
const F_HI: f32 = 20_000.0;
/// The analyzer's own vertical scale, independent of the curve's range.
const SPECTRUM_TOP_DB: f32 = 0.0;
const SPECTRUM_BOTTOM_DB: f32 = -96.0;
/// Input in a quiet neutral gray, output in a clear blue, so where they
/// differ is easy to see. Mid-bright, for either theme.
pub(super) const SPECTRUM_IN: Color32 = Color32::from_rgb(125, 128, 136);
pub(super) const SPECTRUM_OUT: Color32 = Color32::from_rgb(80, 150, 245);

/// Where things are on the graph: the conversions between frequency and
/// gain and the screen.
struct Scale {
    plot: Rect,
    /// The curve's range, ± this many dB.
    range: f32,
}

impl Scale {
    /// The x of a frequency.
    fn x(&self, f: f32) -> f32 {
        self.plot.left() + self.plot.width() * (f / F_LO).ln() / (F_HI / F_LO).ln()
    }

    /// The frequency at an x.
    fn freq(&self, x: f32) -> f32 {
        let t = ((x - self.plot.left()) / self.plot.width()).clamp(0.0, 1.0);
        F_LO * (F_HI / F_LO).powf(t)
    }

    /// The y of a gain, kept inside the graph.
    fn y(&self, db: f32) -> f32 {
        self.y_unclamped(db.clamp(-self.range, self.range))
    }

    /// The y of a gain on a curve, which may run off the top or bottom and
    /// be clipped there, rather than flatten along the edge as if that were
    /// its level.
    fn y_unclamped(&self, db: f32) -> f32 {
        self.plot.center().y - db.clamp(-200.0, 200.0) / self.range * self.plot.height() / 2.0
    }

    /// The gain at a y, within what a band can do.
    fn gain(&self, y: f32) -> f32 {
        ((self.plot.center().y - y) / (self.plot.height() / 2.0) * self.range)
            .clamp(EQ_GAIN_MIN_DB, EQ_GAIN_MAX_DB)
    }

    /// Where a band's handle sits: at its gain, or on the 0 dB line for
    /// kinds without one.
    fn handle(&self, b: &EqBand) -> Pos2 {
        let db = if b.kind.uses_gain() { b.gain_db } else { 0.0 };
        pos2(self.x(b.freq_hz), self.y(db))
    }
}

impl FxWindow {
    /// The frequency response, with a handle per band to drag.
    pub(super) fn curve(
        &mut self,
        ui: &mut Ui,
        bands: &mut Vec<EqBand>,
        enabled: bool,
        rate: f32,
        h: f32,
    ) {
        let w = ui.available_width();
        let (rect, bg) = ui.allocate_exact_size(vec2(w, h), Sense::click_and_drag());
        // The right margin holds the analyzer's scale.
        let scale = Scale {
            plot: Rect::from_min_max(
                pos2(rect.left() + 34.0, rect.top() + 6.0),
                pos2(rect.right() - 30.0, rect.bottom() - 18.0),
            ),
            range: self.range_db,
        };
        let hovered_band = self.band_handles(ui, bands, &scale);
        self.curve_clicks(ui, &bg, bands, hovered_band, &scale);

        let painter = ui.painter_at(rect);
        paint_grid(&painter, &scale);
        let clip = painter.with_clip_rect(scale.plot);
        if let Some(sp) = self
            .spectrum
            .as_ref()
            .filter(|_| self.view != SpectrumView::Off)
        {
            self.paint_spectrum(&painter, &clip, &scale, sp);
        }
        self.paint_curves(&clip, &scale, bands, enabled, rate);
        self.paint_handles(&painter, &scale, bands, hovered_band);

        // Readout of where the pointer is, and a hint when there is nothing.
        let plot = scale.plot;
        if let Some(pos) = bg.hover_pos().filter(|p| plot.contains(*p)) {
            painter.text(
                pos2(plot.right() - 6.0, plot.top() + 4.0),
                Align2::RIGHT_TOP,
                format!(
                    "{}  {:+.1} dB",
                    fmt_freq(scale.freq(pos.x)),
                    scale.gain(pos.y)
                ),
                FontId::proportional(11.0),
                theme::p().text_dim,
            );
        }
        if bands.is_empty() {
            painter.text(
                plot.center() - vec2(0.0, 24.0),
                Align2::CENTER_CENTER,
                "Double-click the curve to add a band, or pick a preset above.",
                FontId::proportional(13.0),
                theme::p().text_dim,
            );
        }
    }

    /// The handles: drag to move a band, double-click to flatten it,
    /// right-click for its kind and to remove it. They are registered after
    /// the background, which puts them on top. Returns the band under the
    /// pointer.
    fn band_handles(
        &mut self,
        ui: &mut Ui,
        bands: &mut Vec<EqBand>,
        scale: &Scale,
    ) -> Option<usize> {
        let plot = scale.plot;
        let mut remove: Option<usize> = None;
        let mut hovered_band: Option<usize> = None;
        for (i, band) in bands.iter_mut().enumerate() {
            let hit = Rect::from_center_size(scale.handle(band), vec2(18.0, 18.0));
            let r = ui.interact(hit, ui.id().with(("band", i)), Sense::click_and_drag());
            if r.hovered() || r.dragged() {
                hovered_band = Some(i);
            }
            if r.drag_started() || r.clicked() {
                self.selected = Some(i);
            }
            if r.drag_started() {
                self.dragging = Some(i);
            }
            if r.dragged() {
                if let Some(pos) = r.interact_pointer_pos() {
                    band.freq_hz = round_freq(scale.freq(pos.x.clamp(plot.left(), plot.right())));
                    if band.kind.uses_gain() {
                        band.gain_db = (scale.gain(pos.y) * 10.0).round() / 10.0;
                    }
                }
            }
            if r.drag_stopped() {
                self.dragging = None;
            }
            if r.double_clicked() && band.kind.uses_gain() {
                band.gain_db = 0.0;
            }
            let b = *band;
            let gain = if b.kind.uses_gain() {
                format!(" {:+.1} dB", b.gain_db)
            } else {
                String::new()
            };
            let r = r.on_hover_text(format!(
                "Band {}: {} {}{gain} Q {:.2}\nDrag to move · scroll to change the width · \
                 double-click for 0 dB · right-click for more",
                i + 1,
                b.kind.label(),
                fmt_freq(b.freq_hz),
                b.q
            ));
            r.context_menu(|ui| {
                ui.label(RichText::new(format!("Band {}", i + 1)).strong());
                for k in EqBandKind::ALL {
                    if ui.selectable_label(band.kind == k, k.label()).clicked() {
                        band.kind = k;
                        ui.close();
                    }
                }
                ui.separator();
                let label = if band.enabled {
                    "Switch off"
                } else {
                    "Switch on"
                };
                if ui.button(label).clicked() {
                    band.enabled = !band.enabled;
                    ui.close();
                }
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                    ui.close();
                }
            });
        }
        if let Some(i) = remove {
            bands.remove(i);
            self.selected = None;
            self.dragging = None;
            hovered_band = None;
        }
        hovered_band
    }

    /// What the background does: scrolling widens or narrows a band,
    /// double-clicking adds one, and a click elsewhere deselects.
    fn curve_clicks(
        &mut self,
        ui: &mut Ui,
        bg: &egui::Response,
        bands: &mut Vec<EqBand>,
        hovered_band: Option<usize>,
        scale: &Scale,
    ) {
        // Scrolling changes the width of the band under the pointer, or of
        // the selected one.
        let scroll = if bg.hovered() || hovered_band.is_some() {
            ui.input(|i| i.raw_scroll_delta.y)
        } else {
            0.0
        };
        if scroll != 0.0 {
            if let Some(b) = hovered_band
                .or(self.selected)
                .and_then(|i| bands.get_mut(i))
            {
                let step = if scroll > 0.0 { 1.12 } else { 1.0 / 1.12 };
                b.q = (b.q * step).clamp(EQ_Q_MIN, EQ_Q_MAX);
            }
        }
        if bg.double_clicked() && hovered_band.is_none() && bands.len() < EQ_MAX_BANDS {
            if let Some(pos) = bg
                .interact_pointer_pos()
                .filter(|p| scale.plot.contains(*p))
            {
                bands.push(EqBand::new(
                    EqBandKind::Peak,
                    round_freq(scale.freq(pos.x)),
                    (scale.gain(pos.y) * 10.0).round() / 10.0,
                    1.0,
                ));
                self.selected = Some(bands.len() - 1);
            }
        } else if bg.clicked() && hovered_band.is_none() {
            self.selected = None;
        }
    }

    /// Each band's own contribution, faint, with the selected one filled;
    /// then all of them together.
    fn paint_curves(
        &self,
        clip: &egui::Painter,
        scale: &Scale,
        bands: &[EqBand],
        enabled: bool,
        rate: f32,
    ) {
        let plot = scale.plot;
        // A point every two pixels.
        let n = (plot.width() / 2.0).max(2.0) as usize;
        let xs: Vec<f32> = (0..=n)
            .map(|k| plot.left() + plot.width() * k as f32 / n as f32)
            .collect();
        let line = |db: &dyn Fn(f32) -> f32| -> Vec<Pos2> {
            xs.iter()
                .map(|&x| pos2(x, scale.y_unclamped(db(scale.freq(x)))))
                .collect()
        };
        for (i, b) in bands.iter().enumerate().filter(|(_, b)| b.enabled) {
            let c = b.coefs(rate);
            let color = band_color(i);
            let pts = line(&|f| c.response_db(f, rate));
            if self.selected == Some(i) {
                let zero = scale.y(0.0);
                for p in &pts {
                    let y = p.y.clamp(plot.top(), plot.bottom());
                    clip.vline(
                        p.x,
                        zero.min(y)..=zero.max(y),
                        Stroke::new(2.0_f32, color.gamma_multiply(0.18)),
                    );
                }
            }
            clip.add(egui::Shape::line(
                pts,
                Stroke::new(1.0_f32, color.gamma_multiply(0.55)),
            ));
        }
        let total = Equalizer {
            enabled: true,
            bands: bands.to_vec(),
        };
        let curve_color = if enabled {
            theme::p().accent
        } else {
            theme::p().text_dim
        };
        clip.add(egui::Shape::line(
            line(&|f| total.response_db(f, rate)),
            Stroke::new(2.5_f32, curve_color),
        ));
    }

    /// A numbered handle per band, ringed when selected or under the
    /// pointer.
    fn paint_handles(
        &self,
        painter: &egui::Painter,
        scale: &Scale,
        bands: &[EqBand],
        hovered_band: Option<usize>,
    ) {
        let plot = scale.plot;
        for (i, b) in bands.iter().enumerate() {
            let p = scale.handle(b);
            let p = pos2(p.x, p.y.clamp(plot.top() + 7.0, plot.bottom() - 7.0));
            let color = if b.enabled {
                band_color(i)
            } else {
                theme::p().button_off
            };
            let selected = self.selected == Some(i);
            let radius = if selected { 9.0 } else { 7.5 };
            let outline = if selected || hovered_band == Some(i) {
                Stroke::new(2.0_f32, theme::p().marker)
            } else {
                Stroke::new(1.0_f32, Color32::BLACK)
            };
            painter.circle(p, radius, color, outline);
            painter.text(
                p,
                Align2::CENTER_CENTER,
                format!("{}", i + 1),
                FontId::proportional(10.0),
                Color32::BLACK,
            );
        }
    }

    /// The live spectrum, behind everything else on the curve.
    fn paint_spectrum(
        &self,
        painter: &egui::Painter,
        clip: &egui::Painter,
        scale: &Scale,
        sp: &Spectrum,
    ) {
        let plot = scale.plot;
        let sy = |db: f32| {
            let t = (db - SPECTRUM_BOTTOM_DB) / (SPECTRUM_TOP_DB - SPECTRUM_BOTTOM_DB);
            plot.bottom() - t.clamp(0.0, 1.0) * plot.height()
        };
        let points = |levels: &[f32]| -> Vec<Pos2> {
            levels
                .iter()
                .enumerate()
                .map(|(i, &db)| pos2(scale.x(spectrum_freq(i)), sy(db)))
                .collect()
        };
        if self.view == SpectrumView::InputAndOutput && sp.input_db.len() == SPECTRUM_POINTS {
            fill_under(
                clip,
                &points(&sp.input_db),
                plot.bottom(),
                SPECTRUM_IN,
                0.28,
            );
        }
        if sp.output_db.len() == SPECTRUM_POINTS {
            let out = points(&sp.output_db);
            fill_under(clip, &out, plot.bottom(), SPECTRUM_OUT, 0.30);
            clip.add(egui::Shape::line(
                out,
                Stroke::new(1.0_f32, SPECTRUM_OUT.gamma_multiply(0.9)),
            ));
        }
        // The analyzer's scale, on the right. It is not the curve's scale:
        // the curve is a change in level, the spectrum a level.
        let font = FontId::proportional(9.5);
        let mut db = SPECTRUM_TOP_DB - 12.0;
        while db > SPECTRUM_BOTTOM_DB {
            painter.text(
                pos2(plot.right() + 4.0, sy(db)),
                Align2::LEFT_CENTER,
                format!("{db:.0}"),
                font.clone(),
                SPECTRUM_OUT.gamma_multiply(0.7),
            );
            db -= 12.0;
        }
    }
}

/// The background: grid lines at every frequency and gain step, labeled
/// along the bottom and the left.
fn paint_grid(painter: &egui::Painter, scale: &Scale) {
    let plot = scale.plot;
    painter.rect_filled(plot, 4, theme::p().track);
    let grid = Stroke::new(1.0_f32, theme::p().grid);
    let grid_major = Stroke::new(1.0_f32, theme::p().grid_major);
    let label_font = FontId::proportional(10.0);
    for f in minor_lines() {
        painter.vline(scale.x(f), plot.y_range(), grid);
    }
    for (f, text) in [
        (20.0, "20"),
        (50.0, "50"),
        (100.0, "100"),
        (200.0, "200"),
        (500.0, "500"),
        (1000.0, "1k"),
        (2000.0, "2k"),
        (5000.0, "5k"),
        (10000.0, "10k"),
        (20000.0, "20k"),
    ] {
        let x = scale.x(f);
        painter.vline(x, plot.y_range(), grid_major);
        painter.text(
            pos2(x, plot.bottom() + 3.0),
            Align2::CENTER_TOP,
            text,
            label_font.clone(),
            theme::p().text_dim,
        );
    }
    let range = scale.range;
    let step = if range <= 6.0 {
        2.0
    } else if range <= 12.0 {
        3.0
    } else {
        6.0
    };
    let mut db = -range;
    while db <= range + 0.01 {
        let y = scale.y(db);
        let stroke = if db == 0.0 {
            Stroke::new(1.0_f32, theme::p().grid_zero)
        } else {
            grid
        };
        painter.hline(plot.x_range(), y, stroke);
        painter.text(
            pos2(plot.left() - 4.0, y),
            Align2::RIGHT_CENTER,
            format!("{db:+.0}"),
            label_font.clone(),
            theme::p().text_dim,
        );
        db += step;
    }
}

/// Fill the area under a line down to `bottom`, fading towards the bottom.
fn fill_under(painter: &egui::Painter, pts: &[Pos2], bottom: f32, color: Color32, alpha: f32) {
    if pts.len() < 2 {
        return;
    }
    let top = color.gamma_multiply(alpha);
    let base = color.gamma_multiply(alpha * 0.25);
    let mut mesh = egui::Mesh::default();
    for (k, p) in pts.iter().enumerate() {
        mesh.colored_vertex(pos2(p.x, bottom), base);
        mesh.colored_vertex(*p, top);
        if k > 0 {
            let i = (2 * k) as u32;
            mesh.add_triangle(i - 2, i - 1, i);
            mesh.add_triangle(i - 1, i, i + 1);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Frequencies of the unlabeled grid lines: 30, 40 ... 90, 300, 400 and so
/// on.
fn minor_lines() -> Vec<f32> {
    let mut out = Vec::new();
    for decade in [10.0f32, 100.0, 1000.0, 10_000.0] {
        for m in 2..10 {
            let f = decade * m as f32;
            if (F_LO..=F_HI).contains(&f) {
                out.push(f);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_maps_back_and_forth() {
        let scale = Scale {
            plot: Rect::from_min_max(pos2(0.0, 0.0), pos2(300.0, 200.0)),
            range: 12.0,
        };
        assert_eq!(scale.x(F_LO), 0.0);
        assert!((scale.x(F_HI) - 300.0).abs() < 1e-3);
        assert!((scale.freq(scale.x(1000.0)) - 1000.0).abs() < 0.1);
        assert_eq!(scale.y(0.0), 100.0);
        assert_eq!(scale.y(48.0), 0.0, "gains past the range stay on the graph");
        assert!(scale.y_unclamped(24.0) < 0.0, "curves run off it");
        assert!((scale.gain(scale.y(6.0)) - 6.0).abs() < 1e-4);
    }
}
