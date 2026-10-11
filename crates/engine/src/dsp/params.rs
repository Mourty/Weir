//! The immutable real-time parameter snapshot and its builder.
//!
//! The control thread turns the user-facing [`MixerState`] into an
//! [`RtParams`]: every level as a linear gain, every route as a matrix of
//! coefficients from the strip's channels to the bus's, every effect's
//! settings in the form the real-time thread wants. Everything that thread
//! will need is allocated here, so that it never has to.

use std::cell::UnsafeCell;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use weir_protocol::{
    db_to_linear, linear_to_db, Bus, BusId, ChannelPosition, Compressor, CompressorMeter, Denoise,
    Gate, GateMeter, Insert, InsertFallback, InsertPoint, Limiter, MixerState, Side, SoloMode,
    Strip, StripId, StripOrBus,
};

use super::fx::{DenoiseBank, EqParams, FxState, RtDuck};
use super::handoff::Handoff;
use super::mapping::{channel_matrix, surround_feed_row, wants_surround_feed};
use super::sounds::{SoundBank, SoundQueue};

/// Peak levels in dBFS per channel, for strips and buses, keyed by id.
pub type PeakSet = (Vec<(StripId, Vec<f32>)>, Vec<(BusId, Vec<f32>)>);

/// Largest PipeWire quantum we process in one go. Larger cycles are truncated
/// (PipeWire's default maximum is 8192).
pub const MAX_QUANTUM: usize = 16384;

/// Opaque handle to a `pw_filter` port (`port_data`). Null means "no port",
/// which the processor treats as silence (input) or as a sink for nothing
/// (output).
pub type PortPtr = *mut c_void;

/// A cell that only the real-time thread touches after the snapshot has been
/// published. The control thread writes initial values *before* publishing.
///
/// The `Sync` impl is sound only under that protocol, which is upheld by
/// [`build_rt_params`] and [`super::Processor`].
pub struct RtCell<T>(UnsafeCell<T>);

unsafe impl<T: Send> Sync for RtCell<T> {}
unsafe impl<T: Send> Send for RtCell<T> {}

impl<T> RtCell<T> {
    /// A cell holding `v`.
    pub fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }

    /// # Safety
    /// Only the real-time thread may call this after publication, and never
    /// while another reference from `get_mut` is alive.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get_mut(&self) -> &mut T {
        &mut *self.0.get()
    }
}

impl<T: Copy> RtCell<T> {
    /// # Safety
    /// See [`RtCell::get_mut`].
    pub unsafe fn get(&self) -> T {
        *self.0.get()
    }

    /// # Safety
    /// See [`RtCell::get_mut`].
    pub unsafe fn set(&self, v: T) {
        *self.0.get() = v;
    }
}

/// One strip's send into one bus.
pub struct RtSend {
    /// Target coefficient matrix, `n_in x n_out` row-major. All zero when the
    /// route is off. A strip that feeds subwoofers has one more row, for its
    /// low-passed bass (see [`RtStrip::sub_feed`]), which only has
    /// coefficients for the subwoofer channels of buses that have one. A
    /// strip decoding passive surround has one more after that, for its
    /// surround feed (see [`RtStrip::surround_feed`]).
    pub target: Vec<f32>,
    /// Ramped current coefficients, same shape as `target`.
    pub current: RtCell<Vec<f32>>,
}

/// A strip's or bus's external effects, as the real-time thread sees them:
/// where its sound goes out to another program and comes back.
pub struct RtInsert {
    /// Whether the sound goes out at all. Off, nothing is sent, and once
    /// [`RtInsert::dry`] and [`RtInsert::wet`] have settled at 1 and 0 the
    /// insert costs nothing.
    pub enabled: bool,
    /// Where in the chain.
    pub position: InsertPoint,
    /// How much of the sound as it went out carries on: 1 while nothing
    /// comes back and the fallback passes it through, or while off.
    pub dry: f32,
    /// How much of what comes back carries on: 1 while something plays
    /// into "from effects".
    pub wet: f32,
    /// Per channel, the output port to "to effects"; may be null.
    pub send_ports: Vec<PortPtr>,
    /// What comes back, from the return node, while they are on.
    pub handoff: Option<Arc<Handoff>>,
    /// Per-cycle cache of the send ports' buffers (null = none).
    pub send_bufs: Vec<RtCell<*mut f32>>,
    /// Per channel, what came back this cycle (null = silence).
    pub return_bufs: Vec<RtCell<*const f32>>,
}

