//! What each command does: build the request, send it, print the answer.
//!
//! Every command prints the daemon's JSON answer as it is when `json` is
//! set, and something for people otherwise.

use crate::args::{BusArgs, EqCmd, EqTarget, LibraryCmd, StripArgs};
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
    let patch = BusPatch {
        id: find_bus(&st.mixer, &a.bus)?,
        name: a.name,
        gain_db: a.gain,
        gain_delta_db: a.gain_by,
        mute: flag(a.mute.as_deref())?,
        mono: flag(a.mono.as_deref())?,
        layout: a.layout.as_deref().map(parse_layout).transpose()?,
        device: device_patch(a.device),
        color: a.color.as_deref().map(parse_color_arg).transpose()?,
        eq: eq_switch(a.eq.as_deref())?,
        limiter: some_unless_default(limiter),
        downmix: some_unless_default(downmix),
    };
    call(c, &Request::SetBus(patch), json, |b: Bus| {
        show::bus(&b);
        Ok(())
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

/// Print notifications until interrupted: as JSON lines with `json`, and
/// meters as a line of levels otherwise.
pub fn watch(c: &mut Client, meters: bool, json: bool) -> Result<()> {
    let mut topics = vec![Topic::State, Topic::Devices, Topic::Apps, Topic::Engine];
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
