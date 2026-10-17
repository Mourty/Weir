//! Recording strips and buses to files.
//!
//! The strips and buses ticked for recording ([`RecorderSettings::tracks`])
//! each become a track: a strip as it is heard in the mix, after its
//! effects and fader, a bus as it plays. A recording is either a folder of
//! files, one per track, or one Matroska audio file (`.mka`) holding every
//! track. Every track starts and ends at the same moment, so they line up
//! in an editor.
//!
//! Where the files go and what they are called comes from a pattern such as
//! `{date} {time}/{track}`, worked out here so that every client shows the
//! same names the daemon writes.

use serde::{Deserialize, Serialize};

fn is_false(b: &bool) -> bool {
    !*b
}

/// The file formats a recording can be in.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RecordFormat {
    /// Uncompressed PCM, 16 or 24 bits.
    Wav,
    /// Lossless and about half the size of WAV, 16 or 24 bits.
    #[default]
    Flac,
    /// MP3, mixed down to stereo when a track has more channels.
    Mp3,
    /// Opus in Ogg: the smallest files for the quality.
    Opus,
    /// Ogg Vorbis.
    Vorbis,
}

impl RecordFormat {
    /// Every format, in the order to offer them.
    pub const ALL: [RecordFormat; 5] = [
        RecordFormat::Wav,
        RecordFormat::Flac,
        RecordFormat::Mp3,
        RecordFormat::Opus,
        RecordFormat::Vorbis,
    ];

    /// Its name for people.
    pub fn label(self) -> &'static str {
        match self {
            RecordFormat::Wav => "WAV",
            RecordFormat::Flac => "FLAC",
            RecordFormat::Mp3 => "MP3",
            RecordFormat::Opus => "Opus",
            RecordFormat::Vorbis => "Ogg Vorbis",
        }
    }

    /// The extension of a file of its own, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            RecordFormat::Wav => "wav",
            RecordFormat::Flac => "flac",
            RecordFormat::Mp3 => "mp3",
            RecordFormat::Opus => "opus",
            RecordFormat::Vorbis => "ogg",
        }
    }

    /// Whether it keeps every sample as it was.
    pub fn lossless(self) -> bool {
        matches!(self, RecordFormat::Wav | RecordFormat::Flac)
    }

    /// The qualities it offers: bits per sample for lossless formats,
    /// kilobits a second for a stereo track for the others.
    pub fn qualities(self) -> &'static [u32] {
        match self {
            RecordFormat::Wav | RecordFormat::Flac => &[16, 24],
            RecordFormat::Mp3 => &[128, 192, 256, 320],
            RecordFormat::Opus => &[64, 96, 128, 192, 256],
            RecordFormat::Vorbis => &[128, 192, 256, 320],
        }
    }

    /// A quality in words: "24-bit", "192 kbps".
    pub fn quality_label(self, quality: u32) -> String {
        if self.lossless() {
            format!("{quality}-bit")
        } else {
            format!("{quality} kbps")
        }
    }

    /// About how many bytes a second a track of `channels` channels at
    /// `rate` takes, for guessing how much room a recording needs.
    pub fn bytes_per_second(self, quality: u32, rate: u32, channels: usize) -> f64 {
        let pcm = rate as f64 * channels as f64 * (quality as f64 / 8.0);
        let lossy = |stereo_kbps: f64, scale: f64| stereo_kbps * 1000.0 / 8.0 * scale;
        match self {
            RecordFormat::Wav => pcm,
            // Music and speech mostly come out at a half to two thirds.
            RecordFormat::Flac => pcm * 0.6,
            // Never more than stereo.
            RecordFormat::Mp3 => lossy(quality as f64, if channels == 1 { 0.5 } else { 1.0 }),
            RecordFormat::Opus | RecordFormat::Vorbis => {
                lossy(quality as f64, (channels as f64 / 2.0).max(0.5))
            }
        }
    }
}

/// How the tracks of a recording are kept.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RecordLayout {
    /// A file for each track.
    #[default]
    Files,
    /// One Matroska audio file (`.mka`) holding every track.
    Multitrack,
}

/// A strip or bus to record, by name.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TrackSource {
    /// A strip: what it sounds like in the mix, after its effects and
    /// fader.
    Strip(String),
    /// A bus: what it plays.
    Bus(String),
}

impl TrackSource {
    /// The strip's or bus's name.
    pub fn name(&self) -> &str {
        match self {
            TrackSource::Strip(n) | TrackSource::Bus(n) => n,
        }
    }

