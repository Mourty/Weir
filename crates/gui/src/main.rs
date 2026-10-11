//! `weir`: the mixer window. A thin client of `weir-daemon`.

mod app;
mod appearance;
mod client;
mod effects;
mod file_dialog;
mod fx_window;
mod hotkeys;
mod patch;
mod prefs;
mod sounds;
mod theme;
mod transfer;
mod widgets;

use clap::Parser;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// The window icon, for desktops that take it from the window rather than
/// from the installed desktop file (X11). One copy, so windows that pass it
/// every frame pass the same one. Rendered from `packaging/weir.svg` with
/// `make icons`.
pub fn icon() -> Arc<egui::IconData> {
    static ICON: OnceLock<Arc<egui::IconData>> = OnceLock::new();
    ICON.get_or_init(|| {
        Arc::new(
            eframe::icon_data::from_png_bytes(include_bytes!("../assets/weir.png"))
                .unwrap_or_default(),
        )
    })
    .clone()
}

#[derive(Parser, Debug)]
#[command(name = "weir", version, about = "Weir mixer window")]
struct Args {
    /// Control socket path (default: $XDG_RUNTIME_DIR/weir/control.sock).
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Do not start weir-daemon automatically when it is not running.
    #[arg(long)]
    no_spawn: bool,
    /// Start with the window minimized to the taskbar.
    #[arg(long)]
    minimized: bool,
}

fn main() -> eframe::Result {
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(std::io::stdout().is_terminal())
        .init();
    let args = Args::parse();
    // Ask the desktop whether it is light or dark before the window opens,
    // so it opens in the right colors.
    appearance::watch(std::time::Duration::from_millis(300));
    let socket = args
        .socket
        .clone()
        .unwrap_or_else(weir_protocol::default_socket_path);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Weir")
            .with_app_id("weir")
            .with_icon(icon())
            .with_inner_size([1180.0, 680.0])
            .with_min_inner_size([520.0, 420.0]),
        // With vsync on, showing a frame waits for the desktop to say it is
        // ready for the next one, which on Wayland it never does while the
        // window is minimized or on another desktop. The window then hung
        // until it was shown again: it could not even close at logout, and
        // was killed. Weir paces its own redraws (about 30 a second, see
        // `App::update`), so it does not need vsync to keep from spinning.
        vsync: false,
        ..Default::default()
    };
    eframe::run_native(
        "Weir",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::App::new(
                cc,
                socket,
                !args.no_spawn,
                args.minimized,
            )))
        }),
    )
}
