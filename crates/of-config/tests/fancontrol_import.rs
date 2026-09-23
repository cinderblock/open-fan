//! Importing real FanControl configurations.
//!
//! The fixtures are files written by the tool itself on the reference machine, kept
//! verbatim. That matters: the things this importer has to get right are the things a
//! real file does that a hand-written sample would not think to do — a field whose name
//! does not match its contents, a layout that moved between versions, an array whose
//! arity changed.

use of_config::import::{Fidelity, fancontrol};
use of_config::presets::{ChannelSummary, HardwareSummary, SensorSummary};
use of_core::NodeKind;
use of_units::Quantity;

const V277: &str = include_str!("fixtures/fancontrol-v277.json");
const V245: &str = include_str!("fixtures/fancontrol-v245-legacy-layout.json");

/// The reference machine: an NCT6798D with seven PWM channels and two temperatures.
fn reference_machine() -> HardwareSummary {
    HardwareSummary {
        sensors: vec![
            SensorSummary {
                id: "nct6798d/temp/systin".into(),
                label: "System (SYSTIN)".into(),
                quantity: Quantity::Temperature,
            },
            SensorSummary {
                id: "nct6798d/temp/cputin".into(),
                label: "CPU socket (CPUTIN)".into(),
                quantity: Quantity::Temperature,
            },
        ],
        channels: (0..7)
            .map(|i| ChannelSummary {
                id: format!("nct6798d/pwm/{i}"),
                label: format!("PWM {i}"),
                tachometer: Some(format!("nct6798d/fan/{i}")),
            })
            .collect(),
    }
}

fn note_for<'a>(
    imported: &'a of_config::import::Imported,
    subject: &str,
) -> Vec<&'a of_config::import::Note> {
    imported
        .notes
        .iter()
        .filter(|n| n.subject == subject)
        .collect()
}

/// The single most important assertion in this file.
///
/// The pump's fixed curve stores `2200` in a field called `Percent`. It is an RPM. An
/// importer that clamped it to 100 would produce a profile that looks entirely reasonable
/// and runs a pump flat out — on the one channel whose owner deliberately keeps it slow.
#[test]
fn a_fixed_speed_that_is_not_a_percentage_is_refused_rather_than_clamped() {
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    // Nothing anywhere in the graph commands a duty derived from 2200.
    for node in imported.profile.graph.nodes.values() {
        if let NodeKind::Constant { value } = &node.kind {
            assert!(
                *value <= 100.0,
                "a constant of {value} reached the graph; an out-of-range fixed speed \
                 must never be clamped into a duty"
            );
            assert_ne!(*value, 100.0, "2200 must not have become full speed");
        }
    }

    // And it is called out rather than silently dropped.
    let flagged = note_for(&imported, "Flat");
    assert!(
        flagged
            .iter()
            .any(|n| n.fidelity == Fidelity::NeedsAttention && n.detail.contains("2200")),
        "the refused value must be named for the user, got {flagged:?}"
    );
}

#[test]
fn the_pump_is_left_alone_entirely_when_its_curve_cannot_be_translated() {
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    // The pump is on PWM 4. Its curve was refused, so nothing may drive it: handing a
    // channel a half-translated duty would be worse than leaving it to the firmware.
    let drives_pump =
        imported.profile.graph.nodes.values().any(
            |n| matches!(&n.kind, NodeKind::FanOutput { channel } if channel == "nct6798d/pwm/4"),
        );

    assert!(
        !drives_pump,
        "the pump must be left to the motherboard when its curve could not be brought across"
    );
}

#[test]
fn an_enabled_control_with_a_line_curve_comes_across_with_its_shape_intact() {
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    // The CPU fan follows "Linear": 30 % at 35 °C rising to 100 % at 75 °C.
    let curve = imported
        .profile
        .graph
        .nodes
        .values()
        .find_map(|n| match &n.kind {
            NodeKind::Curve { points } => Some(points.clone()),
            _ => None,
        })
        .expect("a curve came across");

    assert_eq!(curve.len(), 2);
    assert_eq!((curve[0].x, curve[0].y), (35.0, 30.0));
    assert_eq!((curve[1].x, curve[1].y), (75.0, 100.0));

    // And it reaches the CPU fan's channel, PWM 1.
    assert!(
        imported.profile.graph.nodes.values().any(
            |n| matches!(&n.kind, NodeKind::FanOutput { channel } if channel == "nct6798d/pwm/1")
        ),
        "the CPU fan channel must be driven"
    );
}

#[test]
fn a_control_that_was_switched_off_stays_switched_off() {
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    // The chassis fan is disabled in this configuration. Importing it as an active
    // output would start driving a fan the user had deliberately left to the firmware —
    // a change to their machine they did not ask for.
    assert!(
        !imported.profile.graph.nodes.values().any(
            |n| matches!(&n.kind, NodeKind::FanOutput { channel } if channel == "nct6798d/pwm/0")
        ),
        "a disabled control must not become a driven channel"
    );

    let notes = note_for(&imported, "Chassis Fan");
    assert!(
        notes
            .iter()
            .any(|n| n.fidelity == Fidelity::Skipped && n.detail.contains("turned off")),
        "the user should be told why their chassis fan did not come across: {notes:?}"
    );
}

#[test]
fn hardware_this_machine_does_not_have_is_named_rather_than_addressed() {
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    // Every output addresses a channel the summary actually lists.
    for node in imported.profile.graph.nodes.values() {
        if let NodeKind::FanOutput { channel } = &node.kind {
            assert!(
                reference_machine()
                    .channels
                    .iter()
                    .any(|c| c.id == *channel),
                "{channel} is not a channel this machine has"
            );
        }
    }
}

