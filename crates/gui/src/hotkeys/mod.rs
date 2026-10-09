//! Hotkeys in the window: the list of them ([`HotkeysWindow`]), the window
//! for making or changing one ([`Editor`]), the simple form most are made
//! with (`simple`), and recording keys (`record`).

mod editor;
mod record;
pub mod simple;

pub use editor::Editor;

use crate::{theme, widgets};
use egui::{vec2, Align, Layout, RichText, Ui};
use serde_json::json;
use simple::{Action, Simple};
use weir_protocol::*;

/// What was asked for in the list, for the mixer window to open.
pub enum ListAction {
    /// A new hotkey.
    Add,
    /// Change this one.
    Edit(HotkeyId),
}

/// What is being dragged in the list: a hotkey by its grip, or a group by
/// its header's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Dragged {
    Hotkey(HotkeyId),
    Group(HotkeyGroupId),
}

/// How far in a group's hotkeys sit, under its header.
const GROUP_INDENT: f32 = 18.0;

/// A group being renamed: which, the name typed so far, and whether the
/// field still has to take the keyboard.
struct Renaming {
    id: HotkeyGroupId,
    text: String,
    focus: bool,
}

/// The Hotkeys window: every hotkey, in groups or not, switched on or off,
/// tried, changed, moved or removed, and how keys reach Weir on this
/// desktop.
pub struct HotkeysWindow {
    pub raise: bool,
    pub closed: bool,
    /// A hotkey waiting for a yes before it is removed.
    confirm_remove: Option<HotkeyId>,
    /// A group waiting for a yes before it is removed.
    confirm_remove_group: Option<HotkeyGroupId>,
    renaming: Option<Renaming>,
    /// The name of a group just added, to rename once the daemon has it.
    added_group: Option<String>,
}

impl HotkeysWindow {
    pub fn new() -> Self {
        Self {
            raise: false,
            closed: false,
            confirm_remove: None,
            confirm_remove_group: None,
            renaming: None,
            added_group: None,
        }
    }

