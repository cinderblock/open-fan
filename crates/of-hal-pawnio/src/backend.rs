//! The real hardware backend: a Super I/O reached through PawnIO.
//!
//! # Control is opt-in
//!
//! Sensor reading is complete and validated against real hardware. Control is implemented
//! but **disabled until [`enable_control`](SuperIoBackend::enable_control) is called**,
//! and the application does not call it. Writing a PWM register should never become
//! possible by accident, and "who turned on fan control" should be greppable.
//!
//! # The order that makes a mistake recoverable
//!
//! [`acquire`](OutputChannel::acquire) reads and records both register bytes — the fan
//! mode (including the firmware's tolerance nibble) and the duty — *before* it writes
//! anything. [`release`](OutputChannel::release) writes those exact bytes back. A
//! round-trip test proves no mode value loses information, including undocumented ones,
//! which are preserved verbatim rather than normalised.
//!
//! Two orderings inside that are load-bearing and easy to get backwards:
//!
//! * `acquire` writes the duty it just read **before** switching the mode to manual,
//!   because the fan otherwise jumps to whatever was last in the manual duty register the
//!   instant the mode changes.
//! * `release` restores the duty **before** the mode, so the firmware algorithm never
//!   runs for an instant against a duty we chose.
//!
//! # Why `can_restore_firmware_control` still answers `false`
//!
//! The mechanism is exact, but the honest question is not "can we write the bytes back",
//! it is "was what we captured actually the firmware's configuration". On a machine where
//! another controller has already put a header into manual mode, it is not — we would
//! capture that tool's duty and call restoring it "handing back to firmware". See the
//! method's own note.

use std::collections::BTreeMap;

use of_hal::{
    Backend, ChannelId, ChannelInfo, Discovery, HalError, OutputChannel, SensorId, SensorInfo,
    SensorKind, SensorSource,
};

use crate::ffi::PawnIoError;
use crate::lpc::{LpcError, LpcIo, Slot, Unlock};
use crate::nct6775::{
    FanMode, Model, Nct6775, REG_FAN, REG_FAN_MODE, REG_PWM_WRITE, TEMP_INPUTS, decode_pwm,
    decode_rpm, decode_temp_byte, decode_temp_word, encode_pwm, mode_from_register,
    mode_into_register,
};

/// Everything `release` needs to undo an `acquire`, captured before anything was written.
///
/// Both fields are whole register bytes, not decoded values, so restoring is a byte-for-
/// byte write-back rather than a re-encoding that could lose a bit we did not understand.
#[derive(Debug, Clone, Copy)]
struct Acquired {
    /// The fan mode register as found, including the firmware's tolerance nibble.
    mode_register: u8,
    /// The duty as found.
    duty: u8,
}

/// A Nuvoton Super I/O hardware monitor, reached over the LPC bus.
pub struct SuperIoBackend {
    lpc: LpcIo,
    chip: Nct6775,
    model: Model,
    /// Fixed-size on purpose: `release` runs on the dying-breath path and must not
    /// allocate. Indexed by channel, so there is no map lookup either.
    acquired: [Option<Acquired>; 7],
    /// Control is opt-in. A backend that can read is useful on its own, and writing a PWM
    /// register is not something that should become possible by accident.
    control_enabled: bool,
}

impl SuperIoBackend {
    /// Probe a slot for a supported hardware monitor.
    ///
    /// `Ok(None)` means nothing supported is there, which is an ordinary outcome.
    fn probe(slot: Slot) -> Result<Option<Self>, LpcError> {
        let lpc = LpcIo::open(slot)?;

        let chip = {
            let bus = lpc.lock()?;
            let config = bus.enter_config_mode(Unlock::Nuvoton)?;
            let Some(chip_id) = config.chip_id()? else {
                return Ok(None);
            };
            match Nct6775::probe(config.bus(), chip_id)? {
                Some(chip) => chip,
                None => return Ok(None),
            }
        };

        let model = chip.model();
        Ok(Some(Self {
            lpc,
            chip,
            model,
            acquired: [None; 7],
            control_enabled: false,
        }))
    }

    /// Probe the primary LPC slot, returning the concrete type.
    ///
    /// [`Discovery::discover`] hands back a `Box<dyn Backend>`, which is what the engine
    /// wants but hides [`enable_control`](Self::enable_control) and
    /// [`channel_state`](Self::channel_state). Bring-up tooling needs both.
    pub fn probe_primary() -> of_hal::Result<Option<Self>> {
        Self::probe(Slot::Primary).map_err(hal_error)
    }

