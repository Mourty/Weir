//! Safe mode: Weir running with parts of itself held back, so that a
//! setup that makes it crash can still be used, and the part to blame can
//! be found by bringing the others back one at a time.
//!
//! The service keeps one [`SafeMode`] for every client. It starts with
//! everything held back when asked to (`weir-daemon --safe`, `restart`
//! with `safe`, the tray), or after it stopped because of a bug soon after
//! starting. The window asks for its own parts and every strip and bus to
//! be held back (`enter_safe_mode`) after it closed because of a bug soon
//! after starting. Items come back with `bring_back`. A crash report says
//! what came back last, and safe mode after that crash keeps it back.
//!
//! Nothing held back is changed or forgotten: it is only left out of what
//! is drawn or set up, and comes back as it was saved.

use crate::model::MixerState;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A crash this soon after a program started, in seconds, is taken to be
/// one at startup, which safe mode answers.
pub const EARLY_CRASH_SECS: u64 = 30;

/// Something safe mode can hold back.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SafeItem {
    /// The window's look: light or dark, the desktop's font and accent
    /// color. Held back, the window is dark and in its own font and colors.
    Look,
    /// The meters' scale, the number above them and their colors. Held
    /// back, meters are plain and in Weir's own colors.
    Meters,
    /// The lists under strips and buses: application volumes, and the
    /// strips each bus hears.
    Lists,
    /// One strip, by name. Held back, the window shows only its name; when
    /// the service is held back too ([`SafeMode::audio`]), it is not set
    /// up, so it makes no sound.
    Strip(String),
    /// One bus, by name, as for a strip.
    Bus(String),
    /// Hotkeys: their keys are not taken from the desktop, and they cannot
    /// be pressed.
    Hotkeys,
    /// The user's own hotkey sounds: not read from their files. Weir's
    /// built-in sounds still play.
    HotkeySounds,
    /// External effects: strips and buses play as if they were switched
    /// off.
    ExternalEffects,
    /// The recorder: recordings cannot start.
    Recorder,
    /// Application rules: not applied to applications that start playing.
    AppRules,
    /// The check for a newer Weir.
    UpdateCheck,
}

impl SafeItem {
    /// What the window holds back by itself.
    pub const WINDOW: [SafeItem; 3] = [Self::Look, Self::Meters, Self::Lists];

    /// What only the service holds back, and only from its start.
    pub const SERVICE: [SafeItem; 6] = [
        Self::Hotkeys,
        Self::HotkeySounds,
        Self::ExternalEffects,
        Self::Recorder,
        Self::AppRules,
        Self::UpdateCheck,
    ];

    /// Its name in a list, such as `Strip "Mic"`.
    pub fn label(&self) -> String {
        match self {
            Self::Look => "The window's look".into(),
            Self::Meters => "Meter scales, numbers and colors".into(),
            Self::Lists => "Lists under strips and buses".into(),
            Self::Strip(name) => format!("Strip \"{name}\""),
            Self::Bus(name) => format!("Bus \"{name}\""),
            Self::Hotkeys => "Hotkeys".into(),
            Self::HotkeySounds => "Your own hotkey sounds".into(),
            Self::ExternalEffects => "External effects".into(),
            Self::Recorder => "The recorder".into(),
            Self::AppRules => "App rules".into(),
            Self::UpdateCheck => "Checking for updates".into(),
        }
    }

    /// Its name inside a sentence, such as `the window's look`.
    pub fn in_sentence(&self) -> String {
        let label = self.label();
        let mut chars = label.chars();
        match chars.next() {
            Some(c) => c.to_lowercase().chain(chars).collect(),
            None => String::new(),
        }
    }