    /// Whether it names the same strip or bus as `other`, ignoring case.
    pub fn same(&self, other: &TrackSource) -> bool {
        match (self, other) {
            (TrackSource::Strip(a), TrackSource::Strip(b))
            | (TrackSource::Bus(a), TrackSource::Bus(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }
}

/// The default file names: a folder for each recording, named after when it
/// started, with a file for each track.
pub const RECORD_NAMES_DEFAULT: &str = "{date} {time}/{track}";
/// What can go in file names.
pub const RECORD_NAME_TOKENS: [&str; 6] = [
    "{date}", "{time}", "{number}", "{track}", "{part}", "{setup}",
];
/// The longest a recording may go without starting new files, in minutes.
pub const RECORD_SPLIT_MAX_MINUTES: u32 = 24 * 60;

/// How recordings are made. Changes apply to the next recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct RecorderSettings {
    /// The strips and buses to record, in the order of their tracks.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<TrackSource>,
    /// Where recordings go, a whole path, or `~/` and a path in your home
    /// folder. Empty: `Weir recordings` in your Music folder.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Where in the folder each file goes and what it is called, without
    /// its extension: see [`RECORD_NAME_TOKENS`]. A `/` makes a folder.
    pub names: String,
    /// The file format.
    pub format: RecordFormat,
    /// Bits per sample for WAV and FLAC: 16 or 24.
    pub bit_depth: u32,
    /// Kilobits a second for a stereo track, for MP3, Opus and Ogg Vorbis:
    /// the nearest the format offers is used.
    pub kbps: u32,
    /// A file per track, or one holding them all.
    pub layout: RecordLayout,
    /// Start new files every this many minutes, without a gap between
    /// them; 0 never does.
    pub split_minutes: u32,
    /// WAV only: write when each file starts into it (a Broadcast Wave
    /// `bext` chunk), so editors can line files up by time.
    #[serde(skip_serializing_if = "is_false")]
    pub bwf: bool,
    /// The number `{number}` gives the next recording.
    pub next_number: u32,
}

impl Default for RecorderSettings {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            folder: String::new(),
            names: RECORD_NAMES_DEFAULT.into(),
            format: RecordFormat::default(),
            bit_depth: 24,
            kbps: 192,
            layout: RecordLayout::default(),
            split_minutes: 0,
            bwf: false,
            next_number: 1,
        }
    }
}

impl RecorderSettings {
    /// The quality to record at: bits per sample, or the nearest kilobits
    /// a second the format offers.
    pub fn quality(&self) -> u32 {
        let wanted = if self.format.lossless() {
            self.bit_depth
        } else {
            self.kbps
        };
        nearest(self.format.qualities(), wanted)
    }

    /// Whether `source` is ticked to record.
    pub fn records(&self, source: &TrackSource) -> bool {
        self.tracks.iter().any(|t| t.same(source))
    }

    /// Put everything in range: known qualities, a sane split, no track
    /// twice, a usable name pattern.
    pub fn normalize(&mut self) {
        self.bit_depth = nearest(RecordFormat::Wav.qualities(), self.bit_depth);
        self.kbps = self.kbps.clamp(32, 512);
        self.split_minutes = self.split_minutes.min(RECORD_SPLIT_MAX_MINUTES);
        self.next_number = self.next_number.max(1);
        let mut seen: Vec<TrackSource> = Vec::new();
        self.tracks.retain(|t| {
            let new = !t.name().trim().is_empty() && !seen.iter().any(|s| s.same(t));
            seen.push(t.clone());
            new
        });
        self.folder = self.folder.trim().to_string();
        if self.names.trim().is_empty() {
            self.names = RECORD_NAMES_DEFAULT.into();
        }
    }
}

/// The value in `options` nearest `wanted`.
fn nearest(options: &[u32], wanted: u32) -> u32 {
    options
        .iter()
        .copied()
        .min_by_key(|o| o.abs_diff(wanted))
        .unwrap_or(wanted)
}

/// A change to the recorder's settings: only what is given changes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RecorderPatch {
    /// Replaces the strips and buses to record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracks: Option<Vec<TrackSource>>,
    /// Where recordings go; empty for `Weir recordings` in your Music
    /// folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The file name pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<String>,
    /// The file format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RecordFormat>,
    /// Bits per sample for WAV and FLAC: 16 or 24.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    /// Kilobits a second for a stereo track, for lossy formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kbps: Option<u32>,
    /// A file per track, or one holding them all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<RecordLayout>,
    /// Start new files every this many minutes; 0 never does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_minutes: Option<u32>,
    /// WAV only: write when each file starts into it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bwf: Option<crate::rpc::Flag>,
    /// The number `{number}` gives the next recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_number: Option<u32>,
}

/// One track of the recording under way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RecordingTrack {
    /// The strip or bus it records.
    pub source: TrackSource,
    /// Its channels.
    pub channels: usize,
    /// The file it goes into now, a whole path. In a `.mka`, every track's
    /// is the same file.
    pub file: String,
}

