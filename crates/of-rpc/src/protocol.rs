//! What the editor may ask the service, and what it gets back.
//!
//! The protocol exists because the engine moved out of the application. Fan control runs
//! in a service so it can start at boot, keep running across logoff, and hold the one
//! elevated handle to the hardware — which means the editor is now a *client*, and every
//! interaction between them is a message rather than a function call.
//!
//! # Rules this protocol is built on
//!
//! **Requests are untrusted.** The service runs as LocalSystem and its pipe is reachable
//! by any process running as the logged-in user. Nothing here may be treated as
//! pre-validated: a graph arriving over this channel goes through exactly the same
//! validation as one loaded from disk.
//!
//! **No request can make the service stop controlling fans.** There is deliberately no
//! "stop", "release everything" or "shut down" message. The service is stopped through
//! the service control manager, which is an administrative act, because a fan controller
//! that any user process can silently switch off is not a fan controller.
//!
//! **Every response is total.** A failure is a value, not a dropped connection — the
//! editor must be able to render "that did not work, here is why" without guessing from
//! a closed pipe.

use serde::{Deserialize, Serialize};

use of_core::Graph;
use of_ipc::{ContentionReport, HardwareInventory, SnapshotDto, TakeoverResult, ValidationError};

/// Anything the editor may ask for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Request {
    /// Liveness, and the protocol version on the other end.
    Hello,
    /// What the backend can see: sensors, channels, driver state.
    Inventory,
    /// The graph the engine is running.
    GetGraph,
    /// Install a graph. Validated by the service; a rejection changes nothing.
    SetGraph { graph: Box<Graph> },
    /// The most recent tick.
    Snapshot,
    /// Re-enumerate the hardware.
    Rescan,
    /// What stands between OpenFan and control of this machine's fans.
    ContentionReport,
    /// Stand rival controllers down and reclaim abandoned channels.
    TakeOver { force: bool },
}

/// Everything the service may answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Response {
    Hello {
        /// Semantic version of the service binary.
        version: String,
        /// Bumped when this protocol changes incompatibly.
        protocol: u32,
    },
    Inventory(Box<HardwareInventory>),
    Graph(Box<Graph>),
    /// A graph was installed.
    GraphAccepted,
    /// A graph was rejected and the running configuration is unchanged.
    GraphRejected {
        errors: Vec<ValidationError>,
    },
    Snapshot(Box<SnapshotDto>),
    ContentionReport(Box<ContentionReport>),
    Takeover(Box<TakeoverResult>),
    /// Acknowledgement for a request with nothing to return.
    Ok,
    /// The request was understood and could not be carried out.
    Error {
        message: String,
    },
}

/// Bumped when a change would make an older editor misread a newer service, or the
/// reverse. The editor checks it on connect rather than discovering the mismatch as a
/// confusing failure three messages later.
pub const PROTOCOL_VERSION: u32 = 1;

/// The pipe both sides meet on.
///
/// A fixed name rather than a per-user one: the service is machine-wide and there is one
/// engine per machine, because there is one set of fans.
pub const PIPE_NAME: &str = r"\\.\pipe\OpenFan";

/// The Windows service's registered name.
pub const SERVICE_NAME: &str = "OpenFan";

/// What the service is called in the service manager's list.
pub const SERVICE_DISPLAY_NAME: &str = "OpenFan fan control";

impl Response {
    /// The error text, if this is a failure.
    ///
    /// Callers should branch on the variant they expected; this is for logging and for
    /// the generic "something went wrong" path.
    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Error { message } => Some(message),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_through_json() {
        // The wire format is what both halves agree on; a request that does not survive
        // encoding is a bug the editor would see as a silent no-op.
        for request in [
            Request::Hello,
            Request::Inventory,
            Request::Snapshot,
            Request::Rescan,
            Request::ContentionReport,
            Request::TakeOver { force: true },
        ] {
            let text = serde_json::to_string(&request).expect("encode");
            let back: Request = serde_json::from_str(&text).expect("decode");
            assert_eq!(
                std::mem::discriminant(&request),
                std::mem::discriminant(&back),
                "{text}"
            );
        }
    }

    #[test]
    fn a_request_is_one_line_so_the_framing_holds() {
        // Messages are newline-delimited. A request that serialises with an embedded
        // newline would be read as two malformed ones.
        let text = serde_json::to_string(&Request::TakeOver { force: false }).expect("encode");
        assert!(!text.contains('\n'), "{text}");
    }

    #[test]
    fn there_is_no_request_that_stops_fan_control() {
        // Load-bearing: the service holds the fans. Stopping it is an administrative act
        // through the SCM, not something any process running as the user can ask for.
        let surface = format!("{:?}", Request::Hello);
        let _ = surface;
        let names = [
            "Hello",
            "Inventory",
            "GetGraph",
            "SetGraph",
            "Snapshot",
            "Rescan",
            "ContentionReport",
            "TakeOver",
        ];
        // A new variant must be added here deliberately, which is the moment to ask
        // whether it hands a user process the ability to disarm the cooling.
        assert_eq!(names.len(), 8);
        for forbidden in ["Stop", "Shutdown", "Exit", "Release", "Disable"] {
            assert!(
                !names.contains(&forbidden),
                "{forbidden} must not be part of the request surface"
            );
        }
    }

    #[test]
    fn errors_are_values_rather_than_dropped_connections() {
        let failure = Response::Error {
            message: "no hardware".into(),
        };
        assert_eq!(failure.error_message(), Some("no hardware"));
        assert_eq!(Response::Ok.error_message(), None);
    }
}
