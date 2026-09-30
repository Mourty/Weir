//! What every section down the side of the settings window is built from:
//! a header that folds it, and the small print under its controls.

use crate::theme;
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, Ui};

/// What a click on a section's header did.
pub(super) enum HeaderClick {
    None,
    /// Folded or unfolded the section.
    Fold,
    /// Switched its effect on or off.
    Switch,
}

/// The header of a folding section: a chevron, its name, a one-line summary
/// of what it is set to, and its on/off switch when `switch` says whether it
/// is on. Sections that are always at work, such as the fader, have none.
pub(super) fn section_header(
    ui: &mut Ui,
    title: &str,
    summary: &str,
    switch: Option<bool>,
    color: Color32,
    open: bool,
) -> HeaderClick {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 42.0), Sense::click());
    let on = switch.unwrap_or(true);
    let pill = Rect::from_center_size(pos2(rect.right() - 30.0, rect.center().y), vec2(44.0, 22.0));
    let pill_resp = switch.map(|on| {
        ui.interact(pill, resp.id.with("switch"), Sense::click())
            .on_hover_text(if on { "Switch off" } else { "Switch on" })
    });
    let on_pill = pill_resp.as_ref().is_some_and(|r| r.hovered());
    let painter = ui.painter();
    let bg = if resp.hovered() && !on_pill {
        theme::p().section_fill
    } else {
        theme::p().strip_bg
    };
    painter.rect_filled(rect, 6, bg);
    // The chevron is drawn: the fonts do not all have the triangles.
    let c = pos2(rect.left() + 12.0, rect.center().y);
    let chevron = if open {
        vec![
            pos2(c.x - 4.0, c.y - 2.0),
            pos2(c.x + 4.0, c.y - 2.0),
            pos2(c.x, c.y + 3.0),
        ]
    } else {
        vec![
            pos2(c.x - 2.0, c.y - 4.0),
            pos2(c.x + 3.0, c.y),
            pos2(c.x - 2.0, c.y + 4.0),
        ]
    };
    painter.add(egui::Shape::convex_polygon(
        chevron,
        theme::p().text_dim,
        Stroke::NONE,
    ));
    painter.text(
        pos2(rect.left() + 24.0, rect.top() + 13.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(14.0),
        theme::p().text,
    );
    let right = match switch {
        Some(_) => pill.left() - 6.0,
        None => rect.right() - 8.0,
    };
    let summary_rect = Rect::from_min_max(
        pos2(rect.left() + 24.0, rect.top() + 22.0),
        pos2(right, rect.bottom()),
    );
    ui.painter_at(summary_rect).text(
        pos2(summary_rect.left(), rect.top() + 30.0),
        Align2::LEFT_CENTER,
        summary,
        FontId::proportional(11.0),
        match switch {
            Some(true) => color,
            _ => theme::p().text_dim,
        },
    );
    if switch.is_some() {
        let fill = if on {
            color
        } else if on_pill {
            theme::p().hover_fill
        } else {
            theme::p().button_off
        };
        painter.rect_filled(pill, 11, fill);
        painter.text(
            pill.center(),
            Align2::CENTER_CENTER,
            if on { "On" } else { "Off" },
            FontId::proportional(12.0),
            if on {
                theme::p().lamp_on_text
            } else {
                theme::p().text
            },
        );
    }
    if pill_resp.is_some_and(|r| r.clicked()) {
        HeaderClick::Switch
    } else if resp.clicked() {
        HeaderClick::Fold
    } else {
        HeaderClick::None
    }
}

/// A few words at the end of a section on what it does.
pub(super) fn note(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).size(11.0).color(theme::p().text_dim));
}
