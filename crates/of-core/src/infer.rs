//! Type inference over the graph.
//!
//! Generic nodes declare their ports as type variables. Inference decides what each
//! variable actually is by unifying across connections, anchored by the concrete types
//! at the edges of the graph — a sensor reads a temperature, a fan takes a duty.
//!
//! The algorithm is plain union-find over variables, with at most one concrete binding
//! per equivalence class:
//!
//! - **concrete ↔ concrete** — must be equal, or the connection is a type error.
//! - **variable ↔ concrete** — binds the variable's whole class to that quantity, and
//!   conflicts with an existing binding are reported against the node.
//! - **variable ↔ variable** — merges the two classes, carrying any binding across.
//!
//! A class that ends with no binding is genuinely unconstrained. That is **not** an
//! error: a chain of generic nodes with nothing concrete attached yet is a perfectly
//! normal half-built graph, and the editor draws it in neutral white until a connection
//! decides it.
//!
//! Variables are scoped per node, so two Clamps each using `T` are independent until
//! something connects them.

use std::collections::BTreeMap;

use of_units::Quantity;

use crate::{Graph, GraphError, NodeId, PortRef, PortType};

/// A type variable, scoped to the node that declares it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct VarKey {
    node: NodeId,
    name: &'static str,
}

/// Union-find over type variables, with an optional concrete binding per class.
#[derive(Default)]
struct Unifier {
    parent: BTreeMap<VarKey, VarKey>,
    binding: BTreeMap<VarKey, Quantity>,
}

impl Unifier {
    fn root(&mut self, key: &VarKey) -> VarKey {
        let mut current = key.clone();
        loop {
            match self.parent.get(&current) {
                Some(next) if *next != current => {
                    let next = next.clone();
                    // Path compression keeps repeated lookups cheap on long chains of
                    // pass-through nodes, which is exactly the shape a fan graph has.
                    let grandparent = self.parent.get(&next).cloned().unwrap_or(next.clone());
                    self.parent.insert(current.clone(), grandparent);
                    current = next;
                }
                _ => return current,
            }
        }
    }

    fn ensure(&mut self, key: &VarKey) {
        self.parent
            .entry(key.clone())
            .or_insert_with(|| key.clone());
    }

    fn binding_of(&mut self, key: &VarKey) -> Option<Quantity> {
        let root = self.root(key);
        self.binding.get(&root).copied()
    }

    /// Bind a variable's class to a concrete quantity.
    ///
    /// Returns the conflicting pair when the class already resolved to something else.
    fn bind(&mut self, key: &VarKey, q: Quantity) -> Result<(), (Quantity, Quantity)> {
        self.ensure(key);
        let root = self.root(key);
        match self.binding.get(&root) {
            Some(existing) if *existing != q => Err((*existing, q)),
            Some(_) => Ok(()),
            None => {
                self.binding.insert(root, q);
                Ok(())
            }
        }
    }

    /// Merge two variables' classes.
    fn union(&mut self, a: &VarKey, b: &VarKey) -> Result<(), (Quantity, Quantity)> {
        self.ensure(a);
        self.ensure(b);
        let (ra, rb) = (self.root(a), self.root(b));
        if ra == rb {
            return Ok(());
        }

        match (
            self.binding.get(&ra).copied(),
            self.binding.get(&rb).copied(),
        ) {
            (Some(x), Some(y)) if x != y => return Err((x, y)),
            _ => {}
        }
        let carried = self
            .binding
            .get(&ra)
            .copied()
            .or_else(|| self.binding.get(&rb).copied());

        self.parent.insert(rb.clone(), ra.clone());
        self.binding.remove(&rb);
        if let Some(q) = carried {
            self.binding.insert(ra, q);
        }
        Ok(())
    }
}

/// The resolved type of every port in the graph.
///
/// `None` means the port is generic and nothing has decided it yet.
pub type PortTypes = BTreeMap<PortRef, Option<Quantity>>;

/// Infer every port's type, reporting connections and nodes whose types cannot agree.
///
/// Structural problems (unknown nodes, wrong-direction edges) are assumed already
/// checked; edges referring to ports that do not exist are skipped rather than reported
/// twice.
pub fn infer(graph: &Graph) -> (PortTypes, Vec<GraphError>) {
    let mut unifier = Unifier::default();
    let mut errors = Vec::new();

    // Seed: every concrete port pins its own type; every variable gets a class.
    for (id, node) in &graph.nodes {
        let spec = node.kind.spec();
        for port in spec.inputs.iter().chain(spec.outputs.iter()) {
            if let PortType::Var(name) = port.ty {
                unifier.ensure(&VarKey {
                    node: id.clone(),
                    name,
                });
            }
        }
    }

    for edge in &graph.edges {
        let Some(from) = port_type(graph, &edge.from) else {
            continue;
        };
        let Some(to) = port_type(graph, &edge.to) else {
            continue;
        };

        match (from, to) {
            (PortType::Concrete(a), PortType::Concrete(b)) => {
                if a != b {
                    errors.push(GraphError::TypeMismatch {
                        from: edge.from.clone(),
                        to: edge.to.clone(),
                        source_ty: a,
                        sink_ty: b,
                    });
                }
            }
            (PortType::Concrete(a), PortType::Var(name)) => {
                let key = VarKey {
                    node: edge.to.node.clone(),
                    name,
                };
                if let Err((existing, new)) = unifier.bind(&key, a) {
                    errors.push(conflict(&edge.to.node, existing, new));
                }
            }
            (PortType::Var(name), PortType::Concrete(b)) => {
                let key = VarKey {
                    node: edge.from.node.clone(),
                    name,
                };
                if let Err((existing, new)) = unifier.bind(&key, b) {
                    errors.push(conflict(&edge.from.node, existing, new));
                }
            }
            (PortType::Var(a), PortType::Var(b)) => {
                let ka = VarKey {
                    node: edge.from.node.clone(),
                    name: a,
                };
                let kb = VarKey {
                    node: edge.to.node.clone(),
                    name: b,
                };
                if let Err((x, y)) = unifier.union(&ka, &kb) {
                    errors.push(conflict(&edge.to.node, x, y));
                }
            }
        }
    }

    // Read the answer back out for every port.
    let mut types = PortTypes::new();
    for (id, node) in &graph.nodes {
        let spec = node.kind.spec();
        for port in spec.inputs.iter().chain(spec.outputs.iter()) {
            let resolved = match port.ty {
                PortType::Concrete(q) => Some(q),
                PortType::Var(name) => unifier.binding_of(&VarKey {
                    node: id.clone(),
                    name,
                }),
            };
            types.insert(PortRef::new(id.clone(), port.key), resolved);
        }
    }

    (types, errors)
}

fn conflict(node: &NodeId, a: Quantity, b: Quantity) -> GraphError {
    GraphError::TypeConflict {
        node: node.clone(),
        a,
        b,
    }
}

/// The declared type of a port, if the port exists.
fn port_type(graph: &Graph, r: &PortRef) -> Option<PortType> {
    let node = graph.nodes.get(&r.node)?;
    let spec = node.kind.spec();
    spec.inputs
        .iter()
        .chain(spec.outputs.iter())
        .find(|p| p.key == r.port)
        .map(|p| p.ty)
}
