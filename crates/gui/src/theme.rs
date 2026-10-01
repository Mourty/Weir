//! Colors and sizes. Every color the window draws with comes from the
//! [`Palette`] in use, dark or light; sizes shared by more than one part of
//! the window are constants here.

use egui::Color32;
use std::sync::RwLock;

/// Every color the window draws with. [`DARK`] and [`LIGHT`] fill it in, and
/// [`p`] returns the one in use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// Dark or light, for egui's own widgets.
    pub dark: bool,
    /// The window background, between strips.
    pub bg: Color32,
    /// Behind a virtual strip, a hardware strip and a bus: slightly
    /// different, so the three kinds can be told apart at a glance.
    pub strip_bg: Color32,
    pub strip_bg_hw: Color32,
    pub bus_bg: Color32,
    /// Fader tracks, meter backgrounds and text fields.
    pub track: Color32,
    /// Secondary text: captions, hints and explanations.
    pub text_dim: Color32,
    pub text: Color32,
    /// The filled part of a fader's track, for strips and buses without a
    /// color of their own.
    pub fader: Color32,
    pub fader_handle: Color32,
    /// A handle under the pointer.
    pub handle_hover: Color32,
    pub meter_green: Color32,
    pub meter_yellow: Color32,
    pub meter_red: Color32,
    /// Text that warns, such as a device that is not plugged in or an
    /// effect that cannot run. The meter's yellow in the dark palette, and
    /// a darker amber in the light one, where yellow is too faint to read.
    pub warning: Color32,
    /// Mute, solo and mono while they are on.
    pub mute_on: Color32,
    pub solo_on: Color32,
    pub mono_on: Color32,
    /// A routing button that is on, to a bus that plays to a device and to
    /// a virtual one.
    pub route_on: Color32,
    pub route_on_virtual: Color32,
    /// A button that is off.
    pub button_off: Color32,
    /// A button under the pointer.
    pub hover_fill: Color32,
    /// A button being pressed.
    pub active_fill: Color32,
    /// Floating windows and menus.
    pub window_fill: Color32,
    /// The strip of controls along the top.
    pub top_bar: Color32,
    /// Weir's teal, or the desktop's accent color: highlights, selections
    /// and the equalizer curve.
    pub accent: Color32,
    /// Text on a lit button.
    pub on_text: Color32,
    /// Effect buttons: noise suppression, noise gate, equalizer, compressor.
    pub ns_on: Color32,
    pub gate_on: Color32,
    pub eq_on: Color32,
    pub comp_on: Color32,
    /// Ducking: its switch, and the ring on a routing button while it is
    /// turning that mix down.
    pub duck: Color32,
    /// The safety limiter's ceiling handle, and its light while it is working.
    pub limit: Color32,
    /// External effects that are connected. While they are not, they are
    /// shown in `meter_yellow`.
    pub ext_on: Color32,
    /// An indicator light that is off, and its label.
    pub lamp_off: Color32,
    pub lamp_off_text: Color32,
    /// The label on a lit indicator light.
    pub lamp_on_text: Color32,
    /// Graph grid lines in the settings window, minor and major, and the
    /// 0 dB line.
    pub grid: Color32,
    pub grid_major: Color32,
    pub grid_zero: Color32,
    /// A folding section header in the settings window.
    pub section_fill: Color32,
    /// Lines and rings that must stand out on a graph: the current level,
    /// the band being edited.
    pub marker: Color32,
}

