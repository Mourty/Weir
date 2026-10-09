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
//! A shortcut's id carries a short hash of the keys first suggested.
//! Elsewhere, changing them in Weir makes a new shortcut, which the desktop
//! then offers with the new keys, rather than keeping the old ones it
//! remembers. On KDE Plasma, which the portal alone serves badly (see
//! `kde.rs`), Weir keeps a hotkey's shortcut and sets its keys itself, so
//! every one of a hotkey's keys works, changes made in either place carry
//! over to the other, and removed hotkeys really leave System Settings.

use super::kde::{self, Kde};
use super::{add_problem, status, Held, Registration};
use crate::controller::Controller;
use crate::hotkeys::Command;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut, Shortcut};
use futures_util::StreamExt;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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

/// What a hotkey's shortcut id starts with.
fn id_prefix(id: HotkeyId) -> String {
    format!("hotkey-{id}-")
}

/// The shortcut id for hotkey `id` suggesting `keys`: its id and a hash of
/// the keys.
fn shortcut_id(id: HotkeyId, keys: &KeyCombo) -> String {
    // FNV-1a: tiny, and the same on every run, unlike std's hasher.
    let mut h: u32 = 0x811c_9dc5;
    for b in keys.to_string().bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{}{h:08x}", id_prefix(id))
}