    /// Permit this backend to write PWM registers.
    ///
    /// Off by default, and deliberately an explicit call rather than a constructor
    /// argument or an environment variable: enabling fan control is a decision, and it
    /// should be greppable. The app does not call this yet — during bring-up only the
    /// supervised `control-test` example does.
    pub fn enable_control(&mut self) {
        tracing::warn!(
            chip = self.model.name,
            "PWM control enabled; this backend can now write fan registers"
        );
        self.control_enabled = true;
    }

    pub fn control_enabled(&self) -> bool {
        self.control_enabled
    }

    /// The error every control entry point returns while control is off.
    ///
    /// Spelled out rather than a bare "unsupported" so a caller — or a user reading a
    /// log — understands this is a deliberate state and not a broken driver.
    fn control_not_enabled(&self) -> HalError {
        HalError::Unavailable(format!(
            "{} is enumerated and readable, but PWM control is not enabled on this \
             backend. Control is opt-in during Phase 3 bring-up; see plans/open-fan.md.",
            self.model.name
        ))
    }

    /// Channel index from an id we issued, rejecting anything else.
    fn index_of(&self, channel: &ChannelId) -> of_hal::Result<usize> {
        (0..REG_FAN.len())
            .find(|&index| self.chip.pwm_id(index) == *channel)
            .ok_or_else(|| HalError::UnknownChannel(channel.clone()))
    }

    /// Read the mode and duty a channel is currently configured with.
    pub fn channel_state(&self, index: usize) -> of_hal::Result<(FanMode, f64)> {
        let bus = self.lpc.lock().map_err(hal_error)?;
        let mode_register = self
            .chip
            .read_byte(&bus, REG_FAN_MODE[index])
            .map_err(hal_error)?;
        let duty = self
            .chip
            .read_byte(&bus, REG_PWM_WRITE[index])
            .map_err(hal_error)?;
        Ok((mode_from_register(mode_register), decode_pwm(duty)))
    }
}

/// Map a bus failure onto the HAL's vocabulary, keeping the actionable cases distinct.
fn hal_error(e: LpcError) -> HalError {
    match e {
        LpcError::PawnIo(PawnIoError::AccessDenied) => {
            HalError::PermissionDenied("Super I/O access via PawnIO".into())
        }
        LpcError::PawnIo(e @ (PawnIoError::NotInstalled | PawnIoError::ModuleNotFound { .. })) => {
            HalError::DriverMissing(e.to_string())
        }
        e => HalError::Io(e.to_string()),
    }
}

impl SensorSource for SuperIoBackend {
    fn sensors(&self) -> of_hal::Result<Vec<SensorInfo>> {
        let mut sensors = Vec::with_capacity(TEMP_INPUTS.len() + REG_FAN.len());

        for input in &TEMP_INPUTS {
            sensors.push(SensorInfo {
                id: self.chip.temp_id(input),
                label: input.label.to_owned(),
                kind: SensorKind::Temperature,
            });
        }

        for index in 0..REG_FAN.len() {
            sensors.push(SensorInfo {
                id: self.chip.fan_id(index),
                label: format!("Fan {index}"),
                kind: SensorKind::Fan,
            });
        }

        Ok(sensors)
    }

    /// Read every sensor in a single bus transaction.
    ///
    /// The whole batch happens under one ISA bus lock, so the readings come from the same
    /// instant and cannot be interleaved with another application's register access.
    ///
    /// A sensor that reads implausibly is **omitted**. The engine treats a missing
    /// reading as a fault and fails the affected channels safe; a substituted value would
    /// look like a measurement and would not.
    fn read_all(&mut self) -> of_hal::Result<BTreeMap<SensorId, f64>> {
        // Failure to get the bus fails the whole batch rather than returning a partial
        // map: "no readings" is a fault the engine handles, and a quietly short map would
        // look like several dead sensors instead of one busy bus.
        let bus = self.lpc.lock().map_err(hal_error)?;

        let mut readings = BTreeMap::new();

        for input in &TEMP_INPUTS {
            let value = if input.word_sized {
                self.chip
                    .read_word(&bus, input.register)
                    .map(decode_temp_word)
            } else {
                self.chip
                    .read_byte(&bus, input.register)
                    .map(decode_temp_byte)
            };
            // An I/O failure and an implausible value are the same outcome here: no
            // reading for this sensor this tick.
            if let Ok(Some(celsius)) = value {
                readings.insert(self.chip.temp_id(input), celsius);
            }
        }

        for (index, &register) in REG_FAN.iter().enumerate() {
            if let Ok(Some(rpm)) = self.chip.read_word(&bus, register).map(decode_rpm) {
                readings.insert(self.chip.fan_id(index), rpm);
            }
        }

        Ok(readings)
    }
}

