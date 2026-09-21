//! Behavioural tests for the node catalogue.
//!
//! `tests.rs` covers the graph: validation, typing, evaluation order and fault
//! propagation. This module covers what individual nodes compute — in particular the
//! stateful ones, where being parameterised in seconds rather than ticks is the property
//! that matters.

use super::*;
use of_units::Quantity;

/// The engine's default 10 Hz cadence.
const DT: f64 = 0.1;

trait TickExt {
    fn tick_t(&self, sensors: &SensorReadings, state: &mut EvalState) -> TickResult;
}

impl TickExt for CompiledGraph {
    fn tick_t(&self, sensors: &SensorReadings, state: &mut EvalState) -> TickResult {
        self.tick_with(sensors, DT, state)
    }
}

fn readings(pairs: &[(&str, Quantity, f64)]) -> SensorReadings {
    pairs
        .iter()
        .map(|&(k, q, v)| (k.to_owned(), Value::raw(q, v)))
        .collect()
}

/// A single-transform pipeline: sensor -> node under test.
fn pipeline(node: NodeKind, quantity: Quantity) -> Graph {
    let mut g = Graph::default();
    g.insert(
        "src",
        NodeKind::Sensor {
            sensor_id: "s".into(),
            quantity,
        },
    );
    g.insert("node", node);
    g.connect(PortRef::new("src", "out"), PortRef::new("node", "in"));
    g
}

/// Feed one value through a pipeline and read the node under test's output.
fn probe(g: &CompiledGraph, value: f64, q: Quantity, state: &mut EvalState) -> f64 {
    g.tick_t(&readings(&[("s", q, value)]), state).wire_values[&PortRef::new("node", "out")].scalar
}

// --- Filters: tuned in seconds, not ticks --------------------------------------------

#[test]
fn low_pass_reaches_63_percent_after_one_time_constant() {
    let g = pipeline(
        NodeKind::LowPass {
            quantity: Quantity::Temperature,
            tau_seconds: 1.0,
        },
        Quantity::Temperature,
    )
    .validate()
    .unwrap();
    let mut state = EvalState::new();

    // The first sample is adopted outright, so a fresh start does not spend a time
    // constant ramping up from zero with the fans off.
    assert_eq!(probe(&g, 0.0, Quantity::Temperature, &mut state), 0.0);

    let mut y = 0.0;
    for _ in 0..10 {
        y = probe(&g, 100.0, Quantity::Temperature, &mut state);
    }
    assert!(
        (y - 63.2).abs() < 1.0,
        "expected ~63.2 after one time constant, got {y}"
    );
}

#[test]
fn low_pass_is_tuned_in_seconds_not_ticks() {
    // The same filter at half the tick rate must land in the same place after the same
    // wall-clock time. This is what makes the tick rate a performance knob rather than a
    // tuning knob, and what stops a busy machine from quietly changing fan behaviour.
    let run = |dt: f64, ticks: usize| {
        let g = pipeline(
            NodeKind::LowPass {
                quantity: Quantity::Temperature,
                tau_seconds: 2.0,
            },
            Quantity::Temperature,
        )
        .validate()
        .unwrap();
        let mut state = EvalState::new();
        g.tick_with(
            &readings(&[("s", Quantity::Temperature, 0.0)]),
            dt,
            &mut state,
        );
        let mut y = 0.0;
        for _ in 0..ticks {
            y = g
                .tick_with(
                    &readings(&[("s", Quantity::Temperature, 100.0)]),
                    dt,
                    &mut state,
                )
                .wire_values[&PortRef::new("node", "out")]
                .scalar;
        }
        y
    };

    let fast = run(0.05, 40); // 2 s
    let slow = run(0.20, 10); // 2 s
    assert!(
        (fast - slow).abs() < 0.5,
        "tick rate must not change the tuning: {fast} vs {slow}"
    );
}

#[test]
fn rate_limit_is_per_second_and_scales_with_dt() {
    let g = pipeline(
        NodeKind::RateLimit {
            quantity: Quantity::Duty,
            max_delta_per_second: 10.0,
        },
        Quantity::Duty,
    )
    .validate()
    .unwrap();
    let mut state = EvalState::new();

    assert_eq!(probe(&g, 0.0, Quantity::Duty, &mut state), 0.0);

    // 0.1 s at 10 %/s is one point.
    assert!((probe(&g, 100.0, Quantity::Duty, &mut state) - 1.0).abs() < 1e-9);

    // A longer tick moves proportionally further rather than being capped at one tick's
    // worth, which is how a late tick would otherwise stall a ramp.
    let y = g
        .tick_with(&readings(&[("s", Quantity::Duty, 100.0)]), 0.5, &mut state)
        .wire_values[&PortRef::new("node", "out")]
        .scalar;
    assert!((y - 6.0).abs() < 1e-9, "expected 1 + 5, got {y}");
}

