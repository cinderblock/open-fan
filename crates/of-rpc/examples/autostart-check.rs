//! Ask the running service what starts with this machine.
//!
//! The service half of the migration survey, exercised without the window: it is the
//! part that needs LocalSystem, so it cannot be checked by running the scan here.
fn main() {
    match of_rpc::request(&of_rpc::Request::AutostartSurvey) {
        Ok(of_rpc::Response::AutostartSurvey { entries, examined }) => {
            println!(
                "service examined {examined} startup entries and found {} rival fan controllers",
                entries.len()
            );
            for entry in entries {
                println!(
                    "  [{}] {} — {}\n      {}\n      {}",
                    entry.id,
                    entry.name,
                    entry.location,
                    entry.command,
                    if entry.reversible {
                        "reversible"
                    } else {
                        "removal would be reported back"
                    }
                );
            }
        }
        Ok(other) => println!("unexpected: {other:?}"),
        Err(e) => println!("failed: {e}"),
    }

    // An id the survey never produced must be refused, not interpreted.
    match of_rpc::request(&of_rpc::Request::DisableAutostart { id: 9999 }) {
        Ok(of_rpc::Response::Error { message }) => println!("\nbogus id refused: {message}"),
        Ok(other) => println!("\nbogus id NOT refused: {other:?}"),
        Err(e) => println!("\ntransport error: {e}"),
    }
}
