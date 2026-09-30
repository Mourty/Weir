//! The controls the mixer draws itself: the fader, the meter, the limiter's
//! ceiling handle, the pan slider, lights, and toggle and routing buttons.
//!
//! Faders and meters share one scale ([`db_to_t`]), from the bottom of the
//! fader's range to its top, so a meter's reading lines up with the fader
//! beside it.

use crate::theme;
use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Response, Sense, Stroke, Ui};
use weir_protocol::{Limiter, GAIN_MAX_DB, GAIN_MIN_DB};

/// Where a level sits on the shared scale: 0 at the bottom of the fader's
/// range, 1 at the top. Linear in dB, as on a mixing desk.
pub fn db_to_t(db: f32) -> f32 {
    ((db - GAIN_MIN_DB) / (GAIN_MAX_DB - GAIN_MIN_DB)).clamp(0.0, 1.0)
}

/// The level at a point on the shared scale.
fn t_to_db(t: f32) -> f32 {
    GAIN_MIN_DB + t.clamp(0.0, 1.0) * (GAIN_MAX_DB - GAIN_MIN_DB)
}

/// A vertical fader. Drag to move, scroll for 1 dB steps, double-click to
/// reset to 0 dB, hold Shift while dragging for fine control. `fill` colors
/// the track below the handle.
pub fn fader(ui: &mut Ui, value_db: &mut f32, height: f32, fill: Color32) -> Response {
    let width = theme::FADER_W;
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
    let handle_h = 16.0;
    let track_top = rect.top() + handle_h / 2.0;
    let track_bottom = rect.bottom() - handle_h / 2.0;
    let travel = (track_bottom - track_top).max(1.0);

    let mut changed = false;
    if response.double_clicked() {
        *value_db = 0.0;
        changed = true;
    } else if response.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let dy = response.drag_delta().y * if fine { 0.2 } else { 1.0 };
        let dt = -dy / travel;
        let new = t_to_db(db_to_t(*value_db) + dt);
        if new != *value_db {
            *value_db = new;
            changed = true;
        }
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            let step = if scroll > 0.0 { 1.0 } else { -1.0 };
            *value_db = (*value_db + step).clamp(GAIN_MIN_DB, GAIN_MAX_DB);
            changed = true;
        }
    }
    if changed {
        response.mark_changed();
    }

    let painter = ui.painter();
    let track = Rect::from_min_max(
        pos2(rect.center().x - 4.0, track_top),
        pos2(rect.center().x + 4.0, track_bottom),
    );
    painter.rect_filled(track, CornerRadius::same(3), theme::p().track);
    // Ticks.
    for db in [12.0, 0.0, -12.0, -24.0, -36.0, -48.0, -60.0] {
        let y = track_bottom - db_to_t(db) * travel;
        let long = db == 0.0;
        painter.hline(
            (track.right() + 2.0)..=(track.right() + if long { 8.0 } else { 5.0 }),
            y,
            Stroke::new(
                1.0_f32,
                if long {
                    theme::p().text_dim
                } else {
                    theme::p().button_off
                },
            ),
        );
    }
    let y = track_bottom - db_to_t(*value_db) * travel;
    // Filled part below the handle.
    let fill_rect = Rect::from_min_max(pos2(track.left(), y), pos2(track.right(), track_bottom));
    painter.rect_filled(fill_rect, CornerRadius::same(3), fill.linear_multiply(0.6));
    // Handle.
    let handle = Rect::from_center_size(pos2(rect.center().x, y), vec2(width - 4.0, handle_h));
    let handle_color = if response.dragged() || response.hovered() {
        theme::p().handle_hover
    } else {
        theme::p().fader_handle
    };
    painter.rect_filled(handle, CornerRadius::same(4), handle_color);
    painter.hline(
        handle.x_range().shrink(4.0),
        y,
        Stroke::new(1.5_f32, theme::p().track),
    );
    response.on_hover_text(format!(
        "{:+.1} dB\nDrag, scroll, double-click to reset, Shift for fine control",
        value_db
    ))
}

