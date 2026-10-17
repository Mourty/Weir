//! Applications: the Apps menu, which moves a playing application to
//! another strip, and the App rules window, which says where applications
//! go as soon as they start playing, and lists those heard before so rules
//! can be made for ones that play only now and then.

use super::App;
use crate::theme;
use crate::transfer::Pane;
use egui::{RichText, Ui};
use weir_protocol::*;

/// Whether a rule's strip, `rule`, is the strip called `name`.
fn same_name(rule: Option<&str>, name: &str) -> bool {
    rule.is_some_and(|r| r.trim().eq_ignore_ascii_case(name))
}

/// The strips applications can play into.
fn virtual_strips(state: &FullState) -> Vec<&Strip> {
    state
        .mixer
        .strips
        .iter()
        .filter(|s| s.kind == StripKind::Virtual)
        .collect()
}

impl App {
    /// The Apps menu: every application playing, where it plays, and where
    /// it can be moved to, now or always.
    pub(super) fn apps_menu(&mut self, ui: &mut Ui, state: &FullState) {
        if ui
            .button("Rules…")
            .on_hover_text("Where applications go when they start playing")
            .clicked()
        {
            self.open_rules();
            ui.close();
        }
        ui.separator();
        if state.apps.is_empty() {
            ui.label(RichText::new("no application is playing audio").color(theme::p().text_dim));
        }
        let strips = virtual_strips(state);
        for a in &state.apps {
            let where_ = where_playing(state, a);
            ui.menu_button(format!("{}  {}", a.name, where_), |ui| {
                ui.label(RichText::new("Move to").color(theme::p().text_dim));
                for s in &strips {
                    if ui.button(&s.name).clicked() {
                        self.actions.push(Request::MoveApp(MoveAppParams {
                            app: a.id,
                            strip: s.id,
                        }));
                        ui.close();
                    }
                }
                ui.separator();
                ui.menu_button(format!("Always play {} into", a.name), |ui| {
                    let current = rule_for(&state.app_rules, a).and_then(|r| r.strip.as_deref());
                    for s in &strips {
                        if ui
                            .selectable_label(same_name(current, &s.name), &s.name)
                            .clicked()
                        {
                            self.set_rule(state, &a.name, Some(s.name.clone()));
                            ui.close();
                        }
                    }
                });
            });
        }
    }

    /// Add or change the rule for `app`, keeping the others.
    fn set_rule(&mut self, state: &FullState, app: &str, strip: Option<String>) {
        let mut rules = state.app_rules.clone();
        match rules.iter_mut().find(|r| r.app.eq_ignore_ascii_case(app)) {
            Some(r) => r.strip = strip,
            None => rules.push(AppRule {
                app: app.to_string(),
                strip,
            }),
        }
        self.actions
            .push(Request::SetAppRules(AppRulesParams { rules }));
    }

    /// Open the App rules window, or bring it to the front.
    pub(super) fn open_rules(&mut self) {
        if !self.show_rules {
            self.clear_seen_asked = false;
        }
        self.show_rules = true;
        self.raise_rules = true;
    }

