//! Doing what hotkeys say when their keys go down and come up.
//!
//! The runner is one task with a mailbox: key presses arrive from the keys
//! (see [`super::keys`]), from clients through the controller, and from its
//! own timers, for repeats and fades. It keeps what lasts beyond one
//! command: which hotkeys are held, where each "next step" hotkey has got
//! to, which fades are running, and each press until it is over.
//!
//! A press is one step in the undo history, however many changes it made:
//! from the mixer before the keys went down to the mixer once they are
//! back up and its fades have finished. A push to talk that puts
//! everything back leaves nothing to undo at all.

use super::restore;
use crate::controller::Controller;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::AbortHandle;
use tracing::{debug, warn};
use weir_protocol::*;

/// What the runner is told.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// A hotkey's keys went down.
    Press(HotkeyId),
    /// A hotkey's keys came up.
    Release(HotkeyId),
    /// A tap: down and up at once.
    Run(HotkeyId),
    /// The hotkeys were changed.
    Changed,
    /// Time for a held hotkey to repeat, for the press numbered so.
    Repeat(HotkeyId, u64),
    /// A fade of the press numbered so came to its end.
    FadeDone(u64, FadeKey),
}

/// What a fade changes. A new fade of the same thing stops the old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FadeKey {
    /// A strip's fader.
    Strip(StripId),
    /// A bus's fader.
    Bus(BusId),
    /// A strip's level in a bus's mix.
    Route(StripId, BusId),
}

/// How long a held hotkey waits before its first repeat, unless it repeats
/// more slowly than that: like a keyboard's own repeat.
const REPEAT_DELAY: Duration = Duration::from_millis(400);
/// How often a fade moves.
const FADE_TICK: Duration = Duration::from_millis(20);

/// One press of a hotkey, from the keys going down until it is over.
struct Press {
    /// What it is called in the undo history.
    label: String,
    /// The mixer before the keys went down.
    before: MixerState,
    /// The mixer right after the hotkey's own latest changes.
    own_after: MixerState,
    /// What its fades change, which goes on changing after `own_after`.
    fade_paths: Vec<restore::Path>,
    /// Whether the keys are still down.
    held: bool,
    /// How many of its fades are still running.
    fades: usize,
    /// Whether it gets a step in the undo history. Not when it undoes or
    /// redoes, which moves through the history itself.
    record: bool,
    /// Whether it loads a setup, putting another mixer in place.
    whole: bool,
    /// When the keys went down.
    at: Instant,
}

/// A hotkey whose keys are down.
struct Held {
    /// Its press.
    press: u64,
    /// Its repeat timer, when it repeats.
    repeat: Option<AbortHandle>,
}

/// The runner's state.
pub struct Runner {
    controller: Arc<Controller>,
    /// Its own mailbox, for its timers.
    tx: UnboundedSender<Command>,
    presses: HashMap<u64, Press>,
    next_press: u64,
    held: HashMap<HotkeyId, Held>,
    /// For hotkeys that do the next step each press: the step next time.
    cycle: HashMap<HotkeyId, usize>,
    fades: HashMap<FadeKey, (u64, AbortHandle)>,
}

/// `state` as JSON, for comparing.
fn json(state: &MixerState) -> Value {
    serde_json::to_value(state).expect("the mixer serializes")
}

/// What a fading step changes, where it starts from now, where it ends,
/// and where in the mixer it is.
fn fade_target(step: &HotkeyStep, m: &MixerState) -> Option<(FadeKey, f64, f64, restore::Path)> {
    use restore::Seg;
    let p = &step.params;
    let id = |k: &str| p.get(k).and_then(Value::as_u64).map(|v| v as u32);
    let num = |k: &str| p.get(k).and_then(Value::as_f64);
    match step.method.as_str() {
        "set_strip" => {
            let id = id("id")?;
            let start = f64::from(m.strip(id)?.gain_db);
            let path = vec![
                Seg::Key("strips".into()),
                Seg::Id(id.into()),
                Seg::Key("gain_db".into()),
            ];
            Some((FadeKey::Strip(id), start, num("gain_db")?, path))
        }
        "set_bus" => {
            let id = id("id")?;
            let start = f64::from(m.bus(id)?.gain_db);
            let path = vec![
                Seg::Key("buses".into()),
                Seg::Id(id.into()),
                Seg::Key("gain_db".into()),
            ];
            Some((FadeKey::Bus(id), start, num("gain_db")?, path))
        }
        "set_route" => {
            let (strip, bus) = (id("strip")?, id("bus")?);
            let start = f64::from(m.strip(strip)?.send_db(bus));
            let path = vec![
                Seg::Key("strips".into()),
                Seg::Id(strip.into()),
                Seg::Key("sends".into()),
                Seg::Key(bus.to_string()),
            ];
            Some((FadeKey::Route(strip, bus), start, num("level_db")?, path))
        }
        _ => None,
    }
}

