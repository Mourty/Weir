//! What the daemon sees of the engine: [`Engine::spawn`] starts the
//! PipeWire thread and returns an [`EngineHandle`] to drive it with, and
//! [`EngineEvent`]s come back as PipeWire changes.

use super::filter::FilterShared;
use super::runner;
use crate::dsp::analyzer::Analyzer;
use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tracing::{error, warn};
use weir_protocol::{
    AppStream, DeviceInfo, EngineStatus, InsertStatus, Meters, MixerState, SoloMode, Spectrum,
    StripId, StripOrBus, SystemVolumes,
};

/// Something the engine could not do.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// PipeWire refused, or is not there.
    #[error("PipeWire error: {0}")]
    PipeWire(String),
    /// The PipeWire thread has stopped.
    #[error("engine thread is not running")]
    NotRunning,
}

/// How the engine runs, apart from the mix itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineOptions {
    /// What soloing a strip does.
    pub solo: SoloMode,
    /// Hold PipeWire at this sample rate, or let it decide.
    pub sample_rate: Option<u32>,
    /// Hold PipeWire at this buffer size in frames, or let it decide.
    pub quantum: Option<u32>,
}

/// Commands from the daemon to the PipeWire thread.
pub(super) enum EngineCommand {
    /// Replace the desired mixer configuration.
    SetState(MixerState),
    /// Change how the engine runs.
    SetOptions(EngineOptions),
    /// Move an application stream to a virtual input strip.
    MoveApp { app: u32, strip: StripId },
    /// Set an application stream's own volume and/or mute.
    SetAppVolume {
        app: u32,
        volume_db: Option<f32>,
        mute: Option<bool>,
    },
    /// An application's volume changed underneath us, so republish the list.
    /// Sent by the node parameter callbacks to themselves.
    AppsDirty,
    /// Filter state changed (sent by the filter callback to itself so the
    /// handler runs outside any other callback).
    FilterStateChanged,
    /// A link this program made, between these output and input ports, was
    /// removed. Sent by the registry callback to itself, so the handler
    /// runs after the rest of the removals that came with it.
    LinkGone(u32, u32),
    /// Stop the main loop.
    Shutdown,
}

/// Events from the PipeWire thread to the daemon. Each is sent only when
/// what it reports changed.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// The engine's connection to PipeWire, sample rate or buffer size.
    Status(EngineStatus),
    /// The devices hardware strips and buses can use.
    Devices(Vec<DeviceInfo>),
    /// The applications playing sound.
    Apps(Vec<AppStream>),
    /// The system volumes of Weir's own virtual devices.
    SystemVolumes(SystemVolumes),
    /// Whether the external effects of each strip and bus that has them on
    /// are connected.
    Inserts(Vec<InsertStatus>),
}

/// Handle used by the daemon to drive the engine from any thread.
pub struct EngineHandle {
    tx: pipewire::channel::Sender<EngineCommand>,
    shared: Arc<FilterShared>,
    thread: Mutex<Option<JoinHandle<()>>>,
    analyzer: Mutex<Analyzer>,
}

impl EngineHandle {
    fn send(&self, command: EngineCommand) -> Result<(), EngineError> {
        self.tx.send(command).map_err(|_| EngineError::NotRunning)
    }

    /// Make the engine mix as `state` says: ports, virtual devices, links
    /// and the real-time snapshot all follow.
    pub fn set_state(&self, state: MixerState) -> Result<(), EngineError> {
        self.send(EngineCommand::SetState(state))
    }

    /// Change how the engine runs.
    pub fn set_options(&self, options: EngineOptions) -> Result<(), EngineError> {
        self.send(EngineCommand::SetOptions(options))
    }

    /// Move application stream `app` onto virtual strip `strip`.
    pub fn move_app(&self, app: u32, strip: StripId) -> Result<(), EngineError> {
        self.send(EngineCommand::MoveApp { app, strip })
    }

    /// Set an application stream's own volume, the one the system volume
    /// applet shows. Absent fields are left alone.
    pub fn set_app_volume(
        &self,
        app: u32,
        volume_db: Option<f32>,
        mute: Option<bool>,
    ) -> Result<(), EngineError> {
        self.send(EngineCommand::SetAppVolume {
            app,
            volume_db,
            mute,
        })
    }

