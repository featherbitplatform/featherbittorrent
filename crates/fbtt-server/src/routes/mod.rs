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
mod settings;
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
use axum::routing::{delete, get, post, put};
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
/// Reads `config.yaml` for an edit: [`Config::load_for_edit_keychained`] on the desktop app's engine, where
/// the file holds a proxy login without its password (even on a run whose keychain is out of reach), and
/// [`Config::load_for_edit`] everywhere else.
fn load_for_edit(path: &std::path::Path, keychained: bool) -> Result<Option<Config>, String> {
    if keychained {
        Config::load_for_edit_keychained(path)
    } else {
        Config::load_for_edit(path)
    }
}

/// How long a keychain call may take before it counts as no answer: the desktop app's own deadline for the
/// session bus (`SESSION_BUS_DEADLINE` in its `deadline` module), which this crate cannot reach.
pub(crate) const KEYCHAIN_DEADLINE: Duration = Duration::from_secs(2);

/// What a keychain call that ran past [`KEYCHAIN_DEADLINE`] answers.
pub(crate) const KEYCHAIN_TIMED_OUT: &str = "the keychain did not answer in time";

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
    /// One config change at a time: the routes read `config.yaml`, change one field of it and write it
    /// back, and two requests doing that at once would lose one another's change - whichever read first
    /// would write its own copy of a file the other had already changed. Every product route that writes
    /// the file goes through [`Shared::write_config`] and therefore through this one lock: the devices,
    /// the speed, the downloads and the proxy. Held across the disk work, which is why it is an async lock -
    /// nothing blocking runs inside it.
    changing_config: tokio::sync::Mutex<()>,
    /// What this run started on, its folders already rooted in the library. The config to write back
    /// when `config.yaml` is not there yet, so that saving a device cannot invent a different library
    /// than the one being served.
    base_config: Config,
    /// The config as the last write left it, this run's own to begin with: what the settings route
    /// reports the downloads section from, so a screen reads back what a save wrote rather than what the
    /// file said before it. Swapped whole under [`changing_config`](Self::changing_config).
    current: Mutex<Config>,
    /// Whether this run has a launch secret at all: the desktop app started this engine, so a request
    /// carrying that secret may name the program a finished torrent runs. A plain `serve` reads the
    /// program from its own `config.yaml` and a browser may not name one, so this is false there and the
    /// route answers `403 set_in_config_file`. What a *request* carries is asked separately
    /// ([`Shared::program_is_editable`]).
    program_editable: bool,
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
    /// The engine settings this run started on before its proxy was applied: what a proxy saved over the
    /// API is derived over ([`fbtt_core::network_for`]), so removing one brings back the switches it had
    /// turned off rather than leaving them off until a restart. Never changed after the start.
    base_settings: fbt::Settings,
    /// Where the proxy password is kept instead of `config.yaml`, on the desktop app's engine
    /// ([`crate::DesktopAccess::proxy_password`]); `None` on any other server. Not in a `Debug` printing.
    proxy_password: Option<Arc<dyn crate::ProxyPasswordStore>>,
    /// The unfinished folder, the watch folder and the completion program this server runs beside the
    /// engine it serves. The settings routes read its two complaints - what went wrong with the watch
    /// folder last time it was read, and with the program last time it was run - so a browser sees them
    /// beside the fields they belong to, and [`crate::Server::shutdown`] stops it. Not in the `Debug`
    /// printing below: what it would print is the watch folder's last scan and the moves waiting.
    automation: fbtt_core::downloads::AutomationHandle,
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
    /// What the config saves report to while they run: the hold a test arms, and nothing a run touches
    /// (see [`ConfigSaves`]). Its own `Arc` because the save carries it down to the blocking pool with the
    /// job, while the test that arms it holds the other end.
    config_saves: Arc<ConfigSaves>,
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
    /// `session` is the engine's own session, cloned: what the torrent routes answer about, and
    /// `base_settings` the settings it started on before the proxy. `launch` is the
    /// desktop app's launch secret and whether it is the only way in, for an engine the app started
    /// ([`crate::DesktopAccess`]), and `None` for any other server.
    // Each argument is one thing the server built at startup and hands over once; a struct to carry them
    // would only rename this list for its one caller.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        config: Config,
        paths: Paths,
        roots: Vec<fs::Root>,
        dummy_password_hash: Option<String>,
        session: fbt::Session,
        base_settings: fbt::Settings,
        launch: Option<(String, bool)>,
        proxy_password: Option<Arc<dyn crate::ProxyPasswordStore>>,
        automation: fbtt_core::downloads::AutomationHandle,
    ) -> Self {
        // The login stores are built from the server section; a config with none is the defaults, which is
        // what `Server::build` serves on in the same case.
        let server = config.server.clone().unwrap_or_default();
        let devices = Mutex::new(DeviceTable::build(&server.auth.devices));
        // A launch secret means the desktop app started this engine - with `expose` off it is the app's
        // own alone, with it on the secret is accepted beside the config's logins - and only then may a
        // request that carries it name the program a finished torrent runs (see `program_editable`).
        let program_editable = launch.is_some();
        let current = Mutex::new(config.clone());
        Shared {
            auth: Arc::new(match launch {
                Some((secret, only)) => FbttAuth::new(&server).with_launch_secret(secret, only),
                None => FbttAuth::new(&server),
            }),
            users: server.auth.users.clone(),
            devices,
            changing_config: tokio::sync::Mutex::new(()),
            base_config: config,
            current,
            program_editable,
            paths,
            roots: roots.into(),
            session,
            base_settings,
            proxy_password,
            automation,
            dummy_password_hash,
            verify_permits: Arc::new(tokio::sync::Semaphore::new(parallel_verifies())),
            verify_tally: Arc::new(VerifyTally::new()),
            config_saves: Arc::new(ConfigSaves::new()),
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

    /// The engine settings this run started on before its proxy was applied: what the proxy route
    /// derives a saved proxy's network settings over.
    pub(crate) fn base_settings(&self) -> &fbt::Settings {
        &self.base_settings
    }

    /// The keychain the proxy password is kept in, when this is the desktop app's engine on a system that
    /// has one: the proxy route writes the password there and the view says `keychain`. `None` when the
    /// password lives in `config.yaml` ([`proxy_password_storage`](Self::proxy_password_storage)).
    pub(crate) fn proxy_keychain(&self) -> Option<&Arc<dyn crate::ProxyPasswordStore>> {
        self.proxy_password
            .as_ref()
            .filter(|store| store.available())
    }

    /// Runs `call` on the keychain on a thread of its own and waits at most [`KEYCHAIN_DEADLINE`] for it,
    /// as the desktop app waits for the session bus: a Secret Service that hangs, or a collection waiting
    /// on an unlock prompt, must not hold a request (or the engine's start) for as long as it takes. A call
    /// that has not answered by then, or that panicked, is a failure with a plain line to show; its thread
    /// is left to finish (or not) on its own.
    pub(crate) async fn keychain_call<T: Send + 'static>(
        store: &Arc<dyn crate::ProxyPasswordStore>,
        call: impl FnOnce(&dyn crate::ProxyPasswordStore) -> T + Send + 'static,
    ) -> Result<T, String> {
        let store = Arc::clone(store);
        let (answer, answered) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("keychain".to_string())
            .spawn(move || {
                // The receiver may be gone by now: the deadline passed, and nobody wants the answer.
                let _ = answer.send(call(store.as_ref()));
            })
            .map_err(|problem| {
                tracing::warn!(problem = %problem, "the keychain thread did not start");
                "the keychain could not be asked".to_string()
            })?;
        match tokio::time::timeout(KEYCHAIN_DEADLINE, answered).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err("the keychain could not be asked".to_string()),
            Err(_) => {
                tracing::warn!("the keychain did not answer in time");
                Err(KEYCHAIN_TIMED_OUT.to_string())
            }
        }
    }

    /// Where this run keeps the proxy password: the keychain, `config.yaml` because the desktop app's
    /// system has no keychain, or `config.yaml` because this is a server.
    pub(crate) fn proxy_password_storage(&self) -> fbtt_core::PasswordStorage {
        match &self.proxy_password {
            Some(store) if store.available() => fbtt_core::PasswordStorage::Keychain,
            Some(_) => fbtt_core::PasswordStorage::Unavailable,
            None => fbtt_core::PasswordStorage::ConfigFile,
        }
    }

    /// The unfinished folder, the watch folder and the completion program this run acts on, for the two
    /// things that need it: [`crate::Server::shutdown`] stops it before the session is written out, and
    /// the settings routes (Task A3) read its complaints beside the fields they belong to.
    pub(crate) fn automation(&self) -> &fbtt_core::downloads::AutomationHandle {
        &self.automation
    }

    /// Reads `config.yaml`, gives `change` the config to alter, writes it back atomically, keeps it as
    /// what this run reports its settings from and only then hands it to `apply` - all under one lock, so
    /// every write and everything it makes happen to this run is one change at a time.
    ///
    /// Every product route that writes the file goes through here and therefore through
    /// [`changing_config`](Self::changing_config): the devices, the speed and the downloads. Two of them
    /// that each read the file, changed their own field and wrote it back would leave one with a change
    /// nobody asked for and the other with none - a speed save and a device add started together must both
    /// survive. Held while the disk work runs on the blocking pool and while `apply` runs, which is why it
    /// is an async lock.
    ///
    /// `apply` is the in-memory half of a change - the session put under new limits, the downloads
    /// automation given the settings the file now holds, the device list logins check against replaced -
    /// and it runs with the guard still held, after the save has landed. That order is the guarantee: with
    /// the apply outside the lock, two changes written in one order could take effect in the other, leaving
    /// the run under limits (or a device list) the file no longer has. Nothing an `apply` may do may wait
    /// on anything that takes this lock, or the server would wait on itself; the three applies are local
    /// calls on the session, the automation and the device list, so none of them does. `apply` is
    /// synchronous for the same reason.
    ///
    /// With no file yet, what `change` alters is the config this run started on, so a save cannot invent a
    /// different library than the one being served. The bytes go into a temp file beside the target and are
    /// renamed over it ([`Config::save`]), so a crash leaves the old file or the new one and never half of
    /// each. `change` refusing - a device that is already there, a name that is not - writes nothing and
    /// applies nothing.
    ///
    /// A file that cannot be read is answered and left exactly where it is, with nothing written and
    /// nothing applied: reading is [`Config::load_for_edit`], which renames nothing, unlike
    /// [`Config::load`](fbtt_core::Config::load), whose job is to let a *run* start on something. A request
    /// has no such need - it can fail - and an ordinary save renaming somebody's hand-edited file is not
    /// this route's to do. The path and the reason go to the log; the answer keeps one sentence. A read
    /// failure is a plain `500 internal` for a route to give its own name to, and a save failure is
    /// `500 config_unwritable`, which says what an operator would have to fix.
    ///
    /// A request that is dropped halfway through - the browser tab that asked for a change closes while the
    /// save is being written - still leaves the file and the run in step. The change does not run in the
    /// request's own future, which hyper drops on a disconnect, but in a task of its own that owns the state
    /// it needs and is not dropped with what awaits it: the save that has already started finishes, the
    /// config this run reports from is stored, and `apply` runs, whether or not anybody is left to read the
    /// answer. This request only awaits that task, and holding nothing but the [`Arc`] means the drop takes
    /// the request away and leaves the change running. A task that panicked is the same plain `500` a save
    /// failure gives, and its join error is never unwrapped.
    pub(crate) async fn write_config(
        self: &Arc<Self>,
        change: impl FnOnce(&mut Config) -> Result<(), RouteError> + Send + 'static,
        apply: impl FnOnce(&Config) + Send + 'static,
    ) -> Result<Config, RouteError> {
        self.write_config_then(
            move |mut config| std::future::ready(change(&mut config).map(|()| config)),
            move |config| {
                apply(config);
                std::future::ready(Ok(()))
            },
        )
        .await
    }

    /// [`write_config`](Self::write_config) with an `apply` that is a future and may fail: the proxy's,
    /// which waits on the engine to close the connections made under the old proxy.
    ///
    /// Everything `write_config` promises holds here: the apply runs inside the same task and under the
    /// same guard, after the save has landed, so two changes take effect in the order they were written -
    /// two proxy saves in quick succession leave the engine on the second. The future may wait on the
    /// session, never on anything that takes the config lock. When it fails, the file and the config this
    /// run reports from already hold the change, and the error is what the request answers; an apply that
    /// has to undo its change does so itself, still under the guard, with
    /// [`rewrite_from_apply`](Self::rewrite_from_apply), before it fails.
    ///
    /// `change` is a future too, handed the config as read and answering the config to save: the proxy's
    /// asks the keychain (on the blocking pool) before the save, still under the guard.
    pub(crate) async fn write_config_then<C, F>(
        self: &Arc<Self>,
        change: impl FnOnce(Config) -> C + Send + 'static,
        apply: impl FnOnce(&Config) -> F + Send + 'static,
    ) -> Result<Config, RouteError>
    where
        C: std::future::Future<Output = Result<Config, RouteError>> + Send,
        F: std::future::Future<Output = Result<(), RouteError>> + Send,
    {
        // What the task needs: the state, and one more handle on the same `Arc` the request holds.
        let shared = Arc::clone(self);
        // Test-support only, and it lives in *this* future rather than in the task's: what a test waits on
        // is the request being gone, and this is the request (see [`ChangeGuard`]).
        #[cfg(feature = "test-support")]
        let _ending = ChangeGuard(Arc::clone(&self.config_saves));
        tokio::spawn(async move { shared.change_config(change, apply).await })
            .await
            .map_err(|problem| {
                tracing::warn!(problem = %problem, "the config change task did not finish");
                RouteError::internal("the server could not change its config; see the server log")
            })?
    }

    /// The body of [`write_config`](Self::write_config), its only caller: the guard, the read, `change`, the
    /// save, `current` and `apply`, in that order, all with the guard held. It runs inside the task
    /// `write_config` spawns, so it runs to the end whether or not the request that asked for it is still
    /// there, and the guard is what keeps two changes from taking effect in the reverse of the order they
    /// were written.
    async fn change_config<C, F>(
        &self,
        change: impl FnOnce(Config) -> C + Send,
        apply: impl FnOnce(&Config) -> F + Send,
    ) -> Result<Config, RouteError>
    where
        C: std::future::Future<Output = Result<Config, RouteError>> + Send,
        F: std::future::Future<Output = Result<(), RouteError>> + Send,
    {
        let _guard = self.changing_config.lock().await;
        // Read on the blocking pool, like every other disk call here. Each call takes a copy of the path
        // of its own and `file` stays here, because the log line about a failure wants the file this
        // request actually used.
        let file = self.paths.config_file();
        let reading = file.clone();
        let keychained = self.proxy_password.is_some();
        let read = tokio::task::spawn_blocking(move || load_for_edit(&reading, keychained))
            .await
            .map_err(|problem| {
                tracing::warn!(
                    config = %file.display(),
                    problem = %problem,
                    "the config read task failed",
                );
                RouteError::internal("the server could not read its config; see the server log")
            })?;
        let config = match read {
            Ok(Some(config)) => config,
            // Nothing there yet: this run's own config is the only one to change.
            Ok(None) => self.base_config.clone(),
            Err(problem) => {
                tracing::warn!(
                    config = %file.display(),
                    problem = %problem,
                    "the config could not be read or applied",
                );
                return Err(RouteError::internal(
                    "the server's config has a setting to fix; see the server log",
                ));
            }
        };
        let config = change(config).await?;
        // The bytes go into a temp file beside it, are synced and renamed over it (`Config::save`). A
        // folder this server has no write permission on, a disk with no room left, a path somebody made
        // read-only: this is an operator's problem, not a client's mistake, so it is answered
        // `500 config_unwritable` with what the operating system said - the one thing whoever runs this
        // server can act on - and the whole path goes into the log line beside it.
        let saving = file.clone();
        let writing = config.clone();
        let saves = Arc::clone(&self.config_saves);
        tokio::task::spawn_blocking(move || {
            // Test-support only, and a no-op in a run: a save a test has armed the hold on stops here,
            // inside the job, so a test can drop the request that asked for it and watch the change finish
            // anyway. See [`ConfigSaves`].
            saves.park_inside_save();
            writing.save(&saving)
        })
        .await
        .map_err(|problem| {
            tracing::warn!(
                config = %file.display(),
                problem = %problem,
                "the config save task failed",
            );
            config_unwritable(&problem.to_string())
        })?
        .map_err(|problem| {
            tracing::warn!(
                config = %file.display(),
                problem = %problem,
                "the config could not be saved",
            );
            config_unwritable(&save_reason(&problem))
        })?;
        // What this run reports from, what an `apply` is handed and what the caller gets back is the config
        // as this run would use it: the same resolve the startup makes, so a relative folder in the file
        // becomes the folder the run works in rather than the spelling the file happened to use. Stored and
        // applied here, before the guard is released, so two changes take effect in the order they were
        // written and never in the reverse.
        let resolved = config.resolve(&self.paths);
        *locked(&self.current) = resolved.clone();
        apply(&resolved).await?;
        Ok(resolved)
    }

    /// Writes `edit` into `config.yaml` and into the config this run reports from, for an `apply` handed
    /// to [`write_config_then`](Self::write_config_then) that has to put back what its change wrote: the
    /// proxy route, when the engine refuses to switch. It takes no lock, because the apply that calls it
    /// already runs under [`changing_config`](Self::changing_config); called from anywhere else it would
    /// race the other writers. The answer is the display-safe reason it could not, naming no path.
    pub(crate) async fn rewrite_from_apply(
        &self,
        edit: impl FnOnce(&mut Config) + Send + 'static,
    ) -> Result<(), String> {
        let file = self.paths.config_file();
        let base = self.base_config.clone();
        let keychained = self.proxy_password.is_some();
        let written = tokio::task::spawn_blocking(move || {
            let mut config = match load_for_edit(&file, keychained) {
                Ok(Some(config)) => config,
                Ok(None) => base,
                Err(problem) => {
                    // The line names the file; the log keeps it and the answer does not.
                    tracing::warn!(problem = %problem, "the config could not be read again");
                    return Err("the config could not be read again".to_string());
                }
            };
            edit(&mut config);
            config
                .save(&file)
                .map(|()| config)
                .map_err(|problem| save_reason(&problem))
        })
        .await
        .map_err(|problem| clean(&problem.to_string()))?
        .map_err(|problem| clean(&problem))?;
        *locked(&self.current) = written.resolve(&self.paths);
        Ok(())
    }

    /// The config as the last write left it, this run's own to begin with: what a settings screen reads
    /// the downloads section from. A clone, so the caller does not hold the lock while it builds a view.
    pub(crate) fn current_config(&self) -> Config {
        locked(&self.current).clone()
    }

    /// The folders this run serves, as `config.yaml` spells them: what a submitted downloads folder has to
    /// be inside, and what gets pinned into the file before a change moves the download folder.
    pub(crate) fn served_roots(&self) -> Vec<std::path::PathBuf> {
        self.roots.iter().map(|root| root.given.clone()).collect()
    }

    /// The folders this run serves in both spellings, given and canonical: what a submitted downloads
    /// folder is judged against before anything touches the disk.
    ///
    /// Both are needed because a folder reaches this server in whichever spelling the client has: the one
    /// the file names, or the one the file system resolves it to. They differ for an ordinary folder on
    /// some platforms - on macOS `/var` is a link to `/private/var`, so a root below a temp directory has
    /// two absolute spellings that are not prefixes of one another, and a client that resolved the path
    /// before sending it would be refused for a folder the server serves. Case is deliberately not
    /// repaired: on Windows two spellings differing only in case are one folder, and a client that gets
    /// that wrong is refused rather than walked through a guess.
    pub(crate) fn served_roots_for_validation(&self) -> Vec<std::path::PathBuf> {
        self.roots
            .iter()
            .flat_map(|root| [root.given.clone(), root.canonical.clone()])
            .collect()
    }

    /// Whether this request may name the program a finished torrent runs on this machine.
    ///
    /// Two things have to hold. This run has a launch secret at all: the desktop app started this engine,
    /// and the app is then the one thing that may name a program
    /// ([`program_editable`](Self::program_editable)). And *this request* carries that secret, as a bearer
    /// header and nothing else ([`FbttAuth::carries_launch_secret`]). A password or key session, a browser's
    /// cookie session and the secret arriving in a URL all answer false, so a client merely visiting this
    /// machine never reaches the file's program.
    pub(crate) fn program_is_editable(&self, headers: &HeaderMap) -> bool {
        self.program_editable && self.carries_launch_secret(headers)
    }

    /// Whether these request headers carry the launch secret, as a bearer header alone. Delegated to
    /// [`FbttAuth`] so the secret lives in one place: this asks about a request, and only the
    /// authenticator holds the secret to compare against.
    pub(crate) fn carries_launch_secret(&self, headers: &HeaderMap) -> bool {
        self.auth.carries_launch_secret(headers)
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

    /// Arms [`VerifyTally::park_inside_verify`] and returns the count it moves: how many checks have reached
    /// the hold since it was armed, published in order and only ever rising, with a change to wait for. Under
    /// `test-support` only, and arming it twice changes nothing about the checks - both watchers see the same
    /// count. See [`Server::park_verifies`].
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

    /// Arms [`ConfigSaves::park_inside_save`] and returns the count it moves: how many saves have reached
    /// the hold since it was armed, published in order and only ever rising. Under `test-support` only, and
    /// arming it twice changes nothing about the saves - both watchers see the same count. See
    /// [`Server::park_config_saves`].
    #[cfg(feature = "test-support")]
    pub(crate) fn park_config_saves(&self) -> watch::Receiver<u64> {
        self.config_saves.park()
    }

    /// Lets every parked save finish, and stops holding the ones that come after
    /// ([`Self::park_config_saves`]). Under `test-support` only.
    #[cfg(feature = "test-support")]
    pub(crate) fn release_parked_config_saves(&self) {
        self.config_saves.release_parked();
    }

    /// A count of how many config changes have ended since this was read, rising once for every change whose
    /// request has been answered or dropped. Under `test-support` only: a test hangs up on a change, waits
    /// here for the request to be gone, and only then lets the parked save finish, so what it goes on to
    /// measure is the change finishing on its own rather than a race with the disconnect arriving. See
    /// [`ConfigSaves`].
    #[cfg(feature = "test-support")]
    pub(crate) fn config_change_endings(&self) -> watch::Receiver<u64> {
        self.config_saves.endings()
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
    hold: OnceLock<Hold>,
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
        let hold = self.hold.get_or_init(Hold::new);
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

/// A place in some blocking-pool work that only a test can put there: work that reaches it says how many
/// are sitting there and then waits until somebody opens it. The waiting is a condvar because it happens on
/// the blocking pool, where nothing may await; the count is a `watch` because whoever reads it is an async
/// task sharing this runtime's worker with the server, so it can afford to be told rather than to sit and
/// poll. Two holds are built from it - the one the Argon2 checks stop at ([`VerifyTally`]) and the one the
/// config saves stop at ([`ConfigSaves`]) - and without `test-support` none of this exists: no test arms a
/// hold, and `Shared` carries neither the field nor the wait.
#[cfg(feature = "test-support")]
#[derive(Debug)]
struct Hold {
    /// How many jobs have reached the hold since it was armed, and the count a watcher waits on. It is
    /// arrivals ever, not a count of what is parked right now, and it is published in order and only ever
    /// rises: a job bumps it and tells watchers in one step, so the number a watcher reads cannot go
    /// backwards - two jobs arriving on two blocking threads once bumped and published as two steps, and the
    /// second to publish could be the first to arrive, which made a watcher read the count fall by one. Every
    /// arrival bumps it and sends the new number, so a watcher reads a count and waits for the next change
    /// instead of polling - which is how a test counts what is parked without guessing at how long the work
    /// takes on somebody else's machine.
    counts: watch::Sender<u64>,
    /// `false` while jobs are being held; [`Hold::release`] makes it `true` for good, so the jobs that
    /// arrive afterwards run through without stopping. A condvar rather than a channel because the people
    /// waiting here are not on a runtime: they are on the blocking pool, where nothing may await.
    open: Mutex<bool>,
    waiting: Condvar,
}

#[cfg(feature = "test-support")]
impl Hold {
    /// A hold nobody has opened yet: everything that arrives waits here until a test says otherwise.
    fn new() -> Self {
        let (counts, _) = watch::channel(0);
        Hold {
            counts,
            open: Mutex::new(false),
            waiting: Condvar::new(),
        }
    }

    /// Says this job has reached the hold, then waits there for the test to stop looking.
    fn arrive_and_wait(&self) {
        // The bump and the publish are one step, under the watch's own lock, so the watched number only ever
        // rises: two jobs arriving on two blocking threads take their turns at it rather than each taking a
        // number and then racing to publish it, which could publish the second arrival before the first and
        // send the count backwards. The number is what a watcher reads; whether anybody is still watching to
        // be told is not its job.
        self.counts.send_modify(|count| *count += 1);
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

/// What the config saves report to: the hold a test arms so a save stops inside the write, and nothing a run
/// ever touches. Its own `Arc` because the save runs on the blocking pool and carries this with the job,
/// while the test that arms it holds the other end (see [`Shared::config_saves`]).
///
/// The count of endings is not the hold: it rises for every config change whose request is over, finished or
/// dropped, and a test reads it to know that the server has really let a request go before it opens the hold.
/// Without that, a test that hangs up on a save and releases it at the same moment is racing the disconnect
/// across the loopback, and what it measures next is a coin toss rather than the change finishing on its own.
#[derive(Debug)]
pub(crate) struct ConfigSaves {
    /// The hold, and nobody arms it but a test. A `OnceLock` for the same reason the verify hold is one:
    /// arming it is a one-way door, and once saves are being held the ones that arrive after the release run
    /// through, so there is nothing to re-arm and nothing to reset.
    #[cfg(feature = "test-support")]
    hold: OnceLock<Hold>,
    /// How many config changes have ended - finished or been dropped - since the server started. Read by
    /// [`Shared::config_change_endings`] under `test-support`, and the `watch` is always here so that the
    /// [`ChangeGuard`] that bumps it needs no `cfg` at the point it is made.
    #[cfg(feature = "test-support")]
    endings: watch::Sender<u64>,
}

impl ConfigSaves {
    /// A report nothing has armed a hold on and nothing has finished, which is what every run has.
    fn new() -> Self {
        ConfigSaves {
            #[cfg(feature = "test-support")]
            hold: OnceLock::new(),
            #[cfg(feature = "test-support")]
            endings: watch::channel(0).0,
        }
    }

    /// Stops this save inside the write until a test lets it go, if a test armed a hold
    /// ([`Shared::park_config_saves`], under `test-support`). Called from inside the blocking job, right
    /// before the file is written, so what is parked there is the save a request is waiting on. In a run
    /// nothing has armed a hold, so there is nowhere to stop and this does nothing - the only cost it ever
    /// has.
    fn park_inside_save(&self) {
        #[cfg(feature = "test-support")]
        {
            if let Some(hold) = self.hold.get() {
                hold.arrive_and_wait();
            }
        }
    }

    /// Arms [`park_inside_save`](Self::park_inside_save) and returns the count it moves. Under `test-support`
    /// only.
    #[cfg(feature = "test-support")]
    fn park(&self) -> watch::Receiver<u64> {
        self.hold.get_or_init(Hold::new).counts.subscribe()
    }

    /// Lets every parked save finish, and stops holding the ones that come after
    /// ([`park`](Self::park)). Under `test-support` only.
    #[cfg(feature = "test-support")]
    fn release_parked(&self) {
        if let Some(hold) = self.hold.get() {
            hold.release();
        }
    }

    /// The count of changes that have ended, with a change to wait for. Under `test-support` only.
    #[cfg(feature = "test-support")]
    fn endings(&self) -> watch::Receiver<u64> {
        self.endings.subscribe()
    }
}

/// A handle kept by one [`Shared::write_config`] call until its future goes away, one way or the other, and
/// counted in [`ConfigSaves`] as it drops. It is the only thing in `write_config`'s own future that outlives
/// the spawn - the change itself has moved into the task - so its `Drop` is what says the request is over.
/// Under `test-support` only, and in a run there is no future to watch.
#[cfg(feature = "test-support")]
#[derive(Debug)]
struct ChangeGuard(Arc<ConfigSaves>);

#[cfg(feature = "test-support")]
impl Drop for ChangeGuard {
    fn drop(&mut self) {
        // One step, so the count a watcher reads is a change and never a bump it can miss.
        self.0.endings.send_modify(|count| *count += 1);
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
            .field("program_editable", &self.program_editable)
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

/// The answer for a save that could not be written down: `500 config_unwritable`, saying which file failed
/// and what the operating system said, and pointing at the log for the rest.
///
/// The answer names the file by what it is - the server's config - and not by where it lives or what it is
/// called: that directory is the operator's business, and an answer that printed it would tell a signed-in
/// stranger how this machine is laid out. The rule is not new here; a *read* failure keeps the same silence
/// for the same reason, and `tests/server.rs` holds a device add to it. The whole path is in the log line
/// this answer points at, which is where whoever runs this server needs it.
fn config_unwritable(reason: &str) -> RouteError {
    RouteError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "config_unwritable",
        format!(
            "the server could not write its config file ({}); see the server log",
            clean(reason),
        ),
    )
}

/// What to say about a save that failed: what the operating system said, with none of where.
///
/// [`ConfigError::Write`] prints the file's own path in its line (`cannot write <path>: ...`), which is
/// exactly the part an answer must not carry, so only the error underneath it - the operating system's own
/// words - is taken from it. That is also what makes the answer useful: it says a folder could not be
/// written to rather than how this machine is laid out. Anything else is said as it is.
fn save_reason(problem: &fbtt_core::ConfigError) -> String {
    match problem {
        fbtt_core::ConfigError::Write { source, .. } => source.to_string(),
        other => other.to_string(),
    }
}

/// The code [`RouteError::internal`] gives, and the one [`RouteError::named`] will rename: a `500` that
/// says only that something went wrong.
const INTERNAL: &str = "internal";

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
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, INTERNAL, message)
    }

    /// The same error with a different code, for a route that gives a shared failure its own name: the
    /// settings routes answer a config they cannot read as `500 config_unreadable`; a device add keeps the
    /// `500 internal` [`Shared::write_config`] builds, and both come from the same write.
    ///
    /// Only an error that says nothing of its own is renamed. A save failure already carries
    /// `config_unwritable`, which names what really went wrong, so a route asking for `config_unreadable`
    /// there does not claim a file it could read is one it could not.
    fn named(mut self, code: &'static str) -> Self {
        if self.code == INTERNAL {
            self.code = code;
        }
        self
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

/// `Cache-Control: no-store` on every answer to a request whose query carries a `token`: a cache would file
/// the answer under the whole URL, credential included. It covers this server's routes only, not fbt-api's.
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
        // The server's own settings, read and written over the API: the running limits and the downloads
        // section, saved to `config.yaml` so a change survives a restart. Every write goes through one lock
        // ([`Shared::write_config`]) with the device routes, and every body is a handful of fields, so the two
        // `PUT`s get a cap of their own rather than fbt-api's room for a `.torrent`.
        .route("/api/v1/settings", get(settings::get))
        .route(
            "/api/v1/settings/speed",
            put(settings::put_speed).layer(DefaultBodyLimit::max(settings::MAX_SETTINGS_BODY)),
        )
        .route(
            "/api/v1/settings/downloads",
            put(settings::put_downloads).layer(DefaultBodyLimit::max(settings::MAX_SETTINGS_BODY)),
        )
        .route(
            "/api/v1/settings/proxy",
            put(settings::put_proxy).layer(DefaultBodyLimit::max(settings::MAX_SETTINGS_BODY)),
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
