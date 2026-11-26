//! Starting a headless featherbittorrent: the engine, fbt-api on top of it, and the save that keeps
//! them agreeing.
//!
//! [`Server`] is what `featherbittorrent serve` runs. It holds the library lock for as long as the run
//! does - through the [`fbtt_core::Engine`], which is the only thing that writes `session.state` - and
//! serves fbt-api's own routes plus featherbittorrent's own logins, devices and folder browsing on
//! [`fbt_api::ApiConfig::bind`]. Two orders matter here, and nothing else in this file does:
//!
//! - **Shutdown closes the API first.** A request that arrived while the last save was running would be
//!   answered from a library being written under it. Stopping the listener, and every connection with
//!   it, before [`fbtt_core::Engine::shutdown`] runs means no handler is touching the session when the
//!   state file is written and the lock goes.
//! - **The save task owns the engine.** The save is the slow part of serving a library, and it must not
//!   be able to delay a login or a torrent list waiting on the same worker. It runs on its own task, and
//!   the only message that task answers to is its stop signal going away - which happens by itself when
//!   the [`Server`] is dropped, so a run that forgets to shut down still ends with the state written.
//!
//! The engine settings come from [`Config::settings_before_proxy`], with the config's `proxy:` applied
//! when the engine starts; [`Server::start_with_settings`] takes them as an argument instead, which is how
//! a test - or the binary's debug-only offline mode - keeps every socket where the caller put it. The
//! settings as handed in are kept as the base a proxy changed at runtime is derived over.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fbt::{SessionError, Settings};
use fbt_api::{ApiConfig, ApiError, ApiServer, Auth};
use fbtt_core::downloads::Automation;
use fbtt_core::engine::SAVE_EVERY;
use fbtt_core::{Config, Engine, LibraryLock, Notice, Paths};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::auth::password;
use crate::routes::{self, Shared, fs};

/// The password an unknown user name is checked against, so that "no such account" arrives no faster
/// than "wrong password" ([`crate::routes::login`]). It is not anybody's password and it is never
/// stored anywhere: only its hash is computed, once, at startup, and nothing can log in with it because
/// no account has it.
const DUMMY_PASSWORD: &str = "featherbittorrent-no-such-account";

/// The shortest launch secret [`Server::start_desktop`] accepts, in characters: the length of 32 random bytes
/// in unpadded base64url, which is what the desktop app makes. A shorter one is refused rather than served,
/// because the secret is the whole defence of the app's engine and an empty one would match an empty
/// `?token=`.
pub const MIN_LAUNCH_SECRET_CHARS: usize = 43;

/// Why a server did not start. Every case is one the person who ran `serve` has to act on, which is why
/// each says what to do about it in the line itself.
#[derive(Debug, Error)]
pub enum ServerError {
    /// The library could not be opened: the state file could not be read or written in a way this run
    /// could recover from, or the session could not start on these settings.
    #[error("the library could not start: {0}")]
    Engine(#[source] SessionError),
    /// One of `server.allowed_roots` could not be made into a directory, or could not be canonicalised.
    /// Nothing is served until it exists: fbt-api refuses to run with an allowed root it cannot resolve,
    /// and a folder the API may name but that is not there has nothing to browse.
    #[error("{0}")]
    Root(String),
    /// The Argon2 work that lets an unknown user name cost a password check could not be prepared: the
    /// blocking pool refused the job, which means this process is already being shut down or has run out
    /// of memory for it. Without that work every wrong user name would answer immediately, and anyone
    /// comparing how long requests take could read the list of accounts off this server.
    #[error("the password check could not be prepared: {0}")]
    Password(String),
    /// fbt-api refused its own config - a bind address already in use, an allowed root it cannot use -
    /// or could not open the listener.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// The desktop launch secret handed to [`Server::start_desktop`] is shorter than
    /// [`MIN_LAUNCH_SECRET_CHARS`]. Nothing was started: an engine guarded by a guessable secret is not one
    /// to serve.
    #[error(
        "the launch secret is too short: it needs at least {MIN_LAUNCH_SECRET_CHARS} characters"
    )]
    LaunchSecret,
}

