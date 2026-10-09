//! Hotkeys through the desktop's shortcut service: the XDG desktop
//! portal's GlobalShortcuts.
//!
//! Weir opens a session, binds a shortcut per hotkey with its first keys as
//! the suggestion, and hears `Activated` and `Deactivated` as the keys go
//! down and up. Each hotkey is one entry in the desktop's shortcut
//! settings. The desktop has the last word on the keys: it may ask the user
//! first, and they can change them later in its settings, or add more to
//! the same entry, so the keys it reports back are what the window shows.
//! When the hotkeys change, the session is closed and a new one bound, with
//! every hotkey that has keys, so the desktop forgets the ones removed; it
//! remembers the keys of the others by shortcut id, so nothing is lost.
//!
//! A shortcut's id carries a short hash of the keys suggested: changing
//! them in Weir makes a new shortcut, which the desktop then offers with
//! the new keys, rather than keeping the old ones it remembers.

use super::{add_problem, status, Held, Registration};
use crate::controller::Controller;
use crate::hotkeys::Command;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut, Shortcut};
use futures_util::StreamExt;
use std::collections::{BTreeMap, HashMap, HashSet};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;
use tracing::{debug, info, warn};
use weir_protocol::*;

/// The name Weir gives itself to the desktop's shortcut service. The
/// service takes a program's word for its name only when a desktop file of
/// that name is installed, and Weir's is `weir.desktop`. It is also the name
/// the service works out by itself when Weir was started from the
/// application menu, so the keys stay filed under one name however Weir
/// started: from the menu, at login or from a terminal.
const DESKTOP_ID: &str = "weir";

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
        "Hotkeys work whichever window is in front. KDE Plasma looks after the keys: each \
         hotkey is one entry in System Settings, under Keyboard, Shortcuts, where you can \
         change its keys or give it more."
            .into()
    } else if d.contains("GNOME") {
        "Hotkeys work whichever window is in front. GNOME looks after the keys: each hotkey \
         is one entry in its Settings, where you can change its keys."
            .into()
    } else {
        "Hotkeys work whichever window is in front. Your desktop looks after the keys: each \
         hotkey is one entry in its shortcut settings, where you can change its keys."
            .into()
    }
}

/// The shortcuts to bind: one for each hotkey with keys, suggesting its
/// first combination, since a shortcut takes one suggestion and more keys
/// for it are added in the desktop's settings. A switched-off hotkey keeps
/// its shortcut when the desktop already has it (`known`, by shortcut id),
/// so keys set for it there are not lost; one never switched on, such as an
/// example just added, is left out until it is, so the desktop does not ask
/// about keys that do nothing.
fn shortcuts(regs: &[Registration], known: &HashSet<String>) -> Vec<Registration> {
    regs.iter()
        .filter(|r| r.index == 0 && (r.enabled || known.contains(&shortcut_id(r))))
        .cloned()
        .collect()
}

/// Note the keys the desktop says `shortcuts` have, by shortcut id. It may
/// tell of only the ones that changed.
fn remember(given: &mut HashMap<String, String>, shortcuts: &[Shortcut]) {
    for s in shortcuts {
        given.insert(s.id().to_string(), s.trigger_description().to_string());
    }
}

/// A shortcut's keys as the desktop describes them, one by one: KDE writes
/// several as "F9, Ctrl+Alt+I".
fn split_keys(text: &str) -> Vec<String> {
    text.split(", ")
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(String::from)
        .collect()
}

