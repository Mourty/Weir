//! Hotkeys: keys that do things in the mixer, wherever you are.
//!
//! A hotkey is a key combination and a list of steps, each step a request
//! as a client would send it, such as `set_strip` with `"mute": "toggle"`.
//! Hotkeys can also have no keys at all, and be pressed by name from
//! `weirctl` or the API, so a Stream Deck button can share them.
//!
//! This module also puts hotkeys into words, for the window and `weirctl`
//! to show the same descriptions.

use crate::model::{MixerState, StripOrBus};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Identifies a hotkey. Stable for its whole life.
pub type HotkeyId = u32;

/// Longest a hotkey's name may be.
pub const HOTKEY_NAME_MAX: usize = 60;
/// Most steps a hotkey may have, on press and on release each.
pub const HOTKEY_STEPS_MAX: usize = 32;
/// The shortest and longest time between repeats while held, in ms.
pub const HOTKEY_REPEAT_MS: (u32, u32) = (50, 2000);
/// The longest a step may take to fade, in ms.
pub const HOTKEY_FADE_MS_MAX: u32 = 60_000;
/// Most key combinations a hotkey may have.
pub const HOTKEY_KEYS_MAX: usize = 8;

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// A list, or a single item on its own: hotkeys had one key combination
/// before they could have several, written as a string.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match Option::<OneOrMany>::deserialize(d)? {
        None => Vec::new(),
        Some(OneOrMany::One(s)) => vec![s],
        Some(OneOrMany::Many(v)) => v,
    })
}

/// Keys and what they do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Hotkey {
    /// Unique, given by the daemon. In `set_hotkey`, 0 or left out adds a
    /// new hotkey; an existing id replaces that one.
    #[serde(default)]
    pub id: HotkeyId,
    /// Unique among hotkeys, ignoring case, up to 60 characters. Scripts and
    /// Stream Deck buttons press hotkeys by name.
    pub name: String,
    /// Whether its keys work. Off, it can still be pressed by name.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The keys: one or more combinations, up to 8, such as
    /// `["Ctrl+Alt+M", "F9"]`, each any of Ctrl, Alt, Shift and Super and
    /// one key. Any of them presses the hotkey. A single combination may be
    /// given as a string. Left out or empty, the hotkey has no keys and is
    /// only pressed by name.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "one_or_many"
    )]
    pub keys: Vec<String>,
    /// What pressing it does, in order.
    pub steps: Vec<HotkeyStep>,
    /// Whether each press does every step (`all`), or only the next one,
    /// going round (`next`), to go through presets or scenes with one key.
    #[serde(default, skip_serializing_if = "is_default")]
    pub each_press: EachPress,
    /// What letting go of the keys does.
    #[serde(default, skip_serializing_if = "is_default")]
    pub on_release: OnRelease,
    /// With `on_release` `steps`: what letting go does, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub release_steps: Vec<HotkeyStep>,
    /// Do the steps again every this many milliseconds while the keys are
    /// held, from 50 to 2000: for turning a volume up or down by holding a
    /// key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_ms: Option<u32>,
}

impl Hotkey {
    /// Whether it does anything when the keys are let go, so it needs to
    /// see them being let go.
    pub fn acts_on_release(&self) -> bool {
        self.on_release != OnRelease::Nothing || self.repeat_ms.is_some()
    }
}

/// One thing a hotkey does: a request, as a client would send it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HotkeyStep {
    /// The method, such as `set_strip` or `load_scene`.
    pub method: String,
    /// Its parameters, as in the request. Strips and buses may be given by
    /// name; the daemon keeps their ids, so renaming one does not break the
    /// hotkey.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
    /// Take this many milliseconds, up to 60000, instead of changing at
    /// once: a fade. For `gain_db` in `set_strip` and `set_bus`, and
    /// `level_db` in `set_route`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over_ms: Option<u32>,
}

impl HotkeyStep {
    /// A step with `method` and `params`.
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            method: method.into(),
            params,
            over_ms: None,
        }
    }
}

/// What each press of a hotkey does.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EachPress {
    /// Every step, in order.
    #[default]
    All,
    /// The next step only, back to the first after the last.
    Next,
}

