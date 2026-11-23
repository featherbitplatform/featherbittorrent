//! The window's chrome: which title bar the window wears, and the commands that read and change it.
//!
//! The app draws its own title bar (the page's `TitleBar`). On macOS the system's title strip is made
//! transparent and its title hidden, so the native traffic lights sit on the page's own bar: the
//! `titleBarStyle`, `hiddenTitle` and `trafficLightPosition` keys in `tauri.conf.json`. On Windows and
//! Linux the window has no system decorations from its first frame (`"decorations": false` in
//! `tauri.windows.conf.json` and `tauri.linux.conf.json`, which Tauri merges over `tauri.conf.json` on
//! that platform) and the page draws Minimize, Maximize and Close itself.
//!
//! The one fallback is Linux's: with `system_title_bar` on in the desktop settings ("Use the system
//! title bar", design spec 2.4), [`apply_chrome`] turns the system's decorations back on and the page
//! draws no controls. Windows always draws its own bar, as macOS always keeps its traffic lights, so on
//! both the setting does nothing.
//!
//! The per-platform conf files repeat the whole window entry, because Tauri merges them as a JSON merge
//! patch, which replaces arrays: `platform_confs_are_the_main_window_undecorated` keeps them equal to
//! `tauri.conf.json`'s entry apart from `decorations`.

use std::path::PathBuf;
use std::sync::Mutex;

use fbtt_desktop_core::DesktopSettings;
use tauri::{Emitter, WebviewWindow};

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
    /// Windows, and Linux by default: no system decorations; the page draws the bar and the controls.
    Drawn,
    /// Linux with `system_title_bar` on: the system's own title bar and controls.
    System,
}

/// The chrome for `platform` when the setting says `system_title_bar`. Only Linux reads the setting.
pub fn chrome_for(platform: Platform, system_title_bar: bool) -> Chrome {
    match platform {
        Platform::Macos => Chrome::Overlay,
        Platform::Windows => Chrome::Drawn,
        Platform::Linux if system_title_bar => Chrome::System,
        Platform::Linux => Chrome::Drawn,
    }
}

/// Whether the page draws a bare toolbar under the system's title bar: Linux with the setting on.
pub fn uses_system_title_bar(platform: Platform, system_title_bar: bool) -> bool {
    chrome_for(platform, system_title_bar) == Chrome::System
}

/// The decorations [`apply_chrome`] has to set at runtime, or `None` when the conf files already gave
/// the window the right ones. Only Linux changes anything: on, under the system title bar; off again
/// when the setting is switched back while the app runs.
pub fn runtime_decorations(platform: Platform, system_title_bar: bool) -> Option<bool> {
    match platform {
        Platform::Linux => Some(chrome_for(platform, system_title_bar) == Chrome::System),
        Platform::Macos | Platform::Windows => None,
    }
}

/// Puts the decorations [`runtime_decorations`] names on `window`. Called first thing once the window
/// exists, before it is shown. A refusal from the window system is said on stderr and otherwise
/// ignored: a window with the wrong decorations is still a window that works.
pub fn apply_chrome(window: &WebviewWindow, platform: Platform, system_title_bar: bool) {
    if let Some(decorations) = runtime_decorations(platform, system_title_bar)
        && let Err(problem) = window.set_decorations(decorations)
    {
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

/// On Linux: saves the title bar setting, puts the matching decorations on the window, and tells the
/// page (the [`SYSTEM_TITLE_BAR_EVENT`] event) so its bar follows. On Windows and macOS it does nothing
/// and answers `false`. Answers what [`system_title_bar`] now answers.
#[tauri::command]
pub fn set_system_title_bar(
    on: bool,
    window: WebviewWindow,
    state: tauri::State<'_, ChromeState>,
) -> Result<bool, String> {
    let platform = Platform::current();
    // Windows and macOS always draw the app's own bar: the setting is not offered there.
    if platform != Platform::Linux {
        return Ok(false);
    }
    save_system_title_bar(&state.settings_file, on)?;
    state.set(on);
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

    use super::{
        Chrome, chrome_for, runtime_decorations, save_system_title_bar, uses_system_title_bar,
    };
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
        assert_eq!(runtime_decorations(Platform::Macos, true), None);
    }

    /// Design spec 2.4: the fallback is Linux's. Windows always draws its own bar.
    #[test]
    fn windows_always_draws_its_own_bar() {
        assert_eq!(chrome_for(Platform::Windows, false), Chrome::Drawn);
        assert_eq!(chrome_for(Platform::Windows, true), Chrome::Drawn);
        assert!(!uses_system_title_bar(Platform::Windows, true));
        assert_eq!(runtime_decorations(Platform::Windows, true), None);
        assert_eq!(runtime_decorations(Platform::Windows, false), None);
    }

    #[test]
    fn linux_decorations_come_back_only_under_the_system_title_bar() {
        assert_eq!(chrome_for(Platform::Linux, false), Chrome::Drawn);
        assert_eq!(chrome_for(Platform::Linux, true), Chrome::System);
        assert!(uses_system_title_bar(Platform::Linux, true));
        assert!(!uses_system_title_bar(Platform::Linux, false));
        assert_eq!(runtime_decorations(Platform::Linux, true), Some(true));
        assert_eq!(runtime_decorations(Platform::Linux, false), Some(false));
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

    fn conf(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("the conf parses")
    }

    /// On macOS the title strip is the overlay with no title and the traffic lights placed on the bar.
    /// An unknown `titleBarStyle` would silently be `Visible`, so the exact spelling is pinned here.
    #[test]
    fn tauri_conf_hides_the_mac_title() {
        let conf = conf(include_str!("../tauri.conf.json"));
        let main = &conf["app"]["windows"][0];
        assert_eq!(main["label"], "main");
        assert_eq!(main["titleBarStyle"], "Overlay");
        assert_eq!(main["hiddenTitle"], true);
        assert_eq!(main["trafficLightPosition"]["x"], 16);
        assert_eq!(main["trafficLightPosition"]["y"], 34);
    }

    /// Windows and Linux open undecorated from the first frame. Tauri merges the platform file as a
    /// JSON merge patch, which replaces the whole `windows` array, so each platform file must be the
    /// base window entry with `decorations: false` and nothing else changed: a key added to the base
    /// entry (and not to these) would silently be lost on that platform.
    #[test]
    fn platform_confs_are_the_main_window_undecorated() {
        let base = conf(include_str!("../tauri.conf.json"));
        let mut expected = base["app"]["windows"].clone();
        expected[0]["decorations"] = serde_json::Value::Bool(false);
        for (name, text) in [
            (
                "tauri.windows.conf.json",
                include_str!("../tauri.windows.conf.json"),
            ),
            (
                "tauri.linux.conf.json",
                include_str!("../tauri.linux.conf.json"),
            ),
        ] {
            let platform = conf(text);
            assert_eq!(platform["app"]["windows"], expected, "{name}");
            assert_eq!(
                platform.as_object().map(|o| o.len()),
                Some(1),
                "{name} sets only app.windows"
            );
            assert_eq!(
                platform["app"].as_object().map(|o| o.len()),
                Some(1),
                "{name}"
            );
        }
    }
}
