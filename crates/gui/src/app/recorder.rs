//! The Recorder window: which strips and buses to record, how, and starting
//! and stopping; and what the mixer shows while it records, the REC pill in
//! the top bar and an outline and badge on each strip and bus recorded. The
//! daemon records; this window asks it to, and shows what it says.

use super::App;
use crate::file_dialog::{Dialog, Picked};
use crate::theme;
use crate::transfer::{Pane, WindowSize};
use egui::{pos2, vec2, Color32, CornerRadius, Rect, RichText, Sense, Stroke, Ui};
use std::path::{Path, PathBuf};
use weir_protocol::*;

/// Ready-made file name patterns, with what they are called.
const NAME_PRESETS: [(&str, &str); 3] = [
    ("Date and time", RECORD_NAMES_DEFAULT),
    ("Numbered", "Recording {number}/{track}"),
    ("By setup", "{setup}/{date} {time} {track}"),
];

/// Below this width the two halves of the window go one above the other.
const SIDE_BY_SIDE_W: f32 = 760.0;

/// The Recorder window's own state.
#[derive(Default)]
pub(super) struct RecorderWindow {
    open: bool,
    raise: bool,
    /// The folder as being typed, until Enter or leaving the field sends
    /// it.
    folder: Option<String>,
    /// The file name pattern as being typed, sent as it changes when it is
    /// usable.
    names: Option<String>,
    /// The minutes between new files, while being changed, so an older
    /// answer cannot pull the number back.
    split: Option<u32>,
    /// The folder dialog, while open.
    dialog: Option<Dialog>,
    /// Why there was no folder dialog to show.
    no_dialog: Option<String>,
}

impl RecorderWindow {
    /// A window open from the start, or not.
    pub(super) fn opened(open: bool) -> Self {
        Self {
            open,
            raise: open,
            ..Self::default()
        }
    }
}

/// A line of small dim text.
fn explain(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(theme::p().text_dim));
}

/// A group of things in a box, under its title, with a note beside it.
fn card(ui: &mut Ui, title: &str, note: &str, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::p().strip_bg)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(15.0).strong());
                ui.label(RichText::new(note).size(11.0).color(theme::p().text_dim));
            });
            ui.add_space(6.0);
            add(ui);
        });
}

/// "3 tracks".
fn tracks_words(n: usize) -> String {
    if n == 1 {
        "1 track".into()
    } else {
        format!("{n} tracks")
    }
}

/// A path with the home folder written as `~`.
fn home_short(path: &str) -> String {
    match dirs::home_dir().map(|h| h.display().to_string()) {
        Some(home) if path.starts_with(&home) => format!("~{}", &path[home.len()..]),
        _ => path.to_string(),
    }
}

/// A red dot, `radius` across the middle of `rect`.
fn rec_dot(ui: &Ui, center: egui::Pos2, radius: f32, on: bool) {
    let p = theme::p();
    let color = if on { p.rec } else { p.lamp_off_text };
    ui.painter().circle_filled(center, radius, color);
}

/// What a big button shows before its words.
#[derive(Clone, Copy)]
enum Mark {
    Dot,
    Square,
}

