//! The simple way to make a hotkey: one thing it does, picked from a list,
//! and turned into steps. A hotkey made this way reads back into the same
//! form; any other opens with all of its options.

use serde_json::{json, Map, Value};
use weir_protocol::*;

/// How often a hotkey that keeps going while held repeats, unless it says.
pub const REPEAT_MS: u32 = 150;

/// `v` as JSON, rounded to what people set: 0.1 rather than the
/// 0.10000000149 an f32 makes, so the file stays readable and the
/// number reads back the same.
fn short(v: f32) -> Value {
    json!((f64::from(v) * 1000.0).round() / 1000.0)
}

/// What a simple hotkey does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Mute,
    PushToTalk,
    VolumeBy,
    VolumeTo,
    Effect,
    Preset,
    Route,
    Solo,
    Scene,
    ShowWindow,
}

impl Action {
    /// What a strip or bus can be made to do.
    pub const ON_ONE: [Action; 8] = [
        Action::Mute,
        Action::PushToTalk,
        Action::VolumeBy,
        Action::VolumeTo,
        Action::Effect,
        Action::Preset,
        Action::Route,
        Action::Solo,
    ];
    /// What the whole mixer can be made to do.
    pub const ON_ALL: [Action; 2] = [Action::Scene, Action::ShowWindow];

    pub fn label(self) -> &'static str {
        match self {
            Action::Mute => "Mute or unmute",
            Action::PushToTalk => "Push to talk (unmuted only while held)",
            Action::VolumeBy => "Turn the volume up or down",
            Action::VolumeTo => "Set the volume",
            Action::Effect => "Switch an effect on or off",
            Action::Preset => "Change the equalizer preset",
            Action::Route => "Send to a bus, or stop",
            Action::Solo => "Solo",
            Action::Scene => "Load a scene",
            Action::ShowWindow => "Bring up the Weir window",
        }
    }

    /// Whether it works on one strip or bus, picked under "On".
    pub fn has_target(self) -> bool {
        !matches!(self, Action::Scene | Action::ShowWindow)
    }

    /// Whether only strips can do it: buses have no solo, and are not
    /// sent anywhere.
    pub fn strips_only(self) -> bool {
        matches!(self, Action::Route | Action::Solo)
    }

    /// Whether it switches something, with a choice of how.
    pub fn has_mode(self) -> bool {
        matches!(
            self,
            Action::Mute | Action::Effect | Action::Route | Action::Solo
        )
    }
}

/// How a switch is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// On if off, off if on.
    Switch,
    On,
    Off,
    /// On while the keys are held, and back as it was when let go.
    WhileHeld,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Switch, Mode::On, Mode::Off, Mode::WhileHeld];

    /// What the choice is called for `action`.
    pub fn label(self, action: Action) -> &'static str {
        match (action, self) {
            (_, Mode::Switch) => "Switch",
            (Action::Mute, Mode::On) => "Mute",
            (Action::Mute, Mode::Off) => "Unmute",
            (Action::Mute, Mode::WhileHeld) => "Mute while held",
            (Action::Route, Mode::On) => "Send",
            (Action::Route, Mode::Off) => "Stop",
            (Action::Route, Mode::WhileHeld) => "Send while held",
            (Action::Solo, Mode::On) => "Solo",
            (Action::Solo, Mode::Off) => "Solo off",
            (Action::Solo, Mode::WhileHeld) => "Solo while held",
            (_, Mode::On) => "On",
            (_, Mode::Off) => "Off",
            (_, Mode::WhileHeld) => "On while held",
        }
    }

    /// The switch's value in a request.
    fn flag(self) -> Value {
        match self {
            Mode::Switch => json!("toggle"),
            Mode::On | Mode::WhileHeld => json!(true),
            Mode::Off => json!(false),
        }
    }

    /// The mode a switch's value and what letting go does mean.
    fn read(v: &Value, release: OnRelease) -> Option<Mode> {
        match (v, release) {
            (Value::String(s), OnRelease::Nothing) if s == "toggle" => Some(Mode::Switch),
            (Value::Bool(true), OnRelease::Nothing) => Some(Mode::On),
            (Value::Bool(false), OnRelease::Nothing) => Some(Mode::Off),
            (Value::Bool(true), OnRelease::Restore) => Some(Mode::WhileHeld),
            _ => None,
        }
    }
}