    /// Draw the window. Requests for the daemon are appended to `actions`.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        state: &FullState,
        actions: &mut Vec<Request>,
    ) -> Option<ListAction> {
        let id = egui::ViewportId::from_hash_of("hotkeys");
        if self.raise {
            self.raise = false;
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
        }
        let builder = egui::ViewportBuilder::default()
            .with_title("Hotkeys · Weir")
            .with_app_id("weir")
            .with_icon(crate::icon())
            .with_inner_size([880.0, 600.0])
            .with_min_inner_size([640.0, 360.0]);
        let mut asked = None;
        ctx.show_viewport_immediate(id, builder, |ctx, class| {
            if class == egui::ViewportClass::Embedded {
                let mut open = true;
                egui::Window::new("Hotkeys")
                    .id(egui::Id::new("hotkeys_embedded"))
                    .open(&mut open)
                    .default_size([820.0, 520.0])
                    .show(ctx, |ui| {
                        asked = self.contents(ui, state, actions);
                        ui.separator();
                        asked = asked.take().or(self.footer(ui, state, actions));
                    });
                if !open {
                    self.closed = true;
                }
            } else {
                egui::TopBottomPanel::bottom(egui::Id::new("hotkeys_footer"))
                    .frame(
                        egui::Frame::new()
                            .fill(theme::p().bg)
                            .inner_margin(egui::Margin::symmetric(16, 10)),
                    )
                    .show(ctx, |ui| asked = self.footer(ui, state, actions));
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::p().bg).inner_margin(16))
                    .show(ctx, |ui| {
                        if let Some(a) = self.contents(ui, state, actions) {
                            asked = Some(a);
                        }
                    });
                if ctx.input(|i| i.viewport().close_requested()) {
                    self.closed = true;
                }
            }
        });
        asked
    }

    /// How keys reach Weir, then the hotkeys in no group, then each group
    /// with its own.
    fn contents(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        actions: &mut Vec<Request>,
    ) -> Option<ListAction> {
        let info = &state.hotkeys;
        status_line(ui, &info.keys);
        ui.add_space(8.0);
        // Problems that are not any one hotkey's, such as a file that could
        // not be read.
        for p in info
            .problems
            .iter()
            .filter(|p| !info.hotkeys.iter().any(|h| h.id == p.hotkey))
        {
            ui.label(RichText::new(&p.problem).color(theme::p().warning));
        }
        if info.hotkeys.is_empty() && info.groups.is_empty() {
            return self.empty_view(ui, state, actions);
        }
        // A group just added gets its name typed straight away.
        if let Some(name) = &self.added_group {
            if let Some(g) = info.groups.iter().find(|g| &g.name == name) {
                self.renaming = Some(Renaming {
                    id: g.id,
                    text: String::new(),
                    focus: true,
                });
                self.added_group = None;
            }
        }
        let grouped = |h: &Hotkey| info.groups.iter().any(|g| g.id == h.group);
        let loose: Vec<&Hotkey> = info.hotkeys.iter().filter(|h| !grouped(h)).collect();
        let dragged = egui::DragAndDrop::payload::<Dragged>(ui.ctx()).map(|d| *d);
        let mut asked = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                // With no hotkey in no group, somewhere else to drop one
                // out of its group.
                let out_of_group = matches!(dragged, Some(Dragged::Hotkey(id))
                    if info.hotkeys.iter().any(|h| h.id == id && grouped(h)));
                if loose.is_empty() && out_of_group {
                    loose_drop(ui, actions);
                }
                let ids: Vec<HotkeyId> = loose.iter().map(|h| h.id).collect();
                for (i, h) in loose.iter().enumerate() {
                    let place = Place {
                        group: 0,
                        group_on: true,
                        index: i,
                        members: &ids,
                    };
                    if let Some(a) = self.row(ui, state, h, place, actions) {
                        asked = Some(a);
                    }
                }
                let order: Vec<HotkeyGroupId> = info.groups.iter().map(|g| g.id).collect();
                for (gi, g) in info.groups.iter().enumerate() {
                    let members: Vec<&Hotkey> =
                        info.hotkeys.iter().filter(|h| h.group == g.id).collect();
                    self.group_header(ui, g, gi, &order, members.len(), actions);
                    let ids: Vec<HotkeyId> = members.iter().map(|h| h.id).collect();
                    for (i, h) in members.iter().enumerate() {
                        let place = Place {
                            group: g.id,
                            group_on: g.enabled,
                            index: i,
                            members: &ids,
                        };
                        if let Some(a) = self.row(ui, state, h, place, actions) {
                            asked = Some(a);
                        }
                    }
                }
            });
        asked
    }

    /// No hotkeys yet: what they are, and two ways to start.
    fn empty_view(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        actions: &mut Vec<Request>,
    ) -> Option<ListAction> {
        ui.add_space(20.0);
        ui.label(RichText::new("No hotkeys yet.").size(16.0).strong());
        ui.label(
            "A hotkey is keys that do something in the mixer, whichever window is in \
             front: mute your microphone, push to talk, turn the music down, load a scene.",
        );
        ui.add_space(10.0);
        let examples = examples(state);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !examples.is_empty(),
                    egui::Button::new("Add a few examples"),
                )
                .on_hover_text(
                    "Push to talk, muting the microphone, music up and down, and more. They \
                     start switched off: switch on the ones you want, and change their keys \
                     if you like.",
                )
                .clicked()
            {
                actions.extend(examples.into_iter().map(Request::SetHotkey));
            }
            ui.label(
                RichText::new("or add your own with the button below.").color(theme::p().text_dim),
            );
        });
        None
    }

    /// One hotkey: a grip to drag it by, its switch, keys, name and what it
    /// does, and buttons.
    fn row(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        h: &Hotkey,
        place: Place<'_>,
        actions: &mut Vec<Request>,
    ) -> Option<ListAction> {
        let mut asked = None;
        let problem = state
            .hotkeys
            .problems
            .iter()
            .find(|p| p.hotkey == h.id)
            .map(|p| p.problem.as_str());
        // Everything lines up at the top, with the first of its keys, since
        // a hotkey with several keys has a line for each.
        let resp = ui
            .horizontal_top(|ui| {
                ui.set_min_height(38.0);
                if place.group != 0 {
                    ui.add_space(GROUP_INDENT);
                }
                // A switched-off group's hotkeys do nothing: shown faded.
                if !place.group_on {
                    ui.multiply_opacity(0.5);
                }
                drag_grip(
                    ui,
                    Dragged::Hotkey(h.id),
                    "Drag to move it, or into a group",
                );
                let mut tip = match (h.enabled, state.hotkeys.keys.method) {
                    (true, _) => "On: its keys work. Click to switch them off.".to_string(),
                    (false, KeysMethod::Desktop) => "Off: its keys do nothing, but your desktop \
                                                     keeps them for it. It can still be pressed \
                                                     by name. Click to switch it on."
                        .to_string(),
                    (false, _) => "Off: its keys do nothing, but it can still be pressed by \
                                   name. Click to switch it on."
                        .to_string(),
                };
                if !place.group_on {
                    tip.push_str(" Its group is switched off, so its keys do nothing for now.");
                }
                if widgets::switch(ui, h.enabled).on_hover_text(tip).clicked() {
                    actions.push(Request::SwitchHotkey(SwitchHotkeyParams {
                        hotkey: HotkeyKey::Id(h.id),
                        enabled: Flag::Set(!h.enabled),
                    }));
                }
                ui.add_space(4.0);
                // Each combination on a line of its own, so that several
                // never run into the name.
                let keys = shown_keys(state, h);
                let width = 210.0;
                ui.allocate_ui_with_layout(vec2(width, 24.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(width);
                    ui.spacing_mut().item_spacing.y = 4.0;
                    if keys.is_empty() {
                        ui.label(RichText::new("no keys").color(theme::p().text_dim));
                    }
                    for k in &keys {
                        ui.horizontal(|ui| key_chips(ui, k, 12.0));
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if self.confirm_remove == Some(h.id) {
                        if ui.button("Keep").clicked() {
                            self.confirm_remove = None;
                        }
                        if ui
                            .button(RichText::new("Remove").color(theme::p().meter_red))
                            .clicked()
                        {
                            actions.push(Request::RemoveHotkey(HotkeyRef {
                                hotkey: HotkeyKey::Id(h.id),
                            }));
                            self.confirm_remove = None;
                        }
                        ui.label("Remove it?");
                    } else {
                        if ui.button("×").on_hover_text("Remove this hotkey").clicked() {
                            self.confirm_remove = Some(h.id);
                        }
                        if ui.button("Edit").clicked() {
                            asked = Some(ListAction::Edit(h.id));
                        }
                        if ui
                            .button("Try")
                            .on_hover_text("Do what pressing and letting go of its keys does")
                            .clicked()
                        {
                            actions.push(Request::RunHotkey(HotkeyRef {
                                hotkey: HotkeyKey::Id(h.id),
                            }));
                        }
                    }
                    if let Some(tag) = tag(h) {
                        ui.add(
                            egui::Button::new(RichText::new(tag).size(11.0).color(theme::p().text))
                                .fill(theme::p().button_off)
                                .corner_radius(9)
                                .sense(egui::Sense::hover()),
                        );
                    }
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.add(egui::Label::new(RichText::new(&h.name).strong()).truncate());
                        let words = describe_hotkey(h, &state.mixer);
                        ui.add(
                            egui::Label::new(
                                RichText::new(words).size(12.0).color(theme::p().text_dim),
                            )
                            .truncate(),
                        );
                        if let Some(p) = problem {
                            ui.label(RichText::new(p).size(12.0).color(theme::p().warning));
                        }
                    });
                });
            })
            .response;
        hotkey_drop(ui, &resp, &place, actions);
        ui.separator();
        asked
    }

    /// A group's header: a grip to drag it by, its switch, name and how many
    /// hotkeys it has, and buttons to rename or remove it. Hotkeys dropped
    /// on it go into it.
    fn group_header(
        &mut self,
        ui: &mut Ui,
        g: &HotkeyGroup,
        index: usize,
        order: &[HotkeyGroupId],
        count: usize,
        actions: &mut Vec<Request>,
    ) {
        ui.add_space(6.0);
        let resp = egui::Frame::new()
            .fill(theme::p().section_fill)
            .corner_radius(5)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    drag_grip(ui, Dragged::Group(g.id), "Drag to move the group");
                    let tip = if g.enabled {
                        "On: its hotkeys' keys work, each as its own switch says. Click to \
                         switch them all off."
                    } else {
                        "Off: none of its hotkeys' keys work. Each keeps its own switch for \
                         when the group is on again. Click to switch the group on."
                    };
                    if widgets::switch(ui, g.enabled).on_hover_text(tip).clicked() {
                        actions.push(Request::SetHotkeyGroup(SetHotkeyGroupParams {
                            group: HotkeyGroupKey::Id(g.id),
                            name: None,
                            enabled: Some(Flag::Set(!g.enabled)),
                        }));
                    }
                    ui.add_space(4.0);
                    self.group_name(ui, g, actions);
                    let words = match count {
                        0 => "no hotkeys".to_string(),
                        1 => "1 hotkey".to_string(),
                        n => format!("{n} hotkeys"),
                    };
                    ui.label(RichText::new(words).size(12.0).color(theme::p().text_dim));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.confirm_remove_group == Some(g.id) {
                            if ui.button("Keep").clicked() {
                                self.confirm_remove_group = None;
                            }
                            if ui
                                .button(RichText::new("Remove").color(theme::p().meter_red))
                                .clicked()
                            {
                                actions.push(Request::RemoveHotkeyGroup(HotkeyGroupRef {
                                    group: HotkeyGroupKey::Id(g.id),
                                }));
                                self.confirm_remove_group = None;
                            }
                            ui.label("Remove the group? Its hotkeys stay.");
                        } else {
                            if ui
                                .button("×")
                                .on_hover_text("Remove this group. Its hotkeys stay, in no group.")
                                .clicked()
                            {
                                self.confirm_remove_group = Some(g.id);
                            }
                            if ui.button("Rename").clicked() {
                                self.renaming = Some(Renaming {
                                    id: g.id,
                                    text: g.name.clone(),
                                    focus: true,
                                });
                            }
                        }
                    });
                });
            })
            .response;
        match resp.dnd_hover_payload::<Dragged>().map(|d| *d) {
            // A hotkey dropped on the header goes into the group, last.
            Some(Dragged::Hotkey(id)) => {
                ui.painter().rect_stroke(
                    resp.rect,
                    5.0,
                    egui::Stroke::new(2.0_f32, theme::p().accent),
                    egui::StrokeKind::Inside,
                );
                if resp.dnd_release_payload::<Dragged>().is_some() {
                    actions.push(Request::MoveHotkey(MoveHotkeyParams {
                        hotkey: HotkeyKey::Id(id),
                        group: Some(HotkeyGroupKey::Id(g.id)),
                        index: None,
                    }));
                }
            }
            // A group lands above or below this one.
            Some(Dragged::Group(id)) => {
                let after = drop_line(ui, &resp);
                let from = order.iter().position(|o| *o == id);
                if resp.dnd_release_payload::<Dragged>().is_some() {
                    if let Some(to) = from.and_then(|f| widgets::drop_index(f, index, after)) {
                        actions.push(Request::MoveHotkeyGroup(MoveHotkeyGroupParams {
                            group: HotkeyGroupKey::Id(id),
                            index: to,
                        }));
                    }
                }
            }
            None => {}
        }
        if count == 0 {
            ui.horizontal(|ui| {
                ui.add_space(GROUP_INDENT + 4.0);
                ui.label(
                    RichText::new("Drag hotkeys here, or pick this group when editing one.")
                        .size(12.0)
                        .color(theme::p().text_dim),
                );
            });
        }
        ui.add_space(2.0);
    }

    /// A group's name, or the field to type a new one into while it is
    /// being renamed: Enter or clicking elsewhere keeps what was typed, and
    /// Esc does not.
    fn group_name(&mut self, ui: &mut Ui, g: &HotkeyGroup, actions: &mut Vec<Request>) {
        let Some(r) = self.renaming.as_mut().filter(|r| r.id == g.id) else {
            ui.add(egui::Label::new(RichText::new(&g.name).strong().size(15.0)).truncate());
            return;
        };
        let field = ui.add(
            egui::TextEdit::singleline(&mut r.text)
                .hint_text(g.name.as_str())
                .char_limit(HOTKEY_NAME_MAX)
                .desired_width(220.0),
        );
        if r.focus {
            field.request_focus();
            r.focus = false;
        }
        if !field.lost_focus() {
            return;
        }
        let name = r.text.trim().to_string();
        let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if !escaped && !name.is_empty() && name != g.name {
            actions.push(Request::SetHotkeyGroup(SetHotkeyGroupParams {
                group: HotkeyGroupKey::Id(g.id),
                name: Some(name),
                enabled: None,
            }));
        }
        self.renaming = None;
    }

    /// The buttons to add a hotkey or a group, where else hotkeys come
    /// from, and the desktop's shortcut settings when it can open them.
    fn footer(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        actions: &mut Vec<Request>,
    ) -> Option<ListAction> {
        let mut asked = None;
        ui.horizontal(|ui| {
            let add = egui::Button::new(
                RichText::new("+ Add hotkey")
                    .strong()
                    .color(theme::p().on_text),
            )
            .fill(theme::p().accent);
            if ui.add(add).clicked() {
                asked = Some(ListAction::Add);
            }
            if ui
                .button("+ Add group")
                .on_hover_text("Hotkeys in a group are switched on and off together")
                .clicked()
            {
                let name = new_group_name(&state.hotkeys.groups);
                actions.push(Request::AddHotkeyGroup(AddHotkeyGroupParams {
                    name: name.clone(),
                    enabled: true,
                }));
                self.added_group = Some(name);
            }
            let keys = &state.hotkeys.keys;
            if keys.method == KeysMethod::Desktop
                && keys.configurable
                && ui
                    .button(format!("Keys in {}", settings_name()))
                    .on_hover_text(
                        "Each hotkey is one entry there: change its keys, or give it more",
                    )
                    .clicked()
            {
                actions.push(Request::OpenShortcutSettings);
            }
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Or right-click a mute, solo or routing button, or a fader, in the mixer.",
                    )
                    .size(12.0)
                    .color(theme::p().text_dim),
                )
                .truncate(),
            );
        });
        asked
    }
}

