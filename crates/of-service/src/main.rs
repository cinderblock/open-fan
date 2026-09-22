//! The OpenFan service executable.
//!
//! Three ways to run, all the same host underneath:
//!
//! | Invocation | What happens |
//! | --- | --- |
//! | (none) | run under the service control manager |
//! | `--console` | run in the foreground, for development and debugging |
//! | `--install` / `--uninstall` | register or remove the service (needs administrator) |
//!
//! # Shutting down is a safety operation
//!
//! When the service is asked to stop, every channel it holds has to be handed back before
//! it reports STOPPED. That is what `Host`'s drop does — `EngineHandle`'s `Drop` joins the
//! control thread, whose own `Drop` applies the dying breath — so the stop handler's job
//! is simply to let the host drop *before* reporting the final status, and to ask the
//! system for enough time to do it.
//!
//! `PRESHUTDOWN` is registered as well as `SHUTDOWN` for exactly that reason: a plain
//! shutdown notification gives a service a few seconds, while preshutdown is delivered
//! earlier and with a configurable budget. Handing fans back to the firmware is fast, but
//! being rushed is not a risk worth taking with the thing that stops the machine cooking.

use std::sync::Arc;

use anyhow::Context as _;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    if has("--install") {
        return platform::install();
    }
    if has("--uninstall") {
        return platform::uninstall();
    }
    if has("--console") {
        init_logging();
        return run_console();
    }

    platform::run_as_service()
}

/// Logs go to a file as well as stdout: a service has no console, and the first question
/// about a service that is not behaving is always "what does its log say".
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    // A file first: a service has no console, and the first question about one that is
    // misbehaving is always "what does its log say". Falling through to stdout keeps
    // `--console` useful when the file cannot be opened.
    if let Some(path) = log_path() {
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .try_init();
            return;
        }
    }

    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

/// Where the service writes its log.
///
/// Machine-wide rather than per-user: the service runs as LocalSystem and may be
/// controlling fans before anybody has logged in.
fn log_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("ProgramData").map(std::path::PathBuf::from)?;
    Some(base.join("OpenFan").join("service.log"))
}

/// Run in the foreground until interrupted.
fn run_console() -> anyhow::Result<()> {
    tracing::info!("starting in console mode");
    let host = Arc::new(of_service::Host::start());
    of_service::serve(host).context("serving the editor pipe")?;
    Ok(())
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsString;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    use anyhow::Context as _;
    use of_rpc::{SERVICE_DISPLAY_NAME, SERVICE_NAME};
    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    windows_service::define_windows_service!(ffi_service_main, service_main);

    pub fn run_as_service() -> anyhow::Result<()> {
        windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .context("connecting to the service control manager")?;
        Ok(())
    }

    fn service_main(_arguments: Vec<OsString>) {
        super::init_logging();
        if let Err(e) = run() {
            tracing::error!(error = %e, "service failed");
        }
    }

    fn run() -> anyhow::Result<()> {
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

        let handler = move |control| match control {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            // All three mean the same thing to us: hand the fans back, then go.
            ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        };

        let status_handle = service_control_handler::register(SERVICE_NAME, handler)
            .context("registering the service control handler")?;

        let report = |state: ServiceState, wait: Duration| ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: state,
            // Preshutdown as well as shutdown: it is delivered earlier and with a
            // budget we can extend, and handing fans back should never be rushed.
            controls_accepted: ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::PRESHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: wait,
            process_id: None,
        };

        status_handle
            .set_service_status(report(ServiceState::StartPending, Duration::from_secs(10)))?;

        // The engine comes up before the pipe, so a client that connects the moment we
        // report RUNNING finds a working engine rather than a half-built one.
        let host = Arc::new(of_service::Host::start());

        let serving = Arc::clone(&host);
        std::thread::Builder::new()
            .name("openfan-pipe".into())
            .spawn(move || {
                if let Err(e) = of_service::serve(serving) {
                    tracing::error!(error = %e, "editor pipe stopped");
                }
            })
            .context("spawning the editor pipe")?;

        status_handle.set_service_status(report(ServiceState::Running, Duration::default()))?;
        tracing::info!("running");

        // Wait to be told to stop. Nothing else happens on this thread, which is why the
        // control loop lives on its own.
        let _ = shutdown_rx.recv();
        tracing::info!("stop requested; handing channels back");

        // Ask for time *before* dropping the host, because the drop is the part that
        // writes to the hardware.
        status_handle
            .set_service_status(report(ServiceState::StopPending, Duration::from_secs(30)))?;

        // Dropping the host joins the control thread, whose drop applies the dying breath
        // to every channel it holds. Reporting STOPPED before this would be a lie.
        drop(host);
        tracing::info!("channels handed back; stopping");

        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        Ok(())
    }

    pub fn install() -> anyhow::Result<()> {
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )
        .context("opening the service control manager (administrator required)")?;

        let binary = std::env::current_exe().context("locating this executable")?;

        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(SERVICE_DISPLAY_NAME),
            service_type: SERVICE_TYPE,
            // Automatic, because the point of a service is that the fans are managed
            // before anyone logs in.
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: binary,
            launch_arguments: vec![],
            dependencies: vec![],
            // LocalSystem: PawnIO refuses a handle to an unprivileged caller, and this is
            // the account that has one without a user being present.
            account_name: None,
            account_password: None,
        };

        let service = manager
            .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
            .context("creating the service")?;

        service
            .set_description(
                "Controls system fans from a node graph. Runs independently of the OpenFan \
                 window so cooling continues when it is closed.",
            )
            .ok();

        service.start(&[] as &[&std::ffi::OsStr]).ok();

        println!("Installed and started {SERVICE_DISPLAY_NAME}.");
        Ok(())
    }

    pub fn uninstall() -> anyhow::Result<()> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .context("opening the service control manager (administrator required)")?;

        let service = manager
            .open_service(
                SERVICE_NAME,
                ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
            )
            .context("opening the service")?;

        // Stop it first, so the dying breath runs and the fans go back to the board's own
        // curve. Deleting a running service would leave them wherever they were.
        let status = service.query_status().context("querying the service")?;
        if status.current_state != ServiceState::Stopped {
            service.stop().context("stopping the service")?;
            for _ in 0..60 {
                std::thread::sleep(Duration::from_millis(500));
                if service.query_status()?.current_state == ServiceState::Stopped {
                    break;
                }
            }
        }

        service.delete().context("deleting the service")?;
        println!("Removed {SERVICE_DISPLAY_NAME}.");
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn run_as_service() -> anyhow::Result<()> {
        anyhow::bail!("the OpenFan service is only implemented on Windows; use --console")
    }
    pub fn install() -> anyhow::Result<()> {
        anyhow::bail!("service installation is only implemented on Windows")
    }
    pub fn uninstall() -> anyhow::Result<()> {
        anyhow::bail!("service removal is only implemented on Windows")
    }
}
