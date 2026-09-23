//! Importers for other tools' configuration formats.
//!
//! Interoperability only. These parse documented-by-observation on-disk formats, which
//! is the one place the clean-room rules in `plans/open-fan.md` permit a competing
//! product to be referenced. Nothing in this module may derive from decompiled code.
//!
//! # What an importer is allowed to do
//!
//! Produce a [`Profile`] for a person to look at. That is all.
//!
//! An import is **never applied to hardware by the act of importing**. It lands in the
//! editor, the notes say what came across and what did not, and the user decides. This
//! is not politeness: a fan curve translated between two tools that disagree about what
//! a field means is exactly the kind of thing that should be read before it drives a
//! pump.
//!
//! # Honesty about fidelity
//!
//! Two tools do not share a model, so some things cross exactly, some approximately, and
//! some not at all. Every one of those outcomes produces a [`Note`]. An importer that
//! silently dropped a control would be worse than one that refused outright, because the
//! user would believe their configuration had come across intact.
//!
//! The rule that matters most: **a value that cannot be interpreted is never guessed at**
//! and never clamped into range. See [`fancontrol`] for the case that motivated it.

pub mod fancontrol;

use crate::Profile;

/// How faithfully one piece of a foreign configuration crossed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Fidelity {
    /// Came across with the same meaning it had.
    Exact,
    /// Came across as the nearest thing this model can express.
    Approximated,
    /// Came across, but a person has to confirm it is right.
    NeedsAttention,
    /// Did not come across. The detail says why.
    Skipped,
}

/// One thing an importer did, or declined to do.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub fidelity: Fidelity,
    /// What this is about, in the source tool's own words — a control or curve name.
    pub subject: String,
    /// What happened, phrased for the person who wrote the original configuration.
    pub detail: String,
}

impl Note {
    fn new(fidelity: Fidelity, subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            fidelity,
            subject: subject.into(),
            detail: detail.into(),
        }
    }
}

/// A measured point on a fan's duty-to-speed relationship.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationPoint {
    pub duty_percent: f64,
    pub rpm: f64,
}

/// What another tool measured about a fan.
///
/// The most valuable thing in a foreign configuration, and the part with no equivalent
/// in ours: somebody already ran this fan down to a stop and wrote down where it
/// happened. That is a measurement we would otherwise have to repeat by driving a fan
/// slowly towards stall on a machine that is trying to stay cool.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    /// Our channel id, already translated.
    pub channel: String,
    /// What the other tool called it.
    pub label: String,
    /// Measured duty/speed pairs, ascending by duty.
    pub points: Vec<CalibrationPoint>,
    /// Duty the fan needed in order to start turning, if recorded.
    pub start_percent: Option<f64>,
    /// Duty below which the fan stopped, if recorded.
    pub stop_percent: Option<f64>,
    /// Floor the other tool refused to command below.
    pub minimum_percent: Option<f64>,
}

impl Calibration {
    /// The lowest duty at which this fan was observed still turning.
    ///
    /// Derived from the measurements rather than from the other tool's settings, because
    /// a setting records an intention and a measurement records the fan. Returns `None`
    /// when nothing was measured turning — including the case where every recorded point
    /// reads zero, which means the table says nothing about where this fan stalls.
    pub fn lowest_turning_duty(&self) -> Option<f64> {
        self.points
            .iter()
            .filter(|p| p.rpm > 0.0)
            .map(|p| p.duty_percent)
            .fold(None, |acc: Option<f64>, d| {
                Some(acc.map_or(d, |a| a.min(d)))
            })
    }

    /// Whether this table actually contains a stall — a point that reads zero below a
    /// point that turns.
    ///
    /// A table with no zero reading has not found the bottom, so it must not be treated
    /// as proof that low duties are safe.
    pub fn found_the_stall(&self) -> bool {
        match self.lowest_turning_duty() {
            Some(lowest) => self
                .points
                .iter()
                .any(|p| p.rpm == 0.0 && p.duty_percent < lowest),
            None => false,
        }
    }

    /// The lowest duty measured to reach `rpm`, if these measurements reach it at all.
    ///
    /// This is what turns a fixed speed we cannot command into one we can: we drive duty,
    /// the other tool drove RPM, and the bridge between them is the table it left behind
    /// — measured on *this* fan, on *this* machine.
    ///
    /// # What it refuses
    ///
    /// * **Anything outside the measured range.** Extrapolating past the last measurement
    ///   is guessing, and guessing below the slowest measured speed guesses in the
    ///   direction of a stalled fan.
    /// * **A flat stretch.** Where two measurements barely differ in speed, duty says
    ///   almost nothing about RPM, and interpolating across it would invent precision the
    ///   measurements do not have. The pump on the reference machine has exactly such a
    ///   dead zone — 970 rpm at 1 % and 979 rpm at 10 %.
    ///
    /// The *lowest* qualifying duty wins, because a table need not be monotonic and the
    /// quietest way to reach a speed is the right bias for a fan controller.
    pub fn duty_reaching(&self, rpm: f64) -> Option<f64> {
        /// Minimum rise across a segment for interpolation within it to mean anything.
        const MEANINGFUL_RISE_RPM: f64 = 50.0;

        if !rpm.is_finite() || rpm <= 0.0 {
            return None;
        }

        let mut best: Option<f64> = None;
        for pair in self.points.windows(2) {
            let (low, high) = (pair[0], pair[1]);
            if high.rpm - low.rpm < MEANINGFUL_RISE_RPM {
                continue;
            }
            if !(low.rpm..=high.rpm).contains(&rpm) {
                continue;
            }

            let fraction = (rpm - low.rpm) / (high.rpm - low.rpm);
            let duty = low.duty_percent + fraction * (high.duty_percent - low.duty_percent);
            if (0.0..=100.0).contains(&duty) {
                best = Some(best.map_or(duty, |b: f64| b.min(duty)));
            }
        }
        best
    }
}

/// The result of reading somebody else's configuration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    /// A profile to review. **Not applied by importing.**
    pub profile: Profile,
    /// What came across, what did not, and what needs looking at.
    pub notes: Vec<Note>,
    /// Fan measurements found alongside the configuration.
    pub calibration: Vec<Calibration>,
}

impl Imported {
    /// Notes a person must act on before this profile should be trusted.
    pub fn needs_attention(&self) -> impl Iterator<Item = &Note> {
        self.notes
            .iter()
            .filter(|n| n.fidelity == Fidelity::NeedsAttention)
    }

    /// Whether anything at all was translated.
    ///
    /// An import that produced no outputs is a failure worth reporting as one, however
    /// many notes explain it.
    pub fn is_empty(&self) -> bool {
        self.profile.graph.nodes.is_empty()
    }
}
