//! Matroska audio: every track of a recording in one `.mka` file, the way
//! OBS keeps several audio tracks in one recording.
//!
//! Matroska is EBML: elements of an id, a size and a payload, nested. The
//! file is an EBML header and a Segment holding, in order: room for a
//! SeekHead (filled in at the end, pointing at the rest), Info (with the
//! duration, filled in at the end), Tracks, Clusters of about a second
//! each, holding every track's blocks for that second in time order, and
//! Cues, one per cluster, for seeking. The Segment's size is written as
//! unknown until the end, so a file cut short still plays.
//!
//! Each track is encoded as the recording's format would be on its own:
//! PCM for WAV, FLAC frames, MP3 frames, Opus packets, Vorbis packets.
//! Nothing is written until every track can say what it holds (LAME's
//! output rate, Vorbis's headers), so the start of the file is written
//! with the first cluster.

use crate::channels::{pcm_bytes, wav_order, Order};
use crate::flac::{FlacFrames, BLOCK};
use crate::mp3::{FrameCutter, Mp3Encoder, Mp3Frame};
use crate::opus::{OpusPackets, FRAME as OPUS_FRAME, RATE as OPUS_RATE};
use crate::vorbis::{Page, PageReader, VorbisStream};
use crate::TrackSpec;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use weir_protocol::RecordFormat;

// Element ids.
const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
const VOID: u32 = 0xEC;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const CODEC_PRIVATE: u32 = 0x63A2;
const CLUSTER: u32 = 0x1F43_B675;
const CUES: u32 = 0x1C53_BB6B;

/// Nanoseconds in a millisecond, the timestamp scale used.
const MS: u64 = 1_000_000;
/// How long a cluster holds, in milliseconds.
const CLUSTER_MS: u64 = 1000;
/// Room kept for the SeekHead at the start of the segment.
const SEEK_ROOM: usize = 160;
/// Samples LAME's MP3s start with before the sound: its own delay and
/// the decoder's, as LAME's tag gives them to players.
const MP3_DELAY: u64 = 576 + 529;
/// Milliseconds from the Unix epoch to 2001-01-01, Matroska's own epoch.
const MATROSKA_EPOCH_MS: i64 = 978_307_200_000;

/// An element id as its bytes: ids carry their own length.
fn id_bytes(id: u32) -> Vec<u8> {
    let b = id.to_be_bytes();
    let skip = b.iter().take_while(|&&x| x == 0).count();
    b[skip..].to_vec()
}

/// A size as an EBML variable-length integer, as short as it can be.
fn vint(n: u64) -> Vec<u8> {
    let len = (1..=8usize)
        .find(|l| n < (1u64 << (7 * l)) - 1)
        .unwrap_or(8);
    let mut out = vec![0u8; len];
    for (i, b) in out.iter_mut().enumerate() {
        *b = (n >> (8 * (len - 1 - i))) as u8;
    }
    out[0] |= 1 << (8 - len);
    out
}

/// A size as an eight-byte EBML integer, for a size written again later.
fn vint8(n: u64) -> [u8; 8] {
    let mut out = n.to_be_bytes();
    out[0] = 0x01;
    out
}

/// An element's id and size, before its payload of `len` bytes.
fn head(id: u32, len: usize) -> Vec<u8> {
    let mut out = id_bytes(id);
    out.extend_from_slice(&vint(len as u64));
    out
}

fn el(out: &mut Vec<u8>, id: u32, payload: &[u8]) {
    out.extend_from_slice(&head(id, payload.len()));
    out.extend_from_slice(payload);
}

fn el_uint(out: &mut Vec<u8>, id: u32, v: u64) {
    let b = v.to_be_bytes();
    let skip = b.iter().take_while(|&&x| x == 0).count().min(7);
    el(out, id, &b[skip..]);
}

fn el_float(out: &mut Vec<u8>, id: u32, v: f64) {
    el(out, id, &v.to_be_bytes());
}

fn el_str(out: &mut Vec<u8>, id: u32, s: &str) {
    el(out, id, s.as_bytes());
}

