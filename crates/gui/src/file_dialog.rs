//! The desktop's own dialogs for picking a file to open or a place to save
//! one, through the XDG desktop portal's FileChooser, so that they look
//! like every other program's. KDE Plasma, GNOME and the other desktops
//! answer there. A dialog runs on a thread of its own, since it waits for
//! the person, and the window asks each frame whether it has an answer.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use tracing::debug;
use zbus::zvariant::{OwnedValue, Value};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";

/// How a dialog ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    /// The file picked, or the place to save to.
    File(PathBuf),
    /// Closed without picking.
    Cancelled,
    /// There is no dialog to show, and why: no portal, as on some bare
    /// window managers. The window asks for the path in words instead.
    NoDialog(String),
}

/// A dialog that is open.
pub struct Dialog {
    answer: mpsc::Receiver<Picked>,
}

impl Dialog {
    /// Ask for a file to open, of the kinds `filters` names: each a name
    /// and its patterns, such as `("Weir settings", &["*.zip", "*.json"])`.
    pub fn open(ctx: &egui::Context, title: &str, filters: &[(&str, &[&str])]) -> Self {
        let options = vec![("filters", filters_value(filters))];
        Self::start(ctx, "OpenFile", title, options)
    }

    /// Ask where to save a file, suggesting `name`.
    pub fn save(ctx: &egui::Context, title: &str, name: &str, filters: &[(&str, &[&str])]) -> Self {
        let options = vec![
            (
                "current_name",
                Value::from(name.to_string()).try_into().ok(),
            ),
            ("filters", filters_value(filters)),
        ];
        Self::start(ctx, "SaveFile", title, options)
    }

    fn start(
        ctx: &egui::Context,
        method: &'static str,
        title: &str,
        options: Vec<(&'static str, Option<OwnedValue>)>,
    ) -> Self {
        let (tx, answer) = mpsc::channel();
        let title = title.to_string();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("weir-file-dialog".into())
            .spawn({
                let tx = tx.clone();
                move || {
                    let picked = match ask(method, &title, options) {
                        Ok(Some(path)) => Picked::File(path),
                        Ok(None) => Picked::Cancelled,
                        Err(e) => {
                            debug!("no file dialog: {e}");
                            Picked::NoDialog(e.to_string())
                        }
                    };
                    let _ = tx.send(picked);
                    ctx.request_repaint();
                }
            });
        if let Err(e) = spawned {
            let _ = tx.send(Picked::NoDialog(e.to_string()));
        }
        Self { answer }
    }

    /// How it ended, once it has.
    pub fn poll(&self) -> Option<Picked> {
        match self.answer.try_recv() {
            Ok(picked) => Some(picked),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Picked::Cancelled),
        }
    }
}

/// The portal's filters, `a(sa(us))`: each a name and its glob patterns
/// (kind 0).
fn filters_value(filters: &[(&str, &[&str])]) -> Option<OwnedValue> {
    let filters: Vec<(String, Vec<(u32, String)>)> = filters
        .iter()
        .map(|(name, globs)| {
            (
                name.to_string(),
                globs.iter().map(|g| (0u32, g.to_string())).collect(),
            )
        })
        .collect();
    Value::from(filters).try_into().ok()
}

/// Show the dialog and wait for the person. `None` when they cancel.
fn ask(
    method: &str,
    title: &str,
    options: Vec<(&str, Option<OwnedValue>)>,
) -> zbus::Result<Option<PathBuf>> {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let conn = zbus::blocking::Connection::session()?;
    // The request's path follows from the connection's name and a token of
    // our choosing, so its answer can be listened for before asking: a
    // quick answer is never missed.
    let token = format!("weir{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let sender = conn
        .unique_name()
        .map(|n| n.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default();
    let path = format!("{DESKTOP_PATH}/request/{sender}/{token}");
    let request = zbus::blocking::Proxy::new(
        &conn,
        DESKTOP,
        path.as_str(),
        "org.freedesktop.portal.Request",
    )?;
    let mut responses = request.receive_signal("Response")?;
    let chooser = zbus::blocking::Proxy::new(
        &conn,
        DESKTOP,
        DESKTOP_PATH,
        "org.freedesktop.portal.FileChooser",
    )?;
    let mut opts: HashMap<&str, OwnedValue> = options
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect();
    if let Ok(t) = Value::from(token.clone()).try_into() {
        opts.insert("handle_token", t);
    }
    if let Ok(t) = Value::from(true).try_into() {
        opts.insert("modal", t);
    }
    let _handle: zbus::zvariant::OwnedObjectPath = chooser.call(method, &("", title, opts))?;
    let Some(message) = responses.next() else {
        return Ok(None);
    };
    let (code, results): (u32, HashMap<String, OwnedValue>) = message.body().deserialize()?;
    // 0 picked, 1 cancelled, 2 ended some other way.
    if code != 0 {
        return Ok(None);
    }
    let uris: Vec<String> = results
        .get("uris")
        .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default();
    Ok(uris.first().and_then(|u| file_path(u)))
}

/// The path a `file://` URI names, its `%XX` escapes undone.
fn file_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(rest.len());
    let mut it = rest.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hex: Vec<u8> = it.by_ref().take(2).collect();
            if hex.len() != 2 {
                return None;
            }
            let byte = std::str::from_utf8(&hex)
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())?;
            bytes.push(byte);
        } else {
            bytes.push(b);
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_become_paths() {
        assert_eq!(
            file_path("file:///home/me/My%20settings.zip"),
            Some(PathBuf::from("/home/me/My settings.zip"))
        );
        assert_eq!(
            file_path("file:///tmp/caf%C3%A9.json"),
            Some(PathBuf::from("/tmp/café.json"))
        );
        assert_eq!(file_path("https://example.com/x"), None);
        assert_eq!(file_path("file:///bad%2"), None);
    }
}
