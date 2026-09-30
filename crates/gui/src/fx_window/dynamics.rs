//! The sections for effects that follow the level: noise suppression, the
//! noise gate and the compressor on strips, the safety limiter on buses.
//! Each shows what it is doing right now as well as its settings.

use super::section::{note, section_header, HeaderClick};
use super::{CompView, FxWindow, GateView, Local, SIDE_W};
use crate::theme;
use egui::{pos2, vec2, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui};
use weir_protocol::*;

impl FxWindow {
    pub(super) fn denoise_section(
        &mut self,
        ui: &mut Ui,
        strip: &Strip,
        state: &FullState,
        open: &mut bool,
    ) {
        let mut d = self.denoise.as_ref().map_or(strip.denoise, |l| l.value);
        let before = d;
        let rate = state.engine.sample_rate;
        let summary = if !d.enabled {
            "off".to_string()
        } else if rate != 0 && rate != Denoise::SAMPLE_RATE {
            format!("not running at {rate} Hz")
        } else {
            format!("{:.0}% · adds 20 ms", d.amount * 100.0)
        };
        match section_header(
            ui,
            "Noise suppression",
            &summary,
            Some(d.enabled),
            theme::p().ns_on,
            *open,
        ) {
            HeaderClick::Switch => d.enabled = !d.enabled,
            HeaderClick::Fold => *open = !*open,
            HeaderClick::None => {}
        }
        if *open {
            let mut pct = d.amount * 100.0;
            ui.add_enabled_ui(d.enabled, |ui| {
                if ui
                    .add(
                        egui::Slider::new(&mut pct, 0.0..=100.0)
                            .text("amount")
                            .suffix(" %")
                            .fixed_decimals(0),
                    )
                    .on_hover_text(
                        "How much of the suppressed signal to use. Lower values mix some of \
                         the original back in, which sounds more natural but leaves more noise.",
                    )
                    .changed()
                {
                    d.amount = pct / 100.0;
                }
            });
            if d.enabled && rate != 0 && rate != Denoise::SAMPLE_RATE {
                ui.label(
                    RichText::new(format!(
                        "Not running: it needs a 48000 Hz sample rate and PipeWire is running \
                         at {rate} Hz."
                    ))
                    .color(theme::p().warning)
                    .size(11.0),
                );
            }
            note(
                ui,
                "Removes steady background noise such as fans, hum and hiss from speech. \
                 It is trained on voices, so it will mangle music. Adds 20 ms of delay.",
            );
        }
        if d != before {
            self.denoise = Some(Local::new(d));
        }
    }

    pub(super) fn gate_section(
        &mut self,
        ui: &mut Ui,
        strip: &Strip,
        view: Option<GateView>,
        open: &mut bool,
    ) {
        let mut g = self.gate.as_ref().map_or(strip.gate, |l| l.value);
        let before = g;
        let summary = if !g.enabled {
            "off".to_string()
        } else {
            let state = match view {
                Some(v) if v.reduction <= -1.0 => "closed",
                Some(_) => "open",
                None => "waiting for audio",
            };
            format!("{state} · opens above {:.0} dB", g.threshold_db)
        };
        match section_header(
            ui,
            "Noise gate",
            &summary,
            Some(g.enabled),
            theme::p().gate_on,
            *open,
        ) {
            HeaderClick::Switch => g.enabled = !g.enabled,
            HeaderClick::Fold => *open = !*open,
            HeaderClick::None => {}
        }
        if *open {
            gate_level(ui, g, view);
            ui.add_enabled_ui(g.enabled, |ui| gate_controls(ui, &mut g));
            note(
                ui,
                "Silences the strip while it is quieter than the threshold, such as a \
                 microphone while nobody is talking.",
            );
        }
        if g != before {
            g.normalize();
            self.gate = Some(Local::new(g));
        }
    }

