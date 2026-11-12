//! The system tray icon.
//!
//! This lives in the daemon rather than the window, because the daemon is
//! what keeps running when the window is closed. A tray icon that vanished
//! with the window would be useless, and "start with no window at all" would
//! be impossible.
//!
//! The tray talks to the rest of the daemon through a channel. Menu callbacks
//! run on the tray's own task, so they must not touch the mixer directly.

use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Icon, MenuItem, OfflineReason, Status, ToolTip, Tray};
use std::sync::OnceLock;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info};
use weir_protocol::{clock, Startup, TrayIcon};

/// What the user picked from the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Bring the mixer window up, starting it if it is not running.
    Show,
    /// Open the recorder's window, starting the mixer window if need be.
    ShowRecorder,
    /// Start recording what is ticked for it.
    StartRecording,
    /// Stop recording.
    StopRecording,
    /// Stop everything and start again: opening the mixer window then
    /// (`window`; one that is open comes back anyway), in safe mode
    /// (`safe`).
    Restart { window: bool, safe: bool },
    /// Bring back everything safe mode holds back, and leave it.
    LeaveSafeMode,
    /// Stop everything.
    Exit,
    /// Change what happens the next time the daemon starts.
    SetStartup(Startup),
}

/// The tray icon and its menu. Its fields mirror the daemon's state; the
/// main loop updates them as that changes.
pub struct MixerTray {
    tx: UnboundedSender<TrayCommand>,
    /// Mirrored so the menu can show which option is selected.
    pub startup: Startup,
    /// Which icon to show.
    pub icon: TrayIcon,
    /// Mirrored for the tooltip.
    pub engine_state: String,
    /// How many strips there are, for the tooltip.
    pub strips: usize,
    /// How many buses there are, for the tooltip.
    pub buses: usize,
    /// While recording: for how long, in seconds, and how many tracks.
    pub recording: Option<(f64, usize)>,
    /// Whether any strip or bus is ticked for recording.
    pub can_record: bool,
    /// Whether Weir is in safe mode.
    pub safe_mode: bool,
}

impl MixerTray {
    /// A tray that sends what the user picks down `tx`.
    pub fn new(tx: UnboundedSender<TrayCommand>, startup: Startup, icon: TrayIcon) -> Self {
        Self {
            tx,
            startup,
            icon,
            engine_state: "starting".into(),
            strips: 0,
            buses: 0,
            recording: None,
            can_record: false,
            safe_mode: false,
        }
    }

    fn send(&self, command: TrayCommand) {
        // The receiver only goes away while the daemon is shutting down, at
        // which point there is nothing useful left to do anyway.
        let _ = self.tx.send(command);
    }
}

impl Tray for MixerTray {
    fn id(&self) -> String {
        "weir".into()
    }

    fn title(&self) -> String {
        "Weir".into()
    }

    /// The one-color icon by name, from the icon theme: packages and
    /// `make install` put `weir-symbolic.svg` in
    /// `share/icons/hicolor/symbolic/apps`, and Plasma recolors it to the
    /// panel's text color through its `ColorScheme-Text` class, as GNOME
    /// does any icon named `-symbolic`. The color icon goes as a picture
    /// instead ([`Self::icon_pixmap`]): given the name `weir`, Plasma shows
    /// `weir-symbolic` whenever it is installed.
    fn icon_name(&self) -> String {
        match self.icon {
            TrayIcon::Color if !color_pixmaps().is_empty() => String::new(),
            TrayIcon::Color => "weir".into(),
            TrayIcon::OneColor => "weir-symbolic".into(),
        }
    }

    /// A folder the desktop searches for the icon named above, besides
    /// its icon theme. An AppImage installs nothing (its menu entry adds
    /// icons, but only if asked for), so it points inside the AppImage,
    /// which stays mounted while the service runs.
    fn icon_theme_path(&self) -> String {
        appimage_icons().unwrap_or_default()
    }

