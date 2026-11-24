//! The routes featherbittorrent adds in front of fbt-api: logins, devices, folder browsing and the address
//! to stream one file from.
//!
//! Every one of them answers JSON, and every error has the body fbt-api's own errors have
//! (`{"error": {"code": ..., "message": ...}}`), so a client parses one shape for the whole API. They
//! are merged into fbt-api's router by [`fbt_api::ApiServer::start_with`], which means they arrive
//! with that server's layers on them: the [`fbt_api::Auth::Custom`] guard built from [`FbttAuth`]
//! decides who may reach them, and so do the request body limit and CORS. Only the three login calls are
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
mod media;
mod stream;
mod web_session;

use std::collections::BTreeMap;
use std::io;
// The media read timeout's own field needs these, and so do the test-support counters and the hold they
// update: the counters' fields exist only with that feature, the timeout's exist always.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(feature = "test-support")]
use std::sync::{Condvar, OnceLock};
use std::time::Duration;

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
    /// outlive the request that asked for it ([`login::verify_password`]).
    verify_permits: Arc<tokio::sync::Semaphore>,
    /// What the checks report to while they run: the counts nothing in a run reads and the hold no run
    /// arms. Its own `Arc`, because a check carries it down to the blocking pool with its permit - see
    /// [`VerifyTally`] for why both the permit and this have to travel with the work rather than with the
    /// request, and [`login::verify_password`] for the check that does it.
    verify_tally: Arc<VerifyTally>,
    /// How many of the media routes may run at once, across all four of them: [`media::MEDIA_PERMITS`].
    ///
    /// Every media request is a thread on the blocking pool holding a container demuxer, a file reader and
    /// the read-ahead the engine keeps warm for it, which is nothing the login limiter bounds and only
    /// partly what the engine's own bound of readers per torrent bounds. The same reason [`Self`] carries
    /// `verify_permits` is the reason for this one: the pool will start as many threads as it is asked to,
    /// and the cost of a media request is not the handler's but the job's. An `Arc` for the same reason
    /// again - the stream route's permit has to outlive its handler, because it is the job that plays the
    /// file ([`media`]).
    media_permits: Arc<tokio::sync::Semaphore>,
    /// How long one read or one seek of a media route may take, in milliseconds: [`media::READ_TIMEOUT`]
    /// until [`Server::set_media_read_timeout`](crate::Server) says otherwise.
    ///
    /// Milliseconds in an `AtomicU64` rather than a `Mutex<Duration>` because the two things that touch it
    /// have no reason to wait for one another: a handler reads it once, when it builds its reader, and a
    /// test or an operator writes it whenever they want the next request to give up sooner. A media route
    /// takes its deadline when it opens the file and keeps it for the life of that reader, which is the
    /// honest meaning of "changing the timeout": requests already playing finish on the window they started
    /// with.
    media_read_ms: AtomicU64,
}

