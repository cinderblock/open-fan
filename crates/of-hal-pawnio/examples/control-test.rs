//! Supervised acquire/release test for a single channel.
//!
//! **This is the only thing in the repository that writes a fan register.** It exists so
//! the first writes on a new chip happen in a small, scripted, interruptible program
//! rather than inside the control loop.
//!
//! With no second argument it **writes no duty at all** — it only proves that taking a
//! channel and handing it back works. Pass a duty percentage to also command a speed
//! while the channel is held; it must be *higher* than the current one, because a fan
//! stalls at the bottom of its range and a stalled fan reads as quiet while not cooling.
//!
//! # What "restored" means, and what it does not
//!
//! The obvious check — that the duty after release equals the duty before acquire — is
//! **wrong for a channel under a firmware curve**, and the first run of this example
//! failed on exactly that. Smart Fan IV changes the duty continuously; on this board the
//! chassis header wanders over tens of raw counts on its own. Comparing a value read
//! before a human answered a prompt with one read afterwards measures how long the human
//! took, not whether the restore worked.
//!
//! So three things are checked instead, in order of how much they actually prove:
//!
//! 1. **The mode register byte is restored exactly** — the whole byte, including the
//!    firmware's tolerance nibble, compared against what `acquire` recorded.
//! 2. **The duty register is restored to the byte `acquire` recorded**, sampled
//!    immediately so the firmware has had the least possible chance to move it.
//! 3. **The firmware demonstrably takes the channel back** — after release, the duty is
//!    watched for a few seconds, and a firmware-controlled channel should start moving on
//!    its own again. This is the only one of the three that proves control was *handed
//!    back* rather than merely that some bytes were written.
//!
//! ```sh
//! # Run from an elevated terminal. Default channel is 0.
//! .\target\debug\examples\control-test.exe 0        # acquire/release only
//! .\target\debug\examples\control-test.exe 0 90     # also command 90 %
//! ```

use std::io::Write;
use std::time::{Duration, Instant};

use of_hal::{Backend, OutputChannel, SensorSource};
use of_hal_pawnio::SuperIoBackend;
use of_hal_pawnio::nct6775::{FanMode, decode_pwm, encode_pwm, mode_from_register};

/// How long to watch for the firmware moving the duty after we hand the channel back.
const OBSERVE_FOR: Duration = Duration::from_secs(8);

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

    // Optional second argument: a duty to command while we hold the channel. Absent, the
    // run is acquire/release only and commands nothing.
    let target: Option<f64> = match std::env::args().nth(2) {
        Some(arg) => Some(arg.parse()?),
        None => None,
    };

    // The concrete type, not a Box<dyn Backend>: this needs `enable_control` and the raw
    // register accessors, and one handle is enough.
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

    println!(
        "\nNote: a firmware curve moves the duty on its own, so the value above will have \
         drifted by the time you answer. That is expected and is not a restore failure."
    );

    confirm("Acquire this channel? It will switch to manual at its current duty.")?;

    chip.enable_control();
    chip.acquire(&channel.id)?;

    // What acquire actually recorded. This, not anything read before the prompt, is what
    // release has to reproduce.
    let (recorded_mode, recorded_duty) = chip
        .acquired_registers(index)
        .ok_or("acquire reported success but recorded nothing")?;
    println!(
        "\nrecorded: mode={:#04X} ({:?})  duty={recorded_duty} ({:.1} %)",
        recorded_mode,
        mode_from_register(recorded_mode),
        decode_pwm(recorded_duty)
    );

    // From here on the channel is ours, so every path out must release it. Releasing
    // unconditionally means an error — or a declined confirmation — cannot leave the
    // channel stranded in manual mode with nobody driving it.
    let held = held_steps(&mut chip, index, &channel, recorded_duty, target);
    let released = chip.release(&channel.id);

    held?;
    released?;

    // Sampled immediately: the firmware may start moving the duty again within
    // milliseconds, which is precisely what we want it to do.
    let (mode_after, duty_after) = chip.channel_registers(index)?;
    println!(
        "\nafter:   mode={mode_after:#04X} ({:?})  duty={duty_after} ({:.1} %)",
        mode_from_register(mode_after),
        decode_pwm(duty_after)
    );

    let mode_ok = mode_after == recorded_mode;
    let duty_ok = duty_after == recorded_duty;

    println!(
        "\nmode register restored exactly: {}",
        yes_no(mode_ok, recorded_mode, mode_after)
    );
    println!(
        "duty register restored exactly: {}",
        yes_no(duty_ok, recorded_duty, duty_after)
    );

    if !mode_ok {
        return Err(
            "the mode register was not restored; the channel may not be back under \
                    firmware control"
                .into(),
        );
    }

    let moved = observe_firmware(&chip, index, duty_after)?;

    println!();
    if moved {
        println!("Firmware control confirmed: the chip moved the duty on its own after release.");
    } else if duty_ok {
        println!(
            "Registers restored exactly. The duty did not move while watching, which is \
             normal on a stable temperature — the mode register is the authoritative \
             evidence here."
        );
    } else {
        // Duty differs *and* nothing moved: worth a human look rather than a pass.
        return Err(format!(
            "duty register reads {duty_after} but {recorded_duty} was recorded, and the \
             firmware did not move it while watching. Check the channel in another monitor."
        )
        .into());
    }

    println!("Confirm in another monitor that this header's firmware curve is back.");
    Ok(())
}

