//! Wrapper over `pw_filter`: the engine node, and the node external effects
//! come back into.
//!
//! One `pw_filter` does all of Weir's mixing. It has an input port for every
//! strip channel and an output port for every bus channel, and one more
//! output for every channel of a strip or bus with external effects on.
//! PipeWire calls [`on_process`] on its real-time thread once per cycle
//! with a buffer for each.
//!
//! What comes back from external effects arrives in a second, much smaller
//! node, "Weir effects return", whose [`on_return_process`] passes it on to
//! the engine through each insert's [`Handoff`]. Why it is a node of its
//! own is in [`crate::dsp::handoff`].

use crate::dsp::handoff::Handoff;
use crate::dsp::params::{NoPorts, RtInsert, MAX_QUANTUM};
use crate::dsp::{build_rt_params, PortPtr, Processor, RtParams};
use arc_swap::ArcSwap;
use libspa_sys as spa_sys;
use pipewire::properties::PropertiesBox;
use pipewire::sys as pw_sys;
use std::cell::UnsafeCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use weir_protocol::{ChannelPosition, EngineStatus, MixerState, SoloMode};

/// How long every change of level, route or pan takes to ramp, in ms: long
/// enough never to click, short enough to feel immediate.
const RAMP_MS: u32 = 10;

/// Which way a port carries audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Into the node: a strip channel, or one coming back from external
    /// effects.
    Input,
    /// Out of the node: a bus channel, or one going to external effects.
    Output,
}

/// The group both of Weir's nodes are in, so that one driver always runs
/// them, and the return node keeps time with the engine whatever it is
/// linked to.
const NODE_GROUP: &str = "weir";

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
    /// The PipeWire server's version, once it has said.
    pub server_version: Mutex<Option<String>>,
    /// The mixer panicked: from then on every output is silence, since
    /// what it was in the middle of may be left half done. A restart
    /// starts it afresh.
    pub broken: AtomicBool,
    /// The connection to PipeWire was lost, and the engine waits for it to
    /// come back. The filter's own state says nothing then: there is none.
    pub waiting: AtomicBool,
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
            server_version: Mutex::new(None),
            broken: AtomicBool::new(false),
            waiting: AtomicBool::new(false),
        })
    }

    /// The connection to PipeWire is gone, and the filter with it. Its
    /// ports went too, so the snapshot naming them must go before a new
    /// filter can start on it: [`Self::rest`] replaces it.
    pub fn lost(&self) {
        self.waiting.store(true, Ordering::Relaxed);
        self.node_id.store(u32::MAX, Ordering::Relaxed);
        self.rate.store(0, Ordering::Relaxed);
        self.quantum.store(0, Ordering::Relaxed);
        if let Ok(mut v) = self.server_version.lock() {
            *v = None;
        }
        if let Ok(mut e) = self.error.lock() {
            *e = None;
        }
    }

    /// While there is no filter: a snapshot of `state` without ports, so
    /// that meters read silence for every strip and bus, not nothing.
    pub fn rest(&self, state: &MixerState, solo: SoloMode) {
        self.params
            .store(build_rt_params(state, solo, &NoPorts, None));
    }

    /// A new filter is made: its own state counts again, from connecting.
    pub fn connected_again(&self) {
        self.state.store(
            pw_sys::pw_filter_state_PW_FILTER_STATE_CONNECTING,
            Ordering::Relaxed,
        );
        self.waiting.store(false, Ordering::Relaxed);
    }

    /// The filter's state as the protocol names it.
    pub fn state_name(&self) -> &'static str {
        if self.waiting.load(Ordering::Relaxed) {
            return "waiting";
        }
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
    weir_protocol::crash::name_part("audio");
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

    let buffer = |port: PortPtr| -> *mut f32 {
        if port.is_null() {
            std::ptr::null_mut()
        } else {
            pw_sys::pw_filter_get_dsp_buffer(port, n as u32) as *mut f32
        }
    };
    let resolve_insert = |ins: &RtInsert| {
        for (c, &port) in ins.send_ports.iter().enumerate() {
            ins.send_bufs[c].set(buffer(port));
        }
        // What came back in the return node's last cycle.
        let back = ins.handoff.as_ref().map(|h| h.pull(n));
        for (c, cell) in ins.return_bufs.iter().enumerate() {
            let buf = back
                .and_then(|b| b.get(c))
                .map_or(std::ptr::null(), |b| b.as_ptr());
            cell.set(buf);
        }
    };
    for strip in &params.strips {
        for (c, &port) in strip.ports.iter().enumerate() {
            strip.in_bufs[c].set(buffer(port));
        }
        resolve_insert(&strip.insert);
    }
    for bus in &params.buses {
        for (c, &port) in bus.ports.iter().enumerate() {
            bus.out_bufs[c].set(buffer(port));
        }
        resolve_insert(&bus.insert);
    }
    for (cell, &port) in params.sound_bufs.iter().zip(&params.sound_ports) {
        cell.set(buffer(port));
    }

    let input = |si: usize, c: usize| -> *const f32 { params.strips[si].in_bufs[c].get() };
    let output = |bi: usize, c: usize| -> *mut f32 { params.buses[bi].out_bufs[c].get() };
    let ramp = (rate * RAMP_MS / 1000).max(16) as usize;
    if shared.broken.load(Ordering::Relaxed) {
        silence(params, n);
        return;
    }
    // A panic cannot unwind out of a C callback: it would end the whole
    // daemon. Caught, it is kept as a crash report (the hook runs here),
    // and the mixer falls silent rather than playing whatever it left.
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if CRASH_TEST.load(Ordering::Relaxed) {
            panic!("a crash on purpose (WEIR_CRASH_TEST=audio)");
        }
        (*shared.processor.get()).process(params, n, &input, &output, ramp, rate);
    }));
    if ran.is_err() {
        shared.broken.store(true, Ordering::Relaxed);
        silence(params, n);
    }
}

