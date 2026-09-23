//! What starts with this machine, and which of it competes for the fans.
//!
//! Read-only. Run it to check the survey is actually seeing the machine — "nothing
//! found" and "not working" look identical from the outside, and the difference matters
//! when the answer is an all-clear somebody will act on.
//!
//! Run as an administrator (or as the service) to see scheduled tasks; the task folder
//! is not readable otherwise.

fn main() {
    let all = of_contention::autostart::survey();
    println!("{} autostart entries visible\n", all.len());

    for (location, command) in &all {
        let known = of_contention::autostart::recognise(command);
        println!(
            "  [{}] {}\n      {}",
            if known.is_some() {
                "fan control"
            } else {
                "           "
            },
            location.describe(),
            command.chars().take(120).collect::<String>()
        );
    }

    let rivals = of_contention::autostart::find();
    println!("\n{} of them are known fan controllers", rivals.len());
    for entry in &rivals {
        println!(
            "  {} — {} ({})",
            entry.app.name,
            entry.location.describe(),
            if entry.location.is_reversible() {
                "can be switched off reversibly"
            } else {
                "removal would have to be reported back"
            }
        );
    }
}
