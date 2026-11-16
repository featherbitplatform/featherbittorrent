//! Where a connect password lives between runs: the system keychain, or nothing at all.
//!
//! A password is the one thing about a connection featherbittorrent cannot keep in a file: `config.yaml`
//! and `ui.yaml` are plain text somebody can cat, and a server password protects the library behind it. So
//! it goes to the platform's own store - the Apple keychain on macOS, the credential manager on Windows,
//! the secret service on Linux - under this app's service name and an account named after the server, and
//! nowhere else. A device key is different by nature: its bytes stay in their file or in ssh-agent, so
//! only a password is asked of the keychain, and a run that signs some other way never opens it.
//!
//! A gateway API key is the second secret this app keeps, and it follows the same rules on its own account:
//! `ui.yaml` holds only which header the gateway reads (`gateway_key_header`), while the value sits beside
//! the passwords under `gateway-key:<server>` (see [`gateway_account`]). A target can therefore have two
//! entries - a password and a front-door key - and one connect answer says what to do with both (see
//! [`keep_plans`]).
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
// The address half of an account name is spelled out of a parsed URL, so `http://h:1` and `http://h:1/` are
// one entry (see [`normalised_server`]) - the same reading the connect screen and reqwest use.
use url::Url;
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

/// The name a target's password is kept under: `<user>@<server>`, because one person can hold accounts on
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
    let server = normalised_server(target.url.trim())?;
    Some(format!("{user}@{server}"))
}

/// The name a gateway's API key is kept under: `gateway-key:<server>`, because that key opens the front door
/// of one server rather than an account on it - the gateway decides who is in before it knows who is signing
/// in, and the same person's password for that server sits beside it under `<user>@<server>`.
///
/// `None` for a target that names no header, which is every server reached directly: there is no front door,
/// so there is nothing of ours to look up or keep. A target whose address this run cannot read as http or
/// https has no account either (see [`normalised_server`]).
fn gateway_account(target: &RemoteTarget) -> Option<String> {
    let names_a_header = target
        .gateway_key_header
        .as_deref()
        .is_some_and(|header| !header.trim().is_empty());
    if !names_a_header {
        return None;
    }
    Some(format!(
        "gateway-key:{}",
        normalised_server(target.url.trim())?
    ))
}

/// The address half of an account name, spelled one way. `http://alice:8090`, `http://alice:8090/` and
/// `HTTP://Alice:8090` are one entry, because none of those differences is a different server to anybody
/// typing a password twice - and the connect screen's own answer, a saved `ui.yaml`, and something typed in
/// by hand have to land on the same name or the password would not be found where it was put.
///
/// A path *is* kept (`https://gateway.example/bit` is a different place from `https://gateway.example`),
/// except for the bare `/` that every parser adds and nobody types. A query or fragment is not part of the
/// address a server is dialled by, so neither is kept. What this cannot read as an http or https address has
/// no account at all, which is the safe answer: nothing is looked up and nothing is kept.
fn normalised_server(url: &str) -> Option<String> {
    let parsed = url.parse::<Url>().ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    // `Url` has already lower-cased the scheme and the host, and `port()` says nothing about a port that is
    // its scheme's default. An IPv6 address comes out of `host_str` with the brackets a URL needs already on
    // it, so `[::1]` is filed once rather than as both `::1` and `[::1]`.
    let host = parsed.host_str()?;
    let port = parsed
        .port()
        .map_or(String::new(), |port| format!(":{port}"));
    let path = match parsed.path().trim_end_matches('/') {
        "" => String::new(),
        path => path.to_string(),
    };
    Some(format!("{}://{}{}{}", parsed.scheme(), host, port, path))
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
    /// Everything this target signs with is here, and nothing is left to ask anybody. `from_keychain` says
    /// where the password came from, which is the only way this run can later tell "the server said no to the
    /// password we kept" from "the server said no to the password somebody just typed": only the first is
    /// worth taking out of the keychain, and only the first means the person has to be asked again.
    Ready {
        secret: Secret,
        gateway_key: Option<Zeroizing<String>>,
        from_keychain: bool,
    },
    /// Something this target signs in with is not here. Start the connection anyway with whatever *is* in
    /// hand - it says once, through the client, why nothing appeared - and open the connect screen prefilled
    /// so the person can supply what is missing. `notice` is the line to put beside that question when the
    /// keychain itself could not be consulted, and `None` when it answered plainly that nothing is kept.
    Ask {
        secret: Secret,
        gateway_key: Option<Zeroizing<String>>,
        notice: Option<String>,
    },
}