#[test]
fn a_temperature_binding_always_asks_to_be_checked() {
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    // Their temperatures are numbered by position and ours are keyed by function, so no
    // translation between them can be certain. Every curve that follows a temperature
    // must say so rather than presenting a guess as a fact.
    let uses_a_sensor = imported
        .profile
        .graph
        .nodes
        .values()
        .any(|n| matches!(&n.kind, NodeKind::Sensor { .. }));

    if uses_a_sensor {
        assert!(
            imported
                .needs_attention()
                .any(|n| n.detail.contains("check")),
            "a guessed temperature binding must be flagged for review"
        );
    }
}

#[test]
fn the_measured_fan_speeds_come_across_including_where_the_fan_stops() {
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    let cpu = imported
        .calibration
        .iter()
        .find(|c| c.channel == "nct6798d/pwm/1")
        .expect("the CPU fan was calibrated");

    // Ascending, and the table covers the bottom of the range.
    assert!(
        cpu.points
            .windows(2)
            .all(|w| w[0].duty_percent <= w[1].duty_percent)
    );
    assert_eq!(cpu.points.first().map(|p| p.duty_percent), Some(0.0));

    // The measurement that matters: somebody already found where this fan stops, so we
    // do not have to go looking for it by running a fan down to a stall.
    assert!(
        cpu.found_the_stall(),
        "this table records a zero reading below a turning one"
    );
    let lowest = cpu.lowest_turning_duty().expect("some point turns");
    assert!(
        lowest > 0.0 && lowest <= 10.0,
        "the fan was observed turning at {lowest} %"
    );
}

#[test]
fn calibration_is_taken_even_from_a_control_that_was_switched_off() {
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    // The chassis fan is disabled, and its measurements are still worth having: they
    // describe the fan, not the configuration.
    assert!(
        imported
            .calibration
            .iter()
            .any(|c| c.channel == "nct6798d/pwm/0"),
        "measurements should survive a disabled control"
    );
}

#[test]
fn a_table_that_never_reaches_zero_does_not_claim_to_have_found_the_stall() {
    // Guarding the inference itself: without a zero reading below a turning one, the
    // table says nothing about where this fan stops, and must not be read as proof that
    // low duties are safe.
    use of_config::import::{Calibration, CalibrationPoint};

    let never_stopped = Calibration {
        channel: "x".into(),
        label: "x".into(),
        points: vec![
            CalibrationPoint {
                duty_percent: 20.0,
                rpm: 600.0,
            },
            CalibrationPoint {
                duty_percent: 100.0,
                rpm: 2000.0,
            },
        ],
        start_percent: None,
        stop_percent: None,
        minimum_percent: None,
    };
    assert!(!never_stopped.found_the_stall());
    assert_eq!(never_stopped.lowest_turning_duty(), Some(20.0));

    let all_stopped = Calibration {
        points: vec![CalibrationPoint {
            duty_percent: 0.0,
            rpm: 0.0,
        }],
        ..never_stopped.clone()
    };
    assert!(!all_stopped.found_the_stall());
    assert_eq!(all_stopped.lowest_turning_duty(), None);
}

#[test]
fn the_older_layout_is_read_too() {
    // The section key moved from `Main` to `FanControl` between versions. Both are real
    // files people have on disk.
    let imported = fancontrol::import(V245, &reference_machine()).expect("the old layout imports");
    assert!(
        !imported.notes.is_empty(),
        "an old configuration should still produce an account of itself"
    );
    assert!(
        imported
            .calibration
            .iter()
            .any(|c| c.channel == "nct6798d/pwm/0"),
        "the two-column calibration rows in the old format still parse"
    );
}

#[test]
fn a_machine_with_no_matching_hardware_produces_notes_and_no_graph() {
    // A configuration from somebody else's board. Nothing should be addressed.
    let elsewhere = HardwareSummary {
        sensors: vec![],
        channels: vec![ChannelSummary {
            id: "it8686e/pwm/0".into(),
            label: "PWM 0".into(),
            tachometer: None,
        }],
    };

    let imported = fancontrol::import(V277, &elsewhere).expect("imports");
    assert!(imported.is_empty(), "nothing should have been built");
    assert!(
        imported
            .notes
            .iter()
            .any(|n| n.fidelity == Fidelity::Skipped),
        "and the user should be told why"
    );
}

#[test]
fn something_that_is_not_a_fancontrol_configuration_is_refused_clearly() {
    let hw = reference_machine();
    assert!(fancontrol::import("{}", &hw).is_err());
    assert!(fancontrol::import("not json", &hw).is_err());
    assert!(fancontrol::import(r#"{"something":"else"}"#, &hw).is_err());
}

#[test]
fn importing_never_produces_a_profile_that_drives_a_channel_twice() {
    // Two outputs on one channel would be two things commanding one fan — the tug-of-war
    // this whole product exists to end.
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    let mut seen = std::collections::BTreeSet::new();
    for node in imported.profile.graph.nodes.values() {
        if let NodeKind::FanOutput { channel } = &node.kind {
            assert!(seen.insert(channel.clone()), "{channel} is driven twice");
        }
    }
}

#[test]
fn an_imported_profile_is_a_valid_graph_the_engine_would_accept() {
    // An import that produces something unloadable is not an import. This is the test
    // that makes the translation real rather than merely well-intentioned.
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    match imported.profile.graph.validate() {
        Ok(_) => {}
        Err(errors) => panic!("the imported profile does not validate: {errors:?}"),
    }
}

#[test]
fn an_imported_profile_survives_being_saved_and_loaded_again() {
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");
    let json = serde_json::to_string(&imported.profile).expect("serialises");
    let round_tripped = of_config::load(&json).expect("loads");
    assert_eq!(round_tripped, imported.profile);
}
