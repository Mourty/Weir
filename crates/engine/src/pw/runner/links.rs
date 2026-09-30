//! Links between the engine node and devices: each strip's source into the
//! strip's input ports, and each bus's output ports into its device.

use super::{Owner, Runner};
use crate::pw::graph::{PortDirection, PortEntry};
use crate::pw::{virtual_input_node_name, virtual_output_node_name};
use pipewire::link::Link;
use pipewire::properties::PropertiesBox;
use std::collections::HashMap;
use tracing::{debug, warn};
use weir_protocol::{BusKind, ChannelPosition, Side, StripKind};

/// Pair our ports with a device's ports by channel position. `ours` is
/// `(position, global id)`, `theirs` likewise, where a device port may not
/// say its position. `into_device` is true when our ports play into the
/// device (a bus), false when they take from it (a strip). Returns
/// `(ours, theirs)` global id pairs.
///
/// * A device that names no positions is paired in port order.
/// * Otherwise each of ours goes to the device's port of the same position,
///   or its mono port, or failing those the front port on the same side.
/// * Our mono or center goes to both of the device's fronts when playing
///   into it, and takes from its front left (or first port) when taking
///   from it.
/// * Our subwoofer goes nowhere it has no place of its own.
fn pair(
    ours: &[(ChannelPosition, u32)],
    theirs: &[(Option<ChannelPosition>, u32)],
    into_device: bool,
) -> Vec<(u32, u32)> {
    use ChannelPosition::*;
    let mut out = Vec::new();
    let their_pos = |p: ChannelPosition| theirs.iter().find(|(tp, _)| *tp == Some(p)).map(|t| t.1);
    let untyped = theirs.iter().all(|(tp, _)| tp.is_none());
    for (i, &(pos, ours_id)) in ours.iter().enumerate() {
        if untyped {
            if let Some(t) = theirs.get(i) {
                out.push((ours_id, t.1));
            }
            continue;
        }
        if let Some(t) = their_pos(pos).or_else(|| their_pos(Mono)) {
            out.push((ours_id, t));
            continue;
        }
        match pos.side() {
            Side::Left => out.extend(their_pos(FL).map(|t| (ours_id, t))),
            Side::Right => out.extend(their_pos(FR).map(|t| (ours_id, t))),
            Side::Center if into_device => {
                for p in [FL, FR] {
                    out.extend(their_pos(p).map(|t| (ours_id, t)));
                }
            }
            Side::Center => out.extend(
                their_pos(FL)
                    .or_else(|| theirs.first().map(|t| t.1))
                    .map(|t| (ours_id, t)),
            ),
            Side::Lfe => {}
        }
    }
    out
}

impl Runner {
    /// Global ids of the engine node's ports, by name.
    fn our_port_ids(&self, engine_id: u32) -> HashMap<String, u32> {
        self.graph
            .ports
            .values()
            .filter(|p| p.node_id == engine_id)
            .map(|p| (p.name.clone(), p.id))
            .collect()
    }

    /// `owner`'s ports on the engine node that PipeWire knows about yet, as
    /// `(position, global id)`, given its `n` channels and the node's ports
    /// by name.
    fn ours(
        &self,
        owner: Owner,
        n: usize,
        by_name: &HashMap<String, u32>,
    ) -> Vec<(ChannelPosition, u32)> {
        (0..n)
            .filter_map(|c| {
                let lp = self.ports.get(&owner.port(c))?;
                Some((lp.position, *by_name.get(&lp.name)?))
            })
            .collect()
    }

    /// The links the mixer state calls for, as `(output port, input port)`.
    fn desired_links(&self, engine_id: u32) -> Vec<(u32, u32)> {
        let by_name = self.our_port_ids(engine_id);
        let theirs = |ports: Vec<&PortEntry>| -> Vec<(Option<ChannelPosition>, u32)> {
            ports.iter().map(|p| (p.channel, p.id)).collect()
        };
        let mut desired = Vec::new();
        for s in &self.state.strips {
            let ours = self.ours(Owner::Strip(s.id), s.layout.channel_count(), &by_name);
            let source = match s.kind {
                StripKind::Hardware => s
                    .device
                    .as_deref()
                    .and_then(|d| self.graph.resolve_source(d)),
                // A virtual strip takes what applications play into its
                // device from that device's monitor ports.
                StripKind::Virtual => {
                    self.graph
                        .node_by_name(&virtual_input_node_name(s.id))
                        .map(|n| {
                            (
                                n.id,
                                self.graph.ports_of(n.id, PortDirection::Out, Some(true)),
                            )
                        })
                }
            };
            if let Some((_, dev_ports)) = source {
                for (our_in, dev_out) in pair(&ours, &theirs(dev_ports), false) {
                    desired.push((dev_out, our_in));
                }
            }
        }
        for b in &self.state.buses {
            let ours = self.ours(Owner::Bus(b.id), b.layout.channel_count(), &by_name);
            let sink = match b.kind {
                BusKind::Hardware => b.device.as_deref().and_then(|d| self.graph.resolve_sink(d)),
                BusKind::Virtual => self
                    .graph
                    .node_by_name(&virtual_output_node_name(b.id))
                    .map(|n| (n.id, self.graph.ports_of(n.id, PortDirection::In, None))),
            };
            if let Some((_, dev_ports)) = sink {
                for (our_out, dev_in) in pair(&ours, &theirs(dev_ports), true) {
                    desired.push((our_out, dev_in));
                }
            }
        }
        desired
    }

