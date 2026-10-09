//! One output bus, top to bottom: its caption and name, its device (or,
//! for a virtual bus, a note that apps can record from it), the button to
//! show its mix on the strip faders, the strips it receives from, the
//! limiter's ceiling handle beside its meter and fader, the level readout,
//! its equalizer switch, and mute, mono and its menu.

use super::controls::{
    caption, drag_caption, own_color, paint_color_band, system_volume_note, system_volume_text,
    NAME_SIZE,
};
use super::mixer::Metrics;
use super::strip::db_field;
use super::{bus_label, App, Key};
use crate::fx_window::FxTarget;
use crate::hotkeys::simple::{Action, Fx, Simple};
use crate::prefs::BusSources;
use crate::{theme, widgets};
use egui::{vec2, Frame, RichText, Ui};
use weir_protocol::*;

impl App {
    pub(super) fn bus_view(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        b: &Bus,
        lay: &Metrics,
    ) -> egui::Response {
        let inner_w = lay.bus_w - 12.0;
        let target = StripOrBus::Bus(b.id);
        let frame = Frame::new()
            .fill(theme::p().bus_bg)
            .corner_radius(8)
            .inner_margin(6)
            .show(ui, |ui| {
                ui.set_width(inner_w);
                ui.set_min_height(lay.strip_h - 12.0);
                ui.vertical(|ui| {
                    let kind_text = match b.kind {
                        BusKind::Hardware => "HW OUT",
                        BusKind::Virtual => "VIRTUAL OUT",
                    };
                    let title = format!(
                        "{} {kind_text} · {}",
                        bus_label(&state.mixer, b),
                        b.layout.label().to_uppercase()
                    );
                    drag_caption(ui, &title, target);
                    self.name_editor(ui, state, target, &b.name);
                    self.bus_band(ui, state, b, inner_w);
                    self.bus_fader_row(ui, state, b, lay, inner_w);
                    self.readout_row(ui, inner_w, target, Some(b), |this, ui| {
                        let key = Key::Gain(target);
                        let mut gain = this.value(key, b.gain_db);
                        if ui.add(db_field(&mut gain)).changed() {
                            this.set_value(key, gain);
                        }
                    });
                    self.bus_fx_row(ui, b, inner_w);
                    self.bus_buttons(ui, state, b, inner_w);
                });
            });
        // Outline the bus whose mix the strip faders are showing. Painted
        // over the frame rather than as its border, which would take up
        // room and push this bus's fader out of line with the others.
        if self.mix_view == Some(b.id) {
            ui.painter().rect_stroke(
                frame.response.rect,
                8,
                egui::Stroke::new(2.0_f32, theme::bus_color(b)),
                egui::StrokeKind::Inside,
            );
        }
        paint_color_band(ui, frame.response.rect, b.color.as_deref());
        frame.response
    }

    /// The band between the name and the fader, as tall as a strip's: the
    /// device, the "show mix" button, and the strips feeding the bus.
    fn bus_band(&mut self, ui: &mut Ui, state: &FullState, b: &Bus, inner_w: f32) {
        ui.allocate_ui(vec2(inner_w, theme::BAND_H), |ui| {
            ui.set_min_height(theme::BAND_H);
            ui.vertical(|ui| {
                match b.kind {
                    BusKind::Hardware => {
                        let target = StripOrBus::Bus(b.id);
                        self.device_picker(ui, state, target, b.device.as_deref());
                    }
                    BusKind::Virtual => virtual_bus_note(ui, state, b),
                }
                let showing = self.mix_view == Some(b.id);
                if widgets::toggle(
                    ui,
                    if showing {
                        "Showing this mix"
                    } else {
                        "Show mix on faders"
                    },
                    showing,
                    theme::bus_color(b),
                    vec2(inner_w, 20.0),
                )
                .on_hover_text("Make the strip faders set how loud each strip is in this bus")
                .clicked()
                {
                    self.mix_view = if showing { None } else { Some(b.id) };
                }
                // Give the source list an exact rectangle and clip to it. A
                // long list then scrolls inside its own box instead of
                // pushing this bus's fader out of line with every other
                // column.
                let rest = ui.available_height().max(14.0);
                let (rect, _) = ui.allocate_exact_size(vec2(inner_w, rest), egui::Sense::hover());
                let mut band =
                    ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
                band.set_clip_rect(rect);
                self.bus_sources(&mut band, state, b, inner_w);
            });
        });
    }

