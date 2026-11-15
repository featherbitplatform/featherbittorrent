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
use std::path::Path;
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
        server: ServerConfig {
            bind: "127.0.0.1:0".parse().expect("a loopback bind address"),
            auth: AuthConfig {
                enabled: auth,
                users,
                devices,
            },
            ..ServerConfig::default()
        },
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
/// server actually gave, and there is no per-request timeout: [`within`] is what bounds a test.
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
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

    // A link inside the root that points out of it: spelled like it belongs, and refused once it is
    // resolved. This is the check a lexical rule cannot make on its own, so it happens after canonicalising
    // - and it is why the listing of a linked folder would need the same rule another day.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../outside", root.join("link-out")).expect("make a symlink");
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
/// still answers correctly that you are logged out.
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

    let devices = get(&client, &served.url("/api/v1/auth/devices"), None).await;
    assert_eq!(devices.status(), StatusCode::OK);

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
