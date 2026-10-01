//! Applications: the Apps menu, which moves a playing application to
//! another strip, and the App rules window, which says where applications
//! go as soon as they start playing.

use super::App;
use crate::theme;
use egui::{RichText, Ui};
use weir_protocol::*;

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
            self.show_rules = true;
            ui.close();
        }
        ui.separator();
        if state.apps.is_empty() {
            ui.label(RichText::new("no application is playing audio").color(theme::p().text_dim));
        }
        let strips = virtual_strips(state);
        for a in &state.apps {
            let where_ = match a.strip.and_then(|id| state.mixer.strip(id)) {
                // Worded, not an arrow: the fonts do not all have one.
                Some(s) => format!("in {}", s.name),
                None => match &a.target {
                    Some(t) => match crate::effects::return_device(state, t) {
                        Some(device) => format!("in {device}"),
                        None => format!("in {t}"),
                    },
                    None => "(not playing anywhere)".into(),
                },
            };
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
                    let current = rule_for(&state.app_rules, a).and_then(|r| r.strip);
                    for s in &strips {
                        if ui
                            .selectable_label(current == Some(s.id), &s.name)
                            .clicked()
                        {
                            self.set_rule(state, &a.name, Some(s.id));
                            ui.close();
                        }
                    }
                });
            });
        }
    }

    /// Add or change the rule for `app`, keeping the others.
    fn set_rule(&mut self, state: &FullState, app: &str, strip: Option<StripId>) {
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

    /// The App rules window. The whole list is edited here and sent back
    /// in one request whenever it changes.
    pub(super) fn rules_window(&mut self, ctx: &egui::Context, state: &FullState) {
        if !self.show_rules {
            return;
        }
        let mut open = true;
        let mut rules = state.app_rules.clone();
        let mut changed = false;
        egui::Window::new("App rules")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Applications matching a rule are moved there as soon as they start \
                         playing. Move one by hand afterwards and it stays where you put it.",
                    )
                    .size(11.0)
                    .color(theme::p().text_dim),
                );
                ui.add_space(6.0);
                if rules.is_empty() {
                    ui.label(RichText::new("No rules yet.").color(theme::p().text_dim));
                }
                changed |= rule_table(ui, state, &mut rules);
                ui.add_space(8.0);
                changed |= self.new_rule_row(ui, state, &mut rules);
            });
        if changed {
            self.actions
                .push(Request::SetAppRules(AppRulesParams { rules }));
        }
        self.show_rules = open;
    }

    /// Buttons to add a rule, for an application playing now or by name.
    /// Returns whether one was added.
    fn new_rule_row(&mut self, ui: &mut Ui, state: &FullState, rules: &mut Vec<AppRule>) -> bool {
        let mut changed = false;
        let first = virtual_strips(state).first().map(|s| s.id);
        ui.horizontal(|ui| {
            let unruled: Vec<&AppStream> = state
                .apps
                .iter()
                .filter(|a| rule_for(rules, a).is_none())
                .collect();
            ui.add_enabled_ui(!unruled.is_empty(), |ui| {
                ui.menu_button("+ Rule for a playing app", |ui| {
                    for a in &unruled {
                        if ui.button(&a.name).clicked() {
                            rules.push(AppRule {
                                app: a.name.clone(),
                                strip: a.strip.or(first),
                            });
                            changed = true;
                            ui.close();
                        }
                    }
                });
            });
        });
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.rule_name)
                    .hint_text("application or program name")
                    .desired_width(200.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let name = self.rule_name.trim().to_string();
            let valid =
                !name.is_empty() && !rules.iter().any(|x| x.app.eq_ignore_ascii_case(&name));
            let clicked = ui
                .add_enabled(valid, egui::Button::new("+ Rule by name"))
                .on_hover_text(
                    "For an application that is not playing now. Use the name the Apps \
                     menu shows, or the name of its program, such as firefox.",
                )
                .clicked();
            if (clicked || enter) && valid {
                rules.push(AppRule {
                    app: name,
                    strip: first,
                });
                self.rule_name.clear();
                changed = true;
            }
        });
        changed
    }
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
                let text = match r.strip.and_then(|id| state.mixer.strip(id)) {
                    Some(s) => s.name.clone(),
                    None if r.strip.is_some() => "(removed strip)".to_string(),
                    None => "leave alone".to_string(),
                };
                egui::ComboBox::from_id_salt(("rule", k))
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        for s in &strips {
                            if ui
                                .selectable_label(r.strip == Some(s.id), &s.name)
                                .clicked()
                            {
                                r.strip = Some(s.id);
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