    pub(super) fn comp_section(
        &mut self,
        ui: &mut Ui,
        strip: &Strip,
        view: Option<CompView>,
        open: &mut bool,
    ) {
        let mut c = self.comp.as_ref().map_or(strip.compressor, |l| l.value);
        let before = c;
        let summary = if c.enabled {
            format!(
                "{}:1 above {:.0} dB, lift {:+.1} dB",
                fmt_ratio(c.ratio),
                c.threshold_db,
                c.makeup()
            )
        } else {
            "off".to_string()
        };
        match section_header(
            ui,
            "Compressor",
            &summary,
            Some(c.enabled),
            theme::p().comp_on,
            *open,
        ) {
            HeaderClick::Switch => c.enabled = !c.enabled,
            HeaderClick::Fold => *open = !*open,
            HeaderClick::None => {}
        }
        if *open {
            comp_graph(ui, &c, view);
            reduction_bar(
                ui,
                c.enabled,
                view.map_or(0.0, |v| v.reduction),
                theme::p().comp_on,
            );
            ui.add_space(2.0);
            ui.add_enabled_ui(c.enabled, |ui| comp_controls(ui, &mut c));
            note(
                ui,
                "Evens out a voice: turns it down while it is loud, then lifts the whole \
                 thing back up, so quiet words are easier to hear and shouts do not blast.",
            );
        }
        if c != before {
            c.normalize();
            self.comp = Some(Local::new(c));
        }
    }

    /// A bus's safety limiter: on or off, its ceiling and release, and how
    /// hard it is working.
    pub(super) fn limiter_section(
        &mut self,
        ui: &mut Ui,
        bus: &Bus,
        reduction: Option<f32>,
        open: &mut bool,
    ) {
        let mut l = self.limiter.as_ref().map_or(bus.limiter, |x| x.value);
        let before = l;
        let summary = if l.enabled {
            format!(
                "ceiling {:+.1} dB · release {:.0} ms",
                l.ceiling_db, l.release_ms
            )
        } else {
            "off: nothing stops it going over 0 dB".to_string()
        };
        match section_header(
            ui,
            "Safety limiter",
            &summary,
            Some(l.enabled),
            theme::p().limit,
            *open,
        ) {
            HeaderClick::Switch => l.enabled = !l.enabled,
            HeaderClick::Fold => *open = !*open,
            HeaderClick::None => {}
        }
        if *open {
            reduction_bar(ui, l.enabled, reduction.unwrap_or(0.0), theme::p().limit);
            ui.add_space(2.0);
            ui.add_enabled_ui(l.enabled, |ui| {
                ui.add(
                    egui::Slider::new(&mut l.ceiling_db, -24.0..=0.0)
                        .text("ceiling")
                        .suffix(" dB")
                        .fixed_decimals(1),
                )
                .on_hover_text(
                    "The level this bus never goes over. -1 dB leaves a little room for \
                     whatever records or hears it.",
                );
                ui.add(
                    egui::Slider::new(&mut l.release_ms, 10.0..=2000.0)
                        .logarithmic(true)
                        .text("release")
                        .suffix(" ms")
                        .fixed_decimals(0),
                )
                .on_hover_text(
                    "How long the bus takes to come back up after the limiter turned it \
                     down. Shorter is louder but can pump.",
                );
            });
            note(
                ui,
                "Keeps this bus from ever going over the ceiling, so nothing that records \
                 or hears it clips. It looks 1.5 ms ahead, which delays the bus that much \
                 while it is on. The handle beside the bus's meter moves the ceiling too.",
            );
        }
        if l != before {
            l.normalize();
            self.limiter = Some(Local::new(l));
        }
    }
}

/// The gate's sliders.
fn gate_controls(ui: &mut Ui, g: &mut Gate) {
    ui.add(
        egui::Slider::new(
            &mut g.threshold_db,
            GATE_THRESHOLD_MIN_DB..=GATE_THRESHOLD_MAX_DB,
        )
        .text("threshold")
        .suffix(" dB")
        .fixed_decimals(0),
    )
    .on_hover_text(
        "The gate opens when the level rises above this. Set it just above the \
         level of the noise you want to shut out: talk, and watch the bar above.",
    );
    // The range is stored as a negative gain; people think of it as how
    // much quieter it gets.
    let mut reduce = -g.range_db;
    if ui
        .add(
            egui::Slider::new(&mut reduce, 0.0..=90.0)
                .text("turn down by")
                .suffix(" dB")
                .fixed_decimals(0),
        )
        .on_hover_text(
            "How much quieter the strip gets while the gate is closed. 90 dB is \
             silence; something like 15 dB only takes the edge off the noise.",
        )
        .changed()
    {
        g.range_db = -reduce;
    }
    ui.add(
        egui::Slider::new(&mut g.attack_ms, 0.1..=100.0)
            .logarithmic(true)
            .text("attack")
            .suffix(" ms")
            .max_decimals(1),
    )
    .on_hover_text("How quickly the gate opens when you start talking.");
    ui.add(
        egui::Slider::new(&mut g.hold_ms, 0.0..=2000.0)
            .text("hold")
            .suffix(" ms")
            .fixed_decimals(0),
    )
    .on_hover_text(
        "How long the gate stays open after the level drops, so it does not close \
         in the gaps between words.",
    );
    ui.add(
        egui::Slider::new(&mut g.release_ms, 5.0..=3000.0)
            .logarithmic(true)
            .text("release")
            .suffix(" ms")
            .fixed_decimals(0),
    )
    .on_hover_text("How gradually the gate closes once the hold time is over.");
}

