//! A track folded into stereo for a recording format that holds two
//! channels at most (MP3), the way the mixer folds a strip into a stereo
//! bus: the same downmix, then, if wanted, the bus's safety limiter.
//!
//! This runs on the recorder's thread, not the audio one, so it may
//! allocate.

use super::fx::LimiterRt;
use super::mapping::channel_matrix;
use weir_protocol::{ChannelPosition, Downmix, Limiter, Upmix};

/// A wider track folded into stereo, sample for sample in step with the
/// tracks that are not.
pub struct StereoFold {
    /// Each input channel's share of the left and the right.
    weights: Vec<(f32, f32)>,
    /// The limiter, when wanted.
    limiter: Option<LimiterRt>,
    /// Its settings: the bus limiter's defaults, switched on.
    settings: Limiter,
    rate: u32,
    /// Output frames still to drop at the start: the limiter holds the
    /// sound back by its lookahead, and the track must not start late.
    skip: usize,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl StereoFold {
    /// Fold channels at `positions`, at `rate`, as a stereo bus with the
    /// standard downmix does: the fronts at full level, the center and the
    /// surrounds at -3 dB on their side, the subwoofer left out. With
    /// `limit`, the bus limiter at its defaults (ceiling -1 dB, release
    /// 300 ms) turns down the moments that would go over.
    pub fn new(positions: &[ChannelPosition], rate: u32, limit: bool) -> Self {
        use ChannelPosition::{FL, FR};
        let m = channel_matrix(positions, &[FL, FR], 0.0, Upmix::Off, &Downmix::default());
        let weights = m.as_chunks::<2>().0.iter().map(|&[l, r]| (l, r)).collect();
        Self {
            weights,
            limiter: limit.then(|| LimiterRt::new(2)),
            settings: Limiter {
                enabled: true,
                ..Limiter::default()
            },
            rate,
            skip: if limit { LimiterRt::lookahead(rate) } else { 0 },
            left: Vec::new(),
            right: Vec::new(),
        }
    }

    /// Each input channel's share of the left and the right.
    pub fn weights(&self) -> &[(f32, f32)] {
        &self.weights
    }

    /// Fold the whole frames of interleaved `samples`, adding stereo frames
    /// to `out`. With the limiter, the last few come out on the next call
    /// or from [`finish`](Self::finish).
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        let n = self.weights.len().max(1);
        self.left.clear();
        self.right.clear();
        for frame in samples.chunks_exact(n) {
            let (mut l, mut r) = (0.0, 0.0);
            for (x, (wl, wr)) in frame.iter().zip(&self.weights) {
                l += x * wl;
                r += x * wr;
            }
            self.left.push(l);
            self.right.push(r);
        }
        self.limit_into(out);
    }

