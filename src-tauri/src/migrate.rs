//! Arriving on a machine that already has fan-control software on it.
//!
//! Three states a new installation can find, and each needs a different answer:
//!
//! 1. **Nothing else here.** Offer a starting point built from the hardware actually
//!    present. There is nothing to take over and nothing to import.
//! 2. **Something installed, not running.** Nothing is fighting us, but there is tuning
//!    work on disk worth reading, and possibly an autostart entry that will start a fight
//!    at the next reboot.
//! 3. **Something running now.** Both of the above, plus a rival to stand down.
//!
//! # Why the import happens here and not in the service
//!
//! Reading and translating a configuration is pure computation on a file the signed-in
//! user can already read. Doing it here means the LocalSystem service never opens a path
//! a client named — and "a privileged process reads whatever file it is told to" is a
//! much larger capability than importing a fan curve warrants.
//!
//! The service is asked only for the things that genuinely need its privileges: what is
//! running, and what is scheduled to start. Scheduled tasks are not readable without
//! elevation, which is why that half cannot move here.
//!
//! # Nothing here changes anything
//!
//! Discovery reads. Import translates and hands back a profile to look at. Applying it is
//! a separate, explicit step through the ordinary graph-setting path, and switching off
//! somebody's startup entry is a separate, explicit request per entry.

use of_config::import::{Fidelity, fancontrol};
use of_config::presets::{ChannelSummary, HardwareSummary, SensorSummary};
use of_ipc::{
    CalibrationDto, ForeignConfigDto, ImportNoteDto, ImportedProfileDto, MigrationSurvey,
};
use of_rpc::{Request, Response};

use crate::client::ask;

/// Everything we can say about the other fan-control software on this machine.
#[tauri::command]
pub fn migration_survey() -> Result<MigrationSurvey, String> {
    let contention = match ask(Request::ContentionReport)? {
        Response::ContentionReport(report) => *report,
        other => {
            return Err(format!(
                "unexpected answer to a contention report: {other:?}"
            ));
        }
    };

    // Best-effort: a service too old to answer this still gives a useful survey, and a
    // machine with no startup entries is the common case anyway.
    let autostart = match ask(Request::AutostartSurvey) {
        Ok(Response::AutostartSurvey(entries)) => entries,
        _ => Vec::new(),
    };

    let configs = find_configs();

    Ok(MigrationSurvey {
        nothing_else_here: contention.apps.is_empty()
            && autostart.is_empty()
            && configs.is_empty()
            && contention.stranded.is_empty(),
        contention,
        autostart,
        configs,
    })
}

/// Switch off one startup entry the service found. **Explicit, one at a time.**
#[tauri::command]
pub fn disable_rival_autostart(id: usize) -> Result<of_ipc::AutostartDisabledDto, String> {
    match ask(Request::DisableAutostart { id })? {
        Response::AutostartDisabled(done) => Ok(*done),
        other => Err(format!("unexpected answer: {other:?}")),
    }
}

/// Read a configuration and translate it. **Does not apply it.**
///
/// The path comes from [`find_configs`] or from a file the user picked, and is opened by
/// this process as the signed-in user — the same access they already have.
#[tauri::command]
pub fn import_foreign_config(path: String) -> Result<ImportedProfileDto, String> {
    let text = std::fs::read_to_string(&path).map_err(|e| format!("could not read {path}: {e}"))?;

    let hardware = hardware_summary()?;
    let imported = fancontrol::import(&text, &hardware).map_err(|e| e.to_string())?;

    Ok(ImportedProfileDto {
        name: imported.profile.name.clone(),
        empty: imported.is_empty(),
        graph: imported.profile.graph,
        notes: imported
            .notes
            .iter()
            .map(|n| ImportNoteDto {
                fidelity: match n.fidelity {
                    Fidelity::Exact => "exact",
                    Fidelity::Approximated => "approximated",
                    Fidelity::NeedsAttention => "needs-attention",
                    Fidelity::Skipped => "skipped",
                }
                .to_owned(),
                subject: n.subject.clone(),
                detail: n.detail.clone(),
            })
            .collect(),
        calibration: imported
            .calibration
            .iter()
            .map(|c| CalibrationDto {
                channel: c.channel.clone(),
                label: c.label.clone(),
                points: c.points.iter().map(|p| (p.duty_percent, p.rpm)).collect(),
                lowest_turning_duty: c.lowest_turning_duty(),
                found_the_stall: c.found_the_stall(),
            })
            .collect(),
    })
}

/// Ready-made configurations for this machine's actual hardware.
///
/// The answer for somebody who has never run fan-control software: there is nothing to
/// take over and nothing to import, so the useful thing to offer is a working starting
/// point rather than an empty canvas.
#[tauri::command]
pub fn starter_presets() -> Result<Vec<of_config::Preset>, String> {
    Ok(of_config::presets(&hardware_summary()?))
}

