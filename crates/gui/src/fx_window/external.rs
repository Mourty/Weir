//! The external effects section: switching them on, the two devices to
//! connect the effects program to, where in the chain they go, and what
//! happens while nothing is connected. Every control here takes effect in
//! one click, so each sends its request straight away.

use super::section::{note, section_header_with, HeaderClick};
use super::{FxTarget, FxWindow, Section};
use crate::effects::{self, Status};
use crate::theme;
use egui::{RichText, Ui};
use weir_protocol::*;

/// Where a strip's or bus's external effects can go, in chain order.
pub(super) fn places(target: FxTarget) -> &'static [InsertPoint] {
    match target {
        FxTarget::Strip(_) => &InsertPoint::STRIP,
        FxTarget::Bus(_) => &InsertPoint::BUS,
    }
}

/// The name and external effects settings of `target`.
pub(super) fn settings(state: &FullState, target: FxTarget) -> Option<(&str, Insert)> {
    match target {
        FxTarget::Strip(id) => state.mixer.strip(id).map(|s| (s.name.as_str(), s.insert)),
        FxTarget::Bus(id) => state.mixer.bus(id).map(|b| (b.name.as_str(), b.insert)),
    }
}

impl FxWindow {
    /// Move the external effects to `at`, and keep their section in view
    /// as it moves down the side with them.
    pub(super) fn move_effects(&mut self, at: InsertPoint, actions: &mut Vec<Request>) {
        let patch = InsertPatch {
            position: Some(at),
            ..Default::default()
        };
        actions.push(effects::request(self.target, patch));
        self.focus = Some(Section::Insert);
    }

    pub(super) fn insert_section(
        &mut self,
        ui: &mut Ui,
        state: &FullState,
        open: &mut bool,
        actions: &mut Vec<Request>,
    ) {
        let Some((name, insert)) = settings(state, self.target) else {
            return;
        };
        let status = Status::of(state, self.target);
        // Where they are shows in the chain; the summary says what matters
        // at a glance, which is whether they are working.
        let summary = match status {
            Some(s) => s.short().to_string(),
            None => "off".to_string(),
        };
        let color = status.map_or(theme::p().ext_on, |s| s.color());
        let summary_color = match status {
            Some(s) if !s.connected => theme::p().warning,
            Some(_) => color,
            None => theme::p().text_dim,
        };
        match section_header_with(
            ui,
            "External effects",
            (&summary, summary_color),
            Some(insert.enabled),
            color,
            *open,
        ) {
            HeaderClick::Switch => {
                let patch = InsertPatch {
                    enabled: Some(Flag::from(!insert.enabled)),
                    ..Default::default()
                };
                actions.push(effects::request(self.target, patch));
            }
            HeaderClick::Fold => *open = !*open,
            HeaderClick::None => {}
        }
        if !*open {
            return;
        }
        let what = match self.target {
            FxTarget::Strip(_) => "sound",
            FxTarget::Bus(_) => "mix",
        };
        match status {
            Some(s) => {
                let color = if s.connected {
                    theme::p().text
                } else {
                    theme::p().warning
                };
                ui.label(RichText::new(s.explain(name)).color(color));
                ui.add_space(4.0);
                let (to_fx, back) = effects::device_names(name);
                egui::Grid::new(("insert_devices", self.target))
                    .num_columns(2)
                    .spacing([6.0, 2.0])
                    .show(ui, |ui| {
                        for (label, device, hint) in [
                            ("Out", to_fx, "The effects program records from this"),
                            ("Back", back, "The effects program plays into this"),
                        ] {
                            ui.label(RichText::new(label).color(theme::p().text_dim));
                            ui.add(
                                egui::Label::new(RichText::new(&device).strong())
                                    .truncate()
                                    .selectable(true),
                            )
                            .on_hover_text(format!("{device}\n{hint}"));
                            ui.end_row();
                        }
                    });
            }
            None => {
                ui.label(format!(
                    "Send the {what} to another program, such as Carla or EasyEffects, and \
                     bring it back to carry on through the rest of the chain. Switching on \
                     makes the two devices to connect it to."
                ));
            }
        }
        ui.add_space(4.0);
        let mut moved = None;
        ui.horizontal(|ui| {
            ui.label("Where");
            egui::ComboBox::from_id_salt(("insert_place", self.target))
                .selected_text(insert.position.label())
                .show_ui(ui, |ui| {
                    for &at in places(self.target) {
                        if ui
                            .selectable_label(at == insert.position, at.label())
                            .clicked()
                            && at != insert.position
                        {
                            moved = Some(at);
                        }
                    }
                });
        });
        if let Some(at) = moved {
            self.move_effects(at, actions);
        }
        ui.label("While nothing is connected:");
        for fallback in InsertFallback::ALL {
            let hint = match fallback {
                InsertFallback::PassThrough => "Carry on as if external effects were off",
                InsertFallback::Silence => "Be silent until the effects program plays",
            };
            if ui
                .radio(insert.fallback == fallback, fallback.label())
                .on_hover_text(hint)
                .clicked()
                && insert.fallback != fallback
            {
                let patch = InsertPatch {
                    fallback: Some(fallback),
                    ..Default::default()
                };
                actions.push(effects::request(self.target, patch));
            }
        }
        note(
            ui,
            "Connect them in the effects program's settings, or in a patchbay such as \
             qpwgraph. Drag Ext FX along the signal chain to move them. The trip out and \
             back adds a few milliseconds of delay.",
        );
    }
}
