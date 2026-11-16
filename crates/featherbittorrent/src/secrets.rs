//! Where a connect password lives between runs: the system keychain, or nothing at all.
//!
//! A password is the one thing about a connection featherbittorrent cannot keep in a file: `config.yaml`
//! and `ui.yaml` are plain text somebody can cat, and a server password protects the library behind it. So
//! it goes to the platform's own store - the Apple keychain on macOS, the credential manager on Windows,
//! the secret service on Linux - under this app's service name and an account named after the server, and
//! nowhere else. A device key is different by nature: its bytes stay in their file or in ssh-agent, so
//! only a password is asked of the keychain, and a run that signs some other way never opens it.
//!
//! The keychain is somebody else's process, so two rules follow. Everything here goes through
//! [`SecretStore`], which a test can answer from a map instead of waking the real one. And every call runs
//! on a blocking thread: the thread that draws the screen does not wait on another process (CLAUDE.md).
//!
//! A keychain that is missing, locked, or simply not running is not a failure and never was: this says so
//! in one line, stores nothing, and asks the person each run instead.

use std::sync::Arc;

use fbtt_core::Secret;
use fbtt_core::ui_state::{LoginMethod, RemoteTarget};
use fbtt_tui::SecretInput;
use keyring::{Entry, Error as KeychainError};
use zeroize::Zeroizing;

/// The service every featherbittorrent entry is filed under, so one person's servers sit together and
/// nothing of ours lands in another application's namespace.
const SERVICE: &str = "featherbittorrent";

/// What the screen says when the system keychain could not be used at all. It says what happens next
/// rather than only what went wrong, because the person still has to decide whether to type it again.
pub const KEYCHAIN_UNAVAILABLE: &str =
    "The system keychain is not available; the password will be asked each time.";

/// Where a connect password is kept between runs.
///
/// The `Err` of every method is one line about why the keychain could not be consulted, fit to show the
/// person as it came back: a keychain that will not answer is a thing to say out loud, not to retry.
pub trait SecretStore {
    /// The password kept for `account`, or `None` when nothing is kept under that name.
    fn get(&self, account: &str) -> Result<Option<Zeroizing<String>>, String>;

    /// Keeps `password` for `account`, replacing whatever was kept there.
    fn set(&self, account: &str, password: &str) -> Result<(), String>;

    /// Takes whatever is kept for `account` out. An entry that was not there is done, not an error.
    fn delete(&self, account: &str) -> Result<(), String>;
}

/// The system keychain: the Apple keychain on macOS, the credential manager on Windows, and the secret
/// service over D-Bus on Linux - each one the platform's own, reached through `keyring`'s `v1` feature.
pub struct Keychain;

impl SecretStore for Keychain {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<String>>, String> {
        let entry = entry(account)?;
        match entry.get_password() {
            Ok(password) => Ok(Some(Zeroizing::new(password))),
            // Nothing kept under this name is not trouble: it is the answer that sends the person to the
            // connect screen.
            Err(KeychainError::NoEntry) => Ok(None),
            Err(problem) => Err(problem.to_string()),
        }
    }

    fn set(&self, account: &str, password: &str) -> Result<(), String> {
        match entry(account) {
            Ok(entry) => entry
                .set_password(password)
                .map_err(|problem| problem.to_string()),
            Err(problem) => Err(problem),
        }
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        let entry = entry(account)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            // Taking out what was never put in is what was asked for.
            Err(KeychainError::NoEntry) => Ok(()),
            Err(problem) => Err(problem.to_string()),
        }
    }
}

/// One entry of this app's, or one line about a keychain that would not hand one over (including a store
/// this platform could not open at all).
fn entry(account: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, account).map_err(|problem| problem.to_string())
}

/// Whether this login method signs in with a password, which is the only kind of secret kept here: a
/// device key stays in its file or in ssh-agent, and its passphrase is typed afresh whenever it is needed.
fn signs_with_a_password(login: LoginMethod) -> bool {
    matches!(login, LoginMethod::Password | LoginMethod::GatewayBasic)
}

