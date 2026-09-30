//! The equalizer, in the middle of the settings window: a toolbar with the
//! switch and presets, the curve to drag bands on (see `curve`), and a
//! table of the bands to type values into.

use super::curve::{SPECTRUM_IN, SPECTRUM_OUT};
use super::{FxTarget, FxWindow, Local};
use crate::patch::CommonPatch;
use crate::prefs::SpectrumView;
use crate::theme;
use egui::{vec2, Align, Color32, Layout, RichText, Sense, Ui};
use weir_protocol::*;

/// One color per band, reused in order. Fixed rather than from the palette,
/// and mid-bright, so they show on the light theme's graph as well as the
/// dark one's.
const BAND_COLORS: [Color32; 8] = [
    Color32::from_rgb(240, 125, 95),
    Color32::from_rgb(240, 190, 70),
    Color32::from_rgb(150, 210, 90),
    Color32::from_rgb(70, 200, 185),
    Color32::from_rgb(95, 155, 240),
    Color32::from_rgb(170, 125, 240),
    Color32::from_rgb(230, 110, 185),
    Color32::from_rgb(150, 156, 172),
];

/// The color of band `i`, on the curve and in the table.
pub(super) fn band_color(i: usize) -> Color32 {
    BAND_COLORS[i % BAND_COLORS.len()]
}

