//! Hotkeys through the desktop's shortcut service: the XDG desktop
//! portal's GlobalShortcuts.
//!
//! Weir opens a session, binds a shortcut per hotkey with its keys as the
//! suggestion, and hears `Activated` and `Deactivated` as the keys go down
//! and up. The desktop has the last word on the keys: it may ask the user
//! first, and they can change them later in its settings, so the keys it
//! reports back are what the window shows. When the hotkeys change, the
//! session is closed and a new one bound; the desktop remembers keys by
//! shortcut id, so nothing is lost.
//!
//! A shortcut's id carries a short hash of the hotkey's keys: changing the
//! keys in Weir makes a new shortcut, which the desktop then offers with
//! the new keys, rather than keeping the old ones it remembers.

use super::{status, Registration};
use crate::controller::Controller;
use crate::hotkeys::Command;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut, Shortcut};
use futures_util::StreamExt;
use std::collections::{BTreeMap, HashMap};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;
use tracing::{debug, info, warn};
use weir_protocol::*;

/// Weir's application id, as in its desktop file and AppStream data.
const APP_ID: &str = "io.github.mourty.weir";

/// The shortcut id for `r`: its hotkey's id and a hash of its keys.
fn shortcut_id(r: &Registration) -> String {
    // FNV-1a: tiny, and the same on every run, unlike std's hasher.
    let mut h: u32 = 0x811c_9dc5;
    for b in r.keys.to_string().bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("hotkey-{}-{h:08x}", r.id)
}

/// The sentence for this way, naming the desktop when it is one we know.
fn message(desktop: Option<&str>) -> String {
    let d = desktop.unwrap_or_default().to_ascii_uppercase();
    if d.contains("KDE") {
        "Hotkeys work whichever window is in front. KDE Plasma looks after the keys: you \
         can also see and change them in System Settings, under Shortcuts."
            .into()
    } else if d.contains("GNOME") {
        "Hotkeys work whichever window is in front. GNOME looks after the keys: you can \
         also see and change them in its Settings."
            .into()
    } else {
        "Hotkeys work whichever window is in front. Your desktop looks after the keys: you \
         can also see and change them in its shortcut settings."
            .into()
    }
}

/// The keys the desktop gave each hotkey, and the hotkeys it gave none.
fn assigned(
    shortcuts: &[Shortcut],
    ids: &HashMap<String, HotkeyId>,
    regs: &[Registration],
) -> (BTreeMap<HotkeyId, String>, BTreeMap<HotkeyId, String>) {
    let mut keys = BTreeMap::new();
    for s in shortcuts {
        if let Some(&id) = ids.get(s.id()) {
            if !s.trigger_description().is_empty() {
                keys.insert(id, s.trigger_description().to_string());
            }
        }
    }
    let problems = regs
        .iter()
        .filter(|r| !keys.contains_key(&r.id))
        .map(|r| {
            (
                r.id,
                "The desktop has not given this hotkey any keys yet. Set them in its shortcut \
                 settings, or pick other keys."
                    .to_string(),
            )
        })
        .collect();
    (keys, problems)
}

/// Why the portal stopped working for Weir.
#[derive(Debug)]
pub enum Failure {
    /// There is no shortcut service, or not yet: at login it may still be
    /// starting.
    Missing(ashpd::Error),
    /// It was there, and then failed.
    Lost(ashpd::Error),
}

/// Bind the hotkeys through the portal, and pass their presses on, until
/// the daemon stops.
pub async fn run(
    controller: &Controller,
    tx: &UnboundedSender<Command>,
    mut regs: watch::Receiver<Vec<Registration>>,
    desktop: Option<&str>,
) -> Result<(), Failure> {
    // Programs outside a sandbox have to say who they are, for the desktop
    // to file their shortcuts under their name. Older portals cannot be
    // told, and then work it out themselves.
    match ashpd::AppID::try_from(APP_ID) {
        Ok(app) => {
            if let Err(e) = ashpd::register_host_app(app).await {
                debug!("could not tell the portal Weir's application id: {e}");
            }
        }
        Err(e) => warn!("{APP_ID} is not a valid application id: {e}"),
    }
    let portal = GlobalShortcuts::new().await.map_err(Failure::Missing)?;
    // ashpd takes a portal that is not running for an old one; asking
    // tells the two apart.
    portal
        .get_property::<u32>("version")
        .await
        .map_err(|e| Failure::Missing(e.into()))?;
    let mut activated = portal.receive_activated().await.map_err(Failure::Lost)?;
    let mut deactivated = portal.receive_deactivated().await.map_err(Failure::Lost)?;
    let mut changed = portal
        .receive_shortcuts_changed()
        .await
        .map_err(Failure::Lost)?;
    info!("hotkeys go through the desktop's shortcut service");
    let mut session: Option<ashpd::desktop::Session<'_, GlobalShortcuts<'_>>> = None;
    loop {
        let current = regs.borrow_and_update().clone();
        if let Some(old) = session.take() {
            if let Err(e) = old.close().await {
                debug!("closing the old shortcut session: {e}");
            }
        }
        let ids: HashMap<String, HotkeyId> =
            current.iter().map(|r| (shortcut_id(r), r.id)).collect();
        let mut keys_status = status(KeysMethod::Desktop, message(desktop));
        let mut problems = BTreeMap::new();
        if !current.is_empty() {
            let s = portal.create_session().await.map_err(Failure::Lost)?;
            let shortcuts: Vec<NewShortcut> = current
                .iter()
                .map(|r| {
                    NewShortcut::new(shortcut_id(r), r.name.clone())
                        .preferred_trigger(r.keys.to_xdg().as_str())
                })
                .collect();
            match portal
                .bind_shortcuts(&s, &shortcuts, None)
                .await
                .and_then(|request| request.response())
            {
                Ok(bound) => {
                    let (keys, missing) = assigned(bound.shortcuts(), &ids, &current);
                    debug!("the desktop gave these keys: {keys:?}");
                    keys_status.assigned = keys;
                    problems = missing;
                }
                Err(e) => {
                    warn!("the desktop did not take the hotkeys: {e}");
                    for r in &current {
                        problems.insert(
                            r.id,
                            "The desktop turned these keys down. Try again, or pick other keys."
                                .to_string(),
                        );
                    }
                }
            }
            session = Some(s);
        }
        controller.set_keys_status(keys_status.clone(), problems.clone());

        // Pass presses on until the hotkeys change.
        loop {
            tokio::select! {
                Some(a) = activated.next() => {
                    if let Some(&id) = ids.get(a.shortcut_id()) {
                        let _ = tx.send(Command::Press(id));
                    }
                }
                Some(d) = deactivated.next() => {
                    if let Some(&id) = ids.get(d.shortcut_id()) {
                        let _ = tx.send(Command::Release(id));
                    }
                }
                Some(c) = changed.next() => {
                    let (keys, missing) = assigned(c.shortcuts(), &ids, &current);
                    info!("the desktop's keys for the hotkeys changed: {keys:?}");
                    keys_status.assigned = keys;
                    controller.set_keys_status(keys_status.clone(), missing);
                }
                r = regs.changed() => {
                    if r.is_err() {
                        return Ok(());
                    }
                    break;
                }
            }
        }
    }
}
