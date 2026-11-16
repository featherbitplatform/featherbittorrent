//! A keychain that answers from a map, so no test in this crate wakes the one this machine really has.
//!
//! Everything about a connect password goes through [`SecretStore`], which exists so that this can stand in
//! for it: the entries are a map, every operation is recorded so a test can see that some were never asked
//! for, and `broken` answers like a keychain that is locked, missing, or simply not running.

use std::collections::HashMap;
use std::sync::Mutex;

use zeroize::Zeroizing;

use crate::secrets::SecretStore;

/// A keychain that answers from a map, and can be made to answer like one that is not there.
#[derive(Debug, Default)]
pub(crate) struct FakeStore {
    entries: Mutex<HashMap<String, String>>,
    /// Every operation asked of it, in order, so a test can see that some were never asked for.
    calls: Mutex<Vec<String>>,
    /// Whether this keychain refuses everything, the way a locked or missing one does.
    unavailable: bool,
}

impl FakeStore {
    pub(crate) fn new() -> FakeStore {
        FakeStore::default()
    }

    /// A keychain that hands back its own error for every operation.
    pub(crate) fn broken() -> FakeStore {
        FakeStore {
            unavailable: true,
            ..FakeStore::default()
        }
    }

    /// Puts an entry in before anybody asks, which is how a test says "the keychain already holds this".
    pub(crate) fn put(&self, account: &str, password: &str) {
        self.entries
            .lock()
            .expect("no panic inside the lock")
            .insert(account.to_string(), password.to_string());
    }

    /// What is kept under `account`, if anything.
    pub(crate) fn kept(&self, account: &str) -> Option<String> {
        self.entries
            .lock()
            .expect("no panic inside the lock")
            .get(account)
            .cloned()
    }

    /// Every operation this keychain was asked for, in order. `asked` is what proves a code path never
    /// reached it: an empty answer means nobody opened it at all.
    pub(crate) fn asked(&self) -> Vec<String> {
        std::mem::take(&mut *self.calls.lock().expect("no panic inside the lock"))
    }
}

impl SecretStore for FakeStore {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<String>>, String> {
        record(self, "get");
        if self.unavailable {
            return Err("the secret service is not running".to_string());
        }
        Ok(self
            .entries
            .lock()
            .expect("no panic inside the lock")
            .get(account)
            .map(|password| Zeroizing::new(password.clone())))
    }

    fn set(&self, account: &str, password: &str) -> Result<(), String> {
        record(self, "set");
        if self.unavailable {
            return Err("the secret service is not running".to_string());
        }
        self.entries
            .lock()
            .expect("no panic inside the lock")
            .insert(account.to_string(), password.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        record(self, "delete");
        if self.unavailable {
            return Err("the secret service is not running".to_string());
        }
        self.entries
            .lock()
            .expect("no panic inside the lock")
            .remove(account);
        Ok(())
    }
}

fn record(store: &FakeStore, call: &str) {
    store
        .calls
        .lock()
        .expect("no panic inside the lock")
        .push(call.to_string());
}