/// An effect, or another switch a strip or bus has, that a hotkey can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fx {
    Denoise,
    Gate,
    Eq,
    Compressor,
    Ducking,
    Limiter,
    Mono,
    Insert,
}

impl Fx {
    pub const STRIP: [Fx; 6] = [
        Fx::Denoise,
        Fx::Gate,
        Fx::Eq,
        Fx::Compressor,
        Fx::Ducking,
        Fx::Insert,
    ];
    pub const BUS: [Fx; 4] = [Fx::Eq, Fx::Limiter, Fx::Mono, Fx::Insert];

    pub fn label(self) -> &'static str {
        match self {
            Fx::Denoise => "Noise suppression",
            Fx::Gate => "Noise gate",
            Fx::Eq => "Equalizer",
            Fx::Compressor => "Compressor",
            Fx::Ducking => "Ducking",
            Fx::Limiter => "Safety limiter",
            Fx::Mono => "Mono",
            Fx::Insert => "External effects",
        }
    }

    /// Its field in `set_strip` or `set_bus`.
    fn key(self) -> &'static str {
        match self {
            Fx::Denoise => "denoise",
            Fx::Gate => "gate",
            Fx::Eq => "eq",
            Fx::Compressor => "compressor",
            Fx::Ducking => "ducking",
            Fx::Limiter => "limiter",
            Fx::Mono => "mono",
            Fx::Insert => "insert",
        }
    }

    /// Those `target` has.
    pub fn for_target(target: StripOrBus) -> &'static [Fx] {
        match target {
            StripOrBus::Strip(_) => &Fx::STRIP,
            StripOrBus::Bus(_) => &Fx::BUS,
        }
    }

    fn from_key(key: &str) -> Option<Fx> {
        [
            Fx::Denoise,
            Fx::Gate,
            Fx::Eq,
            Fx::Compressor,
            Fx::Ducking,
            Fx::Limiter,
            Fx::Mono,
            Fx::Insert,
        ]
        .into_iter()
        .find(|f| f.key() == key)
    }
}

/// A hotkey that does one thing, as the simple editor shows it. Fields
/// the chosen action does not use keep their values, so trying another
/// action and coming back loses nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct Simple {
    pub action: Action,
    /// The strip or bus it works on.
    pub target: StripOrBus,
    pub mode: Mode,
    pub fx: Fx,
    /// For `Route`: the bus.
    pub bus: BusId,
    /// For the volume on a strip: its level in this bus's mix rather than
    /// its own fader.
    pub send: Option<BusId>,
    /// For `VolumeBy`: up or down, and by how much.
    pub up: bool,
    pub amount_db: f32,
    /// For `VolumeBy`: keep going while held, this often.
    pub repeat: bool,
    pub repeat_ms: u32,
    /// For `VolumeTo`: the level, and whether letting go puts it back.
    pub level_db: f32,
    pub put_back: bool,
    pub preset: String,
    pub scene: String,
    /// For `PushToTalk`: mute when let go, rather than put the mute back
    /// as it was.
    pub mute_after: bool,
}

impl Simple {
    /// A mute switch for `target`, with the other choices ready for
    /// `state`'s buses, presets and scenes.
    pub fn new(target: StripOrBus, state: &FullState) -> Simple {
        let bus = state.mixer.buses.first().map_or(0, |b| b.id);
        Simple {
            action: Action::Mute,
            target,
            mode: Mode::Switch,
            fx: Fx::Eq,
            bus,
            send: None,
            up: false,
            amount_db: 2.0,
            repeat: true,
            repeat_ms: REPEAT_MS,
            level_db: -20.0,
            put_back: false,
            preset: state
                .eq_presets
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_default(),
            scene: state.library.scenes.first().cloned().unwrap_or_default(),
            mute_after: true,
        }
    }

