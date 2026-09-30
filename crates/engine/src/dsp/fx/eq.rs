//! The equalizer: up to [`EQ_MAX_BANDS`] state variable filters in a row,
//! crossfading to a new set whenever the settings change.

use super::flush_denormals;
use std::hash::{Hash, Hasher};
use weir_protocol::{EqBand, SvfCoefs, EQ_MAX_BANDS};

/// Integrator memory of every band of one filter bank, for one channel.
type BankState = [[f32; 2]; EQ_MAX_BANDS];

/// A set of band coefficients computed for one sample rate.
#[derive(Clone, Copy)]
struct Bank {
    coefs: [SvfCoefs; EQ_MAX_BANDS],
    /// How many of `coefs` are in use.
    len: usize,
}

impl Bank {
    const EMPTY: Bank = Bank {
        coefs: [SvfCoefs::IDENTITY; EQ_MAX_BANDS],
        len: 0,
    };

    /// Filter `buf` in place through every band, one after the other.
    fn run(&self, state: &mut BankState, buf: &mut [f32]) {
        if self.len == 0 {
            return;
        }
        for x in buf.iter_mut() {
            let mut v = *x;
            for (c, st) in self.coefs[..self.len].iter().zip(state.iter_mut()) {
                v = c.tick(st, v);
            }
            *x = v;
        }
        flush_denormals(state[..self.len].iter_mut().flatten());
    }
}

/// What the snapshot says about an equalizer.
#[derive(Clone, Default)]
pub struct EqParams {
    /// Only the bands that change the sound; empty when the equalizer is
    /// off.
    pub bands: Vec<EqBand>,
    /// Identifies these settings, so the real-time thread can tell when they
    /// changed without comparing every band.
    pub key: u64,
}

impl EqParams {
    /// What the real-time thread needs of `eq`.
    pub fn from_eq(eq: &weir_protocol::Equalizer) -> Self {
        let bands: Vec<EqBand> = eq.active_bands().copied().take(EQ_MAX_BANDS).collect();
        let key = if bands.is_empty() {
            0
        } else {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for b in &bands {
                b.kind.hash(&mut h);
                b.freq_hz.to_bits().hash(&mut h);
                b.gain_db.to_bits().hash(&mut h);
                b.q.to_bits().hash(&mut h);
            }
            // 0 means "no bands", so keep real settings away from it.
            h.finish().max(1)
        };
        Self { bands, key }
    }
}

/// The equalizer's real-time state.
///
/// Settings changes crossfade from the old set of filters to the new one
/// over one ramp, rather than switching instantly, so that adding a band,
/// removing one or switching the equalizer off never clicks.
pub(super) struct EqRt {
    /// The settings `banks[active]` was built from.
    key: u64,
    /// The sample rate it was built for.
    rate: u32,
    banks: [Bank; 2],
    /// Per channel, per bank.
    state: Vec<[BankState; 2]>,
    /// The bank playing now; the other is the one being faded to.
    active: usize,
    /// Samples into the crossfade towards `1 - active`, or `None`.
    fade: Option<usize>,
}

impl EqRt {
    pub(super) fn new(channels: usize) -> Self {
        Self {
            key: 0,
            rate: 0,
            banks: [Bank::EMPTY; 2],
            state: vec![[[[0.0; 2]; EQ_MAX_BANDS]; 2]; channels],
            active: 0,
            fade: None,
        }
    }

    /// Start a crossfade if the settings or the rate changed.
    pub(super) fn update(&mut self, eq: &EqParams, rate: u32) {
        if eq.key == self.key && rate == self.rate {
            return;
        }
        // A change arriving mid-fade completes the old fade first. Changes
        // come at most once per cycle, and a fade is a ramp long, so this
        // only cuts a fade short when cycles are shorter than a ramp.
        if self.fade.is_some() {
            self.active = 1 - self.active;
            self.fade = None;
        }
        let next = 1 - self.active;
        let mut bank = Bank::EMPTY;
        for (slot, band) in bank.coefs.iter_mut().zip(eq.bands.iter()) {
            *slot = band.coefs(rate as f32);
        }
        bank.len = eq.bands.len().min(EQ_MAX_BANDS);
        let kept = self.banks[self.active].len.min(bank.len);
        // The new filters take over the old ones' memory so that, when only
        // a setting moved, the two outputs start out nearly identical.
        for ch in self.state.iter_mut() {
            let [a, b] = ch;
            let (old, new) = if self.active == 0 { (a, b) } else { (b, a) };
            new[..kept].copy_from_slice(&old[..kept]);
            for st in new[kept..].iter_mut() {
                *st = [0.0; 2];
            }
        }
        self.banks[next] = bank;
        self.key = eq.key;
        self.rate = rate;
        self.fade = Some(0);
    }

    /// Whether it passes audio through untouched: no bands, and no fade.
    pub(super) fn is_idle(&self) -> bool {
        self.fade.is_none() && self.banks[self.active].len == 0
    }

    /// Process one channel in place. `weights` holds the crossfade position
    /// for every sample and `tmp` is scratch space of the same length.
    pub(super) fn run(
        &mut self,
        channel: usize,
        buf: &mut [f32],
        weights: &[f32],
        tmp: &mut [f32],
    ) {
        let active = self.active;
        let [s0, s1] = &mut self.state[channel];
        let (old_state, new_state) = if active == 0 { (s0, s1) } else { (s1, s0) };
        match self.fade {
            None => self.banks[active].run(old_state, buf),
            Some(_) => {
                let tmp = &mut tmp[..buf.len()];
                tmp.copy_from_slice(buf);
                self.banks[active].run(old_state, buf);
                self.banks[1 - active].run(new_state, tmp);
                for ((x, &y), &w) in buf.iter_mut().zip(tmp.iter()).zip(weights) {
                    *x += (y - *x) * w;
                }
            }
        }
    }

    /// Fill `weights` with this cycle's crossfade position per sample.
    pub(super) fn weights(&self, weights: &mut [f32], ramp: usize) {
        if let Some(done) = self.fade {
            for (k, w) in weights.iter_mut().enumerate() {
                *w = ((done + k + 1) as f32 / ramp as f32).min(1.0);
            }
        }
    }

    /// Move the crossfade on by `n` samples, finishing it if it is done.
    pub(super) fn advance(&mut self, n: usize, ramp: usize) {
        if let Some(done) = self.fade {
            if done + n >= ramp {
                self.active = 1 - self.active;
                self.fade = None;
            } else {
                self.fade = Some(done + n);
            }
        }
    }
}
