use super::*;
use of_units::Quantity;

fn curve(points: &[(f64, f64)]) -> NodeKind {
    NodeKind::Curve {
        input: Quantity::Temperature,
        points: points
            .iter()
            .map(|&(x, y)| node::CurvePoint { x, y })
            .collect(),
    }
}

/// The canonical minimal graph: one sensor, through one curve, into one fan.
fn simple_graph() -> Graph {
    let mut g = Graph::default();
    g.insert(
        "cpu",
        NodeKind::Sensor {
            sensor_id: "cpu/package".into(),
            quantity: Quantity::Temperature,
        },
    );
    g.insert("curve", curve(&[(30.0, 20.0), (70.0, 100.0)]));
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "sysfan1".into(),
        },
    );
    g.connect(PortRef::new("cpu", "out"), PortRef::new("curve", "in"));
    g.connect(PortRef::new("curve", "out"), PortRef::new("fan", "duty"));
    g
}

fn readings(pairs: &[(&str, Quantity, f64)]) -> SensorReadings {
    pairs
        .iter()
        .map(|&(k, q, v)| (k.to_owned(), Value::raw(q, v)))
        .collect()
}

#[test]
fn a_well_formed_graph_compiles_and_runs() {
    let compiled = simple_graph().validate().expect("graph should be valid");
    let mut state = EvalState::new();
    let out = compiled.tick(
        &readings(&[("cpu/package", Quantity::Temperature, 50.0)]),
        &mut state,
    );

    // Halfway along the curve.
    assert_eq!(out.commands["sysfan1"].scalar, 60.0);
    assert!(out.faulted_channels.is_empty());
}

#[test]
fn temperature_cannot_be_wired_straight_into_a_fan() {
    // The thesis of the whole project, as an executable assertion.
    let mut g = Graph::default();
    g.insert(
        "cpu",
        NodeKind::Sensor {
            sensor_id: "cpu/package".into(),
            quantity: Quantity::Temperature,
        },
    );
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "sysfan1".into(),
        },
    );
    g.connect(PortRef::new("cpu", "out"), PortRef::new("fan", "duty"));

    let errors = g
        .validate()
        .expect_err("temperature -> duty must be rejected");
    assert!(
        errors.iter().any(|e| matches!(
            e,
            GraphError::TypeMismatch {
                source_ty: Quantity::Temperature,
                sink_ty: Quantity::Duty,
                ..
            }
        )),
        "{errors:?}"
    );
}

#[test]
fn load_and_duty_do_not_interchange_despite_sharing_a_unit() {
    let mut g = Graph::default();
    g.insert(
        "gpu",
        NodeKind::Sensor {
            sensor_id: "gpu/load".into(),
            quantity: Quantity::Load,
        },
    );
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "sysfan1".into(),
        },
    );
    g.connect(PortRef::new("gpu", "out"), PortRef::new("fan", "duty"));

    let errors = g.validate().expect_err("load -> duty must be rejected");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, GraphError::TypeMismatch { .. })),
        "{errors:?}"
    );
}

#[test]
fn validation_reports_every_error_not_just_the_first() {
    let mut g = Graph::default();
    g.insert(
        "cpu",
        NodeKind::Sensor {
            sensor_id: "cpu".into(),
            quantity: Quantity::Temperature,
        },
    );
    g.insert(
        "fan_a",
        NodeKind::FanOutput {
            channel: "a".into(),
        },
    );
    g.insert(
        "fan_b",
        NodeKind::FanOutput {
            channel: "b".into(),
        },
    );
    g.connect(PortRef::new("cpu", "out"), PortRef::new("fan_a", "duty"));
    g.connect(PortRef::new("cpu", "out"), PortRef::new("fan_b", "duty"));

    let errors = g.validate().unwrap_err();
    let mismatches = errors
        .iter()
        .filter(|e| matches!(e, GraphError::TypeMismatch { .. }))
        .count();
    assert_eq!(
        mismatches, 2,
        "editor needs to mark both bad edges at once: {errors:?}"
    );
}