impl FxWindow {
    fn eq_of<'a>(&self, state: &'a FullState) -> Option<&'a Equalizer> {
        match self.target {
            FxTarget::Strip(id) => state.mixer.strip(id).map(|s| &s.eq),
            FxTarget::Bus(id) => state.mixer.bus(id).map(|b| &b.eq),
        }
    }

    /// A request changing this window's equalizer.
    pub(super) fn eq_request(&self, patch: EqPatch) -> Request {
        let patch = CommonPatch {
            eq: Some(patch),
            ..Default::default()
        };
        patch.to(self.target)
    }

    pub(super) fn eq_section(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        actions: &mut Vec<Request>,
    ) {
        let Some(eq) = self.eq_of(state) else {
            return;
        };
        let enabled = eq.enabled;
        let mut bands = self
            .bands
            .as_ref()
            .map_or_else(|| eq.bands.clone(), |l| l.value.clone());
        let before = bands.clone();
        if self.selected.is_some_and(|i| i >= bands.len()) {
            self.selected = None;
        }
        let rate = match state.engine.sample_rate {
            0 => 48_000.0,
            r => r as f32,
        };

        ui.label(RichText::new("Equalizer").size(15.0).strong());
        self.toolbar(ui, state, enabled, &mut bands, actions);
        self.save_preset_row(ui, &bands, actions);
        if !enabled {
            ui.label(
                RichText::new(
                    "The equalizer is off: audio passes through unchanged. You can still \
                     edit the bands.",
                )
                .size(11.0)
                .color(theme::p().warning),
            );
        }
        ui.add_space(2.0);
        self.view_row(ui);
        ui.add_space(2.0);

        let curve_h = (ui.available_height() * 0.6).clamp(180.0, 560.0);
        self.curve(ui, &mut bands, enabled, rate, curve_h);
        ui.add_space(6.0);
        self.band_table(ui, &mut bands);

        // Delete removes the selected band, unless a text field is being
        // typed into.
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        if !typing && ui.input(|i| i.key_pressed(egui::Key::Delete)) {
            if let Some(i) = self.selected.take() {
                if i < bands.len() {
                    bands.remove(i);
                }
            }
        }

        if bands != before {
            self.bands = Some(Local::new(bands));
        }
    }

    /// The switch, the presets, and adding and removing bands.
    fn toolbar(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        enabled: bool,
        bands: &mut Vec<EqBand>,
        actions: &mut Vec<Request>,
    ) {
        ui.horizontal_wrapped(|ui| {
            let label = if enabled { "On" } else { "Off" };
            if crate::widgets::toggle(ui, label, enabled, theme::p().eq_on, vec2(52.0, 24.0))
                .clicked()
            {
                actions.push(self.eq_request(EqPatch {
                    enabled: Some(Flag::from(!enabled)),
                    bands: None,
                }));
            }
            ui.add_space(8.0);
            self.preset_picker(ui, state, bands, actions);
            if ui
                .add_enabled(!bands.is_empty(), egui::Button::new("Save as preset…"))
                .clicked()
            {
                self.save_name = Some(String::new());
                self.focus_save_name = true;
            }
            ui.add_space(8.0);
            if ui
                .add_enabled(bands.len() < EQ_MAX_BANDS, egui::Button::new("+ Band"))
                .on_hover_text("Add a band. You can also double-click the curve.")
                .clicked()
            {
                bands.push(EqBand::new(
                    EqBandKind::Peak,
                    free_frequency(bands),
                    0.0,
                    1.0,
                ));
                self.selected = Some(bands.len() - 1);
            }
            if ui
                .add_enabled(!bands.is_empty(), egui::Button::new("Remove all"))
                .clicked()
            {
                bands.clear();
                self.selected = None;
            }
        });
    }

    /// The field for naming a new preset, while it is open.
    fn save_preset_row(&mut self, ui: &mut Ui, bands: &[EqBand], actions: &mut Vec<Request>) {
        let Some(name) = self.save_name.as_mut() else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.label("Name");
            let r = ui.add(egui::TextEdit::singleline(name).desired_width(200.0));
            if std::mem::take(&mut self.focus_save_name) {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                save = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
            if ui
                .add_enabled(!name.trim().is_empty(), egui::Button::new("Save"))
                .clicked()
            {
                save = true;
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
        if save && !name.trim().is_empty() {
            actions.push(Request::SaveEqPreset(SaveEqPresetParams {
                name: name.trim().to_string(),
                bands: bands.to_vec(),
            }));
            self.save_name = None;
        } else if cancel {
            self.save_name = None;
        }
    }

    /// What the analyzer shows, and the curve's vertical range.
    fn view_row(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Spectrum")
                    .size(11.0)
                    .color(theme::p().text_dim),
            );
            for v in SpectrumView::ALL {
                if ui
                    .selectable_label(self.view == v, RichText::new(v.label()).size(11.0))
                    .on_hover_text(
                        "Show the audio behind the curve: what goes into the equalizer \
                         (gray) and what comes out (blue). It only runs while a settings \
                         window is open.",
                    )
                    .clicked()
                {
                    self.view = v;
                }
            }
            if self.view != SpectrumView::Off {
                ui.add_space(6.0);
                if self.view == SpectrumView::InputAndOutput {
                    legend(ui, SPECTRUM_IN, "in");
                }
                legend(ui, SPECTRUM_OUT, "out");
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for r in [24.0, 12.0, 6.0] {
                    if ui
                        .selectable_label(
                            self.range_db == r,
                            RichText::new(format!("±{r:.0} dB")).size(11.0),
                        )
                        .on_hover_text("Vertical range of the curve")
                        .clicked()
                    {
                        self.range_db = r;
                    }
                }
                ui.label(RichText::new("Range").size(11.0).color(theme::p().text_dim));
            });
        });
    }

    /// The preset menu: built-in presets, then the user's, which can be
    /// deleted from it. Picking one replaces the bands and switches the
    /// equalizer on.
    fn preset_picker(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        bands: &mut Vec<EqBand>,
        actions: &mut Vec<Request>,
    ) {
        let current = if bands.is_empty() {
            "none".to_string()
        } else {
            state
                .eq_presets
                .iter()
                .find(|p| p.bands == *bands)
                .map_or_else(|| "Custom".to_string(), |p| p.name.clone())
        };
        let mut chosen: Option<EqPreset> = None;
        egui::ComboBox::from_id_salt(("eq_preset", self.target))
            .selected_text(format!("Preset: {current}"))
            .width(230.0)
            .height(420.0)
            .show_ui(ui, |ui| {
                for p in state.eq_presets.iter().filter(|p| p.builtin) {
                    if ui.selectable_label(p.name == current, &p.name).clicked() {
                        chosen = Some(p.clone());
                    }
                }
                let mine: Vec<&EqPreset> = state.eq_presets.iter().filter(|p| !p.builtin).collect();
                ui.separator();
                if mine.is_empty() {
                    ui.label(
                        RichText::new("Your presets appear here once saved.")
                            .size(11.0)
                            .color(theme::p().text_dim),
                    );
                }
                for p in mine {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(p.name == current, &p.name).clicked() {
                            chosen = Some(p.clone());
                        }
                        if ui
                            .small_button("×")
                            .on_hover_text("Delete this preset")
                            .clicked()
                        {
                            actions.push(Request::DeleteEqPreset(NameParams {
                                name: p.name.clone(),
                            }));
                        }
                    });
                }
            })
            .response
            .on_hover_text("Replace the bands with a preset, and switch the equalizer on.");
        if let Some(p) = chosen {
            *bands = p.bands.clone();
            self.selected = None;
            // Send the bands and switch on in one go, and treat the bands as
            // already sent so the throttled path does not send them again.
            actions.push(self.eq_request(EqPatch {
                enabled: Some(Flag::from(true)),
                bands: Some(p.bands),
            }));
            let mut local = Local::new(bands.clone());
            local.unsent = false;
            self.bands = Some(local);
        }
    }

    /// Every band's settings in a table, to type exact values into.
    fn band_table(&mut self, ui: &mut Ui, bands: &mut Vec<EqBand>) {
        if bands.is_empty() {
            return;
        }
        let mut remove = None;
        egui::ScrollArea::vertical()
            .id_salt("eq_bands")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("eq_band_grid")
                    .num_columns(7)
                    .spacing([10.0, 4.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for h in ["", "On", "Type", "Frequency", "Gain", "Width (Q)", ""] {
                            ui.label(RichText::new(h).size(11.0).color(theme::p().text_dim));
                        }
                        ui.end_row();
                        for (i, b) in bands.iter_mut().enumerate() {
                            let selected = self.selected == Some(i);
                            let tag = RichText::new(format!(" {} ", i + 1))
                                .strong()
                                .color(Color32::BLACK)
                                .background_color(band_color(i));
                            if ui
                                .selectable_label(selected, tag)
                                .on_hover_text("Select this band")
                                .clicked()
                            {
                                self.selected = Some(i);
                            }
                            ui.checkbox(&mut b.enabled, "");
                            egui::ComboBox::from_id_salt(("band_kind", i))
                                .selected_text(b.kind.label())
                                .width(100.0)
                                .show_ui(ui, |ui| {
                                    for k in EqBandKind::ALL {
                                        ui.selectable_value(&mut b.kind, k, k.label());
                                    }
                                });
                            // Frequency and width feel right changing by a
                            // proportion rather than a fixed step.
                            let (f_speed, q_speed) = (b.freq_hz * 0.005, b.q * 0.01);
                            ui.add(
                                egui::DragValue::new(&mut b.freq_hz)
                                    .range(EQ_FREQ_MIN_HZ..=EQ_FREQ_MAX_HZ)
                                    .speed(f_speed)
                                    .custom_formatter(|v, _| fmt_freq(v as f32))
                                    .custom_parser(parse_freq),
                            );
                            ui.add_enabled(
                                b.kind.uses_gain(),
                                egui::DragValue::new(&mut b.gain_db)
                                    .range(EQ_GAIN_MIN_DB..=EQ_GAIN_MAX_DB)
                                    .speed(0.1)
                                    .fixed_decimals(1)
                                    .suffix(" dB"),
                            );
                            ui.add(
                                egui::DragValue::new(&mut b.q)
                                    .range(EQ_Q_MIN..=EQ_Q_MAX)
                                    .speed(q_speed)
                                    .fixed_decimals(2),
                            );
                            if ui
                                .small_button("×")
                                .on_hover_text("Remove this band")
                                .clicked()
                            {
                                remove = Some(i);
                            }
                            ui.end_row();
                        }
                    });
            });
        if let Some(i) = remove {
            bands.remove(i);
            self.selected = None;
        }
    }
}

