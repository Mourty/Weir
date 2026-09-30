//! The command line: every command and option `weirctl` takes.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "weirctl", version, about = "Control the Weir daemon")]
pub struct Cli {
    /// Control socket path (default: $XDG_RUNTIME_DIR/weir/control.sock).
    #[arg(long, global = true)]
    pub socket: Option<PathBuf>,
    /// Print raw JSON results instead of tables.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

// Parsed once per run; the strip command's many options are not worth boxing.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Engine status and a one-line summary.
    Status,
    /// Show every strip, bus and route.
    State,
    /// List devices usable for hardware strips and buses.
    Devices,
    /// List application streams and where they play.
    Apps,
    /// Change a strip (by id or name).
    Strip(StripArgs),
    /// Change a bus (by id or name).
    Bus(BusArgs),
    /// Set or toggle a route from a strip to a bus, and its level.
    Route {
        /// The strip, by id or name.
        strip: String,
        /// The bus, by id, label (A1, B2) or name.
        bus: String,
        /// on, off, or toggle (the default, unless --level is given).
        action: Option<String>,
        /// The strip's level in this bus's mix, in dB, on top of its fader.
        #[arg(long, allow_negative_numbers = true)]
        level: Option<f32>,
        /// Change that level by this many dB.
        #[arg(long, allow_negative_numbers = true)]
        level_by: Option<f32>,
    },
    /// Add a strip.
    AddStrip {
        /// Its name, 40 characters at most.
        name: String,
        /// hardware or virtual.
        #[arg(long)]
        kind: String,
        /// mono, stereo, quad, 5.1, 7.1 or a list such as "FL FR LFE".
        #[arg(long, default_value = "stereo")]
        layout: String,
        /// Device node name (hardware strips only).
        #[arg(long)]
        device: Option<String>,
        /// Buses to route to (id or name), repeatable.
        #[arg(long = "route")]
        routes: Vec<String>,
    },
    /// Remove a strip.
    RemoveStrip { strip: String },
    /// Add a bus.
    AddBus {
        /// Its name, 40 characters at most.
        name: String,
        /// hardware or virtual.
        #[arg(long)]
        kind: String,
        /// mono, stereo, quad, 5.1, 7.1 or a list such as "FL FR LFE".
        #[arg(long, default_value = "stereo")]
        layout: String,
        /// Device node name (hardware buses only).
        #[arg(long)]
        device: Option<String>,
    },
    /// Remove a bus.
    RemoveBus { bus: String },
    /// List where applications go when they start playing.
    Rules,
    /// Always put an application on a strip when it starts playing.
    Rule {
        /// The application's name as `apps` shows it, or its program name.
        app: String,
        /// A virtual strip, or "leave" to leave it alone.
        strip: String,
    },
    /// Remove an application's rule.
    Unrule { app: String },
    /// Move a strip to another place, counting from 1 on the left.
    MoveStrip { strip: String, position: usize },
    /// Move a bus to another place, counting from 1 on the left.
    MoveBus { bus: String, position: usize },
    /// Move an application stream (see `apps`) to a virtual strip.
    MoveApp { app: u32, strip: String },
    /// Set an application stream's own volume (see `apps`).
    AppVolume {
        /// The application's id from `apps`.
        app: u32,
        /// Volume in dB (-60 to 12). 0 is the application's normal level.
        #[arg(long, allow_negative_numbers = true)]
        gain: Option<f32>,
        /// Change the volume by this many dB.
        #[arg(long, allow_negative_numbers = true)]
        gain_by: Option<f32>,
        /// on, off or toggle.
        #[arg(long)]
        mute: Option<String>,
    },
    /// Bring the mixer window to the front, starting it if needed.
    Show,
    /// Take back the most recent change to the mixer, or several.
    Undo {
        /// How many steps to take back.
        #[arg(long, default_value_t = 1)]
        steps: u32,
    },
    /// Bring back what was undone.
    Redo {
        /// How many steps to bring back.
        #[arg(long, default_value_t = 1)]
        steps: u32,
    },
    /// List what can be undone and redone.
    History,
    /// Show the daemon's settings, or change them.
    Settings {
        /// What solo does: "exclusive" (silence the other strips in every
        /// mix), or a bus to cue on (only that bus's mix changes).
        #[arg(long)]
        solo: Option<String>,
        /// Hold PipeWire at this sample rate in Hz, or "auto".
        #[arg(long)]
        rate: Option<String>,
        /// Hold PipeWire at this buffer size in frames, or "auto".
        #[arg(long)]
        buffer: Option<String>,
        /// Meter updates per second.
        #[arg(long)]
        meter_rate: Option<u32>,
        /// window, minimized or tray-only.
        #[arg(long)]
        startup: Option<String>,
        /// Start Weir when you log in: on, off or toggle.
        #[arg(long)]
        start_at_login: Option<String>,
        /// Show the tray icon: on or off.
        #[arg(long)]
        tray: Option<String>,
        /// Draw the tray icon in color, or in one color that matches the
        /// panel: color or one-color.
        #[arg(long)]
        tray_icon: Option<String>,
    },
    /// Setups: the strips and buses, with their devices, names and layouts.
    Setup {
        #[command(subcommand)]
        action: LibraryCmd,
    },
    /// Scenes: levels, routes and effects, without the devices.
    Scene {
        #[command(subcommand)]
        action: LibraryCmd,
    },
    /// Equalizer presets, and what a strip's or bus's equalizer is doing.
    Eq {
        #[command(subcommand)]
        action: EqCmd,
    },
    /// Stream notifications until interrupted.
    Watch {
        /// Include meter updates.
        #[arg(long)]
        meters: bool,
    },
    /// Send any method with raw JSON params.
    Raw {
        /// The method, such as set_strip.
        method: String,
        /// Its params, as JSON.
        params: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct StripArgs {
    /// The strip, by id or name.
    pub strip: String,
    /// Fader gain in dB (-60 to 12).
    #[arg(long, allow_negative_numbers = true)]
    pub gain: Option<f32>,
    /// Move the fader by this many dB, such as 3 or -3.
    #[arg(long, allow_negative_numbers = true)]
    pub gain_by: Option<f32>,
    /// on, off or toggle.
    #[arg(long)]
    pub mute: Option<String>,
    /// on, off or toggle.
    #[arg(long)]
    pub solo: Option<String>,
    /// Pan / balance, -1 (left) to 1 (right).
    #[arg(long, allow_negative_numbers = true)]
    pub pan: Option<f32>,
    /// A new name.
    #[arg(long)]
    pub name: Option<String>,
    /// Device node name, or "none" to clear.
    #[arg(long)]
    pub device: Option<String>,
    /// mono, stereo, quad, 5.1, 7.1 or a list such as "FL FR LFE".
    #[arg(long)]
    pub layout: Option<String>,
    /// Equalizer on or off (edit the bands in the window, or with `eq`).
    #[arg(long)]
    pub eq: Option<String>,
    /// Noise gate on or off.
    #[arg(long)]
    pub gate: Option<String>,
    /// Level in dB above which the gate opens (-90 to 0).
    #[arg(long, allow_negative_numbers = true)]
    pub gate_threshold: Option<f32>,
    /// How far the gate turns the strip down when closed, in dB (-90 to 0).
    #[arg(long, allow_negative_numbers = true)]
    pub gate_range: Option<f32>,
    /// How quickly the gate opens, in milliseconds.
    #[arg(long)]
    pub gate_attack: Option<f32>,
    /// How long the gate stays open after the level drops, in milliseconds.
    #[arg(long)]
    pub gate_hold: Option<f32>,
    /// How gradually the gate closes, in milliseconds.
    #[arg(long)]
    pub gate_release: Option<f32>,
    /// Noise suppression on or off.
    #[arg(long)]
    pub denoise: Option<String>,
    /// How much noise suppression to apply, 0 to 100 percent.
    #[arg(long)]
    pub denoise_amount: Option<f32>,
    /// A color: a name (orange, red, yellow, green, teal, blue, purple,
    /// pink, gray), "#rrggbb", or "none".
    #[arg(long)]
    pub color: Option<String>,
    /// Upmix: how the strip plays on buses with speakers it has no channel
    /// for. off (only its own speakers), center (also the center),
    /// all (all-channel stereo) or passive (passive surround decoding).
    #[arg(long)]
    pub upmix: Option<String>,
    /// Also play the strip's bass on the subwoofer of buses with one.
    #[arg(long)]
    pub subwoofer: Option<String>,
    /// Compressor on or off.
    #[arg(long)]
    pub comp: Option<String>,
    /// Level in dB above which the compressor turns the strip down (-60 to 0).
    #[arg(long, allow_negative_numbers = true)]
    pub comp_threshold: Option<f32>,
    /// How hard it turns down above the threshold, e.g. 4 for 4:1 (1 to 20).
    #[arg(long)]
    pub comp_ratio: Option<f32>,
    /// How quickly it turns down, in milliseconds.
    #[arg(long)]
    pub comp_attack: Option<f32>,
    /// How quickly it lets go, in milliseconds.
    #[arg(long)]
    pub comp_release: Option<f32>,
    /// Lift after compressing, in dB (0 to 24), or "auto".
    #[arg(long)]
    pub comp_lift: Option<String>,
    /// Ducking on or off: turn this strip down while other strips are heard.
    #[arg(long)]
    pub duck: Option<String>,
    /// The strips that duck this one, separated by commas.
    #[arg(long)]
    pub duck_when: Option<String>,
    /// How far to turn it down, in dB.
    #[arg(long)]
    pub duck_by: Option<f32>,
    /// The buses to duck it in, separated by commas, or "all".
    #[arg(long)]
    pub duck_in: Option<String>,
    /// How loud a trigger strip must be to count as heard, in dB.
    #[arg(long, allow_negative_numbers = true)]
    pub duck_threshold: Option<f32>,
}

#[derive(Args, Debug)]
pub struct BusArgs {
    /// The bus, by id, label (A1, B2) or name.
    pub bus: String,
    /// Fader gain in dB (-60 to 12).
    #[arg(long, allow_negative_numbers = true)]
    pub gain: Option<f32>,
    /// Move the fader by this many dB, such as 3 or -3.
    #[arg(long, allow_negative_numbers = true)]
    pub gain_by: Option<f32>,
    /// on, off or toggle.
    #[arg(long)]
    pub mute: Option<String>,
    /// Fold to mono: on, off or toggle.
    #[arg(long)]
    pub mono: Option<String>,
    /// A new name.
    #[arg(long)]
    pub name: Option<String>,
    /// Device node name, or "none" to clear.
    #[arg(long)]
    pub device: Option<String>,
    /// mono, stereo, quad, 5.1, 7.1 or a list such as "FL FR LFE".
    #[arg(long)]
    pub layout: Option<String>,
    /// Equalizer on or off.
    #[arg(long)]
    pub eq: Option<String>,
    /// Safety limiter on or off.
    #[arg(long)]
    pub limiter: Option<String>,
    /// The level the limiter never lets the bus go over, in dB.
    #[arg(long, allow_negative_numbers = true)]
    pub limiter_ceiling: Option<f32>,
    /// A color: a name (orange, red, yellow, green, teal, blue, purple,
    /// pink, gray), "#rrggbb", or "none".
    #[arg(long)]
    pub color: Option<String>,
    /// How the bus plays channels it has no speaker for: standard, matrix,
    /// or one part only: front-only, center-only, lfe-only, surround-only.
    #[arg(long)]
    pub downmix: Option<String>,
    /// Center level in the downmix, in dB: 0, -3 (standard), -4.5 or -6.
    #[arg(long, allow_negative_numbers = true)]
    pub center_level: Option<f32>,
    /// Surround level in the downmix, in dB: 0, -3 (standard), -6, or off.
    #[arg(long, allow_negative_numbers = true)]
    pub surround_level: Option<String>,
    /// Keep the subwoofer (LFE) channel in the downmix: on or off.
    #[arg(long)]
    pub keep_lfe: Option<String>,
}

/// What to do with scenes or setups.
#[derive(Subcommand, Debug)]
pub enum LibraryCmd {
    /// List them, marking the current one.
    List,
    /// Save the mixer as it is now under `name`, replacing one of that name.
    Save { name: String },
    /// Bring one back.
    Load { name: String },
    /// Delete one.
    Delete { name: String },
}

/// Which strip or bus an `eq` command is about. Give exactly one.
#[derive(Args, Debug)]
pub struct EqTarget {
    /// A strip, by id or name.
    #[arg(long)]
    pub strip: Option<String>,
    /// A bus, by id or name.
    #[arg(long)]
    pub bus: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum EqCmd {
    /// List the equalizer presets, built-in ones first.
    Presets,
    /// Show a strip's or bus's equalizer bands.
    Show {
        #[command(flatten)]
        target: EqTarget,
    },
    /// Apply a preset to a strip or bus and switch its equalizer on.
    Apply {
        /// The preset, by name.
        preset: String,
        #[command(flatten)]
        target: EqTarget,
    },
    /// Save a strip's or bus's bands as a preset of your own.
    Save {
        /// The name to save it under.
        name: String,
        #[command(flatten)]
        target: EqTarget,
    },
    /// Delete a preset of your own.
    Delete { name: String },
}