    /// The first strip, or the first bus when there are no strips.
    pub fn first_target(state: &FullState) -> StripOrBus {
        match (state.mixer.strips.first(), state.mixer.buses.first()) {
            (Some(s), _) => StripOrBus::Strip(s.id),
            (None, Some(b)) => StripOrBus::Bus(b.id),
            (None, None) => StripOrBus::Strip(0),
        }
    }

    /// This, made to `action`.
    pub fn with(mut self, action: Action) -> Simple {
        self.action = action;
        self
    }

    /// The target fitted to the action: a strip for strips-only actions,
    /// and an effect the target has.
    pub fn fitted(mut self, state: &FullState) -> Simple {
        if self.action.strips_only() && matches!(self.target, StripOrBus::Bus(_)) {
            if let Some(s) = state.mixer.strips.first() {
                self.target = StripOrBus::Strip(s.id);
            }
        }
        if !Fx::for_target(self.target).contains(&self.fx) {
            self.fx = Fx::Eq;
        }
        if matches!(self.target, StripOrBus::Bus(_)) {
            self.send = None;
        }
        if self.action == Action::PushToTalk || !self.action.has_mode() {
            self.mode = Mode::Switch;
        }
        self
    }

    /// The request for the target: `set_strip` or `set_bus`, and its id.
    fn on_target(&self, field: &str, value: Value) -> HotkeyStep {
        let (method, id) = match self.target {
            StripOrBus::Strip(id) => ("set_strip", id),
            StripOrBus::Bus(id) => ("set_bus", id),
        };
        let mut params = Map::new();
        params.insert("id".into(), json!(id));
        params.insert(field.into(), value);
        HotkeyStep::new(method, Value::Object(params))
    }

    /// A route request for the target strip and `bus`.
    fn on_route(&self, bus: BusId, field: &str, value: Value) -> HotkeyStep {
        let strip = match self.target {
            StripOrBus::Strip(id) => id,
            StripOrBus::Bus(_) => 0,
        };
        let mut params = Map::new();
        params.insert("strip".into(), json!(strip));
        params.insert("bus".into(), json!(bus));
        params.insert(field.into(), value);
        HotkeyStep::new("set_route", Value::Object(params))
    }

    /// The step this does when pressed.
    pub fn step(&self) -> HotkeyStep {
        let flag = self.mode.flag();
        match self.action {
            Action::Mute => self.on_target("mute", flag),
            Action::PushToTalk => self.on_target("mute", json!(false)),
            Action::Solo => self.on_target("solo", flag),
            Action::Effect if self.fx == Fx::Mono => self.on_target("mono", flag),
            Action::Effect => self.on_target(self.fx.key(), json!({ "enabled": flag })),
            Action::Route => self.on_route(self.bus, "enabled", flag),
            Action::VolumeBy => {
                let by = if self.up {
                    self.amount_db
                } else {
                    -self.amount_db
                };
                match self.send {
                    Some(bus) => self.on_route(bus, "level_delta_db", short(by)),
                    None => self.on_target("gain_delta_db", short(by)),
                }
            }
            Action::VolumeTo => match self.send {
                Some(bus) => self.on_route(bus, "level_db", short(self.level_db)),
                None => self.on_target("gain_db", short(self.level_db)),
            },
            Action::Preset => {
                let field = match self.target {
                    StripOrBus::Strip(_) => "strip",
                    StripOrBus::Bus(_) => "bus",
                };
                let id = match self.target {
                    StripOrBus::Strip(id) | StripOrBus::Bus(id) => id,
                };
                HotkeyStep::new("apply_eq_preset", json!({ "name": self.preset, field: id }))
            }
            Action::Scene => HotkeyStep::new("load_scene", json!({ "name": self.scene })),
            Action::ShowWindow => HotkeyStep::new("show_window", Value::Null),
        }
    }

