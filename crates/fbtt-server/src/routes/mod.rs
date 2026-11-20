//! The routes featherbittorrent adds in front of fbt-api: logins, devices, folder browsing and the address
//! to stream one file from.
//!
//! Every one of them answers JSON, and every error has the body fbt-api's own errors have
//! (`{"error": {"code": ..., "message": ...}}`), so a client parses one shape for the whole API. They
//! are merged into fbt-api's router by [`fbt_api::ApiServer::start_with`], which means they arrive
//! with that server's layers on them: the [`fbt_api::Auth::Custom`] guard built from [`FbttAuth`]
//! decides who may reach them, and so do the request body limit and CORS. Only the two login calls are
//! exempt from a token, and only because [`FbttAuth`] says so - see [`crate::auth`].
//!
//! What more than one request needs to know lives in one [`Shared`], handed to every handler as axum
//! state: the session and challenge stores (the same ones the authenticator reads), the devices a key
//! login recognises, the folders the file routes may name, and the config file all of that came from.

mod devices;
// `pub(crate)` rather than private because [`Server`] prepares the allowed folders into this module's
// [`Root`]s before fbt-api is handed any of them, and it has to name the type to do that.
pub(crate) mod fs;
mod login;
mod stream;

use std::collections::BTreeMap;
// Only the test-support counters and the hold they update need these; the fields they belong to exist only
// with that feature too.
#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(feature = "test-support")]
use std::sync::{Condvar, OnceLock};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::DefaultBodyLimit;
use axum::extract::rejection::{BytesRejection, JsonRejection, PathRejection, QueryRejection};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use fbtt_core::{Config, DeviceEntry, Paths, UserEntry};
use futures_util::StreamExt;
use serde::Serialize;
use ssh_key::PublicKey;
#[cfg(feature = "test-support")]
use tokio::sync::watch;

use crate::auth::{FbttAuth, keys, locked};

/// Remote text before it goes into a response or a log line: a folder name read off the disk, a
/// device name from a config file, the words of a parse error about something a client sent. The
/// workspace rule is that remote text is display-safe, and these routes are where remote text first
/// reaches something a person will read.
pub(crate) fn clean(text: &str) -> String {
    fbt::display_safe(text, fbt::MAX_DISPLAY_CHARS)
}

/// At most this many Argon2 verifies run at once on one server.
///
/// A verify is expensive on purpose - about 19 MiB and a core's worth of time here - and it runs on the
/// blocking pool, which will start several hundred threads if nothing tells it not to. The login limiter
/// bounds how many attempts one *address* gets; this bounds what every address together can do to the
/// machine, which is what a distributed guess needs: one attempt per address, each inside its own budget,
/// and no cap at all in between them and memory. Four of them is under 80 MiB and a quarter of a busy
/// machine - enough to keep logins prompt while everything else on this box keeps running.
const MAX_PARALLEL_VERIFIES: usize = 4;

/// How many verifies to let run at once on this machine: one per core as the OS reports them, and never
/// more than [`MAX_PARALLEL_VERIFIES`] or fewer than one. A machine that cannot say gets one - the
/// pessimistic end, and the right one for something whose whole job is to slow a guesser down.
fn parallel_verifies() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|cores| cores.get())
        .unwrap_or(1);
    cores.clamp(1, MAX_PARALLEL_VERIFIES)
}

