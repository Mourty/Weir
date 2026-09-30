//! Upmixing and downmixing: how a strip plays on a bus with more speakers
//! than it has channels, and how a bus plays channels it has no speaker
//! for. Every choice here is one click, so it is sent straight away.

use super::section::{note, section_header, HeaderClick};
use crate::app::bus_label;
use crate::theme;
use egui::{RichText, Ui};
use weir_protocol::*;

/// Upmixing: how a strip plays on buses with more speakers than it has
/// channels.
pub(super) fn upmix_section(
    ui: &mut Ui,
    strip: &Strip,
    mixer: &MixerState,
    open: &mut bool,
    actions: &mut Vec<Request>,
) {
    let mut summary = strip.upmix.label().to_string();
    if strip.subwoofer {
        summary.push_str(", bass on the subwoofer");
    }
    if let HeaderClick::Fold = section_header(ui, "Upmix", &summary, None, theme::p().accent, *open)
    {
        *open = !*open;
    }
    if !*open {
        return;
    }
    let set = |patch: StripPatch| {
        Request::SetStrip(StripPatch {
            id: strip.id,
            ..patch
        })
    };
    for upmix in Upmix::ALL {
        if ui.radio(strip.upmix == upmix, upmix.label()).clicked() && strip.upmix != upmix {
            actions.push(set(StripPatch {
                upmix: Some(upmix),
                ..Default::default()
            }));
        }
        hint(ui, upmix.description());
    }
    ui.add_space(4.0);
    let mut sub = strip.subwoofer;
    let has_own = strip.layout.positions().contains(&ChannelPosition::LFE);
    let r = ui.add_enabled(
        !has_own,
        egui::Checkbox::new(&mut sub, "Bass on the subwoofer"),
    );
    let r = if has_own {
        r.on_disabled_hover_text(
            "This strip has a subwoofer channel of its own, which goes to the subwoofer \
             already.",
        )
    } else {
        r.on_hover_text("Also play everything below 120 Hz on the subwoofer of buses that have one")
    };
    if r.changed() {
        actions.push(set(StripPatch {
            subwoofer: Some(Flag::from(sub)),
            ..Default::default()
        }));
    }
    let wider = wider_buses(strip, mixer);
    if wider.is_empty() {
        note(
            ui,
            &format!(
                "None of the buses {} is sent to has speakers it has no channel for, so \
                 this changes nothing right now.",
                strip.name
            ),
        );
    } else {
        let names: Vec<String> = wider.iter().map(|b| bus_label(mixer, b)).collect();
        note(
            ui,
            &format!("Changes how it plays on {}.", names.join(" and ")),
        );
    }
}

/// The buses `strip` is sent to that have speakers it has no channel for,
/// where upmixing has something to do.
fn wider_buses<'a>(strip: &Strip, mixer: &'a MixerState) -> Vec<&'a Bus> {
    let own = strip.layout.positions();
    let mono = own.contains(&ChannelPosition::Mono);
    mixer
        .buses
        .iter()
        .filter(|b| strip.routes.contains(&b.id))
        .filter(|b| {
            b.layout.positions().iter().any(|p| {
                !own.contains(p)
                    && *p != ChannelPosition::LFE
                    // A mono strip already plays on the front pair.
                    && !(mono && matches!(p, ChannelPosition::FL | ChannelPosition::FR))
            })
        })
        .collect()
}