/// What the keychain was asked for one target, and what it answered: both of the entries a target can have,
/// its password under `<user>@<server>` and its gateway key under `gateway-key:<server>` (see [`account`],
/// [`gateway_account`]). `None` means nothing is kept there - which is also the answer for a method that
/// needs neither, whose run never opened the keychain to find that out.
#[derive(Debug, Default)]
pub struct Stored {
    pub login: Option<Zeroizing<String>>,
    pub gateway_key: Option<Zeroizing<String>>,
}

/// The pure half of [`start_secret_for`]: what to start with, given what the keychain holds (`held`) and
/// whether it could be consulted at all (`unavailable`).
pub fn decide_start_secret(target: &RemoteTarget, held: &Stored, unavailable: bool) -> StartSecret {
    // The credential this connection signs with, from whatever the keychain had. A password login with no
    // password is what the remote backend already reports as one credential that does not go with its method:
    // one line, no attempts against the server, and a header that says why the table is empty - so nothing is
    // invented here. A method that signs some other way is answered by its own rule, which never involves the
    // keychain (see [`remote_secret`]).
    let needs_a_password = signs_with_a_password(target.login);
    let secret = match (&held.login, needs_a_password) {
        // The keychain's own answer, which is what makes a later refusal worth acting on.
        (Some(password), true) => Secret::Password(password.clone()),
        (None, true) => Secret::None,
        (_, false) => remote_secret(target, &SecretInput::default()),
    };

    // What this target expects that is not in hand. A named gateway header with no key behind it is missing in
    // exactly the way a password method with nothing kept is: the connect screen is where it gets typed, and
    // dialling without it would only produce refusals from something that never saw our credential.
    let missing = (needs_a_password && held.login.is_none())
        || (gateway_account(target).is_some() && held.gateway_key.is_none());
    if missing {
        // Nothing was kept - or nothing could be asked. A plain "nothing here" answer needs no line beside it:
        // the connect screen is the line.
        StartSecret::Ask {
            secret,
            gateway_key: held.gateway_key.clone(),
            notice: unavailable.then(|| KEYCHAIN_UNAVAILABLE.to_string()),
        }
    } else {
        StartSecret::Ready {
            secret,
            gateway_key: held.gateway_key.clone(),
            // Where the password came from is the only thing that later tells a refusal apart: this run can now
            // say "the server said no to what we kept" (see `crate::switch`).
            from_keychain: needs_a_password && held.login.is_some(),
        }
    }
}

/// [`decide_start_secret`] with the keychain read for it, off the thread that draws the screen.
///
/// A target that signs with a key or with nothing, and names no gateway header, never opens the keychain at
/// all: there is nothing of ours to look for there, and waking a secret service to find none is not free.
pub async fn start_secret_for(
    store: Arc<dyn SecretStore + Send + Sync>,
    target: RemoteTarget,
) -> StartSecret {
    // The method and the header name say which entries could exist before the keychain is opened: `account`
    // cannot tell a key login apart from a password login with no name yet - both are `None` - so the method is
    // asked first, and a target that names neither has nothing of ours to look for.
    let account = account(&target);
    let gateway = gateway_account(&target);
    if account.is_none() && gateway.is_none() {
        return decide_start_secret(&target, &Stored::default(), false);
    }
    let read = tokio::task::spawn_blocking(move || {
        (
            account.as_deref().map(|name| store.get(name)),
            gateway.as_deref().map(|name| store.get(name)),
        )
    })
    .await;
    match read {
        Ok((login, gateway_key)) => {
            // A keychain call is a call to another process: one that refused and one that was dropped mid-flight
            // are the same answer to the person, so either one gets the same line - and either one leaves this
            // run asking for what it could not read.
            let unavailable = login.as_ref().is_some_and(|got| got.is_err())
                || gateway_key.as_ref().is_some_and(|got| got.is_err());
            decide_start_secret(
                &target,
                &Stored {
                    login: login.and_then(Result::ok).flatten(),
                    gateway_key: gateway_key.and_then(Result::ok).flatten(),
                },
                unavailable,
            )
        }
        // The blocking call itself went away before it answered, which is the same thing the person can act on.
        Err(_) => decide_start_secret(&target, &Stored::default(), true),
    }
}

