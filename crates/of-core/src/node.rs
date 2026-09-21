//! The node catalogue.
//!
//! Each variant of [`NodeKind`] declares its typed ports via [`NodeKind::spec`] and its
//! behaviour via [`NodeKind::eval`]. Adding a node means adding a variant and both
//! methods — the compiler will not let you forget either.
//!
//! # Time
//!
//! Stateful nodes are parameterised in **seconds**, not ticks, and receive the elapsed
//! time in [`TickInput::dt`]. A filter tuned as "30 second time constant" therefore keeps
//! behaving that way if the tick rate is changed, and a tick that runs late is integrated
//! correctly rather than silently under-weighted. `of-core` still reads no clock itself —
//! `dt` is measured by the engine and handed in, so tests drive time explicitly.
//!
//! # Faults
//!
//! Non-finite values propagate. Nodes do not substitute defaults for a missing or broken
//! input, because a plausible-looking number is indistinguishable from a real measurement
//! by the time it reaches a fan. See the safety tests in `tests.rs`.

use std::collections::{BTreeMap, VecDeque};

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
        Self {
            key,
            label,
            quantity,
            required: true,
            variadic: false,
        }
    }

    pub const fn output(key: &'static str, label: &'static str, quantity: Quantity) -> Self {
        Self {
            key,
            label,
            quantity,
            required: false,
            variadic: false,
        }
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

/// Everything evaluation needs for one tick, besides the graph itself.
#[derive(Debug, Clone, Copy)]
pub struct TickInput<'a> {
    pub sensors: &'a SensorReadings,
    /// Seconds elapsed since the previous tick. Always finite and positive; the engine
    /// clamps it so a suspended machine cannot hand the filters an hour-long step.
    pub dt: f64,
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

/// How a comparator's output relates to its threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Compare {
    Above,
    Below,
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
    /// Last emitted value, for rate limiters, filters and hold-style nodes.
    pub last: Option<f64>,
    /// Latched boolean, for comparators with hysteresis.
    pub flag: bool,
    /// Accumulated integral term, for PID.
    pub integral: f64,
    /// Previous error, for PID's derivative term.
    pub prev_error: Option<f64>,
    /// Recent samples, for moving averages.
    pub history: VecDeque<f64>,
}