/// What letting go of a hotkey's keys does.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OnRelease {
    /// Nothing.
    #[default]
    Nothing,
    /// Put back everything pressing changed: push to talk, or dipping the
    /// music while held.
    Restore,
    /// Do the hotkey's `release_steps`.
    Steps,
}

/// Every hotkey, and how the keys reach Weir. What `list_hotkeys` returns
/// and `hotkeys_changed` carries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HotkeysInfo {
    /// Every hotkey, in the order they were added.
    pub hotkeys: Vec<Hotkey>,
    /// How keys reach Weir on this desktop.
    pub keys: KeysStatus,
    /// What is wrong with any of them, such as keys another program has
    /// taken, or a strip that was removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<HotkeyProblem>,
}

/// How keys reach Weir on this desktop.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct KeysStatus {
    /// Which way: `desktop`, the desktop's shortcut service (KDE Plasma,
    /// GNOME 48 and newer, Hyprland); `x11`, Weir watching the keys itself
    /// on an X11 desktop; `unavailable`, neither; or `starting`, not known
    /// yet.
    pub method: KeysMethod,
    /// What that means, in a sentence for people.
    pub message: String,
    /// With `desktop`: the keys each hotkey really has, by id, as the
    /// desktop describes them, one for each of the hotkey's `keys` in the
    /// same order, and empty for one the desktop gave no keys. People can
    /// change them in the desktop's settings, so they can differ from the
    /// hotkey's `keys`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assigned: BTreeMap<HotkeyId, Vec<String>>,
}

/// Which way keys reach Weir.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeysMethod {
    /// Not known yet: the desktop has not started, or Weir is still asking.
    #[default]
    Starting,
    /// The desktop's shortcut service looks after the keys.
    Desktop,
    /// Weir watches the keys itself, on an X11 desktop.
    X11,
    /// Hotkeys can only be pressed by name here.
    Unavailable,
}

/// Something wrong with a hotkey.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HotkeyProblem {
    /// The hotkey.
    pub hotkey: HotkeyId,
    /// What is wrong, in a sentence for people.
    pub problem: String,
}

/// Which hotkey a request is about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HotkeyRef {
    /// The hotkey, by id or by name.
    pub hotkey: HotkeyKey,
}

/// A hotkey by id or by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum HotkeyKey {
    /// By id.
    Id(HotkeyId),
    /// By name, ignoring case.
    Name(String),
}

/// The strips and buses `step` works on, by id, for noticing ones that no
/// longer exist.
pub fn step_targets(step: &HotkeyStep) -> Vec<StripOrBus> {
    let p = &step.params;
    let id = |key: &str| p.get(key).and_then(Value::as_u64).map(|v| v as u32);
    let mut out = Vec::new();
    match step.method.as_str() {
        "set_strip" => out.extend(id("id").map(StripOrBus::Strip)),
        "set_bus" => out.extend(id("id").map(StripOrBus::Bus)),
        "set_route" | "apply_eq_preset" | "move_app" => {
            out.extend(id("strip").map(StripOrBus::Strip));
            out.extend(id("bus").map(StripOrBus::Bus));
        }
        _ => {}
    }
    out
}

/// A number of dB without a sign: `3`, `4.5`.
fn plain(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// A level for people: `-20 dB`, `+3 dB`, `0 dB`.
fn db(v: f64) -> String {
    if v > 0.0 {
        format!("+{} dB", plain(v))
    } else {
        format!("{} dB", plain(v))
    }
}

/// The size of a step up or down: `3 dB`.
fn db_size(v: f64) -> String {
    format!("{} dB", plain(v.abs()))
}

/// What effects are called in words.
fn effect_name(key: &str) -> Option<&'static str> {
    Some(match key {
        "eq" => "equalizer",
        "gate" => "noise gate",
        "denoise" => "noise suppression",
        "compressor" => "compressor",
        "ducking" => "ducking",
        "insert" => "external effects",
        "limiter" => "limiter",
        "downmix" => "downmix",
        _ => return None,
    })
}