/// What to do about one secret a connect answer just collected: either the password it signs in with, or the
/// key for the gateway header its target names. [`keep_plans`] answers for both at once.
#[derive(Clone, PartialEq, Eq)]
pub enum KeepPlan {
    /// Nothing of ours for the keychain to hold: this login signs with a key or with nothing, or no secret was
    /// collected to keep.
    Nothing,
    /// Keep this secret under `account` so the next run does not ask for it.
    Keep {
        account: String,
        secret: Zeroizing<String>,
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

/// The pure half of what an answer says about the keychain: which one operation, if any, it asks for.
///
/// What comes back is carried to [`crate::switch`], which runs it only once the connection has proved it can
/// sign in - see [`apply_keep`].
pub fn keep_plan(target: &RemoteTarget, input: &SecretInput) -> KeepPlan {
    let Some(account) = account(target) else {
        return KeepPlan::Nothing;
    };
    match (&input.password, input.remember) {
        // The box was ticked: the next run starts from here without asking. An empty box is not a password
        // worth keeping - a later run would sign in with nothing and wonder why the server said no - so an
        // answer that typed nothing keeps whatever is already there instead.
        (Some(password), true) if !password.is_empty() => KeepPlan::Keep {
            account,
            secret: password.clone(),
        },
        (Some(_), true) => KeepPlan::Nothing,
        // It was not ticked, so nothing is kept and what an earlier answer left behind goes: "asked each
        // time" has to mean that even for somebody who connected here with a remembered password before.
        (Some(_), false) => KeepPlan::Forget { account },
        // Nothing was collected (a key method, or a form that could not be submitted): nothing to say.
        (None, _) => KeepPlan::Nothing,
    }
}

/// What an answer says about the gateway-key entry: the same three rules as a password, on the account named
/// by the server whose front door it opens rather than the account it signs in to.
///
/// A target that names no header has no such entry - that is every server reached directly - so this says
/// nothing about the keychain for it, and an answer that collected no key says nothing either: a key left from
/// an earlier answer is found again by the next run that names the same header, which is what "asked each time"
/// is supposed to mean when the box is ticked and not when it is not.
pub fn gateway_keep_plan(target: &RemoteTarget, input: &SecretInput) -> KeepPlan {
    let Some(account) = gateway_account(target) else {
        return KeepPlan::Nothing;
    };
    match (&input.gateway_key, input.remember) {
        // Ticked and typed: the next run through this front door does not have to be given the key again.
        (Some(key), true) if !key.is_empty() => KeepPlan::Keep {
            account,
            secret: key.clone(),
        },
        (Some(_), true) => KeepPlan::Nothing,
        // Not ticked: this run used the key and is not keeping it, and whatever an earlier answer left goes
        // with it so the next run is asked rather than signed in with a key nobody meant to still have.
        (Some(_), false) => KeepPlan::Forget { account },
        // No key was collected: nothing this answer can say about that entry.
        (None, _) => KeepPlan::Nothing,
    }
}

/// What one connect answer asks of the keychain, both entries it can speak for: the credential this run signs
/// in with and the key its target's gateway header names.
///
/// The two halves are kept apart on purpose - different accounts, and a person can be asked to keep one without
/// the other being relevant at all - but they travel together so that one answer cannot half-apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepPlans {
    pub login: KeepPlan,
    pub gateway_key: KeepPlan,
}

impl KeepPlans {
    /// The pair that asks nothing of the keychain: what a run carries before anybody has answered anything,
    /// and what it carries again once these have been paid for (see [`KeepPlan::Nothing`]).
    pub fn nothing() -> KeepPlans {
        KeepPlans {
            login: KeepPlan::Nothing,
            gateway_key: KeepPlan::Nothing,
        }
    }
}

/// [`keep_plan`] and [`gateway_keep_plan`] for one answer.
pub fn keep_plans(target: &RemoteTarget, input: &SecretInput) -> KeepPlans {
    KeepPlans {
        login: keep_plan(target, input),
        gateway_key: gateway_keep_plan(target, input),
    }
}

/// Does what [`keep_plan`] says: at most one call, on whatever thread it is asked on. That thread is a
/// blocking one, never the thread that draws the screen (see `crate::switch::send_work`).
///
/// The `Err` is the line to show beside a connection that goes ahead anyway - nothing was stored either way,
/// and the password is already in hand, so a keychain that will not answer is worth saying out loud rather
/// than retrying. Its own words are left out on purpose: what the person needs to know is that nothing was
/// remembered, not which system call refused it. A plan that asks for nothing never reaches the keychain.
pub fn apply_keep(store: &dyn SecretStore, plan: &KeepPlan) -> Result<(), String> {
    let asked = match plan {
        KeepPlan::Nothing => Ok(()),
        KeepPlan::Keep { account, secret } => store.set(account, secret),
        KeepPlan::Forget { account } => store.delete(account),
    };
    asked.map_err(|_| KEYCHAIN_UNAVAILABLE.to_string())
}

/// Takes this target's stored password out of the keychain, because the server said no to it: the next run
/// asks instead of repeating the mistake. A target with no account to name by has nothing kept under it, so
/// the keychain is not opened at all.
///
/// An ordinary function rather than a future, because whoever calls it is already off the screen's thread and
/// on one that is allowed to wait (see `crate::switch::forget`): a call that may take a while to answer should
/// not also be something somebody has to keep holding a task open for.
pub fn forget_password(store: &dyn SecretStore, target: &RemoteTarget) -> Result<(), String> {
    let Some(account) = account(target) else {
        return Ok(());
    };
    apply_keep(store, &KeepPlan::Forget { account })
}

#[cfg(test)]
mod tests {
    use super::{
        KEYCHAIN_UNAVAILABLE, KeepPlan, StartSecret, Stored, account, apply_keep,
        decide_start_secret, forget_password, gateway_account, gateway_keep_plan, keep_plan,
        keep_plans, normalised_server, remote_secret, start_secret_for,
    };
    use crate::fake_store::FakeStore;
    use fbtt_core::Secret;
    use fbtt_core::ui_state::{LoginMethod, RemoteTarget};
    use fbtt_tui::SecretInput;
    use std::sync::Arc;
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

