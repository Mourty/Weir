//! The window's connection to the daemon.
//!
//! A thread of its own connects, reconnects when the daemon goes away, and
//! starts the daemon when there is none (see [`spawn_daemon`]). Requests
//! from the window go to it through a channel. A second thread reads what
//! the daemon sends and mirrors it into [`Shared`], which the window reads
//! at the start of each frame, then asks egui for a new frame.

use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};
use weir_protocol::*;

/// What the connection knows, shared with the window.
#[derive(Default)]
pub struct Shared {
    pub connected: bool,
    /// Why the last attempt to connect failed.
    pub connect_error: Option<String>,
    /// The last error the daemon answered a request with, and when.
    pub last_error: Option<(String, Option<Instant>)>,
    /// The daemon's state, kept up to date by its notifications.
    pub state: FullState,
    /// The latest meter readings, and a count bumped with each, so the
    /// window can tell new ones from ones it has seen.
    pub meters: Meters,
    pub meters_seq: u64,
    /// Whether this window started the daemon.
    pub spawned_daemon: bool,
    /// Bumped each time the daemon asks this window to come forward.
    pub show_seq: u64,
    /// Set when the daemon is going away and wants the window closed.
    pub quit_requested: bool,
    /// The latest equalizer spectrum of each watched strip or bus, and when
    /// it arrived.
    pub spectra: HashMap<StripOrBus, (Spectrum, Instant)>,
    /// What this window asked to watch, asked again after reconnecting.
    pub watching: Vec<StripOrBus>,
    /// What can be undone and redone.
    pub history: HistoryInfo,
    /// Why the daemon this window started stopped, when it did not stop
    /// cleanly.
    pub daemon_exit: Option<DaemonExit>,
    /// Start the daemon again: set by the window's Try again button.
    pub retry_spawn: bool,
    /// A window look imported, for the window to take once.
    pub window_look: Option<Value>,
}

/// A daemon started by this window that stopped with an error.
#[derive(Clone, Debug)]
pub struct DaemonExit {
    /// What it printed about why, trimmed to the lines that matter.
    pub reason: String,
    /// Everything it printed.
    pub log: PathBuf,
}

/// The window's end of the connection.
pub struct Client {
    pub shared: Arc<Mutex<Shared>>,
    tx: mpsc::Sender<Request>,
}

impl Client {
    /// Start connecting to the daemon at `socket`, starting the daemon
    /// first when `allow_spawn` says so and there is none.
    pub fn spawn(socket: PathBuf, ctx: egui::Context, allow_spawn: bool) -> Client {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = mpsc::channel::<Request>();
        let s = shared.clone();
        std::thread::Builder::new()
            .name("weir-client".into())
            .spawn(move || connection_loop(socket, ctx, s, rx, allow_spawn))
            .expect("spawn client thread");
        Client { shared, tx }
    }

    /// Send a request. Its answer only matters if it is an error, which
    /// shows in [`Shared::last_error`]; what it changed arrives as
    /// notifications. Requests made while disconnected are dropped.
    pub fn send(&self, req: Request) {
        let _ = self.tx.send(req);
    }

    /// Watch these strips' and buses' spectra, and only these.
    pub fn watch_spectrum(&self, targets: Vec<StripOrBus>) {
        {
            let mut sh = self.shared.lock().unwrap();
            sh.spectra.retain(|t, _| targets.contains(t));
            sh.watching = targets.clone();
        }
        self.send(Request::WatchSpectrum(WatchSpectrumParams { targets }));
    }
}