impl RtInsert {
    /// The insert of `target`, whose settings are `insert`, with `n`
    /// channels.
    fn new(target: StripOrBus, insert: &Insert, n: usize, ports: &dyn PortResolver) -> Self {
        let (dry, wet) = match (insert.enabled, ports.insert_connected(target)) {
            (false, _) => (1.0, 0.0),
            (true, true) => (0.0, 1.0),
            (true, false) => match insert.fallback {
                InsertFallback::PassThrough => (1.0, 0.0),
                InsertFallback::Silence => (0.0, 0.0),
            },
        };
        let send_port = |c: usize| {
            if insert.enabled {
                ports.insert_send_port(target, c)
            } else {
                std::ptr::null_mut()
            }
        };
        Self {
            enabled: insert.enabled,
            // The mixer state is normalized, so this only guards the chain
            // against a place it does not have.
            position: match target {
                StripOrBus::Strip(_) => insert.position.for_strip(),
                StripOrBus::Bus(_) => insert.position.for_bus(),
            },
            dry,
            wet,
            send_ports: (0..n).map(send_port).collect(),
            handoff: ports
                .insert_handoff(target)
                .filter(|h| insert.enabled && h.channels() == n),
            send_bufs: (0..n).map(|_| RtCell::new(std::ptr::null_mut())).collect(),
            return_bufs: (0..n).map(|_| RtCell::new(std::ptr::null())).collect(),
        }
    }
}

/// A strip, as the real-time thread sees it.
pub struct RtStrip {
    /// The strip's id.
    pub id: StripId,
    /// Its channels, in port order.
    pub positions: Vec<ChannelPosition>,
    /// One port per channel, may be null.
    pub ports: Vec<PortPtr>,
    /// Per-cycle cache of the resolved input buffer for each channel (set by
    /// the PipeWire process callback before mixing; null = silence).
    pub in_bufs: Vec<RtCell<*const f32>>,
    /// Fader gain x mute x solo, linear.
    pub level_target: f32,
    /// The level now, ramping towards `level_target`.
    pub level: RtCell<f32>,
    /// One entry per bus, in `RtParams::buses` order.
    pub sends: Vec<RtSend>,
    /// Whether this strip makes a low-passed mix of its channels for the
    /// subwoofer of buses that have one. Its sends then have an extra row.
    pub sub_feed: bool,
    /// The channels of its front left and right, when this strip decodes
    /// passive surround: their difference, low-passed and delayed, is its
    /// surround feed, in the last row of its sends.
    pub surround_feed: Option<(usize, usize)>,
    /// Its ducking.
    pub ducking: RtDuck,
    /// Per bus, in `RtParams::buses` order: whether ducking turns this strip
    /// down in that bus's mix.
    pub duck_mask: Vec<bool>,
    /// Post-fader peak per channel, stored as `f32` bits.
    pub peaks: Vec<AtomicU32>,
    /// Effect state, shared with the snapshots before and after this one.
    pub fx: Arc<FxState>,
    /// Its equalizer.
    pub eq: EqParams,
    /// Its gate.
    pub gate: Gate,
    /// Its noise suppression settings.
    pub denoise: Denoise,
    /// Its compressor.
    pub compressor: Compressor,
    /// Present once noise suppression has been switched on for this strip.
    pub denoise_bank: Option<Arc<DenoiseBank>>,
    /// Its external effects.
    pub insert: RtInsert,
}

impl RtStrip {
    /// The row of its sends that carries its subwoofer feed, if any.
    pub fn sub_row(&self) -> Option<usize> {
        self.sub_feed.then_some(self.positions.len())
    }

    /// The row of its sends that carries its surround feed, if any.
    pub fn surround_row(&self) -> Option<usize> {
        self.surround_feed
            .map(|_| self.positions.len() + usize::from(self.sub_feed))
    }
}

