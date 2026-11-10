//! Starting Weir at login.
//!
//! `make install` and the RPM both install `weir.service`, a systemd user
//! unit, and enabling it is what makes Weir start at login. Whether it is
//! enabled is systemd's to keep, so the daemon asks `systemctl` rather than
//! remembering an answer that a terminal command could make wrong. It
//! enables the unit without `--now`: the daemon asking is already running,
//! and a second one started by systemd would find its socket taken.
//!
//! An AppImage installs nothing, so [`AppImageLogin`] writes the unit
//! itself, naming the AppImage file, and keeps it naming the right one.

use std::path::{Path, PathBuf};
use std::process::Command;
use weir_protocol::appimage::AppImage;

/// The unit `make install` and the RPM install.
const UNIT: &str = "weir.service";

/// Where starting at login is kept: systemd in practice, a stand-in in
/// tests.
pub trait LoginStart: Send + Sync {
    /// Whether Weir starts at login, or `None` where that cannot be set.
    fn get(&self) -> Option<bool>;
    /// Make Weir start at login, or stop it doing so. The error is a
    /// sentence a person can read.
    fn set(&self, on: bool) -> Result<(), String>;
}

/// The systemd user manager, through `systemctl --user`. Each call runs a
/// short process, which is fine for a setting changed by hand.
pub struct Systemd;

impl LoginStart for Systemd {
    fn get(&self) -> Option<bool> {
        let out = Command::new("systemctl")
            .args(["--user", "is-enabled", UNIT])
            .output()
            .ok()?;
        parse_is_enabled(&String::from_utf8_lossy(&out.stdout))
    }

    fn set(&self, on: bool) -> Result<(), String> {
        let verb = if on { "enable" } else { "disable" };
        let out = Command::new("systemctl")
            .args(["--user", verb, UNIT])
            .output()
            .map_err(|e| format!("could not run systemctl: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let why = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "systemctl could not {verb} Weir's service: {}",
            why.trim()
        ))
    }
}

/// Where Weir is started at login, for this copy of it: a unit of its own
/// when it runs from an AppImage, the installed one otherwise.
pub fn here(appimage: Option<&AppImage>) -> Box<dyn LoginStart> {
    match appimage.and_then(|a| AppImageLogin::new(a.file.clone())) {
        Some(own) => {
            own.follow();
            Box::new(own)
        }
        None => Box::new(Systemd),
    }
}

/// The first line of a unit Weir wrote for an AppImage. Weir only ever
/// changes or removes a unit that starts with it.
const MARK: &str = "# Written by Weir to start its AppImage at login.";

/// The installed unit, the model for the one an AppImage writes.
const TEMPLATE: &str = include_str!("../../../packaging/weir.service.in");

/// Starting at login from an AppImage: `~/.config/systemd/user/weir.service`,
/// written by Weir, starting the AppImage file. It takes the place of an
/// installed unit, if there is one too.
pub struct AppImageLogin {
    /// The AppImage file.
    file: PathBuf,
    /// Where the unit goes.
    unit: PathBuf,
}

impl AppImageLogin {
    /// For the AppImage `file`, or `None` without a configuration folder.
    pub fn new(file: PathBuf) -> Option<Self> {
        let unit = dirs::config_dir()?.join("systemd/user").join(UNIT);
        Some(Self { file, unit })
    }

    /// The unit there now, if Weir wrote it.
    fn ours(&self) -> Option<String> {
        std::fs::read_to_string(&self.unit)
            .ok()
            .filter(|text| text.starts_with(MARK))
    }

    /// Point the unit Weir wrote at this AppImage, if it names another
    /// file: the AppImage was moved, or a newer one with another name
    /// replaced it. Run at startup; a unit Weir did not write is left
    /// alone.
    pub fn follow(&self) {
        let Some(text) = self.ours() else {
            return;
        };
        match unit_for(&self.file) {
            Ok(wanted) if wanted != text => match write_unit(&self.unit, &wanted) {
                Ok(()) => tracing::info!("start at login now starts {}", self.file.display()),
                Err(e) => tracing::warn!("{e}"),
            },
            Ok(_) => {}
            Err(e) => tracing::warn!("{e}"),
        }
    }
}

impl LoginStart for AppImageLogin {
    fn get(&self) -> Option<bool> {
        // Without a unit of its own the box can still be ticked: ticking
        // writes one. An installed unit that is enabled answers yes.
        match Systemd.get() {
            Some(on) => Some(on),
            None if self.ours().is_none() => Some(false),
            None => None,
        }
    }