/// Width a meter takes for a given channel count, so callers can lay out
/// around it before drawing.
pub fn meter_width(channels: usize) -> f32 {
    let n = channels.max(1);
    let (bar_w, gap) = meter_metrics(n);
    n as f32 * (bar_w + gap) + gap
}

/// Room a meter leaves above and below its scale.
const METER_INSET: f32 = 8.0;

/// Where a level sits on a meter drawn in `rect`. Anything drawn next to a
/// meter uses this so it lines up with the meter's own scale.
pub fn meter_y(rect: Rect, db: f32) -> f32 {
    let top = rect.top() + METER_INSET;
    let bottom = rect.bottom() - METER_INSET;
    bottom - db_to_t(db) * (bottom - top).max(1.0)
}

/// The width of each bar of a meter with `n` channels, and the gap between
/// them: narrower bars once there are more than two.
fn meter_metrics(n: usize) -> (f32, f32) {
    if n <= 2 {
        (9.0, 2.0)
    } else {
        (6.0, 2.0)
    }
}

/// A multi-channel peak meter with peak-hold marks. Levels in dBFS.
pub fn meter(ui: &mut Ui, levels_db: &[f32], peaks_db: &[f32], height: f32) -> Response {
    let n = levels_db.len().max(1);
    let (bar_w, gap) = meter_metrics(n);
    let width = meter_width(n);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), theme::p().track);
    let top = rect.top() + METER_INSET;
    let bottom = rect.bottom() - METER_INSET;
    let y_of = |db: f32| meter_y(rect, db);
    let y_yellow = y_of(-12.0);
    let y_red = y_of(0.0);
    for (i, &lvl) in levels_db.iter().enumerate() {
        let x0 = rect.left() + gap + i as f32 * (bar_w + gap);
        let x1 = x0 + bar_w;
        let y = y_of(lvl);
        if y < bottom {
            // Green segment.
            let g_top = y.max(y_yellow);
            if g_top < bottom {
                painter.rect_filled(
                    Rect::from_min_max(pos2(x0, g_top), pos2(x1, bottom)),
                    CornerRadius::ZERO,
                    theme::p().meter_green,
                );
            }
            if y < y_yellow {
                let y_top = y.max(y_red);
                painter.rect_filled(
                    Rect::from_min_max(pos2(x0, y_top), pos2(x1, y_yellow)),
                    CornerRadius::ZERO,
                    theme::p().meter_yellow,
                );
            }
            if y < y_red {
                painter.rect_filled(
                    Rect::from_min_max(pos2(x0, y), pos2(x1, y_red)),
                    CornerRadius::ZERO,
                    theme::p().meter_red,
                );
            }
        }
        if let Some(&pk) = peaks_db.get(i) {
            if pk > GAIN_MIN_DB {
                let py = y_of(pk);
                let c = if pk >= 0.0 {
                    theme::p().meter_red
                } else if pk >= -12.0 {
                    theme::p().meter_yellow
                } else {
                    theme::p().meter_green
                };
                painter.hline(x0..=x1, py, Stroke::new(2.0_f32, c));
            }
        }
    }
    // Segment lines for the LED look.
    let mut y = bottom;
    while y > top {
        painter.hline(
            rect.x_range().shrink(1.0),
            y,
            Stroke::new(1.0_f32, theme::p().track.linear_multiply(0.9)),
        );
        y -= 4.0;
    }
    painter.hline(
        rect.x_range().shrink(1.0),
        y_red,
        Stroke::new(1.0_f32, theme::p().text_dim.linear_multiply(0.5)),
    );
    let text = levels_db
        .iter()
        .map(|l| {
            if *l <= GAIN_MIN_DB {
                "-inf".to_string()
            } else {
                format!("{l:.1}")
            }
        })
        .collect::<Vec<_>>()
        .join(" / ");
    response.on_hover_text(format!("{text} dBFS"))
}

