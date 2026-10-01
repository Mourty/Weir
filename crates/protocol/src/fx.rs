//! Per-strip and per-bus processing: the settings of every effect, plus the
//! equalizer's filter design and the compressor's curve.
//!
//! The filter maths lives here rather than in the engine so that every client
//! draws exactly the curve the engine applies. Each band is a state variable
//! filter discretised with the trapezoidal rule (Andrew Simper's "linear trap"
//! SVF). It behaves well when its settings change while audio runs through it,
//! and its frequency response is the analog prototype evaluated through the
//! bilinear transform, which is what [`SvfCoefs::response`] computes.

use crate::model::{finite_or, BusId, StripId, GAIN_MAX_DB, GAIN_MIN_DB};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Most bands one equalizer can have.
pub const EQ_MAX_BANDS: usize = 16;
/// Lowest frequency a band can have, in Hz.
pub const EQ_FREQ_MIN_HZ: f32 = 20.0;
/// Highest frequency a band can have, in Hz.
pub const EQ_FREQ_MAX_HZ: f32 = 20_000.0;
/// Deepest cut a band can make, in dB.
pub const EQ_GAIN_MIN_DB: f32 = -24.0;
/// Biggest boost a band can make, in dB.
pub const EQ_GAIN_MAX_DB: f32 = 24.0;
/// Widest a band can be (its lowest Q).
pub const EQ_Q_MIN: f32 = 0.1;
/// Narrowest a band can be (its highest Q).
pub const EQ_Q_MAX: f32 = 20.0;
/// Q of a Butterworth response: the neutral choice for shelves and cut filters.
pub const Q_BUTTERWORTH: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// The shape of one equalizer band.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EqBandKind {
    /// Boost or cut around a frequency (a "bell").
    #[default]
    Peak,
    /// Boost or cut everything below a frequency.
    LowShelf,
    /// Boost or cut everything above a frequency.
    HighShelf,
    /// Remove everything above a frequency (12 dB per octave).
    LowPass,
    /// Remove everything below a frequency (12 dB per octave).
    HighPass,
    /// Remove a narrow range around a frequency, such as mains hum.
    Notch,
    /// Keep only a range around a frequency.
    BandPass,
}

impl EqBandKind {
    /// Every shape, in the order menus offer them.
    pub const ALL: [EqBandKind; 7] = [
        Self::Peak,
        Self::LowShelf,
        Self::HighShelf,
        Self::LowPass,
        Self::HighPass,
        Self::Notch,
        Self::BandPass,
    ];

    /// A short name for menus. A peak is called a bell there, as most
    /// equalizers call it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Peak => "Bell",
            Self::LowShelf => "Low shelf",
            Self::HighShelf => "High shelf",
            Self::LowPass => "Low pass",
            Self::HighPass => "High pass",
            Self::Notch => "Notch",
            Self::BandPass => "Band pass",
        }
    }

    /// Whether the band's gain setting does anything.
    pub fn uses_gain(self) -> bool {
        matches!(self, Self::Peak | Self::LowShelf | Self::HighShelf)
    }

    /// Read a shape as people type it: `bell`, `low-shelf`, `hp`, `low cut`
    /// and the like.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        match s.as_str() {
            "peak" | "bell" | "peaking" => Some(Self::Peak),
            "low_shelf" | "lowshelf" | "ls" => Some(Self::LowShelf),
            "high_shelf" | "highshelf" | "hs" => Some(Self::HighShelf),
            "low_pass" | "lowpass" | "lp" | "high_cut" => Some(Self::LowPass),
            "high_pass" | "highpass" | "hp" | "low_cut" => Some(Self::HighPass),
            "notch" => Some(Self::Notch),
            "band_pass" | "bandpass" | "bp" => Some(Self::BandPass),
            _ => None,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_q() -> f32 {
    Q_BUTTERWORTH
}

