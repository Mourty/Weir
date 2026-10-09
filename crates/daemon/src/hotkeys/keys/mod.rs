//! Getting hotkeys' keys to the runner, wherever the focus is.
//!
//! On Wayland no program may watch the keyboard, so Weir asks the desktop's
//! shortcut service (the XDG desktop portal's GlobalShortcuts, which KDE
//! Plasma, GNOME 48 and newer and Hyprland have): Weir suggests the keys,
//! the desktop decides, and tells Weir when they go down and up. On X11,
//! Weir grabs the keys itself. Anywhere else, hotkeys can only be pressed
//! by name.
//!
//! `WEIR_HOTKEYS=desktop`, `x11` or `none` picks the way by hand, for
//! testing.

mod portal;
mod x11;

use super::Command;
use crate::controller::Controller;
use crate::display;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;
use tracing::{debug, info, warn};
use weir_protocol::*;

/// A hotkey whose keys should work.
#[derive(Debug, Clone, PartialEq)]
pub struct Registration {
    pub id: HotkeyId,
    pub name: String,
    pub keys: KeyCombo,
}

/// The hotkeys whose keys should work: the enabled ones with keys.
fn registrations(hotkeys: &[Hotkey]) -> Vec<Registration> {
    hotkeys
        .iter()
        .filter(|h| h.enabled)
        .filter_map(|h| {
            let keys = KeyCombo::parse(h.keys.as_deref()?).ok()?;
            Some(Registration {
                id: h.id,
                name: h.name.clone(),
                keys,
            })
        })
        .collect()
}

/// Which way keys reach Weir here.
#[derive(Debug, Clone, PartialEq)]
enum Way {
    Desktop,
    X11 {
        display: String,
        xauthority: Option<String>,
    },
    Unavailable,
}

/// How long to wait between looks for the desktop, at login.
const DESKTOP_POLL: Duration = Duration::from_secs(2);
/// How long to wait before asking for the desktop's shortcut service again,
/// and how many times to ask before deciding there is none: at login it
/// may start after Weir.
const PORTAL_RETRY: Duration = Duration::from_secs(3);
const PORTAL_TRIES: u32 = 10;
/// The longest wait before connecting again to a shortcut service that
/// failed.
const PORTAL_RETRY_MAX: Duration = Duration::from_secs(60);

/// The status for `method`, with its sentence.
pub fn status(method: KeysMethod, message: impl Into<String>) -> KeysStatus {
    KeysStatus {
        method,
        message: message.into(),
        assigned: BTreeMap::new(),
    }
}

/// The sentence for hotkeys that only work by name.
const UNAVAILABLE: &str = "Your desktop does not let programs set their own hotkeys, so \
     hotkeys work only by name here: add a shortcut in your desktop's keyboard settings that \
     runs weirctl hotkey run \"NAME\".";

/// Keep the keys of every hotkey working for as long as the daemon runs.
pub async fn run(controller: Arc<Controller>, tx: UnboundedSender<Command>) {
    let (regs_tx, regs_rx) = watch::channel(registrations(&controller.hotkeys()));
    // Follow the hotkeys as they change.
    {
        let controller = controller.clone();
        let mut notes = controller.subscribe();
        tokio::spawn(async move {
            loop {
                let hotkeys = match notes.recv().await {
                    Ok(Notification::HotkeysChanged(info)) => info.hotkeys,
                    Err(RecvError::Lagged(_)) => controller.hotkeys(),
                    Err(RecvError::Closed) => break,
                    Ok(_) => continue,
                };
                let now = registrations(&hotkeys);
                regs_tx.send_if_modified(|regs| {
                    let changed = *regs != now;
                    if changed {
                        *regs = now;
                    }
                    changed
                });
            }
        });
    }

    let (way, desktop) = find_way(&controller).await;
    info!("hotkeys: {way:?}");
    match way {
        Way::Desktop => through_desktop(&controller, &tx, regs_rx, desktop.as_deref()).await,
        Way::X11 {
            display,
            xauthority,
        } => {
            if let Err(e) = x11::run(&controller, &tx, regs_rx, &display, xauthority).await {
                warn!("could not watch keys on X11: {e}");
                controller.set_keys_status(
                    status(
                        KeysMethod::Unavailable,
                        format!("Weir could not watch the keys on this desktop: {e}"),
                    ),
                    BTreeMap::new(),
                );
            }
        }
        Way::Unavailable => controller.set_keys_status(
            status(KeysMethod::Unavailable, UNAVAILABLE),
            BTreeMap::new(),
        ),
    }
}

