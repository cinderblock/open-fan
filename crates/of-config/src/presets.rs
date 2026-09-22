//! Ready-made configurations, built from whatever hardware is actually present.
//!
//! A preset cannot be a stored document. Sensor and channel ids differ on every machine,
//! so a fixed graph would be broken everywhere except the author's desk. These are
//! *generated* from the inventory, which is the only thing that knows what this machine
//! has.
//!
//! Every preset is **complete and valid**: it drives every channel the machine offers,
//! and the tests assert that each one compiles and commands every fan on a plausible
//! tick. "Working" is a guarantee here rather than a hope — a starter configuration that
//! silently fails to control something would be worse than no starter at all.
//!
//! They are also arranged in ascending order of how much of the model they show off:
//! a fixed duty, then a curve, then filtering, then mixing, then feedback. Opening one
//! and reading it is meant to be the fastest way to understand what this application
//! does.

use of_core::{CurvePoint, Graph, MixMode, NodeId, NodeKind, PortRef};
use of_units::Quantity;
use serde::{Deserialize, Serialize};

use crate::Profile;

/// A sensor the machine can read.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorSummary {
    pub id: String,
    pub label: String,
    pub quantity: Quantity,
}

/// An output channel the machine can drive.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelSummary {
    pub id: String,
    pub label: String,
    pub tachometer: Option<String>,
}

/// What the machine offers, as far as presets are concerned.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HardwareSummary {
    pub sensors: Vec<SensorSummary>,
    pub channels: Vec<ChannelSummary>,
}

/// One offered starting point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    /// Stable identifier, for the UI to key on.
    pub id: String,
    pub name: String,
    /// One or two sentences saying what it does and when to pick it.
    pub description: String,
    pub graph: Graph,
}

impl Preset {
    pub fn into_profile(self) -> Profile {
        Profile::new(self.name, self.graph)
    }
}

// Column geometry, mirroring the editor's layout constants so a generated graph looks
// like a hand-arranged one rather than a pile at the origin. Duplicated deliberately:
// the editor's copy is TypeScript, and this is not worth a shared schema.
const COLUMN_WIDTH: f32 = 280.0;
const ROW_HEIGHT: f32 = 150.0;
const ORIGIN: (f32, f32) = (60.0, 60.0);

fn at(column: usize, row: usize) -> (f32, f32) {
    (
        ORIGIN.0 + column as f32 * COLUMN_WIDTH,
        ORIGIN.1 + row as f32 * ROW_HEIGHT,
    )
}

/// Add a node at a grid position.
fn place(graph: &mut Graph, id: &str, kind: NodeKind, column: usize, row: usize) -> NodeId {
    let node = graph.insert(id, kind);
    if let Some(instance) = graph.nodes.get_mut(&node) {
        instance.position = at(column, row);
    }
    node
}

/// Temperature sensors, most likely to be "the CPU" first.
///
/// Naming is wildly inconsistent across chips, so this is a preference ordering rather
/// than a lookup. Getting it wrong picks a different-but-real temperature, which makes a
/// preset a worse default — never an unsafe one.
fn temperatures(hw: &HardwareSummary) -> Vec<&SensorSummary> {
    const PREFERRED: &[&str] = &["package", "tctl", "tdie", "cpu", "core"];

    let mut temps: Vec<&SensorSummary> = hw
        .sensors
        .iter()
        .filter(|s| s.quantity == Quantity::Temperature)
        .collect();

    temps.sort_by_key(|s| {
        let haystack = format!("{} {}", s.id, s.label).to_lowercase();
        PREFERRED
            .iter()
            .position(|needle| haystack.contains(needle))
            .unwrap_or(PREFERRED.len())
    });
    temps
}

fn find_containing<'a>(temps: &[&'a SensorSummary], needle: &str) -> Option<&'a SensorSummary> {
    temps.iter().copied().find(|s| {
        format!("{} {}", s.id, s.label)
            .to_lowercase()
            .contains(needle)
    })
}