/// Connect, send the requests that come in, and on losing the connection
/// start over, until the window goes away.
fn connection_loop(
    socket: PathBuf,
    ctx: egui::Context,
    shared: Arc<Mutex<Shared>>,
    rx: mpsc::Receiver<Request>,
    allow_spawn: bool,
) {
    let mut tried_spawn = false;
    loop {
        let stream = match UnixStream::connect(&socket) {
            Ok(s) => s,
            Err(e) => {
                {
                    let mut sh = shared.lock().unwrap();
                    sh.connected = false;
                    sh.connect_error = Some(format!("{}: {e}", socket.display()));
                }
                let retry = std::mem::take(&mut shared.lock().unwrap().retry_spawn);
                if allow_spawn && (!tried_spawn || retry) {
                    tried_spawn = true;
                    if spawn_daemon(&socket, &shared, &ctx) {
                        let mut sh = shared.lock().unwrap();
                        sh.spawned_daemon = true;
                        sh.daemon_exit = None;
                    }
                }
                ctx.request_repaint();
                // Drain requests while disconnected so they do not pile up.
                while rx.try_recv().is_ok() {}
                std::thread::sleep(Duration::from_millis(700));
                continue;
            }
        };
        info!("connected to {}", socket.display());
        let disconnected = Arc::new(AtomicBool::new(false));
        let pending: Arc<Mutex<HashMap<u64, Answer>>> = Arc::new(Mutex::new(HashMap::new()));
        let reader_stream = match stream.try_clone() {
            Ok(s) => s,
            Err(e) => {
                warn!("clone failed: {e}");
                continue;
            }
        };
        {
            let mut sh = shared.lock().unwrap();
            sh.connected = true;
            sh.connect_error = None;
            // A daemon that stopped because another was already running
            // is no news once connected to that other one.
            sh.daemon_exit = None;
        }
        let reader = {
            let shared = shared.clone();
            let ctx = ctx.clone();
            let disconnected = disconnected.clone();
            let pending = pending.clone();
            std::thread::spawn(move || {
                read_loop(reader_stream, shared, ctx, pending);
                disconnected.store(true, Ordering::Release);
            })
        };
        let mut writer = stream;
        let mut next_id: u64 = 1;
        let send = |req: &Request, writer: &mut UnixStream, next_id: &mut u64| -> bool {
            let id = *next_id;
            *next_id += 1;
            pending.lock().unwrap().insert(id, Answer::to(req));
            let env = RpcRequest::new(id, req);
            let mut line = serde_json::to_string(&env).unwrap();
            line.push('\n');
            writer.write_all(line.as_bytes()).is_ok()
        };
        // Topic::Window is not in Topic::ALL on purpose: subscribing to it is
        // how this process tells the daemon it is the mixer window, so the
        // tray can raise an open window instead of starting a second one.
        let mut topics = Topic::ALL.to_vec();
        topics.push(Topic::Window);
        let watching = shared.lock().unwrap().watching.clone();
        let initial = [
            Request::Hello,
            Request::Subscribe(SubscribeParams {
                topics,
                meter_rate_hz: None,
            }),
            Request::GetState,
            Request::History,
            // A new connection starts watching nothing.
            Request::WatchSpectrum(WatchSpectrumParams { targets: watching }),
        ];
        let mut ok = true;
        for r in &initial {
            if !send(r, &mut writer, &mut next_id) {
                ok = false;
                break;
            }
        }
        while ok && !disconnected.load(Ordering::Acquire) {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(req) => {
                    if !send(&req, &mut writer, &mut next_id) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let _ = writer.shutdown(std::net::Shutdown::Both);
        let _ = reader.join();
        {
            let mut sh = shared.lock().unwrap();
            sh.connected = false;
            sh.connect_error = Some("connection to the Weir daemon lost".into());
        }
        ctx.request_repaint();
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// What the window takes from the answer to a request. Most answers only
/// matter for their errors: what a request changed arrives as a
/// notification.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Answer {
    /// The whole state.
    State,
    /// What can be undone and redone.
    History,
    Nothing,
}

impl Answer {
    fn to(req: &Request) -> Self {
        match req {
            Request::GetState => Answer::State,
            Request::History | Request::Undo(_) | Request::Redo(_) => Answer::History,
            _ => Answer::Nothing,
        }
    }
}

/// Read what the daemon sends until the connection closes, mirroring it
/// into `shared`.
fn read_loop(
    stream: UnixStream,
    shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
    pending: Arc<Mutex<HashMap<u64, Answer>>>,
) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let msg = match ServerMessage::parse(line.trim()) {
            Ok(m) => m,
            Err(e) => {
                warn!("bad message from daemon: {e}");
                continue;
            }
        };
        let mut sh = shared.lock().unwrap();
        match msg {
            ServerMessage::Notification(n) => match n {
                Notification::StateChanged(m) => sh.state.mixer = m,
                Notification::Meters(m) => {
                    sh.meters = m;
                    sh.meters_seq += 1;
                }
                Notification::DevicesChanged(d) => sh.state.devices = d,
                Notification::AppsChanged(a) => sh.state.apps = a,
                Notification::EngineChanged(e) => sh.state.engine = e,
                Notification::SettingsChanged(s) => sh.state.settings = s,
                Notification::EqPresetsChanged(p) => sh.state.eq_presets = p,
                Notification::Spectrum(s) => {
                    // A spectrum can still be in flight after its window
                    // closed; do not keep it.
                    if sh.watching.contains(&s.target) {
                        sh.spectra.insert(s.target, (s, Instant::now()));
                    }
                }
                Notification::ShowWindow => sh.show_seq += 1,
                Notification::Quit => sh.quit_requested = true,
                Notification::HistoryChanged(h) => sh.history = h,
                Notification::AppRulesChanged(r) => sh.state.app_rules = r,
                Notification::LibraryChanged(l) => sh.state.library = l,
                Notification::SystemVolumesChanged(v) => sh.state.system_volumes = v,
                Notification::InsertsChanged(v) => sh.state.inserts = v,
                Notification::HotkeysChanged(h) => sh.state.hotkeys = h,
                Notification::WindowLook(look) => sh.window_look = Some(look),
            },
            ServerMessage::Response(r) => {
                let answer =
                    r.id.as_u64()
                        .and_then(|id| pending.lock().unwrap().remove(&id))
                        .unwrap_or(Answer::Nothing);
                if let Some(e) = r.error {
                    sh.last_error = Some((e.message, Some(Instant::now())));
                } else if let Some(result) = r.result {
                    apply_result(&mut sh, answer, result);
                }
            }
        }
        drop(sh);
        ctx.request_repaint();
    }
}

fn apply_result(sh: &mut Shared, answer: Answer, result: Value) {
    match answer {
        Answer::State => {
            if let Ok(s) = serde_json::from_value::<FullState>(result) {
                sh.state = s;
            }
        }
        Answer::History => {
            if let Ok(h) = serde_json::from_value::<HistoryInfo>(result) {
                sh.history = h;
            }
        }
        Answer::Nothing => {}
    }
}

/// Where a daemon started by the window writes what it prints.
fn daemon_log_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("weir")
        .join("daemon.log")
}

