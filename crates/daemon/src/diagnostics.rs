//! What a bug report needs from the daemon (`diagnostics`): how Weir was
//! installed, the system's and PipeWire's versions, where the log goes, and
//! the latest warnings and errors, which [`Problems`] keeps as they are
//! logged.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
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
        installed: Install::here().describe(),
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

/// Who installed a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    Rpm,
    Dpkg,
    Pacman,
}

/// How Weir was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// By a package: who installed it, and the package's name and version
    /// (with who built it, for an RPM).
    Package(Manager, String),
    /// Under /usr, in no package.
    Unpackaged(PathBuf),
    /// By `make install`, in the home folder.
    Make(PathBuf),
    /// Run from where it was built.
    BuildFolder(PathBuf),
    /// Somewhere else, or nobody knows.
    Elsewhere(Option<PathBuf>),
}

impl Install {
    /// How this daemon was installed. Asking the package manager takes a
    /// moment, so it is asked once.
    pub fn here() -> Install {
        static HERE: OnceLock<Install> = OnceLock::new();
        HERE.get_or_init(|| match std::env::current_exe() {
            Ok(exe) => Install::of(&exe, dirs::home_dir().as_deref(), package),
            Err(_) => Install::Elsewhere(None),
        })
        .clone()
    }

    /// How a daemon at `exe` was installed. `package` names the package
    /// that installed Weir, if any.
    fn of(
        exe: &Path,
        home: Option<&Path>,
        package: impl Fn() -> Option<(Manager, String)>,
    ) -> Install {
        if exe.starts_with("/usr") {
            match package() {
                Some((manager, p)) => Install::Package(manager, p),
                None => Install::Unpackaged(exe.into()),
            }
        } else if home.is_some_and(|h| exe.starts_with(h.join(".local"))) {
            Install::Make(exe.into())
        } else if exe.components().any(|c| c.as_os_str() == "target") {
            Install::BuildFolder(exe.into())
        } else {
            Install::Elsewhere(Some(exe.into()))
        }
    }

    /// In words, for a bug report.
    pub fn describe(&self) -> String {
        match self {
            Install::Package(_, p) => format!("package {p}"),
            Install::Unpackaged(exe) => format!("{}, in no package", exe.display()),
            Install::Make(exe) => format!("make install, {}", exe.display()),
            Install::BuildFolder(exe) => format!("a build folder, {}", exe.display()),
            Install::Elsewhere(Some(exe)) => exe.display().to_string(),
            Install::Elsewhere(None) => "unknown".into(),
        }
    }

    /// How to update it, for people who are told a new version is out.
    pub fn how_to_update(&self) -> String {
        match self {
            Install::Package(Manager::Rpm, p) if p.contains("Copr") => {
                "Weir came from COPR: Discover offers the update, or run \
                 sudo dnf upgrade --refresh weir"
                    .into()
            }
            Install::Package(Manager::Rpm, _) => "Download the new .rpm from the release page \
                 and install it with sudo dnf install ./weir-*.rpm"
                .into(),
            Install::Package(Manager::Dpkg, _) => "Download the new .deb from the release page \
                 and install it with sudo apt install ./weir_*.deb"
                .into(),
            Install::Package(Manager::Pacman, _) => "Update it the way you installed it: with \
                 your AUR helper, such as paru -Syu, or with the package from the release page"
                .into(),
            Install::Make(_) => "Get the new source (git pull in your copy, or the \
                 release's source download) and run make install again"
                .into(),
            Install::BuildFolder(_) => "Get the new source and build it again".into(),
            Install::Unpackaged(_) | Install::Elsewhere(_) => {
                "Download it from the release page".into()
            }
        }
    }
}

/// The installed package called weir, with its version and, for an RPM,
/// who built it: COPR names itself there.
fn package() -> Option<(Manager, String)> {
    let rpm = output(
        "rpm",
        &[
            "-q",
            "--qf",
            "%{NAME}-%{VERSION}-%{RELEASE} (%{VENDOR})",
            "weir",
        ],
    )
    .map(|p| (Manager::Rpm, p.replace(" ((none))", "")));
    rpm.or_else(|| {
        output("dpkg-query", &["-W", "-f", "${Package} ${Version}", "weir"])
            .map(|p| (Manager::Dpkg, p))
    })
    .or_else(|| output("pacman", &["-Q", "weir"]).map(|p| (Manager::Pacman, p)))
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
        let copr = || {
            Some((
                Manager::Rpm,
                "weir-1.3.0-1.fc44 (Fedora Copr - user mourt)".to_string(),
            ))
        };
        let at = |exe: &str, package: &dyn Fn() -> Option<(Manager, String)>| {
            Install::of(Path::new(exe), Some(home), package)
        };
        let rpm = at("/usr/bin/weir-daemon", &copr);
        assert_eq!(
            rpm.describe(),
            "package weir-1.3.0-1.fc44 (Fedora Copr - user mourt)"
        );
        assert!(rpm.how_to_update().contains("dnf upgrade --refresh weir"));
        let deb = at("/usr/bin/weir-daemon", &|| {
            Some((Manager::Dpkg, "weir 1.3.0".into()))
        });
        assert!(deb.how_to_update().contains("apt install"));
        assert_eq!(
            at("/usr/local/bin/weir-daemon", &|| None).describe(),
            "/usr/local/bin/weir-daemon, in no package"
        );
        let make = at("/home/sam/.local/bin/weir-daemon", &copr);
        assert_eq!(
            make.describe(),
            "make install, /home/sam/.local/bin/weir-daemon"
        );
        assert!(make.how_to_update().contains("make install again"));
        assert_eq!(
            at("/home/sam/src/Weir/target/release/weir-daemon", &copr).describe(),
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
