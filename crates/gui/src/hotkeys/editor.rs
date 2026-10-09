//! The window for making or changing one hotkey: its keys, and what they
//! do, picked from a list or, under More options, any steps at all.

use super::record::{self, Recorded};
use super::simple::{Action, Fx, Mode, Simple};
use super::{key_chips, keys_hint};
use crate::theme;
use egui::{vec2, RichText, Ui};
use std::time::{Duration, Instant};
use weir_protocol::*;

/// How long to wait for the daemon to take a saved hotkey, before closing
/// anyway.
const SAVE_WAIT: Duration = Duration::from_secs(5);

/// A hotkey being made or changed.
pub struct Editor {
    /// The hotkey's id, 0 for a new one.
    id: HotkeyId,
    name: String,
    enabled: bool,
    keys: Vec<String>,
    /// The keys being recorded: an index into `keys`, or its length for
    /// another combination.
    recording: Option<usize>,
    /// Why the last keys pressed could not be taken.
    record_note: Option<String>,
    /// What it does, as the simple form shows it.
    simple: Simple,
    /// Whether More options is open, with `full` the hotkey it edits.
    advanced: bool,
    full: Hotkey,
    /// A step being put together under More options, and whether it is for
    /// letting go.
    builder: Simple,
    adding: Option<bool>,
    /// The hotkey as JSON, and the hotkey it was written from.
    json: String,
    json_of: Option<Hotkey>,
    json_error: Option<String>,
    /// Why it cannot be saved, or why the daemon did not take it.
    error: Option<String>,
    /// Sent to be saved: the hotkeys as they were then, and when.
    saving: Option<(Vec<Hotkey>, Instant)>,
    pub raise: bool,
    pub closed: bool,
}

impl Editor {
    /// A new hotkey doing what `simple` says, recording its keys first.
    pub fn new(simple: Simple) -> Editor {
        Editor {
            id: 0,
            name: String::new(),
            enabled: true,
            keys: Vec::new(),
            recording: Some(0),
            record_note: None,
            full: simple.hotkey("", &[]),
            builder: simple.clone(),
            simple,
            advanced: false,
            adding: None,
            json: String::new(),
            json_of: None,
            json_error: None,
            error: None,
            saving: None,
            raise: false,
            closed: false,
        }
    }

    /// Hotkey `h`, in the simple form when it fits there.
    pub fn edit(h: &Hotkey, state: &FullState) -> Editor {
        let base = Simple::new(Simple::first_target(state), state);
        let simple = Simple::read(h, &base);
        Editor {
            id: h.id,
            name: h.name.clone(),
            enabled: h.enabled,
            keys: h.keys.clone(),
            recording: None,
            record_note: None,
            advanced: simple.is_none(),
            simple: simple.unwrap_or_else(|| base.clone()),
            full: h.clone(),
            builder: base,
            adding: None,
            json: String::new(),
            json_of: None,
            json_error: None,
            error: None,
            saving: None,
            raise: false,
            closed: false,
        }
    }

    /// The hotkey it edits: 0 for a new one.
    pub fn id(&self) -> HotkeyId {
        self.id
    }

    /// The hotkey as it stands: what it does, with the name typed (empty
    /// when none was), keys and switch.
    fn assemble(&self) -> Hotkey {
        let mut h = if self.advanced {
            self.full.clone()
        } else {
            self.simple.hotkey("", &[])
        };
        h.id = self.id;
        h.name = self.name.trim().to_string();
        h.keys = self.keys.clone();
        h.enabled = self.enabled;
        h
    }

    /// The name it gets when none is typed: a short one from the simple
    /// form, or else its first step in words.
    fn suggested_name(&self, state: &FullState) -> String {
        let base = Simple::new(Simple::first_target(state), state);
        let h = self.assemble();
        let simple = if self.advanced {
            Simple::read(&h, &base)
        } else {
            Some(self.simple.clone())
        };
        match (simple, h.steps.as_slice()) {
            (Some(s), _) => s.name(&state.mixer),
            (None, [first]) => describe_step(first, &state.mixer),
            (None, [first, ..]) => format!("{}, and more", describe_step(first, &state.mixer)),
            (None, []) => "Hotkey".into(),
        }
    }