    /// The color icon, with a red dot in its corner while recording.
    fn icon_pixmap(&self) -> Vec<Icon> {
        match self.icon {
            TrayIcon::Color if self.recording.is_some() => recording_pixmaps().to_vec(),
            TrayIcon::Color => color_pixmaps().to_vec(),
            TrayIcon::OneColor => Vec::new(),
        }
    }

    fn status(&self) -> Status {
        Status::Active
    }

    /// While recording, a red dot over an icon that goes by name, which
    /// Weir cannot draw on. Only Plasma shows these. It is given by name:
    /// one given as a picture Plasma cuts to its top-left quarter on a
    /// scaled display. The color icon has the dot drawn in instead
    /// ([`Self::icon_pixmap`]), which every desktop shows.
    fn overlay_icon_name(&self) -> String {
        if self.recording.is_some() && !self.icon_name().is_empty() {
            "media-record".into()
        } else {
            String::new()
        }
    }

    /// The desktop has no tray right now: at login before the panel is up,
    /// or while the panel restarts. Keep waiting rather than give up, so
    /// the icon shows up with the panel.
    fn watcher_offline(&self, reason: OfflineReason) -> bool {
        info!("waiting for the desktop's tray to appear");
        debug!("tray unavailable: {reason:?}");
        // It was there and has gone, rather than not there yet.
        if matches!(reason, OfflineReason::No) {
            crate::logins::tray_went_away();
        }
        true
    }

    fn watcher_online(&self) {
        info!("the desktop's tray is there; showing the tray icon");
        crate::logins::tray_appeared();
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: self.icon_name(),
            icon_pixmap: self.icon_pixmap(),
            title: "Weir".into(),
            description: format!(
                "Engine {}. {} strips, {} buses.{}{}",
                self.engine_state,
                self.strips,
                self.buses,
                self.recording
                    .map(|r| format!(" {}.", recording_words(r)))
                    .unwrap_or_default(),
                if self.safe_mode { " In safe mode." } else { "" }
            ),
        }
    }

    /// Left click opens the mixer, which is what people expect.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayCommand::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let selected = Startup::ALL
            .iter()
            .position(|s| *s == self.startup)
            .unwrap_or(0);
        vec![
            StandardItem {
                label: "Show the mixer".into(),
                icon_data: menu_icon().to_vec(),
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::Show)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ]
        .into_iter()
        .chain(self.recording_items())
        .chain([
            SubMenu {
                label: "When Weir starts".into(),
                submenu: vec![RadioGroup {
                    selected,
                    select: Box::new(|this: &mut Self, index| {
                        if let Some(mode) = Startup::ALL.get(index) {
                            this.startup = *mode;
                            this.send(TrayCommand::SetStartup(*mode));
                        }
                    }),
                    options: Startup::ALL
                        .iter()
                        .map(|mode| RadioItem {
                            label: mode.label().into(),
                            ..Default::default()
                        })
                        .collect(),
                }
                .into()],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Restart Weir".into(),
                icon_name: "view-refresh-symbolic".into(),
                activate: Box::new(|this: &mut Self| {
                    this.send(TrayCommand::Restart {
                        window: false,
                        safe: false,
                    })
                }),
                ..Default::default()
            }
            .into(),
            self.safe_mode_item(),
            StandardItem {
                label: "Quit Weir".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::Exit)),
                ..Default::default()
            }
            .into(),
        ])
        .collect()
    }
}

