//! The node catalogue.
//!
//! Each variant of [`NodeKind`] declares its typed ports via [`NodeKind::spec`] and its
//! behaviour via [`NodeKind::eval`]. Adding a node means adding a variant and both
//! methods — the compiler will not let you forget either.
//!
//! # Generic nodes
//!
//! Most transforms do not care what they are carrying. A rate limiter limits the rate of
//! change of *something*; a mixer takes the maximum of *some* set of same-typed values.
//! Those declare their ports as a **type variable** ([`PortType::Var`]) rather than a
//! concrete quantity, and the type is inferred from whatever they are connected to.
//!
//! Concrete types enter the graph at its edges — a sensor reads a temperature, a fan
//! takes a duty, a curve emits a duty — and propagate inwards through the variables. A
//! node whose variable is still unconstrained is genuinely generic and stays that way;
//! the editor draws such ports in neutral white, and they lock to a colour the moment a
//! connection decides them.
//!
//! The payoff is that most nodes have no type parameter to configure at all, and a chain
//! that is consistent end to end cannot be built wrong.
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

/// The name of the type variable used by every single-parameter generic node.
///
/// Variables are scoped to a node, so every generic node can reuse the same name without
/// them becoming entangled.
pub const T: &str = "T";

/// A port's declared type: either a fixed quantity or a variable to be inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PortType {
    /// A fixed quantity. These are the anchors inference propagates from.
    Concrete(Quantity),
    /// A variable, scoped to the node that declares it. Ports sharing a name on the same
    /// node must resolve to the same quantity.
    Var(&'static str),
}

impl PortType {
    pub const fn concrete(&self) -> Option<Quantity> {
        match self {
            PortType::Concrete(q) => Some(*q),
            PortType::Var(_) => None,
        }
    }

    pub const fn is_generic(&self) -> bool {
        matches!(self, PortType::Var(_))
    }
}

/// One typed port on a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub ty: PortType,
    /// The graph is invalid if a required input is left unconnected. Optional inputs
    /// fall back to a node-defined default.
    pub required: bool,
    /// A variadic input accepts any number of incoming connections (a mixer's inputs).
    pub variadic: bool,
    /// An output whose value does **not** depend on this tick's inputs.
    ///
    /// A fan's tachometer is the motivating case: we read sensors, then evaluate, then
    /// write duties, so a speed reading necessarily reflects an *earlier* duty. The
    /// value is available before evaluation begins, which means an edge leaving such a
    /// port creates no dependency and a graph containing one is still acyclic.
    ///
    /// This is what lets feedback be expressed without an artificial delay node: the
    /// delay is real, it lives in the hardware, and the model says so.
    pub delayed: bool,
}

impl PortSpec {
    pub const fn input(key: &'static str, label: &'static str, ty: PortType) -> Self {
        Self {
            key,
            label,
            ty,
            required: true,
            variadic: false,
            delayed: false,
        }
    }

    pub const fn output(key: &'static str, label: &'static str, ty: PortType) -> Self {
        Self {
            key,
            label,
            ty,
            required: false,
            variadic: false,
            delayed: false,
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

    /// Mark an output as carrying a value from before this tick.
    pub const fn delayed(mut self) -> Self {
        self.delayed = true;
        self
    }
}

/// A concrete-typed input port.
const fn fixed_in(key: &'static str, label: &'static str, q: Quantity) -> PortSpec {
    PortSpec::input(key, label, PortType::Concrete(q))
}

/// A concrete-typed output port.
const fn fixed_out(key: &'static str, label: &'static str, q: Quantity) -> PortSpec {
    PortSpec::output(key, label, PortType::Concrete(q))
}

/// An input whose type is inferred.
const fn any_in(key: &'static str, label: &'static str) -> PortSpec {
    PortSpec::input(key, label, PortType::Var(T))
}

/// An output whose type is inferred.
const fn any_out(key: &'static str, label: &'static str) -> PortSpec {
    PortSpec::output(key, label, PortType::Var(T))
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
    /// Which sensor reads back each output channel, from the hardware inventory.
    ///
    /// Hardware knowledge rather than user configuration, so it is supplied per tick
    /// instead of stored in the document — a fan moved to another header must not leave
    /// a saved profile quietly reading the wrong tachometer.
    pub tachometers: Option<&'a BTreeMap<String, String>>,
}

impl<'a> TickInput<'a> {
    pub fn new(sensors: &'a SensorReadings, dt: f64) -> Self {
        Self {
            sensors,
            dt,
            tachometers: None,
        }
    }

    pub fn with_tachometers(mut self, map: &'a BTreeMap<String, String>) -> Self {
        self.tachometers = Some(map);
        self
    }
}

/// How a mixer combines its inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../../ui/src/bindings/")
)]
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
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../../ui/src/bindings/")
)]
pub struct CurvePoint {
    pub x: f64,
    pub y: f64,
}

