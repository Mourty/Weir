//! Bug reports: what the daemon can tell about Weir and the computer it
//! runs on ([`Diagnostics`], from `diagnostics`), what a client adds about
//! the desktop ([`Desktop`]), and [`bug_report`], which words them the same
//! way in every client.

use crate::model::FullState;
use serde::{Deserialize, Serialize};

/// Most problems [`Diagnostics::problems`] holds.
pub const PROBLEMS_KEPT: usize = 20;

/// How bad a problem the daemon logged was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProblemLevel {
    /// Something went wrong that Weir worked around.
    Warning,
    /// Something went wrong.
    Error,
}

/// A warning or an error the daemon logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoggedProblem {
    /// When, in milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// How bad it was.
    pub level: ProblemLevel,
    /// What the daemon said.
    pub message: String,
}

/// What `diagnostics` returns: what the daemon knows that a bug report
/// needs. Nothing in it leaves the computer unless someone sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Diagnostics {
    /// The daemon's version, such as `1.3.0`.
    pub version: String,
    /// How Weir was installed, in words: the package and where it came
    /// from, `make install`, or the build folder it runs from.
    pub installed: String,
    /// The operating system and its version, from `/etc/os-release`.
    pub system: String,
    /// The version of the PipeWire library the daemon runs with.
    pub pipewire_library: String,
    /// The version of the PipeWire server, once the engine has connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipewire_server: Option<String>,
    /// WirePlumber's version, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireplumber: Option<String>,
    /// Where the daemon's messages go: a file, or the journal.
    pub log: String,
    /// The latest warnings and errors the daemon logged, oldest first,
    /// [`PROBLEMS_KEPT`] at most.
    #[serde(default)]
    pub problems: Vec<LoggedProblem>,
}

/// The desktop a client runs on, for a bug report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Desktop {
    /// The desktop's name, such as `KDE (Plasma 6)`.
    pub name: Option<String>,
    /// `Wayland` or `X11`.
    pub session: Option<String>,
}

impl Desktop {
    /// The desktop this program runs on, from the variables the session
    /// sets.
    pub fn from_env() -> Self {
        Self::from_vars(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
    }

    /// The desktop, reading session variables with `var`.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let name = var("XDG_CURRENT_DESKTOP").map(|d| {
            let d = d.replace(':', ", ");
            match var("KDE_SESSION_VERSION") {
                Some(v) if d.contains("KDE") => format!("{d} (Plasma {v})"),
                _ => d,
            }
        });
        let session = match var("XDG_SESSION_TYPE").as_deref() {
            Some("wayland") => Some("Wayland".to_string()),
            Some("x11") => Some("X11".to_string()),
            Some(other) => Some(other.to_string()),
            None if var("WAYLAND_DISPLAY").is_some() => Some("Wayland".to_string()),
            None if var("DISPLAY").is_some() => Some("X11".to_string()),
            None => None,
        };
        Self { name, session }
    }
}