/// Who may log in, which devices the login route recognises, which folders the file routes may name, which
/// library the torrent routes answer about, and where the file all of that came from. One of these is built
/// per server and shared by every handler.
pub(crate) struct Shared {
    /// The session, challenge and limiter stores - the same ones the [`fbt_api::Authenticator`] fbt-api
    /// calls reads, because exactly one thing knows who is signed in.
    auth: Arc<FbttAuth>,
    /// The accounts a password logs in to, as this run read them. They change only with a restart:
    /// the commands that edit them refuse to while the library lock is held, and no route here edits
    /// them at all.
    users: Vec<UserEntry>,
    /// The devices a key login recognises, swapped whole whenever `config.yaml` is rewritten.
    devices: Mutex<DeviceTable>,
    /// One device change at a time: the routes read `config.yaml`, change one field of it and write it
    /// back, and two requests doing that at once would lose one another's device. Held across the disk
    /// work, which is why it is an async lock - nothing blocking runs inside it.
    changing_devices: tokio::sync::Mutex<()>,
    /// What this run started on, its folders already rooted in the library. The config to write back
    /// when `config.yaml` is not there yet, so that saving a device cannot invent a different library
    /// than the one being served.
    base_config: Config,
    /// Where `config.yaml` is.
    paths: Paths,
    /// The folders the file routes may name, as configured and canonicalised: what a request names is
    /// checked against both spellings, so a root reached through a symlink alias of its own still
    /// answers to itself. See [`fs`]. An `Arc<[..]>` rather than a `Vec` because listing a folder runs on
    /// the blocking pool and hands the roots no ownership it should not have to share.
    roots: Arc<[fs::Root]>,
    /// The library being served, for the routes that answer about a torrent ([`stream`]): which torrents
    /// there are, whether one's metadata has arrived, which files it has. fbt-api holds its own clone of the
    /// same session ([`crate::Server`]), and every clone is the same session, so the address this route
    /// hands out and the stream route that serves it look at one library. Not in the `Debug` printing below:
    /// what a session prints is every torrent in it, which is not what a `Debug` line here is for.
    session: fbt::Session,
    /// The hash an unknown user name is verified against, so that answering "no such account" costs
    /// what answering "wrong password" costs. `None` when no account could have been found anyway -
    /// with no users, or with nobody signing in, there is no name worth hiding. See [`login`].
    dummy_password_hash: Option<String>,
    /// How many Argon2 verifies may run at once, for the whole server: one per core, and never more than a
    /// handful ([`MAX_PARALLEL_VERIFIES`]). A verify is deliberately expensive - about 19 MiB here - and it
    /// runs on the blocking pool, which will happily start several hundred threads if nothing tells it to
    /// stop. The login limiter bounds how many attempts an *address* gets; this bounds what all of them
    /// together can make the machine do, which is what a distributed guess - one attempt per address, and
    /// no address over its budget - would otherwise find. An `Arc` because the permit has to be able to
    /// outlive the request that asked for it ([`login::password_login`]).
    verify_permits: Arc<tokio::sync::Semaphore>,
    /// How many Argon2 verifications this server has run. Nothing in a run reads it; the test that
    /// proves an unknown user name still costs a verify does.
    #[cfg(feature = "test-support")]
    verifies: AtomicU64,
    /// How many verifies are running right now, and the most there have been at once: the pair of counters
    /// a test reads to show that [`parallel_verifies`] is what actually bounds the work rather than just
    /// what it asks for. Nothing in a run reads either.
    #[cfg(feature = "test-support")]
    verifying: AtomicU64,
    #[cfg(feature = "test-support")]
    most_verifying: AtomicU64,
    /// The place inside the verify that a test can put there to hold a check in it ([`VerifyHold`]), so
    /// that the test counting permits does not have to guess how long a hash takes. Set by
    /// [`Server::park_verifies`]; absent in every run that is not one.
    #[cfg(feature = "test-support")]
    verify_hold: OnceLock<VerifyHold>,
}

