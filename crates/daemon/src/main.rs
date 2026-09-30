//! `weir-daemon`: owns the audio engine, the configuration and the control
//! socket. The window, `weirctl` and any other client talk to it over
//! JSON-RPC.
//!
//! * [`config`]: the files under `~/.config/weir`.
//! * [`controller`]: the mixer state and every request handler.
//! * [`server`]: the control socket.
//! * [`history`]: undo and redo.
//! * [`tray`]: the system tray icon.
//! * [`defaults`]: the mixer a first run starts with.

mod config;
mod controller;
mod defaults;
mod history;
mod login;
mod server;
mod tray;

use anyhow::{Context, Result};
use clap::Parser;
use controller::Controller;
use ksni::TrayMethods;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{signal, Signal, SignalKind};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{debug, error, info, warn};
use tray::{MixerTray, TrayCommand};
use weir_engine::{Engine, EngineEvent};
use weir_protocol::{AppRule, MixerState, Notification, Startup};

#[derive(Parser, Debug)]
#[command(name = "weir-daemon", version, about = "Weir mixer daemon")]
struct Args {
    /// Configuration file (default: ~/.config/weir/config.toml).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Control socket path (default: $XDG_RUNTIME_DIR/weir/control.sock).
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Run without connecting to PipeWire (control API only; for development).
    #[arg(long)]
    no_engine: bool,
    /// Ignore the saved configuration and start from the built-in default.
    #[arg(long)]
    reset_config: bool,
    /// Do not show a system tray icon, whatever the configuration says.
    #[arg(long)]
    no_tray: bool,
    /// Do not open the mixer window, whatever the configuration says.
    #[arg(long)]
    no_window: bool,
}

/// Why the main loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Exit,
    Restart,
}

/// The longest the daemon may take to shut down before it is ended by force.
/// Comfortably above the sum of the limits of the individual steps.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(5);

/// How often the configuration is saved, when something changed.
const SAVE_INTERVAL: Duration = Duration::from_millis(500);

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let args = Args::parse();
    let paths = config::Paths::resolve(args.config.clone());
    let socket_path = args
        .socket
        .clone()
        .unwrap_or_else(weir_protocol::default_socket_path);

    back_up_config(&paths);
    let loaded = load_config(&paths, args.reset_config)?;
    let first_run = loaded.first_run;

    let (ev_tx, ev_rx) = tokio::sync::mpsc::unbounded_channel();
    let engine = if args.no_engine {
        warn!("running without the audio engine (--no-engine)");
        None
    } else {
        Some(
            Engine::spawn(loaded.mixer.clone(), move |ev| {
                let _ = ev_tx.send(ev);
            })
            .context("starting the audio engine")?,
        )
    };

    let controller = Arc::new(Controller::new(
        loaded.mixer,
        engine,
        paths,
        loaded.settings,
        loaded.app_rules,
    ));
    controller.restore_current(loaded.scene, loaded.setup);
    controller.apply_engine_options();
    controller.refresh_start_at_login();
    if first_run {
        controller.save_if_dirty();
    }
    spawn_background_tasks(&controller, ev_rx);

    let listener = server::bind(&socket_path).await?;
    let serve = tokio::spawn(server::serve(listener, controller.clone()));

    let (tray_tx, tray_rx) = tokio::sync::mpsc::unbounded_channel::<TrayCommand>();
    // Lets a client ask for the window over the control socket, not just the
    // tray, so a hotkey or a stream deck can raise the mixer.
    controller.set_window_sender(tray_tx.clone());
    let tray = if !args.no_tray && controller.settings().tray {
        start_tray(&controller, tray_tx).await
    } else {
        None
    };

    if !args.no_window {
        match controller.settings().startup {
            Startup::Window => {
                spawn_window(&socket_path, false);
            }
            Startup::Minimized => {
                spawn_window(&socket_path, true);
            }
            startup @ Startup::TrayOnly => {
                info!("starting without a window ({})", startup.label())
            }
        }
    }

    let mut signals = StopSignals::new()?;
    let outcome = run(
        &controller,
        &socket_path,
        &mut signals,
        serve,
        tray_rx,
        tray.as_ref(),
    )
    .await;

    // From here on, every step has a time limit, and the watchdog covers
    // anything that still manages to hang. The session manager waits for
    // this process before it can log out or power off.
    start_shutdown_watchdog(SHUTDOWN_LIMIT);
    shut_down(&controller, tray, &socket_path).await;

    // A stop that arrived while shutting down for a restart cancels the
    // restart: coming back now would only hold up whoever asked us to stop.
    if outcome == Outcome::Restart {
        if signals.stop_pending().await {
            info!("asked to stop while restarting, so not restarting");
        } else {
            return restart();
        }
    }
    info!("bye");
    Ok(())
}