impl MixerTray {
    /// In safe mode a way out of it; otherwise a way in, for when Weir
    /// keeps crashing.
    fn safe_mode_item(&self) -> MenuItem<Self> {
        if self.safe_mode {
            StandardItem {
                label: "Leave safe mode".into(),
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::LeaveSafeMode)),
                ..Default::default()
            }
            .into()
        } else {
            StandardItem {
                label: "Restart in safe mode".into(),
                activate: Box::new(|this: &mut Self| {
                    this.send(TrayCommand::Restart {
                        window: true,
                        safe: true,
                    })
                }),
                ..Default::default()
            }
            .into()
        }
    }

    /// While recording, how long and a way to stop it; otherwise a way to
    /// start, when anything is ticked. Then the recorder's window.
    fn recording_items(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = match self.recording {
            Some(r) => vec![
                StandardItem {
                    label: recording_words(r),
                    icon_data: record_icon().to_vec(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Stop recording".into(),
                    icon_name: "media-playback-stop-symbolic".into(),
                    activate: Box::new(|this: &mut Self| this.send(TrayCommand::StopRecording)),
                    ..Default::default()
                }
                .into(),
            ],
            None if self.can_record => vec![StandardItem {
                label: "Start recording".into(),
                icon_data: record_icon().to_vec(),
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::StartRecording)),
                ..Default::default()
            }
            .into()],
            None => Vec::new(),
        };
        items.push(
            StandardItem {
                label: "Open the recorder".into(),
                activate: Box::new(|this: &mut Self| this.send(TrayCommand::ShowRecorder)),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items
    }
}

/// "Recording · 12:34 · 3 tracks".
fn recording_words((seconds, tracks): (f64, usize)) -> String {
    let s = if tracks == 1 { "" } else { "s" };
    format!("Recording · {} · {tracks} track{s}", clock(seconds))
}

/// A red dot of radius `r` in the middle of a `size` square, with a thin
/// dark ring a pixel further out so it shows on a red icon or panel too:
/// ARGB in network order, not premultiplied.
fn red_dot(size: i32, r: f32) -> Icon {
    let c = (size as f32 - 1.0) / 2.0;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            // Soft edges, a pixel wide.
            let fill = (r - d + 0.5).clamp(0.0, 1.0);
            let ring = (r + 1.0 - d + 0.5).clamp(0.0, 1.0) * 0.7;
            let a = fill + (1.0 - fill) * ring;
            let mix = |red: f32, dark: f32| ((red * fill + dark * (a - fill)) / a.max(1e-6)) as u8;
            data.extend([
                (a * 255.0) as u8,
                mix(235.0, 40.0),
                mix(75.0, 8.0),
                mix(65.0, 8.0),
            ]);
        }
    }
    Icon {
        width: size,
        height: size,
        data,
    }
}

/// The icon folder inside the AppImage Weir runs from, if it does.
fn appimage_icons() -> Option<String> {
    static DIR: OnceLock<Option<String>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = weir_protocol::appimage::appimage()?
            .dir
            .join("usr/share/icons");
        dir.is_dir().then(|| dir.to_string_lossy().into_owned())
    })
    .clone()
}

/// Weir's color icon for the menu, as a 32 px PNG; empty if it cannot be
/// made.
///
/// Menus get pictures, not names: COSMIC draws every named menu icon in
/// one color, so a color icon by name comes out as a white shape (Weir's
/// as a white square), and an AppImage's icons are in no theme. The other
/// entries' icons are named `-symbolic`, as one color is how COSMIC draws
/// them, and where a theme lacks that name the desktop falls back to the
/// plain one.
fn menu_icon() -> &'static [u8] {
    static PNG: OnceLock<Vec<u8>> = OnceLock::new();
    PNG.get_or_init(
        || match decode_square(include_bytes!("../../gui/assets/weir.png")) {
            Some((side, rgba)) => png_of(&shrink(&rgba, side, 32)),
            None => Vec::new(),
        },
    )
}

/// A red dot for the menu's recording entries, as a 32 px PNG, the size
/// of a media-record icon. Every desktop's theme has one, but COSMIC,
/// GNOME and Cinnamon draw it in one color.
fn record_icon() -> &'static [u8] {
    static PNG: OnceLock<Vec<u8>> = OnceLock::new();
    PNG.get_or_init(|| png_of(&red_dot(32, 9.0)))
}