/// The field a fading step moves.
fn fade_field(step: &HotkeyStep) -> &'static str {
    if step.method == "set_route" {
        "level_db"
    } else {
        "gain_db"
    }
}

impl Runner {
    /// A runner for `controller`, whose mailbox `tx` sends to.
    pub fn new(controller: Arc<Controller>, tx: UnboundedSender<Command>) -> Self {
        Self {
            controller,
            tx,
            presses: HashMap::new(),
            next_press: 1,
            held: HashMap::new(),
            cycle: HashMap::new(),
            fades: HashMap::new(),
        }
    }

    /// Act on one command.
    pub fn handle(&mut self, command: Command) {
        match command {
            Command::Press(id) => self.press(id),
            Command::Release(id) => self.release(id),
            Command::Run(id) => {
                self.press(id);
                self.release(id);
            }
            Command::Changed => self.changed(),
            Command::Repeat(id, press) => self.repeat(id, press),
            Command::FadeDone(press, key) => self.fade_done(press, key),
        }
    }

    /// The steps this press of `h` does.
    fn pick_steps(&mut self, h: &Hotkey) -> Vec<HotkeyStep> {
        match h.each_press {
            EachPress::All => h.steps.clone(),
            EachPress::Next if h.steps.is_empty() => Vec::new(),
            EachPress::Next => {
                let at = self.cycle.entry(h.id).or_default();
                let i = *at % h.steps.len();
                *at = i + 1;
                vec![h.steps[i].clone()]
            }
        }
    }

