//! The server end to end: a library in a scratch directory, requests over http, and what every answer
//! has to be.
//!
//! Each test starts its own server on `127.0.0.1:0` with its own library, because the things being tested
//! are per-server: a login limiter counts attempts against an address, a challenge store holds nonces, and
//! a session token belongs to the server that issued it and nobody else. Sharing one server between tests
//! would make one test's failures somebody else's rate limit.
//!
//! The engine settings are loopback-only - no DHT, no multicast, nothing that could leave the machine - so
//! what these tests exercise is logins, devices and folder browsing rather than the BitTorrent parts fbt-api
//! also serves. Every wait has a deadline, so a server that stops answering fails with a message instead of
//! hanging the test job.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use fbtt_core::scratch::ScratchDir;
use fbtt_core::{AuthConfig, Config, DeviceEntry, LibraryLock, Paths, ServerConfig, UserEntry};
use fbtt_server::LOGIN_NAMESPACE;
use fbtt_server::Server;
use fbtt_server::auth::password;
use reqwest::{Response, StatusCode};
use serde_json::Value;
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey, rand_core::UnwrapErr};

/// How long a test waits for anything from the server. Starting an engine takes a moment and an Argon2
/// verify is meant to be slow; nothing here should take longer than that many times over.
const DEADLINE: Duration = Duration::from_secs(60);
/// A session token's life, from this crate's session store: what `expires_in` may never exceed.
const SESSION_TTL_SECS: u64 = 3600;
/// How many failed logins one address gets in a window, and how many challenges it gets in a minute - the
/// two budgets the limiter tests fill up.
const LOGIN_BUDGET: usize = 10;
const CHALLENGE_BUDGET: usize = 30;

/// Runs `what` under the deadline, so a server that never answers is a failure with a message rather than
/// a hung test job.
async fn within<T>(what: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
}

/// A server running on a library of its own, with the scratch directory kept alive beside it.
struct Served {
    /// The scratch library, removed when this goes down. Held for no other reason than that everything
    /// the test works on is inside it.
    dir: ScratchDir,
    paths: Paths,
    config: Config,
    server: Server,
}

impl Served {
    /// A server on a library in a scratch directory named after `tag`. The directory is made here and
    /// handed to `base`, so the config can name its download folder as soon as it knows where it is; what
    /// comes back is resolved the way a run resolves it, which is what turns an empty `allowed_roots` into
    /// "the download folder alone".
    ///
    /// The library lock is taken here and given to the server, which holds it until its last save - so a
    /// second server on this library could not start while this one runs, which is the point of the lock.
    async fn start(tag: &str, base: impl FnOnce(&Path) -> Config) -> Served {
        let dir = ScratchDir::new(tag);
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        let config = base(dir.path()).resolve(&paths);
        let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
        let server = within(Server::start_with_settings(
            loopback(&config),
            config.clone(),
            &paths,
            lock,
        ))
        .await
        .expect("start the server");
        Served {
            dir,
            paths,
            config,
            server,
        }
    }

    /// The address to send requests to.
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.server.local_addr())
    }

    /// The one folder this server may name: the download folder, which is what [`Config::resolve`] makes
    /// the only allowed root when the config named none.
    fn root(&self) -> &Path {
        &self.config.download_dir
    }

    /// Stops serving, saves and releases the library, so the scratch directory goes after the last write
    /// rather than possibly in the middle of it.
    async fn stop(self) {
        let Served {
            dir: _,
            paths: _,
            config: _,
            server,
        } = self;
        within(server.shutdown()).await.expect("the last save ran");
    }
}

/// The settings every engine here runs on: this config's, with the listening moved to loopback on a port
/// the OS picks and with DHT, LSD, UPnP and NAT-PMP turned off. Nothing a test starts can reach another
/// machine, so what these tests measure is the HTTP layer rather than how well the BitTorrent parts find
/// each other.
fn loopback(config: &Config) -> fbt::Settings {
    let mut settings = config.session_settings();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    settings
}

/// A config that serves on a port the OS picks with `users` and `devices` as its logins, auth set to
/// `auth`, and downloads in `<scratch>/downloads`.
fn base_config(
    scratch: &Path,
    users: Vec<UserEntry>,
    devices: Vec<DeviceEntry>,
    auth: Option<bool>,
) -> Config {
    Config {
        download_dir: scratch.join("downloads"),
        // These tests are about serving, so their configs say so.
        server: Some(ServerConfig {
            bind: "127.0.0.1:0".parse().expect("a loopback bind address"),
            auth: AuthConfig {
                enabled: auth,
                users,
                devices,
            },
            ..ServerConfig::default()
        }),
        ..Config::default()
    }
}

/// The account every test that needs a password uses. A name no real person has and a password worth
/// printing in a test log, which is what it is: neither is a secret here.
fn alice() -> UserEntry {
    UserEntry {
        name: "alice".to_string(),
        password_hash: password::hash("hunter2"),
    }
}

/// A device entry for `key`, named `name`.
fn device(name: &str, key: &PrivateKey) -> DeviceEntry {
    DeviceEntry {
        name: name.to_string(),
        public_key: public_line(key),
    }
}

/// A fresh Ed25519 key pair, the kind `ssh-keygen -t ed25519` makes. `PrivateKey::random` wants a source
/// that cannot fail, so the system's random bytes go through `UnwrapErr`.
fn keypair() -> PrivateKey {
    let mut rng = UnwrapErr(getrandom::SysRng);
    PrivateKey::random(&mut rng, Algorithm::Ed25519).expect("an ed25519 key")
}

/// The one-line OpenSSH public key of `key`: what a config stores and what a request names.
fn public_line(key: &PrivateKey) -> String {
    key.public_key().to_openssh().expect("the public key line")
}

/// The armored `sshsig` `key` makes over the challenge nonce spelled `nonce`, in the login namespace -
/// what a device sends to answer a challenge.
fn sign(nonce: &str, key: &PrivateKey) -> String {
    let bytes = BASE64_URL_SAFE_NO_PAD
        .decode(nonce)
        .expect("the nonce we were given");
    key.sign(LOGIN_NAMESPACE, HashAlg::Sha512, &bytes)
        .expect("sign the challenge")
        .to_pem(LineEnding::LF)
        .expect("armour the signature")
}

/// The client every request goes through. Redirects are left un-followed so a test sees the answer the
/// server actually gave, there is no per-request timeout ([`within`] is what bounds a test), and nothing is
/// sent through a proxy: every URL here is `127.0.0.1`, and an `HTTP_PROXY` in the environment of whoever
/// ran `cargo test` must not turn a loopback request into one that leaves the machine.
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("an http client")
}

/// A `GET` with the bearer token, when there is one.
async fn get(client: &reqwest::Client, url: &str, token: Option<&str>) -> Response {
    let mut request = client.get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    within(request.send()).await.expect("the server answered")
}

/// A `GET` with one query parameter, which is how `fs/list` names the folder to look inside.
async fn get_with_query(
    client: &reqwest::Client,
    url: &str,
    name: &str,
    value: &Path,
    token: Option<&str>,
) -> Response {
    let mut request = client
        .get(url)
        .query(&[(name, value.to_string_lossy().as_ref())]);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    within(request.send()).await.expect("the server answered")
}