/// How the desktop app reaches the engine it starts ([`Server::start_desktop`]).
///
/// The app makes a secret at every launch, keeps it in memory only, and presents it as a bearer token (or
/// as the query token on the routes that take one). Its webview calls from an origin of its own, which is
/// why `origins` are added to the server's CORS list. `expose` is whether this engine also serves other
/// people as the config's `server` section says; without it the engine is the app's alone.
#[derive(Clone)]
pub struct DesktopAccess {
    /// The per-launch secret. Accepted as a credential on every route, compared in constant time.
    pub launch_secret: String,
    /// The webview's origins, added to `server.cors_origins`.
    pub origins: Vec<String>,
    /// `false`: bind `127.0.0.1:0` whatever the config says, serve no web UI, mount no login routes, and
    /// accept the launch secret alone. `true`: serve the config's `server` section exactly as
    /// [`Server::start`] does, and accept the launch secret beside its logins.
    pub expose: bool,
}

impl std::fmt::Debug for DesktopAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the secret: a `Debug` printing ends up beside a panic message.
        f.debug_struct("DesktopAccess")
            .field("origins", &self.origins)
            .field("expose", &self.expose)
            .finish_non_exhaustive()
    }
}

/// A running featherbittorrent server: the library, the API and the save in between them.
///
/// Stop it with [`shutdown`](Self::shutdown). Dropping it does the same thing without waiting for any
/// of it: closing the API's listener is fbt-api's own `Drop`, and closing the save task's stop signal is
/// this one's, so the last save still happens - on the task, after the drop.
#[must_use = "dropping the server stops it"]
pub struct Server {
    /// fbt-api's handle: the listener, and its accept loop. Closing this is the first half of shutting
    /// down; fbt-api's `Drop` closes it without waiting for the connections to end.
    api: ApiServer,
    /// What the routes know: the same session and challenge stores the authenticator reads, the devices,
    /// the roots. Kept so the server outlives its router and, under `test-support`, so a test can ask how
    /// many password checks ran.
    shared: Arc<Shared>,
    /// The save task's stop signal. Sending on it, or dropping this field, ends the loop and runs the
    /// engine's final save.
    stop: mpsc::Sender<()>,
    /// The task that owns the engine. Its answer is the last save's failure, if it had one.
    saving: JoinHandle<Result<(), String>>,
}

impl Server {
    /// Serves `config` on `paths`, holding `lock`: the engine on this library's settings, fbt-api plus
    /// these routes on the address its `server` section names (its defaults when it names none), and a
    /// save every [`SAVE_EVERY`] while it runs.
    ///
    /// `config` is the run's resolved one (see [`Config::resolve`]): its allowed roots are what both
    /// this server and fbt-api enforce, and they are made into directories here because fbt-api will not
    /// start with a root that is missing. The notices about the state file - a torrent that did not come
    /// back, bytes that had to be kept aside - arrive as warnings: nobody started this server from a
    /// terminal that could print them, so the log is where they go.
    ///
    /// # Errors
    ///
    /// [`ServerError`] for a folder that could not become an allowed root, for a library that would not
    /// open, and for a listener or API config that could not be used.
    pub async fn start(
        config: Config,
        paths: &Paths,
        lock: LibraryLock,
    ) -> Result<Self, ServerError> {
        Self::build(config.settings_before_proxy(), config, paths, lock, None).await
    }

