//! The mixer window's side of hotkeys: opening the Hotkeys window and the
//! editor, and the "Add a hotkey…" item in controls' right-click menus.

use super::App;
use crate::hotkeys::simple::Simple;
use crate::hotkeys::{key_chips, Editor, HotkeysWindow, ListAction};
use crate::theme;
use egui::{RichText, Ui};
use std::time::Instant;
use weir_protocol::*;

impl App {
    /// Open the Hotkeys window, or bring it forward.
    pub(super) fn open_hotkeys(&mut self) {
        match &mut self.hotkeys_window {
            Some(w) => w.raise = true,
            None => self.hotkeys_window = Some(HotkeysWindow::new()),
        }
    }

    /// Open the editor on `editor`, in place of any open one.
    fn open_editor(&mut self, editor: Editor) {
        let mut editor = editor;
        editor.raise = self.hotkey_editor.is_some();
        self.hotkey_editor = Some(editor);
    }

    /// Open the editor on hotkey `id`, or bring it forward if it is open.
    fn edit_hotkey(&mut self, state: &FullState, id: HotkeyId) {
        if let Some(e) = self.hotkey_editor.as_mut().filter(|e| e.id() == id) {
            e.raise = true;
            return;
        }
        if let Some(h) = state.hotkeys.hotkeys.iter().find(|h| h.id == id) {
            self.open_editor(Editor::edit(h, state));
        }
    }

    /// Draw the Hotkeys window and the editor, when open.
    pub(super) fn show_hotkeys(
        &mut self,
        ctx: &egui::Context,
        state: &FullState,
        error: Option<&(String, Instant)>,
    ) {
        let mut asked = None;
        if let Some(w) = &mut self.hotkeys_window {
            asked = w.show(ctx, state, &mut self.actions);
            if w.closed {
                self.hotkeys_window = None;
            }
        }
        match asked {
            Some(ListAction::Add) => {
                let start = Simple::new(Simple::first_target(state), state);
                self.open_editor(Editor::new(start));
            }
            Some(ListAction::Edit(id)) => self.edit_hotkey(state, id),
            None => {}
        }
        if let Some(e) = &mut self.hotkey_editor {
            e.show(ctx, state, error, &mut self.actions);
            let export = std::mem::take(&mut e.export);
            let id = e.id();
            if e.closed {
                self.hotkey_editor = None;
            }
            if let Some(h) = state.hotkeys.hotkeys.iter().find(|h| export && h.id == id) {
                self.export_one(ctx, super::transfer::One::Hotkey(h.id, h.name.clone()));
            }
        }
    }

    /// A control's right-click items: add a hotkey starting from `start`,
    /// which does what the control does, and the hotkeys already on it.
    pub(super) fn hotkey_items(&mut self, ui: &mut Ui, state: &FullState, start: Simple) {
        if ui
            .button("Add a hotkey…")
            .on_hover_text("Keys that do this from anywhere, even in a full-screen game")
            .clicked()
        {
            self.open_editor(Editor::new(start.clone()));
            ui.close();
        }
        let base = Simple::new(Simple::first_target(state), state);
        let on_this: Vec<&Hotkey> = state
            .hotkeys
            .hotkeys
            .iter()
            .filter(|h| {
                Simple::read(h, &base, &state.mixer).is_some_and(|s| s.same_control(&start))
            })
            .collect();
        if on_this.is_empty() {
            return;
        }
        ui.separator();
        ui.label(
            RichText::new("Hotkeys already on this")
                .size(11.0)
                .color(theme::p().text_dim),
        );
        for h in on_this {
            let r = ui
                .horizontal(|ui| {
                    let r = ui.button(&h.name);
                    if let Some(keys) = h.keys.first() {
                        key_chips(ui, keys, 11.0);
                    }
                    r
                })
                .inner;
            if r.on_hover_text("Change this hotkey").clicked() {
                self.edit_hotkey(state, h.id);
                ui.close();
            }
        }
    }
}
