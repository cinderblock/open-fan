//! Finding — and switching off — another fan controller's autostart.
//!
//! # Why this exists at all
//!
//! A rival that is *installed with autostart* but not running right now is invisible to
//! [`crate::detect`], and the survey will happily report that nothing is in the way. Then
//! the machine reboots, the other tool starts, and two programs are driving one fan.
//!
//! That is the worst shape a contention bug can take: the check passed, the user was
//! told they were clear, and the fight starts later, on a machine nobody is watching. So
//! autostart is surveyed as its own kind of obstacle — a *future* one — rather than
//! folded into "is it running".
//!
//! # Nothing here runs on its own
//!
//! [`find`] reads. [`disable`] writes, and only ever for one entry a person explicitly
//! pointed at. Removing somebody's startup entries because we decided we should be in
//! charge would be exactly the behaviour that makes people distrust software with kernel
//! access.
//!
//! # Reversible where the platform allows it
//!
//! Disabling beats deleting, so this prefers whichever the location supports:
//!
//! | Location | What we do | Reversible |
//! | --- | --- | --- |
//! | Scheduled task | disable the task | yes, by re-enabling it |
//! | Startup shortcut | rename it aside | yes, by renaming it back |
//! | `Run` registry value | delete the value | only from what we report back |
//!
//! A `Run` value has no disabled state we can portably write, so the old command is
//! returned in [`Disabled::restore_hint`] — the user gets told exactly what was removed
//! rather than having it vanish.
//!
//! # Running as LocalSystem changes where "the user" is
//!
//! The service is LocalSystem, so `HKEY_CURRENT_USER` is the *service's* profile and the
//! signed-in user's `Run` key is not in it. Every loaded user hive under `HKEY_USERS` is
//! scanned instead. Getting this wrong would mean reporting "no autostart" on a machine
//! that plainly has one — a false all-clear, which is the failure this module exists to
//! prevent.

use std::path::{Path, PathBuf};

use crate::known::{KNOWN_APPS, KnownApp};

/// Where an autostart entry lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A value under a `Run` key. `hive` is the printable root, for the interface.
    RunValue {
        hive: String,
        subkey: String,
        value: String,
    },
    /// A task in the Windows scheduler. Tasks are how a tool arranges to start with
    /// administrator rights, so this is the common case for a fan controller.
    ScheduledTask { name: String },
    /// A shortcut in a Startup folder.
    StartupShortcut { path: PathBuf },
}

impl Location {
    /// How to describe this to somebody who did not write it.
    pub fn describe(&self) -> String {
        match self {
            Self::RunValue { hive, value, .. } => {
                format!("a startup entry named \"{value}\" in {hive}")
            }
            Self::ScheduledTask { name } => format!("a scheduled task named \"{name}\""),
            Self::StartupShortcut { path } => format!(
                "a shortcut in your Startup folder ({})",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
        }
    }

    /// Whether switching this off can be undone without us having recorded anything.
    pub fn is_reversible(&self) -> bool {
        !matches!(self, Self::RunValue { .. })
    }
}

/// A known application arranged to start with the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartEntry {
    pub app: &'static KnownApp,
    pub location: Location,
    /// The command line it starts, as found.
    pub command: String,
}

/// What switching one off actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disabled {
    pub what: String,
    /// How to put it back, for entries the platform gave us no reversible option for.
    pub restore_hint: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    #[error("could not switch off {what}: {detail}")]
    Failed { what: String, detail: String },

    #[error("autostart handling is only implemented on Windows")]
    Unsupported,
}

/// Every known fan controller arranged to start with this machine.
///
/// Read-only. A machine with none returns an empty list, which is the answer that lets a
/// caller say "nothing will come back after a reboot" honestly.
pub fn find() -> Vec<AutostartEntry> {
    scan().0
}

/// The rivals, and how many entries were examined to find them.
///
/// The count is the point. "No fan controller starts with this machine" and "the scan saw
/// nothing at all" produce the same empty list, and only one of them is an all-clear
/// somebody should act on. A caller that reports the total can tell them apart; a caller
/// that only sees the list cannot.
pub fn scan() -> (Vec<AutostartEntry>, usize) {
    let all = survey();
    let examined = all.len();

    let rivals = all
        .into_iter()
        .filter_map(|(location, command)| {
            // An entry left behind by an uninstall starts nothing, and reporting it would
            // send someone looking for a program that is not there.
            let app = app_for_command(&command)?;
            still_installed(&command).then_some(AutostartEntry {
                app,
                location,
                command,
            })
        })
        .collect();

    (rivals, examined)
}

