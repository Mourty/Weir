//! Hotkeys as the controller keeps them: checking them before they are
//! saved, saving them, noticing what is wrong with them, and passing
//! presses on to the runner, which does the work (see [`crate::hotkeys`]).

use super::{names, Controller, Subscriptions};
use crate::config::{self, HotkeyList};
use crate::history::Step;
use crate::hotkeys::{Command, Runner};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};
use weir_protocol::*;

/// What a client asks of a hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Its keys went down.
    Press,
    /// Its keys came up.
    Release,
    /// A tap: down and up at once.
    Run,
}

/// Whether a request may be a hotkey's step. Requests about the
/// connection itself mean nothing outside one, ones that only ask
/// something are pointless, and hotkeys pressing hotkeys could go round
/// for ever.
fn allowed_in_hotkey(req: &Request) -> bool {
    !matches!(
        req,
        Request::Hello
            | Request::Ping
            | Request::Describe
            | Request::GetState
            | Request::ListDevices
            | Request::ListApps
            | Request::ListSetups
            | Request::ListScenes
            | Request::ListEqPresets
            | Request::History
            | Request::Subscribe(_)
            | Request::Unsubscribe(_)
            | Request::WatchSpectrum(_)
            | Request::ListHotkeys
            | Request::SetHotkey(_)
            | Request::RemoveHotkey(_)
            | Request::SwitchHotkey(_)
            | Request::MoveHotkey(_)
            | Request::AddHotkeyGroup(_)
            | Request::SetHotkeyGroup(_)
            | Request::RemoveHotkeyGroup(_)
            | Request::MoveHotkeyGroup(_)
            | Request::PressHotkey(_)
            | Request::ReleaseHotkey(_)
            | Request::RunHotkey(_)
            | Request::OpenShortcutSettings
    )
}

/// The request `step` makes.
fn parse_step(step: &HotkeyStep) -> Result<Request, RpcError> {
    let envelope = RpcRequest {
        jsonrpc: "2.0".into(),
        id: None,
        method: step.method.clone(),
        params: (!step.params.is_null()).then(|| step.params.clone()),
    };
    envelope.parse()
}

/// Whether `step` changes something a fade can go through gradually.
fn fadeable(step: &HotkeyStep) -> bool {
    match step.method.as_str() {
        "set_strip" | "set_bus" => step.params.get("gain_db").is_some(),
        "set_route" => step.params.get("level_db").is_some(),
        _ => false,
    }
}

impl Controller {
    /// Hand the controller the runner, so presses from clients reach it.
    pub fn set_hotkey_runner(&self, runner: Arc<Mutex<Runner>>) {
        *self.hotkey_runner.lock().unwrap() = Some(runner);
    }

    /// Have the runner act on `command`, if hotkeys run in this daemon.
    /// Never called with the controller's own state locked: the runner
    /// changes the mixer.
    fn tell_runner(&self, command: Command) -> bool {
        let runner = self.hotkey_runner.lock().unwrap().clone();
        match runner {
            Some(runner) => {
                runner.lock().unwrap().handle(command);
                true
            }
            None => false,
        }
    }

    /// Every hotkey and group now.
    fn hotkey_list(&self) -> HotkeyList {
        self.inner
            .lock()
            .unwrap()
            .hotkeys
            .clone()
            .unwrap_or_default()
    }

    /// Every hotkey now.
    pub fn hotkeys(&self) -> Vec<Hotkey> {
        self.hotkey_list().hotkeys
    }

    /// Every group of hotkeys now.
    pub fn hotkey_groups(&self) -> Vec<HotkeyGroup> {
        self.hotkey_list().groups
    }

    /// The hotkey with `id`, if there is one.
    pub fn hotkey(&self, id: HotkeyId) -> Option<Hotkey> {
        self.hotkeys().into_iter().find(|h| h.id == id)
    }