/// A bus, as the real-time thread sees it.
pub struct RtBus {
    /// The bus's id.
    pub id: BusId,
    /// Its channels, in port order.
    pub positions: Vec<ChannelPosition>,
    /// One port per channel, may be null.
    pub ports: Vec<PortPtr>,
    /// Per-cycle cache of the resolved output buffer for each channel.
    pub out_bufs: Vec<RtCell<*mut f32>>,
    /// Fader gain x mute, linear.
    pub level_target: f32,
    /// The level now, ramping towards `level_target`.
    pub level: RtCell<f32>,
    /// 1.0 when folded to mono, 0.0 otherwise (ramped).
    pub mono_target: f32,
    /// The mono fold now, ramping towards `mono_target`.
    pub mono: RtCell<f32>,
    /// Channels that take part in the mono fold (everything but LFE).
    pub fold_mask: Vec<bool>,
    /// Output peak per channel, stored as `f32` bits.
    pub peaks: Vec<AtomicU32>,
    /// Equalizer and limiter state, shared with the snapshots before and
    /// after this one.
    pub fx: Arc<FxState>,
    /// Its equalizer.
    pub eq: EqParams,
    /// Its limiter.
    pub limiter: Limiter,
    /// How long it holds its output back, in milliseconds.
    pub delay_ms: f32,
    /// Its external effects.
    pub insert: RtInsert,
}

/// The complete immutable snapshot handed to the real-time thread.
pub struct RtParams {
    /// Every strip, in mixer order.
    pub strips: Vec<RtStrip>,
    /// Every bus, in mixer order.
    pub buses: Vec<RtBus>,
    /// The sounds hotkeys play, carried from snapshot to snapshot until a
    /// new bank replaces them.
    pub sounds: Arc<SoundBank>,
    /// Sounds asked to play, waiting for the next cycle. The same queue in
    /// every snapshot.
    pub sound_queue: Arc<SoundQueue>,
    /// The `hotkey_sounds` output's left and right ports, may be null.
    pub sound_ports: [PortPtr; 2],
    /// Per-cycle cache of their buffers.
    pub sound_bufs: [RtCell<*mut f32>; 2],
}

// Raw port pointers are only dereferenced by PipeWire on its own thread; the
// snapshot itself is immutable apart from the `RtCell`s documented above.
unsafe impl Send for RtParams {}
unsafe impl Sync for RtParams {}

impl RtParams {
    /// Read and reset all peak meters. Returns dBFS per channel.
    pub fn take_peaks(&self) -> PeakSet {
        let strips = self.strips.iter().map(|s| (s.id, take(&s.peaks))).collect();
        let buses = self.buses.iter().map(|b| (b.id, take(&b.peaks))).collect();
        (strips, buses)
    }

    /// How far each limiting bus's limiter turned things down since the
    /// last read, in dB.
    pub fn take_limiter_meters(&self) -> Vec<(BusId, f32)> {
        self.buses
            .iter()
            .filter(|b| b.limiter.enabled)
            .map(|b| (b.id, b.fx.take_limiter_db()))
            .collect()
    }

    /// Read the compressor meters of every strip with its compressor on,
    /// resetting them.
    pub fn take_compressor_meters(&self) -> Vec<(StripId, CompressorMeter)> {
        self.strips
            .iter()
            .filter(|s| s.compressor.enabled)
            .map(|s| (s.id, s.fx.take_compressor_meter()))
            .collect()
    }

    /// How far each strip with ducking on was turned down since the last
    /// read, in dB.
    pub fn take_duck_meters(&self) -> Vec<(StripId, f32)> {
        self.strips
            .iter()
            .filter(|s| s.ducking.enabled)
            .map(|s| (s.id, s.fx.take_duck_db()))
            .collect()
    }

    /// Read the gate meters of every strip with its gate on, resetting their
    /// peak levels.
    pub fn take_gate_meters(&self) -> Vec<(StripId, GateMeter)> {
        self.strips
            .iter()
            .filter(|s| s.gate.enabled)
            .map(|s| (s.id, s.fx.take_gate_meter()))
            .collect()
    }
}

fn take(peaks: &[AtomicU32]) -> Vec<f32> {
    peaks
        .iter()
        .map(|p| linear_to_db(f32::from_bits(p.swap(0, Ordering::Relaxed))))
        .collect()
}

