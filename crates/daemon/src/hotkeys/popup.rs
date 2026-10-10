//! Showing what a hotkey did, as it is pressed: in the desktop's own small
//! popup, the one its volume keys show, or in a notification.
//!
//! What to show is read from the mixer after the hotkey's steps, not from
//! the steps themselves, so a mute that switches on or off says which it
//! is now, a volume step says where the volume got to, and letting go of
//! push to talk says the microphone is muted again. A single volume shows
//! as a bar; anything else as a line of words.
//!
//! The popup goes out on its own task, so a desktop slow to answer never
//! holds up a hotkey.

use ashpd::zbus;
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tracing::{debug, warn};
use weir_protocol::*;

/// What to show.
#[derive(Debug, Clone, PartialEq)]
pub enum Popup {
    /// Words, with an icon.
    Text { icon: &'static str, text: String },
    /// A volume: how far up its fader is, from 0 to 100, and what it is.
    Level {
        icon: &'static str,
        percent: u32,
        text: String,
    },
}

/// The icon for a volume of `db`, or muted.
fn volume_icon(db: f32, muted: bool) -> &'static str {
    if muted {
        "audio-volume-muted"
    } else if db <= -30.0 {
        "audio-volume-low"
    } else if db <= -10.0 {
        "audio-volume-medium"
    } else {
        "audio-volume-high"
    }
}

/// How far up a fader at `db` is, from 0 to 100, as the window draws it. A
/// fader at the bottom still shows a sliver, since Plasma says "muted" for
/// none at all.
fn percent(db: f32) -> u32 {
    let t = (db - GAIN_MIN_DB) / (GAIN_MAX_DB - GAIN_MIN_DB);
    ((t.clamp(0.0, 1.0) * 100.0).round() as u32).max(1)
}

/// A level in words: `-12 dB`, `+3 dB`.
fn db(v: f32) -> String {
    let v = (v * 10.0).round() / 10.0;
    if v > 0.0 {
        format!("+{v} dB")
    } else {
        format!("{v} dB")
    }
}

