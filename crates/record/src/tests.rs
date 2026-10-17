//! Each format and layout written, then read back: the right length, the
//! right channels, and the sound recognizably what went in.

use super::*;
use std::f32::consts::TAU;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use weir_protocol::ChannelPosition::{self, *};

const RATE: u32 = 48_000;
/// 2.5 seconds: more than two clusters, and not a whole number of frames
/// of any codec.
const FRAMES: usize = 120_017;

/// A folder of its own for one test, removed after it unless it failed,
/// so a failure's files are there to look at.
struct Folder(PathBuf);

impl std::ops::Deref for Folder {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn folder(name: &str) -> Folder {
    let dir = std::env::temp_dir().join(format!("weir-record-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Folder(dir)
}

/// A tone per channel, each its own pitch, at half scale.
fn tone(channels: usize, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(channels * frames);
    for k in 0..frames {
        for c in 0..channels {
            let hz = 220.0 * (c + 1) as f32;
            out.push(0.5 * (TAU * hz * k as f32 / RATE as f32).sin());
        }
    }
    out
}

fn track(name: &str, positions: &[ChannelPosition]) -> TrackSpec {
    TrackSpec {
        name: name.into(),
        positions: positions.to_vec(),
    }
}

/// Write `tracks` (each fed its tone in uneven pieces, as the recorder
/// does) as `spec`, into `paths`.
fn record(spec: Spec, tracks: &[TrackSpec], paths: &[PathBuf], bwf: Option<&Bwf>) -> u64 {
    let mut part = Part::create(&spec, tracks, paths, "Test", bwf, 1_760_000_000_000).unwrap();
    let tones: Vec<Vec<f32>> = tracks
        .iter()
        .map(|t| tone(t.positions.len(), FRAMES))
        .collect();
    let mut at = 0;
    for piece in [4_800, 1_024, 2_047, 9_600].iter().cycle() {
        let end = (at + piece).min(FRAMES);
        for (i, t) in tracks.iter().enumerate() {
            let n = t.positions.len();
            part.write(i, &tones[i][at * n..end * n]).unwrap();
        }
        at = end;
        if at == FRAMES {
            break;
        }
    }
    assert!(part.bytes() > 0);
    part.finish().unwrap()
}

/// Every track of a file, decoded: (channels, sample rate, interleaved).
fn decode(path: &Path) -> Vec<(usize, u32, Vec<f32>)> {
    let file = File::open(path).unwrap();
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let options = FormatOptions {
        enable_gapless: true,
        ..Default::default()
    };
    let mut format = symphonia::default::get_probe()
        .format(&hint, source, &options, &MetadataOptions::default())
        .unwrap()
        .format;
    let tracks: Vec<_> = format.tracks().to_vec();
    let mut decoders: Vec<_> = tracks
        .iter()
        .map(|t| {
            // Symphonia's Matroska reader leaves this out for PCM, which
            // its PCM decoder needs: a second is more than any block.
            let mut params = t.codec_params.clone();
            params.max_frames_per_packet.get_or_insert(RATE as u64);
            symphonia::default::get_codecs()
                .make(&params, &DecoderOptions::default())
                .unwrap()
        })
        .collect();
    let mut out: Vec<(usize, u32, Vec<f32>)> = tracks
        .iter()
        .map(|t| {
            let p = &t.codec_params;
            (
                p.channels.map_or(0, |c| c.count()),
                p.sample_rate.unwrap_or(0),
                Vec::new(),
            )
        })
        .collect();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(DecodeError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => panic!("{}: {e}", path.display()),
        };
        let i = tracks
            .iter()
            .position(|t| t.id == packet.track_id())
            .unwrap();
        let decoded = decoders[i].decode(&packet).unwrap();
        let spec = *decoded.spec();
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        out[i].0 = spec.channels.count();
        out[i].2.extend_from_slice(buf.samples());
    }
    out
}

/// How alike two signals are, from -1 to 1, after shifting `b` by up to
/// `slack` frames either way (lossy codecs delay a little).
fn likeness(a: &[f32], b: &[f32], channels: usize, channel: usize, slack: usize) -> f32 {
    let at = |s: &[f32], k: usize| s[k * channels + channel];
    let frames = (a.len() / channels).min(b.len() / channels);
    // From half a second in, for a fifth of a second.
    let span = RATE as usize / 5;
    let start = RATE as usize / 2;
    assert!(frames > start + span + slack);
    (0..=2 * slack)
        .map(|shift| {
            let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
            for k in start..start + span {
                let x = at(a, k) as f64;
                let y = at(b, k + shift - slack) as f64;
                ab += x * y;
                aa += x * x;
                bb += y * y;
            }
            (ab / (aa.sqrt() * bb.sqrt()).max(1e-12)) as f32
        })
        .fold(-1.0, f32::max)
}

#[test]
fn lossless_files_hold_exactly_what_went_in() {
    let dir = folder("lossless");
    let tracks = [track("Mic", &[Mono]), track("Music", &[FL, FR])];
    for (format, bits, ext) in [
        (RecordFormat::Wav, 16, "wav"),
        (RecordFormat::Wav, 24, "wav"),
        (RecordFormat::Flac, 24, "flac"),
    ] {
        let paths: Vec<PathBuf> = tracks
            .iter()
            .map(|t| dir.join(format!("{}-{bits}.{ext}", t.name)))
            .collect();
        let spec = Spec {
            format,
            quality: bits,
            rate: RATE,
            layout: RecordLayout::Files,
        };
        record(spec, &tracks, &paths, None);
        for (t, path) in tracks.iter().zip(&paths) {
            let n = t.positions.len();
            let got = decode(path);
            assert_eq!(got.len(), 1);
            let (channels, rate, samples) = &got[0];
            assert_eq!((*channels, *rate), (n, RATE), "{}", path.display());
            assert_eq!(samples.len(), FRAMES * n, "{}", path.display());
            let want = tone(n, FRAMES);
            let step = 1.0 / (1u32 << (bits - 1)) as f32;
            let worst = want
                .iter()
                .zip(samples)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            assert!(worst <= step, "{}: off by {worst}", path.display());
        }
    }
}

#[test]
fn lossy_files_sound_like_what_went_in() {
    let dir = folder("lossy");
    let tracks = [track("Music", &[FL, FR]), track("Mic", &[Mono])];
    for (format, kbps) in [(RecordFormat::Mp3, 192), (RecordFormat::Vorbis, 192)] {
        let paths: Vec<PathBuf> = tracks
            .iter()
            .map(|t| dir.join(format!("{}.{}", t.name, format.extension())))
            .collect();
        let spec = Spec {
            format,
            quality: kbps,
            rate: RATE,
            layout: RecordLayout::Files,
        };
        record(spec, &tracks, &paths, None);
        for (t, path) in tracks.iter().zip(&paths) {
            let n = t.positions.len();
            let got = decode(path);
            let (channels, rate, samples) = &got[0];
            assert_eq!((*channels, *rate), (n, RATE), "{}", path.display());
            let frames = samples.len() / n;
            // Gapless MP3 and Vorbis's last granule cut the padding off.
            assert!(
                frames.abs_diff(FRAMES) < 1152,
                "{}: {frames} frames",
                path.display()
            );
            let want = tone(n, FRAMES);
            for c in 0..n {
                let l = likeness(&want, samples, n, c, 1200);
                assert!(l > 0.98, "{} channel {c}: {l}", path.display());
            }
        }
    }
}

#[test]
fn opus_files_trim_to_the_exact_length() {
    let dir = folder("opus");
    let tracks = [track("Music", &[FL, FR])];
    let path = dir.join("Music.opus");
    let spec = Spec {
        format: RecordFormat::Opus,
        quality: 128,
        rate: RATE,
        layout: RecordLayout::Files,
    };
    record(spec, &tracks, std::slice::from_ref(&path), None);
    // Read the Ogg stream back and decode it with libopus.
    let mut reader = ::ogg::PacketReader::new(File::open(&path).unwrap());
    let head = reader.read_packet().unwrap().unwrap();
    assert_eq!(&head.data[..8], b"OpusHead");
    assert_eq!(head.data[9], 2);
    let pre_skip = u16::from_le_bytes([head.data[10], head.data[11]]) as usize;
    let tags = reader.read_packet().unwrap().unwrap();
    assert_eq!(&tags.data[..8], b"OpusTags");
    let mut dec = ::opus::Decoder::new(48_000, ::opus::Channels::Stereo).unwrap();
    let mut pcm = Vec::new();
    let mut last_granule = 0;
    let mut buf = vec![0.0f32; 5760 * 2];
    while let Some(p) = reader.read_packet().unwrap() {
        let n = dec.decode_float(&p.data, &mut buf, false).unwrap();
        pcm.extend_from_slice(&buf[..n * 2]);
        last_granule = p.absgp_page();
    }
    assert_eq!(last_granule as usize - pre_skip, FRAMES);
    let pcm = &pcm[pre_skip * 2..(pre_skip + FRAMES) * 2];
    let want = tone(2, FRAMES);
    for c in 0..2 {
        let l = likeness(&want, pcm, 2, c, 4);
        assert!(l > 0.98, "channel {c}: {l}");
    }
}

#[test]
fn surround_wav_says_which_speaker_is_which() {
    let dir = folder("surround");
    let tracks = [track("Movie", &[FL, FR, FC, LFE, RL, RR])];
    let path = dir.join("Movie.wav");
    let spec = Spec {
        format: RecordFormat::Wav,
        quality: 24,
        rate: RATE,
        layout: RecordLayout::Files,
    };
    let bwf = Bwf {
        start: LocalTime {
            year: 2026,
            month: 10,
            day: 17,
            hour: 20,
            minute: 52,
            second: 9,
        },
        start_ms_fraction: 0,
        reference: "Test".into(),
    };
    record(spec, &tracks, std::slice::from_ref(&path), Some(&bwf));
    let bytes = std::fs::read(&path).unwrap();
    // The extensible header's channel mask: 5.1 with rear speakers.
    assert_eq!(&bytes[20..22], &0xFFFEu16.to_le_bytes());
    assert_eq!(&bytes[40..44], &0x3Fu32.to_le_bytes());
    // The bext chunk follows the format chunk.
    assert_eq!(&bytes[60..64], b"bext");
    let got = decode(&path);
    assert_eq!(got[0].0, 6);
    assert_eq!(got[0].2.len(), FRAMES * 6);
    // Never replaced: a second recording to the same name fails.
    let again = Part::create(&spec, &tracks, &[path], "Test", None, 0);
    assert!(again.is_err());
    assert_eq!(max_frames(&spec, &[6]), Some(0xFFFF_0000 / 18));
}

#[test]
fn multitrack_files_hold_every_track_in_step() {
    let dir = folder("mka");
    let tracks = [
        track("Music", &[FL, FR]),
        track("Mic", &[Mono]),
        track("Stream", &[FL, FR]),
    ];
    for (format, quality) in [
        (RecordFormat::Wav, 24),
        (RecordFormat::Flac, 16),
        (RecordFormat::Mp3, 256),
        (RecordFormat::Vorbis, 192),
    ] {
        let path = dir.join(format!("all-{}.mka", format.extension()));
        let spec = Spec {
            format,
            quality,
            rate: RATE,
            layout: RecordLayout::Multitrack,
        };
        let bytes = record(spec, &tracks, std::slice::from_ref(&path), None);
        assert_eq!(bytes, std::fs::metadata(&path).unwrap().len());
        let got = decode(&path);
        assert_eq!(got.len(), 3, "{}", path.display());
        let lossless = format.lossless();
        for (t, (channels, _, samples)) in tracks.iter().zip(&got) {
            let n = t.positions.len();
            assert_eq!(*channels, n, "{} {}", path.display(), t.name);
            let frames = samples.len() / n;
            if lossless {
                assert_eq!(frames, FRAMES, "{} {}", path.display(), t.name);
            } else {
                assert!(
                    frames.abs_diff(FRAMES) < 2400,
                    "{} {}: {frames}",
                    path.display(),
                    t.name
                );
            }
            let want = tone(n, FRAMES);
            let slack = if lossless { 0 } else { 2400 };
            for c in 0..n {
                let l = likeness(&want, samples, n, c, slack);
                assert!(l > 0.98, "{} {} channel {c}: {l}", path.display(), t.name);
            }
        }
    }
}

#[test]
fn multitrack_opus_holds_its_packets_and_padding() {
    let dir = folder("mka-opus");
    let tracks = [track("Music", &[FL, FR]), track("Mic", &[Mono])];
    let path = dir.join("all.mka");
    let spec = Spec {
        format: RecordFormat::Opus,
        quality: 128,
        rate: RATE,
        layout: RecordLayout::Multitrack,
    };
    record(spec, &tracks, std::slice::from_ref(&path), None);
    // Symphonia reads the file but cannot decode Opus: decode its
    // packets with libopus.
    let file = File::open(&path).unwrap();
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut format = symphonia::default::get_probe()
        .format(
            Hint::new().with_extension("mka"),
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .unwrap()
        .format;
    let ids: Vec<u32> = format.tracks().iter().map(|t| t.id).collect();
    assert_eq!(ids.len(), 2);
    let mut decoders = [
        ::opus::Decoder::new(48_000, ::opus::Channels::Stereo).unwrap(),
        ::opus::Decoder::new(48_000, ::opus::Channels::Mono).unwrap(),
    ];
    let mut pcm = [Vec::new(), Vec::new()];
    let mut buf = vec![0.0f32; 5760 * 2];
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(DecodeError::IoError(_)) => break,
            Err(e) => panic!("{e}"),
        };
        let i = ids.iter().position(|id| *id == packet.track_id()).unwrap();
        let n = decoders[i]
            .decode_float(&packet.data, &mut buf, false)
            .unwrap();
        let ch = if i == 0 { 2 } else { 1 };
        pcm[i].extend_from_slice(&buf[..n * ch]);
    }
    for (i, ch) in [(0, 2), (1, 1)] {
        let frames = pcm[i].len() / ch;
        // Every packet is there: players drop the padding.
        assert!(
            (FRAMES..FRAMES + 2 * 960 + 400).contains(&frames),
            "{frames}"
        );
        let l = likeness(&tone(ch, FRAMES), &pcm[i], ch, 0, 400);
        assert!(l > 0.98, "track {i}: {l}");
    }
}

#[test]
fn other_sample_rates_keep_their_length() {
    let dir = folder("rates");
    let tracks = [track("Music", &[FL, FR])];
    let rate = 44_100;
    let opus = dir.join("Music.opus");
    let spec = Spec {
        format: RecordFormat::Opus,
        quality: 128,
        rate,
        layout: RecordLayout::Files,
    };
    record(spec, &tracks, std::slice::from_ref(&opus), None);
    // Opus runs at 48 kHz: the end says how long it is there.
    let mut reader = ::ogg::PacketReader::new(File::open(&opus).unwrap());
    let head = reader.read_packet().unwrap().unwrap();
    let pre_skip = u16::from_le_bytes([head.data[10], head.data[11]]) as u64;
    assert_eq!(
        u32::from_le_bytes(head.data[12..16].try_into().unwrap()),
        rate
    );
    let mut last = 0;
    while let Some(p) = reader.read_packet().unwrap() {
        last = p.absgp_page();
    }
    let want = (FRAMES as u64 * 48_000 + rate as u64 / 2) / rate as u64;
    assert_eq!(last - pre_skip, want);

    let flac = dir.join("Music.flac");
    let spec = Spec {
        format: RecordFormat::Flac,
        quality: 16,
        rate,
        layout: RecordLayout::Files,
    };
    record(spec, &tracks, std::slice::from_ref(&flac), None);
    let got = decode(&flac);
    assert_eq!((got[0].1, got[0].2.len()), (rate, FRAMES * 2));
}
