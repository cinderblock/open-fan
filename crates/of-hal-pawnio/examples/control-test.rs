//! Supervised acquire/release test for a single channel.
//!
//! **This is the only thing in the repository that writes a fan register.** It exists so
//! the first writes on a new chip happen in a small, scripted, interruptible program
//! rather than inside the control loop.
//!
//! What it does, stopping for confirmation between each step:
//!
//! 1. Show the channel's current mode and duty, and refuse to continue unless the channel
//!    is under **firmware** control — otherwise what we would "restore" is some other
//!    application's manual mode, not the firmware's.
//! 2. `acquire` — record both register bytes, then switch to manual at the duty the fan is
//!    already running at. **No speed change should occur.**
//! 3. `release` — write the recorded bytes back.
//! 4. Verify the mode and duty match what was there at the start.
//!
//! It does **not** write a duty. Proving restore comes first; changing a speed is a
//! separate, later step.
//!
//! ```sh
//! # Run elevated. Default channel is 0.
//! cargo run -p of-hal-pawnio --example control-test -- 0
//! ```

use std::io::Write;

use of_hal::{Backend, OutputChannel, SensorSource};
use of_hal_pawnio::SuperIoBackend;
use of_hal_pawnio::nct6775::FanMode;

fn main() {
    if let Err(e) = run() {
        eprintln!("\nFAILED: {e}");
        eprintln!("If a channel was left acquired, re-run this example to restore it.");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let index: usize = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0".into())
        .parse()?;

    // The concrete type, not a Box<dyn Backend>: this needs `enable_control` and
    // `channel_state`, and one handle is enough.
    let mut chip = SuperIoBackend::probe_primary()?.ok_or("no supported hardware found")?;

    let channels = chip.channels()?;
    let channel = channels
        .get(index)
        .ok_or_else(|| format!("channel {index} does not exist"))?
        .clone();

    println!("chip:    {}", chip.name());
    println!("channel: {} ({})", channel.id, channel.label);
    println!(
        "tach:    {}",
        channel.tachometer.as_deref().unwrap_or("none")
    );

    let (mode_before, duty_before) = chip.channel_state(index)?;
    println!("\nbefore:  mode={mode_before:?}  duty={duty_before:.1} %");

    if !mode_before.is_firmware_controlled() {
        return Err(format!(
            "channel {index} is already in {mode_before:?}, not under firmware control.\n\
             Something else put it there, so what this test would capture and \"restore\" \
             is that application's manual mode — which proves nothing about handing \
             control back to firmware.\n\
             Pick a channel still showing a firmware mode, or stop the other controller \
             and reboot so the BIOS curve is what is actually loaded."
        )
        .into());
    }

    if let Some(tach) = &channel.tachometer {
        let readings = chip.read_all()?;
        match readings.get(tach) {
            Some(rpm) if *rpm > 0.0 => {
                println!("\n!! {tach} reads {rpm:.0} RPM — something is spinning on this header.")
            }
            Some(_) => println!("\n{tach} reads 0 RPM — nothing appears to be connected."),
            None => println!("\n{tach} did not read."),
        }
    }

    confirm("Acquire this channel? It will switch to manual at its current duty.")?;

    chip.enable_control();
    chip.acquire(&channel.id)?;

    let (mode_held, duty_held) = chip.channel_state(index)?;
    println!("held:    mode={mode_held:?}  duty={duty_held:.1} %");

    if mode_held != FanMode::Manual {
        println!("!! expected Manual after acquire; releasing immediately");
    }
    if (duty_held - duty_before).abs() > 0.5 {
        println!(
            "!! duty moved {duty_before:.1} -> {duty_held:.1} % on acquire; it should not have"
        );
    }

    confirm("Release the channel and restore firmware control?")?;

    chip.release(&channel.id)?;

    let (mode_after, duty_after) = chip.channel_state(index)?;
    println!("after:   mode={mode_after:?}  duty={duty_after:.1} %");

    let mode_ok = mode_after == mode_before;
    let duty_ok = (duty_after - duty_before).abs() <= 0.5;

    println!();
    println!("mode restored: {}", if mode_ok { "yes" } else { "NO" });
    println!("duty restored: {}", if duty_ok { "yes" } else { "NO" });

    if mode_ok && duty_ok {
        println!("\nRestore verified. Confirm in another monitor that the firmware curve is back.");
        Ok(())
    } else {
        Err(format!(
            "restore did not reproduce the original state \
             (was {mode_before:?}/{duty_before:.1} %, now {mode_after:?}/{duty_after:.1} %)"
        )
        .into())
    }
}

/// Stop and wait for an explicit yes. Anything else aborts.
fn confirm(question: &str) -> Result<(), Box<dyn std::error::Error>> {
    print!("\n{question} [yes/N] ");
    std::io::stdout().flush()?;

    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;

    if answer.trim().eq_ignore_ascii_case("yes") {
        Ok(())
    } else {
        Err("aborted at confirmation; nothing was changed".into())
    }
}
