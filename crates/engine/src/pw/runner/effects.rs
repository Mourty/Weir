//! External effects, as far as PipeWire goes: the node their sound comes
//! back into, whether anything plays into each "from effects" device, which
//! decides between what comes back and the fallback, and the links to
//! their devices, which outlive the devices being remade when a strip or
//! bus is renamed or changes layout.
//!
//! The sound goes out of the engine into "to effects", through the effects
//! program into "from effects", and from there into a node of its own, the
//! return node, "Weir effects return", which hands it to the engine for its
//! next cycle (see [`crate::dsp::handoff`]). In a patchbay that reads as a
//! line, Weir to the effects and on to Weir, with no wire doubling back;
//! and PipeWire sees no loop, which it could not run through an effects
//! program made of two unlinked nodes, such as a filter chain.
//!
//! The return node exists only while some strip or bus has external effects
//! on, with a port per channel of each, linked from the monitor of its
//! "from effects" device. An effects program can also be wired straight
//! into those ports, in a patchbay, and that counts as connected too.

use super::{with_effects, LocalPort, Owner, PortKey, Runner, OLD_SNAPSHOTS_KEPT};
use crate::dsp::handoff::Handoff;
use crate::pw::filter::{Direction, Filter, ReturnParams, ReturnShared, RtReturn};
use crate::pw::graph::PortDirection;
use crate::pw::{from_effects_node_name, ENGINE_NODE_NAME, RETURN_NODE_NAME};
use pipewire::link::Link;
use pipewire::properties::PropertiesBox;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use weir_protocol::{InsertStatus, StripOrBus};

