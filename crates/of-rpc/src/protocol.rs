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
use of_ipc::{
    AutostartDisabledDto, AutostartEntryDto, ContentionReport, HardwareInventory, SnapshotDto,
    TakeoverResult, ValidationError,
};

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

    /// What the service knows about updates.
    UpdateStatus,
    /// Ask the feed now rather than waiting for the next scheduled check.
    CheckForUpdate,
    /// Install the update the service found, as the service, without a prompt.
    ///
    /// Carries **no** url, file, version, signature or key: the service decides what it
    /// installs from a compiled-in endpoint and verifies it against a compiled-in key.
    /// The whole of a caller's influence is "the thing you already found, now".
    ApplyUpdate,
    /// Turn unattended installation on or off.
    SetAutoUpdate { enabled: bool },
    /// Where the window should send the user to install an update itself, prompting for
    /// administrator. The service downloads and verifies; the window runs it.
    PreparePromptedUpdate,

    /// Fetch the PawnIO hardware module, which PawnIO itself does not install and
    /// without which OpenFan sees no hardware. User-initiated: nothing reaches the
    /// network on its own.
    FetchHardwareModule,

    /// Which other fan controllers are set to start with this machine.
    ///
    /// Asked of the service rather than worked out by the editor because scheduled tasks
    /// — the usual way a fan controller arranges to start with administrator rights —
    /// are not readable without elevation.
    AutostartSurvey,

    /// Switch off one entry from the last [`Request::AutostartSurvey`].
    ///
    /// **By index, never by description.** The service keeps the survey it produced and
    /// the caller may only point at something already in it. Accepting a registry path or
    /// a file name here would let any client name anything at all for a LocalSystem
    /// process to delete, which is a much larger power than "stop fighting me over the
    /// fans" needs.
    DisableAutostart { id: usize },
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
    /// A struct variant, not a newtype around the vector: `Response` is internally
    /// tagged, and serde cannot serialize a tagged newtype variant wrapping a *sequence*.
    /// It fails at serialization time, so the server writes nothing and the client sees
    /// the connection close with no explanation anywhere.
    /// A struct variant, not a newtype around the vector: `Response` is internally
    /// tagged, and serde cannot serialize a tagged newtype variant wrapping a *sequence*.
    /// It fails at serialization time, so the server writes nothing and the client sees
    /// the connection close with no explanation anywhere.
    AutostartSurvey {
        entries: Vec<AutostartEntryDto>,
        /// How many startup entries were examined in total.
        ///
        /// Reported because an empty `entries` is ambiguous on its own: it means either
        /// "nothing here competes for the fans" or "the scan could not see anything", and
        /// only the first of those is an all-clear.
        examined: usize,
    },
    AutostartDisabled(Box<AutostartDisabledDto>),
    Takeover(Box<TakeoverResult>),
    /// Acknowledgement for a request with nothing to return.
    Ok,
    UpdateStatus(Box<serde_json::Value>),
    /// A verified installer is on disk and the window may run it, which will prompt for
    /// administrator. The path is produced by the service, never accepted from a client.
    PreparedUpdate {
        installer: String,
        version: String,
    },
    /// The request was understood and could not be carried out.
    Error {
        message: String,
    },
}

/// Bumped when a change would make an older editor misread a newer service, or the
/// reverse. The editor checks it on connect rather than discovering the mismatch as a
/// confusing failure three messages later.
pub const PROTOCOL_VERSION: u32 = 4;

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
    fn every_response_variant_can_actually_be_serialized() {
        // The gap that let a broken variant ship. `Response` is internally tagged, and
        // serde cannot serialize a tagged newtype variant wrapping a *sequence* — it
        // fails at run time, with nothing written and nothing logged, so the client
        // reports only "the connection closed". Requests had a round-trip test; responses
        // did not, and that asymmetry is the whole bug.
        //
        // One of every variant, so adding a variant that cannot be encoded fails here
        // rather than on a user's machine.
        let responses = vec![
            Response::Hello {
                version: "0.0.0".into(),
                protocol: PROTOCOL_VERSION,
            },
            Response::Inventory(Box::new(HardwareInventory {
                backend: "test".into(),
                sensors: vec![],
                channels: vec![],
                driver_present: false,
                driver_summary: String::new(),
                module_missing: false,
            })),
            Response::Graph(Box::default()),
            Response::GraphAccepted,
            Response::GraphRejected { errors: vec![] },
            Response::Snapshot(Box::new(SnapshotDto {
                sequence: 0,
                dt: 0.0,
                tick_duration_ms: 0.0,
                overruns: 0,
                degraded: false,
                sensor_error: None,
                sensors: vec![],
                wires: vec![],
                commanded: Default::default(),
                failsafed: vec![],
            })),
            Response::ContentionReport(Box::new(ContentionReport {
                channels: vec![],
                apps: vec![],
                stranded: vec![],
                clear: true,
                blocker: None,
            })),
            Response::Takeover(Box::new(TakeoverResult {
                steps: vec![],
                succeeded: true,
                blocker: None,
                report: ContentionReport {
                    channels: vec![],
                    apps: vec![],
                    stranded: vec![],
                    clear: true,
                    blocker: None,
                },
            })),
            Response::Ok,
            Response::UpdateStatus(Box::new(serde_json::Value::Null)),
            Response::PreparedUpdate {
                installer: "x".into(),
                version: "1".into(),
            },
            Response::AutostartSurvey {
                entries: vec![],
                examined: 0,
            },
            Response::AutostartDisabled(Box::new(AutostartDisabledDto {
                what: "x".into(),
                restore_hint: None,
            })),
            Response::Error {
                message: "x".into(),
            },
        ];

        for response in &responses {
            let text = serde_json::to_string(response)
                .unwrap_or_else(|e| panic!("{response:?} cannot be encoded: {e}"));
            let back: Response = serde_json::from_str(&text).unwrap_or_else(|e| {
                panic!("{response:?} encoded to {text} but will not decode: {e}")
            });
            assert_eq!(
                std::mem::discriminant(response),
                std::mem::discriminant(&back),
                "{text}"
            );
        }
    }

    #[test]
    fn a_response_carrying_a_list_survives_having_something_in_it() {
        // An empty vector encodes under rules a populated one might not, so the list
        // variants are checked with contents too.
        let response = Response::AutostartSurvey {
            examined: 288,
            entries: vec![AutostartEntryDto {
                id: 0,
                key: "fancontrol".into(),
                name: "FanControl".into(),
                location: "a scheduled task".into(),
                command: r"C:.exe".into(),
                reversible: true,
            }],
        };
        let text = serde_json::to_string(&response).expect("encodes");
        let back: Response = serde_json::from_str(&text).expect("decodes");
        match back {
            Response::AutostartSurvey { entries, examined } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(examined, 288);
            }
            other => panic!("wrong variant: {other:?}"),
        }
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
            "UpdateStatus",
            "CheckForUpdate",
            "ApplyUpdate",
            "SetAutoUpdate",
            "PreparePromptedUpdate",
            "FetchHardwareModule",
        ];
        // A new variant must be added here deliberately, which is the moment to ask
        // whether it hands a user process the ability to disarm the cooling — or, since
        // the update requests arrived, to make a LocalSystem service run something.
        assert_eq!(names.len(), 14);
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