    /// Set `h` to do this: its steps, and what holding and letting go do.
    /// Its name, keys and whether it is on stay as they are.
    pub fn apply(&self, h: &mut Hotkey) {
        h.steps = vec![self.step()];
        h.each_press = EachPress::All;
        h.release_steps = Vec::new();
        h.repeat_ms = None;
        h.on_release = OnRelease::Nothing;
        match self.action {
            Action::PushToTalk if self.mute_after => {
                h.on_release = OnRelease::Steps;
                h.release_steps = vec![self.on_target("mute", json!(true))];
            }
            Action::PushToTalk => h.on_release = OnRelease::Restore,
            Action::VolumeBy if self.repeat => h.repeat_ms = Some(self.repeat_ms),
            Action::VolumeTo if self.put_back => h.on_release = OnRelease::Restore,
            _ if self.action.has_mode() && self.mode == Mode::WhileHeld => {
                h.on_release = OnRelease::Restore;
            }
            _ => {}
        }
    }

    /// A hotkey that does this, with `name` and `keys`.
    pub fn hotkey(&self, name: &str, keys: &[String]) -> Hotkey {
        let mut h = Hotkey {
            id: 0,
            name: name.to_string(),
            enabled: true,
            group: 0,
            keys: keys.to_vec(),
            steps: Vec::new(),
            each_press: EachPress::All,
            on_release: OnRelease::Nothing,
            release_steps: Vec::new(),
            repeat_ms: None,
            sounds: HotkeySounds::default(),
            popup: true,
        };
        self.apply(&mut h);
        h
    }

    /// A short name for a hotkey doing this, such as "Mute Mic" or "Music
    /// down", for when none is typed.
    pub fn name(&self, mixer: &MixerState) -> String {
        let t = match self.target {
            StripOrBus::Strip(id) => mixer.strip(id).map(|s| s.name.clone()),
            StripOrBus::Bus(id) => mixer.bus(id).map(|b| b.name.clone()),
        }
        .unwrap_or_default();
        let bus = |id: BusId| mixer.bus(id).map(|b| b.name.clone()).unwrap_or_default();
        let switched = |what: &str| match self.mode {
            Mode::Switch => format!("{what} on or off"),
            Mode::On => format!("{what} on"),
            Mode::Off => format!("{what} off"),
            Mode::WhileHeld => format!("{what} while held"),
        };
        let level = |v: f32| {
            let v = (v * 10.0).round() / 10.0;
            if v.fract() == 0.0 {
                format!("{v:.0} dB")
            } else {
                format!("{v:.1} dB")
            }
        };
        let in_mix = match self.send {
            Some(b) => format!(" in {}", bus(b)),
            None => String::new(),
        };
        match self.action {
            Action::Mute => match self.mode {
                Mode::Switch => format!("Mute {t} on or off"),
                Mode::On => format!("Mute {t}"),
                Mode::Off => format!("Unmute {t}"),
                Mode::WhileHeld => format!("Mute {t} while held"),
            },
            Action::PushToTalk => format!("Push to talk on {t}"),
            Action::VolumeBy => {
                format!("{t}{in_mix} {}", if self.up { "up" } else { "down" })
            }
            Action::VolumeTo => format!("{t}{in_mix} to {}", level(self.level_db)),
            Action::Effect => switched(&format!("{t} {}", self.fx.label().to_lowercase())),
            Action::Preset => format!("{t} preset {}", self.preset),
            Action::Route => switched(&format!("{t} to {}", bus(self.bus))),
            Action::Solo => switched(&format!("Solo {t}")),
            Action::Scene => format!("Scene {}", self.scene),
            Action::ShowWindow => "Show Weir".into(),
        }
    }

    /// Whether `other` works the same control as this: the same mute,
    /// fader, routing button, effect or solo, so a control's menu can list
    /// the hotkeys already on it.
    pub fn same_control(&self, other: &Simple) -> bool {
        use Action::*;
        self.target == other.target
            && match (self.action, other.action) {
                (Mute | PushToTalk, Mute | PushToTalk) | (Solo, Solo) => true,
                (VolumeBy | VolumeTo, VolumeBy | VolumeTo) => self.send == other.send,
                (Route, Route) => self.bus == other.bus,
                (Effect, Effect) => self.fx == other.fx,
                _ => false,
            }
    }

