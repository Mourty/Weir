//! The mixer window's side of export and import: opening the Export and
//! Import windows, each scene's, setup's, hotkey's and preset's own
//! "Export…", and handing the daemon's answers to whoever waits for them.

use super::App;
use crate::client::Transfer;
use crate::file_dialog::{Dialog, Picked};
use crate::transfer::{ExportWindow, ImportWindow};
use serde_json::Value;
use std::path::PathBuf;
use weir_protocol::*;

/// One thing exported on its own, to a bare `.json`.
#[derive(Debug, Clone)]
pub(super) enum One {
    Scene(String),
    Setup(String),
    Hotkey(HotkeyId, String),
    EqPreset(String),
}

impl One {
    fn name(&self) -> &str {
        match self {
            One::Scene(n) | One::Setup(n) | One::Hotkey(_, n) | One::EqPreset(n) => n,
        }
    }

    fn word(&self) -> &'static str {
        match self {
            One::Scene(_) => "scene",
            One::Setup(_) => "setup",
            One::Hotkey(..) => "hotkey",
            One::EqPreset(_) => "equalizer preset",
        }
    }

    /// The request writing it to `path`.
    fn request(&self, path: String) -> Request {
        let mut p = ExportParams {
            path,
            ..Default::default()
        };
        match self {
            One::Scene(n) => p.scenes.push(n.clone()),
            One::Setup(n) => p.setups.push(n.clone()),
            One::Hotkey(id, _) => p.hotkeys.push(HotkeyKey::Id(*id)),
            One::EqPreset(n) => p.eq_presets.push(n.clone()),
        }
        Request::ExportSettings(p)
    }
}

/// Who waits for the answer to an export: the Export window, or one
/// thing's own "Export…", which says how it went in a toast.
#[derive(Debug, Clone)]
pub(super) enum ExportWaiter {
    Window,
    One(One),
}

impl App {
    /// Open the Export window, or bring it forward.
    pub(super) fn open_export(&mut self) {
        match &mut self.export_window {
            Some(w) => w.raise = true,
            None => self.export_window = Some(ExportWindow::new()),
        }
    }

    /// Ask for a file to import, in the Import window.
    pub(super) fn open_import(&mut self, ctx: &egui::Context) {
        match &mut self.import_window {
            Some(w) => w.raise = true,
            None => self.import_window = Some(ImportWindow::new(ctx)),
        }
    }

    /// Export `one` alone, asking where.
    pub(super) fn export_one(&mut self, ctx: &egui::Context, one: One) {
        let file = format!("{}.json", one.name().replace('/', "_"));
        let title = format!("Export the {} '{}'", one.word(), one.name());
        let dialog = Dialog::save(ctx, &title, &file, &[("Weir settings", &["*.json"])]);
        self.export_one = Some((dialog, one));
    }

    /// Each frame: the dialog of one thing's export, the daemon's answers,
    /// and the Export and Import windows.
    pub(super) fn show_transfer(
        &mut self,
        ctx: &egui::Context,
        state: &FullState,
        answers: Vec<(Transfer, Result<Value, String>)>,
    ) {
        if let Some(picked) = self.export_one.as_ref().and_then(|(d, _)| d.poll()) {
            let (_, one) = self.export_one.take().expect("polled");
            match picked {
                Picked::File(mut path) => {
                    if path
                        .extension()
                        .is_none_or(|e| !e.eq_ignore_ascii_case("json"))
                    {
                        path.as_mut_os_string().push(".json");
                    }
                    self.actions.push(one.request(path.display().to_string()));
                    self.export_waiters.push_back(ExportWaiter::One(one));
                }
                Picked::Cancelled => {}
                Picked::NoDialog(_) => self.show_toast(
                    "Your desktop has no file dialog Weir can use: use Export settings in the … \
                     menu",
                    false,
                ),
            }
        }
        for (kind, answer) in answers {
            match kind {
                Transfer::Export => match self.export_waiters.pop_front() {
                    Some(ExportWaiter::One(one)) => {
                        let text = match answer {
                            Ok(v) => {
                                let done: Option<ExportResult> = serde_json::from_value(v).ok();
                                let path = done.map(|d| PathBuf::from(d.path));
                                let file = path
                                    .as_ref()
                                    .and_then(|p| p.file_name())
                                    .map(|f| f.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                format!("Exported the {} '{}' to {file}", one.word(), one.name())
                            }
                            Err(e) => format!("Not exported: {e}"),
                        };
                        self.show_toast(&text, false);
                    }
                    Some(ExportWaiter::Window) | None => {
                        if let Some(w) = &mut self.export_window {
                            w.reply(answer);
                        }
                    }
                },
                Transfer::Inspect | Transfer::Import => {
                    if let Some(w) = &mut self.import_window {
                        w.reply(kind, answer);
                    }
                }
            }
        }
        let look = self.prefs.look();
        if let Some(w) = &mut self.export_window {
            let waiting = w.waiting;
            w.show(ctx, state, &look, &mut self.actions);
            if w.waiting && !waiting {
                self.export_waiters.push_back(ExportWaiter::Window);
            }
            if w.closed {
                self.export_window = None;
            }
        }
        if let Some(w) = &mut self.import_window {
            w.show(ctx, state, &mut self.actions);
            if w.closed {
                self.import_window = None;
            }
        }
    }
}
