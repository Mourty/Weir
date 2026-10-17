//! What the daemon knows about Weir's newest release, kept for clients;
//! the checking itself is [`crate::updates`].

use super::Controller;
use crate::diagnostics::Install;
use serde_json::Value;
use tracing::{info, warn};
use weir_protocol::*;

impl Controller {
    /// Waits until a check is asked for: by `check_for_update`, or by
    /// switching checks on.
    pub fn update_wanted(&self) -> tokio::sync::futures::Notified<'_> {
        self.update_wanted.notified()
    }

    /// What is known about Weir's newest release, with how to update here.
    pub(super) fn update_status(&self) -> UpdateStatus {
        let mut status = self.inner.lock().unwrap().update.clone();
        status.how_to_update = Install::here().how_to_update();
        status
    }

    /// Check now. Answers at once; `update_changed` tells what was found.
    pub(super) fn check_for_update(&self) -> Result<Value, RpcError> {
        self.update_wanted.notify_one();
        let mut status = self.update_status();
        status.checking = true;
        Ok(to_json(&status))
    }

    /// A check started.
    pub fn update_checking(&self) {
        self.inner.lock().unwrap().update.checking = true;
        self.announce(Notification::UpdateChanged(self.update_status()));
    }

    /// A check ended with `found`. Returns whether GitHub answered. A check
    /// that failed keeps what the last one found.
    pub fn update_checked(&self, found: Result<Option<Release>, String>) -> bool {
        let ok = found.is_ok();
        {
            let mut inner = self.inner.lock().unwrap();
            let u = &mut inner.update;
            u.checking = false;
            u.checked_at_ms = Some(crate::history::now_ms());
            match found {
                Ok(latest) => {
                    u.newer = latest
                        .as_ref()
                        .is_some_and(|r| is_newer(&r.version, env!("CARGO_PKG_VERSION")));
                    if u.newer {
                        let version = latest.as_ref().map_or("", |r| r.version.as_str());
                        info!("Weir {version} is out");
                    }
                    u.latest = latest;
                    u.error = None;
                }
                Err(e) => {
                    warn!("could not check for a newer Weir: {e}");
                    u.error = Some(e);
                }
            }
        }
        self.announce(Notification::UpdateChanged(self.update_status()));
        ok
    }
}