    /// `h` in the simple form, starting from `base` for what it does not
    /// say, or `None` when it does more than one simple thing, or works on
    /// a strip or bus that `mixer` does not have.
    pub fn read(h: &Hotkey, base: &Simple, mixer: &MixerState) -> Option<Simple> {
        // The form picks strips and buses of the mixer now, by id.
        let mut by_id = h.clone();
        by_id.targets_by_id(mixer);
        let h = &by_id;
        let [step] = h.steps.as_slice() else {
            return None;
        };
        if h.each_press != EachPress::All || step.over_ms.is_some() {
            return None;
        }
        let mut s = base.clone();
        let params = step.params.as_object();
        let num = |key: &str| params.and_then(|p| p.get(key)).and_then(Value::as_u64);
        let f32_of = |key: &str| {
            params
                .and_then(|p| p.get(key))
                .and_then(Value::as_f64)
                .map(|v| v as f32)
        };
        // The one field set besides the strip, bus or route it is about.
        let field = |ids: &[&str]| -> Option<(String, Value)> {
            let p = params?;
            let mut rest = p.iter().filter(|(k, _)| !ids.contains(&k.as_str()));
            let (k, v) = rest.next()?;
            rest.next().is_none().then(|| (k.clone(), v.clone()))
        };
        let repeat_ok = h.repeat_ms.is_none();
        match step.method.as_str() {
            "set_strip" | "set_bus" => {
                let id = num("id")? as u32;
                s.target = if step.method == "set_strip" {
                    StripOrBus::Strip(id)
                } else {
                    StripOrBus::Bus(id)
                };
                s.send = None;
                let (key, value) = field(&["id"])?;
                match key.as_str() {
                    "mute" => match (&value, h.on_release) {
                        (Value::Bool(false), OnRelease::Restore) if repeat_ok => {
                            s.action = Action::PushToTalk;
                            s.mute_after = false;
                        }
                        (Value::Bool(false), OnRelease::Steps) if repeat_ok => {
                            let after = s.on_target("mute", json!(true));
                            if h.release_steps != [after] {
                                return None;
                            }
                            s.action = Action::PushToTalk;
                            s.mute_after = true;
                        }
                        _ => {
                            s.action = Action::Mute;
                            s.mode = Mode::read(&value, h.on_release).filter(|_| repeat_ok)?;
                        }
                    },
                    "solo" if step.method == "set_strip" => {
                        s.action = Action::Solo;
                        s.mode = Mode::read(&value, h.on_release).filter(|_| repeat_ok)?;
                    }
                    "gain_delta_db" if h.on_release == OnRelease::Nothing => {
                        let by = f32_of("gain_delta_db")?;
                        s.action = Action::VolumeBy;
                        s.up = by > 0.0;
                        s.amount_db = by.abs();
                        s.repeat = h.repeat_ms.is_some();
                        s.repeat_ms = h.repeat_ms.unwrap_or(REPEAT_MS);
                    }
                    "gain_db" if repeat_ok => {
                        s.action = Action::VolumeTo;
                        s.level_db = f32_of("gain_db")?;
                        s.put_back = match h.on_release {
                            OnRelease::Nothing => false,
                            OnRelease::Restore => true,
                            OnRelease::Steps => return None,
                        };
                    }
                    _ => {
                        let fx =
                            Fx::from_key(&key).filter(|f| Fx::for_target(s.target).contains(f))?;
                        let flag = if fx == Fx::Mono {
                            value
                        } else {
                            let o = value.as_object()?;
                            if o.len() != 1 {
                                return None;
                            }
                            o.get("enabled")?.clone()
                        };
                        s.action = Action::Effect;
                        s.fx = fx;
                        s.mode = Mode::read(&flag, h.on_release).filter(|_| repeat_ok)?;
                    }
                }
            }
            "set_route" => {
                s.target = StripOrBus::Strip(num("strip")? as u32);
                let bus = num("bus")? as u32;
                let (key, value) = field(&["strip", "bus"])?;
                match key.as_str() {
                    "enabled" => {
                        s.action = Action::Route;
                        s.bus = bus;
                        s.mode = Mode::read(&value, h.on_release).filter(|_| repeat_ok)?;
                    }
                    "level_delta_db" if h.on_release == OnRelease::Nothing => {
                        let by = value.as_f64()? as f32;
                        s.action = Action::VolumeBy;
                        s.send = Some(bus);
                        s.up = by > 0.0;
                        s.amount_db = by.abs();
                        s.repeat = h.repeat_ms.is_some();
                        s.repeat_ms = h.repeat_ms.unwrap_or(REPEAT_MS);
                    }
                    "level_db" if repeat_ok => {
                        s.action = Action::VolumeTo;
                        s.send = Some(bus);
                        s.level_db = value.as_f64()? as f32;
                        s.put_back = match h.on_release {
                            OnRelease::Nothing => false,
                            OnRelease::Restore => true,
                            OnRelease::Steps => return None,
                        };
                    }
                    _ => return None,
                }
            }
            "apply_eq_preset" if h.on_release == OnRelease::Nothing && repeat_ok => {
                s.action = Action::Preset;
                s.preset = params?.get("name")?.as_str()?.to_string();
                s.target = match (num("strip"), num("bus")) {
                    (Some(id), None) => StripOrBus::Strip(id as u32),
                    (None, Some(id)) => StripOrBus::Bus(id as u32),
                    _ => return None,
                };
            }
            "load_scene" if h.on_release == OnRelease::Nothing && repeat_ok => {
                s.action = Action::Scene;
                s.scene = params?.get("name")?.as_str()?.to_string();
            }
            "show_window" if h.on_release == OnRelease::Nothing && repeat_ok => {
                s.action = Action::ShowWindow;
            }
            _ => return None,
        }
        // Only push to talk that mutes when let go has steps for letting go.
        let mutes_after = s.action == Action::PushToTalk && s.mute_after;
        if h.release_steps.is_empty() == mutes_after {
            return None;
        }
        // Only what this form would make again.
        (s.hotkey(&h.name, &h.keys).steps == h.steps).then_some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> FullState {
        let mut st = FullState::default();
        st.mixer.strips = vec![
            Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono),
            Strip::new(2, "Music", StripKind::Virtual, ChannelLayout::Stereo),
        ];
        st.mixer.buses = vec![
            Bus::new(3, "Headset", BusKind::Hardware, ChannelLayout::Stereo),
            Bus::new(4, "Stream Mic", BusKind::Virtual, ChannelLayout::Stereo),
        ];
        st.library.scenes = vec!["Gaming".into()];
        st
    }