    /// The other hotkeys.
    fn others<'a>(&self, state: &'a FullState) -> impl Iterator<Item = &'a Hotkey> {
        let id = self.id;
        state.hotkeys.hotkeys.iter().filter(move |h| h.id != id)
    }

    /// Draw the window. Requests for the daemon are appended to `actions`.
    /// `error` is the daemon's last error and when it came.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        state: &FullState,
        error: Option<&(String, Instant)>,
        actions: &mut Vec<Request>,
    ) {
        // Saved: close once the daemon has it, or say why it did not.
        if let Some((before, at)) = &self.saving {
            if let Some((why, _)) = error.filter(|(_, t)| t > at) {
                self.error = Some(format!("Weir did not save it: {why}"));
                self.saving = None;
            } else if state.hotkeys.hotkeys != *before || at.elapsed() > SAVE_WAIT {
                self.closed = true;
                return;
            }
        }
        let title = if self.id == 0 {
            "New hotkey · Weir"
        } else {
            "Edit hotkey · Weir"
        };
        let id = egui::ViewportId::from_hash_of("hotkey_editor");
        if self.raise {
            self.raise = false;
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
        }
        let builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_app_id("weir")
            .with_icon(crate::icon())
            .with_inner_size([620.0, 700.0])
            .with_min_inner_size([540.0, 420.0]);
        ctx.show_viewport_immediate(id, builder, |ctx, class| {
            if self.recording.is_some() {
                self.take_keys(ctx, state);
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            if class == egui::ViewportClass::Embedded {
                let mut open = true;
                egui::Window::new(title)
                    .id(egui::Id::new("hotkey_editor_embedded"))
                    .open(&mut open)
                    .default_size([620.0, 640.0])
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(520.0)
                            .show(ui, |ui| self.form(ui, state, actions));
                        ui.separator();
                        self.buttons(ui, state, actions);
                    });
                if !open {
                    self.closed = true;
                }
            } else {
                egui::TopBottomPanel::bottom(egui::Id::new("hotkey_editor_buttons"))
                    .frame(
                        egui::Frame::new()
                            .fill(theme::p().bg)
                            .inner_margin(egui::Margin::symmetric(16, 10)),
                    )
                    .show(ctx, |ui| self.buttons(ui, state, actions));
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::p().bg).inner_margin(16))
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink(false)
                            .show(ui, |ui| self.form(ui, state, actions));
                    });
                if ctx.input(|i| i.viewport().close_requested()) {
                    self.closed = true;
                }
            }
        });
    }

    /// Take the keys pressed while recording.
    fn take_keys(&mut self, ctx: &egui::Context, state: &FullState) {
        let Some(slot) = self.recording else {
            return;
        };
        match record::take(ctx) {
            None => {}
            Some(Recorded::Cancel) => {
                self.recording = None;
                self.record_note = None;
            }
            Some(Recorded::Refused(why)) => self.record_note = Some(why),
            Some(Recorded::Keys(keys)) => {
                let mine = self
                    .keys
                    .iter()
                    .enumerate()
                    .any(|(i, k)| i != slot && *k == keys);
                if mine {
                    self.record_note = Some(format!("{keys} is already one of its keys."));
                } else if let Some(other) = self.others(state).find(|h| {
                    h.keys.contains(&keys)
                        || state
                            .hotkeys
                            .keys
                            .assigned
                            .get(&h.id)
                            .is_some_and(|a| a.contains(&keys))
                }) {
                    self.record_note = Some(format!(
                        "{keys} already belongs to the hotkey '{}'. Press other keys.",
                        other.name
                    ));
                } else {
                    if slot < self.keys.len() {
                        self.keys[slot] = keys;
                    } else {
                        self.keys.push(keys);
                    }
                    self.recording = None;
                    self.record_note = None;
                    self.error = None;
                }
            }
        }
    }

    /// Everything above the buttons.
    fn form(&mut self, ui: &mut Ui, state: &FullState, actions: &mut Vec<Request>) {
        ui.spacing_mut().item_spacing.y = 8.0;
        self.keys_section(ui, state, actions);
        ui.add_space(6.0);
        if self.advanced {
            self.name_row(ui, state);
            ui.add_space(6.0);
            self.full_form(ui, state);
        } else {
            let before = self.simple.clone();
            simple_form(ui, state, &mut self.simple, true, "simple");
            if self.simple != before {
                self.simple = self.simple.clone().fitted(state);
                self.error = None;
            }
            ui.add_space(6.0);
            let h = self.assemble();
            egui::Frame::new()
                .fill(theme::p().section_fill)
                .corner_radius(5)
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("In your list of hotkeys")
                            .size(11.0)
                            .color(theme::p().text_dim),
                    );
                    ui.label(describe_hotkey(&h, &state.mixer));
                });
            ui.add_space(6.0);
            self.name_row(ui, state);
        }
    }

    /// The keys, and how to change them: with the desktop's when it looks
    /// after them, or else Weir's own.
    fn keys_section(&mut self, ui: &mut Ui, state: &FullState, actions: &mut Vec<Request>) {
        heading(ui, "When I press");
        let keys = &state.hotkeys.keys;
        if keys.method == KeysMethod::Desktop && !keys.settable {
            self.desktop_keys(ui, state, actions);
        } else {
            self.own_keys(ui, state);
        }
        if let Some(note) = &self.record_note {
            ui.label(RichText::new(note).color(theme::p().warning));
        }
        ui.label(
            RichText::new(keys_hint(state))
                .size(12.0)
                .color(theme::p().text_dim),
        );
    }

    /// The text shown while keys are being recorded.
    fn recording_text() -> RichText {
        RichText::new("Press the keys you want… (Esc to stop)").color(theme::p().warning)
    }

    /// Where Weir looks after the keys: a row of key caps for each
    /// combination, with buttons to record it again or take it away, and a
    /// button for more.
    fn own_keys(&mut self, ui: &mut Ui, state: &FullState) {
        let mut remove = None;
        for (i, keys) in self.keys.iter().enumerate() {
            ui.horizontal(|ui| {
                if self.recording == Some(i) {
                    ui.label(Self::recording_text());
                } else {
                    key_chips(ui, keys, 15.0);
                    ui.add_space(8.0);
                    if ui
                        .small_button("Change")
                        .on_hover_text("Press other keys instead")
                        .clicked()
                    {
                        self.recording = Some(i);
                        self.record_note = None;
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Take these keys away")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                }
            });
        }
        if let Some(i) = remove {
            self.keys.remove(i);
            self.recording = None;
        }
        // Keys set in the desktop's settings that Weir has no name for: they
        // stay as they are, whatever is changed here.
        for k in self.foreign_keys(state) {
            ui.horizontal(|ui| {
                key_chips(ui, &k, 15.0);
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("set in {}", super::settings_name()))
                        .size(12.0)
                        .color(theme::p().text_dim),
                );
            });
        }
        let adding = self.recording == Some(self.keys.len());
        ui.horizontal(|ui| {
            if adding {
                ui.label(Self::recording_text());
            } else if self.keys.is_empty() {
                self.record_button(ui);
            } else if self.keys.len() < HOTKEY_KEYS_MAX
                && ui
                    .small_button("+ Other keys")
                    .on_hover_text("Other keys that do the same, such as a key on a gaming mouse")
                    .clicked()
            {
                self.recording = Some(self.keys.len());
                self.record_note = None;
            }
        });
        // Where Weir sets the desktop's keys, a hotkey the desktop does not
        // have yet is offered to it first, with its first keys.
        let keys = &state.hotkeys.keys;
        let offered = keys.settable
            && self.enabled
            && !self.keys.is_empty()
            && self.recording.is_none()
            && !keys.assigned.contains_key(&self.id);
        if offered {
            ui.label(
                RichText::new(
                    "When you save, your desktop asks you to confirm the first keys; Weir \
                     gives it the others.",
                )
                .size(12.0)
                .color(theme::p().text_dim),
            );
        }
    }

    /// Keys the desktop has for this hotkey that Weir cannot write as its
    /// own.
    fn foreign_keys(&self, state: &FullState) -> Vec<String> {
        state
            .hotkeys
            .keys
            .assigned
            .get(&self.id)
            .into_iter()
            .flatten()
            .filter(|k| KeyCombo::parse(k).is_err())
            .cloned()
            .collect()
    }

    /// The button to record keys for a hotkey that has none.
    fn record_button(&mut self, ui: &mut Ui) {
        if ui
            .button(RichText::new("Record keys").size(14.0))
            .on_hover_text("Click, then press the keys you want")
            .clicked()
        {
            self.recording = Some(0);
            self.record_note = None;
        }
        ui.label(
            RichText::new("No keys: it can still be pressed by name.").color(theme::p().text_dim),
        );
    }

    /// Where the desktop looks after the keys: each hotkey is one entry in
    /// its shortcut settings, which takes one suggestion from Weir, and
    /// where more keys are added. Once saved, the keys shown are the
    /// desktop's.
    fn desktop_keys(&mut self, ui: &mut Ui, state: &FullState, actions: &mut Vec<Request>) {
        let settings = super::settings_name();
        let keys = &state.hotkeys.keys;
        let saved = state.hotkeys.hotkeys.iter().find(|h| h.id == self.id);
        let suggestion_kept = saved.is_some_and(|h| h.keys.first() == self.keys.first());
        let given = keys
            .assigned
            .get(&self.id)
            .filter(|_| suggestion_kept && self.recording.is_none());
        if let Some(given) = given {
            if given.is_empty() {
                ui.label(
                    RichText::new(format!("No keys yet: give it some in {settings}."))
                        .color(theme::p().warning),
                );
            }
            for k in given {
                ui.horizontal(|ui| key_chips(ui, k, 15.0));
            }
            ui.horizontal(|ui| {
                if keys.configurable
                    && ui
                        .button(format!("Add or change keys in {settings}"))
                        .on_hover_text(
                            "Each hotkey is one entry there: give it more keys, or change them",
                        )
                        .clicked()
                {
                    actions.push(Request::OpenShortcutSettings);
                }
                if ui
                    .small_button("Suggest other keys")
                    .on_hover_text(
                        "Press keys for Weir to suggest instead. Your desktop asks you to \
                         confirm them, and they replace the keys it has for this hotkey.",
                    )
                    .clicked()
                {
                    self.recording = Some(0);
                    self.record_note = None;
                }
                if ui
                    .small_button("No keys")
                    .on_hover_text(
                        "Take its keys away: it leaves your desktop's shortcut settings, and \
                         can still be pressed by name",
                    )
                    .clicked()
                {
                    self.keys.clear();
                }
            });
            if !keys.configurable {
                ui.label(
                    RichText::new(format!(
                        "To give it more keys or change them, open {}, where each hotkey is \
                         one entry.",
                        super::settings_path()
                    ))
                    .size(12.0)
                    .color(theme::p().text_dim),
                );
            }
            return;
        }
        // Keys to suggest, or none.
        ui.horizontal(|ui| match (self.recording, self.keys.first()) {
            (Some(_), _) => {
                ui.label(Self::recording_text());
            }
            (None, Some(k)) => {
                key_chips(ui, k, 15.0);
                ui.add_space(8.0);
                if ui
                    .small_button("Change")
                    .on_hover_text("Press other keys instead")
                    .clicked()
                {
                    self.recording = Some(0);
                    self.record_note = None;
                }
                if ui
                    .small_button("×")
                    .on_hover_text("Take the keys away")
                    .clicked()
                {
                    self.keys.clear();
                }
            }
            (None, None) => self.record_button(ui),
        });
        let had_keys = saved.is_some_and(|h| !h.keys.is_empty());
        let note = if self.keys.is_empty() || self.recording.is_some() {
            None
        } else if !self.enabled {
            Some(
                "It is switched off: your desktop asks you to confirm these keys once you \
                 switch it on."
                    .to_string(),
            )
        } else if suggestion_kept {
            // Saved as it is, and the desktop has not said yet.
            None
        } else if had_keys {
            Some(format!(
                "Saving asks your desktop to give it these keys, in place of the ones \
                 {settings} has for it."
            ))
        } else {
            Some(format!(
                "Your desktop asks you to confirm these keys. After saving, you can give it \
                 more in {settings}."
            ))
        };
        if let Some(note) = note {
            ui.label(RichText::new(note).size(12.0).color(theme::p().text_dim));
        }
    }

    /// The name, with what it does as the suggestion.
    fn name_row(&mut self, ui: &mut Ui, state: &FullState) {
        let suggestion = self.suggested_name(state);
        ui.horizontal(|ui| {
            ui.label("Name");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.name)
                        .hint_text(suggestion)
                        .char_limit(HOTKEY_NAME_MAX)
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                self.error = None;
            }
        });
        ui.label(
            RichText::new(
                "Optional. weirctl, scripts and Stream Deck buttons can press it by name.",
            )
            .size(12.0)
            .color(theme::p().text_dim),
        );
    }

    /// More options: every step, what each press and letting go do, and the
    /// hotkey as the requests Weir runs.
    fn full_form(&mut self, ui: &mut Ui, state: &FullState) {
        heading(ui, "Steps");
        if self.full.steps.is_empty() {
            ui.label(RichText::new("No steps yet.").color(theme::p().text_dim));
        }
        steps_list(ui, state, &mut self.full.steps, "steps");
        self.step_builder(ui, state, false);

        ui.add_space(6.0);
        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            heading(ui, "Each press");
            ui.radio_value(&mut self.full.each_press, EachPress::All, "Does every step");
            ui.radio_value(
                &mut self.full.each_press,
                EachPress::Next,
                "Does the next step only, going round",
            );
            ui.add_space(6.0);
            heading(ui, "While held");
            let mut repeat = self.full.repeat_ms.is_some();
            ui.horizontal(|ui| {
                ui.checkbox(&mut repeat, "Do it again every");
                let mut ms = self.full.repeat_ms.unwrap_or(super::simple::REPEAT_MS);
                let (lo, hi) = HOTKEY_REPEAT_MS;
                // Not pulled into range: a value typed in the JSON below
                // stays, and Weir says on saving if it cannot take it.
                ui.add_enabled(
                    repeat,
                    egui::DragValue::new(&mut ms)
                        .range(lo..=hi)
                        .clamp_existing_to_range(false)
                        .speed(5.0)
                        .suffix(" ms"),
                );
                self.full.repeat_ms = repeat.then_some(ms);
            });
            ui.label(
                RichText::new("For volume and pan changes.")
                    .size(12.0)
                    .color(theme::p().text_dim),
            );

            let ui = &mut cols[1];
            heading(ui, "When I let go");
            ui.radio_value(&mut self.full.on_release, OnRelease::Nothing, "Nothing");
            ui.radio_value(
                &mut self.full.on_release,
                OnRelease::Restore,
                "Put back what pressing changed",
            );
            ui.radio_value(
                &mut self.full.on_release,
                OnRelease::Steps,
                "Do other steps",
            );
        });
        if self.full.on_release == OnRelease::Steps {
            ui.add_space(4.0);
            heading(ui, "Steps when I let go");
            steps_list(ui, state, &mut self.full.release_steps, "release");
            self.step_builder(ui, state, true);
        }
        ui.add_space(6.0);
        self.json_section(ui);
    }

    /// The button to add a step, and once clicked, the simple form to make
    /// it with.
    fn step_builder(&mut self, ui: &mut Ui, state: &FullState, release: bool) {
        if self.adding != Some(release) {
            let label = if release {
                "+ Add a step for letting go"
            } else {
                "+ Add a step"
            };
            if ui.button(label).clicked() {
                self.adding = Some(release);
                self.builder = self.builder.clone().fitted(state);
            }
            return;
        }
        egui::Frame::new()
            .fill(theme::p().section_fill)
            .corner_radius(5)
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let before = self.builder.clone();
                simple_form(ui, state, &mut self.builder, false, "builder");
                if self.builder != before {
                    self.builder = self.builder.clone().fitted(state);
                }
                ui.horizontal(|ui| {
                    let list = if release {
                        &mut self.full.release_steps
                    } else {
                        &mut self.full.steps
                    };
                    let room = list.len() < HOTKEY_STEPS_MAX;
                    if ui
                        .add_enabled(room, egui::Button::new("Add this step"))
                        .clicked()
                    {
                        list.push(self.builder.step());
                        self.adding = None;
                        self.error = None;
                    }
                    if ui.button("Cancel").clicked() {
                        self.adding = None;
                    }
                });
            });
    }

    /// The hotkey as JSON, to read or edit: what Weir runs, which the API
    /// documents.
    fn json_section(&mut self, ui: &mut Ui) {
        let current = self.assemble();
        let editing = ui.memory(|m| m.has_focus(egui::Id::new("hotkey_json")));
        if !editing && self.json_of.as_ref() != Some(&current) {
            self.json = to_json_text(&current);
            self.json_of = Some(current);
            self.json_error = None;
        }
        ui.horizontal(|ui| {
            heading(ui, "As API requests");
            ui.label(
                RichText::new("Edit freely: Weir checks it when you save.")
                    .size(12.0)
                    .color(theme::p().text_dim),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(self.json.clone());
                }
            });
        });
        let r = ui.add(
            egui::TextEdit::multiline(&mut self.json)
                .id(egui::Id::new("hotkey_json"))
                .code_editor()
                .desired_rows(8)
                .desired_width(f32::INFINITY),
        );
        if r.changed() {
            match serde_json::from_str::<Hotkey>(&self.json) {
                Ok(h) => {
                    self.name = h.name.clone();
                    self.keys = h.keys.clone();
                    self.enabled = h.enabled;
                    self.full = Hotkey { id: self.id, ..h };
                    self.json_of = Some(self.assemble());
                    self.json_error = None;
                    self.error = None;
                }
                Err(e) => self.json_error = Some(e.to_string()),
            }
        }
        if let Some(e) = &self.json_error {
            ui.label(
                RichText::new(format!("Not valid yet: {e}"))
                    .size(12.0)
                    .color(theme::p().warning),
            );
        }
    }

    /// More or fewer options, Cancel and Save.
    fn buttons(&mut self, ui: &mut Ui, state: &FullState, actions: &mut Vec<Request>) {
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(theme::p().warning));
        }
        ui.horizontal(|ui| {
            if self.advanced {
                let base = Simple::new(Simple::first_target(state), state);
                let simple = Simple::read(&self.assemble(), &base);
                let r = ui.add_enabled(simple.is_some(), egui::Button::new("Fewer options"));
                let r = if simple.is_none() {
                    r.on_disabled_hover_text("This hotkey does more than the simple form can show.")
                } else {
                    r
                };
                if r.clicked() {
                    if let Some(s) = simple {
                        self.simple = s;
                        self.advanced = false;
                    }
                }
            } else if ui
                .button("More options…")
                .on_hover_text(
                    "Several steps, cycling through them, fades, and the requests Weir runs",
                )
                .clicked()
            {
                self.full = self.assemble();
                self.builder = self.simple.clone();
                self.advanced = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let saving = self.saving.is_some();
                let save = egui::Button::new(
                    RichText::new(if saving { "Saving…" } else { "Save" })
                        .strong()
                        .color(theme::p().on_text),
                )
                .fill(theme::p().accent)
                .min_size(vec2(80.0, 0.0));
                if ui.add_enabled(!saving, save).clicked() {
                    self.save(state, actions);
                }
                if ui.button("Cancel").clicked() {
                    self.closed = true;
                }
            });
        });
    }

    /// Check the hotkey and send it to be saved.
    fn save(&mut self, state: &FullState, actions: &mut Vec<Request>) {
        self.recording = None;
        let mut h = self.assemble();
        match self.checked(state, &mut h) {
            Err(why) => self.error = Some(why),
            Ok(()) => {
                let unchanged = state.hotkeys.hotkeys.contains(&h);
                if unchanged {
                    self.closed = true;
                    return;
                }
                actions.push(Request::SetHotkey(h));
                self.saving = Some((state.hotkeys.hotkeys.clone(), Instant::now()));
                self.error = None;
            }
        }
    }

    /// What would stop `h` being saved, and the name it gets when none was
    /// typed.
    fn checked(&self, state: &FullState, h: &mut Hotkey) -> Result<(), String> {
        if h.steps.is_empty() {
            return Err("Add a step for it to do.".into());
        }
        if h.on_release == OnRelease::Steps && h.release_steps.is_empty() {
            return Err(
                "Add a step for letting go, or pick another choice under When I let go.".into(),
            );
        }
        for keys in &h.keys {
            if let Some(other) = self.others(state).find(|o| o.keys.contains(keys)) {
                return Err(format!(
                    "{keys} already belongs to the hotkey '{}'.",
                    other.name
                ));
            }
        }
        let taken = |name: &str| {
            self.others(state)
                .find(|o| o.name.eq_ignore_ascii_case(name))
                .map(|o| o.name.clone())
        };
        if h.name.is_empty() {
            h.name = free_name(&self.suggested_name(state), |n| taken(n).is_some());
        } else if let Some(other) = taken(&h.name) {
            return Err(format!("There is already a hotkey called '{other}'."));
        }
        Ok(())
    }
}