/// Log to standard output, at `info` unless `RUST_LOG` says otherwise.
fn init_logging() {
    // Colors only on a terminal: in a log file or the journal the codes show
    // up as clutter.
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(std::io::stdout().is_terminal())
        .init();
}

/// Keep a copy of the configuration from before this run touches anything,
/// `--reset-config` included.
fn back_up_config(paths: &config::Paths) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match config::backup(&paths.config_file, &paths.backups_dir, now) {
        Ok(Some(path)) => info!("backed up the configuration to {}", path.display()),
        Ok(None) => {}
        Err(e) => warn!("could not back up the configuration: {e:#}"),
    }
}

/// What the daemon starts from.
struct Loaded {
    settings: config::Settings,
    mixer: MixerState,
    app_rules: Vec<AppRule>,
    scene: Option<String>,
    setup: Option<String>,
    /// There was no configuration, so this is the default layout.
    first_run: bool,
}

/// Read the configuration, or start from the default layout when there is
/// none or `reset` asks for it. A file that cannot be read stops the
/// daemon: carrying on would save the default over it.
fn load_config(paths: &config::Paths, reset: bool) -> Result<Loaded> {
    let file = paths.config_file.display();
    let mut loaded = match (reset, config::load(&paths.config_file)) {
        (false, Ok(Some((cfg, report)))) => {
            for step in &report.migrated {
                info!("configuration upgraded: {step}");
            }
            if let Some(v) = report.from_newer {
                warn!(
                    "{file} was written by a newer Weir (format {v}); a copy is kept next to \
                     it, because saving from this version drops what it does not understand"
                );
            }
            Loaded {
                settings: cfg.settings,
                mixer: cfg.mixer,
                app_rules: cfg.app_rules,
                scene: cfg.current_scene,
                setup: cfg.current_setup,
                first_run: false,
            }
        }
        (false, Ok(None)) | (true, _) => {
            info!("no configuration found, using the default layout");
            Loaded {
                settings: config::Settings::default(),
                mixer: defaults::default_mixer(),
                app_rules: Vec::new(),
                scene: None,
                setup: None,
                first_run: true,
            }
        }
        (false, Err(e)) => {
            error!("{e:#}");
            anyhow::bail!("could not read configuration; fix or move {file} and retry");
        }
    };
    for fix in loaded.mixer.normalize() {
        warn!("config fixed: {fix}");
    }
    info!(
        "loaded {} strips and {} buses from {file}",
        loaded.mixer.strips.len(),
        loaded.mixer.buses.len(),
    );
    Ok(loaded)
}

/// The tasks that run for as long as the daemon does: engine events into
/// the controller, the meter tick, and saving the configuration a moment
/// after it changes rather than on every fader movement.
fn spawn_background_tasks(
    controller: &Arc<Controller>,
    mut events: UnboundedReceiver<EngineEvent>,
) {
    let c = controller.clone();
    tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            c.on_engine_event(ev);
        }
    });

    let c = controller.clone();
    let hz = c.settings().meter_rate_hz.clamp(1, 120);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(1000 / hz as u64));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            c.tick();
        }
    });

    let c = controller.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SAVE_INTERVAL);
        loop {
            interval.tick().await;
            c.save_if_dirty();
        }
    });
}

