//! The Preferences window and the About window.
//!
//! Preferences mixes two kinds of setting. Appearance, the bus source lists
//! and the application sliders belong to this window and are saved in its
//! own file ([`Prefs`]). Sample rate, buffer size, solo, what happens at
//! startup and the tray icon belong to the daemon, and changing them sends
//! a `set_settings` request.

use super::{bus_title, fmt_rate, frames_ms, App};
use crate::appearance;
use crate::prefs::{Appearance, BusSources, Prefs};
use crate::theme;
use egui::{RichText, Ui};
use weir_protocol::*;

/// A line of small dim text explaining the setting above it.
fn explain(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(theme::p().text_dim));
}

impl App {
    /// The Preferences and About windows, when open.
    pub(super) fn settings_windows(&mut self, ctx: &egui::Context, state: &FullState) {
        self.prefs_window(ctx, state);
        self.about_window(ctx);
    }

    fn prefs_window(&mut self, ctx: &egui::Context, state: &FullState) {
        if !self.show_prefs {
            return;
        }
        let mut open = true;
        let before = self.prefs.clone();
        egui::Window::new("Preferences")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                // Taller than a small mixer window, so it scrolls.
                let max_height = ctx.content_rect().height() - 60.0;
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .show(ui, |ui| {
                        self.appearance_prefs(ui);
                        ui.add_space(10.0);
                        self.audio_prefs(ui, state);
                        ui.add_space(10.0);
                        self.solo_prefs(ui, state);
                        ui.add_space(10.0);
                        self.layout_prefs(ui);
                        ui.add_space(10.0);
                        self.startup_prefs(ui, state);
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(format!(
                                "Window preferences are saved in {}",
                                Prefs::path().display()
                            ))
                            .size(10.0)
                            .color(theme::p().text_dim),
                        );
                    });
            });
        if self.prefs != before {
            self.prefs.save();
        }
        self.show_prefs = open;
    }

    /// Light or dark, and whether to take the desktop's accent color and
    /// font.
    fn appearance_prefs(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Appearance").strong());
        ui.horizontal(|ui| {
            for mode in Appearance::ALL {
                let r = ui.radio_value(&mut self.prefs.appearance, mode, mode.label());
                if mode == Appearance::System {
                    r.on_hover_text(
                        "Light or dark as set in your desktop's settings, switching when they do",
                    );
                }
            }
        });
        let known = appearance::accent().is_some();
        let r = ui.add_enabled(
            known,
            egui::Checkbox::new(
                &mut self.prefs.system_accent,
                "Use the desktop's accent color",
            ),
        );
        if known {
            r.on_hover_text("In place of Weir's teal, for highlights and selections");
        } else {
            r.on_disabled_hover_text("Your desktop has not said what its accent color is");
        }
        let font = appearance::desktop_font().map(|(name, _)| name.as_str());
        let r = ui.add_enabled(
            font.is_some(),
            egui::Checkbox::new(&mut self.prefs.system_font, "Use the desktop's font"),
        );
        match font {
            Some(name) => r.on_hover_text(format!(
                "Draw text in {name}. Some labels may not fit as neatly, since the \
                 mixer is laid out for its own font."
            )),
            None => r.on_disabled_hover_text("Could not find out which font your desktop uses"),
        };
    }

    /// Sample rate and buffer size: the daemon's settings, which it holds
    /// PipeWire at while it runs.
    fn audio_prefs(&mut self, ui: &mut Ui, state: &FullState) {
        let s = &state.settings;
        let running = state.engine.sample_rate;
        ui.label(RichText::new("Audio").strong());
        explain(
            ui,
            "Weir holds PipeWire at these only while it runs; PipeWire goes back to deciding \
             for itself when it stops.",
        );
        ui.add_space(4.0);
        let rate_text = |r: u32| {
            if r == 48_000 {
                "48 kHz (recommended)".to_string()
            } else {
                fmt_rate(r)
            }
        };
        egui::Grid::new("audio_prefs")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label("Sample rate");
                let current = s
                    .sample_rate
                    .map_or_else(|| "PipeWire decides".to_string(), rate_text);
                egui::ComboBox::from_id_salt("sample_rate")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        // 0 in a patch means "let PipeWire decide".
                        let choices = std::iter::once((None, "PipeWire decides".to_string()))
                            .chain(SAMPLE_RATES.iter().map(|&r| (Some(r), rate_text(r))));
                        for (v, text) in choices {
                            if ui.selectable_label(s.sample_rate == v, text).clicked() {
                                self.actions.push(Request::SetSettings(SettingsPatch {
                                    sample_rate: Some(v.unwrap_or(0)),
                                    ..Default::default()
                                }));
                            }
                        }
                    });
                ui.end_row();
                ui.label("Buffer size");
                ui.horizontal_wrapped(|ui| {
                    let rate = s
                        .sample_rate
                        .unwrap_or(if running > 0 { running } else { 48_000 });
                    let choices = QUANTA.iter().map(|&q| (Some(q), q.to_string()));
                    for (q, text) in choices.chain([(None, "PipeWire decides".to_string())]) {
                        let r = ui.selectable_label(s.quantum == q, text);
                        let r = match q {
                            Some(q) => {
                                r.on_hover_text(format!("{q} frames, {:.1} ms", frames_ms(q, rate)))
                            }
                            None => r,
                        };
                        if r.clicked() {
                            self.actions.push(Request::SetSettings(SettingsPatch {
                                quantum: Some(q.unwrap_or(0)),
                                ..Default::default()
                            }));
                        }
                    }
                });
                ui.end_row();
            });
        let rate_now = s.sample_rate.unwrap_or(running);
        if rate_now != 0 && rate_now != Denoise::SAMPLE_RATE {
            ui.label(
                RichText::new(format!(
                    "At {} noise suppression does not work: it only runs at 48 kHz, and \
                     passes audio through unchanged otherwise.",
                    fmt_rate(rate_now)
                ))
                .size(11.0)
                .color(theme::p().warning),
            );
        }
        explain(
            ui,
            "A smaller buffer is snappier when you listen to yourself, but costs more CPU and \
             may crackle on a busy system.",
        );
    }

    /// What soloing a strip does: silence the others everywhere, or only in
    /// one bus's mix.
    fn solo_prefs(&mut self, ui: &mut Ui, state: &FullState) {
        let s = &state.settings;
        ui.label(RichText::new("Solo").strong());
        let exclusive = s.solo == SoloMode::Exclusive;
        if ui
            .radio(exclusive, "Silence every other strip, in every mix")
            .clicked()
            && !exclusive
        {
            self.actions.push(Request::SetSettings(SettingsPatch {
                solo: Some(SoloMode::Exclusive),
                ..Default::default()
            }));
        }
        let cue_on = |bus: BusId| {
            Request::SetSettings(SettingsPatch {
                solo: Some(SoloMode::Cue(bus)),
                ..Default::default()
            })
        };
        ui.horizontal(|ui| {
            let buses = &state.mixer.buses;
            let cue_bus = match s.solo {
                SoloMode::Cue(b) => state.mixer.bus(b),
                SoloMode::Exclusive => None,
            };
            if ui
                .radio(!exclusive, "Cue: only change what you hear on")
                .on_hover_text(
                    "Soloing then only changes one mix, such as your headphones, so you can \
                     check a strip without your stream or recording hearing the difference.",
                )
                .clicked()
                && exclusive
            {
                if let Some(first) = buses.first() {
                    self.actions.push(cue_on(first.id));
                }
            }
            let text = cue_bus.map_or_else(|| "a bus".to_string(), |b| bus_title(&state.mixer, b));
            egui::ComboBox::from_id_salt("cue_bus")
                .selected_text(text)
                .show_ui(ui, |ui| {
                    for b in buses {
                        let picked = cue_bus.is_some_and(|c| c.id == b.id);
                        if ui
                            .selectable_label(picked, bus_title(&state.mixer, b))
                            .clicked()
                        {
                            self.actions.push(cue_on(b.id));
                        }
                    }
                });
        });
    }

    /// How much the mixer shows: the strips under each bus, and a volume
    /// slider for each application.
    fn layout_prefs(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Buses").strong());
        explain(
            ui,
            "Each bus can list the strips that feed it. The routing buttons on the strips \
             carry the same information, so this is optional.",
        );
        ui.add_space(4.0);
        for mode in BusSources::ALL {
            ui.radio_value(&mut self.prefs.bus_sources, mode, mode.label())
                .on_hover_text(mode.description());
        }
        ui.add_space(10.0);
        ui.label(RichText::new("Applications").strong());
        ui.checkbox(
            &mut self.prefs.show_app_volume,
            "Show a volume slider for each application",
        )
        .on_hover_text(
            "Adjusts the application's own volume, the same one the system volume applet \
             shows.",
        );
    }

    /// Whether Weir starts at login, what the daemon does about the window
    /// when it starts, and how its tray icon looks.
    fn startup_prefs(&mut self, ui: &mut Ui, state: &FullState) {
        let settings = &state.settings;
        ui.label(RichText::new("Starting Weir").strong());
        // systemd keeps this, so the box shows what the daemon read back
        // from it, and cannot be ticked where there is no service to start.
        let mut at_login = settings.start_at_login.unwrap_or(false);
        let r = ui.add_enabled(
            settings.start_at_login.is_some(),
            egui::Checkbox::new(&mut at_login, "Start Weir when I log in"),
        );
        let r = r
            .on_hover_text(
                "Weir's background service starts when you log in, so its devices are \
                 there before any application looks for them. It takes effect from your \
                 next login.",
            )
            .on_disabled_hover_text(
                "Weir's background service is not installed, so it cannot start at login. \
                 Installing Weir with the steps in its README adds it.",
            );
        if r.changed() {
            self.actions.push(Request::SetSettings(SettingsPatch {
                start_at_login: Some(Flag::Set(at_login)),
                ..Default::default()
            }));
        }
        ui.add_space(6.0);
        explain(
            ui,
            "When it starts, at login or when you open it from the application menu:",
        );
        ui.add_space(4.0);
        let mut startup = settings.startup;
        for mode in Startup::ALL {
            if ui.radio_value(&mut startup, mode, mode.label()).clicked()
                && startup != settings.startup
            {
                self.actions.push(Request::SetSettings(SettingsPatch {
                    startup: Some(startup),
                    ..Default::default()
                }));
            }
        }
        if !settings.tray {
            return;
        }
        ui.add_space(10.0);
        ui.label(RichText::new("Tray icon").strong());
        let mut icon = settings.tray_icon;
        for style in TrayIcon::ALL {
            if ui.radio_value(&mut icon, style, style.label()).clicked()
                && icon != settings.tray_icon
            {
                self.actions.push(Request::SetSettings(SettingsPatch {
                    tray_icon: Some(icon),
                    ..Default::default()
                }));
            }
        }
    }

    /// Weir's name, version and icon, how it was made and where it lives,
    /// and, small, the two details a bug report needs.
    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        let icon = self.about_icon(ctx);
        let mut open = true;
        egui::Window::new("About Weir")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(icon) = &icon {
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
                explain(ui, "Free software under the MIT license.");
                ui.add_space(10.0);
                explain(
                    ui,
                    &format!(
                        "Daemon socket: {}\nProtocol version {PROTOCOL_VERSION}",
                        self.socket.display()
                    ),
                );
            });
        self.show_about = open;
    }

    /// The window icon as a texture, made once. `None` if it could not be
    /// read, which leaves the About window without it rather than failing.
    fn about_icon(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.about_icon.is_none() {
            let icon = crate::icon();
            if icon.width == 0 {
                return None;
            }
            let size = [icon.width as usize, icon.height as usize];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, &icon.rgba);
            self.about_icon = Some(ctx.load_texture("about-icon", image, Default::default()));
        }
        self.about_icon.clone()
    }
}
