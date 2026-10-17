//! MP3 through LAME, built into the program: constant bitrate at LAME's
//! own default quality, which is as good as its slowest at these bitrates.
//! MP3 holds stereo at most, so more channels are mixed down first.
//!
//! In a `.mp3` file, LAME's tag in the first frame tells players the exact
//! length. In a `.mka` there is no tag, and LAME's stream is cut into its
//! frames, each with its time.

use crate::channels::{mix_stereo, stereo_weights};
use crate::TrackSpec;
use mp3lame_encoder::{Bitrate, Builder, FlushGap, InterleavedPcm, MonoPcm, Quality};
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};

fn err(e: impl std::fmt::Debug) -> io::Error {
    io::Error::other(format!("MP3: {e:?}"))
}

fn bitrate(kbps: u32) -> Bitrate {
    match kbps {
        0..=128 => Bitrate::Kbps128,
        129..=192 => Bitrate::Kbps192,
        193..=256 => Bitrate::Kbps256,
        _ => Bitrate::Kbps320,
    }
}

/// LAME, fed a track's samples.
pub(crate) struct Mp3Encoder {
    lame: mp3lame_encoder::Encoder,
    /// How to mix more than two channels down, when there are more.
    weights: Option<Vec<(f32, f32)>>,
    mono: bool,
    mixed: Vec<f32>,
}

impl Mp3Encoder {
    /// `tag`: leave room for LAME's tag at the start, for a `.mp3` file.
    pub fn new(track: &TrackSpec, rate: u32, kbps: u32, tag: bool) -> io::Result<Self> {
        let channels = track.positions.len();
        let mono = channels == 1;
        let mut b = Builder::new().ok_or_else(|| err("LAME could not start"))?;
        b.set_num_channels(if mono { 1 } else { 2 }).map_err(err)?;
        b.set_sample_rate(rate).map_err(err)?;
        b.set_brate(bitrate(kbps)).map_err(err)?;
        b.set_quality(Quality::VeryNice).map_err(err)?;
        b.set_to_write_vbr_tag(tag).map_err(err)?;
        Ok(Self {
            lame: b.build().map_err(err)?,
            weights: (channels > 2).then(|| stereo_weights(&track.positions)),
            mono,
            mixed: Vec::new(),
        })
    }

    /// Encode interleaved samples, in the track's channel order, appending
    /// MP3 data to `out`.
    pub fn encode(&mut self, samples: &[f32], out: &mut Vec<u8>) -> io::Result<()> {
        let pcm: &[f32] = match &self.weights {
            Some(w) => {
                mix_stereo(samples, w, &mut self.mixed);
                &self.mixed
            }
            None => samples,
        };
        let frames = if self.mono { pcm.len() } else { pcm.len() / 2 };
        out.reserve(mp3lame_encoder::max_required_buffer_size(frames));
        if self.mono {
            self.lame.encode_to_vec(MonoPcm(pcm), out).map_err(err)?;
        } else {
            self.lame
                .encode_to_vec(InterleavedPcm(pcm), out)
                .map_err(err)?;
        }
        Ok(())
    }

    /// What LAME still holds, appended to `out`.
    pub fn flush(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        out.reserve(7200);
        self.lame.flush_to_vec::<FlushGap>(out).map_err(err)?;
        Ok(())
    }
}

/// A `.mp3` file.
pub(crate) struct Mp3File {
    file: BufWriter<File>,
    lame: Mp3Encoder,
    buf: Vec<u8>,
    bytes: u64,
}

impl Mp3File {
    pub fn new(file: File, track: &TrackSpec, rate: u32, kbps: u32) -> io::Result<Self> {
        Ok(Self {
            file: BufWriter::with_capacity(1 << 16, file),
            lame: Mp3Encoder::new(track, rate, kbps, true)?,
            buf: Vec::new(),
            bytes: 0,
        })
    }