impl Shared {
    /// The state for one server: its own login stores, its devices, the folders `config` names and the
    /// library it is serving.
    ///
    /// `roots` are those folders as canonicalised directories - the same list
    /// [`fbt_api::ApiConfig::allowed_roots`] is handed - and `dummy_password_hash` is the hash computed
    /// once at startup for the accounts this config has, or `None` when it has none worth hiding behind.
    /// `session` is the engine's own session, cloned: what the torrent routes answer about.
    pub(crate) fn new(
        config: Config,
        paths: Paths,
        roots: Vec<fs::Root>,
        dummy_password_hash: Option<String>,
        session: fbt::Session,
    ) -> Self {
        // The login stores are built from the server section; a config with none is the defaults, which is
        // what `Server::build` serves on in the same case.
        let server = config.server.clone().unwrap_or_default();
        let devices = Mutex::new(DeviceTable::build(&server.auth.devices));
        Shared {
            auth: Arc::new(FbttAuth::new(&server)),
            users: server.auth.users.clone(),
            devices,
            changing_devices: tokio::sync::Mutex::new(()),
            base_config: config,
            paths,
            roots: roots.into(),
            session,
            dummy_password_hash,
            verify_permits: Arc::new(tokio::sync::Semaphore::new(parallel_verifies())),
            #[cfg(feature = "test-support")]
            verifies: AtomicU64::new(0),
            #[cfg(feature = "test-support")]
            verifying: AtomicU64::new(0),
            #[cfg(feature = "test-support")]
            most_verifying: AtomicU64::new(0),
            #[cfg(feature = "test-support")]
            verify_hold: OnceLock::new(),
        }
    }

    /// The authenticator for these stores, which is what fbt-api is handed beside the routes: the guard
    /// that decides who may reach them and the handlers that hand out tokens read the same sessions, so a
    /// token issued here is accepted there and nowhere else.
    pub(crate) fn authenticator(&self) -> Arc<FbttAuth> {
        Arc::clone(&self.auth)
    }

    /// The library being served: what [`stream`] answers a torrent question about. A clone of it is another
    /// handle on the same session, which is how this route and fbt-api's own routes agree about which
    /// torrents exist.
    pub(crate) fn session(&self) -> &fbt::Session {
        &self.session
    }

    /// The device named by `fingerprint`: the name to give a successful login, and the public key from
    /// the config that its signature has to satisfy. A key only the client sent is never trusted -
    /// this is the only way a route reaches a verification key.
    pub(crate) fn device(&self, fingerprint: &str) -> Option<(String, PublicKey)> {
        let devices = locked(&self.devices);
        devices
            .by_fingerprint
            .get(fingerprint)
            .map(|(name, key)| (name.clone(), key.clone()))
    }

    /// Whether `fingerprint` names a device this server knows, which is what decides whether a nonce
    /// for it is worth storing. See [`login::challenge`].
    pub(crate) fn has_device(&self, fingerprint: &str) -> bool {
        let devices = locked(&self.devices);
        devices.by_fingerprint.contains_key(fingerprint)
    }

    /// The devices to show, in the order the config has them, with every name cleaned for display.
    ///
    /// A name this server's own route added is already display-safe - adding one refuses anything that is
    /// not - so cleaning changes nothing for it. What it covers is a name someone put into `config.yaml` by
    /// hand: by the time an API response repeats it, it is remote text, and a bidi override in a device
    /// name is how a list lies about what is in it. Such a name shows as a display would draw it; removing
    /// it means naming it as the file spells it, which is what the person who wrote that file has.
    pub(crate) fn device_rows(&self) -> Vec<DeviceRow> {
        let devices = locked(&self.devices);
        devices
            .entries
            .iter()
            .map(|row| DeviceRow {
                name: clean(&row.name),
                fingerprint: row.fingerprint.clone(),
            })
            .collect()
    }

    /// The hash a password for `name` is checked against: the account's, or this server's fixed dummy
    /// when nobody has that name (see [`login`]). `None` means nothing could match it and there is no
    /// name to hide, so the answer can be given without hashing anything.
    pub(crate) fn password_hash_for(&self, name: &str) -> Option<String> {
        let known = self
            .users
            .iter()
            .find(|user| user.name == name)
            .map(|user| user.password_hash.clone());
        known.or_else(|| self.dummy_password_hash.clone())
    }

