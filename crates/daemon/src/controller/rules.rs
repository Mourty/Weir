//! Application rules: putting applications on the strip their rule names
//! when they start playing.

use super::names::Rename;
use super::Inner;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use tracing::warn;
use weir_protocol::{find_named, rule_for, Notification, StripId, TargetKind};

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
    /// Strips were renamed: rules naming them take the new names.
    pub(super) fn rules_follow_renames(&self, renamed: &[Rename]) {
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