/// The name a target's password is kept under: `<user>@<url>`, because one person can hold accounts on
/// several servers and several accounts on one of them.
///
/// `None` for the methods that sign with a key, and for a password method with no user name or no address
/// to name the entry by - which is not a connection yet, so there is nothing to keep it under.
fn account(target: &RemoteTarget) -> Option<String> {
    if !signs_with_a_password(target.login) {
        return None;
    }
    let user = target
        .user
        .as_deref()
        .map(str::trim)
        .filter(|user| !user.is_empty())?;
    let url = target.url.trim();
    (!url.is_empty()).then(|| format!("{user}@{url}"))
}

/// What the connect screen collected, spelled as `fbtt-core` wants a credential.
///
/// The method decides which piece is sent, not whatever happens to be sitting in a field: a key-file login
/// has no password to give a server, and a login with nothing has neither. A target and a credential that
/// do not go together are refused once by the backend, so choosing here is what keeps a half-typed form
/// from being reported as a broken server.
pub fn remote_secret(target: &RemoteTarget, input: &SecretInput) -> Secret {
    match target.login {
        LoginMethod::None => Secret::None,
        LoginMethod::Password | LoginMethod::GatewayBasic => Secret::Password(
            input
                .password
                .clone()
                .unwrap_or_else(|| Zeroizing::new(String::new())),
        ),
        // `Agent` collects no passphrase - its key's bytes are never read - so this is `None` for it, which
        // is what the backend reads as "ssh-agent holds the key".
        LoginMethod::KeyFile | LoginMethod::Agent | LoginMethod::GatewayKey => {
            Secret::KeyPassphrase(input.passphrase.clone())
        }
    }
}

/// What this run can start a connection with.
#[derive(Debug)]
pub enum StartSecret {
    /// The credential to start with, and nothing left to ask anybody.
    Ready(Secret),
    /// Something this target signs in with is not here. Start the connection anyway - it says once, through
    /// the client, why nothing appeared - and open the connect screen prefilled so the person can supply
    /// what is missing. `notice` is the line to put beside that question when the keychain itself could
    /// not be consulted, and `None` when it answered plainly that nothing is kept.
    Ask {
        secret: Secret,
        notice: Option<String>,
    },
}

/// The pure half of [`start_secret_for`]: what to start with, given what the keychain holds (`stored`) and
/// whether it could be consulted at all (`unavailable`).
pub fn decide_start_secret(
    target: &RemoteTarget,
    stored: Option<Zeroizing<String>>,
    unavailable: bool,
) -> StartSecret {
    if !signs_with_a_password(target.login) {
        return StartSecret::Ready(remote_secret(target, &SecretInput::default()));
    }
    match stored {
        Some(password) => StartSecret::Ready(Secret::Password(password)),
        // Nothing was kept - or nothing could be asked. A password login with no password is what the
        // remote backend already reports as one credential that does not go with its method: one line, no
        // attempts against the server, and a header that says why the table is empty.
        None => StartSecret::Ask {
            secret: Secret::None,
            notice: unavailable.then(|| KEYCHAIN_UNAVAILABLE.to_string()),
        },
    }
}

/// [`decide_start_secret`] with the keychain read for it, off the thread that draws the screen.
///
/// A target that signs with a key or with nothing never opens the keychain at all: there is nothing of
/// ours to look for there, and waking a secret service to find none is not free.
pub async fn start_secret_for(
    store: Arc<dyn SecretStore + Send + Sync>,
    target: RemoteTarget,
) -> StartSecret {
    // The method answers this before the keychain is opened: a key login has nothing of ours in there, and
    // waking a secret service to find none is not free. `account` cannot tell that apart from a password
    // login with no name yet - both are `None` - so the method is asked first.
    if !signs_with_a_password(target.login) {
        return decide_start_secret(&target, None, false);
    }
    let Some(account) = account(&target) else {
        // A password method with no user name and no address is not a connection yet, so there is nothing
        // to look up: ask for all of it on the screen.
        return StartSecret::Ask {
            secret: Secret::None,
            notice: None,
        };
    };
    match tokio::task::spawn_blocking(move || store.get(&account)).await {
        Ok(Ok(stored)) => decide_start_secret(&target, stored, false),
        // A keychain call is a call to another process: one that refused and one that was dropped mid-flight
        // are the same answer to the person, so they get the same line.
        Ok(Err(_)) | Err(_) => decide_start_secret(&target, None, true),
    }
}

