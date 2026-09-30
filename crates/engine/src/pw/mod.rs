//! PipeWire integration.
//!
//! * [`engine`]: what the daemon drives the engine with, and the commands
//!   and events between them.
//! * [`runner`]: the thread that owns the PipeWire main loop and keeps
//!   PipeWire in step with the mixer state: the engine node's ports, the
//!   real-time snapshot, Weir's virtual devices, the links, and the
//!   application streams.
//! * [`filter`]: a thin wrapper over `pw_filter`, the single engine node
//!   whose process callback runs the DSP core.
//! * [`graph`]: a mirror of the PipeWire registry: nodes, ports and links.
//! * [`volume`]: reading and writing a node's own volume.

mod engine;
mod filter;
mod graph;
mod runner;
mod volume;

pub use engine::{Engine, EngineError, EngineEvent, EngineHandle, EngineOptions};

/// `node.name` of the engine filter node.
pub const ENGINE_NODE_NAME: &str = "Weir";
/// Prefix of every virtual device node we create.
pub const VIRTUAL_PREFIX: &str = "weir.";

/// `node.name` of a virtual strip's playback device.
pub fn virtual_input_node_name(strip: weir_protocol::StripId) -> String {
    format!("{VIRTUAL_PREFIX}input.{strip}")
}

/// `node.name` of a virtual bus's microphone.
pub fn virtual_output_node_name(bus: weir_protocol::BusId) -> String {
    format!("{VIRTUAL_PREFIX}output.{bus}")
}

/// Parse a strip id out of a virtual input node name.
pub fn strip_of_virtual_input(node_name: &str) -> Option<weir_protocol::StripId> {
    node_name
        .strip_prefix(VIRTUAL_PREFIX)?
        .strip_prefix("input.")?
        .parse()
        .ok()
}

/// Parse a bus id out of a virtual output node name.
pub fn bus_of_virtual_output(node_name: &str) -> Option<weir_protocol::BusId> {
    node_name
        .strip_prefix(VIRTUAL_PREFIX)?
        .strip_prefix("output.")?
        .parse()
        .ok()
}

/// True for nodes that belong to this program (engine or virtual devices).
pub fn is_our_node(node_name: &str) -> bool {
    node_name == ENGINE_NODE_NAME || node_name.starts_with(VIRTUAL_PREFIX)
}
