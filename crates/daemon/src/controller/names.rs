//! Letting clients name strips and buses instead of numbering them, and
//! keeping hotkeys' names up to date when strips and buses are renamed.

use crate::config;
use serde_json::{json, Value};
use std::cell::OnceCell;
use std::path::Path;
use weir_protocol::{
    find_named, target_name, visit_targets, MixerState, RpcError, Setup, TargetKind,
};

/// Whether `method` keeps strips by name rather than by id: app rules, so
/// that a rule can wait for a setup that has its strip.
pub(super) fn takes_names(method: &str) -> bool {
    method == "set_app_rules"
}

/// Let clients name strips and buses instead of giving their ids. In
/// `params`, a `strip` or `bus` given as text, the `id` of the methods about
/// one strip or bus, the strips and buses a ducking lists, a new strip's
/// `routes`, the buses of `sends` and a solo `cue` are looked up by name
/// (buses also by their label, such as `B1`) and replaced by the id. Not
/// in a hotkey's steps.
pub(super) fn resolve_names(
    method: &str,
    params: &mut Value,
    m: &MixerState,
) -> Result<(), RpcError> {
    // A hotkey's steps keep names, which may be those of strips that only a
    // saved setup has; set_hotkey looks them up itself.
    if method == "set_hotkey" {
        return Ok(());
    }
    // These keep names: ids given become names, and names are checked by
    // the handler, which may find them in a saved setup.
    if takes_names(method) {
        return visit_targets(method, params, &mut |kind, v| {
            if let Value::Number(n) = v {
                let id = n.as_u64().and_then(|id| u32::try_from(id).ok());
                let name = id.and_then(|id| target_name(m, kind, id)).ok_or_else(|| {
                    RpcError::application(format!("no {} with id {n}", kind.word()))
                })?;
                *v = Value::String(name.to_string());
            }
            Ok(())
        });
    }
    visit_targets(method, params, &mut |kind, v| {
        let Value::String(key) = v else {
            return Ok(());
        };
        match find(m, kind, key) {
            Some(id) => {
                *v = json!(id);
                Ok(())
            }
            None => Err(no_such(kind, key)),
        }
    })
}

/// The id of the strip or bus `key` names in `m`.
pub(super) fn find(m: &MixerState, kind: TargetKind, key: &str) -> Option<u32> {
    match kind {
        TargetKind::Strip => m.find_strip(key).map(|s| s.id),
        TargetKind::Bus => m.find_bus(key).map(|b| b.id),
    }
}

/// "no strip called 'X'".
pub(super) fn no_such(kind: TargetKind, name: &str) -> RpcError {
    RpcError::application(format!("no {} called '{name}'", kind.word()))
}

/// A strip or bus that has another name now: its kind, the name it had and
/// the one it has.
pub(super) type Rename = (TargetKind, String, String);

/// The strips and buses that are in both `before` and `after` under other
/// names: renamed, when `after` is `before` changed rather than another
/// mixer put in its place.
pub(super) fn renames(before: &MixerState, after: &MixerState) -> Vec<Rename> {
    let strips = after.strips.iter().filter_map(|s| {
        let old = before.strip(s.id)?;
        (old.name != s.name).then(|| (TargetKind::Strip, old.name.clone(), s.name.clone()))
    });
    let buses = after.buses.iter().filter_map(|b| {
        let old = before.bus(b.id)?;
        (old.name != b.name).then(|| (TargetKind::Bus, old.name.clone(), b.name.clone()))
    });
    strips.chain(buses).collect()
}

/// Give the strips and buses in `params` their new names. Each name is
/// looked at once, so two strips swapping names swap in `params` too.
/// Returns whether anything changed.
pub(super) fn rename_targets(method: &str, params: &mut Value, renames: &[Rename]) -> bool {
    let mut changed = false;
    let _ = visit_targets::<()>(method, params, &mut |kind, v| {
        let Value::String(name) = v else {
            return Ok(());
        };
        if let Some((_, _, new)) = renames
            .iter()
            .find(|(k, old, _)| *k == kind && old.eq_ignore_ascii_case(name))
        {
            *name = new.clone();
            changed = true;
        }
        Ok(())
    });
    changed
}

/// Where strips and buses are looked for when something keeps them by name
/// and may be meant for another setup: in the mixer now, then in each saved
/// setup, read only when a name is not in the mixer now.
pub(super) struct Mixers<'a> {
    here: MixerState,
    setups_dir: &'a Path,
    /// Setups that are not saved (yet), looked in after the saved ones.
    extra: Vec<MixerState>,
    setups: OnceCell<Vec<MixerState>>,
}

impl<'a> Mixers<'a> {
    pub(super) fn new(here: MixerState, setups_dir: &'a Path, extra: Vec<MixerState>) -> Self {
        Self {
            here,
            setups_dir,
            extra,
            setups: OnceCell::new(),
        }
    }

    fn setups(&self) -> &[MixerState] {
        self.setups.get_or_init(|| {
            let mut setups: Vec<MixerState> = config::list_saved(self.setups_dir)
                .iter()
                .filter_map(|name| config::load_saved::<Setup>(self.setups_dir, name, "setup").ok())
                .map(|setup| setup.mixer())
                .collect();
            setups.extend(self.extra.iter().cloned());
            setups
        })
    }

    /// The first mixer that has the strip or bus `key` names, and its id
    /// there.
    pub(super) fn locate(&self, kind: TargetKind, key: &str) -> Option<(&MixerState, u32)> {
        if let Some(id) = find_named(&self.here, kind, key) {
            return Some((&self.here, id));
        }
        self.setups()
            .iter()
            .find_map(|m| find_named(m, kind, key).map(|id| (m, id)))
    }

    /// The strip or bus `key` names, as its name and id in the first mixer
    /// that has it.
    pub(super) fn find(&self, kind: TargetKind, key: &str) -> Option<(String, u32)> {
        let (m, id) = self.locate(kind, key)?;
        Some((target_name(m, kind, id)?.to_string(), id))
    }

    /// `v`, a strip or bus given by id or by name, as its name: an id is
    /// one in the mixer now, and a name may be one only a setup has.
    pub(super) fn name(&self, kind: TargetKind, v: &mut Value) -> Result<(), RpcError> {
        match v {
            Value::Number(n) => {
                let id = n.as_u64().and_then(|id| u32::try_from(id).ok());
                let name = id
                    .and_then(|id| target_name(&self.here, kind, id))
                    .ok_or_else(|| {
                        RpcError::application(format!("no {} with id {n}", kind.word()))
                    })?;
                *v = Value::String(name.to_string());
            }
            Value::String(key) => {
                let (name, _) = self.find(kind, key).ok_or_else(|| no_such(kind, key))?;
                *v = Value::String(name);
            }
            _ => {}
        }
        Ok(())
    }
}
