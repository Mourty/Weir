//! One input strip, top to bottom: its caption and name, where its sound
//! comes from (a device, or the applications playing into it) and its pan,
//! the meter and fader with the routing buttons beside them, the level
//! readout, the effect switches, and mute, solo and its menu.

use super::controls::{
    drag_caption, own_color, paint_color_band, system_volume_note, system_volume_text, NAME_SIZE,
};
use super::mixer::Metrics;
use super::{bus_label, App, Key};
use crate::fx_window::FxTarget;
use crate::{theme, widgets};
use egui::{vec2, Frame, RichText, Ui};
use weir_protocol::*;

/// Height of an application's mute button and volume slider row.
const APP_VOLUME_ROW_H: f32 = 14.0;

impl App {
    pub(super) fn strip_view(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        s: &Strip,
        lay: &Metrics,
    ) -> egui::Response {
        let inner_w = lay.strip_w - 12.0;
        let bg = if s.kind == StripKind::Hardware {
            theme::p().strip_bg_hw
        } else {
            theme::p().strip_bg
        };
        let target = StripOrBus::Strip(s.id);
        let frame = Frame::new()
            .fill(bg)
            .corner_radius(8)
            .inner_margin(6)
            .show(ui, |ui| {
                ui.set_width(inner_w);
                ui.set_min_height(lay.strip_h - 12.0);
                ui.vertical(|ui| {
                    let kind_text = match s.kind {
                        StripKind::Hardware => "HARDWARE INPUT",
                        StripKind::Virtual => "VIRTUAL INPUT",
                    };
                    let layout = s.layout.label().to_uppercase();
                    drag_caption(ui, &format!("{kind_text} · {layout}"), target);
                    self.name_editor(ui, target, &s.name);
                    self.strip_source_band(ui, state, s, inner_w);
                    self.strip_fader_row(ui, state, s, lay, inner_w);
                    self.readout_row(ui, inner_w, target, None, |this, ui| {
                        this.strip_readout(ui, state, s);
                    });
                    self.strip_fx_row(ui, s, inner_w);
                    self.strip_buttons(ui, state, s, inner_w);
                });
            });
        paint_color_band(ui, frame.response.rect, s.color.as_deref());
        frame.response
    }

    /// The band between the name and the fader: the device picker, or the
    /// applications playing into the strip, then the pan control. Buses
    /// reserve the same height so all the faders line up.
    fn strip_source_band(&mut self, ui: &mut Ui, state: &FullState, s: &Strip, inner_w: f32) {
        ui.allocate_ui(vec2(inner_w, theme::BAND_H), |ui| {
            ui.set_min_height(theme::BAND_H);
            ui.vertical(|ui| {
                let list_h = theme::BAND_H - 20.0;
                ui.allocate_ui(vec2(inner_w, list_h), |ui| {
                    ui.set_min_height(list_h);
                    match s.kind {
                        StripKind::Hardware => {
                            let target = StripOrBus::Strip(s.id);
                            self.device_picker(ui, state, target, s.device.as_deref());
                        }
                        StripKind::Virtual => self.strip_apps_band(ui, state, s, inner_w, list_h),
                    }
                });
                let mut pan = self.value(Key::StripPan(s.id), s.pan);
                if widgets::pan_slider(ui, &mut pan, inner_w).changed() {
                    self.set_value(Key::StripPan(s.id), pan);
                }
            });
        });
    }

    /// A virtual strip's source band: a note if the system has its device
    /// turned down, then the applications playing into it, clipped to what
    /// is left.
    fn strip_apps_band(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        s: &Strip,
        inner_w: f32,
        mut list_h: f32,
    ) {
        if let Some(v) = state
            .system_volumes
            .strips
            .get(&s.id)
            .filter(|v| v.is_reducing())
        {
            let explain = format!(
                "The system's volume control has \"{} (Weir)\" at {}, so everything playing \
                 into it is turned down before it reaches this fader.",
                s.name,
                system_volume_text(v)
            );
            let r = system_volume_note(ui, v, &explain);
            list_h -= r.rect.height() + ui.spacing().item_spacing.y;
        }
        let list_h = list_h.max(12.0);
        let (rect, _) = ui.allocate_exact_size(vec2(inner_w, list_h), egui::Sense::hover());
        let mut band = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
        band.set_clip_rect(rect);
        self.app_list(&mut band, state, s, inner_w, list_h);
    }

