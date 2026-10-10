//! Undo and redo of changes to the mixer.
//!
//! Every change a client makes is recorded with the state from before it,
//! so it can be taken back from any client: the window's Ctrl+Z, the command
//! line, a Stream Deck button. A burst of the same change, such as the dozens
//! of updates one fader drag sends, is recorded once, as a single step.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use weir_protocol::{HistoryEntry, HistoryInfo, MixerState};

/// Changes with the same key closer together than this are one step.
const COALESCE: Duration = Duration::from_millis(1500);
/// Most steps kept.
const MAX_STEPS: usize = 100;
/// Most steps listed in [`HistoryInfo`] each way.
const MAX_LISTED: usize = 25;

/// A change about to be made: what to call it, and what it is a change of.
#[derive(Debug, Clone)]
pub struct Step {
    /// What people see in the undo menu, such as "Music fader".
    pub label: String,
    /// Changes with the same non-empty key in quick succession are one
    /// step. An empty key never merges.
    pub key: String,
    /// It puts another mixer in place of this one, as loading a setup
    /// does. A strip that keeps its id there may be another strip, so
    /// undoing or redoing it does not take one as renamed.
    pub whole: bool,
}

impl Step {
    /// A step that merges with quick changes of the same `key`.
    pub fn new(label: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            key: key.into(),
            whole: false,
        }
    }

    /// A step that never merges with another.
    pub fn single(label: impl Into<String>) -> Self {
        Self::new(label, "")
    }

    /// This step, putting another mixer in place of this one.
    pub fn whole_mixer(mut self) -> Self {
        self.whole = true;
        self
    }
}

struct Entry {
    label: String,
    key: String,
    /// See [`Step::whole`].
    whole: bool,
    /// The state to go back to: from before the change on the undo list,
    /// from before the undo on the redo list.
    state: MixerState,
    /// When the step was last added to, for merging.
    touched: Instant,
    /// When the step was made.
    created: Instant,
    at_ms: u64,
}

#[derive(Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
}

