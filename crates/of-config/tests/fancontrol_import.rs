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
/// The pump's fixed curve stores `2200` in a field called `Percent`. Whatever that is, it
/// is not a percentage, and an importer that clamped it to 100 would produce a profile
/// that looks entirely reasonable and runs a pump flat out.
///
/// The rule is about the value rather than its cause, which we do not know: a number that
/// cannot be a percentage must never *become* one by being clipped into range.
#[test]
fn a_fixed_speed_that_is_not_a_percentage_is_never_clamped_into_one() {
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

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
}

#[test]
fn a_channel_is_left_alone_when_its_speed_cannot_be_worked_out() {
    // The measurements decide. Where they cannot reach the speed asked for, the channel
    // goes to the motherboard rather than being handed a guessed number — half a
    // translation is worse than none on a pump.
    let unreachable = r#"{
      "__VERSION__": "277",
      "FanControl": {
        "Controls": [
          { "NickName": "Pump", "Identifier": "/lpc/nct6798d/control/4", "Enable": true,
            "SelectedFanCurve": { "Name": "Fixed" },
            "Calibration": [[0,0,false],[50,900,false],[100,1800,false]] }
        ],
        "FanCurves": [ { "Name": "Fixed", "CommandMode": 1, "Percent": 5000 } ]
      }
    }"#;

    let imported = fancontrol::import(unreachable, &reference_machine()).expect("imports");

    let drives_pump =
        imported.profile.graph.nodes.values().any(
            |n| matches!(&n.kind, NodeKind::FanOutput { channel } if channel == "nct6798d/pwm/4"),
        );
    assert!(
        !drives_pump,
        "a speed the fan was never measured reaching must not become a duty"
    );

    assert!(
        imported
            .needs_attention()
            .any(|n| n.detail.contains("5000")),
        "and the user must be told which speed could not be worked out"
    );
}