/// What to do about the password a connect answer just collected.
#[derive(Clone, PartialEq, Eq)]
pub enum KeepPlan {
    /// Nothing of ours for the keychain to hold: this login signs with a key or with nothing, or no
    /// password was collected to keep.
    Nothing,
    /// Keep this password under `account` so the next run does not ask for it.
    Keep {
        account: String,
        password: Zeroizing<String>,
    },
    /// Take out whatever is kept under `account`, because the person said not to keep this one: the next
    /// run asks as this one did, and cannot be surprised by a password left from an earlier answer.
    Forget { account: String },
}

impl std::fmt::Debug for KeepPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Which operation, and never what it would keep: this is the one value beside a target that
        // somebody could repeat out of a log line by accident.
        match self {
            KeepPlan::Nothing => f.write_str("KeepPlan::Nothing"),
            KeepPlan::Keep { account, .. } => write!(f, "KeepPlan::Keep({account}, <redacted>)"),
            KeepPlan::Forget { account } => write!(f, "KeepPlan::Forget({account})"),
        }
    }
}

/// The pure half of [`keep_password`]: what the answer says to do about the keychain.
pub fn keep_plan(target: &RemoteTarget, input: &SecretInput) -> KeepPlan {
    let Some(account) = account(target) else {
        return KeepPlan::Nothing;
    };
    match (&input.password, input.remember) {
        // The box was ticked: the next run starts from here without asking.
        (Some(password), true) => KeepPlan::Keep {
            account,
            password: password.clone(),
        },
        // It was not ticked, so nothing is kept and what an earlier answer left behind goes: "asked each
        // time" has to mean that even for somebody who connected here with a remembered password before.
        (Some(_), false) => KeepPlan::Forget { account },
        // Nothing was collected (a key method, or a form that could not be submitted): nothing to say.
        (None, _) => KeepPlan::Nothing,
    }
}