/// Resolves the `pw_filter` port for a given strip/bus channel. The builder
/// asks for every channel of every strip and bus, and of every external
/// effects insert that is on.
pub trait PortResolver {
    /// The input port of `strip`'s `channel`, or null.
    fn strip_port(&self, strip: StripId, channel: usize) -> PortPtr;
    /// The output port of `bus`'s `channel`, or null.
    fn bus_port(&self, bus: BusId, channel: usize) -> PortPtr;
    /// The output port of `target`'s external effects for `channel`, to
    /// "to effects", or null.
    fn insert_send_port(&self, _target: StripOrBus, _channel: usize) -> PortPtr {
        std::ptr::null_mut()
    }
    /// Where what comes back from `target`'s external effects arrives.
    fn insert_handoff(&self, _target: StripOrBus) -> Option<Arc<Handoff>> {
        None
    }
    /// Whether anything plays into `target`'s "from effects" device.
    fn insert_connected(&self, _target: StripOrBus) -> bool {
        false
    }
    /// The `hotkey_sounds` output's port for `channel` (0 left, 1 right),
    /// or null.
    fn sound_port(&self, _channel: usize) -> PortPtr {
        std::ptr::null_mut()
    }
}

/// A resolver that yields no ports at all (used by tests and before the
/// engine is connected).
pub struct NoPorts;

impl PortResolver for NoPorts {
    fn strip_port(&self, _: StripId, _: usize) -> PortPtr {
        std::ptr::null_mut()
    }
    fn bus_port(&self, _: BusId, _: usize) -> PortPtr {
        std::ptr::null_mut()
    }
}

/// Build a snapshot from the user-facing state, with soloing working as
/// `solo` says. All ramp state starts at zero (a fade-in);
/// [`super::Processor`] migrates continuity from the previously running
/// snapshot when it first sees the new one.
///
/// Effect state is not migrated but shared: every strip and bus that also
/// exists in `prev`, with the same number of channels, takes over `prev`'s
/// state objects. Anything new is allocated here, never on the real-time
/// thread.
pub fn build_rt_params(
    state: &MixerState,
    solo: SoloMode,
    ports: &dyn PortResolver,
    prev: Option<&RtParams>,
) -> Arc<RtParams> {
    // Cueing on a bus that no longer exists would make solo do nothing at
    // all; silencing the other strips everywhere is the safer surprise.
    let cue = match solo {
        SoloMode::Cue(bus) if state.bus(bus).is_some() => Some(bus),
        _ => None,
    };
    let buses = state
        .buses
        .iter()
        .map(|b| {
            let n = b.layout.channel_count();
            let before = prev.and_then(|p| {
                p.buses
                    .iter()
                    .find(|x| x.id == b.id && x.positions.len() == n)
            });
            build_bus(b, ports, before)
        })
        .collect();
    let strips = state
        .strips
        .iter()
        .map(|s| {
            let n = s.layout.channel_count();
            let before = prev.and_then(|p| {
                p.strips
                    .iter()
                    .find(|x| x.id == s.id && x.positions.len() == n)
            });
            build_strip(s, state, cue, ports, before)
        })
        .collect();
    let sounds = prev.map(|p| p.sounds.clone()).unwrap_or_default();
    let sound_queue = prev.map(|p| p.sound_queue.clone()).unwrap_or_default();
    Arc::new(RtParams {
        strips,
        buses,
        sounds,
        sound_queue,
        sound_ports: [ports.sound_port(0), ports.sound_port(1)],
        sound_bufs: std::array::from_fn(|_| RtCell::new(std::ptr::null_mut())),
    })
}

/// One bus of a snapshot. `before` is the same bus in the previous one,
/// when it has the same channels, whose effect state it takes over.
fn build_bus(b: &Bus, ports: &dyn PortResolver, before: Option<&RtBus>) -> RtBus {
    let positions = b.layout.positions();
    let n = positions.len();
    RtBus {
        id: b.id,
        ports: (0..n).map(|c| ports.bus_port(b.id, c)).collect(),
        out_bufs: (0..n).map(|_| RtCell::new(std::ptr::null_mut())).collect(),
        level_target: if b.mute { 0.0 } else { db_to_linear(b.gain_db) },
        level: RtCell::new(0.0),
        mono_target: if b.mono { 1.0 } else { 0.0 },
        mono: RtCell::new(0.0),
        fold_mask: positions.iter().map(|p| p.side() != Side::Lfe).collect(),
        peaks: (0..n).map(|_| AtomicU32::new(0)).collect(),
        fx: before
            .map(|p| p.fx.clone())
            .unwrap_or_else(|| Arc::new(FxState::for_bus(n))),
        eq: EqParams::from_eq(&b.eq),
        limiter: b.limiter,
        delay_ms: b.delay_ms,
        insert: RtInsert::new(StripOrBus::Bus(b.id), &b.insert, n, ports),
        positions,
    }
}

