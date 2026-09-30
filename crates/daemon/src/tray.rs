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
use ksni::{MenuItem, OfflineReason, Status, ToolTip, Tray};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info};
use weir_protocol::{Startup, TrayIcon};

/// What the user picked from the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Bring the mixer window up, starting it if it is not running.
    Show,
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
                "Engine {}. {} strips, {} buses.",
                self.engine_state, self.strips, self.buses
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
        ]
    }
}
