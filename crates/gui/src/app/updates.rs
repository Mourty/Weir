//! New versions: the question asked once, whether Weir should say when one
//! is out; the window that says so, with what is new and how to update;
//! and the Updates part of Preferences. The daemon does the asking (see
//! `updates` in the protocol); Weir never installs anything.

use super::App;
use crate::theme;
use crate::transfer::Pane;
use egui::{Frame, RichText, Ui};
use weir_protocol::*;

/// The window's own memory of new versions, for this run.
#[derive(Default)]
pub(super) struct Updates {
    /// The version the person said to be reminded of later: not shown
    /// again until Weir starts again.
    later: Option<String>,
}

/// A request changing only the update settings.
fn set(update_check: Option<bool>, skipped_version: Option<Option<String>>) -> Request {
    Request::SetSettings(SettingsPatch {
        update_check: update_check.map(Flag::Set),
        skipped_version,
        ..Default::default()
    })
}

impl App {
    /// The question asked once, under the top bar: should Weir tell when a
    /// new version is out? Either answer is one click, and it is never
    /// asked again.
    pub(super) fn update_question(&mut self, ctx: &egui::Context, state: &FullState) {
        if state.settings.update_check.is_some() {
            return;
        }
        egui::TopBottomPanel::top("update_question")
            .frame(
                Frame::new()
                    .fill(theme::p().section_fill)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Should Weir tell you when a new version is out?");
                    ui.label(
                        RichText::new(
                            "Each time it starts it would ask GitHub for its newest release. \
                             It never installs anything.",
                        )
                        .color(theme::p().text_dim),
                    );
                    if ui.button("Yes, tell me").clicked() {
                        self.actions.push(set(Some(true), None));
                    }
                    if ui.button("No thanks").clicked() {
                        self.actions.push(set(Some(false), None));
                    }
                });
            });
    }

    /// The window saying a new version is out, while there is one the
    /// person wants to hear about.
    pub(super) fn update_window(&mut self, ctx: &egui::Context, state: &FullState) {
        let u = &state.update;
        let Some(latest) = u.latest.as_ref().filter(|_| u.newer) else {
            return;
        };
        let version = latest.version.as_str();
        let wanted = state.settings.update_check == Some(true)
            && state.settings.skipped_version.as_deref() != Some(version)
            && self.updates.later.as_deref() != Some(version);
        if !wanted {
            return;
        }
        let (mut raise, mut closed) = (false, false);
        let mut answer = None;
        crate::transfer::window(
            ctx,
            "new-version",
            "New version",
            &mut raise,
            &mut closed,
            |ui, pane| match pane {
                Pane::Body => {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| release_body(ui, latest, &u.how_to_update));
                }
                Pane::Footer => {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button("Remind me later")
                            .on_hover_text("Ask again the next time Weir starts")
                            .clicked()
                        {
                            answer = Some(Answer::Later);
                        }
                        if ui
                            .button("Skip this version")
                            .on_hover_text(format!(
                                "Say nothing more about {version}; tell about the next one"
                            ))
                            .clicked()
                        {
                            answer = Some(Answer::Skip);
                        }
                        if ui
                            .button("Never tell me again")
                            .on_hover_text(
                                "Stop asking GitHub. Preferences › General can start it again.",
                            )
                            .clicked()
                        {
                            answer = Some(Answer::Never);
                        }
                    });
                }
            },
        );
        // Closing the window is asking to be reminded later.
        if closed {
            answer = answer.or(Some(Answer::Later));
        }
        match answer {
            Some(Answer::Later) => self.updates.later = Some(version.to_string()),
            Some(Answer::Skip) => self
                .actions
                .push(set(None, Some(Some(version.to_string())))),
            Some(Answer::Never) => self.actions.push(set(Some(false), None)),
            None => {}
        }
    }

    /// The Updates part of Preferences: the switch, what the last check
    /// found, Check now, and a skipped version to hear about again.
    pub(super) fn update_prefs(&mut self, ui: &mut Ui, state: &FullState) {
        let s = &state.settings;
        let u = &state.update;
        let mut on = s.update_check == Some(true);
        if ui
            .checkbox(&mut on, "Tell me when a new version of Weir is out")
            .changed()
        {
            self.actions.push(set(Some(on), None));
            if on {
                // Asked to be told, the person wants to see it.
                self.updates.later = None;
            }
        }
        explain(
            ui,
            "Once each time it starts, Weir asks GitHub for its newest release. GitHub sees \
             your computer's internet address; nothing else is sent. Weir never installs \
             anything itself.",
        );
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let mine = env!("CARGO_PKG_VERSION");
            let (dot, text) = if u.checking {
                (theme::p().text_dim, "Checking…".to_string())
            } else if let Some(e) = &u.error {
                (theme::p().meter_yellow, format!("Could not check: {e}."))
            } else {
                match (&u.latest, u.checked_at_ms) {
                    (Some(l), Some(_)) if u.newer => (
                        theme::p().meter_yellow,
                        format!("Weir {} is out. You have {mine}.", l.version),
                    ),
                    (_, Some(at)) => (
                        theme::p().meter_green,
                        format!(
                            "You have the newest version, {mine}. Checked {}.",
                            utc_time(at)
                        ),
                    ),
                    (_, None) => (
                        theme::p().text_dim,
                        format!("You have {mine}. Not checked since Weir started."),
                    ),
                }
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 3.5, dot);
            ui.label(text);
            if ui
                .add_enabled(!u.checking, egui::Button::new("Check now"))
                .on_hover_text("Ask GitHub now, even with the box above unticked")
                .clicked()
            {
                self.actions.push(Request::CheckForUpdate);
                // Asked now, the answer should show even if it was put off.
                self.updates.later = None;
            }
            if u.newer {
                if let Some(l) = &u.latest {
                    ui.hyperlink_to("What is new", &l.url);
                }
            }
        });
        if let Some(v) = &s.skipped_version {
            ui.horizontal_wrapped(|ui| {
                explain(ui, &format!("You chose to skip {v}."));
                if ui.small_button("Tell me about it again").clicked() {
                    self.actions.push(set(None, Some(None)));
                }
            });
        }
    }
}