/// Keep hotkeys going through the desktop's shortcut service, connecting
/// again when it fails, until the daemon stops or it turns out there is
/// none.
async fn through_desktop(
    controller: &Controller,
    tx: &UnboundedSender<Command>,
    regs: watch::Receiver<Vec<Registration>>,
    desktop: Option<&str>,
) {
    let (mut missing, mut failing, mut wait) = (0, 0, PORTAL_RETRY);
    loop {
        let started = Instant::now();
        match portal::run(controller, tx, regs.clone(), desktop).await {
            Ok(()) => return,
            Err(portal::Failure::Missing(e)) => {
                missing += 1;
                if missing >= PORTAL_TRIES {
                    warn!("the desktop has no shortcut service: {e}");
                    controller.set_keys_status(
                        status(KeysMethod::Unavailable, UNAVAILABLE),
                        BTreeMap::new(),
                    );
                    return;
                }
                debug!("no shortcut service yet: {e}");
                wait = PORTAL_RETRY;
            }
            Err(portal::Failure::Lost(e)) => {
                missing = 0;
                // After a long while working, a failure is a new one.
                if started.elapsed() > PORTAL_RETRY_MAX {
                    failing = 0;
                }
                failing += 1;
                warn!("the desktop's shortcut service failed; trying again: {e}");
                let (method, message) = if failing > 3 {
                    (
                        KeysMethod::Unavailable,
                        format!(
                            "Hotkeys are not working: the desktop's shortcut service keeps \
                             failing ({e}). Weir keeps trying."
                        ),
                    )
                } else {
                    (KeysMethod::Starting, "Weir is reconnecting hotkeys.".into())
                };
                controller.set_keys_status(status(method, message), BTreeMap::new());
                // Ever more slowly while it keeps failing.
                wait = if failing == 1 {
                    PORTAL_RETRY
                } else {
                    (wait * 2).min(PORTAL_RETRY_MAX)
                };
            }
        }
        tokio::time::sleep(wait).await;
    }
}

/// Wait for the desktop, and pick the way keys reach Weir on it. Also the
/// desktop's name, for the messages.
async fn find_way(controller: &Controller) -> (Way, Option<String>) {
    let forced = std::env::var("WEIR_HOTKEYS").ok();
    if forced.as_deref() == Some("none") {
        return (Way::Unavailable, None);
    }
    let mut told = false;
    let env = loop {
        if let Some(env) = tokio::task::spawn_blocking(display::window_env)
            .await
            .ok()
            .flatten()
        {
            break env;
        }
        if !told {
            controller.set_keys_status(
                status(
                    KeysMethod::Starting,
                    "Hotkeys start working once the desktop is up.",
                ),
                BTreeMap::new(),
            );
            told = true;
        }
        tokio::time::sleep(DESKTOP_POLL).await;
    };
    let var = |k: &str| {
        env.iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(k).ok())
            .filter(|v| !v.is_empty())
    };
    let desktop = var("XDG_CURRENT_DESKTOP");
    let x11 = || Way::X11 {
        display: var("DISPLAY").unwrap_or_default(),
        xauthority: var("XAUTHORITY"),
    };
    let way = match forced.as_deref() {
        Some("desktop") => Way::Desktop,
        Some("x11") => x11(),
        _ => {
            let session = var("XDG_SESSION_TYPE");
            let wayland = var("WAYLAND_DISPLAY").is_some();
            let has_x = var("DISPLAY").is_some();
            if session.as_deref() == Some("x11") || (!wayland && has_x) {
                x11()
            } else if wayland || session.as_deref() == Some("wayland") {
                Way::Desktop
            } else {
                Way::Unavailable
            }
        }
    };
    (way, desktop)
}