#[test]
fn a_cycle_is_rejected() {
    let mut g = Graph::default();
    g.insert(
        "a",
        NodeKind::Clamp {
            quantity: Quantity::Duty,
            min: 0.0,
            max: 100.0,
        },
    );
    g.insert(
        "b",
        NodeKind::Clamp {
            quantity: Quantity::Duty,
            min: 0.0,
            max: 100.0,
        },
    );
    g.connect(PortRef::new("a", "out"), PortRef::new("b", "in"));
    g.connect(PortRef::new("b", "out"), PortRef::new("a", "in"));

    let errors = g.validate().unwrap_err();
    assert!(
        errors.iter().any(|e| matches!(e, GraphError::Cycle(_))),
        "{errors:?}"
    );
}

#[test]
fn an_unconnected_fan_input_is_a_validation_error() {
    let mut g = Graph::default();
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "sysfan1".into(),
        },
    );

    let errors = g.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, GraphError::MissingInput(_))),
        "{errors:?}"
    );
}

#[test]
fn a_plain_input_rejects_two_producers_but_a_mixer_accepts_many() {
    let mut g = Graph::default();
    g.insert(
        "c1",
        NodeKind::Constant {
            quantity: Quantity::Duty,
            value: 10.0,
        },
    );
    g.insert(
        "c2",
        NodeKind::Constant {
            quantity: Quantity::Duty,
            value: 20.0,
        },
    );
    g.insert(
        "clamp",
        NodeKind::Clamp {
            quantity: Quantity::Duty,
            min: 0.0,
            max: 100.0,
        },
    );
    g.connect(PortRef::new("c1", "out"), PortRef::new("clamp", "in"));
    g.connect(PortRef::new("c2", "out"), PortRef::new("clamp", "in"));

    let errors = g.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, GraphError::InputOverSubscribed(_))),
        "{errors:?}"
    );

    let mut g = Graph::default();
    g.insert(
        "c1",
        NodeKind::Constant {
            quantity: Quantity::Duty,
            value: 10.0,
        },
    );
    g.insert(
        "c2",
        NodeKind::Constant {
            quantity: Quantity::Duty,
            value: 20.0,
        },
    );
    g.insert(
        "mix",
        NodeKind::Mix {
            quantity: Quantity::Duty,
            mode: MixMode::Max,
        },
    );
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "sysfan1".into(),
        },
    );
    g.connect(PortRef::new("c1", "out"), PortRef::new("mix", "in"));
    g.connect(PortRef::new("c2", "out"), PortRef::new("mix", "in"));
    g.connect(PortRef::new("mix", "out"), PortRef::new("fan", "duty"));

    let compiled = g.validate().expect("variadic input takes many producers");
    let out = compiled.tick(&SensorReadings::new(), &mut EvalState::new());
    assert_eq!(out.commands["sysfan1"].scalar, 20.0);
}

#[test]
fn wiring_an_output_into_an_output_is_caught() {
    let mut g = simple_graph();
    g.connect(PortRef::new("curve", "out"), PortRef::new("cpu", "out"));
    let errors = g.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, GraphError::NotAnInput { .. })),
        "{errors:?}"
    );
}

#[test]
fn unknown_nodes_and_ports_are_caught() {
    let mut g = simple_graph();
    g.connect(PortRef::new("nope", "out"), PortRef::new("fan", "duty"));
    g.connect(PortRef::new("cpu", "bogus"), PortRef::new("fan", "duty"));
    let errors = g.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, GraphError::UnknownNode(_))),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, GraphError::UnknownPort(..))),
        "{errors:?}"
    );
}

// --- Safety behaviour ----------------------------------------------------------------

#[test]
fn a_missing_sensor_faults_the_channel_rather_than_commanding_a_number() {
    let compiled = simple_graph().validate().unwrap();
    let mut state = EvalState::new();
    // No reading for "cpu/package" at all.
    let out = compiled.tick(&SensorReadings::new(), &mut state);

    assert!(
        out.commands.is_empty(),
        "no duty may be commanded from a missing sensor"
    );
    assert!(
        out.faulted_channels.contains("sysfan1"),
        "the channel must fault so the engine applies its failsafe"
    );
}

