//! The desktop access mode: an engine the desktop app starts on loopback and talks to with a secret it made
//! for this launch.
//!
//! What is checked here is the boundary as a client sees it over http: where the server listens, which
//! credentials open it, which routes are not there at all, and which origins its CORS answers. The engine
//! settings are loopback-only, as in `tests/server.rs`, so nothing here leaves the machine.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use fbtt_core::scratch::ScratchDir;
use fbtt_core::{AuthConfig, Config, LibraryLock, Paths, ServerConfig, UserEntry};
use fbtt_server::auth::password;
use fbtt_server::{DesktopAccess, MIN_LAUNCH_SECRET_CHARS, Server, ServerError};
use reqwest::{Response, StatusCode};
use serde_json::Value;

/// How long a test waits for anything from the server.
const DEADLINE: Duration = Duration::from_secs(60);
/// The secret every test here launches with. A test value, not anybody's credential.
/// As long as the shortest secret `start_desktop` accepts, 43 characters, like the app's own.
const LAUNCH_SECRET: &str = "launch-secret-for-this-test-only-0123456789";
/// The origin the desktop app's webview sends on macOS and Linux.
const DESKTOP_ORIGIN: &str = "tauri://localhost";
/// The account the exposed test signs in as, and its password.
const USER: &str = "alice";
const SECRET: &str = "hunter2";

/// Runs `what` under the deadline, so a server that never answers is a failure with a message rather than a
/// hung test job.
async fn within<T>(what: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
}

/// A desktop server with a library of its own, on a scratch directory kept alive beside it.
struct Served {
    dir: ScratchDir,
    server: Server,
}

impl Served {
    /// A desktop server on a library in a scratch directory named after `tag`, configured by `base` and
    /// started with `expose`.
    async fn start(tag: &str, expose: bool, base: impl FnOnce(&Path) -> Config) -> Served {
        let dir = ScratchDir::new(tag);
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        let config = base(dir.path()).resolve(&paths);
        let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
        let access = DesktopAccess {
            launch_secret: LAUNCH_SECRET.to_string(),
            origins: vec![DESKTOP_ORIGIN.to_string()],
            expose,
        };
        let server = within(Server::start_desktop(
            loopback(&config),
            config,
            &paths,
            lock,
            access,
        ))
        .await
        .expect("start the server");
        Served { dir, server }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.server.local_addr())
    }

    async fn stop(self) {
        let Served { dir, server } = self;
        within(server.shutdown()).await.expect("the last save ran");
        drop(dir);
    }
}

/// The settings every engine here runs on: loopback on a port the OS picks, no DHT, LSD, UPnP or NAT-PMP.
fn loopback(config: &Config) -> fbt::Settings {
    let mut settings = config.session_settings();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    settings
}

/// A config whose server section asks for `bind`, with `users` as its accounts and logins as `auth` says.
fn config_on(
    scratch: &Path,
    bind: SocketAddr,
    auth: Option<bool>,
    users: Vec<UserEntry>,
) -> Config {
    Config {
        download_dir: scratch.join("downloads"),
        server: Some(ServerConfig {
            bind,
            auth: AuthConfig {
                enabled: auth,
                users,
                devices: Vec::new(),
            },
            ..ServerConfig::default()
        }),
        ..Config::default()
    }
}

/// A config with no accounts that asks for the default loopback server, logins left to the default.
fn plain(scratch: &Path) -> Config {
    config_on(
        scratch,
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        None,
        Vec::new(),
    )
}

fn alice() -> UserEntry {
    UserEntry {
        name: USER.to_string(),
        password_hash: password::hash(SECRET),
    }
}

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