/// `wanted`, cut to a hotkey name's length, and numbered when `taken`.
fn free_name(wanted: &str, taken: impl Fn(&str) -> bool) -> String {
    let cut = |text: &str, max: usize| -> String { text.chars().take(max).collect() };
    let first = cut(wanted, HOTKEY_NAME_MAX);
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|n| {
            let tail = format!(" {n}");
            format!(
                "{}{tail}",
                cut(wanted, HOTKEY_NAME_MAX - tail.chars().count())
            )
        })
        .find(|name| !taken(name))
        .unwrap_or(first)
}

/// `h` as pretty JSON, its fields in the order the API lists them.
fn to_json_text(h: &Hotkey) -> String {
    serde_json::to_string_pretty(h).unwrap_or_default()
}

/// A section's heading.
fn heading(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).strong());
}

/// A list of steps in words, each with buttons to move it and take it out.
fn steps_list(ui: &mut Ui, state: &FullState, steps: &mut Vec<HotkeyStep>, salt: &str) {
    let mut up = None;
    let mut remove = None;
    let count = steps.len();
    for (i, step) in steps.iter().enumerate() {
        egui::Frame::new()
            .fill(theme::p().section_fill)
            .corner_radius(5)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{}", i + 1))
                            .strong()
                            .color(theme::p().text_dim),
                    );
                    // The words take what the buttons leave.
                    let words = (ui.available_width() - 110.0).max(60.0);
                    ui.allocate_ui(vec2(words, 20.0), |ui| {
                        ui.set_width(words);
                        ui.add(egui::Label::new(describe_step(step, &state.mixer)).truncate());
                    });
                    ui.push_id((salt, i), |ui| {
                        if ui
                            .add_enabled(i > 0, egui::Button::new("Up").small())
                            .on_hover_text("Do it earlier")
                            .clicked()
                        {
                            up = Some(i);
                        }
                        if ui
                            .add_enabled(i + 1 < count, egui::Button::new("Down").small())
                            .on_hover_text("Do it later")
                            .clicked()
                        {
                            up = Some(i + 1);
                        }
                        if ui
                            .small_button("×")
                            .on_hover_text("Take this step out")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                });
            });
    }
    if let Some(i) = up {
        steps.swap(i - 1, i);
    }
    if let Some(i) = remove {
        steps.remove(i);
    }
}

