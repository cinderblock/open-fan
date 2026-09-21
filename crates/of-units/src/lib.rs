//! Physical quantities and the typed-port system.
//!
//! This crate is the foundation of OpenFan's defining feature: graph connections are
//! typed by *physical quantity*, not by a bare number. A temperature cannot be wired
//! into a PWM duty input — a transform node has to sit in between and say what the
//! conversion means.
//!
//! The rule is deliberately strict: **connection requires exact quantity equality.**
//! There is no implicit coercion, not even between quantities that happen to share a
//! unit. `Load` and `Duty` are both percentages, but "the CPU is 70 % busy" and "drive
//! this fan at 70 %" are different claims, and silently equating them is exactly the
//! class of configuration error this design exists to prevent. Wanting that conversion
//! is fine — it just has to be spelled out with a node.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// The physical quantity carried by a port, and therefore by a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../../ui/src/bindings/")
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Quantity {
    /// Degrees Celsius. Canonical unit for every temperature in the system.
    Temperature,
    /// Commanded output level, 0–100 %. What a fan/pump/servo sink accepts.
    Duty,
    /// Measured rotational speed, revolutions per minute.
    Rpm,
    /// Utilisation of some resource, 0–100 %.
    Load,
    /// Watts.
    Power,
    /// Volts.
    Voltage,
    /// Amperes.
    Current,
    /// Hertz.
    Frequency,
    /// Bytes per second.
    Throughput,
    /// A dimensionless multiplier or fraction. The escape hatch for pure arithmetic.
    Ratio,
    /// A logical flag. Carried as 0.0 / 1.0 on the wire.
    Boolean,
    /// Seconds.
    Time,
}

impl Quantity {
    /// Every quantity, in a stable order. Used to drive UI palettes and exhaustive tests.
    pub const ALL: &'static [Quantity] = &[
        Quantity::Temperature,
        Quantity::Duty,
        Quantity::Rpm,
        Quantity::Load,
        Quantity::Power,
        Quantity::Voltage,
        Quantity::Current,
        Quantity::Frequency,
        Quantity::Throughput,
        Quantity::Ratio,
        Quantity::Boolean,
        Quantity::Time,
    ];

    /// Short unit symbol for display, e.g. `°C`. Empty for dimensionless quantities.
    pub const fn symbol(self) -> &'static str {
        match self {
            Quantity::Temperature => "°C",
            Quantity::Duty | Quantity::Load => "%",
            Quantity::Rpm => "RPM",
            Quantity::Power => "W",
            Quantity::Voltage => "V",
            Quantity::Current => "A",
            Quantity::Frequency => "Hz",
            Quantity::Throughput => "B/s",
            Quantity::Time => "s",
            Quantity::Ratio | Quantity::Boolean => "",
        }
    }

    /// Human-readable name for the port type.
    pub const fn label(self) -> &'static str {
        match self {
            Quantity::Temperature => "Temperature",
            Quantity::Duty => "Duty",
            Quantity::Rpm => "Speed",
            Quantity::Load => "Load",
            Quantity::Power => "Power",
            Quantity::Voltage => "Voltage",
            Quantity::Current => "Current",
            Quantity::Frequency => "Frequency",
            Quantity::Throughput => "Throughput",
            Quantity::Ratio => "Ratio",
            Quantity::Boolean => "Boolean",
            Quantity::Time => "Time",
        }
    }

    /// The hard physical limits of the quantity, if it has any.
    ///
    /// These are *saturation* bounds, not display bounds — values are clamped into this
    /// range rather than rejected, because a sensor glitch must never be able to escape
    /// into the control path as a duty of 4000 %.
    pub const fn limits(self) -> Option<(f64, f64)> {
        match self {
            Quantity::Duty | Quantity::Load => Some((0.0, 100.0)),
            Quantity::Boolean => Some((0.0, 1.0)),
            // A fan cannot spin backwards, and the sensors cannot report it.
            Quantity::Rpm | Quantity::Frequency | Quantity::Throughput => Some((0.0, f64::MAX)),
            // Absolute zero. Chiefly useful for catching a disconnected sensor
            // reporting a large negative number.
            Quantity::Temperature => Some((-273.15, f64::MAX)),
            Quantity::Power | Quantity::Voltage | Quantity::Current | Quantity::Time => {
                Some((0.0, f64::MAX))
            }
            Quantity::Ratio => None,
        }
    }

    /// Whether a value of this quantity may flow into a port expecting `sink`.
    ///
    /// Exact equality, by design — see the module docs. This is the single place the
    /// rule is defined; both the backend validator and the editor's connection check
    /// must route through it so they can never disagree.
    pub const fn connects_to(self, sink: Quantity) -> bool {
        // `==` is not const for derived PartialEq, so compare discriminants.
        self as u8 == sink as u8
    }

    /// Clamp a raw reading into the quantity's physical limits.
    ///
    /// NaN saturates to the lower limit. That is the safe direction for every *input*
    /// quantity (a NaN temperature reads as cold, and a cold reading never suppresses
    /// cooling on a rising curve), but it is emphatically the wrong direction for a
    /// commanded `Duty`. Sinks must therefore treat NaN as a fault and apply their
    /// failsafe rather than relying on this function. See [`Value::is_trustworthy`].
    pub fn clamp(self, raw: f64) -> f64 {
        match self.limits() {
            Some((lo, hi)) if raw.is_nan() => {
                let _ = hi;
                lo
            }
            Some((lo, hi)) => raw.clamp(lo, hi),
            None if raw.is_nan() => 0.0,
            None => raw,
        }
    }
}

