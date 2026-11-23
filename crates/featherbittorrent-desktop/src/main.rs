//! featherbittorrent-desktop: the web UI in a native window, with This computer's engine behind it.
//!
//! This file is wiring only: find the app's directories, start This computer ([`local`]), hand the window
//! the commands it calls ([`commands`]), and stop the engine when the app exits. Every decision with logic
//! in it lives in those modules or in `fbtt-desktop-core`, where it is tested without a webview.

#![forbid(unsafe_code)]
// A release build on Windows is a GUI program: no console window opens behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod instances;
mod keychain;
mod lifecycle;
mod local;
mod preferences;
mod serving;
mod settings_store;
mod tray;
mod window;

use fbtt_desktop_core::{DesktopPaths, DesktopSettings};
use tauri::{Manager, WindowEvent};

use crate::commands::{DesktopState, Platform};
use crate::settings_store::SettingsStore;

/// The exit code for anything else that stops the app before its window opens.
const EXIT_PROBLEM: i32 = 1;

fn main() {
    // Answered before anything else exists, so a package smoke can ask without a display.
    if std::env::args()
        .skip(1)
        .any(|arg| arg == "--version" || arg == "-V")
    {
        println!("featherbittorrent-desktop {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let paths = DesktopPaths::resolve()
        .unwrap_or_else(|problem| refuse(&local::paths_problem(&problem), EXIT_PROBLEM));
    // A settings file that cannot be read is left as it is, and this run uses the defaults: nothing is
    // served, which is the safe reading of a switch nobody can see.
    // Every part of the shell that reads or saves `desktop.yaml` does it through this one store and its lock.
    let settings_store = SettingsStore::new(paths.settings_file());
    let settings = settings_store.read().unwrap_or_else(|problem| {
        eprintln!("{problem}");
        DesktopSettings::default()
    });

    let this_computer =
        tauri::async_runtime::block_on(local::start(&paths, settings.serve.enabled))
            .unwrap_or_else(|problem| {
                let code = if problem == local::ALREADY_RUNNING {
                    local::EXIT_ALREADY_RUNNING
                } else {
                    EXIT_PROBLEM
                };
                refuse(&problem, code)
            });

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![lifecycle::BACKGROUND_FLAG]),
        ))
        .manage(DesktopState::new(this_computer))
        // Read by the Desktop section's commands (`preferences`): shortcuts, closing, start at login.
        .manage(settings_store.clone())
        .manage(serving::Serving::new(paths.clone(), settings_store.clone()))
        .manage(window::ChromeState::new(settings_store.clone()))
        .manage(instances::Instances::new(
            settings_store.clone(),
            std::sync::Arc::new(keychain::SystemKeychain::probe()),
            local::desktop_origin(),
        ))
        .manage(lifecycle::Lifecycle::new(settings_store))
        // The app's one setup. `Builder::setup` replaces rather than chains, so no second `.setup` may
        // be added anywhere: a new step goes into `setup` below, in its place in the order.
        .setup(setup)
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                lifecycle::close_window(window.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::desktop_info,
            commands::connect,
            window::system_title_bar,
            window::set_system_title_bar,
            instances::list_instances,
            instances::add_instance,
            instances::remove_instance,
            instances::set_default_instance,
            instances::save_password,
            instances::forget_password,
            instances::probe_instance,
            lifecycle::set_close_action,
            lifecycle::close_window_now,
            lifecycle::tray_available,
            lifecycle::set_start_at_login,
            tray::set_tray_instance,
            preferences::desktop_preferences,
            preferences::get_shortcuts,
            preferences::set_shortcuts,
            serving::serving_status,
            serving::set_serving
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|problem| {
            refuse(
                &format!("The window could not open: {problem}"),
                EXIT_PROBLEM,
            )
        });

    // Exit requests, the last event (where Cmd+Q, logging out and shutting down arrive on macOS) and the
    // Dock icon: every way out waits for This computer's final save.
    app.run(lifecycle::handle_run_event);
}

/// Everything that needs the app built and its window made, in a fixed order: the window's chrome, then
/// the tray, then how the window starts (which depends on whether there is a tray), then the quit signals.
/// This is the only setup function; `Builder::setup` keeps one.
fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle();
    // 1. The window's chrome, before the window is shown. The config's windows exist by now (Tauri makes
    //    them before it calls the setup); a missing one is said once and the app goes on without it.
    match handle.get_webview_window(lifecycle::MAIN_WINDOW) {
        Some(main) => window::apply_chrome(
            &main,
            Platform::current(),
            app.state::<window::ChromeState>().saved(),
        ),
        None => eprintln!("The main window was not there to put its title bar on."),
    }
    // 2. The tray, and whether this system shows one.
    let tray_available = tray::install(handle);
    app.state::<lifecycle::Lifecycle>()
        .set_tray_available(tray_available);
    // 3. Show, hide or minimize the window (it starts invisible, `tauri.conf.json`).
    lifecycle::apply_initial_state(
        handle,
        lifecycle::initial_window_state(std::env::args(), tray_available),
    );
    // 4. A quit by signal runs the final save too.
    lifecycle::quit_on_signals(handle);
    Ok(())
}

/// Says `message` in a native message box (and on stderr, for a run from a terminal), then exits with
/// `code`. Used only before the window exists, when there is no page to say it in.
fn refuse(message: &str, code: i32) -> ! {
    eprintln!("{message}");
    rfd::MessageDialog::new()
        .set_title("featherbittorrent")
        .set_description(message)
        .set_level(rfd::MessageLevel::Error)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
    std::process::exit(code)
}