/// `icon` as a PNG; empty if it cannot be made.
fn png_of(icon: &Icon) -> Vec<u8> {
    // ARGB back to RGBA.
    let pixels: Vec<u8> = icon
        .data
        .chunks_exact(4)
        .flat_map(|p| [p[1], p[2], p[3], p[0]])
        .collect();
    let (Ok(width), Ok(height)) = (u32::try_from(icon.width), u32::try_from(icon.height)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let written = encoder.write_header().and_then(|mut writer| {
        writer.write_image_data(&pixels)?;
        writer.finish()
    });
    if written.is_err() {
        return Vec::new();
    }
    out
}

/// The color icon with a red dot in its bottom right quarter, as Plasma
/// draws a recording program's icon, made once.
fn recording_pixmaps() -> &'static [Icon] {
    static ICONS: OnceLock<Vec<Icon>> = OnceLock::new();
    ICONS.get_or_init(|| color_pixmaps().iter().map(with_dot).collect())
}

/// `icon` with a red dot laid over its bottom right quarter.
fn with_dot(icon: &Icon) -> Icon {
    let size = icon.width;
    let side = (size + 1) / 2;
    let dot = red_dot(side, side as f32 / 2.0 - 1.0);
    let mut out = icon.clone();
    let at = size - side;
    for y in 0..side {
        for x in 0..side {
            let from = ((y * side + x) * 4) as usize;
            let to = (((at + y) * size + at + x) * 4) as usize;
            let (Some(top), Some(under)) =
                (dot.data.get(from..from + 4), out.data.get_mut(to..to + 4))
            else {
                continue;
            };
            // The dot over the icon, with straight (not premultiplied)
            // alpha.
            let a = top[0] as f32 / 255.0;
            let b = under[0] as f32 / 255.0;
            let both = a + b * (1.0 - a);
            if both <= 0.0 {
                continue;
            }
            let mut mixed = [(both * 255.0).round() as u8, 0, 0, 0];
            for c in 1..4 {
                let v = (top[c] as f32 * a + under[c] as f32 * b * (1.0 - a)) / both;
                mixed[c] = v.round() as u8;
            }
            under.copy_from_slice(&mixed);
        }
    }
    out
}

/// Weir's color icon in the sizes panels use, made once from the 256 px
/// one the window is built with; empty if that cannot be read.
fn color_pixmaps() -> &'static [Icon] {
    static ICONS: OnceLock<Vec<Icon>> = OnceLock::new();
    ICONS.get_or_init(
        || match decode_square(include_bytes!("../../gui/assets/weir.png")) {
            Some((side, rgba)) => [16, 22, 24, 32, 48, 64]
                .into_iter()
                .map(|size| shrink(&rgba, side, size))
                .collect(),
            None => Vec::new(),
        },
    )
}

/// A square 8-bit RGBA PNG: its side, and its pixels.
fn decode_square(png_bytes: &[u8]) -> Option<(usize, Vec<u8>)> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let rgba = info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight;
    if !rgba || info.width != info.height {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some((info.width as usize, buf))
}

