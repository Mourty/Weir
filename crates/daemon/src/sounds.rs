//! The sounds hotkeys play: Weir's own, which the engine makes, and the
//! person's own, kept as files in `sounds/` beside the configuration, one
//! file per sound, named after it. Keeping the files as they came means
//! they can be exported and imported like everything else, and a sound
//! that will not decode is never left half added.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tracing::warn;
use weir_engine::{builtin_sound, SoundBank, SoundData};
use weir_protocol::{
    SoundInfo, BUILTIN_SOUNDS, SOUND_EXTENSIONS, SOUND_FILE_MAX, SOUND_SECONDS_MAX,
};

/// One sound hotkeys can play.
struct Sound {
    name: String,
    builtin: bool,
    data: SoundData,
}

/// Every sound hotkeys can play: Weir's own first, then the person's own in
/// alphabetical order. A sound's place in the list is its number in the
/// engine's bank.
pub struct Library {
    sounds: Vec<Sound>,
}

/// The extension of `path`, in lower case, if Weir reads that kind of file.
pub fn sound_extension(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    SOUND_EXTENSIONS.contains(&ext.as_str()).then_some(ext)
}

/// The file in `dir` that holds the sound called `name`, ignoring case.
pub fn sound_file(dir: &Path, name: &str) -> Option<PathBuf> {
    own_files(dir)
        .into_iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, p)| p)
}

/// The person's own sound files in `dir`, as `(name, path)`, in
/// alphabetical order of name.
pub fn own_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && sound_extension(p).is_some())
        .filter_map(|p| Some((p.file_stem()?.to_str()?.to_string(), p)))
        .collect();
    out.sort_by_key(|(n, _)| n.to_lowercase());
    out
}

/// Decode the sound file `path`: at most `SOUND_FILE_MAX` bytes and
/// `SOUND_SECONDS_MAX` seconds, one or two channels kept (the first two of
/// more). Says what is wrong in words for the person who picked the file.
pub fn decode(path: &Path) -> Result<SoundData> {
    let ext = sound_extension(path).context("Weir plays .wav, .ogg and .flac files")?;
    let size = std::fs::metadata(path)
        .with_context(|| format!("could not read {}", path.display()))?
        .len();
    if size > SOUND_FILE_MAX {
        bail!(
            "the file is too big: {} MB at most",
            SOUND_FILE_MAX / 1024 / 1024
        );
    }
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    decode_bytes(bytes, &ext)
}

/// Decode a sound file's bytes, of the kind `ext` says.
pub fn decode_bytes(bytes: Vec<u8>, ext: &str) -> Result<SoundData> {
    let unreadable = || format!("Weir could not read this {ext} file");
    let source = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(ext);
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .with_context(unreadable)?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .with_context(|| format!("there is no sound in this {ext} file"))?;
    let track_id = track.id;
    let rate = track.codec_params.sample_rate.with_context(unreadable)?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .with_context(|| format!("Weir cannot play the kind of sound in this {ext} file"))?;
    let most = (SOUND_SECONDS_MAX * rate as f32) as usize;
    let mut channels: Vec<Vec<f32>> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // The end of the file.
            Err(DecodeError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(DecodeError::ResetRequired) => break,
            Err(e) => return Err(e).with_context(unreadable),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A damaged packet is passed over, as players do.
            Err(DecodeError::DecodeError(_)) => continue,
            Err(e) => return Err(e).with_context(unreadable),
        };
        let spec = *decoded.spec();
        let n = spec.channels.count();
        if n == 0 {
            continue;
        }
        let keep = n.min(2);
        if channels.is_empty() {
            channels = vec![Vec::new(); keep];
        }
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks_exact(n) {
            for (c, ch) in channels.iter_mut().enumerate() {
                ch.push(frame[c.min(keep - 1)]);
            }
        }
        if channels[0].len() > most {
            bail!(
                "the sound is too long: {} seconds at most",
                SOUND_SECONDS_MAX
            );
        }
    }
    if channels.first().is_none_or(Vec::is_empty) {
        bail!("there is no sound in this {ext} file");
    }
    Ok(SoundData { rate, channels })
}