/// The keys the desktop gave each hotkey, and the switched-on hotkeys it
/// gave none.
fn assigned(
    given: &HashMap<String, String>,
    shortcuts: &[Registration],
) -> (BTreeMap<HotkeyId, Vec<String>>, BTreeMap<HotkeyId, String>) {
    let mut keys = BTreeMap::new();
    let mut problems = BTreeMap::new();
    for r in shortcuts {
        let got = given
            .get(&shortcut_id(r))
            .map(|t| split_keys(t))
            .unwrap_or_default();
        if got.is_empty() && r.enabled {
            add_problem(
                &mut problems,
                r.id,
                "Your desktop has no keys for this hotkey. Give it some in its shortcut \
                 settings, or pick other keys."
                    .to_string(),
            );
        }
        keys.insert(r.id, got);
    }
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

/// Tell the portal which program Weir is, before asking it for anything:
/// it files shortcuts under that name, and refuses them when it cannot tell.
/// It only works the name out by itself for programs started from the
/// application menu, and not for the daemon started at login.
///
/// This goes through ashpd's connection, which its shortcut calls also use,
/// but not through `ashpd::register_host_app`, which takes only names like
/// `org.example.App`, and Weir's desktop file is not named like that.
/// Creating `portal` first is fine: that only reads its version, which the
/// portal does not count as asking for anything.
async fn register(portal: &GlobalShortcuts<'_>) {
    let options: HashMap<&str, ashpd::zvariant::Value<'_>> = HashMap::new();
    let reply = portal
        .connection()
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.host.portal.Registry"),
            "Register",
            &(DESKTOP_ID, options),
        )
        .await;
    match reply {
        Ok(_) => debug!("told the portal that Weir is {DESKTOP_ID}"),
        Err(ashpd::zbus::Error::MethodError(name, text, _))
            if name.as_str().starts_with("org.freedesktop.DBus.Error.") =>
        {
            // No portal yet, which the caller hears about next, or one
            // before 1.20, which cannot be told and works it out itself.
            debug!("the portal was not told which program Weir is: {name} {text:?}");
        }
        Err(ashpd::zbus::Error::MethodError(_, Some(text), _))
            if text.contains("already associated") =>
        {
            // Told already, on an earlier try over this connection.
        }
        Err(e) => warn!(
            "could not tell the portal which program Weir is, so it may refuse the hotkeys: {e}"
        ),
    }
}

