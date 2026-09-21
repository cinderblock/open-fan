//! The node catalogue.
//!
//! Each variant of [`NodeKind`] declares its typed ports via [`NodeKind::spec`] and its
//! behaviour via [`NodeKind::eval`]. Adding a node means adding a variant and both
//! methods — the compiler will not let you forget either.
//!
//! This is a starter set covering one representative node from each family (source,
//! transform, stateful transform, sink), enough to exercise validation and evaluation
//! end to end. The full catalogue named in `plans/open-fan.md` lands in Phase 2.

use std::collections::BTreeMap;

use of_units::{Quantity, Value};
use serde::{Deserialize, Serialize};

use crate::SensorReadings;

/// One typed port on a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub quantity: Quantity,
    /// The graph is invalid if a required input is left unconnected. Optional inputs
    /// fall back to a node-defined default.
    pub required: bool,
    /// A variadic input accepts any number of incoming connections (a mixer's inputs).
    pub variadic: bool,
}

impl PortSpec {
    pub const fn input(key: &'static str, label: &'static str, quantity: Quantity) -> Self {
        Self { key, label, quantity, required: true, variadic: false }
    }

    pub const fn output(key: &'static str, label: &'static str, quantity: Quantity) -> Self {
        Self { key, label, quantity, required: false, variadic: false }
    }

    pub const fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    pub const fn variadic(mut self) -> Self {
        self.variadic = true;
        self
    }
}

/// A node's full port signature.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeSpec {
    pub inputs: Vec<PortSpec>,
    pub outputs: Vec<PortSpec>,
}

/// How a mixer combines its inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MixMode {
    /// The safe default for cooling: the hottest input wins.
    Max,
    Min,
    Average,
    Sum,
}

/// One point on a transfer curve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub x: f64,
    pub y: f64,
}

/// What a node produced during one tick.
#[derive(Debug, Clone, Default)]
pub struct Produced {
    /// Values on this node's output ports.
    pub outputs: Vec<(&'static str, Value)>,
    /// Hardware channel commands, for sink nodes.
    pub commands: Vec<(String, Value)>,
    /// Channels this node could not command safely this tick.
    pub faults: Vec<String>,
}

/// Per-node state carried between ticks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeState {
    /// Last emitted value, for rate limiters and filters.
    pub last: Option<f64>,
}

/// A node's kind and its parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NodeKind {
    /// Reads a hardware sensor by id.
    Sensor { sensor_id: String, quantity: Quantity },

    /// Emits a fixed value. Also the backing node for a manual slider.
    Constant { quantity: Quantity, value: f64 },

    /// Maps an input quantity onto a duty via a piecewise-linear transfer curve.
    ///
    /// This is the node that makes `Temperature → Duty` legal, and the reason that
    /// conversion cannot happen by accident anywhere else.
    Curve { input: Quantity, points: Vec<CurvePoint> },

    /// Combines any number of same-quantity inputs into one.
    Mix { quantity: Quantity, mode: MixMode },

    /// Constrains a value to a range.
    Clamp { quantity: Quantity, min: f64, max: f64 },

    /// Limits how fast a value may change per tick. The primary tool against the
    /// audible hunting that an aggressive curve produces on a slow thermal mass.
    RateLimit { quantity: Quantity, max_delta_per_tick: f64 },

    /// Drives a hardware output channel.
    FanOutput { channel: String },
}

