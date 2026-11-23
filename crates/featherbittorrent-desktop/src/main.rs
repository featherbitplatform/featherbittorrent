//! featherbittorrent-desktop: the web UI in a native window, with This computer's engine behind it.
//!
//! This file is wiring only: find the app's directories, start This computer ([`local`]), hand the window
//! the commands it calls ([`commands`]), and stop the engine when the app exits. Every decision with logic
//! in it lives in those modules or in `fbtt-desktop-core`, where it is tested without a webview.

#![forbid(unsafe_code)]
// A release build on Windows is a GUI program: no console window opens behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod local;
mod window;

use fbtt_desktop_core::{DesktopPaths, DesktopSettings};
use tauri::{Manager, RunEvent};

use crate::commands::{DesktopState, Platform};

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
        .manage(DesktopState::new(this_computer))
        .manage(window::ChromeState::new(
            paths.settings_file(),
            settings.system_title_bar,
        ))
        .invoke_handler(tauri::generate_handler![
            commands::desktop_info,
            commands::connect,
            window::system_title_bar,
            window::set_system_title_bar
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|problem| {
            refuse(
                &format!("The window could not open: {problem}"),
                EXIT_PROBLEM,
            )
        });

    // The title bar goes on the window before the event loop first draws it. This belongs as the first
    // step of the app's single setup function once there is one: `Builder::setup` replaces, never chains.
    if let Some(main) = app.get_webview_window("main") {
        window::apply_chrome(&main, Platform::current(), settings.system_title_bar);
    }

    app.run(|handle, event| {
        if let RunEvent::Exit = event
            && let Some(this_computer) = handle.state::<DesktopState>().take()
        {
            tauri::async_runtime::block_on(this_computer.stop());
        }
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
