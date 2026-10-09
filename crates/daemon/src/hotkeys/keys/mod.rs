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
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;
use tracing::{debug, info, warn};
use weir_protocol::*;

/// One key combination of a hotkey, which should work.
#[derive(Debug, Clone, PartialEq)]
pub struct Registration {
    pub id: HotkeyId,
    /// Which of the hotkey's combinations, from 0.
    pub index: usize,
    pub name: String,
    pub keys: KeyCombo,
}

/// The key combinations that should work: those of the enabled hotkeys.
fn registrations(hotkeys: &[Hotkey]) -> Vec<Registration> {
    hotkeys
        .iter()
        .filter(|h| h.enabled)
        .flat_map(|h| {
            h.keys.iter().enumerate().filter_map(|(index, keys)| {
                Some(Registration {
                    id: h.id,
                    index,
                    name: h.name.clone(),
                    keys: KeyCombo::parse(keys).ok()?,
                })
            })
        })
        .collect()
}

/// Which of each hotkey's key combinations are down, so that a hotkey with
/// several is pressed when the first goes down and let go when the last
/// comes up: holding two of them is still one press.
#[derive(Debug)]
pub struct Held<K>(HashMap<HotkeyId, HashSet<K>>);

impl<K> Default for Held<K> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<K: Eq + Hash> Held<K> {
    /// `combo` of hotkey `id` went down: whether that presses the hotkey.
    pub fn down(&mut self, id: HotkeyId, combo: K) -> bool {
        let combos = self.0.entry(id).or_default();
        let first = combos.is_empty();
        combos.insert(combo) && first
    }

    /// `combo` of hotkey `id` came up: whether that lets go of the hotkey.
    pub fn up(&mut self, id: HotkeyId, combo: &K) -> bool {
        let Some(combos) = self.0.get_mut(&id) else {
            return false;
        };
        if !combos.remove(combo) || !combos.is_empty() {
            return false;
        }
        self.0.remove(&id);
        true
    }
}

/// Add `problem` to what is wrong with hotkey `id`: one of its several key
/// combinations may have a problem of its own.
pub fn add_problem(problems: &mut BTreeMap<HotkeyId, String>, id: HotkeyId, problem: String) {
    problems
        .entry(id)
        .and_modify(|p| {
            p.push(' ');
            p.push_str(&problem);
        })
        .or_insert(problem);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holding_two_combinations_of_a_hotkey_is_one_press() {
        let mut held = Held::default();
        assert!(held.down(1, "F9"));
        assert!(!held.down(1, "Ctrl+Alt+T"), "already pressed");
        assert!(!held.down(1, "F9"), "a key going down twice");
        assert!(!held.up(1, &"F9"), "the other is still held");
        assert!(held.up(1, &"Ctrl+Alt+T"));
        assert!(!held.up(1, &"Ctrl+Alt+T"), "let go already");
        assert!(
            held.down(2, "F9") && held.down(1, "F9"),
            "hotkeys are apart"
        );
    }

    #[test]
    fn every_combination_of_an_enabled_hotkey_is_registered() {
        let hotkey = |id, enabled, keys: &[&str]| Hotkey {
            id,
            name: format!("H{id}"),
            enabled,
            keys: keys.iter().map(|k| k.to_string()).collect(),
            steps: Vec::new(),
            each_press: EachPress::All,
            on_release: OnRelease::Nothing,
            release_steps: Vec::new(),
            repeat_ms: None,
        };
        let regs = registrations(&[
            hotkey(1, true, &["F9", "Ctrl+Alt+T"]),
            hotkey(2, false, &["F10"]),
            hotkey(3, true, &[]),
        ]);
        let got: Vec<(HotkeyId, usize, String)> = regs
            .iter()
            .map(|r| (r.id, r.index, r.keys.to_string()))
            .collect();
        assert_eq!(got, [(1, 0, "F9".into()), (1, 1, "Ctrl+Alt+T".into())]);
    }
}
