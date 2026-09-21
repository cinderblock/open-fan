//! Nuvoton NCT67xx hardware monitors.
//!
//! Split deliberately into two halves:
//!
//! * **Pure decode** — register addresses and the functions turning raw bytes into
//!   physical values. No I/O, so it is tested in CI against a register dump captured from
//!   real hardware (`tests/fixtures/`). This is where decoding mistakes are caught.
//! * **Access** — [`Nct6775`], which performs the banked port I/O. Kept as thin as
//!   possible, because nothing in it can be tested without the chip.
//!
//! # Register access
//!
//! The hardware monitor is a logical device behind the Super I/O's configuration window.
//! Its base address, read once at discovery, exposes an index port at `base + 5` and a
//! data port at `base + 6`. Registers are 16-bit: the high byte selects a bank (written
//! through the bank-select register `0x4E`), the low byte is the index.
//!
//! # Provenance
//!
//! Register addresses are facts about the hardware, cross-checked against the Linux
//! `nct6775` hwmon driver and then **verified by experiment on a real NCT6798D** — see
//! the fixture tests. Where the two disagreed, the hardware won: the seventh tachometer
//! is at `0x4CE`, not the `0x4CC` a naive stride predicts, and `0x4CC` returns garbage.

use crate::lpc::{BASE_ADDRESS_REGISTER, Bus, LpcError};

/// Logical device number of the hardware monitor on Nuvoton Super I/O chips.
pub const LD_HARDWARE_MONITOR: u8 = 0x0B;

/// Offsets from the monitor's base address to its index and data ports.
const ADDR_REG_OFFSET: u16 = 5;
const DATA_REG_OFFSET: u16 = 6;

/// Index that selects which bank subsequent accesses address.
const BANK_SELECT: u8 = 0x4E;

/// Fan tachometers. Word-sized, big-endian, already in RPM — no divisor on this family.
///
/// Note the gap: the seventh is `0x4CE`, **not** `0x4CC`. `0x4CC` is not a tachometer and
/// reading it as one produces a confident, nonsensical RPM.
pub const REG_FAN: [u16; 7] = [0x4C0, 0x4C2, 0x4C4, 0x4C6, 0x4C8, 0x4CA, 0x4CE];

/// PWM duty, one byte each, 0–255.
pub const REG_PWM: [u16; 7] = [0x001, 0x003, 0x011, 0x013, 0x015, 0x017, 0x019];

/// A temperature input and how to read it.
pub struct TempInput {
    /// Stable id fragment. Never an index — a profile references this.
    pub key: &'static str,
    pub label: &'static str,
    pub register: u16,
    /// Word-sized registers carry a fraction in the low byte; byte-sized ones do not.
    pub word_sized: bool,
}

/// Temperature inputs exposed to the engine.
///
/// Only the chip's **fixed-function** thermistor inputs are here. The NCT67xx also has
/// six "monitored source" slots whose meaning is selected by configuration registers, and
/// those are deliberately omitted: their id would be stable while their *meaning* was
/// not, so a BIOS update could silently re-point a user's fan curve at a different
/// sensor. Exposing them needs the source-select registers decoded first. See the plan.
pub const TEMP_INPUTS: [TempInput; 2] = [
    TempInput {
        key: "systin",
        label: "System (SYSTIN)",
        register: 0x027,
        word_sized: false,
    },
    TempInput {
        key: "cputin",
        label: "CPU socket (CPUTIN)",
        register: 0x150,
        word_sized: true,
    },
];

/// A recognised chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// Lowercase, used to build sensor ids. Stable across releases.
    pub key: &'static str,
    pub name: &'static str,
}

/// Identify a chip from its ID register.
///
/// Nuvoton puts the model in the upper bits and a variant in the low nibble. Returning
/// `None` for anything unrecognised is the safe answer: an unknown chip with a guessed
/// register map produces plausible, wrong readings.
pub fn identify(chip_id: u16) -> Option<Model> {
    let model = match chip_id & 0xFFF0 {
        0xC800 => Model {
            key: "nct6791d",
            name: "Nuvoton NCT6791D",
        },
        0xC910 => Model {
            key: "nct6792d",
            name: "Nuvoton NCT6792D",
        },
        0xD120 => Model {
            key: "nct6793d",
            name: "Nuvoton NCT6793D",
        },
        0xD350 => Model {
            key: "nct6795d",
            name: "Nuvoton NCT6795D",
        },
        // The low nibble separates two different parts sharing a prefix.
        0xD420 if chip_id & 0x0008 != 0 => Model {
            key: "nct6798d",
            name: "Nuvoton NCT6798D",
        },
        0xD420 => Model {
            key: "nct6796d",
            name: "Nuvoton NCT6796D",
        },
        0xD450 => Model {
            key: "nct6797d",
            name: "Nuvoton NCT6797D",
        },
        0xD800 => Model {
            key: "nct6799d",
            name: "Nuvoton NCT6799D",
        },
        _ => return None,
    };
    Some(model)
}

