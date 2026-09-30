//! Wrapper over `pw_filter`: the engine node.
//!
//! One `pw_filter` does all of Weir's mixing. It has an input port for every
//! strip channel and an output port for every bus channel, and PipeWire
//! calls [`on_process`] on its real-time thread once per cycle with a
//! buffer for each.

use crate::dsp::{build_rt_params, params::NoPorts, PortPtr, Processor, RtParams};
use arc_swap::ArcSwap;
use libspa_sys as spa_sys;
use pipewire::properties::PropertiesBox;
use pipewire::sys as pw_sys;
use std::cell::UnsafeCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use weir_protocol::{ChannelPosition, EngineStatus, MixerState, SoloMode};

/// How long every change of level, route or pan takes to ramp, in ms: long
/// enough never to click, short enough to feel immediate.
const RAMP_MS: u32 = 10;

/// Which way a port carries audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Into the engine: a strip channel.
    Input,
    /// Out of the engine: a bus channel.
    Output,
}

/// Data shared between the real-time process callback, the runner thread
/// and the daemon (for meters and status).
pub struct FilterShared {
    /// The published parameter snapshot.
    pub params: ArcSwap<RtParams>,
    processor: UnsafeCell<Processor>,
    /// The sample rate of the last cycle, 0 before the first.
    pub rate: AtomicU32,
    /// The length of the last cycle in frames, 0 before the first.
    pub quantum: AtomicU32,
    /// One of the `pw_filter_state` values.
    pub state: AtomicI32,
    /// Global id of the engine node, `u32::MAX` until known.
    pub node_id: AtomicU32,
    /// What PipeWire said went wrong, when the state is an error.
    pub error: Mutex<Option<String>>,
    /// Invoked on the main-loop thread whenever the filter state changes.
    pub state_cb: Mutex<Option<Box<dyn Fn() + Send>>>,
}

// `processor` is only touched from PipeWire's data thread, one cycle at a
// time.
unsafe impl Sync for FilterShared {}
unsafe impl Send for FilterShared {}

impl FilterShared {
    /// Shared state with an empty snapshot, for a filter about to start.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            params: ArcSwap::from(build_rt_params(
                &MixerState::default(),
                SoloMode::Exclusive,
                &NoPorts,
                None,
            )),
            processor: UnsafeCell::new(Processor::new()),
            rate: AtomicU32::new(0),
            quantum: AtomicU32::new(0),
            state: AtomicI32::new(pw_sys::pw_filter_state_PW_FILTER_STATE_UNCONNECTED),
            node_id: AtomicU32::new(u32::MAX),
            error: Mutex::new(None),
            state_cb: Mutex::new(None),
        })
    }

    /// The filter's state as the protocol names it.
    pub fn state_name(&self) -> &'static str {
        match self.state.load(Ordering::Relaxed) {
            pw_sys::pw_filter_state_PW_FILTER_STATE_ERROR => "error",
            pw_sys::pw_filter_state_PW_FILTER_STATE_UNCONNECTED => "unconnected",
            pw_sys::pw_filter_state_PW_FILTER_STATE_CONNECTING => "connecting",
            pw_sys::pw_filter_state_PW_FILTER_STATE_PAUSED => "paused",
            pw_sys::pw_filter_state_PW_FILTER_STATE_STREAMING => "streaming",
            _ => "unknown",
        }
    }

    /// How the engine is doing, from what the callbacks recorded.
    pub fn status(&self) -> EngineStatus {
        let state = self.state_name().to_string();
        EngineStatus {
            connected: state == "streaming" || state == "paused",
            state,
            sample_rate: self.rate.load(Ordering::Relaxed),
            quantum: self.quantum.load(Ordering::Relaxed),
            node_id: match self.node_id.load(Ordering::Relaxed) {
                u32::MAX => None,
                id => Some(id),
            },
            error: self.error.lock().ok().and_then(|e| e.clone()),
        }
    }
}

static EVENTS: pw_sys::pw_filter_events = pw_sys::pw_filter_events {
    version: pw_sys::PW_VERSION_FILTER_EVENTS,
    destroy: None,
    state_changed: Some(on_state_changed),
    io_changed: None,
    param_changed: None,
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_process),
    drained: None,
    command: None,
};