/// What this machine has, as the preset generator and importer need it.
fn hardware_summary() -> Result<HardwareSummary, String> {
    let inventory = match ask(Request::Inventory)? {
        Response::Inventory(inventory) => *inventory,
        other => {
            return Err(format!(
                "unexpected answer to an inventory request: {other:?}"
            ));
        }
    };

    Ok(HardwareSummary {
        sensors: inventory
            .sensors
            .iter()
            .map(|s| SensorSummary {
                id: s.id.clone(),
                label: s.label.clone(),
                quantity: s.quantity,
            })
            .collect(),
        channels: inventory
            .channels
            .iter()
            .map(|c| ChannelSummary {
                id: c.id.clone(),
                label: c.label.clone(),
                tachometer: c.tachometer.clone(),
            })
            .collect(),
    })
}

/// Configuration files belonging to other fan controllers, in the places they live.
///
/// # Why this is a search and not a lookup
///
/// There is no reliable record to consult. The documented place to find an installed
/// program — its uninstall registry entry — is **absent on the reference machine**, which
/// has the software installed and working. So this goes by evidence instead, strongest
/// first: a running process knows its own path, a startup entry names one, and failing
/// both, the usual install directories.
///
/// That last case is not a fallback in practice, it is the common one: software that is
/// installed but idle has no process and often no startup entry to point at it.
///
/// Anything this misses is covered by letting the user point at the file themselves,
/// which is the honest answer to a search that cannot be exhaustive: no guessing and no
/// privilege.
fn find_configs() -> Vec<ForeignConfigDto> {
    let mut found = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for (directory, found_via) in candidate_directories() {
        let path = directory.join(fancontrol::CONFIG_LEAF.replace('/', "\\"));
        if path.is_file() {
            let display = path.display().to_string();
            if seen.insert(display.clone()) {
                found.push(ForeignConfigDto {
                    key: "fancontrol".to_owned(),
                    name: "FanControl".to_owned(),
                    path: display,
                    found_via,
                });
            }
        }
    }

    found
}

/// Where a known fan controller might be installed, best evidence first.
fn candidate_directories() -> Vec<(std::path::PathBuf, String)> {
    let mut out = Vec::new();

    // Strongest evidence: it is running, so we know exactly where it is.
    for running in of_contention::detect().unwrap_or_default() {
        if let Some(directory) = image_directory(running.pid) {
            out.push((directory, "it is running now".to_owned()));
        }
    }

    // Next: something starts it, and the command says from where.
    for entry in of_contention::autostart::find() {
        if let Some(directory) = command_directory(&entry.command) {
            out.push((directory, "it starts with this machine".to_owned()));
        }
    }

    // Then the usual places, which is what covers an installation that is merely sitting
    // there — the case with no process and no startup entry to point at it.
    for variable in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(variable) {
            out.push((
                std::path::PathBuf::from(&base).join("FanControl"),
                "it is installed here".to_owned(),
            ));
            out.push((
                std::path::PathBuf::from(&base).join(r"Programs\FanControl"),
                "it is installed here".to_owned(),
            ));
        }
    }

    out
}

fn command_directory(command: &str) -> Option<std::path::PathBuf> {
    let trimmed = command.trim();
    let executable = if let Some(rest) = trimmed.strip_prefix('"') {
        rest.split('"').next()?
    } else {
        trimmed.split_whitespace().next()?
    };
    std::path::Path::new(executable).parent().map(Into::into)
}

#[cfg(windows)]
fn image_directory(pid: u32) -> Option<std::path::PathBuf> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };

    // SAFETY: the least privilege that answers "where is this process's image".
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;

    let mut buffer = [0u16; 32768];
    let mut length = u32::try_from(buffer.len()).ok()?;

    // SAFETY: `buffer` and `length` describe the same allocation; the call writes at most
    // `length` code units and updates it to what it wrote.
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };

    // SAFETY: the handle came from OpenProcess and is not used again.
    let _ = unsafe { CloseHandle(handle) };

    result.ok()?;
    std::path::PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize]))
        .parent()
        .map(Into::into)
}

#[cfg(not(windows))]
fn image_directory(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_is_read_out_of_a_quoted_command() {
        assert_eq!(
            command_directory(r#""C:\Program Files (x86)\FanControl\FanControl.exe" -m"#),
            Some(std::path::PathBuf::from(
                r"C:\Program Files (x86)\FanControl"
            ))
        );
        assert_eq!(
            command_directory(r"C:\tools\FanControl\FanControl.exe"),
            Some(std::path::PathBuf::from(r"C:\tools\FanControl"))
        );
        assert_eq!(command_directory("   "), None);
    }

    #[test]
    fn searching_is_total_and_reports_only_files_that_exist() {
        // Must never panic, and must never offer a path that is not there — sending
        // somebody to a file that does not exist is worse than finding nothing.
        for config in find_configs() {
            assert!(
                std::path::Path::new(&config.path).is_file(),
                "{} was offered but does not exist",
                config.path
            );
        }
    }
}