/// Where a hotkey's row is: its group (0 for none), whether that group is
/// on, its place among the group's hotkeys, and theirs.
struct Place<'a> {
    group: HotkeyGroupId,
    group_on: bool,
    index: usize,
    members: &'a [HotkeyId],
}

/// Six dots to drag `what` by.
fn drag_grip(ui: &mut Ui, what: Dragged, tip: &str) {
    let id = egui::Id::new(("hotkeys-drag", what));
    let hovered = ui
        .ctx()
        .read_response(id)
        .is_some_and(|r| r.hovered() || r.dragged());
    ui.dnd_drag_source(id, what, |ui| {
        // Down a little, to sit level with the switch beside it.
        ui.vertical(|ui| {
            ui.add_space(3.0);
            widgets::grip(ui, hovered);
        });
    })
    .response
    .on_hover_cursor(egui::CursorIcon::Grab)
    .on_hover_text(tip);
}

/// While something is dragged over `resp`, a line above or below it where
/// it would land, and which: whether below.
fn drop_line(ui: &Ui, resp: &egui::Response) -> bool {
    let after = ui
        .ctx()
        .pointer_interact_pos()
        .is_some_and(|p| p.y > resp.rect.center().y);
    let y = if after {
        resp.rect.bottom() + 3.0
    } else {
        resp.rect.top() - 3.0
    };
    ui.painter().hline(
        resp.rect.x_range(),
        y,
        egui::Stroke::new(3.0_f32, theme::p().accent),
    );
    after
}