    /// [`start`](Self::start) with the engine's settings given rather than taken from the config.
    ///
    /// This is not a test hook: the settings and the config answer different questions, and something that
    /// runs the library somewhere other than where its config points needs to say so. `featherbittorrent
    /// serve` uses it for its debug-only offline mode - every socket on loopback, no DHT, no multicast -
    /// and a test uses it for the same reason on a bigger scale, keeping one run's traffic inside the
    /// machine while still serving the same config file. [`start`](Self::start) is this call with
    /// [`Config::settings_before_proxy`] handed to it, which is what a normal run wants.
    ///
    /// The config's `proxy:` is applied to `settings` when the engine starts, whatever they say: hand in
    /// the settings before the proxy, because they are what a proxy removed over the API goes back to.
    ///
    /// # Errors
    ///
    /// The same as [`start`](Self::start).
    pub async fn start_with_settings(
        settings: Settings,
        config: Config,
        paths: &Paths,
        lock: LibraryLock,
    ) -> Result<Self, ServerError> {
        Self::build(settings, config, paths, lock, None).await
    }

    /// The engine the desktop app starts for its own window: [`start_with_settings`](Self::start_with_settings)
    /// with the access [`DesktopAccess`] describes.
    ///
    /// The CORS list is `access.origins` plus the config's own `server.cors_origins`. With `expose` off the
    /// server binds `127.0.0.1` on a port the OS picks whatever the config asks, serves no web UI, mounts no
    /// login routes (they answer `404`), and accepts only the launch secret. With `expose` on, the config's
    /// `server` section applies exactly as it does for [`start`](Self::start), and the launch secret is
    /// accepted beside it.
    ///
    /// # Errors
    ///
    /// The same as [`start`](Self::start), and [`ServerError::LaunchSecret`] for a launch secret shorter
    /// than [`MIN_LAUNCH_SECRET_CHARS`].
    pub async fn start_desktop(
        settings: Settings,
        config: Config,
        paths: &Paths,
        lock: LibraryLock,
        access: DesktopAccess,
    ) -> Result<Self, ServerError> {
        // Checked before anything starts. Counted in characters, as the secret is text; the app's own is
        // ASCII, so this is its byte length too.
        if access.launch_secret.chars().count() < MIN_LAUNCH_SECRET_CHARS {
            return Err(ServerError::LaunchSecret);
        }
        Self::build(settings, config, paths, lock, Some(access)).await
    }

