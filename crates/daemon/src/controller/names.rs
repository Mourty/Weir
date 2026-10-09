//! Letting clients name strips and buses instead of numbering them, and
//! turning numbers back into names for files that go to another mixer.

use serde_json::{json, Value};
use weir_protocol::{BusId, MixerState, RpcError, TargetKind};

/// Let clients name strips and buses instead of giving their ids. In
/// `params`, a `strip` or `bus` given as text, the `id` of the methods about
/// one strip or bus, the strips and buses a ducking lists, a new strip's
/// `routes`, the buses of `sends` and a solo `cue` are looked up by name
/// (buses also by their label, such as `B1`) and replaced by the id.
pub(super) fn resolve_names(
    method: &str,
    params: &mut Value,
    m: &MixerState,
) -> Result<(), RpcError> {
    visit_targets(method, params, &mut |kind, v| {
        let Value::String(key) = v else {
            return Ok(());
        };
        match find(m, kind, key) {
            Some(id) => {
                *v = json!(id);
                Ok(())
            }
            None => Err(RpcError::application(format!(
                "no {} called '{key}'",
                word(kind)
            ))),
        }
    })
}

/// The other way: the strips and buses in `params` given by id, as their
/// names in `m`, so that the request means the same to a mixer whose ids
/// differ. Ids `m` does not have stay numbers.
pub(super) fn name_targets(method: &str, params: &mut Value, m: &MixerState) {
    let _ = visit_targets(method, params, &mut |kind, v| {
        let Some(id) = v.as_u64().and_then(|id| u32::try_from(id).ok()) else {
            return Ok(());
        };
        let name = match kind {
            TargetKind::Strip => m.strip(id).map(|s| s.name.clone()),
            TargetKind::Bus => m.bus(id).map(|b| b.name.clone()),
        };
        if let Some(name) = name {
            *v = Value::String(name);
        }
        Ok(())
    });
}

/// The id of the strip or bus `key` names in `m`.
pub(super) fn find(m: &MixerState, kind: TargetKind, key: &str) -> Option<u32> {
    match kind {
        TargetKind::Strip => m.find_strip(key).map(|s| s.id),
        TargetKind::Bus => m.find_bus(key).map(|b| b.id),
    }
}

/// "strip" or "bus".
pub(super) fn word(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Strip => "strip",
        TargetKind::Bus => "bus",
    }
}

/// Call `f` on every value in `params` that is a strip or bus, by name or
/// by id: the places [`resolve_names`] lists. A bus in `sends` is a key,
/// so `f` gets it as a number when it is one and as text otherwise, and
/// what `f` leaves is written back as the key.
pub(super) fn visit_targets(
    method: &str,
    params: &mut Value,
    f: &mut dyn FnMut(TargetKind, &mut Value) -> Result<(), RpcError>,
) -> Result<(), RpcError> {
    fn walk(
        v: &mut Value,
        f: &mut dyn FnMut(TargetKind, &mut Value) -> Result<(), RpcError>,
    ) -> Result<(), RpcError> {
        match v {
            Value::Object(map) => {
                for (k, v) in map.iter_mut() {
                    match k.as_str() {
                        "strip" => f(TargetKind::Strip, v)?,
                        "bus" | "cue" => f(TargetKind::Bus, v)?,
                        "triggers" => {
                            if let Value::Array(items) = v {
                                for item in items {
                                    f(TargetKind::Strip, item)?;
                                }
                            }
                        }
                        "buses" | "routes" => {
                            if let Value::Array(items) = v {
                                for item in items {
                                    f(TargetKind::Bus, item)?;
                                }
                            }
                        }
                        "sends" => {
                            if let Value::Object(sends) = v {
                                let mut named = serde_json::Map::new();
                                for (bus, level) in std::mem::take(sends) {
                                    let mut key = match bus.parse::<BusId>() {
                                        Ok(id) => json!(id),
                                        Err(_) => Value::String(bus),
                                    };
                                    f(TargetKind::Bus, &mut key)?;
                                    let key = match key {
                                        Value::String(s) => s,
                                        other => other.to_string(),
                                    };
                                    named.insert(key, level);
                                }
                                *sends = named;
                            }
                        }
                        _ => walk(v, f)?,
                    }
                }
                Ok(())
            }
            Value::Array(items) => items.iter_mut().try_for_each(|i| walk(i, f)),
            _ => Ok(()),
        }
    }
    if let Value::Object(map) = params {
        let kind = match method {
            "set_strip" | "remove_strip" | "move_strip" => Some(TargetKind::Strip),
            "set_bus" | "remove_bus" | "move_bus" => Some(TargetKind::Bus),
            _ => None,
        };
        if let (Some(kind), Some(id)) = (kind, map.get_mut("id")) {
            f(kind, id)?;
        }
    }
    walk(params, f)
}
