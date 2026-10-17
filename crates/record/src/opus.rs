//! Opus through libopus, built into the program. Opus always runs at
//! 48 kHz, so other rates are resampled first. Surround uses Opus's
//! surround mapping (Vorbis channel order), as RFC 7845 describes.
//!
//! The same packets go into an Ogg Opus file or a `.mka`. The encoder
//! starts with `pre_skip` samples of its own, which players drop, and the
//! last packet is padded to a whole frame, which the container's end
//! (Ogg's last granule position, Matroska's discard padding) cuts off.

use crate::channels::vorbis_order;
use crate::channels::Order;
use crate::TrackSpec;
use ogg::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Bitrate, MSEncoder};
use rubato::{FftFixedIn, Resampler};
use std::fs::File;
use std::io::{self, BufWriter, Write};

/// Opus's sample rate.
pub(crate) const RATE: u32 = 48_000;
/// 20 ms, the usual Opus frame for music.
pub(crate) const FRAME: usize = 960;
/// The Ogg stream's serial number: "Weir".
const SERIAL: u32 = 0x5765_6972;

fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("Opus: {e}"))
}

/// Streams, coupled streams, the channel mapping and the mapping family
/// libopus uses for each layout: family 0 for mono and stereo, 1 for the
/// surround layouts Vorbis order knows, 255 (no meaning) for the rest.
fn mapping(channels: usize, known: bool) -> (u8, u8, Vec<u8>, u8) {
    match (channels, known) {
        (1, _) => (1, 0, vec![0], 0),
        (2, _) => (1, 1, vec![0, 1], 0),
        (4, true) => (2, 2, vec![0, 1, 2, 3], 1),
        (5, true) => (3, 2, vec![0, 4, 1, 2, 3], 1),
        (6, true) => (4, 2, vec![0, 4, 1, 2, 3, 5], 1),
        (8, true) => (5, 3, vec![0, 6, 1, 2, 3, 4, 5, 7], 1),
        (n, _) => (n as u8, 0, (0..n as u8).collect(), 255),
    }
}

/// Opus packets from samples.
pub(crate) struct OpusPackets {
    enc: MSEncoder,
    channels: usize,
    order: Order,
    resample: Option<Resample>,
    /// 48 kHz samples waiting for a whole frame, interleaved, Vorbis order.
    pending: Vec<f32>,
    /// Samples of the encoder's own at the start, which players drop.
    pub pre_skip: u64,
    /// 48 kHz frames of the track so far, padding left out.
    pub input: u64,
    /// Packets made so far.
    pub packets: u64,
    head: Vec<u8>,
    packet: Vec<u8>,
}

impl OpusPackets {
    /// `kbps` for a stereo track; more channels get more.
    pub fn new(track: &TrackSpec, rate: u32, kbps: u32) -> io::Result<Self> {
        let channels = track.positions.len();
        let (order, known) = vorbis_order(&track.positions);
        let (streams, coupled, map, family) = mapping(channels, known);
        let mut enc =
            MSEncoder::new(RATE, streams, coupled, &map, Application::Audio).map_err(err)?;
        let per_pair = kbps.max(16) * 1000;
        let bits = if channels <= 2 {
            per_pair
        } else {
            per_pair * channels as u32 / 2
        };
        enc.set_bitrate(Bitrate::Bits(bits as i32)).map_err(err)?;
        enc.set_vbr(true).map_err(err)?;
        let pre_skip = enc.get_lookahead().map_err(err)?.max(0) as u64;

        let mut head = Vec::with_capacity(32);
        head.extend_from_slice(b"OpusHead");
        head.push(1);
        head.push(channels as u8);
        head.extend_from_slice(&(pre_skip as u16).to_le_bytes());
        head.extend_from_slice(&rate.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes());
        head.push(family);
        if family != 0 {
            head.push(streams);
            head.push(coupled);
            head.extend_from_slice(&map);
        }
        let resample = if rate == RATE {
            None
        } else {
            Some(Resample::new(rate, channels)?)
        };
        Ok(Self {
            enc,
            channels,
            order,
            resample,
            pending: Vec::new(),
            pre_skip,
            input: 0,
            packets: 0,
            head,
            packet: vec![0; 4000 * streams as usize],
        })
    }