/// While a hotkey is dragged over the row `resp` of the one at `place`,
/// show where it would land, and move it there when it is dropped: into
/// that row's group, before or after it.
fn hotkey_drop(ui: &Ui, resp: &egui::Response, place: &Place<'_>, actions: &mut Vec<Request>) {
    let Some(Dragged::Hotkey(id)) = resp.dnd_hover_payload::<Dragged>().map(|d| *d) else {
        return;
    };
    let after = drop_line(ui, resp);
    if resp.dnd_release_payload::<Dragged>().is_none() {
        return;
    }
    let from = place.members.iter().position(|m| *m == id);
    let to = match from {
        Some(from) => match widgets::drop_index(from, place.index, after) {
            Some(to) => to,
            None => return,
        },
        None => place.index + usize::from(after),
    };
    actions.push(Request::MoveHotkey(MoveHotkeyParams {
        hotkey: HotkeyKey::Id(id),
        group: from.is_none().then_some(HotkeyGroupKey::Id(place.group)),
        index: Some(to),
    }));
}

/// A place to drop a hotkey out of its group, for when no hotkey is in
/// none.
fn loose_drop(ui: &mut Ui, actions: &mut Vec<Request>) {
    let resp = egui::Frame::new()
        .stroke(egui::Stroke::new(1.0_f32, theme::p().grid_major))
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("Drop here to take it out of its group").color(theme::p().text_dim),
            );
        })
        .response;
    if let Some(Dragged::Hotkey(id)) = resp.dnd_hover_payload::<Dragged>().map(|d| *d) {
        ui.painter().rect_stroke(
            resp.rect,
            5.0,
            egui::Stroke::new(2.0_f32, theme::p().accent),
            egui::StrokeKind::Inside,
        );
        if resp.dnd_release_payload::<Dragged>().is_some() {
            actions.push(Request::MoveHotkey(MoveHotkeyParams {
                hotkey: HotkeyKey::Id(id),
                group: Some(HotkeyGroupKey::Id(0)),
                index: None,
            }));
        }
    }
    ui.add_space(6.0);
}