/// What happens while we hold the channel.
///
/// Split out so the caller can release unconditionally: any `?` in here returns to a
/// caller that is going to hand the channel back regardless.
fn held_steps(
    chip: &mut SuperIoBackend,
    index: usize,
    channel: &of_hal::ChannelInfo,
    recorded_duty: u8,
    target: Option<f64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mode_held, duty_held) = chip.channel_registers(index)?;
    println!(
        "held:    mode={mode_held:#04X} ({:?})  duty={duty_held} ({:.1} %)",
        mode_from_register(mode_held),
        decode_pwm(duty_held)
    );

    if mode_from_register(mode_held) != FanMode::Manual {
        println!("!! expected Manual after acquire");
    }
    // Against what was recorded, not against a pre-prompt reading: taking the channel
    // must not change the speed it is running at.
    if duty_held != recorded_duty {
        println!("!! duty changed on acquire ({recorded_duty} -> {duty_held}); it should not have");
    }

    if let Some(target) = target {
        drive_duty(chip, index, channel, duty_held, target)?;
    }

    confirm("Release the channel and restore firmware control?")
}

/// Command a duty and check the chip actually took it.
///
/// Only ever upward on a first outing. A fan stalls at the bottom of its range, and a
/// stalled fan reads as "quiet" while being "not cooling" — so finding the floor is a
/// deliberate, separate experiment, not something to stumble into while proving that
/// writes land at all.
fn drive_duty(
    chip: &mut SuperIoBackend,
    index: usize,
    channel: &of_hal::ChannelInfo,
    from: u8,
    target: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    let current = decode_pwm(from);
    if target < current {
        return Err(format!(
            "asked for {target:.1} %, which is below the current {current:.1} %. \
             Lowering a duty risks stalling the fan, and a stalled fan reads as quiet \
             while it is in fact not cooling. Use a higher value; finding the floor is \
             the separate stall-point experiment."
        )
        .into());
    }

    let expected = encode_pwm(target);
    println!(
        "\nThis will command {target:.1} % (raw {expected}) on {}, up from {current:.1} %.",
        channel.id
    );
    confirm("Write this duty?")?;

    chip.set_duty(&channel.id, target)?;

    let (_, after) = chip.channel_registers(index)?;
    println!(
        "commanded: {expected}   read back: {after} ({:.1} %)",
        decode_pwm(after)
    );
    if after != expected {
        return Err(format!(
            "the duty register reads {after}, not the {expected} we wrote — the write \
             did not land where we think it did"
        )
        .into());
    }

    // Holding steady is the point: in manual mode the firmware must not be overriding us.
    // A value that drifts back would mean we are not actually in control.
    println!("holding 3s to confirm the firmware is not overriding us...");
    std::thread::sleep(Duration::from_secs(3));
    let (_, still) = chip.channel_registers(index)?;
    if still == expected {
        println!("  held at {still}. We are driving this channel.");
    } else {
        return Err(format!(
            "duty moved {expected} -> {still} while we were supposed to be in control; \
             something else is writing this channel"
        )
        .into());
    }

    if let Some(tach) = &channel.tachometer {
        let readings = chip.read_all()?;
        match readings.get(tach) {
            Some(rpm) => println!("  {tach}: {rpm:.0} RPM"),
            None => println!("  {tach} did not read"),
        }
    }

    Ok(())
}

/// Watch for the firmware moving the duty, which is the real proof it has the channel.
fn observe_firmware(
    chip: &SuperIoBackend,
    index: usize,
    from: u8,
) -> Result<bool, Box<dyn std::error::Error>> {
    println!("\nwatching {OBSERVE_FOR:?} for the firmware to move the duty...");

    let deadline = Instant::now() + OBSERVE_FOR;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        let (_, duty) = chip.channel_registers(index)?;
        if duty != from {
            println!("  duty moved {from} -> {duty} without us writing anything");
            return Ok(true);
        }
    }

    println!("  duty held steady at {from}");
    Ok(false)
}

fn yes_no(ok: bool, expected: u8, actual: u8) -> String {
    if ok {
        "yes".to_owned()
    } else {
        format!("NO (expected {expected:#04X}, read {actual:#04X})")
    }
}

/// Stop and wait for an explicit yes. Anything else aborts.
///
/// End-of-input is reported as its own failure rather than treated as "no". They are both
/// safe outcomes, but they mean different things: a declined prompt is a decision, whereas
/// EOF means there was no console to answer on and the operator never saw the question.
/// Silently calling that "aborted" sends someone hunting for a mistake they did not make.
fn confirm(question: &str) -> Result<(), Box<dyn std::error::Error>> {
    print!("\n{question} [yes/N] ");
    std::io::stdout().flush()?;

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer)? == 0 {
        return Err(
            "stdin reached end of input: this example needs an interactive \
                    console. Run it from an elevated terminal rather than launching it \
                    detached."
                .into(),
        );
    }

    if answer.trim().eq_ignore_ascii_case("yes") {
        Ok(())
    } else {
        Err("aborted at confirmation; nothing was changed".into())
    }
}
