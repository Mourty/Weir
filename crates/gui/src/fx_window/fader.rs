//! The fader section: the same level, pan and mute as on the mixer, so that
//! everything about a strip or bus is in one place.

use super::section::{note, section_header, HeaderClick};
use super::{FxTarget, FxWindow, Local};
use crate::patch::CommonPatch;
use crate::theme;
use egui::Ui;
use weir_protocol::{BusPatch, Flag, FullState, Request, GAIN_MAX_DB, GAIN_MIN_DB};

impl FxWindow {
    /// The fader, as in the mixer, with the strip's pan or the bus's mono
    /// fold.
    pub(super) fn fader_section(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        open: &mut bool,
        actions: &mut Vec<Request>,
    ) {
        let (gain_now, mute, pan_now, mono) = match self.target {
            FxTarget::Strip(id) => {
                let Some(s) = state.mixer.strip(id) else {
                    return;
                };
                (s.gain_db, s.mute, Some(s.pan), None)
            }
            FxTarget::Bus(id) => {
                let Some(b) = state.mixer.bus(id) else {
                    return;
                };
                (b.gain_db, b.mute, None, Some(b.mono))
            }
        };
        let mut gain = self.gain.as_ref().map_or(gain_now, |l| l.value);
        let mut pan = pan_now.map(|p| self.pan.as_ref().map_or(p, |l| l.value));
        let mut summary = vec![if gain <= GAIN_MIN_DB {
            "silent".to_string()
        } else {
            format!("{gain:+.1} dB")
        }];
        if let Some(p) = pan.filter(|p| p.abs() >= 0.005) {
            summary.push(format!("pan {}", pan_text(p)));
        }
        if mono == Some(true) {
            summary.push("mono".into());
        }
        if mute {
            summary.push("muted".into());
        }
        if let HeaderClick::Fold = section_header(
            ui,
            "Fader",
            &summary.join(" · "),
            None,
            theme::p().fader,
            *open,
        ) {
            *open = !*open;
        }
        if !*open {
            return;
        }
        if ui
            .add(
                egui::Slider::new(&mut gain, GAIN_MIN_DB..=GAIN_MAX_DB)
                    .text("level")
                    .suffix(" dB")
                    .fixed_decimals(1),
            )
            .on_hover_text("The same fader as in the mixer. At the bottom, -60 dB, it is silent.")
            .changed()
        {
            self.gain = Some(Local::new(gain));
        }
        if let Some(p) = pan.as_mut() {
            if ui
                .add(
                    egui::Slider::new(p, -1.0..=1.0)
                        .text("pan")
                        .custom_formatter(|v, _| pan_text(v as f32))
                        .custom_parser(parse_pan),
                )
                .on_hover_text(
                    "Balance between left and right: the other side is turned down. Type \
                     L 20, R 50 or center.",
                )
                .changed()
            {
                self.pan = Some(Local::new(*p));
            }
        }
        ui.horizontal(|ui| {
            let mut m = mute;
            if ui.checkbox(&mut m, "Mute").changed() {
                let patch = CommonPatch {
                    mute: Some(Flag::from(m)),
                    ..Default::default()
                };
                actions.push(patch.to(self.target));
            }
            if let (Some(mut v), FxTarget::Bus(id)) = (mono, self.target) {
                if ui
                    .checkbox(&mut v, "Mono")
                    .on_hover_text("Fold every channel into one, on all of its speakers")
                    .changed()
                {
                    actions.push(Request::SetBus(BusPatch {
                        id,
                        mono: Some(Flag::from(v)),
                        ..Default::default()
                    }));
                }
            }
        });
        note(
            ui,
            "The same controls as on the mixer, here so that everything about it is in \
             one place.",
        );
    }
}

/// A pan position as the mixer shows it: "center", "L 20", "R 100".
fn pan_text(p: f32) -> String {
    let pct = (p * 100.0).round();
    if pct == 0.0 {
        "center".into()
    } else if pct < 0.0 {
        format!("L {:.0}", -pct)
    } else {
        format!("R {pct:.0}")
    }
}

/// Read a typed pan position: "L 20", "r50", "center", or -100 to 100.
fn parse_pan(s: &str) -> Option<f64> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() || s == "center" || s == "c" {
        return Some(0.0);
    }
    let number = |t: &str| {
        t.trim()
            .parse::<f64>()
            .ok()
            .map(|v| v.clamp(0.0, 100.0) / 100.0)
    };
    if let Some(rest) = s.strip_prefix('l') {
        return number(rest).map(|v| -v);
    }
    if let Some(rest) = s.strip_prefix('r') {
        return number(rest);
    }
    s.parse::<f64>()
        .ok()
        .map(|v| v.clamp(-100.0, 100.0) / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_reads_back_what_it_shows() {
        assert_eq!(pan_text(0.0), "center");
        assert_eq!(pan_text(-0.2), "L 20");
        assert_eq!(pan_text(1.0), "R 100");
        assert_eq!(parse_pan("L 20"), Some(-0.2));
        assert_eq!(parse_pan("r50"), Some(0.5));
        assert_eq!(parse_pan("Center"), Some(0.0));
        assert_eq!(parse_pan("-150"), Some(-1.0));
        assert_eq!(parse_pan("left"), None);
    }
}