#[test]
fn moving_average_smooths_then_forgets_on_a_fault() {
    let g = pipeline(
        NodeKind::MovingAverage {
            quantity: Quantity::Temperature,
            samples: 4,
        },
        Quantity::Temperature,
    )
    .validate()
    .unwrap();
    let mut state = EvalState::new();

    probe(&g, 0.0, Quantity::Temperature, &mut state);
    probe(&g, 0.0, Quantity::Temperature, &mut state);
    let y = probe(&g, 100.0, Quantity::Temperature, &mut state);
    assert!((y - 33.333).abs() < 0.01, "got {y}");

    // A bad reading clears the window: the average of stale samples is not a
    // measurement of anything.
    let faulted = g.tick_t(&SensorReadings::new(), &mut state);
    assert!(!faulted.wire_values[&PortRef::new("node", "out")].is_trustworthy());

    // Recovery starts clean rather than blending with pre-fault history.
    assert_eq!(probe(&g, 50.0, Quantity::Temperature, &mut state), 50.0);
}

#[test]
fn hold_ignores_noise_inside_its_band_but_follows_real_movement() {
    let g = pipeline(
        NodeKind::Hold {
            quantity: Quantity::Temperature,
            band: 2.0,
        },
        Quantity::Temperature,
    )
    .validate()
    .unwrap();
    let mut state = EvalState::new();

    assert_eq!(probe(&g, 50.0, Quantity::Temperature, &mut state), 50.0);
    // Inside the band: held, so the fan does not twitch at sensor noise.
    assert_eq!(probe(&g, 51.5, Quantity::Temperature, &mut state), 50.0);
    assert_eq!(probe(&g, 48.5, Quantity::Temperature, &mut state), 50.0);
    // Outside: follows.
    assert_eq!(probe(&g, 53.0, Quantity::Temperature, &mut state), 53.0);
}

// --- Logic ---------------------------------------------------------------------------

#[test]
fn comparator_deadband_prevents_chatter_on_the_threshold() {
    let g = pipeline(
        NodeKind::Comparator {
            quantity: Quantity::Temperature,
            threshold: 60.0,
            deadband: 4.0,
            direction: Compare::Above,
        },
        Quantity::Temperature,
    )
    .validate()
    .unwrap();
    let mut state = EvalState::new();

    assert_eq!(probe(&g, 55.0, Quantity::Temperature, &mut state), 0.0);
    // Inside the deadband: no change, in either direction.
    assert_eq!(probe(&g, 61.0, Quantity::Temperature, &mut state), 0.0);
    // Clear of the upper edge: latches on.
    assert_eq!(probe(&g, 63.0, Quantity::Temperature, &mut state), 1.0);
    // Back inside the deadband: stays on.
    assert_eq!(probe(&g, 59.0, Quantity::Temperature, &mut state), 1.0);
    // Clear of the lower edge: releases.
    assert_eq!(probe(&g, 57.0, Quantity::Temperature, &mut state), 0.0);
}

#[test]
fn select_routes_between_two_inputs_and_faults_without_a_flag() {
    let mut g = Graph::default();
    g.insert(
        "flag",
        NodeKind::Sensor {
            sensor_id: "f".into(),
            quantity: Quantity::Boolean,
        },
    );
    g.insert(
        "quiet",
        NodeKind::Constant {
            quantity: Quantity::Duty,
            value: 30.0,
        },
    );
    g.insert(
        "loud",
        NodeKind::Constant {
            quantity: Quantity::Duty,
            value: 80.0,
        },
    );
    g.insert(
        "sel",
        NodeKind::Select {
            quantity: Quantity::Duty,
        },
    );
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f1".into(),
        },
    );
    g.connect(PortRef::new("flag", "out"), PortRef::new("sel", "when"));
    g.connect(PortRef::new("loud", "out"), PortRef::new("sel", "if_true"));
    g.connect(
        PortRef::new("quiet", "out"),
        PortRef::new("sel", "if_false"),
    );
    g.connect(PortRef::new("sel", "out"), PortRef::new("fan", "duty"));

    let compiled = g.validate().unwrap();
    let mut state = EvalState::new();

    let off = compiled.tick_t(&readings(&[("f", Quantity::Boolean, 0.0)]), &mut state);
    assert_eq!(off.commands["f1"].scalar, 30.0);

    let on = compiled.tick_t(&readings(&[("f", Quantity::Boolean, 1.0)]), &mut state);
    assert_eq!(on.commands["f1"].scalar, 80.0);

    // A missing flag must not silently pick a branch.
    let broken = compiled.tick_t(&SensorReadings::new(), &mut state);
    assert!(broken.faulted_channels.contains("f1"));
}

