//! Take fan control over from another application, without a reboot.
//!
//! This is the command-line rehearsal of the in-app takeover flow. It exists so the
//! sequence can be proven on real hardware before it is wired to a button, and so the
//! claim "a reboot is required" is answered by evidence rather than assumption.
//!
//! # The sequence, and why it is in this order
//!
//! 1. **Look at the chip first.** Which channels are under a firmware algorithm and which
//!    are in manual with nobody of ours driving them. This is the authority; a process
//!    list only suggests who to ask about a foreign channel.
//! 2. **Watch for movement.** A foreign manual channel whose duty *changes* is being
//!    actively driven by something running right now.
//!
//!    The converse does **not** hold, and saying otherwise would be worse than saying
//!    nothing: a controller sitting at a stable temperature writes the *same* value every
//!    tick, which is indistinguishable from writing nothing at all. Observed on the
//!    reference machine, where the competing application was demonstrably driving the CPU
//!    fan and its duty did not budge for six seconds. So movement is evidence and
//!    stillness is merely the absence of it.
//! 3. **Name the other application**, by matching processes against a known list.
//! 4. **Stand it down**, escalating only as far as permitted, and record how gently.
//! 5. **Re-read the chip.** An application that exits cleanly *may* hand its channels
//!    back; one that was terminated certainly did not. Believe the registers, not the
//!    exit code.
//! 6. **Restore firmware control on anything still stranded.** This is the step that
//!    removes the reboot: a firmware mode's configuration survives a trip through manual
//!    mode regardless of who made the trip, so writing the mode back restarts the board's
//!    own curve with the board's own settings.
//! 7. **Verify by watching the duty move on its own.** Writing a mode byte proves a byte
//!    was written; the chip changing the duty afterwards proves it is in charge.
//!
//! Step 6 is deliberately *not* `release`. Release restores what we recorded; this
//! imposes a mode we chose, for a channel another application abandoned and whose
//! original mode nobody recorded.
//!
//! ```sh
//! # Run from an elevated terminal.
//! .\target\debug\examples\takeover.exe            # report only, writes nothing
//! .\target\debug\examples\takeover.exe --stop     # stand down and restore
//! ```

use std::io::Write as _;
use std::time::Duration;

use of_contention::{Politeness, Role, StopOutcome};
use of_hal_pawnio::SuperIoBackend;
use of_hal_pawnio::nct6775::FanMode;

/// How long to watch a channel to decide whether something is actively driving it.
const WATCH: Duration = Duration::from_secs(6);

/// How long to give another application to exit at each politeness rung.
const STOP_TIMEOUT: Duration = Duration::from_secs(12);