/// The safety limiter's ceiling: a handle in a narrow column left of a bus
/// meter, pointing at the level the limiter holds the bus under. Drag it
/// anywhere on the meter's scale, scroll for half-decibel steps, and
/// double-click to put it back at -1 dB. While the limiter is turning the
/// bus down, an amber bar hangs from the handle, as long as the reduction on
/// the meter's scale. Give it the meter's height so the two share a scale.
pub fn ceiling_handle(
    ui: &mut Ui,
    ceiling_db: &mut f32,
    enabled: bool,
    reduction_db: f32,
    height: f32,
) -> Response {
    let (rect, mut response) =
        ui.allocate_exact_size(vec2(theme::CEILING_W, height), Sense::click_and_drag());
    let mut changed = false;
    if response.double_clicked() {
        *ceiling_db = Limiter::default().ceiling_db;
        changed = true;
    } else if response.dragged() {
        if let Some(p) = response.interact_pointer_pos() {
            let top = rect.top() + METER_INSET;
            let bottom = rect.bottom() - METER_INSET;
            let t = (bottom - p.y) / (bottom - top).max(1.0);
            let db = (t_to_db(t) * 2.0).round() / 2.0;
            if db != *ceiling_db {
                *ceiling_db = db;
                changed = true;
            }
        }
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            let step = if scroll > 0.0 { 0.5 } else { -0.5 };
            *ceiling_db = (*ceiling_db + step).clamp(GAIN_MIN_DB, GAIN_MAX_DB);
            changed = true;
        }
    }
    if changed {
        response.mark_changed();
    }

    let painter = ui.painter();
    let y = meter_y(rect, *ceiling_db);
    if enabled && reduction_db < -0.05 {
        let per_db = (rect.height() - 2.0 * METER_INSET) / (GAIN_MAX_DB - GAIN_MIN_DB);
        let end = (y + -reduction_db * per_db).min(rect.bottom() - METER_INSET);
        painter.rect_filled(
            Rect::from_min_max(pos2(rect.left() + 1.0, y), pos2(rect.left() + 4.0, end)),
            CornerRadius::same(1),
            theme::p().limit,
        );
    }
    let active = response.hovered() || response.dragged();
    let color = match (enabled, active) {
        (true, true) => theme::p().handle_hover,
        (true, false) => theme::p().limit,
        (false, true) => theme::p().text,
        (false, false) => theme::p().button_off.linear_multiply(1.6),
    };
    let tip = pos2(rect.right() - 1.0, y);
    let points = vec![
        tip,
        pos2(rect.left() + 3.0, y - 5.0),
        pos2(rect.left() + 3.0, y + 5.0),
    ];
    if enabled {
        painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    } else {
        painter.add(egui::Shape::closed_line(
            points,
            Stroke::new(1.0_f32, color),
        ));
    }
    let state = if !enabled {
        "Safety limiter off: nothing stops this bus going over 0 dB.".to_string()
    } else if reduction_db < -0.05 {
        format!(
            "Safety limiter: ceiling {:+.1} dB, turning the bus down by {:.1} dB",
            ceiling_db, -reduction_db
        )
    } else {
        format!("Safety limiter: ceiling {:+.1} dB", ceiling_db)
    };
    response.on_hover_text(format!(
        "{state}\nDrag to move the ceiling, scroll for 0.5 dB steps, double-click for -1 dB, \
         right-click to switch the limiter on or off"
    ))
}

/// A dashed line across a meter at the limiter's ceiling.
pub fn ceiling_line(ui: &Ui, meter: Rect, ceiling_db: f32) {
    let y = meter_y(meter, ceiling_db);
    ui.painter().extend(egui::Shape::dashed_line(
        &[pos2(meter.left() + 1.0, y), pos2(meter.right() - 1.0, y)],
        Stroke::new(1.0_f32, theme::p().limit),
        3.0,
        2.0,
    ));
}

/// A small labeled indicator light, lit in `color`. Clickable, for lights
/// that latch until cleared.
pub fn lamp(ui: &mut Ui, label: &str, lit: bool, color: Color32) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(theme::LAMP_W, theme::LAMP_H), Sense::click());
    let painter = ui.painter();
    let (fill, text) = if lit {
        (color, theme::p().lamp_on_text)
    } else {
        (theme::p().lamp_off, theme::p().lamp_off_text)
    };
    painter.rect_filled(rect, CornerRadius::same(3), fill);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(9.0),
        text,
    );
    response
}