/// One thing a step did, read from the mixer after it.
enum Line {
    Text(&'static str, String),
    Level(&'static str, f32, String),
}

/// What effects are called in words, by their key in a strip or bus.
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

/// What a `set_strip` or `set_bus` step did to `obj` (the strip or bus
/// after it, as JSON), called `name`; `mic` for a strip that is a
/// microphone. A fading step shows where it is going.
fn patch_lines(step: &HotkeyStep, obj: &Value, name: &str, mic: bool, out: &mut Vec<Line>) {
    let Value::Object(params) = &step.params else {
        return;
    };
    let flag = |k: &str| obj.get(k).and_then(Value::as_bool).unwrap_or(false);
    let muted = flag("mute");
    for k in params.keys() {
        match k.as_str() {
            "id" => {}
            "mute" => {
                let icon = match (mic, muted) {
                    (true, true) => "microphone-sensitivity-muted",
                    (true, false) => "microphone-sensitivity-high",
                    (false, m) => volume_icon(0.0, m),
                };
                let what = if muted { "muted" } else { "unmuted" };
                out.push(Line::Text(icon, format!("{name} {what}")));
            }
            "gain_db" | "gain_delta_db" => {
                let now = obj.get("gain_db").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                let to = match (step.over_ms, params.get("gain_db")) {
                    (Some(_), Some(v)) => v.as_f64().map_or(now, |v| v as f32),
                    _ => now,
                };
                out.push(Line::Level(
                    volume_icon(to, muted),
                    to,
                    format!("{name}: {}", db(to)),
                ));
            }
            "solo" | "mono" => {
                let on = if flag(k) { "on" } else { "off" };
                out.push(Line::Text("weir", format!("{name}: {k} {on}")));
            }
            "delay_ms" | "delay_delta_ms" => {
                let ms = obj.get("delay_ms").and_then(Value::as_f64).unwrap_or(0.0);
                let text = if ms <= 0.0 {
                    format!("{name}: delay off")
                } else {
                    format!("{name}: delay {} ms", ms.round())
                };
                out.push(Line::Text("weir", text));
            }
            other => {
                let on = obj
                    .get(other)
                    .and_then(|e| e.get("enabled"))
                    .and_then(Value::as_bool);
                match (effect_name(other), on) {
                    (Some(effect), Some(on)) if params[other].get("enabled").is_some() => {
                        let on = if on { "on" } else { "off" };
                        out.push(Line::Text("weir", format!("{name}: {effect} {on}")));
                    }
                    _ => {}
                }
            }
        }
    }
}

/// What `step` did, read from `m`, the mixer after it. Steps whose strip
/// or bus is gone show nothing.
fn step_lines(step: &HotkeyStep, m: &MixerState, out: &mut Vec<Line>) {
    let p = &step.params;
    let id = |k: &str| p.get(k).and_then(Value::as_u64).map(|v| v as u32);
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
    match step.method.as_str() {
        "set_strip" => {
            if let Some(strip) = id("id").and_then(|i| m.strip(i)) {
                let obj = serde_json::to_value(strip).unwrap_or_default();
                let mic = strip.kind == StripKind::Hardware;
                patch_lines(step, &obj, &strip.name, mic, out);
            }
        }
        "set_bus" => {
            if let Some(bus) = id("id").and_then(|i| m.bus(i)) {
                let obj = serde_json::to_value(bus).unwrap_or_default();
                patch_lines(step, &obj, &bus.name, false, out);
            }
        }
        "set_route" => {
            let (Some(strip), Some(bus)) = (
                id("strip").and_then(|i| m.strip(i)),
                id("bus").and_then(|i| m.bus(i)),
            ) else {
                return;
            };
            let on = strip.routes.contains(&bus.id);
            if p.get("enabled").is_some() {
                let text = if on {
                    format!("{} in {}", strip.name, bus.name)
                } else {
                    format!("{} out of {}", strip.name, bus.name)
                };
                out.push(Line::Text(volume_icon(0.0, !on), text));
            }
            if p.get("level_db").is_some() || p.get("level_delta_db").is_some() {
                let now = strip.send_db(bus.id);
                let to = match (step.over_ms, p.get("level_db").and_then(Value::as_f64)) {
                    (Some(_), Some(v)) => v as f32,
                    _ => now,
                };
                out.push(Line::Level(
                    volume_icon(to, !on),
                    to,
                    format!("{} in {}: {}", strip.name, bus.name, db(to)),
                ));
            }
        }
        "load_scene" => out.push(Line::Text("weir", format!("Scene: {}", s("name")))),
        "load_setup" => {
            let text = match p.get("scene").and_then(Value::as_str) {
                Some(scene) => format!("Setup: {}, scene: {scene}", s("name")),
                None => format!("Setup: {}", s("name")),
            };
            out.push(Line::Text("weir", text));
        }
        "play_sound" => {}
        _ => out.push(Line::Text("weir", describe_step(step, m))),
    }
}

/// What to show for `steps`, with their strips and buses by id, done to
/// make `m`; `None` when there is nothing to say.
pub fn popup_for(steps: &[HotkeyStep], m: &MixerState) -> Option<Popup> {
    let mut lines = Vec::new();
    for step in steps {
        step_lines(step, m, &mut lines);
    }
    // A strip's mute and its volume in one step read best as a bar that
    // says muted: keep the volume, which says both.
    if let [Line::Level(icon, to, text)] = lines.as_slice() {
        return Some(Popup::Level {
            icon,
            percent: percent(*to),
            text: text.clone(),
        });
    }
    let icon = match lines.first()? {
        Line::Text(icon, _) | Line::Level(icon, _, _) => *icon,
    };
    let mut words: Vec<String> = lines
        .into_iter()
        .map(|l| match l {
            Line::Text(_, t) | Line::Level(_, _, t) => t,
        })
        .collect();
    words.dedup();
    const MOST: usize = 3;
    if words.len() > MOST {
        let more = words.len() - MOST;
        words.truncate(MOST);
        words.push(format!("and {more} more"));
    }
    Some(Popup::Text {
        icon,
        text: words.join(" · "),
    })
}

/// Sends popups to the task that shows them.
#[derive(Clone)]
pub struct Popups {
    tx: UnboundedSender<(Popup, HotkeyPopup)>,
}

impl Popups {
    /// Popups that go to `tx` instead of the desktop, for tests.
    #[cfg(test)]
    pub fn to(tx: UnboundedSender<(Popup, HotkeyPopup)>) -> Self {
        Self { tx }
    }

    /// Start the task that shows popups. Needs a Tokio runtime.
    pub fn start() -> Self {
        let (tx, rx) = unbounded_channel();
        tokio::spawn(show_popups(rx));
        Self { tx }
    }

    /// Show `popup` as `how` says.
    pub fn show(&self, popup: Popup, how: HotkeyPopup) {
        if how != HotkeyPopup::Nothing {
            let _ = self.tx.send((popup, how));
        }
    }
}

const PLASMA: &str = "org.kde.plasmashell";

/// Whether `name` is on the bus now.
async fn on_bus(connection: &zbus::Connection, name: &str) -> bool {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(connection).await else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from(name) else {
        return false;
    };
    dbus.name_has_owner(name).await.unwrap_or(false)
}

/// Show `popup` in Plasma's own popup.
async fn plasma(connection: &zbus::Connection, popup: &Popup) -> zbus::Result<()> {
    let path = "/org/kde/osdService";
    let interface = Some("org.kde.osdService");
    match popup {
        Popup::Text { icon, text } => {
            connection
                .call_method(Some(PLASMA), path, interface, "showText", &(icon, text))
                .await?;
        }
        Popup::Level {
            icon,
            percent,
            text,
        } => {
            let percent = *percent as i32;
            connection
                .call_method(
                    Some(PLASMA),
                    path,
                    interface,
                    "mediaPlayerVolumeChanged",
                    &(percent, text, icon),
                )
                .await?;
        }
    }
    Ok(())
}

/// Show `popup` as a notification that replaces Weir's last one and does
/// not stay in the desktop's list. Returns its id, for the next to
/// replace.
async fn notify(connection: &zbus::Connection, popup: &Popup, replaces: u32) -> zbus::Result<u32> {
    use zbus::zvariant::Value as V;
    let (icon, text, level) = match popup {
        Popup::Text { icon, text } => (*icon, text.as_str(), None),
        Popup::Level {
            icon,
            percent,
            text,
        } => (*icon, text.as_str(), Some(*percent as i32)),
    };
    let mut hints: HashMap<&str, V> = HashMap::new();
    hints.insert("transient", V::from(true));
    hints.insert("urgency", V::from(0u8));
    hints.insert("desktop-entry", V::from("weir"));
    // Notification services that know it show one in place of the last.
    hints.insert("x-canonical-private-synchronous", V::from("weir"));
    if let Some(level) = level {
        hints.insert("value", V::from(level));
    }
    let actions: Vec<&str> = Vec::new();
    let reply = connection
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("Weir", replaces, icon, text, "", actions, hints, 2000i32),
        )
        .await?;
    reply.body().deserialize()
}

/// The task showing popups, one at a time; when several wait, as a held
/// volume key makes them, only the newest.
async fn show_popups(mut rx: UnboundedReceiver<(Popup, HotkeyPopup)>) {
    let connection = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            warn!("hotkeys cannot show popups: no session bus ({e})");
            while rx.recv().await.is_some() {}
            return;
        }
    };
    let mut last_id = 0u32;
    while let Some(mut next) = rx.recv().await {
        while let Ok(newer) = rx.try_recv() {
            next = newer;
        }
        let (popup, how) = next;
        if how == HotkeyPopup::Popup && on_bus(&connection, PLASMA).await {
            match plasma(&connection, &popup).await {
                Ok(()) => continue,
                Err(e) => debug!("Plasma's popup failed, so a notification instead: {e}"),
            }
        }
        match notify(&connection, &popup, last_id).await {
            Ok(id) => last_id = id,
            Err(e) => debug!("could not show a notification: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mixer() -> MixerState {
        let mut music = Strip::new(2, "Music", StripKind::Virtual, ChannelLayout::Stereo);
        music.gain_db = -12.0;
        music.routes.insert(3);
        let mut mic = Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono);
        mic.mute = true;
        MixerState {
            strips: vec![mic, music],
            buses: vec![Bus::new(
                3,
                "Stream",
                BusKind::Virtual,
                ChannelLayout::Stereo,
            )],
        }
    }

    fn popup(steps: &[(&str, Value)]) -> Option<Popup> {
        let steps: Vec<HotkeyStep> = steps
            .iter()
            .map(|(m, p)| HotkeyStep::new(*m, p.clone()))
            .collect();
        popup_for(&steps, &mixer())
    }

    #[test]
    fn a_switch_says_where_it_got_to() {
        assert_eq!(
            popup(&[("set_strip", json!({"id": 1, "mute": "toggle"}))]),
            Some(Popup::Text {
                icon: "microphone-sensitivity-muted",
                text: "Mic muted".into()
            })
        );
        assert_eq!(
            popup(&[(
                "set_route",
                json!({"strip": 2, "bus": 3, "enabled": "toggle"})
            )]),
            Some(Popup::Text {
                icon: "audio-volume-high",
                text: "Music in Stream".into()
            })
        );
    }

    #[test]
    fn a_volume_shows_as_a_bar_where_the_fader_is() {
        assert_eq!(
            popup(&[("set_strip", json!({"id": 2, "gain_delta_db": -3}))]),
            Some(Popup::Level {
                icon: "audio-volume-medium",
                percent: 67,
                text: "Music: -12 dB".into()
            })
        );
        // A fade shows where it is going.
        let mut fade = HotkeyStep::new("set_strip", json!({"id": 2, "gain_db": -60}));
        fade.over_ms = Some(500);
        assert_eq!(
            popup_for(&[fade], &mixer()),
            Some(Popup::Level {
                icon: "audio-volume-low",
                percent: 1,
                text: "Music: -60 dB".into()
            })
        );
    }

    #[test]
    fn several_steps_read_as_one_line() {
        let p = popup(&[
            ("set_strip", json!({"id": 1, "mute": false})),
            ("set_strip", json!({"id": 2, "gain_db": -12})),
            ("load_scene", json!({"name": "Gaming"})),
            ("set_strip", json!({"id": 9, "mute": true})),
        ]);
        assert_eq!(
            p,
            Some(Popup::Text {
                icon: "microphone-sensitivity-muted",
                text: "Mic muted · Music: -12 dB · Scene: Gaming".into()
            }),
            "read from the mixer after; a strip that is gone shows nothing"
        );
        assert_eq!(popup(&[("play_sound", json!({"name": "Click"}))]), None);
    }
}