/// Headers laced together the way Xiph does, as a Vorbis track's private
/// data: how many less one, then each size but the last.
fn xiph_lace(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![parts.len().saturating_sub(1) as u8];
    for p in &parts[..parts.len().saturating_sub(1)] {
        let mut n = p.len();
        while n >= 255 {
            out.push(255);
            n -= 255;
        }
        out.push(n as u8);
    }
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

/// A block of one track: one or more frames laced together, from `ts`.
struct Block {
    ts_ns: u64,
    frames: Vec<Vec<u8>>,
    /// Nanoseconds at its end that players drop: the last Opus packet's
    /// padding.
    discard_ns: Option<u64>,
}

/// How a track turns samples into blocks.
enum Codec {
    Pcm {
        order: Order,
        bits: u32,
    },
    Flac {
        flac: FlacFrames,
        /// Frames made so far.
        made: u64,
    },
    Mp3 {
        lame: Mp3Encoder,
        cutter: FrameCutter,
        /// Samples of the frames cut so far, at LAME's rate.
        samples: u64,
        /// The rate LAME writes at, once its first frame says.
        rate: Option<u32>,
    },
    Opus {
        opus: OpusPackets,
        /// Packets made so far.
        made: u64,
    },
    Vorbis {
        stream: VorbisStream,
        reader: PageReader,
        headers: Vec<Vec<u8>>,
        /// The sample position the next page's packets start at.
        granule: i64,
    },
}

/// What a track's entry says about its codec.
struct Description {
    codec_id: &'static str,
    private: Option<Vec<u8>>,
    /// Nanoseconds at the start that players drop.
    delay: Option<u64>,
    /// Nanoseconds to decode before a point sought to.
    preroll: Option<u64>,
    rate: u32,
    bits: Option<u32>,
}

struct Track {
    spec: TrackSpec,
    rate: u32,
    codec: Codec,
    /// Input frames so far, at the track's rate.
    frames: u64,
    queue: VecDeque<Block>,
    /// How far in time the blocks made so far reach.
    end_ns: u64,
    /// Where its private data is in the file: FLAC's is written again at
    /// the end.
    private_at: Option<u64>,
    scratch: Vec<f32>,
}

fn ns(frames: u64, rate: u32) -> u64 {
    (frames as u128 * 1_000_000_000 / rate.max(1) as u128) as u64
}

impl Track {
    fn new(spec: &TrackSpec, format: RecordFormat, rate: u32, quality: u32) -> io::Result<Self> {
        let codec = match format {
            RecordFormat::Wav => Codec::Pcm {
                order: wav_order(&spec.positions).0,
                bits: quality,
            },
            RecordFormat::Flac => Codec::Flac {
                flac: FlacFrames::new(spec, rate, quality)?,
                made: 0,
            },
            RecordFormat::Mp3 => Codec::Mp3 {
                lame: Mp3Encoder::new(spec, rate, quality, false)?,
                cutter: FrameCutter::default(),
                samples: 0,
                rate: None,
            },
            RecordFormat::Opus => Codec::Opus {
                opus: OpusPackets::new(spec, rate, quality)?,
                made: 0,
            },
            RecordFormat::Vorbis => Codec::Vorbis {
                stream: VorbisStream::new(spec, rate, quality)?,
                reader: PageReader::default(),
                headers: Vec::new(),
                granule: 0,
            },
        };
        let mut t = Self {
            spec: spec.clone(),
            rate,
            codec,
            frames: 0,
            queue: VecDeque::new(),
            end_ns: 0,
            private_at: None,
            scratch: Vec::new(),
        };
        // libvorbis writes its headers at once.
        t.vorbis_pages()?;
        Ok(t)
    }

    /// Encode interleaved samples, in the track's channel order, into
    /// blocks.
    fn push(&mut self, samples: &[f32]) -> io::Result<()> {
        let n = self.spec.positions.len();
        let start = self.frames;
        self.frames += (samples.len() / n) as u64;
        match &mut self.codec {
            Codec::Pcm { order, bits } => {
                if samples.is_empty() {
                    return Ok(());
                }
                order.apply(samples, &mut self.scratch);
                let mut data = Vec::new();
                pcm_bytes(&self.scratch, *bits, &mut data);
                self.queue.push_back(Block {
                    ts_ns: ns(start, self.rate),
                    frames: vec![data],
                    discard_ns: None,
                });
                self.end_ns = ns(self.frames, self.rate);
            }
            Codec::Flac { flac, .. } => {
                let mut out = Vec::new();
                flac.push(samples, &mut out)?;
                self.flac_frames(out, false);
            }
            Codec::Mp3 { lame, .. } => {
                let mut bytes = Vec::new();
                lame.encode(samples, &mut bytes)?;
                self.mp3_frames(&bytes);
            }
            Codec::Opus { opus, .. } => {
                let mut out = Vec::new();
                opus.push(samples, &mut out)?;
                self.opus_packets(out, false);
            }
            Codec::Vorbis { stream, .. } => {
                stream.push(samples)?;
                self.vorbis_pages()?;
            }
        }
        Ok(())
    }

    /// Encode what is left.
    fn finish(&mut self) -> io::Result<()> {
        match &mut self.codec {
            Codec::Pcm { .. } => {}
            Codec::Flac { flac, .. } => {
                let mut out = Vec::new();
                flac.finish(&mut out)?;
                self.flac_frames(out, true);
            }
            Codec::Mp3 { lame, .. } => {
                let mut bytes = Vec::new();
                lame.flush(&mut bytes)?;
                self.mp3_frames(&bytes);
                // What the last frame holds past the end.
                if let Codec::Mp3 {
                    samples,
                    rate: Some(rate),
                    ..
                } = &self.codec
                {
                    let input = self.frames * *rate as u64 / self.rate.max(1) as u64;
                    let padding = samples.saturating_sub(MP3_DELAY + input);
                    if let Some(last) = self.queue.back_mut().filter(|_| padding > 0) {
                        last.discard_ns = Some(ns(padding, *rate));
                    }
                }
            }
            Codec::Opus { opus, .. } => {
                let mut out = Vec::new();
                opus.finish(&mut out)?;
                self.opus_packets(out, true);
            }
            Codec::Vorbis { stream, .. } => {
                stream.finish()?;
                self.vorbis_pages()?;
            }
        }
        Ok(())
    }

    fn flac_frames(&mut self, frames: Vec<Vec<u8>>, last: bool) {
        let Codec::Flac { made, .. } = &mut self.codec else {
            return;
        };
        for f in frames {
            self.queue.push_back(Block {
                ts_ns: ns(*made * BLOCK as u64, self.rate),
                frames: vec![f],
                discard_ns: None,
            });
            *made += 1;
        }
        let reach = if last {
            self.frames
        } else {
            *made * BLOCK as u64
        };
        self.end_ns = ns(reach, self.rate);
    }

    fn mp3_frames(&mut self, bytes: &[u8]) {
        let Codec::Mp3 {
            cutter,
            samples,
            rate,
            ..
        } = &mut self.codec
        else {
            return;
        };
        let mut frames: Vec<Mp3Frame> = Vec::new();
        cutter.push(bytes, &mut frames);
        for f in frames {
            rate.get_or_insert(f.rate);
            self.queue.push_back(Block {
                ts_ns: ns(*samples, f.rate),
                frames: vec![f.data],
                discard_ns: None,
            });
            *samples += f.samples as u64;
            self.end_ns = ns(*samples, f.rate);
        }
    }

    fn opus_packets(&mut self, packets: Vec<Vec<u8>>, last: bool) {
        let Codec::Opus { opus, made } = &mut self.codec else {
            return;
        };
        let count = packets.len();
        for (i, p) in packets.into_iter().enumerate() {
            let discard = (last && i + 1 == count)
                .then(|| ns(opus.end_padding(), OPUS_RATE))
                .filter(|d| *d > 0);
            self.queue.push_back(Block {
                ts_ns: ns(*made * OPUS_FRAME as u64, OPUS_RATE),
                frames: vec![p],
                discard_ns: discard,
            });
            *made += 1;
        }
        self.end_ns = ns(*made * OPUS_FRAME as u64, OPUS_RATE);
    }

    fn vorbis_pages(&mut self) -> io::Result<()> {
        let rate = self.rate;
        let Codec::Vorbis {
            stream,
            reader,
            headers,
            granule,
        } = &mut self.codec
        else {
            return Ok(());
        };
        let mut pages: Vec<Page> = Vec::new();
        reader.push(&stream.take(), &mut pages)?;
        for page in pages {
            let mut frames = Vec::new();
            for p in page.packets {
                if headers.len() < 3 {
                    headers.push(p);
                } else {
                    frames.push(p);
                }
            }
            if frames.is_empty() {
                continue;
            }
            self.queue.push_back(Block {
                ts_ns: ns((*granule).max(0) as u64, rate),
                frames,
                discard_ns: None,
            });
            if page.granule >= 0 {
                *granule = page.granule;
            }
            self.end_ns = ns((*granule).max(0) as u64, rate);
        }
        Ok(())
    }

    /// Whether it can say what it holds yet.
    fn described(&self) -> bool {
        match &self.codec {
            Codec::Mp3 { rate, .. } => rate.is_some(),
            Codec::Vorbis { headers, .. } => headers.len() == 3,
            _ => true,
        }
    }

    /// Its TrackEntry, and where its private data starts in it.
    fn entry(&self, number: u64, uid: u64) -> io::Result<(Vec<u8>, Option<usize>)> {
        let channels = self.spec.positions.len() as u64;
        let plain = |codec_id, rate, bits| Description {
            codec_id,
            private: None,
            delay: None,
            preroll: None,
            rate,
            bits,
        };
        let d = match &self.codec {
            Codec::Pcm { bits, .. } => plain("A_PCM/INT/LIT", self.rate, Some(*bits)),
            Codec::Flac { flac, .. } => {
                let mut private = b"fLaC".to_vec();
                private.extend_from_slice(&flac.stream_info()?);
                Description {
                    private: Some(private),
                    ..plain("A_FLAC", self.rate, None)
                }
            }
            Codec::Mp3 { rate, .. } => {
                // LAME keeps 32, 44.1 and 48 kHz, and brings higher rates
                // down to 48 kHz.
                let rate = rate.unwrap_or(self.rate.min(48_000));
                Description {
                    delay: Some(ns(MP3_DELAY, rate)),
                    ..plain("A_MPEG/L3", rate, None)
                }
            }
            Codec::Opus { opus, .. } => Description {
                private: Some(opus.head().to_vec()),
                delay: Some(ns(opus.pre_skip, OPUS_RATE)),
                preroll: Some(80 * MS),
                ..plain("A_OPUS", OPUS_RATE, None)
            },
            Codec::Vorbis { headers, .. } => Description {
                private: Some(xiph_lace(headers)),
                ..plain("A_VORBIS", self.rate, None)
            },
        };
        let Description {
            codec_id,
            private,
            delay,
            preroll,
            rate,
            bits,
        } = d;
        let mut e = Vec::new();
        el_uint(&mut e, 0xD7, number); // TrackNumber
        el_uint(&mut e, 0x73C5, uid); // TrackUID
        el_uint(&mut e, 0x83, 2); // TrackType: audio
        el_uint(
            &mut e,
            0x9C,
            u64::from(matches!(self.codec, Codec::Vorbis { .. })),
        ); // FlagLacing
        el_str(&mut e, 0x536E, &self.spec.name); // Name
        el_str(&mut e, 0x22B59C, "und"); // Language
        el_str(&mut e, 0x86, codec_id); // CodecID
        let mut private_at = None;
        if let Some(p) = &private {
            let h = head(CODEC_PRIVATE, p.len());
            private_at = Some(e.len() + h.len());
            e.extend_from_slice(&h);
            e.extend_from_slice(p);
        }
        if let Some(d) = delay {
            el_uint(&mut e, 0x56AA, d); // CodecDelay
        }
        if let Some(p) = preroll {
            el_uint(&mut e, 0x56BB, p); // SeekPreRoll
        }
        let mut audio = Vec::new();
        el_float(&mut audio, 0xB5, rate as f64); // SamplingFrequency
        el_uint(&mut audio, 0x9F, channels); // Channels
        if let Some(b) = bits {
            el_uint(&mut audio, 0x6264, b as u64); // BitDepth
        }
        el(&mut e, 0xE1, &audio); // Audio
        Ok((e, private_at))
    }
}

/// A `.mka` file being written.
pub(crate) struct Mka {
    file: BufWriter<File>,
    tracks: Vec<Track>,
    title: String,
    started_utc_ms: i64,
    /// Where the segment's size and payload start, once written.
    segment_at: Option<(u64, u64)>,
    /// Where the Info, Tracks and Duration's payload are.
    info_at: u64,
    tracks_at: u64,
    duration_at: u64,
    /// Each cluster's time in milliseconds and place in the segment.
    cues: Vec<(u64, u64)>,
    /// Bytes written.
    pos: u64,
}

impl Mka {
    /// A file of `tracks` in `format` at `quality` (bits or kbps), at
    /// `rate`. `title` names the recording; `started_utc_ms` is when it
    /// started.
    pub fn new(
        file: File,
        tracks: &[TrackSpec],
        format: RecordFormat,
        quality: u32,
        rate: u32,
        title: &str,
        started_utc_ms: i64,
    ) -> io::Result<Self> {
        Ok(Self {
            file: BufWriter::with_capacity(1 << 16, file),
            tracks: tracks
                .iter()
                .map(|t| Track::new(t, format, rate, quality))
                .collect::<io::Result<_>>()?,
            title: title.to_string(),
            started_utc_ms,
            segment_at: None,
            info_at: 0,
            tracks_at: 0,
            duration_at: 0,
            cues: Vec::new(),
            pos: 0,
        })
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }

    /// Add interleaved samples to track `track`.
    pub fn write(&mut self, track: usize, samples: &[f32]) -> io::Result<()> {
        self.tracks[track].push(samples)?;
        self.write_clusters(false)
    }

    pub fn bytes(&self) -> u64 {
        self.pos
    }

    /// The EBML header and the segment up to its first cluster.
    fn write_start(&mut self) -> io::Result<()> {
        let mut ebml = Vec::new();
        el_uint(&mut ebml, 0x4286, 1); // EBMLVersion
        el_uint(&mut ebml, 0x42F7, 1); // EBMLReadVersion
        el_uint(&mut ebml, 0x42F2, 4); // EBMLMaxIDLength
        el_uint(&mut ebml, 0x42F3, 8); // EBMLMaxSizeLength
        el_str(&mut ebml, 0x4282, "matroska"); // DocType
        el_uint(&mut ebml, 0x4287, 4); // DocTypeVersion
        el_uint(&mut ebml, 0x4285, 2); // DocTypeReadVersion
        let mut out = Vec::new();
        el(&mut out, EBML, &ebml);
        out.extend_from_slice(&id_bytes(SEGMENT));
        let size_at = self.pos + out.len() as u64;
        // Unknown until the end.
        out.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        let start = self.pos + out.len() as u64;
        self.segment_at = Some((size_at, start));
        // Room for the SeekHead.
        el(&mut out, VOID, &[0; SEEK_ROOM - 3]);

        let mut info = Vec::new();
        el_uint(&mut info, 0x2AD7B1, MS); // TimestampScale
        el_str(&mut info, 0x4D80, "Weir"); // MuxingApp
        el_str(
            &mut info,
            0x5741,
            &format!("Weir {}", env!("CARGO_PKG_VERSION")),
        ); // WritingApp
        let date_ns = (self.started_utc_ms - MATROSKA_EPOCH_MS) * 1_000_000;
        el(&mut info, 0x4461, &date_ns.to_be_bytes()); // DateUTC
        if !self.title.is_empty() {
            el_str(&mut info, 0x7BA9, &self.title); // Title
        }
        // Duration last, so its place is easy to know: its id, its size,
        // then the eight bytes written again at the end.
        let duration_in_info = info.len() + 2 + 1;
        el_float(&mut info, 0x4489, 0.0);
        self.info_at = self.pos + out.len() as u64 - start;
        let info_head = head(INFO, info.len());
        self.duration_at = self.pos + (out.len() + info_head.len() + duration_in_info) as u64;
        out.extend_from_slice(&info_head);
        out.extend_from_slice(&info);

        let mut entries = Vec::new();
        let mut privates = Vec::new();
        for (i, t) in self.tracks.iter().enumerate() {
            let uid = (self.started_utc_ms as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(i as u64 + 1)
                | 1;
            let (entry, private) = t.entry(i as u64 + 1, uid)?;
            let h = head(TRACK_ENTRY, entry.len());
            privates.push(private.map(|p| entries.len() + h.len() + p));
            entries.extend_from_slice(&h);
            entries.extend_from_slice(&entry);
        }
        self.tracks_at = self.pos + out.len() as u64 - start;
        let tracks_head = head(TRACKS, entries.len());
        let entries_at = self.pos + (out.len() + tracks_head.len()) as u64;
        for (t, p) in self.tracks.iter_mut().zip(privates) {
            t.private_at = p.map(|p| entries_at + p as u64);
        }
        out.extend_from_slice(&tracks_head);
        out.extend_from_slice(&entries);
        self.put(&out)
    }

    /// Write every cluster whose second every track has reached, or, at
    /// the `end`, everything.
    fn write_clusters(&mut self, end: bool) -> io::Result<()> {
        if self.segment_at.is_none() {
            let ready = self.tracks.iter().all(|t| t.described() && t.end_ns > 0);
            if !(ready || end) {
                return Ok(());
            }
            self.write_start()?;
        }
        loop {
            let Some(first) = self
                .tracks
                .iter()
                .filter_map(|t| t.queue.front().map(|b| b.ts_ns))
                .min()
            else {
                return Ok(());
            };
            let cluster_ms = first / MS;
            let until_ns = (cluster_ms + CLUSTER_MS) * MS;
            if !end && self.tracks.iter().any(|t| t.end_ns < until_ns) {
                return Ok(());
            }
            let mut blocks: Vec<(u64, usize, Block)> = Vec::new();
            for (i, t) in self.tracks.iter_mut().enumerate() {
                while t.queue.front().is_some_and(|b| b.ts_ns < until_ns) {
                    let b = t.queue.pop_front().expect("there is one");
                    blocks.push((b.ts_ns, i, b));
                }
            }
            blocks.sort_by_key(|(ts, i, _)| (*ts, *i));
            let mut payload = Vec::new();
            el_uint(&mut payload, 0xE7, cluster_ms); // Timestamp
            for (ts, track, b) in blocks {
                let rel = ((ts + MS / 2) / MS) as i64 - cluster_ms as i64;
                let rel = rel.clamp(i16::MIN as i64, i16::MAX as i64) as i16;
                let laced = b.frames.len() > 1;
                let mut block = vint(track as u64 + 1);
                block.extend_from_slice(&rel.to_be_bytes());
                let flags: u8 = if laced { 0x02 } else { 0 };
                match b.discard_ns {
                    // A SimpleBlock, marked a keyframe: every audio block is.
                    None => block.push(flags | 0x80),
                    Some(_) => block.push(flags),
                }
                if laced {
                    let sizes = xiph_lace(&b.frames);
                    // Drop the frames xiph_lace appended: only the count
                    // and the sizes go before the data.
                    let data: usize = b.frames.iter().map(Vec::len).sum();
                    block.extend_from_slice(&sizes[..sizes.len() - data]);
                }
                for f in &b.frames {
                    block.extend_from_slice(f);
                }
                match b.discard_ns {
                    None => el(&mut payload, 0xA3, &block), // SimpleBlock
                    Some(d) => {
                        let mut group = Vec::new();
                        el(&mut group, 0xA1, &block); // Block
                        el(&mut group, 0x75A2, &(d as i64).to_be_bytes()); // DiscardPadding
                        el(&mut payload, 0xA0, &group); // BlockGroup
                    }
                }
            }
            let (_, start) = self.segment_at.expect("written above");
            self.cues.push((cluster_ms, self.pos - start));
            let mut cluster = head(CLUSTER, payload.len());
            cluster.extend_from_slice(&payload);
            self.put(&cluster)?;
        }
    }

    /// Finish every track, write what is left and fill in the start.
    pub fn finish(mut self) -> io::Result<u64> {
        for t in &mut self.tracks {
            t.finish()?;
        }
        self.write_clusters(true)?;
        let (size_at, start) = self.segment_at.expect("written by write_clusters");

        let mut points = Vec::new();
        for (ms, at) in &self.cues {
            let mut positions = Vec::new();
            el_uint(&mut positions, 0xF7, 1); // CueTrack
            el_uint(&mut positions, 0xF1, *at); // CueClusterPosition
            let mut point = Vec::new();
            el_uint(&mut point, 0xB3, *ms); // CueTime
            el(&mut point, 0xB7, &positions); // CueTrackPositions
            el(&mut points, 0xBB, &point); // CuePoint
        }
        let cues_at = self.pos - start;
        let mut cues = Vec::new();
        el(&mut cues, CUES, &points);
        self.put(&cues)?;
        let end = self.pos;

        let duration_ms = self
            .tracks
            .iter()
            .map(|t| ns(t.frames, t.rate))
            .max()
            .unwrap_or(0) as f64
            / MS as f64;
        let mut seeks = Vec::new();
        for (id, at) in [
            (INFO, self.info_at),
            (TRACKS, self.tracks_at),
            (CUES, cues_at),
        ] {
            let mut seek = Vec::new();
            el(&mut seek, SEEK_ID, &id_bytes(id));
            el_uint(&mut seek, SEEK_POSITION, at);
            el(&mut seeks, SEEK, &seek);
        }
        let mut seek_head = Vec::new();
        el(&mut seek_head, SEEK_HEAD, &seeks);
        // The rest of the room stays a Void: its id, its size, zeros.
        let left = SEEK_ROOM - seek_head.len();
        let size = if left - 2 < 127 { left - 2 } else { left - 3 };
        seek_head.extend_from_slice(&id_bytes(VOID));
        seek_head.extend_from_slice(&vint(size as u64));
        seek_head.resize(SEEK_ROOM, 0);

        let mut patches: Vec<(u64, Vec<u8>)> = vec![
            (size_at, vint8(end - start).to_vec()),
            (start, seek_head),
            (self.duration_at, duration_ms.to_be_bytes().to_vec()),
        ];
        for t in &self.tracks {
            if let (Some(at), Codec::Flac { flac, .. }) = (t.private_at, &t.codec) {
                patches.push((at + 4, flac.stream_info()?));
            }
        }
        for (at, bytes) in patches {
            self.file.seek(SeekFrom::Start(at))?;
            self.file.write_all(&bytes)?;
        }
        self.file.seek(SeekFrom::End(0))?;
        let file = self.file.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ebml_numbers_and_lacing() {
        assert_eq!(vint(1), [0x81]);
        assert_eq!(vint(126), [0xFE]);
        // 127 would read as "unknown" in one byte.
        assert_eq!(vint(127), [0x40, 0x7F]);
        assert_eq!(vint8(5), [1, 0, 0, 0, 0, 0, 0, 5]);
        assert_eq!(id_bytes(SEGMENT), [0x18, 0x53, 0x80, 0x67]);
        assert_eq!(id_bytes(0xA3), [0xA3]);
        let mut e = Vec::new();
        el_uint(&mut e, 0xD7, 0);
        assert_eq!(e, [0xD7, 0x81, 0]);
        let laced = xiph_lace(&[vec![1; 300], vec![2; 3], vec![3; 2]]);
        assert_eq!(&laced[..4], &[2, 255, 45, 3]);
        assert_eq!(laced.len(), 4 + 305);
    }
}