    /// What the limiter still holds back, at the end of a file. The fold
    /// then starts afresh, as for the next file. Without the limiter there
    /// is nothing.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if self.limiter.is_none() {
            return;
        }
        let len = LimiterRt::lookahead(self.rate);
        self.left.clear();
        self.right.clear();
        self.left.resize(len, 0.0);
        self.right.resize(len, 0.0);
        self.limit_into(out);
        self.limiter = Some(LimiterRt::new(2));
        self.skip = len;
    }

    /// Run the limiter over what was folded, if there is one, and add it
    /// to `out` interleaved, less what is still to be skipped.
    fn limit_into(&mut self, out: &mut Vec<f32>) {
        let frames = self.left.len();
        if let Some(limiter) = &mut self.limiter {
            let mut channels: [&mut [f32]; 2] = [&mut self.left, &mut self.right];
            // A ramp of one sample: on from the very first, so nothing gets
            // past it undelayed while it fades in.
            limiter.run(&self.settings, self.rate, 1, &mut channels, frames);
        }
        let drop = self.skip.min(frames);
        self.skip -= drop;
        out.reserve((frames - drop) * 2);
        for k in drop..frames {
            out.push(self.left[k]);
            out.push(self.right[k]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_1_SQRT_2 as H, TAU};
    use ChannelPosition::*;

    const RATE: u32 = 48_000;

    /// `frames` of a sine per channel, each its own pitch, at `level`.
    fn tones(channels: usize, frames: usize, level: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|k| {
                (0..channels).map(move |c| {
                    let hz = 110.0 * (c + 1) as f32;
                    level * (TAU * hz * k as f32 / RATE as f32).sin()
                })
            })
            .collect()
    }

    /// Everything `fold` gives for `input`, pushed in odd-sized pieces.
    fn run(fold: &mut StereoFold, input: &[f32], channels: usize) -> Vec<f32> {
        let mut out = Vec::new();
        for piece in input.chunks(channels * 777) {
            fold.push(piece, &mut out);
        }
        fold.finish(&mut out);
        out
    }

    fn peak(v: &[f32]) -> f32 {
        v.iter().fold(0.0, |m, x| m.max(x.abs()))
    }

    #[test]
    fn surround_folds_as_a_stereo_bus_does() {
        let fold = StereoFold::new(&[FL, FR, FC, LFE, RL, RR], RATE, false);
        let want = [
            (1.0, 0.0),
            (0.0, 1.0),
            (H, H),
            (0.0, 0.0),
            (H, 0.0),
            (0.0, H),
        ];
        for (got, want) in fold.weights().iter().zip(want) {
            assert!((got.0 - want.0).abs() < 1e-6 && (got.1 - want.1).abs() < 1e-6);
        }
        // 7.1: side and rear share their side at -3 dB more each.
        let fold = StereoFold::new(&[FL, FR, FC, LFE, RL, RR, SL, SR], RATE, false);
        assert!((fold.weights()[4].0 - 0.5).abs() < 1e-6);
        assert!((fold.weights()[6].0 - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_limiter_keeps_the_length_and_leaves_quiet_sound_alone() {
        let layout = [FL, FR, FC, LFE, RL, RR];
        for frames in [100, 48_000 + 13] {
            let input = tones(6, frames, 0.2);
            let plain = run(&mut StereoFold::new(&layout, RATE, false), &input, 6);
            let limited = run(&mut StereoFold::new(&layout, RATE, true), &input, 6);
            assert_eq!(plain.len(), 2 * frames, "{frames} frames");
            assert_eq!(limited.len(), plain.len(), "{frames} frames");
            // Under the ceiling, the same samples at the same times.
            let worst = plain
                .iter()
                .zip(&limited)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            assert!(worst < 1e-6, "{frames} frames: off by {worst}");
        }
    }

    #[test]
    fn loud_surround_stays_under_the_ceiling_with_the_limiter() {
        let layout = [FL, FR, FC, LFE, RL, RR];
        // Every channel loud and in phase: the worst case for a fold.
        let input: Vec<f32> = (0..RATE as usize)
            .flat_map(|k| {
                let x = 0.9 * (TAU * 220.0 * k as f32 / RATE as f32).sin();
                [x; 6]
            })
            .collect();
        let plain = run(&mut StereoFold::new(&layout, RATE, false), &input, 6);
        let limited = run(&mut StereoFold::new(&layout, RATE, true), &input, 6);
        assert!(peak(&plain) > 1.9, "{}", peak(&plain));
        let ceiling = weir_protocol::db_to_linear(Limiter::default().ceiling_db);
        assert!(peak(&limited) <= ceiling + 1e-4, "{}", peak(&limited));
        assert_eq!(limited.len(), plain.len());
    }

    #[test]
    fn a_new_file_starts_afresh() {
        let layout = [FL, FR, FC, LFE, RL, RR];
        let input = tones(6, 5_000, 0.2);
        let mut fold = StereoFold::new(&layout, RATE, true);
        let first = run(&mut fold, &input, 6);
        let second = run(&mut fold, &input, 6);
        assert_eq!(first, second);
    }
}