impl OutputChannel for SuperIoBackend {
    /// Enumerate the chip's PWM outputs.
    ///
    /// Every channel the *chip* has is listed, because which of them a *board* actually
    /// brings out to a header cannot be discovered from the chip — an unwired channel
    /// accepts a duty perfectly happily and cools nothing. On the reference machine only
    /// three of the seven are wired. Naming them is a job for configuration, and the
    /// tachometer pairing below is what lets a user tell which is which.
    fn channels(&self) -> of_hal::Result<Vec<ChannelInfo>> {
        Ok((0..REG_FAN.len())
            .map(|index| ChannelInfo {
                id: self.chip.pwm_id(index),
                label: format!("PWM {index}"),
                // Tachometer N reads back PWM channel N on this family.
                tachometer: Some(self.chip.fan_id(index)),
                // Unknown until the stall point has been measured per header, and
                // measuring it means deliberately slowing a real fan. Until then, `None`
                // — a guessed floor is a fan that stalls at what we called safe.
                min_reliable_duty: None,
            })
            .collect())
    }

    /// Take control of a channel, recording what the firmware had first.
    ///
    /// The order is the whole point and is not negotiable: **read and record, then
    /// write.** Both register bytes are captured verbatim before anything changes, so
    /// [`release`](Self::release) is a byte-for-byte restore.
    ///
    /// Idempotent: re-acquiring a channel we already hold keeps the *original* recording
    /// rather than capturing our own manual mode over it. Losing that would mean the
    /// second acquire quietly destroyed the only copy of the firmware's configuration.
    fn acquire(&mut self, channel: &ChannelId) -> of_hal::Result<()> {
        if !self.control_enabled {
            return Err(self.control_not_enabled());
        }

        let index = self.index_of(channel)?;
        if self.acquired[index].is_some() {
            return Ok(());
        }

        let bus = self.lpc.lock().map_err(hal_error)?;

        // Read and record before any write. Everything else depends on this.
        let mode_register = self
            .chip
            .read_byte(&bus, REG_FAN_MODE[index])
            .map_err(hal_error)?;
        let duty = self
            .chip
            .read_byte(&bus, REG_PWM_WRITE[index])
            .map_err(hal_error)?;

        tracing::info!(
            channel = %channel,
            mode = ?mode_from_register(mode_register),
            mode_register = format!("{mode_register:#04X}"),
            duty,
            "acquiring channel; firmware state recorded"
        );

        // Write the duty we just read *before* switching to manual, so the manual duty
        // register already holds the speed the fan is running at. Without this the fan
        // jumps to whatever was last written the instant the mode changes.
        self.chip
            .write_byte(&bus, REG_PWM_WRITE[index], duty)
            .map_err(hal_error)?;

        let manual = mode_into_register(mode_register, FanMode::Manual);
        self.chip
            .write_byte(&bus, REG_FAN_MODE[index], manual)
            .map_err(hal_error)?;

        // Recorded only after the writes succeeded. If we had stored it first and the
        // write failed, `release` would later "restore" a channel it never took.
        self.acquired[index] = Some(Acquired {
            mode_register,
            duty,
        });

        Ok(())
    }

    fn set_duty(&mut self, channel: &ChannelId, percent: f64) -> of_hal::Result<()> {
        if !self.control_enabled {
            return Err(self.control_not_enabled());
        }

        let index = self.index_of(channel)?;
        // Refusing rather than implicitly acquiring: an implicit acquire would capture
        // the *current* configuration as "firmware state" at an arbitrary moment, which
        // is exactly how a restore stops meaning anything.
        if self.acquired[index].is_none() {
            return Err(HalError::NotAcquired(channel.clone()));
        }

        let bus = self.lpc.lock().map_err(hal_error)?;
        self.chip
            .write_byte(&bus, REG_PWM_WRITE[index], encode_pwm(percent))
            .map_err(hal_error)
    }