    /// Every choice the form offers comes back the same from the hotkey it
    /// makes, so editing a hotkey opens it as it was made.
    #[test]
    fn every_simple_hotkey_reads_back_as_it_was_made() {
        let st = state();
        let base = Simple::new(StripOrBus::Strip(1), &st);
        let mut cases = Vec::new();
        for action in Action::ON_ONE.into_iter().chain(Action::ON_ALL) {
            for target in [StripOrBus::Strip(1), StripOrBus::Bus(4)] {
                let s = Simple {
                    target,
                    preset: "Warm voice".into(),
                    ..base.clone()
                }
                .with(action)
                .fitted(&st);
                let modes: &[Mode] = if action.has_mode() {
                    &Mode::ALL
                } else {
                    &[Mode::Switch]
                };
                for &mode in modes {
                    for fx in Fx::for_target(s.target) {
                        cases.push(Simple {
                            mode,
                            fx: *fx,
                            ..s.clone()
                        });
                    }
                }
            }
        }
        cases.push(Simple {
            mute_after: false,
            ..base.clone().with(Action::PushToTalk)
        });
        cases.push(Simple {
            up: true,
            amount_db: 0.1,
            repeat: false,
            ..base.clone().with(Action::VolumeBy)
        });
        cases.push(Simple {
            level_db: -7.3,
            ..base.clone().with(Action::VolumeTo)
        });
        cases.push(Simple {
            send: Some(4),
            ..base.clone().with(Action::VolumeBy)
        });
        cases.push(Simple {
            send: Some(3),
            put_back: true,
            ..base.clone().with(Action::VolumeTo)
        });
        for s in cases {
            let h = s.hotkey("Test", &[]);
            // As the daemon keeps it: by name.
            let mut kept = h.clone();
            kept.targets_by_name(&st.mixer);
            let back = Simple::read(&kept, &base, &st.mixer)
                .unwrap_or_else(|| panic!("{s:?} did not read back"));
            assert_eq!(back.hotkey("Test", &[]), h, "{s:?}");
            assert_eq!(back.action, s.action);
        }
    }

