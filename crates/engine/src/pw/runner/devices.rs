//! Weir's virtual devices: a playback device for each virtual strip, which
//! applications choose as their output, and a microphone for each virtual
//! bus, which applications record from.
//!
//! Each is a `support.null-audio-sink` adapter the runner creates and
//! destroys as strips and buses come and go. Renaming a strip or changing
//! its layout means remaking its device, so the applications playing into
//! it are noted first and moved back once the new one appears.

use super::{Owner, Runner};
use crate::pw::{virtual_input_node_name, virtual_output_node_name};
use pipewire::node::Node;
use pipewire::properties::PropertiesBox;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use weir_protocol::{BusKind, ChannelPosition, StripId, StripKind};

/// How long to wait for a recreated device before giving up on moving its
/// applications back.
const PENDING_MOVE_TIMEOUT: Duration = Duration::from_secs(5);

/// A virtual device this program created.
pub(super) struct VirtualNode {
    pub(super) proxy: Node,
    name: String,
    description: String,
    positions: Vec<ChannelPosition>,
}

/// An application to put back on its strip once the strip's device has been
/// recreated, which happens when the strip is renamed or its layout changes.
pub(super) struct PendingMove {
    app: u32,
    strip: StripId,
    /// The `object.serial` of the device's node before it was recreated,
    /// to wait for a new one. Not its id: PipeWire hands the old id straight
    /// to the new node.
    old_serial: Option<u64>,
    until: Instant,
}

/// A virtual device the mixer state calls for.
struct Wanted {
    owner: Owner,
    /// `node.name`, such as `weir.input.2`.
    name: String,
    /// What the system shows, such as "Music (Weir)".
    description: String,
    /// `Audio/Sink` for a strip, `Audio/Source/Virtual` for a bus.
    class: &'static str,
    positions: Vec<ChannelPosition>,
}

impl Runner {
    /// The virtual devices the mixer state calls for.
    fn wanted_devices(&self) -> Vec<Wanted> {
        let strips = self
            .state
            .strips
            .iter()
            .filter(|s| s.kind == StripKind::Virtual)
            .map(|s| Wanted {
                owner: Owner::Strip(s.id),
                name: virtual_input_node_name(s.id),
                description: format!("{} (Weir)", s.name),
                class: "Audio/Sink",
                positions: s.layout.positions(),
            });
        let buses = self
            .state
            .buses
            .iter()
            .filter(|b| b.kind == BusKind::Virtual)
            .map(|b| Wanted {
                owner: Owner::Bus(b.id),
                name: virtual_output_node_name(b.id),
                description: format!("{} (Weir)", b.name),
                class: "Audio/Source/Virtual",
                positions: b.layout.positions(),
            });
        strips.chain(buses).collect()
    }

    /// Create, remake or destroy virtual devices until there is exactly one
    /// for each virtual strip and bus, with its current name and layout.
    pub(super) fn ensure_virtual_nodes(&mut self) {
        let wanted = self.wanted_devices();
        let stale: Vec<Owner> = self
            .virtual_nodes
            .keys()
            .filter(|k| !wanted.iter().any(|w| w.owner == **k))
            .copied()
            .collect();
        for k in stale {
            if let Some(n) = self.virtual_nodes.remove(&k) {
                info!("destroying virtual device {}", n.name);
                let _ = self.core.destroy_object(n.proxy);
            }
        }
        for w in wanted {
            let up_to_date = self.virtual_nodes.get(&w.owner).is_some_and(|n| {
                n.name == w.name && n.description == w.description && n.positions == w.positions
            });
            if up_to_date {
                continue;
            }
            if let Some(n) = self.virtual_nodes.remove(&w.owner) {
                info!(
                    "recreating virtual device {} ({} -> {})",
                    n.name, n.description, w.description
                );
                if let Owner::Strip(id) = w.owner {
                    self.move_back_later(id, &n.name);
                }
                let _ = self.core.destroy_object(n.proxy);
            }
            self.create_device(w);
        }
    }

    /// Note every application playing into strip `id`, whose device `name`
    /// is about to be remade: they would drop to the default output with
    /// it, so they go back once the new one is up.
    fn move_back_later(&mut self, id: StripId, name: &str) {
        let old_serial = self.graph.node_by_name(name).and_then(|g| g.serial);
        for a in self.last_apps.iter().filter(|a| a.strip == Some(id)) {
            debug!("will move app {} back onto strip {id}", a.id);
            self.pending_moves.push(PendingMove {
                app: a.id,
                strip: id,
                old_serial,
                until: Instant::now() + PENDING_MOVE_TIMEOUT,
            });
        }
    }

    /// Ask PipeWire for the device `w` describes.
    fn create_device(&mut self, w: Wanted) {
        let positions: Vec<&str> = w.positions.iter().map(|p| p.as_str()).collect();
        let mut props = PropertiesBox::new();
        props.insert("factory.name", "support.null-audio-sink");
        props.insert("node.name", w.name.as_str());
        props.insert("node.description", w.description.as_str());
        props.insert("node.nick", w.description.as_str());
        props.insert("media.class", w.class);
        props.insert("audio.channels", w.positions.len().to_string());
        props.insert("audio.position", format!("[ {} ]", positions.join(" ")));
        props.insert("monitor.channel-volumes", "true");
        props.insert("node.virtual", "true");
        props.insert("object.linger", "false");
        match self.core.create_object::<Node>("adapter", &props) {
            Ok(proxy) => {
                info!(
                    "created virtual device {} ({}, {})",
                    w.name, w.description, w.class
                );
                self.virtual_nodes.insert(
                    w.owner,
                    VirtualNode {
                        proxy,
                        name: w.name,
                        description: w.description,
                        positions: w.positions,
                    },
                );
            }
            Err(e) => error!("could not create virtual device {}: {e}", w.name),
        }
    }

    /// Move applications back onto recreated devices that have now appeared.
    pub(super) fn finish_pending_moves(&mut self) {
        if self.pending_moves.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut waiting = Vec::new();
        for m in std::mem::take(&mut self.pending_moves) {
            if now > m.until {
                warn!(
                    "gave up moving app {} back to strip {}: its device did not come back",
                    m.app, m.strip
                );
                continue;
            }
            let name = virtual_input_node_name(m.strip);
            let ready = self
                .graph
                .node_by_name(&name)
                .is_some_and(|n| n.serial.is_some() && n.serial != m.old_serial);
            if ready {
                self.move_app(m.app, m.strip);
            } else {
                waiting.push(m);
            }
        }
        self.pending_moves = waiting;
    }
}
