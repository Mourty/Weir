//! The mixer data model.
//!
//! A mixer is a list of input [`Strip`]s and a list of output [`Bus`]es.
//! Each strip plays in any number of buses, at a level of its own in each
//! ([`Strip::sends`]). Each bus mixes what reaches it and plays the result on
//! a device, or offers it to other programs as a virtual microphone.
//! [`MixerState`] holds both lists: it is what the daemon saves, undoes, and
//! sends to every client.
//!
//! The rest of this module describes the world around the mix: the devices
//! and application streams PipeWire reports, the engine's status, meters,
//! and the daemon's own [`Settings`].

use crate::fx::{Compressor, Denoise, Ducking, EqPreset, Equalizer, Gate, Insert, Limiter};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Identifies a strip. Unique among strips; a new strip gets one more than
/// the highest in use.
pub type StripId = u32;
/// Identifies a bus. Unique among buses; a new bus gets one more than the
/// highest in use.
pub type BusId = u32;

/// Serde helper for a table keyed by strip or bus id, reading the keys
/// whether they arrive as numbers or as text. TOML keys are always text, and
/// a TOML file read through `toml::Table` hands them over as text that
/// serde's own map reading will not turn back into a number.
pub mod id_keys {
    use serde::de::{self, Deserialize, Deserializer, MapAccess, Visitor};
    use std::collections::BTreeMap;
    use std::fmt;
    use std::marker::PhantomData;

    struct Id(u32);

    impl<'de> Deserialize<'de> for Id {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct IdVisitor;
            impl Visitor<'_> for IdVisitor {
                type Value = Id;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("a strip or bus number")
                }
                fn visit_u64<E: de::Error>(self, v: u64) -> Result<Id, E> {
                    u32::try_from(v)
                        .map(Id)
                        .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(v), &self))
                }
                fn visit_i64<E: de::Error>(self, v: i64) -> Result<Id, E> {
                    u32::try_from(v)
                        .map(Id)
                        .map_err(|_| E::invalid_value(de::Unexpected::Signed(v), &self))
                }
                fn visit_str<E: de::Error>(self, v: &str) -> Result<Id, E> {
                    v.trim()
                        .parse()
                        .map(Id)
                        .map_err(|_| E::invalid_value(de::Unexpected::Str(v), &self))
                }
            }
            d.deserialize_any(IdVisitor)
        }
    }

    /// Read a table keyed by strip or bus id; see the module documentation.
    pub fn deserialize<'de, D, V>(d: D) -> Result<BTreeMap<u32, V>, D::Error>
    where
        D: Deserializer<'de>,
        V: Deserialize<'de>,
    {
        struct MapVisitor<V>(PhantomData<V>);
        impl<'de, V: Deserialize<'de>> Visitor<'de> for MapVisitor<V> {
            type Value = BTreeMap<u32, V>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a table keyed by strip or bus number")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = BTreeMap::new();
                while let Some((Id(id), v)) = map.next_entry::<Id, V>()? {
                    out.insert(id, v);
                }
                Ok(out)
            }
        }
        d.deserialize_map(MapVisitor(PhantomData))
    }
}

/// Lowest fader position. At or below this value the channel is silent.
pub const GAIN_MIN_DB: f32 = -60.0;
/// Highest fader position.
pub const GAIN_MAX_DB: f32 = 12.0;

/// Longest delay a bus can add to its output, in milliseconds.
pub const BUS_DELAY_MAX_MS: f32 = 500.0;

/// Value reported by meters for silence (JSON has no -inf).
pub const METER_FLOOR_DB: f32 = -100.0;

/// `v` when it is a number, `fallback` when it is NaN or infinite. Settings
/// arrive from files and from clients, so every `normalize` starts here.
pub(crate) fn finite_or(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

/// A speaker position, named exactly as PipeWire names them in `audio.position`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChannelPosition {
    /// The one channel of a mono stream.
    Mono,
    /// Front left.
    FL,
    /// Front right.
    FR,
    /// Front center.
    FC,
    /// Low-frequency effects: the subwoofer.
    LFE,
    /// Rear left.
    RL,
    /// Rear right.
    RR,
    /// Side left.
    SL,
    /// Side right.
    SR,
}

/// Which side of the room a position is on, for panning and for folding
/// channels together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Front, rear or side left.
    Left,
    /// Front, rear or side right.
    Right,
    /// Mono or front center: both sides at once.
    Center,
    /// The subwoofer, which belongs to no side.
    Lfe,
}

impl ChannelPosition {
    /// Every position.
    pub const ALL: [ChannelPosition; 9] = [
        Self::Mono,
        Self::FL,
        Self::FR,
        Self::FC,
        Self::LFE,
        Self::RL,
        Self::RR,
        Self::SL,
        Self::SR,
    ];

    /// The name PipeWire uses for it, such as `FL`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mono => "MONO",
            Self::FL => "FL",
            Self::FR => "FR",
            Self::FC => "FC",
            Self::LFE => "LFE",
            Self::RL => "RL",
            Self::RR => "RR",
            Self::SL => "SL",
            Self::SR => "SR",
        }
    }

    /// Parse a PipeWire channel name such as `FL` or `MONO`. Also accepts
    /// `AUX0`/`AUX1` (used by some devices for an unlabeled stereo pair).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL
            .iter()
            .copied()
            .find(|p| p.as_str().eq_ignore_ascii_case(s))
            .or(match s.to_ascii_uppercase().as_str() {
                "AUX0" => Some(Self::FL),
                "AUX1" => Some(Self::FR),
                _ => None,
            })
    }

    /// Which side of the room it is on.
    pub fn side(self) -> Side {
        match self {
            Self::FL | Self::RL | Self::SL => Side::Left,
            Self::FR | Self::RR | Self::SR => Side::Right,
            Self::Mono | Self::FC => Side::Center,
            Self::LFE => Side::Lfe,
        }
    }
}

impl std::fmt::Display for ChannelPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Channel layout of a strip or bus. Every strip and bus carries its own.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ChannelLayout {
    /// One channel.
    Mono,
    /// Front left and right.
    #[default]
    Stereo,
    /// Front left and right, rear left and right.
    Quad,
    /// 5.1: front left, right and center, subwoofer, rear left and right.
    /// `"5.1"` is accepted too.
    #[serde(rename = "surround_5_1", alias = "5.1")]
    Surround51,
    /// 7.1: 5.1 with a side left and right as well. `"7.1"` is accepted too.
    #[serde(rename = "surround_7_1", alias = "7.1")]
    Surround71,

    /// Any other list of positions, in port order, as
    /// `{"custom": ["FL", "FR", "LFE"]}`.
    Custom(Vec<ChannelPosition>),
}

impl ChannelLayout {
    /// The named layouts offered in pickers.
    pub const PRESETS: [ChannelLayout; 5] = [
        Self::Mono,
        Self::Stereo,
        Self::Quad,
        Self::Surround51,
        Self::Surround71,
    ];

    /// Its channels' positions, in port order.
    pub fn positions(&self) -> Vec<ChannelPosition> {
        use ChannelPosition::*;
        match self {
            Self::Mono => vec![Mono],
            Self::Stereo => vec![FL, FR],
            Self::Quad => vec![FL, FR, RL, RR],
            Self::Surround51 => vec![FL, FR, FC, LFE, RL, RR],
            Self::Surround71 => vec![FL, FR, FC, LFE, RL, RR, SL, SR],
            Self::Custom(v) => v.clone(),
        }
    }

