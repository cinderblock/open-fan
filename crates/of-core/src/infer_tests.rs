//! Tests for type inference across the graph.
//!
//! The property under test throughout: a generic node is generic until something
//! concrete decides it, and once decided it is decided everywhere consistently.

use super::*;
use of_units::Quantity;

fn sensor(id: &str, q: Quantity) -> NodeKind {
    NodeKind::Sensor {
        sensor_id: id.into(),
        quantity: q,
    }
}

fn clamp() -> NodeKind {
    NodeKind::Clamp {
        min: 0.0,
        max: 100.0,
    }
}

/// The resolved type of one port in a graph that need not be fully valid.
fn ty(graph: &Graph, node: &str, port: &str) -> Option<Quantity> {
    let (types, _) = infer::infer(graph);
    types[&PortRef::new(node, port)]
}

fn errors_of(graph: &Graph) -> Vec<GraphError> {
    infer::infer(graph).1
}

#[test]
fn a_generic_chain_with_nothing_attached_stays_generic() {
    // Half-built graphs are normal. Nothing here is an error; the editor simply draws
    // these ports as undecided.
    let mut g = Graph::default();
    g.insert("a", clamp());
    g.insert("b", NodeKind::Offset { delta: 1.0 });
    g.connect(PortRef::new("a", "out"), PortRef::new("b", "in"));

    assert_eq!(ty(&g, "a", "in"), None);
    assert_eq!(ty(&g, "b", "out"), None);
    assert!(errors_of(&g).is_empty(), "generic is not an error");
}

#[test]
fn a_sensor_anchors_everything_downstream_of_it() {
    let mut g = Graph::default();
    g.insert("t", sensor("cpu", Quantity::Temperature));
    g.insert("hold", NodeKind::Hold { band: 1.0 });
    g.insert("avg", NodeKind::MovingAverage { samples: 4 });
    g.connect(PortRef::new("t", "out"), PortRef::new("hold", "in"));
    g.connect(PortRef::new("hold", "out"), PortRef::new("avg", "in"));

    assert_eq!(ty(&g, "hold", "in"), Some(Quantity::Temperature));
    assert_eq!(ty(&g, "hold", "out"), Some(Quantity::Temperature));
    // Propagates through an arbitrary number of hops.
    assert_eq!(ty(&g, "avg", "out"), Some(Quantity::Temperature));
    assert!(errors_of(&g).is_empty());
}

#[test]
fn a_fan_output_anchors_everything_upstream_of_it() {
    // Inference is not directional: a concrete sink decides its generic feeders just as
    // a concrete source decides its consumers.
    let mut g = Graph::default();
    g.insert(
        "limit",
        NodeKind::RateLimit {
            max_delta_per_second: 10.0,
        },
    );
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f1".into(),
        },
    );
    g.connect(PortRef::new("limit", "out"), PortRef::new("fan", "duty"));

    assert_eq!(ty(&g, "limit", "out"), Some(Quantity::Duty));
    assert_eq!(ty(&g, "limit", "in"), Some(Quantity::Duty));
}

#[test]
fn a_constant_takes_the_type_of_whatever_it_feeds() {
    let mut g = Graph::default();
    g.insert("k", NodeKind::Constant { value: 40.0 });
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f1".into(),
        },
    );
    g.connect(PortRef::new("k", "out"), PortRef::new("fan", "duty"));

    assert_eq!(ty(&g, "k", "out"), Some(Quantity::Duty));
}

#[test]
fn an_unattached_constant_is_generic_and_evaluates_dimensionlessly() {
    let mut g = Graph::default();
    g.insert("k", NodeKind::Constant { value: 40.0 });
    assert_eq!(ty(&g, "k", "out"), None);

    // It still evaluates rather than faulting: a value with no decided unit is a
    // perfectly good number, it just has nothing to flow into yet.
    let compiled = g.validate().unwrap();
    let out = compiled.tick_with(&SensorReadings::new(), 0.1, &mut EvalState::new());
    let value = out.wire_values[&PortRef::new("k", "out")];
    assert_eq!(value.scalar, 40.0);
    assert_eq!(value.quantity, Quantity::Ratio);
}