/// The sentence for this way, naming the desktop when it is one we know.
fn message(desktop: Option<&str>, settable: bool) -> String {
    if settable {
        return "Hotkeys work whichever window is in front. KDE Plasma looks after the keys: \
                each hotkey is also one entry in System Settings, under Keyboard, Shortcuts, \
                and keys changed there show up here."
            .into();
    }
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

/// A hotkey as one shortcut of the desktop's.
#[derive(Debug, Clone, PartialEq)]
struct Bound {
    id: HotkeyId,
    /// The shortcut's id.
    shortcut: String,
    name: String,
    /// The keys suggested, should the desktop not know the shortcut yet.
    suggested: KeyCombo,
    /// All of the hotkey's keys, in Weir.
    keys: Vec<KeyCombo>,
    enabled: bool,
}

/// The shortcuts to bind: one for each hotkey with keys, since a shortcut
/// takes one suggestion and the desktop keeps the rest of its keys. A
/// switched-off hotkey keeps its shortcut when the desktop already has it
/// (`known`, by shortcut id), so keys set for it there are not lost; one
/// never switched on, such as an example just added, is left out until it
/// is, so the desktop does not ask about keys that do nothing.
///
/// With `keep` (where Weir sets the keys itself), a hotkey whose keys
/// changed keeps the shortcut the desktop has for it.
fn plan(regs: &[Registration], known: &HashSet<String>, keep: bool) -> Vec<Bound> {
    let mut out: Vec<Bound> = Vec::new();
    for r in regs {
        if let Some(b) = out.iter_mut().find(|b| b.id == r.id) {
            b.keys.push(r.keys.clone());
            continue;
        }
        let fresh = shortcut_id(r.id, &r.keys);
        let mut theirs: Vec<&String> = known
            .iter()
            .filter(|k| k.starts_with(&id_prefix(r.id)))
            .collect();
        theirs.sort();
        let shortcut = match theirs.first() {
            Some(first) if keep && !known.contains(&fresh) => (*first).clone(),
            _ => fresh,
        };
        out.push(Bound {
            id: r.id,
            shortcut,
            name: r.name.clone(),
            suggested: r.keys.clone(),
            keys: vec![r.keys.clone()],
            enabled: r.enabled,
        });
    }
    out.retain(|b| b.enabled || known.contains(&b.shortcut));
    out
}

/// What the desktop is told by a binding: each shortcut, with its name and
/// whether it is switched on. Nothing else needs a new binding.
fn binding(plan: &[Bound]) -> Vec<(&str, &str, bool)> {
    plan.iter()
        .map(|b| (b.shortcut.as_str(), b.name.as_str(), b.enabled))
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

/// The problem of a switched-on hotkey the desktop gave no keys.
const NO_KEYS: &str = "Your desktop has no keys for this hotkey. Give it some in its shortcut \
                       settings, or pick other keys.";

/// The keys the desktop gave each hotkey, as it describes them, and the
/// switched-on hotkeys it gave none.
fn assigned(
    given: &HashMap<String, String>,
    plan: &[Bound],
) -> (BTreeMap<HotkeyId, Vec<String>>, BTreeMap<HotkeyId, String>) {
    let mut keys = BTreeMap::new();
    let mut problems = BTreeMap::new();
    for b in plan {
        let got = given
            .get(&b.shortcut)
            .map(|t| split_keys(t))
            .unwrap_or_default();
        if got.is_empty() && b.enabled {
            add_problem(&mut problems, b.id, NO_KEYS.to_string());
        }
        keys.insert(b.id, got);
    }
    (keys, problems)
}

/// The keys each hotkey has on KDE, from their key codes, the switched-on
/// hotkeys with none, and keys left out because another program has them.
fn assigned_codes(
    codes: &HashMap<HotkeyId, Vec<i32>>,
    taken: &BTreeMap<HotkeyId, String>,
    plan: &[Bound],
) -> (BTreeMap<HotkeyId, Vec<String>>, BTreeMap<HotkeyId, String>) {
    let mut keys = BTreeMap::new();
    let mut problems = BTreeMap::new();
    for b in plan {
        let got: Vec<String> = codes
            .get(&b.id)
            .map(|c| c.iter().map(|&c| name_of(c)).collect())
            .unwrap_or_default();
        if got.is_empty() && b.enabled {
            add_problem(&mut problems, b.id, NO_KEYS.to_string());
        }
        if let Some(t) = taken.get(&b.id) {
            add_problem(&mut problems, b.id, t.clone());
        }
        keys.insert(b.id, got);
    }
    (keys, problems)
}

/// A key code as Weir writes keys, or as best it can.
fn name_of(code: i32) -> String {
    KeyCombo::from_qt(code).map_or_else(|| kde::foreign_name(code), |c| c.to_string())
}

/// What Weir remembers between bindings on KDE.
#[derive(Default)]
struct Settled {
    /// Each hotkey's keys in Weir at the last binding: keys differing from
    /// them were changed in Weir, and go to the desktop.
    wished: HashMap<HotkeyId, BTreeSet<i32>>,
    /// The keys the desktop has for each hotkey.
    codes: HashMap<HotkeyId, Vec<i32>>,
    /// Why keys asked for were left out: another program has them.
    taken: BTreeMap<HotkeyId, String>,
}

impl Settled {
    /// Forget hotkeys that are gone.
    fn keep_only(&mut self, regs: &[Registration]) {
        let ids: HashSet<HotkeyId> = regs.iter().map(|r| r.id).collect();
        self.wished.retain(|id, _| ids.contains(id));
        self.codes.retain(|id, _| ids.contains(id));
        self.taken.retain(|id, _| ids.contains(id));
    }
}

/// On KDE, after binding: give `b`'s shortcut the keys changed in Weir,
/// free a switched-off hotkey's keys, and take keys changed in System
/// Settings back into Weir. `new` says the desktop did not know the
/// shortcut before this binding.
async fn settle(kde: &Kde, controller: &Controller, b: &Bound, new: bool, s: &mut Settled) {
    let wanted = kde::codes(&b.keys);
    let mut desktop = match kde.keys(&b.shortcut).await {
        Ok(keys) => keys,
        Err(e) => {
            warn!("could not read the desktop's keys for '{}': {e}", b.name);
            return;
        }
    };
    let give = match s.wished.get(&b.id) {
        Some(before) => *before != wanted,
        // A new shortcut people said yes to: give it the rest of its keys.
        None => new && !desktop.is_empty() && !wanted.iter().all(|k| desktop.contains(k)),
    };
    s.wished.insert(b.id, wanted);
    if give {
        // Keys Weir has no name for stay as they are.
        let mut keys: Vec<i32> = desktop
            .iter()
            .copied()
            .filter(|&c| KeyCombo::from_qt(c).is_none())
            .collect();
        let mut refused = Vec::new();
        for k in &b.keys {
            let code = k.to_qt();
            if !desktop.contains(&code) {
                if let Some(owner) = kde.owner(code).await {
                    refused.push(format!("{k} is already used by {owner}"));
                    continue;
                }
            }
            keys.push(code);
        }
        let names: Vec<String> = b.keys.iter().map(KeyCombo::to_string).collect();
        info!("giving '{}' the keys {}", b.name, names.join(", "));
        if let Err(e) = kde.set_keys(&b.shortcut, &b.name, &keys).await {
            warn!("could not give '{}' its keys: {e}", b.name);
        }
        // Keys given in Weir are the shortcut's defaults, as the first ones
        // were: otherwise System Settings shows the old default unchecked
        // and the new keys as custom ones.
        let given: Vec<i32> = keys
            .iter()
            .copied()
            .filter(|&c| KeyCombo::from_qt(c).is_some())
            .collect();
        if let Err(e) = kde.set_defaults(&b.shortcut, &b.name, &given).await {
            debug!("could not make the keys of '{}' its defaults: {e}", b.name);
        }
        if refused.is_empty() {
            s.taken.remove(&b.id);
        } else {
            s.taken.insert(
                b.id,
                format!("{}, so Weir left it out.", refused.join("; ")),
            );
        }
        desktop = kde.keys(&b.shortcut).await.unwrap_or(keys);
    }
    if !b.enabled {
        if let Err(e) = kde.release(&b.shortcut, &b.name).await {
            debug!("could not free the keys of '{}': {e}", b.name);
        }
    }
    adopt(controller, b, &desktop, s);
    s.codes.insert(b.id, desktop);
}

/// Take the keys the desktop has for `b` into Weir when they differ, as
/// after a change in System Settings: Weir's order for keys both have, then
/// the desktop's new ones. None at all, or only ones Weir cannot name,
/// leave Weir's keys alone.
fn adopt(controller: &Controller, b: &Bound, desktop: &[i32], s: &mut Settled) {
    let named: Vec<KeyCombo> = desktop
        .iter()
        .filter_map(|&c| KeyCombo::from_qt(c))
        .collect();
    if named.is_empty() || kde::codes(&named) == kde::codes(&b.keys) {
        return;
    }
    let mut keys: Vec<KeyCombo> = b
        .keys
        .iter()
        .filter(|k| named.contains(k))
        .cloned()
        .collect();
    keys.extend(named.into_iter().filter(|k| !b.keys.contains(k)));
    s.wished.insert(b.id, kde::codes(&keys));
    controller.adopt_desktop_keys(b.id, keys.iter().map(KeyCombo::to_string).collect());
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
    let on_kde = desktop.is_some_and(|d| d.to_ascii_uppercase().contains("KDE"));
    let kde = match on_kde {
        true => Kde::find(portal.connection(), DESKTOP_ID).await,
        false => None,
    };
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
    info!(
        "hotkeys go through the desktop's shortcut service{}",
        if kde.is_some() {
            ", and Weir sets their keys in Plasma's"
        } else {
            ""
        }
    );
    let mut keys_status = status(KeysMethod::Desktop, message(desktop, kde.is_some()));
    // Opening the settings at Weir's shortcuts came in version 2.
    keys_status.configurable = version >= 2;
    keys_status.settable = kde.is_some();
    let mut settled = Settled::default();
    let mut session: Option<ashpd::desktop::Session<'_, GlobalShortcuts<'_>>> = None;
    loop {
        let now = regs.borrow_and_update().clone();
        settled.keep_only(&now);
        if let Some(old) = session.take() {
            if let Err(e) = old.close().await {
                debug!("closing the old shortcut session: {e}");
            }
        }
        // Bound even with none, so that the desktop forgets removed ones.
        let s = portal.create_session().await.map_err(Failure::Lost)?;
        // A new session starts with the shortcuts the desktop already has.
        let mut given = HashMap::new();
        let known: HashSet<String> = match portal
            .list_shortcuts(&s)
            .await
            .and_then(|request| request.response())
        {
            Ok(list) => {
                remember(&mut given, list.shortcuts());
                list.shortcuts()
                    .iter()
                    .map(|s| s.id().to_string())
                    .collect()
            }
            Err(e) => {
                debug!("the desktop did not list Weir's shortcuts: {e}");
                HashSet::new()
            }
        };
        let mut current = plan(&now, &known, kde.is_some());
        let new: Vec<NewShortcut> = current
            .iter()
            .map(|b| {
                NewShortcut::new(b.shortcut.clone(), b.name.clone())
                    .preferred_trigger(b.suggested.to_xdg().as_str())
            })
            .collect();
        let mut problems = BTreeMap::new();
        match portal
            .bind_shortcuts(&s, &new, None)
            .await
            .and_then(|request| request.response())
        {
            Ok(bound) => remember(&mut given, bound.shortcuts()),
            Err(e) if current.is_empty() => debug!("the desktop took no hotkeys: {e}"),
            Err(e) => {
                warn!("the desktop did not take the hotkeys: {e}");
                for b in current.iter().filter(|b| b.enabled) {
                    problems.insert(
                        b.id,
                        "The desktop turned these keys down. Try again, or pick other keys."
                            .to_string(),
                    );
                }
            }
        }
        session = Some(s);
        if let Some(kde) = &kde {
            // Plasma keeps shortcuts left out of a binding unless this
            // session bound them before: forget them here.
            for gone in known
                .iter()
                .filter(|k| k.starts_with("hotkey-") && !current.iter().any(|b| &&b.shortcut == k))
            {
                match kde.forget(gone).await {
                    Ok(true) => info!("removed the shortcut {gone} from the desktop's"),
                    Ok(false) => debug!("the desktop had no shortcut {gone} to remove"),
                    Err(e) => warn!("could not remove the shortcut {gone}: {e}"),
                }
            }
            for b in &current {
                settle(
                    kde,
                    controller,
                    b,
                    !known.contains(&b.shortcut),
                    &mut settled,
                )
                .await;
            }
        }
        let report = |given: &HashMap<String, String>, settled: &Settled, plan: &[Bound]| {
            if kde.is_some() {
                assigned_codes(&settled.codes, &settled.taken, plan)
            } else {
                assigned(given, plan)
            }
        };
        let (keys, missing) = report(&given, &settled, &current);
        debug!("the desktop gave these keys: {keys:?}");
        keys_status.assigned = keys;
        if problems.is_empty() {
            problems = missing;
        }
        controller.set_keys_status(keys_status.clone(), problems);
        let mut ids: HashMap<String, (HotkeyId, bool)> = current
            .iter()
            .map(|b| (b.shortcut.clone(), (b.id, b.enabled)))
            .collect();
        let mut held = Held::default();

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
                    if let Some(kde) = &kde {
                        for b in &current {
                            settle(kde, controller, b, false, &mut settled).await;
                        }
                    }
                    let (keys, missing) = report(&given, &settled, &current);
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
                    // On KDE, keys changed in Weir or in System Settings
                    // need no new binding: the shortcuts stay as they are.
                    let Some(kde) = &kde else { break };
                    let now = regs.borrow_and_update().clone();
                    let bound: HashSet<String> =
                        current.iter().map(|b| b.shortcut.clone()).collect();
                    let next = plan(&now, &known.union(&bound).cloned().collect(), true);
                    if binding(&next) != binding(&current) {
                        break;
                    }
                    settled.keep_only(&now);
                    current = next;
                    for b in &current {
                        settle(kde, controller, b, false, &mut settled).await;
                    }
                    let (keys, missing) = report(&given, &settled, &current);
                    keys_status.assigned = keys;
                    controller.set_keys_status(keys_status.clone(), missing);
                    ids = current
                        .iter()
                        .map(|b| (b.shortcut.clone(), (b.id, b.enabled)))
                        .collect();
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

    fn id(id: HotkeyId, keys: &str) -> String {
        shortcut_id(id, &KeyCombo::parse(keys).unwrap())
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
        let known = HashSet::from([id(3, "F11")]);
        let current = plan(&regs, &known, false);
        let ids: Vec<HotkeyId> = current.iter().map(|b| b.id).collect();
        assert_eq!(ids, [1, 2, 3], "one each, and switched off only if known");
        assert_eq!(current[0].keys.len(), 2);
        assert_eq!(current[0].suggested.to_string(), "F9");
        let mut given = HashMap::new();
        given.insert(current[0].shortcut.clone(), "F9, Ctrl+Alt+I".to_string());
        given.insert(current[1].shortcut.clone(), String::new());
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
    fn changing_the_suggested_keys_makes_a_new_shortcut_except_on_kde() {
        assert_eq!(id(1, "F9"), shortcut_id(1, &KeyCombo::parse("f9").unwrap()));
        assert_ne!(id(1, "F9"), id(1, "F10"));
        // The desktop knows hotkey 1 by its first keys, F9, and Weir now
        // has F10 first.
        let known = HashSet::from([id(1, "F9"), id(7, "F9")]);
        let regs = [reg(1, 0, "F10", true), reg(1, 1, "F9", true)];
        assert_eq!(plan(&regs, &known, false)[0].shortcut, id(1, "F10"));
        assert_eq!(plan(&regs, &known, true)[0].shortcut, id(1, "F9"));
        // A shortcut of hotkey 1's own first keys is the one kept.
        let both = HashSet::from([id(1, "F9"), id(1, "F10")]);
        assert_eq!(plan(&regs, &both, true)[0].shortcut, id(1, "F10"));
        // Only what the desktop is told needs a new binding.
        let a = plan(&regs, &known, true);
        let b = plan(&[reg(1, 0, "F10", true)], &known, true);
        assert_eq!(binding(&a), binding(&b));
    }

    #[test]
    fn kde_keys_read_as_weirs_or_as_best_they_can() {
        let current = plan(
            &[reg(1, 0, "F9", true), reg(2, 0, "F10", true)],
            &HashSet::new(),
            true,
        );
        let codes = HashMap::from([
            (1, vec![0x0100_0038, 0x0c00_0049, 0x0200_0021]),
            (2, vec![]),
        ]);
        let taken = BTreeMap::from([(1, "Ctrl+Alt+T is already used by Konsole".to_string())]);
        let (keys, problems) = assigned_codes(&codes, &taken, &current);
        assert_eq!(keys[&1], ["F9", "Ctrl+Alt+I", "Shift+!"]);
        assert!(problems[&1].contains("Konsole"));
        assert_eq!(problems[&2], NO_KEYS);
    }
}