    /// Counts one Argon2 verification as having run, and records how many are running now against the most
    /// this server has had at once. Under `test-support` that is what lets a test prove an unknown user name
    /// still costs a verify, and that the bound in [`parallel_verifies`] is real rather than asked for; in a
    /// run nothing reads any of these counts, so this records nothing and the fields it would update are not
    /// even there. See [`crate::Server::password_verifies`] and [`crate::Server::max_concurrent_verifies`].
    pub(crate) fn verify_started(&self) {
        #[cfg(feature = "test-support")]
        {
            self.verifies.fetch_add(1, Ordering::Relaxed);
            let running = self.verifying.fetch_add(1, Ordering::Relaxed) + 1;
            self.most_verifying.fetch_max(running, Ordering::Relaxed);
        }
    }

    /// How many Argon2 verifications this server has run since it started. Under `test-support` this is
    /// how the test that says "an unknown user name costs a verify" reads the answer; in a run nothing
    /// counts and so there is nothing to read. See [`crate::Server::password_verifies`].
    #[cfg(feature = "test-support")]
    pub(crate) fn password_verifies(&self) -> u64 {
        self.verifies.load(Ordering::Relaxed)
    }

    /// Says the verify [`verify_started`](Self::verify_started) counted has finished. A verify that panicked
    /// would leave the count one too high: argon2's verify does not panic, nothing in a run reads the
    /// number, and the permit itself is returned by a `Drop` whichever way this ends.
    pub(crate) fn verify_finished(&self) {
        #[cfg(feature = "test-support")]
        self.verifying.fetch_sub(1, Ordering::Relaxed);
    }

    /// The most Argon2 verifies this server has had running at once. Under `test-support` this is how a test
    /// checks that [`parallel_verifies`] bounds the work when logins arrive in a burst instead of merely
    /// asking it to; in a run nothing counts, so there is nothing to read. See
    /// [`crate::Server::max_concurrent_verifies`].
    #[cfg(feature = "test-support")]
    pub(crate) fn max_concurrent_verifies(&self) -> u64 {
        self.most_verifying.load(Ordering::Relaxed)
    }

    /// Stops this check inside the verify until a test lets it go, if a test armed a hold ([`Server::park_verifies`],
    /// under `test-support`). Called from inside the blocking job, after [`verify_started`](Self::verify_started)
    /// and before any hashing, so what is parked there is a check that is holding one of this server's permits.
    /// In a run nothing has armed a hold, so there is nowhere to stop and this does nothing - which is the only
    /// cost it ever has.
    pub(crate) fn park_inside_verify(&self) {
        #[cfg(feature = "test-support")]
        {
            if let Some(hold) = self.verify_hold.get() {
                hold.arrive_and_wait();
            }
        }
    }

    /// Arms [`park_inside_verify`](Self::park_inside_verify) and returns the count it moves: how many checks
    /// are sitting inside the verify right now, with a change to wait for. Under `test-support` only, and
    /// arming it twice changes nothing about the checks - both watchers see the same count. See
    /// [`Server::park_verifies`].
    #[cfg(feature = "test-support")]
    pub(crate) fn park_verifies(&self) -> watch::Receiver<u64> {
        let hold = self.verify_hold.get_or_init(VerifyHold::new);
        hold.counts.subscribe()
    }

    /// Lets every parked check finish, and stops holding the ones that come after ([`park_verifies`](Self::park_verifies)).
    /// Under `test-support` only.
    #[cfg(feature = "test-support")]
    pub(crate) fn release_parked_verifies(&self) {
        if let Some(hold) = self.verify_hold.get() {
            hold.release();
        }
    }
}

/// A place inside the Argon2 check that only a test can put there: a check that reaches it says how many are
/// sitting there and then waits until somebody opens it. The waiting is a condvar because it happens on the
/// blocking pool, where nothing may await; the count is a `watch` because whoever reads it is an async task
/// sharing this runtime's only worker with the server, so it can afford to be told rather than to sit and
/// poll. Without `test-support` none of this exists: no test arms a hold, and `Shared` carries neither the
/// field nor the wait.
#[cfg(feature = "test-support")]
#[derive(Debug)]
struct VerifyHold {
    /// How many checks are parked here right now. Every arrival bumps it and sends the new number, so a
    /// watcher reads a count and waits for the next change instead of polling - which is how a test counts
    /// what is holding the permits without guessing at how long a hash takes on somebody else's machine.
    parked: AtomicU64,
    counts: watch::Sender<u64>,
    /// `false` while checks are being held; [`VerifyHold::release`] makes it `true` for good, so the checks
    /// that arrive afterwards run through without stopping. A condvar rather than a channel because the
    /// people waiting here are not on a runtime: they are on the blocking pool, where nothing may await.
    open: Mutex<bool>,
    waiting: Condvar,
}

