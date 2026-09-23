//! Talking to the OpenFan service.
//!
//! The application no longer owns an engine. Fan control runs in a service that started
//! at boot, holds the only elevated handle to the hardware, and will still be running
//! when this window is closed — so everything here is a request over a pipe.
//!
//! That inverts one thing worth being explicit about: **this process is now disposable**.
//! It can be closed, crashed, killed or never started, and the fans carry on. The old
//! arrangement kept the engine alive by keeping the application alive; this one does not
//! have to.
//!
//! # Service not running is an ordinary state
//!
//! On a machine where OpenFan has been installed but the service has not started yet —
//! or has been stopped deliberately — every command here returns a plain, readable
//! failure rather than an exception. The editor renders that as guidance. Treating it as
//! a crash would turn the most common first-run state into a bug report.

use of_ipc::{ContentionReport, Graph, HardwareInventory, SnapshotDto, TakeoverResult};
use of_rpc::{Request, Response};

/// What the editor knows about the service.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub running: bool,
    /// Service version, when it answered.
    pub version: Option<String>,
    /// True when the service speaks a protocol this editor understands.
    pub compatible: bool,
    /// A sentence the interface can show verbatim.
    pub summary: String,
}

/// Send a request, turning transport failures into readable strings.
///
/// Tauri commands return `Result<_, String>` because the failure is something the editor
/// shows a person, not something it branches on.
pub(crate) fn ask(request: Request) -> Result<Response, String> {
    match of_rpc::request(&request) {
        Ok(response) => {
            if let Some(message) = response.error_message() {
                return Err(message.to_owned());
            }
            Ok(response)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Is the service there, and does it speak our protocol?
#[tauri::command]
pub fn service_status() -> ServiceStatus {
    match of_rpc::request(&Request::Hello) {
        Ok(Response::Hello { version, protocol }) => {
            let compatible = protocol == of_rpc::PROTOCOL_VERSION;
            ServiceStatus {
                running: true,
                version: Some(version.clone()),
                compatible,
                summary: if compatible {
                    format!("Connected to the OpenFan service (version {version}).")
                } else {
                    // Naming both numbers turns "it does not work" into "one of these is
                    // out of date", which is the thing the user can act on.
                    format!(
                        "The OpenFan service speaks protocol {protocol} and this window \
                         speaks {}. Update whichever is older.",
                        of_rpc::PROTOCOL_VERSION
                    )
                },
            }
        }
        Ok(_) => ServiceStatus {
            running: true,
            version: None,
            compatible: false,
            summary: "The OpenFan service answered something unexpected.".to_owned(),
        },
        Err(e) => ServiceStatus {
            running: false,
            version: None,
            compatible: false,
            summary: format!(
                "{e}. Fan control runs in a background service; without it this window can \
                 only show what it last knew."
            ),
        },
    }
}

#[tauri::command]
pub fn inventory() -> Result<HardwareInventory, String> {
    match ask(Request::Inventory)? {
        Response::Inventory(inventory) => Ok(*inventory),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[tauri::command]
pub fn get_graph() -> Result<Graph, String> {
    match ask(Request::GetGraph)? {
        Response::Graph(graph) => Ok(*graph),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

/// Install a graph.
///
/// A rejection is a list of problems, not an error: it is the expected outcome of
/// editing, and the running configuration is untouched either way.
#[tauri::command]
pub fn set_graph(graph: Graph) -> Result<Vec<of_ipc::ValidationError>, String> {
    match ask(Request::SetGraph {
        graph: Box::new(graph),
    })? {
        Response::GraphAccepted => Ok(Vec::new()),
        Response::GraphRejected { errors } => Ok(errors),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[tauri::command]
pub fn snapshot() -> Result<SnapshotDto, String> {
    match ask(Request::Snapshot)? {
        Response::Snapshot(snapshot) => Ok(*snapshot),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[tauri::command]
pub fn rescan() -> Result<(), String> {
    ask(Request::Rescan).map(|_| ())
}

#[tauri::command]
pub fn contention_report() -> Result<ContentionReport, String> {
    match ask(Request::ContentionReport)? {
        Response::ContentionReport(report) => Ok(*report),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[tauri::command]
pub fn take_over(force: bool) -> Result<TakeoverResult, String> {
    match ask(Request::TakeOver { force })? {
        Response::Takeover(result) => Ok(*result),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

/// What the service knows about updates.
///
/// Passed through as opaque JSON rather than re-modelled here. The service owns the
/// meaning; the window renders it.
#[tauri::command]
pub fn update_status() -> Result<serde_json::Value, String> {
    match ask(Request::UpdateStatus)? {
        Response::UpdateStatus(status) => Ok(*status),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

/// Ask the feed now.
#[tauri::command]
pub fn check_for_update() -> Result<serde_json::Value, String> {
    match ask(Request::CheckForUpdate)? {
        Response::UpdateStatus(status) => Ok(*status),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

/// Turn unattended installation on or off.
#[tauri::command]
pub fn set_auto_update(enabled: bool) -> Result<serde_json::Value, String> {
    match ask(Request::SetAutoUpdate { enabled })? {
        Response::UpdateStatus(status) => Ok(*status),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

/// Install silently, as the service. No prompt.
///
/// The service downloads, verifies and runs the installer itself. This window is not
/// involved beyond asking, and will be closed by the installer along with the service.
#[tauri::command]
pub fn apply_update_silently() -> Result<(), String> {
    ask(Request::ApplyUpdate).map(|_| ())
}

/// Install with a prompt, as the user.
///
/// The service still does the downloading and the signature check — that must not move
/// into an unelevated process, where a caller could substitute the file. All the window
/// does is launch what the service verified, which raises the UAC dialogue because the
/// installer requires administrator.
#[tauri::command]
pub fn apply_update_prompted() -> Result<String, String> {
    let (installer, version) = match ask(Request::PreparePromptedUpdate)? {
        Response::PreparedUpdate { installer, version } => (installer, version),
        other => return Err(format!("unexpected reply: {other:?}")),
    };

    launch_installer(&installer)?;
    Ok(version)
}

/// Run an installer such that Windows raises the elevation prompt.
///
/// `ShellExecute`, not `CreateProcess`: only the former honours the target's manifest and
/// elevates. A plain spawn of an application that requires administrator fails outright
/// with `ERROR_ELEVATION_REQUIRED`, which is a confusing way to discover this.
#[cfg(windows)]
fn launch_installer(path: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR};

    let file = HSTRING::from(path);
    let verb = HSTRING::from("open");

    // SAFETY: both strings are NUL-terminated and outlive the call; no output pointers.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    // ShellExecute reports failure as a value of 32 or less, including the user simply
    // declining the elevation prompt — which is a decision, not a fault.
    if result.0 as isize <= 32 {
        return Err(
            "the installer could not be started. If you declined the administrator \
             prompt, the update was not applied."
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn launch_installer(path: &str) -> Result<(), String> {
    let _ = path;
    Err("installing an update is only implemented on Windows".to_owned())
}

/// Fetch the PawnIO hardware module.
///
/// PawnIO installs a driver and no modules, so a machine can have it working and OpenFan
/// still see nothing. The service does the downloading — it is the one with somewhere
/// machine-wide to put the file, and the one that will use it.
///
/// Returns the service's own message, which names where the module went and that the
/// service must restart to pick it up.
#[tauri::command]
pub fn fetch_hardware_module() -> Result<String, String> {
    // This request answers with an Error-shaped message on success too, because the
    // outcome is a sentence for a person rather than a value to branch on.
    match of_rpc::request(&Request::FetchHardwareModule) {
        Ok(Response::Error { message }) => Ok(message),
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Whether this window opens at sign-in.
///
/// Only the window: the service starts at boot regardless, which is what keeps the fans
/// managed before anyone logs in. This is about whether the tray icon is there.
///
/// It became possible at all when the window stopped requiring administrator — Windows
/// will not launch an elevated application from the Run key, so "start with Windows" and
/// `requireAdministrator` were mutually exclusive.
#[tauri::command]
pub fn autostart_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt as _;
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt as _;

    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())?;
    } else {
        manager.disable().map_err(|e| e.to_string())?;
    }
    manager.is_enabled().map_err(|e| e.to_string())
}
