//! Spectrum analysis of the equalizer taps, for drawing behind the curve.
//!
//! Runs on the control thread, never the real-time one: it reads the newest
//! samples from a [`Tap`], windows them, runs an FFT and folds the bins into
//! the fixed set of points in [`spectrum_freq`], with a peak display that
//! rises at once and falls away gently.

use super::fx::Tap;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Instant;
use weir_protocol::{
    spectrum_freq, StripOrBus, SPECTRUM_FLOOR_DB, SPECTRUM_POINTS, SPECTRUM_TILT_DB_PER_OCTAVE,
};

/// Samples per analysis: 170 ms at 48 kHz, which resolves about 6 Hz, fine
/// enough to tell 50 Hz from 60 Hz hum.
pub const FFT_LEN: usize = 8192;
/// How quickly a displayed level falls once the sound behind it stops.
const FALL_DB_PER_SECOND: f32 = 45.0;

/// What one tap has been showing.
struct Trace {
    /// The levels being shown, at each of the spectrum's points.
    display: Vec<f32>,
    /// How many samples the tap had written when it was last analyzed.
    written: usize,
    /// When that was.
    at: Instant,
}

/// Turns taps into spectra. Holds the FFT and its buffers, so nothing is
/// allocated per analysis, and what each tap has been showing.
pub struct Analyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    /// The Hann window.
    window: Vec<f32>,
    /// The samples being analyzed.
    frame: Vec<f32>,
    /// What the FFT made of them.
    bins: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Each bin's level in dB.
    bin_db: Vec<f32>,
    /// The bins folded into the spectrum's points.
    points: Vec<f32>,
    /// Keyed by target and whether it is the output tap.
    traces: HashMap<(StripOrBus, bool), Trace>,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    /// An analyzer with its FFT planned and its buffers allocated.
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_LEN);
        // Hann window. Its leakage falls off fast enough that a loud bass
        // note does not smear across the whole display.
        let window = (0..FFT_LEN)
            .map(|k| {
                let x = std::f32::consts::TAU * k as f32 / FFT_LEN as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        Self {
            frame: fft.make_input_vec(),
            bins: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            bin_db: vec![SPECTRUM_FLOOR_DB; FFT_LEN / 2 + 1],
            points: vec![SPECTRUM_FLOOR_DB; SPECTRUM_POINTS],
            fft,
            window,
            traces: HashMap::new(),
        }
    }

    /// Forget targets nobody is watching any more.
    pub fn retain(&mut self, targets: &BTreeSet<StripOrBus>) {
        self.traces.retain(|(t, _), _| targets.contains(t));
    }

    /// The spectrum of `tap` as it should be drawn now.
    pub fn analyze(
        &mut self,
        target: StripOrBus,
        output: bool,
        tap: &Tap,
        rate: u32,
        now: Instant,
    ) -> Vec<f32> {
        let written = tap.written();
        let trace = self
            .traces
            .entry((target, output))
            .or_insert_with(|| Trace {
                display: vec![SPECTRUM_FLOOR_DB; SPECTRUM_POINTS],
                // Anything but the current count, so the first look analyzes
                // what is there.
                written: written.wrapping_add(1),
                at: now,
            });
        // Nothing new since last time means the audio stopped arriving (a
        // silent strip is skipped entirely), so let the display fall away
        // instead of freezing on the last thing heard.
        if written != trace.written {
            tap.read_latest(&mut self.frame);
            for (x, w) in self.frame.iter_mut().zip(&self.window) {
                *x *= w;
            }
            if self
                .fft
                .process_with_scratch(&mut self.frame, &mut self.bins, &mut self.scratch)
                .is_ok()
            {
                // A full scale sine lands at 0 dB: the window halves the
                // amplitude and a real FFT puts half of it in each of the
                // positive and negative bins.
                let scale = 4.0 / FFT_LEN as f32;
                for (db, c) in self.bin_db.iter_mut().zip(&self.bins) {
                    let amp = c.norm() * scale;
                    *db = if amp > 0.0 {
                        (20.0 * amp.log10()).max(SPECTRUM_FLOOR_DB)
                    } else {
                        SPECTRUM_FLOOR_DB
                    };
                }
                bins_to_points(&self.bin_db, rate.max(8000) as f32, &mut self.points);
            }
        } else {
            self.points.fill(SPECTRUM_FLOOR_DB);
        }
        let dt = now.duration_since(trace.at).as_secs_f32().min(0.5);
        let fall = FALL_DB_PER_SECOND * dt;
        for (d, &p) in trace.display.iter_mut().zip(&self.points) {
            *d = p.max(*d - fall).max(SPECTRUM_FLOOR_DB);
        }
        trace.written = written;
        trace.at = now;
        trace.display.clone()
    }
}