/// The simple form's choices for `s`. `whole`: for a whole hotkey, with
/// what holding and letting go do; otherwise for one step of one.
fn simple_form(ui: &mut Ui, state: &FullState, s: &mut Simple, whole: bool, salt: &str) {
    let mixer = &state.mixer;
    let target_name = |t: StripOrBus| match t {
        StripOrBus::Strip(id) => mixer
            .strip(id)
            .map_or_else(|| "a removed strip".into(), |x| x.name.clone()),
        StripOrBus::Bus(id) => mixer.bus(id).map_or_else(
            || "a removed bus".into(),
            |b| format!("{} {}", mixer.bus_label(b.id).unwrap_or_default(), b.name),
        ),
    };
    let bus_name = |id: BusId| target_name(StripOrBus::Bus(id));
    egui::Grid::new(("hotkey_form", salt))
        .num_columns(2)
        .spacing([12.0, 10.0])
        .show(ui, |ui| {
            ui.label("Do this");
            egui::ComboBox::from_id_salt((salt, "action"))
                .selected_text(s.action.label())
                .width(320.0)
                .show_ui(ui, |ui| {
                    ui.label(
                        RichText::new("A strip or bus")
                            .small()
                            .color(theme::p().text_dim),
                    );
                    for a in Action::ON_ONE {
                        if whole || a != Action::PushToTalk {
                            ui.selectable_value(&mut s.action, a, a.label());
                        }
                    }
                    ui.separator();
                    ui.label(
                        RichText::new("The whole mixer")
                            .small()
                            .color(theme::p().text_dim),
                    );
                    for a in Action::ON_ALL {
                        ui.selectable_value(&mut s.action, a, a.label());
                    }
                });
            ui.end_row();

            if s.action.has_target() {
                ui.label("On");
                egui::ComboBox::from_id_salt((salt, "target"))
                    .selected_text(target_name(s.target))
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        ui.label(RichText::new("Strips").small().color(theme::p().text_dim));
                        for x in &mixer.strips {
                            ui.selectable_value(&mut s.target, StripOrBus::Strip(x.id), &x.name);
                        }
                        if !s.action.strips_only() {
                            ui.separator();
                            ui.label(RichText::new("Buses").small().color(theme::p().text_dim));
                            for b in &mixer.buses {
                                ui.selectable_value(
                                    &mut s.target,
                                    StripOrBus::Bus(b.id),
                                    bus_name(b.id),
                                );
                            }
                        }
                    });
                ui.end_row();
            }

            match s.action {
                Action::Effect => {
                    ui.label("Effect");
                    egui::ComboBox::from_id_salt((salt, "fx"))
                        .selected_text(s.fx.label())
                        .width(320.0)
                        .show_ui(ui, |ui| {
                            for f in Fx::for_target(s.target) {
                                ui.selectable_value(&mut s.fx, *f, f.label());
                            }
                        });
                    ui.end_row();
                }
                Action::Route => {
                    ui.label("To bus");
                    egui::ComboBox::from_id_salt((salt, "bus"))
                        .selected_text(bus_name(s.bus))
                        .width(320.0)
                        .show_ui(ui, |ui| {
                            for b in &mixer.buses {
                                ui.selectable_value(&mut s.bus, b.id, bus_name(b.id));
                            }
                        });
                    ui.end_row();
                }
                Action::VolumeBy | Action::VolumeTo if matches!(s.target, StripOrBus::Strip(_)) => {
                    ui.label("Fader");
                    let shown = match s.send {
                        None => "Its own fader".to_string(),
                        Some(b) => format!("Its level in {}", bus_name(b)),
                    };
                    egui::ComboBox::from_id_salt((salt, "fader"))
                        .selected_text(shown)
                        .width(320.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut s.send, None, "Its own fader");
                            for b in &mixer.buses {
                                ui.selectable_value(
                                    &mut s.send,
                                    Some(b.id),
                                    format!("Its level in {}", bus_name(b.id)),
                                );
                            }
                        });
                    ui.end_row();
                }
                _ => {}
            }

            if s.action.has_mode() {
                ui.label("How");
                ui.horizontal(|ui| {
                    for m in Mode::ALL {
                        if whole || m != Mode::WhileHeld {
                            ui.selectable_value(&mut s.mode, m, m.label(s.action));
                        }
                    }
                });
                ui.end_row();
            }

            match s.action {
                Action::VolumeBy => {
                    ui.label("Which way");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut s.up, true, "Up");
                        ui.selectable_value(&mut s.up, false, "Down");
                        ui.label("by");
                        ui.add(
                            egui::DragValue::new(&mut s.amount_db)
                                .range(0.1..=24.0)
                                .clamp_existing_to_range(false)
                                .speed(0.1)
                                .fixed_decimals(1)
                                .suffix(" dB"),
                        );
                    });
                    ui.end_row();
                    if whole {
                        ui.label("");
                        ui.checkbox(&mut s.repeat, "Keep going while the keys are held");
                        ui.end_row();
                    }
                }
                Action::VolumeTo => {
                    ui.label("To");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut s.level_db)
                                .range(GAIN_MIN_DB..=GAIN_MAX_DB)
                                .clamp_existing_to_range(false)
                                .speed(0.1)
                                .fixed_decimals(1)
                                .suffix(" dB"),
                        );
                        ui.label(
                            RichText::new("0 dB is full level, -60 is silent")
                                .size(12.0)
                                .color(theme::p().text_dim),
                        );
                    });
                    ui.end_row();
                    if whole {
                        ui.label("");
                        ui.checkbox(&mut s.put_back, "Put it back when I let go");
                        ui.end_row();
                    }
                }
                Action::PushToTalk => {
                    ui.label("When I let go");
                    ui.vertical(|ui| {
                        ui.radio_value(&mut s.mute_after, true, "Mute it");
                        ui.radio_value(&mut s.mute_after, false, "Put the mute back how it was");
                    });
                    ui.end_row();
                }
                Action::Preset => {
                    ui.label("Preset");
                    egui::ComboBox::from_id_salt((salt, "preset"))
                        .selected_text(s.preset.as_str())
                        .width(320.0)
                        .show_ui(ui, |ui| {
                            for p in &state.eq_presets {
                                ui.selectable_value(&mut s.preset, p.name.clone(), &p.name);
                            }
                        });
                    ui.end_row();
                }
                Action::Scene => {
                    ui.label("Scene");
                    if state.library.scenes.is_empty() {
                        ui.label(
                            RichText::new("No scenes saved yet: save one from the Scenes menu.")
                                .color(theme::p().warning),
                        );
                    } else {
                        egui::ComboBox::from_id_salt((salt, "scene"))
                            .selected_text(s.scene.as_str())
                            .width(320.0)
                            .show_ui(ui, |ui| {
                                for name in &state.library.scenes {
                                    ui.selectable_value(&mut s.scene, name.clone(), name);
                                }
                            });
                    }
                    ui.end_row();
                }
                _ => {}
            }
        });
    if whole && s.action == Action::PushToTalk {
        let name = target_name(s.target);
        let note = if s.mute_after {
            format!("{name} is unmuted while you hold the keys, and muted when you let go.")
        } else {
            format!("{name} is unmuted while you hold the keys, then muted or not as before.")
        };
        ui.label(RichText::new(note).size(12.0).color(theme::p().text_dim));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_made_up_are_short_and_free() {
        let taken = |n: &str| n == "Mic: mute" || n == "Mic: mute 2";
        assert_eq!(free_name("Mic: unmute", taken), "Mic: unmute");
        assert_eq!(free_name("Mic: mute", taken), "Mic: mute 3");
        let long = "x".repeat(80);
        let name = free_name(&long, |n| {
            n.chars().count() == HOTKEY_NAME_MAX && !n.ends_with('2')
        });
        assert_eq!(name.chars().count(), HOTKEY_NAME_MAX);
        assert!(name.ends_with(" 2"));
    }

    #[test]
    fn the_json_reads_back_with_the_name_first() {
        let st = FullState::default();
        let mut h = Simple::new(StripOrBus::Strip(1), &st).hotkey("Mute mic", &["F9".into()]);
        h.id = 7;
        let text = to_json_text(&h);
        assert!(text.find("\"name\"") < text.find("\"keys\""), "{text}");
        let back: Hotkey = serde_json::from_str(&text).unwrap();
        assert_eq!(back, h);
    }
}