/// How a comparator's output relates to its threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../../ui/src/bindings/")
)]
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
    /// Recent samples, for moving averages; sample *ages* in seconds, for delays.
    pub history: VecDeque<f64>,
    /// Buffered values matching `history`'s ages, for delays.
    pub buffer: VecDeque<f64>,
}

/// A node's kind and its parameters.
///
/// Note how few variants carry a `quantity`: the generic ones infer it, so there is no
/// type to set and no way to set one inconsistently with what it is wired to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../../ui/src/bindings/")
)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NodeKind {
    // --- Sources ---------------------------------------------------------------------
    /// Reads a hardware sensor by id. One of the places a concrete type enters the graph.
    Sensor {
        sensor_id: String,
        quantity: Quantity,
    },

    /// Emits a fixed value. Also the backing node for a manual slider.
    ///
    /// Generic: a constant wired into a duty input is a duty, and the same node wired
    /// into a temperature input is a temperature.
    Constant { value: f64 },

    // --- Stateless transforms --------------------------------------------------------
    /// Maps any input quantity onto a duty via a piecewise-linear transfer curve.
    ///
    /// This is the node that makes `Temperature → Duty` legal, and the reason that
    /// conversion cannot happen by accident anywhere else.
    Curve { points: Vec<CurvePoint> },

    /// Combines any number of same-typed inputs into one.
    Mix { mode: MixMode },

    /// Constrains a value to a range.
    Clamp { min: f64, max: f64 },

    /// Adds a constant.
    Offset { delta: f64 },

    /// Multiplies by a constant.
    Scale { factor: f64 },

    /// Reads a value as a different type.
    ///
    /// The explicit escape hatch for conversions the type system otherwise forbids, and
    /// deliberately the only node with two independent concrete types — crossing the
    /// type system should be visible when reading a graph.
    Reinterpret { from: Quantity, to: Quantity },

    // --- Stateful transforms ---------------------------------------------------------
    /// Limits how fast a value may change, in units per second.
    ///
    /// The primary tool against the audible hunting an aggressive curve produces on a
    /// slow thermal mass.
    RateLimit { max_delta_per_second: f64 },

    /// First-order exponential smoothing with a time constant in seconds.
    LowPass { tau_seconds: f64 },

    /// Unweighted mean of the last `samples` values.
    MovingAverage { samples: usize },

    /// Holds its output until the input moves further than `band` from the held value.
    /// Stops a fan twitching at every 0.1 °C of sensor noise.
    Hold { band: f64 },

    /// Threshold test with a deadband, producing a boolean.
    Comparator {
        threshold: f64,
        deadband: f64,
        direction: Compare,
    },

    /// Chooses between two same-typed inputs.
    Select,

    /// Emits what it was given `seconds` ago.
    ///
    /// Useful in its own right — staggering fans, holding a boost for a while — and its
    /// output is delayed, so it can also sit inside a feedback path. Note that feedback
    /// on a *tachometer* needs no delay node: that loop is already broken by the
    /// hardware, which is what `FanOutput`'s speed output being delayed expresses.
    Delay { seconds: f64 },

    /// Proportional–integral–derivative controller driving a duty from a measurement.
    ///
    /// Gains are in duty-percent per unit of error. The integral term is clamped rather
    /// than allowed to wind up, because a saturated integrator is how a controller ends
    /// up commanding full duty for minutes after the load has gone away.
    Pid {
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
            NodeKind::Select => "Select",
            NodeKind::Delay { .. } => "Delay",
            NodeKind::Pid { .. } => "PID",
            NodeKind::FanOutput { .. } => "Fan output",
        }
    }

    /// The node's typed port signature.
    pub fn spec(&self) -> NodeSpec {
        match self {
            NodeKind::Sensor { quantity, .. } => NodeSpec {
                inputs: vec![],
                outputs: vec![fixed_out("out", "Reading", *quantity)],
            },
            NodeKind::Constant { .. } => NodeSpec {
                inputs: vec![],
                outputs: vec![any_out("out", "Value")],
            },
            NodeKind::Curve { .. } => NodeSpec {
                inputs: vec![any_in("in", "Input")],
                outputs: vec![fixed_out("out", "Duty", Quantity::Duty)],
            },
            NodeKind::Mix { .. } => NodeSpec {
                inputs: vec![any_in("in", "Inputs").variadic()],
                outputs: vec![any_out("out", "Result")],
            },
            NodeKind::Clamp { .. }
            | NodeKind::Offset { .. }
            | NodeKind::Scale { .. }
            | NodeKind::RateLimit { .. }
            | NodeKind::LowPass { .. }
            | NodeKind::MovingAverage { .. }
            | NodeKind::Hold { .. } => NodeSpec {
                inputs: vec![any_in("in", "Input")],
                outputs: vec![any_out("out", "Output")],
            },
            NodeKind::Reinterpret { from, to } => NodeSpec {
                inputs: vec![fixed_in("in", "Input", *from)],
                outputs: vec![fixed_out("out", "Output", *to)],
            },
            NodeKind::Comparator { .. } => NodeSpec {
                inputs: vec![any_in("in", "Input")],
                outputs: vec![fixed_out("out", "Result", Quantity::Boolean)],
            },
            NodeKind::Select => NodeSpec {
                inputs: vec![
                    fixed_in("when", "When", Quantity::Boolean),
                    any_in("if_true", "If true"),
                    any_in("if_false", "If false"),
                ],
                outputs: vec![any_out("out", "Output")],
            },
            NodeKind::Delay { .. } => NodeSpec {
                inputs: vec![any_in("in", "Input")],
                outputs: vec![any_out("out", "Output").delayed()],
            },
            NodeKind::Pid { .. } => NodeSpec {
                inputs: vec![any_in("in", "Measurement")],
                outputs: vec![fixed_out("out", "Duty", Quantity::Duty)],
            },
            // The speed output is delayed: it is what the tachometer measured before
            // this tick's duty was written. An edge from it therefore creates no
            // dependency, and feeding it back into the graph is not a cycle.
            NodeKind::FanOutput { .. } => NodeSpec {
                inputs: vec![fixed_in("duty", "Duty", Quantity::Duty)],
                outputs: vec![fixed_out("rpm", "Speed", Quantity::Rpm).delayed()],
            },
        }
    }

    /// Produce this node's delayed outputs.
    ///
    /// Runs **before** the topological pass, because by definition these values do not
    /// depend on anything computed this tick. That ordering is the whole mechanism: it
    /// is why a tachometer can feed back into the graph that drives its own fan without
    /// the graph containing a cycle.
    pub(crate) fn sourced(
        &self,
        ctx: &TickInput<'_>,
        state: &NodeState,
        out_type: Option<Quantity>,
    ) -> Vec<(&'static str, Value)> {
        match self {
            NodeKind::FanOutput { channel } => {
                // The reading has to be both present and actually a speed; a channel
                // with no tachometer, or one re-enumerated as something else, yields a
                // fault rather than a number.
                let reading = ctx
                    .tachometers
                    .and_then(|map| map.get(channel))
                    .and_then(|sensor| ctx.sensors.get(sensor))
                    .filter(|v| v.quantity == Quantity::Rpm)
                    .copied()
                    .unwrap_or(Value::raw(Quantity::Rpm, f64::NAN));
                vec![("rpm", reading)]
            }

            NodeKind::Delay { .. } => {
                let q = out_type.unwrap_or(Quantity::Ratio);
                // Nothing buffered yet means nothing has been observed to delay, which
                // is a fault rather than a zero — a fan must not be driven from a value
                // that was never measured.
                let value = state.buffer.front().copied().unwrap_or(f64::NAN);
                vec![("out", Value::raw(q, value))]
            }

            _ => Vec::new(),
        }
    }

    /// Evaluate this node for one tick.
    ///
    /// `out_type` is the inferred quantity of the node's `out` port, used only by nodes
    /// with no input to take it from. Everything else carries its input's type through,
    /// which is what makes a generic node generic at runtime as well as at edit time.
    pub(crate) fn eval(
        &self,
        inputs: &BTreeMap<&str, Vec<Value>>,
        ctx: &TickInput<'_>,
        state: &mut NodeState,
        out_type: Option<Quantity>,
    ) -> Produced {
        // The single value on a non-variadic input, or NaN when nothing is connected.
        let scalar = |key: &str| -> f64 {
            inputs
                .get(key)
                .and_then(|v| v.first())
                .map(|v| v.scalar)
                .unwrap_or(f64::NAN)
        };
        // What a pass-through node should emit as. Prefer the type actually flowing in;
        // fall back to what inference decided, and finally to a dimensionless value —
        // reached only when the input is missing, in which case the value is a fault
        // anyway and its unit is immaterial.
        let carried = |key: &str| -> Quantity {
            inputs
                .get(key)
                .and_then(|v| v.first())
                .map(|v| v.quantity)
                .or(out_type)
                .unwrap_or(Quantity::Ratio)
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

            NodeKind::Constant { value } => {
                // Nothing flows in, so the type is whatever inference settled on.
                let q = out_type.unwrap_or(Quantity::Ratio);
                emit(q, Quantity::clamp(q, *value))
            }

            NodeKind::Curve { points } => emit(Quantity::Duty, interpolate(points, scalar("in"))),

            NodeKind::Mix { mode } => {
                let values: Vec<f64> = inputs
                    .get("in")
                    .map(|v| v.iter().map(|x| x.scalar).collect())
                    .unwrap_or_default();
                emit(carried("in"), mix(*mode, &values))
            }

            NodeKind::Clamp { min, max } => {
                let x = scalar("in");
                // Preserve NaN rather than clamping it to `min`: a clamp must not be
                // able to launder a broken reading into a confident-looking number.
                emit(
                    carried("in"),
                    if x.is_nan() { x } else { x.clamp(*min, *max) },
                )
            }

            NodeKind::Offset { delta } => emit(carried("in"), scalar("in") + delta),

            NodeKind::Scale { factor } => emit(carried("in"), scalar("in") * factor),

            NodeKind::Reinterpret { to, .. } => emit(*to, scalar("in")),

            NodeKind::RateLimit {
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
                emit(carried("in"), y)
            }

            NodeKind::LowPass { tau_seconds } => {
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
                emit(carried("in"), y)
            }

            NodeKind::MovingAverage { samples } => {
                let x = scalar("in");
                let q = carried("in");
                if !x.is_finite() {
                    // Drop the window: once a reading is bad, the average of the stale
                    // ones is not a measurement of anything.
                    state.history.clear();
                    return emit(q, f64::NAN);
                }
                let window = (*samples).max(1);
                state.history.push_back(x);
                while state.history.len() > window {
                    state.history.pop_front();
                }
                let sum: f64 = state.history.iter().sum();
                emit(q, sum / state.history.len() as f64)
            }

            NodeKind::Hold { band } => {
                let x = scalar("in");
                let y = match state.last {
                    _ if !x.is_finite() => f64::NAN,
                    Some(held) if (x - held).abs() <= band.abs() => held,
                    _ => x,
                };
                if y.is_finite() {
                    state.last = Some(y);
                }
                emit(carried("in"), y)
            }

            NodeKind::Comparator {
                threshold,
                deadband,
                direction,
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

            NodeKind::Select => {
                let when = scalar("when");
                // Either branch tells us the type; prefer whichever is actually present.
                let q = inputs
                    .get("if_true")
                    .and_then(|v| v.first())
                    .or_else(|| inputs.get("if_false").and_then(|v| v.first()))
                    .map(|v| v.quantity)
                    .or(out_type)
                    .unwrap_or(Quantity::Ratio);
                if !when.is_finite() {
                    return emit(q, f64::NAN);
                }
                let chosen = if when >= 0.5 {
                    scalar("if_true")
                } else {
                    scalar("if_false")
                };
                emit(q, chosen)
            }

            NodeKind::Pid {
                setpoint,
                kp,
                ki,
                kd,
                integral_limit,
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

            NodeKind::Delay { seconds } => {
                // Samples are stamped with how long ago they arrived, so the delay stays
                // in seconds and survives a change of tick rate like every other
                // stateful node.
                for age in state.history.iter_mut() {
                    *age += ctx.dt;
                }
                let value = scalar("in");
                state.history.push_back(0.0);
                state.buffer.push_back(value);

                // Drop everything older than the delay, keeping the newest such sample
                // as the one to emit next tick.
                while state.history.len() > 1 && state.history[1] >= seconds.max(0.0) {
                    state.history.pop_front();
                    state.buffer.pop_front();
                }

                // The output was produced in the delayed pass; emit nothing here.
                Produced::default()
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