/// A small color swatch and its name.
fn legend(ui: &mut Ui, color: Color32, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().rect_filled(rect, 2, color.gamma_multiply(0.8));
    ui.label(RichText::new(text).size(11.0).color(theme::p().text_dim));
}

/// Somewhere to put a new band where there is not one already: the first of
/// a few likely spots that is furthest from every existing band.
fn free_frequency(bands: &[EqBand]) -> f32 {
    let candidates = [
        1000.0, 250.0, 4000.0, 100.0, 8000.0, 500.0, 2000.0, 60.0, 12000.0,
    ];
    let gap = |f: f32| {
        bands
            .iter()
            .map(|x| (x.freq_hz / f).ln().abs())
            .fold(f32::INFINITY, f32::min)
    };
    let mut best = (candidates[0], gap(candidates[0]));
    for f in candidates.into_iter().skip(1) {
        let g = gap(f);
        // Strictly further only, so ties go to the earlier, likelier spot.
        if g > best.1 {
            best = (f, g);
        }
    }
    best.0
}

/// Round a dragged frequency to what anyone would type: three significant
/// figures.
pub(super) fn round_freq(f: f32) -> f32 {
    let f = f.clamp(EQ_FREQ_MIN_HZ, EQ_FREQ_MAX_HZ);
    let mag = 10f32.powf(f.log10().floor() - 2.0);
    (f / mag).round() * mag
}

/// A frequency as people write it: "80 Hz", "1.50 kHz", "12.0 kHz".
pub(super) fn fmt_freq(f: f32) -> String {
    if f >= 1000.0 {
        let k = f / 1000.0;
        if k >= 10.0 {
            format!("{k:.1} kHz")
        } else {
            format!("{k:.2} kHz")
        }
    } else {
        format!("{f:.0} Hz")
    }
}

/// Accept "1500", "1500 Hz", "1.5k" and "1.5 kHz".
fn parse_freq(s: &str) -> Option<f64> {
    let t = s.trim().to_ascii_lowercase().replace("hz", "");
    let t = t.trim();
    if let Some(k) = t.strip_suffix('k') {
        k.trim().parse::<f64>().ok().map(|v| v * 1000.0)
    } else {
        t.parse::<f64>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies_read_and_write_like_people_expect() {
        assert_eq!(fmt_freq(80.0), "80 Hz");
        assert_eq!(fmt_freq(1500.0), "1.50 kHz");
        assert_eq!(fmt_freq(12000.0), "12.0 kHz");
        assert_eq!(parse_freq("1.5k"), Some(1500.0));
        assert_eq!(parse_freq("1.5 kHz"), Some(1500.0));
        assert_eq!(parse_freq("250 Hz"), Some(250.0));
        assert_eq!(parse_freq("nope"), None);
        assert_eq!(round_freq(1234.5), 1230.0);
        assert_eq!(round_freq(87.64), 87.6);
    }

    #[test]
    fn new_bands_go_somewhere_empty() {
        assert_eq!(free_frequency(&[]), 1000.0);
        let one = [EqBand::new(EqBandKind::Peak, 1000.0, 0.0, 1.0)];
        assert_ne!(free_frequency(&one), 1000.0);
    }
}
