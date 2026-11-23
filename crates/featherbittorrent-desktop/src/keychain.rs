//! Where a saved instance password lives: the system keychain, or nowhere.
//!
//! The settings file says only whether a password is saved for an instance and which user it belongs to;
//! the password itself goes to the platform's own store (the Apple keychain on macOS, the credential
//! manager on Windows, the Secret Service on Linux) under this app's service name and the instance's id.
//! A system without a store (a Linux desktop with no Secret Service running) saves nothing, and the page
//! says so instead of offering a checkbox that would do nothing.
//!
//! Everything goes through [`Keychain`], so a test answers from a map (`MemoryKeychain`) and never wakes
//! the real store.

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;

use keyring::{Entry, Error as KeychainError};

/// The service every entry of this app is filed under: the app's identifier, so nothing of ours lands in
/// the terminal client's namespace or anybody else's.
pub const SERVICE: &str = "io.featherbit.featherbittorrent.desktop";

/// The account [`SystemKeychain::probe`] asks for. Nothing is ever saved under it: asking is the test.
const PROBE_ACCOUNT: &str = "featherbittorrent-desktop-probe";

/// A store of passwords by instance id.
pub trait Keychain: Send + Sync {
    /// The password saved for instance `id`, or `None` when there is none or the store would not say.
    fn get(&self, id: &str) -> Option<String>;
    /// Saves `password` for instance `id`, replacing whatever was there. The error is one line to show.
    fn set(&self, id: &str, password: &str) -> Result<(), String>;
    /// Takes the password for `id` out. Nothing saved is already done.
    fn delete(&self, id: &str);
    /// Whether this system has a store to save in at all.
    fn available(&self) -> bool;
}

/// The platform's own store, through `keyring`.
#[derive(Debug)]
pub struct SystemKeychain {
    available: bool,
}

impl SystemKeychain {
    /// The system store, asked once whether it is there: its initialization worked, and a lookup of an
    /// account nobody saves answers "no entry" rather than "cannot reach the store".
    pub fn probe() -> Self {
        let available = Entry::store_status().is_ok()
            && Entry::new(SERVICE, PROBE_ACCOUNT).is_ok_and(|entry| {
                matches!(entry.get_password(), Ok(_) | Err(KeychainError::NoEntry))
            });
        SystemKeychain { available }
    }
}

impl Keychain for SystemKeychain {
    fn get(&self, id: &str) -> Option<String> {
        if !self.available {
            return None;
        }
        Entry::new(SERVICE, id).ok()?.get_password().ok()
    }

    fn set(&self, id: &str, password: &str) -> Result<(), String> {
        if !self.available {
            return Err(NO_KEYCHAIN.to_string());
        }
        Entry::new(SERVICE, id)
            .and_then(|entry| entry.set_password(password))
            .map_err(|problem| format!("The keychain did not save the password: {problem}"))
    }

    fn delete(&self, id: &str) {
        if !self.available {
            return;
        }
        if let Ok(entry) = Entry::new(SERVICE, id) {
            // Nothing saved, or a store that will not answer: either way there is nothing left to do.
            let _ = entry.delete_credential();
        }
    }

    fn available(&self) -> bool {
        self.available
    }
}

/// What the page says, and what saving answers, on a system with no keychain.
pub const NO_KEYCHAIN: &str = "This system has no keychain, so passwords are not saved.";

/// A keychain in a map, for tests: available or not, as the test asks.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct MemoryKeychain {
    entries: Mutex<HashMap<String, String>>,
    missing: bool,
}

#[cfg(test)]
impl MemoryKeychain {
    /// An empty keychain that saves.
    pub fn new() -> Self {
        MemoryKeychain::default()
    }

    /// A system with no keychain: nothing is saved and nothing is found.
    pub fn unavailable() -> Self {
        MemoryKeychain {
            entries: Mutex::default(),
            missing: true,
        }
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
impl Keychain for MemoryKeychain {
    fn get(&self, id: &str) -> Option<String> {
        self.entries().get(id).cloned()
    }

    fn set(&self, id: &str, password: &str) -> Result<(), String> {
        if self.missing {
            return Err(NO_KEYCHAIN.to_string());
        }
        self.entries().insert(id.to_string(), password.to_string());
        Ok(())
    }

    fn delete(&self, id: &str) {
        self.entries().remove(id);
    }

    fn available(&self) -> bool {
        !self.missing
    }
}
