//! The window's chrome: which title bar the window wears, and the commands that read and change it.
//!
//! The app draws its own title bar (the page's `TitleBar`). On macOS the system's title strip is made
//! transparent and its title hidden, so the native traffic lights sit on the page's own bar (the
//! `titleBarStyle`, `hiddenTitle` and `trafficLightPosition` keys in `tauri.conf.json` set that up when
//! the window is made, and [`apply_chrome`] says it again). On Windows and Linux the window has no system
//! decorations and the page draws Minimize, Maximize and Close itself, unless the person asked for the
//! system title bar (`system_title_bar` in the desktop settings): then the system's decorations come
//! back and the page draws no controls. macOS has no such fallback: its traffic lights are the system's
//! own already.
//!
//! [`apply_chrome`] runs before the event loop first draws the window, so a window that appeared with
//! decorations and lost them a moment later does not flash.

use std::path::PathBuf;
use std::sync::Mutex;

use fbtt_desktop_core::DesktopSettings;
use tauri::{Emitter, TitleBarStyle, WebviewWindow};

use crate::commands::Platform;

/// The event the shell emits after the title bar setting changed, carrying whether the page should now
/// draw a bare toolbar under the system's title bar (`true`) or its own bar with window controls.
pub const SYSTEM_TITLE_BAR_EVENT: &str = "system-title-bar";

/// Which title bar the window wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chrome {
    /// macOS: the system's title strip, transparent and untitled, with the native traffic lights kept
    /// and drawn over the page's own bar.
    Overlay,
    /// Windows and Linux: no system decorations; the page draws the bar and the window controls.
    Drawn,
    /// Windows and Linux with `system_title_bar` on: the system's own title bar and controls.
    System,
}

/// The chrome for `platform` when the setting says `system_title_bar`.
pub fn chrome_for(platform: Platform, system_title_bar: bool) -> Chrome {
    match platform {
        Platform::Macos => Chrome::Overlay,
        Platform::Windows | Platform::Linux if system_title_bar => Chrome::System,
        Platform::Windows | Platform::Linux => Chrome::Drawn,
    }
}

/// Whether the page draws a bare toolbar under the system's title bar. Never on macOS, where the
/// setting is not offered and the bar is always the app's own.
pub fn uses_system_title_bar(platform: Platform, system_title_bar: bool) -> bool {
    chrome_for(platform, system_title_bar) == Chrome::System
}

/// Puts the chrome [`chrome_for`] names on `window`. A refusal from the window system is said on stderr
/// and otherwise ignored: a window with the wrong decorations is still a window that works.
pub fn apply_chrome(window: &WebviewWindow, platform: Platform, system_title_bar: bool) {
    let result = match chrome_for(platform, system_title_bar) {
        Chrome::Overlay => window
            .set_decorations(true)
            .and_then(|()| window.set_title_bar_style(TitleBarStyle::Overlay)),
        Chrome::Drawn => window.set_decorations(false),
        Chrome::System => window.set_decorations(true),
    };
    if let Err(problem) = result {
        eprintln!("The window's title bar could not be set: {problem}");
    }
}

/// What the chrome commands keep: where the settings file is, and the setting as this run knows it.
#[derive(Debug)]
pub struct ChromeState {
    settings_file: PathBuf,
    system_title_bar: Mutex<bool>,
}

impl ChromeState {
    /// The state for a run whose settings live at `settings_file` and say `system_title_bar`.
    pub fn new(settings_file: PathBuf, system_title_bar: bool) -> Self {
        ChromeState {
            settings_file,
            system_title_bar: Mutex::new(system_title_bar),
        }
    }

