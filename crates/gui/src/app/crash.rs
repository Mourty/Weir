//! The crash window: what went wrong when Weir's window or part of its
//! service panicked, as a report to copy or send as a GitHub issue (see
//! `weir_protocol::crash`). It opens at once when a part of the service
//! or of the window stops while the window runs on, and at the next start
//! after the window, or the whole service, crashed.

use super::App;
use crate::theme;
use egui::RichText;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use weir_protocol::crash::{self, Locked, Report};
use weir_protocol::{Request, RestartParams};

/// Set to `window` or `window-part`, the window crashes on purpose a few
/// seconds after it opens: the window itself, or a thread of its own. For
/// trying the crash window and its report. The service has its own tests
/// on the same variable.
const CRASH_TEST: &str = "WEIR_CRASH_TEST";

/// The crash window's state.
#[derive(Default)]
pub(super) struct CrashUi {
    /// The reports to show, oldest first; the newest is shown.
    reports: Vec<Report>,
    /// Shown while Weir runs on (a part stopped), rather than at the next
    /// start.
    now: bool,
    /// Hide personal details: on each time the window opens.
    hide: bool,
    /// What the last button did, said under the buttons.
    note: Option<String>,
    /// Reports taken in already, so one heard of twice (from the service
    /// and from the file of reports waiting) shows once.
    known: HashSet<(String, String, String)>,
    /// Whether the service was connected last frame, to notice it going.
    connected: bool,
    /// A crash to make on purpose, and when.
    test: Option<(String, Instant)>,
}

impl CrashUi {
    /// Reports kept from before this run, shown when the window starts.
    pub(super) fn at_start(ctx: &egui::Context) -> Self {
        let wake = ctx.clone();
        crash::on_crash(move || wake.request_repaint());
        let mut ui = Self {
            hide: true,
            test: std::env::var(CRASH_TEST)
                .ok()
                .filter(|t| t.starts_with("window"))
                .map(|t| (t, Instant::now() + Duration::from_secs(3))),
            ..Default::default()
        };
        ui.take(crash::pending());
        ui
    }

    /// Take in `reports`, passing over any already taken.
    fn take(&mut self, reports: Vec<Report>) -> bool {
        let mut any = false;
        for r in reports {
            if self
                .known
                .insert((r.time.clone(), r.at.clone(), r.message.clone()))
            {
                if self.reports.is_empty() {
                    self.hide = true;
                    self.note = None;
                }
                self.reports.push(r);
                any = true;
            }
        }
        any
    }
}

impl App {
    /// Take in crashes that happened just now: in the window's own parts,
    /// in the service (`from_service`, as it told), or the whole service,
    /// whose report is waiting when it goes away.
    pub(super) fn crash_tick(&mut self, from_service: Vec<Report>, connected: bool) {
        if let Some((test, _)) = self.crash.test.take_if(|(_, at)| *at <= Instant::now()) {
            crash_on_purpose(&test);
        }
        let went = self.crash.connected && !connected;
        self.crash.connected = connected;
        let mut fresh = crash::take_fresh();
        fresh.extend(from_service);
        if self.crash.take(fresh) {
            self.crash.now = true;
        }
        // A service that crashed whole could not say so; its report waits
        // in the file.
        if went && self.crash.take(crash::pending()) {
            self.crash.now = true;
        }
    }

