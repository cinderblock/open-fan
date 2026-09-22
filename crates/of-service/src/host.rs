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
    /// The release the last check found, if it was newer than us and installable.
    ///
    /// Held so that "install it" refers to something *we* decided, rather than to
    /// anything a caller could name.
    update: std::sync::Mutex<Option<crate::update::Release>>,
}

/// This build's version, compared against whatever the feed offers.
const VERSION: &str = env!("CARGO_PKG_VERSION");

impl Host {
    pub fn start() -> Self {
        let backend = select_backend();
        tracing::info!(backend = %backend.name(), "starting control loop");

        let engine = Engine::new(backend, EngineConfig::default());
        Self {
            engine: EngineHandle::spawn(engine),
            update: std::sync::Mutex::new(None),
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

            Request::UpdateStatus => self.update_status(None),

            Request::CheckForUpdate => match crate::update::check(VERSION) {
                Ok((found, rejected)) => {
                    *self.update.lock().unwrap_or_else(|e| e.into_inner()) = found;
                    self.update_status(rejected.map(|r| r.to_string()))
                }
                Err(e) => self.update_error(e),
            },

            // The whole of a caller's influence over what gets installed: "the thing you
            // already found". No url, file, version, signature or key crosses the pipe.
            Request::ApplyUpdate => match self.apply_found_update(Install::Silent) {
                Ok(response) => response,
                Err(e) => self.update_error(e),
            },

            Request::PreparePromptedUpdate => match self.apply_found_update(Install::Prompted) {
                Ok(response) => response,
                Err(e) => self.update_error(e),
            },

            Request::SetAutoUpdate { enabled } => {
                let mut settings = crate::update::load_settings();
                settings.automatic = enabled;
                match crate::update::save_settings(&settings) {
                    Ok(()) => {
                        tracing::warn!(enabled, "unattended update installation changed");
                        self.update_status(None)
                    }
                    Err(e) => self.update_error(e),
                }
            }
        }
    }

    /// Download and verify whatever the last check found, then either run it ourselves or
    /// hand the path back for the window to run with a prompt.
    fn apply_found_update(&self, how: Install) -> anyhow::Result<Response> {
        let release = self
            .update
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no update has been found; check first"))?;

        let installer = crate::update::download_verified(&release)?;

        match how {
            Install::Silent => {
                crate::update::install_silently(&installer)?;
                Ok(Response::Ok)
            }
            Install::Prompted => Ok(Response::PreparedUpdate {
                installer: installer.display().to_string(),
                version: release.version,
            }),
        }
    }

    fn update_status(&self, rejected: Option<String>) -> Response {
        let found = self
            .update
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let settings = crate::update::load_settings();

        let status = crate::update::UpdateStatus {
            current_version: VERSION.to_owned(),
            available: found.as_ref().map(|r| r.version.clone()),
            notes: found.as_ref().and_then(|r| r.notes.clone()),
            rejected,
            automatic: settings.automatic,
            verifiable: crate::update::verifiable(),
            error: None,
        };

        match serde_json::to_value(status) {
            Ok(value) => Response::UpdateStatus(Box::new(value)),
            Err(e) => Response::Error {
                message: e.to_string(),
            },
        }
    }

    fn update_error(&self, error: impl std::fmt::Display) -> Response {
        // Reported as a status rather than a bare error so the interface keeps the rest
        // of what it knows — the version, the setting — instead of blanking.
        let settings = crate::update::load_settings();
        let status = crate::update::UpdateStatus {
            current_version: VERSION.to_owned(),
            available: None,
            notes: None,
            rejected: None,
            automatic: settings.automatic,
            verifiable: crate::update::verifiable(),
            error: Some(error.to_string()),
        };

        match serde_json::to_value(status) {
            Ok(value) => Response::UpdateStatus(Box::new(value)),
            Err(e) => Response::Error {
                message: e.to_string(),
            },
        }
    }
}

/// Which of the two update paths to take.
#[derive(Debug, Clone, Copy)]
enum Install {
    /// The service runs the installer. No prompt; requires the user to have opted in.
    Silent,
    /// The window runs the installer. One UAC prompt.
    Prompted,
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

/// Look for updates on a schedule, and install them when the user has opted in.
///
/// Runs on its own thread. The control loop must never wait on a network call, and a feed
/// that hangs must cost a tick nothing — which is why this is a separate thread rather
/// than work folded into the engine.
///
/// **It only installs when `automatic` is on.** With it off this still checks, so the
/// interface can offer an update, but applying one stays a decision somebody made.
pub fn watch_for_updates(host: Arc<Host>) {
    // A first check shortly after start rather than immediately: the machine may still be
    // finding its network, and the fans matter more than the version number.
    const SETTLE: std::time::Duration = std::time::Duration::from_secs(120);
    std::thread::sleep(SETTLE);

    loop {
        let settings = crate::update::load_settings();

        match crate::update::check(VERSION) {
            Ok((Some(release), _)) => {
                tracing::info!(version = %release.version, "an update is available");
                *host.update.lock().unwrap_or_else(|e| e.into_inner()) = Some(release);

                if settings.automatic {
                    match host.apply_found_update(Install::Silent) {
                        Ok(_) => tracing::warn!("silent update started; the service will restart"),
                        Err(e) => tracing::error!(error = %e, "silent update failed"),
                    }
                }
            }
            Ok((None, Some(rejection))) => {
                tracing::debug!(reason = %rejection, "nothing to install");
            }
            Ok((None, None)) => {}
            // A check that fails is ordinary — a laptop on a train, a blocked endpoint.
            // Log it and try again later rather than making noise about it.
            Err(e) => tracing::debug!(error = %e, "update check failed"),
        }

        std::thread::sleep(settings.interval());
    }
}

/// Serve the editor until the process ends. Blocks.
pub fn serve(host: Arc<Host>) -> of_rpc::pipe::Result<()> {
    of_rpc::pipe::serve(move |request| host.handle(request))
}