fn curve(points: &[(f64, f64)]) -> NodeKind {
    NodeKind::Curve {
        points: points.iter().map(|&(x, y)| CurvePoint { x, y }).collect(),
    }
}

/// Wire one source into every channel the machine has.
fn fan_out(graph: &mut Graph, source: &PortRef, hw: &HardwareSummary, column: usize) {
    for (row, channel) in hw.channels.iter().enumerate() {
        let id = format!("fan-{row}");
        let node = place(
            graph,
            &id,
            NodeKind::FanOutput {
                channel: channel.id.clone(),
            },
            column,
            row,
        );
        graph.connect(source.clone(), PortRef::new(node, "duty"));
    }
}

fn sensor_node(s: &SensorSummary) -> NodeKind {
    NodeKind::Sensor {
        sensor_id: s.id.clone(),
        quantity: s.quantity,
    }
}

/// Every preset that makes sense for this machine, simplest first.
///
/// Presets needing hardware the machine lacks are omitted rather than offered broken —
/// there is no value in a starting point that cannot work here.
pub fn presets(hw: &HardwareSummary) -> Vec<Preset> {
    if hw.channels.is_empty() {
        // Nothing to drive. Offering a "configuration" that controls nothing would be
        // worse than admitting there is nothing to configure.
        return Vec::new();
    }

    let temps = temperatures(hw);
    let mut out = vec![fixed_duty(hw)];

    if let Some(primary) = temps.first().copied() {
        out.push(balanced(hw, primary));
        out.push(quiet(hw, primary));

        // Only worth offering when there really are two things worth watching.
        let gpu = find_containing(&temps, "gpu");
        if let Some(gpu) = gpu
            && gpu.id != primary.id
        {
            out.push(cpu_and_gpu(hw, primary, gpu));
        }

        if hw.channels.iter().any(|c| c.tachometer.is_some()) {
            out.push(stall_protected(hw, primary));
        }
    }

    out
}

/// The simplest thing that works: one duty, everywhere.
fn fixed_duty(hw: &HardwareSummary) -> Preset {
    let mut graph = Graph::default();
    let k = place(&mut graph, "duty", NodeKind::Constant { value: 50.0 }, 0, 0);
    fan_out(&mut graph, &PortRef::new(k, "out"), hw, 3);

    Preset {
        id: "fixed".into(),
        name: "Fixed speed".into(),
        description: "Runs every fan at a constant 50 %. The simplest configuration that \
                      works, and a good place to start if you just want the firmware out \
                      of the way."
            .into(),
        graph,
    }
}

/// The sensible default: one temperature, one curve.
fn balanced(hw: &HardwareSummary, primary: &SensorSummary) -> Preset {
    let mut graph = Graph::default();
    let t = place(&mut graph, "temp", sensor_node(primary), 0, 0);
    let c = place(
        &mut graph,
        "curve",
        curve(&[(30.0, 25.0), (55.0, 40.0), (80.0, 100.0)]),
        1,
        0,
    );
    graph.connect(PortRef::new(t, "out"), PortRef::new(c.clone(), "in"));
    fan_out(&mut graph, &PortRef::new(c, "out"), hw, 3);

    Preset {
        id: "balanced".into(),
        name: "Balanced".into(),
        description: format!(
            "Follows {} along a gentle curve, reaching full speed at 80 °C. The one to \
             pick if you are not sure.",
            primary.label
        ),
        graph,
    }
}