/// A bug report's details, as text to paste: versions, the system, the
/// engine, what the mixer holds, the devices in use and the latest
/// problems. `home`, when given, is written as `~` wherever it appears,
/// so the report does not carry the person's user name.
pub fn bug_report(
    d: &Diagnostics,
    state: &FullState,
    desktop: &Desktop,
    home: Option<&str>,
) -> String {
    let mut out = Vec::new();
    let client = env!("CARGO_PKG_VERSION");
    out.push(if d.version == client {
        format!("Weir {}", d.version)
    } else {
        format!("Weir {} (this program {client})", d.version)
    });
    out.push(format!("Installed: {}", d.installed));
    let system: Vec<&str> = [
        Some(d.system.as_str()),
        desktop.name.as_deref(),
        desktop.session.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    out.push(format!("System: {}", system.join(" · ")));
    let pipewire = match &d.pipewire_server {
        Some(server) if *server == d.pipewire_library => format!("PipeWire {server}"),
        Some(server) => format!("PipeWire {server} (library {})", d.pipewire_library),
        None => format!("PipeWire library {}", d.pipewire_library),
    };
    out.push(match &d.wireplumber {
        Some(wp) => format!("{pipewire} · WirePlumber {wp}"),
        None => format!("{pipewire} · WirePlumber unknown"),
    });

    let e = &state.engine;
    let mut engine = format!("Engine: {}", e.state);
    if e.sample_rate > 0 && e.quantum > 0 {
        engine.push_str(&format!(
            ", {} Hz, {} frames ({:.1} ms)",
            e.sample_rate,
            e.quantum,
            e.quantum as f32 * 1000.0 / e.sample_rate as f32
        ));
    }
    if let Some(error) = &e.error {
        engine.push_str(&format!(" ({error})"));
    }
    out.push(engine);
    let m = &state.mixer;
    let count =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    out.push(format!(
        "Mixer: {}, {} · {} · {}",
        count(m.strips.len(), "strip", "strips"),
        count(m.buses.len(), "bus", "buses"),
        count(state.hotkeys.hotkeys.len(), "hotkey", "hotkeys"),
        count(state.app_rules.len(), "app rule", "app rules"),
    ));
    out.push(format!(
        "Hotkeys: {}",
        state.hotkeys.keys.message.trim().trim_end_matches('.')
    ));

    // The devices in use, by the names people know them by.
    let devices: Vec<(&str, &str)> = m
        .strips
        .iter()
        .filter_map(|s| Some((s.name.as_str(), s.device.as_deref()?)))
        .chain(
            m.buses
                .iter()
                .filter_map(|b| Some((b.name.as_str(), b.device.as_deref()?))),
        )
        .collect();
    if !devices.is_empty() {
        out.push("Devices:".into());
        for (on, node) in devices {
            match state.devices.iter().find(|d| d.name == node) {
                Some(dev) => out.push(format!("  {on}: {}", dev.description)),
                None => out.push(format!("  {on}: {node} (not plugged in)")),
            }
        }
    }

    if d.problems.is_empty() {
        out.push("Recent problems: none".into());
    } else {
        out.push("Recent problems:".into());
        for p in &d.problems {
            let level = match p.level {
                ProblemLevel::Warning => "warning",
                ProblemLevel::Error => "error",
            };
            out.push(format!("  {}  {level}: {}", utc_time(p.at_ms), p.message));
        }
    }
    out.push(format!("Log: {}", d.log));

    let text = out.join("\n");
    match home.map(|h| h.trim_end_matches('/')) {
        Some(h) if h.len() > 1 => text.replace(h, "~"),
        _ => text,
    }
}

/// `at_ms`, milliseconds since the Unix epoch, as `2026-10-17 21:02:13
/// UTC`.
pub fn utc_time(at_ms: u64) -> String {
    // `2026-10-17T21-02-13Z`, made easier to read.
    let stamp = crate::transfer::utc_stamp(at_ms / 1000);
    let (date, time) = stamp.split_at(10);
    format!(
        "{date} {} UTC",
        time.trim_start_matches('T')
            .trim_end_matches('Z')
            .replace('-', ":")
    )
}

/// Longest a new issue's address may grow, so browsers and GitHub take it.
const ISSUE_URL_MAX: usize = 7000;

/// The address of a new bug report at `repository` (such as
/// `https://github.com/Mourty/Weir`), its Details filled with `details`.
/// Long details lose lines from the end, with a note to paste the rest.
pub fn new_issue_url(repository: &str, details: &str) -> String {
    let base = format!(
        "{}/issues/new?template=bug_report.yml&details=",
        repository.trim_end_matches('/')
    );
    let mut lines: Vec<&str> = details.lines().collect();
    let mut cut = false;
    loop {
        let mut text = lines.join("\n");
        if cut {
            text.push_str("\n(more was copied; paste the rest here)");
        }
        let url = format!("{base}{}", percent_encode(&text));
        if url.len() <= ISSUE_URL_MAX || lines.is_empty() {
            return url;
        }
        lines.pop();
        cut = true;
    }
}

/// `text` with everything but letters, digits and `-._~` as `%XX`, byte by
/// byte, for a query string.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bus, BusKind, ChannelLayout, DeviceInfo, DeviceKind, Strip, StripKind};

    fn diagnostics() -> Diagnostics {
        Diagnostics {
            version: env!("CARGO_PKG_VERSION").into(),
            installed: "package weir-1.3.0-1.fc44 (Fedora Copr - user mourt)".into(),
            system: "Nobara Linux 44".into(),
            pipewire_library: "1.4.8".into(),
            pipewire_server: Some("1.4.8".into()),
            wireplumber: Some("0.5.10".into()),
            log: "/home/sam/.local/state/weir/daemon.log".into(),
            problems: vec![LoggedProblem {
                at_ms: 1_792_270_933_000,
                level: ProblemLevel::Warning,
                message: "could not move Firefox".into(),
            }],
        }
    }

    #[test]
    fn a_report_says_what_matters_and_hides_the_home_folder() {
        let mut state = FullState::default();
        let mut mic = Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono);
        mic.device = Some("alsa_input.yeti".into());
        let mut headset = Bus::new(1, "Headset", BusKind::Hardware, ChannelLayout::Stereo);
        headset.device = Some("alsa_output.arctis".into());
        state.mixer.strips.push(mic);
        state.mixer.buses.push(headset);
        state.devices.push(DeviceInfo {
            id: 40,
            name: "alsa_input.yeti".into(),
            description: "Yeti Stereo Microphone".into(),
            kind: DeviceKind::Source,
            channels: Vec::new(),
        });
        state.engine.state = "streaming".into();
        state.engine.sample_rate = 48000;
        state.engine.quantum = 1024;
        let desktop = Desktop::from_vars(|v| {
            match v {
                "XDG_CURRENT_DESKTOP" => Some("KDE"),
                "KDE_SESSION_VERSION" => Some("6"),
                "XDG_SESSION_TYPE" => Some("wayland"),
                _ => None,
            }
            .map(String::from)
        });
        let text = bug_report(&diagnostics(), &state, &desktop, Some("/home/sam/"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], format!("Weir {}", env!("CARGO_PKG_VERSION")));
        assert_eq!(
            lines[2],
            "System: Nobara Linux 44 · KDE (Plasma 6) · Wayland"
        );
        assert_eq!(lines[3], "PipeWire 1.4.8 · WirePlumber 0.5.10");
        assert_eq!(
            lines[4],
            "Engine: streaming, 48000 Hz, 1024 frames (21.3 ms)"
        );
        assert_eq!(lines[5], "Mixer: 1 strip, 1 bus · 0 hotkeys · 0 app rules");
        assert!(text.contains("  Mic: Yeti Stereo Microphone\n"));
        assert!(text.contains("  Headset: alsa_output.arctis (not plugged in)\n"));
        assert!(text.contains("  2026-10-17 21:02:13 UTC  warning: could not move Firefox"));
        assert!(text.ends_with("Log: ~/.local/state/weir/daemon.log"));
        assert!(!text.contains("sam"));
    }

    #[test]
    fn the_session_comes_from_the_display_without_a_session_type() {
        let x11 = Desktop::from_vars(|v| (v == "DISPLAY").then(|| ":0".to_string()));
        assert_eq!(x11.session.as_deref(), Some("X11"));
        assert_eq!(x11.name, None);
        let gnome = Desktop::from_vars(|v| match v {
            "XDG_CURRENT_DESKTOP" => Some("ubuntu:GNOME".into()),
            "KDE_SESSION_VERSION" => Some("6".into()),
            _ => None,
        });
        assert_eq!(gnome.name.as_deref(), Some("ubuntu, GNOME"));
    }

    #[test]
    fn long_details_are_cut_to_fit_an_address() {
        let short = new_issue_url("https://github.com/Mourty/Weir/", "Weir 1.3.0\nOK & done");
        assert_eq!(
            short,
            "https://github.com/Mourty/Weir/issues/new?template=bug_report.yml\
             &details=Weir%201.3.0%0AOK%20%26%20done"
        );
        let long: String = (0..2000).map(|k| format!("line {k}\n")).collect();
        let url = new_issue_url("https://github.com/Mourty/Weir", &long);
        assert!(url.len() <= ISSUE_URL_MAX);
        assert!(url.ends_with(&percent_encode("\n(more was copied; paste the rest here)")));
        assert!(url.contains("line%200%0A"));
    }
}