    /// The equalizer spectra of `targets`, and nothing for anything else.
    ///
    /// Call this regularly, with an empty set when nobody is watching: it is
    /// also what tells the real-time thread which strips and buses to feed
    /// the analyzer, so it can stop the moment the last window closes.
    pub fn take_spectra(&self, targets: &BTreeSet<StripOrBus>) -> Vec<Spectrum> {
        let params = self.shared.params.load();
        for s in &params.strips {
            s.fx.set_analyzing(targets.contains(&StripOrBus::Strip(s.id)));
        }
        for b in &params.buses {
            b.fx.set_analyzing(targets.contains(&StripOrBus::Bus(b.id)));
        }
        let Ok(mut analyzer) = self.analyzer.lock() else {
            return Vec::new();
        };
        analyzer.retain(targets);
        if targets.is_empty() {
            return Vec::new();
        }
        let rate = match self.shared.rate.load(Ordering::Relaxed) {
            0 => 48_000,
            r => r,
        };
        let now = std::time::Instant::now();
        targets
            .iter()
            .filter_map(|&target| {
                let fx = match target {
                    StripOrBus::Strip(id) => &params.strips.iter().find(|s| s.id == id)?.fx,
                    StripOrBus::Bus(id) => &params.buses.iter().find(|b| b.id == id)?.fx,
                };
                Some(Spectrum {
                    target,
                    input_db: analyzer.analyze(target, false, &fx.tap_in, rate, now),
                    output_db: analyzer.analyze(target, true, &fx.tap_out, rate, now),
                })
            })
            .collect()
    }

    /// Read and reset the peak meters of the currently published snapshot.
    pub fn take_meters(&self) -> Meters {
        let params = self.shared.params.load();
        let (strips, buses) = params.take_peaks();
        Meters {
            strips: strips.into_iter().collect(),
            buses: buses.into_iter().collect(),
            gates: params.take_gate_meters().into_iter().collect(),
            limiters: params.take_limiter_meters().into_iter().collect(),
            compressors: params.take_compressor_meters().into_iter().collect(),
            ducking: params.take_duck_meters().into_iter().collect(),
        }
    }

    /// How the engine is doing now.
    pub fn status(&self) -> EngineStatus {
        self.shared.status()
    }

    /// Stop the PipeWire thread and wait for it, but no longer than `limit`.
    /// Returns false if the thread was still running when time ran out.
    ///
    /// The wait is bounded because this runs while the session is ending,
    /// when PipeWire itself may be stopping at the same moment. A daemon that
    /// waits forever there holds up logout and shutdown.
    pub fn shutdown(&self, limit: std::time::Duration) -> bool {
        let _ = self.tx.send(EngineCommand::Shutdown);
        let Ok(mut t) = self.thread.lock() else {
            return false;
        };
        let Some(h) = t.take() else {
            return true;
        };
        let deadline = std::time::Instant::now() + limit;
        while !h.is_finished() {
            if std::time::Instant::now() >= deadline {
                warn!("the PipeWire thread did not stop in time; leaving it behind");
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = h.join();
        true
    }
}

/// Starts the engine.
pub struct Engine;

impl Engine {
    /// Start the PipeWire thread, mixing as `initial` says, with `on_event`
    /// called (on that thread) whenever something PipeWire reports changes.
    /// Returns once the connection to PipeWire is established, or failed.
    pub fn spawn(
        initial: MixerState,
        on_event: impl Fn(EngineEvent) + Send + 'static,
    ) -> Result<EngineHandle, EngineError> {
        let (tx, rx) = pipewire::channel::channel::<EngineCommand>();
        let shared = FilterShared::new();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let thread = {
            let shared = shared.clone();
            let tx = tx.clone();
            std::thread::Builder::new()
                .name("weir-pw".into())
                .spawn(move || {
                    if let Err(e) =
                        runner::run(initial, rx, tx, shared, Box::new(on_event), ready_tx)
                    {
                        error!("engine thread ended with error: {e}");
                    }
                })
                .map_err(|e| EngineError::PipeWire(e.to_string()))?
        };
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(EngineHandle {
                tx,
                shared,
                thread: Mutex::new(Some(thread)),
                analyzer: Mutex::new(Analyzer::new()),
            }),
            Ok(Err(e)) => Err(EngineError::PipeWire(e)),
            Err(_) => Err(EngineError::PipeWire(
                "engine thread died during startup".into(),
            )),
        }
    }
}
