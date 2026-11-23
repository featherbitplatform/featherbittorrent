//! The tray: the app's icon while the window is hidden, with Show, the connected instance, and Quit.
//!
//! Whether there is a tray at all is what the tray builder says: a Linux desktop without an
//! AppIndicator host refuses it, and then closing the window quits and the tray answer is not offered
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
    use super::{DEFAULT_INSTANCE, instance_line};

    #[test]
    fn the_instance_line_is_the_name_made_safe() {
        assert_eq!(instance_line("studio"), "studio");
        assert_eq!(instance_line("  "), DEFAULT_INSTANCE);
        assert!(!instance_line("bad\u{1b}[31mname").contains('\u{1b}'));
    }
}