/// A fresh log file for a daemon about to start, opened twice: once for its
/// output and once for its errors. `None` when it cannot be created, and the
/// daemon then prints wherever this window does.
fn open_daemon_log(path: &Path) -> Option<(Stdio, Stdio)> {
    std::fs::create_dir_all(path.parent()?).ok()?;
    let out = std::fs::File::create(path).ok()?;
    let err = out.try_clone().ok()?;
    Some((out.into(), err.into()))
}

/// Try to start `weir-daemon` from next to our own binary or from PATH.
/// What it prints goes to a log file, and a thread waits for it, so that if
/// it stops with an error the window can say why rather than wait forever.
fn spawn_daemon(socket: &Path, shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) -> bool {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("weir-daemon"));
        }
    }
    candidates.push(PathBuf::from("weir-daemon"));
    let log = daemon_log_path();
    for c in candidates {
        let mut cmd = std::process::Command::new(&c);
        // This process is the window, so the daemon must not open another.
        // Without this you get two: the one you launched and the one the
        // daemon opens for its startup mode.
        cmd.arg("--no-window");
        if socket != default_socket_path() {
            cmd.arg("--socket").arg(socket);
        }
        cmd.stdin(Stdio::null());
        if let Some((out, err)) = open_daemon_log(&log) {
            cmd.stdout(out).stderr(err);
        }
        match cmd.spawn() {
            Ok(child) => {
                info!("started {}, logging to {}", c.display(), log.display());
                watch_daemon(child, log, shared.clone(), ctx.clone());
                return true;
            }
            Err(_) => continue,
        }
    }
    warn!("weir-daemon not found; start it manually");
    false
}

