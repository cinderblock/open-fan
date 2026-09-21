//! Application state: the running control loop, and how the backend is chosen.

use of_engine::{Engine, EngineConfig, EngineHandle};
use of_hal::Backend;

/// State shared with every Tauri command.
///
/// The engine is started before the window exists and outlives it. Nothing here is
/// optional or lazily initialised, because "the fans are not being controlled yet" must
/// not be a reachable state once the app is up.
pub struct AppState {
    pub engine: EngineHandle,
}

impl AppState {
    pub fn new() -> Self {
        let backend = select_backend();
        tracing::info!(backend = %backend.name(), "starting control loop");

        let engine = Engine::new(backend, EngineConfig::default());
        Self {
            engine: EngineHandle::spawn(engine),
        }
    }
}

/// Pick the best backend available on this machine.
///
/// Falls back to the simulated thermal plant rather than failing to start. A running app
/// that can explain why it has no hardware is far more useful than one that will not
/// open — and the mock is what lets the project be developed on machines with no fans at
/// all, including the NUC this was written on.
///
/// `OPENFAN_MOCK=1` forces the simulated backend even where real hardware exists. That is
/// the safe way to exercise the UI on a machine you do not want to experiment on.
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
            // Nothing supported on this machine: no PawnIO, no module, or no chip we
            // recognise. An ordinary outcome, not a failure.
            Ok(None) => {
                tracing::info!("no supported hardware found; using the simulated backend");
            }
            // Worth a warning rather than a silent downgrade: this one the user can fix,
            // and it is otherwise indistinguishable from having no fans at all.
            Err(e) => {
                tracing::warn!(error = %e, "hardware present but unusable; using the simulated backend");
            }
        }
    }

    Box::new(of_hal_mock::MockBackend::default())
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
