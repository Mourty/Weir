//! The PipeWire thread: owns the main loop, the engine filter, the virtual
//! device nodes and the links, and reconciles all of them with the mixer
//! state it is given.
//!
//! Everything here runs on that one thread, driven by PipeWire's callbacks
//! and by [`EngineCommand`]s from the daemon. After anything changes,
//! [`Runner::reconcile`] brings PipeWire in line with the wanted state, in
//! order: the engine node's ports, the real-time snapshot, the virtual
//! devices ([`devices`]), the links ([`links`]), and the reports back to
//! the daemon. Application streams have a module of their own ([`apps`]).

mod apps;
mod devices;
mod links;

use super::engine::{EngineCommand, EngineError, EngineEvent, EngineOptions};
use super::filter::{Direction, Filter, FilterShared};
use super::graph::Graph;
use super::volume::AppVolume;
use super::ENGINE_NODE_NAME;
use crate::dsp::{build_rt_params, PortPtr, PortResolver, RtParams};
use devices::{PendingMove, VirtualNode};
use pipewire::context::ContextRc;
use pipewire::core::CoreRc;
use pipewire::link::Link;
use pipewire::main_loop::MainLoopRc;
use pipewire::metadata::Metadata;
use pipewire::node::{Node, NodeListener};
use pipewire::properties::PropertiesBox;
use pipewire::registry::RegistryRc;
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use weir_protocol::{
    AppStream, BusId, ChannelPosition, DeviceInfo, EngineStatus, MixerState, StripId, SystemVolumes,
};

/// `errno` values PipeWire reports errors with.
const EPIPE: i32 = 32;
const ENOENT: i32 = 2;

/// How many replaced snapshots to keep alive. The real-time thread may still
/// be running the one before the latest, and it must never be the one to
/// free a snapshot, so old ones are dropped here, long after.
const OLD_SNAPSHOTS_KEPT: usize = 64;

/// One port of the engine node: a strip's or bus's channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PortKey {
    Strip(StripId, usize),
    Bus(BusId, usize),
}

/// A strip or a bus, as the owner of ports and virtual devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Owner {
    Strip(StripId),
    Bus(BusId),
}

impl Owner {
    /// Its `channel`'s port.
    fn port(self, channel: usize) -> PortKey {
        match self {
            Owner::Strip(id) => PortKey::Strip(id, channel),
            Owner::Bus(id) => PortKey::Bus(id, channel),
        }
    }
}

/// A port this program added to the engine node.
struct LocalPort {
    ptr: PortPtr,
    name: String,
    position: ChannelPosition,
}

/// Finds the engine node's ports for the snapshot builder.
struct PortMap<'a>(&'a HashMap<PortKey, LocalPort>);

impl PortResolver for PortMap<'_> {
    fn strip_port(&self, strip: StripId, channel: usize) -> PortPtr {
        self.0
            .get(&PortKey::Strip(strip, channel))
            .map_or(std::ptr::null_mut(), |p| p.ptr)
    }
    fn bus_port(&self, bus: BusId, channel: usize) -> PortPtr {
        self.0
            .get(&PortKey::Bus(bus, channel))
            .map_or(std::ptr::null_mut(), |p| p.ptr)
    }
}

/// Everything the PipeWire thread keeps.
struct Runner {
    /// The mix the daemon wants.
    state: MixerState,
    options: EngineOptions,
    /// What PipeWire has, as far as we know.
    graph: Graph,
    core: CoreRc,
    registry: RegistryRc,
    /// The engine node.
    filter: Filter,
    /// The engine node's ports.
    ports: HashMap<PortKey, LocalPort>,
    /// Ports scheduled for removal once the snapshot that dropped them has
    /// been published.
    stale_ports: Vec<LocalPort>,
    /// Weir's virtual devices.
    virtual_nodes: HashMap<Owner, VirtualNode>,
    /// Links this program made, by (output port, input port) global id.
    links: HashMap<(u32, u32), Link>,
    /// Bound proxies for application streams, kept alive so their parameter
    /// listeners keep firing.
    app_nodes: HashMap<u32, (Node, NodeListener)>,
    /// Each application's own volume, updated by those listeners. Shared with
    /// the callbacks, which run on this same thread.
    app_volumes: Rc<RefCell<BTreeMap<u32, AppVolume>>>,
    /// Lets callbacks hand work back to the command loop instead of
    /// re-entering the runner while it is already borrowed.
    self_tx: pipewire::channel::Sender<EngineCommand>,
    /// PipeWire's `default` metadata, where application targets are set.
    metadata: Option<Metadata>,
    on_event: Box<dyn Fn(EngineEvent) + Send>,
    /// Set when the state or the port set changed, cleared by `publish_params`.
    params_dirty: bool,
    /// Recently replaced snapshots, kept alive so the real-time thread never
    /// frees one.
    old_params: VecDeque<Arc<RtParams>>,
    /// What was last reported to the daemon, to report only changes.
    last_devices: Vec<DeviceInfo>,
    last_apps: Vec<AppStream>,
    last_status: Option<EngineStatus>,
    last_system_volumes: SystemVolumes,
    /// Applications to put back on strips whose devices are being remade.
    pending_moves: Vec<PendingMove>,
}

