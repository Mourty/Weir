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
fn a_bus_delay_can_be_moved_by_an_amount() {
    let mut r = Rig::new("bus-delay-by");
    r.ok("set_bus", json!({"id": "Headset", "delay_ms": 190}));
    let b = r.ok("set_bus", json!({"id": "Headset", "delay_delta_ms": 1}));
    assert_eq!(b["delay_ms"].as_f64(), Some(191.0));
    r.ok("set_bus", json!({"id": "Headset", "delay_delta_ms": -11}));
    assert_eq!(r.c.mixer().buses[0].delay_ms, 180.0);
    // Both given: the step comes after the value, and the result stays in
    // range at either end.
    let b = r.ok(
        "set_bus",
        json!({"id": 1, "delay_ms": 100, "delay_delta_ms": 5}),
    );
    assert_eq!(b["delay_ms"].as_f64(), Some(105.0));
    let b = r.ok("set_bus", json!({"id": 1, "delay_delta_ms": 9999}));
    assert_eq!(b["delay_ms"].as_f64(), Some(500.0));
    r.ok("set_bus", json!({"id": 1, "delay_delta_ms": -9999}));
    assert_eq!(r.c.mixer().buses[0].delay_ms, 0.0);
    // Whichever way it was changed, the history calls it the delay.
    for patch in [
        json!({"id": 1, "delay_delta_ms": 3}),
        json!({"id": 1, "delay_ms": 7, "delay_delta_ms": 3}),
    ] {
        r.ok("set_bus", patch);
        let h = r.ok("history", json!({}));
        assert_eq!(h["undo"][0]["label"], "A1 Headset delay");
    }
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
fn setups_hold_what_is_there_and_scenes_how_it_sounds() {
    let mut r = Rig::new("setups");
    r.ok("set_strip", json!({"id": "Music", "gain_db": -12}));
    r.ok("set_bus", json!({"id": "Stream Mic", "delay_ms": 40}));
    r.ok("save_scene", json!({"name": "Main"}));
    let lib = r.ok("save_setup", json!({"name": "Headset"}));
    // What each holds, for telling what a scene would miss in a setup.
    assert_eq!(
        lib["setup_members"]["Headset"]["strips"],
        json!(["Mic", "Music"])
    );
    assert_eq!(
        lib["scene_members"]["Main"]["buses"],
        json!(["Headset", "Stream Mic"])
    );

    // Alone, a setup brings what is there, at its default mix: nothing
    // routed, whatever the mix was.
    let m = r.ok("load_setup", json!({"name": "Headset"}));
    assert_eq!(m["strips"][1]["gain_db"].as_f64(), Some(0.0));
    assert!(m["strips"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["routes"] == json!([])));
    let lib = r.ok("list_scenes", json!({}));
    assert_eq!(lib, json!(["Main"]));
    let state = r.ok("get_state", json!({}));
    assert!(
        state["library"].get("scene").is_none(),
        "no scene is current"
    );

    // With a scene, the scene's mix, bus delay included. One undo step.
    let m = r.ok("load_setup", json!({"name": "Headset", "scene": "Main"}));
    assert_eq!(m["strips"][1]["gain_db"].as_f64(), Some(-12.0));
    assert_eq!(m["buses"][1]["delay_ms"].as_f64(), Some(40.0));
    let state = r.ok("get_state", json!({}));
    assert_eq!(state["library"]["scene"], json!("Main"));
    let history = r.ok("history", json!({}));
    assert_eq!(
        history["undo"][0]["label"],
        json!("Load setup Headset with scene Main")
    );

    // A scene describes the whole mix: a strip it never knew goes back to
    // its default, unrouted, when it loads, alone or with a setup.
    let game = r.ok(
        "add_strip",
        json!({"name": "Game", "kind": "virtual", "routes": [1]}),
    );
    r.ok("set_strip", json!({"id": game["id"], "gain_db": -5}));
    let m = r.ok("load_scene", json!({"name": "Main"}));
    let game = &m["strips"][2];
    assert_eq!(
        (game["gain_db"].as_f64(), game["routes"].clone()),
        (Some(0.0), json!([]))
    );

    // A scene with strips the setup lacks still loads what it can.
    r.ok("save_scene", json!({"name": "With game"}));
    let m = r.ok(
        "load_setup",
        json!({"name": "Headset", "scene": "With game"}),
    );
    assert_eq!(m["strips"].as_array().unwrap().len(), 2);
    assert_eq!(
        r.code("load_setup", json!({"name": "Headset", "scene": "Nope"})),
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

#[test]
fn hotkeys_are_checked_kept_by_name_and_saved() {
    let mut r = Rig::new("hotkeys");
    let h = r.ok(
        "set_hotkey",
        json!({
            "name": "Mute mic",
            "keys": "ctrl + alt + m",
            "steps": [{"method": "set_strip", "params": {"id": "mic", "mute": "toggle"}}]
        }),
    );
    assert_eq!(h["id"], json!(1));
    assert_eq!(h["keys"], json!(["Ctrl+Alt+M"]));
    // Strips and buses are kept by name, written as the mixer has them,
    // however they were given.
    assert_eq!(h["steps"][0]["params"]["id"], json!("Mic"));
    let id = |r: &mut Rig| {
        r.ok("list_hotkeys", json!({}))["hotkeys"][0]["steps"][0]["params"]["id"].clone()
    };
    // Renaming the strip renames it in the hotkey, and so does taking the
    // rename back.
    r.ok("set_strip", json!({"id": "Mic", "name": "Voice"}));
    assert_eq!(id(&mut r), json!("Voice"));
    r.ok("undo", json!({}));
    assert_eq!(id(&mut r), json!("Mic"));
    r.ok("redo", json!({}));
    assert_eq!(id(&mut r), json!("Voice"));
    let info = r.ok("list_hotkeys", json!({}));
    assert!(info.get("problems").is_none(), "{info}");
    let route = r.ok(
        "set_hotkey",
        json!({
            "name": "Music on stream",
            "steps": [{"method": "set_route", "params": {"strip": 2, "bus": "B1", "enabled": true}}]
        }),
    );
    assert_eq!(
        route["steps"][0]["params"],
        json!({"strip": "Music", "bus": "Stream Mic", "enabled": true})
    );
    r.ok("remove_hotkey", json!({"hotkey": "Music on stream"}));

    // A second one with the same name or keys is turned down, even when
    // they are only one of its key combinations.
    let step = json!([{"method": "load_scene", "params": {"name": "Gaming"}}]);
    for clash in [
        json!({"name": "mute MIC", "steps": step}),
        json!({"name": "Other", "keys": "Ctrl+Alt+M", "steps": step}),
        json!({"name": "Other", "keys": ["F9", "Alt+Ctrl+M"], "steps": step}),
    ] {
        assert_eq!(r.code("set_hotkey", clash), RpcError::APPLICATION);
    }
    // So are keys that would stop typing, requests that make no sense in a
    // hotkey, fades of things that cannot fade, and strips that do not
    // exist.
    for bad in [
        json!({"name": "A", "keys": "M", "steps": step}),
        json!({"name": "A", "keys": ["F9", "M"], "steps": step}),
        json!({"name": "A", "keys": ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9"], "steps": step}),
        json!({"name": "B", "steps": [{"method": "subscribe", "params": {}}]}),
        json!({"name": "C", "steps": [{"method": "set_strip", "params": {"id": 1, "mute": true}, "over_ms": 100}]}),
        json!({"name": "D", "steps": [{"method": "set_strip", "params": {"id": "Nobody", "mute": true}}]}),
        json!({"name": "D", "steps": [{"method": "set_strip", "params": {"id": 9, "mute": true}}]}),
        json!({"name": "E", "steps": [{"method": "fly", "params": {}}]}),
    ] {
        assert!(r.call("set_hotkey", bad.clone()).is_err(), "{bad}");
    }

    // Several combinations work alike, written the usual way, without
    // repeats.
    let h = r.ok(
        "set_hotkey",
        json!({"name": "Talk", "keys": ["f9", " ctrl+alt+t", "F9", ""], "steps": step}),
    );
    assert_eq!(h["keys"], json!(["F9", "Ctrl+Alt+T"]));
    r.ok("remove_hotkey", json!({"hotkey": "Talk"}));

    // Saved to its own file, and read back by the next daemon.
    let paths = Paths::resolve(Some(r.dir.join("config.toml")));
    let again = Controller::new(mixer(), None, paths, Settings::default(), Vec::new());
    assert_eq!(again.hotkeys().len(), 1);
    assert_eq!(again.hotkeys()[0].name, "Mute mic");
}

#[test]
fn hotkeys_work_on_the_strip_of_their_name_in_each_setup() {
    let mut r = Rig::new("hotkey-setups");
    r.ok("save_setup", json!({"name": "Home"}));
    // Another setup, where strip 2 is another strip: removing the last
    // strip and adding one gives the new one its id.
    r.ok("remove_strip", json!({"id": "Music"}));
    let game = r.ok("add_strip", json!({"name": "Game", "kind": "virtual"}));
    assert_eq!(game["id"], json!(2));
    r.ok("save_setup", json!({"name": "Gaming"}));

    // A hotkey for a strip only a saved setup has can be made.
    r.ok(
        "set_hotkey",
        json!({
            "name": "Quiet music",
            "steps": [{"method": "set_strip", "params": {"id": "Music", "gain_db": -20}}]
        }),
    );
    let problems = |r: &mut Rig| r.ok("list_hotkeys", json!({}))["problems"].clone();
    let p = problems(&mut r);
    assert!(
        p[0]["problem"]
            .as_str()
            .unwrap()
            .contains("no strip called 'Music'"),
        "{p}"
    );
    // Here it does nothing, and above all not to the strip with Music's
    // id.
    let step = HotkeyStep::new("set_strip", json!({"id": "Music", "gain_db": -20}));
    assert!(r.c.run_step(&step).is_err());
    assert_eq!(r.c.mixer().strip(2).unwrap().gain_db, 0.0);

    // Loading the setup with Music makes it work there. Strip 2 having
    // another name there is not a rename, whichever way.
    r.ok("load_setup", json!({"name": "Home"}));
    assert!(problems(&mut r).is_null());
    r.c.run_step(&step).unwrap();
    assert_eq!(r.c.mixer().find_strip("Music").unwrap().gain_db, -20.0);
    r.ok("load_setup", json!({"name": "Gaming"}));
    r.ok("undo", json!({}));
    r.ok("redo", json!({}));
    let h = r.ok("list_hotkeys", json!({}));
    assert_eq!(h["hotkeys"][0]["steps"][0]["params"]["id"], json!("Music"));

    // A name no mixer has is turned down.
    let ghost = json!({
        "name": "Ghost",
        "steps": [{"method": "set_strip", "params": {"id": "Guitar", "mute": true}}]
    });
    assert_eq!(r.code("set_hotkey", ghost), RpcError::APPLICATION);
}

#[test]
fn hotkeys_from_before_names_take_the_names_of_the_mixer_now() {
    let dir = std::env::temp_dir().join(format!("weir-ctl-hotkeys-v1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let paths = Paths::resolve(Some(dir.join("config.toml")));
    let old = json!({
        "version": 1,
        "hotkeys": [{
            "id": 1,
            "name": "Talk",
            "steps": [
                {"method": "set_strip", "params": {"id": 1, "mute": false}},
                {"method": "set_route", "params": {"strip": 2, "bus": 3, "level_db": -6}},
                {"method": "set_strip", "params": {"id": 7, "mute": true}}
            ]
        }]
    });
    std::fs::write(&paths.hotkeys_file, old.to_string()).unwrap();
    let c = Controller::new(
        mixer(),
        None,
        paths.clone(),
        Settings::default(),
        Vec::new(),
    );
    let steps = &c.hotkeys()[0].steps;
    assert_eq!(steps[0].params, json!({"id": "Mic", "mute": false}));
    assert_eq!(
        steps[1].params,
        json!({"strip": "Music", "bus": "Stream Mic", "level_db": -6})
    );
    // A strip already gone stays a number, and is said to be removed.
    assert_eq!(steps[2].params, json!({"id": 7, "mute": true}));
    let info = c.hotkeys_info();
    assert!(info.problems[0].problem.contains("removed"), "{info:?}");
    // Written back once, the old file kept.
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.hotkeys_file).unwrap()).unwrap();
    assert_eq!(saved["version"], json!(2));
    let copy = paths.backups_dir.join("hotkeys-version-1.json");
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(copy).unwrap()).unwrap();
    assert_eq!(kept, old);

    // A file from a newer Weir is left alone, and hotkeys are read-only.
    std::fs::write(
        &paths.hotkeys_file,
        json!({"version": 99, "hotkeys": []}).to_string(),
    )
    .unwrap();
    let c = Controller::new(mixer(), None, paths, Settings::default(), Vec::new());
    assert!(c.hotkeys_info().problems[0].problem.contains("newer Weir"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hotkeys_are_ordered_grouped_and_switched() {
    let mut r = Rig::new("hotkey-groups");
    let step = json!([{"method": "load_scene", "params": {"name": "Gaming"}}]);
    for name in ["A", "B", "C"] {
        r.ok("set_hotkey", json!({"name": name, "steps": step}));
    }
    // Each hotkey's name and group, in the list's order.
    let list = |r: &mut Rig| -> Vec<(String, u64)> {
        r.ok("list_hotkeys", json!({}))["hotkeys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                let group = h.get("group").and_then(Value::as_u64).unwrap_or(0);
                (h["name"].as_str().unwrap().to_string(), group)
            })
            .collect()
    };
    let named = |pairs: &[(&str, u64)]| -> Vec<(String, u64)> {
        pairs.iter().map(|&(n, g)| (n.to_string(), g)).collect()
    };
    r.ok("move_hotkey", json!({"hotkey": "C", "index": 0}));
    assert_eq!(list(&mut r), named(&[("C", 0), ("A", 0), ("B", 0)]));

    let g = r.ok("add_hotkey_group", json!({"name": "Games"}));
    assert_eq!(g, json!({"id": 1, "name": "Games"}));
    assert_eq!(
        r.code("add_hotkey_group", json!({"name": "GAMES"})),
        RpcError::APPLICATION
    );
    assert!(r.call("add_hotkey_group", json!({"name": "  "})).is_err());
    r.ok(
        "add_hotkey_group",
        json!({"name": "Stream", "enabled": false}),
    );
    // Into a group: last by default, or at a place among its hotkeys.
    r.ok("move_hotkey", json!({"hotkey": "A", "group": "Games"}));
    r.ok(
        "move_hotkey",
        json!({"hotkey": "B", "group": 1, "index": 0}),
    );
    r.ok("move_hotkey", json!({"hotkey": "C", "group": "stream"}));
    assert_eq!(list(&mut r), named(&[("B", 1), ("A", 1), ("C", 2)]));
    assert!(r
        .call(
            "set_hotkey",
            json!({"name": "D", "group": 9, "steps": step})
        )
        .is_err());
    // Changed into another group in set_hotkey, it goes last there too.
    let mut b = r.ok("list_hotkeys", json!({}))["hotkeys"][0].clone();
    b["group"] = json!(2);
    r.ok("set_hotkey", b);
    assert_eq!(list(&mut r), named(&[("A", 1), ("C", 2), ("B", 2)]));
    r.ok(
        "move_hotkey",
        json!({"hotkey": "B", "group": 1, "index": 0}),
    );
    assert_eq!(list(&mut r), named(&[("B", 1), ("A", 1), ("C", 2)]));

    let a = r.ok("switch_hotkey", json!({"hotkey": "a", "enabled": "toggle"}));
    assert_eq!(a["enabled"], json!(false));
    let g = r.ok(
        "set_hotkey_group",
        json!({"group": "Stream", "enabled": "toggle", "name": "Streaming"}),
    );
    assert_eq!(g, json!({"id": 2, "name": "Streaming"}));
    let info = r.ok(
        "move_hotkey_group",
        json!({"group": "Streaming", "index": 0}),
    );
    assert_eq!(info["groups"][0]["name"], json!("Streaming"));
    assert_eq!(info["groups"][1]["name"], json!("Games"));

    // Removing a group keeps its hotkeys, in no group, where it was.
    let info = r.ok("remove_hotkey_group", json!({"group": "Games"}));
    // A is 1, B 2 and C 3.
    assert_eq!(
        info["order"],
        json!([{"group": 2}, {"hotkey": 2}, {"hotkey": 1}])
    );
    assert!(r
        .call("move_hotkey", json!({"hotkey": "A", "group": "Games"}))
        .is_err());
    r.ok("move_hotkey", json!({"hotkey": "C", "group": 0}));
    assert_eq!(list(&mut r), named(&[("B", 0), ("A", 0), ("C", 0)]));

    // Groups and hotkeys in no group go anywhere among each other.
    let info = r.ok(
        "move_hotkey_group",
        json!({"group": "Streaming", "index": 2}),
    );
    assert_eq!(
        info["order"],
        json!([{"hotkey": 2}, {"hotkey": 1}, {"group": 2}, {"hotkey": 3}])
    );
    let info = r.ok("move_hotkey", json!({"hotkey": "C", "index": 0}));
    assert_eq!(
        info["order"],
        json!([{"hotkey": 3}, {"hotkey": 2}, {"hotkey": 1}, {"group": 2}])
    );
    // Into the group, it leaves its place; out again, it takes one.
    r.ok("move_hotkey", json!({"hotkey": "B", "group": 2}));
    let info = r.ok(
        "move_hotkey",
        json!({"hotkey": "B", "group": 0, "index": 3}),
    );
    let order = json!([{"hotkey": 3}, {"hotkey": 1}, {"group": 2}, {"hotkey": 2}]);
    assert_eq!(info["order"], order);
    assert_eq!(list(&mut r), named(&[("C", 0), ("A", 0), ("B", 0)]));

    // Saved, groups and order too.
    let paths = Paths::resolve(Some(r.dir.join("config.toml")));
    let again = Controller::new(mixer(), None, paths, Settings::default(), Vec::new());
    let names: Vec<String> = again.hotkeys().into_iter().map(|h| h.name).collect();
    assert_eq!(names, ["C", "A", "B"]);
    assert_eq!(again.hotkey_groups().len(), 1);
    assert_eq!(again.hotkey_groups()[0].name, "Streaming");
    assert_eq!(to_json(&again.hotkeys_info().order), order);
}

#[test]
fn a_hotkey_on_a_removed_strip_is_a_problem_and_can_be_removed_by_name() {
    let mut r = Rig::new("hotkey-problems");
    r.ok(
        "set_hotkey",
        json!({
            "name": "Music down",
            "steps": [{"method": "set_strip", "params": {"id": "Music", "gain_delta_db": -3}}]
        }),
    );
    r.ok("remove_strip", json!({"id": "Music"}));
    let info = r.ok("list_hotkeys", json!({}));
    assert_eq!(info["problems"][0]["hotkey"], json!(1), "{info}");
    let left = r.ok("remove_hotkey", json!({"hotkey": "music DOWN"}));
    assert_eq!(left["hotkeys"], json!([]));
    assert!(r.call("remove_hotkey", json!({"hotkey": 7})).is_err());
    // Pressing needs the runner, which a bare controller does not have.
    assert!(r.call("run_hotkey", json!({"hotkey": 1})).is_err());
}

#[test]
fn settings_go_to_another_mixer_by_name() {
    let mut a = Rig::new("export-a");
    a.ok("save_scene", json!({"name": "Gaming"}));
    a.ok("save_setup", json!({"name": "Home"}));
    a.ok("save_eq_preset", json!({"name": "Warm", "bands": []}));
    a.ok(
        "set_app_rules",
        json!({"rules": [{"app": "firefox", "strip": 2}]}),
    );
    a.ok(
        "add_hotkey_group",
        json!({"name": "Streaming", "enabled": false}),
    );
    a.ok(
        "set_hotkey",
        json!({"name": "Music down", "keys": "Ctrl+Alt+Down", "group": 1, "steps": [
            {"method": "set_strip", "params": {"id": "Music", "gain_delta_db": -3}}]}),
    );
    a.ok(
        "set_hotkey",
        json!({"name": "Mute mic", "keys": "Ctrl+Alt+M", "steps": [
            {"method": "set_strip", "params": {"id": "Mic", "mute": "toggle"}}]}),
    );
    a.ok(
        "set_settings",
        json!({"solo": {"cue": 3}, "meter_rate_hz": 20}),
    );
    let zip = a.dir.join("all.zip");
    let out = a.ok(
        "export_settings",
        json!({"path": zip, "all": true,
               "window_look": {"appearance": "light", "device_names": {"x": "y"}}}),
    );
    assert_eq!(
        out["files"],
        json!([
            "scenes/Gaming.json",
            "setups/Home.json",
            "hotkeys/Music down.json",
            "hotkeys/Mute mic.json",
            "hotkeys/groups.json",
            "eq-presets/Warm.json",
            "app-rules/app-rules.json",
            "preferences/preferences.json"
        ])
    );
    // In the files, strips and buses go by name, and only the window's
    // look leaves the computer.
    let read = crate::transfer::read_export(&zip).unwrap();
    let body = |path: &str| {
        read.iter()
            .find(|f| f.path == path)
            .and_then(|f| f.body.clone().ok())
            .unwrap()
    };
    let crate::transfer::Body::Hotkey(h) = body("hotkeys/Music down.json") else {
        panic!("not a hotkey");
    };
    assert_eq!(h.group.as_deref(), Some("Streaming"));
    assert_eq!(h.hotkey.steps[0].params["id"], json!("Music"));
    let crate::transfer::Body::Preferences(p) = body("preferences/preferences.json") else {
        panic!("not preferences");
    };
    assert_eq!(p.window_look, Some(json!({"appearance": "light"})));
    assert_eq!(
        p.mixer.unwrap().solo,
        crate::transfer::SoloFile::Cue("Stream Mic".into())
    );

    // Another computer: its Music strip is called Media, and it has a
    // scene and a hotkey of the same names already.
    let mut b = Rig::new("export-b");
    b.ok("set_strip", json!({"id": "Music", "name": "Media"}));
    b.ok("save_scene", json!({"name": "Gaming"}));
    b.ok(
        "set_hotkey",
        json!({"name": "Mute mic", "keys": "Ctrl+Alt+M", "steps": []}),
    );
    let seen = b.ok("inspect_import", json!({"path": zip}));
    assert_eq!(seen["missing"], json!([{"kind": "strip", "name": "Music"}]));
    let item = |id: &str| {
        seen["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no {id} in {seen}"))
    };
    let gaming = item("scenes/Gaming.json");
    assert_eq!(
        (gaming["taken"].clone(), gaming["free_name"].clone()),
        (json!(true), json!("Gaming 2"))
    );
    assert_eq!(
        item("hotkeys/Mute mic.json")["keys_taken"],
        json!([{"keys": "Ctrl+Alt+M", "by": "Mute mic"}])
    );
    // Music down names a strip this mixer lacks, but the setup Home in the
    // file has it: it works while Home is loaded, and asks for no strip.
    let down = item("hotkeys/Music down.json");
    assert!(down.get("missing").is_none(), "{down}");
    assert_eq!(down["setup_items"], json!(["setups/Home.json"]));
    assert!(
        down["note"]
            .as_str()
            .unwrap()
            .contains("'Home' (in this file)"),
        "{down}"
    );
    assert_eq!(
        item("app-rules/app-rules.json")["missing"],
        json!([{"kind": "strip", "name": "Music"}])
    );
    assert!(item("preferences/preferences.json#audio_timing")["note"].is_string());
    assert!(
        b.call(
            "import_settings",
            json!({"path": zip, "map_strips": {"Music": "Nope"}})
        )
        .is_err(),
        "a map to a strip that is not here"
    );

    // Unmapped, the app rules cannot come; taken names are skipped. Music
    // down comes with Home, keeping its strip's name.
    let done = b.ok("import_settings", json!({"path": zip}));
    let skipped = done["skipped"].to_string();
    assert!(
        skipped.contains("app rules: this mixer has no strip 'Music'"),
        "{skipped}"
    );
    assert!(
        skipped.contains("scene 'Gaming': there is one called that already"),
        "{skipped}"
    );
    let imported = done["imported"].to_string();
    assert!(imported.contains("setup 'Home'"), "{imported}");
    assert!(imported.contains("hotkey 'Music down'"), "{imported}");

    // Mapped, and keeping both.
    let done = b.ok(
        "import_settings",
        json!({"path": zip, "map_strips": {"Music": "Media"}, "when_taken": "keep_both",
               "items": ["scenes/Gaming.json", "hotkeys/Music down.json",
                         "hotkeys/Mute mic.json", "app-rules/app-rules.json",
                         "preferences/preferences.json#mixer",
                         "preferences/preferences.json#window_look"]}),
    );
    assert_eq!(
        done["imported"],
        json!([
            "scene 'Gaming 2'",
            "app rules",
            "hotkey 'Music down 2'",
            "hotkey 'Mute mic 2'",
            "preferences: window look",
            "preferences: mixer behavior"
        ]),
        "{done}"
    );
    assert!(done["notes"].to_string().contains("without Ctrl+Alt+M"));
    assert_eq!(done["window_look"], json!({"appearance": "light"}));
    assert!(std::path::Path::new(done["backup"].as_str().unwrap())
        .join("hotkeys.json")
        .exists());
    let info = b.ok("list_hotkeys", json!({}));
    let strip_of = |name: &str| {
        info["hotkeys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["name"] == name)
            .unwrap()["steps"][0]["params"]["id"]
            .clone()
    };
    assert_eq!(strip_of("Music down"), json!("Music"));
    assert_eq!(strip_of("Music down 2"), json!("Media"));
    assert_eq!(
        info["groups"],
        json!([{"id": 1, "name": "Streaming", "enabled": false}])
    );
    let state = b.ok("get_state", json!({}));
    assert_eq!(state["app_rules"], json!([{"app": "firefox", "strip": 2}]));
    assert_eq!(state["settings"]["solo"], json!({"cue": 3}));
    assert_eq!(state["settings"]["meter_rate_hz"], json!(20));

    // Without Home, and with no setup here that has Music, Music down
    // cannot come.
    let mut d = Rig::new("export-d");
    d.ok("set_strip", json!({"id": "Music", "name": "Media"}));
    let done = d.ok(
        "import_settings",
        json!({"path": zip, "items": ["hotkeys/Music down.json"]}),
    );
    assert!(
        done["skipped"]
            .to_string()
            .contains("hotkey 'Music down': no strip called 'Music'"),
        "{done}"
    );
    // A setup saved here that has it will do.
    d.ok("set_strip", json!({"id": "Media", "name": "Music"}));
    d.ok("save_setup", json!({"name": "Old"}));
    d.ok("set_strip", json!({"id": "Music", "name": "Media"}));
    let seen = d.ok("inspect_import", json!({"path": zip}));
    let down = seen["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "hotkeys/Music down.json")
        .unwrap()
        .clone();
    assert_eq!(down["setups"], json!(["Old"]));

    // Replacing keeps a copy of what was there.
    let done = b.ok(
        "import_settings",
        json!({"path": zip, "items": ["scenes/Gaming.json"],
               "choices": {"scenes/Gaming.json": "replace"}}),
    );
    assert_eq!(done["imported"], json!(["scene 'Gaming'"]));
    assert!(std::path::Path::new(done["backup"].as_str().unwrap())
        .join("scenes/Gaming.toml")
        .exists());

    // Replacing every hotkey takes the imported ones' order and groups,
    // but only when some come in: importing a scene alone clears nothing.
    let mut c = Rig::new("export-c");
    c.ok(
        "set_hotkey",
        json!({"name": "Old", "keys": "F9", "steps": []}),
    );
    c.ok(
        "import_settings",
        json!({"path": zip, "hotkeys": "replace_all", "items": ["scenes/Gaming.json"]}),
    );
    assert_eq!(
        c.ok("list_hotkeys", json!({}))["hotkeys"][0]["name"],
        json!("Old")
    );
    c.ok(
        "import_settings",
        json!({"path": zip, "hotkeys": "replace_all", "map_strips": {"Music": "Mic"},
               "items": ["hotkeys/Music down.json", "hotkeys/Mute mic.json"]}),
    );
    let info = c.ok("list_hotkeys", json!({}));
    let names: Vec<&str> = info["hotkeys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Music down", "Mute mic"]);
    assert_eq!(info["order"], json!([{"group": 1}, {"hotkey": 2}]));

    // One thing alone goes to a bare .json, and comes back from one.
    let one = a.dir.join("Warm.json");
    a.ok(
        "export_settings",
        json!({"path": one, "eq_presets": ["warm"]}),
    );
    let seen = c.ok("inspect_import", json!({"path": one}));
    assert_eq!(seen["items"][0]["id"], json!("Warm.json"));
    assert_eq!(seen["items"][0]["kind"], json!("eq_preset"));
    c.ok("import_settings", json!({"path": one}));
    assert!(c
        .ok("list_eq_presets", json!({}))
        .to_string()
        .contains("\"Warm\""));
    assert!(a
        .call(
            "export_settings",
            json!({"path": a.dir.join("two.json"), "scenes": ["Gaming"], "setups": ["Home"]})
        )
        .is_err());
    assert!(a
        .call(
            "export_settings",
            json!({"path": "relative.zip", "all": true})
        )
        .is_err());

    // A later Weir's file is not half read.
    let later = a.dir.join("later.json");
    std::fs::write(
        &later,
        json!({"weir": "scene", "format": 99, "weir_version": "9.0.0"}).to_string(),
    )
    .unwrap();
    let e = c
        .call("inspect_import", json!({"path": later}))
        .unwrap_err();
    assert!(
        e.message.contains("newer version of Weir (9.0.0)"),
        "{}",
        e.message
    );
}