/// Milliseconds since the Unix epoch, for the history's time stamps.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl History {
    /// Record a change from `before` to `after`. Returns whether what can be
    /// undone or redone changed.
    pub fn record(&mut self, step: Step, before: &MixerState, after: &MixerState) -> bool {
        if before == after {
            return false;
        }
        self.redo.clear();
        if let Some(last) = self.undo.last_mut() {
            if !step.key.is_empty() && last.key == step.key && last.touched.elapsed() < COALESCE {
                last.touched = Instant::now();
                last.at_ms = now_ms();
                last.whole |= step.whole;
                // Dragged back to where it started: nothing left to undo.
                if last.state == *after {
                    self.undo.pop();
                }
                return true;
            }
        }
        self.undo.push(Entry {
            label: step.label,
            key: step.key,
            whole: step.whole,
            state: before.clone(),
            touched: Instant::now(),
            created: Instant::now(),
            at_ms: now_ms(),
        });
        if self.undo.len() > MAX_STEPS {
            self.undo.remove(0);
        }
        true
    }

    /// Take back the most recent step. Returns the state to restore.
    pub fn undo(&mut self, current: &MixerState) -> Option<MixerState> {
        let e = self.undo.pop()?;
        self.redo.push(Entry {
            label: e.label,
            key: String::new(),
            whole: e.whole,
            state: current.clone(),
            touched: Instant::now(),
            created: Instant::now(),
            at_ms: e.at_ms,
        });
        self.seal();
        Some(e.state)
    }

    /// Bring back the step most recently undone. Returns the state to
    /// restore.
    pub fn redo(&mut self, current: &MixerState) -> Option<MixerState> {
        let e = self.redo.pop()?;
        let restore = e.state;
        self.undo.push(Entry {
            label: e.label,
            key: String::new(),
            whole: e.whole,
            state: current.clone(),
            touched: Instant::now(),
            created: Instant::now(),
            at_ms: e.at_ms,
        });
        Some(restore)
    }

    /// Whether the step an undo (`back`) or a redo would take puts
    /// another mixer in place, as loading a setup does.
    pub fn next_is_whole(&self, back: bool) -> bool {
        let list = if back { &self.undo } else { &self.redo };
        list.last().is_some_and(|e| e.whole)
    }

    /// Rewrite the states of the steps made since `since` with `f`. A held
    /// hotkey that puts back what it changed uses this, so that undoing a
    /// change made while it was held does not bring its change back.
    pub fn rewrite_since(&mut self, since: Instant, mut f: impl FnMut(&MixerState) -> MixerState) {
        for e in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            if e.created >= since {
                e.state = f(&e.state);
            }
        }
    }

    /// Stop the most recent step from taking in the next change, so a
    /// change right after an undo is a step of its own.
    fn seal(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.key.clear();
        }
    }

    /// What can be undone and redone, most recent first, for clients.
    pub fn info(&self) -> HistoryInfo {
        let entry = |e: &Entry| HistoryEntry {
            label: e.label.clone(),
            at_ms: e.at_ms,
        };
        HistoryInfo {
            undo: self.undo.iter().rev().take(MAX_LISTED).map(entry).collect(),
            redo: self.redo.iter().rev().take(MAX_LISTED).map(entry).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weir_protocol::{ChannelLayout, Strip, StripKind};

    fn with_gain(db: f32) -> MixerState {
        let mut m = MixerState::default();
        m.strips.push(Strip {
            gain_db: db,
            ..Strip::new(1, "Music", StripKind::Virtual, ChannelLayout::Stereo)
        });
        m
    }

    #[test]
    fn a_drag_is_one_step() {
        let mut h = History::default();
        let mut state = with_gain(0.0);
        for k in 1..=20 {
            let next = with_gain(-(k as f32));
            h.record(Step::new("Music fader", "strip/1/gain_db"), &state, &next);
            state = next;
        }
        assert_eq!(h.info().undo.len(), 1);
        assert_eq!(h.undo(&state), Some(with_gain(0.0)));
        assert!(h.info().undo.is_empty());
        assert_eq!(h.info().redo[0].label, "Music fader");
    }

    #[test]
    fn different_changes_are_different_steps() {
        let mut h = History::default();
        h.record(
            Step::new("a", "strip/1/gain_db"),
            &with_gain(0.0),
            &with_gain(-1.0),
        );
        h.record(
            Step::new("b", "strip/1/pan"),
            &with_gain(-1.0),
            &with_gain(-2.0),
        );
        h.record(Step::single("c"), &with_gain(-2.0), &with_gain(-3.0));
        h.record(Step::single("d"), &with_gain(-3.0), &with_gain(-4.0));
        let labels: Vec<String> = h.info().undo.into_iter().map(|e| e.label).collect();
        assert_eq!(labels, ["d", "c", "b", "a"]);
    }

    #[test]
    fn undo_and_redo_walk_back_and_forth() {
        let mut h = History::default();
        h.record(Step::single("one"), &with_gain(0.0), &with_gain(-1.0));
        h.record(Step::single("two"), &with_gain(-1.0), &with_gain(-2.0));
        let back = h.undo(&with_gain(-2.0)).unwrap();
        assert_eq!(back, with_gain(-1.0));
        let back = h.undo(&back).unwrap();
        assert_eq!(back, with_gain(0.0));
        assert!(h.undo(&back).is_none());
        let fwd = h.redo(&back).unwrap();
        assert_eq!(fwd, with_gain(-1.0));
        // A new change drops what could still be redone.
        h.record(Step::single("three"), &fwd, &with_gain(-5.0));
        assert!(h.info().redo.is_empty());
        assert_eq!(h.undo(&with_gain(-5.0)), Some(with_gain(-1.0)));
    }

    #[test]
    fn a_change_after_an_undo_does_not_merge_into_an_older_step() {
        let mut h = History::default();
        let key = "strip/1/gain_db";
        h.record(Step::new("a", key), &with_gain(0.0), &with_gain(-1.0));
        h.record(Step::new("b", key), &with_gain(-1.0), &with_gain(-2.0));
        assert_eq!(h.info().undo.len(), 1);
        h.record(Step::single("x"), &with_gain(-2.0), &with_gain(-3.0));
        let back = h.undo(&with_gain(-3.0)).unwrap();
        h.record(Step::new("c", key), &back, &with_gain(-4.0));
        assert_eq!(h.info().undo.len(), 2);
    }

    #[test]
    fn nothing_changed_is_not_a_step_and_a_round_trip_cancels_out() {
        let mut h = History::default();
        assert!(!h.record(Step::single("x"), &with_gain(0.0), &with_gain(0.0)));
        h.record(
            Step::new("m", "strip/1/mute"),
            &with_gain(0.0),
            &with_gain(-1.0),
        );
        h.record(
            Step::new("m", "strip/1/mute"),
            &with_gain(-1.0),
            &with_gain(0.0),
        );
        assert!(h.info().undo.is_empty());
    }
}