/// Bind the hotkeys through the portal, and pass their presses on, until
/// the daemon stops.
pub async fn run(
    controller: &Controller,
    tx: &UnboundedSender<Command>,
    mut regs: watch::Receiver<Vec<Registration>>,
    desktop: Option<&str>,
) -> Result<(), Failure> {
    let portal = GlobalShortcuts::new().await.map_err(Failure::Missing)?;
    register(&portal).await;
    // ashpd takes a portal that is not running for an old one; asking
    // tells the two apart.
    let version = portal
        .get_property::<u32>("version")
        .await
        .map_err(|e| Failure::Missing(e.into()))?;
    let mut activated = portal.receive_activated().await.map_err(Failure::Lost)?;
    let mut deactivated = portal.receive_deactivated().await.map_err(Failure::Lost)?;
    let mut changed = portal
        .receive_shortcuts_changed()
        .await
        .map_err(Failure::Lost)?;
    // A portal that restarts has forgotten Weir's name and shortcuts, while
    // the streams above carry on as if nothing happened: start over then.
    let mut restarted = portal
        .receive_owner_changed()
        .await
        .map_err(|e| Failure::Lost(e.into()))?;
    info!("hotkeys go through the desktop's shortcut service");
    let mut session: Option<ashpd::desktop::Session<'_, GlobalShortcuts<'_>>> = None;
    loop {
        let regs_now = regs.borrow_and_update().clone();
        if let Some(old) = session.take() {
            if let Err(e) = old.close().await {
                debug!("closing the old shortcut session: {e}");
            }
        }
        // Bound even with none, so that the desktop forgets removed ones.
        let s = portal.create_session().await.map_err(Failure::Lost)?;
        // A new session starts with the shortcuts the desktop has for Weir.
        let known: HashSet<String> = match portal
            .list_shortcuts(&s)
            .await
            .and_then(|request| request.response())
        {
            Ok(list) => list
                .shortcuts()
                .iter()
                .map(|s| s.id().to_string())
                .collect(),
            Err(e) => {
                debug!("the desktop did not list Weir's shortcuts: {e}");
                HashSet::new()
            }
        };
        let current = shortcuts(&regs_now, &known);
        let ids: HashMap<String, (HotkeyId, bool)> = current
            .iter()
            .map(|r| (shortcut_id(r), (r.id, r.enabled)))
            .collect();
        let mut given = HashMap::new();
        let mut held = Held::default();
        let mut keys_status = status(KeysMethod::Desktop, message(desktop));
        // Opening the settings at Weir's shortcuts came in version 2.
        keys_status.configurable = version >= 2;
        let mut problems = BTreeMap::new();
        let new: Vec<NewShortcut> = current
            .iter()
            .map(|r| {
                NewShortcut::new(shortcut_id(r), r.name.clone())
                    .preferred_trigger(r.keys.to_xdg().as_str())
            })
            .collect();
        match portal
            .bind_shortcuts(&s, &new, None)
            .await
            .and_then(|request| request.response())
        {
            Ok(bound) => {
                remember(&mut given, bound.shortcuts());
                let (keys, missing) = assigned(&given, &current);
                debug!("the desktop gave these keys: {keys:?}");
                keys_status.assigned = keys;
                problems = missing;
            }
            Err(e) if current.is_empty() => debug!("the desktop took no hotkeys: {e}"),
            Err(e) => {
                warn!("the desktop did not take the hotkeys: {e}");
                for r in current.iter().filter(|r| r.enabled) {
                    problems.insert(
                        r.id,
                        "The desktop turned these keys down. Try again, or pick other keys."
                            .to_string(),
                    );
                }
            }
        }
        session = Some(s);
        controller.set_keys_status(keys_status.clone(), problems.clone());

        // Pass presses on until the hotkeys change. A switched-off
        // hotkey's are ignored.
        loop {
            tokio::select! {
                Some(a) = activated.next() => {
                    if let Some(&(id, true)) = ids.get(a.shortcut_id()) {
                        if held.down(id, a.shortcut_id().to_string()) {
                            let _ = tx.send(Command::Press(id));
                        }
                    }
                }
                Some(d) = deactivated.next() => {
                    if let Some(&(id, _)) = ids.get(d.shortcut_id()) {
                        if held.up(id, &d.shortcut_id().to_string()) {
                            let _ = tx.send(Command::Release(id));
                        }
                    }
                }
                Some(_) = restarted.next() => {
                    return Err(Failure::Lost(ashpd::Error::Zbus(ashpd::zbus::Error::Failure(
                        "the shortcut service restarted".into(),
                    ))));
                }
                Some(c) = changed.next() => {
                    remember(&mut given, c.shortcuts());
                    let (keys, missing) = assigned(&given, &current);
                    info!("the desktop's keys for the hotkeys changed: {keys:?}");
                    keys_status.assigned = keys;
                    controller.set_keys_status(keys_status.clone(), missing);
                }
                () = controller.shortcut_settings_wanted() => {
                    if let Some(s) = &session {
                        if let Err(e) = portal.configure_shortcuts(s, None, None).await {
                            warn!("could not open the desktop's shortcut settings: {e}");
                        }
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(id: HotkeyId, index: usize, keys: &str, enabled: bool) -> Registration {
        Registration {
            id,
            index,
            name: "Talk".into(),
            keys: KeyCombo::parse(keys).unwrap(),
            enabled,
        }
    }

    #[test]
    fn each_hotkey_is_one_shortcut_with_all_the_keys_the_desktop_gave_it() {
        let regs = [
            reg(1, 0, "F9", true),
            reg(1, 1, "Ctrl+Alt+T", true),
            reg(2, 0, "F10", true),
            reg(3, 0, "F11", false),
            reg(4, 0, "F12", false),
        ];
        // Hotkey 3 was on once, so the desktop has it; 4 never was.
        let known = HashSet::from([shortcut_id(&regs[3])]);
        let current = shortcuts(&regs, &known);
        let ids: Vec<HotkeyId> = current.iter().map(|r| r.id).collect();
        assert_eq!(ids, [1, 2, 3], "one each, and switched off only if known");
        let mut given = HashMap::new();
        given.insert(shortcut_id(&current[0]), "F9, Ctrl+Alt+I".to_string());
        given.insert(shortcut_id(&current[1]), String::new());
        let (keys, problems) = assigned(&given, &current);
        assert_eq!(keys[&1], ["F9", "Ctrl+Alt+I"]);
        assert!(keys[&2].is_empty() && keys[&3].is_empty());
        assert!(problems.contains_key(&2));
        assert!(
            !problems.contains_key(&3),
            "a switched-off hotkey needs no keys"
        );
        assert!(!problems.contains_key(&1));
    }

    #[test]
    fn changing_the_suggested_keys_makes_a_new_shortcut() {
        assert_eq!(
            shortcut_id(&reg(1, 0, "F9", true)),
            shortcut_id(&reg(1, 0, "F9", false))
        );
        assert_ne!(
            shortcut_id(&reg(1, 0, "F9", true)),
            shortcut_id(&reg(1, 0, "F10", true))
        );
    }
}