    /// Every hotkey, how keys reach Weir, and what is wrong.
    pub fn hotkeys_info(&self) -> HotkeysInfo {
        let inner = self.inner.lock().unwrap();
        let HotkeyList { hotkeys, groups } = inner.hotkeys.clone().unwrap_or_default();
        let mut problems: Vec<HotkeyProblem> = Vec::new();
        if let Err(e) = &inner.hotkeys {
            problems.push(HotkeyProblem {
                hotkey: 0,
                problem: format!("Hotkeys could not be read, so they are not working: {e}"),
            });
        }
        for h in &hotkeys {
            if let Some(p) = inner.key_problems.get(&h.id) {
                problems.push(HotkeyProblem {
                    hotkey: h.id,
                    problem: p.clone(),
                });
            }
        }
        problems.extend(target_problems(&hotkeys, &inner.mixer));
        HotkeysInfo {
            hotkeys,
            groups,
            keys: inner.keys_status.clone(),
            problems,
        }
    }

    /// The runner reports how keys reach Weir, and which hotkeys' keys do
    /// not work and why.
    pub fn set_keys_status(&self, status: KeysStatus, problems: BTreeMap<HotkeyId, String>) {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.keys_status == status && inner.key_problems == problems {
                return;
            }
            inner.keys_status = status;
            inner.key_problems = problems;
        }
        self.announce(Notification::HotkeysChanged(self.hotkeys_info()));
    }

    /// Take `keys`, which the desktop has for hotkey `id`, as its keys:
    /// where Weir can set the desktop's keys (KDE Plasma), keys changed in
    /// its settings come back, so that `keys` says what works and the next
    /// change made in Weir starts from them.
    pub fn adopt_desktop_keys(&self, id: HotkeyId, keys: Vec<String>) {
        let changed = self.edit_hotkeys(|list| {
            let h = list.hotkey_mut(id)?;
            let changed = h.keys != keys;
            h.keys = keys;
            Ok(changed.then(|| h.name.clone()))
        });
        match changed {
            Ok(Some(name)) => info!("the desktop's settings changed the keys of '{name}'"),
            Ok(None) => {}
            Err(e) => warn!("could not take the keys the desktop gave: {}", e.message),
        }
    }

    /// Have the desktop open its shortcut settings at Weir's hotkeys, when
    /// it can.
    pub(super) fn open_shortcut_settings(&self) -> Result<Value, RpcError> {
        let status = self.inner.lock().unwrap().keys_status.clone();
        if status.method == KeysMethod::Desktop && status.configurable {
            self.shortcut_settings.notify_one();
            return Ok(Value::Null);
        }
        Err(RpcError::application(
            "your desktop cannot open its shortcut settings from Weir; open them yourself, \
             such as System Settings, Keyboard, Shortcuts on KDE Plasma",
        ))
    }

    /// Wait until a client asks for the desktop's shortcut settings.
    pub async fn shortcut_settings_wanted(&self) {
        self.shortcut_settings.notified().await;
    }

    /// Tell clients when the mixer changing made a hotkey point at a strip
    /// or bus that is gone, or back. Called with every new mixer state.
    pub(super) fn check_hotkey_targets(&self, mixer: &MixerState) {
        let changed = {
            let mut inner = self.inner.lock().unwrap();
            let hotkeys = inner.hotkeys.clone().unwrap_or_default().hotkeys;
            let now = target_problems(&hotkeys, mixer);
            let changed = now != inner.target_problems;
            inner.target_problems = now;
            changed
        };
        if changed {
            self.announce(Notification::HotkeysChanged(self.hotkeys_info()));
        }
    }

    /// Check `h`, then add it or replace the one with its id, save, and
    /// tell the runner and clients.
    pub(super) fn set_hotkey(&self, mut h: Hotkey) -> Result<Value, RpcError> {
        let mixer = self.mixer();
        h.name = h.name.trim().to_string();
        if h.name.is_empty() {
            return Err(RpcError::invalid_params("a hotkey needs a name"));
        }
        if h.name.chars().count() > HOTKEY_NAME_MAX {
            return Err(RpcError::invalid_params(format!(
                "a hotkey's name can be at most {HOTKEY_NAME_MAX} characters"
            )));
        }
        let mut keys: Vec<String> = Vec::new();
        for text in h.keys.iter().map(|k| k.trim()).filter(|k| !k.is_empty()) {
            let combo = KeyCombo::parse(text)
                .map_err(RpcError::invalid_params)?
                .to_string();
            // The same keys twice would only be registered twice.
            if !keys.contains(&combo) {
                keys.push(combo);
            }
        }
        if keys.len() > HOTKEY_KEYS_MAX {
            return Err(RpcError::invalid_params(format!(
                "a hotkey can have at most {HOTKEY_KEYS_MAX} key combinations"
            )));
        }
        h.keys = keys;
        for steps in [&mut h.steps, &mut h.release_steps] {
            if steps.len() > HOTKEY_STEPS_MAX {
                return Err(RpcError::invalid_params(format!(
                    "a hotkey can have at most {HOTKEY_STEPS_MAX} steps"
                )));
            }
            for step in steps.iter_mut() {
                check_step(step, &mixer)?;
            }
        }
        if h.on_release != OnRelease::Steps {
            h.release_steps.clear();
        }
        if let Some(ms) = h.repeat_ms {
            let (lo, hi) = HOTKEY_REPEAT_MS;
            if !(lo..=hi).contains(&ms) {
                return Err(RpcError::invalid_params(format!(
                    "repeat_ms must be from {lo} to {hi}"
                )));
            }
        }
        let saved = self.edit_hotkeys(|list| {
            if h.id != 0 && !list.hotkeys.iter().any(|o| o.id == h.id) {
                return Err(RpcError::application(format!("no hotkey with id {}", h.id)));
            }
            if h.group != 0 {
                list.group_mut(h.group)?;
            }
            if let Some(other) = list
                .hotkeys
                .iter()
                .find(|o| o.id != h.id && o.name.eq_ignore_ascii_case(&h.name))
            {
                return Err(RpcError::application(format!(
                    "there is already a hotkey called '{}'",
                    other.name
                )));
            }
            for keys in &h.keys {
                if let Some(other) = list
                    .hotkeys
                    .iter()
                    .find(|o| o.id != h.id && o.keys.contains(keys))
                {
                    return Err(RpcError::application(format!(
                        "{keys} already belongs to the hotkey '{}'",
                        other.name
                    )));
                }
            }
            if h.id == 0 {
                h.id = list.hotkeys.iter().map(|o| o.id).max().unwrap_or(0) + 1;
                list.hotkeys.push(h.clone());
            } else {
                let pos = list.position(h.id)?;
                if list.hotkeys[pos].group == h.group {
                    list.hotkeys[pos] = h.clone();
                } else {
                    // Into another group: last in it, as moving it there
                    // would put it.
                    list.hotkeys.remove(pos);
                    let at = list
                        .hotkeys
                        .iter()
                        .rposition(|o| o.group == h.group)
                        .map_or(list.hotkeys.len(), |last| last + 1);
                    list.hotkeys.insert(at, h.clone());
                }
            }
            Ok(h)
        })?;
        info!("saved the hotkey '{}'", saved.name);
        Ok(to_json(&saved))
    }

    /// Remove a hotkey, save, and tell the runner and clients.
    pub(super) fn remove_hotkey(&self, r: HotkeyRef) -> Result<Value, RpcError> {
        let id = self.find_hotkey(&r.hotkey)?;
        let gone = self.edit_hotkeys(|list| {
            let pos = list.position(id)?;
            Ok(list.hotkeys.remove(pos))
        })?;
        info!("removed the hotkey '{}'", gone.name);
        Ok(to_json(&self.hotkeys_info()))
    }

    /// Switch a hotkey's keys on or off.
    pub(super) fn switch_hotkey(&self, p: SwitchHotkeyParams) -> Result<Value, RpcError> {
        let id = self.find_hotkey(&p.hotkey)?;
        let h = self.edit_hotkeys(|list| {
            let h = list.hotkey_mut(id)?;
            h.enabled = p.enabled.apply(h.enabled);
            Ok(h.clone())
        })?;
        info!(
            "switched the hotkey '{}' {}",
            h.name,
            if h.enabled { "on" } else { "off" }
        );
        Ok(to_json(&h))
    }

    /// Move a hotkey to another place in the list, or into another group:
    /// to `index` among the hotkeys of its group.
    pub(super) fn move_hotkey(&self, p: MoveHotkeyParams) -> Result<Value, RpcError> {
        let id = self.find_hotkey(&p.hotkey)?;
        let group = match &p.group {
            Some(key) => Some(self.find_group(key)?),
            None => None,
        };
        self.edit_hotkeys(|list| {
            let pos = list.position(id)?;
            let mut h = list.hotkeys.remove(pos);
            if let Some(group) = group {
                h.group = group;
            }
            let members: Vec<usize> = (0..list.hotkeys.len())
                .filter(|&i| list.hotkeys[i].group == h.group)
                .collect();
            let at = match p.index {
                Some(i) if i < members.len() => members[i],
                _ => members.last().map_or(list.hotkeys.len(), |&m| m + 1),
            };
            list.hotkeys.insert(at, h);
            Ok(())
        })?;
        Ok(to_json(&self.hotkeys_info()))
    }

    /// Add a group of hotkeys, last in the list.
    pub(super) fn add_hotkey_group(&self, p: AddHotkeyGroupParams) -> Result<Value, RpcError> {
        let name = group_name(&p.name)?;
        let group = self.edit_hotkeys(|list| {
            list.check_group_name(0, &name)?;
            let group = HotkeyGroup {
                id: list.groups.iter().map(|g| g.id).max().unwrap_or(0) + 1,
                name,
                enabled: p.enabled,
            };
            list.groups.push(group.clone());
            Ok(group)
        })?;
        info!("added the hotkey group '{}'", group.name);
        Ok(to_json(&group))
    }

    /// Rename a group of hotkeys, or switch it on or off.
    pub(super) fn set_hotkey_group(&self, p: SetHotkeyGroupParams) -> Result<Value, RpcError> {
        let id = self.find_group(&p.group)?;
        let name = p.name.as_deref().map(group_name).transpose()?;
        let group = self.edit_hotkeys(|list| {
            if let Some(name) = &name {
                list.check_group_name(id, name)?;
            }
            let g = list.group_mut(id)?;
            if let Some(name) = name {
                g.name = name;
            }
            if let Some(flag) = p.enabled {
                g.enabled = flag.apply(g.enabled);
            }
            Ok(g.clone())
        })?;
        Ok(to_json(&group))
    }

    /// Remove a group of hotkeys; its hotkeys stay, in no group.
    pub(super) fn remove_hotkey_group(&self, r: HotkeyGroupRef) -> Result<Value, RpcError> {
        let id = self.find_group(&r.group)?;
        let gone = self.edit_hotkeys(|list| {
            let pos = list
                .groups
                .iter()
                .position(|g| g.id == id)
                .ok_or_else(|| no_group(id))?;
            for h in list.hotkeys.iter_mut().filter(|h| h.group == id) {
                h.group = 0;
            }
            Ok(list.groups.remove(pos))
        })?;
        info!("removed the hotkey group '{}'", gone.name);
        Ok(to_json(&self.hotkeys_info()))
    }

    /// Move a group of hotkeys to `index` among the groups.
    pub(super) fn move_hotkey_group(&self, p: MoveHotkeyGroupParams) -> Result<Value, RpcError> {
        let id = self.find_group(&p.group)?;
        self.edit_hotkeys(|list| {
            let pos = list
                .groups
                .iter()
                .position(|g| g.id == id)
                .ok_or_else(|| no_group(id))?;
            let g = list.groups.remove(pos);
            let at = p.index.min(list.groups.len());
            list.groups.insert(at, g);
            Ok(())
        })?;
        Ok(to_json(&self.hotkeys_info()))
    }

    /// Change the hotkeys and their groups with `f`, save them, and tell
    /// the runner and clients. Nothing changes when `f` fails, or saving
    /// does.
    fn edit_hotkeys<T>(
        &self,
        f: impl FnOnce(&mut HotkeyList) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        let out = {
            let mut inner = self.inner.lock().unwrap();
            let list = inner.hotkeys.as_mut().map_err(|e| {
                RpcError::application(format!("hotkeys are read-only until this is fixed: {e}"))
            })?;
            let mut candidate = list.clone();
            let out = f(&mut candidate)?;
            if candidate == *list {
                return Ok(out);
            }
            config::save_hotkeys(&self.paths.hotkeys_file, &candidate)
                .map_err(|e| RpcError::application(format!("{e:#}")))?;
            *list = candidate.clone();
            inner
                .key_problems
                .retain(|id, _| candidate.hotkeys.iter().any(|h| h.id == *id));
            inner.target_problems = target_problems(&candidate.hotkeys, &inner.mixer);
            out
        };
        self.hotkeys_changed();
        Ok(out)
    }

    /// Press, let go of, or tap a hotkey, for a client. Answers with the
    /// hotkey once its steps are done; fades carry on.
    pub(super) fn hotkey_action(&self, r: HotkeyRef, action: Action) -> Result<Value, RpcError> {
        let id = self.find_hotkey(&r.hotkey)?;
        let hotkey = self.hotkey(id);
        let command = match action {
            Action::Press => Command::Press(id),
            Action::Release => Command::Release(id),
            Action::Run => Command::Run(id),
        };
        if self.tell_runner(command) {
            Ok(to_json(&hotkey))
        } else {
            Err(RpcError::application(
                "hotkeys are not running in this daemon",
            ))
        }
    }

    /// The id of the hotkey `key` names.
    fn find_hotkey(&self, key: &HotkeyKey) -> Result<HotkeyId, RpcError> {
        let hotkeys = self.hotkeys();
        let found = match key {
            HotkeyKey::Id(id) => hotkeys.iter().find(|h| h.id == *id),
            HotkeyKey::Name(name) => hotkeys
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case(name.trim())),
        };
        found.map(|h| h.id).ok_or_else(|| {
            RpcError::application(match key {
                HotkeyKey::Id(id) => format!("no hotkey with id {id}"),
                HotkeyKey::Name(name) => format!("no hotkey called '{name}'"),
            })
        })
    }

    /// The id of the group `key` names; 0, or an empty name, is no group.
    fn find_group(&self, key: &HotkeyGroupKey) -> Result<HotkeyGroupId, RpcError> {
        let groups = self.hotkey_groups();
        let found = match key {
            HotkeyGroupKey::Id(0) => return Ok(0),
            HotkeyGroupKey::Id(id) => groups.iter().find(|g| g.id == *id),
            HotkeyGroupKey::Name(name) => groups
                .iter()
                .find(|g| g.name.eq_ignore_ascii_case(name.trim())),
        };
        found.map(|g| g.id).ok_or_else(|| match key {
            HotkeyGroupKey::Id(id) => no_group(*id),
            HotkeyGroupKey::Name(name) => {
                RpcError::application(format!("no hotkey group called '{name}'"))
            }
        })
    }

    /// The hotkeys changed: tell the runner, which also updates the keys,
    /// and every client.
    fn hotkeys_changed(&self) {
        self.tell_runner(Command::Changed);
        self.announce(Notification::HotkeysChanged(self.hotkeys_info()));
    }

    /// Carry out one of a hotkey's steps, as a client's request would be,
    /// but without its own entry in the undo history: the runner records
    /// each press as a whole.
    pub fn run_step(&self, step: &HotkeyStep) -> Result<Value, RpcError> {
        let req = parse_step(step)?;
        if !allowed_in_hotkey(&req) {
            return Err(RpcError::invalid_params(format!(
                "{} cannot be a hotkey's step",
                step.method
            )));
        }
        self.handle_as(req, &mut Subscriptions::default(), false)
    }

    /// Record a press of a hotkey, from `before` to `after`, as one step
    /// in the undo history.
    pub fn record_change(&self, label: String, before: &MixerState, after: &MixerState) {
        let info = {
            let mut inner = self.inner.lock().unwrap();
            inner
                .history
                .record(Step::single(label), before, after)
                .then(|| inner.history.info())
        };
        if let Some(info) = info {
            self.announce(Notification::HistoryChanged(info));
        }
    }

    /// Rewrite the undo history's steps made since `since` with `f`; see
    /// [`crate::history::History::rewrite_since`].
    pub fn rewrite_history_since(
        &self,
        since: std::time::Instant,
        f: impl FnMut(&MixerState) -> MixerState,
    ) {
        self.inner.lock().unwrap().history.rewrite_since(since, f);
    }

    /// Replace the mixer with `state`, without an undo step: putting back
    /// what a held hotkey changed.
    pub fn set_mixer_quietly(&self, state: MixerState) {
        if let Err(e) = self.mutate(None, |m| {
            *m = state;
            Ok(())
        }) {
            warn!("could not put the mixer back: {}", e.message);
        }
    }
}