/// What the recorder is doing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RecordingStatus {
    /// Whether it is recording.
    #[serde(default)]
    pub recording: bool,
    /// When the recording under way, or the last one, started, in
    /// milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// How long it has recorded, in seconds.
    #[serde(default)]
    pub seconds: f64,
    /// Where it records to, a whole path: while recording, the folder its
    /// files are in; otherwise where the next recording goes.
    #[serde(default)]
    pub folder: String,
    /// Its tracks, while recording, or the last recording's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<RecordingTrack>,
    /// Which part of the recording the files are, counting from 1, when it
    /// starts new files now and then.
    #[serde(default)]
    pub part: u32,
    /// How long until the next part starts, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_part_in: Option<f64>,
    /// Bytes written so far, every part together.
    #[serde(default)]
    pub bytes: u64,
    /// Bytes free on the disk it records to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_bytes: Option<u64>,
    /// Sound lost because the computer could not keep up, in seconds. It
    /// is silence in the files, so the tracks still line up.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub lost_seconds: f64,
    /// Ticked strips and buses the mixer did not have when it started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<TrackSource>,
    /// Why the last recording stopped by itself or could not start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// Parameters of `set_recording`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetRecordingParams {
    /// Start (`true`), stop (`false`), or `"toggle"`.
    pub on: crate::rpc::Flag,
}

/// Parameters of `set_record_track`: one strip or bus, given as `strip` or
/// `bus`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetRecordTrackParams {
    /// The strip, by id or name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip: Option<crate::model::StripId>,
    /// The bus, by id, label or name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus: Option<crate::model::BusId>,
    /// Record it (`true`), not (`false`), or `"toggle"`.
    pub on: crate::rpc::Flag,
}

/// A moment in local time, for file names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocalTime {
    /// The year, such as 2026.
    pub year: i32,
    /// 1 to 12.
    pub month: u32,
    /// 1 to 31.
    pub day: u32,
    /// 0 to 23.
    pub hour: u32,
    /// 0 to 59.
    pub minute: u32,
    /// 0 to 60.
    pub second: u32,
}

impl LocalTime {
    /// `2026-10-17`.
    pub fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `20.52`: colons are not allowed in file names everywhere.
    pub fn time(&self) -> String {
        format!("{:02}.{:02}", self.hour, self.minute)
    }
}

/// What a recording's file names are made from.
#[derive(Debug, Clone, Copy)]
pub struct NameParts<'a> {
    /// The pattern, as in [`RecorderSettings::names`].
    pub pattern: &'a str,
    /// A file per track, or one holding them all.
    pub layout: RecordLayout,
    /// The format, for the extension.
    pub format: RecordFormat,
    /// When the recording started.
    pub when: LocalTime,
    /// `{number}`.
    pub number: u32,
    /// The part, counting from 1.
    pub part: u32,
    /// Whether it starts new files now and then, so parts need telling
    /// apart.
    pub splits: bool,
    /// The setup loaded, for `{setup}`.
    pub setup: Option<&'a str>,
}

/// The file of each track, or the one file of them all, relative to the
/// recordings folder, with `/` between folders: a list as long as `tracks`
/// for [`RecordLayout::Files`], of one for [`RecordLayout::Multitrack`].
/// Names are made safe for any file system: no `/` inside a name, no
/// empty or `..` folders.
pub fn recording_files(parts: &NameParts, tracks: &[&str]) -> Vec<String> {
    let pattern = if parts.pattern.trim().is_empty() {
        RECORD_NAMES_DEFAULT
    } else {
        parts.pattern
    };
    let ext = match parts.layout {
        RecordLayout::Files => parts.format.extension(),
        RecordLayout::Multitrack => "mka",
    };
    let one = |track: &str| {
        let has_track = pattern.contains("{track}");
        let mut pattern = pattern.to_string();
        // Every track needs a file of its own.
        if parts.layout == RecordLayout::Files && !has_track {
            pattern.push_str(" {track}");
        }
        let part = parts.part.max(1);
        let rendered = pattern
            .replace("{date}", &parts.when.date())
            .replace("{time}", &parts.when.time())
            .replace("{number}", &format!("{:03}", parts.number))
            .replace("{part}", &part.to_string())
            .replace("{setup}", &one_name(parts.setup.unwrap_or("Weir")))
            .replace("{track}", &one_name(track));
        let mut folders: Vec<String> = rendered
            .split('/')
            .map(clean_name)
            .filter(|c| !c.is_empty())
            .collect();
        if folders.is_empty() {
            folders.push("Recording".into());
        }
        if parts.splits && part > 1 && !pattern.contains("{part}") {
            let last = folders.last_mut().expect("one at least");
            last.push_str(&format!(" part {part}"));
        }
        format!("{}.{ext}", folders.join("/"))
    };
    match parts.layout {
        RecordLayout::Files => tracks.iter().map(|t| one(t)).collect(),
        RecordLayout::Multitrack => vec![one("")],
    }
}

