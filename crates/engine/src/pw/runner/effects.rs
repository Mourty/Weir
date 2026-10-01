//! External effects' connections: whether anything plays into each "back
//! from effects" device, which decides between what comes back and the
//! fallback, and the links to their devices, which outlive the devices
//! being remade when a strip or bus is renamed or changes layout.
//!
//! The sound going out and coming back makes a loop: the engine, "to
//! effects", the effects program, "back from effects", the engine again.
//! PipeWire runs a loop by marking the link that closes it as feedback,
//! which carries the previous cycle's sound, but it only sees loops made of
//! links. Many effects programs are two nodes inside, one that records and
//! one that plays, with no link between them: `pw-loopback`, filter chains.
//! Through those, PipeWire sees no loop, marks nothing, and each node waits
//! for the one before it forever, the engine included.
//!
//! So Weir makes the loop visible itself: the engine's silent
//! `effects_loop` output is linked into every "back from effects" device
//! first, and the links coming back are made only once that one is there.
//! Each of them closes a loop of links, engine to device and back, so
//! PipeWire marks it as feedback, whatever the effects program is made of.

use super::{with_effects, Runner};
use crate::pw::from_effects_node_name;
use crate::pw::graph::PortDirection;
use pipewire::link::Link;
use pipewire::properties::PropertiesBox;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use weir_protocol::{InsertStatus, StripOrBus};

/// How long to wait for a remade device before giving up on making its
/// links again.
const RELINK_TIMEOUT: Duration = Duration::from_secs(5);

/// One end of a link, by the names that outlast a device being remade.
struct End {
    /// Its node's `node.name`.
    node: String,
    /// Its node's `object.serial`, to tell a remade node from the old one.
    serial: Option<u64>,
    /// Its `port.name`, such as `playback_FL`, and direction.
    port: String,
    direction: PortDirection,
}

/// A link with an external effects device about to be remade, to make
/// again with the new device.
pub(super) struct PendingRelink {
    /// The end on that device.
    own: End,
    /// The other end, a port of the effects program as a rule, or of
    /// another of Weir's devices being remade too.
    peer: End,
    /// The other end's port id, while it lasts.
    peer_id: u32,
    until: Instant,
}

/// "strip 3" or "bus 2", for the log.
fn describe(target: StripOrBus) -> String {
    match target {
        StripOrBus::Strip(id) => format!("strip {id}"),
        StripOrBus::Bus(id) => format!("bus {id}"),
    }
}

impl Runner {
    /// Note whether each strip's and bus's external effects are connected:
    /// whether anything plays into its "back from effects" device. Returns
    /// true, and marks the snapshot out of date, when that changed.
    pub(super) fn check_inserts(&mut self) -> bool {
        let engine = self.graph.engine_node().map(|n| n.id);
        let now: Vec<InsertStatus> = with_effects(&self.state)
            .iter()
            .map(|w| {
                let target = w.owner.target();
                // The engine's own silent link does not count.
                let connected = self
                    .graph
                    .node_by_name(&from_effects_node_name(target))
                    .is_some_and(|n| {
                        self.graph
                            .links
                            .values()
                            .any(|l| l.in_node == n.id && Some(l.out_node) != engine)
                    });
                InsertStatus { target, connected }
            })
            .collect();
        if now == self.inserts {
            return false;
        }
        for i in &now {
            let before = self.inserts.iter().find(|b| b.target == i.target);
            if before.map(|b| b.connected) != Some(i.connected) {
                let what = if i.connected {
                    "connected"
                } else {
                    "not connected"
                };
                info!("external effects of {} {what}", describe(i.target));
            }
        }
        self.inserts = now;
        self.params_dirty = true;
        true
    }