/// One equalizer band.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EqBand {
    /// Its shape; a bell when not given.
    #[serde(default)]
    pub kind: EqBandKind,
    /// Center or corner frequency in Hz, from 20 to 20000.
    pub freq_hz: f32,
    /// Boost (positive) or cut (negative), from -24 to 24 dB. Only bells and
    /// shelves use it.
    #[serde(default)]
    pub gain_db: f32,
    /// Width for bells, notches and band passes (higher is narrower);
    /// resonance for shelves and cut filters (0.707, the default, is the
    /// neutral choice). From 0.1 to 20.
    #[serde(default = "default_q")]
    pub q: f32,
    /// A disabled band keeps its settings but does nothing.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl EqBand {
    /// An enabled band.
    pub fn new(kind: EqBandKind, freq_hz: f32, gain_db: f32, q: f32) -> Self {
        Self {
            kind,
            freq_hz,
            gain_db,
            q,
            enabled: true,
        }
    }

    /// Clamp every setting into its valid range, replacing anything that is
    /// not a number with a neutral value.
    pub fn clamped(mut self) -> Self {
        self.freq_hz = finite_or(self.freq_hz, 1000.0).clamp(EQ_FREQ_MIN_HZ, EQ_FREQ_MAX_HZ);
        self.gain_db = finite_or(self.gain_db, 0.0).clamp(EQ_GAIN_MIN_DB, EQ_GAIN_MAX_DB);
        self.q = finite_or(self.q, Q_BUTTERWORTH).clamp(EQ_Q_MIN, EQ_Q_MAX);
        self
    }

    /// Whether this band changes the sound at all. A bell or shelf at 0 dB
    /// passes everything through untouched.
    pub fn is_active(&self) -> bool {
        self.enabled && (!self.kind.uses_gain() || self.gain_db != 0.0)
    }

    /// Filter coefficients at `sample_rate`.
    pub fn coefs(&self, sample_rate: f32) -> SvfCoefs {
        let b = self.clamped();
        let fs = sample_rate.max(8000.0) as f64;
        // Keep the frequency below Nyquist, where tan() heads to infinity.
        let f = (b.freq_hz as f64).min(fs * 0.49);
        let w = (std::f64::consts::PI * f / fs).tan();
        let q = b.q as f64;
        let a = 10f64.powf(b.gain_db as f64 / 40.0);
        let (g, k, m0, m1, m2) = match b.kind {
            EqBandKind::Peak => {
                let k = 1.0 / (q * a);
                (w, k, 1.0, k * (a * a - 1.0), 0.0)
            }
            EqBandKind::LowShelf => {
                let k = 1.0 / q;
                (w / a.sqrt(), k, 1.0, k * (a - 1.0), a * a - 1.0)
            }
            EqBandKind::HighShelf => {
                let k = 1.0 / q;
                (w * a.sqrt(), k, a * a, k * (1.0 - a) * a, 1.0 - a * a)
            }
            EqBandKind::LowPass => (w, 1.0 / q, 0.0, 0.0, 1.0),
            EqBandKind::HighPass => {
                let k = 1.0 / q;
                (w, k, 1.0, -k, -1.0)
            }
            EqBandKind::Notch => {
                let k = 1.0 / q;
                (w, k, 1.0, -k, 0.0)
            }
            EqBandKind::BandPass => {
                let k = 1.0 / q;
                (w, k, 0.0, k, 0.0)
            }
        };
        SvfCoefs::from_parts(g, k, m0, m1, m2)
    }

    /// Gain of this band alone at `freq` Hz, in dB.
    pub fn response_db(&self, freq: f32, sample_rate: f32) -> f32 {
        if !self.enabled {
            return 0.0;
        }
        self.coefs(sample_rate).response_db(freq, sample_rate)
    }
}

/// Coefficients of one trapezoidal state variable filter.
///
/// The output is `m0 * input + m1 * bandpass + m2 * lowpass`, where the band
/// and low pass outputs come from the same pair of integrators. Every band
/// shape is just a different mix of those three signals.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SvfCoefs {
    /// The integrators' gain: the prewarped cutoff, `tan(pi f / fs)`.
    pub g: f32,
    /// Damping, `1 / Q`: how much of the band pass is fed back.
    pub k: f32,
    /// `1 / (1 + g (g + k))`, which solves the filter's feedback loop.
    pub a1: f32,
    /// `g * a1`.
    pub a2: f32,
    /// `g * a2`.
    pub a3: f32,
    /// How much of the input the output has.
    pub m0: f32,
    /// How much of the band pass the output has.
    pub m1: f32,
    /// How much of the low pass the output has.
    pub m2: f32,
}

impl SvfCoefs {
    /// Passes the signal through unchanged.
    pub const IDENTITY: SvfCoefs = SvfCoefs {
        g: 0.0,
        k: 1.0,
        a1: 1.0,
        a2: 0.0,
        a3: 0.0,
        m0: 1.0,
        m1: 0.0,
        m2: 0.0,
    };

    fn from_parts(g: f64, k: f64, m0: f64, m1: f64, m2: f64) -> Self {
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        Self {
            g: g as f32,
            k: k as f32,
            a1: a1 as f32,
            a2: a2 as f32,
            a3: a3 as f32,
            m0: m0 as f32,
            m1: m1 as f32,
            m2: m2 as f32,
        }
    }

    /// Run one sample through the filter. `state` holds the two integrators.
    #[inline(always)]
    pub fn tick(&self, state: &mut [f32; 2], v0: f32) -> f32 {
        let [ic1, ic2] = *state;
        let v3 = v0 - ic2;
        let v1 = self.a1 * ic1 + self.a2 * v3;
        let v2 = ic2 + self.a2 * ic1 + self.a3 * v3;
        state[0] = 2.0 * v1 - ic1;
        state[1] = 2.0 * v2 - ic2;
        self.m0 * v0 + self.m1 * v1 + self.m2 * v2
    }

    /// Gain at `freq` Hz, in dB. Exact for the discrete filter, not an
    /// approximation of it.
    pub fn response_db(&self, freq: f32, sample_rate: f32) -> f32 {
        let (re, im) = self.response(freq, sample_rate);
        let mag2 = re * re + im * im;
        (10.0 * mag2.max(1e-20).log10()) as f32
    }

    /// Complex response at `freq` Hz as `(re, im)`.
    pub fn response(&self, freq: f32, sample_rate: f32) -> (f64, f64) {
        if self.g == 0.0 {
            return (self.m0 as f64, 0.0);
        }
        let fs = sample_rate.max(8000.0) as f64;
        let f = (freq as f64).clamp(0.0, fs * 0.4999);
        // The bilinear transform maps this frequency to s = j * w.
        let w = (std::f64::consts::PI * f / fs).tan() / self.g as f64;
        let k = self.k as f64;
        // Denominator s^2 + k s + 1 at s = jw.
        let (dr, di) = (1.0 - w * w, k * w);
        // Numerator m0 (s^2 + k s + 1) + m1 s + m2.
        let (m0, m1, m2) = (self.m0 as f64, self.m1 as f64, self.m2 as f64);
        let (nr, ni) = (m0 * dr + m2, m0 * di + m1 * w);
        let d2 = dr * dr + di * di;
        if d2 == 0.0 {
            return (f64::INFINITY, 0.0);
        }
        ((nr * dr + ni * di) / d2, (ni * dr - nr * di) / d2)
    }
}

