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
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
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

/// The longest line a client may send. Far more than any request takes,
/// and a limit all the same: one that never ends would otherwise be kept
/// in memory for ever.
const LINE_MAX: usize = 16 << 20;

/// What reading the next line from a client found.
enum Line {
    /// A whole line, without its newline.
    Whole(String),
    /// More than [`LINE_MAX`] bytes with no newline.
    TooLong,
    /// The client is gone.
    End,
}

/// Read up to the next newline into `buf`, at most [`LINE_MAX`] bytes.
/// What is read stays in `buf` if this is dropped part way, as it is when
/// a notification goes out first in a `select!`, so nothing is lost.
async fn next_line<R: AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> Result<Line> {
    loop {
        let room = (LINE_MAX + 1).saturating_sub(buf.len()) as u64;
        let n = r.take(room).read_until(b'\n', buf).await?;
        if buf.last() == Some(&b'\n') {
            buf.pop();
            let line = String::from_utf8_lossy(buf).into_owned();
            buf.clear();
            return Ok(Line::Whole(line));
        }
        if buf.len() > LINE_MAX {
            return Ok(Line::TooLong);
        }
        if n == 0 {
            // Whatever came last without a newline is a line too.
            if buf.is_empty() {
                return Ok(Line::End);
            }
            let line = String::from_utf8_lossy(buf).into_owned();
            buf.clear();
            return Ok(Line::Whole(line));
        }
    }
}

/// Serve one connection until it closes.
async fn handle_conn(stream: UnixStream, controller: Arc<Controller>) -> Result<()> {
    // Who is at the other end, to end a mixer window that stopped drawing.
    let peer = stream.peer_cred().ok().and_then(|c| c.pid());
    let conn = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
    // Track whether this connection has claimed to be the mixer window, so
    // the tray can tell an open window from none. The guard makes sure the
    // claim is let go however the connection ends.
    let mut window = WindowClient::new(controller.clone(), conn, peer);
    let served = serve_conn(stream, &controller, &mut window).await;
    if served.as_ref().is_err_and(|e| e.is::<NotReading>()) && window.attached {
        // Stopped reading while being the window, so its process hangs or
        // was stopped: it is closed, and another window may take its place.
        controller.window_hangs(conn);
    }
    served
}