/// The compressor's sliders, and whether it sets its own lift.
fn comp_controls(ui: &mut Ui, c: &mut Compressor) {
    ui.add(
        egui::Slider::new(
            &mut c.threshold_db,
            COMP_THRESHOLD_MIN_DB..=COMP_THRESHOLD_MAX_DB,
        )
        .text("threshold")
        .suffix(" dB")
        .fixed_decimals(0),
    )
    .on_hover_text(
        "The compressor turns the strip down while it is louder than this. The \
         dot on the graph is the level going in right now.",
    );
    ui.add(
        egui::Slider::new(&mut c.ratio, 1.0..=COMP_RATIO_MAX)
            .logarithmic(true)
            .text("ratio")
            .custom_formatter(|v, _| format!("{} : 1", fmt_ratio(v as f32)))
            .custom_parser(|s| s.trim().trim_end_matches(": 1").trim().parse().ok()),
    )
    .on_hover_text(
        "How hard it turns down above the threshold. At 4 : 1, every 4 dB \
         over the threshold comes out as 1 dB over. 2 to 4 is gentle, 8 and \
         up is heavy.",
    );
    ui.add(
        egui::Slider::new(&mut c.attack_ms, 0.1..=200.0)
            .logarithmic(true)
            .text("attack")
            .suffix(" ms")
            .max_decimals(1),
    )
    .on_hover_text(
        "How quickly it turns down when the level goes over. Slower lets the \
         start of each word through, which keeps speech crisp.",
    );
    ui.add(
        egui::Slider::new(&mut c.release_ms, 10.0..=3000.0)
            .logarithmic(true)
            .text("release")
            .suffix(" ms")
            .fixed_decimals(0),
    )
    .on_hover_text("How quickly it lets go once the level drops back.");
    let mut lift = c.makeup();
    let r = ui
        .add_enabled(
            !c.auto_makeup,
            egui::Slider::new(&mut lift, 0.0..=COMP_MAKEUP_MAX_DB)
                .text("lift")
                .suffix(" dB")
                .max_decimals(1),
        )
        .on_hover_text(
            "Turns the whole strip back up after compressing, so it is as loud \
             as before, only more even.",
        );
    if r.changed() {
        c.makeup_db = lift;
    }
    let mut auto = c.auto_makeup;
    if ui
        .checkbox(&mut auto, "Set the lift automatically")
        .on_hover_text("Work the lift out from the threshold and ratio")
        .changed()
    {
        // Start the manual lift where the automatic one was.
        if !auto {
            c.makeup_db = c.makeup();
        }
        c.auto_makeup = auto;
    }
}

/// The level going into the gate, with its threshold marked.
fn gate_level(ui: &mut Ui, gate: Gate, view: Option<GateView>) {
    let w = ui.available_width().min(SIDE_W);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 16.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3, theme::p().track);
    let x = |db: f32| {
        rect.left()
            + rect.width() * ((db - GATE_THRESHOLD_MIN_DB) / -GATE_THRESHOLD_MIN_DB).clamp(0.0, 1.0)
    };
    if let Some(v) = view {
        let open = v.reduction > -1.0;
        let color = if open {
            theme::p().gate_on
        } else {
            theme::p().text_dim
        };
        let bar = Rect::from_min_max(rect.min, pos2(x(v.level), rect.bottom()));
        painter.rect_filled(bar, 3, color.gamma_multiply(0.8));
    }
    let tx = x(gate.threshold_db);
    painter.vline(tx, rect.y_range(), Stroke::new(2.0_f32, theme::p().marker));
    resp.on_hover_text(match view {
        Some(v) => format!(
            "Level going into the gate: {:.0} dB. The white line is the threshold.",
            v.level
        ),
        None => "The level shows here while the gate is on.".to_string(),
    });
}