/// A `POST` of a JSON body, with the bearer token when there is one.
async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: Value,
    token: Option<&str>,
) -> Response {
    let mut request = client.post(url).json(&body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    within(request.send()).await.expect("the server answered")
}

/// A `POST` of nothing at all with `Authorization: Basic`, the password login's credential.
async fn post_basic(client: &reqwest::Client, url: &str, name: &str, secret: &str) -> Response {
    within(client.post(url).basic_auth(name, Some(secret)).send())
        .await
        .expect("the server answered")
}

/// A `DELETE` with the bearer token, when there is one.
async fn delete(client: &reqwest::Client, url: &str, token: Option<&str>) -> Response {
    let mut request = client.delete(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    within(request.send()).await.expect("the server answered")
}

/// A `GET` that says which name it came to, and where it came from. reqwest takes the `Host` from the URL -
/// an address, since every test here connects to one - so overriding it is how a test sends the name a
/// browser would send for it, which is what the server reads when nobody has to sign in.
async fn get_from(
    client: &reqwest::Client,
    url: &str,
    host: &str,
    origin: Option<&str>,
) -> Response {
    let mut request = client.get(url).header(reqwest::header::HOST, host);
    if let Some(origin) = origin {
        request = request.header(reqwest::header::ORIGIN, origin);
    }
    within(request.send()).await.expect("the server answered")
}

/// The `{error: {code, message}}` of a response, with the message as well as the code - for the errors
/// whose point is in the words.
async fn problem_of(response: Response) -> (StatusCode, String, String) {
    let status = response.status();
    let body = body(response).await;
    let code = body["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("an error without a code in it: {body}"))
        .to_string();
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("an error without a message in it: {body}"))
        .to_string();
    (status, code, message)
}

/// The JSON a response carries, which every route here answers in - errors included.
async fn body(response: Response) -> Value {
    within(response.json::<Value>()).await.expect("a JSON body")
}

/// The `{token, expires_in, subject}` of a login that succeeded, checked while it is still news: a token
/// with no life in it, or no subject behind it, is not a session.
async fn session_of(response: Response) -> (String, String) {
    let status = response.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "a login that did not succeed: {status}"
    );
    let body = body(response).await;
    let token = body["token"].as_str().expect("a session token").to_string();
    let subject = body["subject"].as_str().expect("a subject").to_string();
    let expires_in = body["expires_in"].as_u64().expect("a lifetime");
    assert!(
        !token.is_empty() && expires_in > 0 && expires_in <= SESSION_TTL_SECS,
        "an implausible token lifetime: {expires_in}",
    );
    (token, subject)
}

/// The `{error: {code, message}}` every error here carries, so that one parse handles the whole API - our
/// routes and fbt-api's alike.
async fn error_of(response: Response) -> (StatusCode, String) {
    let status = response.status();
    let body = body(response).await;
    let code = body["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("an error without a code in it: {body}"))
        .to_string();
    (status, code)
}

/// `POST /api/v1/auth/challenge` for `public_key`, and the nonce it was given. A challenge is answerable
/// for a minute, which is what `expires_in` says.
async fn challenge(client: &reqwest::Client, base: &str, public_key: &str) -> String {
    let response = post_json(
        client,
        &format!("{base}/api/v1/auth/challenge"),
        serde_json::json!({ "public_key": public_key }),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "a challenge refused");
    let body = body(response).await;
    assert_eq!(
        body["expires_in"].as_u64(),
        Some(60),
        "a challenge is answerable for a minute",
    );
    body["nonce"].as_str().expect("a nonce").to_string()
}

/// A password login, from the credential to the token.
async fn log_in_with_password(served: &Served, name: &str, secret: &str) -> String {
    let response = post_basic(&http(), &served.url("/api/v1/auth/session"), name, secret).await;
    session_of(response).await.0
}

/// A key login: the challenge, the answer, the token - and the public key it was done with, because half
/// these tests want to name that key afterwards.
async fn log_in_with_key(served: &Served, key: &PrivateKey) -> (String, String, String) {
    let client = http();
    let base = served.url("");
    let line = public_line(key);
    let nonce = challenge(&client, &base, &line).await;
    let response = post_json(
        &client,
        &format!("{base}/api/v1/auth/session"),
        serde_json::json!({
            "public_key": line,
            "nonce": nonce,
            "signature": sign(&nonce, key),
        }),
        None,
    )
    .await;
    let (token, subject) = session_of(response).await;
    (token, line, subject)
}

/// A directory, and its parents, made.
fn make_dirs(path: &Path) {
    fs::create_dir_all(path).expect("make a test folder");
}

/// Deterministic bytes, so a test can compare what it streamed with what it seeded without a random seed.
fn sample(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state: u32 = 0x2545_f491;
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Writes `bytes` to `<dir>/<name>` and returns the path.
fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    make_dirs(dir);
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write a test file");
    path
}

/// A folder `name` under `dir` holding `files` - `(name, bytes)` for each - and its path. Two files is what
/// a test that tells file indices apart needs; the names sort in the order the list is written in.
fn write_folder(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let folder = dir.join(name);
    make_dirs(&folder);
    for (file, bytes) in files {
        fs::write(folder.join(file), bytes).expect("write a test file");
    }
    folder
}

/// The `.torrent` for the file or folder at `path`, hashed off this thread: a v1 torrent, so its file list
/// is the files themselves. A hybrid one (what [`fbt::CreateTorrent`] makes by default) puts a BEP 47 pad
/// between them, and then index 1 is not the second file anybody wrote.
async fn torrent_for(path: &Path) -> Vec<u8> {
    let path = path.to_path_buf();
    within(tokio::task::spawn_blocking(move || {
        fbt::CreateTorrent::new(path)
            .version(fbt::TorrentVersion::V1)
            .build(&mut |_done, _total| std::ops::ControlFlow::Continue(()))
            .expect("hash the test torrent")
    }))
    .await
    .expect("the hashing task ran")
}

/// The id of a torrent `served` is seeding: `path_in_root` (a file, or a folder whose files the torrent
/// names) was written inside this server's own root before this, so its add - through fbt-api's own route,
/// exactly as any client's arrives - has nothing to download, only to check. Seeded rather than downloaded
/// because the routes under test answer about a file that is there.
async fn seed_torrent(served: &Served, token: Option<&str>, path_in_root: &Path) -> String {
    let bytes = torrent_for(path_in_root).await;
    let mut request = http()
        .post(served.url("/api/v1/torrents"))
        .query(&[("save_path", served.root().to_string_lossy().as_ref())])
        .header(reqwest::header::CONTENT_TYPE, "application/x-bittorrent")
        .body(bytes);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = within(request.send())
        .await
        .expect("the add reached the server");
    if !response.status().is_success() {
        let (status, code) = error_of(response).await;
        panic!("the test torrent was not added: {status} {code}");
    }
    let info = body(response).await;
    let id = info["id"]
        .as_str()
        .expect("the answer names the torrent by its 40-hex wire hash")
        .to_string();
    wait_until_seeding(served, token, &id).await;
    id
}

/// Waits until this server says torrent `id` is seeding - every piece of what was already on disk checked -
/// so that a stream request after it reads bytes rather than waiting for pieces nobody has.
async fn wait_until_seeding(served: &Served, token: Option<&str>, id: &str) {
    let client = http();
    let until = tokio::time::Instant::now() + DEADLINE;
    loop {
        let info = body(
            get(
                &client,
                &served.url(&format!("/api/v1/torrents/{id}")),
                token,
            )
            .await,
        )
        .await;
        if info["state"].as_str() == Some("seeding") {
            return;
        }
        if tokio::time::Instant::now() >= until {
            panic!(
                "torrent {id} was not seeding within {} seconds: {info}",
                DEADLINE.as_secs(),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One request on a stream address, which carries its own credential in its query: `HEAD` or `GET`, and a
/// `Range` header with it when the test asks for part of the file.
async fn open_stream(
    client: &reqwest::Client,
    head: bool,
    url: &str,
    range: Option<&str>,
) -> Response {
    let mut request = if head {
        client.head(url)
    } else {
        client.get(url)
    };
    if let Some(range) = range {
        request = request.header(reqwest::header::RANGE, range);
    }
    within(request.send()).await.expect("the server answered")
}

/// What `POST /api/v1/stream-tokens` answers for file `file` of torrent `id`: the 201 checked while it is
/// still news, and the path it names - with `?token=` on it on a server that hands tokens out at all.
async fn stream_address(served: &Served, token: Option<&str>, id: &str, file: usize) -> String {
    let response = post_json(
        &http(),
        &served.url("/api/v1/stream-tokens"),
        serde_json::json!({ "torrent": id, "file": file }),
        token,
    )
    .await;
    let status = response.status();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "no stream address for {id} file {file}"
    );
    let answer = body(response).await;
    assert_eq!(
        answer["expires_in"].as_u64(),
        Some(60),
        "the token's first-use window is what the answer says: {answer}",
    );
    answer["path"]
        .as_str()
        .expect("the answer names the path to stream")
        .to_string()
}

// ---------------------------------------------------------------------------
// Getting a session token
// ---------------------------------------------------------------------------

/// A password buys a token, the token buys the API, and no token buys nothing. This is the design in one
/// test: `Authorization: Basic` at the login route, `Authorization: Bearer` everywhere else, and fbt-api's
/// own routes behind the same guard as ours - which is the only way a password login is worth having.
#[tokio::test]
async fn password_login_then_api() {
    let served = Served::start("password-login", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let client = http();

    let (token, subject) = session_of(
        post_basic(
            &client,
            &served.url("/api/v1/auth/session"),
            "alice",
            "hunter2",
        )
        .await,
    )
    .await;
    assert_eq!(
        subject, "alice",
        "the subject is this server's own name for the account, never the text a client typed",
    );

    let torrents = get(&client, &served.url("/api/v1/torrents"), Some(&token)).await;
    assert_eq!(
        torrents.status(),
        StatusCode::OK,
        "a token opens fbt-api's own routes",
    );

    let (status, code) = error_of(get(&client, &served.url("/api/v1/torrents"), None).await).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no token, no API");
    assert_eq!(code, "unauthorized");

    // The wrong password gets the same shape of answer as a missing token, and nothing that says whether
    // the name was known.
    let (status, code) = error_of(
        post_basic(
            &client,
            &served.url("/api/v1/auth/session"),
            "alice",
            "hunter3",
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(code, "unauthorized");

    served.stop().await;
}

/// The login routes cap what a stranger can make this server hold: 64 KiB, not the 32 MiB fbt-api keeps
/// for uploading a `.torrent`. A key login is three short fields, and these are the only two calls that
/// take bytes from someone whose identity is not known yet. The refusal is this API's own error shape, and
/// it costs the address nothing: no verify ran to find out the body was too big.
#[tokio::test]
async fn login_bodies_are_capped() {
    let key = keypair();
    let served = Served::start("login-body", |scratch| {
        base_config(
            scratch,
            vec![alice()],
            vec![device("watch", &key)],
            Some(true),
        )
    })
    .await;
    let client = http();

    // Each refused body gets a client of its own, on purpose: when this server stops reading a body it has
    // refused, the connection that carried it is over, and a client reusing that connection would be
    // writing into a socket the server had already closed. What is being checked here is what the route
    // answers, not what hyper makes of a half-written request on a connection it no longer wants.
    let oversized = serde_json::json!({
        "public_key": "ssh-ed25519 AAAA",
        "nonce": "x".repeat(70 * 1024),
        "signature": "not a signature",
    });
    let (status, code) = error_of(
        post_json(
            &http(),
            &served.url("/api/v1/auth/session"),
            oversized,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a body this route does not take",
    );
    assert_eq!(code, "body_too_large");

    // The same size is refused on the challenge route, which is the other place a stranger can post, and
    // with the same body shape: an extractor's own refusal comes back in this API's error shape rather
    // than axum's plain-text one. A body that is not the right shape at all gets the same treatment.
    let (status, code) = error_of(
        post_json(
            &http(),
            &served.url("/api/v1/auth/challenge"),
            serde_json::json!({ "public_key": "x".repeat(70 * 1024) }),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(code, "body_too_large");

    let (status, code) = error_of(
        post_json(
            &http(),
            &served.url("/api/v1/auth/challenge"),
            serde_json::json!({ "no_public_key_here": 1 }),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a body of the wrong shape");
    assert_eq!(code, "invalid_request");

    // A body of a normal size still gets in, so this is a cap and not a broken route.
    let line = public_line(&key);
    let nonce = challenge(&client, &served.url(""), &line).await;
    let response = post_json(
        &client,
        &served.url("/api/v1/auth/session"),
        serde_json::json!({ "public_key": line, "nonce": nonce, "signature": sign(&nonce, &key) }),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "a body of a normal size");

    // And the refusals above cost the address nothing: no credential was looked at, so none of them was a
    // failed login. The password login that follows has its whole budget left.
    let limited = post_basic(
        &client,
        &served.url("/api/v1/auth/session"),
        "alice",
        "hunter2",
    )
    .await;
    assert_eq!(
        limited.status(),
        StatusCode::OK,
        "the address still has its attempts left",
    );

    served.stop().await;
}

/// A device key answers a challenge and gets the same kind of session a password gets. The signature is
/// made by this test's private key over the nonce the server handed out, in `featherbittorrent-login`, and
/// the subject is the name the config gave that key.
#[tokio::test]
async fn key_login_then_api() {
    let key = keypair();
    let served = Served::start("key-login", |scratch| {
        base_config(scratch, Vec::new(), vec![device("phone", &key)], Some(true))
    })
    .await;
    let client = http();

    let (token, _, subject) = log_in_with_key(&served, &key).await;
    assert_eq!(
        subject, "phone",
        "the subject is the name the config gave the key"
    );

    let torrents = get(&client, &served.url("/api/v1/torrents"), Some(&token)).await;
    assert_eq!(
        torrents.status(),
        StatusCode::OK,
        "a key login opened the API"
    );

    served.stop().await;
}

/// One challenge buys one attempt. Presenting the nonce spends it whether or not the signature then held,
/// so the same answer presented twice is refused - which is what stops a response someone captured from
/// being replayed for as long as a token would have lived.
#[tokio::test]
async fn challenge_is_single_use() {
    let key = keypair();
    let served = Served::start("challenge-once", |scratch| {
        base_config(
            scratch,
            Vec::new(),
            vec![device("laptop", &key)],
            Some(true),
        )
    })
    .await;
    let client = http();
    let base = served.url("");
    let line = public_line(&key);

    let nonce = challenge(&client, &base, &line).await;
    let answer = serde_json::json!({
        "public_key": line,
        "nonce": nonce,
        "signature": sign(&nonce, &key),
    });
    let first = post_json(
        &client,
        &format!("{base}/api/v1/auth/session"),
        answer.clone(),
        None,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK, "the first attempt got in");

    let (status, code) = error_of(
        post_json(
            &client,
            &format!("{base}/api/v1/auth/session"),
            answer,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the same challenge answered twice",
    );
    assert_eq!(code, "unauthorized");

    // A new challenge works again: what was spent was the nonce, not the device.
    let fresh = challenge(&client, &base, &line).await;
    let again = post_json(
        &client,
        &format!("{base}/api/v1/auth/session"),
        serde_json::json!({
            "public_key": line,
            "nonce": fresh,
            "signature": sign(&fresh, &key),
        }),
        None,
    )
    .await;
    assert_eq!(
        again.status(),
        StatusCode::OK,
        "a new challenge is a new attempt"
    );

    served.stop().await;
}

/// A nonce belongs to the key it was given to. Answering it with somebody else's private key - and naming
/// that key - gets the answer an unknown device gets, so a challenge cannot be carried between devices.
#[tokio::test]
async fn challenge_is_bound_to_key() {
    let holder = keypair();
    let other = keypair();
    let served = Served::start("challenge-bound", |scratch| {
        base_config(
            scratch,
            Vec::new(),
            vec![device("tablet", &holder)],
            Some(true),
        )
    })
    .await;
    let client = http();
    let base = served.url("");

    // The nonce is issued for the configured key...
    let nonce = challenge(&client, &base, &public_line(&holder)).await;
    // ...and here it is answered by a key this config has never heard of, correctly signed.
    let (status, code) = error_of(
        post_json(
            &client,
            &format!("{base}/api/v1/auth/session"),
            serde_json::json!({
                "public_key": public_line(&other),
                "nonce": nonce,
                "signature": sign(&nonce, &other),
            }),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a nonce is not a bearer token: it belongs to the key it was handed to",
    );
    assert_eq!(code, "unauthorized");

    served.stop().await;
}

/// An unknown key still gets a nonce - that is how the challenge route avoids becoming somewhere to list
/// who is authorised - but a perfect signature with it never gets in, because this server has no copy of
/// that key to verify against. The verification key always comes from the config.
#[tokio::test]
async fn challenge_for_unknown_key_never_verifies() {
    let unknown = keypair();
    let served = Served::start("challenge-unknown", |scratch| {
        base_config(scratch, Vec::new(), Vec::new(), Some(true))
    })
    .await;
    let client = http();
    let base = served.url("");
    let line = public_line(&unknown);

    let nonce = challenge(&client, &base, &line).await;
    let (status, code) = error_of(
        post_json(
            &client,
            &format!("{base}/api/v1/auth/session"),
            serde_json::json!({
                "public_key": line,
                "nonce": nonce,
                "signature": sign(&nonce, &unknown),
            }),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an unknown key is not a device here",
    );
    assert_eq!(code, "unauthorized");

    served.stop().await;
}

/// Ten failed logins in a window close the door on an eleventh, whatever it contains. The refusal arriving
/// before the credential is looked at is the point: the eleventh attempt costs this server no hashing, so a
/// brute-force run spends its own budget rather than the machine's CPU - and the right password arriving
/// after the budget went has to wait as well.
#[tokio::test]
async fn login_failures_are_rate_limited() {
    let served = Served::start("login-limited", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let client = http();
    let url = served.url("/api/v1/auth/session");

    for attempt in 1..=LOGIN_BUDGET {
        let response = post_basic(&client, &url, "alice", "wrong").await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "attempt {attempt} of the budget",
        );
    }

    let (status, code) = error_of(post_basic(&client, &url, "alice", "wrong").await).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "the budget is spent");
    assert_eq!(code, "too_many_requests");

    let correct = post_basic(&client, &url, "alice", "hunter2").await;
    assert_eq!(
        correct.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "it is the address that is closed, not the password",
    );
    let retry_after = correct
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .expect("a 429 says when to come back");
    let secs: u64 = retry_after.parse().expect("Retry-After is delay-seconds");
    assert!(secs > 0, "a wait of nothing is not a wait");

    served.stop().await;
}

/// Asking for a challenge is capped too, and higher than the login budget: asking has guessed nothing yet,
/// so it should not eat the attempts a device has. What it can cost this server is memory for every
/// *configured* key that asks - which is why an answer for an unknown key is issued but never stored.
#[tokio::test]
async fn challenge_issuance_is_rate_limited() {
    let unknown = keypair();
    let served = Served::start("challenge-limited", |scratch| {
        base_config(scratch, Vec::new(), Vec::new(), Some(true))
    })
    .await;
    let client = http();
    let base = served.url("");
    let line = public_line(&unknown);

    for attempt in 1..=CHALLENGE_BUDGET {
        let response = post_json(
            &client,
            &format!("{base}/api/v1/auth/challenge"),
            serde_json::json!({ "public_key": line }),
            None,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "challenge {attempt} of the budget",
        );
    }

    let (status, code) = error_of(
        post_json(
            &client,
            &format!("{base}/api/v1/auth/challenge"),
            serde_json::json!({ "public_key": line }),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "asking is capped as well, just higher",
    );
    assert_eq!(code, "too_many_requests");

    served.stop().await;
}

/// An unknown user name costs an Argon2 verify. That is the whole reason a fixed dummy hash exists: if "no
/// such account" answered immediately, someone holding no credential at all could read this server's list
/// of accounts by timing requests, and the login route is exactly where they would try it.
#[tokio::test]
async fn unknown_user_costs_a_verify() {
    let served = Served::start("unknown-user", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let client = http();
    let url = served.url("/api/v1/auth/session");

    let before = served.server.password_verifies();
    let response = post_basic(&client, &url, "nobody", "whatever").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let after_unknown = served.server.password_verifies();
    assert_eq!(
        after_unknown,
        before + 1,
        "an unknown name was answered without hashing anything",
    );

    // A known name with the wrong password costs exactly as much - which is what makes the two answers
    // indistinguishable to whoever is timing them.
    let response = post_basic(&client, &url, "alice", "wrong").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(served.server.password_verifies(), after_unknown + 1);

    // And a right password costs one too: the count is about the work, not about the answer.
    let response = post_basic(&client, &url, "alice", "hunter2").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(served.server.password_verifies(), after_unknown + 2);

    served.stop().await;
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

/// The devices a key login recognises can be listed, added and removed over the API, and `config.yaml`
/// says so afterwards: what a person sees in the list is what the next run will read. A name already taken
/// is refused, adding one is not free, and the running server keeps agreeing with the file it just rewrote.
#[tokio::test]
async fn devices_add_list_remove_persist() {
    let kept = keypair();
    let gone = keypair();
    let served = Served::start("devices", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let client = http();
    let token = log_in_with_password(&served, "alice", "hunter2").await;
    let url = served.url("/api/v1/auth/devices");

    for (name, key) in [("kept", &kept), ("gone", &gone)] {
        let response = post_json(
            &client,
            &url,
            serde_json::json!({ "name": name, "public_key": public_line(key) }),
            Some(&token),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "{name} was not added"
        );
    }

    let listed = body(get(&client, &url, Some(&token)).await).await;
    let rows = listed.as_array().expect("a list of devices");
    let names: Vec<&str> = rows
        .iter()
        .map(|row| row["name"].as_str().expect("a device name"))
        .collect();
    assert_eq!(
        names,
        vec!["kept", "gone"],
        "the list is the config's order"
    );
    let fingerprint = rows[0]["fingerprint"]
        .as_str()
        .expect("a fingerprint")
        .to_string();
    assert!(
        fingerprint.starts_with("SHA256:"),
        "the short print a person checks by eye: {fingerprint}",
    );

    let (status, code) = error_of(
        post_json(
            &client,
            &url,
            serde_json::json!({ "name": "kept", "public_key": public_line(&gone) }),
            Some(&token),
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "two devices cannot share a name"
    );
    assert_eq!(code, "already_exists");

    // A device is part of who may log in, so an anonymous caller does not get to add one.
    let (status, code) = error_of(
        post_json(
            &client,
            &url,
            serde_json::json!({ "name": "nobody", "public_key": public_line(&gone) }),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "adding a device is not free"
    );
    assert_eq!(code, "unauthorized");

    // A key this server cannot use never reaches the file: its fingerprint cannot even be computed.
    let (status, _) = error_of(
        post_json(
            &client,
            &url,
            serde_json::json!({ "name": "rsa", "public_key": "ssh-rsa AAAA not-a-key" }),
            Some(&token),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "not a key we can use");

    let removed = delete(&client, &format!("{url}/gone"), Some(&token)).await;
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    let (status, code) =
        error_of(delete(&client, &format!("{url}/gone"), Some(&token)).await).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "it is gone twice over");
    assert_eq!(code, "not_found");

    // The file, not just the list: the run that starts after this one reads these names.
    let written = fs::read_to_string(served.paths.config_file()).expect("the config we rewrote");
    assert!(written.contains("kept"), "{written}");
    assert!(!written.contains("gone"), "{written}");

    // And the server still running agrees with its own file.
    let listed = body(get(&client, &url, Some(&token)).await).await;
    assert_eq!(
        listed.as_array().expect("a list").len(),
        1,
        "the list follows the file",
    );

    // A removed key cannot log in any more, even though it is signed correctly: the config's copy of a key
    // is the only thing a signature is ever checked against.
    let gone_line = public_line(&gone);
    let nonce = challenge(&client, &served.url(""), &gone_line).await;
    let (status, _) = error_of(
        post_json(
            &client,
            &served.url("/api/v1/auth/session"),
            serde_json::json!({
                "public_key": gone_line,
                "nonce": nonce,
                "signature": sign(&nonce, &gone),
            }),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the removed key is not a device"
    );

    served.stop().await;
}

/// A name this server's own route added cannot need cleaning - adding one refuses anything a display cannot
/// show - so the only way an invisible character gets into a device list is through `config.yaml`, which most
/// people still edit by hand. Repeating it to a client as it was written is how a list lies: `phone\u{202e}`
/// draws as `enohp`, and the row that looks like a phone is not one. What the API answers is the name as a
/// display would draw it, which is also what fbt does with every other remote string a user reads.
#[tokio::test]
async fn device_names_reach_the_api_cleaned() {
    let odd = "phone\u{202e}".to_string();
    let key = keypair();
    let served = Served::start("device-names", |scratch| {
        base_config(scratch, vec![alice()], vec![device(&odd, &key)], Some(true))
    })
    .await;
    let client = http();
    let token = log_in_with_password(&served, "alice", "hunter2").await;
    let url = served.url("/api/v1/auth/devices");

    let listed = body(get(&client, &url, Some(&token)).await).await;
    let rows = listed.as_array().expect("a list of devices");
    assert_eq!(rows.len(), 1, "the device in the config is listed");
    assert_eq!(
        rows[0]["name"].as_str().expect("a name"),
        "phone",
        "what a client reads is what a display shows",
    );

    // And the same name cannot be put in at all, which is the rule that keeps this from being about tidiness:
    // what would need cleaning on the way out is refused on the way in.
    let (status, code, message) = problem_of(
        post_json(
            &client,
            &url,
            serde_json::json!({ "name": odd, "public_key": public_line(&keypair()) }),
            Some(&token),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{odd:?} is not a name");
    assert_eq!(code, "invalid_request");
    assert!(
        message.contains("a display can show"),
        "the answer says what a name has to be: {message}"
    );

    served.stop().await;
}

// ---------------------------------------------------------------------------
// Folder browsing
// ---------------------------------------------------------------------------

/// The destination picker's two calls: the roots, and the directories inside one of them. What they may
/// name is `server.allowed_roots` and nothing else - not `..`, not a folder somewhere else on the machine,
/// not a symlink that leads out - and what they report is folders only, sorted, cleaned for display, with
/// the dot convention left out because a hidden folder is nowhere a person means to download into.
#[tokio::test]
async fn fs_list_stays_in_roots() {
    let served = Served::start("fs", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let root = served.root().to_path_buf();
    make_dirs(&root.join("alpha"));
    make_dirs(&root.join("beta"));
    make_dirs(&root.join(".hidden"));
    fs::write(root.join("a-file"), b"not a folder").expect("write a file in the root");
    let outside = served.dir.path().join("outside");
    make_dirs(&outside);

    let client = http();
    let token = log_in_with_password(&served, "alice", "hunter2").await;
    let list = served.url("/api/v1/fs/list");

    let roots = body(get(&client, &served.url("/api/v1/fs/roots"), Some(&token)).await).await;
    assert_eq!(
        roots.as_array().expect("a list").as_slice(),
        [Value::String(root.to_string_lossy().into_owned())],
        "the root is spelled as the config spells it",
    );

    let inside = get_with_query(&client, &list, "path", &root, Some(&token)).await;
    assert_eq!(inside.status(), StatusCode::OK, "the root lists itself");
    let listing = body(inside).await;
    assert_eq!(
        listing["dirs"].as_array().expect("a list").as_slice(),
        [
            Value::String("alpha".to_string()),
            Value::String("beta".to_string()),
        ],
        "folders only, sorted, nothing hidden",
    );
    assert_eq!(
        listing["path"].as_str(),
        Some(root.to_string_lossy().as_ref()),
        "the answer reports the folder that was asked for, in the spelling it was asked for",
    );

    let empty = get_with_query(&client, &list, "path", &root.join("alpha"), Some(&token)).await;
    assert_eq!(empty.status(), StatusCode::OK);
    assert_eq!(
        body(empty).await["dirs"].as_array().expect("a list").len(),
        0,
        "an empty folder answers with an empty list",
    );

    for elsewhere in [Path::new(".."), Path::new("/"), outside.as_path()] {
        let (status, code) =
            error_of(get_with_query(&client, &list, "path", elsewhere, Some(&token)).await).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{} is not inside the allowed folder",
            elsewhere.display(),
        );
        assert_eq!(code, "path_not_allowed");
    }

    let (status, code) = error_of(
        get_with_query(&client, &list, "path", &root.join("not-here"), Some(&token)).await,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a folder that is not there");
    assert_eq!(code, "not_found");

    // A file is not a folder: the picker asks for directories, and something that cannot hold a download
    // is a bad request rather than an empty list.
    let (status, code) =
        error_of(get_with_query(&client, &list, "path", &root.join("a-file"), Some(&token)).await)
            .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "not a folder");
    assert_eq!(code, "invalid_request");

    // A NUL byte - which `%00` in a query string hands over as an ordinary character - is where a path
    // stops, so one in the middle is not a path anything could resolve. It used to arrive as a `500`,
    // blaming this server for a name somebody else invented.
    let (status, code, message) = problem_of(
        get_with_query(
            &client,
            &list,
            "path",
            &root.join(format!("a{}b", char::from(0u8))),
            Some(&token),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "not a path at all");
    assert_eq!(code, "invalid_request");
    assert!(
        message.contains("invalid path"),
        "the answer says what is wrong with it: {message}"
    );

    // A link inside the root that points out of it: spelled like it belongs, and refused once it is
    // resolved. This is the check a lexical rule cannot make on its own, so it happens after canonicalising.
    // And it is not offered by the listing of the folder it sits in either - a picker that could step
    // through it would leave the library one directory at a time, with every answer still spelling itself
    // as part of the root. A link to a folder inside the same root stays in the list: it is somewhere a
    // torrent can go.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../outside", root.join("link-out"))
            .expect("make a link out of the root");
        std::os::unix::fs::symlink("alpha", root.join("link-in"))
            .expect("make a link inside the root");
        let listing = body(get_with_query(&client, &list, "path", &root, Some(&token)).await).await;
        assert_eq!(
            listing["dirs"].as_array().expect("a list").as_slice(),
            [
                Value::String("alpha".to_string()),
                Value::String("beta".to_string()),
                Value::String("link-in".to_string()),
            ],
            "the link out of the root is not offered; the link inside it is",
        );

        let (status, code) = error_of(
            get_with_query(&client, &list, "path", &root.join("link-out"), Some(&token)).await,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a link out of the library is not inside it",
        );
        assert_eq!(code, "path_not_allowed");
    }

    served.stop().await;
}

/// With auth off - which is what a loopback-only server means by default, and what an explicit
/// `enabled: false` says about any address - every request is served without a token, including the calls
/// that would otherwise be how you got one. There is nothing to log out of here, and logging out anyway
/// still answers correctly that you are logged out. What is *not* open this way is the device list, and what
/// decides whether a request is served at all is the name it carries: see
/// [`auth_off_refuses_device_routes`] and the host tests beside this one. Every request here arrives with an
/// address in `Host`, because every test connects to one, which is a name this server answers.
#[tokio::test]
async fn auth_off_allows_all() {
    let served = Served::start("auth-off", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(false))
    })
    .await;
    let client = http();

    let torrents = get(&client, &served.url("/api/v1/torrents"), None).await;
    assert_eq!(torrents.status(), StatusCode::OK, "no token needed here");

    let roots = get(&client, &served.url("/api/v1/fs/roots"), None).await;
    assert_eq!(roots.status(), StatusCode::OK);

    let logged_out = delete(&client, &served.url("/api/v1/auth/session"), None).await;
    assert_eq!(
        logged_out.status(),
        StatusCode::NO_CONTENT,
        "logging out of nothing still means you are logged out",
    );

    // A password login still works when nobody had to log in: the accounts are in the config whether or
    // not this server asks about them, and a client that was set up with a password should not break
    // because this address happens to be trusted. What changes is what an unknown name costs - with auth
    // off there is no list of names to guard, so refusing one needs no hashing behind it.
    let before = served.server.password_verifies();
    let (token, subject) = session_of(
        post_basic(
            &client,
            &served.url("/api/v1/auth/session"),
            "alice",
            "hunter2",
        )
        .await,
    )
    .await;
    assert_eq!(subject, "alice", "the account is still the account");
    assert_eq!(
        served.server.password_verifies(),
        before + 1,
        "a real account costs a verify even here",
    );
    assert_eq!(
        get(&client, &served.url("/api/v1/torrents"), Some(&token))
            .await
            .status(),
        StatusCode::OK,
        "and the token it got works, like everything else without one",
    );

    let (status, _) = error_of(
        post_basic(
            &client,
            &served.url("/api/v1/auth/session"),
            "nobody",
            "whatever",
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "there is no account here");
    assert_eq!(
        served.server.password_verifies(),
        before + 1,
        "nothing was hashed to find that out: with auth off there is no name worth hiding",
    );

    served.stop().await;
}

/// A rebinding attack connects to 127.0.0.1 and still names the attacker's domain, so with nobody signing
/// in the name a request says it came to is what stands between a hostile page and this library. Every name
/// that is not this server's own is refused before any route sees the request - including fbt-api's own
/// routes, because this server hands fbt-api its authenticator - and the answer is the one for "you may not
/// have this", not a `401` inviting the page to guess at a token.
#[tokio::test]
async fn auth_off_refuses_foreign_host() {
    let served = Served::start("host-rebind", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(false))
    })
    .await;
    let client = http();

    for elsewhere in [
        "evil.example",
        "evil.example:8090",
        "localhost.evil.example",
        "127.0.0.1.nip.io",
    ] {
        for path in ["/api/v1/torrents", "/api/v1/fs/roots"] {
            let (status, code) =
                error_of(get_from(&client, &served.url(path), elsewhere, None).await).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{elsewhere} is not a name this server was given, and {path} is the route it reached",
            );
            assert_eq!(code, "forbidden");
        }
    }

    // Including the call a page would use to get a token of its own: refusing the name is what makes the
    // login routes unreachable to it, since with logins off those are open to whoever asks.
    let (status, code) = error_of(
        within(
            client
                .post(served.url("/api/v1/auth/session"))
                .header(reqwest::header::HOST, "evil.example")
                .send(),
        )
        .await
        .expect("the server answered"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no token for a rebound page");
    assert_eq!(code, "forbidden");

    served.stop().await;
}

/// A browser sends `Origin` on exactly the cross-site requests a rebinding page is allowed to make, so an
/// origin this server would not answer as a `Host` is refused as an origin too - even when the `Host` itself
/// is one of its own names, which is what the page gets by connecting to the address it was told about.
#[tokio::test]
async fn auth_off_refuses_foreign_origin() {
    let served = Served::start("origin-rebind", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(false))
    })
    .await;
    let client = http();
    let address = served.server.local_addr();
    let own = format!("127.0.0.1:{}", address.port());

    // The same-origin request of someone who really is using this server: the name and the place it came
    // from are both its own.
    assert_eq!(
        get_from(
            &client,
            &served.url("/api/v1/torrents"),
            &own,
            Some(&format!("http://{address}")),
        )
        .await
        .status(),
        StatusCode::OK,
    );

    for origin in [
        "http://evil.example",
        "https://evil.example:8443",
        "http://localhost.evil.example",
        "null",
        "ftp://127.0.0.1",
        "http://",
    ] {
        let (status, code) =
            error_of(get_from(&client, &served.url("/api/v1/torrents"), &own, Some(origin)).await)
                .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{origin} is not a place this server answers",
        );
        assert_eq!(code, "forbidden");
    }

    served.stop().await;
}

/// What a person reaches this server by, with nobody signing in: `localhost`, or an address - IPv4, or IPv6
/// in the brackets a host must use. A browser cannot be rebound into naming an address (the name comes from
/// the URL it was handed and only a domain is resolved), so addresses are how a trusted network uses a server
/// with no logins, and any port on one of them is the same place.
#[tokio::test]
async fn auth_off_allows_ip_literal_host() {
    let served = Served::start("host-address", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(false))
    })
    .await;
    let client = http();
    let port = served.server.local_addr().port();

    for own in [
        "localhost".to_string(),
        format!("LocalHost:{port}"),
        "127.0.0.1".to_string(),
        format!("127.0.0.1:{port}"),
        "[::1]".to_string(),
        "[::1]:8090".to_string(),
        "192.168.1.5:8090".to_string(),
    ] {
        assert_eq!(
            get_from(&client, &served.url("/api/v1/torrents"), &own, None)
                .await
                .status(),
            StatusCode::OK,
            "{own} is this server's own name",
        );
    }

    // An unbracketed IPv6 address is not a host a browser sends, and a port that is not digits means the
    // value is not a host either: both are refused rather than guessed at.
    for unusable in ["::1", "[::1]:80x", "evil.example:notaport"] {
        let (status, _) =
            error_of(get_from(&client, &served.url("/api/v1/torrents"), unusable, None).await)
                .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{unusable} is not a host");
    }

    served.stop().await;
}

/// The list for someone who runs this behind a name of their own and turned logins off on purpose: the name
/// answers in any case with any port, and only that name - a suffix trick is somebody else's domain, and a
/// prefix is somebody else's host. With logins on the list would be pointless, because every request would
/// need a token instead.
#[tokio::test]
async fn auth_off_allows_listed_host() {
    let served = Served::start("host-listed", |scratch| {
        let mut config = base_config(scratch, vec![alice()], Vec::new(), Some(false));
        config
            .server
            .as_mut()
            .expect("a config that serves")
            .allowed_hosts = vec!["bit.local".to_string()];
        config
    })
    .await;
    let client = http();

    for own in ["bit.local", "BIT.LOCAL:8090"] {
        assert_eq!(
            get_from(&client, &served.url("/api/v1/torrents"), own, None)
                .await
                .status(),
            StatusCode::OK,
            "{own} is what the config named",
        );
    }

    // A name that only looks like the one the config named: a domain that ends with it, a host that starts
    // with it, and the same name with a trailing dot - which DNS treats as the same host and this server
    // does not, the safe direction for something whose whole job is refusing names.
    for elsewhere in [
        "bit.local.evil.example",
        "mybit.local",
        "evil.example",
        "bit.local.:8090",
    ] {
        let (status, code) =
            error_of(get_from(&client, &served.url("/api/v1/torrents"), elsewhere, None).await)
                .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{elsewhere} is not listed");
        assert_eq!(code, "forbidden");
    }

    // An origin spells the same name and gets the same answer, and a login attempt still arrives: what this
    // list decides is which names are served, not whether anyone has to sign in.
    assert_eq!(
        get_from(
            &client,
            &served.url("/api/v1/torrents"),
            "bit.local:8090",
            Some("http://BIT.LOCAL:8090"),
        )
        .await
        .status(),
        StatusCode::OK,
    );

    served.stop().await;
}

/// A device is a way of proving who you are, and a server that asks nobody to prove anything has nothing for
/// one to be proof against - so with logins off the three device routes answer `403` and say what to do
/// instead. That closes the worst thing an unauthenticated server could do to itself: let a request it cannot
/// authenticate add a key that every later run would log in, and keep it in `config.yaml` after the page
/// that added it was closed. Everything else about an open server stays open.
#[tokio::test]
async fn auth_off_refuses_device_routes() {
    let served = Served::start("auth-off-devices", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(false))
    })
    .await;
    let client = http();

    let (_, _, message) =
        problem_of(get(&client, &served.url("/api/v1/auth/devices"), None).await).await;
    assert!(
        message.contains("devices need authentication on"),
        "the answer says why: {message}"
    );

    let (status, code, message) = problem_of(
        post_json(
            &client,
            &served.url("/api/v1/auth/devices"),
            serde_json::json!({ "name": "phone", "public_key": public_line(&keypair()) }),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "adding one is refused too");
    assert_eq!(code, "forbidden");
    assert!(
        message.contains("devices need authentication on"),
        "{message}"
    );

    let (status, _) =
        error_of(delete(&client, &served.url("/api/v1/auth/devices/phone"), None).await).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "and so is removing one");

    // A password login still works here, and its token still opens everything else - but not the device
    // list: what that route describes is what a key login checks against, and with logins off there is no
    // such thing to describe, token or not.
    let (token, _) = session_of(
        post_basic(
            &client,
            &served.url("/api/v1/auth/session"),
            "alice",
            "hunter2",
        )
        .await,
    )
    .await;
    assert_eq!(
        get(&client, &served.url("/api/v1/fs/roots"), Some(&token))
            .await
            .status(),
        StatusCode::OK,
        "nothing else about this server got stricter on the way",
    );
    let (status, code) =
        error_of(get(&client, &served.url("/api/v1/auth/devices"), Some(&token)).await).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a token from a password does not make there be devices",
    );
    assert_eq!(code, "forbidden");

    served.stop().await;
}

/// What the login limit is really for: not a person typing a wrong password ten times, but thirty guesses
/// from one address arriving together, which is what something with a list of passwords does. Counting an
/// attempt when it arrives - before a hash is chosen or a verify starts - means at most one window's worth
/// of them is ever worked on; a limit that asked permission first and counted afterwards would have said yes
/// to all thirty and then run thirty Argon2 checks.
#[tokio::test]
async fn parallel_password_guesses_are_capped() {
    let served = Served::start("guess-burst", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let url = served.url("/api/v1/auth/session");
    const GUESSERS: usize = 30;

    let mut guesses = Vec::new();
    for _ in 0..GUESSERS {
        let client = http();
        let url = url.clone();
        guesses.push(tokio::spawn(async move {
            within(
                client
                    .post(url.as_str())
                    .basic_auth("alice", Some("wrong"))
                    .send(),
            )
            .await
            .expect("the server answered")
            .status()
        }));
    }

    let mut checked = 0;
    let mut refused = 0;
    let mut busy = 0;
    for guess in guesses {
        match within(guess).await.expect("the guess was answered") {
            StatusCode::UNAUTHORIZED => checked += 1,
            StatusCode::TOO_MANY_REQUESTS => refused += 1,
            StatusCode::SERVICE_UNAVAILABLE => busy += 1,
            other => panic!("a password guess got {other}"),
        }
    }

    assert_eq!(
        checked + refused + busy,
        GUESSERS,
        "all thirty were answered"
    );
    assert!(
        checked <= LOGIN_BUDGET,
        "{checked} guesses were worked on, and one address has {LOGIN_BUDGET} attempts in a window",
    );
    assert!(
        refused >= GUESSERS - LOGIN_BUDGET,
        "the rest were refused before any of that work started: {refused}",
    );

    served.stop().await;
}

/// How many Argon2 checks one server may run at once, restated here rather than read back from the server:
/// one per core as the OS reports them, never more than four and never fewer than one. A test that trusts
/// the number the server itself would give could not tell a cap from a wish.
fn verify_permit_cap() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|cores| cores.get())
        .unwrap_or(1);
    cores.clamp(1, 4)
}

/// Releases the checks a test parked inside the Argon2 verify when it goes out of scope - including on the
/// way out of a failed assertion, which is what makes the difference between a failure that says what was
/// measured and one that hangs: tokio waits for every blocking job when a runtime is dropped, and a parked
/// check only stops waiting when somebody opens the hold. Releasing twice costs nothing.
struct ReleaseParked<'a>(&'a Server);

impl Drop for ReleaseParked<'_> {
    fn drop(&mut self) {
        self.0.release_parked_verifies();
    }
}

/// How long the burst test waits for a check that must not start. Nothing has to travel further than this
/// process: the attempts that are waiting are already queued at the permit, and all that would have to happen
/// is a permit coming back and a blocking thread picking the next job up - so what this settles is whether a
/// permit was returned at all, not how fast argon2 runs on the machine next door.
const SETTLE: Duration = Duration::from_secs(3);

/// Waits until `want` Argon2 checks are sitting inside the check (see `Server::park_verifies`) and returns
/// how many are. The count moves when a check arrives, so this waits on that change instead of polling a
/// counter - and it leaves the count marked as read, which is what lets the caller then wait for the next one.
async fn wait_until_parked(parked: &mut tokio::sync::watch::Receiver<u64>, want: u64) -> u64 {
    while *parked.borrow_and_update() < want {
        within(parked.changed()).await.expect("a check parked");
    }
    *parked.borrow_and_update()
}

/// The bound on the slow part of logging in, measured rather than inferred: an Argon2 check wants about 19
/// MiB and a core's worth of time, and the blocking pool runs as many as arrive if nothing tells it not to.
/// Every permit is taken by a check that is holding it - not by the request that asked for it, which is the
/// part that matters, because whoever connected is free to hang up halfway through. So: park exactly a cap's
/// worth of checks inside their checks, add more attempts than there are permits, hang up on the ones that
/// are running, and see whether anything else gets a turn while they are still hashing. A permit that went
/// back with its handler would come free here, and one of the queued attempts would take it - which is what
/// this test fails on if the permit is not held by the work itself.
#[tokio::test]
async fn verify_concurrency_is_bounded() {
    let served = Served::start("verify-burst", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let cap = verify_permit_cap();
    // More attempts than there are permits, and still inside the ten one address gets in a window - so what
    // they are waiting for is this bound, not their own budget.
    const EXTRA: usize = 3;
    assert!(
        cap + EXTRA <= LOGIN_BUDGET,
        "the attempts this test sends have to be inside one address's budget",
    );

    // Armed before the first request: from here every check stops inside the check with its permit in hand,
    // until this test lets it go. That is what makes the measurement not a race against hashing speed - and
    // the guard is what makes a failure report itself instead of hanging, because tokio waits for a parked
    // check when the runtime this test runs on is dropped.
    let mut parked = served.server.park_verifies();
    let release = ReleaseParked(&served.server);
    let url = served.url("/api/v1/auth/session");

    let mut running = Vec::new();
    for _ in 0..cap {
        let client = http();
        let url = url.clone();
        running.push(tokio::spawn(async move {
            within(
                client
                    .post(url.as_str())
                    .basic_auth("alice", Some("wrong"))
                    .send(),
            )
            .await
            .expect("the server answered")
            .status()
        }));
    }
    let want = u64::try_from(cap).expect("a count of permits");
    assert_eq!(
        wait_until_parked(&mut parked, want).await,
        want,
        "every permit this server has is held by a check that has not finished",
    );
    assert_eq!(
        usize::try_from(served.server.max_concurrent_verifies()).expect("a count of checks"),
        cap,
        "the checks that are running are the whole bound, not more than it",
    );

    let mut queued = Vec::new();
    for _ in 0..EXTRA {
        let client = http();
        let url = url.clone();
        queued.push(tokio::spawn(async move {
            within(
                client
                    .post(url.as_str())
                    .basic_auth("alice", Some("wrong"))
                    .send(),
            )
            .await
            .expect("the server answered")
            .status()
        }));
    }

    // Hang up on the attempts that are being worked on: cancelling a task stops it where it waits, which is
    // the client closing the connection mid-check, and hyper drops the handler with it (the red run of this
    // test showed the queued attempts getting their turn the moment that happened). The hashing does not
    // stop - a blocking job cannot be cancelled - so all that can change here is who is holding the permit.
    for attempt in running {
        attempt.abort();
    }
    // Measured first and asserted after the hold is opened, because a check that stayed parked would keep
    // tokio waiting for it when this test's runtime is dropped: a failure has to report what it measured
    // rather than hang.
    let started_after_hangup = match tokio::time::timeout(SETTLE, parked.changed()).await {
        // Nothing arrived: the checks that are still hashing hold every permit there is, and a queued
        // attempt cannot start until one of them finishes.
        Err(_silent) => 0,
        Ok(Ok(())) => usize::try_from(*parked.borrow_and_update()).expect("a count of checks"),
        Ok(Err(disconnected)) => panic!("the parked-check count stopped ({disconnected})"),
    };
    let most_while_held = served.server.max_concurrent_verifies();
    // Let them finish, for good: the queued attempts take the turns as they come back, so what hung up did
    // not cost anybody their answer.
    drop(release);

    assert_eq!(
        started_after_hangup, 0,
        "Argon2 checks started after every client that asked for them had hung up: a check handed its turn \
         back with the request while it was still hashing",
    );
    assert_eq!(
        usize::try_from(most_while_held).expect("a count of checks"),
        cap,
        "nothing started while every permit was held by a check that had already taken it",
    );

    for attempt in queued {
        let status = within(attempt)
            .await
            .expect("the queued attempt was answered");
        assert!(
            status == StatusCode::UNAUTHORIZED || status == StatusCode::SERVICE_UNAVAILABLE,
            "an attempt inside the budget is checked, or told to come back once: {status}",
        );
    }
    assert_eq!(
        usize::try_from(served.server.max_concurrent_verifies()).expect("a count of checks"),
        cap,
        "at no point did this server run more checks together than the machine is allowed",
    );

    served.stop().await;
}

/// A `config.yaml` this server cannot read is somebody's hand-edited file, and a request that cannot parse it
/// has no business rearranging it: adding a device answers that it cannot be read and says to fix it by hand,
/// writes nothing, and leaves the bytes under the name they had. It used to move them aside as
/// `config.yaml.bad` and write a config of its own in their place - from inside a `POST`, on the strength of
/// YAML it could not understand.
#[tokio::test]
async fn an_unreadable_config_is_left_for_its_owner() {
    let served = Served::start("bad-config", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let client = http();
    let token = log_in_with_password(&served, "alice", "hunter2").await;
    let file = served.paths.config_file();
    const BROKEN: &str = "server:\n  allowed_hosts: [bit.local\n";

    // Nothing was there when this server started - the tests hand their config over in memory, and serving a
    // library does not write a config file - so this is the state a person leaves behind by editing the file
    // and getting the YAML wrong before fixing it: bytes on disk that disagree with what the run is using.
    fs::create_dir_all(file.parent().expect("the config directory"))
        .expect("make the config directory");
    fs::write(&file, BROKEN).expect("write an unreadable config");

    let (status, code, message) = problem_of(
        post_json(
            &client,
            &served.url("/api/v1/auth/devices"),
            serde_json::json!({ "name": "phone", "public_key": public_line(&keypair()) }),
            Some(&token),
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a file this server cannot read is not its to repair",
    );
    assert_eq!(code, "internal");
    assert!(
        message.starts_with("config.yaml cannot be read; fix it by hand"),
        "the answer says what to do: {message}"
    );

    // The bytes are where they were, under the name they had, with nothing beside them: no `.bad` left behind
    // by a request, and no half-config with the new device in it.
    assert_eq!(
        fs::read_to_string(&file).expect("the file is still where it was"),
        BROKEN,
    );
    let mut names: Vec<String> = fs::read_dir(file.parent().expect("the config directory"))
        .expect("read the config directory")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["config.yaml"],
        "nothing was renamed and nothing was written: {names:?}"
    );

    served.stop().await;
}

/// A token ends when its holder says so: `DELETE /api/v1/auth/session` revokes the one that authenticated
/// the call, and the same bearer afterwards gets what a stranger with a copied token would get. Asking to
/// log out with a token that is already dead is refused rather than granted - everywhere else a dead token
/// is refused too, and "you are not signed in" arriving as a 401 is how a client finds that out.
#[tokio::test]
async fn logout_revokes() {
    let served = Served::start("logout", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let client = http();
    let token = log_in_with_password(&served, "alice", "hunter2").await;
    let session = served.url("/api/v1/auth/session");

    assert_eq!(
        get(&client, &served.url("/api/v1/torrents"), Some(&token))
            .await
            .status(),
        StatusCode::OK,
        "the token works before it is revoked",
    );

    let response = delete(&client, &session, Some(&token)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let (status, code) =
        error_of(get(&client, &served.url("/api/v1/torrents"), Some(&token)).await).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the token is spent");
    assert_eq!(code, "unauthorized");

    let (status, _) = error_of(delete(&client, &session, Some(&token)).await).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a dead token does not get to log itself out a second time",
    );

    // A second login works, and revoking it leaves the first token's death unchanged: sessions are per
    // token, not per account.
    let another = log_in_with_password(&served, "alice", "hunter2").await;
    assert_eq!(
        delete(&client, &session, Some(&another)).await.status(),
        StatusCode::NO_CONTENT,
    );
    assert_eq!(
        get(&client, &served.url("/api/v1/torrents"), Some(&another))
            .await
            .status(),
        StatusCode::UNAUTHORIZED,
        "the new token is spent too",
    );

    served.stop().await;
}

// ---------------------------------------------------------------------------
// Stream tokens
// ---------------------------------------------------------------------------

/// Asking for an address to stream a file from is not free: it is the one route that hands out a credential,
/// so it needs the session every other non-login route needs. With nobody signed in it answers `401` before
/// it looks at which torrent was named - and once signed in, the route is there to be answered (an unknown
/// id gets `404`, not another `401`).
#[tokio::test]
async fn stream_token_route_needs_login() {
    let served = Served::start("stream-token-login", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let body = serde_json::json!({ "torrent": "ab".repeat(20), "file": 0 });

    let (status, code) = error_of(
        post_json(
            &http(),
            &served.url("/api/v1/stream-tokens"),
            body.clone(),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a stream address is a credential, so it is not free",
    );
    assert_eq!(code, "unauthorized");

    // Signed in, the route answers: this id is well-formed and simply not in the library.
    let token = log_in_with_password(&served, "alice", "hunter2").await;
    let (status, code) = error_of(
        post_json(
            &http(),
            &served.url("/api/v1/stream-tokens"),
            body,
            Some(&token),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the route is reached");
    assert_eq!(code, "not_found");

    served.stop().await;
}

/// One token opens one file. A token issued for file 0 of one torrent does not open file 1 of the same
/// torrent, nor file 0 of another one, and it is not a session token either: `GET /api/v1/events` - the
/// other route fbt-api takes a `?token=` on - refuses it, and so does an `Authorization: Bearer` header that
/// carries it. The path it was issued for is the scope, byte for byte: this is Review Focus item 1.
#[tokio::test]
async fn stream_token_is_scoped_to_one_file() {
    let served = Served::start("stream-scope", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let client = http();
    let session = log_in_with_password(&served, "alice", "hunter2").await;

    // A two-file torrent - a v1 torrent's file list is its files, so 0 and 1 are both real ones - and a
    // second torrent beside it.
    let folder = write_folder(
        served.root(),
        "set",
        &[("a.bin", &sample(100)), ("b.bin", &sample(200))],
    );
    let first = seed_torrent(&served, Some(&session), &folder).await;
    let second = seed_torrent(
        &served,
        Some(&session),
        &write_file(served.root(), "other.bin", &sample(150)),
    )
    .await;

    let path = stream_address(&served, Some(&session), &first, 0).await;
    assert!(
        path.starts_with(&format!("/api/v1/torrents/{first}/files/0/stream?token=")),
        "the address is the file it was issued for, with its token on it: {path}",
    );
    let token = path.rsplit("token=").next().expect("a token in the path");

    // The file it was issued for.
    assert_eq!(
        open_stream(&client, true, &served.url(&path), None)
            .await
            .status(),
        StatusCode::OK,
        "the token opens its own file",
    );

    // Another file of the same torrent, and another torrent's first file. The first is asked for with `HEAD`
    // like the one above - a refusal answers that with no body at all, so only the status is there to read -
    // and the second with `GET`, which carries this API's error body like every other refusal does.
    let head = open_stream(
        &client,
        true,
        &served.url(&format!(
            "/api/v1/torrents/{first}/files/1/stream?token={token}"
        )),
        None,
    )
    .await;
    assert_eq!(
        head.status(),
        StatusCode::UNAUTHORIZED,
        "the token does not open the other file of the same torrent",
    );
    let (status, code) = error_of(
        open_stream(
            &client,
            false,
            &served.url(&format!(
                "/api/v1/torrents/{second}/files/0/stream?token={token}"
            )),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "nor another torrent's file",
    );
    assert_eq!(code, "unauthorized");

    // The event stream takes a `?token=` too, and this one is not a session token: refused.
    let (status, code) = error_of(
        get(
            &client,
            &served.url(&format!("/api/v1/events?token={token}")),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a stream token is not a session token",
    );
    assert_eq!(code, "unauthorized");
    // The control: this server's session token does open the event stream, so the answer above is about
    // which credential arrived and not about the route refusing everything.
    assert_eq!(
        get(&client, &served.url("/api/v1/events"), Some(&session))
            .await
            .status(),
        StatusCode::OK,
        "a session token opens the event stream",
    );

    // And never as a bearer header: `/api/v1/torrents` does not take a query token at all, and a stream
    // token is not in the session store either.
    let (status, code) =
        error_of(get(&client, &served.url("/api/v1/torrents"), Some(token)).await).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a stream token is not a bearer credential",
    );
    assert_eq!(code, "unauthorized");

    served.stop().await;
}

/// The address the route hands out is fbt-api's own stream route on this server: one range comes back as
/// `206` with `Content-Range` and exactly the bytes the file holds. What the token opens is a real stream,
/// not a promise about one.
#[tokio::test]
async fn stream_token_streams_a_range() {
    let served = Served::start("stream-range", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let client = http();
    let session = log_in_with_password(&served, "alice", "hunter2").await;
    let bytes = sample(200);
    let id = seed_torrent(
        &served,
        Some(&session),
        &write_file(served.root(), "payload.bin", &bytes),
    )
    .await;

    let path = stream_address(&served, Some(&session), &id, 0).await;
    let response = open_stream(&client, false, &served.url(&path), Some("bytes=4-9")).await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{path}");
    assert_eq!(
        response.headers()["content-range"],
        "bytes 4-9/200",
        "the range the answer says it served",
    );
    let body = within(response.bytes()).await.expect("a body");
    assert_eq!(body.as_ref(), &bytes[4..10], "the six bytes asked for");

    served.stop().await;
}

/// A film outlives the terminal that started it: logging the session out leaves the stream address working,
/// because a stream token is its own credential and not a second name for the session. The session token
/// stops working at the same moment, which is what makes this an independence proof rather than a test of a
/// server that never revoked anything.
#[tokio::test]
async fn stream_token_survives_logout() {
    let served = Served::start("stream-logout", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let client = http();
    let session = log_in_with_password(&served, "alice", "hunter2").await;
    let bytes = sample(120);
    let id = seed_torrent(
        &served,
        Some(&session),
        &write_file(served.root(), "payload.bin", &bytes),
    )
    .await;

    let path = stream_address(&served, Some(&session), &id, 0).await;
    let response = delete(&client, &served.url("/api/v1/auth/session"), Some(&session)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "signed out");

    let (status, _) =
        error_of(get(&client, &served.url("/api/v1/torrents"), Some(&session)).await).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the session is spent");

    let response = open_stream(&client, false, &served.url(&path), Some("bytes=0-3")).await;
    assert_eq!(
        response.status(),
        StatusCode::PARTIAL_CONTENT,
        "the stream address is not the session's",
    );
    let body = within(response.bytes()).await.expect("a body");
    assert_eq!(body.as_ref(), &bytes[0..4], "the first four bytes");

    served.stop().await;
}

/// With nobody signing in there is no credential to hand out, so the address comes back with no `?token=` on
/// it and the stream route answers from loopback on its own. This is the shape the local endpoint uses too,
/// and a client that has no token of its own still gets a playable URL.
#[tokio::test]
async fn stream_token_route_auth_off() {
    let served = Served::start("stream-auth-off", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(false))
    })
    .await;
    let client = http();
    let bytes = sample(120);
    let id = seed_torrent(
        &served,
        None,
        &write_file(served.root(), "payload.bin", &bytes),
    )
    .await;

    let path = stream_address(&served, None, &id, 0).await;
    assert_eq!(
        path,
        format!("/api/v1/torrents/{id}/files/0/stream"),
        "with logins off nothing is issued, so the answer carries no token",
    );

    let response = open_stream(&client, false, &served.url(&path), Some("bytes=10-13")).await;
    assert_eq!(
        response.status(),
        StatusCode::PARTIAL_CONTENT,
        "the address streams from loopback with no credential at all",
    );
    let body = within(response.bytes()).await.expect("a body");
    assert_eq!(body.as_ref(), &bytes[10..14], "the four bytes asked for");

    served.stop().await;
}

/// What a request can get wrong: an id that is not this library's (`404`), an index past the end of the file
/// list (`404`, the same answer fbt-api's own stream route gives for one), an id that is not hex at all
/// (`400 invalid_id`, the same code fbt-api uses), and a body far bigger than two short fields (`413`).
#[tokio::test]
async fn stream_token_bad_requests() {
    let served = Served::start("stream-bad-requests", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let session = log_in_with_password(&served, "alice", "hunter2").await;
    let id = seed_torrent(
        &served,
        Some(&session),
        &write_file(served.root(), "payload.bin", &sample(64)),
    )
    .await;

    // A well-formed hash of a torrent this library does not have.
    let (status, code) = error_of(
        post_json(
            &http(),
            &served.url("/api/v1/stream-tokens"),
            serde_json::json!({ "torrent": "0".repeat(40), "file": 0 }),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not this library's torrent");
    assert_eq!(code, "not_found");

    // An index past the end of the file list: `usize::MAX` is the widest one a client can name.
    let (status, code) = error_of(
        post_json(
            &http(),
            &served.url("/api/v1/stream-tokens"),
            serde_json::json!({ "torrent": id, "file": usize::MAX }),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no file with that index");
    assert_eq!(code, "not_found");

    // Not hex, so not an id: fbt-api's own code for that.
    let (status, code) = error_of(
        post_json(
            &http(),
            &served.url("/api/v1/stream-tokens"),
            serde_json::json!({ "torrent": "not-a-hash", "file": 0 }),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{status} {code}");
    assert_eq!(code, "invalid_id");

    // A body far bigger than the two fields this route takes: refused for its size, in this API's error
    // shape. Its own client, because a server that stops reading a body it refused has closed the
    // connection that carried it (see `login_bodies_are_capped`).
    let junk = "x".repeat(1024 * 1024);
    let (status, code) = error_of(
        post_json(
            &http(),
            &served.url("/api/v1/stream-tokens"),
            serde_json::json!({ "torrent": id, "file": 0, "junk": junk }),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a body this route has no use for"
    );
    assert_eq!(code, "body_too_large");

    // And the route still answers a normal request afterwards: this was a cap, not a broken route.
    let path = stream_address(&served, Some(&session), &id, 0).await;
    assert!(path.contains("/files/0/stream?token="), "{path}");

    served.stop().await;
}

/// A torrent whose metadata has not arrived has no file list to name a file from: the answer is `409` with
/// fbt-api's own code for "no metadata yet", which is what the stream route itself answers too. Nothing here
/// can bring its metadata - DHT, LSD and peers are all absent - so it stays true for the length of the test.
#[tokio::test]
async fn a_torrent_without_metadata_has_no_stream_address() {
    let served = Served::start("stream-no-metadata", |scratch| {
        base_config(scratch, vec![alice()], Vec::new(), Some(true))
    })
    .await;
    let session = log_in_with_password(&served, "alice", "hunter2").await;
    let id = "cd".repeat(20);

    let response = post_json(
        &http(),
        &served.url("/api/v1/torrents"),
        serde_json::json!({
            "magnet": format!("magnet:?xt=urn:btih:{id}"),
            "save_path": served.root().to_string_lossy(),
        }),
        Some(&session),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "a magnet is added before its metadata is known"
    );

    let (status, code) = error_of(
        post_json(
            &http(),
            &served.url("/api/v1/stream-tokens"),
            serde_json::json!({ "torrent": id, "file": 0 }),
            Some(&session),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "no file list to name yet");
    assert_eq!(code, "no_metadata");

    served.stop().await;
}