/// A red button with a white dot or square drawn before `text`; the font
/// has neither.
fn big_button(ui: &mut Ui, text: &str, mark: Mark, enabled: bool) -> egui::Response {
    let p = theme::p();
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(14.0),
        Color32::WHITE,
    );
    let size = vec2((galley.size().x + 50.0).max(104.0), 34.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let (fill, ink) = match (enabled, response.hovered()) {
        (false, _) => (p.button_off, p.text_dim),
        (true, true) => (p.rec.gamma_multiply(1.15), Color32::WHITE),
        (true, false) => (p.rec, Color32::WHITE),
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
    let left = rect.center().x - (galley.size().x + 22.0) / 2.0;
    let mark_c = pos2(left + 6.0, rect.center().y);
    match mark {
        Mark::Dot => ui.painter().circle_filled(mark_c, 5.5, ink),
        Mark::Square => ui.painter().rect_filled(
            Rect::from_center_size(mark_c, vec2(10.0, 10.0)),
            CornerRadius::same(1),
            ink,
        ),
    };
    ui.painter().galley_with_override_text_color(
        pos2(left + 20.0, rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// What a strip or bus is, as a track to record: its name, its channels in
/// words, how many channels, and whether it is a strip.
struct TrackRow {
    target: StripOrBus,
    source: TrackSource,
    layout: String,
    channels: usize,
}

/// Every strip, then every bus, as tracks.
fn track_rows(m: &MixerState) -> (Vec<TrackRow>, Vec<TrackRow>) {
    let strips = m
        .strips
        .iter()
        .map(|s| TrackRow {
            target: StripOrBus::Strip(s.id),
            source: TrackSource::Strip(s.name.clone()),
            layout: s.layout.label().to_lowercase(),
            channels: s.layout.positions().len(),
        })
        .collect();
    let buses = m
        .buses
        .iter()
        .map(|b| TrackRow {
            target: StripOrBus::Bus(b.id),
            source: TrackSource::Bus(b.name.clone()),
            layout: b.layout.label().to_lowercase(),
            channels: b.layout.positions().len(),
        })
        .collect();
    (strips, buses)
}

impl App {
    /// Open the Recorder window, or bring it forward.
    pub(super) fn open_recorder(&mut self) {
        self.recorder.open = true;
        self.recorder.raise = true;
    }

    /// Whether `target` is being recorded now.
    pub(super) fn recording_it(state: &FullState, target: StripOrBus) -> bool {
        let r = &state.recording;
        if !r.recording {
            return false;
        }
        let name = match target {
            StripOrBus::Strip(id) => state
                .mixer
                .strip(id)
                .map(|s| TrackSource::Strip(s.name.clone())),
            StripOrBus::Bus(id) => state
                .mixer
                .bus(id)
                .map(|b| TrackSource::Bus(b.name.clone())),
        };
        // The recording names what it records as it was called when it
        // started; renamed since, the settings have the new name.
        name.is_some_and(|n| {
            r.tracks.iter().any(|t| t.source.same(&n)) || state.recorder.records(&n)
        })
    }

    /// Start or stop recording.
    fn set_recording(&mut self, on: bool) {
        self.actions.push(Request::SetRecording(SetRecordingParams {
            on: Flag::Set(on),
        }));
    }

    /// Tick `target` for recording, or untick it.
    pub(super) fn set_record_track(&mut self, target: StripOrBus, on: bool) {
        let (strip, bus) = match target {
            StripOrBus::Strip(id) => (Some(id), None),
            StripOrBus::Bus(id) => (None, Some(id)),
        };
        self.actions
            .push(Request::SetRecordTrack(SetRecordTrackParams {
                strip,
                bus,
                on: Flag::Set(on),
            }));
    }

    fn set_recorder(&mut self, patch: RecorderPatch) {
        self.actions.push(Request::SetRecorder(patch));
    }

    /// The top bar's way to the recorder: a button, or while recording a
    /// red pill saying for how long.
    pub(super) fn recorder_button(&mut self, ui: &mut Ui, state: &FullState) {
        let r = &state.recording;
        if !r.recording {
            if ui
                .button("Recorder")
                .on_hover_text("Record strips and buses, each to its own track")
                .clicked()
            {
                self.open_recorder();
            }
            return;
        }
        let p = theme::p();
        let text = format!(
            "REC {} · {}",
            clock(r.seconds),
            tracks_words(r.tracks.len())
        );
        let galley =
            ui.painter()
                .layout_no_wrap(text, egui::FontId::proportional(12.0), p.rec_text);
        let size = vec2(galley.size().x + 30.0, ui.spacing().interact_size.y);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let fill = if response.hovered() {
            p.rec_fill.gamma_multiply(1.4)
        } else {
            p.rec_fill
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(10),
            fill,
            Stroke::new(1.0_f32, p.rec),
            egui::StrokeKind::Inside,
        );
        rec_dot(ui, pos2(rect.left() + 12.0, rect.center().y), 4.5, true);
        ui.painter().galley(
            pos2(rect.left() + 21.0, rect.center().y - galley.size().y / 2.0),
            galley,
            p.rec_text,
        );
        if response
            .on_hover_text("Recording. Click to open the recorder.")
            .clicked()
        {
            self.open_recorder();
        }
    }

    /// The red outline around a strip or bus being recorded, its frame at
    /// `rect`: outside it, so it leaves the color band along the top and a
    /// bus's own outline, while its mix is on the faders, as they are.
    pub(super) fn paint_recording_outline(ui: &Ui, rect: Rect) {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(9),
            Stroke::new(2.0_f32, theme::p().rec),
            egui::StrokeKind::Outside,
        );
    }

    /// The Recorder window, when open.
    pub(super) fn recorder_window(&mut self, ctx: &egui::Context, state: &FullState) {
        if !self.recorder.open {
            return;
        }
        self.poll_folder_dialog();
        let (mut raise, mut closed) = (self.recorder.raise, false);
        let size = WindowSize {
            size: [940.0, 680.0],
            min: [560.0, 420.0],
        };
        crate::transfer::window_sized(
            ctx,
            "recorder",
            "Recorder",
            size,
            &mut raise,
            &mut closed,
            |ui, pane| match pane {
                Pane::Body => {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| self.recorder_body(ui, state));
                }
                Pane::Footer => self.recorder_footer(ui, state),
            },
        );
        self.recorder.raise = raise;
        if closed {
            self.recorder.open = false;
        }
    }

    fn recorder_footer(&mut self, ui: &mut Ui, state: &FullState) {
        ui.horizontal(|ui| {
            let folder = PathBuf::from(&state.recording.folder);
            let there = folder.is_dir();
            if ui
                .add_enabled(there, egui::Button::new("Open the folder"))
                .on_hover_text(home_short(&state.recording.folder))
                .on_disabled_hover_text("Nothing has been recorded there yet.")
                .clicked()
            {
                open_folder(&folder);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let close = ui.button("Close");
                let close = if state.recording.recording {
                    close.on_hover_text("Recording carries on with the window closed")
                } else {
                    close
                };
                if close.clicked() {
                    self.recorder.open = false;
                }
            });
        });
    }

    fn recorder_body(&mut self, ui: &mut Ui, state: &FullState) {
        self.recording_header(ui, state);
        ui.add_space(12.0);
        if ui.available_width() >= SIDE_BY_SIDE_W {
            ui.columns(2, |cols| {
                self.tracks_card(&mut cols[0], state);
                self.recorder_settings_card(&mut cols[1], state);
            });
        } else {
            self.tracks_card(ui, state);
            ui.add_space(12.0);
            self.recorder_settings_card(ui, state);
        }
    }

    /// How the recording goes, and the button that starts or stops it.
    fn recording_header(&mut self, ui: &mut Ui, state: &FullState) {
        let p = theme::p();
        let r = &state.recording;
        let settings = &state.recorder;
        let (strips, buses) = track_rows(&state.mixer);
        let ticked: Vec<&TrackRow> = strips
            .iter()
            .chain(&buses)
            .filter(|t| settings.records(&t.source))
            .collect();
        egui::Frame::new()
            .fill(if r.recording { p.rec_fill } else { p.strip_bg })
            .stroke(Stroke::new(
                1.0_f32,
                if r.recording { p.rec } else { p.strip_bg },
            ))
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(18.0, 36.0), Sense::hover());
                    rec_dot(ui, dot.center(), 7.0, r.recording);
                    ui.vertical(|ui| {
                        let big = if r.recording {
                            clock(r.seconds)
                        } else {
                            "Not recording".to_string()
                        };
                        let color = if r.recording { p.rec_text } else { p.text };
                        ui.label(RichText::new(big).size(24.0).strong().color(color));
                        let line = if r.recording {
                            let mut words = vec![
                                format!("Recording {}", tracks_words(r.tracks.len())),
                                format!("{} so far", size_words(r.bytes)),
                            ];
                            if let Some(free) = r.free_bytes {
                                words.push(format!("{} free", size_words(free)));
                            }
                            words.join(" · ")
                        } else if ticked.is_empty() {
                            "Tick the strips and buses to record, below.".to_string()
                        } else {
                            let mut words = vec![format!("{} ticked", tracks_words(ticked.len()))];
                            if let Some(hour) = hourly_bytes(state, &ticked) {
                                words.push(format!("about {} an hour", size_words(hour)));
                            }
                            if let Some(free) = r.free_bytes {
                                words.push(format!("{} free", size_words(free)));
                            }
                            words.join(" · ")
                        };
                        ui.label(RichText::new(line).size(12.0).color(if r.recording {
                            p.rec_text
                        } else {
                            p.text_dim
                        }));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if r.recording {
                            if big_button(ui, "Stop", Mark::Square, true)
                                .on_hover_text("Stop, and finish the files")
                                .clicked()
                            {
                                self.set_recording(false);
                            }
                        } else {
                            let engine = state.engine.state == "streaming";
                            let can = !ticked.is_empty() && engine;
                            let why = if engine {
                                "Tick a strip or bus to record first."
                            } else {
                                "The audio engine is not running."
                            };
                            if big_button(ui, "Record", Mark::Dot, can)
                                .on_hover_text("Start recording what is ticked")
                                .on_disabled_hover_text(why)
                                .clicked()
                            {
                                self.set_recording(true);
                            }
                        }
                        ui.add_space(12.0);
                        ui.vertical(|ui| {
                            ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                                if let Some(next) = r.next_part_in.filter(|_| r.recording) {
                                    explain(ui, &format!("Next part starts in {}", clock(next)));
                                }
                                let into = if r.recording {
                                    "Writing to"
                                } else {
                                    "Recording to"
                                };
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "{into} {}",
                                            home_short(&recording_place(r))
                                        ))
                                        .size(11.0)
                                        .color(p.text_dim),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(recording_place(r));
                            });
                        });
                    });
                });
                if r.lost_seconds > 0.0 {
                    ui.label(
                        RichText::new(format!(
                            "{:.1} s of sound was lost because the computer fell behind. It \
                             is silence in the files, so the tracks still line up.",
                            r.lost_seconds
                        ))
                        .size(11.0)
                        .color(p.warning),
                    );
                }
                if r.recording && !r.missing.is_empty() {
                    let names: Vec<&str> = r.missing.iter().map(|t| t.name()).collect();
                    ui.label(
                        RichText::new(format!(
                            "Not recorded, since this setup has none called: {}",
                            names.join(", ")
                        ))
                        .size(11.0)
                        .color(p.warning),
                    );
                }
                if let Some(e) = r.error.as_ref().filter(|_| !r.recording) {
                    ui.label(
                        RichText::new(format!("The last recording stopped by itself: {e}"))
                            .size(11.0)
                            .color(p.warning),
                    );
                }
            });
    }

    /// The strips and buses, each with a box to tick it for recording.
    fn tracks_card(&mut self, ui: &mut Ui, state: &FullState) {
        let settings = &state.recorder;
        let note = match settings.layout {
            RecordLayout::Files => "Each one is its own file",
            RecordLayout::Multitrack => "All in one .mka file",
        };
        let (strips, buses) = track_rows(&state.mixer);
        let next = self.next_files(state);
        card(ui, "Tracks", note, |ui| {
            for (heading, rows) in [("STRIPS", &strips), ("BUSES", &buses)] {
                ui.label(
                    RichText::new(heading)
                        .size(10.0)
                        .strong()
                        .color(theme::p().text_dim),
                );
                for row in rows.iter() {
                    self.track_row(ui, state, row, &next);
                }
                ui.add_space(6.0);
            }
            explain(
                ui,
                "Strips record what you hear from them in the mix, after their effects and \
                 fader. Buses record what they send out. A strip's or bus's \"…\" menu has \
                 Record too.",
            );
        });
    }

    fn track_row(&mut self, ui: &mut Ui, state: &FullState, row: &TrackRow, next: &[String]) {
        let p = theme::p();
        let r = &state.recording;
        let ticked = state.recorder.records(&row.source);
        ui.horizontal(|ui| {
            let mut on = ticked;
            let name_w = 110.0;
            let box_r = ui
                .add_enabled(!r.recording, egui::Checkbox::without_text(&mut on))
                .on_disabled_hover_text("Stop recording to change what is recorded");
            if box_r.changed() {
                self.set_record_track(row.target, on);
            }
            let name =
                RichText::new(row.source.name()).color(if ticked { p.text } else { p.text_dim });
            cell(ui, name_w, egui::Label::new(name).truncate()).on_hover_text(row.source.name());
            let layout = RichText::new(&row.layout).size(11.0).color(p.text_dim);
            cell(ui, 56.0, egui::Label::new(layout).truncate());
            self.level_bar(ui, state, row.target);
            let file = if r.recording {
                r.tracks
                    .iter()
                    .find(|t| t.source.same(&row.source))
                    .map(|t| file_name(&t.file))
            } else if ticked {
                track_file(state, next, &row.source)
            } else {
                None
            };
            if let Some(file) = file {
                ui.add(
                    egui::Label::new(RichText::new(&file).size(11.0).color(p.text_dim)).truncate(),
                )
                .on_hover_text(file);
            }
        });
    }

    /// A small level meter for one strip or bus.
    fn level_bar(&self, ui: &mut Ui, state: &FullState, target: StripOrBus) {
        let (rect, _) = ui.allocate_exact_size(vec2(64.0, 6.0), Sense::hover());
        let p = theme::p();
        ui.painter()
            .rect_filled(rect, CornerRadius::same(2), p.track);
        let db = self
            .meters
            .get(&target)
            .and_then(|m| m.level.iter().copied().reduce(f32::max))
            .unwrap_or(METER_FLOOR_DB);
        // The meter's own scale, turned on its side.
        let t = crate::widgets::db_to_t(db).clamp(0.0, 1.0);
        if db > METER_FLOOR_DB && t > 0.0 {
            let paint = self.meter_paint(state);
            let fill = Rect::from_min_size(rect.min, vec2(rect.width() * t, rect.height()));
            ui.painter()
                .rect_filled(fill, CornerRadius::same(2), paint.color_at(db));
        }
    }

    /// Where each file of the next recording goes, from the pattern as
    /// typed: one per ticked strip or bus, or one for them all.
    fn next_files(&self, state: &FullState) -> Vec<String> {
        let s = &state.recorder;
        let pattern = self.recorder.names.as_deref().unwrap_or(&s.names);
        let (strips, buses) = track_rows(&state.mixer);
        let names: Vec<&str> = strips
            .iter()
            .chain(&buses)
            .filter(|t| s.records(&t.source))
            .map(|t| t.source.name())
            .collect();
        let parts = NameParts {
            pattern,
            layout: s.layout,
            format: s.format,
            when: LocalTime::now(),
            number: s.next_number,
            part: 1,
            splits: s.split_minutes > 0,
            setup: state.library.setup.as_deref(),
        };
        recording_files(&parts, &names)
    }

    /// How recordings are made.
    fn recorder_settings_card(&mut self, ui: &mut Ui, state: &FullState) {
        let s = state.recorder.clone();
        card(
            ui,
            "Settings",
            "Changes apply to the next recording",
            |ui| {
                ui.label(RichText::new("Save as").strong());
                let mut layout = s.layout;
                ui.radio_value(
                    &mut layout,
                    RecordLayout::Files,
                    "A folder of files, one per track",
                );
                ui.radio_value(
                    &mut layout,
                    RecordLayout::Multitrack,
                    "One file holding every track (.mka)",
                );
                if layout == RecordLayout::Multitrack {
                    explain(ui, "Some editors open only the first track of a .mka file.");
                }
                if layout != s.layout {
                    self.set_recorder(RecorderPatch {
                        layout: Some(layout),
                        ..Default::default()
                    });
                }
                ui.add_space(8.0);
                self.format_row(ui, &s);
                if s.format == RecordFormat::Wav {
                    let mut bwf = s.bwf;
                    if ui
                        .checkbox(&mut bwf, "Write when each file starts (Broadcast Wave)")
                        .changed()
                    {
                        self.set_recorder(RecorderPatch {
                            bwf: Some(Flag::Set(bwf)),
                            ..Default::default()
                        });
                    }
                    explain(
                        ui,
                        "Editors that read it can line the files up by the time they were \
                     recorded.",
                    );
                }
                ui.add_space(8.0);
                self.folder_row(ui, state);
                ui.add_space(8.0);
                self.names_rows(ui, state);
                ui.add_space(8.0);
                self.split_row(ui, &s);
            },
        );
    }

    fn format_row(&mut self, ui: &mut Ui, s: &RecorderSettings) {
        ui.horizontal(|ui| {
            ui.label("Format");
            let mut format = s.format;
            egui::ComboBox::from_id_salt("record-format")
                .selected_text(format.label())
                .width(110.0)
                .show_ui(ui, |ui| {
                    for f in RecordFormat::ALL {
                        ui.selectable_value(&mut format, f, f.label())
                            .on_hover_text(format_words(f));
                    }
                });
            if format != s.format {
                self.set_recorder(RecorderPatch {
                    format: Some(format),
                    ..Default::default()
                });
            }
            ui.add_space(10.0);
            ui.label("Quality");
            let quality = s.quality();
            let mut picked = quality;
            egui::ComboBox::from_id_salt("record-quality")
                .selected_text(s.format.quality_label(quality))
                .width(90.0)
                .show_ui(ui, |ui| {
                    for &q in s.format.qualities() {
                        ui.selectable_value(&mut picked, q, s.format.quality_label(q));
                    }
                });
            if picked != quality {
                let patch = if s.format.lossless() {
                    RecorderPatch {
                        bit_depth: Some(picked),
                        ..Default::default()
                    }
                } else {
                    RecorderPatch {
                        kbps: Some(picked),
                        ..Default::default()
                    }
                };
                self.set_recorder(patch);
            }
        });
        explain(ui, format_words(s.format));
    }

    fn folder_row(&mut self, ui: &mut Ui, state: &FullState) {
        let s = &state.recorder;
        ui.label(RichText::new("Save in").strong());
        // Empty for the default, which the hint names: the Music folder's
        // name depends on the language.
        let shown = s.folder.clone();
        ui.horizontal(|ui| {
            let mut text = self
                .recorder
                .folder
                .clone()
                .unwrap_or_else(|| shown.clone());
            let field = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .desired_width(ui.available_width() - 150.0)
                    .hint_text("Weir recordings, in your Music folder"),
            );
            if field.changed() {
                self.recorder.folder = Some(text.clone());
            }
            // One that will not do stays here, with why, until put right.
            if field.lost_focus()
                && folder_problem(&text).is_none()
                && self.recorder.folder.take().is_some()
                && text.trim() != shown
            {
                self.send_folder(text.trim());
            }
            if ui
                .add_enabled(self.recorder.dialog.is_none(), egui::Button::new("Change…"))
                .on_hover_text("Pick a folder")
                .clicked()
            {
                let start = PathBuf::from(&state.recording.folder);
                self.recorder.no_dialog = None;
                self.recorder.dialog = Some(Dialog::folder(
                    ui.ctx(),
                    "Where to save recordings",
                    start.ancestors().find(|p| p.is_dir()),
                ));
            }
            if !s.folder.is_empty()
                && ui
                    .small_button("Default")
                    .on_hover_text("Back to Weir recordings in your Music folder")
                    .clicked()
            {
                self.recorder.folder = None;
                self.send_folder("");
            }
        });
        if let Some(problem) = self.recorder.folder.as_deref().and_then(folder_problem) {
            ui.label(RichText::new(problem).size(11.0).color(theme::p().warning));
        }
        if let Some(why) = &self.recorder.no_dialog {
            explain(
                ui,
                &format!("There is no folder dialog here ({why}): type the folder instead."),
            );
        }
    }

    /// The folder typed or picked: `~/` for the home folder, empty for the
    /// default.
    fn send_folder(&mut self, folder: &str) {
        self.set_recorder(RecorderPatch {
            folder: Some(folder.to_string()),
            ..Default::default()
        });
    }

    fn poll_folder_dialog(&mut self) {
        let Some(picked) = self.recorder.dialog.as_ref().and_then(Dialog::poll) else {
            return;
        };
        self.recorder.dialog = None;
        match picked {
            Picked::File(path) => {
                let text = path.display().to_string();
                self.recorder.folder = None;
                self.send_folder(&home_short(&text));
            }
            Picked::Cancelled => {}
            Picked::NoDialog(why) => self.recorder.no_dialog = Some(why),
        }
    }

    fn names_rows(&mut self, ui: &mut Ui, state: &FullState) {
        let s = &state.recorder;
        let pattern = self
            .recorder
            .names
            .clone()
            .unwrap_or_else(|| s.names.clone());
        ui.horizontal(|ui| {
            ui.label(RichText::new("File names").strong());
            let preset = NAME_PRESETS
                .iter()
                .find(|(_, p)| *p == pattern)
                .map_or("Your own", |(name, _)| name);
            egui::ComboBox::from_id_salt("record-names")
                .selected_text(preset)
                .width(120.0)
                .show_ui(ui, |ui| {
                    for (name, p) in NAME_PRESETS {
                        if ui.selectable_label(pattern == p, name).clicked() {
                            self.recorder.names = None;
                            self.set_recorder(RecorderPatch {
                                names: Some(p.to_string()),
                                ..Default::default()
                            });
                        }
                    }
                });
        });
        let mut text = pattern.clone();
        let field = ui.add(
            egui::TextEdit::singleline(&mut text)
                .desired_width(ui.available_width())
                .font(egui::TextStyle::Monospace),
        );
        let problem = record_names_problem(&text);
        if field.changed() {
            self.recorder.names = Some(text.clone());
        }
        // Sent when done with, so a pattern half typed is never kept; one
        // that will not do stays here, with what is wrong with it.
        if field.lost_focus() && problem.is_none() && text.trim() != s.names {
            self.set_recorder(RecorderPatch {
                names: Some(text.trim().to_string()),
                ..Default::default()
            });
        }
        if !field.has_focus()
            && self
                .recorder
                .names
                .as_ref()
                .is_some_and(|t| t.trim() == s.names || t.trim().is_empty())
        {
            self.recorder.names = None;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for token in RECORD_NAME_TOKENS {
                if ui
                    .small_button(format!("+ {token}"))
                    .on_hover_text(token_words(token))
                    .clicked()
                {
                    let joined = if pattern.is_empty() || pattern.ends_with(['/', ' ']) {
                        format!("{pattern}{token}")
                    } else {
                        format!("{pattern} {token}")
                    };
                    self.recorder.names = None;
                    self.set_recorder(RecorderPatch {
                        names: Some(joined),
                        ..Default::default()
                    });
                }
            }
        });
        match problem {
            Some(problem) => {
                ui.label(RichText::new(problem).size(11.0).color(theme::p().warning));
            }
            None => {
                let files = self.next_files(state);
                let text = if files.is_empty() {
                    "Next recording: nothing is ticked yet".to_string()
                } else {
                    format!("Next recording: {}", files.join(", "))
                };
                ui.add(
                    egui::Label::new(RichText::new(text).size(11.0).color(theme::p().text_dim))
                        .wrap(),
                );
            }
        }
    }

    fn split_row(&mut self, ui: &mut Ui, s: &RecorderSettings) {
        ui.horizontal(|ui| {
            let mut on = s.split_minutes > 0;
            let minutes = self
                .recorder
                .split
                .unwrap_or(if on { s.split_minutes } else { 30 });
            if ui.checkbox(&mut on, "Start new files every").changed() {
                self.recorder.split = None;
                self.set_recorder(RecorderPatch {
                    split_minutes: Some(if on { minutes } else { 0 }),
                    ..Default::default()
                });
            }
            let mut value = minutes;
            let r = ui.add_enabled(
                on,
                egui::DragValue::new(&mut value)
                    .range(1..=RECORD_SPLIT_MAX_MINUTES)
                    .clamp_existing_to_range(false)
                    .speed(0.2),
            );
            ui.label("minutes");
            if r.changed() {
                self.recorder.split = Some(value);
                self.set_recorder(RecorderPatch {
                    split_minutes: Some(value),
                    ..Default::default()
                });
            }
            if !r.dragged() && !r.has_focus() {
                self.recorder.split = None;
            }
        });
        explain(
            ui,
            "Every track starts its next part at the same moment, with nothing lost between \
             parts. Use {part} in the file names to number them.",
        );
    }
}

