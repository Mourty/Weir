//! Application rules: putting applications on the strip their rule names
//! when they start playing.

use super::Inner;
use std::collections::HashSet;
use tracing::warn;
use weir_protocol::{rule_for, StripId};

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
        let target = rule_for(&inner.app_rules, a)
            .and_then(|r| r.strip)
            .filter(|id| inner.mixer.strip(*id).is_some());
        let Some(target) = target else {
            inner.rules_done.insert(key);
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