/// A port nothing is listening on right now, for the test that needs the config's own bind to be used.
fn free_port() -> u16 {
    let listener =
        std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind a probe listener");
    listener.local_addr().expect("the probe's address").port()
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_mode_binds_loopback_only() {
    let served = Served::start("desktop-bind", false, |scratch| {
        config_on(
            scratch,
            "0.0.0.0:9999".parse().expect("an any-address bind"),
            None,
            Vec::new(),
        )
    })
    .await;
    let address = served.server.local_addr();
    assert_eq!(address.ip(), Ipv4Addr::LOCALHOST, "bound to {address}");
    assert_ne!(address.port(), 9999, "the config's port was used");
    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_mode_refuses_requests_without_the_launch_secret() {
    let served = Served::start("desktop-refuses", false, plain).await;
    let client = http();
    for path in [
        "/api/v1/torrents",
        "/api/v1/events",
        "/api/v1/media/x/0/tracks/0/stream",
    ] {
        let response = get(&client, &served.url(path), None).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} without a credential"
        );
        let response = get(&client, &served.url(path), Some("not-the-secret")).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} with a wrong bearer"
        );
    }
    // A wrong query token is no better than a wrong header.
    let response = get(&client, &served.url("/api/v1/events?token=nope"), None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // The Host guard of a server without logins is not a stand-in for the secret: a request that names
    // this server by its address, the way the guard would accept, is still refused.
    let response = within(
        client
            .get(served.url("/api/v1/torrents"))
            .header(reqwest::header::HOST, "localhost")
            .send(),
    )
    .await
    .expect("the server answered");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // No web UI is served, with or without the secret.
    for path in ["/", "/assets/x.js"] {
        for token in [None, Some(LAUNCH_SECRET)] {
            let response = get(&client, &served.url(path), token).await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{path} (credential: {})",
                token.is_some()
            );
        }
    }
    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_mode_accepts_the_secret_as_bearer_and_query_token() {
    let served = Served::start("desktop-accepts", false, plain).await;
    let client = http();
    let response = get(
        &client,
        &served.url("/api/v1/torrents"),
        Some(LAUNCH_SECRET),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = get(
        &client,
        &served.url(&format!("/api/v1/events?token={LAUNCH_SECRET}")),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    // The query token is fbt-api's own, offered on its streams alone: elsewhere it is no credential.
    let response = get(
        &client,
        &served.url(&format!("/api/v1/torrents?token={LAUNCH_SECRET}")),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // A request from the desktop webview carries its origin; the secret is what decides, not the origin.
    let response = within(
        client
            .get(served.url("/api/v1/torrents"))
            .bearer_auth(LAUNCH_SECRET)
            .header(reqwest::header::ORIGIN, DESKTOP_ORIGIN)
            .send(),
    )
    .await
    .expect("the server answered");
    assert_eq!(response.status(), StatusCode::OK);
    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_mode_has_no_login_routes() {
    // Even a config with an account and logins on: the launch secret is the only credential here.
    let served = Served::start("desktop-no-logins", false, |scratch| {
        config_on(
            scratch,
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            Some(true),
            vec![alice()],
        )
    })
    .await;
    let client = http();
    for path in [
        "/api/v1/auth/session",
        "/api/v1/auth/challenge",
        "/api/v1/auth/web-session",
    ] {
        let response = within(
            client
                .post(served.url(path))
                .basic_auth(USER, Some(SECRET))
                .send(),
        )
        .await
        .expect("the server answered");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "POST {path}");
        let response = within(
            client
                .post(served.url(path))
                .bearer_auth(LAUNCH_SECRET)
                .send(),
        )
        .await
        .expect("the server answered");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "POST {path} with the secret"
        );
    }
    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_mode_answers_the_desktop_origin_preflight() {
    let served = Served::start("desktop-preflight", false, plain).await;
    let client = http();
    let preflight = |origin: &'static str| {
        client
            .request(reqwest::Method::OPTIONS, served.url("/api/v1/torrents"))
            .header(reqwest::header::ORIGIN, origin)
            .header(reqwest::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .send()
    };
    let response = within(preflight(DESKTOP_ORIGIN))
        .await
        .expect("the server answered");
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok()),
        Some(DESKTOP_ORIGIN),
    );
    let response = within(preflight("http://evil.example"))
        .await
        .expect("the server answered");
    assert!(
        response
            .headers()
            .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "a stranger's origin was allowed",
    );
    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exposed_desktop_mode_requires_logins_and_keeps_the_secret() {
    let port = free_port();
    let served = Served::start("desktop-exposed", true, |scratch| {
        config_on(
            scratch,
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            Some(true),
            vec![alice()],
        )
    })
    .await;
    assert_eq!(
        served.server.local_addr(),
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        "the config's bind",
    );
    let client = http();
    // Logins are on: no credential, no library.
    let response = get(&client, &served.url("/api/v1/torrents"), None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // A password login works and its session opens the API.
    let response = within(
        client
            .post(served.url("/api/v1/auth/session"))
            .basic_auth(USER, Some(SECRET))
            .send(),
    )
    .await
    .expect("the server answered");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = within(response.json()).await.expect("a JSON body");
    let token = body["token"].as_str().expect("a session token").to_string();
    let response = get(&client, &served.url("/api/v1/torrents"), Some(&token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    // The secret still works beside it.
    let response = get(
        &client,
        &served.url("/api/v1/torrents"),
        Some(LAUNCH_SECRET),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    served.stop().await;
}

/// Starts a secret-only desktop engine with `secret` and returns what `start_desktop` answered, shutting
/// down a server that did start.
async fn start_with_secret(tag: &str, secret: String) -> Result<(), ServerError> {
    let dir = ScratchDir::new(tag);
    let paths = Paths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
    };
    let config = plain(dir.path()).resolve(&paths);
    let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
    let access = DesktopAccess {
        launch_secret: secret,
        origins: vec![DESKTOP_ORIGIN.to_string()],
        expose: false,
    };
    let started = within(Server::start_desktop(
        loopback(&config),
        config,
        &paths,
        lock,
        access,
    ))
    .await;
    let result = match started {
        Ok(server) => {
            within(server.shutdown()).await.expect("the last save ran");
            Ok(())
        }
        Err(problem) => Err(problem),
    };
    drop(dir);
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_mode_refuses_an_empty_launch_secret() {
    let refused = start_with_secret("desktop-empty-secret", String::new()).await;
    assert!(
        matches!(refused, Err(ServerError::LaunchSecret)),
        "an empty secret was served: {refused:?}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_mode_refuses_a_launch_secret_one_character_short() {
    assert_eq!(MIN_LAUNCH_SECRET_CHARS, 43);
    let short = "a".repeat(MIN_LAUNCH_SECRET_CHARS - 1);
    let refused = start_with_secret("desktop-short-secret", short).await;
    assert!(
        matches!(refused, Err(ServerError::LaunchSecret)),
        "a 42-character secret was served: {refused:?}",
    );
    // And exactly the minimum is enough.
    let enough = "a".repeat(MIN_LAUNCH_SECRET_CHARS);
    start_with_secret("desktop-enough-secret", enough)
        .await
        .expect("a 43-character secret starts the engine");
}