/// Quiet: the same idea, but smoothed so it never audibly hunts.
fn quiet(hw: &HardwareSummary, primary: &SensorSummary) -> Preset {
    let mut graph = Graph::default();
    let t = place(&mut graph, "temp", sensor_node(primary), 0, 0);
    let smooth = place(
        &mut graph,
        "smooth",
        NodeKind::LowPass { tau_seconds: 15.0 },
        1,
        0,
    );
    let c = place(
        &mut graph,
        "curve",
        curve(&[(40.0, 20.0), (70.0, 45.0), (90.0, 100.0)]),
        2,
        0,
    );
    let ramp = place(
        &mut graph,
        "ramp",
        NodeKind::RateLimit {
            max_delta_per_second: 5.0,
        },
        2,
        1,
    );

    graph.connect(PortRef::new(t, "out"), PortRef::new(smooth.clone(), "in"));
    graph.connect(PortRef::new(smooth, "out"), PortRef::new(c.clone(), "in"));
    graph.connect(PortRef::new(c, "out"), PortRef::new(ramp.clone(), "in"));
    fan_out(&mut graph, &PortRef::new(ramp, "out"), hw, 3);

    Preset {
        id: "quiet".into(),
        name: "Quiet".into(),
        description: "Ignores short spikes and ramps slowly, so the fans change speed \
                      rarely and never audibly hunt. Runs warmer than Balanced."
            .into(),
        graph,
    }
}

/// Two heat sources, hottest wins.
fn cpu_and_gpu(hw: &HardwareSummary, cpu: &SensorSummary, gpu: &SensorSummary) -> Preset {
    let mut graph = Graph::default();
    let a = place(&mut graph, "cpu", sensor_node(cpu), 0, 0);
    let b = place(&mut graph, "gpu", sensor_node(gpu), 0, 1);
    let mix = place(
        &mut graph,
        "hottest",
        NodeKind::Mix { mode: MixMode::Max },
        1,
        0,
    );
    let c = place(
        &mut graph,
        "curve",
        curve(&[(35.0, 25.0), (60.0, 45.0), (85.0, 100.0)]),
        2,
        0,
    );

    graph.connect(PortRef::new(a, "out"), PortRef::new(mix.clone(), "in"));
    graph.connect(PortRef::new(b, "out"), PortRef::new(mix.clone(), "in"));
    graph.connect(PortRef::new(mix, "out"), PortRef::new(c.clone(), "in"));
    fan_out(&mut graph, &PortRef::new(c, "out"), hw, 3);

    Preset {
        id: "cpu-and-gpu".into(),
        name: "Whichever is hotter".into(),
        description: format!(
            "Watches {} and {} together and follows whichever is hotter. If one input \
             stops reporting the fans fail safe rather than averaging the fault away.",
            cpu.label, gpu.label
        ),
        graph,
    }
}

