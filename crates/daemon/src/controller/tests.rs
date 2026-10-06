use super::names::resolve_names;
use super::*;
use serde_json::json;

fn mixer() -> MixerState {
    MixerState {
        strips: vec![
            Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono),
            Strip::new(2, "Music", StripKind::Virtual, ChannelLayout::Stereo),
        ],
        buses: vec![
            Bus::new(1, "Headset", BusKind::Hardware, ChannelLayout::Stereo),
            Bus::new(3, "Stream Mic", BusKind::Virtual, ChannelLayout::Stereo),
        ],
    }
}

/// A controller with no engine, its files in a directory of its own,
/// called the way the server calls it.
struct Rig {
    c: Controller,
    subs: Subscriptions,
    dir: std::path::PathBuf,
}

impl Rig {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("weir-ctl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::resolve(Some(dir.join("config.toml")));
        let c = Controller::new(mixer(), None, paths, Settings::default(), Vec::new());
        Self {
            c,
            subs: Subscriptions::default(),
            dir,
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        let mut params = Some(params);
        self.c.resolve_names(method, &mut params)?;
        let envelope = RpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: method.into(),
            params,
        };
        self.c.handle(envelope.parse()?, &mut self.subs)
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        self.call(method, params)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    fn code(&mut self, method: &str, params: Value) -> i32 {
        match self.call(method, params) {
            Ok(v) => panic!("{method} worked but should not have: {v}"),
            Err(e) => e.code,
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_strip_update_changes_only_what_it_names() {
    let mut r = Rig::new("strip");
    let s = r.ok(
        "set_strip",
        json!({"id": "Music", "gain_db": -6, "pan": 0.5}),
    );
    assert_eq!(
        (s["gain_db"].as_f64(), s["pan"].as_f64()),
        (Some(-6.0), Some(0.5))
    );
    let s = r.ok("set_strip", json!({"id": 2, "gain_delta_db": 3}));
    assert_eq!(
        (s["gain_db"].as_f64(), s["pan"].as_f64()),
        (Some(-3.0), Some(0.5))
    );
    let s = r.ok("set_strip", json!({"id": 2, "mute": "toggle"}));
    assert_eq!(s["mute"], true);
    let s = r.ok(
        "set_strip",
        json!({"id": 2, "mute": "toggle", "gain_db": 99}),
    );
    assert_eq!(
        (s["mute"].clone(), s["gain_db"].as_f64()),
        (json!(false), Some(12.0))
    );
}

#[test]
fn a_bus_delay_is_kept_within_range_and_undone_as_one_step() {
    let mut r = Rig::new("bus-delay");
    let b = r.ok("set_bus", json!({"id": "Headset", "delay_ms": 180.5}));
    assert_eq!(b["delay_ms"].as_f64(), Some(180.5));
    // Out of range is brought into range, as a fader is.
    let b = r.ok("set_bus", json!({"id": 1, "delay_ms": 9999}));
    assert_eq!(b["delay_ms"].as_f64(), Some(500.0));
    let b = r.ok("set_bus", json!({"id": 1, "delay_ms": -5}));
    assert_eq!(r.c.mixer().buses[0].delay_ms, 0.0);
    // A delay of nothing is left out of what the bus reports, as of its
    // file; a reader takes a missing one as none.
    assert!(b.get("delay_ms").is_none());
    // Other buses are untouched.
    assert_eq!(r.c.mixer().buses[1].delay_ms, 0.0);
    // Dragging the delay is one step in the history.
    for ms in [10, 20, 30] {
        r.ok("set_bus", json!({"id": 1, "delay_ms": ms}));
    }
    let h = r.ok("history", json!({}));
    assert_eq!(h["undo"][0]["label"], "A1 Headset delay");
    r.ok("undo", json!({}));
    assert_eq!(r.c.mixer().buses[0].delay_ms, 0.0);
}

#[test]
fn names_are_unique_and_short() {
    let mut r = Rig::new("names");
    assert_eq!(
        r.code("set_strip", json!({"id": 2, "name": "MIC"})),
        RpcError::APPLICATION
    );
    assert_eq!(
        r.code("set_strip", json!({"id": 2, "name": " "})),
        RpcError::INVALID_PARAMS
    );
    let long = "x".repeat(41);
    assert_eq!(
        r.code("add_bus", json!({"name": long, "kind": "virtual"})),
        RpcError::INVALID_PARAMS
    );
    let s = r.ok("set_strip", json!({"id": 2, "name": "  Tunes "}));
    assert_eq!(s["name"], "Tunes");
}

#[test]
fn only_hardware_takes_a_device() {
    let mut r = Rig::new("device");
    assert_eq!(
        r.code("set_strip", json!({"id": 2, "device": "x"})),
        RpcError::APPLICATION
    );
    let s = r.ok("set_strip", json!({"id": 1, "device": "alsa_input.usb"}));
    assert_eq!(s["device"], "alsa_input.usb");
    let s = r.ok("set_strip", json!({"id": 1, "device": null}));
    assert!(s.get("device").is_none());
    let add = json!({"name": "Game", "kind": "virtual", "device": "x"});
    assert_eq!(r.code("add_strip", add), RpcError::APPLICATION);
    let b = r.ok(
        "add_bus",
        json!({"name": "Speakers", "kind": "hardware", "layout": "5.1"}),
    );
    assert_eq!(
        (b["id"].clone(), b["layout"].clone()),
        (json!(4), json!("surround_5_1"))
    );
}

#[test]
fn routes_toggle_and_levels_leave_them_as_they_are() {
    let mut r = Rig::new("route");
    let routed = |s: &Value| s["routes"].as_array().unwrap().contains(&json!(1));
    let s = r.ok("set_route", json!({"strip": "Mic", "bus": "A1"}));
    assert!(routed(&s));
    let s = r.ok("set_route", json!({"strip": 1, "bus": 1}));
    assert!(!routed(&s));
    let s = r.ok("set_route", json!({"strip": 1, "bus": 1, "level_db": -6}));
    assert!(!routed(&s));
    assert_eq!(s["sends"]["1"].as_f64(), Some(-6.0));
    let s = r.ok(
        "set_route",
        json!({"strip": 1, "bus": 1, "level_delta_db": -3, "enabled": true}),
    );
    assert!(routed(&s));
    assert_eq!(s["sends"]["1"].as_f64(), Some(-9.0));
    // Removing a bus takes the routes to it along.
    r.ok("remove_bus", json!({"id": 1}));
    let m = r.c.mixer();
    assert!(m.strips[0].routes.is_empty() && m.strips[0].sends.is_empty());
}

#[test]
fn external_effects_go_where_a_strip_or_bus_has_room() {
    let mut r = Rig::new("insert");
    let s = r.ok(
        "set_strip",
        json!({"id": "Mic", "insert": {"enabled": true, "position": "before_gate"}}),
    );
    assert_eq!(
        s["insert"],
        json!({"enabled": true, "position": "before_gate", "fallback": "pass_through"})
    );
    let s = r.ok(
        "set_strip",
        json!({"id": "Mic", "insert": {"enabled": "toggle", "fallback": "silence"}}),
    );
    assert_eq!(s["insert"]["enabled"], false);
    assert_eq!(s["insert"]["fallback"], "silence");
    // A strip has no limiter, and a bus no gate.
    let bad =
        |r: &mut Rig, method, id, at| r.code(method, json!({"id": id, "insert": {"position": at}}));
    assert_eq!(
        bad(&mut r, "set_strip", 1, "after_limiter"),
        RpcError::INVALID_PARAMS
    );
    assert_eq!(
        bad(&mut r, "set_bus", 1, "before_gate"),
        RpcError::INVALID_PARAMS
    );
    let b = r.ok(
        "set_bus",
        json!({"id": "Headset", "insert": {"enabled": true, "position": "after_limiter"}}),
    );
    assert_eq!(b["insert"]["position"], "after_limiter");
    let h = r.ok("history", json!({}));
    assert_eq!(h["undo"][0]["label"], "A1 Headset external effects");
    assert_eq!(h["undo"][1]["label"], "Mic external effects");
}

#[test]
fn undo_takes_back_a_fader_drag_as_one_step() {
    let mut r = Rig::new("undo");
    for db in [-1, -2, -3] {
        r.ok("set_strip", json!({"id": 2, "gain_db": db}));
    }
    r.ok("set_strip", json!({"id": 2, "pan": -1}));
    let h = r.ok("history", json!({}));
    let labels: Vec<&str> = h["undo"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Music pan", "Music fader"]);
    r.ok("undo", json!({}));
    assert_eq!(r.c.mixer().strips[1].pan, 0.0);
    assert_eq!(r.c.mixer().strips[1].gain_db, -3.0);
    r.ok("undo", json!({}));
    assert_eq!(r.c.mixer().strips[1].gain_db, 0.0);
    assert_eq!(r.code("undo", json!({})), RpcError::APPLICATION);
    r.ok("redo", json!({"steps": 2}));
    assert_eq!(r.c.mixer().strips[1].pan, -1.0);
}

#[test]
fn strips_and_buses_move_in_the_list() {
    let mut r = Rig::new("move");
    assert_eq!(
        r.ok("move_strip", json!({"id": "Music", "index": 0})),
        json!([2, 1])
    );
    assert_eq!(
        r.ok("move_bus", json!({"id": 1, "index": 99})),
        json!([3, 1])
    );
}

#[test]
fn settings_are_checked() {
    let mut r = Rig::new("settings");
    assert_eq!(
        r.code("set_settings", json!({"sample_rate": 1234})),
        RpcError::INVALID_PARAMS
    );
    assert_eq!(
        r.code("set_settings", json!({"solo": {"cue": 9}})),
        RpcError::APPLICATION
    );
    let s = r.ok(
        "set_settings",
        json!({"solo": {"cue": "Headset"}, "quantum": 256, "meter_rate_hz": 500}),
    );
    assert_eq!(s["solo"], json!({"cue": 1}));
    assert_eq!(
        (s["quantum"].as_u64(), s["meter_rate_hz"].as_u64()),
        (Some(256), Some(120))
    );
    let s = r.ok("set_settings", json!({"quantum": 0}));
    assert!(s.get("quantum").is_none());
}

/// Stands in for systemd: remembers whether Weir starts at login, and can
/// refuse to change it.
#[derive(Clone)]
struct FakeLogin {
    enabled: std::sync::Arc<Mutex<Option<bool>>>,
    refuse: bool,
}

impl FakeLogin {
    fn new(enabled: Option<bool>) -> Self {
        Self {
            enabled: std::sync::Arc::new(Mutex::new(enabled)),
            refuse: false,
        }
    }
}

impl LoginStart for FakeLogin {
    fn get(&self) -> Option<bool> {
        *self.enabled.lock().unwrap()
    }

    fn set(&self, on: bool) -> Result<(), String> {
        if self.refuse {
            return Err("systemctl could not enable Weir's service: Access denied".into());
        }
        *self.enabled.lock().unwrap() = Some(on);
        Ok(())
    }
}

impl Rig {
    fn with_login(mut self, login: &FakeLogin) -> Self {
        self.c.login = Box::new(login.clone());
        self.c.refresh_start_at_login();
        self
    }
}

#[test]
fn starting_at_login_follows_systemd() {
    let login = FakeLogin::new(Some(false));
    let mut r = Rig::new("login").with_login(&login);
    assert_eq!(
        r.ok("get_state", json!({}))["settings"]["start_at_login"],
        false
    );

    let s = r.ok("set_settings", json!({"start_at_login": true}));
    assert_eq!(s["start_at_login"], true);
    assert_eq!(login.get(), Some(true), "systemd was asked");
    let s = r.ok("set_settings", json!({"start_at_login": "toggle"}));
    assert_eq!(s["start_at_login"], false);
    assert_eq!(login.get(), Some(false));

    // A patch that fails elsewhere leaves systemd alone.
    r.code(
        "set_settings",
        json!({"start_at_login": true, "quantum": 3}),
    );
    assert_eq!(login.get(), Some(false));
}

#[test]
fn starting_at_login_explains_why_it_cannot() {
    // Not installed as a service: the setting is absent, and asking for it
    // says why.
    let mut r = Rig::new("login-none").with_login(&FakeLogin::new(None));
    assert!(r.ok("get_state", json!({}))["settings"]
        .get("start_at_login")
        .is_none());
    let e = r
        .call("set_settings", json!({"start_at_login": true}))
        .unwrap_err();
    assert!(e.message.contains("not installed"), "{}", e.message);

    // systemctl refusing is passed on, and nothing changes.
    let login = FakeLogin {
        refuse: true,
        ..FakeLogin::new(Some(false))
    };
    let mut r = Rig::new("login-refused").with_login(&login);
    let e = r
        .call("set_settings", json!({"start_at_login": true}))
        .unwrap_err();
    assert_eq!(e.code, RpcError::APPLICATION);
    assert!(e.message.contains("Access denied"), "{}", e.message);
    assert_eq!(
        r.ok("get_state", json!({}))["settings"]["start_at_login"],
        false
    );
}

#[test]
fn scenes_bring_a_mix_back() {
    let mut r = Rig::new("scenes");
    r.ok("set_strip", json!({"id": 2, "gain_db": -12}));
    let lib = r.ok("save_scene", json!({"name": "Late night"}));
    assert_eq!(
        (lib["scenes"].clone(), lib["scene"].clone()),
        (json!(["Late night"]), json!("Late night"))
    );
    r.ok("set_strip", json!({"id": 2, "gain_db": 0}));
    let m = r.ok("load_scene", json!({"name": "Late night"}));
    assert_eq!(m["strips"][1]["gain_db"].as_f64(), Some(-12.0));
    assert_eq!(
        r.code("save_scene", json!({"name": "../escape"})),
        RpcError::APPLICATION
    );
    let lib = r.ok("delete_scene", json!({"name": "Late night"}));
    assert_eq!((lib["scenes"].clone(), lib.get("scene")), (json!([]), None));
    assert_eq!(
        r.code("load_scene", json!({"name": "Late night"})),
        RpcError::APPLICATION
    );
}

#[test]
fn equalizer_presets() {
    let mut r = Rig::new("eq");
    let both = json!({"name": "Bass boost", "strip": 2, "bus": 1});
    assert_eq!(r.code("apply_eq_preset", both), RpcError::INVALID_PARAMS);
    let s = r.ok(
        "apply_eq_preset",
        json!({"name": "bass BOOST", "strip": "Music"}),
    );
    assert_eq!(
        (
            s["eq"]["enabled"].clone(),
            s["eq"]["bands"].as_array().map(Vec::len)
        ),
        (json!(true), Some(1))
    );
    let band = json!([{"kind": "peak", "freq_hz": 1000, "gain_db": 3}]);
    assert_eq!(
        r.code(
            "save_eq_preset",
            json!({"name": "Bass boost", "bands": band})
        ),
        RpcError::APPLICATION
    );
    let all = r.ok("save_eq_preset", json!({"name": "Mine", "bands": band}));
    assert!(all.as_array().unwrap().iter().any(|p| p["name"] == "Mine"));
    assert_eq!(
        r.code("delete_eq_preset", json!({"name": "Telephone"})),
        RpcError::APPLICATION
    );
    let all = r.ok("delete_eq_preset", json!({"name": "mine"}));
    assert!(!all.as_array().unwrap().iter().any(|p| p["name"] == "Mine"));
}

#[test]
fn subscribing_to_nothing_in_particular_means_everything_but_the_window() {
    let mut r = Rig::new("subscribe");
    let topics = r.ok("subscribe", json!({}));
    assert_eq!(topics, json!(Topic::ALL));
    let topics = r.ok("unsubscribe", json!({"topics": ["meters"]}));
    assert!(!topics.as_array().unwrap().contains(&json!("meters")));
    assert_eq!(r.ok("unsubscribe", json!({})), json!([]));
}

#[test]
fn there_is_only_ever_one_window() {
    let r = Rig::new("one-window");
    assert!(!r.c.window_attached());
    assert!(r.c.claim_window(), "the first window gets it");
    assert!(!r.c.claim_window(), "a second one does not");
    assert!(r.c.window_attached());
    r.c.remove_window_client();
    assert!(!r.c.window_attached());
    assert!(r.c.claim_window(), "the next window after it closed does");
}

#[test]
fn strips_and_buses_can_be_named_instead_of_numbered() {
    let m = mixer();
    let mut p = json!({"id": "music", "gain_delta_db": 3, "sends": {"B1": -6, "1": 0},
                       "ducking": {"triggers": ["Mic"], "buses": ["Stream Mic"]}});
    resolve_names("set_strip", &mut p, &m).unwrap();
    assert_eq!(p["id"], 2);
    assert_eq!(p["sends"]["3"], -6);
    assert_eq!(p["sends"]["1"], 0);
    assert_eq!(p["ducking"]["triggers"], json!([1]));
    assert_eq!(p["ducking"]["buses"], json!([3]));

    let mut p = json!({"strip": "Mic", "bus": "A1", "enabled": "toggle"});
    resolve_names("set_route", &mut p, &m).unwrap();
    assert_eq!((p["strip"].clone(), p["bus"].clone()), (json!(1), json!(1)));

    let mut p = json!({"targets": [{"strip": "Music"}, {"bus": "Headset"}]});
    resolve_names("watch_spectrum", &mut p, &m).unwrap();
    assert_eq!(p["targets"], json!([{"strip": 2}, {"bus": 1}]));

    let mut p = json!({"name": "Game", "kind": "virtual", "routes": ["A1", "Stream Mic", 1]});
    resolve_names("add_strip", &mut p, &m).unwrap();
    assert_eq!(p["routes"], json!([1, 3, 1]));

    let mut p = json!({"rules": [{"app": "Spotify", "strip": "Music"}]});
    resolve_names("set_app_rules", &mut p, &m).unwrap();
    assert_eq!(p["rules"][0]["strip"], 2);

    // Numbers pass straight through, and unknown names are errors.
    let mut p = json!({"id": 1});
    resolve_names("set_bus", &mut p, &m).unwrap();
    assert_eq!(p["id"], 1);
    let mut p = json!({"id": "Nope"});
    assert!(resolve_names("set_strip", &mut p, &m).is_err());
}
