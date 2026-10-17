//! Putting a track's channels where each format expects them.
//!
//! Weir gives each track's samples interleaved in the order of its strip's
//! or bus's channel positions. WAV and FLAC want speakers in the order of
//! WAV's channel mask; Ogg Vorbis and Opus have an order of their own; MP3
//! holds stereo at most, so more channels are mixed down for it.

use weir_protocol::ChannelPosition;

/// The bit of a position in WAV's channel mask.
fn wav_bit(p: ChannelPosition) -> u32 {
    match p {
        ChannelPosition::FL => 0x1,
        ChannelPosition::FR => 0x2,
        ChannelPosition::FC | ChannelPosition::Mono => 0x4,
        ChannelPosition::LFE => 0x8,
        ChannelPosition::RL => 0x10,
        ChannelPosition::RR => 0x20,
        ChannelPosition::SL => 0x200,
        ChannelPosition::SR => 0x400,
    }
}

/// How to reorder a track's channels for a format: for each channel of
/// the file, the track's channel it takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Order(pub Vec<usize>);

impl Order {
    /// The channels as they come.
    pub fn same(channels: usize) -> Self {
        Self((0..channels).collect())
    }

    /// Reorder interleaved `samples` into `out`, replacing what it held.
    pub fn apply(&self, samples: &[f32], out: &mut Vec<f32>) {
        let n = self.0.len();
        out.clear();
        if self.0.iter().enumerate().all(|(i, &c)| i == c) {
            out.extend_from_slice(samples);
            return;
        }
        out.reserve(samples.len());
        for frame in samples.chunks_exact(n) {
            out.extend(self.0.iter().map(|&c| frame[c]));
        }
    }
}

/// The order WAV and FLAC want, and WAV's channel mask: speakers sorted by
/// their bit in the mask. A layout using a speaker twice keeps its own
/// order and gets no mask.
pub(crate) fn wav_order(positions: &[ChannelPosition]) -> (Order, u32) {
    let mut by_bit: Vec<(u32, usize)> = positions
        .iter()
        .enumerate()
        .map(|(i, p)| (wav_bit(*p), i))
        .collect();
    by_bit.sort();
    let mask = by_bit.iter().fold(0, |m, (b, _)| m | b);
    if by_bit.windows(2).any(|w| w[0].0 == w[1].0) {
        return (Order::same(positions.len()), 0);
    }
    (Order(by_bit.into_iter().map(|(_, i)| i).collect()), mask)
}

/// The order Ogg Vorbis and Opus want, and whether the layout is one they
/// know: mono, stereo, quad, 5.0, 5.1 or 7.1. Others keep their own order.
pub(crate) fn vorbis_order(positions: &[ChannelPosition]) -> (Order, bool) {
    use ChannelPosition::*;
    let wanted: &[ChannelPosition] = match positions.len() {
        1 => &[Mono],
        2 => &[FL, FR],
        4 => &[FL, FR, RL, RR],
        5 => &[FL, FC, FR, RL, RR],
        6 => &[FL, FC, FR, RL, RR, LFE],
        8 => &[FL, FC, FR, SL, SR, RL, RR, LFE],
        _ => &[],
    };
    let mono = positions.len() == 1;
    let found: Option<Vec<usize>> = wanted
        .iter()
        .map(|w| positions.iter().position(|p| p == w || (mono && *p == FC)))
        .collect();
    match found {
        Some(order) if !wanted.is_empty() => (Order(order), true),
        _ => (Order::same(positions.len()), false),
    }
}