    /// How many channels it has.
    pub fn channel_count(&self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Quad => 4,
            Self::Surround51 => 6,
            Self::Surround71 => 8,
            Self::Custom(v) => v.len(),
        }
    }

    /// A short name for menus: `Stereo`, `5.1`, or the positions of a
    /// custom layout.
    pub fn label(&self) -> String {
        match self {
            Self::Mono => "Mono".into(),
            Self::Stereo => "Stereo".into(),
            Self::Quad => "Quad".into(),
            Self::Surround51 => "5.1".into(),
            Self::Surround71 => "7.1".into(),
            Self::Custom(v) => v.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(" "),
        }
    }

    /// Parse a user-facing name (`mono`, `stereo`, `quad`, `5.1`, `7.1`,
    /// `surround_5_1`) or a space/comma separated list of positions.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mono" | "1" => return Some(Self::Mono),
            "stereo" | "2" => return Some(Self::Stereo),
            "quad" | "4" => return Some(Self::Quad),
            "5.1" | "surround_5_1" | "surround51" | "6" => return Some(Self::Surround51),
            "7.1" | "surround_7_1" | "surround71" | "8" => return Some(Self::Surround71),
            _ => {}
        }
        let positions: Option<Vec<_>> = s
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .map(ChannelPosition::parse)
            .collect();
        match positions {
            Some(v) if !v.is_empty() => Some(Self::from_positions(v)),
            _ => None,
        }
    }

    /// Build a layout from a list of positions, collapsing to a named preset
    /// when the list matches one.
    pub fn from_positions(positions: Vec<ChannelPosition>) -> Self {
        for preset in Self::PRESETS.iter() {
            if preset.positions() == positions {
                return preset.clone();
            }
        }
        Self::Custom(positions)
    }
}

/// A strip or a bus, as `{"strip": 3}` or `{"bus": 1}`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StripOrBus {
    /// A strip, by id.
    Strip(StripId),
    /// A bus, by id.
    Bus(BusId),
}

/// Where a strip takes its sound from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StripKind {
    /// Captures from a physical (or any other PipeWire) source node.
    Hardware,
    /// A virtual playback device that applications choose as their output.
    Virtual,
}

/// Where a bus sends its mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BusKind {
    /// Plays to a physical (or any other PipeWire) sink node.
    Hardware,
    /// A virtual microphone that applications can capture from.
    Virtual,
}

/// Upmixing: how a strip plays on a bus with speakers it has no channel of
/// its own for, such as a stereo strip on a 7.1 bus. Called `spread` before,
/// with values `front`, `front_center` and `all`, which are still accepted.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Upmix {
    /// Only on the speakers it has channels for: a stereo strip plays on
    /// the front left and right, and a mono one on both of those.
    #[default]
    #[serde(alias = "front")]
    Off,
    /// Also on the center speaker, with the average of its front left and
    /// right.
    #[serde(alias = "front_center")]
    Center,
    /// On every speaker: its left channel on each left speaker, its right
    /// channel on each right one, and the center as for `center`. What AV
    /// receivers call all-channel stereo.
    #[serde(alias = "all")]
    AllChannelStereo,
    /// Passive surround decoding, the classic matrix decode: the center as
    /// for `center`, and on the surround speakers what differs between left
    /// and right (room sound, crowds, effects), delayed by
    /// [`PASSIVE_SURROUND_DELAY_MS`] and low-passed at
    /// [`PASSIVE_SURROUND_CUTOFF_HZ`] so it stays behind the fronts. The
    /// left surrounds get `0.707 (L - R)` and the right ones `0.707 (R - L)`.
    /// A strip with surround channels of its own plays them on the surround
    /// speakers it lacks, as for `all_channel_stereo`.
    PassiveSurround,
}

/// How much later than the fronts a passive surround decode reaches the
/// surround speakers, so sound still seems to come from the front.
pub const PASSIVE_SURROUND_DELAY_MS: f32 = 12.0;
/// Where a passive surround decode's surround feed stops, as in Dolby
/// Surround decoders: above it, the difference signal is mostly the front
/// sound leaking through.
pub const PASSIVE_SURROUND_CUTOFF_HZ: f32 = 7000.0;

impl Upmix {
    /// Every choice, in the order menus offer them.
    pub const ALL: [Upmix; 4] = [
        Upmix::Off,
        Upmix::Center,
        Upmix::AllChannelStereo,
        Upmix::PassiveSurround,
    ];

    /// Whether this is the default, [`Upmix::Off`].
    pub fn is_default(&self) -> bool {
        *self == Upmix::Off
    }

    /// A short name for menus.
    pub fn label(&self) -> &'static str {
        match self {
            Upmix::Off => "Off",
            Upmix::Center => "Center fill",
            Upmix::AllChannelStereo => "All-channel stereo",
            Upmix::PassiveSurround => "Passive surround",
        }
    }

    /// What it does to the sound, for people who have not met the term.
    pub fn description(&self) -> &'static str {
        match self {
            Upmix::Off => "Plays only on the speakers it has channels for: stereo stays on the front left and right.",
            Upmix::Center => "Also on the center speaker, which gets what left and right have in common.",
            Upmix::AllChannelStereo => "Left on every left speaker, right on every right one, and the center too. Loud and even, but everything comes from everywhere.",
            Upmix::PassiveSurround => "The classic matrix decode: what differs between left and right (room sound, crowds, effects) goes to the surround speakers, slightly delayed; the center gets what they share. Brings out the surround in films and TV that carry it in stereo.",
        }
    }
}

/// Downmixing: how a bus plays a strip's channels it has no speaker for,
/// such as a 5.1 strip on a stereo bus.
///
/// The standard method is ITU-R BS.775's, with the center and surround
/// levels ATSC A/52 lets a downmix choose from. For a strip with both side
/// and rear surrounds (7.1) on a bus with only one pair, each goes into
/// that pair at -3 dB.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Downmix {
    /// Which way to fold channels together.
    pub method: DownmixMethod,
    /// Level of the center channel in each front speaker, in dB, from
    /// `-60` (left out) to `0`. The standard is -3; [`CENTER_MIX_LEVELS`]
    /// are the usual choices, and louder makes dialog clearer.
    pub center_db: f32,
    /// Level of each surround channel in the front speaker on its side, in
    /// dB, from `-60` (left out) to `0`. The standard is -3;
    /// [`SURROUND_MIX_LEVELS`] are the usual choices. The matrix method uses
    /// levels of its own instead.
    pub surround_db: f32,
    /// Keep the subwoofer (LFE) channel, in each front speaker at -3 dB (or
    /// in the center speaker, if the bus has one and no front pair).
    /// Standard downmixes leave it out.
    pub lfe: bool,
}

/// The usual center levels for a downmix, in dB.
pub const CENTER_MIX_LEVELS: [f32; 4] = [0.0, -3.0, -4.5, -6.0];
/// The usual surround levels for a downmix, in dB. The last leaves them out.
pub const SURROUND_MIX_LEVELS: [f32; 4] = [0.0, -3.0, -6.0, GAIN_MIN_DB];