/// A strip's or bus's equalizer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Equalizer {
    /// Off means the audio passes through untouched, whatever the bands say.
    #[serde(default)]
    pub enabled: bool,
    /// Up to [`EQ_MAX_BANDS`] bands, applied one after the other.
    #[serde(default)]
    pub bands: Vec<EqBand>,
}

impl Equalizer {
    /// Whether it is off with no bands.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The bands that change the sound, when the equalizer is on.
    pub fn active_bands(&self) -> impl Iterator<Item = &EqBand> {
        self.bands
            .iter()
            .filter(move |b| self.enabled && b.is_active())
    }

    /// Combined gain of every enabled band at `freq` Hz, in dB, ignoring
    /// whether the equalizer as a whole is on.
    pub fn response_db(&self, freq: f32, sample_rate: f32) -> f32 {
        let (mut re, mut im) = (1.0f64, 0.0f64);
        for b in self.bands.iter().filter(|b| b.enabled) {
            let (r, i) = b.coefs(sample_rate).response(freq, sample_rate);
            (re, im) = (re * r - im * i, re * i + im * r);
        }
        (10.0 * (re * re + im * im).max(1e-20).log10()) as f32
    }

    /// Keep at most [`EQ_MAX_BANDS`] bands, each within range.
    pub fn normalize(&mut self) -> bool {
        let before = self.clone();
        self.bands.truncate(EQ_MAX_BANDS);
        for b in &mut self.bands {
            *b = b.clamped();
        }
        *self != before
    }
}

/// Lowest threshold a gate can have.
pub const GATE_THRESHOLD_MIN_DB: f32 = -90.0;
/// Highest threshold a gate can have.
pub const GATE_THRESHOLD_MAX_DB: f32 = 0.0;
/// The most a closed gate can turn a strip down: effectively silence.
pub const GATE_RANGE_MIN_DB: f32 = -90.0;
/// The least a closed gate can turn a strip down: not at all.
pub const GATE_RANGE_MAX_DB: f32 = 0.0;
/// The gate closes this far below the threshold, not at it, so that a level
/// hovering around the threshold does not make it chatter.
pub const GATE_HYSTERESIS_DB: f32 = 4.0;

/// A noise gate: silences a strip while its level is below a threshold, for
/// example the room noise from a microphone while nobody is talking.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Gate {
    /// Whether the gate works on the strip.
    pub enabled: bool,
    /// The gate opens when the level rises above this, from -90 to 0 dB;
    /// default -45.
    pub threshold_db: f32,
    /// How much quieter the strip gets while the gate is closed, from -90
    /// to 0 dB; default -90. -90 dB is effectively silence; something like
    /// -20 dB only turns noise down.
    pub range_db: f32,
    /// How quickly the gate opens once the level crosses the threshold, from
    /// 0.1 to 100 ms; default 2.
    pub attack_ms: f32,
    /// How long the gate stays open after the level drops, so it does not
    /// close in the gaps between words, from 0 to 2000 ms; default 200.
    pub hold_ms: f32,
    /// How gradually the gate closes once the hold time is over, from 5 to
    /// 3000 ms; default 150.
    pub release_ms: f32,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -45.0,
            range_db: -90.0,
            attack_ms: 2.0,
            hold_ms: 200.0,
            release_ms: 150.0,
        }
    }
}

impl Gate {
    /// Whether every setting is at its default.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Bring every setting into range. Returns whether anything changed.
    pub fn normalize(&mut self) -> bool {
        let before = *self;
        let d = Self::default();
        self.threshold_db = finite_or(self.threshold_db, d.threshold_db)
            .clamp(GATE_THRESHOLD_MIN_DB, GATE_THRESHOLD_MAX_DB);
        self.range_db =
            finite_or(self.range_db, d.range_db).clamp(GATE_RANGE_MIN_DB, GATE_RANGE_MAX_DB);
        self.attack_ms = finite_or(self.attack_ms, d.attack_ms).clamp(0.1, 100.0);
        self.hold_ms = finite_or(self.hold_ms, d.hold_ms).clamp(0.0, 2000.0);
        self.release_ms = finite_or(self.release_ms, d.release_ms).clamp(5.0, 3000.0);
        *self != before
    }
}

/// Speech noise suppression (RNNoise): removes steady background noise such
/// as fans, hiss and keyboard clatter from a voice. It is trained on speech,
/// so it also mangles music; use it on microphones.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Denoise {
    /// Whether noise suppression works on the strip.
    pub enabled: bool,
    /// From 0 to 1; default 1, full suppression. Lower values mix some of
    /// the original back in, which sounds more natural at the cost of
    /// leaving some noise.
    pub amount: f32,
}

impl Default for Denoise {
    fn default() -> Self {
        Self {
            enabled: false,
            amount: 1.0,
        }
    }
}

impl Denoise {
    /// Noise suppression only works at this sample rate. At any other rate
    /// the engine leaves the audio untouched.
    pub const SAMPLE_RATE: u32 = 48_000;

    /// Whether every setting is at its default.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Bring the amount into range. Returns whether it changed.
    pub fn normalize(&mut self) -> bool {
        let before = *self;
        self.amount = finite_or(self.amount, 1.0).clamp(0.0, 1.0);
        *self != before
    }
}

