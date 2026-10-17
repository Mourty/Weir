//! Writing Weir's recordings.
//!
//! A recording is made of parts: one, or a new one every so many minutes.
//! A [`Part`] is either a file for each track or one Matroska file (`.mka`)
//! holding every track, in WAV, FLAC, MP3, Opus or Ogg Vorbis. Tracks are
//! given interleaved `f32` samples in the order of their strip's or bus's
//! channels; each format gets the order and speaker layout it expects.
//!
//! Files are written in place under their real names, and kept playable
//! as they grow, so a recording cut short by a crash or a power cut keeps
//! what it had.

mod channels;
mod flac;
mod mka;
mod mp3;
mod opus;
mod vorbis;
mod wav;

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use weir_protocol::{ChannelPosition, LocalTime, RecordFormat, RecordLayout};

/// One track: what it is called, and its channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSpec {
    /// The strip's or bus's name.
    pub name: String,
    /// Its channels, in the order its samples come in.
    pub positions: Vec<ChannelPosition>,
}

/// What a recording is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    /// The format.
    pub format: RecordFormat,
    /// Bits per sample for WAV and FLAC, kilobits a second for a stereo
    /// track otherwise.
    pub quality: u32,
    /// The sample rate the samples come at.
    pub rate: u32,
    /// A file per track, or one holding them all.
    pub layout: RecordLayout,
}

/// When a WAV file starts, for its Broadcast Wave `bext` chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bwf {
    /// The moment, in local time.
    pub start: LocalTime,
    /// Milliseconds past `start`'s second.
    pub start_ms_fraction: u32,
    /// Something that tells this recording apart, up to 32 characters.
    pub reference: String,
}

/// The longest a part of a recording can be in `spec` with tracks of
/// these many channels, in frames, when there is a limit: WAV files cannot
/// grow past 4 GB.
pub fn max_frames(spec: &Spec, channels: &[usize]) -> Option<u64> {
    if spec.format != RecordFormat::Wav || spec.layout != RecordLayout::Files {
        return None;
    }
    let widest = channels.iter().copied().max().unwrap_or(1).max(1) as u64;
    Some(wav::WAV_MAX_DATA / (widest * spec.quality as u64 / 8))
}

/// One track's file.
enum TrackFile {
    Wav(wav::Wav),
    Flac(flac::FlacFile),
    Mp3(mp3::Mp3File),
    Opus(opus::OggOpus),
    Vorbis(vorbis::OggVorbis),
}

impl TrackFile {
    fn new(file: File, spec: &Spec, track: &TrackSpec, bwf: Option<&Bwf>) -> io::Result<Self> {
        let (q, rate) = (spec.quality, spec.rate);
        Ok(match spec.format {
            RecordFormat::Wav => TrackFile::Wav(wav::Wav::new(file, track, rate, q, bwf)?),
            RecordFormat::Flac => TrackFile::Flac(flac::FlacFile::new(file, track, rate, q)?),
            RecordFormat::Mp3 => TrackFile::Mp3(mp3::Mp3File::new(file, track, rate, q)?),
            RecordFormat::Opus => TrackFile::Opus(opus::OggOpus::new(file, track, rate, q)?),
            RecordFormat::Vorbis => {
                TrackFile::Vorbis(vorbis::OggVorbis::new(file, track, rate, q)?)
            }
        })
    }

    fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        match self {
            TrackFile::Wav(f) => f.write(samples),
            TrackFile::Flac(f) => f.write(samples),
            TrackFile::Mp3(f) => f.write(samples),
            TrackFile::Opus(f) => f.write(samples),
            TrackFile::Vorbis(f) => f.write(samples),
        }
    }

    fn bytes(&self) -> u64 {
        match self {
            TrackFile::Wav(f) => f.bytes(),
            TrackFile::Flac(f) => f.bytes(),
            TrackFile::Mp3(f) => f.bytes(),
            TrackFile::Opus(f) => f.bytes(),
            TrackFile::Vorbis(f) => f.bytes(),
        }
    }

    fn finish(self) -> io::Result<u64> {
        match self {
            TrackFile::Wav(f) => f.finish(),
            TrackFile::Flac(f) => f.finish(),
            TrackFile::Mp3(f) => f.finish(),
            TrackFile::Opus(f) => f.finish(),
            TrackFile::Vorbis(f) => f.finish(),
        }
    }
}

