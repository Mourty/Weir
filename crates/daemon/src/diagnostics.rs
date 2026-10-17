//! What a bug report needs from the daemon (`diagnostics`): how Weir was
//! installed, the system's and PipeWire's versions, where the log goes, and
//! the latest warnings and errors, which [`Problems`] keeps as they are
//! logged.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use weir_protocol::{Diagnostics, LoggedProblem, ProblemLevel, PROBLEMS_KEPT};

/// The latest warnings and errors, oldest first.
static PROBLEMS: Mutex<VecDeque<LoggedProblem>> = Mutex::new(VecDeque::new());

/// A logging layer that keeps the latest warnings and errors for
/// `diagnostics`, whatever else is done with them.
pub struct Problems;

impl<S: Subscriber> Layer<S> for Problems {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = match *event.metadata().level() {
            Level::ERROR => ProblemLevel::Error,
            Level::WARN => ProblemLevel::Warning,
            _ => return,
        };
        let mut text = Message::default();
        event.record(&mut text);
        let Ok(mut problems) = PROBLEMS.lock() else {
            return;
        };
        if problems.len() == PROBLEMS_KEPT {
            problems.pop_front();
        }
        problems.push_back(LoggedProblem {
            at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            level,
            message: text.0,
        });
    }
}

/// An event's message, then its other fields as `name=value`.
#[derive(Default)]
struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.insert_str(0, &format!("{value:?}"));
        } else {
            self.0.push_str(&format!(" {}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.insert_str(0, value);
        } else {
            self.0.push_str(&format!(" {}={value}", field.name()));
        }
    }
}

/// Everything `diagnostics` returns. `server` is the PipeWire server's
/// version, when the engine knows it.
pub fn gather(server: Option<String>) -> Diagnostics {
    Diagnostics {
        version: env!("CARGO_PKG_VERSION").into(),
        installed: installed(),
        system: system(),
        pipewire_library: weir_engine::library_version(),
        pipewire_server: server,
        wireplumber: output("wireplumber", &["--version"]).and_then(|o| linked_version(&o)),
        log: log_place(),
        problems: PROBLEMS
            .lock()
            .map(|p| p.iter().cloned().collect())
            .unwrap_or_default(),
    }
}

/// How Weir was installed. Asking the package manager takes a moment, so
/// it is asked once.
fn installed() -> String {
    static INSTALLED: OnceLock<String> = OnceLock::new();
    INSTALLED
        .get_or_init(|| match std::env::current_exe() {
            Ok(exe) => describe_install(&exe, dirs::home_dir().as_deref(), package),
            Err(_) => "unknown".into(),
        })
        .clone()
}

/// How a daemon at `exe` was installed, in words. `package` names the
/// package that installed Weir, if any.
fn describe_install(
    exe: &Path,
    home: Option<&Path>,
    package: impl Fn() -> Option<String>,
) -> String {
    if exe.starts_with("/usr") {
        match package() {
            Some(p) => format!("package {p}"),
            None => format!("{}, in no package", exe.display()),
        }
    } else if home.is_some_and(|h| exe.starts_with(h.join(".local"))) {
        format!("make install, {}", exe.display())
    } else if exe.components().any(|c| c.as_os_str() == "target") {
        format!("a build folder, {}", exe.display())
    } else {
        exe.display().to_string()
    }
}

/// The installed package called weir, with its version and, for an RPM,
/// who built it: COPR names itself there.
fn package() -> Option<String> {
    output(
        "rpm",
        &[
            "-q",
            "--qf",
            "%{NAME}-%{VERSION}-%{RELEASE} (%{VENDOR})",
            "weir",
        ],
    )
    .map(|p| p.replace(" ((none))", ""))
    .or_else(|| output("dpkg-query", &["-W", "-f", "${Package} ${Version}", "weir"]))
    .or_else(|| output("pacman", &["-Q", "weir"]))
}

/// What `program` prints, trimmed, when it runs and succeeds.
fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The operating system and the kernel: "Fedora Linux 44 (KDE Plasma
/// Desktop Edition) · Linux 6.11.3".
fn system() -> String {
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| os_name(&text))
        .unwrap_or_else(|| "Linux".into());
    match std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        Ok(kernel) => format!("{os} · Linux {}", kernel.trim()),
        Err(_) => os,
    }
}