#[cfg(feature = "test-support")]
impl VerifyHold {
    /// A hold nobody has opened yet: everything that arrives waits here until a test says otherwise.
    fn new() -> Self {
        let (counts, _) = watch::channel(0);
        VerifyHold {
            parked: AtomicU64::new(0),
            counts,
            open: Mutex::new(false),
            waiting: Condvar::new(),
        }
    }

    /// Says this check has reached the inside of the verify, then waits there for the test to stop looking.
    fn arrive_and_wait(&self) {
        let parked = self.parked.fetch_add(1, Ordering::Relaxed) + 1;
        // The number is what a watcher reads; whether anybody is still watching to be told is not its job.
        let _ = self.counts.send(parked);
        let mut open = locked(&self.open);
        while !*open {
            open = self
                .waiting
                .wait(open)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Opens the hold for good: everyone waiting here goes on, and so does anything that arrives later.
    fn release(&self) {
        let mut open = locked(&self.open);
        *open = true;
        self.waiting.notify_all();
    }
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Counts and nothing else: no password hash and no device key belongs in a `Debug` printing
        // that ends up beside a panic message.
        let devices = locked(&self.devices);
        f.debug_struct("Shared")
            .field("auth", &self.auth)
            .field("users", &self.users.len())
            .field("devices", &devices.entries.len())
            .field("roots", &self.roots.len())
            .finish()
    }
}

/// The devices a key login checks against: the config's list, plus the same entries looked up by the
/// fingerprint of the key they name. A table rather than a scan because the login route is asked for a
/// fingerprint by someone who may not be in the list at all, and it is asked on every key login.
#[derive(Debug)]
struct DeviceTable {
    /// The entries as `config.yaml` has them, in file order: what `GET /api/v1/auth/devices` lists.
    entries: Vec<DeviceRow>,
    /// Fingerprint to the name and key of the entry that names it. A key listed under two names answers
    /// to the first, which is the one a person reading the list from the top would pick.
    by_fingerprint: BTreeMap<String, (String, PublicKey)>,
}

/// One device as the API shows it: what it was named, and the short thing a person checks before
/// trusting that name.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DeviceRow {
    /// What the person named this device.
    name: String,
    /// The key's fingerprint, `SHA256:<base64>`, the short form of it a person checks by eye.
    fingerprint: String,
}

impl DeviceTable {
    /// The table for `entries`. An entry whose key is not a usable Ed25519 key cannot be listed - its
    /// fingerprint cannot even be computed - and cannot log in, so it is skipped rather than
    /// half-accepted, and said out loud from here: the two places that build a table both want to say
    /// the same line, and the person to tell is whoever edited the file.
    fn build(entries: &[DeviceEntry]) -> DeviceTable {
        let mut rows = Vec::with_capacity(entries.len());
        let mut by_fingerprint = BTreeMap::new();
        for entry in entries {
            match keys::parse_device_key(&entry.public_key) {
                Ok(key) => {
                    let fingerprint = keys::fingerprint(&key);
                    rows.push(DeviceRow {
                        name: entry.name.clone(),
                        fingerprint: fingerprint.clone(),
                    });
                    by_fingerprint
                        .entry(fingerprint)
                        .or_insert_with(|| (entry.name.clone(), key.clone()));
                }
                Err(problem) => tracing::warn!(
                    device = %clean(&entry.name),
                    problem = %clean(&problem.to_string()),
                    "the key this device names cannot be used; the entry is ignored until the config is fixed",
                ),
            }
        }
        DeviceTable {
            entries: rows,
            by_fingerprint,
        }
    }
}