/// How each channel goes into a stereo mix: to the left, to the right.
/// Centers go to both at -3 dB, surrounds to their side at -3 dB, the
/// subwoofer nowhere; then everything is scaled so a full-scale signal in
/// every channel cannot clip.
pub(crate) fn stereo_weights(positions: &[ChannelPosition]) -> Vec<(f32, f32)> {
    use ChannelPosition::*;
    const H: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let mut w: Vec<(f32, f32)> = positions
        .iter()
        .map(|p| match p {
            FL => (1.0, 0.0),
            FR => (0.0, 1.0),
            Mono | FC => (H, H),
            RL | SL => (H, 0.0),
            RR | SR => (0.0, H),
            LFE => (0.0, 0.0),
        })
        .collect();
    let left: f32 = w.iter().map(|x| x.0).sum();
    let right: f32 = w.iter().map(|x| x.1).sum();
    let most = left.max(right);
    if most > 1.0 {
        w.iter_mut().for_each(|x| {
            x.0 /= most;
            x.1 /= most;
        });
    }
    w
}

/// Mix interleaved `samples` down to stereo with `weights`.
pub(crate) fn mix_stereo(samples: &[f32], weights: &[(f32, f32)], out: &mut Vec<f32>) {
    out.clear();
    let n = weights.len();
    out.reserve(samples.len() / n * 2);
    for frame in samples.chunks_exact(n) {
        let (mut l, mut r) = (0.0, 0.0);
        for (x, (wl, wr)) in frame.iter().zip(weights) {
            l += x * wl;
            r += x * wr;
        }
        out.push(l);
        out.push(r);
    }
}

/// A sample as a signed integer of `bits` bits, rounded and clipped.
#[inline]
pub(crate) fn to_int(s: f32, bits: u32) -> i32 {
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    let v = (s * (max + 1.0)).round();
    v.clamp(-(max + 1.0), max) as i32
}

/// Samples as little-endian integers of `bits` bits (16 or 24), appended
/// to `out`.
pub(crate) fn pcm_bytes(samples: &[f32], bits: u32, out: &mut Vec<u8>) {
    out.reserve(samples.len() * bits as usize / 8);
    for &s in samples {
        let v = to_int(s, bits);
        match bits {
            24 => out.extend_from_slice(&v.to_le_bytes()[..3]),
            _ => out.extend_from_slice(&(v as i16).to_le_bytes()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChannelPosition::*;

    #[test]
    fn layouts_take_each_formats_order() {
        let surround = [FL, FR, FC, LFE, RL, RR];
        assert_eq!(wav_order(&surround), (Order::same(6), 0x3F));
        assert_eq!(wav_order(&[FR, FL]), (Order(vec![1, 0]), 0x3));
        assert_eq!(wav_order(&[FL, FL]).1, 0);
        assert_eq!(
            vorbis_order(&surround),
            (Order(vec![0, 2, 1, 4, 5, 3]), true)
        );
        let s71 = [FL, FR, FC, LFE, RL, RR, SL, SR];
        assert_eq!(
            vorbis_order(&s71),
            (Order(vec![0, 2, 1, 6, 7, 4, 5, 3]), true)
        );
        assert_eq!(vorbis_order(&[FC]), (Order(vec![0]), true));
        assert!(!vorbis_order(&[FL, FR, LFE]).1);

        let mut out = Vec::new();
        Order(vec![1, 0]).apply(&[1.0, 2.0, 3.0, 4.0], &mut out);
        assert_eq!(out, [2.0, 1.0, 4.0, 3.0]);
    }

    #[test]
    fn surround_mixes_down_without_clipping() {
        let w = stereo_weights(&[FL, FR, FC, LFE, RL, RR]);
        let mut out = Vec::new();
        mix_stereo(&[1.0; 6], &w, &mut out);
        assert!((out[0] - 1.0).abs() < 1e-6 && (out[1] - 1.0).abs() < 1e-6);
        // Stereo and mono pass as they are.
        assert_eq!(stereo_weights(&[FL, FR]), [(1.0, 0.0), (0.0, 1.0)]);
        assert_eq!(to_int(1.0, 16), 32767);
        assert_eq!(to_int(-1.0, 24), -8_388_608);
        let mut bytes = Vec::new();
        pcm_bytes(&[0.5, -0.5], 24, &mut bytes);
        assert_eq!(bytes, [0, 0, 0x40, 0, 0, 0xC0]);
    }
}
