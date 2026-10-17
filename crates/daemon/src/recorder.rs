//! Recordings: a thread that takes what the engine's taps hold and writes
//! it into files, a part at a time.
//!
//! The engine writes every tap every cycle, so the tracks of a recording
//! always hold the same number of frames, give or take the cycle being
//! written while they are read. The thread reads every tenth of a second,
//! writes each track up to where the current part ends, and starts the
//! next part when every track has reached it, so parts line up exactly.
//! A change of sample rate starts a new part too, since a file has one.
//!
//! What it is doing goes to the controller as [`Event`]s: its status once
//! a second, and once more when it has ended, whether asked to or not.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{info, warn};
use weir_engine::RecordTap;
use weir_protocol::{
    recording_files, LocalTime, NameParts, RecordFormat, RecorderSettings, RecordingStatus,
    RecordingTrack, TrackSource,
};
use weir_record::{Bwf, Part, Spec, TrackSpec};

/// How often the taps are read.
const READ_EVERY: Duration = Duration::from_millis(100);
/// How often the status is told.
const STATUS_EVERY: Duration = Duration::from_secs(1);
/// How long to wait for the engine's first cycle.
const FIRST_CYCLE_WAIT: Duration = Duration::from_secs(3);

/// One strip or bus to record.
pub struct PlanTrack {
    pub source: TrackSource,
    pub spec: TrackSpec,
    pub tap: Arc<RecordTap>,
}

/// Everything a recording needs to start.
pub struct Plan {
    pub tracks: Vec<PlanTrack>,
    pub settings: RecorderSettings,
    /// The recordings folder, a whole path.
    pub folder: PathBuf,
    /// The setup loaded, for `{setup}`.
    pub setup: Option<String>,
    /// Ticked strips and buses the mixer did not have.
    pub missing: Vec<TrackSource>,
    /// When it started, in milliseconds since the Unix epoch.
    pub started_ms: i64,
}

/// What the recording thread tells the controller.
pub enum Event {
    /// How it is going.
    Status(RecordingStatus),
    /// It has stopped and its files are finished.
    Ended(RecordingStatus),
}

/// A recording under way.
pub struct Recording {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Recording {
    /// Start writing `plan`, telling `send` how it goes.
    pub fn start(plan: Plan, send: impl Fn(Event) + Send + 'static) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("recorder".into())
            .spawn(move || Writer::new(plan, flag).run(&send))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// Ask it to stop: it takes what the taps still hold, and finishes its
    /// files.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Wait until its files are finished.
    pub fn join(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Whether it has ended.
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
}

/// The local time at `unix_ms`.
pub fn local_time(unix_ms: i64) -> LocalTime {
    let t = (unix_ms.div_euclid(1000)) as libc::time_t;
    // SAFETY: localtime_r only writes into the tm given.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    LocalTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
    }
}

/// Bytes free for this user on the disk holding `path`, or the nearest
/// folder above it that exists.
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let existing = path.ancestors().find(|p| p.exists())?;
    let c = std::ffi::CString::new(existing.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs only writes into the struct given.
    let st = unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        st
    };
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

/// The recordings folder `setting` names: a whole path, or `~/` and a path
/// in the home folder; empty is `Weir recordings` in the Music folder.
pub fn resolve_folder(setting: &str) -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("there is no home folder to record in")?;
    let setting = setting.trim();
    if setting.is_empty() {
        let music = dirs::audio_dir().unwrap_or_else(|| home.join("Music"));
        return Ok(music.join("Weir recordings"));
    }
    let path = match setting.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if setting == "~" => home,
        None => PathBuf::from(setting),
    };
    if !path.is_absolute() {
        return Err(format!(
            "'{setting}' is not a whole path: start it with / or ~/"
        ));
    }
    Ok(path)
}

/// `paths` with " 2", " 3"… added to every file's name, the same to each,
/// when any of them is taken: a recording never replaces a file.
fn free_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let with = |n: u32| -> Vec<PathBuf> {
        paths
            .iter()
            .map(|p| {
                let stem = p.file_stem().unwrap_or_default().to_string_lossy();
                let ext = p.extension().unwrap_or_default().to_string_lossy();
                p.with_file_name(format!("{stem} {n}.{ext}"))
            })
            .collect()
    };
    if !paths.iter().any(|p| p.exists()) {
        return paths;
    }
    (2..)
        .map(with)
        .find(|candidate| !candidate.iter().any(|p| p.exists()))
        .expect("some number is free")
}

/// An error writing files, in words.
fn say(e: &std::io::Error, folder: &Path) -> String {
    match e.kind() {
        std::io::ErrorKind::StorageFull => "the disk is full".into(),
        std::io::ErrorKind::PermissionDenied => {
            format!("Weir may not write in {}", folder.display())
        }
        _ => e.to_string(),
    }
}

