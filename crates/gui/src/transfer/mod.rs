//! Moving settings to another computer, or sharing them: the Export window
//! picks what goes in a `.zip`, and the Import window shows what a file
//! holds, what each thing would meet here, and how it comes in.

mod export;
mod import;

pub use export::ExportWindow;
pub use import::ImportWindow;

use crate::theme;
use egui::{RichText, Ui};
use weir_protocol::*;

/// Which part of a window to draw.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pane {
    /// The buttons along the bottom.
    Footer,
    /// Everything else.
    Body,
}

/// A window of its own, `title`, drawn by `draw` once for each [`Pane`];
/// inside the mixer window where the system gives no separate windows.
pub(crate) fn window(
    ctx: &egui::Context,
    key: &str,
    title: &str,
    raise: &mut bool,
    closed: &mut bool,
    mut draw: impl FnMut(&mut Ui, Pane),
) {
    let id = egui::ViewportId::from_hash_of(key);
    if std::mem::take(raise) {
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
    }
    let builder = egui::ViewportBuilder::default()
        .with_title(format!("{title} · Weir"))
        .with_app_id("weir")
        .with_icon(crate::icon())
        .with_inner_size([720.0, 620.0])
        .with_min_inner_size([520.0, 360.0]);
    ctx.show_viewport_immediate(id, builder, |ctx, class| {
        if class == egui::ViewportClass::Embedded {
            let mut open = true;
            egui::Window::new(title)
                .id(egui::Id::new((key, "embedded")))
                .open(&mut open)
                .default_size([680.0, 560.0])
                .show(ctx, |ui| {
                    draw(ui, Pane::Body);
                    ui.separator();
                    draw(ui, Pane::Footer);
                });
            if !open {
                *closed = true;
            }
            return;
        }
        egui::TopBottomPanel::bottom(egui::Id::new((key, "footer")))
            .frame(
                egui::Frame::new()
                    .fill(theme::p().bg)
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ctx, |ui| draw(ui, Pane::Footer));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::p().bg).inner_margin(16))
            .show(ctx, |ui| draw(ui, Pane::Body));
        if ctx.input(|i| i.viewport().close_requested()) {
            *closed = true;
        }
    });
}

/// A section of a list that folds away: a header with a box ticking or
/// unticking all `total` of its items, `ticked` of which are, its title,
/// and how many are ticked. Returns what the header box asks for.
pub(crate) fn section(
    ui: &mut Ui,
    id: egui::Id,
    title: &str,
    ticked: usize,
    total: usize,
    body: impl FnOnce(&mut Ui),
) -> Option<bool> {
    let mut asked = None;
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
        .show_header(ui, |ui| {
            let mut all = total > 0 && ticked == total;
            let some = ticked > 0 && ticked < total;
            let r = ui.add_enabled(
                total > 0,
                egui::Checkbox::without_text(&mut all).indeterminate(some),
            );
            if r.clicked() {
                // A box with some ticked ticks all of them.
                asked = Some(some || all);
            }
            ui.label(RichText::new(title).strong().size(15.0));
            ui.label(
                RichText::new(format!("{ticked} of {total}"))
                    .size(12.0)
                    .color(theme::p().text_dim),
            );
        })
        .body(|ui| {
            ui.add_space(2.0);
            body(ui);
            ui.add_space(4.0);
        });
    ui.add_space(4.0);
    asked
}

/// One item: a box to tick and its name, and a line of what it is, dim and
/// cut short to fit.
pub(crate) fn item_row(
    ui: &mut Ui,
    ticked: &mut bool,
    enabled: bool,
    name: &str,
    detail: &str,
) -> bool {
    ui.horizontal(|ui| {
        let changed = ui
            .add_enabled(
                enabled,
                egui::Checkbox::new(ticked, RichText::new(name).strong()),
            )
            .changed();
        if !detail.is_empty() {
            ui.add(
                egui::Label::new(RichText::new(detail).size(12.0).color(theme::p().text_dim))
                    .truncate(),
            )
            .on_hover_text(detail);
        }
        changed
    })
    .inner
}

/// Today, as `2026-10-10`, for file names.
pub(crate) fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    utc_stamp(secs).chars().take(10).collect()
}

/// A count in words: "1 scene", "3 scenes".
pub(crate) fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}
