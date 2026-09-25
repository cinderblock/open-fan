//! Read-only register dump of a Nuvoton NCT67xx hardware monitor.
//!
//! Captures the chip's banked register space to a file so the decode can be developed and
//! tested offline, against bytes from real hardware, with no chip present. The brief asks
//! for exactly this: register decode belongs in pure functions tested against captured
//! dumps, because hardware-touching code cannot run in CI.
//!
//! Must run elevated.
//!
//! # What this writes
//!
//! The vendor unlock sequence to the index port, the logical-device selector so the
//! monitor's base address can be read, and the monitor's own bank selector — which is how
//! the chip is read at all. **No fan, PWM, temperature or configuration register is
//! written.**
//!
//! ```sh
//! cargo run -p of-hal-pawnio --example nct-dump -- dump.txt
//! ```

use std::fmt::Write as _;

use of_hal_pawnio::lpc::{LpcIo, Slot, Unlock};
use of_hal_pawnio::nct6775::{Nct6775, REG_FAN, REG_PWM, TEMP_INPUTS, decode_pwm, decode_rpm};

/// Banks to capture. Fans and voltages live in bank 4, PWM and temperatures in the low
/// banks, source selection in bank 6.
const BANKS: std::ops::Range<u8> = 0..16;

/// Index that selects the bank for subsequent accesses.
const BANK_SELECT: u8 = 0x4E;

fn main() {
    if let Err(e) = run() {
        eprintln!("dump failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let out_path = std::env::args().nth(1).unwrap_or_else(|| "dump.txt".into());

    let lpc = LpcIo::open(Slot::Primary)?;
    let bus = lpc.lock()?;

    let chip_id = {
        let config = bus.enter_config_mode(Unlock::Nuvoton)?;
        let id = config.chip_id()?.ok_or("no chip answered at slot 0")?;
        // Probing needs configuration mode; reading sensors afterwards does not.
        match Nct6775::probe(config.bus(), id)? {
            Some(chip) => {
                drop(config);
                dump(&bus, chip, &out_path)?;
                return Ok(());
            }
            None => id,
        }
    };

    Err(format!("chip {chip_id:#06X} is not a supported hardware monitor").into())
}

fn dump(
    bus: &of_hal_pawnio::lpc::Bus<'_>,
    chip: Nct6775,
    out_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let model = chip.model();
    let base = chip.base();
    println!("{} (base {base:#06X})", model.name);

    let addr_port = base + 5;
    let data_port = base + 6;

    let mut text = String::new();
    writeln!(text, "# NCT67xx register dump")?;
    writeln!(text, "# chip={}", model.key)?;
    writeln!(text, "# base={base:#06X}")?;
    writeln!(
        text,
        "# Rows are bank:offset, 16 bytes each. 'xx' means the read failed."
    )?;

    let mut captured = 0usize;
    for bank in BANKS {
        bus.pio_outb(addr_port, BANK_SELECT)?;
        bus.pio_outb(data_port, bank)?;

        for row in 0..16u16 {
            write!(text, "{bank:02X}:{:02X} ", row * 16)?;
            for col in 0..16u16 {
                let offset = (row * 16 + col) as u8;
                bus.pio_outb(addr_port, offset)?;
                match bus.pio_inb(data_port) {
                    Ok(value) => {
                        captured += 1;
                        write!(text, "{value:02X} ")?;
                    }
                    Err(_) => text.push_str("xx "),
                }
            }
            text.push('\n');
        }
        text.push('\n');
    }

    std::fs::write(out_path, &text)?;
    println!("wrote {captured} registers to {out_path}\n");

    // Decode through exactly the same functions the fixture tests exercise, so what is
    // printed here and what CI checks cannot drift apart.
    println!("temperatures:");
    for input in &TEMP_INPUTS {
        let value = chip.read_temp(bus, input)?;
        match value {
            Some(c) => println!("  {:<28} {c:>7.1} C", chip.temp_id(input)),
            None => println!("  {:<28} {:>7}", chip.temp_id(input), "absent"),
        }
    }

    println!("\ntachometers:");
    for (index, &register) in REG_FAN.iter().enumerate() {
        match decode_rpm(chip.read_word(bus, register)?) {
            Some(rpm) => println!("  {:<28} {rpm:>7.0} RPM", chip.fan_id(index)),
            None => println!("  {:<28} {:>7}", chip.fan_id(index), "absent"),
        }
    }

    println!("\npwm duty:");
    for (index, &register) in REG_PWM.iter().enumerate() {
        let duty = decode_pwm(chip.read_byte(bus, register)?);
        println!("  {:<28} {duty:>7.1} %", chip.pwm_id(index));
    }

    Ok(())
}
