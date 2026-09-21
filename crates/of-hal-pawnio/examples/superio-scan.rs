//! Read-only Super I/O identification scan.
//!
//! Probes both LPC slots and reports what answers. Must run elevated; PawnIO refuses a
//! handle to an unprivileged process.
//!
//! # What this writes
//!
//! Only the vendor unlock sequence to the chip's *index* port (`0x87 0x87` in, `0xAA`
//! out) — the standard, non-destructive handshake that makes configuration registers
//! visible, and the same thing every monitoring tool does continuously. **No fan, PWM or
//! configuration register is written**, and the unlock is undone on the way out.
//!
//! ```sh
//! cargo run -p of-hal-pawnio --example superio-scan
//! ```

use of_hal_pawnio::lpc::{LpcIo, Slot, Unlock};
use of_hal_pawnio::nct6775;

fn main() {
    if let Err(e) = run() {
        eprintln!("scan failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut lpc = LpcIo::open(Slot::Primary)?;

    for slot in Slot::ALL {
        lpc.select_slot(slot)?;
        println!(
            "slot {slot:?} (ports {:#06X}/{:#06X}):",
            slot.index_port(),
            slot.index_port() + 1
        );

        // One bus lock for the whole slot, released before moving on so other monitoring
        // tools are not kept waiting any longer than the reads take.
        let bus = lpc.lock()?;
        for vendor in [Unlock::Nuvoton, Unlock::Ite] {
            let config = bus.enter_config_mode(vendor)?;
            match config.chip_id()? {
                Some(id) => {
                    let name = nct6775::identify(id).map_or("unrecognised", |m| m.name);
                    println!("  {vendor:?} unlock -> chip id {id:#06X}  {name}");
                }
                None => println!("  {vendor:?} unlock -> nothing (bus floating)"),
            }
        }
        println!();
    }

    Ok(())
}
