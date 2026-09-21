//! The parameter schema: what the editor may configure on each node kind.
//!
//! Parameters are described by the backend rather than hand-written into the editor, for
//! the same reason ports are — adding a node kind should not require touching the
//! frontend, and the two cannot then disagree about what a node is configurable by.
//!
//! A spec's `key` is the serde field name on the [`NodeKind`] variant, so applying an
//! edit is literally `{ ...node.kind, [key]: value }`. There is no mapping table to fall
//! out of sync, and a renamed field shows up as a missing control rather than a silently
//! ignored write.

use serde::{Deserialize, Serialize};

use crate::NodeKind;

/// What a numeric parameter is measured in.
///
/// Several parameters are denominated in whatever the node's own `quantity` is set to —
/// a Hold band on a temperature is degrees, on a duty is percent. Those resolve in the
/// editor against the live value rather than being frozen into a fixed string here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/bindings/")]
#[serde(tag = "unit", rename_all = "kebab-case")]
pub enum ParamUnit {
    /// Dimensionless.
    None,
    /// A fixed symbol, such as seconds.
    Fixed { symbol: String },
    /// The symbol of the node's `quantity` parameter.
    Quantity,
    /// The node's quantity, per second.
    QuantityRate,
}

/// One option in a choice parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/bindings/")]
pub struct ChoiceOption {
    /// The serialized value, exactly as the document stores it.
    pub value: String,
    pub label: String,
}

/// How a parameter should be edited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/bindings/")]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ParamKind {
    Number {
        min: Option<f64>,
        max: Option<f64>,
        step: f64,
        unit: ParamUnit,
    },
    Integer {
        // i32 rather than i64: these are editor bounds, and ts-rs maps 64-bit integers
        // to `bigint`, which a number input cannot consume.
        min: Option<i32>,
        max: Option<i32>,
    },
    /// One of the port types.
    Quantity,
    Choice {
        options: Vec<ChoiceOption>,
    },
    /// A sensor from the current hardware inventory.
    Sensor,
    /// An output channel from the current hardware inventory.
    Channel,
    /// A list of transfer-curve points.
    Curve,
}

/// One editable parameter of a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../../ui/src/bindings/")]
#[serde(rename_all = "camelCase")]
pub struct ParamSpec {
    /// The serde field name on the node kind. Editing writes straight to it.
    pub key: String,
    pub label: String,
    /// Shown beneath the control. Inline, never as hover text.
    pub help: Option<String>,
    pub kind: ParamKind,
}

impl ParamSpec {
    fn new(key: &str, label: &str, kind: ParamKind) -> Self {
        Self {
            key: key.to_owned(),
            label: label.to_owned(),
            help: None,
            kind,
        }
    }

    fn help(mut self, help: &str) -> Self {
        self.help = Some(help.to_owned());
        self
    }
}

fn number(key: &str, label: &str, step: f64, unit: ParamUnit) -> ParamSpec {
    ParamSpec::new(
        key,
        label,
        ParamKind::Number {
            min: None,
            max: None,
            step,
            unit,
        },
    )
}

fn bounded(key: &str, label: &str, lo: f64, hi: f64, step: f64, unit: ParamUnit) -> ParamSpec {
    ParamSpec::new(
        key,
        label,
        ParamKind::Number {
            min: Some(lo),
            max: Some(hi),
            step,
            unit,
        },
    )
}

/// A value denominated in the node's own quantity.
fn in_quantity(key: &str, label: &str) -> ParamSpec {
    number(key, label, 0.5, ParamUnit::Quantity)
}

fn seconds(key: &str, label: &str) -> ParamSpec {
    ParamSpec::new(
        key,
        label,
        ParamKind::Number {
            min: Some(0.0),
            max: None,
            step: 0.1,
            unit: ParamUnit::Fixed {
                symbol: "s".to_owned(),
            },
        },
    )
}

fn quantity_picker(key: &str, label: &str) -> ParamSpec {
    ParamSpec::new(key, label, ParamKind::Quantity)
}

fn choice(key: &str, label: &str, options: &[(&str, &str)]) -> ParamSpec {
    ParamSpec::new(
        key,
        label,
        ParamKind::Choice {
            options: options
                .iter()
                .map(|(value, label)| ChoiceOption {
                    value: (*value).to_owned(),
                    label: (*label).to_owned(),
                })
                .collect(),
        },
    )
}

