//! What the fuzz targets do with each made-up input: hand it to Weir the
//! way Weir meets it, and check what it must never do (panic in its own
//! code, keep a mixer that does not hold together, make sound that is not
//! a number).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Once;
use weir_daemon::config::{self, Paths, Settings};
use weir_daemon::controller::{Controller, Subscriptions};
use weir_daemon::login::LoginStart;
use weir_protocol::*;

/// Make a panic in Weir's own code (crates/, and the standard library,
/// which may have been called from it) a crash the fuzzer reports. One
/// in other people's crates is left to Weir's guard
/// (`weir_protocol::crash::guarded`), which makes it an error about the
/// input, as when Weir runs; WEIR_FUZZ_ALL=1 reports those too, as does
/// `unguarded`, for what Weir reads with no guard.
pub fn watch_panics_all(unguarded: bool) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let all = unguarded || std::env::var_os("WEIR_FUZZ_ALL").is_some();
        let fuzzer = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let mut file = info.location().map(|l| l.file()).unwrap_or("").to_string();
            // A panic in the standard library (a slice index, say) is the
            // fault of the first code outside it that called it.
            if file.starts_with("/rustc/") {
                let trace = std::backtrace::Backtrace::force_capture().to_string();
                if let Some(caller) = trace
                    .lines()
                    .filter_map(|l| l.trim().strip_prefix("at "))
                    .skip_while(|f| !f.contains("core/src/panicking.rs"))
                    .find(|f| !f.starts_with("/rustc/"))
                {
                    file = caller.to_string();
                }
            }
            let ours = !file.contains(".cargo/registry") && !file.contains("vendor/");
            if all || ours {
                fuzzer(info);
            }
        }));
    });
}

/// [`watch_panics_all`], for input read behind the guard.
pub fn watch_panics() {
    watch_panics_all(false);
}