/// A name for a new group that no group has yet.
fn new_group_name(groups: &[HotkeyGroup]) -> String {
    (1..)
        .map(|n| match n {
            1 => "New group".to_string(),
            n => format!("New group {n}"),
        })
        .find(|name| !groups.iter().any(|g| g.name.eq_ignore_ascii_case(name)))
        .expect("some name is free")
}

/// How keys reach Weir, with a light: green when they work.
fn status_line(ui: &mut Ui, keys: &KeysStatus) {
    let color = match keys.method {
        KeysMethod::Desktop | KeysMethod::X11 => theme::p().meter_green,
        KeysMethod::Starting => theme::p().meter_yellow,
        KeysMethod::Unavailable => theme::p().text_dim,
    };
    egui::Frame::new()
        .fill(theme::p().section_fill)
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("•").size(18.0).color(color));
                ui.label(&keys.message);
            });
        });
}

/// Whether the desktop is KDE Plasma, going by the session's variables.
fn on_kde() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_ascii_uppercase().contains("KDE"))
}

/// What the desktop's settings are called, to name them in a sentence.
pub(crate) fn settings_name() -> &'static str {
    if on_kde() {
        "System Settings"
    } else {
        "your desktop's settings"
    }
}

/// Where in the desktop's settings Weir's hotkeys are.
pub(crate) fn settings_path() -> &'static str {
    if on_kde() {
        "System Settings › Keyboard › Shortcuts › Weir"
    } else {
        "your desktop's shortcut settings"
    }
}