    /// One server is one account, whatever way its address was spelled: the connect screen's answer, what a
    /// `ui.yaml` was edited into, and what an earlier run kept have to land on the same name or the password
    /// would not be found where it was put.
    #[test]
    fn one_server_is_one_account_name() {
        let spelled = |url: &str| {
            account(&RemoteTarget {
                url: url.to_string(),
                ..target(LoginMethod::Password)
            })
        };
        let one_entry = [
            ("http://alice:8090", "alice@http://alice:8090"),
            ("http://alice:8090/", "alice@http://alice:8090"),
            ("  http://alice:8090/  ", "alice@http://alice:8090"),
            ("HTTP://Alice:8090", "alice@http://alice:8090"),
            // A port that is its scheme's own default is not a second server either.
            ("http://alice:80", "alice@http://alice"),
            ("http://alice", "alice@http://alice"),
        ];
        for (url, name) in one_entry {
            assert_eq!(
                spelled(url),
                Some(name.to_string()),
                "{url} should name one account"
            );
        }

        // A path *is* another place, so it stays; the bare `/` every parser adds is not.
        assert_eq!(
            spelled("https://gateway.example/bit/"),
            Some("alice@https://gateway.example/bit".to_string())
        );
        assert_eq!(
            spelled("https://gateway.example"),
            Some("alice@https://gateway.example".to_string())
        );

        // An IPv6 address keeps the brackets a URL needs, so `::1` is not filed twice.
        assert_eq!(
            spelled("http://[::1]:8090"),
            Some("alice@http://[::1]:8090".to_string())
        );

        // An address this run cannot read as http or https has no account at all: nothing is looked up, and
        // nothing is kept, which is the safe answer for something typed into the wrong field.
        assert_eq!(spelled("ssh://alice@example"), None);
        assert_eq!(spelled("not an address"), None);

        // The address on its own, which is what the account name is made of: lower-cased, with the default
        // port and the bare `/` left out.
        assert_eq!(
            normalised_server("HTTPS://Example.COM:443/"),
            Some("https://example.com".to_string())
        );
    }

