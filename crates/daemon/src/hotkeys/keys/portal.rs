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

use super::{add_problem, status, Held, Registration};
use crate::controller::Controller;
use crate::hotkeys::Command;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut, Shortcut};
use futures_util::StreamExt;
use std::collections::{BTreeMap, HashMap};
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

/// How a combination is listed in the desktop's settings: the hotkey's
/// name, numbered after the first when it has several.
fn description(r: &Registration) -> String {
    match r.index {
        0 => r.name.clone(),
        i => format!("{} ({})", r.name, i + 1),
    }
}

/// Note the keys the desktop says `shortcuts` have, by shortcut id. It may
/// tell of only the ones that changed.
fn remember(given: &mut HashMap<String, String>, shortcuts: &[Shortcut]) {
    for s in shortcuts {
        given.insert(s.id().to_string(), s.trigger_description().to_string());
    }
}

/// The keys the desktop gave each hotkey's combinations, in order, and the
/// combinations it gave none.
fn assigned(
    given: &HashMap<String, String>,
    regs: &[Registration],
) -> (BTreeMap<HotkeyId, Vec<String>>, BTreeMap<HotkeyId, String>) {
    let mut keys: BTreeMap<HotkeyId, Vec<String>> = BTreeMap::new();
    let mut problems = BTreeMap::new();
    for r in regs {
        let got = given
            .get(&shortcut_id(r))
            .map(String::as_str)
            .unwrap_or_default();
        let list = keys.entry(r.id).or_default();
        if list.len() <= r.index {
            list.resize(r.index + 1, String::new());
        }
        list[r.index] = got.to_string();
        if got.is_empty() {
            add_problem(
                &mut problems,
                r.id,
                format!(
                    "The desktop has not given {} to this hotkey yet. Set it in its shortcut \
                     settings, or pick other keys.",
                    r.keys
                ),
            );
        }
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
    // A portal that restarts has forgotten Weir's name and shortcuts, while
    // the streams above carry on as if nothing happened: start over then.
    let mut restarted = portal
        .receive_owner_changed()
        .await
        .map_err(|e| Failure::Lost(e.into()))?;
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
        let mut given = HashMap::new();
        let mut held = Held::default();
        let mut keys_status = status(KeysMethod::Desktop, message(desktop));
        let mut problems = BTreeMap::new();
        if !current.is_empty() {
            let s = portal.create_session().await.map_err(Failure::Lost)?;
            let shortcuts: Vec<NewShortcut> = current
                .iter()
                .map(|r| {
                    NewShortcut::new(shortcut_id(r), description(r))
                        .preferred_trigger(r.keys.to_xdg().as_str())
                })
                .collect();
            match portal
                .bind_shortcuts(&s, &shortcuts, None)
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
                        if held.down(id, a.shortcut_id().to_string()) {
                            let _ = tx.send(Command::Press(id));
                        }
                    }
                }
                Some(d) = deactivated.next() => {
                    if let Some(&id) = ids.get(d.shortcut_id()) {
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

    fn reg(id: HotkeyId, index: usize, keys: &str) -> Registration {
        Registration {
            id,
            index,
            name: "Talk".into(),
            keys: KeyCombo::parse(keys).unwrap(),
        }
    }

    #[test]
    fn the_desktops_keys_line_up_with_each_combination() {
        let regs = [reg(1, 0, "F9"), reg(1, 1, "Ctrl+Alt+T"), reg(2, 0, "F10")];
        assert_eq!(description(&regs[0]), "Talk");
        assert_eq!(description(&regs[1]), "Talk (2)");
        // Each combination has its own shortcut, so changing one hotkey's
        // keys leaves the others' as the desktop has them.
        assert_ne!(shortcut_id(&regs[0]), shortcut_id(&regs[1]));
        let mut given = HashMap::new();
        given.insert(shortcut_id(&regs[0]), "F9, Ctrl+Alt+I".to_string());
        given.insert(shortcut_id(&regs[1]), String::new());
        let (keys, problems) = assigned(&given, &regs);
        assert_eq!(keys[&1], ["F9, Ctrl+Alt+I", ""]);
        assert_eq!(keys[&2], [""]);
        assert!(problems[&1].contains("Ctrl+Alt+T"), "{problems:?}");
        assert!(problems[&2].contains("F10"), "{problems:?}");
    }
}