    /// The App rules window: the rules, the applications playing now and
    /// those heard before, each with a button to make it a rule. The whole
    /// list of rules is edited here and sent back in one request whenever
    /// it changes.
    pub(super) fn rules_window(&mut self, ctx: &egui::Context, state: &FullState) {
        if !self.show_rules {
            return;
        }
        let mut rules = state.app_rules.clone();
        let mut changed = false;
        let (mut raise, mut closed) = (self.raise_rules, false);
        crate::transfer::window(
            ctx,
            "app-rules",
            "App rules",
            &mut raise,
            &mut closed,
            |ui, pane| match pane {
                Pane::Body => {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| changed |= self.rules_body(ui, state, &mut rules));
                }
                Pane::Footer => self.rules_footer(ui, state),
            },
        );
        if changed {
            self.actions
                .push(Request::SetAppRules(AppRulesParams { rules }));
        }
        self.raise_rules = raise;
        if closed {
            self.show_rules = false;
            self.clear_seen_asked = false;
        }
    }

    /// Everything but the footer. Returns whether the rules changed.
    fn rules_body(&mut self, ui: &mut Ui, state: &FullState, rules: &mut Vec<AppRule>) -> bool {
        let mut changed = false;
        ui.label(
            RichText::new(
                "A rule puts an application on the same strip every time it starts playing. \
                 Move one by hand afterwards and it stays where you put it.",
            )
            .color(theme::p().text_dim),
        );
        ui.add_space(10.0);
        ui.label(RichText::new("Rules").strong().size(15.0));
        ui.add_space(2.0);
        card(ui, |ui| {
            if rules.is_empty() {
                ui.label(
                    RichText::new(
                        "No rules yet. Make one for an application below, playing now or \
                         heard before, or by its name.",
                    )
                    .color(theme::p().text_dim),
                );
            } else {
                changed |= rule_table(ui, state, rules);
            }
        });
        ui.add_space(4.0);
        changed |= self.rule_by_name(ui, state, rules);
        ui.add_space(14.0);

        // Side by side when there is room, one above the other when not.
        let gap = 16.0;
        let wide = ui.available_width() >= 640.0;
        if wide {
            let w = ui.available_width() - gap;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.allocate_ui_with_layout(
                    egui::vec2(w * 0.42, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| changed |= playing_list(ui, state, rules),
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(w * 0.58, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| changed |= self.seen_list(ui, state, rules),
                );
            });
        } else {
            changed |= playing_list(ui, state, rules);
            ui.add_space(14.0);
            changed |= self.seen_list(ui, state, rules);
        }
        changed
    }

    /// A field and a button to make a rule for an application by its
    /// name, for one that has not played yet. Returns whether one was made.
    fn rule_by_name(&mut self, ui: &mut Ui, state: &FullState, rules: &mut Vec<AppRule>) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.rule_name)
                    .hint_text("application or program name")
                    .desired_width(220.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let name = self.rule_name.trim().to_string();
            let valid =
                !name.is_empty() && !rules.iter().any(|x| x.app.eq_ignore_ascii_case(&name));
            let clicked = ui
                .add_enabled(valid, egui::Button::new("+ Rule by name"))
                .on_hover_text(
                    "For an application Weir has not heard yet. Use the name the Apps \
                     menu shows, or the name of its program, such as firefox.",
                )
                .clicked();
            if (clicked || enter) && valid {
                rules.push(AppRule {
                    app: name,
                    strip: first_strip(state),
                });
                self.rule_name.clear();
                changed = true;
            }
        });
        changed
    }

    /// The applications heard before and not playing now, newest first,
    /// with a box to find one. Returns whether a rule was made.
    fn seen_list(&mut self, ui: &mut Ui, state: &FullState, rules: &mut Vec<AppRule>) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Seen before").strong().size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.seen_filter)
                        .hint_text("Find an app")
                        .desired_width(150.0),
                );
            });
        });
        ui.add_space(2.0);
        let find = self.seen_filter.trim().to_lowercase();
        let seen: Vec<&SeenApp> = state
            .seen_apps
            .iter()
            .filter(|s| {
                !state
                    .apps
                    .iter()
                    .any(|a| a.name.eq_ignore_ascii_case(&s.name))
            })
            .collect();
        let shown: Vec<&SeenApp> = seen
            .iter()
            .copied()
            .filter(|s| {
                find.is_empty()
                    || s.name.to_lowercase().contains(&find)
                    || s.binary
                        .as_deref()
                        .is_some_and(|b| b.to_lowercase().contains(&find))
            })
            .collect();
        let now = now_ms();
        let mut forget = None;
        card(ui, |ui| {
            if seen.is_empty() {
                ui.label(
                    RichText::new(
                        "Applications show here once they have played and stopped, so \
                         you can make rules for ones that play only now and then.",
                    )
                    .color(theme::p().text_dim),
                );
            } else if shown.is_empty() {
                ui.label(RichText::new("No application matches.").color(theme::p().text_dim));
            }
            for s in &shown {
                let when = heard_ago(now, s.last_seen_ms);
                let detail = match &s.strip {
                    Some(strip) => format!("{when} · last in {strip}"),
                    None => when,
                };
                let rule = rules
                    .iter()
                    .find(|r| r.matches_app(&s.name, s.binary.as_deref()));
                let clicked = app_row(ui, false, &s.name, s.binary.as_deref(), &detail, rule, true);
                match clicked {
                    RowClick::Rule => {
                        // Where it played last, when this setup has it.
                        let strip = s
                            .strip
                            .as_deref()
                            .and_then(|n| state.mixer.find_strip(n))
                            .filter(|x| x.kind == StripKind::Virtual)
                            .map(|x| x.name.clone())
                            .or_else(|| first_strip(state));
                        rules.push(AppRule {
                            app: s.name.clone(),
                            strip,
                        });
                        changed = true;
                    }
                    RowClick::Forget => forget = Some(s.name.clone()),
                    RowClick::Nothing => {}
                }
            }
        });
        if let Some(name) = forget {
            self.actions
                .push(Request::ForgetSeenApp(NameParams { name }));
        }
        changed
    }

    /// The footer: what the list of applications heard is, and a button
    /// to empty it, which asks first.
    fn rules_footer(&mut self, ui: &mut Ui, state: &FullState) {
        ui.horizontal_wrapped(|ui| {
            if self.clear_seen_asked {
                ui.label("Forget every application heard before? Rules stay as they are.");
                if ui.button("Forget them").clicked() {
                    self.actions.push(Request::ClearSeenApps);
                    self.clear_seen_asked = false;
                }
                if ui.button("Keep them").clicked() {
                    self.clear_seen_asked = false;
                }
                return;
            }
            ui.label(
                RichText::new(format!(
                    "Applications heard in the last {SEEN_APPS_DAYS} days are kept on this \
                     computer only."
                ))
                .color(theme::p().text_dim),
            );
            // Applications playing now stay on the list, so there is
            // nothing to clear without others.
            let any = state.seen_apps.iter().any(|s| {
                !state
                    .apps
                    .iter()
                    .any(|a| a.name.eq_ignore_ascii_case(&s.name))
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.show_rules = false;
                }
                if ui
                    .add_enabled(any, egui::Button::new("Clear history…"))
                    .on_hover_text("Forget every application heard before")
                    .clicked()
                {
                    self.clear_seen_asked = true;
                }
            });
        });
    }
}