    /// The three starts, which differ only in where the settings came from and in the desktop access.
    async fn build(
        settings: Settings,
        config: Config,
        paths: &Paths,
        lock: LibraryLock,
        desktop: Option<DesktopAccess>,
    ) -> Result<Self, ServerError> {
        // The section this server was started from. A config with none is not a contradiction: `serve`
        // only gets here once something chose to serve, but a caller that hands its own settings (a test,
        // or a run serving a library somewhere other than where its config points) may hand a config that
        // says nothing about a server, and the defaults are then what an unwritten section means.
        let server = config.server.clone().unwrap_or_default();
        // The desktop app's own engine, serving nobody else: loopback on a port the OS picks, whatever the
        // config asks, because the window is the only client and the secret the only credential.
        let secret_only = desktop.as_ref().is_some_and(|access| !access.expose);
        let bind = if secret_only {
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
        } else {
            server.bind
        };
        let auth_on = secret_only || server.auth_on();
        let mut cors_origins = desktop
            .as_ref()
            .map(|access| access.origins.clone())
            .unwrap_or_default();
        for origin in &server.cors_origins {
            if !cors_origins.contains(origin) {
                cors_origins.push(origin.clone());
            }
        }
        let launch = desktop.map(|access| (access.launch_secret, !access.expose));
        // No password is taken on the secret-only engine: its login routes are not there.
        let passwords_in_use = !secret_only && !server.auth.users.is_empty();
        // Said before any of the slow work, because it is about how this server will be reached: if
        // someone is going to type a password over this address, they should know now.
        warn_if_passwords_are_exposed(bind, auth_on, passwords_in_use);
        warn_on_hosts_that_cannot_match(&server.allowed_hosts);

        let roots = prepare_roots(&server.allowed_roots).await?;
        // Both spellings of every root go to fbt-api too: it enforces the same containment on the paths
        // its own routes take (save targets, move destinations), and this is the one place the two
        // servers of this library agree about what is allowed.
        let given_roots: Vec<PathBuf> = roots.iter().map(|root| root.given.clone()).collect();

        // The hash an unknown name costs a verify against - see `routes::login`. Only when there are
        // accounts to be unknown *among*: with none, every password attempt is refused outright and there
        // is no name worth hiding behind half a second of hashing. It is computed here, once, rather than
        // per request: the point is that the answer arrives after the same work either way, and doing it
        // per request would make an unknown name cost more than a known one instead of the same.
        let dummy_password_hash = if auth_on && passwords_in_use {
            let hashing = tokio::task::spawn_blocking(|| password::hash(DUMMY_PASSWORD));
            let hash = hashing.await.map_err(|problem| {
                ServerError::Password(format!("the blocking pool refused the work ({problem})"))
            })?;
            Some(hash)
        } else {
            None
        };

        // The settings as handed in are the base a proxy saved at runtime is derived over, so removing it
        // brings back what this run started on; the engine starts on them with the config's proxy applied,
        // whatever the caller did, so a run never starts direct on a config that names a proxy.
        let base_settings = settings.clone();
        let mut settings = settings;
        fbtt_core::apply_to_settings(&mut settings, config.proxy.as_ref());
        let (mut engine, start_problems) = Engine::start(settings, paths, lock)
            .await
            .map_err(ServerError::Engine)?;
        // What went wrong with the saved state, one line each. A restore reports one per torrent that
        // did not come back, so this is where a person finds out their library is missing something.
        for problem in start_problems {
            warn_notice(&problem);
        }

        // Per-torrent reports from the saves this task makes: a torrent whose resume data would not come
        // back is worth a line every time it is left out, and the queue is bounded on purpose - the save
        // must not wait on a log that has stopped keeping up. The downloads automation reports on the
        // same queue, so it is made here, once, and both of them are handed a handle on it.
        let (reports, reading_reports) = mpsc::channel::<Notice>(REPORTS);
        engine.report_to(reports.clone());
        // The unfinished folder, the watch folder and the program a finished torrent runs, beside the
        // engine this server serves. `editable` is false: a server reads the program from its own
        // `config.yaml` and a browser has no business naming a program to run on the machine it is only
        // visiting (see `OnCompleteView`). The handle goes to the routes so the settings they answer with
        // can carry what went wrong with these two things (Task A3), and `shutdown` stops it.
        let automation = Automation::spawn(
            engine.session().clone(),
            config.download_dir.clone(),
            config.downloads.clone(),
            false,
            reports,
        );

        // What the routes know, built on the library they answer about: the torrent routes - the one that
        // hands out an address to stream a file from included - read the same session fbt-api is handed
        // below, and every clone of a session is the same session. It comes after the engine for that
        // reason: a route cannot be given a library that does not exist yet.
        let shared = Arc::new(Shared::new(
            config,
            paths.clone(),
            roots,
            dummy_password_hash,
            engine.session().clone(),
            base_settings,
            launch,
            automation,
        ));

        // What fbt-api is configured with: our bind address, our authenticator and the same folders this
        // server's own routes may name. `ApiConfig` is non-exhaustive, so its fields are set one by one
        // rather than named in a literal; everything it does not hear about keeps fbt-api's own default -
        // including the 32 MiB body limit, which is for uploading a `.torrent` and which the login routes and
        // the stream-token route cut down to their own far smaller ones.
        let mut api_config = ApiConfig::default();
        api_config.bind = bind;
        // Always our own authenticator, auth on or off: with `Auth::Disabled` fbt-api would decide who is
        // trusted by looking at the peer address, and here that decision - which logins are allowed, which
        // tokens are this server's - is already made in `crate::auth`.
        api_config.auth = Auth::Custom(shared.authenticator());
        api_config.allowed_roots = Some(given_roots);
        // The origins a browser engine may call from: `server.cors_origins`, plus the desktop webview's own
        // when the app started this engine. fbt-api answers their preflights before auth and adds the CORS
        // headers to every answer; who may do what is still decided by the authenticator above.
        api_config.cors_origins = cors_origins;

        // The routes fbt-api is handed, which are featherbittorrent's own: the logins, the devices, the
        // folder browsing, the stream token.
        let extra = routes::router(Arc::clone(&shared));
        // A build that carries the front end serves it on the same router, for the reason in `crate::web`:
        // the sign-in call has to be same-origin with the page that makes it. `server.web_ui` is the
        // switch, and `web_ui_on` is the only question asked of it; a headless build never gets here, so
        // the same config key on that build is accepted, ignored, and said once at startup instead.
        #[cfg(feature = "web-ui")]
        let extra = if server.web_ui_on() && !secret_only {
            extra.merge(crate::web::router())
        } else {
            extra
        };

        // Every answer of this server's own routes to a request with `?token=` is not to be stored: the
        // credential is in the URL a cache would file it under.
        let extra = extra.layer(axum::middleware::from_fn(routes::no_store_with_token));

        let api = ApiServer::start_with(
            // A clone, and every clone is the same session: fbt-api answers requests from it while this
            // task keeps driving the saves, and neither owns it more than the other.
            engine.session().clone(),
            api_config,
            extra,
        )
        .await
        .map_err(ServerError::Api)?;

        let (stop, stopping) = mpsc::channel::<()>(1);
        let saving = tokio::spawn(save_loop(engine, reading_reports, stopping));

        tracing::info!(
            address = %api.local_addr(),
            auth = auth_on,
            "featherbittorrent is serving",
        );
        Ok(Server {
            api,
            shared,
            stop,
            saving,
        })
    }