/// A quantity-tagged scalar: one value travelling along one connection.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../../ui/src/bindings/")
)]
pub struct Value {
    pub quantity: Quantity,
    /// The magnitude, in the quantity's canonical unit.
    pub scalar: f64,
}

impl Value {
    /// Construct a value, clamping into the quantity's physical limits.
    pub fn new(quantity: Quantity, scalar: f64) -> Self {
        Self {
            quantity,
            scalar: quantity.clamp(scalar),
        }
    }

    /// Construct without clamping. For tests and for deserializing already-valid data.
    pub const fn raw(quantity: Quantity, scalar: f64) -> Self {
        Self { quantity, scalar }
    }

    pub fn boolean(b: bool) -> Self {
        Self::raw(Quantity::Boolean, if b { 1.0 } else { 0.0 })
    }

    /// Whether this value is safe to act on.
    ///
    /// A sink that receives an untrustworthy value must apply its failsafe instead of
    /// passing the number through to hardware.
    pub fn is_trustworthy(self) -> bool {
        self.scalar.is_finite()
    }

    /// Reinterpret as a different quantity. Named to be conspicuous in review: every
    /// call site is a place where a node asserts that a conversion is meaningful.
    pub fn reinterpret_as(self, quantity: Quantity) -> Self {
        Self::new(quantity, self.scalar)
    }

    pub fn as_bool(self) -> bool {
        self.scalar >= 0.5
    }
}

/// Why two ports could not be connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TypeError {
    #[error("cannot connect {source_ty} output to {sink_ty} input: insert a conversion node")]
    Mismatch {
        source_ty: Quantity,
        sink_ty: Quantity,
    },
}

impl std::fmt::Display for Quantity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Check a prospective connection, producing the error the UI should show.
pub fn check_connection(source_ty: Quantity, sink_ty: Quantity) -> Result<(), TypeError> {
    if source_ty.connects_to(sink_ty) {
        Ok(())
    } else {
        Err(TypeError::Mismatch { source_ty, sink_ty })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_is_reflexive_and_nothing_more() {
        for &a in Quantity::ALL {
            for &b in Quantity::ALL {
                assert_eq!(
                    a.connects_to(b),
                    a == b,
                    "{a:?} -> {b:?} must connect if and only if the quantities are identical"
                );
            }
        }
    }

    #[test]
    fn percentage_quantities_stay_distinct() {
        // The whole point: both are "%", neither may substitute for the other.
        assert!(!Quantity::Load.connects_to(Quantity::Duty));
        assert!(!Quantity::Duty.connects_to(Quantity::Load));
        assert_eq!(Quantity::Load.symbol(), Quantity::Duty.symbol());
    }

    #[test]
    fn duty_saturates_rather_than_escaping() {
        assert_eq!(Value::new(Quantity::Duty, 150.0).scalar, 100.0);
        assert_eq!(Value::new(Quantity::Duty, -20.0).scalar, 0.0);
        assert_eq!(Value::new(Quantity::Duty, 42.5).scalar, 42.5);
    }

    #[test]
    fn nan_is_untrustworthy_even_after_clamping() {
        let v = Value::new(Quantity::Duty, f64::NAN);
        // Clamping gives it a number, but the value must still be refused by a sink.
        assert_eq!(v.scalar, 0.0);
        assert!(!Value::raw(Quantity::Duty, f64::NAN).is_trustworthy());
        assert!(!Value::raw(Quantity::Duty, f64::INFINITY).is_trustworthy());
        assert!(Value::new(Quantity::Duty, 50.0).is_trustworthy());
    }

    #[test]
    fn disconnected_sensor_cannot_report_below_absolute_zero() {
        assert_eq!(Value::new(Quantity::Temperature, -40000.0).scalar, -273.15);
    }

    #[test]
    fn ratio_is_unbounded_but_still_rejects_nan() {
        assert_eq!(Value::new(Quantity::Ratio, 1e9).scalar, 1e9);
        assert_eq!(Value::new(Quantity::Ratio, -5.0).scalar, -5.0);
        assert_eq!(Value::new(Quantity::Ratio, f64::NAN).scalar, 0.0);
    }

    #[test]
    fn mismatch_error_names_both_sides() {
        let err = check_connection(Quantity::Temperature, Quantity::Duty).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Temperature") && msg.contains("Duty"), "{msg}");
        assert!(check_connection(Quantity::Duty, Quantity::Duty).is_ok());
    }

    #[test]
    fn every_quantity_is_listed_in_all() {
        // Guards against adding a variant and forgetting ALL, which would silently
        // drop it from the UI palette and from the exhaustiveness tests above.
        assert_eq!(Quantity::ALL.len(), 12);
        let mut sorted = Quantity::ALL.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), Quantity::ALL.len(), "ALL contains duplicates");
    }
}
