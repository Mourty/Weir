//! Undo and redo from the window, and the short messages ("toasts") at the
//! bottom of it that say what just happened.

use super::App;
use crate::theme;
use egui::{vec2, Frame, RichText};
use std::time::{Duration, Instant};
use weir_protocol::{HistoryStepParams, Request};

/// Which way a keyboard shortcut asked to go through the history.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryKey {
    Undo,
    Redo,
}

/// Ctrl+Z to undo, and Ctrl+Shift+Z or Ctrl+Y to redo, unless something is
/// being typed, where those belong to the text field.
pub(crate) fn history_shortcut(ctx: &egui::Context) -> Option<HistoryKey> {
    use egui::{Key, KeyboardShortcut, Modifiers};
    if ctx.wants_keyboard_input() {
        return None;
    }
    ctx.input_mut(|i| {
        // Shift+Z first: the plain one would also match it.
        if i.consume_shortcut(&KeyboardShortcut::new(
            Modifiers::COMMAND | Modifiers::SHIFT,
            Key::Z,
        )) || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Y))
        {
            Some(HistoryKey::Redo)
        } else if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)) {
            Some(HistoryKey::Undo)
        } else {
            None
        }
    })
}

/// A short message at the bottom of the window, optionally with an Undo
/// button.
pub(super) struct Toast {
    text: String,
    until: Instant,
    undo: bool,
}

impl App {
    /// Undo or redo one step, saying what it was.
    pub(super) fn step_history(&mut self, key: HistoryKey) {
        let (list, verb) = match key {
            HistoryKey::Undo => (&self.history.undo, "Undid"),
            HistoryKey::Redo => (&self.history.redo, "Redid"),
        };
        let Some(entry) = list.first() else {
            self.show_toast(
                match key {
                    HistoryKey::Undo => "Nothing to undo",
                    HistoryKey::Redo => "Nothing to redo",
                },
                false,
            );
            return;
        };
        let text = format!("{verb}: {}", entry.label);
        let params = HistoryStepParams::default();
        self.actions.push(match key {
            HistoryKey::Undo => Request::Undo(params),
            HistoryKey::Redo => Request::Redo(params),
        });
        self.show_toast(&text, false);
    }

    /// Undo the last `steps` changes at once, from the list of recent ones.
    pub(super) fn undo_steps(&mut self, steps: usize) {
        self.actions.push(Request::Undo(HistoryStepParams {
            steps: Some(steps as u32),
        }));
        let plural = if steps == 1 { "" } else { "s" };
        self.show_toast(&format!("Undid {steps} change{plural}"), false);
    }

    /// Show `text` for a few seconds, or longer with an Undo button beside
    /// it for changes that are easy to regret.
    pub(super) fn show_toast(&mut self, text: &str, undo: bool) {
        self.toast = Some(Toast {
            text: text.to_string(),
            until: Instant::now() + Duration::from_secs(if undo { 8 } else { 3 }),
            undo,
        });
    }

    /// Draw the toast, if one is showing.
    pub(super) fn toast_view(&mut self, ctx: &egui::Context) {
        let Some(t) = &self.toast else {
            return;
        };
        if Instant::now() > t.until {
            self.toast = None;
            return;
        }
        let (text, undo) = (t.text.clone(), t.undo);
        let mut clicked = false;
        egui::Area::new(egui::Id::new("toast"))
            .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -18.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(theme::p().button_off)
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(text).color(theme::p().text));
                            if undo {
                                ui.add_space(8.0);
                                clicked = ui
                                    .button(RichText::new("Undo").strong().color(theme::p().accent))
                                    .clicked();
                            }
                        });
                    });
            });
        if clicked {
            self.toast = None;
            self.step_history(HistoryKey::Undo);
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

/// How long ago a change was, for the list of recent changes.
pub(super) fn ago(at_ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    ago_from(now, at_ms)
}

/// How long before `now_ms` the moment `at_ms` was.
fn ago_from(now_ms: u64, at_ms: u64) -> String {
    let s = now_ms.saturating_sub(at_ms) / 1000;
    match s {
        0..=9 => "just now".to_string(),
        10..=59 => format!("{s} s ago"),
        60..=3599 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_ago_round_to_what_matters() {
        let now = 10_000_000;
        assert_eq!(ago_from(now, now - 3_000), "just now");
        assert_eq!(ago_from(now, now - 42_000), "42 s ago");
        assert_eq!(ago_from(now, now - 600_000), "10 min ago");
        assert_eq!(ago_from(now, now - 7_200_000), "2 h ago");
        assert_eq!(ago_from(now, now + 5_000), "just now", "clocks disagree");
    }
}
