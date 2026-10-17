//! Meter colors: which way of coloring meters the window draws with, and
//! the part of Preferences › Meters that picks, makes and edits them. The
//! daemon keeps them, as it keeps equalizer presets; which one is in use
//! is this window's own preference.

use super::transfer::One;
use super::App;
use crate::theme;
use crate::widgets::{self, MeterPaint};
use egui::{
    pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Response, RichText, Sense, Stroke, Ui,
};
use std::time::{Duration, Instant};
use weir_protocol::*;

/// How often edits are sent while a line is dragged or a color picked.
const SEND_EVERY: Duration = Duration::from_millis(250);
/// How long edits sent are kept over the daemon's copy, for its answer to
/// come back.
const ANSWER_TIME: Duration = Duration::from_secs(1);
/// The editor's meter: its height, and its width.
const EDIT_H: f32 = 360.0;
const EDIT_W: f32 = 80.0;
/// The scale left of it, and the column of line tabs right of it.
const EDIT_SCALE_W: f32 = 30.0;
const TAB_W: f32 = 76.0;
const TAB_H: f32 = 18.0;
/// How far a line is dragged sideways off the meter to remove it.
const DRAG_OFF: f32 = 60.0;
/// The round button in each band that changes its color.
const PENCIL_D: f32 = 20.0;

/// What the Meters page is in the middle of, besides editing.
#[derive(Debug, Clone, PartialEq)]
enum Asking {
    Nothing,
    /// A name for a copy of the colors shown.
    SaveAs(String),
    /// A new name for the colors shown.
    Rename(String),
    /// Whether to delete the colors shown.
    Delete,
}

/// The Meters page's own state.
#[derive(Debug)]
pub(super) struct MeterColorsPage {
    asking: Asking,
    /// The scheme being edited, as edited: drawn everywhere at once, and
    /// sent to the daemon now and then.
    editing: Option<MeterColors>,
    /// Edits not sent yet, and when the last were.
    unsent: bool,
    sent: Instant,
    /// The line being dragged.
    dragging: Option<usize>,
    /// The name field has just appeared and should take the keys.
    focus_name: bool,
}

impl Default for MeterColorsPage {
    fn default() -> Self {
        Self {
            asking: Asking::Nothing,
            editing: None,
            unsent: false,
            sent: Instant::now(),
            dragging: None,
            focus_name: false,
        }
    }
}

/// The scheme called `name`, or Weir's own.
fn scheme(state: &FullState, name: &str) -> MeterColors {
    state
        .meter_colors
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))
        .cloned()
        .unwrap_or_else(MeterColors::weir_default)
}

impl App {
    /// The colors meters are drawn in now: the scheme chosen in
    /// Preferences, as it is being edited.
    pub(super) fn meter_paint(&self, state: &FullState) -> MeterPaint {
        match self.newer_edits() {
            Some(c) => MeterPaint::of(c),
            None => MeterPaint::of(&scheme(state, &self.prefs.meter_colors)),
        }
    }

    /// The colors in use as edited, while that is newer than the daemon's
    /// copy: still being made, or sent so lately that its answer may not
    /// be back yet.
    fn newer_edits(&self) -> Option<&MeterColors> {
        let page = &self.meter_page;
        let busy = page.unsent || page.dragging.is_some() || page.sent.elapsed() < ANSWER_TIME;
        page.editing
            .as_ref()
            .filter(|e| busy && e.name.eq_ignore_ascii_case(&self.prefs.meter_colors))
    }