/// The first virtual strip, by name: where a new rule puts an application
/// until another is picked.
fn first_strip(state: &FullState) -> Option<String> {
    virtual_strips(state).first().map(|s| s.name.clone())
}

/// A box around part of the window.
fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::p().section_fill)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// What was clicked in an application's row.
#[derive(PartialEq, Eq)]
enum RowClick {
    Nothing,
    Rule,
    Forget,
}

/// One application: a dot (filled while it plays), its name and `detail`
/// below, "has a rule" or a button to make one, and, when `forgettable`,
/// a button to forget it.
fn app_row(
    ui: &mut Ui,
    playing: bool,
    name: &str,
    binary: Option<&str>,
    detail: &str,
    rule: Option<&AppRule>,
    forgettable: bool,
) -> RowClick {
    let mut click = RowClick::Nothing;
    ui.horizontal(|ui| {
        ui.set_min_height(34.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if forgettable
                && ui
                    .small_button("×")
                    .on_hover_text(format!("Forget {name}"))
                    .clicked()
            {
                click = RowClick::Forget;
            }
            match rule {
                Some(r) => {
                    let to = r.strip.as_deref().unwrap_or("where it goes by itself");
                    ui.label(RichText::new("has a rule").color(theme::p().text_dim))
                        .on_hover_text(format!("It plays into {to}"));
                }
                None => {
                    if ui
                        .button("+ Rule")
                        .on_hover_text(format!("Always play {name} into the same strip"))
                        .clicked()
                    {
                        click = RowClick::Rule;
                    }
                }
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                let p = theme::p();
                if playing {
                    ui.painter()
                        .circle_filled(rect.center(), 3.5, p.meter_green);
                } else {
                    ui.painter().circle_stroke(
                        rect.center(),
                        3.0,
                        egui::Stroke::new(1.0_f32, p.text_dim),
                    );
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    let r = ui.add(egui::Label::new(name).truncate());
                    match binary.filter(|b| !b.eq_ignore_ascii_case(name)) {
                        Some(b) => r.on_hover_text(format!("{name}, played by the program {b}")),
                        None => r.on_hover_text(name),
                    };
                    ui.add(
                        egui::Label::new(RichText::new(detail).size(11.0).color(p.text_dim))
                            .truncate(),
                    );
                });
            });
        });
    });
    click
}