/// Show the tray icon. Failing to is not fatal: there may be no tray, no
/// session bus, or no desktop at all when running headless.
async fn start_tray(
    controller: &Controller,
    tx: UnboundedSender<TrayCommand>,
) -> Option<ksni::Handle<MixerTray>> {
    let settings = controller.settings();
    let mut tray = MixerTray::new(tx, settings.startup, settings.tray_icon);
    let mixer = controller.mixer();
    (tray.strips, tray.buses) = (mixer.strips.len(), mixer.buses.len());
    match tray.spawn().await {
        Ok(h) => {
            info!("tray icon registered");
            Some(h)
        }
        Err(e) => {
            warn!("no tray icon: {e}");
            None
        }
    }
}

/// The signals that stop the daemon, and the one that restarts it.
///
/// SIGTERM, SIGINT and SIGHUP all mean "stop". SIGHUP in particular is what
/// a session sends alongside SIGTERM when it ends, so it must never mean
/// anything else: treating it as "restart" once let the daemon replace
/// itself in the middle of a shutdown and hold the shutdown up. Restarting
/// has its own signal, SIGUSR1, as well as the tray entry.
struct StopSignals {
    term: Signal,
    int: Signal,
    hup: Signal,
    usr1: Signal,
}

impl StopSignals {
    fn new() -> Result<Self> {
        Ok(Self {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
            hup: signal(SignalKind::hangup())?,
            usr1: signal(SignalKind::user_defined1())?,
        })
    }

    /// Whether a stop signal has arrived and not been seen yet, without
    /// waiting for one.
    async fn stop_pending(&mut self) -> bool {
        tokio::select! {
            biased;
            _ = self.term.recv() => true,
            _ = self.int.recv() => true,
            _ = self.hup.recv() => true,
            _ = std::future::ready(()) => false,
        }
    }
}

/// The daemon's life: serve clients, act on the tray, keep the tray up to
/// date, until something says to stop or restart.
async fn run(
    controller: &Controller,
    socket_path: &Path,
    signals: &mut StopSignals,
    mut serve: tokio::task::JoinHandle<()>,
    mut tray_rx: UnboundedReceiver<TrayCommand>,
    tray: Option<&ksni::Handle<MixerTray>>,
) -> Outcome {
    // Keeps the tray's icon, menu and tooltip in step with the mixer.
    let mut notes = controller.subscribe();
    let mut notes_open = true;
    loop {
        tokio::select! {
            // Checked in order, so a stop that arrives together with a
            // restart request always wins.
            biased;
            _ = signals.term.recv() => {
                info!("terminated");
                return Outcome::Exit;
            }
            _ = signals.int.recv() => {
                info!("interrupted");
                return Outcome::Exit;
            }
            _ = signals.hup.recv() => {
                info!("hung up, which means the session is ending");
                return Outcome::Exit;
            }
            _ = signals.usr1.recv() => {
                info!("restarting on SIGUSR1");
                return Outcome::Restart;
            }
            _ = &mut serve => {
                error!("server task ended");
                return Outcome::Exit;
            }
            command = tray_rx.recv() => match command {
                Some(TrayCommand::Show) => {
                    if controller.window_attached() {
                        debug!("a window is already open, asking it to come forward");
                        controller.show_window();
                    } else {
                        spawn_window(socket_path, false);
                    }
                }
                Some(TrayCommand::SetStartup(mode)) => set_startup(controller, mode),
                Some(TrayCommand::Restart) => {
                    info!("restarting at the tray's request");
                    return Outcome::Restart;
                }
                Some(TrayCommand::Exit) => {
                    info!("quitting at the tray's request");
                    return Outcome::Exit;
                }
                None => return Outcome::Exit,

            },
            note = notes.recv(), if notes_open && tray.is_some() => {
                let Some(handle) = tray else { continue };
                match note {
                    Ok(Notification::SettingsChanged(s)) => {
                        handle.update(|t| (t.startup, t.icon) = (s.startup, s.tray_icon)).await;
                    }
                    Ok(Notification::EngineChanged(e)) => {
                        handle.update(|t| t.engine_state = e.state).await;
                    }
                    Ok(Notification::StateChanged(m)) => {
                        let counts = (m.strips.len(), m.buses.len());
                        handle.update(|t| (t.strips, t.buses) = counts).await;
                    }
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => notes_open = false,
                }
            }
        }
    }
}

