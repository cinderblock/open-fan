//! Hardware abstraction for OpenFan.
//!
//! Everything the engine knows about real hardware goes through these traits. Backends
//! implement them: [`of-hal-mock`] with a simulated thermal plant, `of-hal-pawnio` with
//! Super I/O and EC access on Windows, and later backends for Linux `hwmon`, GPUs, AIO
//! pumps and USB fan hubs.
//!
//! Two rules shape the design:
//!
//! 1. **Reads are batched per tick.** A backend is asked for all of its readings at once
//!    so it can do one bus transaction instead of one per sensor. Chip access is slow
//!    and often serialized behind a global lock; per-sensor polling is how a control loop
//!    ends up missing its deadline.
//! 2. **Acquiring a channel is a borrow, not a takeover.** [`OutputChannel::acquire`]
//!    captures whatever the firmware had configured so [`OutputChannel::release`] can put
//!    it back. Handing control to the BIOS/EC is the safest possible exit, and it is only
//!    possible if we recorded the original state before we touched anything.
//!
//! [`of-hal-mock`]: https://github.com/cinderblock/open-fan

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

/// Stable identifier for a sensor, unique within a backend, e.g. `nct6687/temp/cpu`.
pub type SensorId = String;

/// Stable identifier for a controllable output, e.g. `nct6687/pwm/2`.
pub type ChannelId = String;

/// What a sensor measures, mirroring [`of_units::Quantity`] once wired up in Phase 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SensorKind {
    Temperature,
    Fan,
    Voltage,
    Current,
    Power,
    Load,
}

/// What a backend found on the machine.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorInfo {
    pub id: SensorId,
    /// Name as the hardware reports it, before any user renaming.
    pub label: String,
    pub kind: SensorKind,
}

/// A controllable output and what it can physically do.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelInfo {
    pub id: ChannelId,
    pub label: String,
    /// The tachometer that reads back this channel's speed, when one is wired to it.
    /// Absent for outputs with no feedback, which cannot be stall-detected.
    pub tachometer: Option<SensorId>,
    /// Lowest duty the device will still reliably spin at. Below this a fan may stall
    /// silently, which reads as "quiet" and is in fact "not cooling".
    pub min_reliable_duty: Option<f64>,
}

/// Anything that can be wrong with a hardware backend.
#[derive(Debug, thiserror::Error)]
pub enum HalError {
    #[error("hardware backend unavailable: {0}")]
    Unavailable(String),

    #[error("required driver is not installed: {0}")]
    DriverMissing(String),

    #[error("administrator privileges are required for {0}")]
    PermissionDenied(String),

    #[error("unknown sensor {0}")]
    UnknownSensor(SensorId),

    #[error("unknown channel {0}")]
    UnknownChannel(ChannelId),

    #[error("channel {0} is not currently under our control")]
    NotAcquired(ChannelId),

    #[error("hardware I/O failed: {0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, HalError>;

/// A source of sensor readings.
pub trait SensorSource {
    /// Enumerate what this backend can read. Called on startup and on rescan.
    fn sensors(&self) -> Result<Vec<SensorInfo>>;

    /// Read every sensor for this tick in one batch.
    ///
    /// A sensor that fails to read must be **omitted** from the map rather than given a
    /// placeholder. The graph treats a missing reading as a fault and fails the affected
    /// channels safe; a placeholder would look like a real measurement and would not.
    fn read_all(&mut self) -> Result<BTreeMap<SensorId, f64>>;
}

/// A backend that can drive outputs.
pub trait OutputChannel {
    /// Enumerate controllable outputs.
    fn channels(&self) -> Result<Vec<ChannelInfo>>;

    /// Take control of a channel, recording the firmware's existing configuration so it
    /// can be restored later. Must be idempotent.
    fn acquire(&mut self, channel: &ChannelId) -> Result<()>;

    /// Command a duty in percent. The channel must have been acquired first.
    fn set_duty(&mut self, channel: &ChannelId, percent: f64) -> Result<()>;

    /// Hand the channel back to the firmware, restoring the configuration captured by
    /// [`acquire`](OutputChannel::acquire).
    ///
    /// This runs on the dying-breath path. Implementations must not allocate, block on a
    /// lock the faulting thread might hold, or depend on an async runtime still being
    /// alive — see the safety section of `plans/open-fan.md`.
    fn release(&mut self, channel: &ChannelId) -> Result<()>;

    /// Whether this backend can genuinely restore firmware control on release. When
    /// false, the engine's failsafe for these channels must be a fixed duty instead,
    /// because releasing would leave the chip in manual mode with nobody driving it.
    fn can_restore_firmware_control(&self) -> bool;

    /// Who is driving this channel right now.
    ///
    /// Defaults to [`ChannelControl::Unknown`], which is the honest answer for a backend
    /// that cannot inspect the hardware's control mode. Callers must treat `Unknown` as
    /// "no information", never as "nobody else".
    fn control_of(&self, channel: &ChannelId) -> Result<ChannelControl> {
        let _ = channel;
        Ok(ChannelControl::Unknown)
    }

    /// Hand a channel to the device's own control algorithm.
    ///
    /// **Not the same as [`release`](OutputChannel::release).** Release restores what
    /// *this* backend recorded when it acquired the channel. This imposes firmware
    /// control on a channel we may never have held — the recovery for one some other
    /// application left in manual and abandoned, which is otherwise a channel with
    /// nothing responding to temperature.
    fn hand_back_to_firmware(&mut self, channel: &ChannelId) -> Result<()> {
        Err(HalError::Unavailable(format!(
            "this backend cannot hand {channel} back to firmware control"
        )))
    }
}

/// Who is driving an output channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChannelControl {
    /// The device's own algorithm — a BIOS fan curve or equivalent. Nothing external
    /// needs to drive it, and it responds to temperature on its own.
    Firmware,
    /// We acquired it and are responsible for it.
    Ours,
    /// Under manual control that is not ours. Either another application is driving it,
    /// or one left it this way and **nothing** is. Those look identical in a single
    /// reading and need opposite responses, so they are not distinguished here.
    Foreign,
    /// The backend cannot tell. Not a synonym for "nobody else".
    Unknown,
}

impl ChannelControl {
    /// Whether something is demonstrably responsible for this channel.
    ///
    /// `Foreign` is deliberately *not* safe: it covers the abandoned case, where the duty
    /// is frozen and nothing will react to a rising temperature.
    pub fn is_accounted_for(self) -> bool {
        matches!(self, Self::Firmware | Self::Ours)
    }
}

/// A complete hardware backend.
pub trait Backend: SensorSource + OutputChannel + Send {
    /// Short name for logs and the UI, e.g. `Nuvoton NCT6687D`.
    fn name(&self) -> String;
}

/// Probe for a usable backend. Returns `Ok(None)` when the backend's hardware or driver
/// simply is not present, which is an ordinary outcome rather than an error.
pub trait Discovery {
    fn discover() -> Result<Option<Box<dyn Backend>>>;
}
