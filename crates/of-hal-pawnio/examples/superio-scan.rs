//! Read-only Super I/O identification scan.
//!
//! Probes both LPC slots for a Super I/O chip and reports what answers. Must run
//! elevated; PawnIO refuses a handle to an unprivileged process.
//!
//! # What this writes
//!
//! Only the vendor unlock sequence to the chip's *index* port (`0x87 0x87` in, `0xAA`
//! out) — the standard, non-destructive handshake that makes configuration registers
//! visible, and the same thing every monitoring tool does continuously. **It writes no
//! fan, PWM or configuration register**, and the unlock is undone on the way out.
//!
//! ```sh
//! cargo run -p of-hal-pawnio --example superio-scan
//! ```

use std::time::Duration;

use of_hal_pawnio::isa::IsaBusLock;
use of_hal_pawnio::lpc::{LpcIo, Slot, Unlock};

fn main() {
    if let Err(e) = run() {
        eprintln!("scan failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    println!("acquiring the ISA bus mutex...");
    let bus = IsaBusLock::acquire(Duration::from_secs(5))?;
    println!("  held. Other monitoring tools will wait while we read.\n");

    let mut lpc = LpcIo::open(Slot::Primary, bus)?;

    for slot in Slot::ALL {
        lpc.select_slot(slot)?;
        println!(
            "slot {slot:?} (ports {:#06X}/{:#06X}):",
            slot.index_port(),
            slot.index_port() + 1
        );

        for vendor in [Unlock::Nuvoton, Unlock::Ite] {
            let config = lpc.enter_config_mode(vendor)?;
            match config.chip_id()? {
                Some(id) => println!("  {vendor:?} unlock -> chip id {id:#06X}  {}", describe(id)),
                None => println!("  {vendor:?} unlock -> nothing (bus floating)"),
            }
        }
        println!();
    }

    Ok(())
}

/// Name a chip ID where we recognise it.
///
/// Nuvoton encodes the model in the top 12 bits and a variant in the low nibble, so the
/// comparison is deliberately on the masked value. Anything unrecognised is reported as
/// unknown rather than guessed at — a mis-identified chip means a wrong register map,
/// which means confidently wrong temperatures.
fn describe(id: u16) -> &'static str {
    match id & 0xFFF0 {
        0xC330 => "Nuvoton NCT6102D/NCT6106D",
        0xC560 => "Nuvoton NCT6771F/NCT6772F",
        0xC730 => "Nuvoton NCT6776F",
        0xC800 => "Nuvoton NCT6791D",
        0xC910 => "Nuvoton NCT6792D",
        0xD120 => "Nuvoton NCT6793D",
        0xD350 => "Nuvoton NCT6795D",
        0xD420 if id & 0x0008 != 0 => "Nuvoton NCT6798D",
        0xD420 => "Nuvoton NCT6796D",
        0xD450 => "Nuvoton NCT6797D",
        0xD800 => "Nuvoton NCT6799D",
        0x8680 => "ITE IT8686E",
        0x8720 => "ITE IT8728F",
        _ => "unrecognised",
    }
}