#[test]
fn conflicting_anchors_are_reported_against_the_node_that_cannot_satisfy_them() {
    // A clamp fed a temperature but driving a fan would have to be both at once.
    let mut g = Graph::default();
    g.insert("t", sensor("cpu", Quantity::Temperature));
    g.insert("clamp", clamp());
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f1".into(),
        },
    );
    g.connect(PortRef::new("t", "out"), PortRef::new("clamp", "in"));
    g.connect(PortRef::new("clamp", "out"), PortRef::new("fan", "duty"));

    let errors = errors_of(&g);
    assert!(
        errors.iter().any(|e| matches!(
            e,
            GraphError::TypeConflict { node, .. } if node.0 == "clamp"
        )),
        "{errors:?}"
    );
    assert!(g.validate().is_err(), "the graph must not compile");
}

#[test]
fn a_mixer_fed_two_different_types_is_rejected() {
    let mut g = Graph::default();
    g.insert("cpu", sensor("cpu", Quantity::Temperature));
    g.insert("fan_rpm", sensor("rpm", Quantity::Rpm));
    g.insert("mix", NodeKind::Mix { mode: MixMode::Max });
    g.connect(PortRef::new("cpu", "out"), PortRef::new("mix", "in"));
    g.connect(PortRef::new("fan_rpm", "out"), PortRef::new("mix", "in"));

    let errors = errors_of(&g);
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, GraphError::TypeConflict { .. })),
        "{errors:?}"
    );
}

#[test]
fn a_mixer_fed_one_type_is_fine_however_many_inputs() {
    let mut g = Graph::default();
    g.insert("cpu", sensor("cpu", Quantity::Temperature));
    g.insert("gpu", sensor("gpu", Quantity::Temperature));
    g.insert("vrm", sensor("vrm", Quantity::Temperature));
    g.insert("mix", NodeKind::Mix { mode: MixMode::Max });
    for src in ["cpu", "gpu", "vrm"] {
        g.connect(PortRef::new(src, "out"), PortRef::new("mix", "in"));
    }

    assert!(errors_of(&g).is_empty());
    assert_eq!(ty(&g, "mix", "out"), Some(Quantity::Temperature));
}

#[test]
fn variables_are_scoped_per_node() {
    // Two clamps both call their variable T. Deciding one must not decide the other.
    let mut g = Graph::default();
    g.insert("t", sensor("cpu", Quantity::Temperature));
    g.insert("a", clamp());
    g.insert("b", clamp());
    g.connect(PortRef::new("t", "out"), PortRef::new("a", "in"));

    assert_eq!(ty(&g, "a", "out"), Some(Quantity::Temperature));
    assert_eq!(ty(&g, "b", "out"), None, "b is untouched and stays generic");
}

#[test]
fn a_curve_always_emits_a_duty_whatever_it_reads() {
    let mut g = Graph::default();
    g.insert("t", sensor("cpu", Quantity::Temperature));
    g.insert(
        "curve",
        NodeKind::Curve {
            points: vec![
                CurvePoint { x: 30.0, y: 20.0 },
                CurvePoint { x: 80.0, y: 100.0 },
            ],
        },
    );
    g.connect(PortRef::new("t", "out"), PortRef::new("curve", "in"));

    assert_eq!(ty(&g, "curve", "in"), Some(Quantity::Temperature));
    assert_eq!(ty(&g, "curve", "out"), Some(Quantity::Duty));

    // And it is the node that makes temperature-to-duty legal at all.
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f1".into(),
        },
    );
    g.connect(PortRef::new("curve", "out"), PortRef::new("fan", "duty"));
    assert!(g.validate().is_ok());
}