#[test]
fn a_boolean_cannot_be_wired_into_a_duty_input() {
    let mut g = Graph::default();
    g.insert(
        "flag",
        NodeKind::Sensor {
            sensor_id: "f".into(),
            quantity: Quantity::Boolean,
        },
    );
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f1".into(),
        },
    );
    g.connect(PortRef::new("flag", "out"), PortRef::new("fan", "duty"));

    let errors = g.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, GraphError::TypeMismatch { .. })),
        "{errors:?}"
    );
}

// --- Arithmetic and the escape hatch -------------------------------------------------

#[test]
fn reinterpret_is_the_only_way_across_types() {
    // Load into a duty input is rejected...
    let mut g = Graph::default();
    g.insert(
        "load",
        NodeKind::Sensor {
            sensor_id: "l".into(),
            quantity: Quantity::Load,
        },
    );
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f1".into(),
        },
    );
    g.connect(PortRef::new("load", "out"), PortRef::new("fan", "duty"));
    assert!(g.validate().is_err());

    // ...and legal only through a node that says, visibly, that the conversion is meant.
    let mut g = Graph::default();
    g.insert(
        "load",
        NodeKind::Sensor {
            sensor_id: "l".into(),
            quantity: Quantity::Load,
        },
    );
    g.insert(
        "cast",
        NodeKind::Reinterpret {
            from: Quantity::Load,
            to: Quantity::Duty,
        },
    );
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f1".into(),
        },
    );
    g.connect(PortRef::new("load", "out"), PortRef::new("cast", "in"));
    g.connect(PortRef::new("cast", "out"), PortRef::new("fan", "duty"));

    let compiled = g.validate().unwrap();
    let out = compiled.tick_t(
        &readings(&[("l", Quantity::Load, 70.0)]),
        &mut EvalState::new(),
    );
    assert_eq!(out.commands["f1"].scalar, 70.0);
}

#[test]
fn offset_and_scale_shift_a_value() {
    let g = pipeline(
        NodeKind::Offset {
            quantity: Quantity::Temperature,
            delta: -5.0,
        },
        Quantity::Temperature,
    )
    .validate()
    .unwrap();
    assert_eq!(
        probe(&g, 50.0, Quantity::Temperature, &mut EvalState::new()),
        45.0
    );

    let g = pipeline(
        NodeKind::Scale {
            quantity: Quantity::Duty,
            factor: 0.5,
        },
        Quantity::Duty,
    )
    .validate()
    .unwrap();
    assert_eq!(probe(&g, 80.0, Quantity::Duty, &mut EvalState::new()), 40.0);
}

// --- PID -----------------------------------------------------------------------------

#[test]
fn pid_is_proportional_with_no_derivative_kick_on_the_first_sample() {
    let g = pipeline(
        NodeKind::Pid {
            quantity: Quantity::Temperature,
            setpoint: 50.0,
            kp: 2.0,
            ki: 0.0,
            kd: 100.0,
            integral_limit: 0.0,
        },
        Quantity::Temperature,
    )
    .validate()
    .unwrap();

    // 10 degrees over setpoint at kp=2 is 20 % duty. A large kd must contribute nothing
    // on the first sample, or every start would slam the fans to full.
    assert_eq!(
        probe(&g, 60.0, Quantity::Temperature, &mut EvalState::new()),
        20.0
    );
}

#[test]
fn pid_integral_is_bounded_by_its_contribution() {
    let g = pipeline(
        NodeKind::Pid {
            quantity: Quantity::Temperature,
            setpoint: 50.0,
            kp: 0.0,
            ki: 1.0,
            kd: 0.0,
            integral_limit: 15.0,
        },
        Quantity::Temperature,
    )
    .validate()
    .unwrap();
    let mut state = EvalState::new();

    // Hold a large error for a long time. The integral must saturate at the configured
    // contribution rather than winding up and commanding full duty for minutes after the
    // load has gone away.
    let mut y = 0.0;
    for _ in 0..2000 {
        y = probe(&g, 90.0, Quantity::Temperature, &mut state);
    }
    assert!(
        (y - 15.0).abs() < 1e-6,
        "integral should saturate at 15, got {y}"
    );
}

