//! Recording: the recorder's settings, kept with the configuration, which
//! strips and buses it records, and starting and stopping it. The writing
//! itself is `crate::recorder`'s thread.

use super::names::Rename;
use super::Controller;
use crate::recorder::{self, Event, Plan, PlanTrack, Recording};
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use tracing::{info, warn};
use weir_engine::RecordTap;
use weir_protocol::*;
use weir_record::TrackSpec;

impl Controller {
    /// Let the controller hand recordings a way back to it. Called once,
    /// when it is in its `Arc`.
    pub fn set_self(self: &Arc<Self>) {
        let _ = self.me.set(Arc::downgrade(self));
    }

    /// The recorder's settings, as the configuration had them.
    pub fn set_recorder_settings(&self, mut settings: RecorderSettings) {
        settings.normalize();
        self.inner.lock().unwrap().recorder = settings;
    }

    pub(super) fn recorder_settings(&self) -> RecorderSettings {
        self.inner.lock().unwrap().recorder.clone()
    }

    /// What the recorder is doing; when idle, where the next recording
    /// goes.
    pub(super) fn recording_status(&self) -> RecordingStatus {
        let inner = self.inner.lock().unwrap();
        let mut status = inner.recording.clone();
        if !status.recording {
            if let Ok(folder) = recorder::resolve_folder(&inner.recorder.folder) {
                status.free_bytes = recorder::free_bytes(&folder);
                status.folder = folder.display().to_string();
            }
        }
        status
    }

    fn recorder_changed(&self) {
        let settings = self.recorder_settings();
        self.dirty.store(true, Ordering::Release);
        self.announce(Notification::RecorderChanged(settings));
    }

    pub(super) fn set_recorder(&self, p: RecorderPatch) -> Result<Value, RpcError> {
        if let Some(names) = &p.names {
            if let Some(problem) = record_names_problem(names) {
                return Err(RpcError::invalid_params(format!(
                    "these file names will not do: {problem}"
                )));
            }
        }
        if let Some(folder) = &p.folder {
            recorder::resolve_folder(folder).map_err(RpcError::invalid_params)?;
        }
        if let Some(b) = p.bit_depth {
            if !RecordFormat::Wav.qualities().contains(&b) {
                return Err(RpcError::invalid_params("bit_depth is 16 or 24"));
            }
        }
        if p.split_minutes
            .is_some_and(|m| m > RECORD_SPLIT_MAX_MINUTES)
        {
            return Err(RpcError::invalid_params(format!(
                "split_minutes is at most {RECORD_SPLIT_MAX_MINUTES}"
            )));
        }
        let folder_changed = {
            let mut inner = self.inner.lock().unwrap();
            let s = &mut inner.recorder;
            let before = s.folder.clone();
            if let Some(v) = p.tracks {
                s.tracks = v;
            }
            if let Some(v) = p.folder {
                s.folder = v;
            }
            if let Some(v) = p.names {
                s.names = v;
            }
            if let Some(v) = p.format {
                s.format = v;
            }
            if let Some(v) = p.bit_depth {
                s.bit_depth = v;
            }
            if let Some(v) = p.kbps {
                s.kbps = v;
            }
            if let Some(v) = p.layout {
                s.layout = v;
            }
            if let Some(v) = p.split_minutes {
                s.split_minutes = v;
            }
            if let Some(v) = p.bwf {
                s.bwf = v.apply(s.bwf);
            }
            if let Some(v) = p.next_number {
                s.next_number = v;
            }
            s.normalize();
            s.folder != before
        };
        self.recorder_changed();
        if folder_changed {
            self.announce(Notification::RecordingChanged(self.recording_status()));
        }
        Ok(to_json(&self.recorder_settings()))
    }