/// The system's name from the text of `/etc/os-release`.
fn os_name(os_release: &str) -> Option<String> {
    let value = |key: &str| {
        os_release.lines().find_map(|line| {
            let v = line.strip_prefix(key)?.strip_prefix('=')?;
            let v = v.trim().trim_matches('"').trim_matches('\'');
            (!v.is_empty()).then(|| v.to_string())
        })
    };
    value("PRETTY_NAME").or_else(|| {
        let name = value("NAME")?;
        Some(match value("VERSION_ID") {
            Some(v) => format!("{name} {v}"),
            None => name,
        })
    })
}

/// The library version from `wireplumber --version`, which says
/// "Linked with libwireplumber 0.5.10".
fn linked_version(output: &str) -> Option<String> {
    output
        .lines()
        .find(|l| l.trim_start().starts_with("Linked with"))
        .and_then(|l| l.split_whitespace().last())
        .map(String::from)
}

/// Where what the daemon logs goes.
fn log_place() -> String {
    // systemd sets this for what it starts, and sends their output to the
    // journal.
    if std::env::var_os("INVOCATION_ID").is_some() {
        return "the journal (journalctl --user -u weir)".into();
    }
    match std::fs::read_link("/proc/self/fd/1") {
        Ok(p) if p.starts_with("/dev/pts") || p.starts_with("/dev/tty") => "a terminal".into(),
        Ok(p) if p.is_absolute() && !p.starts_with("/dev") => p.display().to_string(),
        _ => "standard output".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn warnings_and_errors_are_kept_and_the_rest_is_not() {
        let subscriber = tracing_subscriber::registry().with(Problems);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("all is well");
            tracing::warn!(app = "Firefox", "could not move it");
            for k in 0..PROBLEMS_KEPT {
                tracing::error!("problem {k}");
            }
        });
        let problems = PROBLEMS.lock().unwrap().clone();
        assert_eq!(problems.len(), PROBLEMS_KEPT, "the oldest went");
        assert_eq!(problems[0].message, "problem 0");
        assert_eq!(problems[0].level, ProblemLevel::Error);
        assert!(problems.iter().all(|p| !p.message.contains("all is well")));

        PROBLEMS.lock().unwrap().clear();
        let subscriber = tracing_subscriber::registry().with(Problems);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(app = "Firefox", "could not move it");
        });
        let problems = PROBLEMS.lock().unwrap().clone();
        assert_eq!(problems[0].message, "could not move it app=Firefox");
        assert_eq!(problems[0].level, ProblemLevel::Warning);
    }

    #[test]
    fn the_install_is_told_by_where_the_daemon_is() {
        let home = Path::new("/home/sam");
        let rpm = || Some("weir-1.3.0-1.fc44 (Fedora Copr - user mourt)".to_string());
        assert_eq!(
            describe_install(Path::new("/usr/bin/weir-daemon"), Some(home), rpm),
            "package weir-1.3.0-1.fc44 (Fedora Copr - user mourt)"
        );
        assert_eq!(
            describe_install(Path::new("/usr/local/bin/weir-daemon"), Some(home), || None),
            "/usr/local/bin/weir-daemon, in no package"
        );
        assert_eq!(
            describe_install(
                Path::new("/home/sam/.local/bin/weir-daemon"),
                Some(home),
                rpm
            ),
            "make install, /home/sam/.local/bin/weir-daemon"
        );
        assert_eq!(
            describe_install(
                Path::new("/home/sam/src/Weir/target/release/weir-daemon"),
                Some(home),
                rpm
            ),
            "a build folder, /home/sam/src/Weir/target/release/weir-daemon"
        );
    }

    #[test]
    fn the_system_and_wireplumber_versions_are_read() {
        let nobara = "NAME=\"Nobara Linux\"\nVERSION_ID=44\nPRETTY_NAME=\"Nobara Linux 44 (KDE Plasma Desktop Edition)\"\n";
        assert_eq!(
            os_name(nobara).as_deref(),
            Some("Nobara Linux 44 (KDE Plasma Desktop Edition)")
        );
        assert_eq!(
            os_name("NAME=Arch\nVERSION_ID=rolling\n").as_deref(),
            Some("Arch rolling")
        );
        assert_eq!(os_name("ID=x\n"), None);
        let wp =
            "wireplumber\nCompiled with libwireplumber 0.5.10\nLinked with libwireplumber 0.5.10\n";
        assert_eq!(linked_version(wp).as_deref(), Some("0.5.10"));
        assert_eq!(linked_version("nothing"), None);
    }
}