#[test]
fn select_unifies_its_branches_but_not_its_condition() {
    let mut g = Graph::default();
    g.insert("flag", sensor("f", Quantity::Boolean));
    g.insert("quiet", NodeKind::Constant { value: 30.0 });
    g.insert("sel", NodeKind::Select);
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f1".into(),
        },
    );
    g.connect(PortRef::new("flag", "out"), PortRef::new("sel", "when"));
    g.connect(
        PortRef::new("quiet", "out"),
        PortRef::new("sel", "if_false"),
    );
    g.connect(PortRef::new("sel", "out"), PortRef::new("fan", "duty"));

    // The fan decides the branches...
    assert_eq!(ty(&g, "sel", "if_true"), Some(Quantity::Duty));
    assert_eq!(ty(&g, "sel", "if_false"), Some(Quantity::Duty));
    // ...and the constant feeding one of them follows.
    assert_eq!(ty(&g, "quiet", "out"), Some(Quantity::Duty));
    // The condition is concrete and unaffected.
    assert_eq!(ty(&g, "sel", "when"), Some(Quantity::Boolean));
}

#[test]
fn a_generic_chain_carries_the_right_type_at_runtime_too() {
    // Inference decides the colours; evaluation has to agree, or a value could reach a
    // sink tagged as something it is not.
    let mut g = Graph::default();
    g.insert("t", sensor("cpu", Quantity::Temperature));
    g.insert("hold", NodeKind::Hold { band: 0.5 });
    g.insert(
        "curve",
        NodeKind::Curve {
            points: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 100.0, y: 100.0 },
            ],
        },
    );
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f1".into(),
        },
    );
    g.connect(PortRef::new("t", "out"), PortRef::new("hold", "in"));
    g.connect(PortRef::new("hold", "out"), PortRef::new("curve", "in"));
    g.connect(PortRef::new("curve", "out"), PortRef::new("fan", "duty"));

    let compiled = g.validate().unwrap();
    let readings =
        SensorReadings::from([("cpu".to_owned(), Value::raw(Quantity::Temperature, 55.0))]);
    let out = compiled.tick_with(&readings, 0.1, &mut EvalState::new());

    let held = out.wire_values[&PortRef::new("hold", "out")];
    assert_eq!(
        held.quantity,
        Quantity::Temperature,
        "the hold carried the type through"
    );
    assert_eq!(held.scalar, 55.0);
    // Interpolation is floating point, so compare with a tolerance rather than exactly.
    assert!((out.commands["f1"].scalar - 55.0).abs() < 1e-9);
}

#[test]
fn reinterpret_is_still_the_only_way_to_change_type() {
    // Generic nodes pass a type along; they never convert one. Crossing types stays a
    // visible, deliberate act.
    let mut g = Graph::default();
    g.insert("load", sensor("l", Quantity::Load));
    g.insert("scale", NodeKind::Scale { factor: 1.0 });
    g.insert(
        "fan",
        NodeKind::FanOutput {
            channel: "f1".into(),
        },
    );
    g.connect(PortRef::new("load", "out"), PortRef::new("scale", "in"));
    g.connect(PortRef::new("scale", "out"), PortRef::new("fan", "duty"));
    assert!(
        g.validate().is_err(),
        "a scale cannot quietly turn a load into a duty"
    );

    let mut g = Graph::default();
    g.insert("load", sensor("l", Quantity::Load));
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
    assert!(g.validate().is_ok());
}

#[test]
fn a_compiled_graph_exposes_the_types_it_settled_on() {
    let mut g = Graph::default();
    g.insert("t", sensor("cpu", Quantity::Temperature));
    g.insert("hold", NodeKind::Hold { band: 1.0 });
    g.connect(PortRef::new("t", "out"), PortRef::new("hold", "in"));

    let compiled = g.validate().unwrap();
    assert_eq!(
        compiled.types()[&PortRef::new("hold", "out")],
        Some(Quantity::Temperature)
    );
}