/// Wait for a daemon this window started, and if it stops with an error,
/// put why into `shared` for the window to show. Waiting also collects the
/// finished process, which would otherwise linger until the window closes.
fn watch_daemon(mut child: Child, log: PathBuf, shared: Arc<Mutex<Shared>>, ctx: egui::Context) {
    let spawned = std::thread::Builder::new()
        .name("weir-daemon-watch".into())
        .spawn(move || {
            let Ok(status) = child.wait() else {
                return;
            };
            // A clean exit is the daemon being quit on purpose; the window
            // is told about that over the socket.
            if status.success() {
                return;
            }
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let reason = exit_reason(&text, &status.to_string());
            warn!("weir-daemon stopped ({status}): {reason}");
            shared.lock().unwrap().daemon_exit = Some(DaemonExit { reason, log });
            ctx.request_repaint();
        });
    if let Err(e) = spawned {
        warn!("cannot watch the daemon: {e}");
    }
}

/// Whether `line` starts a message the daemon logged, which begins with
/// the time, as in `2026-09-28T14:30:34.042722Z`.
fn is_stamped(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() > 20 && b[0].is_ascii_digit() && b[4] == b'-' && b[10] == b'T'
}

/// The part of a stopped daemon's output that says why it stopped: the
/// errors it logged, the error it ended with and a panic, each with the
/// lines that belong to it but without stack traces. Failing those, its
/// last few lines.
fn exit_reason(log: &str, status: &str) -> String {
    let mut kept: Vec<Vec<&str>> = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in log.lines() {
        let lower = line.to_ascii_lowercase();
        let ends = lower.starts_with("stack backtrace:") || lower.starts_with("note: run with");
        let error = line.starts_with("Error:") || line.starts_with("thread '");
        if is_stamped(line) || error || ends {
            kept.extend(current.take());
            if error {
                current = Some(vec![line]);
            } else if is_stamped(line) {
                // Logged errors, without the time in front.
                current = line
                    .split_once(" ERROR ")
                    .map(|(_, rest)| vec![rest.trim()]);
            }
        } else if let Some(entry) = current.as_mut() {
            if !line.trim().is_empty() {
                entry.push(line);
            }
        }
    }
    kept.extend(current);
    let lines: Vec<&str> = if kept.is_empty() {
        let all: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
        all[all.len().saturating_sub(6)..].to_vec()
    } else {
        kept[kept.len().saturating_sub(3)..].concat()
    };
    if lines.is_empty() {
        return format!("It stopped ({status}) without printing anything.");
    }
    lines[lines.len().saturating_sub(16)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::exit_reason;

    #[test]
    fn a_stopped_daemon_is_explained_by_its_last_words() {
        let log = "2026-09-28T14:15:55.1Z  INFO weir_daemon: loaded 4 strips\n\
                   2026-09-28T14:15:55.2Z ERROR weir_daemon: parsing config.toml: TOML \
                   parse error at line 1\n  |\n1 | strips = [\n\n\
                   Error: could not read configuration; fix or move config.toml\n\n\
                   Stack backtrace:\n   0: anyhow::msg\n             at src/x.rs:1\n";
        assert_eq!(
            exit_reason(log, "exit status: 1"),
            "weir_daemon: parsing config.toml: TOML parse error at line 1\n  |\n\
             1 | strips = [\n\
             Error: could not read configuration; fix or move config.toml"
        );

        let log = "2026-09-28T14:15:55.1Z  INFO weir_daemon: starting\n\
                   thread 'main' panicked at src/x.rs:1:2:\nindex out of bounds\n\
                   note: run with `RUST_BACKTRACE=1` to display a backtrace\n";
        assert_eq!(
            exit_reason(log, "exit status: 101"),
            "thread 'main' panicked at src/x.rs:1:2:\nindex out of bounds"
        );

        let log = "2026-09-28T14:15:55.1Z  INFO weir_daemon: one\nplain words\n";
        assert_eq!(
            exit_reason(log, "signal: 6"),
            "2026-09-28T14:15:55.1Z  INFO weir_daemon: one\nplain words"
        );

        assert!(exit_reason("", "signal: 9").contains("signal: 9"));
    }
}