/// Set to make the next cycle panic, to try crash reports.
static CRASH_TEST: AtomicBool = AtomicBool::new(false);

/// Make the mixer panic in its next cycle, to try what happens then
/// (`WEIR_CRASH_TEST=audio`).
pub fn crash_on_purpose() {
    CRASH_TEST.store(true, Ordering::Relaxed);
}

/// Fill every output this cycle with silence: the buses, external effects'
/// sends and hotkey sounds. Their buffers were resolved this cycle.
unsafe fn silence(params: &RtParams, n: usize) {
    let zero = |p: *mut f32| {
        if !p.is_null() {
            std::ptr::write_bytes(p, 0, n);
        }
    };
    for bus in &params.buses {
        bus.out_bufs.iter().for_each(|b| zero(b.get()));
        bus.insert.send_bufs.iter().for_each(|b| zero(b.get()));
    }
    for strip in &params.strips {
        strip.insert.send_bufs.iter().for_each(|b| zero(b.get()));
    }
    params.sound_bufs.iter().for_each(|b| zero(b.get()));
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

/// What the return node shares with its process callback.
pub struct ReturnShared {
    /// The published parameter snapshot.
    pub params: ArcSwap<ReturnParams>,
}

impl ReturnShared {
    /// Shared state with nothing to pass on yet.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            params: ArcSwap::from_pointee(ReturnParams::default()),
        })
    }
}

/// What the return node passes on: one entry per strip or bus with external
/// effects on.
#[derive(Default)]
pub struct ReturnParams {
    pub returns: Vec<RtReturn>,
}

/// One strip's or bus's way back from its external effects.
pub struct RtReturn {
    /// One input port per channel, may be null.
    pub ports: Vec<PortPtr>,
    /// Where they go, for the engine.
    pub handoff: Arc<Handoff>,
}

// As for `RtParams`: the port pointers are only used by PipeWire on its
// own thread.
unsafe impl Send for ReturnParams {}
unsafe impl Sync for ReturnParams {}

static RETURN_EVENTS: pw_sys::pw_filter_events = pw_sys::pw_filter_events {
    version: pw_sys::PW_VERSION_FILTER_EVENTS,
    destroy: None,
    state_changed: None,
    io_changed: None,
    param_changed: None,
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_return_process),
    drained: None,
    command: None,
};

