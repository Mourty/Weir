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
    /// Whether dragging to an edge of the list scrolls it yet.
    scroll_armed: bool,
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
            scroll_armed: false,
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

    /// How keys reach Weir, then the list: hotkeys in no group and groups,
    /// in the order they were put in.
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
        let order = hotkey_order(&info.order, &info.hotkeys, &info.groups);
        let dragged = egui::DragAndDrop::payload::<Dragged>(ui.ctx()).map(|d| *d);
        let mut asked = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                if dragged.is_some() {
                    scroll_at_edges(ui, &mut self.scroll_armed);
                } else {
                    self.scroll_armed = false;
                }
                // Every place something dragged could go, found while the
                // list is drawn.
                let mut slots = Vec::new();
                let width = ui.max_rect().x_range();
                for (i, item) in order.iter().enumerate() {
                    slots.push(Slot::before(ui, 0, i, width));
                    let a = match *item {
                        HotkeyListItem::Hotkey(id) => info
                            .hotkeys
                            .iter()
                            .find(|h| h.id == id)
                            .and_then(|h| self.row(ui, state, h, None, true, actions)),
                        HotkeyListItem::Group(id) => info
                            .groups
                            .iter()
                            .find(|g| g.id == id)
                            .and_then(|g| self.group(ui, state, g, &mut slots, actions)),
                    };
                    asked = asked.take().or(a);
                }
                slots.push(Slot::before(ui, 0, order.len(), width));
                if let Some(dragged) = dragged {
                    drop_nearest(ui, dragged, &slots, &order, info, actions);
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
    /// does, and buttons, then a line under it if `line` (a group's last
    /// hotkey has its group's edge instead). `group` is the group it is
    /// drawn in, if any.
    fn row(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        h: &Hotkey,
        group: Option<&HotkeyGroup>,
        line: bool,
        actions: &mut Vec<Request>,
    ) -> Option<ListAction> {
        let mut asked = None;
        let group_on = group.is_none_or(|g| g.enabled);
        let problem = state
            .hotkeys
            .problems
            .iter()
            .find(|p| p.hotkey == h.id)
            .map(|p| p.problem.as_str());
        // Everything lines up at the top, with the first of its keys, since
        // a hotkey with several keys has a line for each.
        ui.horizontal_top(|ui| {
            ui.set_min_height(38.0);
            // A switched-off group's hotkeys do nothing: shown faded.
            if !group_on {
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
            if !group_on {
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
            // Narrower in a group by as much as its box takes in, so that
            // names line up all down the list.
            let width = match group {
                Some(_) => 210.0 - GROUP_INDENT - 1.0,
                None => 210.0,
            };
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
                let pill = |ui: &mut Ui, text: &str| {
                    ui.add(
                        egui::Button::new(RichText::new(text).size(11.0).color(theme::p().text))
                            .fill(theme::p().button_off)
                            .corner_radius(9)
                            .sense(egui::Sense::hover()),
                    )
                };
                if let Some(tag) = tag(h) {
                    pill(ui, tag);
                }
                if let Some(sounds) = h.sounds.describe() {
                    let mut words = sounds;
                    words.replace_range(..1, "P");
                    pill(ui, "sounds").on_hover_text(words);
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
        });
        if line {
            ui.separator();
        }
        asked
    }

    /// A group: a box holding its header and its hotkeys.
    fn group(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        g: &HotkeyGroup,
        slots: &mut Vec<Slot>,
        actions: &mut Vec<Request>,
    ) -> Option<ListAction> {
        let members: Vec<&Hotkey> = state
            .hotkeys
            .hotkeys
            .iter()
            .filter(|h| h.group == g.id)
            .collect();
        let mut asked = None;
        ui.add_space(4.0);
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0_f32, theme::p().grid_major))
            .corner_radius(6)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.group_header(ui, g, members.len(), actions);
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: GROUP_INDENT as i8,
                        right: 8,
                        top: 4,
                        bottom: 2,
                    })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let width = ui.max_rect().x_range();
                        if members.is_empty() {
                            slots.push(Slot::before(ui, g.id, 0, width));
                            ui.label(
                                RichText::new(
                                    "Drag hotkeys here, or pick this group when editing one.",
                                )
                                .size(12.0)
                                .color(theme::p().text_dim),
                            );
                        }
                        for (i, h) in members.iter().enumerate() {
                            slots.push(Slot::before(ui, g.id, i, width));
                            let last = i + 1 == members.len();
                            asked =
                                asked
                                    .take()
                                    .or(self.row(ui, state, h, Some(g), !last, actions));
                        }
                        if !members.is_empty() {
                            slots.push(Slot::before(ui, g.id, members.len(), width));
                        }
                    });
            });
        ui.add_space(4.0);
        asked
    }

    /// A group's header, across the top of its box: a grip to drag it by,
    /// its switch, name and how many hotkeys it has, and buttons to rename or
    /// remove it.
    fn group_header(
        &mut self,
        ui: &mut Ui,
        g: &HotkeyGroup,
        count: usize,
        actions: &mut Vec<Request>,
    ) {
        egui::Frame::new()
            .fill(theme::p().section_fill)
            .corner_radius(egui::CornerRadius {
                nw: 5,
                ne: 5,
                sw: 0,
                se: 0,
            })
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
            });
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