/// A strip's compressor: turns the strip down while it is louder than a
/// threshold, which evens out a voice that is sometimes quiet and sometimes
/// loud. The lift (makeup gain) then brings the whole thing back up.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Compressor {
    /// Whether the compressor works on the strip.
    pub enabled: bool,
    /// Level above which the compressor starts turning the strip down, from
    /// -60 to 0 dB; default -24.
    pub threshold_db: f32,
    /// How much it turns down above the threshold: at 4, every 4 dB over
    /// the threshold comes out as 1 dB over. From 1 (nothing) to 20;
    /// default 4.
    pub ratio: f32,
    /// How quickly it turns down when the level goes over, from 0.1 to
    /// 200 ms; default 10.
    pub attack_ms: f32,
    /// How quickly it lets go once the level drops back, from 10 to
    /// 3000 ms; default 150.
    pub release_ms: f32,
    /// Gain added after compressing, from 0 to 24 dB; default 0. Ignored
    /// while `auto_makeup` is on.
    pub makeup_db: f32,
    /// Work the lift out from the threshold and ratio; on by default. See
    /// [`Compressor::makeup`].
    pub auto_makeup: bool,
}

/// Lowest threshold a compressor can have.
pub const COMP_THRESHOLD_MIN_DB: f32 = -60.0;
/// Highest threshold a compressor can have.
pub const COMP_THRESHOLD_MAX_DB: f32 = 0.0;
/// Highest ratio a compressor can have.
pub const COMP_RATIO_MAX: f32 = 20.0;
/// Most lift a compressor can add.
pub const COMP_MAKEUP_MAX_DB: f32 = 24.0;
/// Width of the soft knee around the threshold, in dB: the compressor eases
/// in over this range rather than starting abruptly.
pub const COMP_KNEE_DB: f32 = 6.0;

impl Default for Compressor {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -24.0,
            ratio: 4.0,
            attack_ms: 10.0,
            release_ms: 150.0,
            makeup_db: 0.0,
            auto_makeup: true,
        }
    }
}

impl Compressor {
    /// Whether every setting is at its default.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Bring every setting into range. Returns whether anything changed.
    pub fn normalize(&mut self) -> bool {
        let before = *self;
        let d = Self::default();
        self.threshold_db = finite_or(self.threshold_db, d.threshold_db)
            .clamp(COMP_THRESHOLD_MIN_DB, COMP_THRESHOLD_MAX_DB);
        self.ratio = finite_or(self.ratio, d.ratio).clamp(1.0, COMP_RATIO_MAX);
        self.attack_ms = finite_or(self.attack_ms, d.attack_ms).clamp(0.1, 200.0);
        self.release_ms = finite_or(self.release_ms, d.release_ms).clamp(10.0, 3000.0);
        self.makeup_db = finite_or(self.makeup_db, d.makeup_db).clamp(0.0, COMP_MAKEUP_MAX_DB);
        *self != before
    }

    /// The lift actually applied, in dB. The automatic one makes up half of
    /// what a full scale signal is turned down by, which keeps the strip at
    /// about the same loudness as without the compressor.
    pub fn makeup(&self) -> f32 {
        if self.auto_makeup {
            let lift = -self.threshold_db * (1.0 - 1.0 / self.ratio.max(1.0)) * 0.5;
            (lift * 2.0).round() / 2.0
        } else {
            self.makeup_db
        }
    }

    /// How far a steady level of `input_db` is turned down, in dB (0 or
    /// less), before the lift: the compressor's curve, soft knee included.
    /// The engine uses exactly this, so it is also what to draw.
    pub fn reduction_db(&self, input_db: f32) -> f32 {
        let over = input_db - self.threshold_db;
        let slope = 1.0 / self.ratio.max(1.0) - 1.0;
        let half = COMP_KNEE_DB / 2.0;
        if over <= -half {
            0.0
        } else if over < half {
            slope * (over + half) * (over + half) / (2.0 * COMP_KNEE_DB)
        } else {
            slope * over
        }
    }

    /// The level that comes out for a steady `input_db`, lift included.
    pub fn output_db(&self, input_db: f32) -> f32 {
        input_db + self.reduction_db(input_db) + self.makeup()
    }
}

/// Ducking: turn a strip down while other strips are heard, such as music
/// while you talk. Set on the strip that gets turned down.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Ducking {
    /// Whether ducking works on the strip.
    pub enabled: bool,
    /// The strips whose sound turns this one down.
    pub triggers: BTreeSet<StripId>,
    /// How far to turn it down, in dB (a positive number), from 0 to 60;
    /// default 12.
    pub amount_db: f32,
    /// The buses whose mixes it is turned down in. Empty means every bus.
    pub buses: BTreeSet<BusId>,
    /// A trigger strip counts as heard while it is louder than this, after
    /// its fader, from -80 to 0 dB; default -40. A trigger with its gate on
    /// must also have the gate open.
    pub threshold_db: f32,
    /// How quickly it goes down once a trigger is heard, from 1 to 2000 ms;
    /// default 50.
    pub attack_ms: f32,
    /// How long it stays down after the triggers go quiet, so it does not
    /// bob up between words, from 0 to 5000 ms; default 300.
    pub hold_ms: f32,
    /// How gradually it comes back up after that, from 10 to 10000 ms;
    /// default 800.
    pub release_ms: f32,
}

/// The most ducking can turn a strip down.
pub const DUCK_AMOUNT_MAX_DB: f32 = 60.0;
/// Lowest threshold ducking can have.
pub const DUCK_THRESHOLD_MIN_DB: f32 = -80.0;
/// Highest threshold ducking can have.
pub const DUCK_THRESHOLD_MAX_DB: f32 = 0.0;