    /// The address being served on - with the real port when the config asked for `0`.
    pub fn local_addr(&self) -> SocketAddr {
        self.api.local_addr()
    }

    /// How long one read or one seek of a media route may take on this server, for every media request
    /// that opens a file after the call.
    ///
    /// The default is the media routes' own bound - sixty seconds, longer than any piece of a healthy
    /// torrent takes and far shorter than waiting forever for one - and nothing in `config.yaml` sets it,
    /// because it is not a preference about a library: it is how long a request is willing to wait on a
    /// peer for somebody else's piece. A test that wants to watch a stream give up, or a run behind a
    /// tracker that has gone quiet, is what changes it. A media stream already playing keeps the window it
    /// started with, because a media route takes its deadline once, when it opens the file.
    pub fn set_media_read_timeout(&self, timeout: Duration) {
        self.shared.set_media_read_timeout(timeout);
    }

    /// How many Argon2 password checks this server has run. Only a test reads this: the one that says an
    /// unknown user name costs the same work as a known one, which is the thing a timing difference
    /// between the two would give away to someone with no credential at all.
    #[cfg(feature = "test-support")]
    pub fn password_verifies(&self) -> u64 {
        self.shared.password_verifies()
    }

    /// The most Argon2 password checks this server has run at the same time. Only a test reads this: the
    /// one that says the bound on how many verifies run together - one per core, at most four - is what
    /// actually limits the work, however many logins arrive at once.
    #[cfg(feature = "test-support")]
    pub fn max_concurrent_verifies(&self) -> u64 {
        self.shared.max_concurrent_verifies()
    }

    /// Stops every Argon2 password check this server starts inside the check, and returns a count of how many
    /// have reached that stopping point since it was armed - a count a test waits on rather than polls, and
    /// releases with [`release_parked_verifies`](Self::release_parked_verifies). The count is arrivals ever,
    /// not how many are held right now, and it is published in order and only ever rises (see `VerifyTally`),
    /// so a test can wait for a check to arrive and read the number back without it moving under it. Only a
    /// test does this, and it is how that test says the bound is real without timing somebody else's hashing:
    /// hold the checks that are holding every permit, hang up on their connections, and see whether anything
    /// else gets a turn. A check stays parked until released even after its client has gone, which is the
    /// point of the exercise.
    #[cfg(feature = "test-support")]
    pub fn park_verifies(&self) -> tokio::sync::watch::Receiver<u64> {
        self.shared.park_verifies()
    }