/// The dark palette, and the default.
pub const DARK: Palette = Palette {
    dark: true,
    bg: Color32::from_rgb(24, 27, 33),
    strip_bg: Color32::from_rgb(38, 42, 51),
    strip_bg_hw: Color32::from_rgb(41, 44, 55),
    bus_bg: Color32::from_rgb(35, 40, 47),
    track: Color32::from_rgb(20, 22, 27),
    text_dim: Color32::from_rgb(150, 158, 170),
    text: Color32::from_rgb(225, 228, 235),
    fader: Color32::from_rgb(120, 200, 150),
    fader_handle: Color32::from_rgb(205, 215, 225),
    handle_hover: Color32::from_rgb(255, 255, 255),
    meter_green: Color32::from_rgb(70, 200, 100),
    meter_yellow: Color32::from_rgb(235, 200, 70),
    meter_red: Color32::from_rgb(235, 75, 65),
    warning: Color32::from_rgb(235, 200, 70),
    mute_on: Color32::from_rgb(205, 65, 60),
    solo_on: Color32::from_rgb(230, 190, 50),
    mono_on: Color32::from_rgb(150, 120, 230),
    route_on: Color32::from_rgb(60, 145, 225),
    route_on_virtual: Color32::from_rgb(60, 190, 175),
    button_off: Color32::from_rgb(52, 57, 68),
    hover_fill: Color32::from_rgb(66, 72, 86),
    active_fill: Color32::from_rgb(80, 88, 104),
    window_fill: Color32::from_rgb(32, 36, 44),
    top_bar: Color32::from_rgb(30, 33, 40),
    accent: Color32::from_rgb(90, 180, 165),
    on_text: Color32::from_rgb(0, 0, 0),
    ns_on: Color32::from_rgb(205, 110, 170),
    gate_on: Color32::from_rgb(90, 195, 115),
    eq_on: Color32::from_rgb(230, 145, 60),
    comp_on: Color32::from_rgb(180, 140, 240),
    duck: Color32::from_rgb(230, 162, 60),
    limit: Color32::from_rgb(230, 162, 60),
    ext_on: Color32::from_rgb(120, 185, 245),
    lamp_off: Color32::from_rgb(42, 46, 55),
    lamp_off_text: Color32::from_rgb(90, 97, 112),
    lamp_on_text: Color32::from_rgb(11, 13, 16),
    grid: Color32::from_rgb(40, 44, 54),
    grid_major: Color32::from_rgb(58, 63, 76),
    grid_zero: Color32::from_rgb(90, 96, 112),
    section_fill: Color32::from_rgb(46, 51, 62),
    marker: Color32::from_rgb(255, 255, 255),
};

/// The light palette, for desktops set to light.
pub const LIGHT: Palette = Palette {
    dark: false,
    bg: Color32::from_rgb(222, 226, 231),
    strip_bg: Color32::from_rgb(246, 247, 249),
    strip_bg_hw: Color32::from_rgb(240, 242, 247),
    bus_bg: Color32::from_rgb(239, 243, 245),
    track: Color32::from_rgb(210, 215, 222),
    text_dim: Color32::from_rgb(96, 103, 114),
    text: Color32::from_rgb(35, 38, 41),
    fader: Color32::from_rgb(56, 160, 106),
    fader_handle: Color32::from_rgb(118, 126, 140),
    handle_hover: Color32::from_rgb(52, 58, 70),
    meter_green: Color32::from_rgb(36, 168, 78),
    meter_yellow: Color32::from_rgb(214, 166, 20),
    meter_red: Color32::from_rgb(218, 56, 46),
    warning: Color32::from_rgb(150, 98, 0),
    mute_on: Color32::from_rgb(214, 64, 58),
    solo_on: Color32::from_rgb(232, 186, 40),
    mono_on: Color32::from_rgb(146, 112, 230),
    route_on: Color32::from_rgb(66, 146, 226),
    route_on_virtual: Color32::from_rgb(54, 184, 168),
    button_off: Color32::from_rgb(226, 229, 234),
    hover_fill: Color32::from_rgb(208, 213, 221),
    active_fill: Color32::from_rgb(194, 200, 210),
    window_fill: Color32::from_rgb(250, 250, 251),
    top_bar: Color32::from_rgb(234, 237, 241),
    accent: Color32::from_rgb(34, 140, 124),
    on_text: Color32::from_rgb(0, 0, 0),
    ns_on: Color32::from_rgb(214, 110, 172),
    gate_on: Color32::from_rgb(84, 194, 110),
    eq_on: Color32::from_rgb(236, 146, 56),
    comp_on: Color32::from_rgb(174, 132, 240),
    duck: Color32::from_rgb(222, 146, 36),
    limit: Color32::from_rgb(222, 146, 36),
    ext_on: Color32::from_rgb(110, 172, 240),
    lamp_off: Color32::from_rgb(218, 222, 228),
    lamp_off_text: Color32::from_rgb(136, 142, 154),
    lamp_on_text: Color32::from_rgb(11, 13, 16),
    grid: Color32::from_rgb(226, 229, 234),
    grid_major: Color32::from_rgb(204, 209, 216),
    grid_zero: Color32::from_rgb(150, 157, 168),
    section_fill: Color32::from_rgb(232, 235, 240),
    marker: Color32::from_rgb(35, 38, 41),
};

