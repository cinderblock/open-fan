//! Windows process enumeration and shutdown.
//!
//! Kept behind the platform gate so the rest of the crate stays readable, and so a Linux
//! build gets a clean `Unsupported` rather than a compile error.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, PostMessageW, PostThreadMessageW, WM_CLOSE, WM_QUIT,
};

use crate::known::lookup;
use crate::{ContentionError, Politeness, Result, Running, StopOutcome};

/// How long to wait between checking whether a process has gone.
const POLL: Duration = Duration::from_millis(100);

pub fn detect() -> Result<Vec<Running>> {
    // SAFETY: a snapshot handle is returned or the call fails; nothing is borrowed.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| ContentionError::Enumerate(e.to_string()))?;

    let mut found = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX),
        ..Default::default()
    };

    // SAFETY: `entry.dwSize` is set as the API requires, and `snapshot` is live until the
    // CloseHandle below.
    let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while ok {
        let name = String::from_utf16_lossy(
            &entry.szExeFile[..entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len())],
        );

        if let Some(app) = lookup(&name) {
            found.push(Running {
                app,
                pid: entry.th32ProcessID,
                process_name: name,
            });
        }

        // SAFETY: same invariants as the first call.
        ok = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }

    // SAFETY: the snapshot handle came from CreateToolhelp32Snapshot and is not used again.
    let _ = unsafe { CloseHandle(snapshot) };

    Ok(found)
}

pub fn is_running(pid: u32) -> bool {
    // A handle we can open means the process still exists. `PROCESS_QUERY_LIMITED_-
    // INFORMATION` is the least privilege that answers the question, so this works for
    // processes we could never terminate.
    // SAFETY: no pointers are involved; a failure is reported as an error.
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) if !handle.is_invalid() => {
            // SAFETY: the handle came from OpenProcess and is not used again.
            let _ = unsafe { CloseHandle(handle) };
            true
        }
        _ => false,
    }
}

/// Top-level windows belonging to a process, and the threads that own them.
///
/// A tray-minimised application often has **no visible window and no main window**, but
/// it still has hidden top-level windows carrying its message loop — that is where a
/// close request has to go.
fn windows_of(pid: u32) -> (Vec<HWND>, Vec<u32>) {
    struct Collect {
        pid: u32,
        windows: Vec<HWND>,
        threads: Vec<u32>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, param: LPARAM) -> windows::core::BOOL {
        // SAFETY: `param` is the &mut Collect we passed to EnumWindows, valid for the
        // duration of that call, and EnumWindows is not reentrant here.
        let collect = unsafe { &mut *(param.0 as *mut Collect) };

        let mut owner = 0u32;
        // SAFETY: `hwnd` comes from the enumeration; `owner` is a valid out-pointer.
        let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };

        if owner == collect.pid {
            collect.windows.push(hwnd);
            if !collect.threads.contains(&thread) {
                collect.threads.push(thread);
            }
        }
        true.into()
    }

    let mut collect = Collect {
        pid,
        windows: Vec::new(),
        threads: Vec::new(),
    };

    // SAFETY: the callback matches the expected signature and `collect` outlives the call.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut collect as isize)) };

    (collect.windows, collect.threads)
}

/// Run an application's own documented exit command.
///
/// Against its **own executable**, located from the running process rather than guessed
/// at: that is how the tools documenting such a flag say to reach a running instance, and
/// it means we cannot accidentally invoke some other program of the same name from PATH.
fn ask_to_exit(running: &Running, args: &[&str]) -> Result<()> {
    let exe = image_path(running.pid).ok_or_else(|| ContentionError::Stop {
        name: running.app.name.to_owned(),
        pid: running.pid,
        detail: "could not locate the running executable".to_owned(),
    })?;

    tracing::info!(app = running.app.name, ?args, "asking it to exit");

    std::process::Command::new(&exe)
        .args(args)
        .spawn()
        .map_err(|e| ContentionError::Stop {
            name: running.app.name.to_owned(),
            pid: running.pid,
            detail: format!("running {}: {e}", exe.display()),
        })?;

    Ok(())
}

