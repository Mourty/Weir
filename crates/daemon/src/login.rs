//! Starting Weir at login.
//!
//! `make install` and the RPM both install `weir.service`, a systemd user
//! unit, and enabling it is what makes Weir start at login. Whether it is
//! enabled is systemd's to keep, so the daemon asks `systemctl` rather than
//! remembering an answer that a terminal command could make wrong. It
//! enables the unit without `--now`: the daemon asking is already running,
//! and a second one started by systemd would find its socket taken.

use std::process::Command;

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
}
