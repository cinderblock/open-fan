//! The actual sensor and channel ids this machine reports.
//!
//! The importer translates `/lpc/<chip>/control/N` to `<chip>/pwm/N` and checks the
//! result against the inventory rather than trusting it. This prints what that check is
//! matching against.
fn main() {
    match of_rpc::request(&of_rpc::Request::Inventory) {
        Ok(of_rpc::Response::Inventory(inventory)) => {
            println!("backend: {}", inventory.backend);
            println!("channels:");
            for c in &inventory.channels {
                println!("  {:<24} {:<18} tach {:?}", c.id, c.label, c.tachometer);
            }
            println!("sensors:");
            for s in &inventory.sensors {
                println!("  {:<24} {:<22} {:?}", s.id, s.label, s.quantity);
            }
        }
        other => println!("{other:?}"),
    }
}
