//! JSON-RPC 2.0 over a Unix domain socket, one JSON object (or batch array)
//! per line.
//!
//! Each connection gets a task of its own, which reads requests and hands
//! them to the [`Controller`], and forwards the notifications the
//! connection subscribed to. The socket and its directory are private to
//! the user who runs the daemon.

use crate::controller::{Controller, Subscriptions};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};
use weir_protocol::*;

/// Listen on `path`, replacing a socket left behind by a daemon that died,
/// but refusing to start next to one that is still running.
pub async fn bind(path: &Path) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_) => bail!(
                "another weir-daemon is already listening on {}",
                path.display()
            ),
            Err(_) => {
                debug!("removing stale socket {}", path.display());
                std::fs::remove_file(path).ok();
            }
        }
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    info!("listening on {}", path.display());
    Ok(listener)
}

/// Accept connections for as long as the daemon runs.
pub async fn serve(listener: UnixListener, controller: Arc<Controller>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let c = controller.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(stream, c).await {
                        debug!("connection ended: {e}");
                    }
                });
            }
            Err(e) => {
                warn!("accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Serve one connection until it closes.
async fn handle_conn(stream: UnixStream, controller: Arc<Controller>) -> Result<()> {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let mut rx = controller.subscribe();
    let mut subs = Subscriptions::default();
    // Track whether this connection has claimed to be the mixer window, so
    // the tray can tell an open window from none. The guard makes sure the
    // count comes back down however the connection ends.
    let mut window = WindowClient::new(controller.clone());
    // Set when this connection wanted to be the window while another one
    // already was: it was told to close, and hears nothing more for windows.
    let mut surplus_window = false;
    // Likewise for the spectra this connection watches.
    let mut spectra = SpectrumWatch::new(controller.clone());
    let mut meters = MeterThrottle::default();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line? {
                    Some(line) => {
                        if let Some(resp) = process_line(&line, &controller, &mut subs) {
                            w.write_all(resp.as_bytes()).await?;
                            w.write_all(b"\n").await?;
                        }
                        let wants_window = subs.topics.contains(&Topic::Window);
                        if !window.set(wants_window && !surplus_window) {
                            // A second window, from the menu while one is
                            // open or from the desktop reopening it at login:
                            // close it and bring the first one forward.
                            info!("a mixer window is already open, so closing the new one");
                            surplus_window = true;
                            w.write_all(Notification::Quit.to_wire().as_bytes()).await?;
                            w.write_all(b"\n").await?;
                            controller.show_window();
                        }
                        spectra.set(&subs.spectrum);
                    }
                    None => break,
                }
            }
            n = rx.recv() => {
                match n {
                    Ok(n) => {
                        let wanted = match &n {
                            Notification::Spectrum(s) => subs.spectrum.contains(&s.target),
                            n if n.topic() == Topic::Window && surplus_window => false,
                            n => subs.topics.contains(&n.topic()),
                        };
                        if !wanted {
                            continue;
                        }
                        let n = match (n, subs.meter_rate_hz) {
                            (Notification::Meters(m), Some(hz)) => match meters.offer(m, hz) {
                                Some(m) => Notification::Meters(m),
                                None => continue,
                            },
                            (n, _) => n,
                        };
                        let text = n.to_wire();
                        w.write_all(text.as_bytes()).await?;
                        w.write_all(b"\n").await?;
                    }
                    Err(RecvError::Lagged(n)) => debug!("client lagged, dropped {n} notifications"),
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }
    Ok(())
}

/// Handle one line: a request, or a batch of them as a JSON array, which
/// is answered with an array of responses in the same order. Returns the
/// line to send back, if any.
fn process_line(line: &str, controller: &Controller, subs: &mut Subscriptions) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            let err = RpcResponse::err(Value::Null, RpcError::parse_error(e.to_string()));
            return Some(serde_json::to_string(&err).unwrap());
        }
    };
    match value {
        Value::Array(items) if items.is_empty() => {
            let err = RpcResponse::err(Value::Null, RpcError::invalid_request("an empty batch"));
            Some(serde_json::to_string(&err).unwrap())
        }
        Value::Array(items) => {
            let responses: Vec<RpcResponse> = items
                .into_iter()
                .filter_map(|v| process_request(v, controller, subs))
                .collect();
            // A batch of notifications gets no answer at all.
            (!responses.is_empty()).then(|| serde_json::to_string(&responses).unwrap())
        }
        v => process_request(v, controller, subs).map(|r| serde_json::to_string(&r).unwrap()),
    }
}