    /// What the connect screen collected is chosen by the method, not by what was left in a field.
    #[test]
    fn the_credential_follows_the_method() {
        let typed = SecretInput {
            password: Some(Zeroizing::new("hunter2".to_string())),
            passphrase: Some(Zeroizing::new("correct horse".to_string())),
            // The gateway key is not part of what a connection signs with - it rides beside the credential on
            // every request - so nothing here should pick it up (see [`gateway_keep_plan`] for where it goes).
            gateway_key: Some(Zeroizing::new("a front-door key".to_string())),
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

    /// What the keychain answered about a target's own credential, with nothing kept for a gateway header -
    /// which is every server reached directly.
    fn held(login: Option<Zeroizing<String>>) -> Stored {
        Stored {
            login,
            gateway_key: None,
        }
    }

    /// The same server as [`target`], with a header named for the front door in front of it. The name is what
    /// `ui.yaml` keeps; every case below is about where the key for it came from.
    fn keyed(login: LoginMethod) -> RemoteTarget {
        RemoteTarget {
            gateway_key_header: Some("X-API-Key".to_string()),
            ..target(login)
        }
    }

    /// What the keychain holds decides whether anybody is asked for a password.
    #[test]
    fn a_stored_password_starts_the_connection_without_asking() {
        let stored = Some(Zeroizing::new("hunter2".to_string()));
        match decide_start_secret(&target(LoginMethod::Password), &held(stored), false) {
            StartSecret::Ready {
                secret: Secret::Password(password),
                gateway_key,
                from_keychain,
            } => {
                assert_eq!(&*password, "hunter2");
                // Where the password came from is the only thing that later tells a refusal apart: this run
                // can now say "the server said no to what we kept" (see `crate::switch`).
                assert!(
                    from_keychain,
                    "a password out of the keychain is the keychain's own"
                );
                assert!(
                    gateway_key.is_none(),
                    "a server reached directly was handed a front-door key nobody named"
                );
            }
            other => panic!("the keychain had the password, so this run asks: {other:?}"),
        }

        // Nothing kept: ask, and start with no credential at all so the connection says once that there
        // is nothing to sign in with instead of trying an empty password. A plain "nothing here" answer
        // needs no line beside it - the connect screen is the line.
        match decide_start_secret(&target(LoginMethod::Password), &held(None), false) {
            StartSecret::Ask {
                secret,
                gateway_key,
                notice,
            } => {
                assert!(matches!(secret, Secret::None));
                assert!(gateway_key.is_none());
                assert_eq!(notice, None);
            }
            other => panic!("no password was kept, so this run should ask: {other:?}"),
        }

        // A keychain that could not be consulted asks the same question and adds the one line the person
        // can act on. Its own words are not shown: which system call refused is not their decision to make.
        match decide_start_secret(&target(LoginMethod::Password), &held(None), true) {
            StartSecret::Ask { secret, notice, .. } => {
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
            match decide_start_secret(&target(login), &held(None), true) {
                StartSecret::Ready {
                    secret,
                    gateway_key,
                    from_keychain,
                } => {
                    assert!(
                        !matches!(secret, Secret::Password(_)),
                        "{login:?} was handed a password it does not sign with"
                    );
                    assert!(
                        gateway_key.is_none(),
                        "{login:?} was dialled with a front-door key its target does not name"
                    );
                    assert!(
                        !from_keychain,
                        "{login:?} was told something came out of the keychain when nothing was looked up",
                    );
                }
                StartSecret::Ask { .. } => panic!("{login:?} had nothing missing to ask about"),
            }
        }
    }

    /// A header named for a front door is a piece this run cannot invent, so it is asked for in the same
    /// breath as a missing password - whatever else the target signs with - and dialled with whenever the
    /// keychain has it.
    #[test]
    fn a_gateway_key_is_asked_for_whenever_a_header_is_named() {
        let key = Some(Zeroizing::new("a front-door key".to_string()));

        // Found, on a login that needs no password of its own: dial with both and ask nobody.
        match decide_start_secret(
            &keyed(LoginMethod::KeyFile),
            &Stored {
                login: None,
                gateway_key: key.clone(),
            },
            false,
        ) {
            StartSecret::Ready {
                secret,
                gateway_key,
                from_keychain,
            } => {
                assert!(
                    matches!(secret, Secret::KeyPassphrase(None)),
                    "a device key was sent to the keychain for its passphrase"
                );
                assert_eq!(
                    gateway_key.as_deref().map(|key| &**key),
                    Some("a front-door key"),
                    "the key that was kept should have been dialled with"
                );
                assert!(
                    !from_keychain,
                    "no password was read out of anything, so a refusal is about nothing this run kept",
                );
            }
            other => panic!("the front-door key was in hand, got {other:?}"),
        }

        // Missing: ask, and dial with the credential alone rather than pretending the door is open. Which piece
        // is missing is a line the backend says once before anything is sent (see `fbtt_core::remote`).
        match decide_start_secret(
            &keyed(LoginMethod::KeyFile),
            &Stored {
                login: None,
                gateway_key: None,
            },
            false,
        ) {
            StartSecret::Ask {
                secret,
                gateway_key,
                notice,
            } => {
                assert!(
                    matches!(secret, Secret::KeyPassphrase(None)),
                    "a missing front-door key cost the credential this login does have"
                );
                assert!(gateway_key.is_none());
                assert_eq!(notice, None, "nothing was kept, so nothing went wrong");
            }
            other => panic!("no front-door key was kept, so this run should ask: {other:?}"),
        }

        // A password method with its password but without its key is still a question for the screen, and the
        // password it does hold goes out with the answer.
        match decide_start_secret(
            &keyed(LoginMethod::Password),
            &Stored {
                login: Some(Zeroizing::new("hunter2".to_string())),
                gateway_key: None,
            },
            false,
        ) {
            StartSecret::Ask {
                secret: Secret::Password(password),
                gateway_key,
                ..
            } => {
                assert_eq!(&*password, "hunter2");
                assert!(gateway_key.is_none());
            }
            other => panic!("a target missing its key should ask, got {other:?}"),
        }

        // A keychain that could not be consulted says so once, whichever half it was asked about.
        match decide_start_secret(&keyed(LoginMethod::Password), &Stored::default(), true) {
            StartSecret::Ask { notice, .. } => {
                assert_eq!(notice.as_deref(), Some(KEYCHAIN_UNAVAILABLE))
            }
            other => panic!("a refused keychain should still ask: {other:?}"),
        }

        // Which account that key lives under, and when there is no such account at all: the front door belongs
        // to a server, not to an account on it, so one name serves every login method that target uses.
        assert_eq!(
            gateway_account(&keyed(LoginMethod::Password)),
            Some("gateway-key:http://127.0.0.1:8090".to_string()),
            "the key is filed under the server whose door it opens"
        );
        for login in [
            LoginMethod::None,
            LoginMethod::Password,
            LoginMethod::KeyFile,
            LoginMethod::Agent,
            LoginMethod::GatewayKey,
            LoginMethod::GatewayBasic,
        ] {
            assert_eq!(
                gateway_account(&target(login)),
                None,
                "{login:?} reached directly had an entry invented for it"
            );
        }
    }

    /// The whole start path over a fake keychain: the map answers, and the real one is never touched. One read
    /// per entry this target could have, and none at all for a target that can have none.
    #[tokio::test]
    async fn the_keychain_is_read_once_for_each_entry_a_target_has() {
        let store = Arc::new(FakeStore::new());
        store.put("alice@http://127.0.0.1:8090", "hunter2");

        let started = start_secret_for(store.clone(), target(LoginMethod::Password)).await;
        match started {
            StartSecret::Ready {
                secret: Secret::Password(password),
                gateway_key,
                from_keychain,
            } => {
                assert_eq!(&*password, "hunter2");
                assert!(
                    gateway_key.is_none(),
                    "no header was named to look a key up under"
                );
                assert!(
                    from_keychain,
                    "this is the run that has to notice a refusal of it",
                );
            }
            other => panic!("the entry was there, got {other:?}"),
        }
        assert_eq!(store.asked(), vec!["get".to_string()]);

        // A method that signs with a key has nothing of ours in the keychain, so it is not opened for one.
        let store = Arc::new(FakeStore::new());
        let started = start_secret_for(store.clone(), target(LoginMethod::KeyFile)).await;
        assert!(
            matches!(
                started,
                StartSecret::Ready {
                    secret: Secret::KeyPassphrase(None),
                    gateway_key: None,
                    from_keychain: false,
                }
            ),
            "a key file was sent to the keychain: {started:?}"
        );
        assert!(
            store.asked().is_empty(),
            "the keychain was asked about anyway"
        );

        // An empty keychain asks, with no complaint to add: the screen itself is the question.
        let store = Arc::new(FakeStore::new());
        match start_secret_for(store.clone(), target(LoginMethod::Password)).await {
            StartSecret::Ask {
                secret,
                gateway_key,
                notice,
            } => {
                assert!(matches!(secret, Secret::None));
                assert!(gateway_key.is_none());
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

        // A target with a front door has two entries, and both are read - in order, credential first - because
        // a run cannot say which one the person is missing until it has asked for both.
        let store = Arc::new(FakeStore::new());
        store.put("alice@http://127.0.0.1:8090", "hunter2");
        store.put("gateway-key:http://127.0.0.1:8090", "a front-door key");
        match start_secret_for(store.clone(), keyed(LoginMethod::Password)).await {
            StartSecret::Ready {
                secret: Secret::Password(password),
                gateway_key,
                from_keychain,
            } => {
                assert_eq!(&*password, "hunter2");
                assert_eq!(
                    gateway_key.as_deref().map(|key| &**key),
                    Some("a front-door key"),
                    "the front door was shut on a key that was sitting there"
                );
                assert!(from_keychain);
            }
            other => panic!("both entries were kept, got {other:?}"),
        }
        assert_eq!(
            store.asked(),
            vec!["get".to_string(), "get".to_string()],
            "the keychain was asked about once per entry"
        );

        // Only the half that is missing is asked for: a password kept and no key kept still dials with the
        // password, so the person is not made to retype something this run already has.
        let store = Arc::new(FakeStore::new());
        store.put("alice@http://127.0.0.1:8090", "hunter2");
        match start_secret_for(store.clone(), keyed(LoginMethod::Password)).await {
            StartSecret::Ask {
                secret: Secret::Password(password),
                gateway_key,
                notice,
            } => {
                assert_eq!(&*password, "hunter2");
                assert!(gateway_key.is_none());
                assert_eq!(notice, None);
            }
            other => panic!("the front-door key was not kept, so this run should ask: {other:?}"),
        }

        // A keychain that cannot answer the second question is reported the same way as one that cannot answer
        // the first, and the credential it did hand over is still used.
        let store = Arc::new(FakeStore::broken());
        store.put("alice@http://127.0.0.1:8090", "hunter2");
        match start_secret_for(store.clone(), keyed(LoginMethod::Password)).await {
            StartSecret::Ask { notice, .. } => assert_eq!(
                notice.as_deref(),
                Some(KEYCHAIN_UNAVAILABLE),
                "a keychain that refused one entry says so once"
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
                secret,
            } => {
                assert_eq!(named, account);
                assert_eq!(&*secret, "hunter2");
            }
            other => panic!("the box was ticked, got {other:?}"),
        }

        // An empty box is not a password worth keeping: a later run would sign in with nothing and wonder why
        // the server said no. Whatever is already kept stays kept.
        let empty = SecretInput {
            password: Some(Zeroizing::new(String::new())),
            remember: true,
            ..SecretInput::default()
        };
        assert_eq!(
            keep_plan(&target(LoginMethod::Password), &empty),
            KeepPlan::Nothing,
            "nothing was typed, so nothing is kept"
        );

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

    /// The front-door key follows the password's rules on its own account: ticked keeps it under
    /// `gateway-key:<server>`, unticked takes it out, and a target with no header named says nothing about the
    /// keychain at all - which is what keeps a plain server from waking one.
    #[test]
    fn a_gateway_key_follows_the_same_rules_on_its_own_account() {
        let door = "gateway-key:http://127.0.0.1:8090";
        let typed = SecretInput {
            gateway_key: Some(Zeroizing::new("a front-door key".to_string())),
            ..SecretInput::default()
        };

        // Unticked: the next run asks as this one did, and what an earlier answer left goes with it.
        assert_eq!(
            gateway_keep_plan(&keyed(LoginMethod::Password), &typed),
            KeepPlan::Forget {
                account: door.to_string()
            },
            "an unticked box means the key is not kept"
        );

        // Ticked: kept under the server whose door it opens, beside the password rather than in its place.
        let remembered = SecretInput {
            remember: true,
            ..typed.clone()
        };
        match gateway_keep_plan(&keyed(LoginMethod::Password), &remembered) {
            KeepPlan::Keep { account, secret } => {
                assert_eq!(account, door);
                assert_eq!(&*secret, "a front-door key");
            }
            other => panic!("the box was ticked, got {other:?}"),
        }

        // A device key with a front door in front of it: the key file is not the keychain's to hold, but the
        // gateway key is, and one answer says what to do with both.
        let plans = keep_plans(&keyed(LoginMethod::KeyFile), &remembered);
        assert_eq!(
            plans.login,
            KeepPlan::Nothing,
            "a device key was offered to the keychain for a key it never holds"
        );
        assert_eq!(
            plans.gateway_key,
            KeepPlan::Keep {
                account: door.to_string(),
                secret: Zeroizing::new("a front-door key".to_string())
            },
            "the front-door key was collected with no way to keep it"
        );

        // A password answer for a server reached directly keeps only the password: there is no front-door entry
        // for such a server, and writing one would leave a line in the keychain that nothing looks up.
        let only_a_password = SecretInput {
            password: Some(Zeroizing::new("hunter2".to_string())),
            remember: true,
            ..SecretInput::default()
        };
        let plans = keep_plans(&target(LoginMethod::Password), &only_a_password);
        assert_eq!(
            plans.login,
            KeepPlan::Keep {
                account: "alice@http://127.0.0.1:8090".to_string(),
                secret: Zeroizing::new("hunter2".to_string())
            }
        );
        assert_eq!(plans.gateway_key, KeepPlan::Nothing);

        // `Debug` redacts both halves the way it redacts one.
        let plans = keep_plans(&keyed(LoginMethod::Password), &remembered);
        assert_eq!(
            format!("{plans:?}"),
            "KeepPlans { login: KeepPlan::Nothing, gateway_key: KeepPlan::Keep(gateway-key:http://127.0.0.1:8090, <redacted>) }",
            "a plan repeated in a log line must not repeat the key"
        );
    }

    /// The same three answers, carried out against a fake keychain: one plan, at most one call, and the words
    /// the person can act on when the keychain will not answer.
    #[test]
    fn keeping_a_password_writes_it_once() {
        let account = "alice@http://127.0.0.1:8090";
        let remembered = SecretInput {
            password: Some(Zeroizing::new("hunter2".to_string())),
            remember: true,
            ..SecretInput::default()
        };

        let store = FakeStore::new();
        apply_keep(
            &store,
            &keep_plan(&target(LoginMethod::Password), &remembered),
        )
        .expect("the fake keychain takes it");
        assert_eq!(store.kept(account), Some("hunter2".to_string()));
        assert_eq!(store.asked(), vec!["set".to_string()]);

        // The same answer with the box left alone takes the entry out again.
        let store = FakeStore::new();
        store.put(account, "stale");
        apply_keep(
            &store,
            &keep_plan(&target(LoginMethod::Password), &not_remembered(&remembered)),
        )
        .expect("forgetting is an ordinary answer");
        assert_eq!(store.kept(account), None);

        // A plan that asks for nothing never reaches the keychain: a key login leaves it alone whatever the
        // form collected.
        let store = FakeStore::new();
        apply_keep(
            &store,
            &keep_plan(&target(LoginMethod::KeyFile), &remembered),
        )
        .expect("nothing was asked of the keychain");
        assert!(store.asked().is_empty(), "a key login opened the keychain");

        // A keychain that will not answer says so once, in the words the person can act on, and stores
        // nothing.
        let store = FakeStore::broken();
        let problem = apply_keep(
            &store,
            &keep_plan(&target(LoginMethod::Password), &remembered),
        )
        .expect_err("a locked keychain does not take a password");
        assert_eq!(problem, KEYCHAIN_UNAVAILABLE);
        assert!(store.kept(account).is_none(), "nothing was stored");
    }

    /// What the run does with a password the server said no to: it comes out of the keychain, so the next run
    /// asks instead of making the same refusal happen again.
    #[test]
    fn a_refused_password_is_taken_out_of_the_keychain() {
        let account = "alice@http://127.0.0.1:8090";
        let store = FakeStore::new();
        store.put(account, "wrong");

        forget_password(&store, &target(LoginMethod::Password))
            .expect("the fake keychain hands it back");
        assert_eq!(store.kept(account), None);
        assert_eq!(store.asked(), vec!["delete".to_string()]);

        // A login that signs some other way has no entry of ours to take out, so the keychain is not opened.
        let store = FakeStore::new();
        forget_password(&store, &target(LoginMethod::KeyFile))
            .expect("there was nothing to take out");
        assert!(store.asked().is_empty(), "a key login opened the keychain");
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
