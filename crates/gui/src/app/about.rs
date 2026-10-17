//! The About window: Weir's name, version and icon, where it lives and its
//! licenses, and help with reporting a problem. The details a bug report
//! needs come from the daemon (`diagnostics`) and this window (the
//! desktop), worded by [`bug_report`], shown to read over and change, then
//! copied or sent to GitHub's new issue page in the browser.

use super::App;
use crate::theme;
use crate::transfer::Pane;
use egui::{RichText, Ui};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use weir_protocol::*;

/// How long "Copied" shows after copying.
const COPIED_FOR: Duration = Duration::from_secs(3);

/// The About window's own state.
#[derive(Default)]
pub(super) struct About {
    open: bool,
    raise: bool,
    /// Weir's icon, made into a texture the first time the window opens.
    icon: Option<egui::TextureHandle>,
    /// A bug report's details, as shown and perhaps changed; `None` until
    /// the daemon answers.
    details: Option<String>,
    /// When the details were last copied.
    copied: Option<Instant>,
    /// Set when no folder of licenses was found, to say where they are
    /// instead.
    no_licenses: bool,
}

/// A line of small dim text.
fn explain(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(theme::p().text_dim));
}

impl App {
    /// Open the About window, or bring it forward. Opening it asks the
    /// daemon afresh for what a bug report needs.
    pub(super) fn open_about(&mut self) {
        if !self.about.open {
            self.about.details = None;
            self.about.no_licenses = false;
            self.actions.push(Request::Diagnostics);
        }
        self.about.open = true;
        self.about.raise = true;
    }

    /// The daemon said what a bug report needs: word it, with the desktop
    /// this window runs on, the home folder hidden.
    pub(super) fn take_diagnostics(&mut self, d: Diagnostics, state: &FullState) {
        let home = dirs::home_dir().map(|h| h.display().to_string());
        self.about.details = Some(bug_report(&d, state, &Desktop::from_env(), home.as_deref()));
    }

    /// The About window, when open.
    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about.open {
            return;
        }
        let icon = self.about_icon(ctx);
        let (mut raise, mut closed) = (self.about.raise, false);
        crate::transfer::window(
            ctx,
            "about",
            "About",
            &mut raise,
            &mut closed,
            |ui, pane| match pane {
                Pane::Body => {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| self.about_body(ui, icon.as_ref()));
                }
                Pane::Footer => {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.about.open = false;
                        }
                    });
                }
            },
        );
        self.about.raise = raise;
        if closed {
            self.about.open = false;
        }
    }

    fn about_body(&mut self, ui: &mut Ui, icon: Option<&egui::TextureHandle>) {
        ui.horizontal(|ui| {
            if let Some(icon) = icon {
                ui.add(egui::Image::new(icon).fit_to_exact_size(egui::vec2(64.0, 64.0)));
                ui.add_space(6.0);
            }
            ui.vertical(|ui| {
                ui.label(RichText::new("Weir").size(22.0).strong());
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label("A Voicemeeter-style audio mixer for Linux, built on PipeWire.");
            });
        });
        ui.add_space(10.0);
        ui.label("Made with Claude Code, Anthropic's AI coding tool.");
        let repo = env!("CARGO_PKG_REPOSITORY");
        ui.hyperlink_to(repo.trim_start_matches("https://"), repo)
            .on_hover_text("Weir's code, documentation and bug reports");
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("Free software under the MIT license.").color(theme::p().text_dim),
            );
            if ui
                .button("Licenses…")
                .on_hover_text("Weir's license, and those of the libraries it is built from")
                .clicked()
            {
                self.about.no_licenses = !open_licenses();
            }
        });
        if self.about.no_licenses {
            explain(
                ui,
                "This copy of Weir has no folder of licenses. They come with its source: \
                 LICENSE, and packaging/licenses for the libraries it is built from.",
            );
        }
        ui.add_space(14.0);
        self.report_card(ui);
        ui.add_space(10.0);
        explain(
            ui,
            &format!(
                "Daemon socket: {}\nProtocol version {PROTOCOL_VERSION}",
                self.socket.display()
            ),
        );
    }

    /// "Something not working?": the details a bug report needs, to read
    /// over, change, copy, or take to GitHub's new issue page.
    fn report_card(&mut self, ui: &mut Ui) {
        egui::Frame::new()
            .fill(theme::p().section_fill)
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Something not working?").size(15.0).strong());
                ui.add_space(4.0);
                let ready = self.about.details.is_some();
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(ready, egui::Button::new("Copy details for a bug report"))
                        .clicked()
                    {
                        if let Some(text) = &self.about.details {
                            ui.ctx().copy_text(text.clone());
                            self.about.copied = Some(Instant::now());
                        }
                    }
                    if ui
                        .add_enabled(ready, egui::Button::new("Report a problem…"))
                        .on_hover_text("Opens GitHub's page for a new issue in your browser")
                        .clicked()
                    {
                        if let Some(text) = &self.about.details {
                            let url = new_issue_url(env!("CARGO_PKG_REPOSITORY"), text);
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                        }
                    }
                    if self.about.copied.is_some_and(|t| t.elapsed() < COPIED_FOR) {
                        ui.label(RichText::new("Copied").color(theme::p().text_dim));
                    }
                });
                explain(
                    ui,
                    "Report a problem opens GitHub's issue page with these details filled \
                     in, for you to say what went wrong. Nothing is sent until you send it \
                     there. You need a GitHub account to report.",
                );
                ui.add_space(6.0);
                match &mut self.about.details {
                    Some(text) => {
                        explain(
                            ui,
                            "The details, as they will be copied. You can change anything.",
                        );
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(13)
                                .desired_width(f32::INFINITY),
                        );
                        explain(
                            ui,
                            "Your home folder's name is hidden as ~. Read it over before you \
                             share it.",
                        );
                    }
                    None => explain(ui, "Asking the service for the details…"),
                }
            });
    }

    /// The window icon as a texture, made once. `None` if it could not be
    /// read, which leaves the About window without it rather than failing.
    fn about_icon(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.about.icon.is_none() {
            let icon = crate::icon();
            if icon.width == 0 {
                return None;
            }
            let size = [icon.width as usize, icon.height as usize];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, &icon.rgba);
            self.about.icon = Some(ctx.load_texture("about-icon", image, Default::default()));
        }
        self.about.icon.clone()
    }
}

/// Open the folder of Weir's licenses in the file manager. Returns false
/// when there is none to open.
fn open_licenses() -> bool {
    let Some(folder) = licenses_folder() else {
        return false;
    };
    match std::process::Command::new("xdg-open").arg(&folder).spawn() {
        Ok(mut child) => {
            // Waited for, so it leaves nothing behind when it is done.
            std::thread::spawn(move || child.wait());
            true
        }
        Err(e) => {
            tracing::warn!("could not open {}: {e}", folder.display());
            false
        }
    }
}

/// Where Weir's licenses are: installed by a package under /usr, or by
/// `make install` in the home folder. The one beside this program first,
/// should both be there.
fn licenses_folder() -> Option<PathBuf> {
    let system = PathBuf::from("/usr/share/licenses/weir");
    let own = dirs::data_dir().map(|d| d.join("licenses").join("weir"));
    let packaged = std::env::current_exe().is_ok_and(|e| e.starts_with("/usr"));
    let mut candidates = Vec::new();
    if packaged {
        candidates.push(system);
        candidates.extend(own);
    } else {
        candidates.extend(own);
        candidates.push(system);
    }
    candidates.into_iter().find(|p| p.is_dir())
}