    pub(super) fn set_record_track(&self, p: SetRecordTrackParams) -> Result<Value, RpcError> {
        let source = {
            let inner = self.inner.lock().unwrap();
            match (p.strip, p.bus) {
                (Some(id), None) => inner
                    .mixer
                    .strip(id)
                    .map(|s| TrackSource::Strip(s.name.clone()))
                    .ok_or_else(|| RpcError::application(format!("no strip with id {id}")))?,
                (None, Some(id)) => inner
                    .mixer
                    .bus(id)
                    .map(|b| TrackSource::Bus(b.name.clone()))
                    .ok_or_else(|| RpcError::application(format!("no bus with id {id}")))?,
                _ => return Err(RpcError::invalid_params("give a strip or a bus")),
            }
        };
        {
            let mut inner = self.inner.lock().unwrap();
            let s = &mut inner.recorder;
            let on = p.on.apply(s.records(&source));
            s.tracks.retain(|t| !t.same(&source));
            if on {
                s.tracks.push(source);
            }
        }
        self.recorder_changed();
        Ok(to_json(&self.recorder_settings()))
    }

    pub(super) fn set_recording(&self, p: SetRecordingParams) -> Result<Value, RpcError> {
        let recording = self.recording_now();
        if p.on.apply(recording) {
            if !recording {
                self.start_recording()?;
            }
        } else if recording {
            self.stop_recording();
        }
        Ok(to_json(&self.recording_status()))
    }

    /// Start or stop recording, for the tray. Stopping waits until the
    /// files are finished.
    pub fn record(&self, on: bool) -> Result<(), String> {
        self.set_recording(SetRecordingParams { on: Flag::Set(on) })
            .map(|_| ())
            .map_err(|e| e.message)
    }

    /// Whether any strip or bus is ticked for recording.
    pub fn can_record(&self) -> bool {
        !self.inner.lock().unwrap().recorder.tracks.is_empty()
    }