    /// The meter, the fader, and the routing buttons in one or more columns
    /// beside them. While a bus's mix is shown, the meter reads how loud the
    /// strip is in that mix and the fader is its level there; a strip not
    /// sent to that bus is grayed out.
    fn strip_fader_row(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        s: &Strip,
        lay: &Metrics,
        inner_w: f32,
    ) {
        let n_ch = s.layout.channel_count();
        let meter_w = widgets::meter_width(n_ch);
        let routes_w = lay.route_cols as f32 * (theme::ROUTE_BTN_W + theme::ROUTE_GAP);
        let row_w = meter_w + 4.0 + theme::FADER_W + 4.0 + routes_w;
        ui.horizontal(|ui| {
            ui.add_space(((inner_w - row_w) / 2.0).max(0.0));
            let (lv, pk) = self
                .meters
                .entry(StripOrBus::Strip(s.id))
                .or_default()
                .shown(n_ch);
            let view = self.mix_view.and_then(|id| state.mixer.bus(id));
            let in_view = view.is_none_or(|b| s.routes.contains(&b.id));
            let shift = view.map_or(0.0, |b| s.send_db(b.id));
            let (lv, pk) = if in_view {
                let add = |v: &[f32]| -> Vec<f32> {
                    v.iter().map(|x| (x + shift).max(METER_FLOOR_DB)).collect()
                };
                (add(&lv), add(&pk))
            } else {
                (vec![METER_FLOOR_DB; n_ch], vec![METER_FLOOR_DB; n_ch])
            };
            widgets::meter(ui, &lv, &pk, lay.fader_h);
            ui.add_space(4.0);
            match view {
                None => {
                    let key = Key::Gain(StripOrBus::Strip(s.id));
                    let mut gain = self.value(key, s.gain_db);
                    let fill = own_color(s.color.as_deref()).unwrap_or(theme::p().fader);
                    if widgets::fader(ui, &mut gain, lay.fader_h, fill).changed() {
                        self.set_value(key, gain);
                    }
                }
                Some(b) => {
                    let key = Key::StripSend(s.id, b.id);
                    let mut level = self.value(key, s.send_db(b.id));
                    ui.scope(|ui| {
                        if !in_view {
                            ui.disable();
                            ui.set_opacity(0.35);
                        }
                        let color = theme::bus_color(b);
                        if widgets::fader(ui, &mut level, lay.fader_h, color).changed() {
                            self.set_value(key, level);
                        }
                    });
                }
            }
            ui.add_space(4.0);
            self.route_buttons(ui, state, s, lay);
        });
    }

    /// The level under the fader, to type into: the strip's own, or its
    /// level in the mix being shown.
    fn strip_readout(&mut self, ui: &mut Ui, state: &FullState, s: &Strip) {
        match self.mix_view.and_then(|id| state.mixer.bus(id)) {
            None => {
                let key = Key::Gain(StripOrBus::Strip(s.id));
                let mut gain = self.value(key, s.gain_db);
                if ui.add(db_field(&mut gain)).changed() {
                    self.set_value(key, gain);
                }
            }
            Some(b) if s.routes.contains(&b.id) => {
                let key = Key::StripSend(s.id, b.id);
                let mut level = self.value(key, s.send_db(b.id));
                ui.visuals_mut().override_text_color = Some(theme::bus_color(b));
                let r = ui
                    .add(db_field(&mut level).prefix(format!("{} ", bus_label(&state.mixer, b))))
                    .on_hover_text(format!(
                        "{}'s level in {}, on top of its fader",
                        s.name, b.name
                    ));
                if r.changed() {
                    self.set_value(key, level);
                }
            }
            Some(b) => {
                ui.label(
                    RichText::new(format!("not in {}", bus_label(&state.mixer, b)))
                        .size(12.0)
                        .color(theme::p().text_dim),
                );
            }
        }
    }

