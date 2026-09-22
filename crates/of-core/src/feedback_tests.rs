//! Tests for delayed ports and feedback.
//!
//! The claim under test: a fan's tachometer can steer the fan it measures, and that is
//! not a cycle — because the loop is broken by the hardware, not by a modelling trick.
//! We read sensors, then evaluate, then write duties, so a speed reading necessarily
//! predates this tick's duty.

use std::collections::BTreeMap;

use super::*;
use of_units::Quantity;

const CHANNEL: &str = "chip/pwm/1";
const TACH: &str = "chip/fan/1";

fn tachometers() -> BTreeMap<String, String> {
    BTreeMap::from([(CHANNEL.to_owned(), TACH.to_owned())])
}

fn readings(pairs: &[(&str, Quantity, f64)]) -> SensorReadings {
    pairs
        .iter()
        .map(|&(k, q, v)| (k.to_owned(), Value::raw(q, v)))
        .collect()
}

fn tick(compiled: &CompiledGraph, sensors: &SensorReadings, state: &mut EvalState) -> TickResult {
    let tachs = tachometers();
    compiled.tick(
        &TickInput::new(sensors, 0.1).with_tachometers(&tachs),
        state,
    )
}

fn fan() -> NodeKind {
    NodeKind::FanOutput {
        channel: CHANNEL.into(),
    }
}

// --- The tachometer is a delayed source ------------------------------------------------

#[test]
fn a_fan_reports_the_speed_its_tachometer_measured() {
    let mut g = Graph::default();
    g.insert("k", NodeKind::Constant { value: 60.0 });
    g.insert("fan", fan());
    g.connect(PortRef::new("k", "out"), PortRef::new("fan", "duty"));

    let compiled = g.validate().unwrap();
    let out = tick(
        &compiled,
        &readings(&[(TACH, Quantity::Rpm, 1450.0)]),
        &mut EvalState::new(),
    );

    let rpm = out.wire_values[&PortRef::new("fan", "rpm")];
    assert_eq!(rpm.quantity, Quantity::Rpm);
    assert_eq!(rpm.scalar, 1450.0);
    // And it still drove the channel.
    assert_eq!(out.commands[CHANNEL].scalar, 60.0);
}

#[test]
fn a_channel_with_no_tachometer_reports_a_fault_not_a_zero() {
    let mut g = Graph::default();
    g.insert("k", NodeKind::Constant { value: 60.0 });
    g.insert("fan", fan());
    g.connect(PortRef::new("k", "out"), PortRef::new("fan", "duty"));

    let compiled = g.validate().unwrap();
    // No tachometer map supplied at all.
    let out = compiled.tick_with(&SensorReadings::new(), 0.1, &mut EvalState::new());

    let rpm = out.wire_values[&PortRef::new("fan", "rpm")];
    assert!(
        !rpm.is_trustworthy(),
        "an unmeasured speed must not read as a stopped fan"
    );
}

#[test]
fn a_tachometer_reporting_the_wrong_quantity_is_refused() {
    let mut g = Graph::default();
    g.insert("k", NodeKind::Constant { value: 60.0 });
    g.insert("fan", fan());
    g.connect(PortRef::new("k", "out"), PortRef::new("fan", "duty"));

    let compiled = g.validate().unwrap();
    // The sensor re-enumerated as a temperature.
    let out = tick(
        &compiled,
        &readings(&[(TACH, Quantity::Temperature, 42.0)]),
        &mut EvalState::new(),
    );

    assert!(!out.wire_values[&PortRef::new("fan", "rpm")].is_trustworthy());
}

// --- Feedback, without a delay node ----------------------------------------------------

/// Stall detection: if the fan is not turning, drive it harder.
///
/// This is the shape that motivated delayed ports, and it must compile as written —
/// with no artificial delay node inserted to make the graph acyclic.
fn stall_detector() -> Graph {
    let mut g = Graph::default();
    g.insert("fan", fan());
    g.insert(
        "stalled",
        NodeKind::Comparator {
            threshold: 200.0,
            deadband: 50.0,
            direction: Compare::Below,
        },
    );
    g.insert("boost", NodeKind::Constant { value: 100.0 });
    g.insert("normal", NodeKind::Constant { value: 30.0 });
    g.insert("sel", NodeKind::Select);

    // The fan's own speed decides whether to boost it.
    g.connect(PortRef::new("fan", "rpm"), PortRef::new("stalled", "in"));
    g.connect(PortRef::new("stalled", "out"), PortRef::new("sel", "when"));
    g.connect(PortRef::new("boost", "out"), PortRef::new("sel", "if_true"));
    g.connect(
        PortRef::new("normal", "out"),
        PortRef::new("sel", "if_false"),
    );
    g.connect(PortRef::new("sel", "out"), PortRef::new("fan", "duty"));
    g
}

