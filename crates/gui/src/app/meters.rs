//! Turning the daemon's meter readings into meters that move smoothly.
//!
//! Readings arrive up to 30 times a second. Between them, and when they
//! drop, a meter falls at a steady rate rather than jumping, and its peak
//! mark holds for a moment before falling too, as on a hardware desk. The
//! gate's, compressor's and limiter's displays behave the same way.

use super::App;
use crate::fx_window::CompView;
use crate::{theme, widgets};
use egui::{pos2, vec2, Rect, Sense, Ui};
use std::time::{Duration, Instant};
use weir_protocol::{Meters, StripOrBus, METER_FLOOR_DB};

/// How fast a meter falls, in dB per second.
const METER_FALL: f32 = 50.0;
/// How long a peak mark holds before it starts falling.
const PEAK_HOLD: Duration = Duration::from_millis(1500);
/// How fast a peak mark falls once it does, in dB per second.
const PEAK_FALL: f32 = 30.0;
/// A strip lights its CLIP light when it reaches full scale: a microphone
/// that loud has clipped on the way in, and anything else will clip the
/// hardware it ends up on.
const STRIP_CLIP_DB: f32 = -0.01;
/// A bus lights it only when it goes over full scale. A limiter with its
/// ceiling at 0 dB holds the bus at exactly full scale, which does not clip.
const BUS_CLIP_DB: f32 = 0.01;
/// How fast the limiter's and compressor's gain reduction displays recover,
/// in dB per second.
const REDUCTION_RECOVERY: f32 = 20.0;

/// One strip's or bus's meter, per channel, as drawn.
pub(super) struct MeterDisplay {
    /// The latest reading.
    raw: Vec<f32>,
    /// The level drawn: the reading, or falling towards it.
    pub(super) level: Vec<f32>,
    /// The peak mark drawn.
    pub(super) peak: Vec<f32>,
    /// When each peak was reached, for the hold.
    peak_at: Vec<Option<Instant>>,
    /// The highest reading since the peak number was last clicked, of any
    /// channel.
    highest: f32,
}

impl Default for MeterDisplay {
    fn default() -> Self {
        Self {
            raw: Vec::new(),
            level: Vec::new(),
            peak: Vec::new(),
            peak_at: Vec::new(),
            highest: METER_FLOOR_DB,
        }
    }
}

impl MeterDisplay {
    /// Take a new reading. A change in the number of channels starts over.
    fn feed(&mut self, raw: &[f32]) {
        if self.raw.len() != raw.len() {
            self.raw = vec![METER_FLOOR_DB; raw.len()];
            self.level = vec![METER_FLOOR_DB; raw.len()];
            self.peak = vec![METER_FLOOR_DB; raw.len()];
            self.peak_at = vec![None; raw.len()];
            self.highest = METER_FLOOR_DB;
        }
        self.raw.copy_from_slice(raw);
        for &db in raw {
            self.highest = self.highest.max(db);
        }
    }

    /// The highest reading since [`Self::start_again`], of any channel.
    pub(super) fn highest(&self) -> f32 {
        self.highest.max(METER_FLOOR_DB)
    }

    /// Forget the highest reading: the peak number starts again.
    pub(super) fn start_again(&mut self) {
        self.highest = METER_FLOOR_DB;
    }

    /// Move `dt` seconds on: levels fall towards the reading, and peaks
    /// fall once their hold is over.
    fn animate(&mut self, dt: f32, now: Instant) {
        for c in 0..self.raw.len() {
            let target = self.raw[c];
            self.level[c] = (self.level[c] - METER_FALL * dt)
                .max(target)
                .max(METER_FLOOR_DB);
            if target >= self.peak[c] {
                self.peak[c] = target;
                self.peak_at[c] = Some(now);
            } else if self.peak_at[c].is_none_or(|t| now.duration_since(t) > PEAK_HOLD) {
                self.peak[c] = (self.peak[c] - PEAK_FALL * dt).max(self.level[c]);
            }
        }
    }

    /// The levels and peaks to draw for `channels` channels: silence until
    /// readings with that many arrive.
    pub(super) fn shown(&self, channels: usize) -> (Vec<f32>, Vec<f32>) {
        if self.level.len() == channels {
            (self.level.clone(), self.peak.clone())
        } else {
            let floor = vec![METER_FLOOR_DB; channels];
            (floor.clone(), floor)
        }
    }
}

/// How wide the peak number above a meter is: room for "-60.0 dB".
const PEAK_W: f32 = 54.0;

impl App {
    /// Room for the peak number above a meter, across the strip or bus.
    pub(super) fn peak_area(&self, ui: &mut Ui, inner_w: f32) -> Rect {
        let (rect, _) = ui.allocate_exact_size(
            vec2(inner_w, theme::PEAK_H + theme::PEAK_GAP),
            Sense::hover(),
        );
        rect
    }