/// The recording thread's state.
struct Writer {
    plan: Plan,
    stop: Arc<AtomicBool>,
    rate: u32,
    part: Option<Part>,
    part_number: u32,
    /// Frames each track has written into the current part.
    in_part: Vec<u64>,
    /// Frames the current part may hold; `u64::MAX` for no limit.
    part_frames: u64,
    /// Frames a part holds when the recording starts new files now and
    /// then.
    split: Option<u64>,
    /// Frames written before the current part.
    before_part: u64,
    /// Samples read but not written yet, per track.
    carry: Vec<Vec<f32>>,
    /// Bytes in the parts finished.
    done_bytes: u64,
    /// When the status was last told.
    told: Instant,
    error: Option<String>,
}

impl Writer {
    fn new(plan: Plan, stop: Arc<AtomicBool>) -> Self {
        let n = plan.tracks.len();
        Self {
            plan,
            stop,
            rate: 0,
            part: None,
            part_number: 0,
            in_part: vec![0; n],
            part_frames: u64::MAX,
            split: None,
            before_part: 0,
            carry: vec![Vec::new(); n],
            done_bytes: 0,
            told: Instant::now(),
            error: None,
        }
    }

    fn spec(&self) -> Spec {
        Spec {
            format: self.plan.settings.format,
            quality: self.plan.settings.quality(),
            rate: self.rate,
            layout: self.plan.settings.layout,
        }
    }

    /// Take up `rate`: the length of parts goes by it.
    fn set_rate(&mut self, rate: u32) {
        self.rate = rate;
        let minutes = self.plan.settings.split_minutes as u64;
        self.split = (minutes > 0).then(|| minutes * 60 * rate as u64);
    }

