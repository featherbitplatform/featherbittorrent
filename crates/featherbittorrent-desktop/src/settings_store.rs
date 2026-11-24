//! The one way the shell reads and changes `desktop.yaml`.
//!
//! Several parts of the shell save into the same file: the title bar switch (`window`), the instance list
//! and saved passwords (`instances`), and the close choice and start at login (`lifecycle`). Each change
//! goes through [`SettingsStore::change`], which holds one lock shared by all of them, reads the file
//! again inside that lock, applies the change and writes the file back. So no writer saves an older copy
//! over a field another writer just changed, and a file that cannot be parsed is refused and left exactly
//! as it is. No part of the shell keeps its own copy of a setting: each reads the file through
//! [`SettingsStore::read`] when it needs one.
//!
//! The store is cheap to clone; every clone shares the same lock. `main.rs` makes one and hands a clone
//! to each part that needs it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use fbtt_desktop_core::DesktopSettings;

/// The settings file and the lock every change to it takes.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    path: PathBuf,
    writing: Mutex<()>,
}

impl SettingsStore {
    /// The store for the settings file at `path`.
    pub fn new(path: PathBuf) -> Self {
        SettingsStore {
            shared: Arc::new(Shared {
                path,
                writing: Mutex::new(()),
            }),
        }
    }

    /// Where the settings file is.
    pub fn path(&self) -> &Path {
        &self.shared.path
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.shared
            .writing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The settings as the file says now (the defaults when there is no file yet). A file that cannot
    /// be read or parsed is refused with its line.
    pub fn read(&self) -> Result<DesktopSettings, String> {
        let _one = self.lock();
        DesktopSettings::load(self.path()).map_err(|problem| problem.to_string())
    }

    /// Reads the file, applies `change`, and writes it back, all under the one lock. Nothing is written
    /// when the file cannot be parsed or `change` refuses; the answer is `change`'s.
    pub fn change<T>(
        &self,
        change: impl FnOnce(&mut DesktopSettings) -> Result<T, String>,
    ) -> Result<T, String> {
        let _one = self.lock();
        let mut settings =
            DesktopSettings::load(self.path()).map_err(|problem| problem.to_string())?;
        let answer = change(&mut settings)?;
        settings
            .save(self.path())
            .map_err(|problem| problem.to_string())?;
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use fbtt_core::scratch::ScratchDir;
    use fbtt_desktop_core::{CloseAction, DesktopSettings};

    use super::SettingsStore;

    /// Two writers, each holding its own clone, change different fields at the same moment, many times
    /// over: every change survives, because each one re-reads the file inside the shared lock.
    #[test]
    fn concurrent_settings_writes_keep_both_changes() {
        let dir = ScratchDir::new("desktop-settings-store-concurrent");
        let store = SettingsStore::new(dir.path().join("desktop.yaml"));
        DesktopSettings::default()
            .save(store.path())
            .expect("the settings save");

        for round in 0..50 {
            let start = Arc::new(Barrier::new(2));
            let title_bar = {
                let (store, start) = (store.clone(), Arc::clone(&start));
                std::thread::spawn(move || {
                    start.wait();
                    store.change(|settings| {
                        settings.system_title_bar = round % 2 == 0;
                        Ok(())
                    })
                })
            };
            let close = {
                let (store, start) = (store.clone(), Arc::clone(&start));
                std::thread::spawn(move || {
                    start.wait();
                    store.change(|settings| {
                        settings.close = if round % 2 == 0 {
                            CloseAction::Tray
                        } else {
                            CloseAction::Quit
                        };
                        Ok(())
                    })
                })
            };
            title_bar
                .join()
                .expect("the title bar writer")
                .expect("saves");
            close.join().expect("the close writer").expect("saves");

            let after = store.read().expect("the settings load");
            assert_eq!(after.system_title_bar, round % 2 == 0, "round {round}");
            let close = if round % 2 == 0 {
                CloseAction::Tray
            } else {
                CloseAction::Quit
            };
            assert_eq!(after.close, close, "round {round}");
        }
    }

    #[test]
    fn an_unparsable_file_is_refused_and_left_alone() {
        let dir = ScratchDir::new("desktop-settings-store-unparsable");
        let store = SettingsStore::new(dir.path().join("desktop.yaml"));
        std::fs::write(store.path(), "close: [not, a, choice").expect("the file writes");
        assert!(store.read().is_err());
        assert!(
            store
                .change(|settings| {
                    settings.start_at_login = true;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(store.path()).expect("reads"),
            "close: [not, a, choice"
        );
    }

    #[test]
    fn a_refused_change_writes_nothing() {
        let dir = ScratchDir::new("desktop-settings-store-refused");
        let store = SettingsStore::new(dir.path().join("desktop.yaml"));
        let refused: Result<(), String> = store.change(|settings| {
            settings.start_at_login = true;
            Err("no".to_string())
        });
        assert!(refused.is_err());
        assert!(!store.path().exists());
    }
}