/// What the person said in the new version window.
enum Answer {
    Later,
    Skip,
    Never,
}

/// A line of small dim text.
fn explain(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(theme::p().text_dim));
}

/// The new version window's body: which version, what is new, and how to
/// update.
fn release_body(ui: &mut Ui, latest: &Release, how: &str) {
    ui.label(
        RichText::new(format!("Weir {} is out", latest.version))
            .size(20.0)
            .strong(),
    );
    let published = latest.published_at.get(..10).unwrap_or("");
    let mine = env!("CARGO_PKG_VERSION");
    ui.label(
        RichText::new(if published.is_empty() {
            format!("You have {mine}.")
        } else {
            format!("Published {published}. You have {mine}.")
        })
        .color(theme::p().text_dim),
    );
    ui.add_space(10.0);
    Frame::new()
        .fill(theme::p().section_fill)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("How to update").size(15.0).strong());
            ui.add_space(2.0);
            ui.label(format!("{how}."));
            ui.add_space(4.0);
            ui.hyperlink_to("Open the release page", &latest.url);
        });
    ui.add_space(10.0);
    ui.label(RichText::new("Release notes").size(15.0).strong());
    ui.add_space(2.0);
    notes(ui, &latest.notes);
}

/// Release notes, written in Markdown, drawn plainly: headings bold,
/// list items with a bullet, links as their text, the rest as it is.
fn notes(ui: &mut Ui, markdown: &str) {
    if markdown.trim().is_empty() {
        ui.label(RichText::new("The release has no notes.").color(theme::p().text_dim));
        return;
    }
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let indent = (line.len() - trimmed.len()) as f32 * 4.0;
        if trimmed.is_empty() {
            ui.add_space(4.0);
        } else if let Some(heading) = trimmed.strip_prefix('#') {
            ui.add_space(4.0);
            ui.label(RichText::new(plain(heading.trim_start_matches('#').trim())).strong());
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            ui.horizontal_top(|ui| {
                ui.add_space(indent);
                ui.label("•");
                ui.add(egui::Label::new(plain(item)).wrap());
            });
        } else {
            ui.add(egui::Label::new(plain(trimmed)).wrap());
        }
    }
}

/// A line of Markdown without its marks: `**bold**` and `` `code` `` as
/// their text, `[text](link)` as the text.
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find('[') {
        let Some(mid) = rest[start..].find("](") else {
            break;
        };
        let Some(end) = rest[start + mid..].find(')') else {
            break;
        };
        out.push_str(&rest[..start]);
        out.push_str(&rest[start + 1..start + mid]);
        rest = &rest[start + mid + end + 1..];
    }
    out.push_str(rest);
    out.replace("**", "").replace('`', "")
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn markdown_marks_come_off() {
        assert_eq!(
            plain("**Recorder**: see [the guide](https://x.y/z) and `weirctl record`"),
            "Recorder: see the guide and weirctl record"
        );
        assert_eq!(plain("a [b] c"), "a [b] c");
    }
}