/// Serve one connection, for [`handle_conn`].
async fn serve_conn(
    stream: UnixStream,
    controller: &Arc<Controller>,
    window: &mut WindowClient,
) -> Result<()> {
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut buf = Vec::new();
    let mut rx = controller.subscribe();
    let mut subs = Subscriptions::default();
    // Set when this connection wanted to be the window while another one
    // already was: it was told to close, and hears nothing more for windows.
    let mut surplus_window = false;
    // Set while this connection wants to be the window and another one
    // holds the place: whether that one still answers decides which goes.
    let mut checking: Option<Pin<Box<dyn Future<Output = bool> + Send>>> = None;
    // Tells the connection that was the window that it lost its place,
    // having stopped answering.
    let mut evictions = controller.window_evictions();
    // Likewise for the spectra this connection watches.
    let mut spectra = SpectrumWatch::new(controller.clone());
    let mut meters = MeterThrottle::default();
    loop {
        tokio::select! {
            line = next_line(&mut reader, &mut buf) => {
                match line? {
                    Line::TooLong => {
                        warn!("a client sent a line longer than {LINE_MAX} bytes; closing it");
                        let err = RpcResponse::err(
                            Value::Null,
                            RpcError::invalid_request(format!(
                                "a request may be {} MB at most",
                                LINE_MAX >> 20
                            )),
                        );
                        if let Ok(text) = serde_json::to_string(&err) {
                            send_line(&mut w, &text, WRITE_LIMIT).await?;
                        }
                        break;
                    }
                    Line::Whole(line) => {
                        if let Some(resp) = process_line(&line, controller, &mut subs) {
                            send_line(&mut w, &resp, WRITE_LIMIT).await?;
                        }
                        let wants_window = subs.topics.contains(&Topic::Window);
                        if checking.is_none() && !window.set(wants_window && !surplus_window) {
                            // A second window, from the menu while one is
                            // open or from the desktop reopening it at login.
                            // The open one is asked to come forward; if it
                            // answers, this one closes, and if not, this one
                            // takes its place.
                            let c = controller.clone();
                            checking = Some(Box::pin(async move { c.window_shown().await }));
                        }
                        subs.window = window.attached;
                        spectra.set(&subs.spectrum);
                    }
                    Line::End => break,
                }
            }
            alive = async { checking.as_mut().expect("checked").await }, if checking.is_some() => {
                checking = None;
                if !alive && window.set(true) {
                    info!("the new mixer window takes the place of the one that stopped answering");
                    subs.window = true;
                } else {
                    info!("a mixer window is already open, so closing the new one");
                    surplus_window = true;
                    send_line(&mut w, &Notification::Quit.to_wire(), WRITE_LIMIT).await?;
                }
            }
            Ok(()) = evictions.changed(), if window.attached => {
                if !window.lost() {
                    continue;
                }
                // Its process was ended too; a stand-in that stopped
                // answering is told to quit, if it still reads.
                info!("closing the connection of the mixer window that stopped answering");
                let quit = Notification::Quit.to_wire();
                let _ = send_line(&mut w, &quit, Duration::from_secs(1)).await;
                break;
            }
            n = rx.recv() => {
                match n {
                    Ok(n) => {
                        let wanted = match &n {
                            Notification::Spectrum(s) => subs.spectrum.contains(&s.target),
                            n if n.topic() == Topic::Window
                                && (surplus_window || checking.is_some()) =>
                            {
                                false
                            }
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
                        send_line(&mut w, &n.to_wire(), WRITE_LIMIT).await?;
                    }
                    Err(RecvError::Lagged(n)) => debug!("client lagged, dropped {n} notifications"),
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }
    Ok(())
}

/// How long a write to a client may wait for the client to read. One that
/// stops reading while the daemon has things to tell it, a process that
/// was stopped or hangs, is let go then: otherwise its connection would
/// wait for ever.
const WRITE_LIMIT: Duration = Duration::from_secs(30);

/// Numbers connections, to tell them apart as the mixer window.
static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

/// Write one line to a client, giving up after `limit` with [`NotReading`].
async fn send_line<W: AsyncWrite + Unpin>(w: &mut W, text: &str, limit: Duration) -> Result<()> {
    let write = async {
        w.write_all(text.as_bytes()).await?;
        w.write_all(b"\n").await
    };
    match tokio::time::timeout(limit, write).await {
        Ok(done) => Ok(done?),
        Err(_) => Err(NotReading(limit).into()),
    }
}

/// A client stopped reading for this long, and was let go.
#[derive(Debug)]
struct NotReading(Duration);

impl std::fmt::Display for NotReading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the client did not read for {:?}, so it was let go",
            self.0
        )
    }
}

impl std::error::Error for NotReading {}

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
    // A bug met while doing a request is kept as a crash report and
    // answered as an error, and the daemon carries on.
    let doing = weir_protocol::crash::doing(format!("request {}", envelope.method));
    let result = weir_protocol::crash::caught(|| {
        controller
            .resolve_names(&envelope.method, &mut envelope.params)
            .and_then(|_| envelope.parse())
            .and_then(|req| controller.handle(req, subs))
    })
    .unwrap_or_else(|why| {
        Err(RpcError::internal(format!(
            "Weir ran into a bug doing that ({why}). A crash report was kept, and the \
             mixer window shows it."
        )))
    });
    drop(doing);
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
    /// This connection's number and the process at its other end.
    conn: u64,
    pid: Option<i32>,
    attached: bool,
}

impl WindowClient {
    fn new(controller: Arc<Controller>, conn: u64, pid: Option<i32>) -> Self {
        Self {
            controller,
            conn,
            pid,
            attached: false,
        }
    }

    /// True when this connection was the window and lost its place, having
    /// stopped answering.
    fn lost(&self) -> bool {
        self.attached && !self.controller.is_window(self.conn)
    }

    /// Claim or release the window. Returns false when the claim failed
    /// because another connection is the window.
    fn set(&mut self, attached: bool) -> bool {
        if attached == self.attached {
            return true;
        }
        if attached {
            if !self.controller.claim_window(self.conn, self.pid) {
                return false;
            }
        } else {
            self.controller.release_window(self.conn);
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
            self.controller.release_window(self.conn);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_client_that_stops_reading_is_let_go() {
        // The other end never reads, so the small pipe fills.
        let (mut w, _other) = tokio::io::duplex(64);
        let limit = Duration::from_millis(100);
        send_line(&mut w, "short", limit).await.unwrap();
        let long = "x".repeat(1000);
        let started = Instant::now();
        assert!(send_line(&mut w, &long, limit).await.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn lines_are_read_whole_and_too_long_ones_refused() {
        let text = b"{\"a\": 1}\r\n\nlast without a newline";
        let mut r = BufReader::new(&text[..]);
        let mut buf = Vec::new();
        let mut got = Vec::new();
        loop {
            match next_line(&mut r, &mut buf).await.unwrap() {
                Line::Whole(l) => got.push(l),
                Line::TooLong => panic!("not too long"),
                Line::End => break,
            }
        }
        assert_eq!(got, ["{\"a\": 1}\r", "", "last without a newline"]);

        let long = vec![b'x'; LINE_MAX + 10];
        let mut r = BufReader::new(&long[..]);
        assert!(matches!(
            next_line(&mut r, &mut Vec::new()).await.unwrap(),
            Line::TooLong
        ));
    }
}
