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
fn ask(request: Request) -> Result<Response, String> {
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