impl Shared {
    /// The state for one server: its own login stores, its devices, the folders `config` names and the
    /// library it is serving.
    ///
    /// `roots` are those folders as canonicalised directories - the same list
    /// [`fbt_api::ApiConfig::allowed_roots`] is handed - and `dummy_password_hash` is the hash computed
    /// once at startup for the accounts this config has, or `None` when it has none worth hiding behind.
    /// `session` is the engine's own session, cloned: what the torrent routes answer about. `launch` is the
    /// desktop app's launch secret and whether it is the only way in, for an engine the app started
    /// ([`crate::DesktopAccess`]), and `None` for any other server.
    pub(crate) fn new(
        config: Config,
        paths: Paths,
        roots: Vec<fs::Root>,
        dummy_password_hash: Option<String>,
        session: fbt::Session,
        launch: Option<(String, bool)>,
    ) -> Self {
        // The login stores are built from the server section; a config with none is the defaults, which is
        // what `Server::build` serves on in the same case.
        let server = config.server.clone().unwrap_or_default();
        let devices = Mutex::new(DeviceTable::build(&server.auth.devices));
        Shared {
            auth: Arc::new(match launch {
                Some((secret, only)) => FbttAuth::new(&server).with_launch_secret(secret, only),
                None => FbttAuth::new(&server),
            }),
            users: server.auth.users.clone(),
            devices,
            changing_devices: tokio::sync::Mutex::new(()),
            base_config: config,
            paths,
            roots: roots.into(),
            session,
            dummy_password_hash,
            verify_permits: Arc::new(tokio::sync::Semaphore::new(parallel_verifies())),
            verify_tally: Arc::new(VerifyTally::new()),
            media_permits: Arc::new(tokio::sync::Semaphore::new(media::MEDIA_PERMITS)),
            media_read_ms: AtomicU64::new(media::READ_TIMEOUT.as_millis() as u64),
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

    /// The pool the media routes take a place from before they start demuxing: `media`'s `media_permit` is
    /// what asks it for one.
    ///
    /// An `Arc` clone rather than a borrow: the stream route hands its permit to the blocking job that
    /// writes its response, and that job outlives the handler that started it, so the permit cannot be
    /// borrowing anything the handler owns ([`media`]).
    pub(crate) fn media_permits(&self) -> Arc<tokio::sync::Semaphore> {
        Arc::clone(&self.media_permits)
    }

    /// How long one read or one seek of a media route may take: [`media::READ_TIMEOUT`] until
    /// [`set_media_read_timeout`](Self::set_media_read_timeout) says otherwise. A media route asks once, when
    /// it opens its reader, and keeps that answer for the whole request.
    pub(crate) fn media_read_timeout(&self) -> Duration {
        Duration::from_millis(self.media_read_ms.load(Ordering::Relaxed))
    }

    /// Sets that window, for every media request that opens a file from now on.
    ///
    /// Nothing in a normal run changes it; the reason it is not a config key is that the window is not a
    /// preference about a library, it is about how long a request is willing to wait on a peer, and a test
    /// that wants to watch a stream give up cannot afford a minute per attempt. A request already playing
    /// keeps the window it started with ([`Self::media_read_timeout`]).
    pub(crate) fn set_media_read_timeout(&self, timeout: Duration) {
        let millis = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        self.media_read_ms.store(millis, Ordering::Relaxed);
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

    /// How many Argon2 verifications this server has run since it started. Under `test-support` this is
    /// how the test that says "an unknown user name costs a verify" reads the answer; in a run nothing
    /// counts and so there is nothing to read. See [`crate::Server::password_verifies`].
    #[cfg(feature = "test-support")]
    pub(crate) fn password_verifies(&self) -> u64 {
        self.verify_tally.password_verifies()
    }

    /// The most Argon2 verifies this server has had running at once. Under `test-support` this is how a test
    /// checks that [`parallel_verifies`] bounds the work when logins arrive in a burst instead of merely
    /// asking it to; in a run nothing counts, so there is nothing to read. See
    /// [`crate::Server::max_concurrent_verifies`].
    #[cfg(feature = "test-support")]
    pub(crate) fn max_concurrent_verifies(&self) -> u64 {
        self.verify_tally.max_concurrent_verifies()
    }

    /// Arms [`VerifyTally::park_inside_verify`] and returns the count it moves: how many checks are sitting
    /// inside the verify right now, with a change to wait for. Under `test-support` only, and arming it
    /// twice changes nothing about the checks - both watchers see the same count. See
    /// [`Server::park_verifies`].
    #[cfg(feature = "test-support")]
    pub(crate) fn park_verifies(&self) -> watch::Receiver<u64> {
        self.verify_tally.park()
    }

    /// Lets every parked check finish, and stops holding the ones that come after ([`Self::park_verifies`]).
    /// Under `test-support` only.
    #[cfg(feature = "test-support")]
    pub(crate) fn release_parked_verifies(&self) {
        self.verify_tally.release_parked();
    }
}

/// What the Argon2 checks report to while they run: the counts no run reads, and the hold no run arms.
///
/// A separate `Arc` rather than fields on [`Shared`] for one reason: a check has to carry its reporting
/// down to the blocking pool with the permit it holds, because it is the work - not the request - that
/// makes the memory and takes the time (see [`login::verify_password`]). The permit must be able to
/// outlive the handler for the same reason, and it is the check's task that gets parked, not the
/// handler's, so the hold lives beside it rather than beside the state the handler borrows.
#[derive(Debug)]
pub(crate) struct VerifyTally {
    /// How many checks have run since the server started, under `test-support`. See
    /// [`Shared::password_verifies`].
    #[cfg(feature = "test-support")]
    verifies: AtomicU64,
    /// How many are running right now, and the most there has ever been at once, under `test-support`.
    /// See [`Shared::max_concurrent_verifies`].
    #[cfg(feature = "test-support")]
    verifying: AtomicU64,
    #[cfg(feature = "test-support")]
    most_verifying: AtomicU64,
    /// The hold a test arms, and nobody else ever does. A `OnceLock` because arming it is a one-way door:
    /// once checks are being held, the checks that arrive after the release run through, so there is
    /// nothing to re-arm and nothing to reset.
    #[cfg(feature = "test-support")]
    hold: OnceLock<VerifyHold>,
}

impl VerifyTally {
    /// A tally no test has armed a hold on, which is what every run has.
    fn new() -> Self {
        Self {
            #[cfg(feature = "test-support")]
            verifies: AtomicU64::new(0),
            #[cfg(feature = "test-support")]
            verifying: AtomicU64::new(0),
            #[cfg(feature = "test-support")]
            most_verifying: AtomicU64::new(0),
            #[cfg(feature = "test-support")]
            hold: OnceLock::new(),
        }
    }

    /// Counts one Argon2 verification as having run, and records how many are running now against the most
    /// this server has had at once. Under `test-support` that is what lets a test prove an unknown user name
    /// still costs a verify, and that the bound in [`parallel_verifies`] is real rather than asked for; in a
    /// run nothing reads any of these counts, so this records nothing and the counts it would update are not
    /// even there. See [`Server::password_verifies`] and [`Server::max_concurrent_verifies`].
    pub(crate) fn verify_started(&self) {
        #[cfg(feature = "test-support")]
        {
            self.verifies.fetch_add(1, Ordering::Relaxed);
            let running = self.verifying.fetch_add(1, Ordering::Relaxed) + 1;
            self.most_verifying.fetch_max(running, Ordering::Relaxed);
        }
    }

    /// Says the verify [`verify_started`](Self::verify_started) counted has finished. A verify that panicked
    /// would leave the count one too high: argon2's verify does not panic, nothing in a run reads the
    /// number, and the permit itself is returned by a `Drop` whichever way this ends.
    pub(crate) fn verify_finished(&self) {
        #[cfg(feature = "test-support")]
        self.verifying.fetch_sub(1, Ordering::Relaxed);
    }

    /// Stops this check inside the verify until a test lets it go, if a test armed a hold
    /// ([`Server::park_verifies`], under `test-support`). Called from inside the blocking job, after
    /// [`verify_started`](Self::verify_started) and before any hashing, so what is parked there is a check
    /// that is holding one of this server's permits. In a run nothing has armed a hold, so there is
    /// nowhere to stop and this does nothing - which is the only cost it ever has.
    pub(crate) fn park_inside_verify(&self) {
        #[cfg(feature = "test-support")]
        {
            if let Some(hold) = self.hold.get() {
                hold.arrive_and_wait();
            }
        }
    }

    /// How many verifications have run, read from [`verify_started`](Self::verify_started). Under
    /// `test-support` only, because in a run nothing counts.
    #[cfg(feature = "test-support")]
    fn password_verifies(&self) -> u64 {
        self.verifies.load(Ordering::Relaxed)
    }

    /// The most verifications run at once, read from [`verify_started`](Self::verify_started). Under
    /// `test-support` only, because in a run nothing counts.
    #[cfg(feature = "test-support")]
    fn max_concurrent_verifies(&self) -> u64 {
        self.most_verifying.load(Ordering::Relaxed)
    }

    /// Arms [`park_inside_verify`](Self::park_inside_verify) and returns the count it moves. Under
    /// `test-support` only.
    #[cfg(feature = "test-support")]
    fn park(&self) -> watch::Receiver<u64> {
        let hold = self.hold.get_or_init(VerifyHold::new);
        hold.counts.subscribe()
    }

    /// Lets every parked check finish, and stops holding the ones that come after
    /// ([`park`](Self::park)). Under `test-support` only.
    #[cfg(feature = "test-support")]
    fn release_parked(&self) {
        if let Some(hold) = self.hold.get() {
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
/// `forbidden`, `not_found`, `request_timeout`, `already_exists`, `too_many_requests`, `busy`,
/// `internal`, and `unsupported_media_type`, which no fbt-api route answers but which is spelled the
/// same way) and a message.
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
    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// `408 request_timeout`: a request body fbt-lib stopped reading because it arrived too slowly.
    /// fbt-api's own routes answer this same client this way, and so do these: the two login calls
    /// and the stream-token call read their bodies through [`read_capped`], which sorts a body-read
    /// failure from a broken one with [`is_body_timeout`].
    fn request_timeout() -> Self {
        Self::new(
            StatusCode::REQUEST_TIMEOUT,
            "request_timeout",
            "the request body arrived too slowly",
        )
    }

    /// `409 already_exists`: a name the list already has.
    fn already_exists(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "already_exists", message)
    }

    /// `415 unsupported_media_type`: the body is not the media type this route reads. Only the web sign-in
    /// answers it - it is the one place here that reads JSON from someone it has not decided is anybody yet,
    /// and the other two login calls take whatever they are handed and refuse a wrong shape as
    /// `400 invalid_request`, which is what they have always answered.
    fn unsupported_media_type(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            message,
        )
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
    pub(crate) fn internal(message: impl Into<String>) -> Self {
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
        // serde's message quotes what the client sent (an unknown field's name, a string it could not read),
        // so it is remote text like any other on its way into a response.
        Self::rejection(problem.status(), clean(&problem.body_text()))
    }
}

impl From<BytesRejection> for RouteError {
    fn from(problem: BytesRejection) -> Self {
        // Remote text, like the JSON rejection's: it can quote what the client sent.
        Self::rejection(problem.status(), clean(&problem.body_text()))
    }
}

impl From<QueryRejection> for RouteError {
    fn from(problem: QueryRejection) -> Self {
        // Remote text, like the JSON rejection's: it can quote what the client sent.
        Self::rejection(problem.status(), clean(&problem.body_text()))
    }
}

impl From<PathRejection> for RouteError {
    fn from(problem: PathRejection) -> Self {
        // Remote text, like the JSON rejection's: it can quote what the client sent.
        Self::rejection(problem.status(), clean(&problem.body_text()))
    }
}

/// Whether `err` is the error of a body that arrived too slowly: whether its `source()` chain,
/// starting at `err` itself, holds an [`io::Error`] of kind [`io::ErrorKind::TimedOut`], what fbt-lib
/// leaves there when it cuts a body off at its deadline, however many layers of axum wrap it. The
/// same walk fbt-api's `error.rs` makes to give the same answer on its own routes.
fn is_body_timeout(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut node = Some(err);
    while let Some(cur) = node {
        if let Some(io_err) = cur.downcast_ref::<io::Error>()
            && io_err.kind() == io::ErrorKind::TimedOut
        {
            return true;
        }
        node = cur.source();
    }
    false
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
        // A chunk that arrives too slowly is fbt-lib's deadline answering, not a client that sent a
        // broken body: it gets the `408` fbt-api's routes give for the same client, not a `400`.
        let chunk = chunk.map_err(|err| {
            if is_body_timeout(&err) {
                RouteError::request_timeout()
            } else {
                RouteError::invalid_request("the request body could not be read")
            }
        })?;
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
///
/// On the desktop app's own engine that serves nobody else, the login routes are not mounted at all: the
/// launch secret is the only credential there, so no session can be asked for and those paths answer `404`.
///
/// And no route may be added here - or on fbt-api's router - that catches what every other route missed
/// (`Router::fallback`, a `/{*rest}` nest) while that mode stands: its auth accepts a request that matched
/// no route without asking for any credential (`FbttAuth`'s `matched_path.is_none()` shortcut in
/// `crate::auth`), and that is safe only because an unmatched path really has nothing behind it and gets
/// the empty `404` it is shown. A fallback would make every guessed path answer something to a request
/// that never presented the secret; `secret_only_unknown_path_is_an_empty_404` in `tests/desktop_access.rs`
/// is what notices if one appears.
/// `Cache-Control: no-store` on every answer to a request whose query carries a `token`.
///
/// A player or an `EventSource` cannot set a header, so the routes behind them take the credential as
/// `?token=`, and a webview or a browser that stores the answer files it under that whole URL, token
/// included, in a cache on disk that outlives the session. This server's own routes say not to. It is a
/// layer on this server's routes only: fbt-api's own (the file stream and the event stream) are behind
/// its router, which this one is merged into and cannot wrap.
pub(crate) async fn no_store_with_token(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let carries = request.uri().query().is_some_and(query_has_token);
    let mut response = next.run(request).await;
    if carries {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
    }
    response
}

/// Whether a query string names a `token` parameter, with a value or without.
fn query_has_token(query: &str) -> bool {
    query
        .split('&')
        .any(|pair| pair.split('=').next() == Some("token"))
}

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    // The login calls read their bodies under their own cap, far smaller than the 32 MiB fbt-api keeps
    // for uploading a `.torrent`: they are the three places this server takes bytes from someone it has not
    // yet decided is anybody, and the three fields of a key login and the two of a password fit in a few
    // hundred bytes. All three read the body themselves through [`read_capped`] and re-extract what they
    // kept, so this limit only bounds that second step - of bytes already kept under it, so it never
    // refuses on its own - and otherwise stays put so a future extractor on any of them inherits the same
    // cap rather than fbt-api's.
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
        )
        // The browser's own sign-in, and the two calls that read and end the session its cookie carries.
        // Only the `POST` has a body; the `GET` and the `DELETE` are gated by the cookie and its CSRF token,
        // which is [`crate::auth`]'s business, not this router's.
        .route(
            "/api/v1/auth/web-session",
            post(web_session::sign_in)
                .get(web_session::status)
                .delete(web_session::sign_out)
                .layer(DefaultBodyLimit::max(login::MAX_LOGIN_BODY)),
        );
    let logins = if shared.auth.secret_only() {
        Router::new()
    } else {
        logins
    };
    Router::new()
        .merge(logins)
        .route(
            "/api/v1/auth/devices",
            get(devices::list).post(devices::add),
        )
        .route("/api/v1/auth/devices/{name}", delete(devices::remove))
        .route("/api/v1/fs/roots", get(fs::roots))
        .route("/api/v1/fs/list", get(fs::list))
        // The one route here that writes to the server's disk on a client's say-so: one folder, inside the
        // allowed folders, behind the same login and CSRF check as every other change. Its body is a path
        // and a name, so it gets a cap of its own rather than fbt-api's room for a `.torrent`.
        .route(
            "/api/v1/fs/mkdir",
            post(fs::mkdir).layer(DefaultBodyLimit::max(fs::MAX_MKDIR_BODY)),
        )
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
        // The media routes: what a torrent's file is, one track of it as fragmented MP4, when a subtitle
        // track of it says what, and the cue list of a subtitle file of the same torrent. All four read the
        // file through fbtt-media from the blocking pool, and all four take a place in the media pool
        // before they touch the engine ([`media`]).
        .route("/api/v1/media/{torrent}/{file}", get(media::probe))
        .route(
            "/api/v1/media/{torrent}/{file}/tracks/{track}/stream",
            get(media::track_stream),
        )
        .route(
            "/api/v1/media/{torrent}/{file}/tracks/{track}/cues",
            get(media::track_cues),
        )
        .route(
            "/api/v1/media/{torrent}/{file}/subtitles/{sub}",
            get(media::subtitle_file),
        )
        .with_state(shared)
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, Bytes};
    use axum::http::{HeaderMap, StatusCode};
    use futures::stream::iter;

    use super::{login, read_capped};

    /// [`read_capped`] over a body that sends one chunk and then fails, under the same caps the login
    /// route reads under: the two answers these routes give for a body that stops mid-flight.
    ///
    /// fbt-lib's body deadline leaves an [`std::io::Error`] of kind `TimedOut` in the error axum
    /// reports, and fbt-api's own routes answer that with `408 request_timeout`. These routes read
    /// their bodies here instead of through an extractor, so the split has to be made here too: the
    /// same slow client must not get `408` from `/api/v1/torrents` and `400` from `/api/v1/auth/session`.
    #[tokio::test]
    async fn a_timed_out_body_is_a_408() {
        let body = Body::from_stream(iter([
            Ok(Bytes::from("{}")),
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request body too slow",
            )),
        ]));
        let answer = read_capped(
            &HeaderMap::new(),
            body,
            login::MAX_LOGIN_BODY,
            login::MAX_LOGIN_REFUSED_BODY,
            "a login request",
        )
        .await
        .expect_err("a body that stops at an error is refused, not accepted");
        assert_eq!(
            answer.status,
            StatusCode::REQUEST_TIMEOUT,
            "a body that timed out was not answered 408"
        );
        assert_eq!(answer.code, "request_timeout");
    }

    /// A body that fails for any other reason - the connection reset under it, say - is still the
    /// broken body it always was, and gets the answer it always got.
    #[tokio::test]
    async fn another_body_error_is_still_a_400() {
        let body = Body::from_stream(iter([
            Ok(Bytes::from("{}")),
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "reset",
            )),
        ]));
        let answer = read_capped(
            &HeaderMap::new(),
            body,
            login::MAX_LOGIN_BODY,
            login::MAX_LOGIN_REFUSED_BODY,
            "a login request",
        )
        .await
        .expect_err("a body that stops at an error is refused, not accepted");
        assert_eq!(
            answer.status,
            StatusCode::BAD_REQUEST,
            "a body that failed for another reason was not answered 400"
        );
        assert_eq!(answer.code, "invalid_request");
    }
}
