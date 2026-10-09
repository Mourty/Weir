//! Where requests name strips and buses, and looking them up.
//!
//! A client can give a strip or bus by id or by name. Hotkeys keep them by
//! name and look the name up each time they run, so that a hotkey works on
//! the strip of that name in whichever setup is loaded: each setup is a
//! whole mixer, with ids of its own, and the same id can be another strip
//! there.

use crate::model::{BusId, MixerState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Strip or bus.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// A strip.
    Strip,
    /// A bus.
    Bus,
}

impl TargetKind {
    /// "strip" or "bus".
    pub fn word(self) -> &'static str {
        match self {
            Self::Strip => "strip",
            Self::Bus => "bus",
        }
    }
}

/// Call `f` on every value in the `params` of a `method` request that is a
/// strip or bus, by name or by id: a `strip` or `bus`, the `id` of the
/// methods about one strip or bus, the strips and buses a ducking lists, a
/// new strip's `routes`, the buses of `sends` and a solo `cue`. A bus in
/// `sends` is a key, so `f` gets it as a number when it is one and as text
/// otherwise, and what `f` leaves is written back as the key.
pub fn visit_targets<E>(
    method: &str,
    params: &mut Value,
    f: &mut dyn FnMut(TargetKind, &mut Value) -> Result<(), E>,
) -> Result<(), E> {
    fn walk<E>(
        v: &mut Value,
        f: &mut dyn FnMut(TargetKind, &mut Value) -> Result<(), E>,
    ) -> Result<(), E> {
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

/// The id of the strip or bus called `name` in `m`, as a hotkey means it:
/// by name (ignoring case) first, then a bus by its label (`B1`), then an
/// id written as text. Hotkeys keep names, so a strip called "2" is found
/// by its name rather than taken for strip 2.
pub fn find_named(m: &MixerState, kind: TargetKind, name: &str) -> Option<u32> {
    let name = name.trim();
    match kind {
        TargetKind::Strip => m
            .strips
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .or_else(|| m.find_strip(name))
            .map(|s| s.id),
        TargetKind::Bus => m
            .buses
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case(name))
            .or_else(|| m.find_bus(name))
            .map(|b| b.id),
    }
}

/// The name of the strip or bus with `id` in `m`.
pub fn target_name(m: &MixerState, kind: TargetKind, id: u32) -> Option<&str> {
    match kind {
        TargetKind::Strip => m.strip(id).map(|s| s.name.as_str()),
        TargetKind::Bus => m.bus(id).map(|b| b.name.as_str()),
    }
}

/// The strips and buses in `params` that `m` has, by name; ids `m` does
/// not have stay numbers.
pub fn targets_by_name(method: &str, params: &mut Value, m: &MixerState) {
    let _ = visit_targets::<()>(method, params, &mut |kind, v| {
        let id = v.as_u64().and_then(|id| u32::try_from(id).ok());
        if let Some(name) = id.and_then(|id| target_name(m, kind, id)) {
            *v = Value::String(name.to_string());
        }
        Ok(())
    });
}

/// The strips and buses in `params` that `m` has, by id; names `m` does
/// not have stay names.
pub fn targets_by_id(method: &str, params: &mut Value, m: &MixerState) {
    let _ = visit_targets::<()>(method, params, &mut |kind, v| {
        if let Some(id) = v.as_str().and_then(|name| find_named(m, kind, name)) {
            *v = json!(id);
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bus, BusKind, ChannelLayout, Strip, StripKind};

    fn mixer() -> MixerState {
        MixerState {
            strips: vec![
                Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono),
                Strip::new(2, "Music", StripKind::Virtual, ChannelLayout::Stereo),
                Strip::new(3, "1", StripKind::Virtual, ChannelLayout::Stereo),
            ],
            buses: vec![Bus::new(
                4,
                "Stream",
                BusKind::Virtual,
                ChannelLayout::Stereo,
            )],
        }
    }

    #[test]
    fn every_place_is_visited() {
        let mut p = json!({
            "id": 1,
            "sends": {"4": -6.0, "Stream": -3.0},
            "ducking": {"triggers": [2, "Mic"], "buses": ["B1"]},
        });
        let mut seen = Vec::new();
        visit_targets::<()>("set_strip", &mut p, &mut |kind, v| {
            seen.push((kind, v.clone()));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen.len(), 6);
        assert!(seen.contains(&(TargetKind::Strip, json!(1))));
        assert!(seen.contains(&(TargetKind::Bus, json!(4))));
        assert!(seen.contains(&(TargetKind::Bus, json!("B1"))));
    }

    #[test]
    fn names_and_ids_go_both_ways() {
        let m = mixer();
        let mut p = json!({"strip": 2, "bus": 4, "level_db": -6.0});
        targets_by_name("set_route", &mut p, &m);
        assert_eq!(
            p,
            json!({"strip": "Music", "bus": "Stream", "level_db": -6.0})
        );
        targets_by_id("set_route", &mut p, &m);
        assert_eq!(p, json!({"strip": 2, "bus": 4, "level_db": -6.0}));

        let mut p = json!({"id": 9, "sends": {"4": -6.0}});
        targets_by_name("set_strip", &mut p, &m);
        assert_eq!(p, json!({"id": 9, "sends": {"Stream": -6.0}}));
    }

    #[test]
    fn a_name_wins_over_an_id_written_as_text() {
        let m = mixer();
        assert_eq!(find_named(&m, TargetKind::Strip, "1"), Some(3));
        assert_eq!(find_named(&m, TargetKind::Strip, "music"), Some(2));
        assert_eq!(find_named(&m, TargetKind::Strip, "2"), Some(2));
        assert_eq!(find_named(&m, TargetKind::Bus, "b1"), Some(4));
        assert_eq!(find_named(&m, TargetKind::Strip, "Guitar"), None);
    }
}
