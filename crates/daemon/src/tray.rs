//! The system tray icon.
//!
//! This lives in the daemon rather than the window, because the daemon is
//! what keeps running when the window is closed. A tray icon that vanished
//! with the window would be useless, and "start with no window at all" would
//! be impossible.
//!
//! The tray talks to the rest of the daemon through a channel. Menu callbacks
//! run on the tray's own task, so they must not touch the mixer directly.

use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Icon, MenuItem, OfflineReason, Status, ToolTip, Tray};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info};
use weir_protocol::{clock, Startup, TrayIcon};

/// What the user picked from the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Bring the mixer window up, starting it if it is not running.
    Show,
    /// Open the recorder's window, starting the mixer window if need be.
    ShowRecorder,
    /// Start recording what is ticked for it.
    StartRecording,
    /// Stop recording.
    StopRecording,
    /// Stop everything and start again.
    Restart,
    /// Stop everything.
    Exit,
    /// Change what happens the next time the daemon starts.
    SetStartup(Startup),
}

/// The tray icon and its menu. Its fields mirror the daemon's state; the
/// main loop updates them as that changes.
pub struct MixerTray {
    tx: UnboundedSender<TrayCommand>,
    /// Mirrored so the menu can show which option is selected.
    pub startup: Startup,
    /// Which icon to show.
    pub icon: TrayIcon,
    /// Mirrored for the tooltip.
    pub engine_state: String,
    /// How many strips there are, for the tooltip.
    pub strips: usize,
    /// How many buses there are, for the tooltip.
    pub buses: usize,
    /// While recording: for how long, in seconds, and how many tracks.
    pub recording: Option<(f64, usize)>,
    /// Whether any strip or bus is ticked for recording.
    pub can_record: bool,
}

impl MixerTray {
    /// A tray that sends what the user picks down `tx`.
    pub fn new(tx: UnboundedSender<TrayCommand>, startup: Startup, icon: TrayIcon) -> Self {
        Self {
            tx,
            startup,
            icon,
            engine_state: "starting".into(),
            strips: 0,
            buses: 0,
            recording: None,
            can_record: false,
        }
    }

    fn send(&self, command: TrayCommand) {
        // The receiver only goes away while the daemon is shutting down, at
        // which point there is nothing useful left to do anyway.
        let _ = self.tx.send(command);
    }
}

impl Tray for MixerTray {
    fn id(&self) -> String {
        "weir".into()
    }

    fn title(&self) -> String {
        "Weir".into()
    }

    /// Resolved from the icon theme. `make install` and the RPM both put
    /// `weir.svg` in `share/icons/hicolor/scalable/apps` and the one-color
    /// `weir-symbolic.svg` in `share/icons/hicolor/symbolic/apps`. Plasma
    /// recolors the one-color icon to the panel's text color through its
    /// `ColorScheme-Text` class, and GNOME does the same for any icon named
    /// `-symbolic`.
    fn icon_name(&self) -> String {
        match self.icon {
            TrayIcon::Color => "weir".into(),
            TrayIcon::OneColor => "weir-symbolic".into(),
        }
    }

    fn status(&self) -> Status {
        Status::Active
    }

    /// A red dot over the icon while recording.
    fn overlay_icon_pixmap(&self) -> Vec<Icon> {
        if self.recording.is_none() {
            return Vec::new();
        }
        [16, 32].into_iter().map(red_dot).collect()
    }

    /// The desktop has no tray right now: at login before the panel is up,
    /// or while the panel restarts. Keep waiting rather than give up, so
    /// the icon shows up with the panel.
    fn watcher_offline(&self, reason: OfflineReason) -> bool {
        info!("waiting for the desktop's tray to appear");
        debug!("tray unavailable: {reason:?}");
        true
    }

