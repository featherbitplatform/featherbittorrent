//! featherbittorrent-desktop: the web UI in a native window, with This computer's engine behind it.
//!
//! This file is wiring only: find the app's directories, start This computer ([`local`]), hand the window
//! the commands it calls ([`commands`]), and stop the engine when the app exits. Every decision with logic
//! in it lives in those modules or in `fbtt-desktop-core`, where it is tested without a webview.

#![forbid(unsafe_code)]
// A release build on Windows is a GUI program: no console window opens behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod deadline;
mod download;
mod instances;
mod keychain;
mod lifecycle;
mod local;
mod navigation;
mod opened;
mod player;
mod preferences;
mod serving;
mod settings_store;
mod startup;
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

    // This computer starts in the setup, after the single-instance plugin has had its say: a second
    // launch hands its arguments to the copy already running and exits before it would find the library
    // locked.
    let serve = settings.serve.enabled;

    let context = tauri::generate_context!();
    // The main window never leaves the app's own origin (`navigation`): the origins are read off the
    // context before the builder takes it.
    let own_origins = navigation::own_origins(context.config().build.dev_url.as_ref());

    let app = tauri::Builder::default()
        // First among the plugins, as its documentation requires: a second launch forwards what it was
        // opened with to this copy, which shows its window, and exits without starting anything.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            opened::on_second_launch(
                &app.state::<opened::OpenedState>(),
                argv,
                &cwd,
                &opened::AppPage(app),
            );
        }))
        .plugin(navigation::guard(own_origins))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![lifecycle::BACKGROUND_FLAG]),
        ))
        .manage(DesktopState::starting())
        .manage(opened::OpenedState::new())
        .manage(download::Downloads::new())
        // Read by the Desktop section's commands (`preferences`): shortcuts, closing, start at login.
        .manage(settings_store.clone())
        // The player's playlist folder, beside This computer's files. Made before `paths` moves into
        // Serving on the next line: the command and the sweep at app start share it (`player::Playlists`).
        .manage(player::Playlists::new(&paths.data_dir))
        .manage(serving::Serving::new(paths, settings_store.clone()))
        .manage(window::ChromeState::new(settings_store.clone()))
        .manage(instances::Instances::new(
            settings_store.clone(),
            std::sync::Arc::new(keychain::SystemKeychain::probe()),
            local::desktop_origin(),
        ))
        .manage(lifecycle::Lifecycle::new(settings_store))
        // The app's one setup. `Builder::setup` replaces rather than chains, so no second `.setup` may
        // be added anywhere: a new step goes into `setup` below, in its place in the order.
        .setup(move |app| setup(app, serve))
        // The app's one page-load hook (`Builder::on_page_load` replaces too): a page that starts loading
        // is not listening for opened links until it asks again.
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Started {
                webview
                    .app_handle()
                    .state::<opened::OpenedState>()
                    .page_gone();
            }
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
            download::choose_download_place,
            download::save_download,
            download::cancel_download,
            player::open_in_player,
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
            opened::take_opened,
            startup::startup_problem,
            startup::quit_app,
            preferences::desktop_preferences,
            preferences::get_shortcuts,
            preferences::set_shortcuts,
            serving::serving_status,
            serving::set_serving
        ])
        .build(context)
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

/// Everything that needs the app built and its window made, in a fixed order: This computer, the window's
/// chrome, then the tray, then how the window starts (which depends on whether there is a tray), then the
/// quit signals, then what this launch was opened with. This is the only setup function; `Builder::setup`
/// keeps one.
fn setup(app: &mut tauri::App, serve: bool) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle();
    // 0. This computer, which only the one copy that got past the single-instance plugin starts. It
    //    starts off the main thread, so the event loop keeps running (a second launch's forward is
    //    answered meanwhile); a failure is kept for the page, which says it with a Quit button. It starts
    //    through Serve the web UI's own start, so a saved switch that is on serves from the first start,
    //    and an engine that cannot serve falls back to the window alone with the line kept for Settings.
    let serving = app.state::<serving::Serving>().inner().clone();
    startup::spawn(handle, async move {
        serving
            .start_this_computer(serve, fbtt_core::Config::settings_before_proxy)
            .await
    });
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
    // 5. The links and files this launch was opened with, kept until the page asks, and on macOS the
    //    ones the system opens the running app with from now on.
    opened::install(handle, &app.state::<opened::OpenedState>());
    // 6. A playlist is written for one player and taken out when that player exits, so any playlist a
    //    previous run left belongs to one that never cleaned up. Sweeping it is off the event loop and
    //    best effort (`player::Playlists::sweep_stale`).
    app.state::<player::Playlists>().sweep_stale();
    Ok(())
}

/// Says `message` in a native message box (and on stderr, for a run from a terminal), then exits with
/// `code`. Used only before the event loop runs (no home directory, a window that could not be made), when
/// there is no page to say it in; never from inside the loop, where on Linux the box would wait on the
/// loop it blocks. A failed start of This computer is said by the page instead (`startup`).
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