enum Files {
    Each(Vec<TrackFile>),
    One(Box<mka::Mka>),
}

/// One part of a recording, being written.
pub struct Part {
    files: Files,
    channels: Vec<usize>,
    paths: Vec<PathBuf>,
}

/// Make a new file at `path`, never replacing one, and the folders it is
/// in.
fn create(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

impl Part {
    /// Start writing `tracks` as `spec` says: to `paths`, one per track
    /// for [`RecordLayout::Files`], one for all of them for
    /// [`RecordLayout::Multitrack`]. A file already there is never
    /// replaced: that is an error. `title` names a `.mka`; `bwf` adds
    /// when each WAV file starts to it; `started_utc_ms` is when the part
    /// starts.
    pub fn create(
        spec: &Spec,
        tracks: &[TrackSpec],
        paths: &[PathBuf],
        title: &str,
        bwf: Option<&Bwf>,
        started_utc_ms: i64,
    ) -> io::Result<Self> {
        let channels: Vec<usize> = tracks.iter().map(|t| t.positions.len().max(1)).collect();
        if tracks.iter().any(|t| t.positions.is_empty()) || spec.rate == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a track without channels, or no sample rate",
            ));
        }
        let wanted = match spec.layout {
            RecordLayout::Files => tracks.len(),
            RecordLayout::Multitrack => 1,
        };
        if paths.len() != wanted {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a file name for each track, or one for them all",
            ));
        }
        let files = match spec.layout {
            RecordLayout::Files => {
                let mut each = Vec::new();
                for (track, path) in tracks.iter().zip(paths) {
                    match create(path).and_then(|f| TrackFile::new(f, spec, track, bwf)) {
                        Ok(f) => each.push(f),
                        Err(e) => {
                            // Leave nothing half made behind.
                            drop(each);
                            for p in paths {
                                let _ = std::fs::remove_file(p);
                            }
                            return Err(e);
                        }
                    }
                }
                Files::Each(each)
            }
            RecordLayout::Multitrack => {
                let file = create(&paths[0])?;
                let mka = mka::Mka::new(
                    file,
                    tracks,
                    spec.format,
                    spec.quality,
                    spec.rate,
                    title,
                    started_utc_ms,
                );
                match mka {
                    Ok(m) => Files::One(Box::new(m)),
                    Err(e) => {
                        let _ = std::fs::remove_file(&paths[0]);
                        return Err(e);
                    }
                }
            }
        };
        Ok(Self {
            files,
            channels,
            paths: paths.to_vec(),
        })
    }

    /// Add interleaved samples to track `track`, as many channels as it
    /// has.
    pub fn write(&mut self, track: usize, samples: &[f32]) -> io::Result<()> {
        let n = self.channels[track];
        let whole = samples.len() / n * n;
        match &mut self.files {
            Files::Each(each) => each[track].write(&samples[..whole]),
            Files::One(mka) => mka.write(track, &samples[..whole]),
        }
    }

    /// Bytes written so far.
    pub fn bytes(&self) -> u64 {
        match &self.files {
            Files::Each(each) => each.iter().map(TrackFile::bytes).sum(),
            Files::One(mka) => mka.bytes(),
        }
    }

    /// The files being written.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Finish every file. Returns how many bytes they hold.
    pub fn finish(self) -> io::Result<u64> {
        match self.files {
            Files::Each(each) => {
                let mut total = 0;
                let mut first_error = None;
                // Finish them all, even when one fails.
                for f in each {
                    match f.finish() {
                        Ok(n) => total += n,
                        Err(e) => {
                            first_error.get_or_insert(e);
                        }
                    }
                }
                match first_error {
                    Some(e) => Err(e),
                    None => Ok(total),
                }
            }
            Files::One(mka) => mka.finish(),
        }
    }
}

#[cfg(test)]
mod tests;
