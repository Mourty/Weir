//! The ducking section: turning a strip down while others are heard. It is
//! set on the strip that gets turned down; the strips that do the turning
//! down show a summary of it in their own windows.

use super::section::{note, section_header, HeaderClick};
use super::{FxWindow, Local};
use crate::app::bus_label;
use crate::theme;
use egui::{pos2, vec2, Rect, RichText, Sense, Ui};
use std::collections::BTreeSet;
use weir_protocol::*;

impl FxWindow {
    pub(super) fn duck_section(
        &mut self,
        ui: &mut Ui,
        strip: &Strip,
        state: &FullState,
        now_db: Option<f32>,
        open: &mut bool,
    ) {
        let mut d = self
            .duck
            .as_ref()
            .map_or_else(|| strip.ducking.clone(), |l| l.value.clone());
        let before = d.clone();
        let mixer = &state.mixer;
        let when = listing(
            d.triggers
                .iter()
                .filter_map(|t| mixer.strip(*t))
                .map(|s| s.name.clone()),
            ", ",
        );
        let mixes = mixes(mixer, &d, ", ");
        let summary = if d.enabled {
            format!("down {:.0} dB in {mixes} for {when}", d.amount_db)
        } else {
            "off".to_string()
        };
        match section_header(
            ui,
            "Ducking",
            &summary,
            Some(d.enabled),
            theme::p().duck,
            *open,
        ) {
            HeaderClick::Switch => {
                d.enabled = !d.enabled;
                // Switching on with nobody to listen to would do nothing, so
                // start with the first microphone, as the most likely one.
                if d.enabled && d.triggers.is_empty() {
                    let mic = mixer
                        .strips
                        .iter()
                        .filter(|s| s.id != strip.id)
                        .find(|s| s.kind == StripKind::Hardware)
                        .or_else(|| mixer.strips.iter().find(|s| s.id != strip.id));
                    if let Some(mic) = mic {
                        d.triggers.insert(mic.id);
                        *open = true;
                    }
                }
            }
            HeaderClick::Fold => *open = !*open,
            HeaderClick::None => {}
        }
        if *open {
            ui.label(
                RichText::new(format!(
                    "Turn {} down while another strip is making sound, such as your \
                     microphone while you talk.",
                    strip.name
                ))
                .size(11.0)
                .color(theme::p().text_dim),
            );
            ui.add_space(4.0);
            ui.add_enabled_ui(d.enabled, |ui| duck_controls(ui, strip, mixer, &mut d));
            let now = if d.enabled {
                now_db.unwrap_or(0.0)
            } else {
                0.0
            };
            ducking_now(ui, strip, now, &mixes);
        }
        if d != before {
            d.normalize();
            self.duck = Some(Local::new(d));
        }
    }
}

/// Who to listen to, how far to turn down, in which mixes, and how fast.
fn duck_controls(ui: &mut Ui, strip: &Strip, mixer: &MixerState, d: &mut Ducking) {
    ui.label(RichText::new("While this is heard").size(12.0));
    ui.horizontal_wrapped(|ui| {
        for other in mixer.strips.iter().filter(|s| s.id != strip.id) {
            let on = d.triggers.contains(&other.id);
            let w = 20.0 + 7.0 * other.name.chars().count().min(14) as f32;
            if crate::widgets::toggle(ui, &other.name, on, theme::p().duck, vec2(w, 22.0)).clicked()
            {
                if on {
                    d.triggers.remove(&other.id);
                } else {
                    d.triggers.insert(other.id);
                }
            }
        }
    });
    ui.add_space(2.0);
    ui.add(
        egui::Slider::new(&mut d.amount_db, 0.0..=DUCK_AMOUNT_MAX_DB)
            .text(format!("turn {} down by", strip.name))
            .suffix(" dB")
            .fixed_decimals(0),
    );
    ui.add_space(2.0);
    ui.label(RichText::new("Only in these mixes").size(12.0));
    ui.horizontal_wrapped(|ui| {
        let all: BTreeSet<BusId> = mixer.buses.iter().map(|b| b.id).collect();
        for b in &mixer.buses {
            let routed = strip.routes.contains(&b.id);
            let on = d.applies_to(b.id);
            let label = bus_label(mixer, b);
            let r = ui
                .add_enabled_ui(routed, |ui| {
                    crate::widgets::toggle(
                        ui,
                        &label,
                        on && routed,
                        theme::bus_color(b),
                        vec2(36.0, 22.0),
                    )
                })
                .inner
                .on_hover_text(b.name.clone())
                .on_disabled_hover_text(format!("{} is not sent to {}", strip.name, b.name));
            if r.clicked() {
                // An empty list means every bus; spell it out before taking
                // one away.
                let mut set = if d.buses.is_empty() {
                    all.clone()
                } else {
                    d.buses.clone()
                };
                if on {
                    set.remove(&b.id);
                } else {
                    set.insert(b.id);
                }
                d.buses = if set == all { BTreeSet::new() } else { set };
            }
        }
    });
    ui.add_space(2.0);
    egui::CollapsingHeader::new(
        RichText::new(format!(
            "Timing: {:.0} ms down, {:.0} ms pause, {:.0} ms up",
            d.attack_ms, d.hold_ms, d.release_ms
        ))
        .size(11.0),
    )
    .id_salt("duck_timing")
    .show(ui, |ui| {
        ui.add(
            egui::Slider::new(
                &mut d.threshold_db,
                DUCK_THRESHOLD_MIN_DB..=DUCK_THRESHOLD_MAX_DB,
            )
            .text("heard above")
            .suffix(" dB")
            .fixed_decimals(0),
        )
        .on_hover_text(
            "How loud a strip above must be to count as heard. A strip with its \
             gate on also needs the gate open.",
        );
        ui.add(
            egui::Slider::new(&mut d.attack_ms, 1.0..=2000.0)
                .logarithmic(true)
                .text("down in")
                .suffix(" ms")
                .fixed_decimals(0),
        );
        ui.add(
            egui::Slider::new(&mut d.hold_ms, 0.0..=5000.0)
                .text("pause")
                .suffix(" ms")
                .fixed_decimals(0),
        )
        .on_hover_text(
            "How long to stay down after it goes quiet, so it does not bob up between words.",
        );
        ui.add(
            egui::Slider::new(&mut d.release_ms, 10.0..=10000.0)
                .logarithmic(true)
                .text("back up over")
                .suffix(" ms")
                .fixed_decimals(0),
        );
    });
}

