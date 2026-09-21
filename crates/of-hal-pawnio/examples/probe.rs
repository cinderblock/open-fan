//! Read-only PawnIO bring-up probe.
//!
//! Reports whether `PawnIOLib.dll` can be resolved, what version answers, and whether the
//! hardware modules we need are present. Touches no hardware and writes nothing: it does
//! not open an executor unless a module was actually found, and even then it only loads.
//!
//! ```sh
//! cargo run -p of-hal-pawnio --example probe
//! ```

fn main() {
    println!("is_available(): {}", of_hal_pawnio::is_available());

    match of_hal_pawnio::library_version() {
        Ok((major, minor, patch)) => println!("PawnIOLib version: {major}.{minor}.{patch}"),
        Err(e) => println!("library_version() failed: {e}"),
    }

    println!("\nmodule search path:");
    for dir in of_hal_pawnio::module_search_dirs() {
        let exists = if dir.is_dir() { "exists" } else { "absent" };
        println!("  [{exists}] {}", dir.display());
    }

    // LpcIO is the Super I/O module: the one the NCT6798D on the reference machine needs.
    println!("\nloading LpcIO:");
    match of_hal_pawnio::PawnIo::load_module_by_name("LpcIO") {
        Ok(io) => println!("  ok: {io:?}"),
        Err(e) => println!("  failed: {e}"),
    }
}
