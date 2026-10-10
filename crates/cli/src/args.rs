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
    /// List the hotkeys, and how keys reach Weir on this desktop.
    Hotkeys,
    /// Add, change, remove or press a hotkey.
    Hotkey {
        #[command(subcommand)]
        action: HotkeyCmd,
    },
    /// Save scenes, setups, hotkeys, equalizer presets, app rules and
    /// preferences to a .zip, or one of them to a .json, to keep or to take
    /// to another computer.
    Export(ExportArgs),
    /// Bring in settings from a .zip or .json Weir exported. See what it
    /// holds first with --list.
    Import(ImportArgs),
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
pub struct ExportArgs {
    /// The file to write: a .zip, or a .json for one thing alone.
    pub file: std::path::PathBuf,
    /// Everything: every scene, setup, hotkey and equalizer preset of your
    /// own, the app rules and the preferences.
    #[arg(long)]
    pub all: bool,
    /// A scene, by name. Give it again for more.
    #[arg(long = "scene", value_name = "NAME")]
    pub scenes: Vec<String>,
    /// A setup, by name. Give it again for more.
    #[arg(long = "setup", value_name = "NAME")]
    pub setups: Vec<String>,
    /// A hotkey, by name or id. Give it again for more. Their groups and
    /// order go with them.
    #[arg(long = "hotkey", value_name = "HOTKEY")]
    pub hotkeys: Vec<String>,
    /// An equalizer preset of your own, by name. Give it again for more.
    #[arg(long = "eq-preset", value_name = "NAME")]
    pub eq_presets: Vec<String>,
    /// The app rules.
    #[arg(long)]
    pub app_rules: bool,
    /// A part of the preferences: window-look, mixer, audio-timing or
    /// start-at-login. Give it again for more.
    #[arg(long = "preferences", value_name = "PART")]
    pub preferences: Vec<String>,
}

#[derive(Args, Debug)]
pub struct ImportArgs {
    /// The .zip or .json to import.
    pub file: std::path::PathBuf,
    /// Only show what it holds and what importing it would meet.
    #[arg(long)]
    pub list: bool,
    /// Only this item, by the ID --list shows. Give it again for more.
    #[arg(long = "only", value_name = "ID")]
    pub only: Vec<String>,
    /// What to do with things you have one of the same name of already:
    /// skip, replace, or keep-both (the imported one gets a number).
    #[arg(long, default_value = "skip")]
    pub taken: String,
    /// Import an item under another name, as ID=NAME. Give it again for
    /// more.
    #[arg(long = "rename", value_name = "ID=NAME")]
    pub rename: Vec<String>,
    /// Replace all your hotkeys and their groups with the imported ones,
    /// rather than adding them.
    #[arg(long)]
    pub replace_hotkeys: bool,
    /// Use one of your strips for one the file names, as FROM=TO. Give it
    /// again for more.
    #[arg(long = "map-strip", value_name = "FROM=TO")]
    pub map_strips: Vec<String>,
    /// Use one of your buses for one the file names, as FROM=TO.
    #[arg(long = "map-bus", value_name = "FROM=TO")]
    pub map_buses: Vec<String>,
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
    /// External effects on or off: the strip's sound goes out to another
    /// program, such as Carla, through "NAME to effects (Weir)", and comes
    /// back through "NAME from effects (Weir)".
    #[arg(long)]
    pub external_effects: Option<String>,
    /// Where in the strip the external effects go: before-denoise,
    /// before-gate, before-eq, before-compressor, before-fader or
    /// after-fader.
    #[arg(long)]
    pub external_effects_at: Option<String>,
    /// What the strip plays while nothing comes back from its external
    /// effects: pass (its sound, as if they were off) or silence.
    #[arg(long)]
    pub external_effects_fallback: Option<String>,
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
    /// Hold the bus's output back by this many milliseconds, 0 to 500: for
    /// lining it up with a bus that plays later, such as a Bluetooth
    /// speaker.
    #[arg(long)]
    pub delay: Option<f32>,
    /// Move the delay by this many milliseconds, such as 5 or -5.
    #[arg(long, allow_negative_numbers = true)]
    pub delay_by: Option<f32>,
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
    /// External effects on or off: the bus's mix goes out to another
    /// program, such as Carla, through "NAME to effects (Weir)", and comes
    /// back through "NAME from effects (Weir)".
    #[arg(long)]
    pub external_effects: Option<String>,
    /// Where in the bus the external effects go: before-eq, before-fader,
    /// before-limiter or after-limiter.
    #[arg(long)]
    pub external_effects_at: Option<String>,
    /// What the bus plays while nothing comes back from its external
    /// effects: pass (its mix, as if they were off) or silence.
    #[arg(long)]
    pub external_effects_fallback: Option<String>,
}

