//! The delay section of a bus: how long it holds its sound back.

use super::section::{note, section_header, HeaderClick};
use super::{FxWindow, Local};
use crate::theme;
use egui::Ui;
use weir_protocol::{Bus, BUS_DELAY_MAX_MS};

impl FxWindow {
    /// The delay, as a slider with a value to type in, and buttons to
    /// move it by a millisecond for lining up by ear.
    pub(super) fn delay_section(&mut self, ui: &mut Ui, bus: &Bus, open: &mut bool) {
        let mut delay = self.delay.as_ref().map_or(bus.delay_ms, |l| l.value);
        let summary = if delay > 0.0 {
            format!("{delay:.0} ms")
        } else {
            "off".to_string()
        };
        if let HeaderClick::Fold =
            section_header(ui, "Delay", &summary, None, theme::p().accent, *open)
        {
            *open = !*open;
        }
        if !*open {
            return;
        }
        let before = delay;
        ui.add(
            egui::Slider::new(&mut delay, 0.0..=BUS_DELAY_MAX_MS)
                .text("delay")
                .suffix(" ms")
                .step_by(1.0)
                .fixed_decimals(0),
        )
        .on_hover_text("How long this bus holds its sound back. Click the number to type a value.");
        ui.horizontal(|ui| {
            if ui
                .small_button("-1 ms")
                .on_hover_text("A millisecond shorter")
                .clicked()
            {
                delay = (delay.round() - 1.0).max(0.0);
            }
            if ui
                .small_button("+1 ms")
                .on_hover_text("A millisecond longer")
                .clicked()
            {
                delay = (delay.round() + 1.0).min(BUS_DELAY_MAX_MS);
            }
            if ui.small_button("Reset").on_hover_text("No delay").clicked() {
                delay = 0.0;
            }
        });
        if delay != before {
            self.delay = Some(Local::new(delay));
        }
        note(
            ui,
            "Holds this bus's sound back, to line it up with a bus that plays later, \
             such as a Bluetooth speaker, which is usually 100 to 250 ms behind. It is \
             the last thing in the bus, after its limiter and external effects.",
        );
    }
}