/// Where the recording goes or went, as the daemon says.
fn recording_place(r: &RecordingStatus) -> String {
    if r.folder.is_empty() {
        "the recordings folder".into()
    } else {
        r.folder.clone()
    }
}

/// What is wrong with a folder as typed, if anything: the daemon has no
/// working folder of the window's to go by.
fn folder_problem(folder: &str) -> Option<&'static str> {
    let f = folder.trim();
    (!f.is_empty() && !f.starts_with('/') && !f.starts_with("~/") && f != "~").then_some(
        "Give the whole path, such as /home/you/Recordings, or start it with ~/ for your \
         home folder.",
    )
}

/// `widget` in a column `width` wide, from its left.
fn cell(ui: &mut Ui, width: f32, widget: impl egui::Widget) -> egui::Response {
    ui.allocate_ui_with_layout(
        vec2(width, 18.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.add(widget)
        },
    )
    .inner
}

/// About how many bytes an hour of the ticked tracks takes.
fn hourly_bytes(state: &FullState, ticked: &[&TrackRow]) -> Option<u64> {
    let s = &state.recorder;
    let rate = match state.engine.sample_rate {
        0 => 48_000,
        r => r,
    };
    let per_second: f64 = ticked
        .iter()
        .map(|t| s.format.bytes_per_second(s.quality(), rate, t.channels))
        .sum();
    (per_second > 0.0).then_some((per_second * 3600.0) as u64)
}