#[test]
fn a_nan_reading_does_not_get_laundered_into_a_duty() {
    let compiled = simple_graph().validate().unwrap();
    let mut state = EvalState::new();
    let out = compiled.tick(
        &readings(&[("cpu/package", Quantity::Temperature, f64::NAN)]),
        &mut state,
    );

    assert!(out.commands.is_empty());
    assert!(out.faulted_channels.contains("sysfan1"));
}

#[test]
fn one_dead_sensor_poisons_a_mix_instead_of_being_averaged_away() {
    let mut g = Graph::default();
    g.insert(
        "a",
        NodeKind::Sensor {
            sensor_id: "a".into(),
            quantity: Quantity::Temperature,
        },
    );
    g.insert(
        "b",
        NodeKind::Sensor {
            sensor_id: "b".into(),
            quantity: Quantity::Temperature,
        },
    );
    g.insert(
        "mix",
        NodeKind::Mix {
            quantity: Quantity::Temperature,
            mode: MixMode::Average,
        },
    );
    g.insert("curve", curve(&[(30.0, 20.0), (70.0, 100.0)]));
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "sysfan1".into(),
        },
    );
    g.connect(PortRef::new("a", "out"), PortRef::new("mix", "in"));
    g.connect(PortRef::new("b", "out"), PortRef::new("mix", "in"));
    g.connect(PortRef::new("mix", "out"), PortRef::new("curve", "in"));
    g.connect(PortRef::new("curve", "out"), PortRef::new("fan", "duty"));

    let compiled = g.validate().unwrap();
    let mut state = EvalState::new();
    // "a" reports 90 °C; "b" is dead. Averaging 90 with a substituted 0 would command a
    // dangerously low duty, which is exactly the failure this test exists to prevent.
    let out = compiled.tick(&readings(&[("a", Quantity::Temperature, 90.0)]), &mut state);

    assert!(out.commands.is_empty());
    assert!(out.faulted_channels.contains("sysfan1"));
}

#[test]
fn a_clamp_cannot_disguise_a_broken_reading() {
    let mut g = Graph::default();
    g.insert(
        "t",
        NodeKind::Sensor {
            sensor_id: "t".into(),
            quantity: Quantity::Temperature,
        },
    );
    g.insert(
        "clamp",
        NodeKind::Clamp {
            quantity: Quantity::Temperature,
            min: 20.0,
            max: 90.0,
        },
    );
    g.insert("curve", curve(&[(30.0, 20.0), (70.0, 100.0)]));
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "sysfan1".into(),
        },
    );
    g.connect(PortRef::new("t", "out"), PortRef::new("clamp", "in"));
    g.connect(PortRef::new("clamp", "out"), PortRef::new("curve", "in"));
    g.connect(PortRef::new("curve", "out"), PortRef::new("fan", "duty"));

    let compiled = g.validate().unwrap();
    let out = compiled.tick(&SensorReadings::new(), &mut EvalState::new());
    assert!(
        out.faulted_channels.contains("sysfan1"),
        "clamp must pass the fault through"
    );
}

// --- Curve and transform behaviour ---------------------------------------------------

#[test]
fn curve_holds_its_endpoints_flat_outside_the_domain() {
    let compiled = simple_graph().validate().unwrap();
    let mut state = EvalState::new();

    let cold = compiled.tick(
        &readings(&[("cpu/package", Quantity::Temperature, 5.0)]),
        &mut state,
    );
    assert_eq!(cold.commands["sysfan1"].scalar, 20.0);

    let hot = compiled.tick(
        &readings(&[("cpu/package", Quantity::Temperature, 120.0)]),
        &mut state,
    );
    assert_eq!(hot.commands["sysfan1"].scalar, 100.0);
}