    /// Start the next part.
    fn open_part(&mut self) -> Result<(), String> {
        self.part_number += 1;
        let s = &self.plan.settings;
        let when = local_time(self.plan.started_ms);
        let names: Vec<&str> = self
            .plan
            .tracks
            .iter()
            .map(|t| t.spec.name.as_str())
            .collect();
        let parts = NameParts {
            pattern: &s.names,
            layout: s.layout,
            format: s.format,
            when,
            number: s.next_number,
            part: self.part_number,
            splits: s.split_minutes > 0 || self.part_number > 1,
            setup: self.plan.setup.as_deref(),
        };
        let paths: Vec<PathBuf> = recording_files(&parts, &names)
            .into_iter()
            .map(|f| self.plan.folder.join(f))
            .collect();
        let paths = free_paths(paths);
        let now_ms = crate::history::now_ms() as i64;
        let part_start = local_time(now_ms);
        let bwf = (s.format == RecordFormat::Wav && s.bwf).then(|| Bwf {
            start: part_start,
            start_ms_fraction: now_ms.rem_euclid(1000) as u32,
            reference: format!("Weir {} {}", part_start.date(), part_start.time()),
        });
        let title = paths[0]
            .file_stem()
            .map(|t| t.to_string_lossy().into_owned())
            .unwrap_or_default();
        let specs: Vec<TrackSpec> = self.plan.tracks.iter().map(|t| t.spec.clone()).collect();
        let spec = self.spec();
        let part = Part::create(&spec, &specs, &paths, &title, bwf.as_ref(), now_ms)
            .map_err(|e| format!("could not start recording: {}", say(&e, &self.plan.folder)))?;
        info!(
            "recording part {} to {}",
            self.part_number,
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let channels: Vec<usize> = specs.iter().map(|t| t.positions.len()).collect();
        self.part_frames = [self.split, weir_record::max_frames(&spec, &channels)]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(u64::MAX);
        self.in_part.iter_mut().for_each(|f| *f = 0);
        self.part = Some(part);
        Ok(())
    }

    /// Finish the current part, its tracks made the same length first.
    fn close_part(&mut self) -> Result<(), String> {
        let Some(mut part) = self.part.take() else {
            return Ok(());
        };
        let longest = self.in_part.iter().copied().max().unwrap_or(0);
        let mut result = Ok(());
        for (i, t) in self.plan.tracks.iter().enumerate() {
            let short = longest - self.in_part[i];
            if short > 0 {
                let silence = vec![0.0; short as usize * t.spec.positions.len()];
                if let Err(e) = part.write(i, &silence) {
                    result = Err(say(&e, &self.plan.folder));
                }
            }
        }
        self.before_part += longest;
        match part.finish() {
            Ok(bytes) => self.done_bytes += bytes,
            Err(e) => result = Err(say(&e, &self.plan.folder)),
        }
        self.in_part.iter_mut().for_each(|f| *f = 0);
        result
    }

    /// Read every tap, keeping what they held.
    fn read_taps(&mut self) -> usize {
        let mut got = 0;
        for (t, carry) in self.plan.tracks.iter().zip(&mut self.carry) {
            got += t.tap.take(carry);
        }
        got
    }

    /// Write what was read, starting new parts where they are due.
    fn write_carried(&mut self) -> Result<(), String> {
        loop {
            let Some(part) = self.part.as_mut() else {
                return Ok(());
            };
            for (i, t) in self.plan.tracks.iter().enumerate() {
                let n = t.spec.positions.len();
                let room = self.part_frames - self.in_part[i];
                let frames = ((self.carry[i].len() / n) as u64).min(room) as usize;
                if frames == 0 {
                    continue;
                }
                part.write(i, &self.carry[i][..frames * n])
                    .map_err(|e| say(&e, &self.plan.folder))?;
                self.carry[i].drain(..frames * n);
                self.in_part[i] += frames as u64;
            }
            if self.in_part.iter().all(|f| *f >= self.part_frames) {
                self.close_part()?;
                self.open_part()?;
            } else {
                return Ok(());
            }
        }
    }

    fn status(&self, recording: bool) -> RecordingStatus {
        let rate = self.rate.max(1) as f64;
        let in_part = self.in_part.iter().copied().max().unwrap_or(0);
        let files: Vec<String> = self
            .part
            .as_ref()
            .map(|p| p.paths().iter().map(|p| p.display().to_string()).collect())
            .unwrap_or_default();
        let folder = self
            .part
            .as_ref()
            .and_then(|p| p.paths().first())
            .and_then(|p| p.parent())
            .unwrap_or(&self.plan.folder)
            .display()
            .to_string();
        let tracks = self
            .plan
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| RecordingTrack {
                source: t.source.clone(),
                channels: t.spec.positions.len(),
                file: files
                    .get(i)
                    .or_else(|| files.first())
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect();
        let lost = self
            .plan
            .tracks
            .iter()
            .map(|t| t.tap.lost_frames())
            .max()
            .unwrap_or(0);
        RecordingStatus {
            recording,
            started_at_ms: Some(self.plan.started_ms as u64),
            seconds: (self.before_part + in_part) as f64 / rate,
            folder,
            tracks,
            part: self.part_number.max(1),
            next_part_in: (recording && self.part_frames != u64::MAX)
                .then(|| self.part_frames.saturating_sub(in_part) as f64 / rate),
            bytes: self.done_bytes + self.part.as_ref().map_or(0, Part::bytes),
            free_bytes: free_bytes(&self.plan.folder),
            lost_seconds: lost as f64 / rate,
            missing: self.plan.missing.clone(),
            error: self.error.clone(),
        }
    }

    /// Wait for the engine's first cycle into every tap, for the rate.
    fn first_rate(&self) -> Option<u32> {
        let until = Instant::now() + FIRST_CYCLE_WAIT;
        loop {
            let rates: Vec<u32> = self.plan.tracks.iter().map(|t| t.tap.rate()).collect();
            if rates.iter().all(|r| *r > 0) {
                return rates.first().copied();
            }
            if Instant::now() > until || self.stop.load(Ordering::Acquire) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn run(mut self, send: &dyn Fn(Event)) {
        match self.first_rate() {
            Some(rate) => self.set_rate(rate),
            None => {
                self.error =
                    Some("the audio engine is not running, so there is nothing to record".into());
                send(Event::Ended(self.status(false)));
                return;
            }
        }
        if let Err(e) = self.open_part() {
            warn!("{e}");
            self.error = Some(e);
            send(Event::Ended(self.status(false)));
            return;
        }
        send(Event::Status(self.status(true)));
        while !self.stop.load(Ordering::Acquire) {
            std::thread::sleep(READ_EVERY);
            if let Err(e) = self.step() {
                warn!("recording stopped: {e}");
                self.error = Some(e);
                break;
            }
            if self.told.elapsed() >= STATUS_EVERY {
                self.told = Instant::now();
                send(Event::Status(self.status(true)));
            }
        }
        if self.error.is_none() {
            // The engine may still be writing a cycle or two: take what
            // comes until nothing does.
            let until = Instant::now() + Duration::from_secs(1);
            let mut quiet = 0;
            while quiet < 2 && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(30));
                if self.read_taps() == 0 {
                    quiet += 1;
                } else {
                    quiet = 0;
                }
            }
            if let Err(e) = self.write_carried() {
                self.error = Some(e);
            }
        }
        if let Err(e) = self.close_part() {
            self.error.get_or_insert(e);
        }
        let status = self.status(false);
        info!(
            "recorded {:.1} s, {} bytes{}",
            status.seconds,
            status.bytes,
            status
                .error
                .as_ref()
                .map(|e| format!(", stopped early: {e}"))
                .unwrap_or_default()
        );
        send(Event::Ended(status));
    }

    /// One read of the taps, written out.
    fn step(&mut self) -> Result<(), String> {
        self.read_taps();
        let rate = self.plan.tracks.first().map_or(0, |t| t.tap.rate());
        if rate != 0 && rate != self.rate {
            // A file has one sample rate: what was read goes into the part
            // under way, and a new part starts at the new rate.
            info!("the sample rate changed to {rate} Hz: starting a new part");
            let limit = self.part_frames;
            self.part_frames = u64::MAX;
            self.write_carried()?;
            self.part_frames = limit;
            self.close_part()?;
            self.set_rate(rate);
            self.open_part()?;
            return Ok(());
        }
        self.write_carried()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weir_protocol::{ChannelPosition, StripOrBus};

    fn feed(taps: &[Arc<RecordTap>], cycle: u64, frames: usize, value: f32) {
        let block = vec![value; frames];
        for t in taps {
            t.write(cycle, frames, 48_000, |_| Some(&block[..]), &[]);
        }
    }

    #[test]
    fn parts_split_at_the_same_frame_and_end_the_same_length() {
        let dir = std::env::temp_dir().join(format!("weir-recorder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let taps: Vec<Arc<RecordTap>> = [StripOrBus::Strip(1), StripOrBus::Bus(1)]
            .into_iter()
            .map(|t| Arc::new(RecordTap::new(t, 2)))
            .collect();
        let sources = [
            TrackSource::Strip("Mic".into()),
            TrackSource::Bus("Headset".into()),
        ];
        let plan = Plan {
            tracks: taps
                .iter()
                .zip(&sources)
                .map(|(tap, source)| PlanTrack {
                    source: source.clone(),
                    spec: TrackSpec {
                        name: source.name().into(),
                        positions: vec![ChannelPosition::FL, ChannelPosition::FR],
                    },
                    tap: tap.clone(),
                })
                .collect(),
            settings: RecorderSettings {
                format: RecordFormat::Wav,
                bit_depth: 16,
                names: "{track}".into(),
                bwf: true,
                ..Default::default()
            },
            folder: dir.clone(),
            setup: None,
            missing: Vec::new(),
            started_ms: 1_760_000_000_000,
        };
        let mut writer = Writer::new(plan, Arc::new(AtomicBool::new(false)));
        // A part every 10000 frames, as splitting would.
        writer.set_rate(48_000);
        writer.split = Some(10_000);
        writer.open_part().unwrap();
        // The bus is a cycle behind the strip when read.
        let block = vec![0.25f32; 4096];
        for cycle in 0..6u64 {
            taps[0].write(cycle, 4096, 48_000, |_| Some(&block[..]), &[]);
            writer.step().unwrap();
            taps[1].write(cycle, 4096, 48_000, |_| Some(&block[..]), &[]);
        }
        writer.step().unwrap();
        feed(&taps[..1], 6, 100, 0.5);
        writer.step().unwrap();
        assert_eq!(writer.part_number, 3);
        writer.close_part().unwrap();
        let status = writer.status(false);
        // 6 cycles, then 100 more frames of the strip; the bus is made
        // as long in the last part.
        assert_eq!(status.part, 3);
        assert!((status.seconds - (6.0 * 4096.0 + 100.0) / 48_000.0).abs() < 1e-9);
        for name in ["Mic", "Headset"] {
            for (file, frames) in [
                (format!("{name}.wav"), 10_000),
                (format!("{name} part 2.wav"), 10_000),
                (format!("{name} part 3.wav"), 6 * 4096 + 100 - 20_000),
            ] {
                let bytes = std::fs::read(dir.join(&file)).unwrap();
                let data = bytes.windows(4).position(|w| w == b"data").unwrap();
                let len = u32::from_le_bytes(bytes[data + 4..data + 8].try_into().unwrap());
                assert_eq!(len as usize, frames * 4, "{file}");
                assert!(bytes.windows(4).any(|w| w == b"bext"), "{file}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn taken_names_get_a_number_and_folders_resolve() {
        let dir = std::env::temp_dir().join(format!("weir-recorder-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("Mic.flac");
        let b = dir.join("Music.flac");
        std::fs::write(&b, b"").unwrap();
        assert_eq!(
            free_paths(vec![a.clone(), b.clone()]),
            [dir.join("Mic 2.flac"), dir.join("Music 2.flac")]
        );
        std::fs::remove_file(&b).unwrap();
        assert_eq!(free_paths(vec![a.clone()]), [a]);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(resolve_folder("recordings").is_err());
        assert_eq!(resolve_folder("/x/y").unwrap(), PathBuf::from("/x/y"));
        assert!(resolve_folder("~/Rec").unwrap().ends_with("Rec"));
        assert!(resolve_folder("").unwrap().ends_with("Weir recordings"));
        let t = local_time(0);
        assert!(t.year == 1970 || t.year == 1969);
    }
}
