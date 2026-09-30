//! A mirror of the PipeWire registry: just enough about nodes, ports and
//! links to discover devices and manage our own links.

use super::volume::AppVolume;
use super::{is_our_node, strip_of_virtual_input, ENGINE_NODE_NAME};
use pipewire::registry::GlobalObject;
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;
use std::collections::BTreeMap;
use weir_protocol::{linear_to_db, AppStream, ChannelPosition, DeviceInfo, DeviceKind};

/// A node: a device, an application stream, or one of ours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeEntry {
    /// Global id. PipeWire reuses these; `serial` it does not.
    pub id: u32,
    /// `node.name`, stable across restarts.
    pub name: String,
    /// `node.description`, or failing that its nick or name: what people see.
    pub description: String,
    /// `node.nick`.
    pub nick: Option<String>,
    /// `media.class`, such as `Audio/Sink` or `Stream/Output/Audio`.
    pub media_class: String,
    /// For an application stream, `application.name`.
    pub app_name: Option<String>,
    /// For an application stream, `application.process.binary`.
    pub app_binary: Option<String>,
    /// For an application stream, `media.name`: what it is playing.
    pub media_name: Option<String>,
    /// `object.serial`, unique for as long as PipeWire runs.
    pub serial: Option<u64>,
}

/// Which way a port carries audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDirection {
    /// Into its node.
    In,
    /// Out of its node.
    Out,
}

/// A port of some node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortEntry {
    /// Global id.
    pub id: u32,
    /// The node it belongs to.
    pub node_id: u32,
    /// `port.name`.
    pub name: String,
    /// Which way it carries audio.
    pub direction: PortDirection,
    /// `audio.channel`, when it says.
    pub channel: Option<ChannelPosition>,
    /// Whether it is a sink's monitor port: a copy of what the sink plays.
    pub monitor: bool,
    /// `port.id` inside the node, used to keep a stable order.
    pub index: u32,
}

/// A link from one port to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkEntry {
    /// Global id.
    pub id: u32,
    /// The output side's node and port.
    pub out_node: u32,
    pub out_port: u32,
    /// The input side's node and port.
    pub in_node: u32,
    pub in_port: u32,
}

/// What this program knows of the PipeWire graph, from the registry.
#[derive(Debug, Default)]
pub struct Graph {
    /// Every node, by global id.
    pub nodes: BTreeMap<u32, NodeEntry>,
    /// Every port, by global id.
    pub ports: BTreeMap<u32, PortEntry>,
    /// Every link, by global id.
    pub links: BTreeMap<u32, LinkEntry>,
    /// Global id of the `default` metadata object, if seen.
    pub default_metadata: Option<u32>,
}

fn get(props: &DictRef, key: &str) -> Option<String> {
    props.get(key).map(|s| s.to_string())
}

impl Graph {
    /// Record a new global. Returns true when it was something we track.
    pub fn add_global<P: AsRef<DictRef>>(&mut self, obj: &GlobalObject<P>) -> bool {
        let Some(props) = obj.props.as_ref().map(|p| p.as_ref()) else {
            return false;
        };
        match obj.type_ {
            ObjectType::Node => {
                let name = get(props, "node.name").unwrap_or_else(|| format!("node-{}", obj.id));
                let description = get(props, "node.description")
                    .or_else(|| get(props, "node.nick"))
                    .unwrap_or_else(|| name.clone());
                self.nodes.insert(
                    obj.id,
                    NodeEntry {
                        id: obj.id,
                        name,
                        description,
                        nick: get(props, "node.nick"),
                        media_class: get(props, "media.class").unwrap_or_default(),
                        app_name: get(props, "application.name"),
                        app_binary: get(props, "application.process.binary"),
                        media_name: get(props, "media.name"),
                        serial: get(props, "object.serial").and_then(|s| s.parse().ok()),
                    },
                );
                true
            }
            ObjectType::Port => {
                let Some(node_id) = get(props, "node.id").and_then(|s| s.parse().ok()) else {
                    return false;
                };
                let direction = match props.get("port.direction") {
                    Some("in") => PortDirection::In,
                    Some("out") => PortDirection::Out,
                    _ => return false,
                };
                self.ports.insert(
                    obj.id,
                    PortEntry {
                        id: obj.id,
                        node_id,
                        name: get(props, "port.name").unwrap_or_default(),
                        direction,
                        channel: props.get("audio.channel").and_then(ChannelPosition::parse),
                        monitor: props.get("port.monitor") == Some("true"),
                        index: get(props, "port.id")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(obj.id),
                    },
                );
                true
            }
            ObjectType::Link => {
                let num = |k: &str| get(props, k).and_then(|s| s.parse::<u32>().ok());
                if let (Some(out_node), Some(out_port), Some(in_node), Some(in_port)) = (
                    num("link.output.node"),
                    num("link.output.port"),
                    num("link.input.node"),
                    num("link.input.port"),
                ) {
                    self.links.insert(
                        obj.id,
                        LinkEntry {
                            id: obj.id,
                            out_node,
                            out_port,
                            in_node,
                            in_port,
                        },
                    );
                    true
                } else {
                    false
                }
            }
            ObjectType::Metadata => {
                if props.get("metadata.name") == Some("default") {
                    self.default_metadata = Some(obj.id);
                }
                false
            }
            _ => false,
        }
    }

