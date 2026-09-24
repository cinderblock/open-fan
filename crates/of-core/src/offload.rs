//! Can this channel run in the chip, and if not, exactly why?
//!
//! A Super I/O can run a fan curve by itself: one temperature, a few points, no software
//! involved. That is what a BIOS fan curve is. It is a genuinely better place for a
//! configuration to live when it fits — it keeps working if this program dies — so the
//! product's job is to *offer* it, not to assume the engine is the answer.
//!
//! This module decides whether a channel's configuration fits, and when it does not, says
//! which node stopped it. **That explanation is the feature.** "Your fan follows GPU
//! temperature, which the motherboard chip cannot read" tells somebody something true
//! about their machine; "not supported" tells them nothing.
//!
//! # Pure, and deliberately hardware-ignorant
//!
//! This answers only the question the *graph* can answer: does this channel reduce to a
//! constant duty, or to a curve on a single sensor? Whether a particular chip can then
//! read that sensor, or hold that many points, is a hardware question and belongs to the
//! backend. Keeping the split here means the whole analysis is testable without a chip,
//! which is where the subtle mistakes are.
//!
//! So a `Reduction` is a *candidate*, not a promise.
//!
//! # Folding
//!
//! Some nodes disappear into the curve rather than blocking it. A `Clamp` after a curve is
//! the same as clamping its points; `Offset` and `Scale` likewise shift and stretch them.
//! Folding those matters in practice because they are exactly what an imported
//! configuration puts there — a minimum-duty floor becomes a `Clamp`, and refusing to
//! offload over it would be a needless "no".

use std::collections::BTreeMap;

use of_units::Quantity;

use crate::{CurvePoint, Graph, NodeId, NodeKind, PortRef};

/// What a channel's configuration reduces to, when it reduces at all.
#[derive(Debug, Clone, PartialEq)]
pub enum Reduction {
    /// A single duty, held forever.
    Fixed { duty: f64 },
    /// A piecewise transfer from one sensor to a duty.
    Curve {
        sensor_id: String,
        /// What the sensor reads. A chip curve needs a temperature; anything else is a
        /// hardware question, which is why it is reported rather than judged here.
        quantity: Quantity,
        points: Vec<CurvePoint>,
    },
}

