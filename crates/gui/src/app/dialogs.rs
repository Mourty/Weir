//! The dialogs that add a strip or a bus, and the one that asks before
//! removing either.

use super::{bus_title, App};
use crate::theme;
use egui::{RichText, Ui};
use weir_protocol::*;

/// The Add strip dialog, as filled in so far.
pub(super) struct AddStripDialog {
    name: String,
    kind: StripKind,
    layout: ChannelLayout,
    device: Option<String>,
    routes: Vec<BusId>,
}

impl AddStripDialog {
    /// A virtual stereo strip sent to every bus, the most common kind.
    pub(super) fn new(mixer: &MixerState) -> Self {
        Self {
            name: String::new(),
            kind: StripKind::Virtual,
            layout: ChannelLayout::Stereo,
            device: None,
            routes: mixer.buses.iter().map(|b| b.id).collect(),
        }
    }
}

/// The Add bus dialog, as filled in so far.
pub(super) struct AddBusDialog {
    name: String,
    kind: BusKind,
    layout: ChannelLayout,
    device: Option<String>,
}

impl AddBusDialog {
    /// A stereo bus that plays to a device.
    pub(super) fn new() -> Self {
        Self {
            name: String::new(),
            kind: BusKind::Hardware,
            layout: ChannelLayout::Stereo,
            device: None,
        }
    }
}

/// What the buttons at the bottom of a dialog were used for.
#[derive(Default)]
struct Outcome {
    submit: bool,
    cancel: bool,
}

impl App {
    /// The add and remove dialogs, when open.
    pub(super) fn dialogs(&mut self, ctx: &egui::Context, state: &FullState) {
        self.add_strip_dialog(ctx, state);
        self.add_bus_dialog(ctx, state);
        self.confirm_remove_dialog(ctx);
    }