/// The applications playing now, one row each however many streams they
/// play. Returns whether a rule was made.
fn playing_list(ui: &mut Ui, state: &FullState, rules: &mut Vec<AppRule>) -> bool {
    let mut changed = false;
    // As tall as the other list's heading, which holds a search box.
    ui.horizontal(|ui| {
        ui.set_min_height(ui.spacing().interact_size.y);
        ui.label(RichText::new("Playing now").strong().size(15.0));
    });
    ui.add_space(2.0);
    let mut apps: Vec<&AppStream> = Vec::new();
    for a in &state.apps {
        if !apps.iter().any(|x| x.name.eq_ignore_ascii_case(&a.name)) {
            apps.push(a);
        }
    }
    card(ui, |ui| {
        if apps.is_empty() {
            ui.label(RichText::new("No application is playing now.").color(theme::p().text_dim));
        }
        for a in apps {
            let rule = rule_for(rules, a);
            let detail = where_playing(state, a);
            if app_row(ui, true, &a.name, a.binary.as_deref(), &detail, rule, false)
                == RowClick::Rule
            {
                let on = a.strip.and_then(|id| state.mixer.strip(id));
                rules.push(AppRule {
                    app: a.name.clone(),
                    strip: on.map(|s| s.name.clone()).or_else(|| first_strip(state)),
                });
                changed = true;
            }
        }
    });
    changed
}

/// Where an application plays, in words: "in Music".
fn where_playing(state: &FullState, a: &AppStream) -> String {
    match a.strip.and_then(|id| state.mixer.strip(id)) {
        // Worded, not an arrow: the fonts do not all have one.
        Some(s) => format!("in {}", s.name),
        None => match &a.target {
            Some(t) => match crate::effects::return_device(state, t) {
                Some(device) => format!("in {device}"),
                // A device's own name rather than PipeWire's for it.
                None => match state.devices.iter().find(|d| &d.name == t) {
                    Some(d) => format!("in {}", d.description),
                    None => format!("in {t}"),
                },
            },
            None => "(not playing anywhere)".into(),
        },
    }
}

/// Now, in milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The rules, one row each: the application, the strip it plays into, and
/// a button to remove it. Returns whether anything changed.
fn rule_table(ui: &mut Ui, state: &FullState, rules: &mut Vec<AppRule>) -> bool {
    let strips = virtual_strips(state);
    let mut changed = false;
    let mut remove = None;
    egui::Grid::new("rules")
        .num_columns(3)
        .spacing([10.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            if !rules.is_empty() {
                let heading = |text| RichText::new(text).size(11.0).color(theme::p().text_dim);
                ui.label(heading("App"));
                ui.label(heading("Plays into"));
                ui.label("");
                ui.end_row();
            }
            for (k, r) in rules.iter_mut().enumerate() {
                let playing = state.apps.iter().any(|a| r.matches(a));
                let color = if playing {
                    theme::p().text
                } else {
                    theme::p().text_dim
                };
                ui.label(RichText::new(&r.app).color(color))
                    .on_hover_text(if playing {
                        "Playing now"
                    } else {
                        "Not playing now"
                    });
                // A strip this setup lacks: the rule waits for one that has
                // it.
                let here = r.strip.as_deref().and_then(|n| state.mixer.find_strip(n));
                let text = match (&r.strip, here) {
                    (Some(_), Some(s)) => s.name.clone(),
                    (Some(name), None) => format!("{name} (not in this setup)"),
                    (None, _) => "leave alone".to_string(),
                };
                egui::ComboBox::from_id_salt(("rule", k))
                    .selected_text(text)
                    .truncate()
                    .show_ui(ui, |ui| {
                        for s in &strips {
                            if ui
                                .selectable_label(same_name(r.strip.as_deref(), &s.name), &s.name)
                                .clicked()
                            {
                                r.strip = Some(s.name.clone());
                                changed = true;
                            }
                        }
                        if ui
                            .selectable_label(r.strip.is_none(), "leave alone")
                            .on_hover_text("Let it play wherever it goes by itself")
                            .clicked()
                        {
                            r.strip = None;
                            changed = true;
                        }
                    });
                if ui
                    .small_button("×")
                    .on_hover_text("Remove this rule")
                    .clicked()
                {
                    remove = Some(k);
                }
                ui.end_row();
            }
        });
    if let Some(k) = remove {
        rules.remove(k);
        changed = true;
    }
    changed
}