    /// Lets every check stopped by [`park_verifies`](Self::park_verifies) finish, and stops stopping the
    /// ones that come after. Only a test calls this; without `test-support` there is nothing to stop.
    #[cfg(feature = "test-support")]
    pub fn release_parked_verifies(&self) {
        self.shared.release_parked_verifies();
    }

    /// Stops every change to `config.yaml` this server starts inside the save, and returns a count of how
    /// many have reached that stopping point since it was armed - a count a test waits on rather than polls,
    /// and releases with [`release_parked_config_saves`](Self::release_parked_config_saves). The count is
    /// arrivals ever and only ever rises, like the verify hold's. Only a test does this, and it is how that
    /// test hangs up on a change partway through and proves the change finishes without the request: the save
    /// is held where a request is waiting on it, the request is dropped, and what happens next is the thing
    /// under test. A save stays parked until released even after its request has gone, which is the point.
    #[cfg(feature = "test-support")]
    pub fn park_config_saves(&self) -> tokio::sync::watch::Receiver<u64> {
        self.shared.park_config_saves()
    }

    /// Lets every save stopped by [`park_config_saves`](Self::park_config_saves) finish, and stops stopping
    /// the ones that come after. Only a test calls this; without `test-support` there is nothing to stop.
    #[cfg(feature = "test-support")]
    pub fn release_parked_config_saves(&self) {
        self.shared.release_parked_config_saves();
    }

    /// A count of how many config changes have ended - answered or dropped - rising once for each. A test
    /// hangs up on a change, waits here until the request is really gone, and only then opens the hold, so
    /// what it measures next is the change finishing on its own rather than a race with the disconnect
    /// arriving. Only a test reads this; without `test-support` no change is counted.
    #[cfg(feature = "test-support")]
    pub fn config_change_endings(&self) -> tokio::sync::watch::Receiver<u64> {
        self.shared.config_change_endings()
    }

    /// The network settings the engine is running under right now: the proxy and the switches it implies.
    /// Only a test reads this, to see that a proxy saved over the API took hold, and the `Debug` of what it
    /// returns hides the proxy password (fbt-lib prints it as `***`).
    #[cfg(feature = "test-support")]
    pub fn network_settings(&self) -> fbt::NetworkSettings {
        self.shared.session().network_settings()
    }

    /// The engine's proxy health right now, in this product's words. Only a test reads this.
    #[cfg(feature = "test-support")]
    pub fn proxy_health(&self) -> fbtt_core::ProxyHealth {
        fbtt_core::proxy::health(self.shared.session())
    }

    /// The addresses the engine's network stack has bound (its BitTorrent port, not the API's). Only a
    /// test reads this, to take that port away and make the next network change fail to build.
    #[cfg(feature = "test-support")]
    pub fn engine_local_addrs(&self) -> Vec<SocketAddr> {
        self.shared.session().local_addrs()
    }

    /// Stops serving, saves, and releases the library. Returns once every connection is closed, the last
    /// save has been written or failed to be, and the lock has gone with it - so another
    /// featherbittorrent may start on this library as soon as this returns.
    ///
    /// The downloads automation is told first, so it is not asking this session to move files while that
    /// session is being written out, and the API goes next, for the reason in this module's comment: no
    /// handler may still be answering a request from a session that is about to be written out. The
    /// `Err` is the last save's own failure, one line about it; nothing here can fix that, and the library
    /// on disk stays the one from before this run.
    pub async fn shutdown(self) -> Result<(), String> {
        let Server {
            api,
            shared,
            stop,
            saving,
        } = self;
        shared.automation().shutdown();
        api.shutdown().await;
        // The save task's cue to do the last one. A full queue means it has already been told, or the
        // task is gone; either way the send has nothing to wait for.
        let _ = stop.send(()).await;
        match saving.await {
            Ok(result) => result,
            Err(problem) => Err(format!(
                "the save task did not finish ({})",
                if problem.is_panic() {
                    "it panicked"
                } else {
                    "it was cancelled"
                },
            )),
        }
    }
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Where it is serving and what its routes know. No password hash and no device key is in here,
        // because a `Debug` printing ends up beside a panic message.
        f.debug_struct("Server")
            .field("local_addr", &self.api.local_addr())
            .field("shared", &self.shared)
            .finish_non_exhaustive()
    }
}