    pub(super) fn crash_window(&mut self, ctx: &egui::Context) {
        let Some(report) = self.crash.reports.last().cloned() else {
            return;
        };
        let p = theme::p();
        let now = self.crash.now;
        let earlier = self.crash.reports.len() - 1;
        let (text, hidden) = report.text(self.crash.hide);
        let service = report.program == "service";
        let mut open = true;
        let mut close = false;
        let mut restart = false;
        let mut safe = false;
        let title = match (now, report.whole) {
            (true, false) => "Part of Weir stopped",
            _ => "Weir crashed",
        };
        egui::Window::new(title)
            .id(egui::Id::unique("crash-window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(620.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(620.0);
                let big = match (now, report.whole, service) {
                    (true, false, _) => "Part of Weir stopped working",
                    (true, true, true) => "Weir's service stopped unexpectedly",
                    (false, true, false) => "Weir's window closed unexpectedly last time",
                    (false, true, true) => "Weir's service stopped unexpectedly last time",
                    (_, false, _) => "Part of Weir stopped working last time",
                    (true, true, false) => "Weir's window closed unexpectedly",
                };
                ui.label(RichText::new(big).size(20.0).strong());
                let when = if now && report.part == "audio" {
                    "Just now, in the part that mixes the sound. Weir is silent until \
                     it restarts; the rest of it is still running."
                        .to_string()
                } else if now && !report.whole {
                    format!(
                        "Just now, in its \"{}\" part. The rest of Weir is still running, \
                         but restarting it is the safest way to get everything working \
                         again.",
                        report.part
                    )
                } else {
                    format!(
                        "At {}. Your mixer and settings are as they were last saved.",
                        report.time
                    )
                };
                ui.label(RichText::new(when).color(p.text_dim));
                if earlier > 0 {
                    let times = if earlier == 1 {
                        "once".to_string()
                    } else {
                        format!("{earlier} times")
                    };
                    ui.label(
                        RichText::new(format!("It also happened {times} before that."))
                            .color(p.text_dim),
                    );
                }
                if !now && report.calls_for_safe_mode() && self.safe.on() {
                    ui.label(
                        RichText::new(
                            "So that it does not happen again straight away, Weir is in safe \
                             mode: the bar at the top says what it holds back.",
                        )
                        .color(p.warning),
                    );
                }
                ui.add_space(6.0);
                egui::Frame::new()
                    .fill(p.section_fill)
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("Help get it fixed").strong());
                        ui.label(
                            "This is a bug in Weir. The report below says what went wrong \
                             and where in Weir's code. It's ready to send as a GitHub \
                             issue, or to copy anywhere you like.",
                        );
                        ui.add_space(4.0);
                        ui.checkbox(&mut self.crash.hide, "Hide personal details");
                        if self.crash.hide {
                            hidden_chips(ui, &hidden);
                        } else {
                            ui.label(
                                RichText::new(
                                    "The report now shows your user name and the names of \
                                     your files and folders. GitHub issues are public, so \
                                     anyone can read what you send. Copy and Report on \
                                     GitHub use the report as shown.",
                                )
                                .color(p.warning),
                            );
                        }
                        ui.add_space(4.0);
                        egui::Frame::new()
                            .fill(p.bg)
                            .corner_radius(4)
                            .inner_margin(8)
                            .show(ui, |ui| {
                                egui::ScrollArea::both()
                                    .max_height(240.0)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&text).monospace().size(12.0),
                                            )
                                            .extend(),
                                        );
                                    });
                            });
                        hint(
                            ui,
                            &format!(
                                "The full crash log, with nothing hidden, stays on this \
                                 computer: {}",
                                crash::log_path().display()
                            ),
                        );
                    });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let github =
                        egui::Button::new(RichText::new("Report on GitHub").color(p.on_text))
                            .fill(p.accent);
                    if ui.add(github).clicked() {
                        let (link, fits) = crash::issue_link(
                            env!("CARGO_PKG_REPOSITORY"),
                            &report.title(self.crash.hide),
                            &text,
                        );
                        if !fits {
                            ui.ctx().copy_text(text.clone());
                        }
                        ui.ctx().open_url(egui::OpenUrl::new_tab(link));
                        self.crash.note = Some(
                            if fits {
                                "The report is filled in on GitHub. Nothing is sent until you \
                             press Create there."
                            } else {
                                "The report was too long for a link, so it's copied: paste it \
                             into the report box on GitHub."
                            }
                            .into(),
                        );
                    }
                    if ui.button("Copy report").clicked() {
                        ui.ctx().copy_text(text.clone());
                        self.crash.note = Some("Copied.".into());
                    }
                    if ui.button("Open the crash log").clicked() {
                        open_file(&crash::log_path());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                        if now && ui.button("Restart Weir").clicked() {
                            restart = true;
                        }
                        if now
                            && service
                            && ui
                                .button("Restart in safe mode")
                                .on_hover_text(
                                    "Start again with strips, buses and the parts most likely \
                                     to make Weir crash held back, to bring back one at a time",
                                )
                                .clicked()
                        {
                            (restart, safe) = (true, true);
                        }
                    });
                });
                if let Some(note) = &self.crash.note {
                    hint(ui, note);
                }
            });
        if !open || close || restart {
            let connected = self.crash.connected;
            let known = std::mem::take(&mut self.crash.known);
            self.crash = CrashUi {
                hide: true,
                known,
                connected,
                ..Default::default()
            };
            crash::seen();
        }
        if restart {
            match (service, self.crash.connected) {
                // The service starts again, and opens a new window: this
                // one is told to close as it goes.
                (true, true) => self
                    .actions
                    .push(Request::Restart(RestartParams { window: true, safe })),
                // The whole service went: start it again from here.
                (true, false) => {
                    let mut shared = self.client.shared.locked();
                    shared.start_service = true;
                    shared.safe_start = safe;
                }
                (false, _) => restart_window(ctx),
            }
        }
    }
}

