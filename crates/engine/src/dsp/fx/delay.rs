//! The bus delay: holds a bus's output back by a set time, so a bus that
//! plays fast can be lined up with one that plays slowly, such as speakers
//! next to a Bluetooth speaker.

use weir_protocol::BUS_DELAY_MAX_MS;

/// The highest sample rate PipeWire is asked for (see `Settings::rate`),
/// which sets how much room a delay line needs.
const MAX_RATE: f32 = 384_000.0;

/// Room in each channel's delay line: [`BUS_DELAY_MAX_MS`] at [`MAX_RATE`],
/// plus the one sample that a delay of the full length reaches back past.
///
/// Allocated zeroed, so the system hands out the memory a page at a time as
/// it is used: a delay line only costs what the sample rate it runs at
/// needs, and a bus that never delays costs nothing.
const LINE_LEN: usize = (BUS_DELAY_MAX_MS * 0.001 * MAX_RATE) as usize + 1;

/// A per channel delay line with a crossfade between delays, so changing
/// the time never clicks.
pub(super) struct DelayRt {
    /// Per channel ring of the most recent samples. Empty for strips.
    line: Vec<Vec<f32>>,
    /// Where the next sample goes, in `0..wrap`.
    pos: usize,
    /// How much of `line` is in use at `rate`.
    wrap: usize,
    rate: u32,
    /// The delay being heard, in samples.
    len: usize,
    /// The delay being faded out, while `fade` is below 1.
    prev_len: usize,
    /// Crossfade from `prev_len` (0) to `len` (1).
    fade: f32,
    /// Whether `line` holds audio, which it must not once it has been idle,
    /// when it would play old sound.
    dirty: bool,
}

impl DelayRt {
    /// A delay line for `channels` channels. Strips have none.
    pub(super) fn new(channels: usize) -> Self {
        Self {
            // Not `vec![vec![..]; n]`, whose copies would touch every page.
            line: (0..channels).map(|_| vec![0.0; LINE_LEN]).collect(),
            pos: 0,
            wrap: 1,
            rate: 0,
            len: 0,
            prev_len: 0,
            fade: 1.0,
            dirty: false,
        }
    }

    /// Whether no delay is wanted and none is left to fade out.
    fn is_off(&self, delay_ms: f32) -> bool {
        delay_ms <= 0.0 && self.len == 0 && self.fade >= 1.0
    }

    /// Whether it has nothing to do: built for a strip, or off with its
    /// line already cleared. Off with old sound still in the line is not
    /// idle: [`DelayRt::run`] must clear it once, or it would play when the
    /// delay comes back.
    pub(super) fn is_idle(&self, delay_ms: f32) -> bool {
        self.line.is_empty() || (self.is_off(delay_ms) && !self.dirty)
    }

    /// The delay `delay_ms` is, in samples at `rate`.
    fn samples(delay_ms: f32, rate: u32) -> usize {
        let wanted = (delay_ms.max(0.0) * 0.001 * rate as f32).round() as usize;
        // One less than `wrap`, so the newest sample is never overwritten
        // by the one being added.
        wanted.min(Self::wrap_for(rate) - 1)
    }

    /// How much of a line `rate` uses.
    fn wrap_for(rate: u32) -> usize {
        ((BUS_DELAY_MAX_MS * 0.001 * rate as f32) as usize + 1).min(LINE_LEN)
    }

    /// Forget everything, for a fresh start at `rate`.
    fn reset(&mut self, rate: u32) {
        self.rate = rate;
        self.wrap = Self::wrap_for(rate);
        if self.dirty {
            for l in self.line.iter_mut() {
                l.fill(0.0);
            }
        }
        self.pos = 0;
        self.len = 0;
        self.prev_len = 0;
        self.fade = 1.0;
        self.dirty = false;
    }

    /// Delay `channels` in place, all `n` samples long, by `delay_ms`.
    /// A channel's index in `channels` is its line, so a channel without a
    /// port is passed as `None` and keeps its place.
    pub(super) fn run(
        &mut self,
        delay_ms: f32,
        rate: u32,
        ramp: usize,
        channels: &mut [Option<&mut [f32]>],
        n: usize,
    ) {
        if rate != self.rate {
            self.reset(rate);
        }
        if self.is_off(delay_ms) {
            // Off: clear the line, so the next delay starts from silence.
            for l in self.line.iter_mut() {
                l[..self.wrap].fill(0.0);
            }
            self.dirty = false;
            self.pos = 0;
            return;
        }
        // A new delay starts a crossfade from the one heard now, once the
        // last one has finished.
        let target = Self::samples(delay_ms, rate);
        if target != self.len && self.fade >= 1.0 {
            self.prev_len = self.len;
            self.len = target;
            self.fade = 0.0;
        }
        let step = 1.0 / ramp.max(1) as f32;
        let wrap = self.wrap;
        let start = self.pos;
        let (len, prev_len, fade0) = (self.len, self.prev_len, self.fade);
        for (c, chan) in channels.iter_mut().enumerate() {
            let (Some(buf), Some(line)) = (chan.as_deref_mut(), self.line.get_mut(c)) else {
                continue;
            };
            let mut pos = start;
            let mut fade = fade0;
            for x in buf[..n].iter_mut() {
                line[pos] = *x;
                let now = line[(pos + wrap - len) % wrap];
                *x = if fade < 1.0 {
                    let before = line[(pos + wrap - prev_len) % wrap];
                    fade = (fade + step).min(1.0);
                    before + (now - before) * fade
                } else {
                    now
                };
                pos = (pos + 1) % wrap;
            }
        }
        self.pos = (start + n) % wrap;
        // Every channel advances through the fade together.
        self.fade = (fade0 + step * n as f32).min(1.0);
        self.dirty = true;
    }
}