/// The error for a group that is not there.
fn no_group(id: HotkeyGroupId) -> RpcError {
    RpcError::application(format!("no hotkey group with id {id}"))
}

/// `name` as a group's name: trimmed, not empty, not too long.
fn group_name(name: &str) -> Result<String, RpcError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(RpcError::invalid_params("a hotkey group needs a name"));
    }
    if name.chars().count() > HOTKEY_NAME_MAX {
        return Err(RpcError::invalid_params(format!(
            "a hotkey group's name can be at most {HOTKEY_NAME_MAX} characters"
        )));
    }
    Ok(name.to_string())
}

impl HotkeyList {
    /// Where hotkey `id` is in the list.
    fn position(&self, id: HotkeyId) -> Result<usize, RpcError> {
        self.hotkeys
            .iter()
            .position(|h| h.id == id)
            .ok_or_else(|| RpcError::application(format!("no hotkey with id {id}")))
    }

    /// Hotkey `id`, to change.
    fn hotkey_mut(&mut self, id: HotkeyId) -> Result<&mut Hotkey, RpcError> {
        let pos = self.position(id)?;
        Ok(&mut self.hotkeys[pos])
    }

    /// Group `id`, to change.
    fn group_mut(&mut self, id: HotkeyGroupId) -> Result<&mut HotkeyGroup, RpcError> {
        self.groups
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or_else(|| no_group(id))
    }