/// Start the window again, as it was started, and close this one.
fn restart_window(ctx: &egui::Context) {
    match weir_protocol::own_exe() {
        Ok(exe) => {
            let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
            let started = std::process::Command::new(exe)
                .args(args)
                .env_remove(CRASH_TEST)
                .spawn();
            if let Err(e) = started {
                tracing::warn!("Weir's window could not be started again: {e}");
            }
        }
        Err(e) => tracing::warn!("Weir's window could not be started again: {e}"),
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

/// Crash as `test` asks: in a thread of the window's own, or the window
/// itself.
fn crash_on_purpose(test: &str) {
    if test == "window" {
        panic!("a crash on purpose ({CRASH_TEST}=window)");
    }
    let _ = std::thread::Builder::new()
        .name("crash test".into())
        .spawn(|| panic!("a crash on purpose ({CRASH_TEST}=window-part)"));
}

/// What the report hides, as chips.
fn hidden_chips(ui: &mut egui::Ui, hidden: &crash::Hidden) {
    ui.horizontal_wrapped(|ui| {
        if !hidden.any() {
            hint(ui, "Nothing personal was found in the report.");
            return;
        }
        hint(ui, "Hidden from the report:");
        if hidden.user {
            chip(ui, "your user name");
        }
        if hidden.names > 0 {
            chip(
                ui,
                &if hidden.names == 1 {
                    "1 file or folder name".to_string()
                } else {
                    format!("{} file or folder names", hidden.names)
                },
            );
        }
        if hidden.links > 0 {
            chip(
                ui,
                &if hidden.links == 1 {
                    "1 web address".to_string()
                } else {
                    format!("{} web addresses", hidden.links)
                },
            );
        }
    });
}

/// A small note in dim text.
fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(theme::p().text_dim));
}

/// A rounded label, for a thing named in a list.
fn chip(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(theme::p().button_off)
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0));
        });
}

/// Open `path` in the program the system opens such files with.
fn open_file(path: &std::path::Path) {
    match std::process::Command::new("xdg-open").arg(path).spawn() {
        Ok(mut child) => {
            // Waited for, so it leaves nothing behind when it is done.
            std::thread::spawn(move || child.wait());
        }
        Err(e) => tracing::warn!("could not open {}: {e}", path.display()),
    }
}

/// A logging layer that keeps the window's last log lines for crash
/// reports, whatever else is done with them.
pub(crate) struct LogLines;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LogLines {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = *event.metadata().level();
        if level > tracing::Level::INFO {
            return;
        }
        let mut text = Message::default();
        event.record(&mut text);
        crash::remember(level.as_str(), &text.0);
    }
}

/// An event's message, then its other fields as `name=value`.
#[derive(Default)]
struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.insert_str(0, &format!("{value:?}"));
        } else {
            self.0.push_str(&format!(" {}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.insert_str(0, value);
        } else {
            self.0.push_str(&format!(" {}={value}", field.name()));
        }
    }
}