/// A switch in words: `mute`, `unmute`, `switch mute on or off`.
fn switch(v: &Value, on: &str, off: &str, what: &str) -> String {
    match v {
        Value::Bool(true) => on.into(),
        Value::Bool(false) => off.into(),
        _ => format!("switch {what} on or off"),
    }
}

/// The name of strip `v` (an id, or a name as given).
fn strip_name(m: &MixerState, v: Option<&Value>) -> String {
    match v {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|id| m.strip(id as u32))
            .map_or_else(|| "a removed strip".into(), |s| s.name.clone()),
        Some(Value::String(s)) => s.clone(),
        _ => "a strip".into(),
    }
}

/// The name of bus `v` (an id, or a name or label as given).
fn bus_name(m: &MixerState, v: Option<&Value>) -> String {
    match v {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|id| m.bus(id as u32))
            .map_or_else(|| "a removed bus".into(), |b| b.name.clone()),
        Some(Value::String(s)) => s.clone(),
        _ => "a bus".into(),
    }
}

/// What a `set_strip` or `set_bus` changes, in words.
fn patch_words(p: &Value) -> Vec<String> {
    let Value::Object(map) = p else {
        return Vec::new();
    };
    let num = |k: &str| map.get(k).and_then(Value::as_f64);
    let mut out = Vec::new();
    for (k, v) in map {
        match k.as_str() {
            "id" => {}
            "mute" => out.push(switch(v, "mute", "unmute", "mute")),
            "solo" => out.push(switch(v, "solo", "solo off", "solo")),
            "mono" => out.push(switch(v, "mono", "mono off", "mono")),
            "gain_db" => out.push(format!("volume to {}", db(num(k).unwrap_or(0.0)))),
            "gain_delta_db" => {
                let d = num(k).unwrap_or(0.0);
                let way = if d < 0.0 { "down" } else { "up" };
                out.push(format!("volume {way} {}", db_size(d)));
            }
            "pan" => {
                let p = num(k).unwrap_or(0.0);
                out.push(if p == 0.0 {
                    "pan to the middle".into()
                } else {
                    let side = if p < 0.0 { "left" } else { "right" };
                    format!("pan {:.0}% {side}", p.abs() * 100.0)
                });
            }
            "pan_delta" => {
                let d = num(k).unwrap_or(0.0);
                let side = if d < 0.0 { "left" } else { "right" };
                out.push(format!("pan a little {side}"));
            }
            "delay_ms" => {
                let ms = num(k).unwrap_or(0.0);
                out.push(if ms <= 0.0 {
                    "delay off".into()
                } else {
                    format!("delay to {} ms", plain(ms))
                });
            }
            "delay_delta_ms" => {
                let d = num(k).unwrap_or(0.0);
                let way = if d < 0.0 { "shorter" } else { "longer" };
                out.push(format!("delay {} ms {way}", plain(d.abs())));
            }
            "name" => out.push(format!("rename to {}", v.as_str().unwrap_or("?"))),
            _ => match (effect_name(k), v.get("enabled")) {
                (Some(effect), Some(on)) => {
                    let words = switch(
                        on,
                        &format!("{effect} on"),
                        &format!("{effect} off"),
                        &format!("the {effect}"),
                    );
                    out.push(if v.as_object().is_some_and(|o| o.len() > 1) {
                        format!("{words}, with new settings")
                    } else {
                        words
                    });
                }
                (Some(effect), None) => out.push(format!("{effect} settings")),
                (None, _) => out.push(k.replace('_', " ")),
            },
        }
    }
    out
}

