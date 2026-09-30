//! The compressor: turns a strip down while it is louder than a threshold.

use super::step_towards;
use weir_protocol::Compressor;

/// The compressor's real-time state.
///
/// A feed-forward design working in decibels: the level of the loudest
/// channel goes through the curve in [`Compressor::reduction_db`], and the
/// reduction that comes out is smoothed in two steps. The first holds each
/// peak and lets it fall at the release rate, so the reduction does not
/// ripple along with the waveform; the second eases towards that at the
/// attack rate. All channels get the same gain, so the stereo image stays
/// put.
pub(super) struct CompRt {
    /// The reduction the curve asks for, with peaks held; in dB, 0 or more.
    held: f32,
    /// Smoothed reduction, in dB, 0 or more.
    reduction: f32,
    /// 1 while on, fading to 0 after being switched off, so that switching
    /// it never clicks.
    mix: f32,
}

impl CompRt {
    pub(super) fn new() -> Self {
        Self {
            held: 0.0,
            reduction: 0.0,
            mix: 0.0,
        }
    }

    /// Whether it leaves the audio alone: switched off, and faded out.
    pub(super) fn is_idle(&self, comp: &Compressor) -> bool {
        !comp.enabled && self.mix == 0.0
    }

    /// Fill `gains` with the gain for each sample of `channels`. Returns the
    /// peak level that went in and the most the compressor turned down, in
    /// dB (0 or more).
    pub(super) fn gains(
        &mut self,
        comp: &Compressor,
        rate: u32,
        ramp: usize,
        channels: &[&[f32]],
        gains: &mut [f32],
    ) -> (f32, f32) {
        let fs = rate.max(1) as f32;
        // Per-sample decay of a one-pole smoother with this time constant.
        let coef = |ms: f32| (-1.0 / (ms.max(0.01) * 0.001 * fs)).exp();
        let (attack, release) = (coef(comp.attack_ms), coef(comp.release_ms));
        let makeup = comp.makeup();
        let target = if comp.enabled { 1.0 } else { 0.0 };
        let step = 1.0 / ramp.max(1) as f32;
        // Multiplying a level in dB by this and taking `exp` gives the gain.
        let to_gain = std::f32::consts::LN_10 / 20.0;
        let mut peak = 0.0f32;
        let mut most = 0.0f32;
        for (k, g) in gains.iter_mut().enumerate() {
            let x = channels.iter().fold(0.0f32, |m, ch| m.max(ch[k].abs()));
            peak = peak.max(x);
            let x_db = if x > 1e-6 { 20.0 * x.log10() } else { -120.0 };
            let want = -comp.reduction_db(x_db);
            self.held = want.max(release * self.held + (1.0 - release) * want);
            self.reduction = attack * self.reduction + (1.0 - attack) * self.held;
            most = most.max(self.reduction);
            self.mix = step_towards(self.mix, target, step);
            *g = ((makeup - self.reduction) * self.mix * to_gain).exp();
        }
        // Like a filter ringing down, a reduction decaying towards zero ends
        // up denormal, which is slow.
        if self.held < 1e-9 {
            self.held = 0.0;
        }
        if self.reduction < 1e-9 {
            self.reduction = 0.0;
        }
        (peak, most)
    }
}