/// The node external effects come back into, and what it keeps.
pub(super) struct Returns {
    filter: Filter<ReturnShared>,
    /// Its ports, `PortKey::FromEffects`, one per channel of each strip's
    /// or bus's external effects.
    pub(super) ports: HashMap<PortKey, LocalPort>,
    /// Ports to remove once a snapshot without them is published.
    stale: Vec<LocalPort>,
    /// Replaced snapshots, kept so its real-time thread never frees one.
    old: VecDeque<Arc<ReturnParams>>,
}

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
    /// Keep a hand-off for each strip or bus with external effects on, and
    /// the return node with a port for each of their channels, or no return
    /// node while none has them on. Ports no longer wanted go to its stale
    /// list, for after the next snapshot.
    pub(super) fn ensure_returns(&mut self) {
        let wanted = with_effects(&self.state);
        // A hand-off is made for a number of channels, so a new layout
        // needs a new one.
        self.handoffs.retain(|owner, h| {
            wanted
                .iter()
                .any(|w| w.owner == *owner && w.positions.len() == h.channels())
        });
        for w in &wanted {
            self.handoffs
                .entry(w.owner)
                .or_insert_with(|| Arc::new(Handoff::new(w.positions.len())));
        }
        if wanted.is_empty() {
            if self.returns.take().is_some() {
                info!("removed the effects return node: no external effects are on");
            }
            return;
        }
        if self.returns.is_none() {
            match Filter::new_return(
                self.main_loop.loop_(),
                ReturnShared::new(),
                RETURN_NODE_NAME,
            ) {
                Ok(filter) => {
                    info!("created the effects return node");
                    self.returns = Some(Returns {
                        filter,
                        ports: HashMap::new(),
                        stale: Vec::new(),
                        old: VecDeque::new(),
                    });
                }
                Err(e) => {
                    error!("could not create the effects return node: {e}");
                    return;
                }
            }
        }
        let Some(r) = self.returns.as_mut() else {
            return;
        };
        for w in &wanted {
            let (kind, id, positions) = (w.owner.kind(), w.owner.id(), &w.positions);
            for (c, &pos) in positions.iter().enumerate() {
                let key = PortKey::FromEffects(w.owner, c);
                let name = Self::port_name(&format!("from_effects_{kind}"), id, positions, c);
                if r.ports
                    .get(&key)
                    .is_some_and(|p| p.name == name && p.position == pos)
                {
                    continue;
                }
                if let Some(old) = r.ports.remove(&key) {
                    r.stale.push(old);
                }
                match r.filter.add_port(Direction::Input, &name, pos) {
                    Ok(ptr) => {
                        debug!("added port {name} to the effects return node");
                        r.ports.insert(
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
        }
        let gone: Vec<PortKey> = r
            .ports
            .keys()
            .filter(|k| match **k {
                PortKey::FromEffects(owner, c) => !wanted
                    .iter()
                    .any(|w| w.owner == owner && c < w.positions.len()),
                _ => true,
            })
            .copied()
            .collect();
        for k in gone {
            if let Some(p) = r.ports.remove(&k) {
                r.stale.push(p);
            }
        }
    }

    /// Hand the return node what it passes on: each strip's or bus's ports
    /// and hand-off.
    pub(super) fn publish_returns(&mut self) {
        let Some(r) = self.returns.as_mut() else {
            return;
        };
        let returns = with_effects(&self.state)
            .iter()
            .filter_map(|w| {
                let handoff = self.handoffs.get(&w.owner)?.clone();
                let ports = (0..w.positions.len())
                    .map(|c| {
                        r.ports
                            .get(&PortKey::FromEffects(w.owner, c))
                            .map_or(std::ptr::null_mut(), |p| p.ptr)
                    })
                    .collect();
                Some(RtReturn { ports, handoff })
            })
            .collect();
        let old = r
            .filter
            .shared()
            .params
            .swap(Arc::new(ReturnParams { returns }));
        r.old.push_back(old);
        while r.old.len() > OLD_SNAPSHOTS_KEPT {
            r.old.pop_front();
        }
    }

    /// Remove the return node's ports `ensure_returns` retired.
    pub(super) fn remove_stale_return_ports(&mut self) {
        let Some(r) = self.returns.as_mut() else {
            return;
        };
        for p in std::mem::take(&mut r.stale) {
            debug!("removing port {} from the effects return node", p.name);
            // SAFETY: the port came from this filter and the snapshot that
            // dropped it was published in `publish_returns`.
            unsafe { r.filter.remove_port(p.ptr) };
        }
    }

    /// Global ids of the return node's ports for `owner`'s channels, as
    /// PipeWire knows them so far.
    pub(super) fn return_port_ids(&self, owner: Owner) -> Vec<(usize, u32)> {
        let (Some(r), Some(node)) = (
            self.returns.as_ref(),
            self.graph.node_by_name(RETURN_NODE_NAME),
        ) else {
            return Vec::new();
        };
        r.ports
            .iter()
            .filter_map(|(key, lp)| match *key {
                PortKey::FromEffects(o, c) if o == owner => {
                    let id = self
                        .graph
                        .ports
                        .values()
                        .find(|p| p.node_id == node.id && p.name == lp.name)?
                        .id;
                    Some((c, id))
                }
                _ => None,
            })
            .collect()
    }

    /// Note whether each strip's and bus's external effects are connected:
    /// whether anything plays into its "from effects" device, or straight
    /// into its ports on the return node. Returns true, and marks the
    /// snapshot out of date, when that changed.
    pub(super) fn check_inserts(&mut self) -> bool {
        let now: Vec<InsertStatus> = with_effects(&self.state)
            .iter()
            .map(|w| {
                let target = w.owner.target();
                let device = self
                    .graph
                    .node_by_name(&from_effects_node_name(target))
                    .map(|n| n.id);
                let ours: Vec<u32> = self
                    .return_port_ids(w.owner)
                    .into_iter()
                    .map(|(_, id)| id)
                    .collect();
                // The device's own link into the return node is Weir's.
                let connected = self.graph.links.values().any(|l| {
                    Some(l.in_node) == device
                        || (ours.contains(&l.in_port) && Some(l.out_node) != device)
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
    /// one. Links with Weir's own nodes come back by themselves.
    pub(super) fn relink_later(&mut self, name: &str) {
        let Some(node) = self.graph.node_by_name(name) else {
            return;
        };
        let weir: Vec<u32> = [ENGINE_NODE_NAME, RETURN_NODE_NAME]
            .iter()
            .filter_map(|n| self.graph.node_by_name(n).map(|n| n.id))
            .collect();
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
            if weir.contains(&l.out_node) || weir.contains(&l.in_node) {
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