/// Everything that starts with this machine, recognised or not.
///
/// The unfiltered scan behind [`find`]. Exposed because a survey that finds nothing is
/// indistinguishable from a survey that is not working — and the difference matters here,
/// since "no autostart" is an all-clear somebody will act on.
pub fn survey() -> Vec<(Location, String)> {
    #[cfg(windows)]
    {
        windows_impl::survey()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Switch one entry off. **Only ever from an explicit instruction.**
pub fn disable(entry: &AutostartEntry) -> Result<Disabled, AutostartError> {
    #[cfg(windows)]
    {
        windows_impl::disable(entry)
    }
    #[cfg(not(windows))]
    {
        let _ = entry;
        Err(AutostartError::Unsupported)
    }
}

/// Match a command line against the applications we know.
///
/// Public so a survey can show which entries were recognised without repeating the rule.
pub fn recognise(command: &str) -> Option<&'static KnownApp> {
    app_for_command(command)
}

/// Match a command line against the applications we know.
///
/// Matched on the executable name rather than on the name of the entry, because the
/// entry's name is whatever its author typed and the executable is what actually runs.
fn app_for_command(command: &str) -> Option<&'static KnownApp> {
    let haystack = command.to_ascii_lowercase();
    KNOWN_APPS.iter().find(|app| {
        app.process_names.iter().any(|process| {
            // Bounded by the extension so a tool called `fan.exe` cannot be matched by a
            // path that merely contains the word.
            haystack.contains(&format!("{}.exe", process.to_ascii_lowercase()))
        })
    })
}

/// Pull the executable out of a command line, quoted or not.
fn executable_of(command: &str) -> Option<&str> {
    let trimmed = command.trim();
    if let Some(rest) = trimmed.strip_prefix('"') {
        rest.split('"').next()
    } else {
        trimmed.split_whitespace().next()
    }
}

/// Find an executable path embedded in arbitrary text.
///
/// Shell links store their target inside a binary structure, so there is no command line
/// to split — the path has to be found. Anchored on `.exe` and walked back to a drive
/// letter, which is the shape every entry we care about has.
fn executable_in(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let end = lower.find(".exe")? + ".exe".len();

    let bytes = text.as_bytes();
    // Back to the `X:\` that starts the path. Without one there is nothing checkable.
    let start = (0..end.saturating_sub(4)).rev().find(|&i| {
        bytes.get(i + 1) == Some(&b':')
            && matches!(bytes.get(i + 2), Some(b'\\') | Some(b'/'))
            && bytes[i].is_ascii_alphabetic()
    })?;

    Some(text.get(start..end)?.to_owned())
}

/// Whether a path still exists, used to ignore entries left behind by an uninstall.
fn still_installed(command: &str) -> bool {
    executable_of(command).is_some_and(|exe| Path::new(exe).is_file())
}

#[cfg(windows)]
mod windows_impl {
    use super::*;

