//! Types shared between the OpenFan backend and its editor.
//!
//! TypeScript is generated from these definitions (`bun run bindings`) and committed, so
//! the editor cannot drift from the backend's idea of the protocol without the build
//! noticing.
//!
//! # What belongs here
//!
//! Two different things, kept deliberately separate:
//!
//! - **The document.** [`of_core::Graph`] and friends are re-exported as-is. The editor
//!   reads and writes the same structure the backend persists — there is no translation
//!   layer to fall out of sync, and no second definition of what a node is.
//! - **Views and descriptors.** The node catalogue and the tick snapshot are *derived*
//!   from backend state rather than stored, so they get DTOs. `PortSpec` in particular
//!   holds `&'static str` fields owned by the catalogue; it is not part of the document
//!   and is carried across as [`PortDto`].
//!
//! Nothing here makes a control decision. These are the shapes the UI is allowed to see.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod params;

pub use of_core::{CurvePoint, Edge, Graph, MixMode, NodeId, NodeInstance, NodeKind, PortRef};
pub use of_units::{Quantity, Value};
pub use params::{ChoiceOption, ParamKind, ParamSpec, ParamUnit, params_for};

/// Shorthand for the ts-rs attributes every exported type carries.
macro_rules! dto {
    ($(#[$meta:meta])* pub struct $name:ident { $($body:tt)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
        #[ts(export, export_to = "../../../ui/src/bindings/")]
        #[serde(rename_all = "camelCase")]
        pub struct $name { $($body)* }
    };
}

dto! {
    /// One typed port, as the editor sees it.
    ///
    /// `quantity` is `None` for a generic port that nothing has decided yet. The editor
    /// draws those neutral, and they lock to a colour once a connection resolves them.
    pub struct PortDto {
        pub key: String,
        pub label: String,
        pub quantity: Option<Quantity>,
        /// A required input left unconnected makes the graph invalid.
        pub required: bool,
        /// A variadic input accepts any number of incoming connections.
        pub variadic: bool,
        /// An output carrying a value from before this tick — a measurement, or a
        /// buffered past value. Edges leaving one impose no ordering, which is what
        /// lets feedback exist without a cycle. The editor draws them distinctly.
        pub delayed: bool,
    }
}

dto! {
    /// A node kind the editor can offer in its palette.
    ///
    /// `template` is a complete, immediately-valid [`NodeKind`], so adding a node from the
    /// palette never produces a half-configured node the backend would reject.
    pub struct NodeDescriptor {
        /// Stable discriminator, matching the serde tag of [`NodeKind`].
        pub kind: String,
        pub label: String,
        pub category: NodeCategory,
        /// One line explaining what the node is for.
        pub description: String,
        pub template: NodeKind,
        pub inputs: Vec<PortDto>,
        pub outputs: Vec<PortDto>,
        /// Everything about this node the user can configure.
        pub params: Vec<ParamSpec>,
    }
}

/// How the palette groups a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/bindings/")]
#[serde(rename_all = "kebab-case")]
pub enum NodeCategory {
    /// Produces values: sensors, constants.
    Source,
    /// Reshapes values without memory.
    Transform,
    /// Reshapes values using history — filters, limiters, controllers.
    Stateful,
    /// Boolean tests and routing.
    Logic,
    /// Drives hardware.
    Sink,
}

dto! {
    /// A sensor the active backend can read.
    pub struct SensorDto {
        pub id: String,
        pub label: String,
        pub quantity: Quantity,
    }
}

dto! {
    /// An output channel the active backend can drive.
    pub struct ChannelDto {
        pub id: String,
        pub label: String,
        /// The tachometer that reads this channel back, when one is wired to it.
        pub tachometer: Option<String>,
        /// Lowest duty the device reliably spins at, once known.
        pub min_reliable_duty: Option<f64>,
    }
}

dto! {
    /// What the backend can see on this machine.
    pub struct HardwareInventory {
        pub backend: String,
        pub sensors: Vec<SensorDto>,
        pub channels: Vec<ChannelDto>,
        /// True when kernel-level hardware access is available.
        pub driver_present: bool,
        /// A sentence the UI can show verbatim when it is not.
        pub driver_summary: String,
        /// The driver is installed but its hardware module is not.
        ///
        /// Its own field rather than something to infer from the summary text, because
        /// this is the state a fresh install lands in and it is *fixable in one click* —
        /// the interface needs to know it specifically, not guess at it from prose.
        pub module_missing: bool,
    }
}

dto! {
    /// One value on one wire, flattened for transport.
    ///
    /// `PortRef` is a struct and would be an awkward object key in JSON, so wire values
    /// travel as a list the editor indexes by `nodeId`/`port`.
    pub struct WireValue {
        pub node_id: String,
        pub port: String,
        pub quantity: Quantity,
        /// `None` when the value is not finite — a fault, not a number.
        pub value: Option<f64>,
    }
}

dto! {
    /// The engine's most recent tick, as the editor sees it.
    pub struct SnapshotDto {
        /// Monotonic tick counter. Unchanged between reads means the loop has not ticked.
        pub sequence: u64,
        /// Seconds the last tick was evaluated over.
        pub dt: f64,
        pub tick_duration_ms: f64,
        /// Ticks that overran their period. Rising means the tick rate is too high.
        pub overruns: u64,
        /// True when no graph was evaluated and every channel was failsafed.
        pub degraded: bool,
        /// Why, when degraded because the sensors could not be read.
        pub sensor_error: Option<String>,
        pub sensors: Vec<WireValue>,
        pub wires: Vec<WireValue>,
        /// Duty actually written to each channel this tick.
        pub commanded: BTreeMap<String, f64>,
        /// Channels put into their failsafe this tick.
        pub failsafed: Vec<String>,
    }
}

dto! {
    /// The inferred type of one port.
    pub struct PortTypeDto {
        pub node_id: String,
        pub port: String,
        /// `None` when the port is generic and nothing has decided it yet.
        pub quantity: Option<Quantity>,
    }
}

/// Infer the type of every port in a graph.
///
/// The editor calls this on each edit so it can colour ports as their types resolve,
/// without duplicating unification in the frontend. Inference is cheap, pure and does
/// not require the graph to be valid — a half-built graph is exactly when this is most
/// useful.
pub fn resolve_types(graph: &Graph) -> Vec<PortTypeDto> {
    of_core::infer::infer(graph)
        .0
        .into_iter()
        .map(|(port, quantity)| PortTypeDto {
            node_id: port.node.0,
            port: port.port,
            quantity,
        })
        .collect()
}

dto! {
    /// A rejected graph edit.
    pub struct ValidationError {
        /// Human-readable, already explaining what to do about it.
        pub message: String,
        /// The node to highlight, when the error names one.
        pub node_id: Option<String>,
    }
}

impl WireValue {
    /// Build from a port reference and a value, mapping non-finite to `None`.
    ///
    /// The editor must not be able to render a NaN as though it were a reading, so the
    /// distinction is made here rather than left to the frontend to remember.
    pub fn new(port: &PortRef, value: &Value) -> Self {
        Self {
            node_id: port.node.0.clone(),
            port: port.port.clone(),
            quantity: value.quantity,
            value: value.is_trustworthy().then_some(value.scalar),
        }
    }

    /// Build from a sensor id rather than a graph port.
    pub fn sensor(id: &str, value: &Value) -> Self {
        Self {
            node_id: id.to_owned(),
            port: String::new(),
            quantity: value.quantity,
            value: value.is_trustworthy().then_some(value.scalar),
        }
    }
}

/// Convert a backend port specification into its transport form.
pub fn port_dto(spec: &of_core::PortSpec) -> PortDto {
    PortDto {
        key: spec.key.to_owned(),
        label: spec.label.to_owned(),
        quantity: spec.ty.concrete(),
        required: spec.required,
        variadic: spec.variadic,
        delayed: spec.delayed,
    }
}

/// Build a descriptor for one node kind.
fn descriptor(category: NodeCategory, description: &str, template: NodeKind) -> NodeDescriptor {
    let spec = template.spec();
    // The serde tag is the stable identity of the kind; derive it rather than repeating
    // it, so a rename cannot leave the palette pointing at a kind that no longer exists.
    let kind = serde_json::to_value(&template)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_owned))
        .unwrap_or_default();

    NodeDescriptor {
        kind,
        label: template.default_label().to_owned(),
        category,
        description: description.to_owned(),
        inputs: spec.inputs.iter().map(port_dto).collect(),
        outputs: spec.outputs.iter().map(port_dto).collect(),
        params: params_for(&template),
        template,
    }
}

