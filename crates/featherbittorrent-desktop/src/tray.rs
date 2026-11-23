//! The tray: the app's icon while the window is hidden, with Show, the connected instance, and Quit.
//!
//! Whether there is a tray at all is what the tray builder says, and on Linux also whether anything will
//! show it: the builder succeeds on a desktop with no StatusNotifier host (stock GNOME without the
//! AppIndicator extension), where a window hidden to the tray could never come back. So on Linux the tray
//! counts only when [`STATUS_NOTIFIER_WATCHER`] has an owner on the session bus, asked once at startup.
//! Without a tray, closing the window quits and the tray answer is not offered
//! ([`crate::lifecycle::Lifecycle::set_tray_available`]).

use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

use crate::lifecycle;

/// The tray menu's first item.
pub const SHOW_LABEL: &str = "Show featherbittorrent";

/// The tray menu's last item.
pub const QUIT_LABEL: &str = "Quit";

/// The instance line before the page has connected anywhere: the app's own engine.
pub const DEFAULT_INSTANCE: &str = "This computer";

const SHOW_ID: &str = "show";
const INSTANCE_ID: &str = "instance";
const QUIT_ID: &str = "quit";

// Asked only on Linux; the other systems always have a tray.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
/// The session bus name a StatusNotifier host (the thing that draws tray icons on Linux) owns.
pub const STATUS_NOTIFIER_WATCHER: &str = "org.kde.StatusNotifierWatcher";

// Asked only on Linux; the other systems always have a tray.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
/// Whether the answer to "does [`STATUS_NOTIFIER_WATCHER`] have an owner" means a tray will show. Only a
/// clear yes does: no owner, no session bus, or a bus that could not be asked all mean no tray, and each
/// is said on stderr.
pub fn host_present(answer: Result<bool, String>) -> bool {
    match answer {
        Ok(true) => true,
        Ok(false) => {
            eprintln!("No tray host on this desktop ({STATUS_NOTIFIER_WATCHER} has no owner).");
            false
        }
        Err(problem) => {
            eprintln!(
                "No tray host on this desktop (the session bus could not be asked: {problem})."
            );
            false
        }
    }
}

/// Asks the session bus whether [`STATUS_NOTIFIER_WATCHER`] has an owner.
#[cfg(not(any(windows, target_os = "macos")))]
fn ask_session_bus() -> Result<bool, String> {
    let connection =
        zbus::blocking::Connection::session().map_err(|problem| problem.to_string())?;
    let bus =
        zbus::blocking::fdo::DBusProxy::new(&connection).map_err(|problem| problem.to_string())?;
    let name = zbus::names::BusName::try_from(STATUS_NOTIFIER_WATCHER)
        .map_err(|problem| problem.to_string())?;
    bus.name_has_owner(name)
        .map_err(|problem| problem.to_string())
}

/// Whether this system will show a tray icon. macOS and Windows always have a tray; Linux has one when a
/// StatusNotifier host runs.
fn system_has_tray_host() -> bool {
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        host_present(ask_session_bus())
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        true
    }
}

/// The instance line in the tray menu, when there is a tray.
pub struct TrayState<R: Runtime> {
    instance: Mutex<Option<MenuItem<R>>>,
}

impl<R: Runtime> TrayState<R> {
    fn new(instance: Option<MenuItem<R>>) -> Self {
        TrayState {
            instance: Mutex::new(instance),
        }
    }
}

/// The text of the instance line for `name`: display-safe, since a saved instance's name is whatever a
/// person typed, and This computer's own name for a blank one.
pub fn instance_line(name: &str) -> String {
    let name = fbt::display_safe(name.trim(), fbt::MAX_DISPLAY_CHARS);
    if name.trim().is_empty() {
        DEFAULT_INSTANCE.to_string()
    } else {
        name
    }
}

/// Builds the tray and manages its state. True when the system gave the app a tray; false (said on
/// stderr) when the builder refused, and the app runs without one.
pub fn install<R: Runtime>(app: &AppHandle<R>) -> bool {
    if !system_has_tray_host() {
        eprintln!("Closing the window quits, since nothing on this desktop would show the tray.");
        app.manage(TrayState::<R>::new(None));
        return false;
    }
    match build(app) {
        Ok(instance) => {
            app.manage(TrayState::new(Some(instance)));
            true
        }
        Err(problem) => {
            eprintln!("No tray on this system, so closing the window quits: {problem}");
            app.manage(TrayState::<R>::new(None));
            false
        }
    }
}

fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<MenuItem<R>> {
    let show = MenuItem::with_id(app, SHOW_ID, SHOW_LABEL, true, None::<&str>)?;
    let instance = MenuItem::with_id(app, INSTANCE_ID, DEFAULT_INSTANCE, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT_ID, QUIT_LABEL, true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &PredefinedMenuItem::separator(app)?,
            &instance,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("featherbittorrent")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            SHOW_ID => lifecycle::show_window(app),
            QUIT_ID => lifecycle::quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                lifecycle::show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(instance)
}

/// Sets the tray's instance line to the connected instance's name. The page calls it when it connects,
/// and again whenever it switches. Without a tray there is nothing to set, and that is not an error.
#[tauri::command]
pub fn set_tray_instance(
    name: String,
    state: tauri::State<'_, TrayState<tauri::Wry>>,
) -> Result<(), String> {
    let instance = state
        .instance
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match instance.as_ref() {
        Some(item) => item
            .set_text(instance_line(&name))
            .map_err(|problem| format!("The tray could not be updated: {problem}")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_INSTANCE, host_present, instance_line};

    /// Only a clear "the watcher has an owner" counts as a tray: no owner, and no bus, are no tray.
    #[test]
    fn a_tray_needs_a_status_notifier_host() {
        assert!(host_present(Ok(true)));
        assert!(!host_present(Ok(false)));
        assert!(!host_present(Err(
            "no session bus address in the environment".to_string()
        )));
    }

    #[test]
    fn the_instance_line_is_the_name_made_safe() {
        assert_eq!(instance_line("studio"), "studio");
        assert_eq!(instance_line("  "), DEFAULT_INSTANCE);
        assert!(!instance_line("bad\u{1b}[31mname").contains('\u{1b}'));
    }
}