    fn set(&self, on: bool) -> Result<(), String> {
        if on {
            let ours_or_none = self.ours().is_some() || !self.unit.exists();
            if ours_or_none {
                write_unit(&self.unit, &unit_for(&self.file)?)?;
            }
            Systemd.set(true)
        } else {
            Systemd.set(false)?;
            if self.ours().is_some() {
                std::fs::remove_file(&self.unit)
                    .map_err(|e| format!("could not remove {}: {e}", self.unit.display()))?;
                daemon_reload();
            }
            Ok(())
        }
    }
}

/// The unit that starts the AppImage `file`, from the installed one's
/// sections.
fn unit_for(file: &Path) -> Result<String, String> {
    let exec = systemd_quote(file).ok_or_else(|| {
        format!(
            "{} has a line break in its name, which systemd cannot start; \
             rename the AppImage",
            file.display()
        )
    })?;
    let body = TEMPLATE
        .find("[Unit]")
        .map_or(TEMPLATE, |at| &TEMPLATE[at..]);
    let mut out = format!(
        "{MARK}\n# It starts {}; Weir rewrites this line if the AppImage moves.\n\n",
        file.display()
    );
    for line in body.lines() {
        if line.starts_with("ExecStart=") {
            out.push_str(&format!("ExecStart={exec} weir-daemon\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    Ok(out)
}

/// `path` as one word on a systemd command line: in double quotes, with
/// backslashes and quotes escaped, and `%` and `$` doubled so systemd
/// takes them as themselves. `None` for a path with a line break.
fn systemd_quote(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    if text.contains(['\n', '\r']) {
        return None;
    }
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    Some(out)
}

/// Write the unit and tell systemd it changed.
fn write_unit(unit: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = unit.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    }
    std::fs::write(unit, text).map_err(|e| format!("could not write {}: {e}", unit.display()))?;
    daemon_reload();
    Ok(())
}

/// Have systemd read its units again. A failure only delays it: systemd
/// also reads them at the next login.
fn daemon_reload() {
    match Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => tracing::warn!(
            "systemctl --user daemon-reload: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => tracing::warn!("could not run systemctl: {e}"),
    }
}

/// What `systemctl is-enabled` printed, as an answer. Only a unit that can
/// be switched either way counts: a missing unit prints nothing or
/// `not-found`, and a masked one needs unmasking first, which is the
/// person's call to make.
fn parse_is_enabled(stdout: &str) -> Option<bool> {
    match stdout.lines().next().unwrap_or("").trim() {
        "enabled" | "enabled-runtime" => Some(true),
        "disabled" | "linked" | "linked-runtime" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_unit_that_can_be_switched_gives_an_answer() {
        assert_eq!(parse_is_enabled("enabled\n"), Some(true));
        assert_eq!(parse_is_enabled("disabled\n"), Some(false));
        assert_eq!(parse_is_enabled("linked\n"), Some(false));
        assert_eq!(parse_is_enabled("not-found\n"), None);
        assert_eq!(parse_is_enabled("masked\n"), None);
        assert_eq!(parse_is_enabled("static\n"), None);
        assert_eq!(parse_is_enabled(""), None, "older systemd prints nothing");
    }

    #[test]
    fn an_appimage_path_is_one_word_to_systemd() {
        assert_eq!(
            systemd_quote(Path::new("/home/me/Apps/Weir.AppImage")).as_deref(),
            Some(r#""/home/me/Apps/Weir.AppImage""#)
        );
        assert_eq!(
            systemd_quote(Path::new(r#"/home/me/My "Apps"/100% $HOME\Weir.AppImage"#)).as_deref(),
            Some(r#""/home/me/My \"Apps\"/100%% $$HOME\\Weir.AppImage""#)
        );
        assert_eq!(systemd_quote(Path::new("/home/me/a\nb.AppImage")), None);
    }

    #[test]
    fn the_appimage_unit_starts_the_appimage_and_says_who_wrote_it() {
        let unit = unit_for(Path::new("/home/me/Apps/Weir-1.5.0-x86_64.AppImage")).unwrap();
        assert!(unit.starts_with(MARK));
        assert!(
            unit.contains("\nExecStart=\"/home/me/Apps/Weir-1.5.0-x86_64.AppImage\" weir-daemon\n")
        );
        assert!(!unit.contains("@BINDIR@"), "{unit}");
        assert!(
            unit.contains("[Install]"),
            "the template's sections are kept"
        );
        assert_eq!(unit.matches("ExecStart=").count(), 1);
    }
}
