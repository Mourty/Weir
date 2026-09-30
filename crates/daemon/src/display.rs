//! Finding the desktop to show the mixer window on.
//!
//! Started at login by systemd, the daemon comes up before the desktop, so
//! its own environment has neither `WAYLAND_DISPLAY` nor `DISPLAY`, and a
//! window it starts cannot open. Once the desktop is running it hands those
//! to the systemd user manager (Plasma and GNOME do, and so do the X session
//! scripts of Debian and Ubuntu), so the daemon asks the manager whenever
//! its own environment has no display.

use std::process::Command;

/// What a window needs to find the desktop and fit in with it.
const SESSION_VARS: &[&str] = &[
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XAUTHORITY",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "XDG_SESSION_TYPE",
    "DESKTOP_SESSION",
];

/// Either of these means there is a screen to show a window on.
const DISPLAY_VARS: &[&str] = &["WAYLAND_DISPLAY", "DISPLAY"];

/// The variables to give the window so it finds the desktop, or `None`
/// while there is no desktop yet. Empty when the daemon's own environment
/// already has a display: then the window inherits it, and a display the
/// daemon was started with (a terminal, or ssh) wins over the manager's.
pub fn window_env() -> Option<Vec<(String, String)>> {
    if DISPLAY_VARS.iter().any(|k| std::env::var_os(k).is_some()) {
        return Some(Vec::new());
    }
    session_from_manager(&manager_env())
}

/// The systemd user manager's environment, or nothing where there is no
/// systemd or it cannot be asked.
fn manager_env() -> String {
    Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

/// The session variables in `systemctl show-environment`'s output, when it
/// names a display. It prints `KEY=VALUE` lines, quoting values with
/// unusual characters as `$'…'`; the session variables never need that,
/// so quoted values are left out rather than unquoted.
fn session_from_manager(text: &str) -> Option<Vec<(String, String)>> {
    let vars: Vec<(String, String)> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, value)| SESSION_VARS.contains(key) && !value.starts_with("$'"))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    let has_display = vars
        .iter()
        .any(|(key, _)| DISPLAY_VARS.contains(&key.as_str()));
    has_display.then_some(vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_session_variables_once_the_desktop_has_set_them() {
        let text = "HOME=/home/ryan\nLANG=en_US.UTF-8\nWAYLAND_DISPLAY=wayland-0\n\
                    DISPLAY=:0\nXDG_CURRENT_DESKTOP=KDE\nLS_COLORS=$'rs=0:\\n'\n";
        let vars = session_from_manager(text).expect("a display");
        assert_eq!(
            vars,
            [
                ("WAYLAND_DISPLAY".to_owned(), "wayland-0".to_owned()),
                ("DISPLAY".to_owned(), ":0".to_owned()),
                ("XDG_CURRENT_DESKTOP".to_owned(), "KDE".to_owned()),
            ]
        );
    }

    #[test]
    fn no_display_yet_means_no_desktop() {
        let text = "HOME=/home/ryan\nXDG_RUNTIME_DIR=/run/user/1000\nXDG_CURRENT_DESKTOP=KDE\n";
        assert_eq!(session_from_manager(text), None);
        assert_eq!(session_from_manager(""), None, "no systemd at all");
    }

    #[test]
    fn a_quoted_display_is_not_trusted() {
        assert_eq!(session_from_manager("DISPLAY=$':0\\n'\n"), None);
    }
}