impl Default for Ducking {
    fn default() -> Self {
        Self {
            enabled: false,
            triggers: BTreeSet::new(),
            amount_db: 12.0,
            buses: BTreeSet::new(),
            threshold_db: -40.0,
            attack_ms: 50.0,
            hold_ms: 300.0,
            release_ms: 800.0,
        }
    }
}

impl Ducking {
    /// Whether every setting is at its default.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether it turns things down in `bus`'s mix.
    pub fn applies_to(&self, bus: BusId) -> bool {
        self.buses.is_empty() || self.buses.contains(&bus)
    }

    /// Clamp the numbers into range. Which strips and buses exist is
    /// checked by [`crate::MixerState::normalize`].
    pub fn normalize(&mut self) -> bool {
        let before = self.clone();
        let d = Self::default();
        self.amount_db = finite_or(self.amount_db, d.amount_db).clamp(0.0, DUCK_AMOUNT_MAX_DB);
        self.threshold_db = finite_or(self.threshold_db, d.threshold_db)
            .clamp(DUCK_THRESHOLD_MIN_DB, DUCK_THRESHOLD_MAX_DB);
        self.attack_ms = finite_or(self.attack_ms, d.attack_ms).clamp(1.0, 2000.0);
        self.hold_ms = finite_or(self.hold_ms, d.hold_ms).clamp(0.0, 5000.0);
        self.release_ms = finite_or(self.release_ms, d.release_ms).clamp(10.0, 10000.0);
        *self != before
    }
}

/// A bus's safety limiter: the last thing before the output, it turns the
/// mix down just enough that it never goes over the ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Limiter {
    /// Whether the limiter works on the bus. New virtual buses start with
    /// it on.
    pub enabled: bool,
    /// The level the output never goes over, from -60 to 12 dB; default -1.
    /// 0 dB and below is what protects against clipping.
    pub ceiling_db: f32,
    /// How long the level takes to come (99% of the way) back up after a
    /// peak, from 10 to 2000 ms; default 300.
    pub release_ms: f32,
}

impl Default for Limiter {
    fn default() -> Self {
        Self {
            enabled: false,
            ceiling_db: -1.0,
            release_ms: 300.0,
        }
    }
}

impl Limiter {
    /// On, at the default ceiling: what a new virtual bus starts with.
    pub fn on() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    /// Whether every setting is at its default.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Bring every setting into range. Returns whether anything changed.
    pub fn normalize(&mut self) -> bool {
        let before = *self;
        let d = Self::default();
        self.ceiling_db = finite_or(self.ceiling_db, d.ceiling_db).clamp(GAIN_MIN_DB, GAIN_MAX_DB);
        self.release_ms = finite_or(self.release_ms, d.release_ms).clamp(10.0, 2000.0);
        *self != before
    }
}

/// Where in a strip's or bus's chain its external effects go: the stage the
/// sound goes on to after coming back.
///
/// A strip's chain is noise suppression, gate, equalizer, compressor,
/// fader; after the fader it splits into the mixes of the buses it plays
/// in, so `after_fader` is as late as a strip's external effects can go. A
/// bus's chain is its mix, equalizer, fader, limiter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InsertPoint {
    /// Strips: first of all, before noise suppression.
    BeforeDenoise,
    /// Strips: before the gate.
    BeforeGate,
    /// Strips and buses: before the equalizer. For a bus, straight after
    /// its mix.
    BeforeEq,
    /// Strips: before the compressor.
    BeforeCompressor,
    /// Strips and buses: before the fader, after every effect of a strip
    /// or the equalizer of a bus.
    #[default]
    BeforeFader,
    /// Strips: after the fader, so the effects hear the strip as loud as it
    /// is in the mix.
    AfterFader,
    /// Buses: after the fader, before the limiter.
    BeforeLimiter,
    /// Buses: last of all, after the limiter.
    AfterLimiter,
}

impl InsertPoint {
    /// Where a strip's external effects can go, in chain order.
    pub const STRIP: [InsertPoint; 6] = [
        Self::BeforeDenoise,
        Self::BeforeGate,
        Self::BeforeEq,
        Self::BeforeCompressor,
        Self::BeforeFader,
        Self::AfterFader,
    ];

    /// Where a bus's external effects can go, in chain order.
    pub const BUS: [InsertPoint; 4] = [
        Self::BeforeEq,
        Self::BeforeFader,
        Self::BeforeLimiter,
        Self::AfterLimiter,
    ];

    /// The nearest place a strip has: both of a bus's places past its fader
    /// are a strip's `after_fader`.
    pub fn for_strip(self) -> Self {
        match self {
            Self::BeforeLimiter | Self::AfterLimiter => Self::AfterFader,
            p => p,
        }
    }

    /// The nearest place a bus has. A bus's mix comes first, so a strip's
    /// places before its equalizer are a bus's `before_eq`; a strip's
    /// compressor sits just before its fader.
    pub fn for_bus(self) -> Self {
        match self {
            Self::BeforeDenoise | Self::BeforeGate => Self::BeforeEq,
            Self::BeforeCompressor => Self::BeforeFader,
            Self::AfterFader => Self::BeforeLimiter,
            p => p,
        }
    }

    /// What it is called, for people.
    pub fn label(self) -> &'static str {
        match self {
            Self::BeforeDenoise => "Before noise suppression",
            Self::BeforeGate => "Before the gate",
            Self::BeforeEq => "Before the equalizer",
            Self::BeforeCompressor => "Before the compressor",
            Self::BeforeFader => "Before the fader",
            Self::AfterFader => "After the fader",
            Self::BeforeLimiter => "Before the limiter",
            Self::AfterLimiter => "After the limiter",
        }
    }
}