    /// The Meter colors part of Preferences › Meters.
    pub(super) fn meter_colors_prefs(&mut self, ui: &mut Ui, state: &FullState) {
        // Edits follow the daemon's copy, unless they are newer.
        if self.newer_edits().is_none() {
            self.meter_page.editing =
                Some(scheme(state, &self.prefs.meter_colors)).filter(|c| !c.builtin);
        }
        let shown = self
            .meter_page
            .editing
            .clone()
            .unwrap_or_else(|| scheme(state, &self.prefs.meter_colors));

        self.scheme_row(ui, state, &shown);
        ui.add_space(4.0);
        explain(
            ui,
            "Default is Weir's own and stays as it is. Yours travel with Export settings.",
        );
        ui.add_space(8.0);
        ui.horizontal_top(|ui| {
            let mut edited = self.meter_page.editing.clone();
            match edited.as_mut() {
                Some(c) => {
                    if editor(ui, c, &mut self.meter_page.dragging) {
                        self.meter_page.editing = Some(c.clone());
                        self.meter_page.unsent = true;
                    }
                }
                None => {
                    editor(ui, &mut shown.clone(), &mut None);
                }
            }
            ui.add_space(24.0);
            ui.vertical(|ui| {
                let paint = self.meter_paint(state);
                ui.label(RichText::new("Preview").color(theme::p().text_dim));
                ui.horizontal(|ui| {
                    for db in [-24.0, -8.0, 3.0] {
                        ui.vertical(|ui| {
                            widgets::meter(ui, &[db, db - 1.5], &[db, db - 1.5], 160.0, &paint);
                            ui.label(RichText::new(widgets::peak_text(db)).size(10.0));
                        });
                    }
                });
                ui.add_space(10.0);
                if self.meter_page.editing.is_some() {
                    for tip in [
                        "Drag a line to move it.",
                        "Click the meter to add a line there.",
                        "Drag a line off the meter, or right-click it, to remove it.",
                        "Click the pencil in a band to change its color.",
                    ] {
                        explain(ui, &format!("• {tip}"));
                    }
                } else {
                    explain(
                        ui,
                        "To change the colors, use Save as… to make a copy of your own, \
                         then change that.",
                    );
                }
            });
        });
        self.send_meter_edits(false);
    }

    /// Send what was edited, now and then while editing, and at once
    /// when `now`.
    fn send_meter_edits(&mut self, now: bool) {
        let page = &mut self.meter_page;
        if page.unsent && (now || page.sent.elapsed() >= SEND_EVERY) {
            if let Some(c) = &page.editing {
                self.actions.push(Request::SaveMeterColors(c.clone()));
            }
            page.unsent = false;
            page.sent = Instant::now();
        }
    }

    /// The scheme chooser and its buttons, or the question one of them
    /// asked.
    fn scheme_row(&mut self, ui: &mut Ui, state: &FullState, shown: &MeterColors) {
        let asking = self.meter_page.asking.clone();
        match asking {
            Asking::Nothing => {
                ui.horizontal_wrapped(|ui| {
                    egui::ComboBox::from_id_salt("meter-colors")
                        .selected_text(&shown.name)
                        .width(180.0)
                        .truncate()
                        .show_ui(ui, |ui| {
                            for c in &state.meter_colors {
                                if ui.selectable_label(c.name == shown.name, &c.name).clicked() {
                                    self.send_meter_edits(true);
                                    self.prefs.meter_colors = c.name.clone();
                                }
                            }
                        });
                    if ui.button("Save as…").clicked() {
                        self.meter_page.asking = Asking::SaveAs(free_name(state, &shown.name));
                        self.meter_page.focus_name = true;
                    }
                    ui.add_enabled_ui(!shown.builtin, |ui| {
                        if ui.button("Rename…").clicked() {
                            self.meter_page.asking = Asking::Rename(shown.name.clone());
                            self.meter_page.focus_name = true;
                        }
                        if ui.button("Export…").clicked() {
                            self.send_meter_edits(true);
                            self.export_one(ui.ctx(), One::MeterColors(shown.name.clone()));
                        }
                        if ui.button("Delete…").clicked() {
                            self.meter_page.asking = Asking::Delete;
                        }
                    });
                });
            }
            Asking::SaveAs(mut name) | Asking::Rename(mut name) => {
                let renaming = matches!(self.meter_page.asking, Asking::Rename(_));
                ui.horizontal_wrapped(|ui| {
                    ui.label(if renaming { "New name:" } else { "Save as:" });
                    let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(180.0));
                    if std::mem::take(&mut self.meter_page.focus_name) {
                        r.request_focus();
                    }
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let problem = name_problem(state, &name, renaming.then_some(&shown.name));
                    let ok = ui
                        .add_enabled(problem.is_none(), egui::Button::new("Save"))
                        .clicked();
                    let cancel = ui.button("Cancel").clicked();
                    if let Some(p) = &problem {
                        ui.label(RichText::new(p).color(theme::p().warning));
                    }
                    self.meter_page.asking = if renaming {
                        Asking::Rename(name.clone())
                    } else {
                        Asking::SaveAs(name.clone())
                    };
                    if (ok || enter) && problem.is_none() {
                        self.send_meter_edits(true);
                        let mut copy = self.meter_page.editing.clone().unwrap_or(shown.clone());
                        copy.name = name.trim().to_string();
                        copy.builtin = false;
                        self.actions.push(Request::SaveMeterColors(copy.clone()));
                        if renaming {
                            self.actions.push(Request::DeleteMeterColors(NameParams {
                                name: shown.name.clone(),
                            }));
                        }
                        self.prefs.meter_colors = name.trim().to_string();
                        self.meter_page.editing = Some(copy);
                        self.meter_page.sent = Instant::now();
                        self.meter_page.asking = Asking::Nothing;
                    } else if cancel {
                        self.meter_page.asking = Asking::Nothing;
                    }
                });
            }
            Asking::Delete => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!(
                        "Delete '{}'? Meters go back to Weir's own colors.",
                        shown.name
                    ));
                    if ui.button("Delete").clicked() {
                        self.actions.push(Request::DeleteMeterColors(NameParams {
                            name: shown.name.clone(),
                        }));
                        self.prefs.meter_colors = METER_COLORS_DEFAULT.into();
                        self.meter_page.editing = None;
                        self.meter_page.asking = Asking::Nothing;
                    }
                    if ui.button("Keep").clicked() {
                        self.meter_page.asking = Asking::Nothing;
                    }
                });
            }
        }
    }
}