/// Handle one request. `None` for a notification, which gets no response.
fn process_request(
    v: Value,
    controller: &Controller,
    subs: &mut Subscriptions,
) -> Option<RpcResponse> {
    let mut envelope: RpcRequest = match serde_json::from_value(v) {
        Ok(r) => r,
        Err(e) => {
            return Some(RpcResponse::err(
                Value::Null,
                RpcError::invalid_request(e.to_string()),
            ))
        }
    };
    let id = envelope.id.clone();
    let result = controller
        .resolve_names(&envelope.method, &mut envelope.params)
        .and_then(|_| envelope.parse())
        .and_then(|req| controller.handle(req, subs));
    let id = id?;
    Some(match result {
        Ok(v) => RpcResponse::ok(id, v),
        Err(e) => RpcResponse::err(id, e),
    })
}

/// Meters for a connection that wants them less often than the daemon makes
/// them. Nothing is dropped: the meters in between are folded into the next
/// ones sent, so each carries the peaks since the one before.
struct MeterThrottle {
    held: Option<Meters>,
    sent: Instant,
}

impl Default for MeterThrottle {
    fn default() -> Self {
        Self {
            held: None,
            // Long enough ago that the first meters go out at once.
            sent: Instant::now() - Duration::from_secs(1),
        }
    }
}

impl MeterThrottle {
    /// Take in `m`, and return what to send now, if anything, at no more
    /// than `hz` a second.
    fn offer(&mut self, m: Meters, hz: u32) -> Option<Meters> {
        match self.held.as_mut() {
            Some(held) => held.merge(&m),
            None => self.held = Some(m),
        }
        if self.sent.elapsed() < Duration::from_secs_f64(1.0 / hz as f64) {
            return None;
        }
        self.sent = Instant::now();
        self.held.take()
    }
}

/// Keeps the daemon's count of attached mixer windows correct for one
/// connection, including when that connection drops without saying goodbye.
struct WindowClient {
    controller: Arc<Controller>,
    attached: bool,
}

impl WindowClient {
    fn new(controller: Arc<Controller>) -> Self {
        Self {
            controller,
            attached: false,
        }
    }

    /// Claim or release the window. Returns false when the claim failed
    /// because another connection is the window.
    fn set(&mut self, attached: bool) -> bool {
        if attached == self.attached {
            return true;
        }
        if attached {
            if !self.controller.claim_window() {
                return false;
            }
        } else {
            self.controller.remove_window_client();
        }
        self.attached = attached;
        true
    }
}

/// Keeps the daemon's count of spectrum watchers right for one connection,
/// including when it drops without saying goodbye.
struct SpectrumWatch {
    controller: Arc<Controller>,
    targets: BTreeSet<StripOrBus>,
}

impl SpectrumWatch {
    fn new(controller: Arc<Controller>) -> Self {
        Self {
            controller,
            targets: BTreeSet::new(),
        }
    }

    fn set(&mut self, targets: &BTreeSet<StripOrBus>) {
        if *targets == self.targets {
            return;
        }
        let added: Vec<_> = targets.difference(&self.targets).copied().collect();
        let removed: Vec<_> = self.targets.difference(targets).copied().collect();
        self.controller.adjust_spectrum_watch(&added, &removed);
        self.targets = targets.clone();
    }
}

impl Drop for SpectrumWatch {
    fn drop(&mut self) {
        let removed: Vec<_> = self.targets.iter().copied().collect();
        self.controller.adjust_spectrum_watch(&[], &removed);
    }
}

impl Drop for WindowClient {
    fn drop(&mut self) {
        if self.attached {
            self.controller.remove_window_client();
        }
    }
}