    /// Forget a global. Returns true when it was something we tracked.
    pub fn remove_global(&mut self, id: u32) -> bool {
        if self.default_metadata == Some(id) {
            self.default_metadata = None;
        }
        self.nodes.remove(&id).is_some()
            | self.ports.remove(&id).is_some()
            | self.links.remove(&id).is_some()
    }

    /// The node called `name`.
    pub fn node_by_name(&self, name: &str) -> Option<&NodeEntry> {
        self.nodes.values().find(|n| n.name == name)
    }

    /// Weir's engine node, once PipeWire has announced it.
    pub fn engine_node(&self) -> Option<&NodeEntry> {
        self.node_by_name(ENGINE_NODE_NAME)
    }

    /// Ports of a node in a direction, sorted by their index. `monitor`
    /// selects monitor ports (`Some(true)`), non-monitor ports
    /// (`Some(false)`) or both (`None`).
    pub fn ports_of(
        &self,
        node_id: u32,
        direction: PortDirection,
        monitor: Option<bool>,
    ) -> Vec<&PortEntry> {
        let mut v: Vec<&PortEntry> = self
            .ports
            .values()
            .filter(|p| {
                p.node_id == node_id
                    && p.direction == direction
                    && monitor.is_none_or(|m| p.monitor == m)
            })
            .collect();
        v.sort_by_key(|p| p.index);
        v
    }

    /// Whether any link joins these two ports, whoever made it.
    pub fn has_link(&self, out_port: u32, in_port: u32) -> bool {
        self.links
            .values()
            .any(|l| l.out_port == out_port && l.in_port == in_port)
    }

    /// Resolve a device name as stored in the mixer state into the node and
    /// the port set to use. `.monitor` suffixed names select a sink's
    /// monitor ports.
    pub fn resolve_source(&self, device: &str) -> Option<(u32, Vec<&PortEntry>)> {
        if let Some(base) = device.strip_suffix(".monitor") {
            let node = self.node_by_name(base)?;
            Some((
                node.id,
                self.ports_of(node.id, PortDirection::Out, Some(true)),
            ))
        } else {
            let node = self.node_by_name(device)?;
            Some((node.id, self.captured_ports(node)))
        }
    }

    /// The output ports that carry what a source captures. A virtual source
    /// made from a null sink, as virtual microphones such as Easy Effects'
    /// are, marks its outputs as monitor ports, so for a source every output
    /// counts; a duplex device's monitor ports are a copy of what it plays,
    /// and do not.
    fn captured_ports(&self, node: &NodeEntry) -> Vec<&PortEntry> {
        let monitor = if node.media_class.starts_with("Audio/Source") {
            None
        } else {
            Some(false)
        };
        self.ports_of(node.id, PortDirection::Out, monitor)
    }

    /// Resolve a sink by name into its node and input ports.
    pub fn resolve_sink(&self, device: &str) -> Option<(u32, Vec<&PortEntry>)> {
        let node = self.node_by_name(device)?;
        Some((node.id, self.ports_of(node.id, PortDirection::In, None)))
    }

    /// Devices a user can pick for hardware strips (sources, including sink
    /// monitors) and buses (sinks). Our own nodes are excluded.
    pub fn devices(&self) -> Vec<DeviceInfo> {
        let mut out = Vec::new();
        for n in self.nodes.values() {
            if is_our_node(&n.name) {
                continue;
            }
            let device =
                |name: String, description: String, kind, ports: Vec<&PortEntry>| DeviceInfo {
                    id: n.id,
                    name,
                    description,
                    kind,
                    channels: ports.iter().filter_map(|p| p.channel).collect(),
                };
            let outputs = || self.captured_ports(n);
            let plain = || (n.name.clone(), n.description.clone());
            match n.media_class.as_str() {
                "Audio/Sink" | "Audio/Duplex" => {
                    let (name, description) = plain();
                    let inputs = self.ports_of(n.id, PortDirection::In, None);
                    out.push(device(name, description, DeviceKind::Sink, inputs));
                    // A sink's monitor is a source too: what it plays.
                    let monitor = self.ports_of(n.id, PortDirection::Out, Some(true));
                    if !monitor.is_empty() {
                        let name = format!("{}.monitor", n.name);
                        let description = format!("Monitor of {}", n.description);
                        out.push(device(name, description, DeviceKind::Source, monitor));
                    }
                    if n.media_class == "Audio/Duplex" {
                        let (name, description) = plain();
                        out.push(device(name, description, DeviceKind::Source, outputs()));
                    }
                }
                "Audio/Source" | "Audio/Source/Virtual" => {
                    let (name, description) = plain();
                    out.push(device(name, description, DeviceKind::Source, outputs()));
                }
                _ => {}
            }
        }
        out.sort_by(|a, b| (a.kind as u8, &a.description).cmp(&(b.kind as u8, &b.description)));
        out
    }