#[test]
fn curve_points_need_not_be_stored_in_order() {
    let mut g = Graph::default();
    g.insert(
        "t",
        NodeKind::Sensor {
            sensor_id: "t".into(),
            quantity: Quantity::Temperature,
        },
    );
    g.insert("curve", curve(&[(70.0, 100.0), (30.0, 20.0)])); // reversed
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f".into(),
        },
    );
    g.connect(PortRef::new("t", "out"), PortRef::new("curve", "in"));
    g.connect(PortRef::new("curve", "out"), PortRef::new("fan", "duty"));

    let compiled = g.validate().unwrap();
    let out = compiled.tick(
        &readings(&[("t", Quantity::Temperature, 50.0)]),
        &mut EvalState::new(),
    );
    assert_eq!(out.commands["f"].scalar, 60.0);
}

#[test]
fn rate_limit_adopts_its_first_value_then_ramps() {
    let mut g = Graph::default();
    g.insert(
        "t",
        NodeKind::Sensor {
            sensor_id: "t".into(),
            quantity: Quantity::Temperature,
        },
    );
    g.insert("curve", curve(&[(30.0, 0.0), (80.0, 100.0)]));
    g.insert(
        "limit",
        NodeKind::RateLimit {
            quantity: Quantity::Duty,
            max_delta_per_tick: 5.0,
        },
    );
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f".into(),
        },
    );
    g.connect(PortRef::new("t", "out"), PortRef::new("curve", "in"));
    g.connect(PortRef::new("curve", "out"), PortRef::new("limit", "in"));
    g.connect(PortRef::new("limit", "out"), PortRef::new("fan", "duty"));

    let compiled = g.validate().unwrap();
    let mut state = EvalState::new();

    // First tick adopts immediately — a fresh start must not ramp up from zero.
    let first = compiled.tick(&readings(&[("t", Quantity::Temperature, 55.0)]), &mut state);
    assert_eq!(first.commands["f"].scalar, 50.0);

    // A step to 100 % is then limited to 5 points per tick.
    let second = compiled.tick(&readings(&[("t", Quantity::Temperature, 80.0)]), &mut state);
    assert_eq!(second.commands["f"].scalar, 55.0);
    let third = compiled.tick(&readings(&[("t", Quantity::Temperature, 80.0)]), &mut state);
    assert_eq!(third.commands["f"].scalar, 60.0);
}

#[test]
fn stale_node_state_is_dropped_when_the_graph_changes() {
    let mut g = Graph::default();
    g.insert(
        "limit",
        NodeKind::RateLimit {
            quantity: Quantity::Duty,
            max_delta_per_tick: 5.0,
        },
    );
    g.insert(
        "c",
        NodeKind::Constant {
            quantity: Quantity::Duty,
            value: 50.0,
        },
    );
    g.connect(PortRef::new("c", "out"), PortRef::new("limit", "in"));

    let compiled = g.validate().unwrap();
    let mut state = EvalState::new();
    compiled.tick(&SensorReadings::new(), &mut state);
    assert_eq!(state.nodes.len(), 2);

    let empty = Graph::default();
    state.retain_nodes(&empty);
    assert!(
        state.nodes.is_empty(),
        "state for deleted nodes must not accumulate"
    );
}

#[test]
fn wire_values_are_reported_for_the_ui() {
    let compiled = simple_graph().validate().unwrap();
    let out = compiled.tick(
        &readings(&[("cpu/package", Quantity::Temperature, 50.0)]),
        &mut EvalState::new(),
    );

    assert_eq!(out.wire_values[&PortRef::new("cpu", "out")].scalar, 50.0);
    assert_eq!(out.wire_values[&PortRef::new("curve", "out")].scalar, 60.0);
}

#[test]
fn evaluation_order_respects_dependencies() {
    let compiled = simple_graph().validate().unwrap();
    let order = compiled.order();
    let pos = |name: &str| order.iter().position(|n| n.0 == name).unwrap();
    assert!(pos("cpu") < pos("curve"));
    assert!(pos("curve") < pos("fan"));
}
