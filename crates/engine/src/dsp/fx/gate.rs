//! The noise gate: silences a strip while it is quieter than a threshold.

use super::{flush_denormals, reach_99};
use weir_protocol::{db_to_linear, Gate, GATE_HYSTERESIS_DB};

/// The noise gate's real-time state.
pub(super) struct GateRt {
    /// Peak envelope of the loudest channel.
    env: f32,
    /// Gain applied right now, between the range and 1.
    pub(super) gain: f32,
    /// Whether the gate is open, or holding open.
    open: bool,
    /// Samples left before a gate whose level dropped starts to close.
    hold_left: usize,
}

impl GateRt {
    pub(super) fn new() -> Self {
        Self {
            env: 0.0,
            gain: 1.0,
            open: true,
            hold_left: 0,
        }
    }

    /// Whether it leaves the audio alone: switched off, and fully open.
    pub(super) fn is_idle(&self, gate: &Gate) -> bool {
        !gate.enabled && self.gain >= 1.0
    }

    /// Work out the gain for every sample from the loudest of `channels`,
    /// writing it to `gains`. Returns the peak level it saw.
    pub(super) fn gains(
        &mut self,
        gate: &Gate,
        rate: u32,
        channels: &[&[f32]],
        gains: &mut [f32],
    ) -> f32 {
        let fs = rate.max(1) as f32;
        let open_at = db_to_linear(gate.threshold_db);
        let close_at = db_to_linear(gate.threshold_db - GATE_HYSTERESIS_DB);
        let floor = if gate.range_db <= -90.0 {
            0.0
        } else {
            10f32.powf(gate.range_db / 20.0)
        };
        let attack = reach_99(gate.attack_ms.max(0.01), fs);
        let release = reach_99(gate.release_ms.max(0.01), fs);
        // The level detector itself falls away over 10 ms, fast enough to
        // follow speech and slow enough to ignore the gaps inside a waveform.
        let env_decay = (-1.0 / (0.010 * fs)).exp();
        let hold = (gate.hold_ms * 0.001 * fs) as usize;

        let mut peak = 0.0f32;
        for (k, g) in gains.iter_mut().enumerate() {
            let mut key = 0.0f32;
            for ch in channels {
                key = key.max(ch[k].abs());
            }
            peak = peak.max(key);
            self.env = key.max(self.env * env_decay);
            if !gate.enabled {
                self.open = true;
            } else if self.env >= open_at || (self.open && self.env >= close_at) {
                self.open = true;
                self.hold_left = hold;
            } else if self.hold_left > 0 {
                self.hold_left -= 1;
            } else {
                self.open = false;
            }
            let target = if self.open { 1.0 } else { floor };
            let c = if target > self.gain { attack } else { release };
            self.gain += (target - self.gain) * c;
            if (self.gain - target).abs() < 1e-6 {
                self.gain = target;
            }
            *g = self.gain;
        }
        flush_denormals([&mut self.env]);
        peak
    }
}
