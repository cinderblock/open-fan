//! The applications we know how to recognise.
//!
//! Recognising a competitor by process name is a heuristic and is treated as one: it
//! tells us *who to ask about* a channel that looks foreign-controlled, never on its own
//! that a channel is contended. The chip is the authority. A tool can sit in the tray
//! controlling nothing at all, and a channel can be under foreign control with no known
//! process to blame.
//!
//! Entries are split by what they actually do, because the right response differs. A
//! program that drives PWM has to stand down before we take a channel. A program that
//! only reads sensors can coexist indefinitely, as long as everyone honours the ISA bus
//! mutex — so listing it is about explaining a busy bus, not about asking it to quit.

/// What an application does to the hardware we care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Drives fan PWM. Cannot share a channel with us.
    Controller,
    /// Reads sensors, and may hold the bus, but does not drive fans.
    Monitor,
    /// Vendor software that may do either, usually through its own kernel driver, and
    /// often cannot be reasoned with. Worth naming so a user is not left guessing.
    Vendor,
}

/// An application we can recognise by process name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownApp {
    /// Stable identifier for logs and configuration.
    pub key: &'static str,
    /// What to call it in the interface.
    pub name: &'static str,
    pub role: Role,
    /// Executable names without extension, matched case-insensitively.
    pub process_names: &'static [&'static str],
    /// Shown to the user when explaining what is in the way.
    pub note: &'static str,
}

/// Applications known to contend for fan control or the Super I/O bus.
///
/// Deliberately conservative. A false positive here means telling someone to close a
/// program that was not the problem, which erodes trust in every later warning.
pub const KNOWN_APPS: &[KnownApp] = &[
    KnownApp {
        key: "fancontrol",
        name: "FanControl",
        role: Role::Controller,
        process_names: &["FanControl", "FanControl.Service"],
        note: "Drives Super I/O PWM channels directly. Also a PawnIO client, so it shares \
               the bus correctly — but it cannot share a fan header.",
    },
    KnownApp {
        key: "librehardwaremonitor",
        name: "LibreHardwareMonitor",
        role: Role::Controller,
        process_names: &["LibreHardwareMonitor"],
        note: "Reads sensors, and can set fan control when asked to.",
    },
    KnownApp {
        key: "openhardwaremonitor",
        name: "Open Hardware Monitor",
        role: Role::Controller,
        process_names: &["OpenHardwareMonitor"],
        note: "Unmaintained predecessor of LibreHardwareMonitor; may use WinRing0.",
    },
    KnownApp {
        key: "speedfan",
        name: "SpeedFan",
        role: Role::Controller,
        process_names: &["speedfan"],
        note: "Long-standing fan controller with its own kernel driver.",
    },
    KnownApp {
        key: "argusmonitor",
        name: "Argus Monitor",
        role: Role::Controller,
        process_names: &["ArgusMonitor"],
        note: "Drives fan headers through its own driver.",
    },
    KnownApp {
        key: "hwinfo",
        name: "HWiNFO",
        role: Role::Monitor,
        process_names: &["HWiNFO64", "HWiNFO32", "HWiNFO64A"],
        note: "Primarily a monitor and safe to run alongside, though it can be configured \
               to control fans. Polls the Super I/O continuously.",
    },
    KnownApp {
        key: "aida64",
        name: "AIDA64",
        role: Role::Monitor,
        process_names: &["aida64"],
        note: "Polls the Super I/O continuously while its sensor panel is open.",
    },
    KnownApp {
        key: "armourycrate",
        name: "ASUS Armoury Crate",
        role: Role::Vendor,
        process_names: &[
            "ArmouryCrate.Service",
            "ArmourySocketServer",
            "ArmouryCrate.UserSessionHelper",
            "AsusCertService",
        ],
        note: "ASUS vendor software. Reaches hardware through its own kernel driver and \
               holds a kernel mutex that user mode cannot acquire, so it cannot be \
               serialised against — avoid contending for the EC ports rather than trying.",
    },
    KnownApp {
        key: "msiafterburner",
        name: "MSI Afterburner",
        role: Role::Vendor,
        process_names: &["MSIAfterburner"],
        note: "Controls GPU fans. Does not usually touch motherboard headers.",
    },
    KnownApp {
        key: "corsairicue",
        name: "Corsair iCUE",
        role: Role::Vendor,
        process_names: &["iCUE"],
        note: "Controls Corsair fan hubs and AIO pumps over USB, not the Super I/O.",
    },
    KnownApp {
        key: "nzxtcam",
        name: "NZXT CAM",
        role: Role::Vendor,
        process_names: &["NZXT CAM"],
        note: "Controls NZXT hardware over USB, not the Super I/O.",
    },
];

/// Find the known application owning a process name, if any.
///
/// Case-insensitive, and tolerates a `.exe` suffix so a caller can pass whatever the OS
/// handed it.
pub fn lookup(process_name: &str) -> Option<&'static KnownApp> {
    let trimmed = process_name
        .strip_suffix(".exe")
        .or_else(|| process_name.strip_suffix(".EXE"))
        .unwrap_or(process_name);

    KNOWN_APPS.iter().find(|app| {
        app.process_names
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(trimmed))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_machines_competitor_is_recognised_however_it_is_spelled() {
        // The exact spellings the OS produces, plus the ones a human might type.
        for spelling in [
            "FanControl.exe",
            "fancontrol",
            "FANCONTROL.EXE",
            "FanControl",
        ] {
            assert_eq!(
                lookup(spelling).map(|a| a.key),
                Some("fancontrol"),
                "failed for {spelling}"
            );
        }
    }

    #[test]
    fn unknown_processes_are_not_guessed_at() {
        // A false positive tells someone to close a program that was not the problem,
        // which costs trust in every later warning.
        assert_eq!(lookup("explorer.exe"), None);
        assert_eq!(lookup("notepad"), None);
        assert_eq!(lookup(""), None);
    }

    #[test]
    fn monitors_are_not_classed_as_controllers() {
        // The response differs: a monitor can coexist indefinitely under the bus mutex,
        // so asking a user to close it would be noise.
        assert_eq!(lookup("HWiNFO64").map(|a| a.role), Some(Role::Monitor));
        assert_eq!(lookup("FanControl").map(|a| a.role), Some(Role::Controller));
    }

    #[test]
    fn every_entry_is_well_formed_and_uniquely_keyed() {
        let mut keys = std::collections::BTreeSet::new();
        let mut names = std::collections::BTreeSet::new();

        for app in KNOWN_APPS {
            assert!(keys.insert(app.key), "duplicate key {}", app.key);
            assert!(!app.process_names.is_empty(), "{} matches nothing", app.key);
            assert!(!app.note.is_empty(), "{} has no explanation", app.key);

            for process in app.process_names {
                // A process name claimed by two entries would make attribution arbitrary.
                assert!(
                    names.insert(process.to_ascii_lowercase()),
                    "{process} is claimed by more than one entry"
                );
            }
        }
    }
}