/// How many per-torrent reports one save may have queued before further ones are dropped. A save that
/// left out a hundred torrents has already said the important thing once; this only bounds what it can
/// hold while the log keeps up.
const REPORTS: usize = 64;

/// Makes every configured root into a directory, on the blocking pool: `mkdir -p` for each one, then its
/// canonical form. See [`fs::Root::prepare`] for why both spellings are kept and why this has to happen
/// before fbt-api starts.
async fn prepare_roots(configured: &[PathBuf]) -> Result<Vec<fs::Root>, ServerError> {
    let configured = configured.to_vec();
    tokio::task::spawn_blocking(move || fs::Root::prepare(&configured))
        .await
        .map_err(|problem| {
            ServerError::Root(format!(
                "the allowed folders could not be prepared ({problem})"
            ))
        })?
        .map_err(ServerError::Root)
}

/// The warning about passwords on a bind address anyone can reach, or `None` when there is nothing to
/// warn about.
///
/// This server speaks plain HTTP: there is no certificate in it and TLS termination is somebody else's
/// job (see the design's reverse-proxy rule). A `Basic` credential over an address that is not loopback
/// therefore crosses the network in the clear, and the person who chose that address may have chosen it
/// precisely so others could reach it. Nothing here can tell whether a proxy is doing the encrypting, so
/// the answer is to say what is unencrypted and let them decide - which is why this is a warning at
/// startup rather than a refusal to start. The name it points at is the featherbit gateway, with any API
/// gateway or reverse proxy equally an answer, because throttling and further authentication come with it
/// and not just TLS; device keys are the alternative that needs no third party at all.
fn passwords_are_exposed_warning(
    bind: SocketAddr,
    auth_on: bool,
    passwords_in_use: bool,
) -> Option<String> {
    if !auth_on || !passwords_in_use || bind.ip().is_loopback() {
        return None;
    }
    Some(format!(
        "passwords cross the network unencrypted on {bind}: on untrusted networks put the featherbit \
         gateway (or another API gateway or reverse proxy) in front for TLS, throttling and further \
         authentication, or use device keys"
    ))
}

/// Says [`passwords_are_exposed_warning`], when there is one to say. The address is in the sentence
/// rather than only in a log field, because the line a person reads is the sentence.
fn warn_if_passwords_are_exposed(bind: SocketAddr, auth_on: bool, passwords_in_use: bool) {
    if let Some(problem) = passwords_are_exposed_warning(bind, auth_on, passwords_in_use) {
        tracing::warn!("{problem}");
    }
}

/// One line for each name in `server.allowed_hosts` that no request could ever carry. What is compared is a
/// bare name with its port already taken off, whole and without regard to case (see [`crate::auth`]), so an
/// entry spelled like a URL, or with a port on it, a path, a wildcard, the dot DNS appends, or a space in it
/// is one nobody can satisfy - and the person who wrote it would otherwise see only a `403` for the name they
/// are sure they listed. It is said here, once, rather than per request because this is when they are still
/// looking at the file, and because nothing changes the list while the server runs. Whether anyone has to
/// sign in does not change the answer: an entry nobody can type is wrong whether or not it is being consulted.
fn warn_on_hosts_that_cannot_match(allowed: &[String]) {
    for host in allowed {
        if let Some(problem) = crate::auth::allowed_host_problem(host) {
            tracing::warn!(
                host = %routes::clean(host),
                problem,
                "this name in server.allowed_hosts can never be the name a request carries",
            );
        }
    }
}