/// Why a channel cannot be reduced to something a chip could run.
#[derive(Debug, Clone, PartialEq)]
pub struct Obstacle {
    /// The node responsible, so an interface can point at it.
    pub node: Option<NodeId>,
    pub kind: ObstacleKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ObstacleKind {
    /// No output node names this channel.
    NotDriven,
    /// Two outputs name it, so there is no single answer.
    DrivenTwice,
    /// An output exists but nothing is connected to it.
    NothingConnected,
    /// A node with no chip equivalent.
    ///
    /// `chip_alternative` names a chip setting that is *similar but not the same*, when one
    /// exists. That distinction is worth surfacing: it turns "you cannot" into "you could,
    /// by giving this up", which is a choice rather than a refusal.
    Unsupported {
        node_kind: &'static str,
        chip_alternative: Option<&'static str>,
    },
    /// More than one thing feeds a combining node, and a chip curve follows one input.
    CombinesInputs { node_kind: &'static str },
    /// A curve's input is not a plain sensor reading.
    CurveInputIsNotASensor { found: &'static str },
}

impl Obstacle {
    /// A sentence for somebody who did not write this graph.
    pub fn explain(&self) -> String {
        match &self.kind {
            ObstacleKind::NotDriven => "Nothing in this configuration drives this fan.".into(),
            ObstacleKind::DrivenTwice => {
                "Two outputs drive this fan, so there is no single curve to hand over.".into()
            }
            ObstacleKind::NothingConnected => {
                "This fan's output has nothing connected to it yet.".into()
            }
            ObstacleKind::Unsupported {
                node_kind,
                chip_alternative: Some(alternative),
            } => format!(
                "The \"{node_kind}\" step has no equivalent in the motherboard chip. The \
                 chip has its own {alternative}, which is similar but not the same — \
                 removing this step would let this fan run in the chip."
            ),
            ObstacleKind::Unsupported {
                node_kind,
                chip_alternative: None,
            } => format!(
                "The \"{node_kind}\" step can only run in software; the motherboard chip \
                 has nothing like it."
            ),
            ObstacleKind::CombinesInputs { node_kind } => format!(
                "The \"{node_kind}\" step combines several inputs. The motherboard chip \
                 follows one temperature per fan, so this has to stay in software."
            ),
            ObstacleKind::CurveInputIsNotASensor { found } => format!(
                "This curve follows a \"{found}\" step rather than a sensor directly. The \
                 motherboard chip can only follow a sensor it reads itself."
            ),
        }
    }
}

/// What every channel in a graph could do, keyed by channel id.
pub fn reduce_all(graph: &Graph) -> BTreeMap<String, Result<Reduction, Vec<Obstacle>>> {
    graph
        .output_channels()
        .into_iter()
        .map(|channel| {
            let verdict = reduce(graph, &channel);
            (channel, verdict)
        })
        .collect()
}

/// Reduce one channel's configuration, or explain what stopped it.
///
/// Every obstacle found is returned, not just the first: somebody deciding whether to
/// simplify a configuration needs the whole list, and a one-at-a-time interface would
/// make them discover it by repetition.
pub fn reduce(graph: &Graph, channel: &str) -> Result<Reduction, Vec<Obstacle>> {
    let outputs: Vec<&NodeId> = graph
        .nodes
        .iter()
        .filter(|(_, n)| matches!(&n.kind, NodeKind::FanOutput { channel: c } if c == channel))
        .map(|(id, _)| id)
        .collect();

    let output = match outputs.as_slice() {
        [] => {
            return Err(vec![Obstacle {
                node: None,
                kind: ObstacleKind::NotDriven,
            }]);
        }
        [single] => (*single).clone(),
        _ => {
            return Err(vec![Obstacle {
                node: None,
                kind: ObstacleKind::DrivenTwice,
            }]);
        }
    };

    let Some(source) = producer_of(graph, &PortRef::new(output.clone(), "duty")) else {
        return Err(vec![Obstacle {
            node: Some(output),
            kind: ObstacleKind::NothingConnected,
        }]);
    };

    let mut obstacles = Vec::new();
    match fold_duty(graph, &source, &mut obstacles) {
        Some(reduction) if obstacles.is_empty() => Ok(reduction),
        // A partial reduction with obstacles is still a failure: reporting a curve that
        // ignores a filter would describe a fan that behaves differently from the one the
        // user configured.
        _ => Err(obstacles),
    }
}

/// The output port feeding an input port, if anything does.
fn producer_of(graph: &Graph, input: &PortRef) -> Option<PortRef> {
    graph
        .edges
        .iter()
        .find(|e| &e.to == input)
        .map(|e| e.from.clone())
}

/// Walk back along the duty path, folding what folds and recording what does not.
fn fold_duty(graph: &Graph, port: &PortRef, obstacles: &mut Vec<Obstacle>) -> Option<Reduction> {
    let node = graph.nodes.get(&port.node)?;

    match &node.kind {
        NodeKind::Constant { value } => Some(Reduction::Fixed { duty: *value }),

        NodeKind::Curve { points } => {
            let sensor = curve_sensor(graph, &port.node, obstacles)?;
            Some(Reduction::Curve {
                sensor_id: sensor.0,
                quantity: sensor.1,
                points: points.clone(),
            })
        }

        // Folds into whatever it is applied to.
        NodeKind::Clamp { min, max } => {
            let inner = fold_input(graph, &port.node, obstacles)?;
            Some(map_duty(inner, |d| d.clamp(*min, *max)))
        }
        NodeKind::Offset { delta } => {
            let inner = fold_input(graph, &port.node, obstacles)?;
            Some(map_duty(inner, |d| d + *delta))
        }
        NodeKind::Scale { factor } => {
            let inner = fold_input(graph, &port.node, obstacles)?;
            Some(map_duty(inner, |d| d * *factor))
        }

        NodeKind::Mix { .. } => {
            obstacles.push(Obstacle {
                node: Some(port.node.clone()),
                kind: ObstacleKind::CombinesInputs {
                    node_kind: node.kind.default_label(),
                },
            });
            None
        }

        other => {
            obstacles.push(Obstacle {
                node: Some(port.node.clone()),
                kind: ObstacleKind::Unsupported {
                    node_kind: other.default_label(),
                    chip_alternative: chip_alternative(other),
                },
            });
            None
        }
    }
}

/// Fold whatever feeds a single-input node's `in` port.
fn fold_input(graph: &Graph, node: &NodeId, obstacles: &mut Vec<Obstacle>) -> Option<Reduction> {
    let source = producer_of(graph, &PortRef::new(node.clone(), "in"))?;
    fold_duty(graph, &source, obstacles)
}

/// Apply a transform to whatever a reduction produces.
fn map_duty(reduction: Reduction, f: impl Fn(f64) -> f64) -> Reduction {
    match reduction {
        Reduction::Fixed { duty } => Reduction::Fixed { duty: f(duty) },
        Reduction::Curve {
            sensor_id,
            quantity,
            points,
        } => Reduction::Curve {
            sensor_id,
            quantity,
            points: points
                .into_iter()
                .map(|p| CurvePoint { x: p.x, y: f(p.y) })
                .collect(),
        },
    }
}

/// The sensor a curve follows, which must be a sensor and nothing else.
fn curve_sensor(
    graph: &Graph,
    curve: &NodeId,
    obstacles: &mut Vec<Obstacle>,
) -> Option<(String, Quantity)> {
    let source = producer_of(graph, &PortRef::new(curve.clone(), "in"))?;
    let node = graph.nodes.get(&source.node)?;

    match &node.kind {
        NodeKind::Sensor {
            sensor_id,
            quantity,
        } => Some((sensor_id.clone(), *quantity)),
        other => {
            obstacles.push(Obstacle {
                node: Some(source.node.clone()),
                kind: match other {
                    NodeKind::Mix { .. } => ObstacleKind::CombinesInputs {
                        node_kind: other.default_label(),
                    },
                    // A filter on the *temperature* side still blocks the handover, but it
                    // is one the chip has a rough equivalent for — and which side of the
                    // curve it sits on does not change that. Saying "this is not a sensor"
                    // here would hide the useful half of the answer.
                    _ if chip_alternative(other).is_some() => ObstacleKind::Unsupported {
                        node_kind: other.default_label(),
                        chip_alternative: chip_alternative(other),
                    },
                    // Everything else genuinely is "the curve follows something that is
                    // not a reading", which is a different problem with a different fix.
                    _ => ObstacleKind::CurveInputIsNotASensor {
                        found: other.default_label(),
                    },
                },
            });
            None
        }
    }
}

/// A chip setting that resembles a node we cannot offload.
///
/// Only where the resemblance is real. Claiming an equivalent that behaves differently
/// would be worse than admitting there is none — somebody would drop a node believing the
/// chip covers it.
fn chip_alternative(kind: &NodeKind) -> Option<&'static str> {
    match kind {
        NodeKind::Hold { .. } => Some("hysteresis setting"),
        NodeKind::LowPass { .. } | NodeKind::RateLimit { .. } | NodeKind::MovingAverage { .. } => {
            Some("step-up and step-down timing")
        }
        _ => None,
    }
}