impl Default for Downmix {
    fn default() -> Self {
        Self {
            method: DownmixMethod::Standard,
            center_db: -3.0,
            surround_db: -3.0,
            lfe: false,
        }
    }
}

impl Downmix {
    /// Whether every setting is at its default.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Bring levels into range. Returns whether anything had to change.
    pub fn normalize(&mut self) -> bool {
        let before = *self;
        let d = Self::default();
        self.center_db = finite_or(self.center_db, d.center_db).clamp(GAIN_MIN_DB, 0.0);
        self.surround_db = finite_or(self.surround_db, d.surround_db).clamp(GAIN_MIN_DB, 0.0);
        *self != before
    }
}

/// The ways a bus can downmix. The last four play one part of a strip on
/// the bus's front left and right and nothing else, for building a surround
/// system out of stereo devices, one bus per device.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DownmixMethod {
    /// Fold each missing channel into the speakers on its side (ITU-R
    /// BS.775, "Lo/Ro"): center into both fronts, left surrounds into the
    /// front left, right surrounds into the front right.
    #[default]
    Standard,
    /// Matrix surround ("Lt/Rt"): the center as for `standard`, and the
    /// surrounds folded in with opposite polarity on the two sides, using
    /// Dolby Pro Logic II's levels (0.872 on their own side, 0.490 on the
    /// other), so a receiver that decodes surround from stereo can pull
    /// them back out. Mixes without a phase shifter, as most software
    /// downmixers do.
    Matrix,
    /// Only the front left and right (or a mono strip), with nothing folded
    /// in.
    FrontOnly,
    /// Only the center channel, on both front speakers.
    CenterOnly,
    /// Only the subwoofer (LFE) channel, on both front speakers.
    LfeOnly,
    /// Only the surround channels: the left ones on the front left, the
    /// right ones on the front right.
    SurroundOnly,
}

impl DownmixMethod {
    /// Every method, in the order menus offer them.
    pub const ALL: [DownmixMethod; 6] = [
        Self::Standard,
        Self::Matrix,
        Self::FrontOnly,
        Self::CenterOnly,
        Self::LfeOnly,
        Self::SurroundOnly,
    ];

    /// True for the methods that play one part of a strip only.
    pub fn is_part(self) -> bool {
        !matches!(self, Self::Standard | Self::Matrix)
    }

    /// A short name for menus.
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::Matrix => "Matrix surround",
            Self::FrontOnly => "Front only",
            Self::CenterOnly => "Center only",
            Self::LfeOnly => "Subwoofer only",
            Self::SurroundOnly => "Surrounds only",
        }
    }

    /// What it does to the sound, in a sentence for people.
    pub fn description(self) -> &'static str {
        match self {
            Self::Standard => "Folds
 the center and surrounds into left and right, the way TVs and players do.",
            Self::Matrix => "Folds the surrounds in so that a receiver or soundbar with Pro Logic II (or similar) can pull them back out. Use it when this bus feeds one.",
            Self::FrontOnly => "Plays only the front left and right, nothing folded in.",
            Self::CenterOnly => "Plays only the center channel, on both speakers.",
            Self::LfeOnly => "Plays only the subwoofer channel, on both speakers.",
            Self::SurroundOnly => "Plays only the surround channels, left on left and right on right.",
        }
    }
}

/// A level in dB as a downmix menu shows it.
pub fn mix_level_label(db: f32) -> String {
    if db <= GAIN_MIN_DB {
        "Off".into()
    } else if db == 0.0 {
        "0 dB".into()
    } else {
        format!("{} dB", format!("{db}").replace('-', "−"))
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// For `skip_serializing_if`: a delay of nothing is left out of files.
fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// What the system calls the virtual device of a strip or bus called
/// `name`: "Music (Weir)".
pub fn device_description(name: &str) -> String {
    format!("{} (Weir)", without_colons(name))
}

/// `name` fit for a device's name. A colon is left out: patchbays that work
/// like JACK's, such as Carla's, take everything before one for the name of
/// the program a port belongs to, and would file the device under it.
pub(crate) fn without_colons(name: &str) -> String {
    name.replace(": ", " - ").replace(':', "-")
}

/// An input strip: one source of sound, with its fader, effects and
/// routing.
///
/// A virtual strip is a playback device that applications choose as their
/// output; a hardware strip captures from a microphone or any other source.
/// Its sound passes through noise suppression, the gate, the equalizer and
/// the compressor, then its fader, and then plays in every bus it is routed
/// to, at that bus's send level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Strip {
    /// Unique among strips.
    pub id: StripId,
    /// Unique among strips, ignoring case, and 40 characters at most. A
    /// virtual strip's device shows in the system as "*name* (Weir)", with
    /// any colon left out (see [`device_description`]).
    pub name: String,
    /// Where it takes its sound from.
    pub kind: StripKind,
    /// Its channels. Changing it recreates a virtual strip's device.
    #[serde(default)]
    pub layout: ChannelLayout,
    /// Fader position in dB, from [`GAIN_MIN_DB`] (silent) to
    /// [`GAIN_MAX_DB`].
    #[serde(default)]
    pub gain_db: f32,
    /// Silences the strip in every mix.
    #[serde(default)]
    pub mute: bool,
    /// While any strip is soloed, the others go quiet, in every mix or in
    /// one, as [`Settings::solo`] says.
    #[serde(default)]
    pub solo: bool,
    /// Pan / balance, `-1.0` (full left) to `1.0` (full right).
    #[serde(default)]
    pub pan: f32,
    /// The buses it plays in.
    #[serde(default)]
    pub routes: BTreeSet<BusId>,
    /// How loud this strip is in each bus's mix, in dB, on top of its
    /// fader. A bus that is not listed is at 0 dB. A level is kept while its
    /// route is off, so switching the route back on brings it back.
    #[serde(
        default,
        deserialize_with = "id_keys::deserialize",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub sends: BTreeMap<BusId, f32>,
    /// How the strip plays on buses with speakers it has no channel for.
    #[serde(default, alias = "spread", skip_serializing_if = "Upmix::is_default")]
    pub upmix: Upmix,
    /// Also play the strip's bass on the subwoofer of buses that have one,
    /// when the strip has no subwoofer channel of its own.
    #[serde(default, skip_serializing_if = "is_false")]
    pub subwoofer: bool,
    /// PipeWire `node.name` of the source to capture from (hardware strips).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Optional accent color, `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Speech noise suppression, applied first.
    #[serde(default, skip_serializing_if = "Denoise::is_default")]
    pub denoise: Denoise,
    /// Noise gate, applied after noise suppression.
    #[serde(default, skip_serializing_if = "Gate::is_default")]
    pub gate: Gate,
    /// Equalizer, applied after the gate.
    #[serde(default, skip_serializing_if = "Equalizer::is_default")]
    pub eq: Equalizer,
    /// Compressor, applied after the equalizer and before the fader.
    #[serde(default, skip_serializing_if = "Compressor::is_default")]
    pub compressor: Compressor,
    /// Turning this strip down while other strips are heard, after its
    /// fader, in some or all of its mixes.
    #[serde(default, skip_serializing_if = "Ducking::is_default")]
    pub ducking: Ducking,
    /// External effects: its sound out to another program and back, at a
    /// point of its chain.
    #[serde(default, skip_serializing_if = "Insert::is_default")]
    pub insert: Insert,
}

impl Strip {
    /// A strip with every setting at its default.
    pub fn new(
        id: StripId,
        name: impl Into<String>,
        kind: StripKind,
        layout: ChannelLayout,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            layout,
            gain_db: 0.0,
            mute: false,
            solo: false,
            pan: 0.0,
            routes: BTreeSet::new(),
            sends: BTreeMap::new(),
            upmix: Upmix::Off,
            subwoofer: false,
            device: None,
            color: None,
            denoise: Denoise::default(),
            gate: Gate::default(),
            eq: Equalizer::default(),
            compressor: Compressor::default(),
            ducking: Ducking::default(),
            insert: Insert::default(),
        }
    }

    /// This strip's level in `bus`'s mix, in dB, on top of its fader.
    pub fn send_db(&self, bus: BusId) -> f32 {
        self.sends.get(&bus).copied().unwrap_or(0.0)
    }

    /// Clamp every setting into range and forget buses and strips that are
    /// not in `buses` and `strips`, adding what had to change to `fixes`.
    fn normalize(
        &mut self,
        buses: &BTreeSet<BusId>,
        strips: &BTreeSet<StripId>,
        fixes: &mut Vec<String>,
    ) {
        let id = self.id;
        let mut fix = |fixed: bool, what: &str| {
            if fixed {
                fixes.push(format!("strip {id} {what}"));
            }
        };
        self.gain_db = self.gain_db.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
        self.pan = self.pan.clamp(-1.0, 1.0);

        let before = self.routes.len();
        self.routes.retain(|b| buses.contains(b));
        fix(self.routes.len() != before, "routed to a missing bus");

        // A level of 0 dB is the same as none, so it is not kept.
        let before = self.sends.len();
        self.sends
            .retain(|b, db| buses.contains(b) && db.is_finite() && db.abs() >= 0.05);
        for db in self.sends.values_mut() {
            *db = db.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
        }
        fix(
            self.sends.len() != before,
            "had send levels for missing buses",
        );

        let empty = self.layout.channel_count() == 0;
        if empty {
            self.layout = ChannelLayout::Stereo;
        }
        fix(empty, "had an empty layout");
        fix(
            !normalize_color(&mut self.color),
            "had a color that is not #rrggbb",
        );
        fix(self.eq.normalize(), "had equalizer settings out of range");
        fix(self.gate.normalize(), "had gate settings out of range");
        fix(
            self.denoise.normalize(),
            "had a noise suppression amount out of range",
        );
        fix(
            self.compressor.normalize(),
            "had compressor settings out of range",
        );

        let d = &mut self.ducking;
        let before = (d.triggers.len(), d.buses.len());
        d.triggers.retain(|t| *t != id && strips.contains(t));
        d.buses.retain(|b| buses.contains(b));
        fix(
            (d.triggers.len(), d.buses.len()) != before,
            "was ducked by or in strips or buses that are gone",
        );
        fix(d.normalize(), "had ducking settings out of range");

        let at = self.insert.position.for_strip();
        fix(
            at != self.insert.position,
            "had its external effects at a place only buses have",
        );
        self.insert.position = at;
    }
}