/// How far ducking is turning the strip down right now, as a bar and in
/// words.
fn ducking_now(ui: &mut Ui, strip: &Strip, now: f32, mixes: &str) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("right now")
                .size(11.0)
                .color(theme::p().text_dim),
        );
        let w = (ui.available_width() - 4.0).max(40.0);
        let (rect, _) = ui.allocate_exact_size(vec2(w, 10.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 3, theme::p().track);
        if now < -0.5 {
            let frac = (-now / DUCK_AMOUNT_MAX_DB.min(40.0)).clamp(0.0, 1.0);
            let bar = Rect::from_min_max(
                rect.min,
                pos2(rect.left() + rect.width() * frac, rect.bottom()),
            );
            painter.rect_filled(bar, 3, theme::p().duck);
        }
    });
    let (text, color) = if now < -0.5 {
        (
            format!("turning {} down {:.0} dB in {mixes}", strip.name, -now),
            theme::p().duck,
        )
    } else {
        ("not ducking".to_string(), theme::p().text_dim)
    };
    ui.label(RichText::new(text).size(11.0).color(color));
}

/// The strips this one turns down while it is heard, for its own window:
/// ducking is set on the strip that gets turned down, so without this the
/// microphone's window would not show it does anything.
pub(super) fn ducks_others(ui: &mut Ui, strip: &Strip, state: &FullState) {
    let mixer = &state.mixer;
    let ducked: Vec<&Strip> = mixer
        .strips
        .iter()
        .filter(|s| s.ducking.enabled && s.ducking.triggers.contains(&strip.id))
        .collect();
    if ducked.is_empty() {
        return;
    }
    ui.add_space(10.0);
    ui.label(RichText::new("Ducks other strips").size(13.0).strong());
    for s in ducked {
        let d = &s.ducking;
        ui.label(
            RichText::new(format!(
                "While {} is heard, {} turns down {:.0} dB in {}.",
                strip.name,
                s.name,
                d.amount_db,
                mixes(mixer, d, " and ")
            ))
            .size(11.0),
        );
    }
    note(ui, "Change these in each strip's own Ducking section.");
}

/// The mixes ducking turns the strip down in, by label: "A1, B1", or
/// "every mix".
fn mixes(mixer: &MixerState, d: &Ducking, separator: &str) -> String {
    if d.buses.is_empty() {
        return "every mix".to_string();
    }
    listing(
        mixer
            .buses
            .iter()
            .filter(|b| d.buses.contains(&b.id))
            .map(|b| bus_label(mixer, b)),
        separator,
    )
}

/// Names joined by `separator`, or "nobody" when there are none.
fn listing(names: impl Iterator<Item = String>, separator: &str) -> String {
    let v: Vec<String> = names.collect();
    if v.is_empty() {
        "nobody".to_string()
    } else {
        v.join(separator)
    }
}
