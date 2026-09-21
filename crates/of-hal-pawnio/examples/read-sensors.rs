//! Exercise the real backend exactly as the engine does.
//!
//! Goes through [`Discovery`] and the [`Backend`] trait rather than the chip driver, so
//! what this prints is precisely what the control loop would see. Polls a few times so
//! repeated batched reads — the thing that actually happens every tick — get exercised
//! rather than a single lucky one.
//!
//! Must run elevated. Read-only: control is refused by this backend by design, and this
//! example does not even ask.
//!
//! ```sh
//! cargo run -p of-hal-pawnio --example read-sensors
//! ```

use std::time::{Duration, Instant};

use of_hal::Discovery;
use of_hal_pawnio::SuperIoBackend;

fn main() {
    let mut backend = match SuperIoBackend::discover() {
        Ok(Some(backend)) => backend,
        Ok(None) => {
            println!("no supported hardware found (this is an ordinary outcome)");
            return;
        }
        Err(e) => {
            eprintln!("hardware present but unusable: {e}");
            std::process::exit(1);
        }
    };

    println!("backend: {}\n", backend.name());

    let sensors = backend.sensors().expect("enumerate sensors");
    println!("{} sensors:", sensors.len());
    for sensor in &sensors {
        println!("  {:<28} {:<22} {:?}", sensor.id, sensor.label, sensor.kind);
    }

    let channels = backend.channels().expect("enumerate channels");
    println!("\n{} channels:", channels.len());
    for channel in &channels {
        println!(
            "  {:<28} {:<10} tach={:<22} min_duty={:?}",
            channel.id,
            channel.label,
            channel.tachometer.as_deref().unwrap_or("-"),
            channel.min_reliable_duty
        );
    }

    println!(
        "\ncan_restore_firmware_control: {}",
        backend.can_restore_firmware_control()
    );

    // Control must refuse, and must say why. Checking it here means the guarantee is
    // observed on real hardware, not just asserted in a unit test.
    let first = channels.first().expect("at least one channel");
    match backend.acquire(&first.id) {
        Ok(()) => println!("\nWARNING: acquire succeeded, which it must not do yet"),
        Err(e) => println!("\nacquire refused, as intended:\n  {e}"),
    }

    println!("\nbatched reads:");
    for round in 1..=5 {
        let started = Instant::now();
        match backend.read_all() {
            Ok(readings) => {
                let elapsed = started.elapsed();
                print!("  {round}: {:>5.1} ms  ", elapsed.as_secs_f64() * 1000.0);
                let mut parts: Vec<String> = readings
                    .iter()
                    .filter(|(id, _)| {
                        id.contains("temp") || id.contains("fan/1") || id.contains("fan/4")
                    })
                    .map(|(id, v)| format!("{id}={v:.1}"))
                    .collect();
                parts.sort();
                println!("{}", parts.join("  "));
            }
            Err(e) => println!("  {round}: read failed: {e}"),
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