/// `rgba`, a square `side` pixels wide, shrunk to `size` by averaging the
/// pixels each new one covers (weighted by their alpha, so transparent
/// ones lend no color), as ARGB in network order.
fn shrink(rgba: &[u8], side: usize, size: i32) -> Icon {
    let n = size as usize;
    let scale = side as f32 / n as f32;
    let span = |t: usize| {
        let from = (t as f32 * scale) as usize;
        let to = (((t + 1) as f32 * scale).ceil() as usize).clamp(from + 1, side);
        from..to
    };
    let mut data = Vec::with_capacity(n * n * 4);
    for ty in 0..n {
        for tx in 0..n {
            let (mut sum, mut alpha, mut count) = ([0.0f32; 3], 0.0f32, 0.0f32);
            for y in span(ty) {
                for x in span(tx) {
                    let p = &rgba[(y * side + x) * 4..][..4];
                    let a = p[3] as f32;
                    for c in 0..3 {
                        sum[c] += p[c] as f32 * a;
                    }
                    alpha += a;
                    count += 1.0;
                }
            }
            let color = |c: usize| {
                if alpha > 0.0 {
                    (sum[c] / alpha) as u8
                } else {
                    0
                }
            };
            data.extend([(alpha / count) as u8, color(0), color(1), color(2)]);
        }
    }
    Icon {
        width: size,
        height: size,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_gets_weirs_icon_as_a_small_png() {
        let (side, rgba) = decode_square(menu_icon()).expect("a square RGBA PNG");
        assert_eq!(side, 32);
        let at = |x: usize, y: usize| &rgba[(y * 32 + x) * 4..][..4];
        assert!(at(16, 16)[3] > 200, "solid in the middle");
    }

    #[test]
    fn recording_draws_a_dot_into_the_bottom_right_of_the_color_icon() {
        let plain = color_pixmaps();
        let dotted = recording_pixmaps();
        assert_eq!(plain.len(), dotted.len());
        for (plain, dotted) in plain.iter().zip(dotted) {
            let n = plain.width as usize;
            assert_eq!((dotted.width, dotted.height), (plain.width, plain.height));
            let at = |icon: &Icon, x: usize, y: usize| icon.data[(y * n + x) * 4..][..4].to_vec();
            assert_eq!(
                at(dotted, n / 4, n / 4),
                at(plain, n / 4, n / 4),
                "{n} px: top left as it was"
            );
            let middle = n - n.div_ceil(2) / 2 - 1;
            assert_eq!(
                at(dotted, middle, middle),
                [255, 235, 75, 65],
                "{n} px: red in the dot"
            );
        }
    }

    #[test]
    fn the_menus_recording_dot_is_a_red_png_with_room_around_it() {
        let (side, rgba) = decode_square(record_icon()).expect("a square RGBA PNG");
        assert_eq!(side, 32);
        let at = |x: usize, y: usize| &rgba[(y * 32 + x) * 4..][..4];
        assert_eq!(at(16, 16), [235, 75, 65, 255], "red in the middle");
        assert_eq!(at(2, 16)[3], 0, "clear at the sides");
    }

    #[test]
    fn the_recording_dot_fills_its_square_and_the_corners_are_clear() {
        let icon = red_dot(16, 7.0);
        assert_eq!(icon.data.len(), 16 * 16 * 4);
        let at = |x: usize, y: usize| &icon.data[(y * 16 + x) * 4..][..4];
        assert_eq!(at(0, 0)[0], 0, "clear in the corners");
        assert_eq!(at(8, 8), [255, 235, 75, 65], "red in the middle");
        assert!(at(8, 0)[0] > 0, "the ring reaches the edges");
        assert_eq!(recording_words((754.0, 3)), "Recording · 12:34 · 3 tracks");
        assert_eq!(recording_words((5.0, 1)), "Recording · 0:05 · 1 track");
    }

    #[test]
    fn the_color_icon_goes_as_pictures_and_the_one_color_by_name() {
        let icons = color_pixmaps();
        assert_eq!(icons.len(), 6);
        let small = &icons[0];
        assert_eq!(
            (small.width, small.height, small.data.len()),
            (16, 16, 16 * 16 * 4)
        );
        // Weir's icon is opaque in the middle and clear in the corners.
        let at = |i: &Icon, x: i32, y: i32| i.data[((y * i.width + x) * 4) as usize];
        assert_eq!(at(small, 8, 8), 255);
        assert_eq!(at(small, 0, 0), 0);
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let tray = MixerTray::new(tx, Startup::Window, TrayIcon::Color);
        assert_eq!(tray.icon_name(), "");
        assert_eq!(tray.icon_pixmap().len(), 6);
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let tray = MixerTray::new(tx, Startup::Window, TrayIcon::OneColor);
        assert_eq!(tray.icon_name(), "weir-symbolic");
        assert!(tray.icon_pixmap().is_empty());
    }
}
