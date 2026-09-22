//! The engine, and the request handler that speaks for it.
//!
//! Deliberately free of Windows service machinery, so the whole host can be run as an
//! ordinary console process and behave identically. `main.rs` decides *how* this is
//! hosted; this decides *what* it does.
//!
//! Translating engine state into wire types lives here rather than in a shared crate on
//! purpose: only the service ever does it. The editor receives DTOs and has no business
//! knowing what an `of_engine::Inventory` is.

use std::sync::Arc;

use of_engine::{Engine, EngineConfig, EngineHandle};
use of_hal::Backend;
use of_ipc::{ChannelDto, HardwareInventory, SensorDto, SnapshotDto, ValidationError, WireValue};
use of_rpc::{Request, Response};

use crate::takeover;

/// Everything the service owns.
///
/// The engine starts before the pipe does, so a client connecting the instant the service
/// reports RUNNING finds a working engine rather than a half-built one.
pub struct Host {
    pub engine: EngineHandle,
}

impl Host {
    pub fn start() -> Self {
        let backend = select_backend();
        tracing::info!(backend = %backend.name(), "starting control loop");

        let engine = Engine::new(backend, EngineConfig::default());
        Self {
            engine: EngineHandle::spawn(engine),
        }
    }

    /// Answer one request.
    ///
    /// Runs on a connection thread. Every arm translates between the engine and the wire
    /// types; **no control decision is made here**, for the same reason the old command
    /// layer made none: a client can vanish mid-request and nothing about cooling may
    /// depend on it.
    pub fn handle(&self, request: Request) -> Response {
        match request {
            Request::Hello => Response::Hello {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                protocol: of_rpc::PROTOCOL_VERSION,
            },

            Request::Inventory => Response::Inventory(Box::new(inventory_dto(&self.engine))),

            Request::GetGraph => Response::Graph(Box::new(self.engine.graph())),

            Request::SetGraph { graph } => match self.engine.set_graph(*graph) {
                Ok(()) => Response::GraphAccepted,
                Err(errors) => Response::GraphRejected {
                    errors: errors
                        .into_iter()
                        .map(|e| ValidationError {
                            message: e.to_string(),
                            node_id: None,
                        })
                        .collect(),
                },
            },

            Request::Snapshot => Response::Snapshot(Box::new(snapshot_dto(&self.engine))),

            Request::Rescan => {
                self.engine.rescan();
                Response::Ok
            }

            Request::ContentionReport => {
                Response::ContentionReport(Box::new(takeover::survey(&self.engine)))
            }

            Request::TakeOver { force } => {
                Response::Takeover(Box::new(takeover::take_over(&self.engine, force)))
            }
        }
    }
}

/// Whether the kernel driver OpenFan needs is present, and what to tell the user.
pub fn driver_summary() -> (bool, String) {
    #[cfg(windows)]
    {
        match of_hal_pawnio::library_version() {
            Ok((major, minor, patch)) => (
                true,
                format!("Hardware access is available (PawnIO {major}.{minor}.{patch})."),
            ),
            // The error types carry install guidance; surface it rather than a code.
            Err(err) => (false, err.to_string()),
        }
    }

    #[cfg(not(windows))]
    (
        false,
        "Hardware access on this platform is not implemented yet.".to_owned(),
    )
}

fn inventory_dto(engine: &EngineHandle) -> HardwareInventory {
    let inv = engine.inventory();
    let (driver_present, driver_summary) = driver_summary();

    HardwareInventory {
        backend: inv.backend,
        sensors: inv
            .sensors
            .into_iter()
            .map(|s| SensorDto {
                id: s.id,
                label: s.label,
                quantity: quantity_of(s.kind),
            })
            .collect(),
        channels: inv
            .channels
            .into_iter()
            .map(|c| ChannelDto {
                id: c.id,
                label: c.label,
                tachometer: c.tachometer,
                min_reliable_duty: c.min_reliable_duty,
            })
            .collect(),
        driver_present,
        driver_summary,
    }
}

fn snapshot_dto(engine: &EngineHandle) -> SnapshotDto {
    let snap = engine.snapshot();
    let report = snap.report;

    SnapshotDto {
        sequence: snap.sequence,
        dt: report.dt,
        tick_duration_ms: snap.tick_duration_ms,
        overruns: snap.overruns,
        degraded: report.degraded,
        sensor_error: report.sensors.error,
        sensors: report
            .sensors
            .values
            .iter()
            .map(|(id, value)| WireValue::sensor(id, value))
            .collect(),
        wires: report
            .wire_values
            .iter()
            .map(|(port, value)| WireValue::new(port, value))
            .collect(),
        commanded: report.applied.commanded,
        failsafed: report.applied.failsafed.into_keys().collect(),
    }
}

fn quantity_of(kind: of_hal::SensorKind) -> of_ipc::Quantity {
    use of_hal::SensorKind as K;
    use of_ipc::Quantity as Q;
    match kind {
        K::Temperature => Q::Temperature,
        K::Fan => Q::Rpm,
        K::Voltage => Q::Voltage,
        K::Current => Q::Current,
        K::Power => Q::Power,
        K::Load => Q::Load,
        _ => Q::Ratio,
    }
}

/// Pick the best backend available on this machine.
///
/// Falls back to the simulated thermal plant rather than refusing to start. A service
/// that runs and can explain why it found no hardware is more useful than one that fails
/// to start and leaves an event-log entry nobody reads.
fn select_backend() -> Box<dyn Backend> {
    if std::env::var("OPENFAN_MOCK").is_ok() {
        tracing::warn!("OPENFAN_MOCK set; using the simulated backend");
        return Box::new(of_hal_mock::MockBackend::default());
    }

    #[cfg(windows)]
    {
        use of_hal::Discovery as _;

        match of_hal_pawnio::SuperIoBackend::discover() {
            Ok(Some(backend)) => {
                tracing::info!(backend = %backend.name(), "found real hardware");
                return backend;
            }
            Ok(None) => tracing::info!("no supported hardware found; using the simulated backend"),
            Err(e) => {
                tracing::warn!(error = %e, "hardware present but unusable; using the simulated backend");
            }
        }
    }

    Box::new(of_hal_mock::MockBackend::default())
}

/// Serve the editor until the process ends. Blocks.
pub fn serve(host: Arc<Host>) -> of_rpc::pipe::Result<()> {
    of_rpc::pipe::serve(move |request| host.handle(request))
}
