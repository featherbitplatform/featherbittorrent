//! The Desktop section's reads and the shortcut overrides: what the page shows for closing and start at
//! login, and the keyboard shortcuts a person changed, kept in `desktop.yaml` through the shell's one
//! settings store (the `SettingsStore` that `main.rs` hands Tauri's state).

use fbtt_desktop_core::{CloseAction, DesktopSettings, ShortcutOverride};
use serde::Serialize;

use crate::settings_store::SettingsStore;

/// The most overrides the file keeps. The page's binding list is a few dozen actions; anything past
/// this is not a person's edit.
const MAX_OVERRIDES: usize = 256;
/// The most keys one action may hold.
const MAX_KEYS: usize = 8;
/// The longest name a group, an action or a key may have.
const MAX_NAME_CHARS: usize = 64;

/// What the Desktop section shows for the switches the shell keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Preferences {
    pub close: CloseAction,
    pub start_at_login: bool,
}

/// The preferences as the file says now; the defaults when it cannot be read.
pub fn preferences(settings: &SettingsStore) -> Preferences {
    let saved = settings.read().unwrap_or_else(|problem| {
        eprintln!("{problem}");
        DesktopSettings::default()
    });
    Preferences {
        close: saved.close,
        start_at_login: saved.start_at_login,
    }
}

/// The saved overrides; none when the file cannot be read.
pub fn shortcuts(settings: &SettingsStore) -> Vec<ShortcutOverride> {
    settings
        .read()
        .map(|settings| settings.shortcuts)
        .unwrap_or_default()
}

/// Replaces the saved overrides with `list`, refusing one that is not a person's edit (too many, too
/// many keys, a name too long or empty). Whether an action exists is the page's question: it ignores an
/// override it does not know, so an older app reads a newer file.
pub fn save_shortcuts(settings: &SettingsStore, list: Vec<ShortcutOverride>) -> Result<(), String> {
    let fits = |text: &str| !text.is_empty() && text.chars().count() <= MAX_NAME_CHARS;
    let sensible = list.len() <= MAX_OVERRIDES
        && list.iter().all(|entry| {
            fits(&entry.group)
                && fits(&entry.action)
                && !entry.keys.is_empty()
                && entry.keys.len() <= MAX_KEYS
                && entry.keys.iter().all(|key| fits(key))
        });
    if !sensible {
        return Err("Those shortcuts could not be saved.".to_string());
    }
    settings.change(|settings| {
        settings.shortcuts = list;
        Ok(())
    })
}

/// What closing the window does and whether the app starts at login, for the Desktop section.
#[tauri::command]
pub fn desktop_preferences(settings: tauri::State<'_, SettingsStore>) -> Preferences {
    preferences(&settings)
}

/// The shortcut overrides the page applies at start.
#[tauri::command]
pub fn get_shortcuts(settings: tauri::State<'_, SettingsStore>) -> Vec<ShortcutOverride> {
    shortcuts(&settings)
}

/// Saves the shortcut overrides after a change in Settings.
#[tauri::command]
pub fn set_shortcuts(
    list: Vec<ShortcutOverride>,
    settings: tauri::State<'_, SettingsStore>,
) -> Result<(), String> {
    save_shortcuts(&settings, list)
}

#[cfg(test)]
mod tests {
    use fbtt_core::scratch::ScratchDir;
    use fbtt_desktop_core::{CloseAction, DesktopSettings, ShortcutOverride};

    use super::{preferences, save_shortcuts, shortcuts};
    use crate::settings_store::SettingsStore;

    fn entry(action: &str, key: &str) -> ShortcutOverride {
        ShortcutOverride {
            group: "Torrent list".to_string(),
            action: action.to_string(),
            keys: vec![key.to_string()],
        }
    }

    /// Saved overrides come back as they went in, as `{ where, action, keys }`, and the rest of the
    /// file is kept.
    #[test]
    fn shortcuts_are_saved_and_read_back() {
        let dir = ScratchDir::new("desktop-shortcuts");
        let store = SettingsStore::new(dir.path().join("desktop.yaml"));
        DesktopSettings {
            close: CloseAction::Tray,
            ..DesktopSettings::default()
        }
        .save(store.path())
        .expect("saves");

        let list = vec![entry("Pause", "q")];
        save_shortcuts(&store, list.clone()).expect("saved");
        assert_eq!(shortcuts(&store), list);
        assert_eq!(preferences(&store).close, CloseAction::Tray, "kept");
        let json = serde_json::to_value(&list).expect("serializes");
        assert_eq!(json[0]["where"], "Torrent list");

        save_shortcuts(&store, Vec::new()).expect("reset");
        assert!(shortcuts(&store).is_empty());
    }

    #[test]
    fn nonsense_shortcuts_are_refused() {
        let dir = ScratchDir::new("desktop-shortcuts-refused");
        let store = SettingsStore::new(dir.path().join("desktop.yaml"));
        assert!(save_shortcuts(&store, vec![entry("Pause", "")]).is_err());
        assert!(save_shortcuts(&store, vec![entry("", "q")]).is_err());
        assert!(save_shortcuts(&store, vec![entry(&"x".repeat(65), "q")]).is_err());
        let many = (0..300).map(|n| entry(&format!("a{n}"), "q")).collect();
        assert!(save_shortcuts(&store, many).is_err());
        assert!(!store.path().exists(), "nothing was written");
    }
}