    /// The `OpusHead` packet, which starts an Ogg Opus stream and is a
    /// `.mka` track's private data.
    pub fn head(&self) -> &[u8] {
        &self.head
    }

    /// Queue 48 kHz samples in the track's order, and encode whole frames.
    fn queue(&mut self, samples: &[f32], out: &mut Vec<Vec<u8>>) -> io::Result<()> {
        let n = self.channels;
        self.input += (samples.len() / n) as u64;
        for f in samples.chunks_exact(n) {
            self.pending.extend(self.order.0.iter().map(|&i| f[i]));
        }
        let frame = FRAME * n;
        let full = self.pending.len() / frame * frame;
        if full == 0 {
            return Ok(());
        }
        let ready: Vec<f32> = self.pending.drain(..full).collect();
        for chunk in ready.chunks_exact(frame) {
            self.encode(chunk, out)?;
        }
        Ok(())
    }

    fn encode(&mut self, frame: &[f32], out: &mut Vec<Vec<u8>>) -> io::Result<()> {
        let len = self
            .enc
            .encode_float(frame, &mut self.packet)
            .map_err(err)?;
        out.push(self.packet[..len].to_vec());
        self.packets += 1;
        Ok(())
    }

    /// Add interleaved samples, in the track's channel order, at the
    /// track's rate; packets of [`FRAME`] samples each come out in `out`.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<Vec<u8>>) -> io::Result<()> {
        match self.resample.as_mut() {
            Some(r) => {
                let resampled = r.push(samples)?;
                self.queue(&resampled, out)
            }
            None => self.queue(samples, out),
        }
    }

    /// The last packets: what is queued, padded with silence, and enough
    /// silence after it for the encoder's delay to come out.
    pub fn finish(&mut self, out: &mut Vec<Vec<u8>>) -> io::Result<()> {
        if let Some(mut r) = self.resample.take() {
            let rest = r.finish()?;
            self.queue(&rest, out)?;
        }
        let n = self.channels;
        let wanted = self.input + self.pre_skip;
        let mut last = std::mem::take(&mut self.pending);
        while self.packets * (FRAME as u64) < wanted {
            last.resize(FRAME * n, 0.0);
            self.encode(&last, out)?;
            last.clear();
        }
        Ok(())
    }

    /// Samples the last packet holds past the end of the track, which
    /// players should drop: only right after `finish`.
    pub fn end_padding(&self) -> u64 {
        self.packets * FRAME as u64 - self.pre_skip - self.input
    }
}

/// An Ogg Opus file.
pub(crate) struct OggOpus {
    ogg: PacketWriter<'static, BufWriter<File>>,
    opus: OpusPackets,
    out: Vec<Vec<u8>>,
    /// Packets written: the next one's index.
    written: u64,
    bytes: u64,
}