    fn watcher_online(&self) {
        info!("the desktop's tray is there; showing the tray icon");
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: self.icon_name(),
            icon_pixmap: Vec::new(),
            title: "Weir".into(),
            description: format!(
                "Engine {}. {} strips, {} buses.{}",
                self.engine_state,
                self.strips,
                self.buses,
                self.recording
                    .map(|r| format!(" {}.", recording_words(r)))
                    .unwrap_or_default()
            ),
        }
    }

    /// Left click opens the mixer, which is what people expect.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayCommand::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let selected = Startup::ALL
            .iter()
            .position(|s| *s == self.startup)
            .unwrap_or(0);
        vec![
            StandardItem {
                label: "Show the mixer".into(),
                icon_name: "weir".into(),
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::Show)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ]
        .into_iter()
        .chain(self.recording_items())
        .chain([
            SubMenu {
                label: "When Weir starts".into(),
                submenu: vec![RadioGroup {
                    selected,
                    select: Box::new(|this: &mut Self, index| {
                        if let Some(mode) = Startup::ALL.get(index) {
                            this.startup = *mode;
                            this.send(TrayCommand::SetStartup(*mode));
                        }
                    }),
                    options: Startup::ALL
                        .iter()
                        .map(|mode| RadioItem {
                            label: mode.label().into(),
                            ..Default::default()
                        })
                        .collect(),
                }
                .into()],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Restart Weir".into(),
                icon_name: "view-refresh".into(),
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::Restart)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit Weir".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::Exit)),
                ..Default::default()
            }
            .into(),
        ])
        .collect()
    }
}

impl MixerTray {
    /// While recording, how long and a way to stop it; otherwise a way to
    /// start, when anything is ticked. Then the recorder's window.
    fn recording_items(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = match self.recording {
            Some(r) => vec![
                StandardItem {
                    label: recording_words(r),
                    icon_name: "media-record".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Stop recording".into(),
                    icon_name: "media-playback-stop".into(),
                    activate: Box::new(|this: &mut Self| this.send(TrayCommand::StopRecording)),
                    ..Default::default()
                }
                .into(),
            ],
            None if self.can_record => vec![StandardItem {
                label: "Start recording".into(),
                icon_name: "media-record".into(),
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::StartRecording)),
                ..Default::default()
            }
            .into()],
            None => Vec::new(),
        };
        items.push(
            StandardItem {
                label: "Open the recorder".into(),
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::ShowRecorder)),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items
    }
}

/// "Recording · 12:34 · 3 tracks".
fn recording_words((seconds, tracks): (f64, usize)) -> String {
    let s = if tracks == 1 { "" } else { "s" };
    format!("Recording · {} · {tracks} track{s}", clock(seconds))
}

/// A red dot filling the lower right quarter of a `size` square, the
/// corner desktops put overlays in, ARGB in network order.
fn red_dot(size: i32) -> Icon {
    let r = size as f32 / 4.0;
    let (cx, cy) = (size as f32 - r - 0.5, size as f32 - r - 0.5);
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            // A soft edge, a pixel wide, and a thin dark ring so the dot
            // shows on a red icon or panel too.
            let inside = (r - d + 0.5).clamp(0.0, 1.0);
            let ring = (r + 1.0 - d + 0.5).clamp(0.0, 1.0);
            let (a, red, green, blue) = if inside > 0.0 {
                (
                    inside.max(ring),
                    235.0 * inside,
                    75.0 * inside,
                    65.0 * inside,
                )
            } else {
                (ring * 0.6, 0.0, 0.0, 0.0)
            };
            data.extend([(a * 255.0) as u8, red as u8, green as u8, blue as u8]);
        }
    }
    Icon {
        width: size,
        height: size,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recording_dot_is_red_in_the_corner_and_clear_elsewhere() {
        let icon = red_dot(16);
        assert_eq!(icon.data.len(), 16 * 16 * 4);
        let at = |x: usize, y: usize| &icon.data[(y * 16 + x) * 4..][..4];
        assert_eq!(at(0, 0)[0], 0, "clear at the top left");
        let middle = at(11, 11);
        assert!(middle[0] > 200 && middle[1] > 200 && middle[2] < 90);
        assert_eq!(recording_words((754.0, 3)), "Recording · 12:34 · 3 tracks");
        assert_eq!(recording_words((5.0, 1)), "Recording · 0:05 · 1 track");
    }
}