    fn get(&self) -> bool {
        *self
            .system_title_bar
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set(&self, on: bool) {
        *self
            .system_title_bar
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = on;
    }
}

/// Writes `system_title_bar = on` into the settings file at `path`, keeping every other setting as the
/// file has it now (it is read again first, so a change saved by another part of the app since startup
/// is not undone). A file that cannot be read is left alone and the refusal says why.
pub fn save_system_title_bar(path: &std::path::Path, on: bool) -> Result<(), String> {
    let mut settings = DesktopSettings::load(path).map_err(|problem| problem.to_string())?;
    settings.system_title_bar = on;
    settings.save(path).map_err(|problem| problem.to_string())
}

/// Whether the page should draw a bare toolbar under the system's title bar: the setting, on a system
/// where it applies.
#[tauri::command]
pub fn system_title_bar(state: tauri::State<'_, ChromeState>) -> bool {
    uses_system_title_bar(Platform::current(), state.get())
}

/// Saves the title bar setting, puts the matching chrome on the window, and tells the page (the
/// [`SYSTEM_TITLE_BAR_EVENT`] event) so its bar follows. Answers what [`system_title_bar`] now answers.
#[tauri::command]
pub fn set_system_title_bar(
    on: bool,
    window: WebviewWindow,
    state: tauri::State<'_, ChromeState>,
) -> Result<bool, String> {
    save_system_title_bar(&state.settings_file, on)?;
    state.set(on);
    let platform = Platform::current();
    apply_chrome(&window, platform, on);
    let system = uses_system_title_bar(platform, on);
    if let Err(problem) = window.emit(SYSTEM_TITLE_BAR_EVENT, system) {
        eprintln!("The page was not told about the title bar: {problem}");
    }
    Ok(system)
}

#[cfg(test)]
mod tests {
    use fbtt_core::scratch::ScratchDir;
    use fbtt_desktop_core::{CloseAction, DesktopSettings};

    use super::{Chrome, chrome_for, save_system_title_bar, uses_system_title_bar};
    use crate::commands::Platform;

    #[test]
    fn mac_keeps_the_traffic_lights_on_an_overlay_bar() {
        assert_eq!(chrome_for(Platform::Macos, false), Chrome::Overlay);
        assert_eq!(
            chrome_for(Platform::Macos, true),
            Chrome::Overlay,
            "macOS has no system title bar fallback"
        );
        assert!(!uses_system_title_bar(Platform::Macos, true));
    }

    #[test]
    fn windows_and_linux_draw_their_own_bar_unless_the_setting_says_otherwise() {
        for platform in [Platform::Windows, Platform::Linux] {
            assert_eq!(chrome_for(platform, false), Chrome::Drawn);
            assert_eq!(chrome_for(platform, true), Chrome::System);
            assert!(!uses_system_title_bar(platform, false));
            assert!(uses_system_title_bar(platform, true));
        }
    }

    #[test]
    fn set_system_title_bar_saves_and_keeps_the_other_settings() {
        let dir = ScratchDir::new("desktop-title-bar-setting");
        let path = dir.path().join("desktop.yaml");
        let before = DesktopSettings {
            close: CloseAction::Tray,
            start_at_login: true,
            ..DesktopSettings::default()
        };
        before.save(&path).expect("the settings save");

        save_system_title_bar(&path, true).expect("the setting saves");
        let after = DesktopSettings::load(&path).expect("the settings load");
        assert!(after.system_title_bar);
        assert_eq!(after.close, CloseAction::Tray);
        assert!(after.start_at_login);

        save_system_title_bar(&path, false).expect("the setting saves");
        assert!(
            !DesktopSettings::load(&path)
                .expect("loads")
                .system_title_bar
        );
    }

    #[test]
    fn an_unreadable_settings_file_is_left_alone() {
        let dir = ScratchDir::new("desktop-title-bar-unparsable");
        let path = dir.path().join("desktop.yaml");
        std::fs::write(&path, "close: [not, a, choice").expect("the file writes");
        assert!(save_system_title_bar(&path, true).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).expect("reads"),
            "close: [not, a, choice"
        );
    }

    /// On macOS the title strip is the overlay with no title and the traffic lights placed on the bar. An unknown `titleBarStyle` would silently be `Visible`, so the exact
    /// spelling is pinned here.
    #[test]
    fn tauri_conf_hides_the_mac_title() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json parses");
        let main = &conf["app"]["windows"][0];
        assert_eq!(main["label"], "main");
        assert_eq!(main["titleBarStyle"], "Overlay");
        assert_eq!(main["hiddenTitle"], true);
        assert!(main["trafficLightPosition"]["x"].is_number());
        assert!(main["trafficLightPosition"]["y"].is_number());
    }
}