impl OggOpus {
    pub fn new(file: File, track: &TrackSpec, rate: u32, kbps: u32) -> io::Result<Self> {
        let opus = OpusPackets::new(track, rate, kbps)?;
        let mut ogg = PacketWriter::new(BufWriter::with_capacity(1 << 16, file));
        let head = opus.head().to_vec();
        let mut bytes = head.len() as u64;
        ogg.write_packet(head, SERIAL, PacketWriteEndInfo::EndPage, 0)?;
        let vendor = format!("Weir (libopus {})", opus::version());
        let mut tags = Vec::new();
        tags.extend_from_slice(b"OpusTags");
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor.as_bytes());
        tags.extend_from_slice(&0u32.to_le_bytes());
        bytes += tags.len() as u64;
        ogg.write_packet(tags, SERIAL, PacketWriteEndInfo::EndPage, 0)?;
        Ok(Self {
            ogg,
            opus,
            out: Vec::new(),
            written: 0,
            bytes,
        })
    }

    /// Write the packets made, the last of all `end` (then with the
    /// track's true end as its granule position).
    fn write_packets(&mut self, end: bool) -> io::Result<()> {
        let count = self.out.len();
        for (i, p) in self.out.drain(..).enumerate() {
            self.written += 1;
            self.bytes += p.len() as u64;
            let last = end && i + 1 == count;
            // A granule position counts 48 kHz samples decoded, the
            // encoder's own at the start included.
            let (granule, info) = if last {
                (
                    self.opus.pre_skip + self.opus.input,
                    PacketWriteEndInfo::EndStream,
                )
            } else {
                (
                    self.written * FRAME as u64,
                    PacketWriteEndInfo::NormalPacket,
                )
            };
            self.ogg.write_packet(p, SERIAL, info, granule)?;
        }
        Ok(())
    }

    pub fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        self.opus.push(samples, &mut self.out)?;
        self.write_packets(false)
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn finish(mut self) -> io::Result<u64> {
        self.opus.finish(&mut self.out)?;
        self.write_packets(true)?;
        let mut file = self.ogg.into_inner();
        file.flush()?;
        let file = file.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(file.metadata()?.len())
    }
}

/// Resampling to 48 kHz, keeping the exact length.
struct Resample {
    r: FftFixedIn<f32>,
    channels: usize,
    rate: u32,
    /// Input waiting for a full chunk, one list per channel.
    input: Vec<Vec<f32>>,
    /// Output frames still to drop: the resampler's delay.
    skip: usize,
    frames_in: u64,
    frames_out: u64,
}

impl Resample {
    fn new(rate: u32, channels: usize) -> io::Result<Self> {
        let r =
            FftFixedIn::<f32>::new(rate as usize, RATE as usize, 1024, 2, channels).map_err(err)?;
        let skip = r.output_delay();
        Ok(Self {
            r,
            channels,
            rate,
            input: vec![Vec::new(); channels],
            skip,
            frames_in: 0,
            frames_out: 0,
        })
    }

    fn push(&mut self, samples: &[f32]) -> io::Result<Vec<f32>> {
        let n = self.channels;
        for (c, ch) in self.input.iter_mut().enumerate() {
            ch.extend(samples.iter().skip(c).step_by(n));
        }
        self.frames_in += (samples.len() / n) as u64;
        let mut out = Vec::new();
        while self.input[0].len() >= self.r.input_frames_next() {
            let need = self.r.input_frames_next();
            let chunk: Vec<Vec<f32>> = self
                .input
                .iter_mut()
                .map(|c| c.drain(..need).collect())
                .collect();
            let res = self.r.process(&chunk, None).map_err(err)?;
            self.emit(&res, &mut out, u64::MAX);
        }
        Ok(out)
    }

    fn finish(&mut self) -> io::Result<Vec<f32>> {
        let wanted = (self.frames_in * RATE as u64 + self.rate as u64 / 2) / self.rate as u64;
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.input);
        if !rest[0].is_empty() {
            let res = self.r.process_partial(Some(&rest), None).map_err(err)?;
            self.emit(&res, &mut out, wanted);
        }
        // Let the delay out.
        let mut guard = 0;
        while self.frames_out < wanted && guard < 16 {
            let res = self
                .r
                .process_partial::<Vec<f32>>(None, None)
                .map_err(err)?;
            self.emit(&res, &mut out, wanted);
            guard += 1;
        }
        Ok(out)
    }

    /// Interleave planar output into `out`, dropping the delay and anything
    /// past `limit` frames.
    fn emit(&mut self, planar: &[Vec<f32>], out: &mut Vec<f32>, limit: u64) {
        let frames = planar.first().map_or(0, Vec::len);
        for i in 0..frames {
            if self.skip > 0 {
                self.skip -= 1;
                continue;
            }
            if self.frames_out >= limit {
                return;
            }
            for ch in planar {
                out.push(ch[i]);
            }
            self.frames_out += 1;
        }
    }
}