/// Fold FFT bins (in dB) into the display points: the loudest bin within
/// each point's share of the spectrum or, where points are closer together
/// than bins (the bass), a value interpolated from the nearest three.
fn bins_to_points(bin_db: &[f32], rate: f32, out: &mut [f32]) {
    let last_bin = bin_db.len() - 1;
    let bin_hz = rate / FFT_LEN as f32;
    for (i, o) in out.iter_mut().enumerate() {
        let f = spectrum_freq(i);
        if f >= rate * 0.5 {
            *o = SPECTRUM_FLOOR_DB;
            continue;
        }
        let lo = if i == 0 {
            f
        } else {
            (spectrum_freq(i - 1) * f).sqrt()
        };
        let hi = if i + 1 == SPECTRUM_POINTS {
            f
        } else {
            (spectrum_freq(i + 1) * f).sqrt()
        };
        let (from, to) = (lo / bin_hz, hi / bin_hz);
        let db = if to - from >= 1.0 {
            let a = (from.ceil() as usize).min(last_bin);
            let b = (to.floor() as usize).clamp(a, last_bin);
            bin_db[a..=b]
                .iter()
                .copied()
                .fold(SPECTRUM_FLOOR_DB, f32::max)
        } else {
            // A parabola through the nearest bin and its neighbors. A
            // straight line between bins would cut the top off any tone that
            // falls between two of them, and in the bass that is most tones.
            let x = f / bin_hz;
            let k = (x.round() as usize).clamp(1, last_bin - 1);
            let d = x - k as f32;
            let (a, b, c) = (bin_db[k - 1], bin_db[k], bin_db[k + 1]);
            let y = b + 0.5 * (c - a) * d + 0.5 * (a - 2.0 * b + c) * d * d;
            y.clamp(a.min(b).min(c), a.max(b).max(c) + 3.0)
        };
        // Silence stays at the floor rather than being tilted off it.
        *o = if db <= SPECTRUM_FLOOR_DB {
            SPECTRUM_FLOOR_DB
        } else {
            let tilt = SPECTRUM_TILT_DB_PER_OCTAVE * (f / 1000.0).log2();
            (db + tilt).max(SPECTRUM_FLOOR_DB)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tap holding a sine, the way the real-time thread would fill it.
    fn sine_tap(freq: f32, amp: f32, rate: f32) -> Tap {
        let tap = Tap::new();
        let samples: Vec<f32> = (0..FFT_LEN * 2)
            .map(|k| amp * (std::f32::consts::TAU * freq * k as f32 / rate).sin())
            .collect();
        tap.write(&samples);
        tap
    }

    fn nearest_point(freq: f32) -> usize {
        (0..SPECTRUM_POINTS)
            .min_by(|&a, &b| {
                let d = |i: usize| (spectrum_freq(i) / freq).ln().abs();
                d(a).total_cmp(&d(b))
            })
            .unwrap()
    }

    #[test]
    fn a_sine_shows_up_at_its_frequency_and_level() {
        let mut an = Analyzer::new();
        let t = StripOrBus::Strip(1);
        for (freq, amp) in [(1000.0, 0.5), (100.0, 0.25), (5000.0, 0.1)] {
            let tap = sine_tap(freq, amp, 48_000.0);
            let spec = an.analyze(t, false, &tap, 48_000, Instant::now());
            // The loudest point near the tone. In the bass the points are
            // closer together than the FFT's bins, so the one nearest the
            // tone can sit on the side of its peak rather than on top.
            let near = (0..SPECTRUM_POINTS)
                .filter(|&i| (spectrum_freq(i) / freq - 1.0).abs() < 0.05)
                .map(|i| spec[i])
                .fold(SPECTRUM_FLOOR_DB, f32::max);
            let expected =
                20.0 * amp.log10() + SPECTRUM_TILT_DB_PER_OCTAVE * (freq / 1000.0).log2();
            // Hann scalloping costs up to 1.4 dB between bins.
            assert!(
                (near - expected).abs() < 1.6,
                "{freq} Hz: {near} vs {expected}"
            );
            // An octave away there is (nearly) nothing.
            let far = nearest_point(freq * 2.0);
            assert!(
                spec[far] < expected - 40.0,
                "{freq} Hz leaks: {}",
                spec[far]
            );
            an.traces.clear();
        }
    }

    #[test]
    fn the_display_falls_away_when_the_audio_stops() {
        let mut an = Analyzer::new();
        let t = StripOrBus::Bus(2);
        let tap = sine_tap(1000.0, 0.5, 48_000.0);
        let start = Instant::now();
        let first = an.analyze(t, true, &tap, 48_000, start);
        let i = nearest_point(1000.0);
        // Nothing new written: the level drops at the fall rate, not at once.
        let later = start + std::time::Duration::from_millis(200);
        let second = an.analyze(t, true, &tap, 48_000, later);
        let dropped = first[i] - second[i];
        assert!(
            (dropped - FALL_DB_PER_SECOND * 0.2).abs() < 0.2,
            "{dropped}"
        );
    }

    #[test]
    fn silence_reads_as_the_floor() {
        let mut an = Analyzer::new();
        let tap = Tap::new();
        tap.write(&vec![0.0; FFT_LEN]);
        let spec = an.analyze(StripOrBus::Strip(1), false, &tap, 48_000, Instant::now());
        assert!(spec.iter().all(|&d| d == SPECTRUM_FLOOR_DB));
    }
}
