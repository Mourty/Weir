//! What strips and buses both have: the caption to drag them by, the name
//! field with the external effects badge beside it, the device picker, the
//! layout and arrange menus, the band of their own color, the row of lights
//! under the fader, and the note shown when the system's volume control is
//! turning one of them down.

use super::dialogs::RenameConfirm;
use super::mixer::move_request;
use super::App;
use crate::effects;
use crate::fx_window::{FxTarget, Section};
use crate::patch::CommonPatch;
use crate::{theme, widgets};
use egui::{vec2, Align, Color32, Layout, RichText, TextStyle, Ui};
use weir_protocol::*;

/// Text size of the strip names listed under a bus, and of the application
/// names on a strip. Both lists measure a real row rather than assuming one,
/// because their boxes are clipped: a row that does not fit is not merely
/// tight, it is cut in half.
pub(super) const NAME_SIZE: f32 = 10.5;

impl App {
    /// The name, edited in place. The new name is sent when the field loses
    /// focus; an empty one puts the old name back.
    ///
    /// With external effects on, a badge beside it says at a glance whether
    /// they are connected, and a new name waits for a yes: it renames their
    /// devices too, which a program may know them by.
    pub(super) fn name_editor(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        target: StripOrBus,
        current: &str,
    ) {
        let status = effects::Status::of(state, target);
        let text = self
            .name_edits
            .entry(target)
            .or_insert_with(|| current.to_string());
        let (resp, badge_clicked) = ui
            .horizontal(|ui| {
                let width = match status {
                    Some(_) => {
                        ui.available_width() - effects::BADGE_W - ui.spacing().item_spacing.x
                    }
                    None => f32::INFINITY,
                };
                let resp = ui.add(
                    egui::TextEdit::singleline(text)
                        .font(TextStyle::Button)
                        .desired_width(width)
                        .margin(egui::Margin::symmetric(4, 2)),
                );
                let clicked = status
                    .is_some_and(|s| effects::badge(ui, &s, current, resp.rect.height()).clicked());
                (resp, clicked)
            })
            .inner;
        if resp.lost_focus() {
            let new = text.trim().to_string();
            if new.is_empty() || new == current {
                *text = current.to_string();
            } else if status.is_some() {
                self.confirm_rename = Some(RenameConfirm {
                    target,
                    from: current.to_string(),
                    to: new,
                });
            } else {
                let patch = CommonPatch {
                    name: Some(new),
                    ..Default::default()
                };
                self.actions.push(patch.to(target));
            }
        } else if !resp.has_focus() && *text != current {
            *text = current.to_string();
        }
        resp.on_hover_text("Click to rename");
        if badge_clicked {
            self.open_fx_at(target, Section::Insert);
        }
    }

    /// The device a hardware strip captures from or a hardware bus plays
    /// to. A long name is cut short to fit, and shown whole on hover. One
    /// that is not plugged in keeps its place, grayed out under the name it
    /// had when it was last seen, with a note saying so below.
    pub(super) fn device_picker(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        target: StripOrBus,
        current: Option<&str>,
    ) {
        let (kind, what, role) = match target {
            StripOrBus::Strip(_) => (
                DeviceKind::Source,
                "strip",
                "the input this strip listens to",
            ),
            StripOrBus::Bus(_) => (DeviceKind::Sink, "bus", "the output this bus plays to"),
        };
        let devices: Vec<&DeviceInfo> = state.devices.iter().filter(|d| d.kind == kind).collect();
        let plugged_in = current.and_then(|name| devices.iter().find(|d| d.name == name));
        let unplugged = current.filter(|_| plugged_in.is_none()).map(|name| {
            let known = self
                .prefs
                .device_names
                .get(name)
                .map_or(name, |n| n.as_str());
            let tip = format!(
                "{known} is not plugged in.\nThis {what} uses it again as soon as it is back."
            );
            (known.to_string(), tip)
        });
        let (selected_text, tip) = match (plugged_in, &unplugged) {
            (Some(d), _) => (
                RichText::new(&d.description),
                format!("{}: {role}.", d.description),
            ),
            (None, Some((known, tip))) => {
                (RichText::new(known).color(theme::p().text_dim), tip.clone())
            }
            (None, None) => (RichText::new("Select device…"), format!("Choose {role}.")),
        };
        let mut choice: Option<Option<String>> = None;
        egui::ComboBox::from_id_salt(("device", target))
            .width(ui.available_width())
            .truncate()
            .selected_text(selected_text.size(11.0))
            .show_ui(ui, |ui| {
                if ui.selectable_label(current.is_none(), "(none)").clicked() {
                    choice = Some(None);
                }
                // Listed so it is plain that the choice is kept.
                if let Some((known, tip)) = &unplugged {
                    ui.selectable_label(true, format!("{known} (unplugged)"))
                        .on_hover_text(tip);
                }
                for d in &devices {
                    let channels: Vec<&str> = d.channels.iter().map(|c| c.as_str()).collect();
                    let label = format!("{}  [{}]", d.description, channels.join(" "));
                    if ui
                        .selectable_label(current == Some(d.name.as_str()), label)
                        .on_hover_text(&d.name)
                        .clicked()
                    {
                        choice = Some(Some(d.name.clone()));
                    }
                }
            })
            .response
            .on_hover_text(tip);
        if let Some((_, tip)) = &unplugged {
            ui.label(
                RichText::new("not plugged in")
                    .size(10.0)
                    .color(theme::p().warning),
            )
            .on_hover_text(tip);
        }
        if let Some(device) = choice {
            let patch = CommonPatch {
                device: Some(device),
                ..Default::default()
            };
            self.actions.push(patch.to(target));
        }
    }