/// The PipeWire thread's body: connect, then run the main loop until told
/// to stop. Sends on `ready_tx` once connected, or why it could not.
pub(super) fn run(
    initial: MixerState,
    rx: pipewire::channel::Receiver<EngineCommand>,
    self_tx: pipewire::channel::Sender<EngineCommand>,
    shared: Arc<FilterShared>,
    on_event: Box<dyn Fn(EngineEvent) + Send>,
    ready_tx: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), EngineError> {
    pipewire::init();
    let fail = |msg: String| {
        let _ = ready_tx.send(Err(msg.clone()));
        EngineError::PipeWire(msg)
    };
    let mainloop = MainLoopRc::new(None).map_err(|e| fail(format!("main loop: {e}")))?;
    let mut props = PropertiesBox::new();
    props.insert("application.name", "Weir");
    props.insert("application.id", "org.weir.daemon");
    let context =
        ContextRc::new(&mainloop, Some(props)).map_err(|e| fail(format!("context: {e}")))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| fail(format!("connect (is PipeWire running?): {e}")))?;
    let registry = core
        .get_registry_rc()
        .map_err(|e| fail(format!("registry: {e}")))?;

    {
        let tx = self_tx.clone();
        if let Ok(mut cb) = shared.state_cb.lock() {
            *cb = Some(Box::new(move || {
                let _ = tx.send(EngineCommand::FilterStateChanged);
            }));
        }
    }
    let filter = Filter::new(mainloop.loop_(), shared.clone(), ENGINE_NODE_NAME)
        .map_err(|e| fail(format!("engine node: {e}")))?;

    let runner = Rc::new(RefCell::new(Runner {
        state: initial,
        options: EngineOptions::default(),
        graph: Graph::default(),
        core: core.clone(),
        registry: registry.clone(),
        filter,
        ports: HashMap::new(),
        stale_ports: Vec::new(),
        virtual_nodes: HashMap::new(),
        links: HashMap::new(),
        app_nodes: HashMap::new(),
        app_volumes: Rc::new(RefCell::new(BTreeMap::new())),
        self_tx: self_tx.clone(),
        metadata: None,
        on_event,
        params_dirty: true,
        old_params: VecDeque::new(),
        last_devices: Vec::new(),
        last_apps: Vec::new(),
        last_status: None,
        last_system_volumes: SystemVolumes::default(),
        pending_moves: Vec::new(),
    }));

    let _core_listener = {
        let ml = mainloop.clone();
        core.add_listener_local()
            .error(move |id, seq, res, message| {
                if res == -ENOENT && message.contains("unknown resource") {
                    // We asked to destroy an object the server had already
                    // removed (e.g. links of a vanished node). Harmless.
                    debug!("PipeWire: {message}");
                    return;
                }
                error!("PipeWire core error id={id} seq={seq} res={res}: {message}");
                if id == pipewire::core::PW_ID_CORE && res == -EPIPE {
                    ml.quit();
                }
            })
            .register()
    };

    let _registry_listener = {
        let r1 = runner.clone();
        let r2 = runner.clone();
        registry
            .add_listener_local()
            .global(move |obj| r1.borrow_mut().on_global(obj))
            .global_remove(move |id| r2.borrow_mut().on_global_remove(id))
            .register()
    };

    let _rx = {
        let r = runner.clone();
        let ml = mainloop.clone();
        rx.attach(mainloop.loop_(), move |cmd| match cmd {
            EngineCommand::Shutdown => ml.quit(),
            cmd => r.borrow_mut().on_command(cmd),
        })
    };

    runner.borrow_mut().reconcile();
    let _ = ready_tx.send(Ok(()));
    info!("engine connected to PipeWire");
    mainloop.run();

    // Tear down explicitly so the server does not wait for the disconnect.
    {
        let mut r = runner.borrow_mut();
        let links: Vec<_> = r.links.drain().map(|(_, l)| l).collect();
        for l in links {
            let _ = r.core.destroy_object(l);
        }
        let nodes: Vec<_> = r.virtual_nodes.drain().map(|(_, n)| n.proxy).collect();
        for n in nodes {
            let _ = r.core.destroy_object(n);
        }
    }
    info!("engine thread stopped");
    Ok(())
}