    /// Note the links other programs have with external effects' device
    /// `name`, which is about to be remade, to make them again with the new
    /// one. Links with the engine node come back by themselves.
    pub(super) fn relink_later(&mut self, name: &str) {
        let Some(node) = self.graph.node_by_name(name) else {
            return;
        };
        let engine = self.graph.engine_node().map(|n| n.id);
        let until = Instant::now() + RELINK_TIMEOUT;
        let end = |port: u32| {
            let p = self.graph.ports.get(&port)?;
            let n = self.graph.nodes.get(&p.node_id)?;
            Some(End {
                node: n.name.clone(),
                serial: n.serial,
                port: p.name.clone(),
                direction: p.direction,
            })
        };
        for l in self.graph.links.values() {
            let (own, peer) = if l.in_node == node.id {
                (l.in_port, l.out_port)
            } else if l.out_node == node.id {
                (l.out_port, l.in_port)
            } else {
                continue;
            };
            if Some(l.out_node) == engine || Some(l.in_node) == engine {
                continue;
            }
            let (Some(own_end), Some(peer_end)) = (end(own), end(peer)) else {
                continue;
            };
            debug!(
                "will reconnect {name}:{} with {}:{}",
                own_end.port, peer_end.node, peer_end.port
            );
            self.pending_relinks.push(PendingRelink {
                own: own_end,
                peer: peer_end,
                peer_id: peer,
                until,
            });
        }
    }

    /// Make the links `relink_later` noted, once the new devices, and the
    /// other ends, are there.
    pub(super) fn finish_relinks(&mut self) {
        if self.pending_relinks.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_relinks);
        // Devices still being remade: their old ports are no end to link.
        let remade: Vec<(String, Option<u64>)> = pending
            .iter()
            .map(|r| (r.own.node.clone(), r.own.serial))
            .collect();
        let now = Instant::now();
        let mut made = Vec::new();
        let mut waiting = Vec::new();
        for r in pending {
            let own = self.remade_port(&r.own);
            let peer = if remade.contains(&(r.peer.node.clone(), r.peer.serial)) {
                self.remade_port(&r.peer)
            } else {
                self.same_port(r.peer_id, &r.peer)
                    .or_else(|| self.remade_port(&r.peer))
            };
            match (own, peer) {
                (Some(own), Some(peer)) => {
                    let link = match r.own.direction {
                        PortDirection::In => (peer, own),
                        PortDirection::Out => (own, peer),
                    };
                    // Noted from both ends when both were remade.
                    if !made.contains(&link) && !self.graph.has_link(link.0, link.1) {
                        self.link_lingering(link.0, link.1);
                        made.push(link);
                    }
                }
                _ if now > r.until => info!(
                    "could not reconnect {}:{} with {}:{} once remade",
                    r.own.node, r.own.port, r.peer.node, r.peer.port
                ),
                _ => waiting.push(r),
            }
        }
        self.pending_relinks = waiting;
    }

    /// `end`'s port on a node remade since it was noted, if there is one
    /// and only one.
    fn remade_port(&self, end: &End) -> Option<u32> {
        let mut found = self.graph.ports.values().filter(|p| {
            p.name == end.port
                && p.direction == end.direction
                && self
                    .graph
                    .nodes
                    .get(&p.node_id)
                    .is_some_and(|n| n.name == end.node && n.serial != end.serial)
        });
        match (found.next(), found.next()) {
            (Some(p), None) => Some(p.id),
            _ => None,
        }
    }

    /// Port `id`, if it is still `end`'s, on the same node as then.
    fn same_port(&self, id: u32, end: &End) -> Option<u32> {
        let p = self.graph.ports.get(&id)?;
        let n = self.graph.nodes.get(&p.node_id)?;
        (p.name == end.port && n.name == end.node && n.serial == end.serial).then_some(id)
    }

    /// Link two ports the way a patchbay does: the link belongs to
    /// PipeWire, not to Weir, like the one it replaces.
    fn link_lingering(&self, out_port: u32, in_port: u32) {
        let (Some(op), Some(ip)) = (
            self.graph.ports.get(&out_port),
            self.graph.ports.get(&in_port),
        ) else {
            return;
        };
        let mut props = PropertiesBox::new();
        props.insert("link.output.node", op.node_id.to_string());
        props.insert("link.output.port", out_port.to_string());
        props.insert("link.input.node", ip.node_id.to_string());
        props.insert("link.input.port", in_port.to_string());
        props.insert("object.linger", "true");
        match self.core.create_object::<Link>("link-factory", &props) {
            // Dropping the proxy leaves a lingering link in place.
            Ok(_) => info!(
                "reconnected {}:{} -> {}:{}",
                op.node_id, op.name, ip.node_id, ip.name
            ),
            Err(e) => warn!("could not reconnect ports {out_port} -> {in_port}: {e}"),
        }
    }
}