    /// What holding it back does, in a line under its name.
    pub fn held_note(&self, audio: bool) -> &'static str {
        match self {
            Self::Look => "Dark, in Weir's own font and colors.",
            Self::Meters => "Plain meters in Weir's own colors.",
            Self::Lists => "No app volumes under strips, no strip lists under buses.",
            Self::Strip(_) | Self::Bus(_) if audio => "Not drawn, and not set up, so silent.",
            Self::Strip(_) | Self::Bus(_) => "Not drawn. It still plays.",
            Self::Hotkeys => "Keys are left to other programs.",
            Self::HotkeySounds => "Not read from their files. Weir's own sounds still play.",
            Self::ExternalEffects => "Strips and buses play as if they were off.",
            Self::Recorder => "Recordings cannot start.",
            Self::AppRules => "Apps that start playing stay where they are.",
            Self::UpdateCheck => "Weir does not look for a newer version.",
        }
    }

    /// Whether the window holds it back by itself.
    pub fn of_window(&self) -> bool {
        Self::WINDOW.contains(self)
    }

    /// Whether only the service holds it back.
    pub fn of_service(&self) -> bool {
        Self::SERVICE.contains(self)
    }
}

/// Why Weir is in safe mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SafeWhy {
    /// Someone asked for it.
    #[default]
    Asked,
    /// The service stopped because of a bug soon after starting or while
    /// in safe mode, or its sound did.
    Service,
    /// The window closed because of a bug soon after opening or while in
    /// safe mode.
    Window,
}

/// What safe mode holds back, if it is on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SafeMode {
    /// Weir is in safe mode. It stays on until it is left
    /// (`leave_safe_mode`) or the service restarts, even with nothing held
    /// back: a crash then still says what came back last.
    pub on: bool,
    /// Why.
    pub why: SafeWhy,
    /// Held-back strips and buses are left out of the sound as well as the
    /// window. True when the service started in safe mode; false when only
    /// the window asked for it, so strips and buses play on.
    pub audio: bool,
    /// What is held back now.
    pub held: Vec<SafeItem>,
    /// What came back last before Weir crashed in safe mode, which is held
    /// back again and likely to blame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suspect: Option<SafeItem>,
    /// What has been brought back, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub brought_back: Vec<SafeItem>,
}

impl SafeMode {
    /// Safe mode as the service starts it: everything in `mixer` held back,
    /// the window's parts and the service's included.
    pub fn everything(why: SafeWhy, mixer: &MixerState, suspect: Option<SafeItem>) -> Self {
        let mut held: Vec<SafeItem> = SafeItem::WINDOW.to_vec();
        held.extend(strips_and_buses(mixer));
        held.extend(SafeItem::SERVICE);
        Self {
            on: true,
            why,
            audio: true,
            held,
            suspect,
            brought_back: Vec::new(),
        }
    }

    /// Safe mode as the window asks for it, after it crashed (`why` is
    /// [`SafeWhy::Window`]) or by hand: its own parts and every strip and
    /// bus held back. Already on, only `suspect` is held back again, since
    /// everything else was fine.
    pub fn enter_for_window(
        &mut self,
        why: SafeWhy,
        mixer: &MixerState,
        suspect: Option<SafeItem>,
    ) {
        if self.on {
            if let Some(s) = &suspect {
                self.hold(s.clone());
            }
            self.suspect = suspect.or(self.suspect.take());
            return;
        }
        let mut held: Vec<SafeItem> = SafeItem::WINDOW.to_vec();
        held.extend(strips_and_buses(mixer));
        *self = Self {
            on: true,
            why,
            audio: false,
            held,
            suspect,
            brought_back: Vec::new(),
        };
    }

    /// Hold `item` back, if it is not already.
    fn hold(&mut self, item: SafeItem) {
        if !self.held.contains(&item) {
            self.held.push(item);
        }
    }

    /// Whether Weir is out of safe mode.
    pub fn is_off(&self) -> bool {
        !self.on
    }

    /// Whether safe mode holds `item` back now.
    pub fn holds(&self, item: &SafeItem) -> bool {
        self.on && self.held.contains(item)
    }

    /// Whether safe mode holds back the strip called `name`.
    pub fn holds_strip(&self, name: &str) -> bool {
        self.on
            && self
                .held
                .iter()
                .any(|i| matches!(i, SafeItem::Strip(n) if n == name))
    }

