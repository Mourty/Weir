//! The signal chain along the top of the settings window: every stage the
//! sound goes through, in order, lit where it is on.

use super::{FxTarget, FxWindow, Section};
use crate::theme;
use egui::{vec2, Color32, RichText, Ui};
use weir_protocol::{FullState, Upmix};

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

impl FxWindow {
    /// The signal chain, lit where it is on. Clicking a link opens its
    /// section down the side and scrolls to it.
    pub(super) fn chain_row(&mut self, ui: &mut Ui, state: &FullState) {
        let Some(links) = self.links(state) else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("SIGNAL CHAIN")
                    .size(10.0)
                    .color(theme::p().text_dim),
            );
            ui.add_space(4.0);
            for (k, link) in links.iter().enumerate() {
                if k > 0 {
                    ui.label(RichText::new("›").color(theme::p().text_dim));
                }
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
                let r = crate::widgets::toggle(ui, link.label, link.on, link.color, vec2(w, 20.0))
                    .on_hover_text(format!("{}{state}{hint}", link.what));
                if let (true, Some(section)) = (r.clicked(), link.section) {
                    self.focus = Some(section);
                }
            }
        });
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