/// The full path of a running process's executable.
fn image_path(pid: u32) -> Option<std::path::PathBuf> {
    use windows::Win32::System::Threading::QueryFullProcessImageNameW;

    // SAFETY: opening a handle with the least privilege that answers the question.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;

    let mut buffer = [0u16; 32768];
    let mut len = u32::try_from(buffer.len()).ok()?;

    // SAFETY: `buffer` and `len` describe the same allocation; the call writes at most
    // `len` code units and updates `len` to what it wrote.
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            windows::Win32::System::Threading::PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };

    // SAFETY: the handle came from OpenProcess and is not used again.
    let _ = unsafe { CloseHandle(handle) };

    result.ok()?;
    Some(std::path::PathBuf::from(String::from_utf16_lossy(
        &buffer[..len as usize],
    )))
}

/// Wait for a process to disappear, or give up.
fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_running(pid) {
            return true;
        }
        std::thread::sleep(POLL);
    }
    !is_running(pid)
}

pub fn stop(running: &Running, limit: Politeness, timeout: Duration) -> Result<StopOutcome> {
    if !is_running(running.pid) {
        return Ok(StopOutcome::AlreadyGone);
    }

    // Each rung gets its own share of the budget rather than the whole thing, so a
    // stubborn application cannot spend the caller's entire timeout on the gentlest
    // attempt and never reach one that works.
    let rungs: &[Politeness] = &[
        Politeness::Ask,
        Politeness::Close,
        Politeness::Quit,
        Politeness::Terminate,
    ];
    let attempts: Vec<Politeness> = rungs.iter().copied().filter(|r| *r <= limit).collect();
    let share = timeout
        .checked_div(u32::try_from(attempts.len().max(1)).unwrap_or(1))
        .unwrap_or(timeout);

    let mut reached = Politeness::Ask;
    for rung in attempts {
        reached = rung;
        tracing::info!(
            app = running.app.name,
            pid = running.pid,
            ?rung,
            "asking to stop"
        );

        match rung {
            // The application's own documented way to be asked to exit. Skipped without
            // a share of the budget when it publishes none, so a tool with no documented
            // command does not lose time to a rung that cannot apply to it.
            Politeness::Ask => match running.app.stop_command {
                Some(args) => match ask_to_exit(running, args) {
                    Ok(()) => {}
                    Err(e) => tracing::debug!(error = %e, "documented exit command failed"),
                },
                None => continue,
            },
            Politeness::Close => {
                let (windows, _) = windows_of(running.pid);
                for hwnd in windows {
                    // SAFETY: posting is asynchronous and does not dereference anything on
                    // our side; a dead window simply fails.
                    let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
                }
            }
            Politeness::Quit => {
                let (_, threads) = windows_of(running.pid);
                for thread in threads {
                    // SAFETY: as above; an exited thread simply fails.
                    let _ = unsafe { PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
                }
            }
            Politeness::Terminate => {
                // SAFETY: no pointers; failure is reported.
                let handle: HANDLE = unsafe { OpenProcess(PROCESS_TERMINATE, false, running.pid) }
                    .map_err(|e| ContentionError::Stop {
                        name: running.app.name.to_owned(),
                        pid: running.pid,
                        detail: format!("could not open for termination: {e}"),
                    })?;

                // SAFETY: `handle` was opened with PROCESS_TERMINATE.
                let killed = unsafe { TerminateProcess(handle, 1) };
                // SAFETY: the handle is not used again.
                let _ = unsafe { CloseHandle(handle) };

                killed.map_err(|e| ContentionError::Stop {
                    name: running.app.name.to_owned(),
                    pid: running.pid,
                    detail: e.to_string(),
                })?;
            }
        }

        if wait_for_exit(running.pid, share) {
            return Ok(StopOutcome::Stopped { via: rung });
        }
    }

    Ok(StopOutcome::StillRunning { tried: reached })
}
