//! featherbittorrent-desktop: the web UI in a native window, with This computer's engine behind it.
//!
//! This file is wiring only: find the app's directories, start This computer ([`local`]), hand the window
//! the commands it calls ([`commands`]), and stop the engine when the app exits. Every decision with logic
//! in it lives in those modules or in `fbtt-desktop-core`, where it is tested without a webview.

#![forbid(unsafe_code)]
// A release build on Windows is a GUI program: no console window opens behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod lifecycle;
mod local;
mod tray;

use fbtt_desktop_core::{DesktopPaths, DesktopSettings};
use tauri::{Manager, RunEvent, WindowEvent};

use crate::commands::DesktopState;

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
    let settings = DesktopSettings::load(&paths.settings_file()).unwrap_or_else(|problem| {
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
        .manage(lifecycle::Lifecycle::new(paths.settings_file()))
        .setup(|app| {
            let tray_available = tray::install(app.handle());
            app.state::<lifecycle::Lifecycle>()
                .set_tray_available(tray_available);
            lifecycle::apply_initial_state(
                app.handle(),
                lifecycle::initial_window_state(std::env::args(), tray_available),
            );
            lifecycle::quit_on_signals(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                lifecycle::close_window(window.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::desktop_info,
            commands::connect,
            lifecycle::set_close_action,
            lifecycle::close_window_now,
            lifecycle::tray_available,
            lifecycle::set_start_at_login,
            tray::set_tray_instance
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|problem| {
            refuse(
                &format!("The window could not open: {problem}"),
                EXIT_PROBLEM,
            )
        });

    app.run(|handle, event| match event {
        // A quit the system asks for while This computer still runs goes through the app's own quit, so
        // the final save completes before the exit it ends in (that exit finds the engine gone).
        RunEvent::ExitRequested { api, .. } if handle.state::<DesktopState>().is_running() => {
            api.prevent_exit();
            lifecycle::quit(handle);
        }
        // The Dock icon clicked while the window is hidden in the tray.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => lifecycle::show_window(handle),
        // The last stop on every path: Cmd+Q, logging out and shutting down on macOS arrive here with
        // nothing before them.
        RunEvent::Exit => {
            if let Some(this_computer) = handle.state::<DesktopState>().take() {
                tauri::async_runtime::block_on(this_computer.stop());
            }
        }
        _ => {}
    });
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