    /// The limiter's ceiling handle, the meter with the ceiling marked on
    /// it, and the fader. The handle sits left of the meter, out of the way
    /// of the fader, and can go anywhere on the meter's scale.
    fn bus_fader_row(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        b: &Bus,
        lay: &Metrics,
        inner_w: f32,
    ) {
        let n_ch = b.layout.channel_count();
        let row_w = theme::CEILING_W + 4.0 + widgets::meter_width(n_ch) + 8.0 + theme::FADER_W;
        ui.horizontal(|ui| {
            ui.add_space(((inner_w - row_w) / 2.0).max(0.0));
            let (lv, pk) = self
                .meters
                .entry(StripOrBus::Bus(b.id))
                .or_default()
                .shown(n_ch);
            let mut ceiling = self.value(Key::BusCeiling(b.id), b.limiter.ceiling_db);
            let reduction = self.reductions.get(&b.id).copied().unwrap_or(0.0);
            let handle = widgets::ceiling_handle(
                ui,
                &mut ceiling,
                b.limiter.enabled,
                reduction,
                lay.fader_h,
            );
            if handle.changed() {
                self.set_value(Key::BusCeiling(b.id), ceiling);
            }
            widgets::field_context_menu(&handle, |ui| self.limiter_menu(ui, b));
            let meter = widgets::meter(ui, &lv, &pk, lay.fader_h);
            if b.limiter.enabled {
                widgets::ceiling_line(ui, meter.rect, ceiling);
            }
            ui.add_space(4.0);
            let key = Key::Gain(StripOrBus::Bus(b.id));
            let mut gain = self.value(key, b.gain_db);
            let fill = own_color(b.color.as_deref()).unwrap_or(theme::p().fader);
            let r = widgets::fader(ui, &mut gain, lay.fader_h, fill);
            if r.changed() {
                self.set_value(key, gain);
            }
            r.context_menu(|ui| {
                ui.label(RichText::new(format!("{}: volume", b.name)).strong());
                let start = Simple::new(StripOrBus::Bus(b.id), state);
                self.hotkey_items(ui, state, start.with(Action::VolumeBy));
            });
        });
    }

    /// The equalizer switch of a bus and the button that opens its settings.
    fn bus_fx_row(&mut self, ui: &mut Ui, b: &Bus, inner_w: f32) {
        ui.horizontal(|ui| {
            let gap = ui.spacing().item_spacing.x;
            let open_w = 24.0;
            let size = vec2(inner_w - open_w - gap, theme::FX_ROW_H);
            let r = widgets::toggle(ui, "EQ", b.eq.enabled, theme::p().eq_on, size).on_hover_text(
                "Equalizer on or off.\nRight-click, or use the button beside it, to edit it.",
            );
            if r.clicked() {
                self.actions.push(Request::SetBus(BusPatch {
                    id: b.id,
                    eq: Some(EqPatch {
                        enabled: Some(Flag::from(!b.eq.enabled)),
                        bands: None,
                    }),
                    ..Default::default()
                }));
            }
            let clicked = ui
                .add(egui::Button::new("⚙").min_size(vec2(open_w, theme::FX_ROW_H)))
                .on_hover_text("Open the settings: equalizer, downmix, fader and limiter")
                .clicked();
            if clicked || r.secondary_clicked() {
                self.open_fx(FxTarget::Bus(b.id));
            }
        });
    }

    /// Mute, mono, and the bus's menu.
    fn bus_buttons(&mut self, ui: &mut Ui, state: &FullState, b: &Bus, inner_w: f32) {
        ui.horizontal(|ui| {
            let w = (inner_w - 8.0 - 26.0) / 2.0;
            let target = StripOrBus::Bus(b.id);
            let r = widgets::toggle(ui, "M", b.mute, theme::p().mute_on, vec2(w, 22.0))
                .on_hover_text("Mute\nRight-click to add a hotkey");
            if r.clicked() {
                self.actions.push(Request::SetBus(BusPatch {
                    id: b.id,
                    mute: Some(Flag::from(!b.mute)),
                    ..Default::default()
                }));
            }
            r.context_menu(|ui| {
                ui.label(RichText::new(format!("{}: mute", b.name)).strong());
                self.hotkey_items(ui, state, Simple::new(target, state).with(Action::Mute));
            });
            let r = widgets::toggle(ui, "mono", b.mono, theme::p().mono_on, vec2(w, 22.0))
                .on_hover_text("Fold to mono\nRight-click to add a hotkey");
            if r.clicked() {
                self.actions.push(Request::SetBus(BusPatch {
                    id: b.id,
                    mono: Some(Flag::from(!b.mono)),
                    ..Default::default()
                }));
            }
            r.context_menu(|ui| {
                ui.label(RichText::new(format!("{}: mono", b.name)).strong());
                let start = Simple {
                    fx: Fx::Mono,
                    ..Simple::new(target, state)
                };
                self.hotkey_items(ui, state, start.with(Action::Effect));
            });
            widgets::field_menu(ui, "…", |ui| self.bus_menu(ui, state, b));
        });
    }

