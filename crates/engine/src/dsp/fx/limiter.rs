//! The bus limiter: keeps a bus's output under its ceiling.

use super::{reach_99, step_towards};
use weir_protocol::{db_to_linear, Limiter};

/// Longest lookahead the limiter uses, in samples.
const MAX_LOOKAHEAD: usize = 512;
/// How far ahead the limiter looks, in seconds. Long enough to turn the
/// level down smoothly before a peak arrives, short enough not to be heard
/// as delay.
const LOOKAHEAD_S: f32 = 0.0015;

/// A lookahead peak limiter, linked across channels.
///
/// The audio goes through a delay of `len` samples. Meanwhile the gain each
/// incoming sample needs is fed through a sliding minimum over `len + 1`
/// samples and then averaged over `len`: the average reaches a peak's gain
/// exactly as that peak leaves the delay, so the limiter never lets it
/// through and never jumps. The level then comes back up at the release
/// rate.
pub(super) struct LimiterRt {
    /// Per channel delay line.
    delay: Vec<Vec<f32>>,
    /// Where the next sample goes in each delay line.
    pos: usize,
    /// The lookahead, in samples, at `rate`.
    len: usize,
    rate: u32,
    /// Monotonic queue of (sample index, required gain), as a ring.
    q_idx: Vec<u64>,
    q_gain: Vec<f32>,
    q_head: usize,
    q_count: usize,
    /// Running average of the sliding minimum.
    avg: Vec<f32>,
    avg_pos: usize,
    avg_sum: f64,
    /// Samples processed since the last reset: the sample index of the
    /// queue.
    t: u64,
    /// The gain applied now.
    gain: f32,
    /// On/off crossfade, 0 = bypassed.
    mix: f32,
    /// Bypassed and faded out, with nothing in the delay worth keeping.
    idle: bool,
}

impl LimiterRt {
    /// A limiter for `channels` channels. Strips have none.
    pub(super) fn new(channels: usize) -> Self {
        Self {
            delay: vec![vec![0.0; MAX_LOOKAHEAD]; channels],
            pos: 0,
            len: 1,
            rate: 0,
            q_idx: vec![0; MAX_LOOKAHEAD + 2],
            q_gain: vec![1.0; MAX_LOOKAHEAD + 2],
            q_head: 0,
            q_count: 0,
            avg: vec![1.0; MAX_LOOKAHEAD],
            avg_pos: 0,
            avg_sum: 0.0,
            t: 0,
            gain: 1.0,
            mix: 0.0,
            idle: true,
        }
    }

    /// Forget everything, for a fresh start at `rate`.
    fn reset(&mut self, rate: u32) {
        self.rate = rate;
        self.len = ((rate as f32 * LOOKAHEAD_S) as usize).clamp(8, MAX_LOOKAHEAD);
        for d in self.delay.iter_mut() {
            d.fill(0.0);
        }
        self.pos = 0;
        self.q_head = 0;
        self.q_count = 0;
        self.avg[..self.len].fill(1.0);
        self.avg_pos = 0;
        self.avg_sum = self.len as f64;
        self.t = 0;
        self.gain = 1.0;
    }

    /// Whether it has nothing to do: switched off and faded out, or built
    /// for a strip, which has no limiter.
    pub(super) fn is_idle(&self, limiter: &Limiter) -> bool {
        (self.idle && !limiter.enabled) || self.delay.is_empty()
    }

    /// Limit `channels` in place, all `n` samples long. Returns the lowest
    /// gain used.
    pub(super) fn run(
        &mut self,
        limiter: &Limiter,
        rate: u32,
        ramp: usize,
        channels: &mut [&mut [f32]],
        n: usize,
    ) -> f32 {
        if self.idle || rate != self.rate {
            self.reset(rate);
            self.idle = false;
        }
        let ceiling = db_to_linear(limiter.ceiling_db).max(1e-6);
        let release = reach_99(limiter.release_ms.max(1.0), rate.max(1) as f32);
        let mix_target = if limiter.enabled { 1.0 } else { 0.0 };
        let mix_step = (mix_target - self.mix) / ramp.max(1) as f32;
        let cap = self.q_idx.len();
        let len = self.len;
        let window = len as u64 + 1;
        let mut lowest = 1.0f32;
        for k in 0..n {
            let mut peak = 0.0f32;
            for ch in channels.iter() {
                peak = peak.max(ch[k].abs());
            }
            let need = if peak > ceiling { ceiling / peak } else { 1.0 };
            // Sliding minimum: drop queued gains that are no lower than the
            // new one, then any that have left the window.
            while self.q_count > 0 {
                let back = (self.q_head + self.q_count - 1) % cap;
                if self.q_gain[back] >= need {
                    self.q_count -= 1;
                } else {
                    break;
                }
            }
            let slot = (self.q_head + self.q_count) % cap;
            self.q_idx[slot] = self.t;
            self.q_gain[slot] = need;
            self.q_count += 1;
            while self.q_idx[self.q_head] + window <= self.t {
                self.q_head = (self.q_head + 1) % cap;
                self.q_count -= 1;
            }
            let min = self.q_gain[self.q_head];
            self.avg_sum += (min - self.avg[self.avg_pos]) as f64;
            self.avg[self.avg_pos] = min;
            self.avg_pos = (self.avg_pos + 1) % len;
            let target = ((self.avg_sum / len as f64) as f32).min(1.0);
            self.gain = if target < self.gain {
                target
            } else {
                self.gain + (target - self.gain) * release
            };
            lowest = lowest.min(self.gain);
            self.mix = step_towards(self.mix, mix_target, mix_step);
            for (c, ch) in channels.iter_mut().enumerate() {
                let x = ch[k];
                let delayed = self.delay[c][self.pos];
                self.delay[c][self.pos] = x;
                let limited = delayed * self.gain;
                ch[k] = x + (limited - x) * self.mix;
            }
            self.pos = (self.pos + 1) % len;
            self.t += 1;
        }
        if !limiter.enabled && self.mix == 0.0 {
            self.idle = true;
        }
        lowest
    }
}
