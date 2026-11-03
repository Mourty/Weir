//! Safe mode: what it holds back, and bringing it back.
//!
//! The service starts in it with everything held back ([`SafeMode::
//! everything`], decided in `main.rs`), or the window asks for its own
//! parts after a crash (`enter_safe_mode`). What is held back is not set
//! up: strips and buses are left out of the engine's mixer
//! ([`SafeMode::engine_mixer`], in `push_state`), the keys task waits
//! ([`Controller::until_back`]), the user's sounds are not read, rules are
//! not applied, the update check waits, and recording is refused.
//! Bringing an item back sets it up as it would have been at the start.

use super::names::Rename;
use super::rules::rule_moves;
use super::Controller;
use serde_json::Value;
use tracing::info;
use weir_protocol::crash::Locked;
use weir_protocol::*;

/// The keys' status while safe mode holds hotkeys back.
pub(super) fn keys_held() -> KeysStatus {
    KeysStatus {
        method: KeysMethod::Unavailable,
        message: "Safe mode is holding hotkeys back. Bring them back to use them.".into(),
        ..Default::default()
    }
}

impl Controller {
    /// What safe mode holds back now.
    pub fn safe_mode(&self) -> SafeMode {
        self.safe.borrow().clone()
    }

    /// Whether safe mode holds `item` back now.
    pub fn holds(&self, item: &SafeItem) -> bool {
        self.safe.borrow().holds(item)
    }

    /// Wait until safe mode no longer holds `item` back; at once if it
    /// does not.
    pub async fn until_back(&self, item: SafeItem) {
        let mut safe = self.safe.subscribe();
        let _ = safe.wait_for(|s| !s.holds(&item)).await;
    }

    /// An error for a request that needs `item`, if safe mode holds it
    /// back.
    pub(super) fn needs(&self, item: SafeItem) -> Result<(), RpcError> {
        if self.holds(&item) {
            return Err(RpcError::application(format!(
                "safe mode is holding back {}; bring it back first",
                item.in_sentence()
            )));
        }
        Ok(())
    }

    /// `enter_safe_mode`: hold back what the window draws.
    pub(super) fn enter_safe_mode(&self, p: EnterSafeModeParams) -> Result<Value, RpcError> {
        let mixer = self.mixer();
        let before = self.safe_mode();
        self.safe
            .send_modify(|s| s.enter_for_window(p.why, &mixer, p.suspect));
        let now = self.safe_mode();
        if now != before {
            info!("{}", now.reason());
            self.safe_mode_changed(&before, &now);
        }
        Ok(to_json(&now))
    }

    /// `bring_back`: bring some or all of what safe mode holds back.
    pub(super) fn bring_back_request(&self, p: BringBackParams) -> Result<Value, RpcError> {
        let before = self.safe_mode();
        if !before.on {
            return Err(RpcError::application("Weir is not in safe mode"));
        }
        let items = if p.all { before.held.clone() } else { p.items };
        if let Some(missing) = items.iter().find(|i| !before.held.contains(i)) {
            return Err(RpcError::application(format!(
                "safe mode is not holding back {}",
                missing.in_sentence()
            )));
        }
        self.before_bringing_back(&items);
        self.safe.send_modify(|s| {
            s.bring_back(&items);
        });
        let now = self.safe_mode();
        let named: Vec<String> = items.iter().map(SafeItem::label).collect();
        info!("safe mode: brought back {}", named.join(", "));
        self.safe_mode_changed(&before, &now);
        Ok(to_json(&now))
    }

    /// Bring everything back, and leave safe mode.
    pub fn leave_safe_mode(&self) {
        let before = self.safe_mode();
        if !before.on {
            return;
        }
        self.before_bringing_back(&before.held);
        self.safe.send_modify(|s| {
            let all = s.held.clone();
            s.bring_back(&all);
            s.on = false;
        });
        info!("left safe mode");
        self.safe_mode_changed(&before, &self.safe_mode());
    }

    /// What must be ready before `items` come back: the keys task starts
    /// as soon as hotkeys do, and says how keys reach Weir, so the words
    /// for hotkeys held back go first.
    fn before_bringing_back(&self, items: &[SafeItem]) {
        if items.contains(&SafeItem::Hotkeys) {
            let status = KeysStatus {
                method: KeysMethod::Starting,
                message: "Weir is getting hotkeys ready.".into(),
                ..Default::default()
            };
            let problems = self.inner.locked().key_problems.clone();
            self.set_keys_status(status, problems);
        }
    }

    /// Set up what came back between `before` and `now`, and tell crash
    /// reports and clients.
    fn safe_mode_changed(&self, before: &SafeMode, now: &SafeMode) {
        let back = |item: &SafeItem| before.holds(item) && !now.holds(item);
        let audio_changed = before.held.iter().any(|i| {
            back(i)
                && matches!(
                    i,
                    SafeItem::Strip(_) | SafeItem::Bus(_) | SafeItem::ExternalEffects
                )
        }) || now.held.iter().any(|i| !before.holds(i));
        if audio_changed {
            self.push_state(self.mixer());
        }
        if back(&SafeItem::HotkeySounds) {
            // Read the user's own sounds now, as at the start.
            self.sounds_changed();
        }
        if back(&SafeItem::AppRules) {
            // Applications playing now get their rules, as if they had
            // just started.
            let moves = {
                let mut inner = self.inner.locked();
                inner.rules_done.clear();
                inner.rules_tried.clear();
                rule_moves(&mut inner)
            };
            self.make_moves(moves);
        }
        weir_protocol::crash::note_safe_mode(now);
        self.announce(Notification::SafeModeChanged(now.clone()));
    }

    /// Strips and buses renamed while held back stay held back.
    pub(super) fn safe_follow_renames(&self, renamed: &[Rename]) {
        if renamed.is_empty() || !self.safe.borrow().on {
            return;
        }
        self.safe.send_modify(|s| {
            for (kind, old, new) in renamed {
                s.rename(*kind == TargetKind::Strip, old, new);
            }
        });
        let now = self.safe_mode();
        weir_protocol::crash::note_safe_mode(&now);
        self.announce(Notification::SafeModeChanged(now));
    }
}