/// A line of small dim text.
fn explain(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(theme::p().text_dim));
}

/// A name for a copy of `name` that nothing has: "Default 2".
fn free_name(state: &FullState, name: &str) -> String {
    (2..)
        .map(|n| format!("{} {n}", name.trim()))
        .find(|n| {
            !state
                .meter_colors
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(n))
        })
        .expect("some number is free")
}

/// What is wrong with `name` for meter colors; `keeping` is the name of
/// the ones being renamed, which may keep it in other letters.
fn name_problem(state: &FullState, name: &str, keeping: Option<&String>) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some("They need a name.".into());
    }
    if name.chars().count() > 40 {
        return Some("That is longer than 40 characters.".into());
    }
    let taken = state.meter_colors.iter().any(|c| {
        c.name.eq_ignore_ascii_case(name) && keeping.is_none_or(|k| !k.eq_ignore_ascii_case(name))
    });
    taken.then(|| "Some meter colors have that name already.".into())
}

/// A tall meter showing `c`'s bands, with a scale, a tab for each line and
/// a pencil in each band to change its color. Weir's own is only shown;
/// others can be edited. Returns whether it was.
fn editor(ui: &mut Ui, c: &mut MeterColors, dragging: &mut Option<usize>) -> bool {
    let editable = !c.builtin;
    let size = vec2(EDIT_SCALE_W + 6.0 + EDIT_W + 6.0 + TAB_W, EDIT_H);
    let (area, _) = ui.allocate_exact_size(size, Sense::hover());
    let meter = Rect::from_min_size(
        pos2(area.left() + EDIT_SCALE_W + 6.0, area.top()),
        vec2(EDIT_W, EDIT_H),
    );
    let y_of = |db: f32| widgets::meter_y(meter, db);
    // The level at a height, to the nearest half dB.
    let db_at = |y: f32| {
        let (top, bottom) = (y_of(GAIN_MAX_DB), y_of(GAIN_MIN_DB));
        let t = ((bottom - y) / (bottom - top).max(1.0)).clamp(0.0, 1.0);
        ((GAIN_MIN_DB + t * (GAIN_MAX_DB - GAIN_MIN_DB)) * 2.0).round() / 2.0
    };
    let p = theme::p();
    let painter = ui.painter().clone();
    let paint = MeterPaint::of(c);

    // The scale.
    for db in [12.0, 6.0, 0.0, -6.0, -12.0, -24.0, -36.0, -48.0] {
        let y = y_of(db);
        painter.hline(
            (meter.left() - 4.0)..=meter.left(),
            y,
            Stroke::new(1.0_f32, p.text_dim.linear_multiply(0.6)),
        );
        let text = if db > 0.0 {
            format!("+{db:.0}")
        } else {
            format!("{db:.0}")
        };
        painter.text(
            pos2(meter.left() - 7.0, y),
            Align2::RIGHT_CENTER,
            text,
            FontId::proportional(10.0),
            p.text_dim,
        );
    }

    // The bands.
    painter.rect_filled(meter, CornerRadius::same(4), p.track);
    for (lo, hi, color) in paint.bands() {
        let band = Rect::from_min_max(
            pos2(meter.left(), y_of(hi).max(meter.top())),
            pos2(meter.right(), y_of(lo).min(meter.bottom())),
        );
        painter.rect_filled(band, CornerRadius::ZERO, color);
    }

    let mut changed = false;

    // Clicking the meter adds a line there; the new band takes the color
    // of the one it splits, ready to change.
    if editable {
        let meter_r = ui.interact(meter, ui.id().with("meter-click"), Sense::click());
        if meter_r.clicked() && c.colors.len() < METER_BANDS_MAX {
            if let Some(pos) = meter_r.interact_pointer_pos() {
                let db = db_at(pos.y);
                let near = c
                    .breaks_db
                    .iter()
                    .any(|b| (b - db).abs() < METER_BREAK_GAP_DB);
                if !near && db > GAIN_MIN_DB && db < GAIN_MAX_DB {
                    let k = c.breaks_db.iter().take_while(|b| **b < db).count();
                    c.breaks_db.insert(k, db);
                    let color = c.colors[k].clone();
                    c.colors.insert(k, color);
                    changed = true;
                }
            }
        }
        meter_r.on_hover_text(if c.colors.len() < METER_BANDS_MAX {
            "Click to add a line here".to_string()
        } else {
            format!("{METER_BANDS_MAX} colors is the most a meter can have")
        });

        // A color button in each band tall enough for one.
        for k in 0..c.colors.len() {
            let lo = if k == 0 {
                GAIN_MIN_DB
            } else {
                c.breaks_db[k - 1]
            };
            let hi = c.breaks_db.get(k).copied().unwrap_or(GAIN_MAX_DB);
            let (top, bottom) = (y_of(hi), y_of(lo));
            if bottom - top < 22.0 {
                continue;
            }
            let at = Rect::from_center_size(
                pos2(meter.center().x, (top + bottom) / 2.0),
                vec2(PENCIL_D, PENCIL_D),
            );
            let [r, g, b] = parse_hex_color(&c.colors[k]).unwrap_or([128, 128, 128]);
            let mut color = Color32::from_rgb(r, g, b);
            let button = pencil_color_button(ui, ui.id().with(("band", k)), at, &mut color)
                .on_hover_text(format!(
                    "Click to change the color {}",
                    band_words(lo, hi, k == 0, k + 1 == c.colors.len())
                ));
            if button.changed() {
                c.colors[k] = hex_color([color.r(), color.g(), color.b()]);
                changed = true;
            }
        }
    }

    // A line and a tab for each break; the tab is dragged.
    let mut remove = None;
    for k in 0..c.breaks_db.len() {
        let db = c.breaks_db[k];
        let y = y_of(db);
        // Dark under light, to show on any color.
        painter.hline(
            meter.x_range(),
            y,
            Stroke::new(3.0_f32, Color32::from_black_alpha(150)),
        );
        painter.hline(meter.x_range(), y, Stroke::new(1.0_f32, Color32::WHITE));
        let tab = Rect::from_min_size(
            pos2(meter.right() + 6.0, y - TAB_H / 2.0),
            vec2(TAB_W, TAB_H),
        );
        let r = editable.then(|| {
            ui.interact(
                tab,
                ui.id().with(("meter-line", k)),
                Sense::click_and_drag(),
            )
        });
        let active = r.as_ref().is_some_and(|r| r.dragged() || r.hovered());
        painter.rect_filled(
            tab,
            CornerRadius::same(4),
            if active { p.hover_fill } else { p.section_fill },
        );
        painter.rect_stroke(
            tab,
            CornerRadius::same(4),
            Stroke::new(1.0_f32, if active { p.accent } else { p.grid_major }),
            egui::StrokeKind::Inside,
        );
        let text_x = if editable {
            // Grip dots.
            for (dx, dy) in [
                (0.0, -3.0),
                (4.0, -3.0),
                (0.0, 0.0),
                (4.0, 0.0),
                (0.0, 3.0),
                (4.0, 3.0),
            ] {
                painter.circle_filled(pos2(tab.left() + 6.0 + dx, y + dy), 0.9, p.text_dim);
            }
            tab.left() + 16.0
        } else {
            tab.left() + 6.0
        };
        painter.text(
            pos2(text_x, y),
            Align2::LEFT_CENTER,
            widgets::peak_text(db),
            FontId::monospace(10.0),
            p.text,
        );
        let Some(r) = r else { continue };
        let off = |pos: egui::Pos2| (pos.x - tab.center().x).abs() > DRAG_OFF + TAB_W;
        if r.drag_started() {
            *dragging = Some(k);
        }
        if r.dragged() {
            if let Some(pos) = r.interact_pointer_pos() {
                let lo = match k {
                    0 => GAIN_MIN_DB + METER_BREAK_GAP_DB,
                    _ => c.breaks_db[k - 1] + METER_BREAK_GAP_DB,
                };
                let hi = c
                    .breaks_db
                    .get(k + 1)
                    .map_or(GAIN_MAX_DB - METER_BREAK_GAP_DB, |b| b - METER_BREAK_GAP_DB);
                let new = db_at(pos.y).clamp(lo, hi);
                if new != db {
                    c.breaks_db[k] = new;
                    changed = true;
                }
                if off(pos) {
                    painter.text(
                        pos + vec2(8.0, -8.0),
                        Align2::LEFT_BOTTOM,
                        "Let go to remove this line",
                        FontId::proportional(11.0),
                        p.warning,
                    );
                }
            }
        }
        if r.drag_stopped() {
            *dragging = None;
            if r.interact_pointer_pos()
                .or_else(|| ui.ctx().pointer_latest_pos())
                .is_some_and(off)
            {
                remove = Some(k);
            }
            changed = true;
        }
        if r.secondary_clicked() {
            remove = Some(k);
        }
        r.on_hover_text(
            "Drag to move this line. Right-click, or drag it off sideways, to remove it.",
        );
    }
    if let Some(k) = remove {
        // The band above the line goes; the one below grows into it.
        c.breaks_db.remove(k);
        c.colors.remove(k + 1);
        changed = true;
    }
    changed
}

