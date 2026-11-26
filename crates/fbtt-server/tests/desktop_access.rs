//! The desktop access mode: an engine the desktop app starts on loopback and talks to with a secret it made
//! for this launch.
//!
//! What is checked here is the boundary as a client sees it over http: where the server listens, which
//! credentials open it, which routes are not there at all, and which origins its CORS answers. The engine
//! settings are loopback-only, as in `tests/server.rs`, so nothing here leaves the machine.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fbtt_core::scratch::ScratchDir;
use fbtt_core::{
    AuthConfig, Config, LibraryLock, Paths, ProxySetting, ProxyType, ServerConfig, UserEntry,
};
use fbtt_server::auth::password;
use fbtt_server::{
    DesktopAccess, MIN_LAUNCH_SECRET_CHARS, ProxyPasswordStore, Server, ServerError,
};
use reqwest::{Response, StatusCode};
use serde_json::{Value, json};

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
            proxy_password: None,
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

/// A path that matched no route on the app's own engine is not asked for the launch secret, and it
/// answers the plain `404` with nothing in it. That is safe only because nothing stands behind such a
/// path: no router this engine serves registers a fallback, which is what `routes::router` and `web.rs`
/// say out loud. Should one ever appear, this test is what notices - a fallback would answer something
/// real, or ask for a credential, where an unmatched path is supposed to say `404` without either.
#[tokio::test(flavor = "multi_thread")]
async fn secret_only_unknown_path_is_an_empty_404() {
    let served = Served::start("desktop-empty-404", false, plain).await;
    let client = http();
    for path in ["/", "/api/v1/not-a-route", "/no-such-page.html"] {
        let response = get(&client, &served.url(path), None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "GET {path}");
        let body = within(response.bytes()).await.expect("the body of a 404");
        assert!(
            body.is_empty(),
            "GET {path} answered {body:?} where an empty 404 was owed",
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
    // The config asks for loopback on a port the OS picks, and an exposed engine serves the config's
    // own address, so `0` here gets a port this server is bound to and nobody else's. A fixed port
    // probed and released before the start would be somebody else's to take in between, and that is a
    // flake, not a failure. What still decides whether the config's bind was served and not the
    // secret-only `127.0.0.1:0` is the behavior below, which only the config's expose path has: a
    // secret-only engine answers `404` to `POST /api/v1/auth/session` (see
    // `desktop_mode_has_no_login_routes`), and here the logins work on the very address that the
    // server reports as its own.
    let served = Served::start("desktop-exposed", true, |scratch| {
        config_on(
            scratch,
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            Some(true),
            vec![alice()],
        )
    })
    .await;
    let address = served.server.local_addr();
    assert_eq!(address.ip(), Ipv4Addr::LOCALHOST, "bound to {address}");
    let client = http();
    // Logins are on: no credential, no library.
    let response = get(&client, &served.url("/api/v1/torrents"), None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // A password login works on the bound address, and its session opens the API.
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
        proxy_password: None,
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

/// The proxy password the keychain tests store. A test value, not anybody's credential.
const PROXY_PASSWORD: &str = "proxy-pass-hunter2";

/// A keychain in memory: the desktop app's store as the server sees it, available or not, and refusing
/// every write when a test says so. Its `Debug` is never asked for.
#[derive(Default)]
struct MemoryStore {
    password: Mutex<Option<String>>,
    unavailable: bool,
    refuse_writes: AtomicBool,
    /// The config file each write is recorded against, once a test names it.
    watch: Mutex<Option<std::path::PathBuf>>,
    /// Every `set` and `clear`, with the config file's text at the moment it ran.
    calls: Mutex<Vec<(&'static str, String)>>,
    /// When set, how many more writes are taken before every write is refused.
    writes_left: Mutex<Option<usize>>,
}

impl MemoryStore {
    fn holding(password: Option<&str>) -> Arc<MemoryStore> {
        Arc::new(MemoryStore {
            password: Mutex::new(password.map(str::to_string)),
            ..MemoryStore::default()
        })
    }

    fn missing() -> Arc<MemoryStore> {
        Arc::new(MemoryStore {
            unavailable: true,
            ..MemoryStore::default()
        })
    }

    fn held(&self) -> Option<String> {
        self.password.lock().expect("the store's lock").clone()
    }

    /// Whether this write is refused: refused writes were asked for, or the writes left ran out.
    fn refuses(&self) -> bool {
        if self.refuse_writes.load(Ordering::SeqCst) {
            return true;
        }
        let mut left = self.writes_left.lock().expect("the store's lock");
        match left.as_mut() {
            Some(0) => true,
            Some(count) => {
                *count -= 1;
                false
            }
            None => false,
        }
    }

    /// Records `call` with the watched file's text as it is now.
    fn record(&self, call: &'static str) {
        let watched = self.watch.lock().expect("the store's lock").clone();
        if let Some(path) = watched {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            self.calls
                .lock()
                .expect("the store's lock")
                .push((call, text));
        }
    }
}

impl ProxyPasswordStore for MemoryStore {
    fn get(&self) -> Option<String> {
        if self.unavailable {
            return None;
        }
        self.held()
    }

    fn set(&self, password: &str) -> Result<(), String> {
        if self.unavailable || self.refuses() {
            return Err("The keychain did not save the password: refused by the test".to_string());
        }
        self.record("set");
        *self.password.lock().expect("the store's lock") = Some(password.to_string());
        Ok(())
    }

    fn clear(&self) -> Result<(), String> {
        if self.refuses() {
            return Err(
                "The keychain did not remove the password: refused by the test".to_string(),
            );
        }
        self.record("clear");
        *self.password.lock().expect("the store's lock") = None;
        Ok(())
    }

    fn available(&self) -> bool {
        !self.unavailable
    }
}

/// A port nothing listens on: the proxy every keychain test names, which the engine never reaches.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener.local_addr().expect("its address").port();
    drop(listener);
    port
}

/// A `kind` proxy at `127.0.0.1:port` logged in to as `bob`, with `password` (none for one the keychain
/// holds).
fn proxy_at(kind: ProxyType, port: u16, password: Option<&str>) -> ProxySetting {
    ProxySetting {
        kind,
        host: "127.0.0.1".to_string(),
        port,
        username: Some("bob".to_string()),
        password: password.map(str::to_string),
    }
}

/// A secret-only desktop engine whose `config.yaml` holds `proxy`, with `store` as its keychain (none for
/// a server with no keychain at all). The file is written before the start, as the app's own start finds
/// it, and the config the engine starts on is the file's.
async fn start_keychained(
    tag: &str,
    proxy: ProxySetting,
    store: Option<Arc<MemoryStore>>,
) -> (Served, Paths) {
    let dir = ScratchDir::new(tag);
    let paths = Paths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
    };
    let file = paths.config_file();
    std::fs::create_dir_all(file.parent().expect("the config directory"))
        .expect("make the config directory");
    Config {
        proxy: Some(proxy),
        ..plain(dir.path())
    }
    .save(&file)
    .expect("write the config file");
    let config = Config::load_for_edit_keychained(&file)
        .expect("the config reads")
        .expect("it is there")
        .resolve(&paths);
    let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
    let access = DesktopAccess {
        launch_secret: LAUNCH_SECRET.to_string(),
        origins: vec![DESKTOP_ORIGIN.to_string()],
        expose: false,
        proxy_password: store.map(|store| store as Arc<dyn ProxyPasswordStore>),
    };
    let server = within(Server::start_desktop(
        offline_before_proxy(&config),
        config,
        &paths,
        lock,
        access,
    ))
    .await
    .expect("start the server");
    (Served { dir, server }, paths)
}

/// [`loopback`] before the proxy, as the app hands its settings: a proxy removed at runtime goes back to
/// these.
fn offline_before_proxy(config: &Config) -> fbt::Settings {
    let mut settings = config.settings_before_proxy();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    settings
}

/// A `PUT /api/v1/settings/proxy` of `body` with the launch secret, answered as the status and the text.
async fn put_proxy(served: &Served, body: Value) -> (StatusCode, String) {
    let response = within(
        http()
            .put(served.url("/api/v1/settings/proxy"))
            .bearer_auth(LAUNCH_SECRET)
            .json(&body)
            .send(),
    )
    .await
    .expect("the server answered");
    let status = response.status();
    let text = within(response.text()).await.expect("a body");
    (status, text)
}

/// `GET /api/v1/settings` with the launch secret, as text.
async fn settings_text(served: &Served) -> String {
    let response = get(
        &http(),
        &served.url("/api/v1/settings"),
        Some(LAUNCH_SECRET),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    within(response.text()).await.expect("a body")
}

fn file_text(paths: &Paths) -> String {
    std::fs::read_to_string(paths.config_file()).expect("the config file")
}

/// On the desktop app's engine with a keychain, a new proxy password goes to the keychain and never into
/// `config.yaml`, and the view says it is saved there without carrying it.
#[tokio::test(flavor = "multi_thread")]
async fn desktop_proxy_password_goes_to_the_store_not_the_file() {
    let store = MemoryStore::holding(None);
    let port = closed_port();
    let (served, paths) = start_keychained(
        "desktop-proxy-keychain",
        ProxySetting {
            username: None,
            ..proxy_at(ProxyType::Socks5, port, None)
        },
        Some(store.clone()),
    )
    .await;

    let (status, text) = put_proxy(
        &served,
        json!({"type": "socks5", "host": "127.0.0.1", "port": port, "username": "bob",
               "password": {"set": PROXY_PASSWORD}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!text.contains(PROXY_PASSWORD), "{text}");
    assert_eq!(store.held().as_deref(), Some(PROXY_PASSWORD));
    let written = file_text(&paths);
    assert!(!written.contains(PROXY_PASSWORD), "{written}");
    assert!(!written.contains("password:"), "{written}");
    assert!(written.contains("proxy:"), "{written}");
    assert_eq!(
        served.server.network_settings().proxy.password.as_deref(),
        Some(PROXY_PASSWORD),
        "the engine logs in with it"
    );

    let view: Value = serde_json::from_str(&settings_text(&served).await).expect("JSON");
    assert_eq!(view["proxy"]["password_saved"], true, "{view}");
    assert_eq!(view["proxy"]["password_storage"], "keychain", "{view}");
    assert!(!view.to_string().contains(PROXY_PASSWORD));
    served.stop().await;
}

/// The engine starts on the keychain's password when the file holds none.
#[tokio::test(flavor = "multi_thread")]
async fn desktop_engine_starts_on_the_keychain_password() {
    let store = MemoryStore::holding(Some(PROXY_PASSWORD));
    let (served, _paths) = start_keychained(
        "desktop-proxy-start",
        proxy_at(ProxyType::Socks5, closed_port(), None),
        Some(store),
    )
    .await;
    let running = served.server.network_settings();
    assert_eq!(running.proxy.kind, fbt::ProxyKind::Socks5);
    assert_eq!(running.proxy.password.as_deref(), Some(PROXY_PASSWORD));
    served.stop().await;
}

/// A SOCKS5 login whose password the keychain no longer holds still starts the engine on the proxy, with
/// no login: never a direct connection, and never an app that cannot start.
#[tokio::test(flavor = "multi_thread")]
async fn desktop_engine_without_the_keychain_password_stays_on_the_proxy() {
    let (served, _paths) = start_keychained(
        "desktop-proxy-lost",
        proxy_at(ProxyType::Socks5, closed_port(), None),
        Some(MemoryStore::holding(None)),
    )
    .await;
    let running = served.server.network_settings();
    assert_eq!(running.proxy.kind, fbt::ProxyKind::Socks5);
    assert_eq!(running.proxy.username, None);
    assert!(!running.enable_incoming_tcp, "still nothing bypasses it");
    served.stop().await;
}

/// `keep` keeps the keychain's password, and B3's rule holds with the keychain as the store: keeping it
/// for a new host or port is refused, with nothing changed.
#[tokio::test(flavor = "multi_thread")]
async fn desktop_keep_uses_the_keychain_password_for_the_same_address_only() {
    let store = MemoryStore::holding(Some(PROXY_PASSWORD));
    let port = closed_port();
    let (served, paths) = start_keychained(
        "desktop-proxy-keep",
        proxy_at(ProxyType::Socks5, port, None),
        Some(store.clone()),
    )
    .await;

    let before = file_text(&paths);
    let (status, text) = put_proxy(
        &served,
        json!({"type": "socks5", "host": "127.0.0.1", "port": closed_port(), "username": "bob",
               "password": "keep"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(
        text.contains("Enter the password again when the proxy's host or port changes."),
        "{text}"
    );
    assert_eq!(file_text(&paths), before, "the file is unchanged");
    assert_eq!(store.held().as_deref(), Some(PROXY_PASSWORD));

    // The same address with another type keeps it: a SOCKS5 login needs both halves, so this is accepted
    // only because the keychain's password counts as the stored one.
    let (status, text) = put_proxy(
        &served,
        json!({"type": "http", "host": "127.0.0.1", "port": port, "username": "bob",
               "password": "keep"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(store.held().as_deref(), Some(PROXY_PASSWORD));
    let running = served.server.network_settings();
    assert_eq!(running.proxy.kind, fbt::ProxyKind::Http);
    assert_eq!(running.proxy.password.as_deref(), Some(PROXY_PASSWORD));
    assert!(!file_text(&paths).contains("password:"));
    served.stop().await;
}

/// A new address takes the old password out of the keychain before `config.yaml` names the new address,
/// and the new password goes in after: a crash between the writes can leave no password, never the old
/// one beside a new host.
#[tokio::test(flavor = "multi_thread")]
async fn desktop_new_address_clears_the_old_password_before_the_file_names_it() {
    let store = MemoryStore::holding(Some(PROXY_PASSWORD));
    let old_port = closed_port();
    let (served, paths) = start_keychained(
        "desktop-proxy-order",
        proxy_at(ProxyType::Socks5, old_port, None),
        Some(store.clone()),
    )
    .await;
    *store.watch.lock().expect("the store's lock") = Some(paths.config_file());
    let new_port = closed_port();

    let (status, text) = put_proxy(
        &served,
        json!({"type": "socks5", "host": "127.0.0.1", "port": new_port, "username": "bob",
               "password": {"set": "the-new-password"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let calls = store.calls.lock().expect("the store's lock").clone();
    let order: Vec<(&str, bool)> = calls
        .iter()
        .map(|(call, file)| (*call, file.contains(&format!("port: {new_port}"))))
        .collect();
    assert_eq!(
        order,
        vec![("clear", false), ("set", true)],
        "cleared while the file named the old address, set once it named the new one"
    );
    assert_eq!(store.held().as_deref(), Some("the-new-password"));
    served.stop().await;
}

/// A keychain that will not take the password is `500` with its reason, and nothing changes: not the
/// file, not the keychain, not the engine.
#[tokio::test(flavor = "multi_thread")]
async fn desktop_keychain_refusal_is_500_and_changes_nothing() {
    let store = MemoryStore::holding(Some(PROXY_PASSWORD));
    let port = closed_port();
    let (served, paths) = start_keychained(
        "desktop-proxy-refused",
        proxy_at(ProxyType::Socks5, port, None),
        Some(store.clone()),
    )
    .await;
    store.refuse_writes.store(true, Ordering::SeqCst);
    let before = file_text(&paths);
    let engine_before = served.server.network_settings();

    for body in [
        json!({"type": "socks5", "host": "127.0.0.1", "port": closed_port(), "username": "bob",
               "password": {"set": "another-password"}}),
        json!({"type": "none", "host": "", "port": 0, "username": null, "password": "keep"}),
    ] {
        let (status, text) = put_proxy(&served, body).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{text}");
        assert!(text.contains("proxy_password_not_saved"), "{text}");
        assert!(text.contains("refused by the test"), "{text}");
        assert!(!text.contains(PROXY_PASSWORD), "{text}");
        assert!(!text.contains("another-password"), "{text}");
        assert_eq!(file_text(&paths), before, "the file is as it was");
        assert_eq!(store.held().as_deref(), Some(PROXY_PASSWORD));
        assert_eq!(served.server.network_settings(), engine_before);
    }
    served.stop().await;
}

/// A switch the engine refuses after the save puts the keychain's password back too, with the same real
/// failure B3's rollback test uses: the engine starts on an HTTP proxy, which lets its BitTorrent port go,
/// the test takes that port, and removing the proxy cannot listen on it again.
#[tokio::test(flavor = "multi_thread")]
async fn desktop_failed_apply_puts_the_keychain_password_back() {
    let store = MemoryStore::holding(Some(PROXY_PASSWORD));
    let proxy_port = closed_port();
    let (served, paths) = start_keychained(
        "desktop-proxy-rollback",
        proxy_at(ProxyType::Http, proxy_port, None),
        Some(store.clone()),
    )
    .await;
    let before = file_text(&paths);
    let engine_port = served
        .server
        .engine_local_addrs()
        .first()
        .expect("the engine bound its port")
        .port();
    let squatter = std::net::TcpListener::bind(("127.0.0.1", engine_port))
        .expect("the engine let its port go under an HTTP proxy");

    let (status, text) = put_proxy(
        &served,
        json!({"type": "none", "host": "", "port": 0, "username": null, "password": "keep"}),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{text}");
    assert!(text.contains("proxy_not_applied"), "{text}");
    assert!(
        text.contains(
            "The engine could not switch to this proxy, so the previous setting is kept: "
        ),
        "{text}"
    );
    assert_eq!(
        store.held().as_deref(),
        Some(PROXY_PASSWORD),
        "the keychain has the password back"
    );
    assert_eq!(file_text(&paths), before, "the file names the proxy again");
    let running = served.server.network_settings();
    assert_eq!(running.proxy.kind, fbt::ProxyKind::Http);
    assert_eq!(running.proxy.password.as_deref(), Some(PROXY_PASSWORD));

    drop(squatter);
    served.stop().await;
}

/// A switch the engine refuses, where the file goes back but the keychain will not take the previous
/// password again: the answer says exactly that, not that config.yaml could not be put back.
#[tokio::test(flavor = "multi_thread")]
async fn desktop_failed_apply_says_when_only_the_keychain_could_not_go_back() {
    let store = MemoryStore::holding(Some(PROXY_PASSWORD));
    let proxy_port = closed_port();
    let (served, paths) = start_keychained(
        "desktop-proxy-keychain-not-back",
        proxy_at(ProxyType::Http, proxy_port, None),
        Some(store.clone()),
    )
    .await;
    let before = file_text(&paths);
    let engine_port = served
        .server
        .engine_local_addrs()
        .first()
        .expect("the engine bound its port")
        .port();
    let squatter = std::net::TcpListener::bind(("127.0.0.1", engine_port))
        .expect("the engine let its port go under an HTTP proxy");
    // The clear before the save is taken; putting the password back afterwards is refused.
    *store.writes_left.lock().expect("the store's lock") = Some(1);

    let (status, text) = put_proxy(
        &served,
        json!({"type": "none", "host": "", "port": 0, "username": null, "password": "keep"}),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{text}");
    assert!(text.contains("proxy_not_applied"), "{text}");
    assert!(
        text.contains(
            "config.yaml names it again, but the keychain could not get the previous password back"
        ),
        "{text}"
    );
    assert!(!text.contains("could not be put back"), "{text}");
    assert!(!text.contains(PROXY_PASSWORD), "{text}");
    assert_eq!(file_text(&paths), before, "the file went back");

    drop(squatter);
    served.stop().await;
}

/// On a system with no keychain the desktop app's engine keeps the password in `config.yaml`, as the CLI
/// does, and the view says the keychain is unavailable.
#[tokio::test(flavor = "multi_thread")]
async fn desktop_without_keychain_keeps_the_password_in_the_file() {
    let port = closed_port();
    let (served, paths) = start_keychained(
        "desktop-proxy-no-keychain",
        // No login yet: the file has to read as a config where the password lives in it.
        ProxySetting {
            username: None,
            ..proxy_at(ProxyType::Socks5, port, None)
        },
        Some(MemoryStore::missing()),
    )
    .await;
    let (status, text) = put_proxy(
        &served,
        json!({"type": "socks5", "host": "127.0.0.1", "port": port, "username": "bob",
               "password": {"set": PROXY_PASSWORD}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!text.contains(PROXY_PASSWORD), "{text}");
    let stored = Config::load_for_edit(&paths.config_file())
        .expect("reads")
        .expect("there")
        .proxy
        .expect("a proxy");
    assert_eq!(stored.password.as_deref(), Some(PROXY_PASSWORD));
    let view: Value = serde_json::from_str(&settings_text(&served).await).expect("JSON");
    assert_eq!(view["proxy"]["password_storage"], "unavailable", "{view}");
    assert_eq!(view["proxy"]["password_saved"], true, "{view}");
    served.stop().await;
}

/// A server with no keychain at all keeps the password in `config.yaml`, as B3 does.
#[tokio::test(flavor = "multi_thread")]
async fn cli_server_keeps_the_password_in_the_file() {
    let port = closed_port();
    let (served, paths) = start_keychained(
        "desktop-proxy-cli",
        // No login yet: the file has to read as a config where the password lives in it.
        ProxySetting {
            username: None,
            ..proxy_at(ProxyType::Socks5, port, None)
        },
        None,
    )
    .await;
    let (status, text) = put_proxy(
        &served,
        json!({"type": "socks5", "host": "127.0.0.1", "port": port, "username": "bob",
               "password": {"set": PROXY_PASSWORD}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(file_text(&paths).contains(PROXY_PASSWORD));
    let view: Value = serde_json::from_str(&settings_text(&served).await).expect("JSON");
    assert_eq!(view["proxy"]["password_storage"], "config_file", "{view}");
    served.stop().await;
}

/// `DesktopAccess`'s `Debug` says whether there is a keychain, never what it holds.
#[test]
fn desktop_access_debug_never_prints_the_proxy_password() {
    let access = DesktopAccess {
        launch_secret: LAUNCH_SECRET.to_string(),
        origins: Vec::new(),
        expose: false,
        proxy_password: Some(MemoryStore::holding(Some(PROXY_PASSWORD))),
    };
    let printed = format!("{access:?}");
    assert!(!printed.contains(PROXY_PASSWORD), "{printed}");
    assert!(!printed.contains(LAUNCH_SECRET), "{printed}");
}