impl Runner {
    /// Something appeared in PipeWire.
    fn on_global<P: AsRef<pipewire::spa::utils::dict::DictRef>>(
        &mut self,
        obj: &pipewire::registry::GlobalObject<P>,
    ) {
        let tracked = self.graph.add_global(obj);
        if obj.type_ == ObjectType::Metadata && self.graph.default_metadata == Some(obj.id) {
            match self.registry.bind::<Metadata, _>(obj) {
                Ok(m) => self.metadata = Some(m),
                Err(e) => warn!("could not bind default metadata: {e}"),
            }
        }
        // Application streams, for their volume sliders, and our own
        // virtual devices, to show the volume the system set on them.
        if obj.type_ == ObjectType::Node
            && self.graph.nodes.get(&obj.id).is_some_and(|n| {
                n.media_class == "Stream/Output/Audio" || n.name.starts_with(super::VIRTUAL_PREFIX)
            })
        {
            self.watch_volume(obj);
        }
        if tracked {
            self.reconcile();
        }
    }

    /// Something went away in PipeWire.
    fn on_global_remove(&mut self, id: u32) {
        self.app_nodes.remove(&id);
        self.app_volumes.borrow_mut().remove(&id);
        if self.graph.remove_global(id) {
            let Runner { links, graph, .. } = &mut *self;
            links.retain(|(o, i), _| graph.ports.contains_key(o) && graph.ports.contains_key(i));
            self.reconcile();
        }
    }