/// The return node's real-time process callback: what came back from each
/// strip's and bus's external effects goes into its hand-off, for the
/// engine's next cycle.
unsafe extern "C" fn on_return_process(data: *mut c_void, position: *mut spa_sys::spa_io_position) {
    let shared = &*(data as *const ReturnShared);
    if position.is_null() {
        return;
    }
    weir_protocol::crash::name_part("audio");
    let n = ((*position).clock.duration as usize).min(MAX_QUANTUM);
    let guard = shared.params.load();
    for r in &guard.returns {
        // PipeWire hands out a port's buffer once per cycle, so each is
        // asked for exactly once, here.
        let mut bufs = [std::ptr::null::<f32>(); 16];
        for (slot, &port) in bufs.iter_mut().zip(&r.ports) {
            if !port.is_null() {
                *slot = pw_sys::pw_filter_get_dsp_buffer(port, n as u32) as *const f32;
            }
        }
        r.handoff
            .push(n, &|c| bufs.get(c).copied().unwrap_or(std::ptr::null()));
    }
}

/// One of Weir's nodes. Must be created, used and dropped on the PipeWire
/// main-loop thread. `S` is what it shares with its callbacks.
pub struct Filter<S: 'static = FilterShared> {
    raw: *mut pw_sys::pw_filter,
    shared: Arc<S>,
}

impl Filter<FilterShared> {
    /// Make the engine node, named `node_name`, on `loop_`, and connect it.
    /// Its callbacks work on `shared`.
    pub fn new(
        loop_: &pipewire::loop_::Loop,
        shared: Arc<FilterShared>,
        node_name: &str,
    ) -> Result<Self, FilterError> {
        Self::create(loop_, shared, node_name, "Weir Engine", "Weir", &EVENTS)
    }
}

impl Filter<ReturnShared> {
    /// Make the node external effects come back into, named `node_name`, on
    /// `loop_`, and connect it.
    pub fn new_return(
        loop_: &pipewire::loop_::Loop,
        shared: Arc<ReturnShared>,
        node_name: &str,
    ) -> Result<Self, FilterError> {
        let description = "Weir effects return";
        Self::create(
            loop_,
            shared,
            node_name,
            description,
            description,
            &RETURN_EVENTS,
        )
    }
}

impl<S: 'static> Filter<S> {
    /// Make a node, and connect it. `events` gets `shared` as its data.
    fn create(
        loop_: &pipewire::loop_::Loop,
        shared: Arc<S>,
        node_name: &str,
        description: &str,
        nick: &str,
        events: &'static pw_sys::pw_filter_events,
    ) -> Result<Self, FilterError> {
        let mut props = PropertiesBox::new();
        props.insert("node.name", node_name);
        props.insert("node.description", description);
        props.insert("node.nick", nick);
        props.insert("node.group", NODE_GROUP);
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
                events,
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

    /// What the node shares with its callbacks.
    pub fn shared(&self) -> &Arc<S> {
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
}

impl Filter<FilterShared> {
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

impl<S: 'static> Drop for Filter<S> {
    fn drop(&mut self) {
        unsafe {
            pw_sys::pw_filter_disconnect(self.raw);
            pw_sys::pw_filter_destroy(self.raw);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_for_pipewire_reads_as_silence_then_connects_again() {
        let shared = FilterShared::new();
        shared.rate.store(48_000, Ordering::Relaxed);
        shared.node_id.store(34, Ordering::Relaxed);
        let mixer = MixerState {
            strips: vec![weir_protocol::Strip::new(
                1,
                "Mic",
                weir_protocol::StripKind::Hardware,
                weir_protocol::ChannelLayout::Mono,
            )],
            buses: vec![weir_protocol::Bus::new(
                1,
                "Headset",
                weir_protocol::BusKind::Hardware,
                weir_protocol::ChannelLayout::Stereo,
            )],
        };

        shared.lost();
        shared.rest(&mixer, SoloMode::Exclusive);
        let status = shared.status();
        assert_eq!(status.state, "waiting");
        assert!(!status.connected);
        assert_eq!((status.sample_rate, status.node_id), (0, None));
        // Every strip and bus still reads, as silence, so that meters fall
        // rather than hold what they last showed.
        let (strips, buses) = shared.params.load().take_peaks();
        assert_eq!((strips.len(), buses.len()), (1, 1));
        assert_eq!((strips[0].1.len(), buses[0].1.len()), (1, 2));
        assert!(strips
            .iter()
            .chain(buses.iter())
            .all(|(_, v)| v.iter().all(|&db| db <= -100.0)));

        shared.connected_again();
        assert_eq!(shared.status().state, "connecting");
    }
}