impl NodeKind {
    pub fn default_label(&self) -> &'static str {
        match self {
            NodeKind::Sensor { .. } => "Sensor",
            NodeKind::Constant { .. } => "Constant",
            NodeKind::Curve { .. } => "Curve",
            NodeKind::Mix { .. } => "Mix",
            NodeKind::Clamp { .. } => "Clamp",
            NodeKind::RateLimit { .. } => "Rate limit",
            NodeKind::FanOutput { .. } => "Fan output",
        }
    }

    /// The node's typed port signature.
    pub fn spec(&self) -> NodeSpec {
        match self {
            NodeKind::Sensor { quantity, .. } => NodeSpec {
                inputs: vec![],
                outputs: vec![PortSpec::output("out", "Reading", *quantity)],
            },
            NodeKind::Constant { quantity, .. } => NodeSpec {
                inputs: vec![],
                outputs: vec![PortSpec::output("out", "Value", *quantity)],
            },
            NodeKind::Curve { input, .. } => NodeSpec {
                inputs: vec![PortSpec::input("in", "Input", *input)],
                outputs: vec![PortSpec::output("out", "Duty", Quantity::Duty)],
            },
            NodeKind::Mix { quantity, .. } => NodeSpec {
                inputs: vec![PortSpec::input("in", "Inputs", *quantity).variadic()],
                outputs: vec![PortSpec::output("out", "Result", *quantity)],
            },
            NodeKind::Clamp { quantity, .. } | NodeKind::RateLimit { quantity, .. } => NodeSpec {
                inputs: vec![PortSpec::input("in", "Input", *quantity)],
                outputs: vec![PortSpec::output("out", "Output", *quantity)],
            },
            NodeKind::FanOutput { .. } => NodeSpec {
                inputs: vec![PortSpec::input("duty", "Duty", Quantity::Duty)],
                outputs: vec![],
            },
        }
    }

    /// Evaluate this node for one tick.
    pub(crate) fn eval(
        &self,
        inputs: &BTreeMap<&str, Vec<Value>>,
        sensors: &SensorReadings,
        state: &mut NodeState,
    ) -> Produced {
        // Convenience: the single value on a non-variadic input, if present.
        let single = |key: &str| -> Option<Value> {
            inputs.get(key).and_then(|v| v.first()).copied()
        };

        match self {
            NodeKind::Sensor { sensor_id, quantity } => {
                // A sensor that is not reporting yields NaN rather than a plausible
                // number. Downstream sinks see an untrustworthy value and fail safe,
                // which is the whole reason we do not substitute a default here.
                let value = sensors
                    .get(sensor_id)
                    .copied()
                    .unwrap_or(Value::raw(*quantity, f64::NAN));
                Produced { outputs: vec![("out", value)], ..Default::default() }
            }

            NodeKind::Constant { quantity, value } => Produced {
                outputs: vec![("out", Value::new(*quantity, *value))],
                ..Default::default()
            },

            NodeKind::Curve { points, .. } => {
                let x = single("in").map(|v| v.scalar).unwrap_or(f64::NAN);
                let y = interpolate(points, x);
                Produced {
                    outputs: vec![("out", Value::raw(Quantity::Duty, y))],
                    ..Default::default()
                }
            }

            NodeKind::Mix { quantity, mode } => {
                let values: Vec<f64> =
                    inputs.get("in").map(|v| v.iter().map(|x| x.scalar).collect()).unwrap_or_default();
                let mixed = mix(*mode, &values);
                Produced {
                    outputs: vec![("out", Value::raw(*quantity, mixed))],
                    ..Default::default()
                }
            }

            NodeKind::Clamp { quantity, min, max } => {
                let x = single("in").map(|v| v.scalar).unwrap_or(f64::NAN);
                // Preserve NaN rather than clamping it to `min`: a clamp must not be
                // able to launder a broken reading into a confident-looking number.
                let y = if x.is_nan() { x } else { x.clamp(*min, *max) };
                Produced {
                    outputs: vec![("out", Value::raw(*quantity, y))],
                    ..Default::default()
                }
            }

            NodeKind::RateLimit { quantity, max_delta_per_tick } => {
                let x = single("in").map(|v| v.scalar).unwrap_or(f64::NAN);
                let y = match (state.last, x.is_finite()) {
                    // No history yet: adopt the input immediately rather than ramping
                    // up from zero, which would start every boot with the fans off.
                    (_, true) if state.last.is_none() => x,
                    (Some(prev), true) => {
                        let delta = (x - prev).clamp(-max_delta_per_tick.abs(), max_delta_per_tick.abs());
                        prev + delta
                    }
                    // A bad input must not be smoothed into the output; pass the fault on.
                    _ => f64::NAN,
                };
                if y.is_finite() {
                    state.last = Some(y);
                }
                Produced {
                    outputs: vec![("out", Value::raw(*quantity, y))],
                    ..Default::default()
                }
            }

            NodeKind::FanOutput { channel } => {
                match single("duty") {
                    Some(v) if v.is_trustworthy() => Produced {
                        commands: vec![(channel.clone(), Value::new(Quantity::Duty, v.scalar))],
                        ..Default::default()
                    },
                    // Unconnected or untrustworthy: report a fault so the engine applies
                    // this channel's failsafe. Never silently hold the last value.
                    _ => Produced { faults: vec![channel.clone()], ..Default::default() },
                }
            }
        }
    }
}

/// Piecewise-linear interpolation over a curve's points.
///
/// Points are sorted defensively on every call so a hand-edited profile with
/// out-of-order points behaves sensibly instead of producing a sawtooth. Outside the
/// curve's domain the endpoints are held flat.
fn interpolate(points: &[CurvePoint], x: f64) -> f64 {
    if points.is_empty() || !x.is_finite() {
        return f64::NAN;
    }
    let mut pts: Vec<CurvePoint> = points.to_vec();
    pts.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));

    if x <= pts[0].x {
        return pts[0].y;
    }
    if x >= pts[pts.len() - 1].x {
        return pts[pts.len() - 1].y;
    }
    for pair in pts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if x >= a.x && x <= b.x {
            let span = b.x - a.x;
            if span.abs() < f64::EPSILON {
                // Coincident points: a vertical step. Take the upper value, which is
                // the more-cooling side.
                return a.y.max(b.y);
            }
            return a.y + (b.y - a.y) * ((x - a.x) / span);
        }
    }
    pts[pts.len() - 1].y
}

/// Combine mixer inputs. Any non-finite input poisons the result, so a single dead
/// sensor in a mix cannot be averaged away into a falsely comfortable number.
fn mix(mode: MixMode, values: &[f64]) -> f64 {
    if values.is_empty() || values.iter().any(|v| !v.is_finite()) {
        return f64::NAN;
    }
    match mode {
        MixMode::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        MixMode::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
        MixMode::Average => values.iter().sum::<f64>() / values.len() as f64,
        MixMode::Sum => values.iter().sum(),
    }
}
