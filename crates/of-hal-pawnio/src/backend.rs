//! The real hardware backend: a Super I/O reached through PawnIO.
//!
//! # Read-only, deliberately
//!
//! Sensor reading is complete and validated against real hardware. **Control is not
//! enabled yet**: [`acquire`](OutputChannel::acquire) and
//! [`set_duty`](OutputChannel::set_duty) refuse, with an explanation.
//!
//! That is not an oversight, it is the required order. A PWM register must not be written
//! until its existing value has been read, recorded, and proven restorable — and proving
//! restoration means watching a real fan with someone present to hear it and cut power.
//! Until that has happened on this chip, the honest thing for this backend to do is
//! report what it can read and refuse to drive anything.
//!
//! [`can_restore_firmware_control`](OutputChannel::can_restore_firmware_control)
//! accordingly answers `false`, which the engine already handles by failsafing to a fixed
//! duty rather than releasing a channel into an unmanaged manual mode.

use std::collections::BTreeMap;

use of_hal::{
    Backend, ChannelId, ChannelInfo, Discovery, HalError, OutputChannel, SensorId, SensorInfo,
    SensorKind, SensorSource,
};

use crate::ffi::PawnIoError;
use crate::lpc::{LpcError, LpcIo, Slot, Unlock};
use crate::nct6775::{
    Model, Nct6775, REG_FAN, TEMP_INPUTS, decode_rpm, decode_temp_byte, decode_temp_word,
};

/// A Nuvoton Super I/O hardware monitor, reached over the LPC bus.
pub struct SuperIoBackend {
    lpc: LpcIo,
    chip: Nct6775,
    model: Model,
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
        Ok(Some(Self { lpc, chip, model }))
    }

    /// The error every control entry point returns until Step 3 is done.
    ///
    /// Spelled out rather than a bare "unsupported" so that a caller — or a user reading
    /// a log — understands this is a deliberate stage of bring-up and not a broken
    /// driver.
    fn control_not_enabled(&self) -> HalError {
        HalError::Unavailable(format!(
            "{} is enumerated and readable, but PWM control is not enabled yet. Writing a \
             duty requires the channel's firmware configuration to be captured and proven \
             restorable first; see Phase 3 in plans/open-fan.md.",
            self.model.name
        ))
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

    fn acquire(&mut self, _channel: &ChannelId) -> of_hal::Result<()> {
        Err(self.control_not_enabled())
    }

    fn set_duty(&mut self, channel: &ChannelId, _percent: f64) -> of_hal::Result<()> {
        // Unreachable while `acquire` refuses, but a backend that silently accepted a
        // duty it did not apply would be far worse than one that says no.
        let _ = channel;
        Err(self.control_not_enabled())
    }

    fn release(&mut self, _channel: &ChannelId) -> of_hal::Result<()> {
        // A no-op, and correct: nothing can have been acquired, so there is nothing to
        // restore. Allocates nothing and takes no lock, as the dying-breath path requires.
        Ok(())
    }

    fn can_restore_firmware_control(&self) -> bool {
        // Honest `false`. Restoring firmware control has not been demonstrated on this
        // chip, and on the reference machine another application currently owns the
        // headers — so what we would capture is *its* manual mode, not the firmware's.
        // The engine responds by failsafing to a fixed duty, which is the right answer.
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