/// PipeWire's report of a change in the filter's state, on the main loop
/// thread.
unsafe extern "C" fn on_state_changed(
    data: *mut c_void,
    _old: pw_sys::pw_filter_state,
    state: pw_sys::pw_filter_state,
    error: *const c_char,
) {
    let shared = &*(data as *const FilterShared);
    shared.state.store(state, Ordering::Relaxed);
    let msg = if error.is_null() {
        None
    } else {
        Some(CStr::from_ptr(error).to_string_lossy().into_owned())
    };
    if let Ok(mut e) = shared.error.lock() {
        *e = msg;
    }
    if let Ok(cb) = shared.state_cb.lock() {
        if let Some(cb) = cb.as_ref() {
            cb();
        }
    }
}

/// Real-time process callback. Resolves every port buffer exactly once
/// (PipeWire hands out a port's buffer only once per cycle) and runs the
/// mixer.
unsafe extern "C" fn on_process(data: *mut c_void, position: *mut spa_sys::spa_io_position) {
    let shared = &*(data as *const FilterShared);
    if position.is_null() {
        return;
    }
    let clock = &(*position).clock;
    let n = clock.duration as usize;
    let rate = clock
        .rate
        .denom
        .checked_div(clock.rate.num)
        .unwrap_or(48_000);
    shared.rate.store(rate, Ordering::Relaxed);
    shared.quantum.store(n as u32, Ordering::Relaxed);

    let guard = shared.params.load();
    let params: &Arc<RtParams> = &guard;

    for strip in &params.strips {
        for (c, &port) in strip.ports.iter().enumerate() {
            let buf = if port.is_null() {
                std::ptr::null()
            } else {
                pw_sys::pw_filter_get_dsp_buffer(port, n as u32) as *const f32
            };
            strip.in_bufs[c].set(buf);
        }
    }
    for bus in &params.buses {
        for (c, &port) in bus.ports.iter().enumerate() {
            let buf = if port.is_null() {
                std::ptr::null_mut()
            } else {
                pw_sys::pw_filter_get_dsp_buffer(port, n as u32) as *mut f32
            };
            bus.out_bufs[c].set(buf);
        }
    }

    let input = |si: usize, c: usize| -> *const f32 { params.strips[si].in_bufs[c].get() };
    let output = |bi: usize, c: usize| -> *mut f32 { params.buses[bi].out_bufs[c].get() };
    let ramp = (rate * RAMP_MS / 1000).max(16) as usize;
    (*shared.processor.get()).process(params, n, &input, &output, ramp, rate);
}

/// What went wrong making the engine node or its ports.
#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    /// PipeWire would not make the node.
    #[error("pw_filter_new_simple failed")]
    Create,
    /// PipeWire would not connect it, with this error code.
    #[error("pw_filter_connect failed: {0}")]
    Connect(i32),
    /// PipeWire would not add a port.
    #[error("pw_filter_add_port failed")]
    AddPort,
}

/// The engine node. Must be created, used and dropped on the PipeWire
/// main-loop thread.
pub struct Filter {
    raw: *mut pw_sys::pw_filter,
    shared: Arc<FilterShared>,
}

impl Filter {
    /// Make the engine node, named `node_name`, on `loop_`, and connect it.
    /// Its callbacks work on `shared`.
    pub fn new(
        loop_: &pipewire::loop_::Loop,
        shared: Arc<FilterShared>,
        node_name: &str,
    ) -> Result<Self, FilterError> {
        let mut props = PropertiesBox::new();
        props.insert("node.name", node_name);
        props.insert("node.description", "Weir Engine");
        props.insert("node.nick", "Weir");
        props.insert("media.type", "Audio");
        props.insert("media.category", "Filter");
        props.insert("media.role", "DSP");
        props.insert("node.autoconnect", "false");
        props.insert("node.dont-reconnect", "true");
        props.insert("node.always-process", "true");
        let name = CString::new(node_name).expect("node name without NUL");
        let raw = unsafe {
            pw_sys::pw_filter_new_simple(
                loop_.as_raw_ptr(),
                name.as_ptr(),
                props.into_raw(),
                &EVENTS,
                Arc::as_ptr(&shared) as *mut c_void,
            )
        };
        if raw.is_null() {
            return Err(FilterError::Create);
        }
        let res = unsafe {
            pw_sys::pw_filter_connect(
                raw,
                pw_sys::pw_filter_flags_PW_FILTER_FLAG_RT_PROCESS,
                std::ptr::null_mut(),
                0,
            )
        };
        if res < 0 {
            unsafe { pw_sys::pw_filter_destroy(raw) };
            return Err(FilterError::Connect(res));
        }
        Ok(Self { raw, shared })
    }