/// What to say under a hotkey's keys about where they work.
pub(crate) fn keys_hint(state: &FullState) -> String {
    match state.hotkeys.keys.method {
        KeysMethod::Desktop if state.hotkeys.keys.settable => format!(
            "Works whichever window is in front, even a full-screen game. Also in {}.",
            settings_path()
        ),
        KeysMethod::Desktop => {
            "Works whichever window is in front, even a full-screen game.".into()
        }
        KeysMethod::X11 => "Works whichever window is in front.".into(),
        KeysMethod::Starting => "Hotkeys start working once the desktop is up.".into(),
        KeysMethod::Unavailable => state.hotkeys.keys.message.clone(),
    }
}

/// A hotkey's keys as they work now: as the desktop has them when it says,
/// since they can be changed or added to in its settings, or else as Weir
/// has them.
fn shown_keys(state: &FullState, h: &Hotkey) -> Vec<String> {
    let keys = &state.hotkeys.keys;
    match keys.assigned.get(&h.id) {
        Some(given) => given.clone(),
        // Only the first keys are suggested to the desktop.
        None if keys.method == KeysMethod::Desktop => h.keys.iter().take(1).cloned().collect(),
        None => h.keys.clone(),
    }
}

/// A word on how a hotkey behaves, for the list.
fn tag(h: &Hotkey) -> Option<&'static str> {
    if h.each_press == EachPress::Next {
        Some("cycles")
    } else if h.repeat_ms.is_some() {
        Some("repeats")
    } else if h.on_release != OnRelease::Nothing {
        Some("while held")
    } else if h.keys.is_empty() {
        Some("by name")
    } else {
        None
    }
}

/// Keys as keyboard keys, `Ctrl` `+` `Alt` `+` `M`, at text `size`.
pub(crate) fn key_chips(ui: &mut Ui, keys: &str, size: f32) {
    ui.spacing_mut().item_spacing.x = 3.0;
    for (i, part) in keys.split('+').enumerate() {
        if i > 0 {
            ui.label(RichText::new("+").size(size).color(theme::p().text_dim));
        }
        egui::Frame::new()
            .fill(theme::p().button_off)
            .stroke(egui::Stroke::new(1.0_f32, theme::p().grid_major))
            .corner_radius(4)
            .inner_margin(egui::Margin::symmetric(6, 1))
            .show(ui, |ui| {
                ui.label(RichText::new(part.trim()).size(size).strong());
            });
    }
}