    pub fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        self.buf.clear();
        self.lame.encode(samples, &mut self.buf)?;
        self.file.write_all(&self.buf)?;
        self.bytes += self.buf.len() as u64;
        Ok(())
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn finish(mut self) -> io::Result<u64> {
        self.buf.clear();
        self.lame.flush(&mut self.buf)?;
        self.file.write_all(&self.buf)?;
        self.bytes += self.buf.len() as u64;
        // The tag goes in the first frame, which LAME left room for.
        let lame = &mut self.lame.lame;
        let mut tag = Vec::with_capacity(lame.lame_tag_size());
        if lame.lame_tag_encode_to_vec(&mut tag).is_some() {
            let at = lame.id3v2_tag_size() as u64;
            self.file.seek(SeekFrom::Start(at))?;
            self.file.write_all(&tag)?;
        }
        let file = self.file.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(self.bytes)
    }
}

/// One MP3 frame's length in bytes, samples and sample rate, read from its
/// four-byte header; `None` when it is not a Layer III header.
pub(crate) fn frame_header(h: &[u8]) -> Option<(usize, u32, u32)> {
    if h.len() < 4 || h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (h[1] >> 3) & 3; // 3: MPEG-1, 2: MPEG-2, 0: MPEG-2.5
    let layer = (h[1] >> 1) & 3; // 1: Layer III
    if version == 1 || layer != 1 {
        return None;
    }
    let bitrate_index = (h[2] >> 4) as usize;
    let rate_index = ((h[2] >> 2) & 3) as usize;
    if bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 {
        return None;
    }
    const MPEG1: [u32; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    const MPEG2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let (kbps, rate, samples) = match version {
        3 => (
            MPEG1[bitrate_index],
            [44_100, 48_000, 32_000][rate_index],
            1152,
        ),
        2 => (
            MPEG2[bitrate_index],
            [22_050, 24_000, 16_000][rate_index],
            576,
        ),
        _ => (
            MPEG2[bitrate_index],
            [11_025, 12_000, 8_000][rate_index],
            576,
        ),
    };
    let padding = ((h[2] >> 1) & 1) as usize;
    let len = (samples / 8 * kbps * 1000 / rate) as usize + padding;
    Some((len, samples, rate))
}

/// Cuts an MP3 stream into its frames.
#[derive(Default)]
pub(crate) struct FrameCutter {
    pending: Vec<u8>,
}

/// One MP3 frame: its bytes, and how many samples it holds at what rate.
pub(crate) struct Mp3Frame {
    pub data: Vec<u8>,
    pub samples: u32,
    pub rate: u32,
}

impl FrameCutter {
    /// Add `bytes`, and take out every whole frame.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Mp3Frame>) {
        self.pending.extend_from_slice(bytes);
        let mut at = 0;
        while self.pending.len() - at >= 4 {
            match frame_header(&self.pending[at..]) {
                Some((len, samples, rate)) => {
                    if self.pending.len() - at < len {
                        break;
                    }
                    out.push(Mp3Frame {
                        data: self.pending[at..at + len].to_vec(),
                        samples,
                        rate,
                    });
                    at += len;
                }
                // Not a frame: LAME writes nothing else, but never stick.
                None => at += 1,
            }
        }
        self.pending.drain(..at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_headers_give_length_and_rate() {
        // MPEG-1 Layer III, 128 kbps, 44.1 kHz, no padding: 417 bytes.
        assert_eq!(
            frame_header(&[0xFF, 0xFB, 0x90, 0x00]),
            Some((417, 1152, 44_100))
        );
        // 192 kbps at 48 kHz, padded: 577 bytes.
        assert_eq!(
            frame_header(&[0xFF, 0xFB, 0xB6, 0x00]),
            Some((577, 1152, 48_000))
        );
        assert_eq!(frame_header(&[0x49, 0x44, 0x33, 0x04]), None);
    }
}