    fn add_strip_dialog(&mut self, ctx: &egui::Context, state: &FullState) {
        let Some(d) = self.add_strip.as_mut() else {
            return;
        };
        let mut open = true;
        let mut out = Outcome::default();
        egui::Window::new("Add strip")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("add_strip")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        out.submit |= name_row(ui, &mut d.name);
                        ui.label("Kind");
                        ui.horizontal(|ui| {
                            ui.radio_value(
                                &mut d.kind,
                                StripKind::Virtual,
                                "Virtual (apps play into it)",
                            );
                            ui.radio_value(
                                &mut d.kind,
                                StripKind::Hardware,
                                "Hardware (mic, capture)",
                            );
                        });
                        ui.end_row();
                        layout_row(ui, "add_strip_layout", &mut d.layout);
                        if d.kind == StripKind::Hardware {
                            device_row(ui, state, DeviceKind::Source, &mut d.device);
                        }
                        ui.label("Send to");
                        ui.vertical(|ui| {
                            for b in &state.mixer.buses {
                                let mut on = d.routes.contains(&b.id);
                                if ui.checkbox(&mut on, bus_title(&state.mixer, b)).changed() {
                                    if on {
                                        d.routes.push(b.id);
                                    } else {
                                        d.routes.retain(|x| *x != b.id);
                                    }
                                }
                            }
                        });
                        ui.end_row();
                    });
                buttons(ui, "Add", !d.name.trim().is_empty(), &mut out);
            });
        let name = d.name.trim();
        if out.submit && !name.is_empty() {
            self.actions.push(Request::AddStrip(AddStripParams {
                name: name.to_string(),
                kind: d.kind,
                layout: d.layout.clone(),
                device: d.device.clone().filter(|_| d.kind == StripKind::Hardware),
                routes: d.routes.clone(),
            }));
            out.cancel = true;
        }
        if !open || out.cancel {
            self.add_strip = None;
        }
    }

    fn add_bus_dialog(&mut self, ctx: &egui::Context, state: &FullState) {
        let Some(d) = self.add_bus.as_mut() else {
            return;
        };
        let mut open = true;
        let mut out = Outcome::default();
        egui::Window::new("Add bus")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("add_bus")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        out.submit |= name_row(ui, &mut d.name);
                        ui.label("Kind");
                        ui.horizontal(|ui| {
                            ui.radio_value(
                                &mut d.kind,
                                BusKind::Hardware,
                                "Hardware (speakers, headset)",
                            );
                            ui.radio_value(&mut d.kind, BusKind::Virtual, "Virtual microphone");
                        });
                        ui.end_row();
                        layout_row(ui, "add_bus_layout", &mut d.layout);
                        if d.kind == BusKind::Hardware {
                            device_row(ui, state, DeviceKind::Sink, &mut d.device);
                        }
                    });
                buttons(ui, "Add", !d.name.trim().is_empty(), &mut out);
            });
        let name = d.name.trim();
        if out.submit && !name.is_empty() {
            self.actions.push(Request::AddBus(AddBusParams {
                name: name.to_string(),
                kind: d.kind,
                layout: d.layout.clone(),
                device: d.device.clone().filter(|_| d.kind == BusKind::Hardware),
            }));
            out.cancel = true;
        }
        if !open || out.cancel {
            self.add_bus = None;
        }
    }

    /// Asking before a strip or bus is removed, and saying what goes with
    /// it. The toast afterwards offers to undo it.
    fn confirm_remove_dialog(&mut self, ctx: &egui::Context) {
        let Some((target, name)) = self.confirm_remove.clone() else {
            return;
        };
        let mut open = true;
        let mut done = false;
        let (what, consequence, request) = match target {
            StripOrBus::Strip(id) => (
                "strip",
                "Applications playing into it move to the default output.",
                Request::RemoveStrip(IdParams { id }),
            ),
            StripOrBus::Bus(id) => (
                "bus",
                "Strips routed to it lose that route.",
                Request::RemoveBus(IdParams { id }),
            ),
        };
        egui::Window::new(format!("Remove {what}?"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("Remove {what} \"{name}\"?"));
                ui.label(RichText::new(consequence).color(theme::p().text_dim));
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("Remove").color(theme::p().meter_red))
                        .clicked()
                    {
                        self.actions.push(request.clone());
                        self.show_toast(&format!("Removed {what} {name}"), true);
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        done = true;
                    }
                });
            });
        if !open || done {
            self.confirm_remove = None;
        }
    }
}

/// The name field. Returns true when Enter was pressed in it.
fn name_row(ui: &mut Ui, name: &mut String) -> bool {
    ui.label("Name");
    let r = ui.text_edit_singleline(name);
    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    ui.end_row();
    enter
}

/// The channel layout, as a drop-down.
fn layout_row(ui: &mut Ui, id: &str, layout: &mut ChannelLayout) {
    ui.label("Channels");
    egui::ComboBox::from_id_salt(id)
        .selected_text(layout.label())
        .show_ui(ui, |ui| {
            for l in ChannelLayout::PRESETS.iter() {
                ui.selectable_value(layout, l.clone(), l.label());
            }
        });
    ui.end_row();
}

/// The device of a new hardware strip or bus, which can also be picked
/// later.
fn device_row(ui: &mut Ui, state: &FullState, kind: DeviceKind, device: &mut Option<String>) {
    ui.label("Device");
    let selected = device
        .as_deref()
        .and_then(|n| state.devices.iter().find(|x| x.name == n))
        .map_or_else(|| "Select later".to_string(), |x| x.description.clone());
    egui::ComboBox::from_id_salt(("add_device", kind == DeviceKind::Sink))
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(device, None, "Select later");
            for dev in state.devices.iter().filter(|x| x.kind == kind) {
                ui.selectable_value(device, Some(dev.name.clone()), &dev.description);
            }
        });
    ui.end_row();
}

/// The dialog's buttons: `yes`, enabled when `ready`, and Cancel.
fn buttons(ui: &mut Ui, yes: &str, ready: bool, out: &mut Outcome) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(ready, egui::Button::new(yes)).clicked() {
            out.submit = true;
        }
        if ui.button("Cancel").clicked() {
            out.cancel = true;
        }
    });
}