static PALETTE: RwLock<Palette> = RwLock::new(DARK);

/// The palette in use.
pub fn p() -> Palette {
    *PALETTE.read().unwrap_or_else(|e| e.into_inner())
}

/// The color a bus's routing buttons light up in: one for buses that play
/// to a device, another for virtual ones.
pub fn bus_color(bus: &weir_protocol::Bus) -> Color32 {
    match bus.kind {
        weir_protocol::BusKind::Hardware => p().route_on,
        weir_protocol::BusKind::Virtual => p().route_on_virtual,
    }
}

/// `palette` with the accent color replaced, for following the desktop's.
/// Keeps it readable against the palette's background.
pub fn with_accent(mut palette: Palette, accent: Color32) -> Palette {
    let luma = |c: Color32| 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
    let bg = luma(palette.bg);
    let mut c = accent;
    // Push it away from the background until the two differ enough.
    for _ in 0..8 {
        if (luma(c) - bg).abs() >= 70.0 {
            break;
        }
        c = if palette.dark {
            c.lerp_to_gamma(Color32::WHITE, 0.2)
        } else {
            c.lerp_to_gamma(Color32::BLACK, 0.2)
        };
    }
    palette.accent = c;
    palette
}

/// Smallest a strip may be drawn, before its meter and routing buttons push
/// it wider.
pub const STRIP_MIN_WIDTH: f32 = 138.0;
/// Smallest a bus may be drawn.
pub const BUS_MIN_WIDTH: f32 = 124.0;
/// Height of the band between the name and the fader. Strips put the source
/// picker and the pan control there, buses put the device and their source
/// list, and both reserve the same height so every fader starts at the same
/// line.
pub const BAND_H: f32 = 96.0;
/// Width of a fader.
pub const FADER_W: f32 = 30.0;
/// Routing buttons are tall enough for two lines: the bus, and the strip's
/// level in that bus's mix when it is not 0 dB.
pub const ROUTE_BTN_W: f32 = 44.0;
pub const ROUTE_BTN_H: f32 = 28.0;
/// Space between routing buttons.
pub const ROUTE_GAP: f32 = 3.0;
/// Height of the row of effect buttons under the fader.
pub const FX_ROW_H: f32 = 22.0;
/// Width of the column left of a bus meter that holds the limiter's
/// ceiling handle.
pub const CEILING_W: f32 = 12.0;
/// The CLIP and LIM lights either side of the level readout.
pub const LAMP_W: f32 = 26.0;
pub const LAMP_H: f32 = 16.0;

/// Draw with `palette` from now on, in every window.
pub fn apply(ctx: &egui::Context, palette: Palette) {
    *PALETTE.write().unwrap_or_else(|e| e.into_inner()) = palette;
    let mut visuals = if palette.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = palette.bg;
    visuals.window_fill = palette.window_fill;
    visuals.extreme_bg_color = palette.track;
    visuals.widgets.noninteractive.bg_fill = palette.strip_bg;
    visuals.widgets.inactive.bg_fill = palette.button_off;
    visuals.widgets.inactive.weak_bg_fill = palette.button_off;
    visuals.widgets.hovered.bg_fill = palette.hover_fill;
    visuals.widgets.hovered.weak_bg_fill = palette.hover_fill;
    visuals.widgets.active.bg_fill = palette.active_fill;
    visuals.selection.bg_fill = palette.accent.linear_multiply(0.6);
    visuals.override_text_color = Some(palette.text);
    ctx.set_visuals(visuals);
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(4.0, 4.0);
    style.spacing.button_padding = egui::vec2(6.0, 3.0);
    ctx.set_style(style);
}
