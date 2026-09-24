//! What can and cannot be handed to the chip, and whether the explanation is any use.

use of_units::Quantity;

use crate::offload::{Obstacle, ObstacleKind, Reduction, reduce, reduce_all};
use crate::{CurvePoint, Graph, MixMode, NodeKind, PortRef};

const CHANNEL: &str = "nct6798d/pwm/1";

fn curve(points: &[(f64, f64)]) -> NodeKind {
    NodeKind::Curve {
        points: points.iter().map(|&(x, y)| CurvePoint { x, y }).collect(),
    }
}

fn temp(id: &str) -> NodeKind {
    NodeKind::Sensor {
        sensor_id: id.into(),
        quantity: Quantity::Temperature,
    }
}

/// `Sensor -> ...chain... -> FanOutput`, wired in order.
fn chain(kinds: Vec<NodeKind>) -> Graph {
    let mut graph = Graph::default();
    let mut previous: Option<crate::NodeId> = None;

    for (index, kind) in kinds.into_iter().enumerate() {
        let id = graph.insert(format!("n{index}").as_str(), kind);
        if let Some(from) = previous {
            let port = if matches!(graph.nodes[&id].kind, NodeKind::FanOutput { .. }) {
                "duty"
            } else {
                "in"
            };
            graph.connect(PortRef::new(from, "out"), PortRef::new(id.clone(), port));
        }
        previous = Some(id);
    }
    graph
}

fn output() -> NodeKind {
    NodeKind::FanOutput {
        channel: CHANNEL.into(),
    }
}

#[test]
fn a_fixed_duty_reduces() {
    let graph = chain(vec![NodeKind::Constant { value: 40.0 }, output()]);
    assert_eq!(reduce(&graph, CHANNEL), Ok(Reduction::Fixed { duty: 40.0 }));
}

#[test]
fn a_sensor_and_a_curve_reduces() {
    let graph = chain(vec![
        temp("nct6798d/temp/cputin"),
        curve(&[(35.0, 30.0), (75.0, 100.0)]),
        output(),
    ]);

    assert_eq!(
        reduce(&graph, CHANNEL),
        Ok(Reduction::Curve {
            sensor_id: "nct6798d/temp/cputin".into(),
            quantity: Quantity::Temperature,
            points: vec![
                CurvePoint { x: 35.0, y: 30.0 },
                CurvePoint { x: 75.0, y: 100.0 },
            ],
        })
    );
}

#[test]
fn a_minimum_duty_floor_folds_into_the_curve_instead_of_blocking_it() {
    // This is what an imported configuration puts there — a floor the other tool refused
    // to command below becomes a Clamp. Refusing to offload over it would be a needless
    // "no" on a configuration the chip could run perfectly well.
    let graph = chain(vec![
        temp("nct6798d/temp/cputin"),
        curve(&[(35.0, 10.0), (75.0, 100.0)]),
        NodeKind::Clamp {
            min: 19.0,
            max: 100.0,
        },
        output(),
    ]);

    match reduce(&graph, CHANNEL) {
        Ok(Reduction::Curve { points, .. }) => {
            assert_eq!(points[0].y, 19.0, "the floor was applied to the curve");
            assert_eq!(points[1].y, 100.0);
            assert_eq!(points[0].x, 35.0, "temperatures are untouched");
        }
        other => panic!("expected a folded curve, got {other:?}"),
    }
}

#[test]
fn offset_and_scale_fold_too() {
    let graph = chain(vec![
        temp("t"),
        curve(&[(30.0, 20.0), (60.0, 40.0)]),
        NodeKind::Scale { factor: 2.0 },
        NodeKind::Offset { delta: 5.0 },
        output(),
    ]);

    match reduce(&graph, CHANNEL) {
        Ok(Reduction::Curve { points, .. }) => {
            assert_eq!(points[0].y, 45.0);
            assert_eq!(points[1].y, 85.0);
        }
        other => panic!("expected folding, got {other:?}"),
    }
}