    /// Application playback streams, where they go and how loud they are.
    pub fn apps(&self, volumes: &BTreeMap<u32, AppVolume>) -> Vec<AppStream> {
        let mut out = Vec::new();
        for n in self.nodes.values() {
            if n.media_class != "Stream/Output/Audio" {
                continue;
            }
            let target_node = self
                .links
                .values()
                .find(|l| l.out_node == n.id)
                .and_then(|l| self.nodes.get(&l.in_node));
            let target = target_node.map(|t| t.name.clone());
            let strip = target.as_deref().and_then(strip_of_virtual_input);
            let volume = volumes.get(&n.id);
            out.push(AppStream {
                id: n.id,
                name: n
                    .app_name
                    .clone()
                    .or_else(|| n.nick.clone())
                    .unwrap_or_else(|| n.name.clone()),
                binary: n.app_binary.clone(),
                media_name: n.media_name.clone(),
                target,
                strip,
                volume_db: volume.and_then(|v| v.level()).map(linear_to_db),
                mute: volume.is_some_and(|v| v.mute),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, name: &str, media_class: &str) -> NodeEntry {
        NodeEntry {
            id,
            name: name.into(),
            description: name.into(),
            nick: None,
            media_class: media_class.into(),
            app_name: None,
            app_binary: None,
            media_name: None,
            serial: None,
        }
    }

    fn port(
        id: u32,
        node_id: u32,
        direction: PortDirection,
        channel: ChannelPosition,
        monitor: bool,
    ) -> PortEntry {
        PortEntry {
            id,
            node_id,
            name: format!("port{id}"),
            direction,
            channel: Some(channel),
            monitor,
            index: id,
        }
    }

    /// A microphone, a virtual one made from a null sink (whose outputs are
    /// marked as monitors), and headphones.
    fn graph() -> Graph {
        use ChannelPosition::{Mono, FL, FR};
        use PortDirection::{In, Out};
        let mut g = Graph::default();
        for n in [
            node(1, "mic", "Audio/Source"),
            node(2, "virtual_mic", "Audio/Source/Virtual"),
            node(3, "headphones", "Audio/Sink"),
        ] {
            g.nodes.insert(n.id, n);
        }
        for p in [
            port(10, 1, Out, Mono, false),
            port(20, 2, In, FL, false),
            port(21, 2, In, FR, false),
            port(22, 2, Out, FL, true),
            port(23, 2, Out, FR, true),
            port(30, 3, In, FL, false),
            port(31, 3, In, FR, false),
            port(32, 3, Out, FL, true),
            port(33, 3, Out, FR, true),
        ] {
            g.ports.insert(p.id, p);
        }
        g
    }

    fn port_ids(found: Option<(u32, Vec<&PortEntry>)>) -> Vec<u32> {
        found.map_or(Vec::new(), |(_, ports)| {
            ports.iter().map(|p| p.id).collect()
        })
    }

    #[test]
    fn sources_capture_from_their_outputs_even_marked_as_monitors() {
        let g = graph();
        assert_eq!(port_ids(g.resolve_source("mic")), [10]);
        assert_eq!(port_ids(g.resolve_source("virtual_mic")), [22, 23]);
        // A sink's monitor is what it plays.
        assert_eq!(port_ids(g.resolve_source("headphones.monitor")), [32, 33]);
        assert_eq!(port_ids(g.resolve_sink("headphones")), [30, 31]);
    }

    #[test]
    fn a_virtual_source_is_listed_with_its_channels() {
        let devices = graph().devices();
        let virtual_mic = devices.iter().find(|d| d.name == "virtual_mic").unwrap();
        assert_eq!(virtual_mic.kind, DeviceKind::Source);
        assert_eq!(
            virtual_mic.channels,
            [ChannelPosition::FL, ChannelPosition::FR]
        );
        let monitor = devices
            .iter()
            .find(|d| d.name == "headphones.monitor")
            .unwrap();
        assert_eq!(monitor.kind, DeviceKind::Source);
    }
}