/// Highest RPM we will believe from a tachometer.
///
/// Well above any real PC fan or pump, and well below the values an unconnected or
/// non-tachometer register produces. A reading past this is reported as *absent* rather
/// than clamped: the engine must see a fault, not a number we made up.
const MAX_PLAUSIBLE_RPM: u16 = 30_000;

/// Temperature window we will believe, in °C. Outside it the input is unconnected or the
/// register is not a temperature.
const MIN_PLAUSIBLE_TEMP: f64 = -40.0;
const MAX_PLAUSIBLE_TEMP: f64 = 150.0;

/// Decode a tachometer.
///
/// `0` is a real, meaningful reading — an empty header or a stopped fan — and is passed
/// through, not filtered. Only physically impossible values become `None`.
pub fn decode_rpm(raw: u16) -> Option<f64> {
    (raw <= MAX_PLAUSIBLE_RPM).then(|| f64::from(raw))
}

/// Decode a PWM duty register into percent.
pub fn decode_pwm(raw: u8) -> f64 {
    f64::from(raw) / 255.0 * 100.0
}

/// Decode a word-sized temperature: high byte whole °C, low byte a 1/256 fraction.
///
/// Signed, so sub-zero readings survive rather than wrapping to something plausible.
pub fn decode_temp_word(raw: u16) -> Option<f64> {
    plausible_temp(f64::from(raw as i16) / 256.0)
}

/// Decode a byte-sized temperature: whole °C, signed.
pub fn decode_temp_byte(raw: u8) -> Option<f64> {
    plausible_temp(f64::from(raw as i8))
}

fn plausible_temp(celsius: f64) -> Option<f64> {
    (MIN_PLAUSIBLE_TEMP..=MAX_PLAUSIBLE_TEMP)
        .contains(&celsius)
        .then_some(celsius)
}

/// A hardware monitor we can read.
#[derive(Debug, Clone, Copy)]
pub struct Nct6775 {
    model: Model,
    base: u16,
}

impl Nct6775 {
    /// Find the monitor behind an already-unlocked Super I/O.
    ///
    /// Takes a [`Bus`] because it must be called with the chip in configuration mode and
    /// the ISA bus held.
    pub fn probe(bus: &Bus<'_>, chip_id: u16) -> Result<Option<Self>, LpcError> {
        let Some(model) = identify(chip_id) else {
            return Ok(None);
        };

        bus.select_logical_device(LD_HARDWARE_MONITOR)?;
        // The low three bits are not part of the address.
        let base = bus.superio_inw(BASE_ADDRESS_REGISTER)? & !7;
        if base == 0 {
            // The monitor exists but is not mapped — disabled in firmware. Not an error.
            return Ok(None);
        }

        // Authorise the module to touch this port range; without it every access below is
        // (correctly) refused.
        bus.find_bars()?;

        Ok(Some(Self { model, base }))
    }

    pub fn model(&self) -> Model {
        self.model
    }

    pub fn base(&self) -> u16 {
        self.base
    }

    fn addr_port(&self) -> u16 {
        self.base + ADDR_REG_OFFSET
    }

    fn data_port(&self) -> u16 {
        self.base + DATA_REG_OFFSET
    }

    /// Read one 8-bit register.
    pub fn read_byte(&self, bus: &Bus<'_>, register: u16) -> Result<u8, LpcError> {
        self.set_bank(bus, (register >> 8) as u8)?;
        bus.pio_outb(self.addr_port(), register as u8)?;
        Ok(bus.pio_inb(self.data_port())?)
    }

    /// Read one 16-bit register as a big-endian pair.
    pub fn read_word(&self, bus: &Bus<'_>, register: u16) -> Result<u16, LpcError> {
        self.set_bank(bus, (register >> 8) as u8)?;
        bus.pio_outb(self.addr_port(), register as u8)?;
        let high = bus.pio_inb(self.data_port())?;
        bus.pio_outb(self.addr_port(), (register as u8).wrapping_add(1))?;
        let low = bus.pio_inb(self.data_port())?;
        Ok((u16::from(high) << 8) | u16::from(low))
    }

