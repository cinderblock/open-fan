//! The OpenFan graph: model, validation and evaluation.
//!
//! This crate is deliberately inert. It performs no I/O, spawns no threads, reads no
//! clock and has no async. Sensor values are handed to it, a tick is evaluated, and
//! commanded duties come back out. Everything about the control behaviour is therefore
//! testable with no hardware, no timing and no flakiness — which matters a great deal
//! for code whose failure mode is a cooked CPU.
//!
//! The pipeline is: [`Graph`] → [`Graph::validate`] → [`CompiledGraph`] → [`CompiledGraph::tick`].
//! Validation is where every structural and type error is caught, so `tick` can be
//! total: once a graph compiles, evaluating it cannot fail.

#![forbid(unsafe_code)]

pub mod infer;
pub mod node;

use std::collections::{BTreeMap, BTreeSet};

use of_units::{Quantity, Value};
use serde::{Deserialize, Serialize};

pub use infer::PortTypes;
pub use node::{Compare, CurvePoint, MixMode, NodeKind, NodeSpec, PortSpec, PortType, TickInput};

/// Identifier for a node instance. Strings, because they round-trip to the editor and
/// survive being hand-edited in a saved profile.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../../ui/src/bindings/")
)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl From<&str> for NodeId {
    fn from(s: &str) -> Self {
        NodeId(s.to_owned())
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifies one port on one node. Port keys are stable strings owned by the node kind.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../../ui/src/bindings/")
)]
pub struct PortRef {
    pub node: NodeId,
    pub port: String,
}

impl PortRef {
    pub fn new(node: impl Into<NodeId>, port: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            port: port.into(),
        }
    }
}

impl std::fmt::Display for PortRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.node, self.port)
    }
}

/// A typed connection from one node's output to another node's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../../ui/src/bindings/")
)]
pub struct Edge {
    pub from: PortRef,
    pub to: PortRef,
}

/// A node instance: its kind (which carries the node's parameters) plus editor metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../../ui/src/bindings/")
)]
pub struct NodeInstance {
    pub kind: NodeKind,
    /// User-visible name. Falls back to the kind's default label when empty.
    #[serde(default)]
    pub label: String,
    /// Canvas position. Carried through so the backend owns the whole document and the
    /// UI stays a pure view of it.
    #[serde(default)]
    pub position: (f32, f32),
}

impl NodeInstance {
    pub fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            label: String::new(),
            position: (0.0, 0.0),
        }
    }

    pub fn display_name(&self) -> &str {
        if self.label.is_empty() {
            self.kind.default_label()
        } else {
            &self.label
        }
    }
}

/// An editable fan-control graph. May be invalid; call [`Graph::validate`] to find out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../../ui/src/bindings/")
)]
pub struct Graph {
    pub nodes: BTreeMap<NodeId, NodeInstance>,
    pub edges: Vec<Edge>,
}

/// Everything that can be wrong with a graph.
///
/// These are reported as a batch rather than bailing on the first one, so the editor can
/// mark every bad node at once instead of making the user fix errors one at a time.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GraphError {
    #[error("edge references unknown node {0}")]
    UnknownNode(NodeId),

    #[error("node {0} has no port named {1:?}")]
    UnknownPort(NodeId, String),

    #[error("{edge_from} is an input, not an output")]
    NotAnOutput { edge_from: PortRef },

    #[error("{edge_to} is an output, not an input")]
    NotAnInput { edge_to: PortRef },

    #[error("{from} ({source_ty}) cannot drive {to} ({sink_ty}): insert a conversion node")]
    TypeMismatch {
        from: PortRef,
        to: PortRef,
        source_ty: Quantity,
        sink_ty: Quantity,
    },

    #[error("input {0} has more than one incoming connection")]
    InputOverSubscribed(PortRef),

    #[error("required input {0} is not connected")]
    MissingInput(PortRef),

    #[error(
        "node {node} would have to carry both {a} and {b}: its connections disagree about          the type flowing through it"
    )]
    TypeConflict {
        node: NodeId,
        a: Quantity,
        b: Quantity,
    },

    #[error("the graph contains a cycle through node {0}; use a delay or filter node instead")]
    Cycle(NodeId),
}