/// Change the startup mode, as the tray's menu asked, the way a client's
/// request would, so clients hear about it.
fn set_startup(controller: &Controller, mode: Startup) {
    info!("startup mode is now {mode:?}");
    let patch = weir_protocol::SettingsPatch {
        startup: Some(mode),
        ..Default::default()
    };
    let mut subs = controller::Subscriptions::default();
    if let Err(e) = controller.handle(weir_protocol::Request::SetSettings(patch), &mut subs) {
        warn!("could not change the startup mode: {e}");
    }
    controller.save_if_dirty();
}

/// Close the window, save, stop the engine and the tray, and remove the
/// socket, each within a time limit.
async fn shut_down(
    controller: &Controller,
    tray: Option<ksni::Handle<MixerTray>>,
    socket_path: &Path,
) {
    // Close the window before the mixer it is showing goes away, and give it
    // a moment to act on that. No window attached means nothing to wait for.
    controller.quit_windows();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1000);
    while controller.window_attached() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    controller.save_if_dirty();
    if let Some(engine) = controller.engine() {
        engine.shutdown(Duration::from_secs(2));
    }
    if let Some(handle) = tray {
        // The session bus may already be gone when the session is ending.
        if tokio::time::timeout(Duration::from_secs(1), handle.shutdown())
            .await
            .is_err()
        {
            warn!("the tray icon did not go away in time");
        }
    }
    let _ = std::fs::remove_file(socket_path);
}

/// Replace this process with a fresh copy of itself, with the same
/// arguments. Replacing rather than forking means whatever started us (a
/// shell, or systemd) keeps supervising the same pid. Returns only if that
/// fails.
fn restart() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().context("finding our own binary to restart")?;
    let rest: Vec<String> = std::env::args().skip(1).collect();
    let error = std::process::Command::new(&exe).args(&rest).exec();
    anyhow::bail!("could not restart {}: {error}", exe.display());
}

/// Start the mixer window. Returns false when the binary cannot be found,
/// which is not fatal: the daemon is perfectly usable without it.
fn spawn_window(socket: &Path, minimized: bool) -> bool {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("weir"));
        }
    }
    candidates.push(PathBuf::from("weir"));
    for candidate in candidates {
        // tokio's process driver reaps the child once we drop the handle.
        // Plain std::process would leave a zombie behind every time the
        // window is started, because nothing here ever waits on it.
        let mut cmd = tokio::process::Command::new(&candidate);
        // Without this the window would try to start a second daemon.
        cmd.arg("--no-spawn");
        if socket != weir_protocol::default_socket_path() {
            cmd.arg("--socket").arg(socket);
        }
        if minimized {
            cmd.arg("--minimized");
        }
        match cmd
            .stdin(std::process::Stdio::null())
            .kill_on_drop(false)
            .spawn()
        {
            Ok(child) => {
                info!("started {}", candidate.display());
                drop(child);
                return true;
            }
            Err(e) => debug!("could not start {}: {e}", candidate.display()),
        }
    }
    warn!("the mixer window (weir) was not found next to this binary or on PATH");
    false
}

/// End the process after `limit`, whatever it is doing by then.
///
/// `_exit` skips destructors and exit handlers on purpose: whatever is stuck
/// may be holding the locks those would need. Exiting with status 0 keeps
/// systemd's `Restart=on-failure` from starting the daemon again after the
/// user chose to quit.
fn start_shutdown_watchdog(limit: Duration) {
    let spawned = std::thread::Builder::new()
        .name("shutdown-watchdog".into())
        .spawn(move || {
            std::thread::sleep(limit);
            error!("shutdown took longer than {limit:?}, exiting now");
            // SAFETY: _exit is async-signal-safe and takes no locks.
            unsafe { libc::_exit(0) };
        });
    if let Err(e) = spawned {
        warn!("no shutdown watchdog: {e}");
    }
}