    /// Carry out a command from the daemon.
    fn on_command(&mut self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::SetState(state) => {
                self.state = state;
                self.params_dirty = true;
                self.reconcile();
            }
            EngineCommand::SetOptions(options) => {
                let old = std::mem::replace(&mut self.options, options);
                if (old.sample_rate, old.quantum) != (options.sample_rate, options.quantum) {
                    let any = |v: Option<u32>| v.map_or("any".to_string(), |v| v.to_string());
                    info!(
                        "holding PipeWire at {} Hz, {} frames",
                        any(options.sample_rate),
                        any(options.quantum)
                    );
                    self.filter.set_timing(options.sample_rate, options.quantum);
                }
                if old.solo != options.solo {
                    self.params_dirty = true;
                    self.reconcile();
                }
            }
            EngineCommand::MoveApp { app, strip } => self.move_app(app, strip),
            EngineCommand::SetAppVolume {
                app,
                volume_db,
                mute,
            } => self.set_app_volume(app, volume_db, mute),
            EngineCommand::AppsDirty => self.emit_devices_apps(),
            EngineCommand::FilterStateChanged => self.emit_status(),
            // Handled by the command loop, which owns the main loop.
            EngineCommand::Shutdown => {}
        }
    }

    /// Bring ports, snapshot, virtual nodes and links in line with `state`
    /// and the current graph. Cheap and idempotent. A new snapshot is only
    /// published when the state or the port set changed, so registry chatter
    /// does not flood the real-time thread.
    fn reconcile(&mut self) {
        if self.params_dirty {
            self.ensure_ports();
            self.publish_params();
            self.remove_stale_ports();
        }
        self.ensure_virtual_nodes();
        self.finish_pending_moves();
        self.ensure_links();
        self.emit_devices_apps();
        self.emit_status();
    }

    /// The name of a port: `strip_2_FL`, with a number after the position
    /// when a layout repeats one.
    fn port_name(prefix: &str, id: u32, positions: &[ChannelPosition], index: usize) -> String {
        let pos = positions[index];
        let dup = positions[..index].iter().filter(|p| **p == pos).count();
        if dup == 0 {
            format!("{prefix}_{id}_{pos}")
        } else {
            format!("{prefix}_{id}_{pos}_{dup}")
        }
    }

    /// Give the engine node an input port per strip channel and an output
    /// port per bus channel. Ports that are no longer wanted, or changed,
    /// go to `stale_ports`.
    fn ensure_ports(&mut self) {
        let mut wanted: Vec<(PortKey, Direction, String, ChannelPosition)> = Vec::new();
        for s in &self.state.strips {
            let positions = s.layout.positions();
            for (c, &pos) in positions.iter().enumerate() {
                wanted.push((
                    PortKey::Strip(s.id, c),
                    Direction::Input,
                    Self::port_name("strip", s.id, &positions, c),
                    pos,
                ));
            }
        }
        for b in &self.state.buses {
            let positions = b.layout.positions();
            for (c, &pos) in positions.iter().enumerate() {
                wanted.push((
                    PortKey::Bus(b.id, c),
                    Direction::Output,
                    Self::port_name("bus", b.id, &positions, c),
                    pos,
                ));
            }
        }
        for (key, dir, name, pos) in wanted {
            let needs_new = match self.ports.get(&key) {
                Some(existing) => existing.name != name || existing.position != pos,
                None => true,
            };
            if !needs_new {
                continue;
            }
            // A changed port is replaced after the new snapshot no longer
            // references the old one.
            if let Some(old) = self.ports.remove(&key) {
                self.stale_ports.push(old);
            }
            match self.filter.add_port(dir, &name, pos) {
                Ok(ptr) => {
                    debug!("added port {name}");
                    self.ports.insert(
                        key,
                        LocalPort {
                            ptr,
                            name,
                            position: pos,
                        },
                    );
                }
                Err(e) => error!("could not add port {name}: {e}"),
            }
        }
        // Ports whose strip/bus (or channel) vanished.
        let keep = |key: &PortKey, state: &MixerState| match *key {
            PortKey::Strip(id, c) => state
                .strip(id)
                .is_some_and(|s| c < s.layout.channel_count()),
            PortKey::Bus(id, c) => state.bus(id).is_some_and(|b| c < b.layout.channel_count()),
        };
        let stale: Vec<PortKey> = self
            .ports
            .keys()
            .filter(|k| !keep(k, &self.state))
            .copied()
            .collect();
        for k in stale {
            if let Some(p) = self.ports.remove(&k) {
                self.stale_ports.push(p);
            }
        }
    }

    /// Remove the ports `ensure_ports` retired.
    fn remove_stale_ports(&mut self) {
        for p in std::mem::take(&mut self.stale_ports) {
            debug!("removing port {}", p.name);
            // SAFETY: the port came from this filter and the snapshot that
            // dropped it was published in `publish_params` above.
            unsafe { self.filter.remove_port(p.ptr) };
        }
    }

    /// Build a snapshot of `state` and hand it to the real-time thread.
    fn publish_params(&mut self) {
        let shared = self.filter.shared();
        let params = {
            let current = shared.params.load();
            build_rt_params(
                &self.state,
                self.options.solo,
                &PortMap(&self.ports),
                Some(&current),
            )
        };
        let old = shared.params.swap(params);
        self.old_params.push_back(old);
        while self.old_params.len() > OLD_SNAPSHOTS_KEPT {
            self.old_params.pop_front();
        }
        self.params_dirty = false;
    }

    /// Tell the daemon about devices, applications and system volumes, when
    /// they changed.
    fn emit_devices_apps(&mut self) {
        let devices = self.graph.devices();
        if devices != self.last_devices {
            self.last_devices = devices.clone();
            (self.on_event)(EngineEvent::Devices(devices));
        }
        let apps = self.graph.apps(&self.app_volumes.borrow());
        if apps != self.last_apps {
            self.last_apps = apps.clone();
            (self.on_event)(EngineEvent::Apps(apps));
        }
        let system = self.system_volumes();
        if system != self.last_system_volumes {
            self.last_system_volumes = system.clone();
            (self.on_event)(EngineEvent::SystemVolumes(system));
        }
    }

    /// Tell the daemon how the engine is doing, when that changed.
    fn emit_status(&mut self) {
        // Asking PipeWire for the node id also records it for `status`.
        self.filter.node_id();
        let status = self.filter.shared().status();
        if self.last_status.as_ref() != Some(&status) {
            self.last_status = Some(status.clone());
            (self.on_event)(EngineEvent::Status(status));
        }
    }
}
