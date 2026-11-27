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
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use fbtt_server::ProxyPasswordStore;
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
    /// [`delete`](Self::delete), saying when the store would not take it out. The error is one line to
    /// show.
    fn remove(&self, id: &str) -> Result<(), String>;
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
    ///
    /// On Linux the store is the Secret Service, asked over the session bus before the window exists, so
    /// the question has a deadline ([`crate::deadline::SESSION_BUS_DEADLINE`]): a bus that does not answer
    /// in time means no keychain for this run, said on stderr, rather than an app that never opens.
    pub fn probe() -> Self {
        #[cfg(not(any(windows, target_os = "macos")))]
        let available = crate::deadline::answer_within(
            crate::deadline::SESSION_BUS_DEADLINE,
            store_answers,
        )
        .unwrap_or_else(|| {
            eprintln!(
                "The Secret Service did not answer in time; passwords are not saved this run."
            );
            false
        });
        #[cfg(any(windows, target_os = "macos"))]
        let available = store_answers();
        SystemKeychain { available }
    }
}

/// Whether the system store initializes and answers a lookup of an account nobody saves with "no entry"
/// rather than "cannot reach the store".
fn store_answers() -> bool {
    Entry::store_status().is_ok()
        && Entry::new(SERVICE, PROBE_ACCOUNT)
            .is_ok_and(|entry| matches!(entry.get_password(), Ok(_) | Err(KeychainError::NoEntry)))
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
        // Nothing saved, or a store that will not answer: either way there is nothing left to do.
        let _ = self.remove(id);
    }

    fn remove(&self, id: &str) -> Result<(), String> {
        if !self.available {
            return Ok(());
        }
        match Entry::new(SERVICE, id).and_then(|entry| entry.delete_credential()) {
            Ok(()) | Err(KeychainError::NoEntry) => Ok(()),
            Err(problem) => Err(format!(
                "The keychain did not remove the password: {problem}"
            )),
        }
    }

    fn available(&self) -> bool {
        self.available
    }
}

/// What the page says, and what saving answers, on a system with no keychain.
pub const NO_KEYCHAIN: &str = "This system has no keychain, so passwords are not saved.";

/// The account This computer's proxy password is filed under, beside the instances' under [`SERVICE`].
/// No instance id looks like it: those are generated without a colon.
pub const PROXY_ACCOUNT: &str = "proxy:this-computer";

/// This computer's proxy password in the keychain: the server's [`ProxyPasswordStore`] on the same store
/// the instance passwords use, under [`PROXY_ACCOUNT`].
///
/// Its `Debug` says only whether the store saves; never what it holds.
pub struct ProxyKeychain {
    keychain: Arc<dyn Keychain>,
}

impl ProxyKeychain {
    /// The proxy password's place in `keychain`.
    pub fn new(keychain: Arc<dyn Keychain>) -> Self {
        ProxyKeychain { keychain }
    }
}

impl std::fmt::Debug for ProxyKeychain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyKeychain")
            .field("available", &self.keychain.available())
            .finish_non_exhaustive()
    }
}

impl ProxyPasswordStore for ProxyKeychain {
    fn get(&self) -> Option<String> {
        self.keychain
            .get(PROXY_ACCOUNT)
            .filter(|password| !password.is_empty())
    }

    fn set(&self, password: &str) -> Result<(), String> {
        self.keychain.set(PROXY_ACCOUNT, password)
    }

    fn clear(&self) -> Result<(), String> {
        self.keychain.remove(PROXY_ACCOUNT)
    }

    fn available(&self) -> bool {
        self.keychain.available()
    }
}

/// A keychain in a map, for tests: available or not, as the test asks, and refusing every write when a
/// test says so ([`fail_writes`](Self::fail_writes)). Its `Debug` prints how many entries it holds, never
/// what they are.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryKeychain {
    entries: Mutex<HashMap<String, String>>,
    missing: bool,
    failing: AtomicBool,
    hanging: AtomicBool,
}

#[cfg(test)]
impl std::fmt::Debug for MemoryKeychain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryKeychain")
            .field("entries", &self.entries().len())
            .field("missing", &self.missing)
            .finish_non_exhaustive()
    }
}

/// How long a [`MemoryKeychain`] told to [`hang`](MemoryKeychain::hang) takes to answer: far past the
/// session bus deadline.
#[cfg(test)]
pub const HANG: std::time::Duration = std::time::Duration::from_secs(10);

/// What [`MemoryKeychain`] answers a write it was told to refuse.
#[cfg(test)]
pub const REFUSED: &str = "The keychain did not save the password: the test refused it";

#[cfg(test)]
impl MemoryKeychain {
    /// An empty keychain that saves.
    pub fn new() -> Self {
        MemoryKeychain::default()
    }

    /// A system with no keychain: nothing is saved and nothing is found.
    pub fn unavailable() -> Self {
        MemoryKeychain {
            missing: true,
            ..MemoryKeychain::default()
        }
    }

    /// From now on every `get`, `set` and `remove` takes [`HANG`] before it answers, like a Secret Service
    /// that does not answer (or answers at once again, with `false`).
    pub fn hang(&self, hanging: bool) {
        self.hanging.store(hanging, Ordering::SeqCst);
    }

    fn wait_if_hanging(&self) {
        if self.hanging.load(Ordering::SeqCst) {
            std::thread::sleep(HANG);
        }
    }

    /// From now on every `set` and `remove` fails with [`REFUSED`] (or works again, with `false`).
    pub fn fail_writes(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
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
        self.wait_if_hanging();
        self.entries().get(id).cloned()
    }

    fn set(&self, id: &str, password: &str) -> Result<(), String> {
        self.wait_if_hanging();
        if self.missing {
            return Err(NO_KEYCHAIN.to_string());
        }
        if self.failing.load(Ordering::SeqCst) {
            return Err(REFUSED.to_string());
        }
        self.entries().insert(id.to_string(), password.to_string());
        Ok(())
    }

    fn delete(&self, id: &str) {
        self.entries().remove(id);
    }

    fn remove(&self, id: &str) -> Result<(), String> {
        self.wait_if_hanging();
        if self.failing.load(Ordering::SeqCst) {
            return Err(REFUSED.to_string());
        }
        self.entries().remove(id);
        Ok(())
    }

    fn available(&self) -> bool {
        !self.missing
    }
}