/// An output bus: a mix of the strips routed to it.
///
/// A hardware bus plays its mix on a device, such as headphones or
/// speakers. A virtual bus is a microphone that other programs, such as a
/// voice chat or a streaming program, can record the mix from. Its mix goes
/// through its equalizer, then its fader, then its limiter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Bus {
    /// Unique among buses.
    pub id: BusId,
    /// Unique among buses, ignoring case, and 40 characters at most. A
    /// virtual bus's device shows in the system as "*name* (Weir)", with
    /// any colon left out (see [`device_description`]).
    pub name: String,
    /// Where it sends its mix.
    pub kind: BusKind,
    /// Its channels. Changing it recreates a virtual bus's device.
    #[serde(default)]
    pub layout: ChannelLayout,
    /// Fader position in dB, from [`GAIN_MIN_DB`] (silent) to
    /// [`GAIN_MAX_DB`].
    #[serde(default)]
    pub gain_db: f32,
    /// Silences the bus.
    #[serde(default)]
    pub mute: bool,
    /// Fold all channels to mono (Voicemeeter "mono" button).
    #[serde(default)]
    pub mono: bool,
    /// Delay added to the bus's output, in milliseconds, from 0 to
    /// [`BUS_DELAY_MAX_MS`]. Lines a bus up with one that takes longer to
    /// play, such as speakers next to a Bluetooth speaker.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub delay_ms: f32,
    /// PipeWire `node.name` of the sink to play to (hardware buses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Optional accent color, `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Equalizer, applied to the mix before the fader.
    #[serde(default, skip_serializing_if = "Equalizer::is_default")]
    pub eq: Equalizer,
    /// Safety limiter, after the fader.
    #[serde(default, skip_serializing_if = "Limiter::is_default")]
    pub limiter: Limiter,
    /// How it plays channels of a strip it has no speaker for.
    #[serde(default, skip_serializing_if = "Downmix::is_default")]
    pub downmix: Downmix,
    /// External effects: its mix out to another program and back, at a
    /// point of its chain.
    #[serde(default, skip_serializing_if = "Insert::is_default")]
    pub insert: Insert,
}

impl Bus {
    /// A bus with every setting at its default. Virtual buses start with
    /// their limiter on: whatever captures from them will clip what goes
    /// over, and nobody listening to the result can tell you it did.
    pub fn new(id: BusId, name: impl Into<String>, kind: BusKind, layout: ChannelLayout) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            layout,
            gain_db: 0.0,
            mute: false,
            mono: false,
            delay_ms: 0.0,
            device: None,
            color: None,
            eq: Equalizer::default(),
            limiter: match kind {
                BusKind::Virtual => Limiter::on(),
                BusKind::Hardware => Limiter::default(),
            },
            downmix: Downmix::default(),
            insert: Insert::default(),
        }
    }

    /// Clamp every setting into range, adding what had to change to
    /// `fixes`.
    fn normalize(&mut self, fixes: &mut Vec<String>) {
        let id = self.id;
        let mut fix = |fixed: bool, what: &str| {
            if fixed {
                fixes.push(format!("bus {id} {what}"));
            }
        };
        self.gain_db = self.gain_db.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
        let delay = self.delay_ms;
        self.delay_ms = finite_or(delay, 0.0).clamp(0.0, BUS_DELAY_MAX_MS);
        fix(self.delay_ms != delay, "had a delay out of range");
        let empty = self.layout.channel_count() == 0;
        if empty {
            self.layout = ChannelLayout::Stereo;
        }
        fix(empty, "had an empty layout");
        fix(
            !normalize_color(&mut self.color),
            "had a color that is not #rrggbb",
        );
        fix(self.eq.normalize(), "had equalizer settings out of range");
        fix(
            self.limiter.normalize(),
            "had limiter settings out of range",
        );
        fix(self.downmix.normalize(), "had downmix levels out of range");
        let at = self.insert.position.for_bus();
        fix(
            at != self.insert.position,
            "had its external effects at a place only strips have",
        );
        self.insert.position = at;
    }
}