    #[test]
    fn push_to_talk_mutes_when_let_go_or_puts_it_back() {
        let st = state();
        let s = Simple::new(StripOrBus::Strip(1), &st).with(Action::PushToTalk);
        let h = s.hotkey("Talk", &["F9".into()]);
        assert_eq!(h.steps[0].params, json!({"id": 1, "mute": false}));
        assert_eq!(h.on_release, OnRelease::Steps);
        assert_eq!(h.release_steps[0].params, json!({"id": 1, "mute": true}));
        let h = Simple {
            mute_after: false,
            ..s
        }
        .hotkey("Talk", &[]);
        assert_eq!(h.on_release, OnRelease::Restore);
        assert!(h.release_steps.is_empty());
    }

    #[test]
    fn a_strips_send_level_goes_through_its_route() {
        let st = state();
        let s = Simple {
            send: Some(4),
            ..Simple::new(StripOrBus::Strip(2), &st).with(Action::VolumeBy)
        };
        let h = s.hotkey("Music down in stream", &[]);
        assert_eq!(h.steps[0].method, "set_route");
        assert_eq!(
            h.steps[0].params,
            json!({"strip": 2, "bus": 4, "level_delta_db": -2.0})
        );
        assert_eq!(h.repeat_ms, Some(REPEAT_MS));
    }

    #[test]
    fn hotkeys_doing_more_open_with_all_their_options() {
        let st = state();
        let base = Simple::new(StripOrBus::Strip(1), &st);
        let mut two = base.hotkey("Two", &[]);
        two.steps.push(two.steps[0].clone());
        assert!(Simple::read(&two, &base, &st.mixer).is_none());
        let mut faded = Simple { ..base.clone() }
            .with(Action::VolumeTo)
            .hotkey("Fade", &[]);
        faded.steps[0].over_ms = Some(300);
        assert!(Simple::read(&faded, &base, &st.mixer).is_none());
        // A strip this mixer does not have, in another setup.
        let mut elsewhere = base.hotkey("Elsewhere", &[]);
        elsewhere.steps[0].params = json!({"id": "Guitar", "mute": "toggle"});
        assert!(Simple::read(&elsewhere, &base, &st.mixer).is_none());
        let mut cycling = base.hotkey("Next", &[]);
        cycling.each_press = EachPress::Next;
        assert!(Simple::read(&cycling, &base, &st.mixer).is_none());
    }

    #[test]
    fn names_made_up_are_short() {
        let st = state();
        let s = Simple::new(StripOrBus::Strip(1), &st);
        assert_eq!(s.name(&st.mixer), "Mute Mic on or off");
        assert_eq!(
            s.clone().with(Action::PushToTalk).name(&st.mixer),
            "Push to talk on Mic"
        );
        let down = Simple {
            send: Some(4),
            target: StripOrBus::Strip(2),
            ..s.clone()
        };
        assert_eq!(
            down.with(Action::VolumeBy).name(&st.mixer),
            "Music in Stream Mic down"
        );
        let route = Simple {
            bus: 3,
            mode: Mode::On,
            target: StripOrBus::Strip(2),
            ..s
        };
        assert_eq!(
            route.with(Action::Route).name(&st.mixer),
            "Music to Headset on"
        );
    }
}