    use windows::Win32::Foundation::{ERROR_SUCCESS, MAX_PATH};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ,
        RegCloseKey, RegDeleteValueW, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
    };
    use windows::core::{HSTRING, PWSTR};

    const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_SUBKEY_WOW: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";

    /// A registry key that closes itself.
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle came from RegOpenKeyExW and is not used again.
            let _ = unsafe { RegCloseKey(self.0) };
        }
    }

    fn open(
        root: HKEY,
        path: &str,
        access: windows::Win32::System::Registry::REG_SAM_FLAGS,
    ) -> Option<Key> {
        let mut handle = HKEY::default();
        // SAFETY: `path` outlives the call; `handle` is a valid out-pointer.
        let status =
            unsafe { RegOpenKeyExW(root, &HSTRING::from(path), None, access, &mut handle) };
        (status == ERROR_SUCCESS).then_some(Key(handle))
    }

    /// Every `(name, data)` string value directly under a key.
    fn string_values(key: &Key) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for index in 0u32.. {
            let mut name = [0u16; 16_384];
            let mut name_len = u32::try_from(name.len()).unwrap_or(u32::MAX);
            let mut data = [0u8; 8192];
            let mut data_len = u32::try_from(data.len()).unwrap_or(u32::MAX);
            let mut kind = 0u32;

            // SAFETY: every buffer and its length describe the same allocation, and the
            // call writes no more than the length it is given.
            let status = unsafe {
                RegEnumValueW(
                    key.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(&mut kind),
                    Some(data.as_mut_ptr()),
                    Some(&mut data_len),
                )
            };
            if status != ERROR_SUCCESS {
                break;
            }
            if kind != REG_SZ.0 && kind != REG_EXPAND_SZ.0 {
                continue;
            }

            let name = String::from_utf16_lossy(&name[..name_len as usize]);
            // Registry strings are UTF-16 and usually, but not always, NUL-terminated.
            let units: Vec<u16> = data[..data_len as usize]
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .take_while(|&c| c != 0)
                .collect();
            out.push((name, String::from_utf16_lossy(&units)));
        }
        out
    }

    /// The names of a key's immediate children.
    fn subkeys(key: &Key) -> Vec<String> {
        let mut out = Vec::new();
        for index in 0u32.. {
            let mut name = [0u16; MAX_PATH as usize];
            let mut len = u32::try_from(name.len()).unwrap_or(u32::MAX);
            // SAFETY: `name` and `len` describe the same allocation.
            let status = unsafe {
                RegEnumKeyExW(
                    key.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if status != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        out
    }

    /// Every `Run` key worth looking in, as `(printable hive, root, subkey)`.
    ///
    /// The signed-in user's hive is reached through `HKEY_USERS` rather than
    /// `HKEY_CURRENT_USER`, because as LocalSystem the latter is the service's own
    /// profile and would quietly report that the user has no startup entries.
    fn run_keys() -> Vec<(String, HKEY, String)> {
        let mut out = vec![
            (
                "this machine".to_owned(),
                HKEY_LOCAL_MACHINE,
                RUN_SUBKEY.to_owned(),
            ),
            (
                "this machine".to_owned(),
                HKEY_LOCAL_MACHINE,
                RUN_SUBKEY_WOW.to_owned(),
            ),
        ];

        if let Some(users) = open(HKEY_USERS, "", KEY_READ) {
            for sid in subkeys(&users) {
                // `_Classes` hives hold file associations, and `.DEFAULT` is the profile
                // used before anyone signs in. Neither carries a person's startup list.
                if sid.ends_with("_Classes") || sid == ".DEFAULT" {
                    continue;
                }
                out.push((
                    "your account".to_owned(),
                    HKEY_USERS,
                    format!(r"{sid}\{RUN_SUBKEY}"),
                ));
            }
        }
        out
    }

    fn startup_folders() -> Vec<PathBuf> {
        ["APPDATA", "ProgramData"]
            .iter()
            .filter_map(std::env::var_os)
            .map(|base| PathBuf::from(base).join(r"Microsoft\Windows\Start Menu\Programs\Startup"))
            .collect()
    }

    /// Where the scheduler keeps task definitions.
    ///
    /// Read as XML rather than through the scheduler's COM interface: the service runs as
    /// LocalSystem and can read this directory outright, and an `<Exec><Command>` element
    /// is a great deal less machinery than instantiating a task service.
    fn task_root() -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join(r"System32\Tasks")
    }

    /// Pull the first `<Command>` out of a task definition.
    ///
    /// Deliberately a substring search rather than a parser. Task XML is written by the
    /// scheduler to a fixed shape, we want exactly one element from it, and a malformed
    /// or unexpected file should yield nothing rather than an error — this is a survey,
    /// not a validator.
    pub(super) fn command_in_task(xml: &str) -> Option<String> {
        let start = xml.find("<Command>")? + "<Command>".len();
        let end = xml[start..].find("</Command>")? + start;
        let raw = xml[start..end].trim();
        (!raw.is_empty()).then(|| raw.replace("&quot;", "\"").replace("&amp;", "&"))
    }

    /// Walk the task folder, yielding `(task name, command)`.
    fn tasks() -> Vec<(String, String)> {
        fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, String)>, depth: usize) {
            // Tasks nest a few folders deep at most; the bound is against a directory
            // loop rather than against any real layout.
            if depth > 8 {
                return;
            }
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if path.is_dir() {
                    walk(&path, &format!("{prefix}{name}\\"), out, depth + 1);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    // Task XML is UTF-16 with a BOM.
                    let text = decode_utf16_or_utf8(&bytes);
                    if let Some(command) = command_in_task(&text) {
                        out.push((format!("{prefix}{name}"), command));
                    }
                }
            }
        }

        let mut out = Vec::new();
        walk(&task_root(), "", &mut out, 0);
        out
    }

    fn decode_utf16_or_utf8(bytes: &[u8]) -> String {
        match bytes {
            [0xFF, 0xFE, rest @ ..] => String::from_utf16_lossy(
                &rest
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]]))
                    .collect::<Vec<_>>(),
            ),
            _ => String::from_utf8_lossy(bytes).into_owned(),
        }
    }

    /// The readable text inside a shortcut, which carries the path it points at.
    ///
    /// A `.lnk` is a structured binary that stores its target as a plain string, often
    /// twice — once narrow and once wide. Recovering those strings is enough to answer
    /// the only question being asked ("does this start that program?") and avoids taking
    /// on a shell-link parser to answer it.
    fn shortcut_target(path: &Path) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;

        let narrow = String::from_utf8_lossy(&bytes).into_owned();
        let wide: String = bytes
            .chunks_exact(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .filter(|&c| c != 0)
            .filter_map(|c| char::from_u32(u32::from(c)))
            .collect();

        // The executable path itself, pulled out of whichever encoding carries it. The
        // whole blob would match by substring but could not then be checked against the
        // filesystem, so a real shortcut would be recognised and then dropped.
        for text in [&narrow, &wide] {
            if let Some(exe) = executable_in(text) {
                return Some(exe);
            }
        }
        // Nothing path-shaped. The shortcut's own name at least shows it exists, and the
        // raw bytes of a shell link are not something to print at a person.
        Some(path.file_stem()?.to_string_lossy().into_owned())
    }

    /// Every autostart entry on this machine, recognised or not.
    pub fn survey() -> Vec<(Location, String)> {
        let mut out = Vec::new();

        for (hive, root, subkey) in run_keys() {
            let Some(key) = open(root, &subkey, KEY_READ) else {
                continue;
            };
            for (value, command) in string_values(&key) {
                out.push((
                    Location::RunValue {
                        hive: hive.clone(),
                        subkey: subkey.clone(),
                        value,
                    },
                    command,
                ));
            }
        }

        for (name, command) in tasks() {
            out.push((Location::ScheduledTask { name }, command));
        }

        for folder in startup_folders() {
            let Ok(entries) = std::fs::read_dir(&folder) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
                    && let Some(target) = shortcut_target(&path)
                {
                    out.push((Location::StartupShortcut { path }, target));
                }
            }
        }

        out
    }

    pub fn disable(entry: &AutostartEntry) -> Result<Disabled, AutostartError> {
        let what = format!("{}: {}", entry.app.name, entry.location.describe());
        let fail = |detail: String| AutostartError::Failed {
            what: what.clone(),
            detail,
        };

        match &entry.location {
            Location::ScheduledTask { name } => {
                // Disabled rather than deleted: it is the reversible option the scheduler
                // offers, and an entry someone can put back is a kinder thing to leave
                // behind than one that is simply gone.
                let output = std::process::Command::new("schtasks.exe")
                    .args(["/change", "/tn", name, "/disable"])
                    .output()
                    .map_err(|e| fail(e.to_string()))?;

                if !output.status.success() {
                    return Err(fail(
                        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                    ));
                }
                Ok(Disabled {
                    what,
                    restore_hint: None,
                })
            }

            Location::StartupShortcut { path } => {
                // Renamed aside rather than deleted, for the same reason.
                let aside = path.with_extension("lnk.openfan-disabled");
                std::fs::rename(path, &aside).map_err(|e| fail(e.to_string()))?;
                Ok(Disabled {
                    what,
                    restore_hint: None,
                })
            }

            Location::RunValue {
                subkey,
                value,
                hive,
            } => {
                // No portable disabled state for a `Run` value, so it goes — and the
                // command goes back to the caller, because a startup entry that vanishes
                // without a trace is not something to do to somebody's machine quietly.
                let root = if hive == "this machine" {
                    HKEY_LOCAL_MACHINE
                } else {
                    HKEY_USERS
                };
                let key = open(root, subkey, KEY_SET_VALUE)
                    .ok_or_else(|| fail("could not open the key for writing".to_owned()))?;

                // SAFETY: `value` outlives the call.
                let status = unsafe { RegDeleteValueW(key.0, &HSTRING::from(value.as_str())) };
                if status != ERROR_SUCCESS {
                    return Err(fail(format!(
                        "the registry refused the change ({status:?})"
                    )));
                }

                Ok(Disabled {
                    what,
                    restore_hint: Some(format!(
                        "It ran: {}. Recreate it as a value named \"{value}\" under {subkey} \
                         to put it back.",
                        entry.command
                    )),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_matched_by_its_executable_not_its_wording() {
        // The name of a startup entry is whatever its author typed; the executable is
        // what actually runs.
        let app = app_for_command(r#""C:\Program Files (x86)\FanControl\FanControl.exe" -m"#);
        assert_eq!(app.map(|a| a.key), Some("fancontrol"));

        // Case and quoting vary between the registry, task XML and shortcuts.
        assert_eq!(
            app_for_command(r"c:\tools\FANCONTROL.EXE").map(|a| a.key),
            Some("fancontrol")
        );
    }

    #[test]
    fn an_unrelated_startup_entry_is_left_alone() {
        // A false positive here would offer to remove somebody's actual startup items.
        assert!(app_for_command(r"C:\Windows\explorer.exe").is_none());
        assert!(app_for_command("").is_none());
        assert!(app_for_command(r"C:\Program Files\Steam\steam.exe -silent").is_none());
    }

    #[test]
    fn a_program_merely_named_after_one_is_not_matched() {
        // Bounded by the extension, so this is a different program and stays untouched.
        assert!(app_for_command(r"C:\tools\FanControlHelperThing\run.exe").is_none());
    }

    #[test]
    fn an_executable_is_read_out_of_a_quoted_command_line() {
        assert_eq!(
            executable_of(r#""C:\Program Files\A B\app.exe" --flag"#),
            Some(r"C:\Program Files\A B\app.exe")
        );
        assert_eq!(executable_of(r"C:\a\b.exe -x"), Some(r"C:\a\b.exe"));
        assert_eq!(executable_of("   "), None);
    }

    #[test]
    fn a_run_value_is_the_one_location_we_cannot_reverse_on_our_own() {
        // Which is why disabling one has to report what it removed.
        let run = Location::RunValue {
            hive: "your account".into(),
            subkey: "Software".into(),
            value: "Thing".into(),
        };
        assert!(!run.is_reversible());

        assert!(
            Location::ScheduledTask {
                name: "Thing".into()
            }
            .is_reversible()
        );
        assert!(
            Location::StartupShortcut {
                path: "a.lnk".into()
            }
            .is_reversible()
        );
    }

    #[test]
    fn every_location_can_describe_itself_to_someone_who_did_not_write_it() {
        for location in [
            Location::RunValue {
                hive: "your account".into(),
                subkey: r"Software\Microsoft\Windows\CurrentVersion\Run".into(),
                value: "FanControl".into(),
            },
            Location::ScheduledTask {
                name: "FanControl".into(),
            },
            Location::StartupShortcut {
                path: r"C:\Users\a\Startup\FanControl.lnk".into(),
            },
        ] {
            let text = location.describe();
            assert!(!text.is_empty());
            // No registry paths or raw handles in something a person reads.
            assert!(!text.contains("HKEY"), "{text}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_task_definition_yields_the_command_it_runs() {
        let xml = r#"<?xml version="1.0"?><Task><Actions><Exec>
            <Command>"C:\Program Files (x86)\FanControl\FanControl.exe"</Command>
            <Arguments>-m</Arguments></Exec></Actions></Task>"#;
        let command = super::windows_impl::command_in_task(xml).expect("finds the command");
        assert!(command.contains("FanControl.exe"));
        assert_eq!(app_for_command(&command).map(|a| a.key), Some("fancontrol"));
    }

    #[cfg(windows)]
    #[test]
    fn a_task_with_no_action_yields_nothing_rather_than_failing() {
        // This is a survey of whatever is on a machine, so an unexpected file is a
        // non-answer and not an error.
        assert_eq!(super::windows_impl::command_in_task("<Task/>"), None);
        assert_eq!(super::windows_impl::command_in_task(""), None);
        assert_eq!(
            super::windows_impl::command_in_task("<Command></Command>"),
            None
        );
    }

    #[test]
    fn a_path_is_recovered_from_the_middle_of_a_binary_shortcut() {
        // The bug this guards: a shell link stores its target inside a binary structure,
        // so matching against the whole blob recognised the application and then failed
        // to check whether it was still installed — a real startup entry, detected and
        // then silently dropped.
        let blob = "L\0\0\0\u{1}\u{14}\u{2}rubbish\0\0\
                    C:\\Program Files (x86)\\FanControl\\FanControl.exe\0more rubbish";

        let found = executable_in(blob).expect("finds the target");
        assert_eq!(found, r"C:\Program Files (x86)\FanControl\FanControl.exe");
        assert_eq!(app_for_command(&found).map(|a| a.key), Some("fancontrol"));
    }

    #[test]
    fn text_with_no_path_in_it_yields_nothing() {
        assert_eq!(executable_in("just some words"), None);
        assert_eq!(executable_in(""), None);
        // An `.exe` with no drive letter is not a path whose existence we could check.
        assert_eq!(executable_in("FanControl.exe"), None);
    }

    #[test]
    fn surveying_is_total() {
        // Must never panic, whatever this machine happens to have.
        let _ = find();
    }
}
