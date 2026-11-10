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
//! That unit, in the person's own folder, takes the place of an installed
//! one, so an installed Weir ([`InstalledLogin`]) clears it away once its
//! AppImage is gone, or when the person turns start at login on there.

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
    if let Some(own) = appimage.and_then(|a| AppImageLogin::new(a.file.clone())) {
        own.follow();
        return Box::new(own);
    }
    let installed = InstalledLogin::new();
    installed.tidy();
    Box::new(installed)
}

/// The first line of a unit Weir wrote for an AppImage. Weir only ever
/// changes or removes a unit that starts with it.
const MARK: &str = "# Written by Weir to start its AppImage at login.";

/// The second line of such a unit names the AppImage between these.
const STARTS: (&str, &str) = (
    "# It starts ",
    "; Weir rewrites this line if the AppImage moves.",
);

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
        appimage_unit(&self.unit)
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
                remove_unit(&self.unit)?;
            }
            Ok(())
        }
    }
}

/// Starting an installed Weir at login: the installed unit, through
/// systemd. An AppImage's unit in the person's folder hides it, so this
/// takes that one away when it can only get in the way.
pub struct InstalledLogin {
    /// Where an AppImage's unit would be.
    unit: Option<PathBuf>,
}

impl InstalledLogin {
    fn new() -> Self {
        Self {
            unit: dirs::config_dir().map(|dir| dir.join("systemd/user").join(UNIT)),
        }
    }

    /// An AppImage's unit, if one is there.
    fn appimage_unit(&self) -> Option<&Path> {
        self.unit
            .as_deref()
            .filter(|unit| appimage_unit(unit).is_some())
    }

    /// Run at startup. An AppImage's unit whose AppImage is gone (deleted
    /// after trying it, say) would start nothing at the next login, and
    /// the installed Weir neither, so it goes, and if it was switched on,
    /// the installed unit is switched on in its place.
    fn tidy(&self) {
        let Some(unit) = self.appimage_unit() else {
            return;
        };
        let names = appimage_unit(unit).and_then(|text| started_file(&text));
        if names.is_none_or(|file| file.exists()) {
            return;
        }
        if let Err(e) = self.replace_appimage_unit(unit) {
            tracing::warn!("{e}");
            return;
        }
        tracing::info!(
            "removed {}: the AppImage it started is gone",
            unit.display()
        );
    }

    /// Take an AppImage's unit away, keeping start at login as it was.
    fn replace_appimage_unit(&self, unit: &Path) -> Result<(), String> {
        let was_on = Systemd.get() == Some(true);
        if was_on {
            // While the file is still there, so its links go with it.
            Systemd.set(false)?;
        }
        remove_unit(unit)?;
        if was_on {
            Systemd.set(true)?;
        }
        Ok(())
    }
}

impl LoginStart for InstalledLogin {
    fn get(&self) -> Option<bool> {
        Systemd.get()
    }

    fn set(&self, on: bool) -> Result<(), String> {
        // Turning it on here means this Weir, not an AppImage.
        if let Some(unit) = self.appimage_unit().filter(|_| on) {
            remove_unit(unit)?;
        }
        Systemd.set(on)
    }
}

/// The unit at `path`, if Weir wrote it for an AppImage.
fn appimage_unit(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .filter(|text| text.starts_with(MARK))
}

/// The AppImage an AppImage's unit starts, from its second line.
fn started_file(unit: &str) -> Option<PathBuf> {
    let line = unit.lines().nth(1)?;
    let file = line.strip_prefix(STARTS.0)?.strip_suffix(STARTS.1)?;
    Some(PathBuf::from(file))
}

/// Remove a unit and tell systemd.
fn remove_unit(unit: &Path) -> Result<(), String> {
    std::fs::remove_file(unit).map_err(|e| format!("could not remove {}: {e}", unit.display()))?;
    daemon_reload();
    Ok(())
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
    let mut out = format!("{MARK}\n{}{}{}\n\n", STARTS.0, file.display(), STARTS.1);
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

    #[test]
    fn an_installed_weir_can_tell_which_appimage_a_unit_starts() {
        let file = Path::new("/home/me/My Apps/Weir; v2.AppImage");
        let unit = unit_for(file).unwrap();
        assert_eq!(started_file(&unit).as_deref(), Some(file));
        assert_eq!(started_file("[Unit]\nDescription=Weir\n"), None);
    }
}
