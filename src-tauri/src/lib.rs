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

pub fn run() {
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
        ])
        .run(tauri::generate_context!())
        .expect("failed to start OpenFan");
}
