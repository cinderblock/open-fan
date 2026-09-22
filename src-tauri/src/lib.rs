//! The OpenFan desktop shell.
//!
//! This layer owns the tray icon, the window lifecycle and the bridge to the control
//! engine. It owns **no control decisions**. Closing the window hides it rather than
//! quitting, the engine keeps ticking, and the fans keep being managed whether or not a
//! webview exists. That separation is the reason the engine lives in `of-engine` as a
//! plain library: it can be hosted here today and by a Windows service later without the
//! safety-critical code moving.
//!
//! # Status
//!
//! Phase 1 scaffold: tray, single-instance, autostart and hide-on-close are wired.
//! Starting the engine's tick loop and streaming its state to the UI is Phase 2.

pub mod commands;
pub mod state;
mod takeover;

use state::AppState;
use tauri::{
    AppHandle, Manager, WindowEvent,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
};

/// Bring the editor window back, creating nothing — the window always exists, it is only
/// ever hidden.
fn show_editor(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open OpenFan", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit OpenFan", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &separator, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(
            app.default_window_icon()
                .expect("bundled window icon")
                .clone(),
        )
        .tooltip("OpenFan")
        .menu(&menu)
        // Left click opens the editor; the menu is reserved for right click, which is
        // what Windows users expect from a tray icon.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_editor(app),
            "quit" => {
                // Phase 4 hooks the dying breath in here: every controlled channel must
                // be handed back to the firmware before the process goes away. Quitting
                // is the one exit path we fully control, so it must be the cleanest.
                tracing::info!("quit requested from tray");
                // Dropping the state joins the control thread, which hands every
                // channel back before the process goes away. Phase 4 extends this to
                // the Windows shutdown and logoff paths.
                app.exit(0);
            }
            other => tracing::warn!(id = other, "unhandled tray menu item"),
        })
        .build(app)?;

    Ok(())
}

/// Reach the terminal that launched us.
///
/// Release builds are a windows-subsystem binary so they have no console, and anything
/// `--diagnose` printed would go nowhere. Attaching to the parent's console makes the
/// diagnostic usable from a shell in a shipped build, which is the only build a person
/// reporting a problem will have.
#[cfg(windows)]
fn attach_parent_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

    // SAFETY: no pointers; failing simply means there was no parent console to attach to,
    // which is the ordinary case when launched from Explorer.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(windows))]
fn attach_parent_console() {}

/// Print what the takeover panel would show, then exit.
///
/// Runs the *same* `survey` the panel calls, so a support report and the interface cannot
/// disagree about what the machine looks like. Writes nothing to hardware.
fn diagnose() {
    attach_parent_console();

    // Built as text first, then written to a file *and* printed. A shipped build is a
    // windows-subsystem binary: attaching to a parent console makes printing work from a
    // shell, but nothing reaches a redirect, and the person filing a report is on the
    // shipped build. A file they can attach is the thing that actually survives.
    let mut out = String::new();
    let mut say = |line: String| {
        println!("{line}");
        out.push_str(&line);
        out.push('\n');
    };

    let state = AppState::new();
    let inventory = state.engine.inventory();
    let report = crate::takeover::survey(&state);

    say(format!("OpenFan {} diagnostic", env!("CARGO_PKG_VERSION")));
    say(format!("backend: {}", inventory.backend));

    say("\nchannels:".to_owned());
    for channel in &report.channels {
        say(format!("  {:<22} {:?}", channel.label, channel.control));
    }

    say("\nother software:".to_owned());
    if report.apps.is_empty() {
        say("  (none recognised)".to_owned());
    }
    for app in &report.apps {
        say(format!(
            "  {:<22} {:<26} pid {:<7} {:<11} {}",
            app.name,
            app.process_name,
            app.pid,
            app.role,
            if app.must_stop {
                "must stand down"
            } else {
                "can stay"
            }
        ));
    }

    say(format!(
        "\nstranded (manual, nobody driving): {:?}",
        report.stranded
    ));
    say(format!("clear: {}", report.clear));

    match diagnostic_path() {
        Some(path) => {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            match std::fs::write(&path, &out) {
                Ok(()) => println!("\nwritten to {}", path.display()),
                Err(e) => println!("\ncould not write {}: {e}", path.display()),
            }
        }
        None => println!("\n(no writable location for a diagnostic file)"),
    }
}

/// Where a diagnostic report is written, so support instructions can name one path.
///
/// An explicit second argument wins, for anyone who wants it somewhere specific.
fn diagnostic_path() -> Option<std::path::PathBuf> {
    let mut args = std::env::args().skip_while(|a| a != "--diagnose");
    args.next();
    if let Some(explicit) = args.next().filter(|a| !a.starts_with("--")) {
        return Some(std::path::PathBuf::from(explicit));
    }

    directories::ProjectDirs::from("", "", "OpenFan")
        .map(|dirs| dirs.data_local_dir().join("diagnostic.txt"))
}

pub fn run() {
    if std::env::args().any(|arg| arg == "--diagnose") {
        diagnose();
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        // A second copy of OpenFan fighting the first one for the same PWM registers is
        // a genuine hazard, not just an annoyance. Refuse to start twice.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tracing::info!("second instance launched; focusing the existing window");
            show_editor(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .setup(|app| {
            build_tray(app.handle())?;

            // The control loop starts here, before the window is shown, and keeps
            // running whatever happens to the webview afterwards.
            app.manage(AppState::new());

            // Launched by autostart: come up in the tray without stealing focus.
            if std::env::args().any(|arg| arg == "--minimized")
                && let Some(window) = app.get_webview_window("main")
            {
                let _ = window.hide();
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Hide rather than quit. The engine is the application; the window is a
                // view of it, and closing a view must never stop the fans.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::hardware_status,
            commands::node_catalogue,
            commands::inventory,
            commands::get_graph,
            commands::set_graph,
            commands::rescan,
            commands::resolve_types,
            commands::snapshot,
            takeover::contention_report,
            takeover::take_over,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start OpenFan");
}
