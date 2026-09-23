//! Reading a FanControl configuration.
//!
//! Interoperability, by parsing a JSON file. Everything below was established by reading
//! configurations written by the tool itself — see the fixtures in `tests/` — and nothing
//! here derives from its code.
//!
//! # The shape
//!
//! A root object with `__VERSION__` and a section holding `Controls`, `FanCurves`,
//! `FanSensors` and `TemperatureSensors`. A control names a channel, whether it is
//! enabled, and which curve drives it; a curve is either a straight line between two
//! temperature/speed pairs or a fixed value.
//!
//! **The section key moved.** Older files put it under `Main`, newer ones under
//! `FanControl`. Both are accepted, and the key present — not the version number — is
//! what selects the layout, because the version number turned out not to be reliable: a
//! file named `backup_V255_userConfig.json` on the reference machine declares version
//! 265 internally. A discriminator that is right by construction beats one that is right
//! by convention.
//!
//! # The trap that shaped this module
//!
//! A fixed-speed curve stores its value in a field called `Percent`. On the reference
//! machine, the AIO pump's fixed curve holds `2200`, and an older backup of the same
//! curve holds `1500`. Those are not percentages. The owner of that machine runs the pump
//! at a deliberate fixed **RPM**, and the field name simply does not mean what it says in
//! that mode.
//!
//! Clamping 2200 into range would have produced a perfectly plausible profile that runs a
//! pump at 100 % — quiet failure in the worst direction, on the one channel whose whole
//! purpose was to be slow. So a value that cannot be a duty is **refused**, named in a
//! note, and left for a person. There is no default here worth substituting.
//!
//! # What cannot come across
//!
//! * **Channels on hardware we do not drive.** GPU fans and embedded-controller headers
//!   appear in these files; we address Super I/O channels.
//! * **Start and stop thresholds.** The other tool can let a fan stop entirely and
//!   restart it at a higher duty. We have no node for that yet, so the numbers are
//!   carried into [`Calibration`] rather than silently dropped or half-implemented.
//! * **Curve kinds beyond a line and a constant.** Named in a note rather than
//!   approximated, because guessing at the shape of somebody's fan curve is how a machine
//!   ends up quieter than it should be.

use std::collections::BTreeMap;

use of_core::{CurvePoint, Graph, NodeId, NodeKind, PortRef};
use of_units::Quantity;
use serde::Deserialize;

use super::{Calibration, CalibrationPoint, Fidelity, Imported, Note};
use crate::Profile;
use crate::presets::{HardwareSummary, SensorSummary};

/// Where FanControl keeps its configuration, relative to its install directory.
///
/// The configuration lives beside the program rather than in a user profile directory,
/// so finding the install is finding the configuration.
pub const CONFIG_LEAF: &str = "Configurations/userConfig.json";

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("this file is not valid JSON: {0}")]
    Malformed(#[from] serde_json::Error),

    #[error(
        "this does not look like a FanControl configuration: it has neither a \
         `FanControl` nor a `Main` section"
    )]
    UnknownLayout,
}