/// One step in words, such as `Mic: unmute` or `Load the scene Gaming`,
/// with names looked up in `m`.
pub fn describe_step(step: &HotkeyStep, m: &MixerState) -> String {
    let p = &step.params;
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
    let mut text = match step.method.as_str() {
        "set_strip" => format!(
            "{}: {}",
            strip_name(m, p.get("id")),
            patch_words(p).join(", ")
        ),
        "set_bus" => format!(
            "{}: {}",
            bus_name(m, p.get("id")),
            patch_words(p).join(", ")
        ),
        "set_route" => {
            let bus = bus_name(m, p.get("bus"));
            let mut words = Vec::new();
            if let Some(v) = p.get("enabled") {
                words.push(switch(
                    v,
                    &format!("send to {bus}"),
                    &format!("stop sending to {bus}"),
                    &format!("sending to {bus}"),
                ));
            }
            if let Some(v) = p.get("level_db").and_then(Value::as_f64) {
                words.push(format!("level in {bus} to {}", db(v)));
            }
            if let Some(d) = p.get("level_delta_db").and_then(Value::as_f64) {
                let way = if d < 0.0 { "down" } else { "up" };
                words.push(format!("level in {bus} {way} {}", db_size(d)));
            }
            format!("{}: {}", strip_name(m, p.get("strip")), words.join(", "))
        }
        "apply_eq_preset" => {
            let target = if p.get("bus").is_some() {
                bus_name(m, p.get("bus"))
            } else {
                strip_name(m, p.get("strip"))
            };
            format!("{target}: equalizer preset {}", s("name"))
        }
        "load_scene" => format!("Load the scene {}", s("name")),
        "load_setup" => format!("Load the setup {}", s("name")),
        "save_scene" => format!("Save the scene {}", s("name")),
        "show_window" => "Show the Weir window".into(),
        "undo" | "redo" => {
            let n = p.get("steps").and_then(Value::as_u64).unwrap_or(1);
            let verb = if step.method == "undo" {
                "Undo"
            } else {
                "Redo"
            };
            if n == 1 {
                format!("{verb} the last change")
            } else {
                format!("{verb} {n} changes")
            }
        }
        "set_app_volume" => "An application's volume".into(),
        "move_app" => format!("Move an application to {}", strip_name(m, p.get("strip"))),
        "set_settings" => "Change Weir's settings".into(),
        other => format!("Request {other}"),
    };
    if let Some(ms) = step.over_ms.filter(|&ms| ms > 0) {
        let secs = f64::from(ms) / 1000.0;
        text.push_str(&format!(", over {secs} s"));
    }
    text
}