/// The editable parameters of a node kind.
///
/// Every field of every [`NodeKind`] variant appears here. A field with no spec would be
/// invisible in the editor and silently un-editable, which is why the tests assert the
/// spec keys cover the serialized fields exactly.
pub fn params_for(kind: &NodeKind) -> Vec<ParamSpec> {
    match kind {
        NodeKind::Sensor { .. } => vec![
            ParamSpec::new("sensor_id", "Sensor", ParamKind::Sensor),
            quantity_picker("quantity", "Reads").help(
                "Set for you when you pick a sensor. A reading that arrives as a \
                 different quantity faults rather than being used.",
            ),
        ],
        NodeKind::Constant { .. } => vec![
            quantity_picker("quantity", "Type"),
            number("value", "Value", 1.0, ParamUnit::Quantity),
        ],
        NodeKind::Curve { .. } => vec![
            quantity_picker("input", "Input type"),
            ParamSpec::new("points", "Curve", ParamKind::Curve)
                .help("Outside the first and last point the curve is held flat."),
        ],
        NodeKind::Mix { .. } => vec![
            quantity_picker("quantity", "Type"),
            choice(
                "mode",
                "Combine using",
                &[
                    ("max", "Maximum (hottest wins)"),
                    ("min", "Minimum"),
                    ("average", "Average"),
                    ("sum", "Sum"),
                ],
            )
            .help("A dead input poisons the result rather than being averaged away."),
        ],
        NodeKind::Clamp { .. } => vec![
            quantity_picker("quantity", "Type"),
            in_quantity("min", "Minimum"),
            in_quantity("max", "Maximum"),
        ],
        NodeKind::Offset { .. } => vec![
            quantity_picker("quantity", "Type"),
            in_quantity("delta", "Add"),
        ],
        NodeKind::Scale { .. } => vec![
            quantity_picker("quantity", "Type"),
            number("factor", "Multiply by", 0.1, ParamUnit::None),
        ],
        NodeKind::Reinterpret { .. } => vec![
            quantity_picker("from", "From"),
            quantity_picker("to", "To").help(
                "Crossing the type system is deliberate and visible. Only do this where \
                 the conversion genuinely means something.",
            ),
        ],
        NodeKind::RateLimit { .. } => vec![
            quantity_picker("quantity", "Type"),
            number(
                "max_delta_per_second",
                "Maximum change",
                1.0,
                ParamUnit::QuantityRate,
            )
            .help("The main tool against fans hunting on a slow heatsink."),
        ],
        NodeKind::LowPass { .. } => vec![
            quantity_picker("quantity", "Type"),
            seconds("tau_seconds", "Time constant").help(
                "Time to cover about 63% of a step. Tuning is in seconds, so it survives \
                 a change of tick rate.",
            ),
        ],
        NodeKind::MovingAverage { .. } => vec![
            quantity_picker("quantity", "Type"),
            ParamSpec::new(
                "samples",
                "Samples",
                ParamKind::Integer {
                    min: Some(1),
                    max: Some(600),
                },
            ),
        ],
        NodeKind::Hold { .. } => vec![
            quantity_picker("quantity", "Type"),
            in_quantity("band", "Deadband").help(
                "Output holds until the input moves further than this. Stops fans \
                 twitching at sensor noise.",
            ),
        ],
        NodeKind::Comparator { .. } => vec![
            quantity_picker("quantity", "Type"),
            in_quantity("threshold", "Threshold"),
            in_quantity("deadband", "Deadband").help(
                "The input must clear the threshold by half this before the result \
                 flips, so it cannot chatter.",
            ),
            choice(
                "direction",
                "True when",
                &[
                    ("above", "Above the threshold"),
                    ("below", "Below the threshold"),
                ],
            ),
        ],
        NodeKind::Select { .. } => vec![quantity_picker("quantity", "Type")],
        NodeKind::Pid { .. } => vec![
            quantity_picker("quantity", "Measures"),
            in_quantity("setpoint", "Setpoint"),
            number("kp", "Proportional gain", 0.1, ParamUnit::None),
            number("ki", "Integral gain", 0.05, ParamUnit::None),
            number("kd", "Derivative gain", 0.1, ParamUnit::None),
            bounded(
                "integral_limit",
                "Integral limit",
                0.0,
                100.0,
                1.0,
                ParamUnit::Fixed {
                    symbol: "%".to_owned(),
                },
            )
            .help(
                "Caps what the integral term may contribute, so it cannot wind up and \
                 hold the fans at full long after the load has gone.",
            ),
        ],
        NodeKind::FanOutput { .. } => {
            vec![ParamSpec::new("channel", "Channel", ParamKind::Channel)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue;

    /// The serialized field names of a node kind, excluding the `kind` tag itself.
    fn fields_of(kind: &NodeKind) -> Vec<String> {
        let value = serde_json::to_value(kind).expect("node kinds serialize");
        let object = value.as_object().expect("variants are struct-shaped");
        object.keys().filter(|k| *k != "kind").cloned().collect()
    }

    #[test]
    fn every_field_of_every_node_kind_is_editable() {
        // A field with no spec is invisible in the editor and silently un-editable —
        // the user would have no way to configure it and no indication it exists.
        for descriptor in catalogue() {
            let fields = fields_of(&descriptor.template);
            let mut keys: Vec<String> = params_for(&descriptor.template)
                .into_iter()
                .map(|p| p.key)
                .collect();
            keys.sort();
            let mut expected = fields;
            expected.sort();
            assert_eq!(
                keys, expected,
                "parameter specs for {} are incomplete",
                descriptor.label
            );
        }
    }

    #[test]
    fn no_spec_names_a_field_that_does_not_exist() {
        // Covered by the equality above, but stated separately because the failure mode
        // is different: a stale key means an edit that writes a field nothing reads.
        for descriptor in catalogue() {
            let fields = fields_of(&descriptor.template);
            for spec in params_for(&descriptor.template) {
                assert!(
                    fields.contains(&spec.key),
                    "{} has a spec for {:?}, which it does not serialize",
                    descriptor.label,
                    spec.key
                );
            }
        }
    }

    #[test]
    fn choice_options_are_values_the_document_actually_accepts() {
        // An option whose value does not deserialize would produce a node the backend
        // rejects, discovered only on apply.
        for descriptor in catalogue() {
            for spec in params_for(&descriptor.template) {
                let ParamKind::Choice { options } = &spec.kind else {
                    continue;
                };
                let mut value = serde_json::to_value(&descriptor.template).unwrap();
                for option in options {
                    value[&spec.key] = serde_json::Value::String(option.value.clone());
                    let round: Result<NodeKind, _> = serde_json::from_value(value.clone());
                    assert!(
                        round.is_ok(),
                        "{} rejects {}={:?}",
                        descriptor.label,
                        spec.key,
                        option.value
                    );
                }
            }
        }
    }

    #[test]
    fn descriptors_carry_their_parameters() {
        for descriptor in catalogue() {
            assert!(
                !descriptor.params.is_empty(),
                "{} has no editable parameters",
                descriptor.label
            );
        }
    }
}
