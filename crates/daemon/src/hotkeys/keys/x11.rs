//! Hotkeys on an X11 desktop: Weir grabs each combination on the root
//! window, so the key comes to Weir wherever the focus is, and a program
//! that already has it is named as the reason a hotkey does not work.
//!
//! Every combination is grabbed four times, with and without Caps Lock and
//! Num Lock, which X11 counts as modifiers too. Keys go down and up on a
//! thread of their own; grabs change from the async side, on the same
//! connection, which x11rb allows.

use super::{add_problem, status, Held, Registration};
use crate::controller::Controller;
use crate::hotkeys::Command;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{watch, Notify};
use tracing::{debug, info, warn};
use weir_protocol::*;
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, Keycode, ModMask, Window};
use x11rb::protocol::{ErrorKind, Event};
use x11rb::rust_connection::{DefaultStream, RustConnection};

/// The modifiers a hotkey can have: Shift, Control, Alt (Mod1), Super
/// (Mod4). Others, such as Num Lock, do not count.
const MODIFIERS: u16 = 1 | 4 | 8 | 64;
/// Caps Lock and Num Lock (Mod2): grabbed with and without.
const LOCKS: [u16; 4] = [0, 2, 16, 2 | 16];

/// What a grabbed combination is: the key and the modifiers.
type Grab = (Keycode, u16);

/// The sentence for this way.
const MESSAGE: &str = "Hotkeys work whichever window is in front.";

/// How often to try again for keys another program had.
const RETRY: Duration = Duration::from_secs(10);

/// The authorization in Xauthority `file` for display `number`: the first
/// entry for it, or for any display.
fn read_xauthority(file: &Path, number: u16) -> Option<(Vec<u8>, Vec<u8>)> {
    let bytes = std::fs::read(file).ok()?;
    let mut at = 0;
    let field = |at: &mut usize| -> Option<Vec<u8>> {
        let len = u16::from_be_bytes([*bytes.get(*at)?, *bytes.get(*at + 1)?]) as usize;
        let v = bytes.get(*at + 2..*at + 2 + len)?.to_vec();
        *at += 2 + len;
        Some(v)
    };
    let want = number.to_string().into_bytes();
    while at + 2 <= bytes.len() {
        at += 2; // family
        let _address = field(&mut at)?;
        let display = field(&mut at)?;
        let name = field(&mut at)?;
        let data = field(&mut at)?;
        if display.is_empty() || display == want {
            return Some((name, data));
        }
    }
    None
}