#[test]
fn pid_does_not_integrate_across_a_sensor_fault() {
    let g = pipeline(
        NodeKind::Pid {
            quantity: Quantity::Temperature,
            setpoint: 50.0,
            kp: 0.0,
            ki: 1.0,
            kd: 10.0,
            integral_limit: 100.0,
        },
        Quantity::Temperature,
    )
    .validate()
    .unwrap();
    let mut state = EvalState::new();

    probe(&g, 60.0, Quantity::Temperature, &mut state);
    let faulted = g.tick_t(&SensorReadings::new(), &mut state);
    assert!(!faulted.wire_values[&PortRef::new("node", "out")].is_trustworthy());

    // The derivative restarts clean after a blind period rather than seeing a huge jump
    // from the last pre-fault error.
    assert!(probe(&g, 60.0, Quantity::Temperature, &mut state).is_finite());
}

// --- Catalogue hygiene ---------------------------------------------------------------

/// One instance of every node kind, so catalogue-wide properties can be asserted.
fn every_kind() -> Vec<NodeKind> {
    vec![
        NodeKind::Sensor {
            sensor_id: "s".into(),
            quantity: Quantity::Temperature,
        },
        NodeKind::Constant {
            quantity: Quantity::Duty,
            value: 0.0,
        },
        NodeKind::Curve {
            input: Quantity::Temperature,
            points: vec![CurvePoint { x: 0.0, y: 0.0 }],
        },
        NodeKind::Mix {
            quantity: Quantity::Duty,
            mode: MixMode::Max,
        },
        NodeKind::Clamp {
            quantity: Quantity::Duty,
            min: 0.0,
            max: 1.0,
        },
        NodeKind::Offset {
            quantity: Quantity::Duty,
            delta: 0.0,
        },
        NodeKind::Scale {
            quantity: Quantity::Duty,
            factor: 1.0,
        },
        NodeKind::Reinterpret {
            from: Quantity::Load,
            to: Quantity::Duty,
        },
        NodeKind::RateLimit {
            quantity: Quantity::Duty,
            max_delta_per_second: 1.0,
        },
        NodeKind::LowPass {
            quantity: Quantity::Duty,
            tau_seconds: 1.0,
        },
        NodeKind::MovingAverage {
            quantity: Quantity::Duty,
            samples: 2,
        },
        NodeKind::Hold {
            quantity: Quantity::Duty,
            band: 1.0,
        },
        NodeKind::Comparator {
            quantity: Quantity::Duty,
            threshold: 0.0,
            deadband: 0.0,
            direction: Compare::Above,
        },
        NodeKind::Select {
            quantity: Quantity::Duty,
        },
        NodeKind::Pid {
            quantity: Quantity::Temperature,
            setpoint: 0.0,
            kp: 0.0,
            ki: 0.0,
            kd: 0.0,
            integral_limit: 0.0,
        },
        NodeKind::FanOutput {
            channel: "c".into(),
        },
    ]
}

#[test]
fn every_node_kind_is_distinctly_labelled_and_has_ports() {
    let kinds = every_kind();

    let mut labels: Vec<&str> = kinds.iter().map(|k| k.default_label()).collect();
    let total = labels.len();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), total, "duplicate node labels: {labels:?}");

    for kind in &kinds {
        let spec = kind.spec();
        assert!(
            !spec.inputs.is_empty() || !spec.outputs.is_empty(),
            "{kind:?} declares no ports"
        );
        // Port keys must be unique per side, or edges become ambiguous.
        let mut keys: Vec<&str> = spec.inputs.iter().map(|p| p.key).collect();
        let n = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), n, "{kind:?} has duplicate input port keys");
    }
}

#[test]
fn every_node_kind_round_trips_through_serde() {
    // Profiles are persisted as JSON. A variant that cannot round-trip is a profile that
    // silently loses configuration on reload.
    for kind in every_kind() {
        let json = serde_json::to_string(&kind).expect("serialize");
        let back: NodeKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(kind, back, "round-trip changed {kind:?}");
    }
}
