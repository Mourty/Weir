//! Ducking: turning a strip down while other strips are heard.

use super::reach_99;

/// A strip's ducking settings, resolved for the real-time thread.
pub struct RtDuck {
    /// Whether ducking works on the strip.
    pub enabled: bool,
    /// Indices into `RtParams::strips` of the strips that trigger it.
    pub triggers: Vec<usize>,
    /// Linear level above which a trigger counts as heard.
    pub threshold: f32,
    /// Linear gain while ducked.
    pub gain: f32,
    /// See [`weir_protocol::Ducking::attack_ms`].
    pub attack_ms: f32,
    /// See [`weir_protocol::Ducking::hold_ms`].
    pub hold_ms: f32,
    /// See [`weir_protocol::Ducking::release_ms`].
    pub release_ms: f32,
}

/// Ducking's real-time state: the gain it is at and how long it still holds.
pub(super) struct DuckRt {
    /// The gain applied now, from the ducked gain up to 1.
    pub(super) gain: f32,
    /// Samples left before it may come back up.
    hold_left: usize,
}

impl DuckRt {
    pub(super) fn new() -> Self {
        Self {
            gain: 1.0,
            hold_left: 0,
        }
    }

    /// Whether it leaves the strip alone: switched off, and all the way back
    /// up.
    pub(super) fn is_idle(&self, duck: &RtDuck) -> bool {
        !duck.enabled && self.gain == 1.0
    }

    /// Fill `out` with `level` times the ducking gain, moving that gain
    /// down while a trigger is `heard` (and through the hold time after) and
    /// back up otherwise.
    pub(super) fn run(
        &mut self,
        duck: &RtDuck,
        heard: bool,
        rate: u32,
        level: &[f32],
        out: &mut [f32],
    ) {
        let fs = rate.max(1) as f32;
        let n = out.len();
        if duck.enabled && heard {
            self.hold_left = (duck.hold_ms * 0.001 * fs) as usize;
        } else {
            self.hold_left = self.hold_left.saturating_sub(n);
        }
        let down = duck.enabled && (heard || self.hold_left > 0);
        let target = if down { duck.gain } else { 1.0 };
        let attack = reach_99(duck.attack_ms.max(0.1), fs);
        let release = reach_99(duck.release_ms.max(0.1), fs);
        let mut g = self.gain;
        for (o, l) in out.iter_mut().zip(level) {
            g += (target - g) * if target < g { attack } else { release };
            *o = l * g;
        }
        if (g - target).abs() < 1e-5 {
            g = target;
        }
        self.gain = g;
    }
}