impl Library {
    /// Weir's own sounds, and every sound file in `dir` that decodes. One
    /// that does not is left out, and said so in the log.
    pub fn load(dir: &Path) -> Self {
        let mut sounds: Vec<Sound> = BUILTIN_SOUNDS
            .iter()
            .filter_map(|&name| {
                Some(Sound {
                    name: name.to_string(),
                    builtin: true,
                    data: builtin_sound(name)?,
                })
            })
            .collect();
        for (name, path) in own_files(dir) {
            if sounds.iter().any(|s| s.name.eq_ignore_ascii_case(&name)) {
                warn!("passing over {}: a sound has that name", path.display());
                continue;
            }
            match decode(&path) {
                Ok(data) => sounds.push(Sound {
                    name,
                    builtin: false,
                    data,
                }),
                Err(e) => warn!("passing over the sound {}: {e:#}", path.display()),
            }
        }
        Self { sounds }
    }

    /// Every sound, for the engine, in the same order as the library.
    pub fn bank(&self) -> SoundBank {
        SoundBank {
            sounds: self.sounds.iter().map(|s| s.data.clone()).collect(),
        }
    }

    /// The place of the sound called `name`, ignoring case.
    pub fn index(&self, name: &str) -> Option<usize> {
        self.sounds
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name.trim()))
    }

    /// The sound called `name`, ignoring case, as clients see it.
    pub fn info(&self, name: &str) -> Option<SoundInfo> {
        self.index(name).map(|i| self.info_at(i))
    }

    fn info_at(&self, i: usize) -> SoundInfo {
        let s = &self.sounds[i];
        SoundInfo {
            name: s.name.clone(),
            builtin: s.builtin,
            seconds: (s.data.seconds() * 1000.0).round() / 1000.0,
        }
    }

    /// Every sound, as clients see them.
    pub fn infos(&self) -> Vec<SoundInfo> {
        (0..self.sounds.len()).map(|i| self.info_at(i)).collect()
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A WAV file of 16-bit samples, `channels` interleaved, at `rate`.
    pub fn wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn a_wav_file_decodes_into_its_channels() {
        let samples: Vec<i16> = (0..200)
            .map(|i| if i % 2 == 0 { 16384 } else { -16384 })
            .collect();
        let s = decode_bytes(wav(44_100, 2, &samples), "wav").unwrap();
        assert_eq!(s.rate, 44_100);
        assert_eq!(s.channels.len(), 2);
        assert_eq!(s.frames(), 100);
        assert!((s.channels[0][0] - 0.5).abs() < 0.001);
        assert!((s.channels[1][0] + 0.5).abs() < 0.001);
    }

    #[test]
    fn too_long_or_not_a_sound_is_refused_in_words() {
        let long = vec![0i16; 8000 * 11];
        let e = decode_bytes(wav(8000, 1, &long), "wav").unwrap_err();
        assert!(format!("{e:#}").contains("too long"), "{e:#}");
        let e = decode_bytes(b"not a sound".to_vec(), "ogg").unwrap_err();
        assert!(
            format!("{e:#}").contains("could not read this ogg file"),
            "{e:#}"
        );
        let e = decode(Path::new("/tmp/x.mp3")).unwrap_err();
        assert!(format!("{e:#}").contains(".wav, .ogg and .flac"), "{e:#}");
    }

    #[test]
    fn the_library_has_weirs_sounds_then_the_persons_by_name() {
        let dir = std::env::temp_dir().join(format!("weir-sounds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let one = wav(48_000, 1, &[1000; 480]);
        std::fs::write(dir.join("Zap.wav"), &one).unwrap();
        std::fs::write(dir.join("airhorn.WAV"), &one).unwrap();
        std::fs::write(dir.join("Broken.wav"), b"RIFF").unwrap();
        std::fs::write(dir.join("click.wav"), &one).unwrap();
        std::fs::write(dir.join("notes.txt"), b"hi").unwrap();
        let lib = Library::load(&dir);
        let names: Vec<String> = lib.infos().into_iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            ["Click", "Beep up", "Beep down", "Tick", "airhorn", "Zap"],
            "the broken one, the one named like Weir's and the text are left out"
        );
        assert_eq!(lib.index("ZAP"), Some(5));
        assert!(!lib.info("Zap").unwrap().builtin && lib.info("click").unwrap().builtin);
        assert_eq!(lib.bank().sounds.len(), 6);
        assert_eq!(lib.info("Zap").unwrap().seconds, 0.01);
        assert_eq!(lib.info("Click").unwrap().length(), "15 ms");
        assert_eq!(
            sound_file(&dir, "AIRHORN").unwrap(),
            dir.join("airhorn.WAV")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