/// Does what [`keep_plan`] says, off the thread that draws the screen.
///
/// The `Err` is the line to show beside a connection that goes ahead anyway: nothing was stored either
/// way, and the password is already in hand, so a keychain that will not answer is worth saying out loud
/// rather than retrying. Its own words are left out on purpose - what the person needs to know is that
/// nothing was remembered, not which system call refused it.
pub async fn keep_password(
    store: Arc<dyn SecretStore + Send + Sync>,
    target: RemoteTarget,
    input: SecretInput,
) -> Result<(), String> {
    let do_it = move |store: Arc<dyn SecretStore + Send + Sync>| match keep_plan(&target, &input) {
        KeepPlan::Nothing => Ok(()),
        KeepPlan::Keep { account, password } => store.set(&account, &password),
        KeepPlan::Forget { account } => store.delete(&account),
    };
    match tokio::task::spawn_blocking(move || do_it(store)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(KEYCHAIN_UNAVAILABLE.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        KEYCHAIN_UNAVAILABLE, KeepPlan, SecretStore, StartSecret, account, decide_start_secret,
        keep_password, keep_plan, remote_secret, start_secret_for,
    };
    use fbtt_core::Secret;
    use fbtt_core::ui_state::{LoginMethod, RemoteTarget};
    use fbtt_tui::SecretInput;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use zeroize::Zeroizing;

    /// A target a person would actually type in: this machine's server, signed in to as `alice`.
    fn target(login: LoginMethod) -> RemoteTarget {
        RemoteTarget {
            url: "http://127.0.0.1:8090".to_string(),
            login,
            user: Some("alice".to_string()),
            ..RemoteTarget::default()
        }
    }

    /// A keychain that answers from a map, and can be made to answer like one that is not there.
    #[derive(Debug, Default)]
    struct FakeStore {
        entries: Mutex<HashMap<String, String>>,
        /// Every operation asked of it, in order, so a test can see that some were never asked for.
        calls: Mutex<Vec<String>>,
        /// Whether this keychain refuses everything, the way a locked or missing one does.
        unavailable: bool,
    }

    impl FakeStore {
        fn new() -> FakeStore {
            FakeStore::default()
        }

        /// A keychain that hands back its own error for every operation.
        fn broken() -> FakeStore {
            FakeStore {
                unavailable: true,
                ..FakeStore::default()
            }
        }

        fn kept(&self, account: &str) -> Option<String> {
            self.entries
                .lock()
                .expect("no panic inside the lock")
                .get(account)
                .cloned()
        }

        fn asked(&self) -> Vec<String> {
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

    /// A password is kept under the server it belongs to, and a key login never asks the keychain.
    #[test]
    fn an_account_is_named_after_the_server_it_signs_in_to() {
        assert_eq!(
            account(&target(LoginMethod::Password)),
            Some("alice@http://127.0.0.1:8090".to_string())
        );
        // A gateway password is still a password to this machine, and the same user on another server is
        // another entry: two accounts on one host, or one account reached two ways, do not share a line.
        assert_eq!(
            account(&target(LoginMethod::GatewayBasic)),
            Some("alice@http://127.0.0.1:8090".to_string())
        );
        for login in [
            LoginMethod::None,
            LoginMethod::KeyFile,
            LoginMethod::Agent,
            LoginMethod::GatewayKey,
        ] {
            assert_eq!(
                account(&target(login)),
                None,
                "{login:?} signs with something that is not the keychain's to hold"
            );
        }

        // A password method with nothing to name the entry by is not a connection yet.
        let no_user = RemoteTarget {
            user: None,
            ..target(LoginMethod::Password)
        };
        assert_eq!(account(&no_user), None);
        let no_url = RemoteTarget {
            url: "  ".to_string(),
            ..target(LoginMethod::Password)
        };
        assert_eq!(account(&no_url), None);
    }

    /// What the connect screen collected is chosen by the method, not by what was left in a field.
    #[test]
    fn the_credential_follows_the_method() {
        let typed = SecretInput {
            password: Some(Zeroizing::new("hunter2".to_string())),
            passphrase: Some(Zeroizing::new("correct horse".to_string())),
            remember: true,
        };

        match remote_secret(&target(LoginMethod::Password), &typed) {
            Secret::Password(password) => assert_eq!(&*password, "hunter2"),
            other => panic!("a password login was given {other:?}"),
        }
        // A key file is opened with its passphrase, and ssh-agent's key needs none at all: what the form
        // did not collect is not invented here.
        match remote_secret(&target(LoginMethod::KeyFile), &typed) {
            Secret::KeyPassphrase(passphrase) => {
                assert_eq!(passphrase.as_deref().map(|p| &**p), Some("correct horse"))
            }
            other => panic!("a key file was given {other:?}"),
        }
        let no_passphrase = SecretInput {
            passphrase: None,
            ..typed.clone()
        };
        assert!(
            matches!(
                remote_secret(&target(LoginMethod::Agent), &no_passphrase),
                Secret::KeyPassphrase(None)
            ),
            "ssh-agent was given a passphrase to open a key with"
        );
        // A login with nothing sends nothing, even with both fields somehow full.
        assert!(matches!(
            remote_secret(&target(LoginMethod::None), &typed),
            Secret::None
        ));
    }

    /// What the keychain holds decides whether anybody is asked for a password.
    #[test]
    fn a_stored_password_starts_the_connection_without_asking() {
        let stored = Some(Zeroizing::new("hunter2".to_string()));
        match decide_start_secret(&target(LoginMethod::Password), stored, false) {
            StartSecret::Ready(Secret::Password(password)) => assert_eq!(&*password, "hunter2"),
            other => panic!("the keychain had the password, so this run asks: {other:?}"),
        }

        // Nothing kept: ask, and start with no credential at all so the connection says once that there
        // is nothing to sign in with instead of trying an empty password. A plain "nothing here" answer
        // needs no line beside it - the connect screen is the line.
        match decide_start_secret(&target(LoginMethod::Password), None, false) {
            StartSecret::Ask { secret, notice } => {
                assert!(matches!(secret, Secret::None));
                assert_eq!(notice, None);
            }
            other => panic!("no password was kept, so this run should ask: {other:?}"),
        }

        // A keychain that could not be consulted asks the same question and adds the one line the person
        // can act on. Its own words are not shown: which system call refused is not their decision to make.
        match decide_start_secret(&target(LoginMethod::Password), None, true) {
            StartSecret::Ask { secret, notice } => {
                assert!(
                    matches!(secret, Secret::None),
                    "a refused keychain was handed a credential"
                );
                assert_eq!(notice.as_deref(), Some(KEYCHAIN_UNAVAILABLE));
            }
            other => panic!("a keychain that refused should still ask: {other:?}"),
        }

        // A method that does not sign with a password is answered without one, and nobody is asked.
        for login in [
            LoginMethod::None,
            LoginMethod::KeyFile,
            LoginMethod::Agent,
            LoginMethod::GatewayKey,
        ] {
            match decide_start_secret(&target(login), None, true) {
                StartSecret::Ready(secret) => assert!(
                    !matches!(secret, Secret::Password(_)),
                    "{login:?} was handed a password it does not sign with"
                ),
                StartSecret::Ask { .. } => panic!("{login:?} had nothing missing to ask about"),
            }
        }
    }

    /// The whole start path over a fake keychain: the map answers, and the real one is never touched.
    #[tokio::test]
    async fn the_keychain_is_read_once_and_only_for_a_password() {
        let store = Arc::new(FakeStore::new());
        store
            .entries
            .lock()
            .expect("no panic inside the lock")
            .insert(
                "alice@http://127.0.0.1:8090".to_string(),
                "hunter2".to_string(),
            );

        let started = start_secret_for(store.clone(), target(LoginMethod::Password)).await;
        match started {
            StartSecret::Ready(Secret::Password(password)) => assert_eq!(&*password, "hunter2"),
            other => panic!("the entry was there, got {other:?}"),
        }
        assert_eq!(store.asked(), vec!["get".to_string()]);

        // A method that signs with a key has nothing of ours in the keychain, so it is not opened for one.
        let store = Arc::new(FakeStore::new());
        let started = start_secret_for(store.clone(), target(LoginMethod::KeyFile)).await;
        assert!(
            matches!(started, StartSecret::Ready(Secret::KeyPassphrase(None))),
            "a key file was sent to the keychain: {started:?}"
        );
        assert!(
            store.asked().is_empty(),
            "the keychain was asked about anyway"
        );

        // An empty keychain asks, with no complaint to add: the screen itself is the question.
        let store = Arc::new(FakeStore::new());
        match start_secret_for(store.clone(), target(LoginMethod::Password)).await {
            StartSecret::Ask { secret, notice } => {
                assert!(matches!(secret, Secret::None));
                assert_eq!(notice, None);
            }
            other => panic!("nothing was kept, got {other:?}"),
        }

        // A keychain that refuses everything is reported once, not retried, and still asks the person.
        let store = Arc::new(FakeStore::broken());
        match start_secret_for(store.clone(), target(LoginMethod::Password)).await {
            StartSecret::Ask { notice, .. } => assert_eq!(
                notice.as_deref(),
                Some(KEYCHAIN_UNAVAILABLE),
                "a keychain that could not be consulted says so before the screen asks"
            ),
            other => panic!("a refused keychain should still ask: {other:?}"),
        }
    }

    /// Ticking the box keeps the password for the next run; leaving it out takes out what an earlier answer left.
    #[test]
    fn remember_decides_what_the_keychain_holds_afterwards() {
        let typed = SecretInput {
            password: Some(Zeroizing::new("hunter2".to_string())),
            ..SecretInput::default()
        };
        let account = "alice@http://127.0.0.1:8090".to_string();

        assert_eq!(
            keep_plan(&target(LoginMethod::Password), &typed.clone()),
            KeepPlan::Forget {
                account: account.clone()
            },
            "an unticked box means the next run asks as this one did"
        );
        let remembered = SecretInput {
            remember: true,
            ..typed.clone()
        };
        match keep_plan(&target(LoginMethod::Password), &remembered) {
            KeepPlan::Keep {
                account: named,
                password,
            } => {
                assert_eq!(named, account);
                assert_eq!(&*password, "hunter2");
            }
            other => panic!("the box was ticked, got {other:?}"),
        }

        // A method with no password of ours to keep is nothing to do, whichever way the box stands - and
        // `remember` cannot even be true for one, which the connect screen's own rule makes sure of.
        assert_eq!(
            keep_plan(&target(LoginMethod::KeyFile), &remembered),
            KeepPlan::Nothing
        );
        assert_eq!(
            keep_plan(&target(LoginMethod::None), &typed),
            KeepPlan::Nothing
        );

        // `Debug` says which operation this is and not what it would keep.
        let keeping = keep_plan(&target(LoginMethod::Password), &remembered);
        assert_eq!(
            format!("{keeping:?}"),
            "KeepPlan::Keep(alice@http://127.0.0.1:8090, <redacted>)"
        );
    }

    /// The same three answers, carried out against a fake keychain.
    #[tokio::test]
    async fn keeping_a_password_writes_it_once() {
        let account = "alice@http://127.0.0.1:8090";
        let remembered = SecretInput {
            password: Some(Zeroizing::new("hunter2".to_string())),
            remember: true,
            ..SecretInput::default()
        };

        let store = Arc::new(FakeStore::new());
        keep_password(
            store.clone(),
            target(LoginMethod::Password),
            remembered.clone(),
        )
        .await
        .expect("the fake keychain takes it");
        assert_eq!(store.kept(account), Some("hunter2".to_string()));
        assert_eq!(store.asked(), vec!["set".to_string()]);

        // The same answer with the box left alone takes the entry out again.
        let store = Arc::new(FakeStore::new());
        store
            .entries
            .lock()
            .expect("no panic inside the lock")
            .insert(account.to_string(), "stale".to_string());
        keep_password(
            store.clone(),
            target(LoginMethod::Password),
            not_remembered(&remembered),
        )
        .await
        .expect("forgetting is an ordinary answer");
        assert_eq!(store.kept(account), None);

        // A key login leaves the keychain alone whatever the form collected.
        let store = Arc::new(FakeStore::new());
        keep_password(
            store.clone(),
            target(LoginMethod::KeyFile),
            remembered.clone(),
        )
        .await
        .expect("nothing was asked of the keychain");
        assert!(store.asked().is_empty(), "a key login opened the keychain");

        // A keychain that will not answer says so once, in the words the person can act on, and stores
        // nothing.
        let store = Arc::new(FakeStore::broken());
        let problem = keep_password(
            store.clone(),
            target(LoginMethod::Password),
            remembered.clone(),
        )
        .await
        .expect_err("a locked keychain does not take a password");
        assert_eq!(problem, KEYCHAIN_UNAVAILABLE);
        assert!(store.kept(account).is_none(), "nothing was stored");
    }

    /// The line the person sees instead of a stored password: it names what happens next, because they
    /// still have to decide whether to type it again.
    #[test]
    fn the_keychain_notice_says_what_happens_next() {
        assert_eq!(
            KEYCHAIN_UNAVAILABLE,
            "The system keychain is not available; the password will be asked each time."
        );
    }

    /// The same answer with the box left unticked.
    fn not_remembered(input: &SecretInput) -> SecretInput {
        SecretInput {
            remember: false,
            ..input.clone()
        }
    }
}