    /// Whether safe mode holds back the bus called `name`.
    pub fn holds_bus(&self, name: &str) -> bool {
        self.on
            && self
                .held
                .iter()
                .any(|i| matches!(i, SafeItem::Bus(n) if n == name))
    }

    /// Bring `items` back. Returns those that were held.
    pub fn bring_back(&mut self, items: &[SafeItem]) -> Vec<SafeItem> {
        let mut back = Vec::new();
        for item in items {
            if let Some(at) = self.held.iter().position(|h| h == item) {
                back.push(self.held.remove(at));
            }
        }
        for item in &back {
            self.brought_back.retain(|b| b != item);
            self.brought_back.push(item.clone());
            if self.suspect.as_ref() == Some(item) {
                self.suspect = None;
            }
        }
        back
    }

    /// What came back last, for a crash report.
    pub fn last_brought_back(&self) -> Option<&SafeItem> {
        self.brought_back.last()
    }

    /// Follow strips and buses renamed while held back.
    pub fn rename(&mut self, strip: bool, old: &str, new: &str) {
        let fix = |item: &mut SafeItem| match item {
            SafeItem::Strip(n) if strip && n == old => *n = new.to_string(),
            SafeItem::Bus(n) if !strip && n == old => *n = new.to_string(),
            _ => {}
        };
        self.held.iter_mut().for_each(fix);
        self.brought_back.iter_mut().for_each(fix);
        if let Some(s) = &mut self.suspect {
            fix(s);
        }
    }

    /// The mixer the audio engine is to set up: without the strips and
    /// buses held back, and with external effects off while they are.
    /// `None` when that is `mixer` as it is.
    pub fn engine_mixer(&self, mixer: &MixerState) -> Option<MixerState> {
        if !self.on || !self.audio {
            return None;
        }
        let effects = self.holds(&SafeItem::ExternalEffects);
        let strips = mixer.strips.iter().any(|s| self.holds_strip(&s.name));
        let buses = mixer.buses.iter().any(|b| self.holds_bus(&b.name));
        if !effects && !strips && !buses {
            return None;
        }
        let mut m = mixer.clone();
        m.strips.retain(|s| !self.holds_strip(&s.name));
        m.buses.retain(|b| !self.holds_bus(&b.name));
        if effects {
            m.strips.iter_mut().for_each(|s| s.insert.enabled = false);
            m.buses.iter_mut().for_each(|b| b.insert.enabled = false);
        }
        // Routes and sends to buses that are gone, and ducking by strips
        // that are, are cleared the way a removal clears them.
        m.normalize();
        Some(m)
    }

    /// Why Weir is in safe mode, as a sentence.
    pub fn reason(&self) -> String {
        let what = match self.why {
            SafeWhy::Asked => return "Weir is in safe mode, as asked.".into(),
            SafeWhy::Service => "Weir stopped unexpectedly",
            SafeWhy::Window => "Weir's window closed unexpectedly",
        };
        match &self.suspect {
            Some(s) => format!(
                "{what} after {} came back in safe mode, so that stays back for now.",
                s.in_sentence()
            ),
            None => format!("{what} last time, so it is in safe mode."),
        }
    }
}

/// Every strip and bus in `mixer`, as items.
fn strips_and_buses(mixer: &MixerState) -> impl Iterator<Item = SafeItem> + '_ {
    let strips = mixer.strips.iter().map(|s| SafeItem::Strip(s.name.clone()));
    strips.chain(mixer.buses.iter().map(|b| SafeItem::Bus(b.name.clone())))
}

/// Parameters of `enter_safe_mode`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EnterSafeModeParams {
    /// Why: `window` when the window asks after it crashed; `asked`, the
    /// default, otherwise.
    #[serde(skip_serializing_if = "is_asked")]
    pub why: SafeWhy,
    /// What came back last before the window crashed, to hold back again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suspect: Option<SafeItem>,
}

/// Whether `why` is the default, [`SafeWhy::Asked`].
fn is_asked(why: &SafeWhy) -> bool {
    *why == SafeWhy::Asked
}

