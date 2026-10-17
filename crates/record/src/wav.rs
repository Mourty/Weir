//! WAV files: 16- or 24-bit PCM. More than two channels, or 24 bits, use
//! the extensible header with a channel mask so players put each channel
//! on the right speaker. Optionally a Broadcast Wave `bext` chunk says
//! when the file starts.
//!
//! The header's sizes are brought up to date every few seconds, so a file
//! cut short (the computer losing power, say) still plays up to there.

use crate::channels::{pcm_bytes, wav_order, Order};
use crate::{Bwf, TrackSpec};
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};

/// The most audio a WAV file can hold: its sizes are 32 bits.
pub(crate) const WAV_MAX_DATA: u64 = 0xFFFF_0000;
/// How often the header's sizes are updated, in seconds of audio.
const HEADER_EVERY_SECONDS: u64 = 5;

pub(crate) struct Wav {
    file: BufWriter<File>,
    order: Order,
    bits: u32,
    channels: usize,
    rate: u32,
    mask: u32,
    extensible: bool,
    bext: Option<Vec<u8>>,
    data_bytes: u64,
    /// `data_bytes` when the header was last written.
    header_at: u64,
    buf: Vec<u8>,
    ordered: Vec<f32>,
}

impl Wav {
    pub fn new(
        file: File,
        track: &TrackSpec,
        rate: u32,
        bits: u32,
        bwf: Option<&Bwf>,
    ) -> io::Result<Self> {
        let (order, mask) = wav_order(&track.positions);
        let channels = track.positions.len();
        let mut w = Self {
            file: BufWriter::with_capacity(1 << 16, file),
            order,
            bits,
            channels,
            rate,
            mask,
            extensible: channels > 2 || bits > 16,
            bext: bwf.map(|b| bext(b, &track.name, rate)),
            data_bytes: 0,
            header_at: 0,
            buf: Vec::new(),
            ordered: Vec::new(),
        };
        w.write_header()?;
        Ok(w)
    }

    fn write_header(&mut self) -> io::Result<()> {
        let channels = self.channels as u16;
        let block_align = channels * self.bits as u16 / 8;
        let fmt_len: u32 = if self.extensible { 40 } else { 16 };
        let data = self.data_bytes.min(WAV_MAX_DATA) as u32;
        let bext_len = self.bext.as_ref().map_or(0, |b| 8 + b.len() as u32);
        let riff_len = 4 + (8 + fmt_len) + bext_len + (8 + data) + (data & 1);
        let w = &mut self.file;
        w.write_all(b"RIFF")?;
        w.write_all(&riff_len.to_le_bytes())?;
        w.write_all(b"WAVE")?;
        w.write_all(b"fmt ")?;
        w.write_all(&fmt_len.to_le_bytes())?;
        let format_tag: u16 = if self.extensible { 0xFFFE } else { 1 };
        w.write_all(&format_tag.to_le_bytes())?;
        w.write_all(&channels.to_le_bytes())?;
        w.write_all(&self.rate.to_le_bytes())?;
        w.write_all(&(self.rate * block_align as u32).to_le_bytes())?;
        w.write_all(&block_align.to_le_bytes())?;
        w.write_all(&(self.bits as u16).to_le_bytes())?;
        if self.extensible {
            w.write_all(&22u16.to_le_bytes())?;
            w.write_all(&(self.bits as u16).to_le_bytes())?; // valid bits
            w.write_all(&self.mask.to_le_bytes())?;
            // KSDATAFORMAT_SUBTYPE_PCM
            w.write_all(&[
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38,
                0x9B, 0x71,
            ])?;
        }
        if let Some(b) = &self.bext {
            w.write_all(b"bext")?;
            w.write_all(&(b.len() as u32).to_le_bytes())?;
            w.write_all(b)?;
        }
        w.write_all(b"data")?;
        w.write_all(&data.to_le_bytes())?;
        Ok(())
    }

    /// Bring the header's sizes up to date, and carry on at the end.
    fn update_header(&mut self) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        self.write_header()?;
        self.file.seek(SeekFrom::End(0))?;
        self.header_at = self.data_bytes;
        Ok(())
    }

    /// Add interleaved samples, in the track's channel order.
    pub fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        self.order.apply(samples, &mut self.ordered);
        self.buf.clear();
        pcm_bytes(&self.ordered, self.bits, &mut self.buf);
        self.file.write_all(&self.buf)?;
        self.data_bytes += self.buf.len() as u64;
        let every =
            HEADER_EVERY_SECONDS * self.rate as u64 * self.channels as u64 * self.bits as u64 / 8;
        if self.data_bytes - self.header_at >= every {
            self.update_header()?;
        }
        Ok(())
    }

    pub fn bytes(&self) -> u64 {
        self.data_bytes
    }

    pub fn finish(mut self) -> io::Result<u64> {
        if self.data_bytes % 2 == 1 {
            self.file.write_all(&[0])?;
        }
        self.update_header()?;
        let file = self.file.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(file.metadata()?.len())
    }
}

/// A `bext` chunk (EBU Tech 3285, version 1): who wrote the file, when it
/// starts, and its start as samples since midnight, which editors line
/// files up by.
fn bext(bwf: &Bwf, track: &str, rate: u32) -> Vec<u8> {
    fn field(out: &mut Vec<u8>, text: &str, len: usize) {
        let bytes: Vec<u8> = text
            .bytes()
            .filter(|b| b.is_ascii() && *b >= 0x20)
            .collect();
        let n = bytes.len().min(len);
        out.extend_from_slice(&bytes[..n]);
        out.resize(out.len() + len - n, 0);
    }
    let t = &bwf.start;
    let since_midnight = (t.hour as u64 * 3600 + t.minute as u64 * 60 + t.second as u64)
        * rate as u64
        + bwf.start_ms_fraction as u64 * rate as u64 / 1000;
    let mut b = Vec::with_capacity(602);
    field(&mut b, &format!("{track}, recorded by Weir"), 256); // Description
    field(&mut b, "Weir", 32); // Originator
    field(&mut b, &bwf.reference, 32); // OriginatorReference
    field(&mut b, &t.date(), 10); // OriginationDate
    field(
        &mut b,
        &format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second),
        8,
    ); // OriginationTime
    b.extend_from_slice(&since_midnight.to_le_bytes()); // TimeReference, low then high
    b.extend_from_slice(&1u16.to_le_bytes()); // Version
    b.resize(b.len() + 64, 0); // UMID: none
    b.resize(b.len() + 190, 0); // Reserved
    debug_assert_eq!(b.len(), 602);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use weir_protocol::LocalTime;

    #[test]
    fn bext_says_when_the_file_starts() {
        let bwf = Bwf {
            start: LocalTime {
                year: 2026,
                month: 10,
                day: 17,
                hour: 1,
                minute: 2,
                second: 3,
            },
            start_ms_fraction: 500,
            reference: "W1".into(),
        };
        let b = bext(&bwf, "Mic", 48_000);
        assert_eq!(b.len(), 602);
        assert_eq!(&b[..23], b"Mic, recorded by Weir\0\0");
        assert_eq!(&b[320..330], b"2026-10-17");
        assert_eq!(&b[330..338], b"01:02:03");
        let at = u64::from_le_bytes(b[338..346].try_into().unwrap());
        assert_eq!(at, 3723 * 48_000 + 24_000);
        assert_eq!(u16::from_le_bytes([b[346], b[347]]), 1);
    }
}