/// Result of validating a graph: either a compiled, evaluable form or every error found.
pub type ValidationResult = Result<CompiledGraph, Vec<GraphError>>;

impl Graph {
    pub fn insert(&mut self, id: impl Into<NodeId>, kind: NodeKind) -> NodeId {
        let id = id.into();
        self.nodes.insert(id.clone(), NodeInstance::new(kind));
        id
    }

    pub fn connect(&mut self, from: PortRef, to: PortRef) {
        self.edges.push(Edge { from, to });
    }

    /// Every hardware channel this graph drives.
    ///
    /// The engine acquires exactly this set, so a channel can never end up held without
    /// a node responsible for it — or driven by a node the engine never acquired.
    pub fn output_channels(&self) -> BTreeSet<String> {
        self.nodes
            .values()
            .filter_map(|n| match &n.kind {
                NodeKind::FanOutput { channel } => Some(channel.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every sensor this graph reads, with the quantity each is declared as.
    pub fn required_sensors(&self) -> BTreeMap<String, Quantity> {
        self.nodes
            .values()
            .filter_map(|n| match &n.kind {
                NodeKind::Sensor {
                    sensor_id,
                    quantity,
                } => Some((sensor_id.clone(), *quantity)),
                _ => None,
            })
            .collect()
    }

    /// Whether this port is an output whose value predates the current tick.
    pub fn is_delayed_source(&self, r: &PortRef) -> bool {
        self.nodes
            .get(&r.node)
            .map(|n| n.kind.spec())
            .and_then(|spec| {
                spec.outputs
                    .iter()
                    .find(|p| p.key == r.port)
                    .map(|p| p.delayed)
            })
            .unwrap_or(false)
    }

    /// Look up the declared type and direction of a port, if it exists.
    fn port_spec(&self, r: &PortRef) -> Option<(PortSpec, Direction)> {
        let node = self.nodes.get(&r.node)?;
        let spec = node.kind.spec();
        if let Some(p) = spec.inputs.iter().find(|p| p.key == r.port) {
            return Some((p.clone(), Direction::In));
        }
        if let Some(p) = spec.outputs.iter().find(|p| p.key == r.port) {
            return Some((p.clone(), Direction::Out));
        }
        None
    }

    /// Check structure, types and acyclicity, producing an evaluable graph.
    ///
    /// Collects *all* errors rather than stopping at the first.
    pub fn validate(&self) -> ValidationResult {
        let mut errors = Vec::new();

        // Which input ports already have a producer, so we can catch double-driving and
        // missing-required in one pass.
        let mut driven: BTreeMap<PortRef, PortRef> = BTreeMap::new();

        for edge in &self.edges {
            for r in [&edge.from, &edge.to] {
                if !self.nodes.contains_key(&r.node) {
                    errors.push(GraphError::UnknownNode(r.node.clone()));
                }
            }
            if !self.nodes.contains_key(&edge.from.node) || !self.nodes.contains_key(&edge.to.node)
            {
                continue;
            }

            let Some((_, from_dir)) = self.port_spec(&edge.from) else {
                errors.push(GraphError::UnknownPort(
                    edge.from.node.clone(),
                    edge.from.port.clone(),
                ));
                continue;
            };
            let Some((to_spec, to_dir)) = self.port_spec(&edge.to) else {
                errors.push(GraphError::UnknownPort(
                    edge.to.node.clone(),
                    edge.to.port.clone(),
                ));
                continue;
            };

            if from_dir != Direction::Out {
                errors.push(GraphError::NotAnOutput {
                    edge_from: edge.from.clone(),
                });
                continue;
            }
            if to_dir != Direction::In {
                errors.push(GraphError::NotAnInput {
                    edge_to: edge.to.clone(),
                });
                continue;
            }

            // A variadic input accepts many producers; a plain input accepts exactly one.
            if !to_spec.variadic && driven.insert(edge.to.clone(), edge.from.clone()).is_some() {
                errors.push(GraphError::InputOverSubscribed(edge.to.clone()));
            }
        }

        for (id, node) in &self.nodes {
            for port in &node.kind.spec().inputs {
                if !port.required {
                    continue;
                }
                let r = PortRef::new(id.clone(), port.key);
                let connected = driven.contains_key(&r) || self.edges.iter().any(|e| e.to == r);
                if !connected {
                    errors.push(GraphError::MissingInput(r));
                }
            }
        }

        let (types, type_errors) = infer::infer(self);
        errors.extend(type_errors);

        match self.topological_order() {
            Ok(order) => {
                if errors.is_empty() {
                    Ok(CompiledGraph {
                        graph: self.clone(),
                        order,
                        types,
                    })
                } else {
                    Err(errors)
                }
            }
            Err(node) => {
                errors.push(GraphError::Cycle(node));
                Err(errors)
            }
        }
    }

    /// Topologically sort nodes, or name a node participating in a cycle.
    fn topological_order(&self) -> Result<Vec<NodeId>, NodeId> {
        use petgraph::graph::DiGraph;

        let mut g = DiGraph::<NodeId, ()>::new();
        let mut index = BTreeMap::new();
        for id in self.nodes.keys() {
            index.insert(id.clone(), g.add_node(id.clone()));
        }
        for edge in &self.edges {
            // An edge leaving a delayed port carries a value from before this tick, so
            // it imposes no ordering. Skipping it here is what allows genuine feedback —
            // a tachometer steering the fan it measures — without the graph being a
            // cycle. The delay is physical, not a modelling trick.
            if self.is_delayed_source(&edge.from) {
                continue;
            }
            if let (Some(&a), Some(&b)) = (index.get(&edge.from.node), index.get(&edge.to.node)) {
                // Self-edges are cycles too; petgraph's toposort catches them.
                g.add_edge(a, b, ());
            }
        }

        petgraph::algo::toposort(&g, None)
            .map(|order| order.into_iter().map(|ix| g[ix].clone()).collect())
            .map_err(|cycle| g[cycle.node_id()].clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    In,
    Out,
}

/// A validated graph plus its evaluation order. Constructing one is proof that the graph
/// is structurally sound, correctly typed and acyclic.
#[derive(Debug, Clone)]
pub struct CompiledGraph {
    graph: Graph,
    order: Vec<NodeId>,
    types: PortTypes,
}

/// Sensor readings for one tick, keyed by the sensor id a [`NodeKind::Sensor`] names.
pub type SensorReadings = BTreeMap<String, Value>;

/// Commanded outputs for one tick, keyed by the channel id a [`NodeKind::FanOutput`] names.
pub type Commands = BTreeMap<String, Value>;

/// Per-tick evaluation output.
#[derive(Debug, Clone, Default)]
pub struct TickResult {
    /// What each output channel should be driven to.
    pub commands: Commands,
    /// Every value on every wire, for the UI's live readouts and sparklines.
    pub wire_values: BTreeMap<PortRef, Value>,
    /// Sinks whose input was missing or untrustworthy this tick. The engine must apply
    /// the channel's failsafe for each of these rather than leaving it at its last value.
    pub faulted_channels: BTreeSet<String>,
}

impl CompiledGraph {
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn order(&self) -> &[NodeId] {
        &self.order
    }

    /// The inferred type of every port. `None` means still generic.
    pub fn types(&self) -> &PortTypes {
        &self.types
    }

    /// Evaluate one tick against sensor readings and an elapsed time.
    ///
    /// Convenience wrapper over [`CompiledGraph::tick`] for the common case.
    pub fn tick_with(
        &self,
        sensors: &SensorReadings,
        dt: f64,
        state: &mut EvalState,
    ) -> TickResult {
        self.tick(&TickInput::new(sensors, dt), state)
    }

    /// Evaluate one tick.
    ///
    /// Total by construction: a compiled graph always produces a result. Individual
    /// channels can still *fault* (see [`TickResult::faulted_channels`]) when a sensor
    /// reads NaN or a node produces a non-finite value; that is reported, never panicked.
    pub fn tick(&self, ctx: &TickInput<'_>, state: &mut EvalState) -> TickResult {
        let mut result = TickResult::default();
        // Values produced by each output port this tick.
        let mut outputs: BTreeMap<PortRef, Value> = BTreeMap::new();

        // Phase one: delayed outputs. These depend on measurements and stored state
        // rather than on anything computed below, so they must be available before the
        // topological pass begins — that is precisely what makes feedback expressible.
        for id in self.graph.nodes.keys() {
            let Some(instance) = self.graph.nodes.get(id) else {
                continue;
            };
            let out_type = self
                .types
                .get(&PortRef::new(id.clone(), "out"))
                .copied()
                .flatten();
            let state = state.entry(id.clone());
            for (key, value) in instance.kind.sourced(ctx, state, out_type) {
                let r = PortRef::new(id.clone(), key);
                outputs.insert(r.clone(), value);
                result.wire_values.insert(r, value);
            }
        }

        // Phase two: everything else, in dependency order.
        for id in &self.order {
            let Some(instance) = self.graph.nodes.get(id) else {
                continue;
            };

            // Gather inputs by following edges backwards. Variadic ports collect many.
            let mut inputs: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
            for port in &instance.kind.spec().inputs {
                let target = PortRef::new(id.clone(), port.key);
                let values: Vec<Value> = self
                    .graph
                    .edges
                    .iter()
                    .filter(|e| e.to == target)
                    .filter_map(|e| outputs.get(&e.from).copied())
                    .collect();
                inputs.insert(port.key, values);
            }

            let node_state = state.entry(id.clone());
            // Nodes with no input take their output type from inference; the rest
            // carry their input's type through.
            let out_type = self
                .types
                .get(&PortRef::new(id.clone(), "out"))
                .copied()
                .flatten();
            let produced = instance.kind.eval(&inputs, ctx, node_state, out_type);

            for (key, value) in produced.outputs {
                let r = PortRef::new(id.clone(), key);
                outputs.insert(r.clone(), value);
                result.wire_values.insert(r, value);
            }
            for (channel, value) in produced.commands {
                if value.is_trustworthy() {
                    result.commands.insert(channel, value);
                } else {
                    result.faulted_channels.insert(channel);
                }
            }
            result.faulted_channels.extend(produced.faults);
        }

        result
    }
}

/// Mutable per-node state that persists across ticks (filters, rate limiters, latches).
///
/// Held outside the graph so the graph itself stays a pure value that can be cloned,
/// serialized and diffed without dragging runtime state along.
#[derive(Debug, Clone, Default)]
pub struct EvalState {
    nodes: BTreeMap<NodeId, node::NodeState>,
}

impl EvalState {
    pub fn new() -> Self {
        Self::default()
    }

    fn entry(&mut self, id: NodeId) -> &mut node::NodeState {
        self.nodes.entry(id).or_default()
    }

    /// Drop state for nodes that no longer exist, so a long-running session does not
    /// accumulate stale entries as the user edits the graph.
    pub fn retain_nodes(&mut self, graph: &Graph) {
        self.nodes.retain(|id, _| graph.nodes.contains_key(id));
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod node_tests;

#[cfg(test)]
mod infer_tests;

#[cfg(test)]
mod feedback_tests;