/// What to do with scenes or setups.
#[derive(Subcommand, Debug)]
pub enum LibraryCmd {
    /// List them, marking the current one.
    List,
    /// Save the mixer as it is now under `name`, replacing one of that name.
    Save { name: String },
    /// Bring one back. A setup alone starts with nothing routed; give a
    /// scene to load with it.
    Load {
        name: String,
        /// For a setup: a scene to load with it.
        #[arg(long)]
        scene: Option<String>,
    },
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

/// What to do with a hotkey.
#[derive(Subcommand, Debug)]
pub enum HotkeyCmd {
    /// Add a hotkey.
    Add {
        /// Its name: unique, 60 characters at most.
        name: String,
        #[command(flatten)]
        opts: HotkeyOpts,
    },
    /// Change a hotkey. Steps given replace all of its steps.
    Change {
        /// The hotkey, by name or id.
        hotkey: String,
        /// A new name.
        #[arg(long)]
        name: Option<String>,
        /// Take its keys away, so it is only pressed by name.
        #[arg(long, conflicts_with_all = ["keys", "add_keys"])]
        no_keys: bool,
        /// Other keys to press it with, keeping the ones it has. Give it
        /// once per key combination.
        #[arg(long = "add-keys", value_name = "KEYS")]
        add_keys: Vec<String>,
        #[command(flatten)]
        opts: HotkeyOpts,
    },
    /// Remove a hotkey.
    Remove {
        /// The hotkey, by name or id.
        hotkey: String,
    },
    /// Do what tapping a hotkey's keys does: press and let go at once.
    Run {
        /// The hotkey, by name or id.
        hotkey: String,
    },
    /// Do what pressing a hotkey's keys does, until `release`.
    Press {
        /// The hotkey, by name or id.
        hotkey: String,
    },
    /// Do what letting go of a hotkey's keys does.
    Release {
        /// The hotkey, by name or id.
        hotkey: String,
    },
    /// Switch a hotkey's keys on.
    On {
        /// The hotkey, by name or id.
        hotkey: String,
    },
    /// Switch a hotkey's keys off. It can still be run by name.
    Off {
        /// The hotkey, by name or id.
        hotkey: String,
    },
    /// Switch a hotkey's keys on if they are off, or off if on.
    Toggle {
        /// The hotkey, by name or id.
        hotkey: String,
    },
    /// Move a hotkey to another place in the list, or into a group.
    Move {
        /// The hotkey, by name or id.
        hotkey: String,
        /// Its place among the hotkeys of its group, or, in no group, among
        /// the places in the list (each hotkey in no group and each group),
        /// counting from 0. Left out, it goes last.
        #[arg(long)]
        to: Option<usize>,
        /// Move it into this group, by name or id, or "none" for no group.
        #[arg(long)]
        group: Option<String>,
    },
    /// Add, switch, rename, move or remove a group of hotkeys.
    Group {
        #[command(subcommand)]
        action: HotkeyGroupCmd,
    },
    /// Open the desktop's shortcut settings at Weir's hotkeys, to change
    /// their keys or add more. On KDE Plasma 6.5 and newer.
    Settings,
}

/// What to do with a group of hotkeys.
#[derive(Subcommand, Debug)]
pub enum HotkeyGroupCmd {
    /// Add a group, last in the list.
    Add {
        /// Its name: unique, 60 characters at most.
        name: String,
        /// Start it switched off.
        #[arg(long)]
        off: bool,
    },
    /// Switch the keys of a group's hotkeys on.
    On {
        /// The group, by name or id.
        group: String,
    },
    /// Switch the keys of a group's hotkeys off. Each keeps its own switch.
    Off {
        /// The group, by name or id.
        group: String,
    },
    /// Switch a group on if it is off, or off if on.
    Toggle {
        /// The group, by name or id.
        group: String,
    },
    /// Rename a group.
    Rename {
        /// The group, by name or id.
        group: String,
        /// Its new name.
        name: String,
    },
    /// Move a group to another place in the list.
    Move {
        /// The group, by name or id.
        group: String,
        /// Its place among the places in the list (each hotkey in no group
        /// and each group), counting from 0.
        #[arg(long)]
        to: usize,
    },
    /// Remove a group. Its hotkeys stay, in no group.
    Remove {
        /// The group, by name or id.
        group: String,
    },
}

/// A hotkey's settings, for `hotkey add` and `hotkey change`.
#[derive(Args, Debug)]
pub struct HotkeyOpts {
    /// The keys, such as "Ctrl+Alt+M": any of Ctrl, Alt, Shift and Super,
    /// and one key. Give it once per key combination for several, any of
    /// which presses the hotkey; on desktops other than KDE Plasma that
    /// look after the keys, only the first is suggested. Replaces the keys
    /// it had.
    #[arg(long)]
    pub keys: Vec<String>,
    /// A step: a method and its parameters as JSON, such as
    /// 'set_strip {"id": "Mic", "mute": "toggle"}', or a whole step as JSON.
    /// Give it once per step, in order.
    #[arg(long = "do", value_name = "STEP")]
    pub steps: Vec<String>,
    /// all (every step at each press, the default) or next (the next step
    /// at each press, going round).
    #[arg(long)]
    pub each_press: Option<String>,
    /// What letting go does: nothing, restore (put back what pressing
    /// changed) or steps (the --release-do steps).
    #[arg(long)]
    pub release: Option<String>,
    /// A step for letting go, like --do. Implies --release steps.
    #[arg(long = "release-do", value_name = "STEP")]
    pub release_steps: Vec<String>,
    /// Do the steps again every this many milliseconds while the keys are
    /// held (20 to 2000), or 0 not to.
    #[arg(long)]
    pub repeat: Option<u32>,
    /// Whether its keys work: on, off or toggle.
    #[arg(long)]
    pub enabled: Option<String>,
    /// The group it is in, by name or id, or "none" for no group.
    #[arg(long)]
    pub group: Option<String>,
}