    /// The channel layout, for a strip's or bus's menu.
    pub(super) fn layout_menu(&mut self, ui: &mut Ui, target: StripOrBus, current: &ChannelLayout) {
        ui.menu_button(format!("Layout: {}", current.label()), |ui| {
            for l in ChannelLayout::PRESETS.iter() {
                if ui.selectable_label(l == current, l.label()).clicked() {
                    let patch = CommonPatch {
                        layout: Some(l.clone()),
                        ..Default::default()
                    };
                    self.actions.push(patch.to(target));
                    ui.close();
                }
            }
        });
    }

    /// Move left and right, and the color, for a strip's or bus's menu.
    /// `index` is where it is among `count` of its kind.
    pub(super) fn arrange_menu(
        &mut self,
        ui: &mut Ui,
        target: StripOrBus,
        index: usize,
        count: usize,
        color: Option<&str>,
    ) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(index > 0, egui::Button::new("Move left"))
                .clicked()
            {
                self.actions.push(move_request(target, index - 1));
                ui.close();
            }
            if ui
                .add_enabled(index + 1 < count, egui::Button::new("Move right"))
                .clicked()
            {
                self.actions.push(move_request(target, index + 1));
                ui.close();
            }
        });
        ui.menu_button("Color", |ui| {
            if let Some(color) = color_menu(ui, target, color) {
                let patch = CommonPatch {
                    color: Some(color),
                    ..Default::default()
                };
                self.actions.push(patch.to(target));
            }
        });
    }

    /// The row under a fader: the CLIP light, the level readout drawn by
    /// `readout`, and on a bus the LIM light. A strip leaves the LIM light's
    /// room empty so its readout stays centered.
    pub(super) fn readout_row(
        &mut self,
        ui: &mut Ui,
        inner_w: f32,
        target: StripOrBus,
        bus: Option<&Bus>,
        readout: impl FnOnce(&mut Self, &mut Ui),
    ) {
        ui.horizontal(|ui| {
            let gap = ui.spacing().item_spacing.x;
            let mid_w = (inner_w - 2.0 * (theme::LAMP_W + gap)).max(40.0);
            let clipped = self.clips.contains(&target);
            let what = match target {
                StripOrBus::Strip(_) => "strip",
                StripOrBus::Bus(_) => "bus",
            };
            let tip = if clipped {
                format!("This {what} clipped: it went over 0 dB.\nClick to clear.")
            } else {
                format!("Lights up and stays lit when this {what} goes over 0 dB.")
            };
            let r = widgets::lamp(ui, "CLIP", clipped, theme::p().meter_red).on_hover_text(tip);
            if r.clicked() {
                self.clips.remove(&target);
            }
            ui.allocate_ui_with_layout(vec2(mid_w, 20.0), Layout::top_down(Align::Center), |ui| {
                ui.set_width(mid_w);
                readout(self, ui);
            });
            match bus {
                Some(b) => self.limiter_lamp(ui, b),
                None => ui.add_space(theme::LAMP_W),
            }
        });
    }

    /// The LIM light: lit while the bus's limiter is holding it down.
    fn limiter_lamp(&mut self, ui: &mut Ui, b: &Bus) {
        let reduction = self.reductions.get(&b.id).copied().unwrap_or(0.0);
        let limiting = b.limiter.enabled && reduction < -0.1;
        let tip = if !b.limiter.enabled {
            "Safety limiter off.".to_string()
        } else if limiting {
            format!(
                "The safety limiter is turning this bus down by {:.1} dB to keep it under \
                 {:+.1} dB.",
                -reduction, b.limiter.ceiling_db
            )
        } else {
            format!(
                "Lights up while the safety limiter is keeping this bus under {:+.1} dB.",
                b.limiter.ceiling_db
            )
        };
        let r = widgets::lamp(ui, "LIM", limiting, theme::p().limit)
            .on_hover_text(format!("{tip}\nClick for the limiter's settings."));
        if r.clicked() {
            self.open_fx_at(FxTarget::Bus(b.id), Section::Limiter);
        }
        widgets::field_context_menu(&r, |ui| self.limiter_menu(ui, b));
    }
}