    /// Make the links the mixer state calls for, and remove the ones this
    /// program made that it no longer does.
    pub(super) fn ensure_links(&mut self) {
        let Some(engine) = self.graph.engine_node() else {
            return;
        };
        let desired = self.desired_links(engine.id);

        for &(out_port, in_port) in &desired {
            if self.links.contains_key(&(out_port, in_port))
                || self.graph.has_link(out_port, in_port)
            {
                continue;
            }
            let (Some(op), Some(ip)) = (
                self.graph.ports.get(&out_port),
                self.graph.ports.get(&in_port),
            ) else {
                continue;
            };
            let mut props = PropertiesBox::new();
            props.insert("link.output.node", op.node_id.to_string());
            props.insert("link.output.port", out_port.to_string());
            props.insert("link.input.node", ip.node_id.to_string());
            props.insert("link.input.port", in_port.to_string());
            props.insert("object.linger", "false");
            match self.core.create_object::<Link>("link-factory", &props) {
                Ok(link) => {
                    debug!(
                        "linked {}:{} -> {}:{}",
                        op.node_id, op.name, ip.node_id, ip.name
                    );
                    self.links.insert((out_port, in_port), link);
                }
                Err(e) => warn!("could not link ports {out_port} -> {in_port}: {e}"),
            }
        }
        let unwanted: Vec<(u32, u32)> = self
            .links
            .keys()
            .filter(|k| !desired.contains(k))
            .copied()
            .collect();
        for key in unwanted {
            if let Some(link) = self.links.remove(&key) {
                debug!("unlinking {} -> {}", key.0, key.1);
                let _ = self.core.destroy_object(link);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pair;
    use weir_protocol::ChannelPosition::*;

    #[test]
    fn channels_pair_by_position() {
        let ours = [(FL, 1), (FR, 2)];
        let theirs = [(Some(FR), 20), (Some(FL), 10)];
        assert_eq!(pair(&ours, &theirs, true), vec![(1, 10), (2, 20)]);
    }

    #[test]
    fn a_device_without_positions_pairs_in_order() {
        let ours = [(FL, 1), (FR, 2), (FC, 3)];
        let theirs = [(None, 10), (None, 11)];
        assert_eq!(pair(&ours, &theirs, true), vec![(1, 10), (2, 11)]);
    }

    #[test]
    fn mono_meets_stereo_both_ways() {
        // A mono bus plays on both speakers of a stereo device.
        assert_eq!(
            pair(&[(Mono, 1)], &[(Some(FL), 10), (Some(FR), 11)], true),
            vec![(1, 10), (1, 11)]
        );
        // A mono strip takes the left of a stereo microphone.
        assert_eq!(
            pair(&[(Mono, 1)], &[(Some(FL), 10), (Some(FR), 11)], false),
            vec![(1, 10)]
        );
        // A stereo strip takes both sides of a mono microphone.
        assert_eq!(
            pair(&[(FL, 1), (FR, 2)], &[(Some(Mono), 10)], false),
            vec![(1, 10), (2, 10)]
        );
    }

    #[test]
    fn surrounds_fold_to_the_front_and_the_subwoofer_stays_out() {
        let ours = [(FL, 1), (FR, 2), (FC, 3), (LFE, 4), (RL, 5), (RR, 6)];
        let theirs = [(Some(FL), 10), (Some(FR), 11)];
        assert_eq!(
            pair(&ours, &theirs, true),
            vec![(1, 10), (2, 11), (3, 10), (3, 11), (5, 10), (6, 11)]
        );
    }
}
