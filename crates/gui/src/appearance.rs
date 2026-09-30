//! The desktop's light or dark preference and its accent color, read from
//! the XDG desktop portal (`org.freedesktop.appearance` in its Settings
//! interface) and followed as they change. KDE Plasma, GNOME and most other
//! desktops answer there. Without a portal, nothing is known and the window
//! stays dark.

use egui::Color32;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use tracing::debug;
use zbus::zvariant::{OwnedValue, Value};

const NAMESPACE: &str = "org.freedesktop.appearance";

/// What the desktop prefers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    NoPreference,
    Dark,
    Light,
}

/// The portal's `color-scheme`: 0 no preference, 1 dark, 2 light.
static SCHEME: AtomicU8 = AtomicU8::new(0);
/// The portal's `accent-color` as `0x01rrggbb`, or 0 while there is none.
static ACCENT: AtomicU32 = AtomicU32::new(0);
/// The window to repaint when either changes, once it exists.
static CTX: OnceLock<egui::Context> = OnceLock::new();

/// Whether the desktop prefers light or dark, as last heard.
pub fn scheme() -> Scheme {
    match SCHEME.load(Ordering::Relaxed) {
        1 => Scheme::Dark,
        2 => Scheme::Light,
        _ => Scheme::NoPreference,
    }
}

/// The desktop's accent color, if it has said what it is.
pub fn accent() -> Option<Color32> {
    let v = ACCENT.load(Ordering::Relaxed);
    (v != 0).then(|| {
        let [_, r, g, b] = v.to_be_bytes();
        Color32::from_rgb(r, g, b)
    })
}

/// Repaint `ctx` whenever the desktop's appearance changes.
pub fn set_context(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}

/// Start following the desktop's appearance on a thread of its own. Waits
/// up to `wait` for the first answer, so the window can open in the right
/// colors, but never longer: a desktop without a portal just means the
/// default.
pub fn watch(wait: Duration) {
    let first = Arc::new((Mutex::new(false), Condvar::new()));
    let signal = first.clone();
    let spawned = std::thread::Builder::new()
        .name("weir-appearance".into())
        .spawn(move || {
            let answered = || {
                *signal.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
                signal.1.notify_all();
            };
            if let Err(e) = follow(&answered) {
                debug!("not following the desktop's appearance: {e}");
            }
            answered();
        });
    if spawned.is_err() {
        return;
    }
    let (done, cv) = &*first;
    let guard = done.lock().unwrap_or_else(|e| e.into_inner());
    let _ = cv.wait_timeout_while(guard, wait, |done| !*done);
}

fn follow(answered: &dyn Fn()) -> zbus::Result<()> {
    let conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
    )?;
    // Listen before reading, so a change in between is not missed.
    let changes = proxy.receive_signal("SettingChanged")?;
    for key in ["color-scheme", "accent-color"] {
        if let Some(value) = read(&proxy, key) {
            store(key, &value);
        }
    }
    debug!("desktop appearance: {:?}, accent {:?}", scheme(), accent());
    answered();
    for message in changes {
        let Ok((namespace, key, value)) =
            message.body().deserialize::<(String, String, OwnedValue)>()
        else {
            continue;
        };
        if namespace == NAMESPACE && store(&key, &value) {
            debug!(
                "desktop appearance now {:?}, accent {:?}",
                scheme(),
                accent()
            );
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint();
            }
        }
    }
    Ok(())
}

/// One setting. `ReadOne` is the current method; portals older than
/// version 2 only have `Read`, which wraps the value in one more variant.
fn read(proxy: &zbus::blocking::Proxy, key: &str) -> Option<OwnedValue> {
    proxy
        .call::<_, _, OwnedValue>("ReadOne", &(NAMESPACE, key))
        .or_else(|_| proxy.call::<_, _, OwnedValue>("Read", &(NAMESPACE, key)))
        .ok()
}