/// Downmixing: how a bus plays channels of a strip it has no speaker for.
pub(super) fn downmix_section(
    ui: &mut Ui,
    bus: &Bus,
    mixer: &MixerState,
    open: &mut bool,
    actions: &mut Vec<Request>,
) {
    let d = bus.downmix;
    if let HeaderClick::Fold = section_header(
        ui,
        "Downmix",
        &downmix_summary(d),
        None,
        theme::p().accent,
        *open,
    ) {
        *open = !*open;
    }
    if !*open {
        return;
    }
    let set = |patch: DownmixPatch| {
        Request::SetBus(BusPatch {
            id: bus.id,
            downmix: Some(patch),
            ..Default::default()
        })
    };
    let method = |ui: &mut Ui, actions: &mut Vec<Request>, m: DownmixMethod| {
        if ui.radio(d.method == m, m.label()).clicked() && d.method != m {
            actions.push(set(DownmixPatch {
                method: Some(m),
                ..Default::default()
            }));
        }
        hint(ui, m.description());
    };
    method(ui, actions, DownmixMethod::Standard);
    method(ui, actions, DownmixMethod::Matrix);
    if !d.method.is_part() {
        ui.add_space(4.0);
        let center = LevelRow {
            title: "Center",
            tip: "How loud the center channel, which carries most dialog, is in the front \
                  speakers. Louder makes speech clearer.",
            choices: &CENTER_MIX_LEVELS,
            now: d.center_db,
            enabled: true,
        };
        if let Some(v) = center.show(ui) {
            actions.push(set(DownmixPatch {
                center_db: Some(v),
                ..Default::default()
            }));
        }
        let surrounds = LevelRow {
            title: "Surrounds",
            tip: if d.method == DownmixMethod::Matrix {
                "Matrix surround uses levels of its own."
            } else {
                "How loud the surround channels are in the front speakers."
            },
            choices: &SURROUND_MIX_LEVELS,
            now: d.surround_db,
            enabled: d.method == DownmixMethod::Standard,
        };
        if let Some(v) = surrounds.show(ui) {
            actions.push(set(DownmixPatch {
                surround_db: Some(v),
                ..Default::default()
            }));
        }
        let mut lfe = d.lfe;
        if ui
            .checkbox(&mut lfe, "Keep the subwoofer channel")
            .on_hover_text(
                "Mix the subwoofer (LFE) channel into the front speakers. Standard downmixes \
                 leave it out, since most films repeat the bass elsewhere.",
            )
            .changed()
        {
            actions.push(set(DownmixPatch {
                lfe: Some(Flag::from(lfe)),
                ..Default::default()
            }));
        }
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Or play one part only, on the front pair. For building surround out of \
             stereo devices, one bus each:",
        )
        .size(11.0)
        .color(theme::p().text_dim),
    );
    for m in DownmixMethod::ALL.into_iter().filter(|m| m.is_part()) {
        method(ui, actions, m);
    }
    let folded = downmixed_strips(bus, mixer);
    if folded.is_empty() {
        note(
            ui,
            &format!(
                "Nothing to downmix right now: every strip sent to {} fits its speakers.",
                bus.name
            ),
        );
    } else {
        note(ui, &format!("Downmixes {} here.", folded.join(" and ")));
    }
}

/// The downmix in a line: "Standard · center -3 dB · surrounds -3 dB".
fn downmix_summary(d: Downmix) -> String {
    if d.method.is_part() {
        return d.method.label().to_string();
    }
    let mut parts = vec![
        d.method.label().to_string(),
        format!("center {}", mix_level_label(d.center_db)),
    ];
    if d.method == DownmixMethod::Standard {
        let surrounds = if d.surround_db <= GAIN_MIN_DB {
            "off".to_string()
        } else {
            mix_level_label(d.surround_db)
        };
        parts.push(format!("surrounds {surrounds}"));
    }
    parts.join(" · ")
}

/// The strips sent to `bus` with channels it has no speaker for, which it
/// downmixes. A mono strip spreads rather than folds, so it is not one.
fn downmixed_strips<'a>(bus: &Bus, mixer: &'a MixerState) -> Vec<&'a str> {
    let speakers = bus.layout.positions();
    mixer
        .strips
        .iter()
        .filter(|s| s.routes.contains(&bus.id))
        .filter(|s| {
            s.layout
                .positions()
                .iter()
                .any(|p| !speakers.contains(p) && *p != ChannelPosition::Mono)
        })
        .map(|s| s.name.as_str())
        .collect()
}

/// A row of levels to pick from, such as how loud the center channel is.
struct LevelRow<'a> {
    title: &'a str,
    tip: &'a str,
    choices: &'a [f32],
    now: f32,
    enabled: bool,
}

impl LevelRow<'_> {
    /// Draw the row; the level clicked, if one was.
    fn show(&self, ui: &mut Ui) -> Option<f32> {
        let mut picked = None;
        ui.add_enabled_ui(self.enabled, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(self.title).on_hover_text(self.tip);
                for &v in self.choices {
                    let r = ui.selectable_label((self.now - v).abs() < 0.01, mix_level_label(v));
                    let r = if v == -3.0 {
                        r.on_hover_text("The standard level")
                    } else {
                        r
                    };
                    if r.clicked() {
                        picked = Some(v);
                    }
                }
            });
        });
        picked
    }
}

/// A small line of explanation under an option.
fn hint(ui: &mut Ui, text: &str) {
    ui.indent(text, |ui| {
        ui.label(RichText::new(text).size(10.5).color(theme::p().text_dim));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixer() -> MixerState {
        let mut music = Strip::new(1, "Music", StripKind::Virtual, ChannelLayout::Stereo);
        music.routes.insert(1);
        let mut film = Strip::new(2, "Film", StripKind::Virtual, ChannelLayout::Surround51);
        film.routes.insert(1);
        MixerState {
            strips: vec![music, film],
            buses: vec![Bus::new(
                1,
                "Speakers",
                BusKind::Hardware,
                ChannelLayout::Surround51,
            )],
        }
    }

    #[test]
    fn upmixing_matters_only_where_there_are_more_speakers() {
        let m = mixer();
        assert_eq!(wider_buses(&m.strips[0], &m).len(), 1);
        assert!(wider_buses(&m.strips[1], &m).is_empty());
    }

    #[test]
    fn a_bus_downmixes_strips_with_more_channels() {
        let mut m = mixer();
        m.buses[0].layout = ChannelLayout::Stereo;
        assert_eq!(downmixed_strips(&m.buses[0], &m), ["Film"]);
    }
}