/// A place something dragged can land, and where the line showing it goes:
/// at `index` among the places in the list (`group` 0), or among a group's
/// hotkeys.
struct Slot {
    group: HotkeyGroupId,
    index: usize,
    y: f32,
    x: egui::Rangef,
}

impl Slot {
    /// The place just before what `ui` draws next.
    fn before(ui: &Ui, group: HotkeyGroupId, index: usize, x: egui::Rangef) -> Slot {
        Slot {
            group,
            index,
            y: ui.cursor().top() - ui.spacing().item_spacing.y / 2.0,
            x,
        }
    }
}

/// While something is dragged over the list, a line where it would land:
/// the place nearest the pointer that can take it, so that anywhere in the
/// list is somewhere. Groups go only among the places in the list, not into
/// other groups. Dropped, it is moved there.
fn drop_nearest(
    ui: &Ui,
    dragged: Dragged,
    slots: &[Slot],
    order: &[HotkeyListItem],
    info: &HotkeysInfo,
    actions: &mut Vec<Request>,
) {
    let Some(pointer) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    if !ui.clip_rect().expand2(vec2(24.0, 0.0)).contains(pointer) {
        return;
    }
    let takes = |s: &&Slot| s.group == 0 || matches!(dragged, Dragged::Hotkey(_));
    let Some(slot) = slots
        .iter()
        .filter(takes)
        .min_by(|a, b| (a.y - pointer.y).abs().total_cmp(&(b.y - pointer.y).abs()))
    else {
        return;
    };
    ui.painter().hline(
        slot.x,
        slot.y,
        egui::Stroke::new(3.0_f32, theme::p().accent),
    );
    if !ui.input(|i| i.pointer.any_released()) {
        return;
    }
    egui::DragAndDrop::clear_payload(ui.ctx());
    // Where it is now among what the place counts, if it is there at all.
    let from = match (dragged, slot.group) {
        (Dragged::Hotkey(id), 0) => order.iter().position(|i| *i == HotkeyListItem::Hotkey(id)),
        (Dragged::Group(id), _) => order.iter().position(|i| *i == HotkeyListItem::Group(id)),
        (Dragged::Hotkey(id), group) => info
            .hotkeys
            .iter()
            .filter(|h| h.group == group)
            .position(|h| h.id == id),
    };
    let to = match from {
        Some(from) => match widgets::drop_index(from, slot.index, false) {
            Some(to) => to,
            None => return,
        },
        None => slot.index,
    };
    actions.push(match dragged {
        Dragged::Hotkey(id) => Request::MoveHotkey(MoveHotkeyParams {
            hotkey: HotkeyKey::Id(id),
            group: from.is_none().then_some(HotkeyGroupKey::Id(slot.group)),
            index: Some(to),
        }),
        Dragged::Group(id) => Request::MoveHotkeyGroup(MoveHotkeyGroupParams {
            group: HotkeyGroupKey::Id(id),
            index: to,
        }),
    });
}

/// While something is dragged near the top or bottom of the list, scroll
/// it, so that a long list can be crossed in one drag. Only once `armed`,
/// when the pointer has been away from both edges in this drag: a drag
/// starting at an edge must not set the list going straight away.
fn scroll_at_edges(ui: &Ui, armed: &mut bool) {
    let Some(p) = ui.ctx().pointer_interact_pos() else {
        return;
    };
    let area = ui.clip_rect();
    if !area.x_range().contains(p.x) {
        return;
    }
    let edge = 24.0;
    let speed = if (area.top()..area.top() + edge).contains(&p.y) {
        6.0
    } else if (area.bottom() - edge..area.bottom()).contains(&p.y) {
        -6.0
    } else {
        *armed = true;
        return;
    };
    if !*armed {
        return;
    }
    ui.scroll_with_delta_animation(vec2(0.0, speed), egui::style::ScrollAnimation::none());
    ui.ctx().request_repaint();
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
        // Push to talk beeps, so you know you are live without looking.
        let mut talk = base(mic.id)
            .with(Action::PushToTalk)
            .hotkey("Push to talk", &["Ctrl+Alt+Space".into()]);
        talk.sounds = HotkeySounds {
            press: Some("Beep up".into()),
            release: Some("Beep down".into()),
            repeat: None,
        };
        out.push(talk);
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
        assert_eq!(all[0].sounds.press.as_deref(), Some("Beep up"));
        assert!(all[1].sounds.is_empty(), "only push to talk makes a sound");
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