    /// The bus's "…" menu.
    fn bus_menu(&mut self, ui: &mut Ui, state: &FullState, b: &Bus) {
        let target = StripOrBus::Bus(b.id);
        if ui
            .button("Settings…")
            .on_hover_text("Equalizer, downmix, fader and limiter, all in one window")
            .clicked()
        {
            self.open_fx(target);
            ui.close();
        }
        self.layout_menu(ui, target, &b.layout);
        let buses = &state.mixer.buses;
        let index = buses.iter().position(|x| x.id == b.id).unwrap_or(0);
        self.arrange_menu(ui, target, index, buses.len(), b.color.as_deref());
        ui.menu_button("Hotkeys", |ui| {
            self.hotkey_items(ui, state, Simple::new(target, state).with(Action::Mute));
        });
        if ui.button("Reset fader to 0 dB").clicked() {
            self.set_value(Key::Gain(target), 0.0);
            ui.close();
        }
        ui.separator();
        self.limiter_menu(ui, b);
        ui.separator();
        self.delay_menu(ui, b);
        ui.separator();
        if ui
            .button(RichText::new("Remove bus").color(theme::p().meter_red))
            .clicked()
        {
            self.confirm_remove = Some((target, b.name.clone()));
            ui.close();
        }
    }

    /// Safety limiter settings for a bus, for its menus.
    pub(super) fn limiter_menu(&mut self, ui: &mut Ui, b: &Bus) {
        let mut on = b.limiter.enabled;
        if ui
            .checkbox(&mut on, "Safety limiter")
            .on_hover_text("Keeps this bus from ever going over its ceiling")
            .changed()
        {
            self.actions.push(Request::SetBus(BusPatch {
                id: b.id,
                limiter: Some(LimiterPatch {
                    enabled: Some(Flag::from(on)),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }
        if !b.limiter.enabled {
            ui.label(
                RichText::new("Off: nothing stops this bus going over 0 dB.")
                    .size(11.0)
                    .color(theme::p().text_dim),
            );
            return;
        }
        egui::Grid::new(("limiter", b.id))
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Ceiling");
                let key = Key::BusCeiling(b.id);
                let mut ceiling = self.value(key, b.limiter.ceiling_db);
                if ui
                    .add(db_field(&mut ceiling))
                    .on_hover_text("The level this bus never goes over")
                    .changed()
                {
                    self.set_value(key, ceiling);
                }
                ui.end_row();
                ui.label("Release");
                let key = Key::BusRelease(b.id);
                let mut release = self.value(key, b.limiter.release_ms);
                if ui
                    .add(
                        egui::DragValue::new(&mut release)
                            .speed(2.0)
                            .range(10.0..=2000.0)
                            .fixed_decimals(0)
                            .suffix(" ms"),
                    )
                    .on_hover_text(
                        "How long the bus takes to come back up after the limiter turned it down",
                    )
                    .changed()
                {
                    self.set_value(key, release);
                }
                ui.end_row();
            });
        ui.label(
            RichText::new("Drag the handle left of the meter to move the ceiling.")
                .size(11.0)
                .color(theme::p().text_dim),
        );
    }

    /// The delay of a bus, for its menus: holds its output back, so it can
    /// be lined up with one that plays later, such as a Bluetooth speaker.
    pub(super) fn delay_menu(&mut self, ui: &mut Ui, b: &Bus) {
        egui::Grid::new(("delay", b.id))
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Delay");
                let key = Key::BusDelay(b.id);
                let mut delay = self.value(key, b.delay_ms);
                if ui
                    .add(
                        egui::DragValue::new(&mut delay)
                            .speed(1.0)
                            .range(0.0..=BUS_DELAY_MAX_MS)
                            .fixed_decimals(0)
                            .suffix(" ms"),
                    )
                    .on_hover_text(
                        "Holds this bus's sound back by this long, to line it up with a bus \
                         that plays later, such as a Bluetooth speaker",
                    )
                    .changed()
                {
                    self.set_value(key, delay);
                }
                ui.end_row();
            });
    }

    /// The list of strips feeding a bus, drawn according to the preference.
    /// Whatever is not shown is still reachable from the tooltip.
    fn bus_sources(&mut self, ui: &mut Ui, state: &FullState, b: &Bus, width: f32) {
        if self.prefs.bus_sources == BusSources::Hidden {
            return;
        }
        let feeders: Vec<&Strip> = state
            .mixer
            .strips
            .iter()
            .filter(|s| s.routes.contains(&b.id))
            .collect();
        caption(ui, "receives from");
        if feeders.is_empty() {
            ui.label(
                RichText::new("nothing")
                    .size(10.0)
                    .color(theme::p().text_dim),
            )
            .on_hover_text("Nothing is routed to this bus.");
            return;
        }
        let any_solo = state.mixer.any_solo();
        let name = |s: &Strip| {
            let color = if s.mute || (any_solo && !s.solo) {
                theme::p().text_dim
            } else {
                theme::p().text
            };
            egui::Label::new(
                RichText::new(format!("• {}", s.name))
                    .size(NAME_SIZE)
                    .color(color),
            )
            .truncate()
        };
        // The caller handed over an exact rectangle, so what is left after
        // the caption is precisely what the list may use. Measure a real row
        // rather than guessing: the band is clipped, so a row that does not
        // fit is not merely tight, it is cut in half.
        let height = ui.available_height().max(12.0);
        let row_h = ui
            .painter()
            .layout_no_wrap(
                "Ag".to_owned(),
                egui::FontId::proportional(NAME_SIZE),
                theme::p().text,
            )
            .size()
            .y;
        let gap = ui.spacing().item_spacing.y;
        let pitch = (row_h + gap).max(1.0);
        // Rows that fit whole. The trailing gap is not needed by the last one.
        let fits = (((height + gap) / pitch).floor() as usize).max(1);
        if self.prefs.bus_sources == BusSources::Full {
            // Size the viewport to whole rows so the resting view never ends
            // on a half-drawn name. max_height alone does not do it:
            // auto_shrink(false) makes the area take the space it is
            // offered, so the space itself has to be the right size.
            let whole = (fits as f32 * pitch - gap).clamp(row_h, height);
            let (view, _) = ui.allocate_exact_size(vec2(width, whole), egui::Sense::hover());
            let mut sui = ui.new_child(egui::UiBuilder::new().max_rect(view).layout(*ui.layout()));
            sui.set_clip_rect(view);
            egui::ScrollArea::vertical()
                .id_salt(("bus_sources", b.id))
                // Without this the area silently grows to egui's 64 px
                // default minimum for a scrolling area. The extra height
                // lands outside the clip, so the last rows can never be
                // scrolled into view.
                .min_scrolled_height(whole)
                .auto_shrink([false, false])
                .show(&mut sui, |ui| {
                    for s in &feeders {
                        ui.add(name(s));
                    }
                });
            return;
        }
        // When everything fits, show everything. When it does not, give up
        // one row to say how many are hidden.
        let shown = if feeders.len() <= fits {
            feeders.len()
        } else {
            fits.saturating_sub(1)
        };
        let inner = ui.allocate_ui(vec2(width, height), |ui| {
            ui.set_min_height(height);
            ui.vertical(|ui| {
                for s in feeders.iter().take(shown) {
                    ui.add(name(s));
                }
                if feeders.len() > shown {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("+{} more", feeders.len() - shown))
                                .size(NAME_SIZE)
                                .color(theme::p().accent),
                        )
                        .sense(egui::Sense::hover()),
                    );
                }
            });
        });
        // Hovering anywhere in the band, not just on a label, shows the
        // complete list. The labels themselves do not sense hovers, so ask
        // for an explicit region over the whole band.
        let names: Vec<String> = feeders.iter().map(|s| format!("• {}", s.name)).collect();
        ui.interact(
            inner.response.rect,
            ui.id().with(("bus_sources", b.id)),
            egui::Sense::hover(),
        )
        .on_hover_text(format!("Receives from:\n{}", names.join("\n")));
    }
}

/// What a virtual bus has in place of a device: a note that applications
/// can record from it, and whether the system has it turned down.
fn virtual_bus_note(ui: &mut Ui, state: &FullState, b: &Bus) {
    ui.label(
        RichText::new("virtual microphone")
            .size(10.0)
            .color(theme::p().text_dim),
    )
    .on_hover_text(format!(
        "Applications can capture from \"{}\" as if it were a microphone.",
        device_description(&b.name)
    ));
    if let Some(v) = state
        .system_volumes
        .buses
        .get(&b.id)
        .filter(|v| v.is_reducing())
    {
        let explain = format!(
            "The system's volume control has \"{}\" at {}, so whatever records it \
             hears it that much quieter than this bus's meter shows.",
            device_description(&b.name),
            system_volume_text(v)
        );
        system_volume_note(ui, v, &explain);
    }
}