/// A whole hotkey in words, such as `Mic: unmute; put back when let go`.
pub fn describe_hotkey(h: &Hotkey, m: &MixerState) -> String {
    if h.steps.is_empty() {
        return "Does nothing yet".into();
    }
    let steps: Vec<String> = h.steps.iter().map(|s| describe_step(s, m)).collect();
    let mut text = match h.each_press {
        EachPress::All => steps.join("; "),
        EachPress::Next => format!("Each press: {}", steps.join(", then ")),
    };
    if h.repeat_ms.is_some() {
        text.push_str(", again and again while held");
    }
    match h.on_release {
        OnRelease::Nothing => {}
        OnRelease::Restore => text.push_str("; put back when let go"),
        OnRelease::Steps => {
            let release: Vec<String> = h
                .release_steps
                .iter()
                .map(|s| describe_step(s, m))
                .collect();
            text.push_str(&format!("; when let go: {}", release.join("; ")));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bus, BusKind, ChannelLayout, Strip, StripKind};
    use serde_json::json;

    fn mixer() -> MixerState {
        MixerState {
            strips: vec![
                Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono),
                Strip::new(2, "Music", StripKind::Virtual, ChannelLayout::Stereo),
            ],
            buses: vec![Bus::new(
                3,
                "Stream Mic",
                BusKind::Virtual,
                ChannelLayout::Stereo,
            )],
        }
    }

    #[test]
    fn steps_read_as_sentences() {
        let m = mixer();
        let d = |method: &str, params: Value| describe_step(&HotkeyStep::new(method, params), &m);
        assert_eq!(
            d("set_strip", json!({"id": 1, "mute": "toggle"})),
            "Mic: switch mute on or off"
        );
        assert_eq!(
            d("set_strip", json!({"id": 1, "mute": false})),
            "Mic: unmute"
        );
        assert_eq!(
            d("set_strip", json!({"id": 2, "gain_delta_db": -3})),
            "Music: volume down 3 dB"
        );
        assert_eq!(
            d("set_strip", json!({"id": 2, "gain_db": -20})),
            "Music: volume to -20 dB"
        );
        assert_eq!(
            d("set_strip", json!({"id": 1, "gate": {"enabled": true}})),
            "Mic: noise gate on"
        );
        assert_eq!(
            d("set_bus", json!({"id": 3, "delay_ms": 180})),
            "Stream Mic: delay to 180 ms"
        );
        assert_eq!(
            d("set_bus", json!({"id": 3, "delay_delta_ms": -5})),
            "Stream Mic: delay 5 ms shorter"
        );
        assert_eq!(
            d("set_bus", json!({"id": 3, "delay_ms": 0})),
            "Stream Mic: delay off"
        );
        assert_eq!(
            d(
                "set_route",
                json!({"strip": 2, "bus": 3, "enabled": "toggle"})
            ),
            "Music: switch sending to Stream Mic on or off"
        );
        assert_eq!(
            d("apply_eq_preset", json!({"strip": 1, "name": "Warm voice"})),
            "Mic: equalizer preset Warm voice"
        );
        assert_eq!(
            d("load_scene", json!({"name": "Gaming"})),
            "Load the scene Gaming"
        );
        assert_eq!(
            d("set_strip", json!({"id": 9, "mute": true})),
            "a removed strip: mute"
        );
        assert_eq!(d("ping", Value::Null), "Request ping");
        let mut fade = HotkeyStep::new("set_strip", json!({"id": 2, "gain_db": -20}));
        fade.over_ms = Some(300);
        assert_eq!(
            describe_step(&fade, &m),
            "Music: volume to -20 dB, over 0.3 s"
        );
    }

    #[test]
    fn whole_hotkeys_read_as_sentences() {
        let m = mixer();
        let mut h = Hotkey {
            id: 1,
            name: "Push to talk".into(),
            enabled: true,
            keys: vec!["Ctrl+Alt+Space".into()],
            steps: vec![HotkeyStep::new(
                "set_strip",
                json!({"id": 1, "mute": false}),
            )],
            each_press: EachPress::All,
            on_release: OnRelease::Restore,
            release_steps: Vec::new(),
            repeat_ms: None,
        };
        assert_eq!(describe_hotkey(&h, &m), "Mic: unmute; put back when let go");
        assert!(h.acts_on_release());
        h.on_release = OnRelease::Nothing;
        h.each_press = EachPress::Next;
        h.steps
            .push(HotkeyStep::new("load_scene", json!({"name": "Gaming"})));
        assert_eq!(
            describe_hotkey(&h, &m),
            "Each press: Mic: unmute, then Load the scene Gaming"
        );
        assert!(!h.acts_on_release());
    }

    #[test]
    fn hotkeys_keep_their_json_short() {
        let h: Hotkey = serde_json::from_value(json!({
            "name": "Mute mic",
            "keys": "Ctrl+Alt+M",
            "steps": [{"method": "set_strip", "params": {"id": 1, "mute": "toggle"}}]
        }))
        .unwrap();
        assert!(h.enabled);
        assert_eq!(h.each_press, EachPress::All);
        // One combination may come as a string, but always goes out as a list.
        assert_eq!(
            serde_json::to_value(&h).unwrap(),
            json!({
                "id": 0,
                "name": "Mute mic",
                "keys": ["Ctrl+Alt+M"],
                "steps": [{"method": "set_strip", "params": {"id": 1, "mute": "toggle"}}]
            })
        );
        let two: Hotkey = serde_json::from_value(json!({
            "name": "Talk", "keys": ["F9", "Ctrl+Alt+T"], "steps": []
        }))
        .unwrap();
        assert_eq!(two.keys, ["F9", "Ctrl+Alt+T"]);
        let none: Hotkey =
            serde_json::from_value(json!({"name": "Intro", "keys": null, "steps": []})).unwrap();
        assert!(none.keys.is_empty());
        assert!(serde_json::to_value(&none).unwrap().get("keys").is_none());
        let r: HotkeyRef = serde_json::from_value(json!({"hotkey": "Mute mic"})).unwrap();
        assert_eq!(r.hotkey, HotkeyKey::Name("Mute mic".into()));
        let r: HotkeyRef = serde_json::from_value(json!({"hotkey": 4})).unwrap();
        assert_eq!(r.hotkey, HotkeyKey::Id(4));
    }
}