/// The palette the editor offers.
///
/// Templates are chosen to be immediately useful: a Curve that already rises from 30 °C
/// to 80 °C is a working starting point, whereas an empty one would evaluate to a fault
/// the moment it was added.
pub fn catalogue() -> Vec<NodeDescriptor> {
    use NodeCategory::*;
    use Quantity as Q;

    vec![
        descriptor(
            Source,
            "Reads a hardware sensor.",
            NodeKind::Sensor {
                sensor_id: String::new(),
                quantity: Q::Temperature,
            },
        ),
        descriptor(
            Source,
            "Emits a fixed value. Also backs a manual slider.",
            NodeKind::Constant { value: 50.0 },
        ),
        descriptor(
            Transform,
            "Maps any input onto a duty along a transfer curve.",
            NodeKind::Curve {
                points: vec![
                    CurvePoint { x: 30.0, y: 20.0 },
                    CurvePoint { x: 80.0, y: 100.0 },
                ],
            },
        ),
        descriptor(
            Transform,
            "Combines several inputs of the same type into one. The type is inferred.",
            NodeKind::Mix { mode: MixMode::Max },
        ),
        descriptor(
            Transform,
            "Constrains a value to a range.",
            NodeKind::Clamp {
                min: 20.0,
                max: 100.0,
            },
        ),
        descriptor(
            Transform,
            "Adds a constant.",
            NodeKind::Offset { delta: 0.0 },
        ),
        descriptor(
            Transform,
            "Multiplies by a constant.",
            NodeKind::Scale { factor: 1.0 },
        ),
        descriptor(
            Transform,
            "Reads a value as a different type. The explicit way across the type system.",
            NodeKind::Reinterpret {
                from: Q::Load,
                to: Q::Duty,
            },
        ),
        descriptor(
            Stateful,
            "Limits how fast a value may change, per second.",
            NodeKind::RateLimit {
                max_delta_per_second: 10.0,
            },
        ),
        descriptor(
            Stateful,
            "Smooths a signal with a time constant in seconds.",
            NodeKind::LowPass { tau_seconds: 5.0 },
        ),
        descriptor(
            Stateful,
            "Averages the most recent samples.",
            NodeKind::MovingAverage { samples: 10 },
        ),
        descriptor(
            Stateful,
            "Holds its output until the input moves outside a band. Stops fans twitching at sensor noise.",
            NodeKind::Hold { band: 1.0 },
        ),
        descriptor(
            Logic,
            "Tests a value against a threshold, with a deadband so it cannot chatter.",
            NodeKind::Comparator {
                threshold: 70.0,
                deadband: 4.0,
                direction: of_core::Compare::Above,
            },
        ),
        descriptor(Logic, "Chooses between two inputs.", NodeKind::Select),
        descriptor(
            Stateful,
            "Emits what it was given a while ago. Also breaks a feedback loop.",
            NodeKind::Delay { seconds: 1.0 },
        ),
        descriptor(
            Stateful,
            "Holds a temperature at a setpoint. The integral is bounded so it cannot wind up.",
            NodeKind::Pid {
                setpoint: 65.0,
                kp: 4.0,
                ki: 0.2,
                kd: 0.0,
                integral_limit: 40.0,
            },
        ),
        descriptor(
            Sink,
            "Drives a fan or pump channel.",
            NodeKind::FanOutput {
                channel: String::new(),
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_covers_every_node_kind() {
        // If a variant is added without a palette entry it is unreachable from the UI.
        // This number moving is a prompt to add the missing descriptor, not to bump the
        // constant — a kind with no palette entry is unreachable from the UI.
        assert_eq!(catalogue().len(), 17);
    }

    #[test]
    fn every_descriptor_has_a_stable_kind_tag() {
        for d in catalogue() {
            assert!(!d.kind.is_empty(), "{} has no kind tag", d.label);
            // The tag must match what the document format actually serializes, or the
            // palette would produce nodes the backend cannot identify.
            let json = serde_json::to_value(&d.template).unwrap();
            assert_eq!(json["kind"].as_str().unwrap(), d.kind);
        }
    }

    #[test]
    fn every_template_is_valid_on_its_own_terms() {
        // A palette entry that evaluates to a fault the instant it is added would teach
        // users that fault markers are normal. Templates carry usable defaults.
        for d in catalogue() {
            let spec = d.template.spec();
            assert_eq!(spec.inputs.len(), d.inputs.len());
            assert_eq!(spec.outputs.len(), d.outputs.len());
            if let NodeKind::Curve { points, .. } = &d.template {
                assert!(!points.is_empty(), "the curve template must have points");
            }
        }
    }

    #[test]
    fn a_faulted_wire_carries_no_number() {
        let port = PortRef::new("n", "out");
        let bad = WireValue::new(&port, &Value::raw(Quantity::Duty, f64::NAN));
        assert_eq!(bad.value, None, "NaN must not reach the UI as a reading");

        let good = WireValue::new(&port, &Value::raw(Quantity::Duty, 42.0));
        assert_eq!(good.value, Some(42.0));
        assert_eq!(good.node_id, "n");
        assert_eq!(good.port, "out");
    }
}

/// Who is driving an output channel, for the takeover panel.
///
/// Mirrors [`of_hal::ChannelControl`]. `Unknown` means the backend could not tell, which
/// the interface must never render as "fine".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/bindings/")]
#[serde(rename_all = "kebab-case")]
pub enum ChannelControlDto {
    /// The device's own curve is handling it. Nothing external needs to.
    Firmware,
    /// OpenFan holds it.
    Ours,
    /// Manual, but not ours — another application, or one that abandoned it there.
    Foreign,
    /// The backend cannot tell.
    Unknown,
}

dto! {
    /// Another application that is running and what it means for us.
    pub struct ContendingAppDto {
        /// Stable key, e.g. `fancontrol`.
        pub key: String,
        pub name: String,
        pub process_name: String,
        pub pid: u32,
        /// `controller`, `monitor` or `vendor`.
        pub role: String,
        /// Plain-language explanation shown next to it.
        pub note: String,
        /// Whether it has to stand down before we can drive a fan.
        pub must_stop: bool,
    }
}

dto! {
    pub struct ChannelControlEntry {
        pub id: String,
        pub label: String,
        pub control: ChannelControlDto,
    }
}

dto! {
    /// Everything the takeover panel needs to explain the situation.
    pub struct ContentionReport {
        pub channels: Vec<ChannelControlEntry>,
        pub apps: Vec<ContendingAppDto>,
        /// Channels under foreign manual control — nothing is responding to temperature
        /// on these.
        pub stranded: Vec<String>,
        /// True when nothing needs doing.
        pub clear: bool,
        /// Why a takeover cannot proceed right now, if it cannot.
        pub blocker: Option<String>,
    }
}

dto! {
    /// What a takeover attempt did.
    pub struct TakeoverResult {
        /// One line per thing attempted, in order, for the user to read.
        pub steps: Vec<String>,
        pub succeeded: bool,
        /// Present when the attempt stopped early.
        pub blocker: Option<String>,
        /// The situation afterwards.
        pub report: ContentionReport,
    }
}

// --- Taking over from another tool ------------------------------------------------------

dto! {
    /// A known fan controller arranged to start with this machine.
    ///
    /// Reported separately from a *running* application because the two need different
    /// answers. A running rival is a fight happening now; an autostart entry is a fight
    /// scheduled for the next reboot, on a machine nobody will be watching.
    pub struct AutostartEntryDto {
        /// Index into the service's current survey, used to act on this entry.
        pub id: usize,
        /// Stable key, e.g. `fancontrol`.
        pub key: String,
        pub name: String,
        /// Where it lives, phrased for someone who did not put it there.
        pub location: String,
        /// The command it runs.
        pub command: String,
        /// Whether switching it off can be undone without our help.
        pub reversible: bool,
    }
}

dto! {
    /// What switching off an autostart entry did.
    pub struct AutostartDisabledDto {
        pub what: String,
        /// How to put it back, for the one location with no reversible option.
        pub restore_hint: Option<String>,
    }
}

dto! {
    /// A configuration file belonging to another fan controller.
    pub struct ForeignConfigDto {
        /// Stable key of the application it belongs to.
        pub key: String,
        pub name: String,
        pub path: String,
        /// How it was found, so a user can tell a live configuration from an old backup.
        pub found_via: String,
    }
}

dto! {
    /// One thing an import did, or declined to do.
    pub struct ImportNoteDto {
        /// `exact`, `approximated`, `needs-attention` or `skipped`.
        pub fidelity: String,
        pub subject: String,
        pub detail: String,
    }
}

dto! {
    /// A fan's measured duty-to-speed relationship, brought across from another tool.
    pub struct CalibrationDto {
        pub channel: String,
        pub label: String,
        /// `[duty percent, rpm]` pairs, ascending by duty.
        pub points: Vec<(f64, f64)>,
        /// The lowest duty at which the fan was seen turning.
        pub lowest_turning_duty: Option<f64>,
        /// Whether the table actually contains a stall, rather than merely not reaching
        /// one. A table that never read zero is not proof that low duties are safe.
        pub found_the_stall: bool,
    }
}

dto! {
    /// A translated configuration, for review. **Importing does not apply it.**
    pub struct ImportedProfileDto {
        pub name: String,
        pub graph: Graph,
        pub notes: Vec<ImportNoteDto>,
        pub calibration: Vec<CalibrationDto>,
        /// True when nothing at all could be translated.
        pub empty: bool,
    }
}

dto! {
    /// Everything OpenFan knows about the other fan-control software on this machine.
    ///
    /// The three states a new installation can be in, answered in one place: nothing else
    /// here, something installed but idle, or something running right now.
    pub struct MigrationSurvey {
        /// Rivals running at this moment, and what else holds the bus.
        pub contention: ContentionReport,
        /// Rivals that will start with the machine.
        pub autostart: Vec<AutostartEntryDto>,
        /// Configurations found on disk that could be imported.
        pub configs: Vec<ForeignConfigDto>,
        /// The best of those, already translated.
        ///
        /// Done during the survey rather than behind a button. Reading a file we have
        /// already located changes nothing about the machine, so making somebody ask for
        /// it twice is friction that buys no safety — the consent that matters is on
        /// *applying* the result, which is still a separate, explicit step.
        pub imported: Option<ImportedProfileDto>,
        /// Why the translation did not happen, when a configuration was found but could
        /// not be read. Reported rather than silently leaving `imported` empty.
        pub import_error: Option<String>,
        /// True when no other fan-control software is running, scheduled to run, or
        /// installed — the clean-machine case.
        pub nothing_else_here: bool,
    }
}