/// The compressor's curve: level in along the bottom, level out up the
/// side, both from -60 to 0 dB. The dim diagonal is what comes out with the
/// compressor off, and the dot is the level going in right now.
fn comp_graph(ui: &mut Ui, c: &Compressor, view: Option<CompView>) {
    let w = ui.available_width();
    let h = (w * 0.62).min(170.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let plot = Rect::from_min_max(
        pos2(rect.left() + 26.0, rect.top() + 4.0),
        pos2(rect.right() - 4.0, rect.bottom() - 16.0),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(plot, 4, theme::p().track);
    let lo = COMP_THRESHOLD_MIN_DB;
    let x = |db: f32| plot.left() + plot.width() * ((db - lo) / -lo).clamp(0.0, 1.0);
    let y = |db: f32| plot.bottom() - plot.height() * ((db - lo) / -lo).clamp(0.0, 1.0);
    let grid = Stroke::new(1.0_f32, theme::p().button_off);
    for db in [-12.0, -24.0, -36.0, -48.0] {
        painter.hline(plot.x_range(), y(db), grid);
        painter.vline(x(db), plot.y_range(), grid);
    }
    for (db, text) in [(0.0, "0"), (-30.0, "-30"), (-60.0, "-60")] {
        painter.text(
            pos2(plot.left() - 4.0, y(db)),
            Align2::RIGHT_CENTER,
            text,
            FontId::proportional(10.0),
            theme::p().text_dim,
        );
        // Both scales start at -60 in the corner; label it once.
        if db == lo {
            continue;
        }
        painter.text(
            pos2(x(db), plot.bottom() + 8.0),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(10.0),
            theme::p().text_dim,
        );
    }
    painter.line_segment(
        [pos2(x(lo), y(lo)), pos2(x(0.0), y(0.0))],
        Stroke::new(1.0_f32, theme::p().text_dim.linear_multiply(0.4)),
    );
    let color = if c.enabled {
        theme::p().comp_on
    } else {
        theme::p().text_dim
    };
    painter.extend(egui::Shape::dashed_line(
        &[
            pos2(x(c.threshold_db), plot.top()),
            pos2(x(c.threshold_db), plot.bottom()),
        ],
        Stroke::new(1.0_f32, color.linear_multiply(0.6)),
        3.0,
        3.0,
    ));
    let pts: Vec<Pos2> = (0..=120)
        .map(|k| {
            let db = lo + k as f32 * 0.5;
            pos2(x(db), y(c.output_db(db)))
        })
        .collect();
    painter.add(egui::Shape::line(pts, Stroke::new(2.0_f32, color)));
    if let Some(v) = view.filter(|v| c.enabled && v.level > lo) {
        painter.circle_filled(
            pos2(x(v.level), y(c.output_db(v.level))),
            4.5,
            theme::p().marker,
        );
    }
    resp.on_hover_text(
        "Level in along the bottom, level out up the side. The dim diagonal is \
         the compressor switched off; the dashed line is the threshold.",
    );
}

/// How far the compressor or limiter is turning things down right now.
fn reduction_bar(ui: &mut Ui, enabled: bool, reduction: f32, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("turning down")
                .size(11.0)
                .color(theme::p().text_dim),
        );
        let w = (ui.available_width() - 60.0).max(40.0);
        let (rect, _) = ui.allocate_exact_size(vec2(w, 10.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 3, theme::p().track);
        let amount = if enabled { -reduction } else { 0.0 };
        if amount > 0.05 {
            let frac = (amount / 20.0).clamp(0.0, 1.0);
            let bar = Rect::from_min_max(
                rect.min,
                pos2(rect.left() + rect.width() * frac, rect.bottom()),
            );
            painter.rect_filled(bar, 3, color);
        }
        ui.label(
            RichText::new(if amount > 0.05 {
                format!("{amount:.1} dB")
            } else {
                "0 dB".to_string()
            })
            .size(11.0)
            .color(if amount > 0.05 {
                color
            } else {
                theme::p().text_dim
            }),
        );
    });
}

/// A ratio as people write it: "4", "2.5", "20".
fn fmt_ratio(r: f32) -> String {
    let r = (r * 10.0).round() / 10.0;
    if r.fract() == 0.0 {
        format!("{r:.0}")
    } else {
        format!("{r:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios_read_like_people_write_them() {
        assert_eq!(fmt_ratio(4.0), "4");
        assert_eq!(fmt_ratio(2.54), "2.5");
        assert_eq!(fmt_ratio(19.98), "20");
    }
}