    /// The highest peak of `target` as a number in `area`, over `meter`,
    /// `shift` dB away from what the meter reads, or silence with `None`.
    /// Clicking it starts again.
    pub(super) fn peak_number(
        &mut self,
        ui: &mut Ui,
        target: StripOrBus,
        area: Rect,
        meter: Rect,
        shift: Option<f32>,
    ) {
        let highest = self
            .meters
            .get(&target)
            .map_or(METER_FLOOR_DB, |m| m.highest());
        let db = match shift {
            Some(s) if highest > METER_FLOOR_DB => (highest + s).max(METER_FLOOR_DB),
            _ => METER_FLOOR_DB,
        };
        // Over the meter, but inside the strip.
        let half = PEAK_W / 2.0;
        let x = meter
            .center()
            .x
            .clamp(area.left() + half, area.right() - half);
        let rect = Rect::from_center_size(
            pos2(x, area.top() + theme::PEAK_H / 2.0),
            vec2(PEAK_W, theme::PEAK_H),
        );
        let id = ui.id().with(("peak", target));
        let r = widgets::peak_number(ui, id, rect, db)
            .on_hover_text("The highest peak since you last clicked here. Click to start again.");
        if r.clicked() {
            self.meters.entry(target).or_default().start_again();
        }
    }

    /// Take in a new set of readings: the meters, the clip lights, and what
    /// each gate, compressor, limiter and ducking is doing.
    pub(super) fn take_meters(&mut self, m: &Meters) {
        for (id, v) in &m.strips {
            let target = StripOrBus::Strip(*id);
            self.meters.entry(target).or_default().feed(v);
            if v.iter().any(|&db| db >= STRIP_CLIP_DB) {
                self.clips.insert(target);
            }
        }
        for (id, v) in &m.buses {
            let target = StripOrBus::Bus(*id);
            self.meters.entry(target).or_default().feed(v);
            if v.iter().any(|&db| db > BUS_CLIP_DB) {
                self.clips.insert(target);
            }
        }
        // Displays jump to a higher level or deeper reduction straight
        // away, and fall back in `animate_meters`.
        self.comp_views
            .retain(|id, _| m.compressors.contains_key(id));
        for (id, c) in &m.compressors {
            let view = self.comp_views.entry(*id).or_insert(CompView {
                level: METER_FLOOR_DB,
                reduction: 0.0,
            });
            view.level = view.level.max(c.level_db);
            view.reduction = view.reduction.min(c.reduction_db);
        }
        self.duck_views = m.ducking.clone().into_iter().collect();
        self.reductions.retain(|id, _| m.limiters.contains_key(id));
        for (id, &gr) in &m.limiters {
            let shown = self.reductions.entry(*id).or_insert(0.0);
            *shown = shown.min(gr);
        }
        self.gate_views.retain(|id, _| m.gates.contains_key(id));
        for (id, g) in &m.gates {
            let view = self.gate_views.entry(*id).or_default();
            view.level = view.level.max(g.level_db);
            view.reduction = g.reduction_db;
        }
    }

    /// Move every display on by `dt` seconds.
    pub(super) fn animate_meters(&mut self, dt: f32, now: Instant) {
        for md in self.meters.values_mut() {
            md.animate(dt, now);
        }
        for view in self.gate_views.values_mut() {
            view.level = (view.level - METER_FALL * dt).max(METER_FLOOR_DB);
        }
        for shown in self.reductions.values_mut() {
            *shown = (*shown + REDUCTION_RECOVERY * dt).min(0.0);
        }
        for view in self.comp_views.values_mut() {
            view.level = (view.level - METER_FALL * dt).max(METER_FLOOR_DB);
            view.reduction = (view.reduction + REDUCTION_RECOVERY * dt).min(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::peak_text;

    #[test]
    fn meters_fall_steadily_and_peaks_hold() {
        let start = Instant::now();
        let mut m = MeterDisplay::default();
        m.feed(&[-6.0, -12.0]);
        m.animate(0.0, start);
        assert_eq!(m.shown(2).0, [-6.0, -12.0]);
        // Silence: a tenth of a second later the level has fallen 5 dB, and
        // the peak has not moved.
        m.feed(&[METER_FLOOR_DB, METER_FLOOR_DB]);
        m.animate(0.1, start + Duration::from_millis(100));
        assert!((m.level[0] + 11.0).abs() < 1e-4);
        assert_eq!(m.peak[0], -6.0);
        // After the hold, the peak falls too, but never below the level.
        m.animate(0.1, start + PEAK_HOLD + Duration::from_millis(200));
        assert!(m.peak[0] < -6.0 && m.peak[0] >= m.level[0]);
    }

    #[test]
    fn a_meter_shows_silence_until_its_channels_arrive() {
        let m = MeterDisplay::default();
        assert_eq!(m.shown(2).0, [METER_FLOOR_DB, METER_FLOOR_DB]);
    }

    #[test]
    fn the_peak_number_holds_the_highest_until_started_again() {
        let mut m = MeterDisplay::default();
        assert_eq!(m.highest(), METER_FLOOR_DB, "silence before any reading");
        m.feed(&[-20.0, -8.4]);
        m.feed(&[-30.0, -25.0]);
        assert_eq!(m.highest(), -8.4);
        m.start_again();
        assert_eq!(m.highest(), METER_FLOOR_DB);
        m.feed(&[-12.5, -40.0]);
        assert_eq!(m.highest(), -12.5);
        assert_eq!(peak_text(-8.44), "-8.4 dB");
        assert_eq!(peak_text(0.6), "+0.6 dB");
        assert_eq!(peak_text(0.0), "0.0 dB");
        assert_eq!(peak_text(METER_FLOOR_DB), "-inf dB");
    }
}