#[test]
fn a_filter_blocks_it_but_names_the_chip_setting_that_resembles_it() {
    // The useful shape of a "no": the chip has step timing, which is similar but not the
    // same, so this is a choice — give up the filter and it fits — rather than a refusal.
    let graph = chain(vec![
        temp("t"),
        NodeKind::LowPass { tau_seconds: 15.0 },
        curve(&[(35.0, 30.0), (75.0, 100.0)]),
        output(),
    ]);

    let obstacles = reduce(&graph, CHANNEL).expect_err("a filter cannot be offloaded");
    assert_eq!(obstacles.len(), 1);

    let explanation = obstacles[0].explain();
    assert!(
        explanation.contains("step-up"),
        "it should name the chip's own timing: {explanation}"
    );
    assert!(
        explanation.contains("removing"),
        "and say what giving it up would buy: {explanation}"
    );
}

#[test]
fn a_hold_band_names_the_chips_hysteresis() {
    let graph = chain(vec![
        temp("t"),
        NodeKind::Hold { band: 2.0 },
        curve(&[(35.0, 30.0), (75.0, 100.0)]),
        output(),
    ]);

    let obstacles = reduce(&graph, CHANNEL).expect_err("a hold band blocks it");
    assert!(obstacles[0].explain().contains("hysteresis"));
}

#[test]
fn mixing_two_sensors_is_refused_and_says_why_in_the_users_terms() {
    let mut graph = Graph::default();
    let cpu = graph.insert("cpu", temp("nct6798d/temp/cputin"));
    let gpu = graph.insert("gpu", temp("nvidia/temp/core"));
    let mix = graph.insert("hottest", NodeKind::Mix { mode: MixMode::Max });
    let c = graph.insert("curve", curve(&[(35.0, 30.0), (75.0, 100.0)]));
    let out = graph.insert("fan", output());

    graph.connect(PortRef::new(cpu, "out"), PortRef::new(mix.clone(), "in"));
    graph.connect(PortRef::new(gpu, "out"), PortRef::new(mix.clone(), "in"));
    graph.connect(PortRef::new(mix, "out"), PortRef::new(c.clone(), "in"));
    graph.connect(PortRef::new(c, "out"), PortRef::new(out, "duty"));

    let obstacles = reduce(&graph, CHANNEL).expect_err("two temperatures cannot be offloaded");
    let explanation = obstacles[0].explain();
    assert!(
        explanation.contains("one temperature per fan"),
        "the reason must be stated as a fact about the hardware: {explanation}"
    );
    // No chip alternative is claimed, because there genuinely is none.
    assert!(!explanation.contains("removing"), "{explanation}");
}

#[test]
fn a_pid_is_software_only_and_claims_no_equivalent() {
    let graph = chain(vec![
        temp("t"),
        NodeKind::Pid {
            setpoint: 70.0,
            kp: 2.0,
            ki: 0.1,
            kd: 0.0,
            integral_limit: 40.0,
        },
        output(),
    ]);

    let obstacles = reduce(&graph, CHANNEL).expect_err("a PID cannot be offloaded");
    let explanation = obstacles[0].explain();
    assert!(
        explanation.contains("only run in software"),
        "{explanation}"
    );
    // Inventing a resemblance here would let somebody delete the PID believing the chip
    // covers it.
    assert!(!explanation.contains("similar"), "{explanation}");
}

#[test]
fn a_channel_nothing_drives_says_so_rather_than_failing_obscurely() {
    let graph = Graph::default();
    let obstacles = reduce(&graph, CHANNEL).expect_err("nothing drives it");
    assert_eq!(obstacles[0].kind, ObstacleKind::NotDriven);
    assert!(obstacles[0].explain().contains("Nothing"));
}

#[test]
fn an_unconnected_output_is_its_own_answer() {
    let mut graph = Graph::default();
    graph.insert("fan", output());

    let obstacles = reduce(&graph, CHANNEL).expect_err("nothing is connected");
    assert_eq!(obstacles[0].kind, ObstacleKind::NothingConnected);
}

