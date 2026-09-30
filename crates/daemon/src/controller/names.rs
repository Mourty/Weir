//! Letting clients name strips and buses instead of numbering them.

use serde_json::{json, Value};
use weir_protocol::{BusId, MixerState, RpcError};

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
    #[derive(Clone, Copy)]
    enum Kind {
        Strip,
        Bus,
    }
    fn look(m: &MixerState, kind: Kind, v: &mut Value) -> Result<(), RpcError> {
        let Value::String(key) = v else {
            return Ok(());
        };
        let found = match kind {
            Kind::Strip => m.find_strip(key).map(|s| s.id),
            Kind::Bus => m.find_bus(key).map(|b| b.id),
        };
        match found {
            Some(id) => {
                *v = json!(id);
                Ok(())
            }
            None => Err(RpcError::application(format!(
                "no {} called '{key}'",
                match kind {
                    Kind::Strip => "strip",
                    Kind::Bus => "bus",
                }
            ))),
        }
    }
    fn walk(m: &MixerState, v: &mut Value) -> Result<(), RpcError> {
        match v {
            Value::Object(map) => {
                for (k, v) in map.iter_mut() {
                    match k.as_str() {
                        "strip" => look(m, Kind::Strip, v)?,
                        "bus" | "cue" => look(m, Kind::Bus, v)?,
                        "triggers" => {
                            if let Value::Array(items) = v {
                                for item in items {
                                    look(m, Kind::Strip, item)?;
                                }
                            }
                        }
                        "buses" | "routes" => {
                            if let Value::Array(items) = v {
                                for item in items {
                                    look(m, Kind::Bus, item)?;
                                }
                            }
                        }
                        "sends" => {
                            if let Value::Object(sends) = v {
                                let mut named = serde_json::Map::new();
                                for (bus, level) in std::mem::take(sends) {
                                    let mut key = Value::String(bus.clone());
                                    if bus.parse::<BusId>().is_err() {
                                        look(m, Kind::Bus, &mut key)?;
                                    }
                                    let key = match key {
                                        Value::String(s) => s,
                                        other => other.to_string(),
                                    };
                                    named.insert(key, level);
                                }
                                *sends = named;
                            }
                        }
                        _ => walk(m, v)?,
                    }
                }
                Ok(())
            }
            Value::Array(items) => items.iter_mut().try_for_each(|i| walk(m, i)),
            _ => Ok(()),
        }
    }
    if let Value::Object(map) = params {
        let kind = match method {
            "set_strip" | "remove_strip" | "move_strip" => Some(Kind::Strip),
            "set_bus" | "remove_bus" | "move_bus" => Some(Kind::Bus),
            _ => None,
        };
        if let (Some(kind), Some(id)) = (kind, map.get_mut("id")) {
            look(m, kind, id)?;
        }
    }
    walk(m, params)
}