    /// What the node shares with its callbacks and the daemon.
    pub fn shared(&self) -> &Arc<FilterShared> {
        &self.shared
    }

    /// Hold the PipeWire graph at `rate` Hz and `quantum` frames for as long
    /// as this node runs, or, for `None`, take the hold off and let PipeWire
    /// decide. Being properties of our own node, the hold goes away with it:
    /// nothing is left behind in PipeWire's settings when Weir stops.
    pub fn set_timing(&self, rate: Option<u32>, quantum: Option<u32>) {
        // 0 is PipeWire's "not forced". Removing the properties instead does
        // not work: a removal does not travel from here to the server.
        let entries = [
            ("node.force-rate", rate.unwrap_or(0)),
            ("node.force-quantum", quantum.unwrap_or(0)),
        ];
        let owned: Vec<(CString, CString)> = entries
            .into_iter()
            .map(|(k, v)| {
                (
                    CString::new(k).expect("no NUL"),
                    CString::new(v.to_string()).expect("no NUL"),
                )
            })
            .collect();
        let items: Vec<spa_sys::spa_dict_item> = owned
            .iter()
            .map(|(k, v)| spa_sys::spa_dict_item {
                key: k.as_ptr(),
                value: v.as_ptr(),
            })
            .collect();
        let dict = spa_sys::spa_dict {
            flags: 0,
            n_items: items.len() as u32,
            items: items.as_ptr(),
        };
        let res =
            unsafe { pw_sys::pw_filter_update_properties(self.raw, std::ptr::null_mut(), &dict) };
        if res < 0 {
            tracing::warn!("could not set the sample rate and buffer size: error {res}");
        }
    }

    /// Add a mono float DSP port. Returns the opaque port handle used by the
    /// process callback.
    pub fn add_port(
        &self,
        direction: Direction,
        name: &str,
        channel: ChannelPosition,
    ) -> Result<PortPtr, FilterError> {
        let mut props = PropertiesBox::new();
        props.insert("format.dsp", "32 bit float mono audio");
        props.insert("port.name", name);
        props.insert("audio.channel", channel.as_str());
        let dir = match direction {
            Direction::Input => spa_sys::SPA_DIRECTION_INPUT,
            Direction::Output => spa_sys::SPA_DIRECTION_OUTPUT,
        };
        let port = unsafe {
            pw_sys::pw_filter_add_port(
                self.raw,
                dir,
                pw_sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
                std::mem::size_of::<u64>(),
                props.into_raw(),
                std::ptr::null_mut(),
                0,
            )
        };
        if port.is_null() {
            Err(FilterError::AddPort)
        } else {
            Ok(port)
        }
    }

    /// Remove a port.
    ///
    /// # Safety
    /// `port` must have come from [`Filter::add_port`] on this filter and
    /// must not have been removed already. The caller must have published a
    /// snapshot that no longer references it first.
    pub unsafe fn remove_port(&self, port: PortPtr) {
        if !port.is_null() {
            pw_sys::pw_filter_remove_port(port);
        }
    }

    /// Global id of the node, or `None` until connected.
    pub fn node_id(&self) -> Option<u32> {
        let id = unsafe { pw_sys::pw_filter_get_node_id(self.raw) };
        self.shared.node_id.store(id, Ordering::Relaxed);
        if id == u32::MAX {
            None
        } else {
            Some(id)
        }
    }
}

impl Drop for Filter {
    fn drop(&mut self) {
        unsafe {
            pw_sys::pw_filter_disconnect(self.raw);
            pw_sys::pw_filter_destroy(self.raw);
        }
    }
}