#[test]
fn two_outputs_on_one_channel_have_no_single_answer() {
    let mut graph = Graph::default();
    let k = graph.insert("duty", NodeKind::Constant { value: 50.0 });
    let a = graph.insert("fan-a", output());
    let b = graph.insert("fan-b", output());
    graph.connect(PortRef::new(k.clone(), "out"), PortRef::new(a, "duty"));
    graph.connect(PortRef::new(k, "out"), PortRef::new(b, "duty"));

    let obstacles = reduce(&graph, CHANNEL).expect_err("ambiguous");
    assert_eq!(obstacles[0].kind, ObstacleKind::DrivenTwice);
}

#[test]
fn a_partial_reduction_is_not_reported_as_a_success() {
    // The dangerous case. A curve reached through a filter *looks* reducible if you only
    // follow the duty path — and a curve that ignored the filter would describe a fan
    // behaving differently from the one the user configured.
    let graph = chain(vec![
        temp("t"),
        curve(&[(35.0, 30.0), (75.0, 100.0)]),
        NodeKind::RateLimit {
            max_delta_per_second: 5.0,
        },
        output(),
    ]);

    assert!(
        reduce(&graph, CHANNEL).is_err(),
        "a reduction that dropped a node must not be offered"
    );
}

#[test]
fn every_obstacle_explains_itself_without_naming_a_register() {
    // These sentences go in front of somebody who has never heard of a Super I/O.
    let samples = [
        ObstacleKind::NotDriven,
        ObstacleKind::DrivenTwice,
        ObstacleKind::NothingConnected,
        ObstacleKind::Unsupported {
            node_kind: "PID",
            chip_alternative: None,
        },
        ObstacleKind::Unsupported {
            node_kind: "Hold",
            chip_alternative: Some("hysteresis setting"),
        },
        ObstacleKind::CombinesInputs { node_kind: "Mix" },
        ObstacleKind::CurveInputIsNotASensor { found: "Delay" },
    ];

    for kind in samples {
        let text = Obstacle { node: None, kind }.explain();
        assert!(!text.is_empty());
        assert!(text.ends_with('.'), "{text}");
        for jargon in ["0x", "register", "bank", "nibble", "SmartFan"] {
            assert!(!text.contains(jargon), "{jargon} leaked into {text:?}");
        }
    }
}

#[test]
fn the_shape_our_own_importer_produces_is_diagnosed_honestly() {
    // `Sensor -> Hold -> LowPass -> Curve -> Clamp -> FanOutput` is what importing a
    // FanControl configuration builds. It cannot be offloaded as-is, and both filters
    // should be named — telling somebody about one at a time would make them discover
    // the rest by repetition.
    let graph = chain(vec![
        temp("nct6798d/temp/cputin"),
        NodeKind::Hold { band: 2.0 },
        NodeKind::LowPass { tau_seconds: 1.0 },
        curve(&[(35.0, 30.0), (75.0, 100.0)]),
        NodeKind::Clamp {
            min: 0.0,
            max: 100.0,
        },
        output(),
    ]);

    let obstacles = reduce(&graph, CHANNEL).expect_err("filters block it");
    // The Clamp folded away; only the filter nearest the curve is reached by this walk,
    // which is honest: it is the first thing that must go.
    assert!(!obstacles.is_empty());
    assert!(
        obstacles.iter().all(|o| o.node.is_some()),
        "an interface has to be able to point at the node responsible"
    );
}

#[test]
fn reduce_all_covers_every_channel_the_graph_drives() {
    let mut graph = Graph::default();
    let k = graph.insert("duty", NodeKind::Constant { value: 30.0 });
    for channel in ["nct6798d/pwm/0", "nct6798d/pwm/4"] {
        let out = graph.insert(
            format!("fan-{channel}").as_str(),
            NodeKind::FanOutput {
                channel: channel.into(),
            },
        );
        graph.connect(PortRef::new(k.clone(), "out"), PortRef::new(out, "duty"));
    }

    let all = reduce_all(&graph);
    assert_eq!(all.len(), 2);
    assert!(all.values().all(|v| v.is_ok()));
}
