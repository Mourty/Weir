//! Ogg Vorbis through libvorbis (the aoTuV tuning), built into the
//! program. Up to 7.1, in Vorbis channel order.
//!
//! libvorbis writes an Ogg stream. A `.ogg` file takes it as it is; for a
//! `.mka`, its pages are read back into Vorbis packets.

use crate::channels::{vorbis_order, Order};
use crate::TrackSpec;
use std::cell::RefCell;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::num::{NonZeroU32, NonZeroU8};
use std::rc::Rc;
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoder, VorbisEncoderBuilder};

fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("Ogg Vorbis: {e}"))
}

/// The quality that gives about this stereo bitrate. Vorbis scales it with
/// the channel count by itself.
fn quality(kbps: u32) -> f32 {
    match kbps {
        0..=128 => 0.4,
        129..=192 => 0.6,
        193..=256 => 0.8,
        _ => 0.9,
    }
}

/// Where libvorbis writes, read by whoever made it.
#[derive(Clone, Default)]
struct Shared(Rc<RefCell<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// An Ogg Vorbis stream from samples, as bytes.
pub(crate) struct VorbisStream {
    enc: Option<VorbisEncoder<Shared>>,
    sink: Shared,
    order: Order,
    planar: Vec<Vec<f32>>,
}

impl VorbisStream {
    pub fn new(track: &TrackSpec, rate: u32, kbps: u32) -> io::Result<Self> {
        let channels = track.positions.len();
        let sink = Shared::default();
        let rate = NonZeroU32::new(rate).ok_or_else(|| err("no sample rate"))?;
        let n = NonZeroU8::new(channels as u8).ok_or_else(|| err("no channels"))?;
        let enc = VorbisEncoderBuilder::new(rate, n, sink.clone())
            .map_err(err)?
            .bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
                target_quality: quality(kbps),
            })
            .build()
            .map_err(err)?;
        Ok(Self {
            enc: Some(enc),
            sink,
            order: vorbis_order(&track.positions).0,
            planar: vec![Vec::new(); channels],
        })
    }

    /// Add interleaved samples, in the track's channel order.
    pub fn push(&mut self, samples: &[f32]) -> io::Result<()> {
        let n = self.order.0.len();
        for (dest, &src) in self.planar.iter_mut().zip(&self.order.0) {
            dest.clear();
            dest.extend(samples.iter().skip(src).step_by(n));
        }
        if self.planar[0].is_empty() {
            return Ok(());
        }
        match self.enc.as_mut() {
            Some(enc) => enc.encode_audio_block(&self.planar).map_err(err),
            None => Ok(()),
        }
    }

    /// End the stream.
    pub fn finish(&mut self) -> io::Result<()> {
        if let Some(enc) = self.enc.take() {
            enc.finish().map_err(err)?;
        }
        Ok(())
    }

    /// The bytes written since the last call.
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.sink.0.borrow_mut())
    }
}

/// An `.ogg` file.
pub(crate) struct OggVorbis {
    file: BufWriter<File>,
    stream: VorbisStream,
    bytes: u64,
}

impl OggVorbis {
    pub fn new(file: File, track: &TrackSpec, rate: u32, kbps: u32) -> io::Result<Self> {
        let mut v = Self {
            file: BufWriter::with_capacity(1 << 16, file),
            stream: VorbisStream::new(track, rate, kbps)?,
            bytes: 0,
        };
        v.write_out()?;
        Ok(v)
    }

    fn write_out(&mut self) -> io::Result<()> {
        let bytes = self.stream.take();
        self.file.write_all(&bytes)?;
        self.bytes += bytes.len() as u64;
        Ok(())
    }

    pub fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        self.stream.push(samples)?;
        self.write_out()
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn finish(mut self) -> io::Result<u64> {
        self.stream.finish()?;
        self.write_out()?;
        let file = self.file.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(self.bytes)
    }
}

/// One Ogg page's worth of packets.
pub(crate) struct Page {
    /// The packets completed on the page.
    pub packets: Vec<Vec<u8>>,
    /// The sample position at the end of its last packet.
    pub granule: i64,
}

/// Reads an Ogg stream back into packets, page by page.
#[derive(Default)]
pub(crate) struct PageReader {
    pending: Vec<u8>,
    /// A packet carried on from one page to the next.
    partial: Vec<u8>,
}

impl PageReader {
    /// Add `bytes`, and take out every whole page.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Page>) -> io::Result<()> {
        self.pending.extend_from_slice(bytes);
        let mut at = 0;
        loop {
            let rest = &self.pending[at..];
            if rest.len() < 27 {
                break;
            }
            if &rest[..4] != b"OggS" {
                return Err(err("libvorbis wrote something that is not an Ogg page"));
            }
            let granule = i64::from_le_bytes(rest[6..14].try_into().expect("8 bytes"));
            let segments = rest[26] as usize;
            if rest.len() < 27 + segments {
                break;
            }
            let table = &rest[27..27 + segments];
            let body: usize = table.iter().map(|&s| s as usize).sum();
            if rest.len() < 27 + segments + body {
                break;
            }
            let mut page = Page {
                packets: Vec::new(),
                granule,
            };
            let mut p = 27 + segments;
            for &len in table {
                self.partial.extend_from_slice(&rest[p..p + len as usize]);
                p += len as usize;
                if len < 255 {
                    page.packets.push(std::mem::take(&mut self.partial));
                }
            }
            out.push(page);
            at += 27 + segments + body;
        }
        self.pending.drain(..at);
        Ok(())
    }
}
