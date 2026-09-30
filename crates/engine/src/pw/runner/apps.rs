//! Application streams: following their own volumes (and the system
//! volumes of Weir's devices), setting those volumes, and moving streams
//! onto strips.

use super::Runner;
use crate::pw::engine::EngineCommand;
use crate::pw::{bus_of_virtual_output, strip_of_virtual_input, virtual_input_node_name, volume};
use libspa::param::ParamType;
use libspa::pod::Pod;
use pipewire::node::Node;
use pipewire::registry::GlobalObject;
use pipewire::spa::utils::dict::DictRef;
use tracing::{debug, error, info, warn};
use weir_protocol::{
    db_to_linear, linear_to_db, StripId, SystemVolume, SystemVolumes, GAIN_MAX_DB, GAIN_MIN_DB,
};

impl Runner {
    /// Bind a node and follow its volume: an application stream's, for its
    /// slider, or one of Weir's devices', to show what the system set.
    pub(super) fn watch_volume<P: AsRef<DictRef>>(&mut self, obj: &GlobalObject<P>) {
        if self.app_nodes.contains_key(&obj.id) {
            return;
        }
        let node: Node = match self.registry.bind(obj) {
            Ok(n) => n,
            Err(e) => {
                warn!("could not bind node {}: {e}", obj.id);
                return;
            }
        };
        let id = obj.id;
        let volumes = self.app_volumes.clone();
        let tx = self.self_tx.clone();
        let listener = node
            .add_listener_local()
            .param(move |_seq, param_type, _index, _next, pod| {
                if param_type != ParamType::Props {
                    return;
                }
                let Some(parsed) = pod.and_then(|p| volume::parse_props(p.as_bytes())) else {
                    return;
                };
                let changed = volumes.borrow().get(&id) != Some(&parsed);
                if changed {
                    volumes.borrow_mut().insert(id, parsed);
                    // The runner may be busy right now; let the command
                    // loop tell the daemon.
                    let _ = tx.send(EngineCommand::AppsDirty);
                }
            })
            .register();
        node.subscribe_params(&[ParamType::Props]);
        self.app_nodes.insert(id, (node, listener));
    }

    /// The system volume of each of our virtual devices that reported one.
    pub(super) fn system_volumes(&self) -> SystemVolumes {
        let volumes = self.app_volumes.borrow();
        let mut out = SystemVolumes::default();
        for n in self.graph.nodes.values() {
            let Some(v) = volumes.get(&n.id) else {
                continue;
            };
            let Some(level) = v.level() else {
                continue;
            };
            let sv = SystemVolume {
                volume_db: linear_to_db(level),
                mute: v.mute,
            };
            if let Some(strip) = strip_of_virtual_input(&n.name) {
                out.strips.insert(strip, sv);
            } else if let Some(bus) = bus_of_virtual_output(&n.name) {
                out.buses.insert(bus, sv);
            }
        }
        out
    }

    /// Write a new volume onto an application stream.
    pub(super) fn set_app_volume(&mut self, app: u32, volume_db: Option<f32>, mute: Option<bool>) {
        // Our own devices are watched too, but their volume belongs to the
        // system and is never written.
        let is_app = self
            .graph
            .nodes
            .get(&app)
            .is_some_and(|n| n.media_class == "Stream/Output/Audio");
        let Some((node, _)) = self.app_nodes.get(&app).filter(|_| is_app) else {
            warn!("cannot set the volume of {app}: not a known application stream");
            return;
        };
        // Reuse the stream's own channel count so we do not change its shape.
        let channels = self
            .app_volumes
            .borrow()
            .get(&app)
            .map(|v| v.channel_volumes.len())
            .filter(|n| *n > 0)
            .unwrap_or(2);
        let volumes = match volume_db {
            Some(db) => vec![db_to_linear(db.clamp(GAIN_MIN_DB, GAIN_MAX_DB)); channels],
            None => Vec::new(),
        };
        let Some(bytes) = volume::build_props(&volumes, mute) else {
            return;
        };
        let Some(pod) = Pod::from_bytes(&bytes) else {
            error!("built an invalid volume pod for {app}");
            return;
        };
        debug!("setting volume of {app}: {volume_db:?} dB, mute {mute:?}");
        node.set_param(ParamType::Props, 0, pod);
    }

    /// Point an application at one of our virtual inputs.
    ///
    /// The session manager reads `target.object` from the default metadata
    /// and resolves it against the target node's `object.serial`. Older
    /// WirePlumber releases also fall back to matching `node.name`, but newer
    /// ones do not, and an unresolvable target sends the application to the
    /// system default output instead. So send the serial, and only fall back
    /// to the name when the serial is somehow missing. The value is a plain
    /// string, not JSON: a quoted one matches nothing.
    pub(super) fn move_app(&mut self, app: u32, strip: StripId) {
        let Some(meta) = self.metadata.as_ref() else {
            warn!("cannot move app {app}: default metadata not available");
            return;
        };
        if !self.graph.nodes.contains_key(&app) {
            warn!("cannot move app {app}: unknown node");
            return;
        }
        let name = virtual_input_node_name(strip);
        let Some(node) = self.graph.node_by_name(&name) else {
            warn!("cannot move app {app}: {name} is not in the graph yet");
            return;
        };
        let target = match node.serial {
            Some(serial) => serial.to_string(),
            None => {
                warn!("{name} has no object.serial; falling back to its name");
                name.clone()
            }
        };
        info!("moving app {app} to {name} (target.object {target})");
        meta.set_property(app, "target.object", None, Some(&target));
    }
}