/// A value for a file name: no `/`.
fn one_name(value: &str) -> String {
    value.replace('/', "-")
}

/// One folder's or file's name made safe: no control characters, no
/// spaces or dots at either end (so never `..`), and not too long.
fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim_matches(|c: char| c == '.' || c.is_whitespace());
    name.chars()
        .take(120)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// What is wrong with a file name pattern, if anything.
pub fn record_names_problem(pattern: &str) -> Option<String> {
    if pattern.chars().count() > 200 {
        return Some("That is longer than 200 characters.".into());
    }
    if pattern.starts_with('/') {
        return Some(
            "File names go inside the recordings folder; choose another folder for that.".into(),
        );
    }
    if let Some(bad) = pattern.split('/').find(|c| c.trim() == "..") {
        return Some(format!("'{bad}' would leave the recordings folder."));
    }
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else {
            break;
        };
        let token = &rest[start..start + len + 1];
        if !RECORD_NAME_TOKENS.contains(&token) {
            return Some(format!(
                "{token} is not something names can hold; they can hold {}.",
                RECORD_NAME_TOKENS.join(", ")
            ));
        }
        rest = &rest[start + len + 1..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(pattern: &str, layout: RecordLayout) -> NameParts<'_> {
        NameParts {
            pattern,
            layout,
            format: RecordFormat::Flac,
            when: LocalTime {
                year: 2026,
                month: 10,
                day: 17,
                hour: 20,
                minute: 52,
                second: 9,
            },
            number: 7,
            part: 1,
            splits: false,
            setup: Some("Streaming"),
        }
    }

    #[test]
    fn file_names_come_from_the_pattern_and_stay_inside_the_folder() {
        let tracks = ["Mic", "Music/Games"];
        let p = parts(RECORD_NAMES_DEFAULT, RecordLayout::Files);
        assert_eq!(
            recording_files(&p, &tracks),
            [
                "2026-10-17 20.52/Mic.flac",
                "2026-10-17 20.52/Music-Games.flac"
            ]
        );
        // One file for every track: the track's part of the name goes.
        let p = parts(RECORD_NAMES_DEFAULT, RecordLayout::Multitrack);
        assert_eq!(recording_files(&p, &tracks), ["2026-10-17 20.52.mka"]);
        // Files need the track's name to differ, so it comes on the end.
        let p = parts("{setup} {number}", RecordLayout::Files);
        assert_eq!(
            recording_files(&p, &tracks[..1]),
            ["Streaming 007 Mic.flac"]
        );
        // Parts are numbered where {part} says, or on the end.
        let mut p = parts("{date}/{track}", RecordLayout::Files);
        p.splits = true;
        p.part = 2;
        assert_eq!(
            recording_files(&p, &tracks[..1]),
            ["2026-10-17/Mic part 2.flac"]
        );
        let mut p = parts("{track} {part}", RecordLayout::Files);
        p.splits = true;
        p.part = 3;
        assert_eq!(recording_files(&p, &tracks[..1]), ["Mic 3.flac"]);
        // Nothing can climb out of the folder or be left nameless.
        let p = parts("../ ./ /x/{track}", RecordLayout::Files);
        assert_eq!(recording_files(&p, &tracks[..1]), ["x/Mic.flac"]);
        let p = parts("{track}", RecordLayout::Multitrack);
        assert_eq!(recording_files(&p, &[]), ["Recording.mka"]);
    }

    #[test]
    fn patterns_are_checked_and_settings_kept_in_range() {
        assert_eq!(record_names_problem(RECORD_NAMES_DEFAULT), None);
        assert!(record_names_problem("{date}/../x").is_some());
        assert!(record_names_problem("/home/x").is_some());
        assert!(record_names_problem("{song}").unwrap().contains("{song}"));
        let mut s = RecorderSettings {
            bit_depth: 32,
            kbps: 200,
            format: RecordFormat::Mp3,
            tracks: vec![
                TrackSource::Strip("Mic".into()),
                TrackSource::Strip("mic".into()),
                TrackSource::Bus("Mic".into()),
                TrackSource::Strip(" ".into()),
            ],
            names: " ".into(),
            ..Default::default()
        };
        s.normalize();
        assert_eq!(s.bit_depth, 24);
        assert_eq!(s.quality(), 192);
        assert_eq!(s.tracks.len(), 2);
        assert_eq!(s.names, RECORD_NAMES_DEFAULT);
        assert!(s.records(&TrackSource::Strip("MIC".into())));
        assert!(!s.records(&TrackSource::Bus("Music".into())));
    }
}