/// A folder of this process's own, empty, in memory where there is
/// /dev/shm: Weir's configuration folder for one input.
fn scratch() -> PathBuf {
    let shm = Path::new("/dev/shm");
    let base = if shm.is_dir() {
        shm.to_path_buf()
    } else {
        std::env::temp_dir()
    };
    let dir = base.join(format!("weir-fuzz-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("cfg")).expect("making the scratch folder");
    dir
}

/// Starting at login, kept nowhere: the fuzzer must not change the
/// machine it runs on.
struct NoLogin;

impl LoginStart for NoLogin {
    fn get(&self) -> Option<bool> {
        None
    }
    fn set(&self, _: bool) -> Result<(), String> {
        Err("not here".into())
    }
}

/// A controller with no engine, its files in `dir`, as the daemon starts
/// one: from what `config.toml` there holds, or the defaults.
fn controller(dir: &Path) -> Controller {
    let paths = Paths::resolve(Some(dir.join("config.toml")));
    let loaded = config::load(&paths.config_file).ok().flatten();
    let (mut mixer, settings, rules, scene, setup, recorder) = match loaded {
        Some((cfg, report)) => {
            if report.split_setups {
                let _ = config::split_setups(&paths);
            }
            (
                cfg.mixer,
                cfg.settings,
                cfg.app_rules,
                cfg.current_scene,
                cfg.current_setup,
                cfg.recorder,
            )
        }
        None => (
            weir_daemon::defaults::default_mixer(),
            Settings::default(),
            Vec::new(),
            None,
            None,
            RecorderSettings::default(),
        ),
    };
    mixer.normalize();
    let mut c = Controller::new_with(mixer, None, paths, settings, rules, SafeMode::default());
    c.set_login(Box::new(NoLogin));
    c.set_recorder_settings(recorder);
    c.restore_current(scene, setup);
    c
}

/// What a mixer Weir keeps must always be: as `normalize` leaves it, and
/// the same after being written as JSON and read back (JSON has no
/// infinity or NaN, so neither may get in).
fn holds_together(m: &MixerState, after: &str) {
    let mut again = m.clone();
    let fixes = again.normalize();
    assert!(
        fixes.is_empty(),
        "the mixer was not normalized after {after}: {fixes:?}"
    );
    let json = serde_json::to_value(m).expect("the mixer as JSON");
    let back: MixerState = serde_json::from_value(json).expect("the mixer read back");
    assert_eq!(
        &back, m,
        "the mixer changed going through JSON after {after}"
    );
}

/// Requests a made-up line may not make here: ones that reach files
/// outside the scratch folder or other programs, or that only take time.
/// Imports have their own target.
const NOT_HERE: [&str; 9] = [
    "export_settings",
    "inspect_import",
    "import_settings",
    "add_sound",
    "set_recording",
    "diagnostics",
    "describe",
    "restart",
    "show_window",
];

/// One request, as the server takes it from a line.
fn request(c: &Controller, subs: &mut Subscriptions, v: Value) {
    let Ok(mut envelope) = serde_json::from_value::<RpcRequest>(v) else {
        return;
    };
    if NOT_HERE.contains(&envelope.method.as_str()) {
        return;
    }
    let method = envelope.method.clone();
    let done = c
        .resolve_names(&envelope.method, &mut envelope.params)
        .and_then(|_| envelope.parse())
        .and_then(|req| c.handle(req, subs));
    // Whatever went wrong is an answer: one that cannot be written
    // would leave the client waiting.
    let answer = match done {
        Ok(v) => RpcResponse::ok(Value::from(1), v),
        Err(e) => RpcResponse::err(Value::from(1), e),
    };
    serde_json::to_string(&answer).expect("the answer as JSON");
    holds_together(&c.mixer(), &method);
}

/// Lines to the control socket, each run on a mixer of the target's own.
pub fn rpc(data: &[u8]) {
    watch_panics_all(true);
    let dir = scratch();
    let c = controller(&dir.join("cfg"));
    let mut subs = Subscriptions::default();
    for line in data.split(|&b| b == b'\n').take(64) {
        match serde_json::from_slice::<Value>(line) {
            Ok(Value::Array(items)) => {
                for v in items.into_iter().take(64) {
                    request(&c, &mut subs, v);
                }
            }
            Ok(v) => request(&c, &mut subs, v),
            Err(_) => {}
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file to import settings from: the first byte says whether it is
/// named .zip or .json and how what is taken is handled; the rest is the
/// file. It is looked at, then imported, as the Import window does.
pub fn import(data: &[u8]) {
    watch_panics();
    let Some((&first, file)) = data.split_first() else {
        return;
    };
    let dir = scratch();
    let path = dir.join(if first & 1 == 0 {
        "settings.zip"
    } else {
        "settings.json"
    });
    std::fs::write(&path, file).expect("writing the file to import");
    let c = controller(&dir.join("cfg"));
    let mut subs = Subscriptions::default();
    let path_text = path.display().to_string();
    let _ = c.handle(
        Request::InspectImport(InspectImportParams {
            path: path_text.clone(),
        }),
        &mut subs,
    );
    let params = ImportParams {
        path: path_text,
        items: None,
        when_taken: if first & 2 == 0 {
            WhenTaken::KeepBoth
        } else {
            WhenTaken::Replace
        },
        hotkeys: if first & 4 == 0 {
            HotkeyImport::Add
        } else {
            HotkeyImport::ReplaceAll
        },
        ..Default::default()
    };
    let _ = c.handle(Request::ImportSettings(params), &mut subs);
    holds_together(&c.mixer(), "an import");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Weir's own files, as damaged as made up: the first byte picks which
/// file the rest is, which the daemon then starts from and uses.
pub fn config(data: &[u8]) {
    watch_panics_all(true);
    let Some((&first, file)) = data.split_first() else {
        return;
    };
    let dir = scratch();
    let cfg = dir.join("cfg");
    let which = [
        "config.toml",
        "hotkeys.json",
        "meter-colors.json",
        "eq-presets.toml",
        "seen-apps.json",
        "setups/Fuzz.toml",
        "scenes/Fuzz.toml",
    ];
    let name = which[first as usize % which.len()];
    let path = cfg.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("making the folder");
    }
    std::fs::write(&path, file).expect("writing the file");
    let c = controller(&cfg);
    holds_together(&c.mixer(), "starting");
    let mut subs = Subscriptions::default();
    let named = || NameParams {
        name: "Fuzz".into(),
    };
    for req in [
        Request::GetState,
        Request::ListHotkeys,
        Request::ListSeenApps,
        Request::LoadSetup(LoadSetupParams {
            name: "Fuzz".into(),
            scene: Some("Fuzz".into()),
        }),
        Request::LoadScene(named()),
        Request::SaveScene(named()),
        Request::SaveSetup(named()),
    ] {
        let _ = c.handle(req, &mut subs);
        holds_together(&c.mixer(), name);
    }
    c.save_if_dirty();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The kinds of sound file Weir plays for hotkeys, by name.
const SOUND_EXTS: [&str; 4] = ["wav", "mp3", "ogg", "flac"];

/// A sound file: the first byte picks what it is named, the rest is the
/// file.
pub fn sound(data: &[u8]) {
    watch_panics();
    let Some((&first, file)) = data.split_first() else {
        return;
    };
    let ext = SOUND_EXTS[first as usize % SOUND_EXTS.len()];
    if let Ok(sound) = weir_daemon::sounds::decode_bytes(file.to_vec(), ext) {
        assert!(sound.rate > 0, "a sound with no rate");
        assert!(!sound.channels.is_empty(), "a sound with no channels");
    }
}

/// Other text from outside, the first byte picking what it is taken for.
pub fn text(data: &[u8]) {
    watch_panics_all(true);
    let Some((&first, rest)) = data.split_first() else {
        return;
    };
    let text = String::from_utf8_lossy(rest);
    let text = text.as_ref();
    let (one, two) = text.split_once('\n').unwrap_or((text, ""));
    let m = weir_daemon::defaults::default_mixer();
    match first % 14 {
        0 => {
            // Keys typed for a hotkey: what is accepted is written back
            // the same way.
            if let Ok(k) = KeyCombo::parse(text) {
                let again = KeyCombo::parse(&k.to_string()).expect("keys written back");
                assert_eq!(again, k, "keys read back differently");
            }
        }
        1 => {
            if let Some(layout) = ChannelLayout::parse(text) {
                assert!(layout.channel_count() > 0, "a layout with no channels");
            }
        }
        2 => {
            // A recordings' name pattern, as the recorder fills it in.
            if record_names_problem(text).is_none() {
                for (layout, format) in [
                    (RecordLayout::Files, RecordFormat::Flac),
                    (RecordLayout::Multitrack, RecordFormat::Opus),
                ] {
                    let parts = NameParts {
                        pattern: text,
                        layout,
                        format,
                        when: LocalTime {
                            year: 2026,
                            month: 11,
                            day: 3,
                            hour: 21,
                            minute: 42,
                            second: 7,
                        },
                        number: u32::MAX,
                        part: 2,
                        splits: true,
                        setup: Some(two),
                    };
                    for f in recording_files(&parts, &[one, two, "Mic"]) {
                        assert!(!f.is_empty(), "an empty file name");
                        assert!(!f.starts_with('/'), "a file name that leaves the folder");
                        assert!(
                            !f.split('/').any(|p| p == ".." || p == "."),
                            "a file name that leaves the folder: {f}"
                        );
                    }
                }
            }
        }
        3 => {
            if let Some(rgb) = parse_hex_color(text) {
                assert_eq!(parse_hex_color(&hex_color(rgb)), Some(rgb));
            }
            let _ = parse_color(text);
        }
        4 => {
            let _ = is_newer(one, two);
            let _ = version_numbers(text);
        }
        5 => {
            let _ = weir_daemon::updates::parse(text);
        }
        6 => {
            let _ = weir_daemon::display::session_from_manager(text);
        }
        7 => {
            // A crash report about anything: hiding personal details must
            // never panic, and must hide the user name.
            let report = crash::Report {
                message: one.into(),
                at: two.into(),
                part: text.into(),
                log: text.lines().map(str::to_string).collect(),
                settings: vec![("folder".into(), one.into())],
                private: crash::Private {
                    home: Some("/home/sam".into()),
                    user: Some("sam".into()),
                },
                ..Default::default()
            };
            let (shown, _) = report.text(true);
            let title = report.title(true);
            let _ = crash::issue_link("https://github.com/Mourty/Weir", &title, &shown);
        }
        8 => {
            let taken: Vec<&str> = two.lines().collect();
            let _ = free_name(one, 64, |n| taken.contains(&n));
            let _ = library_name_problem(text);
            let _ = sound_name_problem(text);
        }
        9 => {
            if let Ok(h) = serde_json::from_str::<Hotkey>(text) {
                let _ = describe_hotkey(&h, &m);
            }
        }
        10 => {
            if let Ok(scene) = serde_json::from_str::<Scene>(text) {
                let mut m = m.clone();
                scene.apply(&mut m);
                m.normalize();
                holds_together(&m, "a scene");
            }
        }
        11 => {
            if let Ok(setup) = serde_json::from_str::<Setup>(text) {
                let mut m = setup.mixer();
                m.normalize();
                holds_together(&m, "a setup");
            }
        }
        12 => {
            // A request's strips and buses, named or by id.
            if let Ok(mut params) = serde_json::from_str::<Value>(two) {
                targets_by_name(one, &mut params, &m);
                targets_by_id(one, &mut params, &m);
            }
        }
        _ => {
            let _ = device_description(text);
            let _ = mix_level_label(f32::from_bits(
                rest.iter()
                    .fold(0u32, |a, &b| a.rotate_left(8) ^ u32::from(b)),
            ));
        }
    }
}

/// Reads made-up values out of the fuzzer's bytes.
struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn byte(&mut self) -> u8 {
        match self.0.split_first() {
            Some((&b, rest)) => {
                self.0 = rest;
                b
            }
            None => 0,
        }
    }
}

/// Numbers that tend to find trouble: zero both ways, the edges of every
/// range Weir has, and the far ends of what an f32 holds.
const ODD_NUMBERS: [f64; 24] = [
    0.0, -0.0, 1.0, -1.0, 0.5, 2.0, -6.0, 6.0, 12.0, -12.0, 24.0, -24.0, -96.0, -144.0, 20.0,
    20_000.0, 24_000.0, 0.01, 1e-30, 1e30, -1e30, 3.4e38, 100.0, 1000.0,
];

/// Whole numbers likewise: ids, counts, channels.
const ODD_COUNTS: [u64; 12] = [0, 1, 2, 3, 4, 7, 8, 16, 64, 255, 65_535, 4_294_967_295];

/// Layouts a strip or bus may have, as written.
const LAYOUTS: [&str; 10] = [
    "mono",
    "stereo",
    "quad",
    "5.1",
    "7.1",
    "FL FR LFE",
    "FL FR LFE RL RR",
    "FC",
    "FL FR FC LFE SL SR",
    "LFE",
];

/// `v` with its leaves changed as the bytes say: numbers to odd ones,
/// switches flipped, layouts swapped.
fn mutate(v: &mut Value, key: &str, b: &mut Bytes) {
    match v {
        Value::Number(n) => {
            let pick = b.byte();
            if pick < 32 {
                return;
            }
            if n.is_u64() {
                *v = Value::from(ODD_COUNTS[pick as usize % ODD_COUNTS.len()]);
            } else if pick < 128 {
                let x = ODD_NUMBERS[pick as usize % ODD_NUMBERS.len()];
                *v = serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number);
            } else {
                let bits = u32::from_le_bytes([b.byte(), b.byte(), b.byte(), b.byte()]);
                let x = f32::from_bits(bits);
                if x.is_finite() {
                    *v = serde_json::Number::from_f64(f64::from(x))
                        .map_or(Value::Null, Value::Number);
                }
            }
        }
        Value::Bool(on) => *on = b.byte() & 1 == 1,
        Value::String(s) if key == "layout" => {
            *s = LAYOUTS[b.byte() as usize % LAYOUTS.len()].into();
        }
        Value::Array(items) => items.iter_mut().for_each(|i| mutate(i, key, b)),
        Value::Object(map) => {
            for (k, item) in map.iter_mut() {
                mutate(item, k, b);
            }
        }
        _ => {}
    }
}

/// Every effect of every strip and bus switched on, so the bytes reach
/// all their settings.
fn everything_on(v: &mut Value) {
    match v {
        Value::Object(map) => {
            if let Some(on) = map.get_mut("enabled") {
                *on = Value::Bool(true);
            }
            map.values_mut().for_each(everything_on);
        }
        Value::Array(items) => items.iter_mut().for_each(everything_on),
        _ => {}
    }
}

/// A mixer made up from the bytes, from Weir's first-run mixer with
/// everything on, played by the mixing code before and after a change.
pub fn mixer(data: &[u8]) {
    watch_panics();
    let mut b = Bytes(data);
    let solo = match b.byte() % 3 {
        0 => SoloMode::Exclusive,
        1 => SoloMode::Cue(1),
        _ => SoloMode::Cue(99),
    };
    let frames = 1 + usize::from(b.byte()) * 8;
    let level = [0.0, 0.25, 1.0, -1.0, 8.0][usize::from(b.byte()) % 5];
    let mut before = weir_daemon::defaults::default_mixer();
    for s in &mut before.strips {
        s.routes = before.buses.iter().map(|b| b.id).collect();
    }
    let mut template = serde_json::to_value(&before).expect("the mixer as JSON");
    everything_on(&mut template);
    let mut changed = template.clone();
    mutate(&mut changed, "", &mut b);
    let Ok(mut after) = serde_json::from_value::<MixerState>(changed) else {
        return;
    };
    after.normalize();
    holds_together(&after, "normalizing a made-up mixer");
    let Ok(mut before) = serde_json::from_value::<MixerState>(template) else {
        return;
    };
    before.normalize();
    let finite = weir_engine::fuzzing::play(&before, &after, solo, frames, 3, level);
    assert!(finite, "the mixer made sound that is not a number");
}
