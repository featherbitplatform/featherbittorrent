//! The routes featherbittorrent adds in front of fbt-api: logins, devices and folder browsing.
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

use std::collections::BTreeMap;
// Only the test-support counter needs this; the field it updates exists only with that feature too.
#[cfg(feature = "test-support")]
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::rejection::{BytesRejection, JsonRejection, PathRejection, QueryRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use fbtt_core::{Config, DeviceEntry, Paths, UserEntry};
use serde::Serialize;
use ssh_key::PublicKey;

use crate::auth::{FbttAuth, keys, locked};

/// Remote text before it goes into a response or a log line: a folder name read off the disk, a
/// device name from a config file, the words of a parse error about something a client sent. The
/// workspace rule is that remote text is display-safe, and these routes are where remote text first
/// reaches something a person will read.
pub(crate) fn clean(text: &str) -> String {
    fbt::display_safe(text, fbt::MAX_DISPLAY_CHARS)
}

/// Who may log in, which devices the login route recognises, which folders the file routes may name,
/// and where the file all of that came from. One of these is built per server and shared by every
/// handler.
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
    /// The hash an unknown user name is verified against, so that answering "no such account" costs
    /// what answering "wrong password" costs. `None` when no account could have been found anyway -
    /// with no users, or with nobody signing in, there is no name worth hiding. See [`login`].
    dummy_password_hash: Option<String>,
    /// How many Argon2 verifications this server has run. Nothing in a run reads it; the test that
    /// proves an unknown user name still costs a verify does.
    #[cfg(feature = "test-support")]
    verifies: AtomicU64,
}

impl Shared {
    /// The state for one server: its own login stores, its devices, and the folders `config` names.
    ///
    /// `roots` are those folders as canonicalised directories - the same list
    /// [`fbt_api::ApiConfig::allowed_roots`] is handed - and `dummy_password_hash` is the hash computed
    /// once at startup for the accounts this config has, or `None` when it has none worth hiding behind.
    pub(crate) fn new(
        config: Config,
        paths: Paths,
        roots: Vec<fs::Root>,
        dummy_password_hash: Option<String>,
    ) -> Self {
        let devices = Mutex::new(DeviceTable::build(&config.server.auth.devices));
        Shared {
            auth: Arc::new(FbttAuth::new(&config.server)),
            users: config.server.auth.users.clone(),
            devices,
            changing_devices: tokio::sync::Mutex::new(()),
            base_config: config,
            paths,
            roots: roots.into(),
            dummy_password_hash,
            #[cfg(feature = "test-support")]
            verifies: AtomicU64::new(0),
        }
    }

    /// The authenticator for these stores, which is what fbt-api is handed beside the routes: the guard
    /// that decides who may reach them and the handlers that hand out tokens read the same sessions, so a
    /// token issued here is accepted there and nowhere else.
    pub(crate) fn authenticator(&self) -> Arc<FbttAuth> {
        Arc::clone(&self.auth)
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

    /// The devices to show, in the order the config has them.
    pub(crate) fn device_rows(&self) -> Vec<DeviceRow> {
        let devices = locked(&self.devices);
        devices.entries.clone()
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

    /// Counts one Argon2 verification as having run. Under `test-support` that is what lets a test
    /// prove an unknown user name still costs a verify; in a run nothing reads the count, so this
    /// records nothing and the field it would update is not even there.
    pub(crate) fn count_verify(&self) {
        #[cfg(feature = "test-support")]
        self.verifies
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// How many Argon2 verifications this server has run since it started. Under `test-support` this is
    /// how the test that says "an unknown user name costs a verify" reads the answer; in a run nothing
    /// counts and so there is nothing to read. See [`crate::Server::password_verifies`].
    #[cfg(feature = "test-support")]
    pub(crate) fn password_verifies(&self) -> u64 {
        self.verifies.load(std::sync::atomic::Ordering::Relaxed)
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
/// `forbidden`, `not_found`, `already_exists`, `too_many_requests`, `internal`) and a message.
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
    // The two login calls carry their own body limit, far smaller than the 32 MiB fbt-api keeps for
    // uploading a `.torrent`: they are the two places this server takes bytes from someone it has not
    // yet decided is anybody, and all three fields of a key login fit in a few hundred bytes.
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
        .with_state(shared)
}