/// Write a color as `#RRGGBB`, or drop it when it is not a color at all.
/// Returns `false` when it had to be dropped.
fn normalize_color(color: &mut Option<String>) -> bool {
    let Some(c) = color else {
        return true;
    };
    match parse_color(c) {
        Some(rgb) => {
            *color = Some(format_color(rgb));
            true
        }
        None => {
            *color = None;
            false
        }
    }
}

/// The complete user-visible mixer configuration.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MixerState {
    /// Every strip, in the order the mixer shows them, left to right.
    #[serde(default)]
    pub strips: Vec<Strip>,
    /// Every bus, in the order the mixer shows them.
    #[serde(default)]
    pub buses: Vec<Bus>,
}

impl MixerState {
    /// The strip with this id.
    pub fn strip(&self, id: StripId) -> Option<&Strip> {
        self.strips.iter().find(|s| s.id == id)
    }

    /// The strip with this id, to change.
    pub fn strip_mut(&mut self, id: StripId) -> Option<&mut Strip> {
        self.strips.iter_mut().find(|s| s.id == id)
    }

    /// The bus with this id.
    pub fn bus(&self, id: BusId) -> Option<&Bus> {
        self.buses.iter().find(|b| b.id == id)
    }

    /// The bus with this id, to change.
    pub fn bus_mut(&mut self, id: BusId) -> Option<&mut Bus> {
        self.buses.iter_mut().find(|b| b.id == id)
    }

    /// A bus's short name as the mixer shows it: `A1`, `A2`... for hardware
    /// buses and `B1`, `B2`... for virtual ones, numbered in list order.
    pub fn bus_label(&self, id: BusId) -> Option<String> {
        let bus = self.bus(id)?;
        let n = self
            .buses
            .iter()
            .filter(|b| b.kind == bus.kind)
            .position(|b| b.id == id)?
            + 1;
        Some(match bus.kind {
            BusKind::Hardware => format!("A{n}"),
            BusKind::Virtual => format!("B{n}"),
        })
    }

    /// Find a strip by its id or its name (ignoring case).
    pub fn find_strip(&self, key: &str) -> Option<&Strip> {
        let key = key.trim();
        key.parse::<StripId>()
            .ok()
            .and_then(|id| self.strip(id))
            .or_else(|| {
                self.strips
                    .iter()
                    .find(|s| s.name.eq_ignore_ascii_case(key))
            })
    }

    /// Find a bus by its id, its label (`A1`, `b2`) or its name (ignoring
    /// case). An id wins over a label, and a label over a name.
    pub fn find_bus(&self, key: &str) -> Option<&Bus> {
        let key = key.trim();
        key.parse::<BusId>()
            .ok()
            .and_then(|id| self.bus(id))
            .or_else(|| {
                self.buses.iter().find(|b| {
                    self.bus_label(b.id)
                        .is_some_and(|l| l.eq_ignore_ascii_case(key))
                })
            })
            .or_else(|| self.buses.iter().find(|b| b.name.eq_ignore_ascii_case(key)))
    }

    /// The id a new strip gets: one more than the highest in use.
    pub fn next_strip_id(&self) -> StripId {
        self.strips.iter().map(|s| s.id).max().map_or(1, |m| m + 1)
    }

    /// The id a new bus gets: one more than the highest in use.
    pub fn next_bus_id(&self) -> BusId {
        self.buses.iter().map(|b| b.id).max().map_or(1, |m| m + 1)
    }

    /// True when any strip is soloed.
    pub fn any_solo(&self) -> bool {
        self.strips.iter().any(|s| s.solo)
    }

    /// Clamp values into range and forget strips and buses that do not
    /// exist, wherever they are mentioned. Returns what had to be fixed, in
    /// words for a log.
    pub fn normalize(&mut self) -> Vec<String> {
        let mut fixes = Vec::new();
        let bus_ids: BTreeSet<BusId> = self.buses.iter().map(|b| b.id).collect();
        let strip_ids: BTreeSet<StripId> = self.strips.iter().map(|s| s.id).collect();
        let mut seen = BTreeSet::new();
        for s in &mut self.strips {
            if !seen.insert(s.id) {
                fixes.push(format!("duplicate strip id {}", s.id));
            }
            s.normalize(&bus_ids, &strip_ids, &mut fixes);
        }
        let mut seen = BTreeSet::new();
        for b in &mut self.buses {
            if !seen.insert(b.id) {
                fixes.push(format!("duplicate bus id {}", b.id));
            }
            b.normalize(&mut fixes);
        }
        fixes
    }
}

/// What the daemon does about the mixer window when it starts.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Startup {
    /// Open the window straight away.
    #[default]
    Window,
    /// Open the window, but minimized to the taskbar.
    Minimized,
    /// Do not open the window. The mixer keeps running and is reachable from
    /// the tray icon and the command line.
    TrayOnly,
}

impl Startup {
    /// Every choice, in the order menus offer them.
    pub const ALL: [Startup; 3] = [Self::Window, Self::Minimized, Self::TrayOnly];

    /// A short description for menus.
    pub fn label(self) -> &'static str {
        match self {
            Self::Window => "Open the mixer window",
            Self::Minimized => "Start minimized",
            Self::TrayOnly => "Tray only, no window",
        }
    }
}

/// How the tray icon is drawn.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TrayIcon {
    /// The full-color app icon.
    #[default]
    Color,
    /// A plain one-color outline that takes the panel's text color, like
    /// the desktop's own tray icons.
    OneColor,
}

impl TrayIcon {
    /// Every choice, in the order menus offer them.
    pub const ALL: [TrayIcon; 2] = [Self::Color, Self::OneColor];

    /// A short description for menus.
    pub fn label(self) -> &'static str {
        match self {
            Self::Color => "In color",
            Self::OneColor => "One color, matching the panel",
        }
    }
}

/// What soloing a strip does. Written `"exclusive"`, or `{"cue": 1}` to cue
/// on bus 1.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SoloMode {
    /// Every strip that is not soloed goes silent, in every mix.
    #[default]
    Exclusive,
    /// Only the given bus's mix changes: it plays just the soloed strips,
    /// while every other mix carries on as it was. For checking a strip in
    /// your headphones without the stream noticing.
    Cue(BusId),
}

/// The colors the window offers for strips and buses, by name. Any other
/// `#rrggbb` works too.
pub const COLOR_PRESETS: [(&str, &str); 9] = [
    ("orange", "#E6913C"),
    ("red", "#EB4B41"),
    ("yellow", "#EBC846"),
    ("green", "#46C864"),
    ("teal", "#3CBEAF"),
    ("blue", "#3C91E1"),
    ("purple", "#9678E6"),
    ("pink", "#CD6EAA"),
    ("gray", "#A0A8B4"),
];