fn main() {
    if let Err(e) = run() {
        eprintln!("\nFAILED: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let act = std::env::args().any(|a| a == "--stop");
    // Skips the confirmations, for driving this without a human at the keyboard. The
    // prompts are the safety interlock, so this is opt-in and says so in the output.
    let assume_yes = std::env::args().any(|a| a == "--yes");
    if assume_yes {
        println!("--yes: confirmations will be answered automatically.\n");
    }

    let mut chip = SuperIoBackend::probe_primary()?.ok_or("no supported hardware found")?;
    println!("chip: {}\n", of_hal::Backend::name(&chip));

    // --- 1. the chip's own account of who is in charge -----------------------------
    let before = chip.ownership()?;
    println!("channels:");
    for c in &before {
        println!(
            "  {:<16} {:<14} duty={:>3} ({:>5.1} %)  {}",
            c.id,
            format!("{:?}", c.mode),
            c.duty,
            f64::from(c.duty) / 255.0 * 100.0,
            if c.held_by_us {
                "ours"
            } else if c.firmware_controlled() {
                "firmware"
            } else {
                "FOREIGN (manual, not ours)"
            }
        );
    }

    let stranded: Vec<usize> = before
        .iter()
        .filter(|c| c.foreign_manual())
        .map(|c| c.index)
        .collect();

    if stranded.is_empty() {
        println!("\nNothing is under foreign control. No takeover needed.");
        return Ok(());
    }

    // --- 2. actively driven, or merely abandoned? ----------------------------------
    println!("\nwatching {WATCH:?} to see which of those are actively driven...");
    let moving = watch_for_movement(&chip, &stranded)?;
    for &index in &stranded {
        println!(
            "  {:<16} {}",
            before[index].id,
            if moving.contains(&index) {
                "duty MOVED — something is actively driving it"
            } else {
                "duty steady — inconclusive (a controller at a stable temperature writes \
                 the same value every tick)"
            }
        );
    }

    // --- 3. who to ask ---------------------------------------------------------------
    let running = of_contention::detect()?;
    let controllers: Vec<_> = running
        .iter()
        .filter(|r| r.app.role == Role::Controller)
        .collect();

    println!("\nfan-control software running:");
    if running.is_empty() {
        println!("  (none recognised)");
    }
    for r in &running {
        println!(
            // The process name matters: one application can match several processes, and
            // "Armoury Crate / Armoury Crate" with nothing to tell them apart reads like
            // a bug in us.
            "  {:<22} {:<26} pid {:<7} {:?}",
            r.app.name, r.process_name, r.pid, r.app.role
        );
    }

    // The mode this board's firmware uses, taken from a channel it still owns rather than
    // assumed. Falls back to Smart Fan IV, which is the Nuvoton default on desktop boards.
    let template = before
        .iter()
        .find(|c| c.firmware_controlled())
        .map(|c| c.mode)
        .unwrap_or(FanMode::SmartFanIv);
    println!(
        "\nfirmware mode to restore: {template:?} (observed on a channel the firmware still owns)"
    );

    if !act {
        println!(
            "\nReport only — nothing was written. Re-run with --stop to stand the other \
             application down and restore firmware control."
        );
        return Ok(());
    }

    // --- 4. stand it down ------------------------------------------------------------
    let mut cleanly = true;
    for r in &controllers {
        confirm(
            &format!(
                "Stop {} (pid {})? Its channels will be unmanaged until step 6 restores them.",
                r.app.name, r.pid
            ),
            assume_yes,
        )?;

        let outcome = of_contention::stop(r, Politeness::Quit, STOP_TIMEOUT)?;
        println!("  {} -> {outcome:?}", r.app.name);

        match &outcome {
            StopOutcome::StillRunning { .. } => {
                cleanly = false;
                println!(
                    "  !! it would not exit politely. Not escalating to termination \
                     automatically; do that yourself if you want it gone."
                );
            }
            o if !o.had_chance_to_clean_up() => cleanly = false,
            _ => {}
        }
    }

    // --- 5. did stopping it give the channels back? ----------------------------------
    let after_stop = chip.ownership()?;
    let still_stranded: Vec<usize> = after_stop
        .iter()
        .filter(|c| c.foreign_manual())
        .map(|c| c.index)
        .collect();

    println!("\nafter stopping:");
    for &index in &stranded {
        let c = &after_stop[index];
        println!(
            "  {:<16} {:<14} {}",
            c.id,
            format!("{:?}", c.mode),
            if c.firmware_controlled() {
                "handed back to firmware by the application itself"
            } else {
                "still in manual — nothing is driving it"
            }
        );
    }
    let _ = cleanly;

    if still_stranded.is_empty() {
        println!("\nThe other application restored firmware control on its way out.");
        return Ok(());
    }

    // --- 6. restore it ourselves ------------------------------------------------------
    println!(
        "\n{} channel(s) are stranded in manual. Restoring {template:?} ourselves.",
        still_stranded.len()
    );
    confirm(
        "Hand these channels back to the board firmware?",
        assume_yes,
    )?;

    chip.enable_control();
    for &index in &still_stranded {
        let id = after_stop[index].id.clone();
        chip.restore_firmware_mode(&id, template)?;
        println!("  {id}: mode written");
    }

    // --- 7. prove the firmware actually took them -------------------------------------
    println!("\nwatching {WATCH:?} for the firmware to start moving these duties...");
    let recovered = chip.ownership()?;
    let took_over = watch_for_movement(&chip, &still_stranded)?;

    let mut all_good = true;
    for &index in &still_stranded {
        let c = &recovered[index];
        let moved = took_over.contains(&index);
        println!(
            "  {:<16} {:<14} {}",
            c.id,
            format!("{:?}", c.mode),
            if c.firmware_controlled() && moved {
                "firmware is curving it"
            } else if c.firmware_controlled() {
                "firmware mode set; duty steady (normal at a stable temperature)"
            } else {
                all_good = false;
                "NOT restored"
            }
        );
    }

    println!();
    if all_good {
        println!(
            "Takeover complete with no reboot. Every channel is either ours or back under \
             the board firmware."
        );
        Ok(())
    } else {
        Err("at least one channel could not be handed back to firmware".into())
    }
}

/// Which of these channels have a duty that changes while we watch, writing nothing.
///
/// **One-sided on purpose.** A duty that moves without us writing proves something else
/// is driving it. A duty that holds still proves nothing: a controller whose curve output
/// is not changing writes the same byte every tick and looks exactly like an abandoned
/// channel. The only conclusive test for the still case is to write a different value and
/// see whether it gets stomped, which means writing to a channel we do not own — not
/// something to do silently behind a diagnostic.
fn watch_for_movement(
    chip: &SuperIoBackend,
    indices: &[usize],
) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    let start = chip.ownership()?;
    let deadline = std::time::Instant::now() + WATCH;
    let mut moved = Vec::new();

    while std::time::Instant::now() < deadline && moved.len() < indices.len() {
        std::thread::sleep(Duration::from_millis(400));
        let now = chip.ownership()?;
        for &index in indices {
            if now[index].duty != start[index].duty && !moved.contains(&index) {
                moved.push(index);
            }
        }
    }

    moved.sort_unstable();
    Ok(moved)
}

/// Stop and wait for an explicit yes, unless the caller opted out of the interlock.
fn confirm(question: &str, assume_yes: bool) -> Result<(), Box<dyn std::error::Error>> {
    if assume_yes {
        println!("\n{question} [auto-yes]");
        return Ok(());
    }
    print!("\n{question} [yes/N] ");
    std::io::stdout().flush()?;

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer)? == 0 {
        return Err("stdin reached end of input; run this from an interactive terminal".into());
    }
    if answer.trim().eq_ignore_ascii_case("yes") {
        Ok(())
    } else {
        Err("aborted at confirmation".into())
    }
}