/// What a strip's or bus's external effects do while nothing plays into
/// its "back from effects" device.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InsertFallback {
    /// Carry on with the sound as it went out, unchanged, so the strip or
    /// bus is heard even before the effects program is running.
    #[default]
    PassThrough,
    /// Silence, so nothing is heard without its effects.
    Silence,
}

impl InsertFallback {
    /// Both, for menus.
    pub const ALL: [InsertFallback; 2] = [Self::PassThrough, Self::Silence];

    /// What it is called, for people.
    pub fn label(self) -> &'static str {
        match self {
            Self::PassThrough => "Pass the sound through",
            Self::Silence => "Silence",
        }
    }
}

/// External effects: a point in a strip's or bus's chain where its sound
/// leaves Weir for another program, such as Carla or EasyEffects, and comes
/// back.
///
/// While on, two devices exist for it: "*name*: to effects (Weir)", which
/// that program records from, and "*name*: back from effects (Weir)", which
/// it plays into. The round trip costs one PipeWire cycle, a few
/// milliseconds, on top of whatever the effects take.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Insert {
    /// Whether the sound goes out and back. Switching it on or off creates
    /// or removes the two devices.
    pub enabled: bool,
    /// Where in the chain.
    pub position: InsertPoint,
    /// What happens while nothing plays into "back from effects".
    pub fallback: InsertFallback,
}

impl Insert {
    /// Whether every setting is at its default.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// A named set of equalizer bands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EqPreset {
    /// Unique among presets, ignoring case; 40 characters at most.
    pub name: String,
    /// The bands it sets.
    #[serde(default)]
    pub bands: Vec<EqBand>,
    /// Built-in presets ship with Weir and cannot be changed or
    /// deleted.
    #[serde(default)]
    pub builtin: bool,
}

/// The presets that ship with Weir.
pub fn builtin_eq_presets() -> Vec<EqPreset> {
    use EqBandKind::*;
    let q = Q_BUTTERWORTH;
    let preset = |name: &str, bands: Vec<EqBand>| EqPreset {
        name: name.into(),
        bands,
        builtin: true,
    };
    vec![
        preset(
            "Flat (5 bands)",
            vec![
                EqBand::new(LowShelf, 100.0, 0.0, q),
                EqBand::new(Peak, 300.0, 0.0, 1.0),
                EqBand::new(Peak, 1000.0, 0.0, 1.0),
                EqBand::new(Peak, 3500.0, 0.0, 1.0),
                EqBand::new(HighShelf, 8000.0, 0.0, q),
            ],
        ),
        preset(
            "Voice: clarity",
            vec![
                EqBand::new(HighPass, 80.0, 0.0, q),
                EqBand::new(Peak, 250.0, -3.0, 1.2),
                EqBand::new(Peak, 3500.0, 3.0, 1.0),
                EqBand::new(HighShelf, 10000.0, 2.0, q),
            ],
        ),
        preset(
            "Voice: broadcast",
            vec![
                EqBand::new(HighPass, 70.0, 0.0, q),
                EqBand::new(Peak, 150.0, 2.5, 1.0),
                EqBand::new(Peak, 400.0, -3.0, 1.4),
                EqBand::new(Peak, 3000.0, 2.5, 0.9),
                EqBand::new(Peak, 6500.0, -2.0, 3.0),
                EqBand::new(HighShelf, 12000.0, 1.5, q),
            ],
        ),
        preset(
            "Voice: remove rumble",
            vec![EqBand::new(HighPass, 100.0, 0.0, q)],
        ),
        preset("Voice: de-ess", vec![EqBand::new(Peak, 6500.0, -5.0, 3.0)]),
        preset(
            "Mains hum 50 Hz",
            vec![
                EqBand::new(Notch, 50.0, 0.0, 10.0),
                EqBand::new(Notch, 100.0, 0.0, 10.0),
                EqBand::new(Notch, 150.0, 0.0, 10.0),
            ],
        ),
        preset(
            "Mains hum 60 Hz",
            vec![
                EqBand::new(Notch, 60.0, 0.0, 10.0),
                EqBand::new(Notch, 120.0, 0.0, 10.0),
                EqBand::new(Notch, 180.0, 0.0, 10.0),
            ],
        ),
        preset("Bass boost", vec![EqBand::new(LowShelf, 110.0, 6.0, q)]),
        preset("Bass cut", vec![EqBand::new(LowShelf, 150.0, -6.0, q)]),
        preset("Treble boost", vec![EqBand::new(HighShelf, 6000.0, 5.0, q)]),
        preset(
            "Treble cut (less hiss)",
            vec![EqBand::new(HighShelf, 6000.0, -6.0, q)],
        ),
        preset(
            "Loudness (quiet listening)",
            vec![
                EqBand::new(LowShelf, 90.0, 6.0, q),
                EqBand::new(Peak, 3000.0, -1.5, 0.8),
                EqBand::new(HighShelf, 9000.0, 4.0, q),
            ],
        ),
        preset(
            "Games: footsteps",
            vec![
                EqBand::new(LowShelf, 200.0, -4.0, q),
                EqBand::new(Peak, 2500.0, 4.0, 0.8),
                EqBand::new(Peak, 5000.0, 2.0, 1.2),
            ],
        ),
        preset(
            "Telephone",
            vec![
                EqBand::new(HighPass, 400.0, 0.0, q),
                EqBand::new(LowPass, 3200.0, 0.0, q),
                EqBand::new(Peak, 1500.0, 4.0, 1.0),
            ],
        ),
    ]
}