    /// Hand a channel back exactly as it was found.
    ///
    /// Runs on the dying-breath path, so: no allocation (the record is a fixed-size
    /// array, and the happy path constructs no error), and the bus lock is bounded by
    /// [`crate::lpc::BUS_TIMEOUT`] rather than waited on indefinitely.
    ///
    /// Duty is restored before mode. Restoring the mode first would let the firmware
    /// algorithm run for an instant against a duty we chose.
    fn release(&mut self, channel: &ChannelId) -> of_hal::Result<()> {
        let index = self.index_of(channel)?;
        // Nothing to restore is success, not an error — and constructing an error here
        // would allocate on a path that must not.
        let Some(state) = self.acquired[index] else {
            return Ok(());
        };

        let bus = self.lpc.lock().map_err(hal_error)?;
        self.chip
            .write_byte(&bus, REG_PWM_WRITE[index], state.duty)
            .map_err(hal_error)?;
        self.chip
            .write_byte(&bus, REG_FAN_MODE[index], state.mode_register)
            .map_err(hal_error)?;

        // Cleared last: while any part of the restore can still fail, we must keep the
        // recording, because it is the only copy of the firmware's configuration.
        self.acquired[index] = None;
        Ok(())
    }

    fn can_restore_firmware_control(&self) -> bool {
        // The mechanism exists and is exact — `release` writes back the captured bytes,
        // and a round-trip test proves no mode value loses information.
        //
        // It still answers `false`, because the honest question is not "can we write the
        // bytes back" but "is what we captured actually the firmware's configuration".
        // On a machine where another controller already put a header in manual mode, it
        // is not: we would capture that tool's manual duty and call restoring it "handing
        // back to firmware". Answering `true` requires knowing the channel was
        // firmware-controlled when we took it, which is per-channel state this trait's
        // signature cannot express. Until that is resolved, `false` makes the engine
        // failsafe to a fixed duty, which is never wrong — only sometimes louder than
        // necessary.
        false
    }
}

impl Backend for SuperIoBackend {
    fn name(&self) -> String {
        self.model.name.to_owned()
    }
}

impl Discovery for SuperIoBackend {
    /// Look for a supported Super I/O on this machine.
    ///
    /// `Ok(None)` covers every ordinary "not here": no PawnIO, no module, no chip. An
    /// error is reserved for something the user can act on — chiefly not running
    /// elevated, which PawnIO requires and which would otherwise look identical to
    /// having no hardware at all.
    fn discover() -> of_hal::Result<Option<Box<dyn Backend>>> {
        for slot in Slot::ALL {
            match SuperIoBackend::probe(slot) {
                Ok(Some(backend)) => return Ok(Some(Box::new(backend))),
                Ok(None) => continue,
                Err(LpcError::PawnIo(PawnIoError::AccessDenied)) => {
                    return Err(HalError::PermissionDenied(
                        "PawnIO requires OpenFan to run as administrator".into(),
                    ));
                }
                // No driver or no module: nothing is installed to talk to, which is an
                // ordinary first-run state rather than a failure.
                Err(LpcError::PawnIo(
                    PawnIoError::NotInstalled | PawnIoError::ModuleNotFound { .. },
                )) => return Ok(None),
                Err(e) => {
                    tracing::debug!(slot = ?slot, error = %e, "Super I/O probe failed");
                    continue;
                }
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_driver_is_not_an_error() {
        // Most users' first run, and CI's every run. "No hardware backend" must be an
        // ordinary outcome the app starts up from, not something it fails on.
        match SuperIoBackend::discover() {
            Ok(_) => {}
            Err(HalError::PermissionDenied(_)) => {}
            Err(e) => panic!("discovery should not fail this way: {e}"),
        }
    }

    #[test]
    fn bus_failures_keep_their_actionable_distinctions() {
        // These three lead to three different things a user should do, so collapsing them
        // into a generic I/O error would be a real loss.
        assert!(matches!(
            hal_error(LpcError::PawnIo(PawnIoError::AccessDenied)),
            HalError::PermissionDenied(_)
        ));
        assert!(matches!(
            hal_error(LpcError::PawnIo(PawnIoError::NotInstalled)),
            HalError::DriverMissing(_)
        ));
        assert!(matches!(
            hal_error(LpcError::PawnIo(PawnIoError::ModuleNotFound {
                module: "LpcIO".into(),
                searched: String::new(),
            })),
            HalError::DriverMissing(_)
        ));
    }
}
