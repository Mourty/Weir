//! FLAC, encoded a frame at a time with the pure-Rust `flacenc`, so a long
//! recording never has to be held in memory. The same frames go into a
//! `.flac` file or a `.mka`. The stream's description (length, checksum)
//! is only known at the end, so it is written again then.

use crate::channels::{to_int, wav_order, Order};
use crate::TrackSpec;
use flacenc::bitsink::MemSink;
use flacenc::component::{BitRepr, StreamInfo};
use flacenc::config;
use flacenc::error::Verify;
use flacenc::source::{Context, Fill, FrameBuf};
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};

/// Samples per channel in each FLAC frame, as the reference encoder uses.
pub(crate) const BLOCK: usize = 4096;

fn err(e: impl std::fmt::Debug) -> io::Error {
    io::Error::other(format!("FLAC: {e:?}"))
}

/// FLAC frames from samples.
pub(crate) struct FlacFrames {
    config: flacenc::error::Verified<config::Encoder>,
    info: StreamInfo,
    frame: FrameBuf,
    context: Context,
    order: Order,
    channels: usize,
    bits: u32,
    /// Samples waiting for a whole frame, interleaved in FLAC's order.
    pending: Vec<i32>,
    ordered: Vec<f32>,
    sink: MemSink<u8>,
}

impl FlacFrames {
    pub fn new(track: &TrackSpec, rate: u32, bits: u32) -> io::Result<Self> {
        let channels = track.positions.len();
        let info = StreamInfo::new(rate as usize, channels, bits as usize).map_err(err)?;
        let config = config::Encoder::default().into_verified().map_err(err)?;
        Ok(Self {
            config,
            info,
            frame: FrameBuf::with_size(channels, BLOCK).map_err(err)?,
            context: Context::new(bits as usize, channels),
            order: wav_order(&track.positions).0,
            channels,
            bits,
            pending: Vec::with_capacity(BLOCK * channels),
            ordered: Vec::new(),
            sink: MemSink::new(),
        })
    }

    fn encode(&mut self, samples: &[i32], out: &mut Vec<Vec<u8>>) -> io::Result<()> {
        let mut fill = (&mut self.frame, &mut self.context);
        fill.fill_interleaved(samples).map_err(err)?;
        let number = self.context.current_frame_number().unwrap_or(0);
        let frame = flacenc::encode_fixed_size_frame(&self.config, &self.frame, number, &self.info)
            .map_err(err)?;
        self.info.update_frame_info(&frame);
        self.sink.clear();
        frame.write(&mut self.sink).map_err(err)?;
        out.push(self.sink.as_slice().to_vec());
        Ok(())
    }

    /// Add interleaved samples, in the track's channel order; whole frames
    /// come out in `out`, each [`BLOCK`] samples long.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<Vec<u8>>) -> io::Result<()> {
        self.order.apply(samples, &mut self.ordered);
        let bits = self.bits;
        self.pending
            .extend(self.ordered.iter().map(|&s| to_int(s, bits)));
        let block = BLOCK * self.channels;
        if self.pending.len() >= block {
            let full = self.pending.len() / block * block;
            let ready: Vec<i32> = self.pending.drain(..full).collect();
            for chunk in ready.chunks_exact(block) {
                self.encode(chunk, out)?;
            }
        }
        Ok(())
    }

    /// The last, shorter frame, if anything is left.
    pub fn finish(&mut self, out: &mut Vec<Vec<u8>>) -> io::Result<()> {
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            self.encode(&rest, out)?;
        }
        // Every frame but the last is BLOCK long, which is what the minimum
        // block size says: decoders take min ≠ max as a variable block size.
        self.info.set_block_sizes(BLOCK, BLOCK).map_err(err)?;
        if self.info.max_frame_size() == 0 {
            // No audio at all: still a valid, empty stream.
            self.info.set_frame_sizes(0, 0).map_err(err)?;
        }
        self.info.set_md5_digest(&self.context.md5_digest());
        self.info.set_total_samples(self.context.total_samples());
        Ok(())
    }

    /// The stream's description as the only metadata block: its header
    /// (last block, type 0, 34 bytes) and the description itself. The same
    /// length before and after `finish`.
    pub fn stream_info(&self) -> io::Result<Vec<u8>> {
        let mut sink = MemSink::<u8>::new();
        self.info.write(&mut sink).map_err(err)?;
        let bytes = sink.as_slice();
        let mut out = vec![0x80, 0, 0, bytes.len() as u8];
        out.extend_from_slice(bytes);
        Ok(out)
    }
}

/// A `.flac` file.
pub(crate) struct FlacFile {
    file: BufWriter<File>,
    frames: FlacFrames,
    out: Vec<Vec<u8>>,
    bytes: u64,
}

impl FlacFile {
    pub fn new(file: File, track: &TrackSpec, rate: u32, bits: u32) -> io::Result<Self> {
        let frames = FlacFrames::new(track, rate, bits)?;
        let mut file = BufWriter::with_capacity(1 << 16, file);
        file.write_all(b"fLaC")?;
        let info = frames.stream_info()?;
        file.write_all(&info)?;
        Ok(Self {
            file,
            frames,
            out: Vec::new(),
            bytes: 4 + info.len() as u64,
        })
    }

    fn write_frames(&mut self) -> io::Result<()> {
        for f in self.out.drain(..) {
            self.file.write_all(&f)?;
            self.bytes += f.len() as u64;
        }
        Ok(())
    }

    pub fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        self.frames.push(samples, &mut self.out)?;
        self.write_frames()
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn finish(mut self) -> io::Result<u64> {
        self.frames.finish(&mut self.out)?;
        self.write_frames()?;
        self.file.seek(SeekFrom::Start(4))?;
        self.file.write_all(&self.frames.stream_info()?)?;
        let file = self.file.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(self.bytes)
    }
}