/// How many points a spectrum has, spaced evenly in octaves from
/// [`EQ_FREQ_MIN_HZ`] to [`EQ_FREQ_MAX_HZ`].
pub const SPECTRUM_POINTS: usize = 192;
/// Level reported for silence in a spectrum.
pub const SPECTRUM_FLOOR_DB: f32 = -120.0;
/// Spectra rise by this much per octave, pivoting at 1 kHz, as most
/// analyzers do. Music and speech carry less energy the higher you go, so
/// without it everything slopes down to the right; with it, pink noise draws
/// flat and the treble is readable.
pub const SPECTRUM_TILT_DB_PER_OCTAVE: f32 = 4.5;

/// The frequency of point `i` of a spectrum, in Hz.
pub fn spectrum_freq(i: usize) -> f32 {
    let t = i as f32 / (SPECTRUM_POINTS - 1) as f32;
    EQ_FREQ_MIN_HZ * (EQ_FREQ_MAX_HZ / EQ_FREQ_MIN_HZ).powf(t)
}

/// What an equalizer is doing to the audio right now: the spectrum of what
/// goes into it and of what comes out, at [`spectrum_freq`] each.
///
/// For a strip, the input is the audio after noise suppression and the
/// gate. Both are before the fader. Values are dB relative to full scale for
/// a sine, plus the tilt of [`SPECTRUM_TILT_DB_PER_OCTAVE`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Spectrum {
    /// The strip or bus whose equalizer this is.
    pub target: crate::model::StripOrBus,
    /// What goes into the equalizer, [`SPECTRUM_POINTS`] levels.
    #[serde(serialize_with = "tenths")]
    pub input_db: Vec<f32>,
    /// What comes out of it, [`SPECTRUM_POINTS`] levels.
    #[serde(serialize_with = "tenths")]
    pub output_db: Vec<f32>,
}