#[test]
fn a_control_with_no_measurements_at_all_cannot_have_its_speed_converted() {
    // Without a table there is no bridge between duty and speed, so there is nothing to
    // interpolate and nothing to guess from.
    let no_table = r#"{
      "__VERSION__": "277",
      "FanControl": {
        "Controls": [
          { "NickName": "Pump", "Identifier": "/lpc/nct6798d/control/4", "Enable": true,
            "SelectedFanCurve": { "Name": "Fixed" }, "Calibration": [] }
        ],
        "FanCurves": [ { "Name": "Fixed", "CommandMode": 1, "Percent": 2200 } ]
      }
    }"#;

    let imported = fancontrol::import(no_table, &reference_machine()).expect("imports");
    assert!(
        imported.is_empty(),
        "nothing should be built without measurements to convert from"
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

// --- rescuing a fixed speed given in RPM ------------------------------------------------

#[test]
fn a_fixed_speed_in_rpm_is_converted_using_the_measurements_in_the_same_file() {
    // The pump asks for 2200 rpm. Its own calibration puts 20 % at 1823 rpm and 30 % at
    // 2927 rpm, so 2200 rpm is inside the measured range and worth about 23 %. Refusing
    // it outright threw away data that was sitting in the same file.
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    let driven =
        imported.profile.graph.nodes.values().any(
            |n| matches!(&n.kind, NodeKind::FanOutput { channel } if channel == "nct6798d/pwm/4"),
        );
    assert!(driven, "the pump should now come across");

    let duty = imported
        .profile
        .graph
        .nodes
        .values()
        .find_map(|n| match &n.kind {
            NodeKind::Constant { value } => Some(*value),
            _ => None,
        })
        .expect("a fixed duty was produced");

    // Interpolated between the two bracketing measurements, not clamped and not guessed.
    assert!(
        (23.0..=24.0).contains(&duty),
        "2200 rpm should land near 23 %, got {duty}"
    );

    // And it is still flagged, because holding a duty is not holding a speed.
    assert!(
        imported
            .needs_attention()
            .any(|n| n.detail.contains("2200") && n.detail.contains('%')),
        "the conversion must be explained rather than presented as exact"
    );
}

#[test]
fn a_speed_the_measurements_never_reached_is_refused_rather_than_extrapolated() {
    use of_config::import::{Calibration, CalibrationPoint};

    let measured = Calibration {
        channel: "c".into(),
        label: "Pump".into(),
        points: vec![
            CalibrationPoint {
                duty_percent: 20.0,
                rpm: 1000.0,
            },
            CalibrationPoint {
                duty_percent: 100.0,
                rpm: 3000.0,
            },
        ],
        start_percent: None,
        stop_percent: None,
        minimum_percent: None,
    };

    // Inside the range, interpolated.
    assert_eq!(measured.duty_reaching(2000.0), Some(60.0));

    // Above anything measured: the fan may simply not go that fast, and guessing that it
    // does is how a pump ends up commanded flat out.
    assert_eq!(measured.duty_reaching(5000.0), None);

    // Below anything measured: extrapolating downwards guesses towards a stall.
    assert_eq!(measured.duty_reaching(200.0), None);

    assert_eq!(measured.duty_reaching(0.0), None);
    assert_eq!(measured.duty_reaching(f64::NAN), None);
}

#[test]
fn a_dead_zone_in_the_measurements_is_not_interpolated_across() {
    use of_config::import::{Calibration, CalibrationPoint};

    // The real shape of the reference machine's pump at the bottom: 970 rpm at 1 % and
    // 979 rpm at 10 %. Duty says almost nothing about speed there, so interpolating would
    // invent precision the measurements do not have.
    let measured = Calibration {
        channel: "c".into(),
        label: "Pump".into(),
        points: vec![
            CalibrationPoint {
                duty_percent: 1.0,
                rpm: 970.0,
            },
            CalibrationPoint {
                duty_percent: 10.0,
                rpm: 979.0,
            },
            CalibrationPoint {
                duty_percent: 20.0,
                rpm: 1823.0,
            },
        ],
        start_percent: None,
        stop_percent: None,
        minimum_percent: None,
    };

    assert_eq!(
        measured.duty_reaching(975.0),
        None,
        "a nine-rpm spread over nine percent of duty is not a measurement to interpolate"
    );

    // The segment above it is a real rise, so that one is usable.
    let duty = measured
        .duty_reaching(1400.0)
        .expect("the rising segment works");
    assert!((14.0..=16.0).contains(&duty), "got {duty}");
}

#[test]
fn the_quietest_duty_that_reaches_a_speed_wins() {
    use of_config::import::{Calibration, CalibrationPoint};

    // A table need not be monotonic. Where two stretches both reach a speed, the lower
    // duty is the right answer for a fan controller.
    let measured = Calibration {
        channel: "c".into(),
        label: "Fan".into(),
        points: vec![
            CalibrationPoint {
                duty_percent: 10.0,
                rpm: 500.0,
            },
            CalibrationPoint {
                duty_percent: 20.0,
                rpm: 1500.0,
            },
            CalibrationPoint {
                duty_percent: 30.0,
                rpm: 900.0,
            },
            CalibrationPoint {
                duty_percent: 40.0,
                rpm: 1900.0,
            },
        ],
        start_percent: None,
        stop_percent: None,
        minimum_percent: None,
    };

    let duty = measured.duty_reaching(1000.0).expect("reachable");
    assert!(duty < 20.0, "should pick the quieter stretch, got {duty}");
}

#[test]
fn two_fans_sharing_one_rpm_curve_each_get_their_own_duty() {
    // The trap in sharing: the duty that spins one fan at a given speed is not the duty
    // that spins another at the same speed, so an RPM curve cannot be cached by name the
    // way a temperature curve is.
    let config = r#"{
      "__VERSION__": "277",
      "FanControl": {
        "Controls": [
          { "NickName": "A", "Identifier": "/lpc/nct6798d/control/0", "Enable": true,
            "SelectedFanCurve": { "Name": "Fixed" },
            "Calibration": [[0,0,false],[50,1000,false],[100,2000,false]] },
          { "NickName": "B", "Identifier": "/lpc/nct6798d/control/1", "Enable": true,
            "SelectedFanCurve": { "Name": "Fixed" },
            "Calibration": [[0,0,false],[25,1000,false],[100,4000,false]] }
        ],
        "FanCurves": [ { "Name": "Fixed", "CommandMode": 1, "Percent": 1000 } ]
      }
    }"#;

    let imported = fancontrol::import(config, &reference_machine()).expect("imports");
    let duties: Vec<f64> = imported
        .profile
        .graph
        .nodes
        .values()
        .filter_map(|n| match &n.kind {
            NodeKind::Constant { value } => Some(*value),
            _ => None,
        })
        .collect();

    assert_eq!(duties.len(), 2, "each fan needs its own constant");
    assert!(
        duties.contains(&50.0) && duties.contains(&25.0),
        "each fan should get the duty its own measurements give for 1000 rpm, got {duties:?}"
    );
}

#[test]
fn a_converted_speed_is_presented_as_a_reading_not_as_a_fact() {
    // We cannot tell *why* a percentage field holds 2200. The file carries no unit and no
    // mode: the value was either set in rpm deliberately or written into the wrong field,
    // and from the outside those are indistinguishable. Reading it as rpm is the only
    // useful thing to do with it, but it stays a reading — so the note has to say that it
    // might be wrong and what goes wrong if it is.
    let imported = fancontrol::import(V277, &reference_machine()).expect("imports");

    let note = imported
        .needs_attention()
        .find(|n| n.detail.contains("2200"))
        .expect("the converted speed is flagged");

    assert!(
        note.detail.contains("check") || note.detail.contains("Check"),
        "it must ask to be checked: {}",
        note.detail
    );
    assert!(
        note.detail.contains("wrong"),
        "it must say what happens if the reading is wrong: {}",
        note.detail
    );
    // And it must not claim to know the cause.
    assert!(
        !note.detail.contains("deliberate"),
        "the cause is unknown and must not be asserted: {}",
        note.detail
    );
}