    /// Noise suppression, gate and equalizer switches, and the button that
    /// opens their settings. Right-clicking a switch opens them too.
    fn strip_fx_row(&mut self, ui: &mut Ui, s: &Strip, inner_w: f32) {
        let gate_closed = s.gate.enabled
            && self
                .gate_views
                .get(&s.id)
                .is_some_and(|g| g.reduction <= -6.0);
        ui.horizontal(|ui| {
            let gap = ui.spacing().item_spacing.x;
            let open_w = 24.0;
            let w = ((inner_w - open_w - 3.0 * gap) / 3.0).floor();
            let size = vec2(w, theme::FX_ROW_H);
            let mut open = false;
            let switch = |patch: StripPatch| Request::SetStrip(StripPatch { id: s.id, ..patch });

            let r = widgets::toggle(ui, "NS", s.denoise.enabled, theme::p().ns_on, size)
                .on_hover_text(
                    "Noise suppression: removes background noise from a voice.\n\
                     Right-click for settings.",
                );
            if r.clicked() {
                self.actions.push(switch(StripPatch {
                    denoise: Some(DenoisePatch {
                        enabled: Some(Flag::from(!s.denoise.enabled)),
                        amount: None,
                    }),
                    ..Default::default()
                }));
            }
            open |= r.secondary_clicked();

            // A closed gate dims its button, so you can see it working.
            let gate_color = if gate_closed {
                theme::p().gate_on.gamma_multiply(0.45)
            } else {
                theme::p().gate_on
            };
            let gate_tip = if s.gate.enabled {
                "Noise gate: bright while open, dim while closed.\nRight-click for settings."
            } else {
                "Noise gate: silences the strip while it is quiet.\nRight-click for settings."
            };
            let r = widgets::toggle(ui, "Gate", s.gate.enabled, gate_color, size)
                .on_hover_text(gate_tip);
            if r.clicked() {
                self.actions.push(switch(StripPatch {
                    gate: Some(GatePatch {
                        enabled: Some(Flag::from(!s.gate.enabled)),
                        ..Default::default()
                    }),
                    ..Default::default()
                }));
            }
            open |= r.secondary_clicked();

            let r = widgets::toggle(ui, "EQ", s.eq.enabled, theme::p().eq_on, size).on_hover_text(
                "Equalizer on or off.\nRight-click, or use the button beside it, to edit.",
            );
            if r.clicked() {
                self.actions.push(switch(StripPatch {
                    eq: Some(EqPatch {
                        enabled: Some(Flag::from(!s.eq.enabled)),
                        bands: None,
                    }),
                    ..Default::default()
                }));
            }
            open |= r.secondary_clicked();

            // There is no room for a fifth switch, so the button that opens
            // the window doubles as the compressor's light.
            let comp = &s.compressor;
            let mut gear = egui::Button::new("⚙").min_size(vec2(open_w, theme::FX_ROW_H));
            let mut tip = "Open the settings: noise suppression, gate, equalizer, \
                           compressor, fader, ducking and upmix"
                .to_string();
            if comp.enabled {
                let working = self.comp_views.get(&s.id).map_or(0.0, |v| -v.reduction);
                gear = gear.fill(if working > 1.0 {
                    theme::p().comp_on
                } else {
                    theme::p().comp_on.gamma_multiply(0.55)
                });
                tip.push_str(&format!(
                    "\n\nCompressor on ({}:1 above {:.0} dB), turning down {:.1} dB",
                    (comp.ratio * 10.0).round() / 10.0,
                    comp.threshold_db,
                    working
                ));
            }
            if ui.add(gear).on_hover_text(tip).clicked() {
                open = true;
            }
            if open {
                self.open_fx(FxTarget::Strip(s.id));
            }
        });
    }

    /// Mute, solo, and the strip's menu.
    fn strip_buttons(&mut self, ui: &mut Ui, state: &FullState, s: &Strip, inner_w: f32) {
        ui.horizontal(|ui| {
            let w = (inner_w - 8.0 - 26.0) / 2.0;
            if widgets::toggle(ui, "M", s.mute, theme::p().mute_on, vec2(w, 22.0))
                .on_hover_text("Mute")
                .clicked()
            {
                self.actions.push(Request::SetStrip(StripPatch {
                    id: s.id,
                    mute: Some(Flag::from(!s.mute)),
                    ..Default::default()
                }));
            }
            let solo_tip = match state.settings.solo {
                SoloMode::Cue(b) => match state.mixer.bus(b) {
                    Some(b) => format!(
                        "Solo: {} plays only the soloed strips; every other mix carries on \
                         as it was",
                        b.name
                    ),
                    None => "Solo".to_string(),
                },
                SoloMode::Exclusive => "Solo: silence every strip that is not soloed".to_string(),
            };
            if widgets::toggle(ui, "S", s.solo, theme::p().solo_on, vec2(w, 22.0))
                .on_hover_text(solo_tip)
                .clicked()
            {
                self.actions.push(Request::SetStrip(StripPatch {
                    id: s.id,
                    solo: Some(Flag::from(!s.solo)),
                    ..Default::default()
                }));
            }
            ui.menu_button("…", |ui| self.strip_menu(ui, state, s));
        });
    }