/// Write levels to a tenth of a dB. Without this, JSON gets the exact value
/// of the nearest `f32` as a double (-54.70000076293945 for -54.7), which
/// doubles the size of a message sent 30 times a second.
fn tenths<S: serde::Serializer>(levels: &[f32], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(levels.iter().map(|&v| (v as f64 * 10.0).round() / 10.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f32 = 48_000.0;

    fn near(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn bell_hits_its_gain_at_the_center() {
        for gain in [-12.0, -3.0, 3.0, 12.0] {
            let b = EqBand::new(EqBandKind::Peak, 1000.0, gain, 1.0);
            assert!(near(b.response_db(1000.0, FS), gain, 0.01), "{gain}");
            assert!(near(b.response_db(30.0, FS), 0.0, 0.2));
            assert!(near(b.response_db(15000.0, FS), 0.0, 0.2));
        }
    }

    #[test]
    fn shelves_reach_their_gain_far_from_the_corner() {
        let low = EqBand::new(EqBandKind::LowShelf, 200.0, 6.0, Q_BUTTERWORTH);
        assert!(near(low.response_db(20.0, FS), 6.0, 0.1));
        assert!(near(low.response_db(10000.0, FS), 0.0, 0.1));
        // Half the gain at the corner.
        assert!(near(low.response_db(200.0, FS), 3.0, 0.1));
        let high = EqBand::new(EqBandKind::HighShelf, 4000.0, -8.0, Q_BUTTERWORTH);
        assert!(near(high.response_db(18000.0, FS), -8.0, 0.3));
        assert!(near(high.response_db(50.0, FS), 0.0, 0.1));
    }

    #[test]
    fn cut_filters_are_3db_down_at_the_corner() {
        let hp = EqBand::new(EqBandKind::HighPass, 100.0, 0.0, Q_BUTTERWORTH);
        assert!(near(hp.response_db(100.0, FS), -3.01, 0.05));
        assert!(hp.response_db(25.0, FS) < -23.0);
        let lp = EqBand::new(EqBandKind::LowPass, 5000.0, 0.0, Q_BUTTERWORTH);
        assert!(near(lp.response_db(5000.0, FS), -3.01, 0.05));
        assert!(near(lp.response_db(100.0, FS), 0.0, 0.01));
    }

    #[test]
    fn notch_removes_its_frequency() {
        let n = EqBand::new(EqBandKind::Notch, 60.0, 0.0, 10.0);
        assert!(n.response_db(60.0, FS) < -60.0);
        assert!(near(n.response_db(1000.0, FS), 0.0, 0.01));
    }

    #[test]
    fn the_filter_does_what_the_curve_says() {
        // Run a sine through the actual filter and compare its level with
        // the drawn response.
        let b = EqBand::new(EqBandKind::Peak, 2000.0, 9.0, 2.0);
        let c = b.coefs(FS);
        for freq in [200.0f32, 1500.0, 2000.0, 3000.0, 9000.0] {
            let mut st = [0.0f32; 2];
            let mut peak = 0.0f32;
            for i in 0..48_000 {
                let x = (2.0 * std::f32::consts::PI * freq * i as f32 / FS).sin();
                let y = c.tick(&mut st, x);
                if i > 24_000 {
                    peak = peak.max(y.abs());
                }
            }
            let measured = 20.0 * peak.log10();
            let drawn = b.response_db(freq, FS);
            assert!(near(measured, drawn, 0.05), "{freq}: {measured} vs {drawn}");
        }
    }

    #[test]
    fn combined_response_adds_up() {
        let eq = Equalizer {
            enabled: true,
            bands: vec![
                EqBand::new(EqBandKind::Peak, 1000.0, 6.0, 1.0),
                EqBand::new(EqBandKind::Peak, 1000.0, -6.0, 1.0),
            ],
        };
        // A bell and the same bell with the opposite gain cancel exactly.
        for f in [50.0, 700.0, 1000.0, 1400.0, 12000.0] {
            assert!(near(eq.response_db(f, FS), 0.0, 1e-3), "{f}");
        }
        let flat = Equalizer::default();
        assert_eq!(flat.response_db(1000.0, FS), 0.0);
    }

    #[test]
    fn zero_gain_bells_are_inactive() {
        let mut b = EqBand::new(EqBandKind::Peak, 1000.0, 0.0, 1.0);
        assert!(!b.is_active());
        b.gain_db = 1.0;
        assert!(b.is_active());
        let hp = EqBand::new(EqBandKind::HighPass, 80.0, 0.0, Q_BUTTERWORTH);
        assert!(hp.is_active());
    }

    #[test]
    fn nonsense_is_clamped() {
        let b = EqBand::new(EqBandKind::Peak, f32::NAN, 99.0, 0.0).clamped();
        assert_eq!(b.freq_hz, 1000.0);
        assert_eq!(b.gain_db, EQ_GAIN_MAX_DB);
        assert_eq!(b.q, EQ_Q_MIN);
        let mut g = Gate {
            threshold_db: 20.0,
            release_ms: 0.0,
            ..Gate::default()
        };
        assert!(g.normalize());
        assert_eq!(g.threshold_db, 0.0);
        assert_eq!(g.release_ms, 5.0);
    }

    #[test]
    fn serde_defaults_keep_old_configs_loading() {
        let b: EqBand = serde_json::from_str(r#"{"freq_hz": 440}"#).unwrap();
        assert_eq!(b.kind, EqBandKind::Peak);
        assert!(b.enabled);
        assert_eq!(b.q, Q_BUTTERWORTH);
        let g: Gate = serde_json::from_str(r#"{"enabled": true}"#).unwrap();
        assert_eq!(g.threshold_db, Gate::default().threshold_db);
        assert_eq!(
            serde_json::to_string(&EqBandKind::HighShelf).unwrap(),
            "\"high_shelf\""
        );
    }

    #[test]
    fn spectra_are_sent_compactly() {
        let s = Spectrum {
            target: crate::model::StripOrBus::Bus(1),
            input_db: vec![-54.7, -120.0],
            output_db: vec![3.25],
        };
        let json = serde_json::to_value(&s).unwrap().to_string();
        assert_eq!(
            json,
            r#"{"input_db":[-54.7,-120.0],"output_db":[3.3],"target":{"bus":1}}"#
        );
        let back: Spectrum = serde_json::from_str(&json).unwrap();
        assert_eq!(back.input_db, vec![-54.7, -120.0]);
    }

    #[test]
    fn spectrum_points_span_the_audible_range() {
        assert_eq!(spectrum_freq(0), EQ_FREQ_MIN_HZ);
        assert!((spectrum_freq(SPECTRUM_POINTS - 1) - EQ_FREQ_MAX_HZ).abs() < 0.5);
        assert!((1..SPECTRUM_POINTS).all(|i| spectrum_freq(i) > spectrum_freq(i - 1)));
    }

    #[test]
    fn builtin_presets_are_valid() {
        let presets = builtin_eq_presets();
        let mut names = std::collections::BTreeSet::new();
        for p in &presets {
            assert!(names.insert(p.name.clone()), "duplicate {}", p.name);
            assert!(p.bands.len() <= EQ_MAX_BANDS);
            for b in &p.bands {
                assert_eq!(*b, b.clamped(), "{}", p.name);
            }
        }
    }

    #[test]
    fn the_compressor_curve_is_continuous_and_has_the_right_slope() {
        let c = Compressor {
            threshold_db: -20.0,
            ratio: 4.0,
            ..Compressor::default()
        };
        // Untouched well below the threshold.
        assert_eq!(c.reduction_db(-40.0), 0.0);
        // Continuous at both edges of the knee.
        for edge in [-23.0f32, -17.0] {
            let below = c.reduction_db(edge - 1e-4);
            let above = c.reduction_db(edge + 1e-4);
            assert!((below - above).abs() < 1e-3, "{edge}: {below} {above}");
        }
        // Above the knee, 4 dB in makes 1 dB out.
        let out = |x: f32| x + c.reduction_db(x);
        assert!(((out(-4.0) - out(-8.0)) - 1.0).abs() < 1e-4);
        assert!((out(0.0) - (-15.0)).abs() < 1e-4);
    }

    #[test]
    fn automatic_lift_makes_up_half_the_reduction_at_full_scale() {
        let mut c = Compressor {
            threshold_db: -24.0,
            ratio: 4.0,
            makeup_db: 3.0,
            ..Compressor::default()
        };
        assert_eq!(c.makeup(), 9.0);
        c.auto_makeup = false;
        assert_eq!(c.makeup(), 3.0);
        assert_eq!(c.output_db(-60.0), -57.0);
    }

    #[test]
    fn compressor_settings_are_kept_in_range() {
        let mut c = Compressor {
            ratio: 0.5,
            threshold_db: f32::NAN,
            makeup_db: 99.0,
            ..Compressor::default()
        };
        assert!(c.normalize());
        assert_eq!(c.ratio, 1.0);
        assert_eq!(c.threshold_db, Compressor::default().threshold_db);
        assert_eq!(c.makeup_db, COMP_MAKEUP_MAX_DB);
    }
}