/// Small horizontal pan / balance control. Double-click to center.
pub fn pan_slider(ui: &mut Ui, value: &mut f32, width: f32) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 14.0), Sense::click_and_drag());
    let mut changed = false;
    if response.double_clicked() {
        *value = 0.0;
        changed = true;
    } else if response.dragged() {
        let dx = response.drag_delta().x / (width - 12.0) * 2.0;
        let new = (*value + dx).clamp(-1.0, 1.0);
        if new != *value {
            *value = new;
            changed = true;
        }
    }
    if changed {
        response.mark_changed();
    }
    let painter = ui.painter();
    let track = rect.shrink2(vec2(6.0, 5.0));
    painter.rect_filled(track, CornerRadius::same(2), theme::p().track);
    painter.vline(
        track.center().x,
        track.y_range(),
        Stroke::new(1.0_f32, theme::p().text_dim),
    );
    let x = track.left() + (*value + 1.0) / 2.0 * track.width();
    painter.circle_filled(
        pos2(x, rect.center().y),
        5.0,
        if response.hovered() {
            theme::p().handle_hover
        } else {
            theme::p().fader_handle
        },
    );
    painter.text(
        pos2(rect.left() + 1.0, rect.center().y),
        Align2::LEFT_CENTER,
        "L",
        FontId::proportional(9.0),
        theme::p().text_dim,
    );
    painter.text(
        pos2(rect.right() - 1.0, rect.center().y),
        Align2::RIGHT_CENTER,
        "R",
        FontId::proportional(9.0),
        theme::p().text_dim,
    );
    let label = if value.abs() < 0.005 {
        "center".to_string()
    } else if *value < 0.0 {
        format!("{:.0}% left", -*value * 100.0)
    } else {
        format!("{:.0}% right", *value * 100.0)
    };
    response.on_hover_text(format!("Pan: {label}\nDrag, double-click to center"))
}

/// A compact on/off button with a colored "on" state.
pub fn toggle(ui: &mut Ui, label: &str, on: bool, on_color: Color32, size: egui::Vec2) -> Response {
    let text = egui::RichText::new(label).size(11.0).strong().color(if on {
        theme::p().on_text
    } else {
        theme::p().text
    });
    let fill = if on { on_color } else { theme::p().button_off };
    ui.add(
        egui::Button::new(text)
            .fill(fill)
            .min_size(size)
            .corner_radius(4),
    )
}

/// The second line of a routing button.
pub enum RouteNote {
    /// The strip's level in that mix, when it is not 0 dB.
    Level(String),
    /// How far ducking is turning the strip down in that mix right now, in
    /// dB (a positive number).
    Ducked(f32),
}

/// A routing button: the bus's short name, and underneath it a
/// [`RouteNote`] when there is something to say. `ring` outlines it, for the
/// bus whose mix the faders are showing or one being ducked.
pub fn route_button(
    ui: &mut Ui,
    label: &str,
    note: Option<RouteNote>,
    on: bool,
    on_color: Color32,
    size: egui::Vec2,
    ring: Option<Color32>,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    let fill = if on {
        on_color
    } else if response.hovered() {
        theme::p().hover_fill
    } else {
        theme::p().button_off
    };
    painter.rect_filled(rect, CornerRadius::same(4), fill);
    if let Some(ring) = ring {
        painter.rect_stroke(
            rect.expand(1.0),
            CornerRadius::same(5),
            Stroke::new(2.0_f32, ring),
            egui::StrokeKind::Outside,
        );
    }
    let color = if on {
        theme::p().on_text
    } else {
        theme::p().text
    };
    match note {
        Some(note) => {
            painter.text(
                pos2(rect.center().x, rect.top() + rect.height() * 0.32),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(11.0),
                color,
            );
            let y = rect.top() + rect.height() * 0.72;
            match note {
                RouteNote::Level(text) => {
                    painter.text(
                        pos2(rect.center().x, y),
                        Align2::CENTER_CENTER,
                        text,
                        FontId::proportional(9.5),
                        color,
                    );
                }
                RouteNote::Ducked(db) => {
                    // A drawn arrow: the fonts do not all have one.
                    let text = format!("{db:.0}");
                    let galley = painter.layout_no_wrap(text, FontId::proportional(9.5), color);
                    let arrow_w = 7.0;
                    let left = rect.center().x - (arrow_w + 2.0 + galley.size().x) / 2.0;
                    let ax = left + arrow_w / 2.0;
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            pos2(ax - 3.5, y - 2.5),
                            pos2(ax + 3.5, y - 2.5),
                            pos2(ax, y + 3.0),
                        ],
                        color,
                        Stroke::NONE,
                    ));
                    painter.galley(
                        pos2(left + arrow_w + 2.0, y - galley.size().y / 2.0),
                        galley,
                        color,
                    );
                }
            }
        }
        None => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(11.0),
                color,
            );
        }
    }
    response
}