/// A few hotkeys to start from, switched off, for the strips this mixer
/// has: push to talk and muting on the first microphone, the music up and
/// down, dipping it to talk over it, and bringing up the window. Keys and
/// names other hotkeys have are left out.
pub(crate) fn examples(state: &FullState) -> Vec<Hotkey> {
    let strips = &state.mixer.strips;
    let mic = strips
        .iter()
        .find(|s| s.kind == StripKind::Hardware)
        .or(strips.first());
    let music = strips
        .iter()
        .find(|s| s.kind == StripKind::Virtual && s.name.to_lowercase().contains("music"))
        .or_else(|| strips.iter().find(|s| s.kind == StripKind::Virtual));
    let mut out = Vec::new();
    let base = |id| Simple::new(StripOrBus::Strip(id), state);
    if let Some(mic) = mic {
        out.push(
            base(mic.id)
                .with(Action::PushToTalk)
                .hotkey("Push to talk", &["Ctrl+Alt+Space".into()]),
        );
        out.push(base(mic.id).hotkey("Mute mic", &["Ctrl+Alt+M".into()]));
    }
    if let Some(music) = music {
        let by = |up| Simple {
            up,
            ..base(music.id).with(Action::VolumeBy)
        };
        out.push(by(true).hotkey("Music up", &["Ctrl+Alt+Up".into()]));
        out.push(by(false).hotkey("Music down", &["Ctrl+Alt+Down".into()]));
        if let Some(mic) = mic.filter(|m| m.id != music.id) {
            let mut dip = HotkeyStep::new("set_strip", json!({"id": music.id, "gain_db": -20.0}));
            dip.over_ms = Some(300);
            let mut talk = base(mic.id).hotkey("Talk over music", &["Ctrl+Alt+D".into()]);
            talk.steps = vec![
                dip,
                HotkeyStep::new("set_strip", json!({"id": mic.id, "mute": false})),
            ];
            talk.on_release = OnRelease::Restore;
            out.push(talk);
        }
    }
    out.push(
        base(0)
            .with(Action::ShowWindow)
            .hotkey("Show Weir", &["Ctrl+Alt+W".into()]),
    );
    let have = &state.hotkeys.hotkeys;
    out.into_iter()
        .filter(|h| !have.iter().any(|o| o.name.eq_ignore_ascii_case(&h.name)))
        .map(|mut h| {
            h.enabled = false;
            h.keys.retain(|k| !have.iter().any(|o| o.keys.contains(k)));
            h
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_start_off_and_leave_alone_what_is_taken() {
        let mut st = FullState::default();
        st.mixer.strips = vec![
            Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono),
            Strip::new(2, "Music", StripKind::Virtual, ChannelLayout::Stereo),
        ];
        let all = examples(&st);
        assert_eq!(all.len(), 6);
        assert!(all.iter().all(|h| !h.enabled));
        let mut mine = all[1].clone();
        mine.name = "My mute".into();
        mine.keys = vec!["Ctrl+Alt+Up".into()];
        st.hotkeys.hotkeys = vec![all[0].clone(), mine];
        let rest = examples(&st);
        assert!(!rest.iter().any(|h| h.name == "Push to talk"));
        let up = rest.iter().find(|h| h.name == "Music up").unwrap();
        assert!(up.keys.is_empty(), "its keys belong to another hotkey");
    }

    #[test]
    fn the_desktops_keys_show_when_it_gives_them() {
        let mut st = FullState::default();
        let h = Simple::new(StripOrBus::Strip(1), &st).hotkey("Talk", &["F9".into(), "F10".into()]);
        st.hotkeys.keys.method = KeysMethod::X11;
        assert_eq!(shown_keys(&st, &h), ["F9", "F10"]);
        st.hotkeys.keys.method = KeysMethod::Desktop;
        assert_eq!(shown_keys(&st, &h), ["F9"], "only the first is suggested");
        st.hotkeys
            .keys
            .assigned
            .insert(0, vec!["F9".into(), "Ctrl+Alt+I".into()]);
        assert_eq!(shown_keys(&st, &h), ["F9", "Ctrl+Alt+I"]);
    }
}
