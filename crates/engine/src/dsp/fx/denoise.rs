//! Speech noise suppression, with RNNoise (as the `nnnoiseless` crate).

use super::step_towards;
use crate::dsp::params::RtCell;
use nnnoiseless::DenoiseState;

/// Samples in one RNNoise frame: 10 ms at 48 kHz.
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// nnnoiseless works on samples scaled like 16-bit integers.
const PCM_SCALE: f32 = 32768.0;

/// One channel of the noise suppressor.
///
/// Audio comes out two frames (20 ms) late: one frame to collect a whole
/// frame, and one inside RNNoise, whose output for a frame is the overlap of
/// that frame's window with the previous one.
struct DenoiseChan {
    state: Box<DenoiseState<'static>>,
    /// The frame being collected.
    input: [f32; FRAME],
    /// The frame before that, which moves on to play out as the original
    /// signal, delayed to line up with the suppressed one.
    older: [f32; FRAME],
    /// The suppressor's output for the last complete frame, being played out.
    output: [f32; FRAME],
}

/// Noise suppression for every channel of one strip. Created the first time
/// suppression is switched on and kept while the strip exists, so switching
/// it off can fade out rather than cut.
pub struct DenoiseBank {
    inner: RtCell<DenoiseInner>,
}

struct DenoiseInner {
    chans: Vec<DenoiseChan>,
    /// Position inside the current frame, shared by every channel.
    pos: usize,
    /// Current on/off crossfade (0 = bypassed), ramped.
    on: f32,
    /// Current wet/dry amount, ramped.
    amount: f32,
    /// True while the suppressor is not running at all.
    idle: bool,
}

impl DenoiseBank {
    /// A suppressor for `channels` channels, allocated here and never on the
    /// real-time thread.
    pub fn new(channels: usize) -> Self {
        let chans = (0..channels)
            .map(|_| DenoiseChan {
                state: DenoiseState::new(),
                input: [0.0; FRAME],
                older: [0.0; FRAME],
                output: [0.0; FRAME],
            })
            .collect();
        Self {
            inner: RtCell::new(DenoiseInner {
                chans,
                pos: 0,
                on: 0.0,
                amount: 1.0,
                idle: true,
            }),
        }
    }

    /// Whether it has work to do this cycle: `wanted` on, or still fading
    /// out.
    ///
    /// # Safety
    /// Real-time thread only.
    pub(super) unsafe fn is_busy(&self, wanted: bool) -> bool {
        wanted || !self.inner.get_mut().idle
    }

    /// Suppress noise in `bufs` in place, `n` samples of each channel,
    /// fading in or out as `wanted` says and mixing `amount` of the result
    /// with the original.
    ///
    /// # Safety
    /// Real-time thread only.
    pub(super) unsafe fn run(
        &self,
        bufs: &mut [Vec<f32>],
        n: usize,
        wanted: bool,
        amount: f32,
        ramp: usize,
    ) {
        let d = self.inner.get_mut();
        if d.idle {
            // Starting (again): forget whatever was left from last time, or it
            // would play out as a snippet of old audio.
            for ch in d.chans.iter_mut() {
                ch.input = [0.0; FRAME];
                ch.older = [0.0; FRAME];
                ch.output = [0.0; FRAME];
            }
            d.pos = 0;
            d.idle = false;
        }
        let on_target = if wanted { 1.0 } else { 0.0 };
        let on_step = (on_target - d.on) / ramp as f32;
        let amount_step = (amount - d.amount) / ramp as f32;
        let start_pos = d.pos;
        let (start_on, start_amount) = (d.on, d.amount);
        let mut end = (start_pos, start_on, start_amount);
        // Every channel runs through the same frame position and ramps, so
        // each starts from where the cycle started and they all end alike.
        for (ch, buf) in d.chans.iter_mut().zip(bufs.iter_mut()) {
            let (mut pos, mut on, mut amt) = (start_pos, start_on, start_amount);
            for x in buf[..n].iter_mut() {
                on = step_towards(on, on_target, on_step);
                amt = step_towards(amt, amount, amount_step);
                let dry = *x;
                // The original from two frames ago lines up with the suppressed
                // output, so the amount control mixes like with like.
                let delayed = ch.older[pos] / PCM_SCALE;
                let wet = ch.output[pos];
                ch.older[pos] = ch.input[pos];
                ch.input[pos] = dry * PCM_SCALE;
                let processed = delayed + (wet - delayed) * amt;
                *x = dry + (processed - dry) * on;
                pos += 1;
                if pos == FRAME {
                    ch.state.process_frame(&mut ch.output, &ch.input);
                    for v in ch.output.iter_mut() {
                        *v /= PCM_SCALE;
                    }
                    pos = 0;
                }
            }
            end = (pos, on, amt);
        }
        (d.pos, d.on, d.amount) = end;
        if !wanted && d.on == 0.0 {
            d.idle = true;
        }
    }
}

/// A suppressor to warm up with, made on another thread; see
/// [`warm_up_denoise`].
pub fn new_warm_up_state() -> Box<DenoiseState<'static>> {
    DenoiseState::new()
}

/// Run a throwaway frame through a suppressor on the current thread.
///
/// The FFT library behind nnnoiseless sets up its tables and scratch space
/// per thread, the first time it runs there. Doing that on the real-time
/// thread at a moment of our choosing (the engine's first cycle, while
/// everything is still fading in from silence) keeps it from happening
/// later, in the middle of someone talking.
pub fn warm_up_denoise(state: &mut DenoiseState<'static>) {
    let input = [0.0f32; FRAME];
    let mut output = [0.0f32; FRAME];
    state.process_frame(&mut output, &input);
}