// --- the on-disk shape ----------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Root {
    #[serde(rename = "__VERSION__")]
    version: Option<String>,
    #[serde(rename = "FanControl")]
    modern: Option<Section>,
    #[serde(rename = "Main")]
    legacy: Option<Section>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Section {
    #[serde(rename = "Controls")]
    controls: Vec<Control>,
    #[serde(rename = "FanCurves")]
    fan_curves: Vec<RawCurve>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Control {
    #[serde(rename = "NickName")]
    nick_name: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Identifier")]
    identifier: String,
    #[serde(rename = "Enable")]
    enable: bool,
    #[serde(rename = "SelectedFanCurve")]
    selected_fan_curve: Option<CurveRef>,
    #[serde(rename = "SelectedStart")]
    selected_start: Option<f64>,
    #[serde(rename = "SelectedStop")]
    selected_stop: Option<f64>,
    #[serde(rename = "MinimumPercent")]
    minimum_percent: Option<f64>,
    #[serde(rename = "ManualControlValue")]
    manual_control_value: Option<f64>,
    #[serde(rename = "ManualControl")]
    manual_control: bool,
    /// Rows of `[duty, rpm]`, with a trailing flag in newer files. Kept loosely typed
    /// because the arity changed between versions and the first two columns did not.
    #[serde(rename = "Calibration")]
    calibration: Vec<Vec<serde_json::Value>>,
}

impl Control {
    fn label(&self) -> String {
        self.nick_name
            .clone()
            .or_else(|| self.name.clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.identifier.clone())
    }
}

#[derive(Debug, Deserialize)]
struct CurveRef {
    #[serde(rename = "Name")]
    name: String,
}

#[derive(Debug, Deserialize)]
struct Ident {
    #[serde(rename = "Identifier")]
    identifier: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawCurve {
    #[serde(rename = "Name")]
    name: String,
    /// 0 is a line between two points; 1 is a fixed value. Anything else is a kind we do
    /// not model, and is named rather than approximated.
    #[serde(rename = "CommandMode")]
    command_mode: i64,
    #[serde(rename = "SelectedTempSource")]
    selected_temp_source: Option<Ident>,
    #[serde(rename = "MinimumTemperature")]
    minimum_temperature: Option<f64>,
    #[serde(rename = "MaximumTemperature")]
    maximum_temperature: Option<f64>,
    #[serde(rename = "MinimumFanSpeed")]
    minimum_fan_speed: Option<f64>,
    #[serde(rename = "MaximumFanSpeed")]
    maximum_fan_speed: Option<f64>,
    #[serde(rename = "Percent")]
    percent: Option<f64>,
    #[serde(rename = "HysteresisConfig")]
    hysteresis: Option<Hysteresis>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Hysteresis {
    #[serde(rename = "ResponseTimeUp")]
    response_time_up: f64,
    #[serde(rename = "ResponseTimeDown")]
    response_time_down: f64,
    #[serde(rename = "HysteresisValueUp")]
    value_up: f64,
    #[serde(rename = "HysteresisValueDown")]
    value_down: f64,
}

const MODE_LINEAR: i64 = 0;
const MODE_FLAT: i64 = 1;

// --- identifiers ----------------------------------------------------------------------

/// A hardware reference in the foreign file, split into its parts.
///
/// The form is `/lpc/<chip>/<kind>/<index>`, with other roots for hardware reached
/// another way (a GPU vendor API, for instance).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ForeignId {
    chip: String,
    kind: String,
    index: usize,
}

impl ForeignId {
    fn parse(raw: &str) -> Option<Self> {
        let parts: Vec<&str> = raw.split('/').filter(|p| !p.is_empty()).collect();
        // `lpc` is the bus, then the chip, then what on it, then which one.
        match parts.as_slice() {
            ["lpc", chip, kind, index] => Some(Self {
                chip: (*chip).to_owned(),
                kind: (*kind).to_owned(),
                index: index.parse().ok()?,
            }),
            _ => None,
        }
    }
}

// --- layout ---------------------------------------------------------------------------

/// Mirrors the presets so an imported graph opens looking hand-arranged.
const COLUMN_WIDTH: f32 = 280.0;
const ROW_HEIGHT: f32 = 150.0;
const ORIGIN: (f32, f32) = (60.0, 60.0);

fn place(graph: &mut Graph, id: &str, kind: NodeKind, column: usize, row: usize) -> NodeId {
    let node = graph.insert(id, kind);
    if let Some(instance) = graph.nodes.get_mut(&node) {
        instance.position = (
            ORIGIN.0 + column as f32 * COLUMN_WIDTH,
            ORIGIN.1 + row as f32 * ROW_HEIGHT,
        );
    }
    node
}

// --- translation ----------------------------------------------------------------------

/// Translate a FanControl configuration into a profile to review.
///
/// `hw` is what this machine actually has. Every translated reference is checked against
/// it rather than trusted, so a configuration written on another machine — or for a
/// header that has since moved — produces notes instead of a graph that addresses
/// channels which are not there.
pub fn import(json: &str, hw: &HardwareSummary) -> Result<Imported, ImportError> {
    let root: Root = serde_json::from_str(json)?;
    let section = root
        .modern
        .or(root.legacy)
        .ok_or(ImportError::UnknownLayout)?;

    let mut notes = Vec::new();
    if let Some(version) = &root.version {
        notes.push(Note::new(
            Fidelity::Exact,
            "Configuration",
            format!("Read a FanControl configuration, format version {version}."),
        ));
    }

    let curves: BTreeMap<&str, &RawCurve> = section
        .fan_curves
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();

    let mut graph = Graph::default();
    let mut calibration = Vec::new();
    // Curve subgraphs are built once and shared, the way the source file shares them by
    // name: two fans on one curve should stay two fans on one curve.
    let mut built: BTreeMap<String, Option<PortRef>> = BTreeMap::new();
    let mut row = 0usize;

    for control in &section.controls {
        let label = control.label();

        let Some(channel) = resolve_channel(&control.identifier, hw) else {
            notes.push(skip_channel_note(&label, &control.identifier));
            continue;
        };

        // Taken even from a control that is switched off: a measurement of where this fan
        // stalls is worth having however the fan is currently driven.
        if let Some(cal) = read_calibration(control, &channel, &label) {
            calibration.push(cal);
        }

        if !control.enable {
            notes.push(Note::new(
                Fidelity::Skipped,
                label,
                "Was turned off, so it is left to the motherboard's own fan curve here \
                 too. Nothing about this fan changes.",
            ));
            continue;
        }

        // A control pinned to a manual value is its own kind of curve, and the simplest
        // one to carry across.
        if control.manual_control {
            let Some(value) = control.manual_control_value else {
                notes.push(Note::new(
                    Fidelity::Skipped,
                    label,
                    "Was set to manual control with no value recorded.",
                ));
                continue;
            };
            match duty_percent(value) {
                Some(duty) => {
                    let node = place(
                        &mut graph,
                        &format!("manual-{row}"),
                        NodeKind::Constant { value: duty },
                        2,
                        row,
                    );
                    attach(&mut graph, &PortRef::new(node, "out"), &channel, row);
                    notes.push(Note::new(
                        Fidelity::Exact,
                        label,
                        format!("Fixed at {duty} %, as it was set manually."),
                    ));
                }
                None => notes.push(not_a_duty_note(&label, value)),
            }
            row += 1;
            continue;
        }

        let Some(curve_ref) = &control.selected_fan_curve else {
            notes.push(Note::new(
                Fidelity::Skipped,
                label,
                "Was enabled but had no curve selected, so there was nothing to bring \
                 across. This fan is left to the motherboard.",
            ));
            continue;
        };

        let Some(raw) = curves.get(curve_ref.name.as_str()) else {
            notes.push(Note::new(
                Fidelity::Skipped,
                label,
                format!(
                    "Refers to a curve called \"{}\" that is not in this file.",
                    curve_ref.name
                ),
            ));
            continue;
        };

        // A fixed speed given in RPM is resolved per control and deliberately *not*
        // shared: the duty that spins one fan at 2200 rpm is not the duty that spins
        // another at 2200 rpm, so caching it by curve name would hand one fan the other
        // fan's answer.
        let source = match fixed_rpm(raw) {
            Some(rpm) => from_measurements(&mut graph, rpm, raw, control, &label, &mut notes, row),
            None => match built.get(&curve_ref.name) {
                // Every other kind of curve is built once, however many controls use it.
                Some(existing) => existing.clone(),
                None => {
                    let built_now = build_curve(&mut graph, raw, hw, &mut notes, row);
                    built.insert(curve_ref.name.clone(), built_now.clone());
                    built_now
                }
            },
        };

        let Some(source) = source else {
            notes.push(Note::new(
                Fidelity::Skipped,
                label,
                format!(
                    "Is driven by \"{}\", which could not be brought across. This fan is \
                     left to the motherboard.",
                    curve_ref.name
                ),
            ));
            continue;
        };

        // A floor the other tool refused to command below is part of how this fan was
        // tuned, so it travels with the connection rather than with the shared curve.
        let end = match control.minimum_percent.filter(|m| *m > 0.0) {
            Some(min) => {
                let clamp = place(
                    &mut graph,
                    &format!("floor-{row}"),
                    NodeKind::Clamp {
                        min: min.clamp(0.0, 100.0),
                        max: 100.0,
                    },
                    3,
                    row,
                );
                graph.connect(source.clone(), PortRef::new(clamp.clone(), "in"));
                notes.push(Note::new(
                    Fidelity::Exact,
                    &label,
                    format!("Never commanded below {min} %, as before."),
                ));
                PortRef::new(clamp, "out")
            }
            None => source,
        };

        attach(&mut graph, &end, &channel, row);
        notes.push(Note::new(
            Fidelity::Exact,
            &label,
            format!("Driven by \"{}\".", curve_ref.name),
        ));

        if control.selected_stop.is_some_and(|s| s > 0.0)
            || control.selected_start.is_some_and(|s| s > 0.0)
        {
            notes.push(Note::new(
                Fidelity::NeedsAttention,
                &label,
                format!(
                    "Was allowed to stop below {} % and restarted at {} %. OpenFan has no \
                     stop-and-restart control yet, so this fan will keep turning instead \
                     of stopping. The measured speeds are kept, so nothing has to be \
                     rediscovered by running the fan down to a stall again.",
                    control.selected_stop.unwrap_or(0.0),
                    control.selected_start.unwrap_or(0.0),
                ),
            ));
        }

        row += 1;
    }

    Ok(Imported {
        profile: Profile::new("Imported from FanControl", graph),
        notes,
        calibration,
    })
}

/// The speed a fixed curve asks for, when it is a speed rather than a percentage.
///
/// A fixed-speed curve keeps its value in a field named `Percent`. When that value cannot
/// be a percentage it is an RPM — the field name simply does not mean what it says in
/// that mode. Recognising which is which is the whole of the distinction.
fn fixed_rpm(raw: &RawCurve) -> Option<f64> {
    if raw.command_mode != MODE_FLAT {
        return None;
    }
    let value = raw.percent?;
    (value.is_finite() && value > 100.0).then_some(value)
}

/// Turn a fixed speed in RPM into a duty, using the measurements in the same file.
///
/// This is the difference between refusing a curve and bringing it across. We command
/// duty and the other tool commanded speed, and the bridge between them is the table it
/// left behind — measured on this fan, on this machine, by its owner.
///
/// Returns `None` when the measurements do not reach that speed, which is the honest
/// answer rather than an extrapolated one. [`Calibration::duty_reaching`] holds the rules.
fn from_measurements(
    graph: &mut Graph,
    rpm: f64,
    raw: &RawCurve,
    control: &Control,
    label: &str,
    notes: &mut Vec<Note>,
    row: usize,
) -> Option<PortRef> {
    let measured = read_calibration(control, "", label)?;

    let Some(duty) = measured.duty_reaching(rpm) else {
        notes.push(Note::new(
            Fidelity::NeedsAttention,
            label,
            format!(
                "Was held at a fixed {rpm} rpm. OpenFan commands a percentage rather than a \
                 speed, and the measurements in this configuration do not say which percentage \
                 produces {rpm} rpm on this fan — so it is left to the motherboard rather than \
                 given a guessed number."
            ),
        ));
        return None;
    };

    let rounded = (duty * 10.0).round() / 10.0;
    let node = place(
        graph,
        &format!("rpm-{row}"),
        NodeKind::Constant { value: rounded },
        2,
        row,
    );

    notes.push(Note::new(
        Fidelity::NeedsAttention,
        label,
        format!(
            "Was held at a fixed {rpm} rpm by \"{}\", and is set to {rounded} % here. \
             OpenFan commands a percentage rather than a speed, and {rounded} % is what \
             your own measurements in that configuration give for {rpm} rpm on this fan. \
             Worth knowing: it is the percentage that is held steady now, not the speed, \
             so nothing will compensate if this fan slows with age.",
            raw.name
        ),
    ));

    Some(PortRef::new(node, "out"))
}

/// Wire a duty source into a channel.
fn attach(graph: &mut Graph, source: &PortRef, channel: &str, row: usize) {
    let out = place(
        graph,
        &format!("fan-{row}"),
        NodeKind::FanOutput {
            channel: channel.to_owned(),
        },
        4,
        row,
    );
    graph.connect(source.clone(), PortRef::new(out, "duty"));
}

/// Build the nodes for one foreign curve, returning where its duty comes out.
///
/// `None` means the curve could not be translated; the caller turns that into a note
/// against every control that used it.
fn build_curve(
    graph: &mut Graph,
    raw: &RawCurve,
    hw: &HardwareSummary,
    notes: &mut Vec<Note>,
    row: usize,
) -> Option<PortRef> {
    match raw.command_mode {
        MODE_FLAT => {
            let value = raw.percent?;
            match duty_percent(value) {
                Some(duty) => {
                    let node = place(
                        graph,
                        &format!("curve-{}", sanitize(&raw.name)),
                        NodeKind::Constant { value: duty },
                        2,
                        row,
                    );
                    notes.push(Note::new(
                        Fidelity::Exact,
                        &raw.name,
                        format!("A fixed {duty} %."),
                    ));
                    Some(PortRef::new(node, "out"))
                }
                None => {
                    notes.push(not_a_duty_note(&raw.name, value));
                    None
                }
            }
        }

        MODE_LINEAR => {
            let (min_t, max_t) = (raw.minimum_temperature?, raw.maximum_temperature?);
            let (min_s, max_s) = (raw.minimum_fan_speed?, raw.maximum_fan_speed?);

            if max_t <= min_t {
                notes.push(Note::new(
                    Fidelity::Skipped,
                    &raw.name,
                    format!(
                        "Runs from {min_t} °C to {max_t} °C, which is not a rising range. \
                         Refusing to guess what was meant."
                    ),
                ));
                return None;
            }
            let (Some(min_s), Some(max_s)) = (duty_percent(min_s), duty_percent(max_s)) else {
                notes.push(not_a_duty_note(&raw.name, min_s.max(max_s)));
                return None;
            };

            let source = raw.selected_temp_source.as_ref()?;
            let sensor = resolve_temperature(&source.identifier, hw, &raw.name, notes)?;

            let mut column = 0usize;
            let sensor_node = place(
                graph,
                &format!("temp-{}", sanitize(&raw.name)),
                NodeKind::Sensor {
                    sensor_id: sensor,
                    quantity: Quantity::Temperature,
                },
                column,
                row,
            );
            let mut tip = PortRef::new(sensor_node, "out");
            column += 1;

            if let Some(h) = &raw.hysteresis {
                // One band where the source has two. The wider of the two is the
                // conservative reading: it holds the fan steady across at least as much
                // movement as was asked for, which is the direction that cannot surprise
                // someone with more noise than they configured.
                let band = h.value_up.max(h.value_down);
                if band > 0.0 {
                    let hold = place(
                        graph,
                        &format!("hold-{}", sanitize(&raw.name)),
                        NodeKind::Hold { band },
                        column,
                        row,
                    );
                    graph.connect(tip.clone(), PortRef::new(hold.clone(), "in"));
                    tip = PortRef::new(hold, "out");
                    column += 1;

                    notes.push(Note::new(
                        Fidelity::Approximated,
                        &raw.name,
                        format!(
                            "Held steady until the temperature moves {band} °C. The \
                             original had separate rising and falling bands ({} and {}); \
                             the wider one is used.",
                            h.value_up, h.value_down
                        ),
                    ));
                }

                // Likewise one time constant where the source has two. Here the *shorter*
                // is the safe reading: responding sooner than configured can only cool
                // harder, while responding later could leave a hot part waiting.
                let response = h.response_time_up.min(h.response_time_down);
                if response > 0.0 {
                    let smooth = place(
                        graph,
                        &format!("smooth-{}", sanitize(&raw.name)),
                        NodeKind::LowPass {
                            tau_seconds: response,
                        },
                        column,
                        row,
                    );
                    graph.connect(tip.clone(), PortRef::new(smooth.clone(), "in"));
                    tip = PortRef::new(smooth, "out");
                    column += 1;

                    notes.push(Note::new(
                        Fidelity::Approximated,
                        &raw.name,
                        format!(
                            "Smoothed over {response} s. The original had separate rising \
                             and falling response times ({} s and {} s); the quicker one \
                             is used, so this never reacts more slowly than it did.",
                            h.response_time_up, h.response_time_down
                        ),
                    ));
                }
            }

            let curve = place(
                graph,
                &format!("line-{}", sanitize(&raw.name)),
                NodeKind::Curve {
                    points: vec![
                        CurvePoint { x: min_t, y: min_s },
                        CurvePoint { x: max_t, y: max_s },
                    ],
                },
                column.max(2),
                row,
            );
            graph.connect(tip, PortRef::new(curve.clone(), "in"));

            notes.push(Note::new(
                Fidelity::Exact,
                &raw.name,
                format!("{min_s} % at {min_t} °C rising to {max_s} % at {max_t} °C."),
            ));

            Some(PortRef::new(curve, "out"))
        }

        other => {
            notes.push(Note::new(
                Fidelity::Skipped,
                &raw.name,
                format!(
                    "Is a kind of curve OpenFan does not model yet (mode {other}). It is \
                     left out rather than approximated — guessing at the shape of a fan \
                     curve is not something this should do on its own."
                ),
            ));
            None
        }
    }
}

/// A percentage, or nothing.
///
/// **The load-bearing function of this module.** A fixed-speed curve's value lives in a
/// field named `Percent` that does not always hold one; out-of-range values are refused
/// rather than clamped, because clamping produces a plausible profile that runs a fan
/// flat out. See the module documentation.
fn duty_percent(value: f64) -> Option<f64> {
    (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(value)
}

fn not_a_duty_note(subject: &str, value: f64) -> Note {
    Note::new(
        Fidelity::NeedsAttention,
        subject,
        format!(
            "Holds the value {value}, which is not a percentage. It is most likely a fixed \
             speed in RPM, which OpenFan cannot command directly yet. Left out \
             deliberately: treating it as a percentage would run this fan at full speed."
        ),
    )
}

fn skip_channel_note(label: &str, identifier: &str) -> Note {
    Note::new(
        Fidelity::Skipped,
        label,
        format!(
            "Drives {identifier}, which is not a fan header OpenFan controls on this \
             machine."
        ),
    )
}

/// Translate a control identifier into one of our channel ids, if this machine has it.
fn resolve_channel(identifier: &str, hw: &HardwareSummary) -> Option<String> {
    let id = ForeignId::parse(identifier)?;
    if id.kind != "control" {
        return None;
    }
    // Their `control/N` is our `pwm/N` on the same chip. Confirmed against the machine's
    // own inventory rather than assumed, so a configuration from a different board
    // produces a note instead of a graph addressing channels that are not there.
    let candidate = format!("{}/pwm/{}", id.chip, id.index);
    hw.channels
        .iter()
        .find(|c| c.id == candidate)
        .map(|c| c.id.clone())
}

/// Translate a temperature source, which is the one reference that cannot be trusted.
///
/// Their temperatures are numbered by position; ours are keyed by what the input
/// measures, deliberately, so an id keeps meaning the same thing across re-enumeration.
/// There is no faithful translation between the two, so this picks the chip's Nth
/// temperature and says so — every imported curve carries a note asking a person to check
/// the fan is watching the thing they meant.
fn resolve_temperature(
    identifier: &str,
    hw: &HardwareSummary,
    subject: &str,
    notes: &mut Vec<Note>,
) -> Option<String> {
    let Some(id) = ForeignId::parse(identifier) else {
        notes.push(Note::new(
            Fidelity::Skipped,
            subject,
            format!("Follows {identifier}, which is not a sensor OpenFan reads."),
        ));
        return None;
    };
    if id.kind != "temperature" {
        notes.push(Note::new(
            Fidelity::Skipped,
            subject,
            format!("Follows {identifier}, which is not a temperature."),
        ));
        return None;
    }

    let prefix = format!("{}/temp/", id.chip);
    let candidates: Vec<&SensorSummary> = hw
        .sensors
        .iter()
        .filter(|s| s.quantity == Quantity::Temperature && s.id.starts_with(&prefix))
        .collect();

    let Some(chosen) = candidates.get(id.index) else {
        notes.push(Note::new(
            Fidelity::Skipped,
            subject,
            format!(
                "Follows {identifier}, and this machine does not report that many \
                 temperatures on {}. The curve is left out rather than pointed at a \
                 different sensor.",
                id.chip
            ),
        ));
        return None;
    };

    notes.push(Note::new(
        Fidelity::NeedsAttention,
        subject,
        format!(
            "Now follows {}. The original named its temperature by position \
             ({identifier}) and OpenFan names sensors by what they measure, so the two \
             cannot be matched up with certainty — please check this is the temperature \
             you meant before relying on it.",
            chosen.label
        ),
    ));
    Some(chosen.id.clone())
}

/// Pull the measured duty/speed table out of a control.
fn read_calibration(control: &Control, channel: &str, label: &str) -> Option<Calibration> {
    let mut points: Vec<CalibrationPoint> = control
        .calibration
        .iter()
        .filter_map(|row| {
            // `[duty, rpm]` in older files and `[duty, rpm, flag]` in newer ones. Only
            // the first two columns have ever been needed.
            let duty = row.first()?.as_f64()?;
            let rpm = row.get(1)?.as_f64()?;
            (duty.is_finite() && rpm.is_finite() && (0.0..=100.0).contains(&duty)).then_some(
                CalibrationPoint {
                    duty_percent: duty,
                    rpm,
                },
            )
        })
        .collect();

    if points.is_empty() {
        return None;
    }
    points.sort_by(|a, b| a.duty_percent.total_cmp(&b.duty_percent));

    Some(Calibration {
        channel: channel.to_owned(),
        label: label.to_owned(),
        points,
        start_percent: control.selected_start.filter(|v| *v > 0.0),
        stop_percent: control.selected_stop.filter(|v| *v > 0.0),
        minimum_percent: control.minimum_percent.filter(|v| *v > 0.0),
    })
}

/// Make a foreign name usable as a node id.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        "curve".to_owned()
    } else {
        trimmed
    }
}
