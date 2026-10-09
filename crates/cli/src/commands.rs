//! What each command does: build the request, send it, print the answer.
//!
//! Every command prints the daemon's JSON answer as it is when `json` is
//! set, and something for people otherwise.

use crate::args::{
    BusArgs, EqCmd, EqTarget, HotkeyCmd, HotkeyGroupCmd, HotkeyOpts, LibraryCmd, StripArgs,
};
use crate::client::Client;
use crate::parse::*;
use crate::show;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use weir_protocol::*;

/// Print `v` as pretty JSON.
pub fn print_json(v: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// Send `req`, then print the answer as JSON when `json` is set, or with
/// `human` otherwise.
fn call<T: serde::de::DeserializeOwned>(
    c: &mut Client,
    req: &Request,
    json: bool,
    human: impl FnOnce(T) -> Result<()>,
) -> Result<()> {
    let v = c.call(req)?;
    if json {
        return print_json(&v);
    }
    human(serde_json::from_value(v)?)
}

pub fn status(c: &mut Client, json: bool) -> Result<()> {
    let st = c.state()?;
    if json {
        return print_json(&st.engine);
    }
    show::status(&st);
    Ok(())
}

pub fn state(c: &mut Client, json: bool) -> Result<()> {
    let st = c.state()?;
    if json {
        return print_json(&st);
    }
    show::state(&st);
    Ok(())
}

pub fn devices(c: &mut Client, json: bool) -> Result<()> {
    call(c, &Request::ListDevices, json, |d: Vec<DeviceInfo>| {
        show::devices(&d);
        Ok(())
    })
}

pub fn apps(c: &mut Client, json: bool) -> Result<()> {
    if json {
        return print_json(&c.call(&Request::ListApps)?);
    }
    // The whole state, for the names of the strips the apps play into.
    let st = c.state()?;
    show::apps(&st.apps, &st.mixer);
    Ok(())
}

pub fn strip(c: &mut Client, a: StripArgs, json: bool) -> Result<()> {
    let st = c.state()?;
    let patch = strip_patch(a, &st.mixer)?;
    call(c, &Request::SetStrip(patch), json, |s: Strip| {
        show::strip(&s, &st);
        Ok(())
    })
}

/// The update a `strip` command asks for. Effects the command leaves alone
/// are left out of it altogether.
fn strip_patch(a: StripArgs, m: &MixerState) -> Result<StripPatch> {
    let (makeup_db, auto_makeup) =
        match a.comp_lift.as_deref() {
            None => (None, None),
            Some("auto") => (None, Some(true)),
            Some(v) => (
                Some(v.parse::<f32>().map_err(|_| {
                    anyhow!("--comp-lift takes a number of dB or \"auto\", got '{v}'")
                })?),
                Some(false),
            ),
        };
    let compressor = CompressorPatch {
        enabled: flag(a.comp.as_deref())?,
        threshold_db: a.comp_threshold,
        ratio: a.comp_ratio,
        attack_ms: a.comp_attack,
        release_ms: a.comp_release,
        makeup_db,
        auto_makeup,
    };
    let triggers = a
        .duck_when
        .as_deref()
        .map(|s| split_list(s).map(|k| find_strip(m, k)).collect())
        .transpose()?;
    let duck_buses = match a.duck_in.as_deref() {
        None => None,
        Some("all") => Some(Default::default()),
        Some(s) => Some(
            split_list(s)
                .map(|k| find_bus(m, k))
                .collect::<Result<_>>()?,
        ),
    };
    let ducking = DuckingPatch {
        enabled: flag(a.duck.as_deref())?,
        triggers,
        amount_db: a.duck_by.map(f32::abs),
        buses: duck_buses,
        threshold_db: a.duck_threshold,
        ..Default::default()
    };
    let gate = GatePatch {
        enabled: flag(a.gate.as_deref())?,
        threshold_db: a.gate_threshold,
        range_db: a.gate_range,
        attack_ms: a.gate_attack,
        hold_ms: a.gate_hold,
        release_ms: a.gate_release,
    };
    let denoise = DenoisePatch {
        enabled: flag(a.denoise.as_deref())?,
        amount: a.denoise_amount.map(|p| p / 100.0),
    };
    let insert = insert_patch(
        a.external_effects.as_deref(),
        a.external_effects_at.as_deref(),
        a.external_effects_fallback.as_deref(),
    )?;
    Ok(StripPatch {
        id: find_strip(m, &a.strip)?,
        name: a.name,
        gain_db: a.gain,
        gain_delta_db: a.gain_by,
        pan_delta: None,
        mute: flag(a.mute.as_deref())?,
        solo: flag(a.solo.as_deref())?,
        pan: a.pan,
        layout: a.layout.as_deref().map(parse_layout).transpose()?,
        device: device_patch(a.device),
        color: a.color.as_deref().map(parse_color_arg).transpose()?,
        sends: None,
        upmix: a.upmix.as_deref().map(parse_upmix).transpose()?,
        subwoofer: flag(a.subwoofer.as_deref())?,
        eq: eq_switch(a.eq.as_deref())?,
        compressor: some_unless_default(compressor),
        ducking: some_unless_default(ducking),
        gate: some_unless_default(gate),
        denoise: some_unless_default(denoise),
        insert: some_unless_default(insert),
    })
}

pub fn bus(c: &mut Client, a: BusArgs, json: bool) -> Result<()> {
    let st = c.state()?;
    let surround_db = match a.surround_level.as_deref() {
        None => None,
        Some("off") => Some(GAIN_MIN_DB),
        Some(v) => Some(
            v.parse()
                .map_err(|_| anyhow!("--surround-level takes a level in dB or off, got '{v}'"))?,
        ),
    };
    let limiter = LimiterPatch {
        enabled: flag(a.limiter.as_deref())?,
        ceiling_db: a.limiter_ceiling,
        release_ms: None,
    };
    let downmix = DownmixPatch {
        method: a.downmix.as_deref().map(parse_downmix).transpose()?,
        center_db: a.center_level,
        surround_db,
        lfe: flag(a.keep_lfe.as_deref())?,
    };
    let insert = insert_patch(
        a.external_effects.as_deref(),
        a.external_effects_at.as_deref(),
        a.external_effects_fallback.as_deref(),
    )?;
    let patch = BusPatch {
        id: find_bus(&st.mixer, &a.bus)?,
        name: a.name,
        gain_db: a.gain,
        gain_delta_db: a.gain_by,
        mute: flag(a.mute.as_deref())?,
        mono: flag(a.mono.as_deref())?,
        delay_ms: a.delay,
        delay_delta_ms: a.delay_by,
        layout: a.layout.as_deref().map(parse_layout).transpose()?,
        device: device_patch(a.device),
        color: a.color.as_deref().map(parse_color_arg).transpose()?,
        eq: eq_switch(a.eq.as_deref())?,
        limiter: some_unless_default(limiter),
        downmix: some_unless_default(downmix),
        insert: some_unless_default(insert),
    };
    call(c, &Request::SetBus(patch), json, |b: Bus| {
        show::bus(&b, &st);
        Ok(())
    })
}

/// An external effects update from `--external-effects`, `-at` and
/// `-fallback`.
fn insert_patch(on: Option<&str>, at: Option<&str>, fallback: Option<&str>) -> Result<InsertPatch> {
    Ok(InsertPatch {
        enabled: flag(on)?,
        position: at.map(parse_insert_point).transpose()?,
        fallback: fallback.map(parse_insert_fallback).transpose()?,
    })
}

/// `Some(patch)`, unless it changes nothing.
fn some_unless_default<T: Default + PartialEq>(patch: T) -> Option<T> {
    (patch != T::default()).then_some(patch)
}

/// An equalizer on/off switch, as an equalizer update.
fn eq_switch(on: Option<&str>) -> Result<Option<EqPatch>> {
    Ok(flag(on)?.map(|v| EqPatch {
        enabled: Some(v),
        bands: None,
    }))
}

pub fn route(
    c: &mut Client,
    strip: &str,
    bus: &str,
    action: Option<&str>,
    level: Option<f32>,
    level_by: Option<f32>,
    json: bool,
) -> Result<()> {
    let st = c.state()?;
    let req = Request::SetRoute(RouteParams {
        strip: find_strip(&st.mixer, strip)?,
        bus: find_bus(&st.mixer, bus)?,
        enabled: flag(action)?,
        level_db: level,
        level_delta_db: level_by,
    });
    call(c, &req, json, |s: Strip| {
        let routes = show::routes(&s, &st.mixer);
        println!(
            "strip '{}' now routes to: {}",
            s.name,
            if routes.is_empty() {
                "(nothing)".into()
            } else {
                routes.join(", ")
            }
        );
        Ok(())
    })
}

pub fn add_strip(
    c: &mut Client,
    name: String,
    kind: &str,
    layout: &str,
    device: Option<String>,
    routes: &[String],
    json: bool,
) -> Result<()> {
    let st = c.state()?;
    let routes = routes
        .iter()
        .map(|r| find_bus(&st.mixer, r))
        .collect::<Result<Vec<_>>>()?;
    let req = Request::AddStrip(AddStripParams {
        name,
        kind: parse_strip_kind(kind)?,
        layout: parse_layout(layout)?,
        device,
        routes,
    });
    call(c, &req, json, |s: Strip| {
        println!("added strip {} '{}'", s.id, s.name);
        Ok(())
    })
}

pub fn remove_strip(c: &mut Client, strip: &str) -> Result<()> {
    let st = c.state()?;
    let id = find_strip(&st.mixer, strip)?;
    c.call(&Request::RemoveStrip(IdParams { id }))?;
    println!("removed strip '{strip}'");
    Ok(())
}

pub fn add_bus(
    c: &mut Client,
    name: String,
    kind: &str,
    layout: &str,
    device: Option<String>,
    json: bool,
) -> Result<()> {
    let req = Request::AddBus(AddBusParams {
        name,
        kind: parse_bus_kind(kind)?,
        layout: parse_layout(layout)?,
        device,
    });
    call(c, &req, json, |b: Bus| {
        println!("added bus {} '{}'", b.id, b.name);
        Ok(())
    })
}

pub fn remove_bus(c: &mut Client, bus: &str) -> Result<()> {
    let st = c.state()?;
    let id = find_bus(&st.mixer, bus)?;
    c.call(&Request::RemoveBus(IdParams { id }))?;
    println!("removed bus '{bus}'");
    Ok(())
}

pub fn move_app(c: &mut Client, app: u32, strip: &str) -> Result<()> {
    let st = c.state()?;
    let strip_id = find_strip(&st.mixer, strip)?;
    c.call(&Request::MoveApp(MoveAppParams {
        app,
        strip: strip_id,
    }))?;
    println!("moved app {app} to strip '{strip}'");
    Ok(())
}

pub fn app_volume(
    c: &mut Client,
    app: u32,
    gain: Option<f32>,
    gain_by: Option<f32>,
    mute: Option<&str>,
) -> Result<()> {
    let mute = flag(mute)?;
    if gain.is_none() && gain_by.is_none() && mute.is_none() {
        bail!("pass --gain, --gain-by or --mute");
    }
    c.call(&Request::SetAppVolume(AppVolumeParams {
        app,
        volume_db: gain,
        volume_delta_db: gain_by,
        mute,
    }))?;
    let what: Vec<String> = [
        gain.map(|g| format!("{g:+.1} dB")),
        gain_by.map(|g| format!("{g:+.1} dB from before")),
        mute.map(|m| match m {
            Flag::Set(true) => "muted".into(),
            Flag::Set(false) => "unmuted".into(),
            Flag::Toggle => "mute toggled".into(),
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    println!("app {app}: {}", what.join(", "));
    Ok(())
}

pub fn show_window(c: &mut Client) -> Result<()> {
    c.call(&Request::ShowWindow)?;
    println!("asked for the mixer window");
    Ok(())
}

pub fn rules(c: &mut Client, json: bool) -> Result<()> {
    let st = c.state()?;
    if json {
        return print_json(&st.app_rules);
    }
    show::rules(&st);
    Ok(())
}

/// Put `app` on `strip` (or leave it alone, for `leave`) whenever it starts
/// playing, replacing any rule it had.
pub fn rule(c: &mut Client, app: String, strip: &str, json: bool) -> Result<()> {
    let st = c.state()?;
    let target = if strip == "leave" {
        None
    } else {
        Some(find_strip(&st.mixer, strip)?)
    };
    let mut rules = st.app_rules;
    match rules.iter_mut().find(|r| r.app.eq_ignore_ascii_case(&app)) {
        Some(r) => r.strip = target,
        None => rules.push(AppRule { app, strip: target }),
    }
    set_rules(c, rules, json)
}

/// Forget `app`'s rule.
pub fn unrule(c: &mut Client, app: &str, json: bool) -> Result<()> {
    let mut rules = c.state()?.app_rules;
    let before = rules.len();
    rules.retain(|r| !r.app.eq_ignore_ascii_case(app));
    if rules.len() == before {
        bail!("no rule for '{app}'");
    }
    set_rules(c, rules, json)
}

fn set_rules(c: &mut Client, rules: Vec<AppRule>, json: bool) -> Result<()> {
    let v = c.call(&Request::SetAppRules(AppRulesParams { rules }))?;
    if json {
        return print_json(&v);
    }
    show::rules(&c.state()?);
    Ok(())
}

pub fn move_strip(c: &mut Client, strip: &str, pos: usize, json: bool) -> Result<()> {
    let st = c.state()?;
    let req = Request::MoveStrip(MoveParams {
        id: find_strip(&st.mixer, strip)?,
        index: position(pos)?,
    });
    call(c, &req, json, |order: Vec<u32>| {
        let names: Vec<&str> = order
            .iter()
            .filter_map(|id| st.mixer.strip(*id).map(|s| s.name.as_str()))
            .collect();
        println!("strips: {}", names.join(", "));
        Ok(())
    })
}

pub fn move_bus(c: &mut Client, bus: &str, pos: usize, json: bool) -> Result<()> {
    let st = c.state()?;
    let req = Request::MoveBus(MoveParams {
        id: find_bus(&st.mixer, bus)?,
        index: position(pos)?,
    });
    call(c, &req, json, |order: Vec<u32>| {
        let names: Vec<&str> = order
            .iter()
            .filter_map(|id| st.mixer.bus(*id).map(|b| b.name.as_str()))
            .collect();
        println!("buses: {}", names.join(", "));
        Ok(())
    })
}

/// Undo (`back`) or redo `steps` steps, saying which.
pub fn step_history(c: &mut Client, steps: u32, back: bool, json: bool) -> Result<()> {
    let before: HistoryInfo = serde_json::from_value(c.call(&Request::History)?)?;
    let params = HistoryStepParams { steps: Some(steps) };
    let v = c.call(&if back {
        Request::Undo(params)
    } else {
        Request::Redo(params)
    })?;
    if json {
        return print_json(&v);
    }
    let list = if back { &before.undo } else { &before.redo };
    for e in list.iter().take(steps as usize) {
        println!("{} {}", if back { "undid" } else { "redid" }, e.label);
    }
    Ok(())
}

pub fn history(c: &mut Client, json: bool) -> Result<()> {
    call(c, &Request::History, json, |h: HistoryInfo| {
        show::history(&h);
        Ok(())
    })
}

/// The settings a `settings` command can change.
pub struct SettingsArgs {
    pub solo: Option<String>,
    pub rate: Option<String>,
    pub buffer: Option<String>,
    pub meter_rate: Option<u32>,
    pub startup: Option<String>,
    pub start_at_login: Option<String>,
    pub tray: Option<String>,
    pub tray_icon: Option<String>,
}

/// Change the settings the command names, then show them all.
pub fn settings(c: &mut Client, a: SettingsArgs, json: bool) -> Result<()> {
    let st = c.state()?;
    let patch = SettingsPatch {
        meter_rate_hz: a.meter_rate,
        startup: a.startup.as_deref().map(parse_startup).transpose()?,
        start_at_login: flag(a.start_at_login.as_deref())?,
        tray: a.tray.as_deref().map(parse_bool).transpose()?,
        tray_icon: a.tray_icon.as_deref().map(parse_tray_icon).transpose()?,
        solo: match a.solo.as_deref() {
            None => None,
            Some("exclusive") => Some(SoloMode::Exclusive),
            Some(b) => Some(SoloMode::Cue(find_bus(&st.mixer, b)?)),
        },
        sample_rate: number_or_auto(a.rate.as_deref(), "rate")?,
        quantum: number_or_auto(a.buffer.as_deref(), "buffer")?,
    };
    let settings: Settings = if patch == SettingsPatch::default() {
        st.settings.clone()
    } else {
        serde_json::from_value(c.call(&Request::SetSettings(patch))?)?
    };
    if json {
        return print_json(&settings);
    }
    show::settings(&settings, &st);
    Ok(())
}

/// List, save, load or delete a scene (`scene`) or setup.
pub fn library(c: &mut Client, action: LibraryCmd, scene: bool, json: bool) -> Result<()> {
    let word = if scene { "scene" } else { "setup" };
    let (req, done) = match action {
        LibraryCmd::List => {
            let lib = c.state()?.library;
            if json {
                return print_json(&lib);
            }
            let (names, current) = if scene {
                (&lib.scenes, &lib.scene)
            } else {
                (&lib.setups, &lib.setup)
            };
            if names.is_empty() {
                println!("(no {word}s)");
            }
            for n in names {
                let mark = if current.as_deref() == Some(n.as_str()) {
                    "*"
                } else {
                    " "
                };
                println!("{mark} {n}");
            }
            return Ok(());
        }
        LibraryCmd::Save { name } => {
            let done = format!("saved {word} '{name}'");
            let p = NameParams { name };
            let req = if scene {
                Request::SaveScene(p)
            } else {
                Request::SaveSetup(p)
            };
            (req, done)
        }
        LibraryCmd::Load { name } => {
            let done = format!("loaded {word} '{name}'");
            let p = NameParams { name };
            let req = if scene {
                Request::LoadScene(p)
            } else {
                Request::LoadSetup(p)
            };
            (req, done)
        }
        LibraryCmd::Delete { name } => {
            let done = format!("deleted {word} '{name}'");
            let p = NameParams { name };
            let req = if scene {
                Request::DeleteScene(p)
            } else {
                Request::DeleteSetup(p)
            };
            (req, done)
        }
    };
    let v = c.call(&req)?;
    if json {
        return print_json(&v);
    }
    println!("{done}");
    Ok(())
}

/// Which strip or bus an `eq` command is about.
enum Target {
    Strip(StripId),
    Bus(BusId),
}

impl EqTarget {
    fn resolve(&self, state: &MixerState) -> Result<Target> {
        match (&self.strip, &self.bus) {
            (Some(s), None) => Ok(Target::Strip(find_strip(state, s)?)),
            (None, Some(b)) => Ok(Target::Bus(find_bus(state, b)?)),
            _ => bail!("give exactly one of --strip or --bus"),
        }
    }
}

/// The name and equalizer of `target`, which was resolved in `state`.
fn target_eq<'a>(state: &'a MixerState, target: &Target) -> (&'a str, &'a Equalizer) {
    match *target {
        Target::Strip(id) => {
            let s = state.strip(id).expect("resolved above");
            (&s.name, &s.eq)
        }
        Target::Bus(id) => {
            let b = state.bus(id).expect("resolved above");
            (&b.name, &b.eq)
        }
    }
}

pub fn eq(c: &mut Client, action: EqCmd, json: bool) -> Result<()> {
    match action {
        EqCmd::Presets => call(c, &Request::ListEqPresets, json, |p: Vec<EqPreset>| {
            show::eq_presets(&p);
            Ok(())
        }),
        EqCmd::Show { target } => {
            let st = c.state()?;
            let target = target.resolve(&st.mixer)?;
            let (name, eq) = target_eq(&st.mixer, &target);
            if json {
                return print_json(eq);
            }
            println!(
                "equalizer of '{name}': {}",
                if eq.enabled { "on" } else { "off" }
            );
            show::bands(&eq.bands);
            Ok(())
        }
        EqCmd::Apply { preset, target } => {
            let st = c.state()?;
            let (strip, bus) = match target.resolve(&st.mixer)? {
                Target::Strip(id) => (Some(id), None),
                Target::Bus(id) => (None, Some(id)),
            };
            let req = Request::ApplyEqPreset(ApplyEqPresetParams {
                name: preset.clone(),
                strip,
                bus,
            });
            call(c, &req, json, |_: Value| {
                println!("applied '{preset}' and switched the equalizer on");
                Ok(())
            })
        }
        EqCmd::Save { name, target } => {
            let st = c.state()?;
            let target = target.resolve(&st.mixer)?;
            let (_, eq) = target_eq(&st.mixer, &target);
            let req = Request::SaveEqPreset(SaveEqPresetParams {
                name: name.clone(),
                bands: eq.bands.clone(),
            });
            call(c, &req, json, |_: Value| {
                println!("saved preset '{name}' with {} bands", eq.bands.len());
                Ok(())
            })
        }
        EqCmd::Delete { name } => {
            let req = Request::DeleteEqPreset(NameParams { name: name.clone() });
            call(c, &req, json, |_: Value| {
                println!("deleted preset '{name}'");
                Ok(())
            })
        }
    }
}

/// List the hotkeys, and how keys reach Weir.
pub fn hotkeys(c: &mut Client, json: bool) -> Result<()> {
    let st = c.state()?;
    call(c, &Request::ListHotkeys, json, |info: HotkeysInfo| {
        show::hotkeys(&info, &st.mixer);
        Ok(())
    })
}

/// Apply the settings in `opts` to `h`. Steps given replace its steps.
fn apply_hotkey_opts(c: &mut Client, h: &mut Hotkey, opts: HotkeyOpts) -> Result<()> {
    if let Some(group) = &opts.group {
        h.group = find_group(c, group)?.map_or(0, |g| g.id);
    }
    if !opts.keys.is_empty() {
        h.keys = opts.keys;
    }
    if !opts.steps.is_empty() {
        h.steps = opts
            .steps
            .iter()
            .map(|s| parse_step(s))
            .collect::<Result<_>>()?;
    }
    if let Some(e) = opts.each_press {
        h.each_press = parse_each_press(&e)?;
    }
    if !opts.release_steps.is_empty() {
        h.release_steps = opts
            .release_steps
            .iter()
            .map(|s| parse_step(s))
            .collect::<Result<_>>()?;
        h.on_release = OnRelease::Steps;
    }
    if let Some(r) = opts.release {
        h.on_release = parse_on_release(&r)?;
    }
    match opts.repeat {
        Some(0) => h.repeat_ms = None,
        Some(ms) => h.repeat_ms = Some(ms),
        None => {}
    }
    if let Some(f) = flag(opts.enabled.as_deref())? {
        h.enabled = f.apply(h.enabled);
    }
    Ok(())
}

/// Add, change, remove or press a hotkey.
pub fn hotkey(c: &mut Client, action: HotkeyCmd, json: bool) -> Result<()> {
    // Press, let go, run and remove: by id, and say which by name.
    let act = |c: &mut Client, hotkey: &str, make: fn(HotkeyRef) -> Request, done: &str| {
        let h = find_hotkey(c, hotkey)?;
        let req = make(HotkeyRef {
            hotkey: HotkeyKey::Id(h.id),
        });
        call(c, &req, json, |_: Value| {
            println!("{done} the hotkey '{}'", h.name);
            Ok(())
        })
    };
    match action {
        HotkeyCmd::Add { name, opts } => {
            let mut h = Hotkey {
                id: 0,
                name,
                enabled: true,
                group: 0,
                keys: Vec::new(),
                steps: Vec::new(),
                each_press: EachPress::All,
                on_release: OnRelease::Nothing,
                release_steps: Vec::new(),
                repeat_ms: None,
            };
            apply_hotkey_opts(c, &mut h, opts)?;
            save_hotkey(c, &h, "added", json)
        }
        HotkeyCmd::Change {
            hotkey,
            name,
            no_keys,
            add_keys,
            opts,
        } => {
            let mut h = find_hotkey(c, &hotkey)?;
            if let Some(name) = name {
                h.name = name;
            }
            if no_keys {
                h.keys.clear();
            }
            apply_hotkey_opts(c, &mut h, opts)?;
            h.keys.extend(add_keys);
            save_hotkey(c, &h, "changed", json)
        }
        HotkeyCmd::Remove { hotkey } => act(c, &hotkey, Request::RemoveHotkey, "removed"),
        HotkeyCmd::Run { hotkey } => act(c, &hotkey, Request::RunHotkey, "ran"),
        HotkeyCmd::Press { hotkey } => act(c, &hotkey, Request::PressHotkey, "pressed"),
        HotkeyCmd::Release { hotkey } => act(c, &hotkey, Request::ReleaseHotkey, "let go of"),
        HotkeyCmd::On { hotkey } => switch_hotkey(c, &hotkey, Flag::Set(true), json),
        HotkeyCmd::Off { hotkey } => switch_hotkey(c, &hotkey, Flag::Set(false), json),
        HotkeyCmd::Toggle { hotkey } => switch_hotkey(c, &hotkey, Flag::Toggle, json),
        HotkeyCmd::Move { hotkey, to, group } => {
            let h = find_hotkey(c, &hotkey)?;
            let group = match group {
                Some(g) => Some(find_group(c, &g)?),
                None => None,
            };
            let req = Request::MoveHotkey(MoveHotkeyParams {
                hotkey: HotkeyKey::Id(h.id),
                group: group
                    .as_ref()
                    .map(|g| HotkeyGroupKey::Id(g.as_ref().map_or(0, |g| g.id))),
                index: to,
            });
            call(c, &req, json, |_: Value| {
                match group {
                    Some(Some(g)) => println!("moved the hotkey '{}' into '{}'", h.name, g.name),
                    Some(None) => println!("moved the hotkey '{}' out of its group", h.name),
                    None => println!("moved the hotkey '{}'", h.name),
                }
                Ok(())
            })
        }
        HotkeyCmd::Group { action } => hotkey_group(c, action, json),
        HotkeyCmd::Settings => call(c, &Request::OpenShortcutSettings, json, |_: Value| {
            println!("opened your desktop's shortcut settings");
            Ok(())
        }),
    }
}

/// Switch a hotkey's keys on or off, and say which.
fn switch_hotkey(c: &mut Client, hotkey: &str, enabled: Flag, json: bool) -> Result<()> {
    let h = find_hotkey(c, hotkey)?;
    let req = Request::SwitchHotkey(SwitchHotkeyParams {
        hotkey: HotkeyKey::Id(h.id),
        enabled,
    });
    call(c, &req, json, |h: Hotkey| {
        let state = if h.enabled { "on" } else { "off" };
        println!("switched the hotkey '{}' {state}", h.name);
        Ok(())
    })
}

/// Add, switch, rename, move or remove a group of hotkeys.
fn hotkey_group(c: &mut Client, action: HotkeyGroupCmd, json: bool) -> Result<()> {
    // The group named on the command line; "none" is not one here.
    let group = |c: &mut Client, text: &str| -> Result<HotkeyGroup> {
        find_group(c, text)?.ok_or_else(|| anyhow!("'{text}' is no group"))
    };
    let switch = |c: &mut Client, g: HotkeyGroup, enabled: Flag| {
        let req = Request::SetHotkeyGroup(SetHotkeyGroupParams {
            group: HotkeyGroupKey::Id(g.id),
            name: None,
            enabled: Some(enabled),
        });
        call(c, &req, json, |g: HotkeyGroup| {
            let state = if g.enabled { "on" } else { "off" };
            println!("switched the hotkey group '{}' {state}", g.name);
            Ok(())
        })
    };
    match action {
        HotkeyGroupCmd::Add { name, off } => {
            let req = Request::AddHotkeyGroup(AddHotkeyGroupParams {
                name,
                enabled: !off,
            });
            call(c, &req, json, |g: HotkeyGroup| {
                println!("added the hotkey group '{}'", g.name);
                Ok(())
            })
        }
        HotkeyGroupCmd::On { group: g } => {
            let g = group(c, &g)?;
            switch(c, g, Flag::Set(true))
        }
        HotkeyGroupCmd::Off { group: g } => {
            let g = group(c, &g)?;
            switch(c, g, Flag::Set(false))
        }
        HotkeyGroupCmd::Toggle { group: g } => {
            let g = group(c, &g)?;
            switch(c, g, Flag::Toggle)
        }
        HotkeyGroupCmd::Rename { group: g, name } => {
            let g = group(c, &g)?;
            let req = Request::SetHotkeyGroup(SetHotkeyGroupParams {
                group: HotkeyGroupKey::Id(g.id),
                name: Some(name),
                enabled: None,
            });
            call(c, &req, json, |new: HotkeyGroup| {
                println!("renamed the hotkey group '{}' to '{}'", g.name, new.name);
                Ok(())
            })
        }
        HotkeyGroupCmd::Move { group: g, to } => {
            let g = group(c, &g)?;
            let req = Request::MoveHotkeyGroup(MoveHotkeyGroupParams {
                group: HotkeyGroupKey::Id(g.id),
                index: to,
            });
            call(c, &req, json, |_: Value| {
                println!("moved the hotkey group '{}'", g.name);
                Ok(())
            })
        }
        HotkeyGroupCmd::Remove { group: g } => {
            let g = group(c, &g)?;
            let req = Request::RemoveHotkeyGroup(HotkeyGroupRef {
                group: HotkeyGroupKey::Id(g.id),
            });
            call(c, &req, json, |_: Value| {
                println!(
                    "removed the hotkey group '{}'; its hotkeys are in no group now",
                    g.name
                );
                Ok(())
            })
        }
    }
}

/// The group `text` names: the one with that id when it is a number, or
/// else the one with that name, ignoring case; none for "none" or 0.
fn find_group(c: &mut Client, text: &str) -> Result<Option<HotkeyGroup>> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("none") || text == "0" {
        return Ok(None);
    }
    let info: HotkeysInfo = serde_json::from_value(c.call(&Request::ListHotkeys)?)?;
    let id = text.parse::<HotkeyGroupId>().ok();
    info.groups
        .iter()
        .find(|g| Some(g.id) == id)
        .or_else(|| {
            info.groups
                .iter()
                .find(|g| g.name.eq_ignore_ascii_case(text))
        })
        .cloned()
        .map(Some)
        .ok_or_else(|| match id {
            Some(id) => anyhow!("no hotkey group with id {id}"),
            None => anyhow!("no hotkey group called '{text}'"),
        })
}

/// The hotkey `text` names: the one with that id when it is a number, or
/// else the one with that name, ignoring case.
fn find_hotkey(c: &mut Client, text: &str) -> Result<Hotkey> {
    let info: HotkeysInfo = serde_json::from_value(c.call(&Request::ListHotkeys)?)?;
    let text = text.trim();
    let id = text.parse::<HotkeyId>().ok();
    info.hotkeys
        .iter()
        .find(|h| Some(h.id) == id)
        .or_else(|| {
            info.hotkeys
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case(text))
        })
        .cloned()
        .ok_or_else(|| match id {
            Some(id) => anyhow!("no hotkey with id {id}"),
            None => anyhow!("no hotkey called '{text}'"),
        })
}

/// Send `h` to be saved, and say what it does.
fn save_hotkey(c: &mut Client, h: &Hotkey, done: &str, json: bool) -> Result<()> {
    let v = c.call(&Request::SetHotkey(h.clone()))?;
    if json {
        return print_json(&v);
    }
    let saved: Hotkey = serde_json::from_value(v)?;
    let mixer = c.state()?.mixer;
    let keys = match saved.keys.join(", ") {
        k if k.is_empty() => "no keys".to_string(),
        k => k,
    };
    println!("{done} the hotkey '{}' ({keys})", saved.name);
    println!("  {}", describe_hotkey(&saved, &mixer));
    Ok(())
}

/// Print notifications until interrupted: as JSON lines with `json`, and
/// meters as a line of levels otherwise.
pub fn watch(c: &mut Client, meters: bool, json: bool) -> Result<()> {
    let mut topics = vec![
        Topic::State,
        Topic::Devices,
        Topic::Apps,
        Topic::Engine,
        Topic::Hotkeys,
    ];
    if meters {
        topics.push(Topic::Meters);
    }
    c.call(&Request::Subscribe(SubscribeParams {
        topics,
        meter_rate_hz: None,
    }))?;
    loop {
        let line = c.next_line()?;
        if json {
            println!("{line}");
            continue;
        }
        match ServerMessage::parse(&line)? {
            ServerMessage::Notification(Notification::Meters(m)) => show::meters(&m),
            ServerMessage::Notification(n) => println!("{}", serde_json::to_string(&n)?),
            ServerMessage::Response(_) => {}
        }
    }
}

/// Send `method` with `params` as they are, and print the answer.
pub fn raw(c: &mut Client, method: &str, params: Option<&str>) -> Result<()> {
    let params = params
        .map(serde_json::from_str)
        .transpose()
        .context("params must be JSON")?;
    print_json(&c.call_raw(method, params)?)
}