/// Read a `#rrggbb` color.
pub fn parse_color(s: &str) -> Option<[u8; 3]> {
    let hex = s.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// A color as `#RRGGBB`.
pub fn format_color(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

/// Sample rates Weir offers to hold PipeWire at.
pub const SAMPLE_RATES: [u32; 5] = [44_100, 48_000, 88_200, 96_000, 192_000];
/// Buffer sizes, in frames, Weir offers to hold PipeWire at.
pub const QUANTA: [u32; 6] = [64, 128, 256, 512, 1024, 2048];

/// Daemon settings, as opposed to the mix itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Settings {
    /// How many `meters` notifications a second the daemon sends, from 1 to
    /// 120. Each connection can ask for fewer when it subscribes.
    pub meter_rate_hz: u32,
    /// What to do about the window when the daemon starts.
    pub startup: Startup,
    /// Whether Weir's service starts when you log in, or `None` where
    /// that cannot be set: systemd is not running, or Weir's service file
    /// is not installed, as when running from a build folder. systemd keeps
    /// this rather than the configuration file, and the daemon reads it
    /// back from there when it starts and after changing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_at_login: Option<bool>,
    /// Show a system tray icon.
    pub tray: bool,
    /// How the tray icon is drawn.
    pub tray_icon: TrayIcon,
    /// What soloing a strip does.
    pub solo: SoloMode,
    /// The sample rate to hold PipeWire at while Weir runs, or
    /// `None` to let PipeWire decide. Noise suppression only works at 48000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// The buffer size, in frames, to hold PipeWire at while Weir
    /// runs, or `None` to let PipeWire decide. Smaller means less delay and
    /// more CPU.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantum: Option<u32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            meter_rate_hz: 30,
            startup: Startup::default(),
            start_at_login: None,
            tray: true,
            tray_icon: TrayIcon::default(),
            solo: SoloMode::default(),
            sample_rate: None,
            quantum: None,
        }
    }
}

/// Whether a device makes sound or plays it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// Produces audio (microphones, capture cards, monitors of sinks).
    Source,
    /// Consumes audio (headphones, speakers).
    Sink,
}

/// A PipeWire node that can be assigned to a hardware strip or bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeviceInfo {
    /// PipeWire object id (changes across reboots; use `name` to persist).
    pub id: u32,
    /// Stable PipeWire `node.name`. Monitors of sinks are suffixed `.monitor`.
    pub name: String,
    /// Human readable description.
    pub description: String,
    /// Source (for hardware strips) or sink (for hardware buses).
    pub kind: DeviceKind,
    /// Channel positions the device exposes, in port order.
    pub channels: Vec<ChannelPosition>,
}

/// An application playing sound, wherever it plays: into one of Weir's
/// strips or straight to a device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AppStream {
    /// PipeWire node id of the stream. PipeWire hands the id of a stream
    /// that ended to the next one, so tell applications apart by id and
    /// name together.
    pub id: u32,
    /// The application's name (`application.name`), such as "Firefox".
    pub name: String,
    /// The program playing it (`application.process.binary`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// What it is playing (`media.name`), such as a video's title, when it
    /// says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_name: Option<String>,
    /// `node.name` of the sink the stream is currently connected to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Strip id when the target is one of our virtual inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip: Option<StripId>,
    /// The stream's own volume, the one the system volume applet shows.
    /// `None` when the stream has not reported one yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_db: Option<f32>,
    /// The stream's own mute flag, independent of the strip's.
    #[serde(default)]
    pub mute: bool,
}

/// How the audio engine is doing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EngineStatus {
    /// Whether the engine is connected to PipeWire.
    pub connected: bool,
    /// `connecting`, `unconnected`, `paused`, `streaming` (all is well) or
    /// `error`; `disabled` when the daemon runs without an engine.
    pub state: String,
    /// The sample rate the engine runs at, in Hz. 0 until known.
    pub sample_rate: u32,
    /// How many frames the engine processes at a time, which sets its delay:
    /// 1024 frames at 48000 Hz is 21 ms. 0 until known.
    pub quantum: u32,
    /// PipeWire node id of the engine's mixing node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<u32>,
    /// What went wrong, in the `error` state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Peak levels in dBFS since the previous meter message, per channel, and
/// what the effects with something to show are doing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Meters {
    /// Each strip's level after its effects and fader, per channel.
    pub strips: BTreeMap<StripId, Vec<f32>>,
    /// Each bus's output, after its limiter, per channel.
    pub buses: BTreeMap<BusId, Vec<f32>>,
    /// What each strip's noise gate is doing. Only strips with the gate
    /// switched on are listed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub gates: BTreeMap<StripId, GateMeter>,
    /// How far each bus's limiter turned the mix down since the previous
    /// message, in dB (0 when it did nothing). Only buses with the limiter on
    /// are listed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub limiters: BTreeMap<BusId, f32>,
    /// What each strip's compressor is doing. Only strips with the
    /// compressor on are listed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub compressors: BTreeMap<StripId, CompressorMeter>,
    /// How far each strip with ducking on is being turned down, in dB (0
    /// or less), at its lowest since the previous message.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ducking: BTreeMap<StripId, f32>,
}

impl Meters {
    /// Fold `later` into these, keeping the loudest peak and the most
    /// reduction of each, for sending both as one message.
    pub fn merge(&mut self, later: &Meters) {
        fn peaks<K: Ord + Copy>(into: &mut BTreeMap<K, Vec<f32>>, from: &BTreeMap<K, Vec<f32>>) {
            for (k, v) in from {
                let e = into.entry(*k).or_insert_with(|| v.clone());
                if e.len() != v.len() {
                    *e = v.clone();
                }
                for (a, b) in e.iter_mut().zip(v) {
                    *a = a.max(*b);
                }
            }
        }
        peaks(&mut self.strips, &later.strips);
        peaks(&mut self.buses, &later.buses);
        for (k, g) in &later.gates {
            let e = self.gates.entry(*k).or_insert(*g);
            e.level_db = e.level_db.max(g.level_db);
            e.reduction_db = g.reduction_db;
        }
        for (k, c) in &later.compressors {
            let e = self.compressors.entry(*k).or_insert(*c);
            e.level_db = e.level_db.max(c.level_db);
            e.reduction_db = e.reduction_db.min(c.reduction_db);
        }
        for (k, v) in &later.limiters {
            let e = self.limiters.entry(*k).or_insert(*v);
            *e = e.min(*v);
        }
        for (k, v) in &later.ducking {
            let e = self.ducking.entry(*k).or_insert(*v);
            *e = e.min(*v);
        }
    }
}

/// A compressor's view of things, for its curve and its gain reduction
/// display.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompressorMeter {
    /// Peak level going into the compressor since the previous meter
    /// message, in dBFS.
    pub level_db: f32,
    /// The most it turned the strip down since the previous meter message,
    /// in dB (0 or less), before the lift.
    pub reduction_db: f32,
}

/// A noise gate's view of things, for setting its threshold.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GateMeter {
    /// Peak level going into the gate since the previous meter message, in
    /// dBFS. The gate opens when this rises above its threshold.
    pub level_db: f32,
    /// How far the gate is turning the strip down right now, in dB: 0 when
    /// open, down to the gate's range when closed.
    pub reduction_db: f32,
}