#[test]
fn a_fan_can_be_steered_by_its_own_tachometer() {
    let compiled = stall_detector()
        .validate()
        .expect("feedback through a delayed port is not a cycle");

    let mut state = EvalState::new();

    // Spinning: normal duty.
    let spinning = tick(
        &compiled,
        &readings(&[(TACH, Quantity::Rpm, 900.0)]),
        &mut state,
    );
    assert_eq!(spinning.commands[CHANNEL].scalar, 30.0);

    // Stalled: the same graph now commands full duty.
    let stalled = tick(
        &compiled,
        &readings(&[(TACH, Quantity::Rpm, 0.0)]),
        &mut state,
    );
    assert_eq!(stalled.commands[CHANNEL].scalar, 100.0);
}

#[test]
fn the_feedback_path_does_not_need_a_delay_node() {
    // Stated as its own test because it is the design claim, not an implementation
    // detail: the delay is physical, so the model should not demand a synthetic one.
    let graph = stall_detector();
    assert!(
        !graph
            .nodes
            .values()
            .any(|n| matches!(n.kind, NodeKind::Delay { .. })),
        "the graph under test deliberately contains no delay node"
    );
    assert!(graph.validate().is_ok());
}

#[test]
fn a_loop_with_no_delayed_edge_is_still_rejected() {
    // The escape hatch must not have quietly disabled cycle detection.
    let mut g = Graph::default();
    g.insert(
        "a",
        NodeKind::Clamp {
            min: 0.0,
            max: 100.0,
        },
    );
    g.insert("b", NodeKind::Offset { delta: 1.0 });
    g.connect(PortRef::new("a", "out"), PortRef::new("b", "in"));
    g.connect(PortRef::new("b", "out"), PortRef::new("a", "in"));

    let errors = g.validate().unwrap_err();
    assert!(
        errors.iter().any(|e| matches!(e, GraphError::Cycle(_))),
        "{errors:?}"
    );
}

#[test]
fn a_delayed_port_is_reported_as_such() {
    let graph = stall_detector();
    assert!(graph.is_delayed_source(&PortRef::new("fan", "rpm")));
    // Ordinary outputs are not, or nothing would ever be ordered.
    assert!(!graph.is_delayed_source(&PortRef::new("sel", "out")));
    // Neither are inputs.
    assert!(!graph.is_delayed_source(&PortRef::new("fan", "duty")));
}

// --- The Delay node --------------------------------------------------------------------

fn delay_chain(seconds: f64) -> Graph {
    let mut g = Graph::default();
    g.insert(
        "t",
        NodeKind::Sensor {
            sensor_id: "cpu".into(),
            quantity: Quantity::Temperature,
        },
    );
    g.insert("delay", NodeKind::Delay { seconds });
    g.connect(PortRef::new("t", "out"), PortRef::new("delay", "in"));
    g
}

#[test]
fn a_delay_emits_nothing_until_it_has_something_to_emit() {
    let compiled = delay_chain(0.0).validate().unwrap();
    let mut state = EvalState::new();

    // Before any input has been seen, there is no past value. That is a fault, not a
    // zero: a fan must never be driven from a value that was never measured.
    let first = tick(
        &compiled,
        &readings(&[("cpu", Quantity::Temperature, 50.0)]),
        &mut state,
    );
    assert!(!first.wire_values[&PortRef::new("delay", "out")].is_trustworthy());
}

#[test]
fn a_delay_emits_the_value_from_its_configured_age() {
    // Half a second at 0.1 s per tick is five ticks.
    let compiled = delay_chain(0.5).validate().unwrap();
    let mut state = EvalState::new();

    for step in 0..5 {
        tick(
            &compiled,
            &readings(&[("cpu", Quantity::Temperature, 10.0 + step as f64)]),
            &mut state,
        );
    }

    let out = tick(
        &compiled,
        &readings(&[("cpu", Quantity::Temperature, 99.0)]),
        &mut state,
    );
    let delayed = out.wire_values[&PortRef::new("delay", "out")];
    assert!(delayed.is_trustworthy());
    assert_eq!(delayed.scalar, 10.0, "should be emitting the oldest sample");
    assert_eq!(delayed.quantity, Quantity::Temperature);
}

#[test]
fn a_delay_can_break_a_loop_of_its_own() {
    // Its output is delayed too, so it is a general-purpose cycle breaker for feedback
    // that is not already broken by hardware.
    let mut g = Graph::default();
    g.insert("delay", NodeKind::Delay { seconds: 1.0 });
    g.insert("scale", NodeKind::Scale { factor: 0.5 });
    g.connect(PortRef::new("delay", "out"), PortRef::new("scale", "in"));
    g.connect(PortRef::new("scale", "out"), PortRef::new("delay", "in"));

    assert!(
        g.validate().is_ok(),
        "a delayed edge should break this loop"
    );
}