/// The last part of a path.
fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |f| f.to_string_lossy().into_owned())
}

/// The file the next recording puts `source` in: its own, or the one
/// file of them all.
fn track_file(state: &FullState, next: &[String], source: &TrackSource) -> Option<String> {
    let s = &state.recorder;
    match s.layout {
        RecordLayout::Multitrack => next.first().map(|f| file_name(f)),
        RecordLayout::Files => {
            let (strips, buses) = track_rows(&state.mixer);
            let k = strips
                .iter()
                .chain(&buses)
                .filter(|t| s.records(&t.source))
                .position(|t| t.source.same(source))?;
            next.get(k).map(|f| file_name(f))
        }
    }
}

/// What a format is good for, in a few words.
fn format_words(f: RecordFormat) -> &'static str {
    match f {
        RecordFormat::Wav => "Uncompressed: the biggest files, opened by everything.",
        RecordFormat::Flac => "Lossless, about half the size of WAV.",
        RecordFormat::Mp3 => "Small files that play anywhere. Stereo at most.",
        RecordFormat::Opus => "The smallest files for the quality.",
        RecordFormat::Vorbis => "Small files, an open format.",
    }
}

/// What a part of a file name pattern says.
fn token_words(token: &str) -> &'static str {
    match token {
        "{date}" => "The date the recording started, such as 2026-10-17",
        "{time}" => "The time it started, such as 20.52",
        "{number}" => "A number counting recordings, such as 007",
        "{track}" => "The strip's or bus's name",
        "{part}" => "Which part, when new files start every so many minutes",
        "{setup}" => "The setup loaded",
        _ => "",
    }
}

/// Show `folder` in the file manager.
fn open_folder(folder: &Path) {
    if let Err(e) = std::process::Command::new("xdg-open").arg(folder).spawn() {
        tracing::warn!("could not open {}: {e}", folder.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_for_the_recorder() {
        assert_eq!(tracks_words(1), "1 track");
        assert_eq!(tracks_words(3), "3 tracks");
        assert_eq!(file_name("/a/b/Mic.flac"), "Mic.flac");
        assert!(folder_problem("Recordings").is_some());
        assert!(folder_problem("~/Recordings").is_none());
        assert!(folder_problem("/srv/rec").is_none());
        assert!(folder_problem("").is_none());
        assert!(NAME_PRESETS
            .iter()
            .all(|(_, p)| record_names_problem(p).is_none()));
    }
}