/// Take in a setting's new value. Returns whether it was one we follow.
fn store(key: &str, value: &Value) -> bool {
    match key {
        "color-scheme" => match scheme_of(value) {
            Some(v) => {
                SCHEME.store(v, Ordering::Relaxed);
                true
            }
            None => false,
        },
        "accent-color" => {
            let v = accent_of(value).map_or(0, |c| u32::from_be_bytes([1, c.r(), c.g(), c.b()]));
            ACCENT.store(v, Ordering::Relaxed);
            true
        }
        _ => false,
    }
}

fn scheme_of(value: &Value) -> Option<u8> {
    match value {
        Value::U32(v) => Some((*v).min(2) as u8),
        Value::Value(inner) => scheme_of(inner),
        _ => None,
    }
}

/// An accent color: three doubles from 0 to 1. Anything outside that range
/// means the desktop has none set.
fn accent_of(value: &Value) -> Option<Color32> {
    match value {
        Value::Value(inner) => accent_of(inner),
        Value::Structure(s) => match s.fields() {
            [Value::F64(r), Value::F64(g), Value::F64(b)] => {
                let channel = |v: f64| (0.0..=1.0).contains(&v).then(|| (v * 255.0).round() as u8);
                Some(Color32::from_rgb(channel(*r)?, channel(*g)?, channel(*b)?))
            }
            _ => None,
        },
        _ => None,
    }
}

/// The desktop's font: its name and the font file's contents, if one can be
/// found. KDE names it in `kdeglobals`; elsewhere fontconfig's usual sans
/// serif face stands in. Looked up once.
pub fn desktop_font() -> Option<&'static (String, Arc<egui::FontData>)> {
    static FONT: OnceLock<Option<(String, Arc<egui::FontData>)>> = OnceLock::new();
    FONT.get_or_init(|| {
        let family = kde_font_family().unwrap_or_else(|| "sans-serif".into());
        let out = std::process::Command::new("fc-match")
            .args(["--format", "%{family[0]}\n%{file}", &family])
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let (name, file) = text.split_once('\n')?;
        let bytes = std::fs::read(file.trim()).ok()?;
        debug!("desktop font: {name} from {}", file.trim());
        Some((
            name.to_string(),
            Arc::new(egui::FontData::from_owned(bytes)),
        ))
    })
    .as_ref()
}

/// The family of KDE's general font, from `font=Noto Sans,10,...` in the
/// `[General]` group of `kdeglobals`.
fn kde_font_family() -> Option<String> {
    let text = std::fs::read_to_string(dirs::config_dir()?.join("kdeglobals")).ok()?;
    let mut general = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            general = line == "[General]";
        } else if general {
            if let Some(v) = line.strip_prefix("font=") {
                let family = v.split(',').next()?.trim();
                return (!family.is_empty()).then(|| family.to_string());
            }
        }
    }
    None
}

/// Draw text in the desktop's font, with egui's own behind it for anything
/// the font lacks, or in egui's alone.
pub fn use_desktop_font(ctx: &egui::Context, on: bool) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some((name, data)) = desktop_font().filter(|_| on) {
        fonts.font_data.insert(name.clone(), data.clone());
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            list.insert(0, name.clone());
        }
    }
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Structure;

    #[test]
    fn settings_are_read_whether_wrapped_or_not() {
        assert_eq!(scheme_of(&Value::U32(1)), Some(1));
        assert_eq!(scheme_of(&Value::Value(Box::new(Value::U32(2)))), Some(2));
        assert_eq!(scheme_of(&Value::U32(7)), Some(2));
        let accent = Value::Structure(Structure::from((0.2f64, 0.6f64, 1.0f64)));
        assert_eq!(accent_of(&accent), Some(Color32::from_rgb(51, 153, 255)));
        let wrapped = Value::Value(Box::new(accent));
        assert!(accent_of(&wrapped).is_some());
        // Out of range means none is set.
        let none = Value::Structure(Structure::from((-1.0f64, 0.5f64, 0.5f64)));
        assert_eq!(accent_of(&none), None);
    }
}