/// An error one of these routes answers with, in the shape fbt-api's errors have: a status, a
/// snake_case code from the same vocabulary (`invalid_request`, `unauthorized`, `path_not_allowed`,
/// `forbidden`, `not_found`, `already_exists`, `too_many_requests`, `busy`, `internal`) and a message.
#[derive(Debug)]
pub(crate) struct RouteError {
    status: StatusCode,
    code: &'static str,
    message: String,
    /// `Retry-After` delay-seconds to send, when the client should come back.
    retry_after_secs: Option<u64>,
}

impl RouteError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        RouteError {
            status,
            code,
            message: message.into(),
            retry_after_secs: None,
        }
    }

    /// `400 invalid_request`: the body or query is not shaped like what the route takes.
    fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    /// `401 unauthorized`: the credential was well-formed and is not this server's.
    fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "the credentials are not valid",
        )
    }

    /// `403 path_not_allowed`: a folder outside the roots this server confines itself to. The path is
    /// the client's own text, cleaned on the way back out.
    fn path_not_allowed(path: &str) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "path_not_allowed",
            format!("{} is outside the allowed folders", clean(path)),
        )
    }

    /// `403 forbidden`: inside a root, but this server is not allowed to look inside it.
    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    /// `404 not_found`.
    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// `409 already_exists`: a name the list already has.
    fn already_exists(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "already_exists", message)
    }

    /// `429 too_many_requests`, with `Retry-After`.
    fn too_many_requests(retry_after_secs: u64) -> Self {
        let mut error = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests",
            "too many requests, retry later",
        );
        error.retry_after_secs = Some(retry_after_secs);
        error
    }

    /// `503 busy`, with `Retry-After: 1`: this server could answer, but it is in the middle of too many of
    /// something right now. It is fbt-api's own spelling for that answer - too many open streams, too many
    /// torrents being added at once - so a client that already knows to try again on `busy` needs nothing
    /// new to learn here.
    fn busy(message: impl Into<String>) -> Self {
        let mut error = Self::new(StatusCode::SERVICE_UNAVAILABLE, "busy", message);
        error.retry_after_secs = Some(1);
        error
    }

    /// `500 internal`: something this server could not do, said as briefly as it can be.
    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    /// An extractor's own refusal - a body over the limit, a query string that does not parse - put in
    /// this shape instead of axum's plain-text one: `413 body_too_large`, a server-side failure as
    /// `500 internal`, anything else `400 invalid_request`. Same words fbt-api uses for the same
    /// failures, so a client that got one shape from `/api/v1/torrents` gets it from `/api/v1/fs/list` too.
    fn rejection(status: StatusCode, message: String) -> Self {
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            Self::new(status, "body_too_large", message)
        } else if status.is_server_error() {
            Self::internal(message)
        } else {
            Self::invalid_request(message)
        }
    }
}

impl From<JsonRejection> for RouteError {
    fn from(problem: JsonRejection) -> Self {
        Self::rejection(problem.status(), problem.body_text())
    }
}

impl From<BytesRejection> for RouteError {
    fn from(problem: BytesRejection) -> Self {
        Self::rejection(problem.status(), problem.body_text())
    }
}

impl From<QueryRejection> for RouteError {
    fn from(problem: QueryRejection) -> Self {
        Self::rejection(problem.status(), problem.body_text())
    }
}

impl From<PathRejection> for RouteError {
    fn from(problem: PathRejection) -> Self {
        Self::rejection(problem.status(), problem.body_text())
    }
}

