//! The commands the editor can answer without leaving this process.
//!
//! Only two things qualify, and what they have in common is that neither touches hardware
//! or the running configuration: the node catalogue is a constant, and type inference is
//! a pure function over a candidate graph the user is still editing.
//!
//! Everything else — sensors, channels, the installed graph, snapshots, contention,
//! takeover — lives in the OpenFan service and is reached through [`crate::client`]. The
//! window is a client of the thing that controls the fans, not the thing itself.
//!
//! Keeping these two here is not an exception to that. Sending an unfinished graph over a
//! pipe on every keystroke, so a service could tell the editor what colour to draw a port,
//! would add a round trip and a failure mode to something that has neither.

use of_ipc::{Graph, NodeDescriptor, PortTypeDto, catalogue};

/// Every node kind the editor may offer, with its ports and parameters.
#[tauri::command]
pub fn node_catalogue() -> Vec<NodeDescriptor> {
    catalogue()
}

/// Infer the type of every port in a candidate graph.
///
/// Called on each structural edit so a generic port can lock to a colour the moment a
/// connection decides it. The graph passed here is whatever is on the canvas, valid or
/// not — it is explicitly *not* installed by this call, and nothing about fan control
/// depends on the answer.
#[tauri::command]
pub fn resolve_types(graph: Graph) -> Vec<PortTypeDto> {
    of_ipc::resolve_types(&graph)
}