/// A level for a route button: whole dB where it is whole, else a tenth.
pub fn short_db(db: f32) -> String {
    let sign = if db < 0.0 { "−" } else { "+" };
    let a = db.abs();
    if (a - a.round()).abs() < 0.05 {
        format!("{sign}{a:.0} dB")
    } else {
        format!("{sign}{a:.1} dB")
    }
}

/// A compact horizontal volume control, used for application streams where
/// there is no room for a full fader. Double-click resets to 0 dB.
pub fn mini_volume(ui: &mut Ui, value_db: &mut f32, width: f32, muted: bool) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 12.0), Sense::click_and_drag());
    let mut changed = false;
    if response.double_clicked() {
        *value_db = 0.0;
        changed = true;
    } else if response.dragged() {
        let dt = response.drag_delta().x / (width - 8.0).max(1.0);
        let new = t_to_db(db_to_t(*value_db) + dt);
        if new != *value_db {
            *value_db = new;
            changed = true;
        }
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            *value_db =
                (*value_db + if scroll > 0.0 { 1.0 } else { -1.0 }).clamp(GAIN_MIN_DB, GAIN_MAX_DB);
            changed = true;
        }
    }
    if changed {
        response.mark_changed();
    }

    let painter = ui.painter();
    let track = rect.shrink2(vec2(0.0, 4.0));
    painter.rect_filled(track, CornerRadius::same(2), theme::p().track);
    let t = db_to_t(*value_db);
    let fill_w = (track.width() * t).max(0.0);
    if fill_w > 0.0 {
        let fill = Rect::from_min_size(track.min, vec2(fill_w, track.height()));
        let color = if muted {
            theme::p().text_dim
        } else {
            theme::p().fader.linear_multiply(0.8)
        };
        painter.rect_filled(fill, CornerRadius::same(2), color);
    }
    let x = track.left() + fill_w;
    painter.circle_filled(
        pos2(x.clamp(track.left(), track.right()), rect.center().y),
        4.0,
        if response.hovered() {
            theme::p().handle_hover
        } else {
            theme::p().fader_handle
        },
    );
    response.on_hover_text(format!(
        "{:+.1} dB{}\nDrag, scroll, double-click to reset",
        value_db,
        if muted { " (muted)" } else { "" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_runs_the_fader_range() {
        assert_eq!(db_to_t(GAIN_MIN_DB), 0.0);
        assert_eq!(db_to_t(GAIN_MAX_DB), 1.0);
        assert_eq!(db_to_t(GAIN_MAX_DB + 10.0), 1.0);
        assert!((t_to_db(db_to_t(-18.0)) + 18.0).abs() < 1e-4);
    }

    #[test]
    fn route_levels_are_short() {
        assert_eq!(short_db(-3.0), "−3 dB");
        assert_eq!(short_db(1.5), "+1.5 dB");
        assert_eq!(short_db(-6.04), "−6 dB");
    }

    #[test]
    fn meters_widen_with_their_channels() {
        assert!(meter_width(2) > meter_width(1));
        assert!(meter_width(8) > meter_width(2));
        assert_eq!(meter_width(0), meter_width(1));
    }
}