    /// The strip's "…" menu.
    fn strip_menu(&mut self, ui: &mut Ui, state: &FullState, s: &Strip) {
        let target = StripOrBus::Strip(s.id);
        if ui
            .button("Settings…")
            .on_hover_text("Effects, equalizer, fader and upmix, all in one window")
            .clicked()
        {
            self.open_fx(target);
            ui.close();
        }
        self.layout_menu(ui, target, &s.layout);
        let strips = &state.mixer.strips;
        let index = strips.iter().position(|x| x.id == s.id).unwrap_or(0);
        self.arrange_menu(ui, target, index, strips.len(), s.color.as_deref());
        if ui.button("Reset fader to 0 dB").clicked() {
            self.set_value(Key::Gain(target), 0.0);
            ui.close();
        }
        if ui.button("Center pan").clicked() {
            self.set_value(Key::StripPan(s.id), 0.0);
            ui.close();
        }
        ui.separator();
        if ui
            .button(RichText::new("Remove strip").color(theme::p().meter_red))
            .clicked()
        {
            self.confirm_remove = Some((target, s.name.clone()));
            ui.close();
        }
    }

    /// The routing buttons for one strip, stacked vertically beside the fader
    /// and wrapping into more columns when the window is too short.
    fn route_buttons(&mut self, ui: &mut Ui, state: &FullState, s: &Strip, lay: &Metrics) {
        for column in state.mixer.buses.chunks(lay.per_col.max(1)) {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = theme::ROUTE_GAP;
                for b in column {
                    self.route_button(ui, state, s, b);
                }
            });
        }
    }

    /// The button that sends strip `s` to bus `b`. It shows the strip's
    /// level in that mix when it is not 0 dB, and how far ducking is
    /// turning it down while it does; right-clicking sets the level.
    fn route_button(&mut self, ui: &mut Ui, state: &FullState, s: &Strip, b: &Bus) {
        let on = s.routes.contains(&b.id);
        let key = Key::StripSend(s.id, b.id);
        let level = self.value(key, s.send_db(b.id));
        let ducked = self
            .duck_views
            .get(&s.id)
            .copied()
            .filter(|db| on && *db < -0.5 && s.ducking.applies_to(b.id));
        let note = match ducked {
            Some(db) => Some(widgets::RouteNote::Ducked(-db)),
            None => (on && level.abs() >= 0.05)
                .then(|| widgets::RouteNote::Level(widgets::short_db(level))),
        };
        let ring = if ducked.is_some() {
            Some(theme::p().duck)
        } else if self.mix_view == Some(b.id) {
            Some(theme::p().text)
        } else {
            None
        };
        let r = widgets::route_button(
            ui,
            &bus_label(&state.mixer, b),
            note,
            on,
            theme::bus_color(b),
            vec2(theme::ROUTE_BTN_W, theme::ROUTE_BTN_H),
            ring,
        );
        let mut hover = if on {
            format!(
                "Sent to {} at {} on top of the fader.\nClick to stop, right-click to set \
                 the level.",
                b.name,
                widgets::short_db(level)
            )
        } else {
            format!("Send to {}.\nRight-click to set the level.", b.name)
        };
        if let Some(db) = ducked {
            hover.push_str(&format!("\n\nDucked: turned down {:.0} dB right now.", -db));
        }
        let r = r.on_hover_text(hover);
        if r.clicked() {
            self.actions.push(Request::SetRoute(RouteParams {
                strip: s.id,
                bus: b.id,
                enabled: Some(Flag::from(!on)),
                level_db: None,
                level_delta_db: None,
            }));
        }
        r.context_menu(|ui| {
            ui.label(RichText::new(format!("{} in {}", s.name, b.name)).strong());
            let mut v = self.value(key, s.send_db(b.id));
            ui.horizontal(|ui| {
                ui.label("Level");
                if ui.add(db_field(&mut v)).changed() {
                    self.set_value(key, v);
                }
            });
            if ui.button("Back to 0 dB").clicked() {
                self.set_value(key, 0.0);
                ui.close();
            }
            if ui
                .button(format!("Show {}'s mix on the faders", b.name))
                .clicked()
            {
                self.mix_view = Some(b.id);
                ui.close();
            }
        });
    }

    /// The applications playing into a virtual strip, each with its own
    /// volume. This is the same volume the system volume applet shows.
    fn app_list(&mut self, ui: &mut Ui, state: &FullState, s: &Strip, width: f32, height: f32) {
        let apps: Vec<&AppStream> = state
            .apps
            .iter()
            .filter(|a| a.strip == Some(s.id))
            .collect();
        if apps.is_empty() {
            ui.label(
                RichText::new("no apps playing here")
                    .size(10.0)
                    .color(theme::p().text_dim),
            )
            .on_hover_text(format!(
                "Applications can choose \"{} (Weir)\" as their output device.",
                s.name
            ));
            return;
        }
        let others: Vec<(StripId, String)> = state
            .mixer
            .strips
            .iter()
            .filter(|x| x.kind == StripKind::Virtual && x.id != s.id)
            .map(|x| (x.id, x.name.clone()))
            .collect();
        // Size the viewport to whole entries, so the resting view never ends
        // on half an application. max_height alone will not do it: with
        // auto_shrink off the area takes the space it is offered, so the
        // space itself has to be the right size.
        let gap = ui.spacing().item_spacing.y;
        let pitch = self.app_pitch.max(1.0);
        let fits = (((height + gap) / pitch).floor() as usize).max(1);
        let whole = (fits as f32 * pitch - gap).clamp(pitch - gap, height);
        let (view, _) = ui.allocate_exact_size(vec2(width, whole), egui::Sense::hover());
        let mut sui = ui.new_child(egui::UiBuilder::new().max_rect(view).layout(*ui.layout()));
        sui.set_clip_rect(view);
        let measured = egui::ScrollArea::vertical()
            .id_salt(("apps", s.id))
            // See the note in bus_sources: the default minimum would push
            // part of the viewport outside the clip.
            .min_scrolled_height(whole)
            .auto_shrink([false, false])
            .show(&mut sui, |ui| {
                // Tighten the rows: this band is short, and the default
                // button padding costs a whole extra entry.
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.spacing_mut().button_padding.y = 1.0;
                let mut pitch = 0.0f32;
                for a in apps {
                    let entry_top = ui.cursor().top();
                    self.app_entry(ui, a, &others, width);
                    pitch = pitch.max(ui.cursor().top() - entry_top);
                }
                pitch
            })
            .inner;
        // Feed the measurement back for the next frame. One frame at a
        // slightly wrong size is invisible; a permanently half-drawn entry
        // is not.
        if measured > 1.0 && (measured - self.app_pitch).abs() > 0.5 {
            self.app_pitch = measured;
            ui.ctx().request_repaint();
        }
    }

    /// One application: its name, which opens a menu to move it to one of
    /// the `others`, and its mute and volume when those are shown.
    fn app_entry(&mut self, ui: &mut Ui, a: &AppStream, others: &[(StripId, String)], width: f32) {
        ui.horizontal(|ui| {
            ui.menu_button(RichText::new(&a.name).size(NAME_SIZE), |ui| {
                ui.label(RichText::new("Move to").color(theme::p().text_dim));
                for (id, name) in others {
                    if ui.button(name).clicked() {
                        self.actions.push(Request::MoveApp(MoveAppParams {
                            app: a.id,
                            strip: *id,
                        }));
                        ui.close();
                    }
                }
                if others.is_empty() {
                    ui.label(RichText::new("no other virtual strips").color(theme::p().text_dim));
                }
            })
            .response
            .on_hover_text(
                a.media_name
                    .clone()
                    .unwrap_or_else(|| "Move this application".into()),
            );
        });
        if !self.prefs.show_app_volume {
            return;
        }
        ui.horizontal(|ui| {
            if widgets::toggle(
                ui,
                "M",
                a.mute,
                theme::p().mute_on,
                vec2(18.0, APP_VOLUME_ROW_H),
            )
            .on_hover_text("Mute this application")
            .clicked()
            {
                self.actions.push(Request::SetAppVolume(AppVolumeParams {
                    app: a.id,
                    volume_db: None,
                    volume_delta_db: None,
                    mute: Some(Flag::from(!a.mute)),
                }));
            }
            let current = a.volume_db.unwrap_or(0.0);
            let mut db = self.value(Key::AppVolume(a.id), current);
            let slider_w = (width - 26.0).max(30.0);
            if widgets::mini_volume(ui, &mut db, slider_w, a.mute).changed() {
                self.set_value(Key::AppVolume(a.id), db);
            }
        });
    }
}

/// A level in dB to drag or type, from silence to the fader's top.
pub(super) fn db_field(value: &mut f32) -> egui::DragValue<'_> {
    egui::DragValue::new(value)
        .speed(0.1)
        .range(GAIN_MIN_DB..=GAIN_MAX_DB)
        .fixed_decimals(1)
        .suffix(" dB")
}