/// Everything a client needs to render itself.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FullState {
    /// The strips and buses.
    pub mixer: MixerState,
    /// Devices that hardware strips and buses can use.
    pub devices: Vec<DeviceInfo>,
    /// Applications playing sound.
    pub apps: Vec<AppStream>,
    /// How the audio engine is doing.
    pub engine: EngineStatus,
    /// The daemon's own settings.
    #[serde(default)]
    pub settings: Settings,
    /// Built-in and saved equalizer presets.
    #[serde(default)]
    pub eq_presets: Vec<EqPreset>,
    /// Where applications are put when they start playing.
    #[serde(default)]
    pub app_rules: Vec<AppRule>,
    /// Saved scenes and setups.
    #[serde(default)]
    pub library: crate::library::Library,
    /// The volumes the system has set on Weir's virtual devices.
    #[serde(default)]
    pub system_volumes: SystemVolumes,
    /// Whether the external effects of each strip and bus that has them
    /// on are connected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inserts: Vec<InsertStatus>,
    /// Every hotkey, and how keys reach Weir.
    #[serde(default)]
    pub hotkeys: crate::hotkeys::HotkeysInfo,
}

/// Whether a strip's or bus's external effects are connected: whether any
/// program plays into its "from effects" device. While nothing does,
/// its sound carries on as [`Insert::fallback`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InsertStatus {
    /// The strip or bus.
    pub target: StripOrBus,
    /// Whether something plays into its "from effects" device, or straight
    /// into its ports on Weir's effects return node.
    pub connected: bool,
}

/// The volume the system has set on one of Weir's own virtual
/// devices, with KDE's volume control or pavucontrol. It turns everything
/// going through that device up or down before (for a strip) or after (for
/// a bus) Weir. Weir shows it and never changes it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SystemVolume {
    /// The loudest channel's gain, in dB; 0 is full volume.
    pub volume_db: f32,
    /// Muted in the system's volume control.
    pub mute: bool,
}

impl SystemVolume {
    /// The volume as a percentage the way KDE and pavucontrol show it: they
    /// use a cubic scale, so 70% there is about -9.3 dB.
    pub fn percent(&self) -> f32 {
        db_to_linear(self.volume_db).cbrt() * 100.0
    }

    /// Whether it is turning anything down.
    pub fn is_reducing(&self) -> bool {
        self.mute || self.volume_db < -0.05
    }
}

/// The system volumes of Weir's virtual devices, by the strip or bus
/// each belongs to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SystemVolumes {
    /// By virtual strip.
    #[serde(default)]
    pub strips: BTreeMap<StripId, SystemVolume>,
    /// By virtual bus.
    #[serde(default)]
    pub buses: BTreeMap<BusId, SystemVolume>,
}

/// Where an application is put when it starts playing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AppRule {
    /// The application's name, or the name of its program, as `apps` lists
    /// them. Case does not matter.
    pub app: String,
    /// The virtual strip to put it on, or `None` to leave it where it goes
    /// by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip: Option<StripId>,
}

impl AppRule {
    /// Whether this rule is about `app`: its name or its program's name is
    /// the rule's `app`, ignoring case.
    pub fn matches(&self, app: &AppStream) -> bool {
        let want = self.app.trim();
        !want.is_empty()
            && (app.name.eq_ignore_ascii_case(want)
                || app
                    .binary
                    .as_deref()
                    .is_some_and(|b| b.eq_ignore_ascii_case(want)))
    }
}

/// The rule for `app`: the first one that matches.
pub fn rule_for<'a>(rules: &'a [AppRule], app: &AppStream) -> Option<&'a AppRule> {
    rules.iter().find(|r| r.matches(app))
}