    /// Whether a recording is under way.
    pub(super) fn recording_now(&self) -> bool {
        self.recording
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|r| !r.finished())
    }

    fn start_recording(&self) -> Result<(), RpcError> {
        let Some(engine) = &self.engine else {
            return Err(RpcError::application(
                "the audio engine is not running, so there is nothing to record",
            ));
        };
        let (settings, mixer, setup) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.recorder.clone(),
                inner.mixer.clone(),
                inner.setup.clone(),
            )
        };
        // Tracks in the mixer's order, strips first.
        let strips = mixer.strips.iter().filter_map(|s| {
            let source = TrackSource::Strip(s.name.clone());
            settings
                .records(&source)
                .then(|| (source, s.layout.positions()))
        });
        let buses = mixer.buses.iter().filter_map(|b| {
            let source = TrackSource::Bus(b.name.clone());
            settings
                .records(&source)
                .then(|| (source, b.layout.positions()))
        });
        let tracks: Vec<PlanTrack> = strips
            .chain(buses)
            .filter(|(_, positions)| !positions.is_empty())
            .map(|(source, positions)| PlanTrack {
                tap: Arc::new(RecordTap::new(source.clone(), positions.len())),
                spec: TrackSpec {
                    name: source.name().to_string(),
                    positions,
                },
                source,
            })
            .collect();
        let missing: Vec<TrackSource> = settings
            .tracks
            .iter()
            .filter(|t| !tracks.iter().any(|p| p.source.same(t)))
            .cloned()
            .collect();
        if tracks.is_empty() {
            return Err(RpcError::application(if settings.tracks.is_empty() {
                "nothing to record: tick the strips and buses to record first"
            } else {
                "none of the strips and buses ticked for recording are in this mixer"
            }));
        }
        let folder = recorder::resolve_folder(&settings.folder).map_err(RpcError::application)?;
        std::fs::create_dir_all(&folder).map_err(|e| {
            RpcError::application(format!(
                "could not make the recordings folder {}: {e}",
                folder.display()
            ))
        })?;
        let taps: Vec<Arc<RecordTap>> = tracks.iter().map(|t| t.tap.clone()).collect();
        let started_ms = crate::history::now_ms() as i64;
        let status = RecordingStatus {
            recording: true,
            started_at_ms: Some(started_ms as u64),
            folder: folder.display().to_string(),
            tracks: tracks
                .iter()
                .map(|t| RecordingTrack {
                    source: t.source.clone(),
                    channels: t.spec.positions.len(),
                    file: String::new(),
                })
                .collect(),
            part: 1,
            free_bytes: recorder::free_bytes(&folder),
            missing: missing.clone(),
            ..Default::default()
        };
        let names: Vec<&str> = tracks.iter().map(|t| t.spec.name.as_str()).collect();
        info!("recording {}", names.join(", "));
        let plan = Plan {
            tracks,
            settings,
            folder,
            setup,
            missing,
            started_ms,
        };
        engine
            .set_record_taps(taps.clone())
            .map_err(|e| RpcError::application(format!("the audio engine: {e}")))?;
        let me: Weak<Controller> = self.me.get().cloned().unwrap_or_default();
        let recording = Recording::start(plan, taps, move |ev| {
            if let Some(c) = me.upgrade() {
                c.on_recorder_event(ev);
            }
        });
        let recording = match recording {
            Ok(r) => r,
            Err(e) => {
                let _ = engine.set_record_taps(Vec::new());
                return Err(RpcError::application(format!(
                    "could not start recording: {e}"
                )));
            }
        };
        *self.recording.lock().unwrap() = Some(recording);
        {
            let mut inner = self.inner.lock().unwrap();
            inner.recording = status.clone();
            inner.recorder.next_number = inner.recorder.next_number.saturating_add(1);
        }
        self.recorder_changed();
        self.announce(Notification::RecordingChanged(status));
        Ok(())
    }

    /// Stop the recording under way, and wait until its files are
    /// finished.
    pub fn stop_recording(&self) {
        // Out of the lock first: the thread's last word takes it too.
        let recording = self.recording.lock().unwrap().take();
        if let Some(r) = recording {
            if let Some(engine) = &self.engine {
                let _ = engine.set_record_taps(Vec::new());
            }
            r.stop();
            r.join();
        }
    }

    /// What the recording thread says.
    pub(crate) fn on_recorder_event(&self, ev: Event) {
        let (status, ended) = match ev {
            Event::Status(s) => (s, false),
            Event::Ended(s) => (s, true),
        };
        if ended {
            if let Some(e) = &status.error {
                warn!("the recording ended early: {e}");
            }
            // It ended by itself, or was stopped: the engine stops filling
            // its taps either way.
            if let Some(engine) = &self.engine {
                let _ = engine.set_record_taps(Vec::new());
            }
        }
        self.inner.lock().unwrap().recording = status;
        self.announce(Notification::RecordingChanged(self.recording_status()));
    }

    /// Ticked strips and buses keep their names when renamed, and so does
    /// a recording under way, before the engine hears of the new names.
    pub(super) fn recorder_follow_renames(&self, renamed: &[Rename]) {
        if let Some(r) = &*self.recording.lock().unwrap() {
            for (kind, old, new) in renamed {
                let old = match kind {
                    TargetKind::Strip => TrackSource::Strip(old.clone()),
                    TargetKind::Bus => TrackSource::Bus(old.clone()),
                };
                r.rename(&old, new);
            }
        }
        let changed = {
            let mut inner = self.inner.lock().unwrap();
            let mut changed = false;
            for t in &mut inner.recorder.tracks {
                let (kind, name) = match t {
                    TrackSource::Strip(n) => (TargetKind::Strip, n),
                    TrackSource::Bus(n) => (TargetKind::Bus, n),
                };
                if let Some((_, _, new)) = renamed
                    .iter()
                    .find(|(k, old, _)| *k == kind && old.eq_ignore_ascii_case(name))
                {
                    *name = new.clone();
                    changed = true;
                }
            }
            changed
        };
        if changed {
            self.recorder_changed();
        }
    }
}