/// Parameters of `bring_back`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BringBackParams {
    /// What to bring back.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<SafeItem>,
    /// Bring back everything held back.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub all: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bus, BusKind, ChannelLayout, Strip, StripKind};

    fn mixer() -> MixerState {
        let mut m = MixerState::default();
        let mut mic = Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono);
        mic.routes.insert(1);
        mic.routes.insert(2);
        mic.insert.enabled = true;
        m.strips.push(mic);
        m.strips.push(Strip::new(
            2,
            "Music",
            StripKind::Virtual,
            ChannelLayout::Stereo,
        ));
        m.buses.push(Bus::new(
            1,
            "Headset",
            BusKind::Hardware,
            ChannelLayout::Stereo,
        ));
        m.buses.push(Bus::new(
            2,
            "Stream",
            BusKind::Virtual,
            ChannelLayout::Stereo,
        ));
        m
    }

    #[test]
    fn items_read_and_write_as_the_api_says() {
        let items = vec![
            SafeItem::Look,
            SafeItem::Strip("Mic".into()),
            SafeItem::HotkeySounds,
        ];
        let json = serde_json::to_string(&items).unwrap();
        assert_eq!(json, r#"["look",{"strip":"Mic"},"hotkey_sounds"]"#);
        let back: Vec<SafeItem> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, items);
    }

    #[test]
    fn the_engine_is_given_what_is_not_held_back() {
        let m = mixer();
        let mut safe = SafeMode::everything(SafeWhy::Asked, &m, None);
        let e = safe.engine_mixer(&m).unwrap();
        assert!(e.strips.is_empty() && e.buses.is_empty());

        safe.bring_back(&[
            SafeItem::Strip("Mic".into()),
            SafeItem::Bus("Headset".into()),
        ]);
        let e = safe.engine_mixer(&m).unwrap();
        assert_eq!(e.strips.len(), 1);
        let mic = &e.strips[0];
        assert_eq!(
            mic.routes.iter().copied().collect::<Vec<_>>(),
            vec![1],
            "no route to a bus held back"
        );
        assert!(!mic.insert.enabled, "external effects held back");

        safe.bring_back(&[SafeItem::ExternalEffects]);
        assert!(safe.engine_mixer(&m).unwrap().strips[0].insert.enabled);

        safe.bring_back(&[
            SafeItem::Strip("Music".into()),
            SafeItem::Bus("Stream".into()),
        ]);
        assert_eq!(safe.engine_mixer(&m), None, "nothing left to leave out");
        assert_eq!(
            safe.last_brought_back(),
            Some(&SafeItem::Bus("Stream".into()))
        );
    }

    #[test]
    fn the_window_holds_back_only_what_it_draws() {
        let m = mixer();
        let mut safe = SafeMode::default();
        safe.enter_for_window(SafeWhy::Window, &m, None);
        assert!(safe.on && !safe.audio);
        assert!(safe.holds(&SafeItem::Look) && safe.holds_strip("Mic"));
        assert!(!safe.holds(&SafeItem::Hotkeys));
        assert_eq!(safe.engine_mixer(&m), None, "strips play on");

        // In safe mode already: only what it crashed after goes back.
        safe.bring_back(&[SafeItem::Lists, SafeItem::Strip("Mic".into())]);
        safe.enter_for_window(SafeWhy::Window, &m, Some(SafeItem::Lists));
        assert!(safe.holds(&SafeItem::Lists));
        assert!(!safe.holds_strip("Mic"));
        assert!(safe.reason().contains("lists under strips"));
    }

    #[test]
    fn renames_are_followed() {
        let m = mixer();
        let mut safe = SafeMode::everything(SafeWhy::Asked, &m, None);
        safe.rename(true, "Music", "Songs");
        assert!(safe.holds_strip("Songs") && !safe.holds_strip("Music"));
        assert!(safe.holds_bus("Stream"));
        safe.rename(false, "Mic", "Voice");
        assert!(safe.holds_strip("Mic"), "a bus rename leaves strips alone");
    }
}
