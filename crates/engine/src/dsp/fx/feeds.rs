//! The two extra signals a strip can send besides its channels: bass for
//! subwoofers, and a passive surround decode for surround speakers.

use super::flush_denormals;
use weir_protocol::{
    EqBand, EqBandKind, SvfCoefs, PASSIVE_SURROUND_CUTOFF_HZ, PASSIVE_SURROUND_DELAY_MS,
};

/// Where a strip's subwoofer feed stops: the usual crossover between
/// subwoofer and speakers.
const SUB_CUTOFF_HZ: f32 = 120.0;

/// Room for a passive surround feed's delay: over 20 ms at the highest
/// sample rate PipeWire can be held at.
pub(super) const SURROUND_DELAY_LEN: usize = 8192;

/// A strip's subwoofer feed filter: a fourth order (Linkwitz-Riley)
/// low-pass, made of two Butterworth sections.
pub(super) struct SubFilter {
    coefs: SvfCoefs,
    /// The sample rate `coefs` is for.
    rate: u32,
    /// The two sections' integrators.
    state: [[f32; 2]; 2],
}

impl SubFilter {
    pub(super) fn new() -> Self {
        Self {
            coefs: SvfCoefs::IDENTITY,
            rate: 0,
            state: [[0.0; 2]; 2],
        }
    }

    /// Low-pass `buf` in place.
    pub(super) fn run(&mut self, rate: u32, buf: &mut [f32]) {
        if rate != self.rate {
            self.rate = rate;
            self.coefs = EqBand::new(
                EqBandKind::LowPass,
                SUB_CUTOFF_HZ,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
            )
            .coefs(rate as f32);
            self.state = [[0.0; 2]; 2];
        }
        for x in buf.iter_mut() {
            let v = self.coefs.tick(&mut self.state[0], *x);
            *x = self.coefs.tick(&mut self.state[1], v);
        }
        flush_denormals(self.state.iter_mut().flatten());
    }
}

/// A passive surround feed's low-pass and delay, which keep it behind the
/// front speakers: without them the ear takes the surround speakers, which
/// carry some of the front sound too, for where that sound comes from.
pub(super) struct SurroundFeed {
    coefs: SvfCoefs,
    /// The sample rate `coefs` and `lag` are for.
    rate: u32,
    state: [f32; 2],
    /// Ring of past samples; empty for buses.
    delay: Vec<f32>,
    /// Where the next sample goes in `delay`.
    at: usize,
    /// How many samples back the output is.
    lag: usize,
}

impl SurroundFeed {
    /// A feed with room for `len` samples of delay; 0 for buses, which have
    /// no feed.
    pub(super) fn new(len: usize) -> Self {
        Self {
            coefs: SvfCoefs::IDENTITY,
            rate: 0,
            state: [0.0; 2],
            delay: vec![0.0; len],
            at: 0,
            lag: 0,
        }
    }

    /// Low-pass and delay `buf` in place.
    pub(super) fn run(&mut self, rate: u32, buf: &mut [f32]) {
        let len = self.delay.len();
        if len == 0 {
            return;
        }
        if rate != self.rate {
            self.rate = rate;
            self.coefs = EqBand::new(
                EqBandKind::LowPass,
                PASSIVE_SURROUND_CUTOFF_HZ.min(rate as f32 * 0.45),
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
            )
            .coefs(rate as f32);
            self.state = [0.0; 2];
            let lag = PASSIVE_SURROUND_DELAY_MS * 0.001 * rate as f32;
            self.lag = (lag.round() as usize).clamp(1, len - 1);
        }
        for x in buf.iter_mut() {
            let v = self.coefs.tick(&mut self.state, *x);
            self.delay[self.at] = v;
            *x = self.delay[(self.at + len - self.lag) % len];
            self.at = (self.at + 1) % len;
        }
        flush_denormals(self.state.iter_mut());
    }
}
