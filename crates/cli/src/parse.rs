//! Reading the words people type into protocol values: `on`, `toggle`,
//! `5.1`, `teal`, `A1`, `passive` and so on.

use anyhow::{anyhow, bail, Context, Result};
use weir_protocol::{
    format_color, parse_color, BusId, BusKind, ChannelLayout, DeviceInfo, DeviceKind,
    DownmixMethod, EachPress, Flag, HotkeyPopup, HotkeyStep, InsertFallback, InsertPoint,
    MixerState, OnRelease, Startup, StripId, StripKind, TrayIcon, Upmix, COLOR_PRESETS,
};

/// on or off, and the usual ways of saying them.
pub fn parse_bool(s: &str) -> Result<bool> {
    match s.to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" => Ok(true),
        "0" | "off" | "false" | "no" => Ok(false),
        _ => bail!("expected on/off, got '{s}'"),
    }
}

/// on, off or toggle.
pub fn parse_flag(s: &str) -> Result<Flag> {
    if s.eq_ignore_ascii_case("toggle") {
        Ok(Flag::Toggle)
    } else {
        Ok(Flag::Set(parse_bool(s).map_err(|_| {
            anyhow!("expected on, off or toggle, got '{s}'")
        })?))
    }
}

/// An optional on/off/toggle argument.
pub fn flag(s: Option<&str>) -> Result<Option<Flag>> {
    s.map(parse_flag).transpose()
}

/// A color given on the command line: a preset name, `#rrggbb` or `none`.
pub fn parse_color_arg(s: &str) -> Result<Option<String>> {
    if s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    if let Some((_, hex)) = COLOR_PRESETS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(s) || (s == "grey" && *name == "gray"))
    {
        return Ok(Some(hex.to_string()));
    }
    parse_color(s)
        .map(|rgb| Some(format_color(rgb)))
        .ok_or_else(|| anyhow!("unknown color '{s}': use a name, #rrggbb or none"))
}

/// A device argument: a `node.name`, or `none` (or nothing) to clear it.
pub fn device_patch(v: Option<String>) -> Option<Option<String>> {
    v.map(|d| {
        if d.eq_ignore_ascii_case("none") || d.is_empty() {
            None
        } else {
            Some(d)
        }
    })
}

/// The items of a comma-separated list.
pub fn split_list(s: &str) -> impl Iterator<Item = &str> {
    s.split(',').map(str::trim).filter(|s| !s.is_empty())
}

/// How a strip plays on buses with more speakers: off, center, all or
/// passive.
pub fn parse_upmix(s: &str) -> Result<Upmix> {
    match s.to_ascii_lowercase().replace('_', "-").as_str() {
        "off" | "front" => Ok(Upmix::Off),
        "center" | "front-center" => Ok(Upmix::Center),
        "all" | "all-channel-stereo" => Ok(Upmix::AllChannelStereo),
        "passive" | "passive-surround" | "psd" => Ok(Upmix::PassiveSurround),
        _ => bail!("expected off, center, all or passive, got '{s}'"),
    }
}

/// How a bus plays channels it has no speaker for.
pub fn parse_downmix(s: &str) -> Result<DownmixMethod> {
    Ok(match s.to_ascii_lowercase().replace('_', "-").as_str() {
        "standard" | "itu" | "lo-ro" | "loro" => DownmixMethod::Standard,
        "matrix" | "lt-rt" | "ltrt" => DownmixMethod::Matrix,
        "front-only" | "front" => DownmixMethod::FrontOnly,
        "center-only" | "center" => DownmixMethod::CenterOnly,
        "lfe-only" | "lfe" | "subwoofer-only" => DownmixMethod::LfeOnly,
        "surround-only" | "surrounds-only" | "surrounds" => DownmixMethod::SurroundOnly,
        _ => bail!(
            "expected standard, matrix, front-only, center-only, lfe-only or surround-only, got '{s}'"
        ),
    })
}

/// Where external effects go.
pub fn parse_insert_point(s: &str) -> Result<InsertPoint> {
    Ok(match s.to_ascii_lowercase().replace('_', "-").as_str() {
        "before-denoise" | "before-noise-suppression" => InsertPoint::BeforeDenoise,
        "before-gate" => InsertPoint::BeforeGate,
        "before-eq" | "before-equalizer" => InsertPoint::BeforeEq,
        "before-compressor" | "before-comp" => InsertPoint::BeforeCompressor,
        "before-fader" => InsertPoint::BeforeFader,
        "after-fader" => InsertPoint::AfterFader,
        "before-limiter" => InsertPoint::BeforeLimiter,
        "after-limiter" => InsertPoint::AfterLimiter,
        _ => bail!(
            "expected before-denoise, before-gate, before-eq, before-compressor, before-fader or after-fader for a strip, or before-eq, before-fader, before-limiter or after-limiter for a bus, got '{s}'"
        ),
    })
}

