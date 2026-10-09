//! Letting clients name strips and buses instead of numbering them, and
//! keeping hotkeys' names up to date when strips and buses are renamed.

use serde_json::{json, Value};
use weir_protocol::{visit_targets, MixerState, RpcError, TargetKind};

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