/// A level in dB as a factor to multiply samples by. At or below
/// [`GAIN_MIN_DB`] it is 0: silence.
pub fn db_to_linear(db: f32) -> f32 {
    if db <= GAIN_MIN_DB {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// A factor or a sample's size as a level in dB, no lower than
/// [`METER_FLOOR_DB`].
pub fn linear_to_db(lin: f32) -> f32 {
    if lin <= 0.0 {
        METER_FLOOR_DB
    } else {
        (20.0 * lin.log10()).max(METER_FLOOR_DB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::InsertPoint;

    #[test]
    fn send_levels_travel_as_json() {
        let mut s = Strip::new(1, "Music", StripKind::Virtual, ChannelLayout::Stereo);
        s.sends.insert(2, -6.0);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""sends":{"2":-6.0}"#), "{json}");
        let back: Strip = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sends, s.sends);
    }

    #[test]
    fn layout_roundtrip() {
        for l in ChannelLayout::PRESETS.iter() {
            let json = serde_json::to_string(l).unwrap();
            let back: ChannelLayout = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, l);
            assert_eq!(ChannelLayout::from_positions(l.positions()), *l);
        }
        assert_eq!(ChannelLayout::parse("5.1"), Some(ChannelLayout::Surround51));
        assert_eq!(ChannelLayout::parse("FL FR"), Some(ChannelLayout::Stereo));
        assert_eq!(
            ChannelLayout::parse("FL,FR,LFE"),
            Some(ChannelLayout::Custom(vec![
                ChannelPosition::FL,
                ChannelPosition::FR,
                ChannelPosition::LFE
            ]))
        );
        assert_eq!(
            serde_json::to_string(&ChannelLayout::Surround51).unwrap(),
            "\"surround_5_1\""
        );
    }

    #[test]
    fn positions_parse() {
        assert_eq!(ChannelPosition::parse("fl"), Some(ChannelPosition::FL));
        assert_eq!(ChannelPosition::parse("MONO"), Some(ChannelPosition::Mono));
        assert_eq!(ChannelPosition::parse("AUX1"), Some(ChannelPosition::FR));
        assert_eq!(ChannelPosition::parse("nope"), None);
        assert_eq!(
            serde_json::to_string(&ChannelPosition::LFE).unwrap(),
            "\"LFE\""
        );
    }

    #[test]
    fn normalize_fixes_routes() {
        let mut m = MixerState {
            strips: vec![Strip {
                gain_db: 40.0,
                pan: -3.0,
                routes: [1, 9].into_iter().collect(),
                ..Strip::new(1, "a", StripKind::Virtual, ChannelLayout::Stereo)
            }],
            buses: vec![Bus::new(1, "b", BusKind::Hardware, ChannelLayout::Stereo)],
        };
        let fixes = m.normalize();
        assert_eq!(fixes.len(), 1);
        assert_eq!(m.strips[0].gain_db, GAIN_MAX_DB);
        assert_eq!(m.strips[0].pan, -1.0);
        assert_eq!(m.strips[0].routes.len(), 1);
    }

    #[test]
    fn external_effects_move_to_a_place_the_strip_or_bus_has() {
        let insert = |position| Insert {
            enabled: true,
            position,
            ..Insert::default()
        };
        let mut m = MixerState {
            strips: vec![Strip {
                insert: insert(InsertPoint::AfterLimiter),
                ..Strip::new(1, "a", StripKind::Virtual, ChannelLayout::Stereo)
            }],
            buses: vec![Bus {
                insert: insert(InsertPoint::BeforeGate),
                ..Bus::new(1, "b", BusKind::Hardware, ChannelLayout::Stereo)
            }],
        };
        assert_eq!(m.normalize().len(), 2);
        assert_eq!(m.strips[0].insert.position, InsertPoint::AfterFader);
        assert_eq!(m.buses[0].insert.position, InsertPoint::BeforeEq);
        // Every place maps onto one the other has.
        for p in InsertPoint::STRIP {
            assert!(InsertPoint::BUS.contains(&p.for_bus()), "{p:?}");
            assert_eq!(p.for_strip(), p);
        }
        for p in InsertPoint::BUS {
            assert!(InsertPoint::STRIP.contains(&p.for_strip()), "{p:?}");
            assert_eq!(p.for_bus(), p);
        }
    }

    #[test]
    fn device_names_leave_colons_out() {
        assert_eq!(device_description("Music"), "Music (Weir)");
        assert_eq!(device_description("Mic: USB"), "Mic - USB (Weir)");
        assert_eq!(
            Insert::device_names("Game:Chat"),
            (
                "Game-Chat to effects (Weir)".to_string(),
                "Game-Chat from effects (Weir)".to_string()
            )
        );
    }

    #[test]
    fn external_effects_stay_out_of_the_json_until_changed() {
        let s = Strip::new(1, "Mic", StripKind::Hardware, ChannelLayout::Mono);
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("insert"), "{json}");
        let on = Strip {
            insert: Insert {
                enabled: true,
                ..Insert::default()
            },
            ..s
        };
        let json = serde_json::to_string(&on).unwrap();
        assert!(
            json.contains(
                r#""insert":{"enabled":true,"position":"before_fader","fallback":"pass_through"}"#
            ),
            "{json}"
        );
    }

    #[test]
    fn ducking_forgets_strips_and_buses_that_are_gone() {
        let mut m = MixerState {
            strips: vec![
                Strip::new(1, "mic", StripKind::Hardware, ChannelLayout::Mono),
                Strip {
                    ducking: Ducking {
                        enabled: true,
                        triggers: [1, 2, 7].into_iter().collect(),
                        buses: [1, 9].into_iter().collect(),
                        ..Ducking::default()
                    },
                    ..Strip::new(2, "music", StripKind::Virtual, ChannelLayout::Stereo)
                },
            ],
            buses: vec![Bus::new(1, "b", BusKind::Hardware, ChannelLayout::Stereo)],
        };
        m.normalize();
        let d = &m.strips[1].ducking;
        // Not itself, not a strip that does not exist.
        assert_eq!(d.triggers, [1].into_iter().collect());
        assert_eq!(d.buses, [1].into_iter().collect());
    }

    #[test]
    fn buses_are_found_by_id_label_or_name() {
        let m = MixerState {
            strips: vec![],
            buses: vec![
                Bus::new(1, "Headset", BusKind::Hardware, ChannelLayout::Stereo),
                Bus::new(5, "Stream", BusKind::Virtual, ChannelLayout::Stereo),
                Bus::new(7, "Speakers", BusKind::Hardware, ChannelLayout::Stereo),
            ],
        };
        assert_eq!(m.bus_label(7).as_deref(), Some("A2"));
        assert_eq!(m.bus_label(5).as_deref(), Some("B1"));
        assert_eq!(m.find_bus("a2").map(|b| b.id), Some(7));
        assert_eq!(m.find_bus("B1").map(|b| b.id), Some(5));
        assert_eq!(m.find_bus("5").map(|b| b.id), Some(5));
        assert_eq!(m.find_bus("stream").map(|b| b.id), Some(5));
        assert!(m.find_bus("B2").is_none());
    }

    #[test]
    fn colors_are_read_and_tidied() {
        assert_eq!(parse_color("#e6913c"), Some([0xE6, 0x91, 0x3C]));
        assert_eq!(parse_color("e6913c"), None);
        assert_eq!(parse_color("#e6913"), None);
        let mut m = MixerState {
            strips: vec![
                Strip {
                    color: Some("#e6913c".into()),
                    ..Strip::new(1, "a", StripKind::Virtual, ChannelLayout::Stereo)
                },
                Strip {
                    color: Some("orange".into()),
                    ..Strip::new(2, "b", StripKind::Virtual, ChannelLayout::Stereo)
                },
            ],
            buses: vec![],
        };
        m.normalize();
        assert_eq!(m.strips[0].color.as_deref(), Some("#E6913C"));
        assert_eq!(m.strips[1].color, None);
    }

    #[test]
    fn system_volumes_read_like_the_system_shows_them() {
        let v = SystemVolume {
            volume_db: linear_to_db(0.343),
            mute: false,
        };
        assert!((v.percent() - 70.0).abs() < 0.1, "{}", v.percent());
        assert!(v.is_reducing());
        let full = SystemVolume {
            volume_db: 0.0,
            mute: false,
        };
        assert!(!full.is_reducing());
        assert!((full.percent() - 100.0).abs() < 1e-3);
    }

    #[test]
    fn merged_meters_keep_the_peaks_and_the_most_reduction() {
        let mut a = Meters::default();
        a.strips.insert(1, vec![-10.0, -20.0]);
        a.limiters.insert(3, -1.0);
        let mut b = Meters::default();
        b.strips.insert(1, vec![-15.0, -5.0]);
        b.strips.insert(2, vec![-30.0]);
        b.limiters.insert(3, -4.0);
        a.merge(&b);
        assert_eq!(a.strips[&1], vec![-10.0, -5.0]);
        assert_eq!(a.strips[&2], vec![-30.0]);
        assert_eq!(a.limiters[&3], -4.0);
    }

    #[test]
    fn db_conversion() {
        assert_eq!(db_to_linear(0.0), 1.0);
        assert!((db_to_linear(-6.0) - 0.5012).abs() < 1e-3);
        assert_eq!(db_to_linear(-60.0), 0.0);
        assert_eq!(linear_to_db(0.0), METER_FLOOR_DB);
        assert!((linear_to_db(1.0)).abs() < 1e-6);
    }

    #[test]
    fn spread_from_older_files_reads_as_upmix() {
        let json = serde_json::json!({
            "id": 1, "name": "Music", "kind": "virtual", "spread": "front_center"
        });
        let s: Strip = serde_json::from_value(json).unwrap();
        assert_eq!(s.upmix, Upmix::Center);
        for (old, new) in [
            ("front", Upmix::Off),
            ("all", Upmix::AllChannelStereo),
            ("passive_surround", Upmix::PassiveSurround),
        ] {
            let u: Upmix = serde_json::from_value(serde_json::json!(old)).unwrap();
            assert_eq!(u, new);
        }
        // Written back under the new names.
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["upmix"], "center");
        assert!(v.get("spread").is_none());
    }

    #[test]
    fn downmix_levels_are_kept_in_range_and_left_out_when_default() {
        let mut d = Downmix {
            center_db: 4.0,
            surround_db: f32::NAN,
            ..Downmix::default()
        };
        assert!(d.normalize());
        assert_eq!((d.center_db, d.surround_db), (0.0, -3.0));
        let mut b = Bus::new(1, "Headset", BusKind::Hardware, ChannelLayout::Stereo);
        assert!(serde_json::to_value(&b).unwrap().get("downmix").is_none());
        b.downmix.method = DownmixMethod::Matrix;
        let v = serde_json::to_value(&b).unwrap();
        assert_eq!(v["downmix"]["method"], "matrix");
        // A file with only the method keeps the standard levels.
        let d: Downmix = serde_json::from_value(serde_json::json!({"method": "matrix"})).unwrap();
        assert_eq!(d.center_db, -3.0);
        assert_eq!(mix_level_label(-4.5), "−4.5 dB");
        assert_eq!(mix_level_label(GAIN_MIN_DB), "Off");
    }
}