/// What a strip or bus plays while nothing comes back from its external
/// effects.
pub fn parse_insert_fallback(s: &str) -> Result<InsertFallback> {
    Ok(match s.to_ascii_lowercase().replace('_', "-").as_str() {
        "pass" | "pass-through" | "passthrough" | "through" => InsertFallback::PassThrough,
        "silence" | "silent" => InsertFallback::Silence,
        _ => bail!("expected pass or silence, got '{s}'"),
    })
}

/// A channel layout: a name such as `5.1`, or a list of positions.
pub fn parse_layout(s: &str) -> Result<ChannelLayout> {
    ChannelLayout::parse(s).ok_or_else(|| {
        anyhow!("unknown layout '{s}' (mono, stereo, quad, 5.1, 7.1 or e.g. \"FL FR\")")
    })
}

/// hardware or virtual, for a new strip.
pub fn parse_strip_kind(s: &str) -> Result<StripKind> {
    match s.to_ascii_lowercase().as_str() {
        "hardware" | "hw" => Ok(StripKind::Hardware),
        "virtual" | "v" => Ok(StripKind::Virtual),
        _ => bail!("kind must be hardware or virtual"),
    }
}

/// hardware or virtual, for a new bus.
pub fn parse_bus_kind(s: &str) -> Result<BusKind> {
    Ok(match parse_strip_kind(s)? {
        StripKind::Hardware => BusKind::Hardware,
        StripKind::Virtual => BusKind::Virtual,
    })
}

/// A number, or `auto` for 0: how settings say "let PipeWire decide".
pub fn number_or_auto(v: Option<&str>, option: &str) -> Result<Option<u32>> {
    match v {
        None => Ok(None),
        Some("auto") => Ok(Some(0)),
        Some(s) => s
            .parse()
            .map(Some)
            .map_err(|_| anyhow!("--{option} takes a number or \"auto\", got '{s}'")),
    }
}

/// What the daemon does about the window when it starts.
pub fn parse_startup(s: &str) -> Result<Startup> {
    match s {
        "window" => Ok(Startup::Window),
        "minimized" => Ok(Startup::Minimized),
        "tray-only" | "tray_only" => Ok(Startup::TrayOnly),
        _ => bail!("--startup takes window, minimized or tray-only, got '{s}'"),
    }
}

/// What pressing a hotkey shows.
pub fn parse_popup(s: &str) -> Result<HotkeyPopup> {
    match s {
        "popup" => Ok(HotkeyPopup::Popup),
        "notification" => Ok(HotkeyPopup::Notification),
        "nothing" | "none" => Ok(HotkeyPopup::Nothing),
        _ => bail!("--popup takes popup, notification or nothing, got '{s}'"),
    }
}

/// The `node.name` of the playback device called `s`, by name or by
/// description, ignoring case.
pub fn find_sink(devices: &[DeviceInfo], s: &str) -> Result<String> {
    let sinks = || devices.iter().filter(|d| d.kind == DeviceKind::Sink);
    sinks()
        .find(|d| d.name == s)
        .or_else(|| sinks().find(|d| d.description.eq_ignore_ascii_case(s)))
        .map(|d| d.name.clone())
        .with_context(|| format!("no playback device called '{s}' (see weirctl devices)"))
}

/// How the tray icon is drawn.
pub fn parse_tray_icon(s: &str) -> Result<TrayIcon> {
    match s {
        "color" => Ok(TrayIcon::Color),
        "one-color" | "one_color" => Ok(TrayIcon::OneColor),
        _ => bail!("--tray-icon takes color or one-color, got '{s}'"),
    }
}

/// A 1-based position on the command line, as the 0-based index the
/// protocol takes.
pub fn position(position: usize) -> Result<usize> {
    position.checked_sub(1).context("positions count from 1")
}

/// A strip by id or name.
pub fn find_strip(state: &MixerState, key: &str) -> Result<StripId> {
    state
        .find_strip(key)
        .map(|s| s.id)
        .ok_or_else(|| anyhow!("no strip called '{key}'"))
}

