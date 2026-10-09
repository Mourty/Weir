//! Putting back what a held hotkey changed, and nothing else.
//!
//! A hotkey that puts things back when let go (push to talk, dipping the
//! music while held) remembers the mixer from before the press. Letting go
//! must not simply bring that whole mixer back: anything else changed in
//! the meantime, such as a fader moved by hand, has to stay. So what the
//! hotkey itself changed is found by comparing the mixer before the press
//! with the mixer right after the hotkey's own changes, and only those
//! places are set back.
//!
//! Places are paths into the mixer as JSON. Lists of things with an `id`,
//! the strips and the buses, are followed by id rather than by position, so
//! a strip added meanwhile does not throw the paths off.

use serde_json::{Map, Value};

/// One step of a path into the mixer as JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    /// A field of an object.
    Key(String),
    /// The item with this `id` in a list.
    Id(u64),
}

/// A place in the mixer as JSON.
pub type Path = Vec<Seg>;

/// Whether `list` is one whose items are told apart by their `id`.
fn by_id(list: &[Value]) -> bool {
    !list.is_empty()
        && list
            .iter()
            .all(|v| v.get("id").and_then(Value::as_u64).is_some())
}

/// The item with `id` in `list`.
fn item(list: &[Value], id: u64) -> Option<&Value> {
    list.iter()
        .find(|v| v.get("id").and_then(Value::as_u64) == Some(id))
}

/// The places where `a` and `b` differ, as deep as they can be told apart.
pub fn diff(a: &Value, b: &Value) -> Vec<Path> {
    let mut out = Vec::new();
    walk(Some(a), Some(b), &mut Vec::new(), &mut out);
    out
}

fn walk(a: Option<&Value>, b: Option<&Value>, path: &mut Path, out: &mut Vec<Path>) {
    match (a, b) {
        (Some(Value::Object(x)), Some(Value::Object(y))) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                path.push(Seg::Key(k.clone()));
                walk(x.get(k), y.get(k), path, out);
                path.pop();
            }
        }
        (Some(Value::Array(x)), Some(Value::Array(y))) if by_id(x) && by_id(y) => {
            let mut ids: Vec<u64> = x
                .iter()
                .chain(y)
                .filter_map(|v| v.get("id").and_then(Value::as_u64))
                .collect();
            ids.sort_unstable();
            ids.dedup();
            for id in ids {
                path.push(Seg::Id(id));
                walk(item(x, id), item(y, id), path, out);
                path.pop();
            }
        }
        _ => {
            if a != b {
                out.push(path.clone());
            }
        }
    }
}

/// What is at `path` in `v`.
fn get<'a>(v: &'a Value, path: &[Seg]) -> Option<&'a Value> {
    path.iter().try_fold(v, |v, seg| match seg {
        Seg::Key(k) => v.get(k),
        Seg::Id(id) => v.as_array().and_then(|list| item(list, *id)),
    })
}

/// Set what is at `path` in `v` to `to`, or take it away for `None`. Does
/// nothing when the place no longer exists, such as a strip removed
/// meanwhile: that is not brought back.
fn set(v: &mut Value, path: &[Seg], to: Option<Value>) {
    let Some((last, parent_path)) = path.split_last() else {
        if let Some(to) = to {
            *v = to;
        }
        return;
    };
    let mut parent = v;
    for seg in parent_path {
        let next = match seg {
            Seg::Key(k) => parent.get_mut(k),
            Seg::Id(id) => parent.as_array_mut().and_then(|list| {
                list.iter_mut()
                    .find(|v| v.get("id").and_then(Value::as_u64) == Some(*id))
            }),
        };
        match next {
            Some(n) => parent = n,
            None => return,
        }
    }
    match last {
        Seg::Key(k) => {
            if !parent.is_object() {
                return;
            }
            let map: &mut Map<String, Value> = parent.as_object_mut().expect("checked");
            match to {
                Some(to) => {
                    map.insert(k.clone(), to);
                }
                None => {
                    map.remove(k);
                }
            }
        }
        Seg::Id(id) => {
            let Some(list) = parent.as_array_mut() else {
                return;
            };
            let at = list
                .iter()
                .position(|v| v.get("id").and_then(Value::as_u64) == Some(*id));
            match (at, to) {
                (Some(i), Some(to)) => list[i] = to,
                (Some(i), None) => {
                    list.remove(i);
                }
                // Gone since: not brought back.
                (None, _) => {}
            }
        }
    }
}

/// `now`, with every place in `paths` as it is in `before`.
pub fn restore(now: &Value, before: &Value, paths: &[Path]) -> Value {
    let mut out = now.clone();
    for path in paths {
        set(&mut out, path, get(before, path).cloned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_what_the_hotkey_changed_goes_back() {
        let before = json!({"strips": [
            {"id": 1, "name": "Mic", "mute": true, "gain_db": 0.0},
            {"id": 2, "name": "Music", "mute": false, "gain_db": -6.0}
        ]});
        // The hotkey unmuted the mic.
        let pressed = json!({"strips": [
            {"id": 1, "name": "Mic", "mute": false, "gain_db": 0.0},
            {"id": 2, "name": "Music", "mute": false, "gain_db": -6.0}
        ]});
        let paths = diff(&before, &pressed);
        assert_eq!(
            paths,
            vec![vec![
                Seg::Key("strips".into()),
                Seg::Id(1),
                Seg::Key("mute".into())
            ]]
        );
        // Meanwhile the music fader moved, and a strip was added in front.
        let now = json!({"strips": [
            {"id": 3, "name": "Game", "mute": false, "gain_db": 0.0},
            {"id": 1, "name": "Mic", "mute": false, "gain_db": 0.0},
            {"id": 2, "name": "Music", "mute": false, "gain_db": -12.0}
        ]});
        let back = restore(&now, &before, &paths);
        assert_eq!(
            back,
            json!({"strips": [
                {"id": 3, "name": "Game", "mute": false, "gain_db": 0.0},
                {"id": 1, "name": "Mic", "mute": true, "gain_db": 0.0},
                {"id": 2, "name": "Music", "mute": false, "gain_db": -12.0}
            ]})
        );
    }

    #[test]
    fn fields_the_hotkey_added_are_taken_away_and_removed_strips_stay_gone() {
        let before = json!({"strips": [{"id": 1, "sends": {}}]});
        let pressed = json!({"strips": [{"id": 1, "sends": {"3": -20.0}}]});
        let paths = diff(&before, &pressed);
        let now = json!({"strips": [{"id": 1, "sends": {"3": -20.0, "4": -1.0}}]});
        assert_eq!(
            restore(&now, &before, &paths),
            json!({"strips": [{"id": 1, "sends": {"4": -1.0}}]})
        );
        let gone = json!({"strips": []});
        assert_eq!(restore(&gone, &before, &paths), json!({"strips": []}));
    }
}
