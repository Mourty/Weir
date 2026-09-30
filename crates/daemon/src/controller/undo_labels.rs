//! What each change is called in the undo history, and which changes
//! count as the same one.
//!
//! A step's label is what people see in the window's Undo menu: "Music
//! fader", "Mute Mic". Its key says what it is a change of: the dozens of
//! updates a fader drag sends all have the key `strip/2/gain_db`, so the
//! history records the drag as one step.

use crate::history::Step;
use serde_json::Value;
use weir_protocol::{BusId, Flag, MixerState, Request, StripId};

/// The fields a patch sets, as dotted paths one level deep (`gain_db`,
/// `gate.threshold_db`, `sends.3`), sorted. Two patches that set the same
/// fields are changes of the same thing.
fn field_paths<T: serde::Serialize>(patch: &T) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(Value::Object(map)) = serde_json::to_value(patch) {
        for (k, v) in map {
            if k == "id" {
                continue;
            }
            match v {
                Value::Object(inner) if !inner.is_empty() => {
                    out.extend(inner.keys().map(|ik| format!("{k}.{ik}")));
                }
                _ => out.push(k),
            }
        }
    }
    out.sort();
    out
}

/// The distinct top-level fields among `paths`.
fn top_fields(paths: &[String]) -> Vec<&str> {
    let mut v: Vec<&str> = paths
        .iter()
        .map(|p| p.split('.').next().unwrap_or(p))
        .collect();
    v.dedup();
    v
}

/// What a request that changes the mixer is called in the undo history,
/// and what it is a change of. `None` for requests that change nothing
/// undoable.
pub(super) fn undo_step(req: &Request, m: &MixerState) -> Option<Step> {
    let strip = |id: StripId| {
        m.strip(id)
            .map_or_else(|| format!("strip {id}"), |s| s.name.clone())
    };
    let bus = |id: BusId| match (m.bus(id), m.bus_label(id)) {
        (Some(b), Some(l)) => format!("{l} {}", b.name),
        _ => format!("bus {id}"),
    };
    // What a flag does, given what the setting is now.
    let on_off = |v: Option<Flag>, now: bool, on: &str, off: &str| {
        if v.is_some_and(|f| f.apply(now)) {
            on.to_string()
        } else {
            off.to_string()
        }
    };
    let strip_now = |id: StripId| m.strip(id).cloned();
    let bus_now = |id: BusId| m.bus(id).cloned();
    Some(match req {
        Request::SetStrip(p) => {
            let paths = field_paths(p);
            let name = strip(p.id);
            let label = match top_fields(&paths).as_slice() {
                ["gain_db"] => format!("{name} fader"),
                ["mute"] => {
                    let now = strip_now(p.id).is_some_and(|s| s.mute);
                    format!("{} {name}", on_off(p.mute, now, "Mute", "Unmute"))
                }
                ["solo"] => {
                    let now = strip_now(p.id).is_some_and(|s| s.solo);
                    format!("{} {name}", on_off(p.solo, now, "Solo", "Unsolo"))
                }
                ["gain_delta_db"] | ["gain_db", "gain_delta_db"] => format!("{name} fader"),
                ["pan"] => format!("{name} pan"),
                ["name"] => format!("Rename {name} to {}", p.name.as_deref().unwrap_or_default()),
                ["layout"] => format!("{name} channel layout"),
                ["device"] => format!("{name} device"),
                ["color"] => format!("{name} color"),
                ["sends"] => match p.sends.as_ref().and_then(|s| s.keys().next()) {
                    Some(b) => format!("{name} level in {}", bus(*b)),
                    None => format!("{name} levels"),
                },
                ["upmix"] => format!("{name} upmix"),
                ["subwoofer"] => format!("{name} bass on the subwoofer"),
                ["subwoofer", "upmix"] => format!("{name} upmix"),
                ["eq"] => format!("{name} equalizer"),
                ["gate"] => format!("{name} gate"),
                ["denoise"] => format!("{name} noise suppression"),
                ["compressor"] => format!("{name} compressor"),
                ["ducking"] => format!("{name} ducking"),
                _ => format!("Change {name}"),
            };
            Step::new(label, format!("strip/{}/{}", p.id, paths.join(",")))
        }
        Request::SetBus(p) => {
            let paths = field_paths(p);
            let name = bus(p.id);
            let label = match top_fields(&paths).as_slice() {
                ["gain_db"] => format!("{name} fader"),
                ["mute"] => {
                    let now = bus_now(p.id).is_some_and(|b| b.mute);
                    format!("{} {name}", on_off(p.mute, now, "Mute", "Unmute"))
                }
                ["gain_delta_db"] | ["gain_db", "gain_delta_db"] => format!("{name} fader"),
                ["mono"] => format!("{name} mono"),
                ["name"] => format!("Rename {name} to {}", p.name.as_deref().unwrap_or_default()),
                ["layout"] => format!("{name} channel layout"),
                ["device"] => format!("{name} device"),
                ["color"] => format!("{name} color"),
                ["eq"] => format!("{name} equalizer"),
                ["limiter"] => format!("{name} limiter"),
                ["downmix"] => format!("{name} downmix"),
                _ => format!("Change {name}"),
            };
            Step::new(label, format!("bus/{}/{}", p.id, paths.join(",")))
        }
        Request::SetRoute(p) => {
            let (s, b) = (strip(p.strip), bus(p.bus));
            let routed = m.strip(p.strip).is_some_and(|x| x.routes.contains(&p.bus));
            let level_only = p.level_db.is_some() || p.level_delta_db.is_some();
            match p.enabled {
                None if level_only => Step::new(
                    format!("{s} level in {b}"),
                    format!("route/{}/{}/level", p.strip, p.bus),
                ),
                enabled => {
                    let on = enabled.map_or(!routed, |f| f.apply(routed));
                    Step::single(if on {
                        format!("Send {s} to {b}")
                    } else {
                        format!("Stop sending {s} to {b}")
                    })
                }
            }
        }
        Request::AddStrip(p) => Step::single(format!("Add strip {}", p.name)),
        Request::RemoveStrip(p) => Step::single(format!("Remove strip {}", strip(p.id))),
        Request::AddBus(p) => Step::single(format!("Add bus {}", p.name)),
        Request::RemoveBus(p) => Step::single(format!("Remove bus {}", bus(p.id))),
        Request::MoveStrip(p) => Step::new(
            format!("Move strip {}", strip(p.id)),
            format!("move/strip/{}", p.id),
        ),
        Request::MoveBus(p) => Step::new(
            format!("Move bus {}", bus(p.id)),
            format!("move/bus/{}", p.id),
        ),
        Request::LoadSetup(p) => Step::single(format!("Load setup {}", p.name)),
        Request::LoadScene(p) => Step::single(format!("Load scene {}", p.name)),
        Request::ApplyEqPreset(p) => {
            let target = match (p.strip, p.bus) {
                (Some(id), _) => strip(id),
                (_, Some(id)) => bus(id),
                _ => String::new(),
            };
            Step::single(format!("{target} equalizer preset {}", p.name))
        }
        _ => return None,
    })
}