/// One strip of a snapshot of `state`. `cue` is the bus soloing cues on, if
/// it does; `before` is the same strip in the previous snapshot, when it
/// has the same channels, whose effect state it takes over.
fn build_strip(
    s: &Strip,
    state: &MixerState,
    cue: Option<BusId>,
    ports: &dyn PortResolver,
    before: Option<&RtStrip>,
) -> RtStrip {
    let positions = s.layout.positions();
    let n = positions.len();
    // Not soloed while something else is: silent everywhere, or, when
    // cueing, only in the cue bus's mix.
    let unsoloed = state.any_solo() && !s.solo;
    let silent = s.mute || (unsoloed && cue.is_none());
    // A strip with a subwoofer channel of its own sends that there already.
    let sub_feed = s.subwoofer && n > 0 && !positions.contains(&ChannelPosition::LFE);
    let surround_feed = wants_surround_feed(&positions, s.upmix).then(|| {
        let at = |p| positions.iter().position(|&q| q == p).unwrap_or(0);
        (at(ChannelPosition::FL), at(ChannelPosition::FR))
    });
    let rows = n + usize::from(sub_feed) + usize::from(surround_feed.is_some());
    let sends = state
        .buses
        .iter()
        .map(|b| {
            let out_pos = b.layout.positions();
            let cued_out = unsoloed && cue == Some(b.id);
            let target =
                if s.routes.contains(&b.id) && !cued_out {
                    let send = db_to_linear(s.send_db(b.id));
                    let mut m = channel_matrix(&positions, &out_pos, s.pan, s.upmix, &b.downmix);
                    let surround =
                        surround_feed.map(|_| surround_feed_row(&out_pos, s.pan, &b.downmix, &m));
                    if sub_feed {
                        // The subwoofer feed's row: into the subwoofer only.
                        m.extend(out_pos.iter().map(|&p| {
                            if p == ChannelPosition::LFE {
                                1.0
                            } else {
                                0.0
                            }
                        }));
                    }
                    m.extend(surround.into_iter().flatten());
                    m.iter_mut().for_each(|c| *c *= send);
                    m
                } else {
                    vec![0.0; rows * out_pos.len()]
                };
            RtSend {
                current: RtCell::new(vec![0.0; target.len()]),
                target,
            }
        })
        .collect();
    let denoise_bank = before
        .and_then(|p| p.denoise_bank.clone())
        .or_else(|| s.denoise.enabled.then(|| Arc::new(DenoiseBank::new(n))));
    RtStrip {
        id: s.id,
        ports: (0..n).map(|c| ports.strip_port(s.id, c)).collect(),
        in_bufs: (0..n).map(|_| RtCell::new(std::ptr::null())).collect(),
        level_target: if silent { 0.0 } else { db_to_linear(s.gain_db) },
        level: RtCell::new(0.0),
        sends,
        sub_feed,
        surround_feed,
        ducking: RtDuck {
            enabled: s.ducking.enabled,
            triggers: s
                .ducking
                .triggers
                .iter()
                .filter_map(|t| state.strips.iter().position(|o| o.id == *t))
                .collect(),
            threshold: db_to_linear(s.ducking.threshold_db),
            gain: db_to_linear(-s.ducking.amount_db),
            attack_ms: s.ducking.attack_ms,
            hold_ms: s.ducking.hold_ms,
            release_ms: s.ducking.release_ms,
        },
        duck_mask: state
            .buses
            .iter()
            .map(|b| s.ducking.applies_to(b.id))
            .collect(),
        peaks: (0..n).map(|_| AtomicU32::new(0)).collect(),
        fx: before
            .map(|p| p.fx.clone())
            .unwrap_or_else(|| Arc::new(FxState::for_strip(n))),
        eq: EqParams::from_eq(&s.eq),
        gate: s.gate,
        denoise: s.denoise,
        compressor: s.compressor,
        denoise_bank,
        insert: RtInsert::new(StripOrBus::Strip(s.id), &s.insert, n, ports),
        positions,
    }
}