/// Balanced, plus: if a fan is not actually turning, drive it hard.
fn stall_protected(hw: &HardwareSummary, primary: &SensorSummary) -> Preset {
    let mut graph = Graph::default();
    let t = place(&mut graph, "temp", sensor_node(primary), 0, 0);
    let c = place(
        &mut graph,
        "curve",
        curve(&[(30.0, 25.0), (55.0, 40.0), (80.0, 100.0)]),
        1,
        0,
    );
    graph.connect(PortRef::new(t, "out"), PortRef::new(c.clone(), "in"));

    let full = place(
        &mut graph,
        "full",
        NodeKind::Constant { value: 100.0 },
        1,
        1,
    );

    // Each channel with a tachometer gets its own stall check, because a stalled fan is
    // a property of that fan and not of the machine.
    for (row, channel) in hw.channels.iter().enumerate() {
        let fan = place(
            &mut graph,
            &format!("fan-{row}"),
            NodeKind::FanOutput {
                channel: channel.id.clone(),
            },
            3,
            row,
        );

        if channel.tachometer.is_none() {
            graph.connect(PortRef::new(c.clone(), "out"), PortRef::new(fan, "duty"));
            continue;
        }

        let stalled = place(
            &mut graph,
            &format!("stalled-{row}"),
            NodeKind::Comparator {
                threshold: 200.0,
                deadband: 100.0,
                direction: of_core::Compare::Below,
            },
            2,
            row,
        );
        let pick = place(&mut graph, &format!("pick-{row}"), NodeKind::Select, 2, row);

        // The fan's measured speed steers the fan. Not a cycle: the reading predates
        // this tick's duty, which the speed port being delayed is what expresses.
        graph.connect(
            PortRef::new(fan.clone(), "rpm"),
            PortRef::new(stalled.clone(), "in"),
        );
        graph.connect(
            PortRef::new(stalled, "out"),
            PortRef::new(pick.clone(), "when"),
        );
        graph.connect(
            PortRef::new(full.clone(), "out"),
            PortRef::new(pick.clone(), "if_true"),
        );
        graph.connect(
            PortRef::new(c.clone(), "out"),
            PortRef::new(pick.clone(), "if_false"),
        );
        graph.connect(PortRef::new(pick, "out"), PortRef::new(fan, "duty"));
    }

    Preset {
        id: "stall-protected".into(),
        name: "Balanced, with stall protection".into(),
        description: "Balanced, but each fan watches its own tachometer: if it stops \
                      turning, it is driven to full. A stalled fan sounds quiet and is \
                      not cooling anything."
            .into(),
        graph,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use of_core::{EvalState, SensorReadings, TickInput};
    use of_units::Value;
    use std::collections::BTreeMap;

    fn hardware() -> HardwareSummary {
        HardwareSummary {
            sensors: vec![
                SensorSummary {
                    id: "chip/temp/1".into(),
                    label: "Systin".into(),
                    quantity: Quantity::Temperature,
                },
                SensorSummary {
                    id: "cpu/package".into(),
                    label: "CPU Package".into(),
                    quantity: Quantity::Temperature,
                },
                SensorSummary {
                    id: "gpu/core".into(),
                    label: "GPU Core".into(),
                    quantity: Quantity::Temperature,
                },
                SensorSummary {
                    id: "chip/fan/1".into(),
                    label: "Front intake speed".into(),
                    quantity: Quantity::Rpm,
                },
            ],
            channels: vec![
                ChannelSummary {
                    id: "chip/pwm/1".into(),
                    label: "Front intake".into(),
                    tachometer: Some("chip/fan/1".into()),
                },
                ChannelSummary {
                    id: "chip/pwm/2".into(),
                    label: "Rear exhaust".into(),
                    tachometer: None,
                },
            ],
        }
    }

    /// Plausible readings for every sensor the fixture advertises.
    fn readings(hw: &HardwareSummary, temp: f64, rpm: f64) -> SensorReadings {
        hw.sensors
            .iter()
            .map(|s| {
                let scalar = match s.quantity {
                    Quantity::Temperature => temp,
                    Quantity::Rpm => rpm,
                    _ => 0.0,
                };
                (s.id.clone(), Value::raw(s.quantity, scalar))
            })
            .collect()
    }

    fn tachometers(hw: &HardwareSummary) -> BTreeMap<String, String> {
        hw.channels
            .iter()
            .filter_map(|c| c.tachometer.clone().map(|t| (c.id.clone(), t)))
            .collect()
    }

    #[test]
    fn every_preset_compiles_and_drives_every_channel() {
        // The whole promise of a preset: pick it and the machine is under control. A
        // starting point that silently fails to drive a fan is worse than none.
        let hw = hardware();
        let tachs = tachometers(&hw);

        for preset in presets(&hw) {
            let compiled = preset
                .graph
                .validate()
                .unwrap_or_else(|e| panic!("preset {:?} does not compile: {e:?}", preset.id));

            let out = compiled.tick(
                &TickInput::new(&readings(&hw, 65.0, 900.0), 0.1).with_tachometers(&tachs),
                &mut EvalState::new(),
            );

            for channel in &hw.channels {
                assert!(
                    out.commands.contains_key(&channel.id),
                    "preset {:?} left {} uncommanded",
                    preset.id,
                    channel.id
                );
            }
            assert!(
                out.faulted_channels.is_empty(),
                "preset {:?} faulted: {:?}",
                preset.id,
                out.faulted_channels
            );
        }
    }

    #[test]
    fn every_preset_responds_to_heat() {
        // A curve that never changes is indistinguishable from a fixed duty, which would
        // make most of these presets a lie.
        let hw = hardware();
        let tachs = tachometers(&hw);

        for preset in presets(&hw) {
            if preset.id == "fixed" {
                continue;
            }
            let compiled = preset.graph.validate().unwrap();
            let mut state = EvalState::new();

            let cool = compiled.tick(
                &TickInput::new(&readings(&hw, 30.0, 900.0), 0.1).with_tachometers(&tachs),
                &mut state,
            );
            // Enough ticks for filtering and rate limiting to settle.
            let mut hot = cool.clone();
            for _ in 0..2000 {
                hot = compiled.tick(
                    &TickInput::new(&readings(&hw, 95.0, 900.0), 0.1).with_tachometers(&tachs),
                    &mut state,
                );
            }

            let channel = &hw.channels[0].id;
            assert!(
                hot.commands[channel].scalar > cool.commands[channel].scalar,
                "preset {:?} does not speed up when hot",
                preset.id
            );
        }
    }

    #[test]
    fn stall_protection_drives_a_stopped_fan_to_full() {
        let hw = hardware();
        let tachs = tachometers(&hw);
        let preset = presets(&hw)
            .into_iter()
            .find(|p| p.id == "stall-protected")
            .expect("offered when a channel has a tachometer");

        let compiled = preset.graph.validate().unwrap();
        let mut state = EvalState::new();

        // Cool and spinning: the curve decides.
        let spinning = compiled.tick(
            &TickInput::new(&readings(&hw, 30.0, 900.0), 0.1).with_tachometers(&tachs),
            &mut state,
        );
        assert!(spinning.commands["chip/pwm/1"].scalar < 100.0);

        // Cool and stopped: full duty regardless of temperature.
        let stalled = compiled.tick(
            &TickInput::new(&readings(&hw, 30.0, 0.0), 0.1).with_tachometers(&tachs),
            &mut state,
        );
        assert_eq!(stalled.commands["chip/pwm/1"].scalar, 100.0);

        // The channel with no tachometer still follows the curve.
        assert!(stalled.commands["chip/pwm/2"].scalar < 100.0);
    }

    #[test]
    fn presets_prefer_a_cpu_sensor_over_an_arbitrary_one() {
        let hw = hardware();
        let balanced = presets(&hw)
            .into_iter()
            .find(|p| p.id == "balanced")
            .unwrap();

        let uses_cpu = balanced.graph.nodes.values().any(
            |n| matches!(&n.kind, NodeKind::Sensor { sensor_id, .. } if sensor_id == "cpu/package"),
        );
        assert!(uses_cpu, "should have picked the CPU package sensor");
    }

    #[test]
    fn hardware_specific_presets_are_omitted_rather_than_offered_broken() {
        // No tachometer anywhere: stall protection cannot work, so it is not offered.
        let mut hw = hardware();
        for channel in &mut hw.channels {
            channel.tachometer = None;
        }
        assert!(!presets(&hw).iter().any(|p| p.id == "stall-protected"));

        // Only one temperature: nothing to compare against.
        let mut hw = hardware();
        hw.sensors
            .retain(|s| s.quantity != Quantity::Temperature || s.id == "cpu/package");
        assert!(!presets(&hw).iter().any(|p| p.id == "cpu-and-gpu"));
        // But the single-sensor ones are still there.
        assert!(presets(&hw).iter().any(|p| p.id == "balanced"));
    }

    #[test]
    fn a_machine_with_no_sensors_still_gets_a_working_configuration() {
        // Degraded hardware support must not mean no way to control the fans at all.
        let hw = HardwareSummary {
            sensors: vec![],
            channels: hardware().channels,
        };
        let offered = presets(&hw);

        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].id, "fixed");
        assert!(offered[0].graph.validate().is_ok());
    }

    #[test]
    fn a_machine_with_no_channels_is_offered_nothing() {
        let hw = HardwareSummary {
            sensors: hardware().sensors,
            channels: vec![],
        };
        assert!(presets(&hw).is_empty());
    }

    #[test]
    fn preset_ids_are_unique_and_every_preset_is_named() {
        let hw = hardware();
        let offered = presets(&hw);

        let mut ids: Vec<&str> = offered.iter().map(|p| p.id.as_str()).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate preset ids");

        for preset in &offered {
            assert!(!preset.name.is_empty());
            assert!(!preset.description.is_empty());
        }
    }
}