/// The color swatches, "None" and a picker for any other color. Returns the
/// color picked, `Some(None)` for none.
fn color_menu(ui: &mut Ui, target: StripOrBus, color: Option<&str>) -> Option<Option<String>> {
    let mut pick: Option<Option<String>> = None;
    egui::Grid::new(("colors", target))
        .spacing([4.0, 4.0])
        .show(ui, |ui| {
            for (k, (name, hex)) in COLOR_PRESETS.iter().enumerate() {
                let c = own_color(Some(hex)).unwrap_or(Color32::GRAY);
                let (rect, r) = ui.allocate_exact_size(vec2(26.0, 20.0), egui::Sense::click());
                ui.painter().rect_filled(rect, 4, c);
                let chosen = color.is_some_and(|x| x.eq_ignore_ascii_case(hex));
                if chosen || r.hovered() {
                    ui.painter().rect_stroke(
                        rect,
                        4,
                        egui::Stroke::new(2.0_f32, theme::p().text),
                        egui::StrokeKind::Outside,
                    );
                }
                if r.on_hover_text(*name).clicked() {
                    pick = Some(Some(hex.to_string()));
                    ui.close();
                }
                if k % 5 == 4 {
                    ui.end_row();
                }
            }
        });
    if ui.selectable_label(color.is_none(), "None").clicked() {
        pick = Some(None);
        ui.close();
    }
    // Any other color. Drawn in the menu itself: a picker in a popup of its
    // own would close the menu when it opens.
    ui.separator();
    ui.label(
        RichText::new("Custom")
            .size(11.0)
            .color(theme::p().text_dim),
    );
    let mut c = own_color(color).unwrap_or(Color32::from_rgb(128, 128, 128));
    if egui::color_picker::color_picker_color32(ui, &mut c, egui::color_picker::Alpha::Opaque) {
        pick = Some(Some(format_color([c.r(), c.g(), c.b()])));
    }
    pick
}

/// Small dim text, cut short with "…" when it does not fit.
pub(super) fn caption(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(9.5).color(theme::p().text_dim)).truncate());
}

/// The caption at the top of a strip or bus, which is also the handle to
/// drag it to another place by. A grip of dots in front of it says so, and
/// the whole line can be grabbed, not just the words.
pub(super) fn drag_caption(ui: &mut Ui, text: &str, item: StripOrBus) {
    let id = egui::Id::new(("drag", item));
    let hovered = ui
        .ctx()
        .read_response(id)
        .is_some_and(|r| r.hovered() || r.dragged());
    ui.dnd_drag_source(id, item, |ui| {
        ui.horizontal(|ui| {
            ui.set_min_width(ui.available_width());
            crate::widgets::grip(ui, hovered);
            caption(ui, text);
        });
    })
    .response
    .on_hover_cursor(egui::CursorIcon::Grab)
    .on_hover_text("Drag to move it");
}

/// A strip's or bus's own color, if it has one.
pub(super) fn own_color(color: Option<&str>) -> Option<Color32> {
    color
        .and_then(parse_color)
        .map(|[r, g, b]| Color32::from_rgb(r, g, b))
}

/// A band of the strip's or bus's color along the top of its frame.
pub(super) fn paint_color_band(ui: &Ui, rect: egui::Rect, color: Option<&str>) {
    if let Some(c) = own_color(color) {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min, vec2(rect.width(), 4.0)),
            egui::CornerRadius {
                nw: 8,
                ne: 8,
                sw: 0,
                se: 0,
            },
            c,
        );
    }
}

/// "70% (-9.3 dB)", or "muted".
pub(super) fn system_volume_text(v: &SystemVolume) -> String {
    if v.mute {
        "muted".to_string()
    } else {
        format!("{:.0}% ({:.1} dB)", v.percent(), v.volume_db)
    }
}

/// A small note that the system's volume control is turning one of Weir's
/// devices down. Shown, never changed: that volume belongs to the system.
pub(super) fn system_volume_note(ui: &mut Ui, v: &SystemVolume, explain: &str) -> egui::Response {
    let text = if v.mute {
        "muted by the system".to_string()
    } else {
        format!("system volume {:.0}%", v.percent())
    };
    ui.label(RichText::new(text).size(10.0).color(theme::p().warning))
        .on_hover_text(format!(
            "{explain}\n\nWeir shows this but leaves it alone. Change it in the system's volume \
         control if you want it back at 100%."
        ))
}