/// Connect to `display`. When the daemon's own environment has no
/// `XAUTHORITY` but the session has one, as at login, its file is read
/// here: x11rb only looks in the environment.
fn connect(display: &str, xauthority: Option<&str>) -> Result<(RustConnection, usize), String> {
    let explicit = xauthority.filter(|_| std::env::var_os("XAUTHORITY").is_none());
    let Some(file) = explicit else {
        return RustConnection::connect(Some(display)).map_err(|e| e.to_string());
    };
    let parsed =
        x11rb_protocol::parse_display::parse_display(Some(display)).map_err(|e| e.to_string())?;
    let (name, data) = read_xauthority(Path::new(file), parsed.display).unwrap_or_default();
    let mut last = String::from("no way to reach the display");
    for addr in parsed.connect_instruction() {
        match DefaultStream::connect(&addr) {
            Ok((stream, _)) => {
                let screen = parsed.screen.into();
                return RustConnection::connect_to_stream_with_auth_info(
                    stream,
                    screen,
                    name.clone(),
                    data.clone(),
                )
                .map(|c| (c, screen))
                .map_err(|e| e.to_string());
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// The keycode that types `keysym` without Shift, if this keyboard has one.
fn keycode_for(conn: &RustConnection, keysym: u32) -> Option<Keycode> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn
        .get_keyboard_mapping(min, max - min + 1)
        .ok()?
        .reply()
        .ok()?;
    let per = usize::from(map.keysyms_per_keycode).max(1);
    map.keysyms
        .chunks(per)
        .position(|syms| syms.first() == Some(&keysym) || syms.get(1) == Some(&keysym))
        .map(|i| min + i as u8)
}

/// Grab `regs`' keys in place of `old`'s. Returns what is grabbed, by
/// combination, and why any hotkey could not be.
fn regrab(
    conn: &RustConnection,
    root: Window,
    old: &HashMap<Grab, HotkeyId>,
    regs: &[Registration],
) -> (HashMap<Grab, HotkeyId>, BTreeMap<HotkeyId, String>) {
    for &(key, mods) in old.keys() {
        for lock in LOCKS {
            if let Ok(cookie) = conn.ungrab_key(key, root, ModMask::from(mods | lock)) {
                let _ = cookie.check();
            }
        }
    }
    let mut grabs = HashMap::new();
    let mut problems = BTreeMap::new();
    for r in regs.iter().filter(|r| r.enabled) {
        let Some(key) = keycode_for(conn, r.keys.key.keysym) else {
            add_problem(
                &mut problems,
                r.id,
                format!("This keyboard has no {} key.", r.keys.key.name),
            );
            continue;
        };
        let mods = r.keys.x11_modifiers();
        let mut done = Vec::new();
        let mut failed = None;
        for lock in LOCKS {
            let result = conn
                .grab_key(
                    false,
                    root,
                    ModMask::from(mods | lock),
                    key,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .map_err(ReplyError::from)
                .and_then(|c| c.check());
            match result {
                Ok(()) => done.push(lock),
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        match failed {
            None => {
                grabs.insert((key, mods), r.id);
            }
            Some(e) => {
                for lock in done {
                    if let Ok(cookie) = conn.ungrab_key(key, root, ModMask::from(mods | lock)) {
                        let _ = cookie.check();
                    }
                }
                let taken =
                    matches!(&e, ReplyError::X11Error(x) if x.error_kind == ErrorKind::Access);
                add_problem(
                    &mut problems,
                    r.id,
                    if taken {
                        format!("Another program already uses {}. Pick other keys.", r.keys)
                    } else {
                        format!("Weir could not take {}: {e}", r.keys)
                    },
                );
            }
        }
    }
    (grabs, problems)
}

/// Pass keys going down and up on to the runner, until the connection
/// ends. `remapped` is told when the keyboard layout changes, since keys
/// may then be elsewhere.
fn listen(
    conn: Arc<RustConnection>,
    grabs: Arc<Mutex<HashMap<Grab, HotkeyId>>>,
    tx: UnboundedSender<Command>,
    remapped: Arc<Notify>,
) {
    // Which hotkey each key held now pressed: let go of by key alone, since
    // the modifiers may come up first.
    let mut down: HashMap<Keycode, HotkeyId> = HashMap::new();
    let mut held = Held::default();
    loop {
        match conn.wait_for_event() {
            Ok(Event::KeyPress(e)) => {
                let mods = u16::from(e.state) & MODIFIERS;
                let id = grabs.lock().unwrap().get(&(e.detail, mods)).copied();
                if let Some(id) = id {
                    if down.insert(e.detail, id).is_none() && held.down(id, e.detail) {
                        let _ = tx.send(Command::Press(id));
                    }
                }
            }
            Ok(Event::KeyRelease(e)) => {
                if let Some(id) = down.remove(&e.detail) {
                    if held.up(id, &e.detail) {
                        let _ = tx.send(Command::Release(id));
                    }
                }
            }
            Ok(Event::MappingNotify(_)) => {
                debug!("the keyboard layout changed; grabbing the hotkeys' keys again");
                remapped.notify_one();
            }
            Ok(_) => {}
            Err(e) => {
                warn!("lost the X11 connection, so hotkeys stop: {e}");
                break;
            }
        }
    }
}

/// Grab the hotkeys' keys on `display`, and keep them grabbed as they
/// change, until the daemon stops.
pub async fn run(
    controller: &Controller,
    tx: &UnboundedSender<Command>,
    mut regs: watch::Receiver<Vec<Registration>>,
    display: &str,
    xauthority: Option<String>,
) -> Result<(), String> {
    let display = display.to_string();
    let (conn, screen) =
        tokio::task::spawn_blocking(move || connect(&display, xauthority.as_deref()))
            .await
            .map_err(|e| e.to_string())??;
    let conn = Arc::new(conn);
    let root = conn.setup().roots.get(screen).ok_or("no such screen")?.root;
    // Without this, a held key comes as up and down again and again, which
    // would end a push to talk many times a second.
    let detectable = conn
        .xkb_use_extension(1, 0)
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some_and(|r| r.supported)
        && conn
            .xkb_per_client_flags(
                xkb::ID::USE_CORE_KBD.into(),
                xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT,
                xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT,
                xkb::BoolCtrl::from(0u32),
                xkb::BoolCtrl::from(0u32),
                xkb::BoolCtrl::from(0u32),
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
    if !detectable {
        warn!("this X server repeats held keys as presses; holding a hotkey may misbehave");
    }
    let grabs: Arc<Mutex<HashMap<Grab, HotkeyId>>> = Arc::default();
    let remapped = Arc::new(Notify::new());
    {
        let (conn, grabs, tx, remapped) =
            (conn.clone(), grabs.clone(), tx.clone(), remapped.clone());
        std::thread::Builder::new()
            .name("hotkeys-x11".into())
            .spawn(move || listen(conn, grabs, tx, remapped))
            .map_err(|e| e.to_string())?;
    }
    info!("hotkeys come straight from X11");
    loop {
        let current = regs.borrow_and_update().clone();
        let old = grabs.lock().unwrap().clone();
        let (new, problems) = {
            let conn = conn.clone();
            tokio::task::spawn_blocking(move || regrab(&conn, root, &old, &current))
                .await
                .map_err(|e| e.to_string())?
        };
        *grabs.lock().unwrap() = new;
        let _ = conn.flush();
        // Keys another program has may come free when it closes.
        let retry = !problems.is_empty();
        controller.set_keys_status(status(KeysMethod::X11, MESSAGE), problems);
        tokio::select! {
            changed = regs.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
            }
            _ = remapped.notified() => {}
            _ = tokio::time::sleep(RETRY), if retry => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xauthority_entries_are_read_by_display() {
        let mut file = Vec::new();
        let mut push = |family: u16, fields: [&[u8]; 4]| {
            file.extend_from_slice(&family.to_be_bytes());
            for f in fields {
                file.extend_from_slice(&(f.len() as u16).to_be_bytes());
                file.extend_from_slice(f);
            }
        };
        push(256, [b"host", b"1", b"MIT-MAGIC-COOKIE-1", &[1, 2, 3]]);
        push(256, [b"host", b"0", b"MIT-MAGIC-COOKIE-1", &[4, 5, 6]]);
        let dir = std::env::temp_dir().join(format!("weir-xauth-{}", std::process::id()));
        std::fs::write(&dir, &file).unwrap();
        assert_eq!(
            read_xauthority(&dir, 0),
            Some((b"MIT-MAGIC-COOKIE-1".to_vec(), vec![4, 5, 6]))
        );
        assert_eq!(read_xauthority(&dir, 7), None);
        let _ = std::fs::remove_file(&dir);
    }
}