/// One line for one report from the engine. Every kind goes out as a warning because every one means
/// something is not as the person expects: a state file that had to be kept aside, a torrent whose resume
/// data would not come back, a save that left a torrent out. The text is the engine's own line, already
/// cleaned for display by whoever made it.
fn warn_notice(notice: &Notice) {
    match notice {
        Notice::Info(text) | Notice::Error(text) | Notice::SignInRefused(text) => {
            tracing::warn!(problem = %text, "the library reported a problem");
        }
        // A file to play is not a report about the library: nothing to warn about.
        Notice::Play(_) => {}
    }
}

/// Saves the library every [`SAVE_EVERY`] until told to stop, then does the last one and releases the
/// engine - which is what releases the library lock.
///
/// The task owns the engine for exactly this reason: the save, the final save and the lock all have to
/// happen in that order, on something that cannot be interrupted halfway by a request being answered.
/// Its stop signal is a channel rather than a message it has to read: whoever started the server tells it
/// to finish by sending, and a server that was simply dropped closes the channel, which counts the same.
async fn save_loop(
    mut engine: Engine,
    mut reports: mpsc::Receiver<Notice>,
    mut stop: mpsc::Receiver<()>,
) -> Result<(), String> {
    // The first tick of a tokio interval is ready at once, and a save then would rewrite a state file
    // that has not changed since it was read - so it is spent before the loop starts, as it is in the
    // terminal's backend. Missed ticks are skipped rather than queued: a save that could not run for ten
    // minutes because the machine was busy does not need to catch up ten saves.
    let mut tick = tokio::time::interval(SAVE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick.tick().await;

    loop {
        tokio::select! {
            biased;
            // Some(()) is the shutdown signal, None is the server being dropped. Both mean stop.
            _ = stop.recv() => break,
            _ = tick.tick() => {
                if let Some(problem) = engine.save_periodic().await {
                    warn_notice(&problem);
                }
                // After the save, not before: what this save queued - a torrent whose resume data would
                // not come back, one it left out - is the news worth having, and reading it here means it
                // is said as soon as it exists instead of one interval later.
                read_reports(&mut reports);
            }
        }
    }
    // Whatever the last save queued is still worth saying, and nobody is left to queue more: the sender
    // lives inside `engine`, which goes when this returns.
    read_reports(&mut reports);
    engine.shutdown().await
}

/// Logs every report the engine has queued, without waiting for room in the queue. Nothing here can block
/// on a log, and a save must not be delayed by one either.
fn read_reports(reports: &mut mpsc::Receiver<Notice>) {
    while let Ok(notice) = reports.try_recv() {
        warn_notice(&notice);
    }
}

#[cfg(test)]
mod tests {
    use super::passwords_are_exposed_warning;
    use std::net::SocketAddr;

    /// What the warning says, and when it is said at all. Checked as text rather than by catching a log
    /// line: the sentence is the product here - it is the one thing a person who bound this server onto a
    /// public address gets to read before they decide what to put in front of it - and no log subscriber
    /// runs in this harness to catch the line itself.
    #[test]
    fn the_password_warning_names_the_gateway_and_the_alternative() {
        let reachable: SocketAddr = ([0, 0, 0, 0], 8090).into();
        let warning = passwords_are_exposed_warning(reachable, true, true)
            .expect("logins on, accounts in the file, an address anyone can reach: warn");
        assert_eq!(
            warning,
            "passwords cross the network unencrypted on 0.0.0.0:8090: on untrusted networks put the \
             featherbit gateway (or another API gateway or reverse proxy) in front for TLS, throttling \
             and further authentication, or use device keys"
        );

        // The three answers that mean there is nothing to say: nobody can reach this address, nobody has
        // to give a password, and there is no account a password could be for.
        let loopback: SocketAddr = ([127, 0, 0, 1], 8090).into();
        assert!(
            passwords_are_exposed_warning(loopback, true, true).is_none(),
            "loopback is already reachable only from this machine"
        );
        assert!(
            passwords_are_exposed_warning(reachable, false, true).is_none(),
            "with logins off no password crosses anything"
        );
        assert!(
            passwords_are_exposed_warning(reachable, true, false).is_none(),
            "with no accounts there is no password to cross"
        );
    }
}