    /// Refuse `name` for group `id` when another group has it.
    fn check_group_name(&self, id: HotkeyGroupId, name: &str) -> Result<(), RpcError> {
        match self
            .groups
            .iter()
            .find(|g| g.id != id && g.name.eq_ignore_ascii_case(name))
        {
            Some(other) => Err(RpcError::application(format!(
                "there is already a hotkey group called '{}'",
                other.name
            ))),
            None => Ok(()),
        }
    }
}

/// Check one step: a request a hotkey may make, with its strips and buses
/// looked up by name and kept by id, and a fade only where one works.
fn check_step(step: &mut HotkeyStep, mixer: &MixerState) -> Result<(), RpcError> {
    step.method = step.method.trim().to_string();
    names::resolve_names(&step.method, &mut step.params, mixer)?;
    let req = parse_step(step).map_err(|e| {
        RpcError::invalid_params(format!("the step {}: {}", step.method, e.message))
    })?;
    if !allowed_in_hotkey(&req) {
        return Err(RpcError::invalid_params(format!(
            "{} cannot be a hotkey's step",
            step.method
        )));
    }
    match step.over_ms {
        Some(0) => step.over_ms = None,
        Some(ms) if ms > HOTKEY_FADE_MS_MAX => {
            return Err(RpcError::invalid_params(format!(
                "a fade can take at most {HOTKEY_FADE_MS_MAX} ms"
            )))
        }
        Some(_) if !fadeable(step) => {
            return Err(RpcError::invalid_params(
                "only gain_db in set_strip and set_bus, and level_db in set_route, can fade",
            ))
        }
        _ => {}
    }
    Ok(())
}

/// The hotkeys whose steps work on a strip or bus that `mixer` no longer
/// has.
fn target_problems(hotkeys: &[Hotkey], mixer: &MixerState) -> Vec<HotkeyProblem> {
    hotkeys
        .iter()
        .filter(|h| {
            h.steps
                .iter()
                .chain(&h.release_steps)
                .flat_map(step_targets)
                .any(|t| match t {
                    StripOrBus::Strip(id) => mixer.strip(id).is_none(),
                    StripOrBus::Bus(id) => mixer.bus(id).is_none(),
                })
        })
        .map(|h| HotkeyProblem {
            hotkey: h.id,
            problem: "It works on a strip or bus that was removed, so part of it does nothing. \
                      Edit it to pick another."
                .into(),
        })
        .collect()
}