    fn set_bank(&self, bus: &Bus<'_>, bank: u8) -> Result<(), LpcError> {
        bus.pio_outb(self.addr_port(), BANK_SELECT)?;
        bus.pio_outb(self.data_port(), bank)?;
        Ok(())
    }

    /// Sensor id for a tachometer, e.g. `nct6798d/fan/1`.
    pub fn fan_id(&self, index: usize) -> String {
        format!("{}/fan/{index}", self.model.key)
    }

    /// Sensor id for a PWM duty readback, e.g. `nct6798d/pwm/4`.
    pub fn pwm_id(&self, index: usize) -> String {
        format!("{}/pwm/{index}", self.model.key)
    }

    /// Sensor id for a temperature, e.g. `nct6798d/temp/cputin`.
    ///
    /// Keyed by the input's fixed function, never by position, so the id keeps meaning
    /// the same thing across reboots and re-enumeration.
    pub fn temp_id(&self, input: &TempInput) -> String {
        format!("{}/temp/{}", self.model.key, input.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_machines_chip_is_identified() {
        // 0xD42B, read from the NCT6798D on the reference machine.
        assert_eq!(identify(0xD42B).unwrap().key, "nct6798d");
    }

    #[test]
    fn the_low_nibble_separates_parts_sharing_a_prefix() {
        // 0xD42x is two different chips with two different register maps. Matching on the
        // masked value alone would mis-identify one as the other.
        assert_eq!(identify(0xD423).unwrap().key, "nct6796d");
        assert_eq!(identify(0xD428).unwrap().key, "nct6798d");
    }

    #[test]
    fn an_unknown_chip_is_not_guessed_at() {
        // Better to support nothing than to apply a guessed register map and report
        // confident nonsense.
        assert_eq!(identify(0x0000), None);
        assert_eq!(identify(0xFFFF), None);
        assert_eq!(identify(0x1234), None);
    }

    #[test]
    fn a_stopped_fan_reads_as_zero_rather_than_missing() {
        // An empty header and a stalled fan both genuinely read 0 RPM. Dropping that
        // would hide a stall, which is the one failure the tachometer exists to reveal.
        assert_eq!(decode_rpm(0), Some(0.0));
    }

    #[test]
    fn an_impossible_rpm_is_omitted_not_clamped() {
        // 0xFF1F is what register 0x4CC returns on the reference machine. Clamping it to
        // a maximum would look like a healthy fan at full tilt.
        assert_eq!(decode_rpm(0xFF1F), None);
        assert_eq!(decode_rpm(1477), Some(1477.0));
    }

    #[test]
    fn pwm_spans_the_full_range() {
        assert_eq!(decode_pwm(0), 0.0);
        assert_eq!(decode_pwm(255), 100.0);
        // The CPU fan's duty at capture time, which the other tool displayed as 45.8 %.
        assert!((decode_pwm(116) - 45.5).abs() < 0.1, "{}", decode_pwm(116));
    }

    #[test]
    fn word_temperatures_carry_a_half_degree() {
        assert_eq!(decode_temp_word(0x2B00), Some(43.0));
        assert_eq!(decode_temp_word(0x2E80), Some(46.5));
    }

    #[test]
    fn sub_zero_temperatures_do_not_wrap_into_plausibility() {
        // Read unsigned, 0xFF00 would be 255 °C; read signed it is -1 °C. Getting this
        // wrong turns a cold sensor into a fake emergency.
        assert_eq!(decode_temp_word(0xFF00), Some(-1.0));
        assert_eq!(decode_temp_byte(0xFF), Some(-1.0));
    }

    #[test]
    fn disconnected_temperature_inputs_are_omitted() {
        // Far outside anything a PC produces: the input is floating, not freezing.
        assert_eq!(decode_temp_word(0x8000), None);
        assert_eq!(decode_temp_byte(0x80), None);
    }

    #[test]
    fn the_seventh_tachometer_is_not_where_a_stride_would_put_it() {
        // Proven on hardware: 0x4CC returns 0xFF1F, 0x4CE is the real register. Encoding
        // this as a stride would produce a seventh "fan" reading pure garbage.
        assert_eq!(REG_FAN[6], 0x4CE);
        assert!(!REG_FAN.contains(&0x4CC));
    }

    #[test]
    fn temperature_ids_are_keyed_by_function_not_position() {
        // A profile stores these. An index would move if enumeration ever changed,
        // silently re-pointing a user's fan curve at a different sensor.
        for input in &TEMP_INPUTS {
            assert!(
                input.key.chars().all(|c| c.is_ascii_lowercase()),
                "{} is not a stable key",
                input.key
            );
        }
    }
}