/// A request body of at most `keep` bytes, or `413 body_too_large` once the client has finished sending one
/// that is bigger - as long as it finished within `drain`. A body declared bigger than `drain` is refused
/// before any of it is read, and one that turns out bigger stops being read there: either way the
/// connection that carried it is over.
///
/// The cap is enforced by the reader rather than by an extractor, because an extractor stops reading at its
/// limit and answers with the rest of the request still unread: the operating system turns a connection
/// closed that way into a reset, and a client still writing sees a failed send rather than the `413` that
/// was already on its way. Reading a refused body to its end - within `drain` - is what lets it see the
/// answer. `what` names the body in that `413`'s message, in the words the route that called it uses.
///
/// Nothing past `keep` is kept either way, so the room a request may make this server hold does not change
/// when a body is refused: `drain` is only how far the reading goes, not how much is held.
pub(crate) async fn read_capped(
    headers: &HeaderMap,
    body: Body,
    keep: usize,
    drain: usize,
    what: &str,
) -> Result<Bytes, RouteError> {
    let too_large = || {
        RouteError::rejection(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("{what} is at most {keep} bytes"),
        )
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > drain as u64) {
        return Err(too_large());
    }
    let mut stream = body.into_data_stream();
    let mut kept = Vec::new();
    let mut read = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| RouteError::invalid_request("the request body could not be read"))?;
        read = read.saturating_add(chunk.len());
        if read > drain {
            return Err(too_large());
        }
        if read <= keep {
            kept.extend_from_slice(&chunk);
        }
    }
    if read > keep {
        return Err(too_large());
    }
    Ok(Bytes::from(kept))
}

impl IntoResponse for RouteError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::debug!(code = self.code, message = %self.message, "request failed");
        }
        let mut response = (
            self.status,
            axum::Json(ErrorBody {
                error: ErrorDetail {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response();
        if let Some(secs) = self.retry_after_secs {
            // `Retry-After` as delay-seconds; the `http` types render a u64 the way fbt-api does.
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, secs.into());
        }
        response
    }
}

/// The body of every error response: fbt-api's two fields, so one parse handles the whole API.
#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

/// What went wrong, in the same words fbt-api uses for it.
#[derive(Debug, Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
}

/// The routes featherbittorrent adds, on paths fbt-api does not serve.
///
/// [`fbt_api::ApiServer::start_with`] merges these ahead of its auth layer, body limit and CORS, so
/// they arrive with all three; axum panics if one of these paths were also one fbt-api serves.
pub(crate) fn router(shared: Arc<Shared>) -> Router {
    // The two login calls read their bodies under their own cap, far smaller than the 32 MiB fbt-api keeps
    // for uploading a `.torrent`: they are the two places this server takes bytes from someone it has not
    // yet decided is anybody, and all three fields of a key login fit in a few hundred bytes. The limit
    // still on the challenge route bounds its inner `Json` re-extraction - of bytes already kept under it,
    // so it never refuses; on the session route the handler uses no body extractor, so it bounds nothing, and
    // it stays only so a future extractor there inherits the same cap.
    let logins = Router::new()
        .route(
            "/api/v1/auth/challenge",
            post(login::challenge).layer(DefaultBodyLimit::max(login::MAX_LOGIN_BODY)),
        )
        .route(
            "/api/v1/auth/session",
            post(login::session)
                .delete(login::logout)
                .layer(DefaultBodyLimit::max(login::MAX_LOGIN_BODY)),
        );
    Router::new()
        .merge(logins)
        .route(
            "/api/v1/auth/devices",
            get(devices::list).post(devices::add),
        )
        .route("/api/v1/auth/devices/{name}", delete(devices::remove))
        .route("/api/v1/fs/roots", get(fs::roots))
        .route("/api/v1/fs/list", get(fs::list))
        // The one route here that hands out a credential, so it needs a session exactly like everything else
        // that is not a login. Its body is two short fields and fbt-api's 32 MiB is room for uploading a
        // `.torrent`, so it gets a cap of its own, which the route reads with in [`read_capped`]: with logins
        // off this route answers whoever reaches the address, and a megabyte of JSON naming one file is not
        // a request worth buffering. The limit still on the route only bounds the handler's inner `Json`
        // re-extraction of bytes it already kept under the same cap, so a second refusal there is impossible.
        .route(
            "/api/v1/stream-tokens",
            post(stream::issue).layer(DefaultBodyLimit::max(stream::MAX_TOKEN_BODY)),
        )
        .with_state(shared)
}