/// A bus by id, label (A1, B2) or name.
pub fn find_bus(state: &MixerState, key: &str) -> Result<BusId> {
    state
        .find_bus(key)
        .map(|b| b.id)
        .ok_or_else(|| anyhow!("no bus called '{key}'"))
}

/// A hotkey's step as given on the command line: `METHOD` or `METHOD
/// PARAMS`, the parameters as JSON, or a whole step as JSON.
pub fn parse_step(s: &str) -> Result<HotkeyStep> {
    let s = s.trim();
    if s.starts_with('{') {
        return serde_json::from_str(s).context("a step given as JSON needs a method");
    }
    let (method, params) = match s.split_once(char::is_whitespace) {
        Some((m, p)) => (m, p.trim()),
        None => (s, ""),
    };
    if method.is_empty() {
        bail!("a step needs a method, such as set_strip");
    }
    let params = if params.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(params)
            .with_context(|| format!("the parameters of {method} must be JSON"))?
    };
    Ok(HotkeyStep::new(method, params))
}

/// all or next.
pub fn parse_each_press(s: &str) -> Result<EachPress> {
    match s.to_ascii_lowercase().as_str() {
        "all" => Ok(EachPress::All),
        "next" => Ok(EachPress::Next),
        _ => bail!("expected all or next, got '{s}'"),
    }
}

/// nothing, restore or steps.
pub fn parse_on_release(s: &str) -> Result<OnRelease> {
    match s.to_ascii_lowercase().as_str() {
        "nothing" => Ok(OnRelease::Nothing),
        "restore" => Ok(OnRelease::Restore),
        "steps" => Ok(OnRelease::Steps),
        _ => bail!("expected nothing, restore or steps, got '{s}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_read_the_usual_words() {
        assert_eq!(parse_flag("ON").unwrap(), Flag::Set(true));
        assert_eq!(parse_flag("no").unwrap(), Flag::Set(false));
        assert_eq!(parse_flag("Toggle").unwrap(), Flag::Toggle);
        assert!(parse_flag("maybe").is_err());
        assert_eq!(flag(None).unwrap(), None);
    }

    #[test]
    fn colors_come_by_name_or_hex() {
        assert_eq!(parse_color_arg("teal").unwrap().as_deref(), Some("#3CBEAF"));
        assert_eq!(parse_color_arg("grey").unwrap().as_deref(), Some("#A0A8B4"));
        assert_eq!(
            parse_color_arg("#abcdef").unwrap().as_deref(),
            Some("#ABCDEF")
        );
        assert_eq!(parse_color_arg("none").unwrap(), None);
        assert!(parse_color_arg("mauve").is_err());
    }

    #[test]
    fn modes_read_their_short_names() {
        assert_eq!(parse_upmix("passive").unwrap(), Upmix::PassiveSurround);
        assert_eq!(
            parse_upmix("all_channel_stereo").unwrap(),
            Upmix::AllChannelStereo
        );
        assert_eq!(parse_downmix("Lt-Rt").unwrap(), DownmixMethod::Matrix);
        assert_eq!(parse_downmix("lfe").unwrap(), DownmixMethod::LfeOnly);
        assert_eq!(parse_layout("7.1").unwrap(), ChannelLayout::Surround71);
        assert_eq!(parse_bus_kind("hw").unwrap(), BusKind::Hardware);
        assert!(parse_strip_kind("software").is_err());
    }

    #[test]
    fn settings_numbers_take_auto() {
        assert_eq!(number_or_auto(Some("auto"), "rate").unwrap(), Some(0));
        assert_eq!(number_or_auto(Some("48000"), "rate").unwrap(), Some(48000));
        assert_eq!(number_or_auto(None, "rate").unwrap(), None);
        assert!(number_or_auto(Some("fast"), "rate").is_err());
        assert_eq!(position(1).unwrap(), 0);
        assert!(position(0).is_err());
    }

    #[test]
    fn devices_clear_with_none() {
        assert_eq!(device_patch(Some("none".into())), Some(None));
        assert_eq!(device_patch(Some("alsa".into())), Some(Some("alsa".into())));
        assert_eq!(device_patch(None), None);
        let items: Vec<&str> = split_list(" Mic, ,Music ").collect();
        assert_eq!(items, ["Mic", "Music"]);
    }
}
