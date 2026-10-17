//! Application rules: putting applications on the strip their rule names
//! when they start playing. And the applications heard lately, for making
//! rules for ones that are not playing now.

use super::names::Rename;
use super::Inner;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use tracing::warn;
use weir_protocol::{
    find_named, rule_for, to_json, Notification, RpcError, SeenApp, StripId, TargetKind,
};

/// How many times a rule tries to move an application before leaving it be,
/// in case something else keeps moving it back.
const RULE_ATTEMPTS: u8 = 3;

/// The moves the application rules call for: each application not dealt with
/// yet whose rule puts it on a strip it is not on. Returns `(app, name,
/// strip)` for each.
pub(super) fn rule_moves(inner: &mut Inner) -> Vec<(u32, String, StripId)> {
    let playing: HashSet<(u32, String)> =
        inner.apps.iter().map(|a| (a.id, a.name.clone())).collect();
    inner.rules_done.retain(|k| playing.contains(k));
    inner.rules_tried.retain(|k, _| playing.contains(k));
    let mut moves = Vec::new();
    for a in &inner.apps {
        let key = (a.id, a.name.clone());
        if inner.rules_done.contains(&key) {
            continue;
        }
        let Some(name) = rule_for(&inner.app_rules, a).and_then(|r| r.strip.as_deref()) else {
            inner.rules_done.insert(key);
            continue;
        };
        // A strip this setup does not have: the rule waits, and is looked
        // at again with the next change of applications or setup.
        let Some(target) = find_named(&inner.mixer, TargetKind::Strip, name) else {
            continue;
        };
        if a.strip == Some(target) {
            inner.rules_done.insert(key);
            continue;
        }
        let tried = inner.rules_tried.entry(key.clone()).or_insert(0);
        if *tried >= RULE_ATTEMPTS {
            warn!(
                "rule: {} keeps leaving strip {target}; leaving it be",
                a.name
            );
            inner.rules_done.insert(key);
            continue;
        }
        *tried += 1;
        moves.push((a.id, a.name.clone(), target));
    }
    moves
}

impl super::Controller {
    /// Strips were renamed: rules naming them take the new names, and so
    /// does where applications heard lately last played.
    pub(super) fn rules_follow_renames(&self, renamed: &[Rename]) {
        let seen = {
            let mut inner = self.inner.lock().unwrap();
            inner
                .seen_apps
                .follow_renames(renamed)
                .then(|| inner.seen_apps.list().to_vec())
        };
        if let Some(list) = seen {
            self.seen_apps_changed(list);
        }
        let changed = {
            let mut inner = self.inner.lock().unwrap();
            let mut changed = false;
            for r in &mut inner.app_rules {
                let Some(strip) = &mut r.strip else { continue };
                if let Some((_, _, new)) = renamed
                    .iter()
                    .find(|(k, old, _)| *k == TargetKind::Strip && old.eq_ignore_ascii_case(strip))
                {
                    *strip = new.clone();
                    changed = true;
                }
            }
            changed.then(|| inner.app_rules.clone())
        };
        if let Some(rules) = changed {
            self.dirty.store(true, Ordering::Release);
            self.announce(Notification::AppRulesChanged(rules));
        }
    }

    /// The applications heard lately, most recently heard first.
    pub(super) fn seen_apps(&self) -> Vec<SeenApp> {
        self.inner.lock().unwrap().seen_apps.list().to_vec()
    }

    /// Take one application off the list of those heard lately.
    pub(super) fn forget_seen_app(&self, name: &str) -> Result<Value, RpcError> {
        let list = {
            let mut inner = self.inner.lock().unwrap();
            let inner = &mut *inner;
            inner
                .seen_apps
                .forget(name, &inner.apps)
                .map_err(|e| RpcError::application(e.to_string()))?;
            inner.seen_apps.list().to_vec()
        };
        self.seen_apps_changed(list.clone());
        Ok(to_json(&list))
    }

    /// Forget every application heard lately but those playing now.
    pub(super) fn clear_seen_apps(&self) -> Result<Value, RpcError> {
        let (cleared, list) = {
            let mut inner = self.inner.lock().unwrap();
            let inner = &mut *inner;
            let cleared = inner.seen_apps.clear(&inner.apps);
            (cleared, inner.seen_apps.list().to_vec())
        };
        if cleared {
            self.seen_apps_changed(list.clone());
        }
        Ok(to_json(&list))
    }

    /// The daemon is stopping: what plays now stops being heard.
    pub fn stop_hearing_apps(&self) {
        let mut inner = self.inner.lock().unwrap();
        let inner = &mut *inner;
        if !inner.apps.is_empty() {
            inner
                .seen_apps
                .stop_all(&inner.apps, crate::seen_apps::now_ms());
            self.seen_dirty.store(true, Ordering::Release);
        }
    }

    /// The applications heard lately changed: save them soon and tell
    /// clients.
    pub(super) fn seen_apps_changed(&self, list: Vec<SeenApp>) {
        self.seen_dirty.store(true, Ordering::Release);
        self.announce(Notification::SeenAppsChanged(list));
    }

    /// Carry out moves the application rules asked for.
    pub(super) fn make_moves(&self, moves: Vec<(u32, String, StripId)>) {
        let Some(engine) = self.engine() else {
            return;
        };
        for (app, name, strip) in moves {
            tracing::info!("rule: moving {name} ({app}) to strip {strip}");
            if let Err(e) = engine.move_app(app, strip) {
                warn!("could not move {name}: {e}");
            }
        }
    }
}
