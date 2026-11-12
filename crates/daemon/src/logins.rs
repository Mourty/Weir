//! Noticing a new login while the service keeps running.
//!
//! Logging out does not always stop the service. The systemd user manager,
//! and everything the session started, outlives a logout when the next
//! login comes within logind's `UserStopDelaySec` (10 seconds unless set
//! otherwise), or while anything of the old session is still closing. Then
//! nothing starts at the next login, start at login included, since the
//! manager never left `default.target`; the tray icon comes back with the
//! new panel, but the window closed with the old desktop and does not. So
//! the service watches logind for a new desktop login of its own user and
//! treats it as a start.

use ashpd::zbus;
use futures_util::StreamExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{debug, info};
use zbus::zvariant::OwnedObjectPath;

/// How many times the desktop's tray has come back while the service ran,
/// with a way to wait for the next time.
static TRAYS_UP: AtomicU64 = AtomicU64::new(0);
static TRAY_UP: Notify = Notify::const_new();

/// The desktop's tray was there and has gone, as at a logout.
static TRAY_GONE: AtomicBool = AtomicBool::new(false);

/// How long a new login's desktop may take to show its tray before the
/// window opens without waiting any longer.
const TRAY_WAIT: Duration = Duration::from_secs(90);

/// How long to give a new login's desktop where there is no tray to wait
/// for.
const NO_TRAY_WAIT: Duration = Duration::from_secs(10);

/// The desktop's tray has come back after it went away, or come up
/// after the service started before it. Called by the tray icon.
pub fn tray_appeared() {
    TRAY_GONE.store(false, Ordering::Release);
    TRAYS_UP.fetch_add(1, Ordering::AcqRel);
    TRAY_UP.notify_waiters();
}

/// The desktop's tray was there and has gone: the desktop is ending, or
/// its tray restarting. Called by the tray icon.
pub fn tray_went_away() {
    TRAY_GONE.store(true, Ordering::Release);
}

/// A new desktop login of this user, as logind told of it.
#[derive(Debug, Clone, Copy)]
pub struct Login {
    /// How many times the tray had come back when the login began.
    trays_up: u64,
    /// Whether the tray had gone with the old desktop, so the new one
    /// will bring it back.
    tray_gone: bool,
}

impl Login {
    /// Wait until this login's desktop is ready for a window: its tray
    /// has come up, where the desktop showed one before, or a short while
    /// has passed. The desktop hands the systemd user manager its display
    /// before it starts its tray, so the manager's then belongs to it.
    pub async fn desktop_ready(self) {
        // No tray to wait for: Weir shows none, or the desktop has none
        // (GNOME without its tray extension).
        if !self.tray_gone {
            tokio::time::sleep(NO_TRAY_WAIT).await;
            return;
        }
        let deadline = tokio::time::Instant::now() + TRAY_WAIT;
        loop {
            let up = TRAY_UP.notified();
            tokio::pin!(up);
            up.as_mut().enable();
            if TRAYS_UP.load(Ordering::Acquire) != self.trays_up {
                return;
            }
            tokio::select! {
                _ = &mut up => {}
                _ = tokio::time::sleep_until(deadline) => {
                    debug!("the new desktop's tray did not come up in time");
                    return;
                }
            }
        }
    }
}

/// Call `on_login` for each new desktop login of this user from now on,
/// for as long as the service runs. Without a system bus or logind (no
/// systemd), logins go unnoticed.
pub async fn watch(mut on_login: impl FnMut(Login) + Send) {
    if let Err(e) = watch_logind(&mut on_login).await {
        debug!("new logins go unnoticed: {e}");
    }
}

async fn watch_logind(on_login: &mut (impl FnMut(Login) + Send)) -> zbus::Result<()> {
    let bus = zbus::Connection::system().await?;
    let manager = zbus::Proxy::new(
        &bus,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let mut news = manager.receive_signal("SessionNew").await?;
    // SAFETY: getuid cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    while let Some(message) = news.next().await {
        // Before asking about the session, which takes a moment: the
        // tray must come up after this to count as the new desktop's.
        let trays_up = TRAYS_UP.load(Ordering::Acquire);
        let tray_gone = TRAY_GONE.load(Ordering::Acquire);
        let Ok((_, path)) = message.body().deserialize::<(String, OwnedObjectPath)>() else {
            continue;
        };
        match desktop_login_of(&bus, path, uid).await {
            Ok(true) => {
                info!("a new login while Weir kept running");
                on_login(Login {
                    trays_up,
                    tray_gone,
                });
            }
            Ok(false) => {}
            Err(e) => debug!("could not tell whose login it was: {e}"),
        }
    }
    Ok(())
}

/// Whether the session at `path` is a desktop login of user `uid` on this
/// computer: not a text console, ssh, a remote desktop or a login screen.
async fn desktop_login_of(
    bus: &zbus::Connection,
    path: OwnedObjectPath,
    uid: u32,
) -> zbus::Result<bool> {
    let session = zbus::Proxy::new(
        bus,
        "org.freedesktop.login1",
        path,
        "org.freedesktop.login1.Session",
    )
    .await?;
    let (user, _): (u32, OwnedObjectPath) = session.get_property("User").await?;
    let kind: String = session.get_property("Type").await?;
    let class: String = session.get_property("Class").await?;
    let remote: bool = session.get_property("Remote").await?;
    Ok(is_desktop_login(user == uid, &kind, &class, remote))
}

/// What makes a session one to open the window for: the user's own, a
/// graphical one, a person's (not a login screen's), and here.
fn is_desktop_login(own: bool, kind: &str, class: &str, remote: bool) -> bool {
    own && matches!(kind, "wayland" | "x11") && class == "user" && !remote
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_users_own_desktop_logins_count() {
        assert!(is_desktop_login(true, "wayland", "user", false));
        assert!(is_desktop_login(true, "x11", "user", false));
        assert!(
            !is_desktop_login(false, "wayland", "user", false),
            "someone else"
        );
        assert!(
            !is_desktop_login(true, "tty", "user", false),
            "a text console or ssh"
        );
        assert!(
            !is_desktop_login(true, "x11", "user", true),
            "a remote desktop"
        );
        assert!(
            !is_desktop_login(true, "wayland", "greeter", false),
            "a login screen"
        );
        assert!(!is_desktop_login(true, "unspecified", "user", false));
    }

    #[tokio::test]
    async fn a_new_desktop_is_ready_once_its_tray_comes_up() {
        tray_went_away();
        let login = Login {
            trays_up: TRAYS_UP.load(Ordering::Acquire),
            tray_gone: true,
        };
        let ready = tokio::spawn(login.desktop_ready());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!ready.is_finished(), "waits for the tray");
        tray_appeared();
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .expect("ready once the tray is up")
            .unwrap();
    }
}