/// A node's kind and its parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NodeKind {
    // --- Sources ---------------------------------------------------------------------
    /// Reads a hardware sensor by id.
    Sensor {
        sensor_id: String,
        quantity: Quantity,
    },

    /// Emits a fixed value. Also the backing node for a manual slider.
    Constant { quantity: Quantity, value: f64 },

    // --- Stateless transforms --------------------------------------------------------
    /// Maps an input quantity onto a duty via a piecewise-linear transfer curve.
    ///
    /// This is the node that makes `Temperature → Duty` legal, and the reason that
    /// conversion cannot happen by accident anywhere else.
    Curve {
        input: Quantity,
        points: Vec<CurvePoint>,
    },

    /// Combines any number of same-quantity inputs into one.
    Mix { quantity: Quantity, mode: MixMode },

    /// Constrains a value to a range.
    Clamp {
        quantity: Quantity,
        min: f64,
        max: f64,
    },

    /// Adds a constant.
    Offset { quantity: Quantity, delta: f64 },

    /// Multiplies by a constant.
    Scale { quantity: Quantity, factor: f64 },

    /// Converts a quantity to a dimensionless ratio against a reference, and back.
    /// The explicit escape hatch for arithmetic the type system otherwise forbids —
    /// conspicuous by design, so it shows up in review of a graph.
    Reinterpret { from: Quantity, to: Quantity },

    // --- Stateful transforms ---------------------------------------------------------
    /// Limits how fast a value may change, in units per second.
    ///
    /// The primary tool against the audible hunting an aggressive curve produces on a
    /// slow thermal mass.
    RateLimit {
        quantity: Quantity,
        max_delta_per_second: f64,
    },

    /// First-order exponential smoothing with a time constant in seconds.
    LowPass {
        quantity: Quantity,
        tau_seconds: f64,
    },

    /// Unweighted mean of the last `samples` values.
    MovingAverage { quantity: Quantity, samples: usize },

    /// Holds its output until the input moves further than `band` from the held value.
    /// Stops a fan twitching at every 0.1 °C of sensor noise.
    Hold { quantity: Quantity, band: f64 },

    /// Threshold test with a deadband, producing a boolean.
    Comparator {
        quantity: Quantity,
        threshold: f64,
        deadband: f64,
        direction: Compare,
    },

    /// Chooses between two same-quantity inputs.
    Select { quantity: Quantity },

    /// Proportional–integral–derivative controller driving a duty from a measurement.
    ///
    /// Gains are in duty-percent per unit of error. The integral term is clamped rather
    /// than allowed to wind up, because a saturated integrator is how a controller ends
    /// up commanding full duty for minutes after the load has gone away.
    Pid {
        quantity: Quantity,
        setpoint: f64,
        kp: f64,
        ki: f64,
        kd: f64,
        /// Bound on the integral's contribution, in duty percent.
        integral_limit: f64,
    },

    // --- Sinks -----------------------------------------------------------------------
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
            NodeKind::Offset { .. } => "Offset",
            NodeKind::Scale { .. } => "Scale",
            NodeKind::Reinterpret { .. } => "Reinterpret",
            NodeKind::RateLimit { .. } => "Rate limit",
            NodeKind::LowPass { .. } => "Low-pass filter",
            NodeKind::MovingAverage { .. } => "Moving average",
            NodeKind::Hold { .. } => "Hold",
            NodeKind::Comparator { .. } => "Comparator",
            NodeKind::Select { .. } => "Select",
            NodeKind::Pid { .. } => "PID",
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
            NodeKind::Clamp { quantity, .. }
            | NodeKind::Offset { quantity, .. }
            | NodeKind::Scale { quantity, .. }
            | NodeKind::RateLimit { quantity, .. }
            | NodeKind::LowPass { quantity, .. }
            | NodeKind::MovingAverage { quantity, .. }
            | NodeKind::Hold { quantity, .. } => NodeSpec {
                inputs: vec![PortSpec::input("in", "Input", *quantity)],
                outputs: vec![PortSpec::output("out", "Output", *quantity)],
            },
            NodeKind::Reinterpret { from, to } => NodeSpec {
                inputs: vec![PortSpec::input("in", "Input", *from)],
                outputs: vec![PortSpec::output("out", "Output", *to)],
            },
            NodeKind::Comparator { quantity, .. } => NodeSpec {
                inputs: vec![PortSpec::input("in", "Input", *quantity)],
                outputs: vec![PortSpec::output("out", "Result", Quantity::Boolean)],
            },
            NodeKind::Select { quantity } => NodeSpec {
                inputs: vec![
                    PortSpec::input("when", "When", Quantity::Boolean),
                    PortSpec::input("if_true", "If true", *quantity),
                    PortSpec::input("if_false", "If false", *quantity),
                ],
                outputs: vec![PortSpec::output("out", "Output", *quantity)],
            },
            NodeKind::Pid { quantity, .. } => NodeSpec {
                inputs: vec![PortSpec::input("in", "Measurement", *quantity)],
                outputs: vec![PortSpec::output("out", "Duty", Quantity::Duty)],
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
        ctx: &TickInput<'_>,
        state: &mut NodeState,
    ) -> Produced {
        // The single value on a non-variadic input, or NaN when nothing is connected.
        let scalar = |key: &str| -> f64 {
            inputs
                .get(key)
                .and_then(|v| v.first())
                .map(|v| v.scalar)
                .unwrap_or(f64::NAN)
        };
        let emit = |q: Quantity, v: f64| Produced {
            outputs: vec![("out", Value::raw(q, v))],
            ..Default::default()
        };

        match self {
            NodeKind::Sensor {
                sensor_id,
                quantity,
            } => {
                // A sensor that is not reporting yields NaN rather than a plausible
                // number. Downstream sinks see an untrustworthy value and fail safe,
                // which is the whole reason we do not substitute a default here.
                //
                // A reading that arrives as the wrong quantity is treated the same way.
                // Hardware can re-enumerate — a header the user moved a fan to, a chip
                // detected differently after a BIOS update — and a graph that asked for
                // a temperature must not silently start steering on an RPM.
                let value = match ctx.sensors.get(sensor_id) {
                    Some(v) if v.quantity == *quantity => *v,
                    _ => Value::raw(*quantity, f64::NAN),
                };
                Produced {
                    outputs: vec![("out", value)],
                    ..Default::default()
                }
            }

            // Fully qualified: `Ord::clamp` is also in scope on `&Quantity` and would
            // otherwise win, silently turning this into a two-argument range clamp.
            NodeKind::Constant { quantity, value } => {
                emit(*quantity, Quantity::clamp(*quantity, *value))
            }

            NodeKind::Curve { points, .. } => {
                emit(Quantity::Duty, interpolate(points, scalar("in")))
            }

            NodeKind::Mix { quantity, mode } => {
                let values: Vec<f64> = inputs
                    .get("in")
                    .map(|v| v.iter().map(|x| x.scalar).collect())
                    .unwrap_or_default();
                emit(*quantity, mix(*mode, &values))
            }

            NodeKind::Clamp { quantity, min, max } => {
                let x = scalar("in");
                // Preserve NaN rather than clamping it to `min`: a clamp must not be
                // able to launder a broken reading into a confident-looking number.
                emit(*quantity, if x.is_nan() { x } else { x.clamp(*min, *max) })
            }

            NodeKind::Offset { quantity, delta } => emit(*quantity, scalar("in") + delta),

            NodeKind::Scale { quantity, factor } => emit(*quantity, scalar("in") * factor),

            NodeKind::Reinterpret { to, .. } => emit(*to, scalar("in")),

            NodeKind::RateLimit {
                quantity,
                max_delta_per_second,
            } => {
                let x = scalar("in");
                let y = match state.last {
                    _ if !x.is_finite() => f64::NAN,
                    // No history yet: adopt the input immediately rather than ramping
                    // up from zero, which would start every boot with the fans off.
                    None => x,
                    Some(prev) => {
                        let step = max_delta_per_second.abs() * ctx.dt;
                        prev + (x - prev).clamp(-step, step)
                    }
                };
                if y.is_finite() {
                    state.last = Some(y);
                }
                emit(*quantity, y)
            }

            NodeKind::LowPass {
                quantity,
                tau_seconds,
            } => {
                let x = scalar("in");
                let y = match state.last {
                    _ if !x.is_finite() => f64::NAN,
                    None => x,
                    Some(prev) => {
                        // Exponential form rather than `dt/tau`, so a long tick cannot
                        // overshoot into instability the way the naive form does.
                        let tau = tau_seconds.max(f64::EPSILON);
                        let alpha = 1.0 - (-ctx.dt / tau).exp();
                        prev + alpha * (x - prev)
                    }
                };
                if y.is_finite() {
                    state.last = Some(y);
                }
                emit(*quantity, y)
            }

            NodeKind::MovingAverage { quantity, samples } => {
                let x = scalar("in");
                if !x.is_finite() {
                    // Drop the window: once a reading is bad, the average of the stale
                    // ones is not a measurement of anything.
                    state.history.clear();
                    return emit(*quantity, f64::NAN);
                }
                let window = (*samples).max(1);
                state.history.push_back(x);
                while state.history.len() > window {
                    state.history.pop_front();
                }
                let sum: f64 = state.history.iter().sum();
                emit(*quantity, sum / state.history.len() as f64)
            }

            NodeKind::Hold { quantity, band } => {
                let x = scalar("in");
                let y = match state.last {
                    _ if !x.is_finite() => f64::NAN,
                    Some(held) if (x - held).abs() <= band.abs() => held,
                    _ => x,
                };
                if y.is_finite() {
                    state.last = Some(y);
                }
                emit(*quantity, y)
            }

            NodeKind::Comparator {
                threshold,
                deadband,
                direction,
                ..
            } => {
                let x = scalar("in");
                if !x.is_finite() {
                    return emit(Quantity::Boolean, f64::NAN);
                }
                let half = deadband.abs() / 2.0;
                // Asymmetric thresholds: the flag only flips once the input has cleared
                // the deadband, so a value sitting on the threshold does not chatter.
                let raised = match direction {
                    Compare::Above => x > threshold + half,
                    Compare::Below => x < threshold - half,
                };
                let lowered = match direction {
                    Compare::Above => x < threshold - half,
                    Compare::Below => x > threshold + half,
                };
                if raised {
                    state.flag = true;
                } else if lowered {
                    state.flag = false;
                }
                emit(Quantity::Boolean, if state.flag { 1.0 } else { 0.0 })
            }

            NodeKind::Select { quantity } => {
                let when = scalar("when");
                if !when.is_finite() {
                    return emit(*quantity, f64::NAN);
                }
                let chosen = if when >= 0.5 {
                    scalar("if_true")
                } else {
                    scalar("if_false")
                };
                emit(*quantity, chosen)
            }

            NodeKind::Pid {
                setpoint,
                kp,
                ki,
                kd,
                integral_limit,
                ..
            } => {
                let x = scalar("in");
                if !x.is_finite() || ctx.dt <= 0.0 {
                    // Do not integrate across a fault; a resumed controller should not
                    // carry error accumulated while it was blind.
                    state.prev_error = None;
                    return emit(Quantity::Duty, f64::NAN);
                }
                // Error is positive when the measurement is above setpoint, so a hotter
                // machine asks for more cooling.
                let error = x - setpoint;

                state.integral += error * ctx.dt;
                // Clamp the integral by its *contribution*, so the limit means the same
                // thing regardless of ki.
                if *ki != 0.0 {
                    let bound = (integral_limit / ki).abs();
                    state.integral = state.integral.clamp(-bound, bound);
                } else {
                    state.integral = 0.0;
                }

                let derivative = match state.prev_error {
                    Some(prev) => (error - prev) / ctx.dt,
                    // No derivative kick on the first sample.
                    None => 0.0,
                };
                state.prev_error = Some(error);

                let out = kp * error + ki * state.integral + kd * derivative;
                emit(Quantity::Duty, out.clamp(0.0, 100.0))
            }

            NodeKind::FanOutput { channel } => {
                match inputs.get("duty").and_then(|v| v.first()) {
                    Some(v) if v.is_trustworthy() => Produced {
                        commands: vec![(channel.clone(), Value::new(Quantity::Duty, v.scalar))],
                        ..Default::default()
                    },
                    // Unconnected or untrustworthy: report a fault so the engine applies
                    // this channel's failsafe. Never silently hold the last value.
                    _ => Produced {
                        faults: vec![channel.clone()],
                        ..Default::default()
                    },
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