/// Where a band is, in words: "from -12 to 0 dB", "below -12 dB",
/// "from 0 dB up".
/// A round button with a pencil, drawn on a band of `color`, that opens
/// egui's color picker under it. A swatch of the band's own color would not
/// show on it, so it is dark and see-through with a white ring.
fn pencil_color_button(ui: &mut Ui, id: egui::Id, rect: Rect, color: &mut Color32) -> Response {
    let mut response = ui.interact(rect, id, Sense::click());
    let popup_id = id.with("popup");
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    let ring = if open || response.hovered() {
        theme::p().accent
    } else {
        Color32::WHITE
    };
    let painter = ui.painter();
    let center = rect.center();
    let radius = rect.width() / 2.0;
    painter.circle(
        center,
        radius - 1.0,
        Color32::from_rgba_unmultiplied(11, 13, 16, 140),
        Stroke::new(2.0_f32, ring),
    );
    // Dark under light, so the ring shows on a white band too.
    painter.circle_stroke(
        center,
        radius,
        Stroke::new(1.0_f32, Color32::from_black_alpha(140)),
    );
    // A pencil pointing down to the left, drawn in a box 24 units wide.
    let scale = 10.0 / 24.0;
    let at = |x: f32, y: f32| center + vec2((x - 12.0) * scale, (y - 12.0) * scale);
    painter.add(egui::Shape::closed_line(
        vec![
            at(4.0, 20.0),
            at(8.0, 20.0),
            at(19.0, 9.0),
            at(15.0, 5.0),
            at(4.0, 16.0),
        ],
        Stroke::new(1.3_f32, Color32::WHITE),
    ));
    egui::Popup::menu(&response)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.spacing_mut().slider_width = 275.0;
            if egui::widgets::color_picker::color_picker_color32(
                ui,
                color,
                egui::widgets::color_picker::Alpha::Opaque,
            ) {
                response.mark_changed();
            }
        });
    response
}

fn band_words(lo: f32, hi: f32, first: bool, last: bool) -> String {
    let db = |v: f32| {
        if v > 0.0 {
            format!("+{v}")
        } else {
            format!("{v}")
        }
    };
    match (first, last) {
        (true, true) => "of the whole meter".into(),
        (true, false) => format!("below {} dB", db(hi)),
        (false, true) => format!("from {} dB up", db(lo)),
        (false, false) => format!("from {} to {} dB", db(lo), db(hi)),
    }
}
