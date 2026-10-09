//! KDE Plasma's own shortcut service, kglobalaccel, which keeps the keys
//! of the shortcuts the portal binds for Weir, and which System Settings
//! changes them through.
//!
//! The portal can only suggest one key combination for a new shortcut, and
//! on Plasma it cannot forget one: its backend removes a shortcut left out
//! of a binding only when the same session bound it before, and each
//! change of hotkeys is a new session. So on Plasma Weir also talks to
//! kglobalaccel directly, for its own shortcuts only: to give a hotkey all
//! its keys, to forget removed hotkeys, to free the keys of switched-off
//! ones, and to read keys as numbers rather than as text in the desktop's
//! language.

use ashpd::zbus;
use std::collections::BTreeSet;
use tracing::debug;
use weir_protocol::KeyCombo;

const SERVICE: &str = "org.kde.kglobalaccel";
const PATH: &str = "/kglobalaccel";
const INTERFACE: &str = "org.kde.KGlobalAccel";

/// One key sequence as kglobalaccel sends it: up to four key codes, of
/// which Weir uses only the first.
type Sequence = (Vec<i32>,);
/// What kglobalaccel says about a shortcut: its id and name, its
/// component's id and name, its context's id and name, its keys and its
/// default keys.
type ShortcutInfo = (
    String,
    String,
    String,
    String,
    String,
    String,
    Vec<i32>,
    Vec<i32>,
);

/// kglobalaccel, for Weir's shortcuts filed under `component` (the name
/// the portal knows Weir by).
pub struct Kde {
    proxy: zbus::Proxy<'static>,
    component: String,
}

impl Kde {
    /// kglobalaccel, when it is running on this desktop.
    pub async fn find(connection: &zbus::Connection, component: &str) -> Option<Kde> {
        let dbus = zbus::fdo::DBusProxy::new(connection).await.ok()?;
        let name = zbus::names::BusName::try_from(SERVICE).ok()?;
        if !dbus.name_has_owner(name).await.unwrap_or(false) {
            return None;
        }
        let proxy = zbus::Proxy::new(connection, SERVICE, PATH, INTERFACE)
            .await
            .ok()?;
        Some(Kde {
            proxy,
            component: component.into(),
        })
    }

    /// kglobalaccel's name for shortcut `id`: component, shortcut, and
    /// their names for people.
    fn action(&self, id: &str, name: &str) -> Vec<String> {
        vec![
            self.component.clone(),
            id.into(),
            "Weir".into(),
            name.into(),
        ]
    }

    /// The keys shortcut `id` has, as Qt key codes.
    pub async fn keys(&self, id: &str) -> zbus::Result<Vec<i32>> {
        let seqs: Vec<Sequence> = self
            .proxy
            .call("shortcutKeys", &(self.action(id, ""),))
            .await?;
        Ok(seqs
            .into_iter()
            .filter_map(|(codes,)| codes.first().copied())
            .filter(|&c| c != 0)
            .collect())
    }

    /// Give shortcut `id` exactly `keys`, as System Settings would.
    pub async fn set_keys(&self, id: &str, name: &str, keys: &[i32]) -> zbus::Result<()> {
        let seqs: Vec<Sequence> = keys.iter().map(|&k| (vec![k, 0, 0, 0],)).collect();
        self.proxy
            .call::<_, _, ()>("setForeignShortcutKeys", &(self.action(id, name), seqs))
            .await
    }

    /// Forget shortcut `id`, keys and all, so it leaves System Settings.
    pub async fn forget(&self, id: &str) -> zbus::Result<bool> {
        self.proxy
            .call("unregister", &(self.component.as_str(), id))
            .await
    }

    /// Stop shortcut `id` taking its keys, keeping it and them in System
    /// Settings. Binding it again takes them back.
    pub async fn release(&self, id: &str, name: &str) -> zbus::Result<()> {
        self.proxy
            .call::<_, _, ()>("setInactive", &(self.action(id, name),))
            .await
    }

    /// What else has `key`, in words, such as "Konsole (Open a terminal)",
    /// when another program has it.
    pub async fn owner(&self, key: i32) -> Option<String> {
        // Match type 0: exactly this key.
        let infos: Vec<ShortcutInfo> = match self
            .proxy
            .call("globalShortcutsByKey", &((vec![key, 0, 0, 0],), (0i32,)))
            .await
        {
            Ok(infos) => infos,
            Err(e) => {
                debug!("could not ask the desktop who has a key: {e}");
                return None;
            }
        };
        infos
            .into_iter()
            .find(|i| i.2 != self.component)
            .map(|(_, name, _, program, ..)| format!("{program} ({name})"))
    }
}

/// Keys as Qt key codes, ignoring order: the desktop keeps them as a set.
pub fn codes(keys: &[KeyCombo]) -> BTreeSet<i32> {
    keys.iter().map(KeyCombo::to_qt).collect()
}

/// A key the desktop has that Weir has no name for, written as best it can.
pub fn foreign_name(code: i32) -> String {
    let code = code as u32;
    let mut parts = Vec::new();
    for (flag, name) in [
        (0x1000_0000, "Super"),
        (0x0400_0000, "Ctrl"),
        (0x0800_0000, "Alt"),
        (0x0200_0000, "Shift"),
    ] {
        if code & flag != 0 {
            parts.push(name.to_string());
        }
    }
    let key = code & 0x01ff_ffff;
    parts.push(match char::from_u32(key).filter(|c| c.is_ascii_graphic()) {
        Some(c) => c.to_string(),
        None => format!("key {key:#x}"),
    });
    parts.join("+")
}