    fn press(&mut self, id: HotkeyId) {
        // Already down: a keyboard repeating the key, or two ways of
        // pressing the same hotkey.
        if self.held.contains_key(&id) {
            return;
        }
        let Some(h) = self.controller.hotkey(id) else {
            return;
        };
        debug!("hotkey '{}' pressed", h.name);
        let before = self.controller.mixer();
        let n = self.next_press;
        self.next_press += 1;
        let record = !h
            .all_steps()
            .any(|s| s.method == "undo" || s.method == "redo");
        let whole = h.all_steps().any(|s| s.method == "load_setup");
        self.presses.insert(
            n,
            Press {
                label: format!("{} (hotkey)", h.name),
                own_after: before.clone(),
                before,
                fade_paths: Vec::new(),
                held: false,
                fades: 0,
                record,
                whole,
                at: Instant::now(),
            },
        );
        let steps = self.pick_steps(&h);
        self.do_steps(n, &steps);
        if h.acts_on_release() {
            let repeat = h.repeat_ms.map(|ms| {
                let every = Duration::from_millis(ms.into());
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(every.max(REPEAT_DELAY)).await;
                    loop {
                        if tx.send(Command::Repeat(id, n)).is_err() {
                            break;
                        }
                        tokio::time::sleep(every).await;
                    }
                })
                .abort_handle()
            });
            self.held.insert(id, Held { press: n, repeat });
            if let Some(p) = self.presses.get_mut(&n) {
                p.held = true;
            }
        }
        self.maybe_finish(n);
    }

    fn release(&mut self, id: HotkeyId) {
        let Some(held) = self.held.remove(&id) else {
            return;
        };
        if let Some(r) = held.repeat {
            r.abort();
        }
        let n = held.press;
        // Gone meanwhile: the press just ends.
        if let Some(h) = self.controller.hotkey(id) {
            debug!("hotkey '{}' let go", h.name);
            match h.on_release {
                OnRelease::Nothing => {}
                OnRelease::Restore => self.put_back(n),
                OnRelease::Steps => {
                    let steps = h.release_steps.clone();
                    self.do_steps(n, &steps);
                }
            }
        }
        if let Some(p) = self.presses.get_mut(&n) {
            p.held = false;
        }
        self.maybe_finish(n);
    }

    fn repeat(&mut self, id: HotkeyId, n: u64) {
        if self.held.get(&id).is_none_or(|h| h.press != n) {
            return;
        }
        let Some(h) = self.controller.hotkey(id) else {
            return;
        };
        let steps = self.pick_steps(&h);
        self.do_steps(n, &steps);
    }

    /// The hotkeys changed: forget where removed ones had got to, and let
    /// go of any held one that is gone.
    fn changed(&mut self) {
        let ids: Vec<HotkeyId> = self.controller.hotkeys().iter().map(|h| h.id).collect();
        self.cycle.retain(|id, _| ids.contains(id));
        let gone: Vec<HotkeyId> = self
            .held
            .keys()
            .filter(|id| !ids.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.release(id);
        }
    }

    /// Do `steps` for press `n`, starting fades for those that fade. Each
    /// step's strips and buses are looked up by name just before it runs,
    /// after the steps before it, which may have loaded a setup.
    fn do_steps(&mut self, n: u64, steps: &[HotkeyStep]) {
        for step in steps {
            let step = match self.controller.step_by_id(step) {
                Ok(step) => step,
                Err(e) => {
                    warn!("a hotkey's step {} failed: {}", step.method, e.message);
                    continue;
                }
            };
            if step.over_ms.is_some() {
                self.start_fade(n, &step);
            } else if let Err(e) = self.controller.run_step(&step) {
                warn!("a hotkey's step {} failed: {}", step.method, e.message);
            }
        }
        let now = self.controller.mixer();
        if let Some(p) = self.presses.get_mut(&n) {
            p.own_after = now;
        }
    }

    /// The places press `n` changed so far, and the mixer from before it.
    fn own_changes(&self, n: u64) -> Option<(Vec<restore::Path>, Value)> {
        let p = self.presses.get(&n)?;
        let before = json(&p.before);
        let mut paths = restore::diff(&before, &json(&p.own_after));
        paths.extend(p.fade_paths.iter().cloned());
        Some((paths, before))
    }

    /// Set back what press `n` changed, and only that. Steps others made
    /// in the undo history meanwhile hold the hotkey's change in the state
    /// they would go back to; that is set back in them too, so undoing a
    /// fader moved while talking does not open the microphone again.
    fn put_back(&mut self, n: u64) {
        self.stop_fades_of(n);
        let Some((paths, before)) = self.own_changes(n) else {
            return;
        };
        let back = |state: &MixerState| -> Option<MixerState> {
            serde_json::from_value(restore::restore(&json(state), &before, &paths)).ok()
        };
        match back(&self.controller.mixer()) {
            Some(m) => self.controller.set_mixer_quietly(m),
            None => warn!("could not put back what a hotkey changed"),
        }
        if let Some(at) = self.presses.get(&n).map(|p| p.at) {
            self.controller
                .rewrite_history_since(at, |s| back(s).unwrap_or_else(|| s.clone()));
        }
    }

    /// End press `n` once its keys are up and its fades done: one step in
    /// the undo history for all it changed.
    fn maybe_finish(&mut self, n: u64) {
        let done = self
            .presses
            .get(&n)
            .is_some_and(|p| !p.held && p.fades == 0);
        if !done {
            return;
        }
        let own = self.own_changes(n);
        let p = self.presses.remove(&n).expect("checked above");
        if !p.record {
            return;
        }
        // Only the hotkey's own changes: anything else changed while it
        // was held has a step of its own.
        let after = self.controller.mixer();
        let from = own.and_then(|(paths, before)| {
            serde_json::from_value(restore::restore(&json(&after), &before, &paths)).ok()
        });
        self.controller
            .record_change(p.label, &from.unwrap_or(p.before), &after, p.whole);
    }

    /// Fade as `step` says, for press `n`. Anything else the step changes
    /// changes at once.
    fn start_fade(&mut self, n: u64, step: &HotkeyStep) {
        let mixer = self.controller.mixer();
        let Some((key, start, end, path)) = fade_target(step, &mixer) else {
            // Nothing to fade, such as a strip that is gone: let the step
            // say why.
            let mut now = step.clone();
            now.over_ms = None;
            if let Err(e) = self.controller.run_step(&now) {
                warn!("a hotkey's step {} failed: {}", step.method, e.message);
            }
            return;
        };
        let field = fade_field(step);
        // The rest of the step, at once.
        let ids = ["id", "strip", "bus"];
        if let Value::Object(map) = &step.params {
            if map.keys().any(|k| k != field && !ids.contains(&k.as_str())) {
                let mut rest = step.clone();
                rest.over_ms = None;
                if let Some(m) = rest.params.as_object_mut() {
                    m.remove(field);
                }
                if let Err(e) = self.controller.run_step(&rest) {
                    warn!("a hotkey's step {} failed: {}", step.method, e.message);
                }
            }
        }
        if let Some((old, handle)) = self.fades.remove(&key) {
            handle.abort();
            self.fade_ended(old);
        }
        let total = Duration::from_millis(step.over_ms.unwrap_or(0).into());
        let mut template = step.clone();
        template.over_ms = None;
        let controller = self.controller.clone();
        let tx = self.tx.clone();
        let task = tokio::spawn(async move {
            let t0 = Instant::now();
            let mut tick = tokio::time::interval(FADE_TICK);
            loop {
                tick.tick().await;
                let f = if total.is_zero() {
                    1.0
                } else {
                    (t0.elapsed().as_secs_f64() / total.as_secs_f64()).min(1.0)
                };
                let mut s = template.clone();
                if let Some(m) = s.params.as_object_mut() {
                    m.retain(|k, _| k == field || ids.contains(&k.as_str()));
                    m.insert(field.into(), Value::from(start + (end - start) * f));
                }
                if let Err(e) = controller.run_step(&s) {
                    warn!("a hotkey's fade stopped: {}", e.message);
                    break;
                }
                if f >= 1.0 {
                    break;
                }
            }
            let _ = tx.send(Command::FadeDone(n, key));
        });
        self.fades.insert(key, (n, task.abort_handle()));
        if let Some(p) = self.presses.get_mut(&n) {
            p.fades += 1;
            p.fade_paths.push(path);
        }
    }

    fn fade_done(&mut self, n: u64, key: FadeKey) {
        if self.fades.get(&key).is_some_and(|(p, _)| *p == n) {
            self.fades.remove(&key);
        }
        self.fade_ended(n);
    }

    /// One of press `n`'s fades is over, finished or stopped.
    fn fade_ended(&mut self, n: u64) {
        if let Some(p) = self.presses.get_mut(&n) {
            p.fades = p.fades.saturating_sub(1);
        }
        self.maybe_finish(n);
    }

    /// Stop press `n`'s fades where they are.
    fn stop_fades_of(&mut self, n: u64) {
        let keys: Vec<FadeKey> = self
            .fades
            .iter()
            .filter(|(_, (p, _))| *p == n)
            .map(|(k, _)| *k)
            .collect();
        for key in keys {
            if let Some((_, handle)) = self.fades.remove(&key) {
                handle.abort();
            }
            if let Some(p) = self.presses.get_mut(&n) {
                p.fades = p.fades.saturating_sub(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Paths, Settings};
    use crate::controller::Subscriptions;
    use serde_json::json;
    use tokio::time::sleep;

    /// A controller with Mic (muted) and Music, a runner working for it,
    /// and the runner's mailbox.
    struct Rig {
        c: Arc<Controller>,
        tx: UnboundedSender<Command>,
        dir: std::path::PathBuf,
    }

    impl Rig {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("weir-run-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let mut mic = Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono);
            mic.mute = true;
            let mixer = MixerState {
                strips: vec![
                    mic,
                    Strip::new(2, "Music", StripKind::Virtual, ChannelLayout::Stereo),
                ],
                buses: vec![Bus::new(
                    1,
                    "Headset",
                    BusKind::Hardware,
                    ChannelLayout::Stereo,
                )],
            };
            let paths = Paths::resolve(Some(dir.join("config.toml")));
            let c = Arc::new(Controller::new(
                mixer,
                None,
                paths,
                Settings::default(),
                Vec::new(),
            ));
            let tx = crate::hotkeys::start_runner(&c);
            Self { c, tx, dir }
        }

        /// Add a hotkey; returns its id.
        fn add(&self, h: Value) -> HotkeyId {
            let h: Hotkey = serde_json::from_value(h).unwrap();
            let saved = self
                .c
                .handle(Request::SetHotkey(h), &mut Subscriptions::default())
                .unwrap();
            saved["id"].as_u64().unwrap() as HotkeyId
        }

        /// Send `c`, and give the runner a moment.
        async fn send(&self, c: Command) {
            self.tx.send(c).unwrap();
            sleep(Duration::from_millis(30)).await;
        }

        fn strip(&self, id: StripId) -> Strip {
            self.c.mixer().strip(id).unwrap().clone()
        }

        /// A request as a client sends it, names and all.
        fn call(&self, method: &str, params: Value) -> Value {
            let mut params = Some(params);
            self.c.resolve_names(method, &mut params).unwrap();
            let envelope = RpcRequest {
                jsonrpc: "2.0".into(),
                id: Some(json!(1)),
                method: method.into(),
                params,
            };
            self.c
                .handle(envelope.parse().unwrap(), &mut Subscriptions::default())
                .unwrap_or_else(|e| panic!("{method}: {e}"))
        }

        fn undo_labels(&self) -> Vec<String> {
            let info = self
                .c
                .handle(Request::History, &mut Subscriptions::default())
                .unwrap();
            serde_json::from_value::<HistoryInfo>(info)
                .unwrap()
                .undo
                .into_iter()
                .map(|e| e.label)
                .collect()
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test]
    async fn push_to_talk_puts_back_only_what_it_changed_and_leaves_no_undo() {
        let r = Rig::new("ptt");
        let id = r.add(json!({
            "name": "Push to talk",
            "steps": [{"method": "set_strip", "params": {"id": 1, "mute": false}}],
            "on_release": "restore"
        }));
        r.send(Command::Press(id)).await;
        assert!(!r.strip(1).mute, "talking");
        // Meanwhile, the music fader moves by hand.
        r.c.handle(
            Request::SetStrip(StripPatch {
                id: 2,
                gain_db: Some(-12.0),
                ..Default::default()
            }),
            &mut Subscriptions::default(),
        )
        .unwrap();
        // A key held down repeats; that is not a second press.
        r.send(Command::Press(id)).await;
        r.send(Command::Release(id)).await;
        assert!(r.strip(1).mute, "muted again");
        assert_eq!(r.strip(2).gain_db, -12.0, "the hand's change stays");
        assert_eq!(
            r.undo_labels(),
            ["Music fader"],
            "nothing to undo for the hotkey"
        );
        // Undoing the fader, moved while talking, must not open the mic.
        r.c.handle(
            Request::Undo(HistoryStepParams { steps: None }),
            &mut Subscriptions::default(),
        )
        .unwrap();
        assert!(r.strip(1).mute, "still muted");
        assert_eq!(r.strip(2).gain_db, 0.0, "the fader is back");
    }

    #[tokio::test]
    async fn each_step_finds_its_strip_by_name_after_the_steps_before_it() {
        let r = Rig::new("names");
        r.call("save_setup", json!({"name": "Home"}));
        // In this setup strip 2 is another strip.
        r.call("remove_strip", json!({"id": "Music"}));
        r.call("add_strip", json!({"name": "Game", "kind": "virtual"}));
        r.call("save_setup", json!({"name": "Gaming"}));
        let id = r.add(json!({
            "name": "Music time",
            "steps": [
                {"method": "load_setup", "params": {"name": "Home"}},
                {"method": "set_strip", "params": {"id": "Music", "gain_db": -10}, "over_ms": 60}
            ]
        }));
        r.send(Command::Run(id)).await;
        sleep(Duration::from_millis(150)).await;
        assert_eq!(r.c.mixer().find_strip("Music").unwrap().gain_db, -10.0);
        assert_eq!(r.undo_labels()[0], "Music time (hotkey)");
        // Undoing it puts Gaming back, which is not a rename of Music.
        r.call("undo", json!({}));
        assert_eq!(r.strip(2).name, "Game");
        let h = r.c.hotkey(id).unwrap();
        assert_eq!(h.steps[1].params["id"], json!("Music"));
    }

    #[tokio::test]
    async fn next_goes_round_and_each_press_is_one_undo_step() {
        let r = Rig::new("next");
        let id = r.add(json!({
            "name": "Music levels",
            "each_press": "next",
            "steps": [
                {"method": "set_strip", "params": {"id": 2, "gain_db": -10}},
                {"method": "set_strip", "params": {"id": 2, "gain_db": -20}}
            ]
        }));
        let mut seen = Vec::new();
        for _ in 0..3 {
            r.send(Command::Run(id)).await;
            seen.push(r.strip(2).gain_db);
        }
        assert_eq!(seen, [-10.0, -20.0, -10.0]);
        assert_eq!(
            r.undo_labels(),
            [
                "Music levels (hotkey)",
                "Music levels (hotkey)",
                "Music levels (hotkey)"
            ]
        );
    }

    #[tokio::test]
    async fn holding_repeats_and_counts_as_one_change() {
        let r = Rig::new("repeat");
        let id = r.add(json!({
            "name": "Music down",
            "repeat_ms": 50,
            "steps": [{"method": "set_strip", "params": {"id": 2, "gain_delta_db": -1}}]
        }));
        r.tx.send(Command::Press(id)).unwrap();
        sleep(Duration::from_millis(700)).await;
        r.send(Command::Release(id)).await;
        let after = r.strip(2).gain_db;
        // Once at the press, then every 50 ms after the first 400.
        assert!(after <= -4.0, "went down by {after} dB only");
        let at_release = after;
        sleep(Duration::from_millis(200)).await;
        assert_eq!(r.strip(2).gain_db, at_release, "stopped when let go");
        assert_eq!(r.undo_labels(), ["Music down (hotkey)"]);
    }

    #[tokio::test]
    async fn a_fade_reaches_its_level_and_is_one_undo_step() {
        let r = Rig::new("fade");
        let id = r.add(json!({
            "name": "Fade music",
            "steps": [{"method": "set_strip", "params": {"id": 2, "gain_db": -20, "mute": false}, "over_ms": 200}]
        }));
        r.send(Command::Run(id)).await;
        let midway = r.strip(2).gain_db;
        assert!(midway < 0.0 && midway > -20.0, "fading, at {midway}");
        assert!(r.undo_labels().is_empty(), "not over yet");
        sleep(Duration::from_millis(300)).await;
        assert_eq!(r.strip(2).gain_db, -20.0);
        assert_eq!(r.undo_labels(), ["Fade music (hotkey)"]);
    }

    #[tokio::test]
    async fn letting_go_can_do_steps_of_its_own() {
        let r = Rig::new("release-steps");
        let id = r.add(json!({
            "name": "Talk",
            "steps": [{"method": "set_strip", "params": {"id": 1, "mute": false}}],
            "on_release": "steps",
            "release_steps": [{"method": "set_strip", "params": {"id": 2, "gain_db": -6}}]
        }));
        r.send(Command::Press(id)).await;
        r.send(Command::Release(id)).await;
        assert!(!r.strip(1).mute);
        assert_eq!(r.strip(2).gain_db, -6.0);
        assert_eq!(r.undo_labels(), ["Talk (hotkey)"]);
        // A client can press it by name, and its answer comes once the
        // steps are done, so a script reading the mixer next sees them.
        r.c.handle(
            Request::SetStrip(StripPatch {
                id: 2,
                gain_db: Some(0.0),
                ..Default::default()
            }),
            &mut Subscriptions::default(),
        )
        .unwrap();
        r.c.handle(
            Request::RunHotkey(HotkeyRef {
                hotkey: HotkeyKey::Name("talk".into()),
            }),
            &mut Subscriptions::default(),
        )
        .unwrap();
        assert_eq!(r.strip(2).gain_db, -6.0);
    }
}
