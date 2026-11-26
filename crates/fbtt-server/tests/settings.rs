//! The server's own settings over the API: the running limits and the downloads section, saved to the
//! server's `config.yaml` so they survive a restart.
//!
//! These are the parts that only show up over http: which credential may reach the routes, what the
//! answers say, and what lands in the file. What a change means for the engine and the downloads
//! automation is checked where those live (`fbtt-core`), and the shape of the view in `fbtt_core`'s own
//! tests; here the questions are who may write, what they get, and what the file holds afterwards.
//!
//! The engine settings are loopback-only, as in `tests/web_session.rs`: nothing here can leave the
//! machine, and every wait has a deadline so a server that stops answering is a failure with a message
//! instead of a hung test job.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fbtt_core::scratch::ScratchDir;
use fbtt_core::{
    AuthConfig, Config, DeviceEntry, DownloadsConfig, LibraryLock, Paths, ServerConfig, UserEntry,
};
use fbtt_server::auth::password;
use fbtt_server::{DesktopAccess, Server};
use reqwest::{Response, StatusCode};
use serde_json::{Value, json};

/// How long a test waits for anything from the server. Starting an engine takes a moment and an Argon2
/// verify is meant to be slow; nothing here should take longer than that many times over.
const DEADLINE: Duration = Duration::from_secs(60);
/// The account these tests sign in as, and the password it signs in with. Neither is a secret: this is a
/// test log, and the point of the pair is that the account is on the config's list.
const USER: &str = "alice";
const SECRET: &str = "hunter2";
/// The secret the desktop app launches an engine with, as long as the shortest `start_desktop` accepts.
/// A test value, not anybody's credential.
const LAUNCH_SECRET: &str = "launch-secret-for-this-test-only-0123456789";
/// The header a change carries its session's CSRF token in. Named here rather than imported: the server's
/// own copy of the name is a detail of its guard, and a test that spelled it differently would be testing
/// the test.
const CSRF: &str = "x-csrf-token";
/// A real Ed25519 public key line, for the one test that puts a device in the file by hand: what a
/// person pastes into `authorized_keys`. Nothing here verifies a signature; the key only has to be one.
const PUBLIC_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJ8DMsPmXeaez8OSQsPlY1gfyaNUfLtK9VVHPGZJ8ay8 settings test";
/// A second real Ed25519 public key line, so a test that races a device removal against a device add is
/// about two different keys rather than one key under two names. A key line, nothing more; nothing here
/// verifies a signature with it.
const OTHER_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDhj3vphewkV33lT9kbTx4mTkKc5zaWFUuQlm65H6IG/ settings race test";
/// The stand-in program these tests name as `downloads.on_complete`: this test binary's own path, which is
/// absolute, is really there and is not a batch file - the three things the route checks a program for.
/// Making the engine name it is about the *value* the route takes; nothing here starts it.
fn fake() -> PathBuf {
    std::env::current_exe().expect("this test binary's own path")
}

/// Runs `what` under the deadline, so a server that never answers is a failure with a message rather than
/// a hung test job.
async fn within<T>(what: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
}

/// A server with a library of its own, on a scratch directory kept alive beside it.
struct Served {
    /// The scratch library, removed when this goes down. Held for no other reason than that everything
    /// the server writes is inside it.
    dir: ScratchDir,
    /// Where `config.yaml` and the state file are.
    paths: Paths,
    server: Server,
}

impl Served {
    /// A server on a library in a scratch directory named after `tag`, configured by `base`. The
    /// directory is made here and handed to `base`, so the config can name its download folder as soon as
    /// it knows where it is; what comes back is resolved the way a run resolves it.
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
            config,
            &paths,
            lock,
        ))
        .await
        .expect("start the server");
        Served { dir, paths, server }
    }

    /// The same, on the desktop app's engine: [`Server::start_desktop`] with [`LAUNCH_SECRET`] and no
    /// origins, which no request here sends.
    async fn start_desktop(tag: &str, expose: bool, base: impl FnOnce(&Path) -> Config) -> Served {
        let dir = ScratchDir::new(tag);
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        let config = base(dir.path()).resolve(&paths);
        let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
        let access = DesktopAccess {
            launch_secret: LAUNCH_SECRET.to_string(),
            origins: Vec::new(),
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
        Served { dir, paths, server }
    }

    /// The address to send requests to.
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.server.local_addr())
    }

    /// Where this server's `config.yaml` is.
    fn file(&self) -> PathBuf {
        self.paths.config_file()
    }

    /// The download folder a run of `base` resolves to: `<scratch>/downloads`.
    fn download_dir(&self) -> PathBuf {
        self.dir.path().join("downloads")
    }

    /// Stops serving, saves and releases the library, then starts again on the same library, with its
    /// config read back from the file - the restart that says a save survives.
    async fn restart(self) -> Served {
        let Served { dir, paths, server } = self;
        within(server.shutdown()).await.expect("the last save ran");
        let config = Config::load_for_edit(&paths.config_file())
            .expect("the config reads back")
            .expect("a file was written");
        let config = config.resolve(&paths);
        let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
        let server = within(Server::start_with_settings(
            loopback(&config),
            config,
            &paths,
            lock,
        ))
        .await
        .expect("start the server again");
        Served { dir, paths, server }
    }

    /// Writes `config` to `config.yaml` as it stands, unresolved: what a person editing the file by hand
    /// leaves behind, and what a route reading it has to keep.
    fn save_config(&self, config: &Config) {
        let file = self.file();
        fs::create_dir_all(file.parent().expect("the config directory"))
            .expect("make the config directory");
        config.save(&file).expect("write the config file");
    }

    /// Stops serving and drops the scratch directory, so nothing is removed while a write is in flight.
    async fn stop(self) {
        let Served { dir, server, .. } = self;
        within(server.shutdown()).await.expect("the last save ran");
        drop(dir);
    }
}

/// The settings every engine here runs on: this config's, with the listening moved to loopback on a port
/// the OS picks and with DHT, LSD, UPnP and NAT-PMP turned off.
fn loopback(config: &Config) -> fbt::Settings {
    let mut settings = config.session_settings();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    settings
}

/// A config that serves on a port the OS picks, with [`USER`] as its only account, logins on and
/// downloads in `<scratch>/downloads`.
fn with_alice(scratch: &Path) -> Config {
    Config {
        download_dir: scratch.join("downloads"),
        server: Some(ServerConfig {
            bind: "127.0.0.1:0".parse().expect("a loopback bind address"),
            auth: AuthConfig {
                enabled: Some(true),
                users: vec![UserEntry {
                    name: USER.to_string(),
                    password_hash: password::hash(SECRET),
                }],
                devices: Vec::new(),
            },
            ..ServerConfig::default()
        }),
        ..Config::default()
    }
}

/// The client every request goes through: no redirects followed, no per-request timeout ([`within`]
/// bounds a test), and no proxy (every URL here is `127.0.0.1`). It has no cookie store, so a cookie is
/// only ever on a request a test put it on.
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("an http client")
}

/// A `GET` with the bearer token.
async fn get_bearer(client: &reqwest::Client, url: &str, token: &str) -> Response {
    within(client.get(url).bearer_auth(token).send())
        .await
        .expect("the server answered")
}

/// A `PUT` of `body` with the bearer token.
async fn put_bearer(client: &reqwest::Client, url: &str, token: &str, body: Value) -> Response {
    within(client.put(url).bearer_auth(token).json(&body).send())
        .await
        .expect("the server answered")
}

/// A `POST` of `body` with the bearer token.
async fn post_bearer(client: &reqwest::Client, url: &str, token: &str, body: Value) -> Response {
    within(client.post(url).bearer_auth(token).json(&body).send())
        .await
        .expect("the server answered")
}

/// A `PUT` of `body` carrying the session cookie and its CSRF token.
async fn put_cookie(
    client: &reqwest::Client,
    url: &str,
    cookie: &str,
    csrf: &str,
    body: Value,
) -> Response {
    within(
        client
            .put(url)
            .header(reqwest::header::COOKIE, cookie)
            .header(CSRF, csrf)
            .json(&body)
            .send(),
    )
    .await
    .expect("the server answered")
}

/// A password login at `/api/v1/auth/session`: the bearer token the protected requests here use.
async fn bearer(served: &Served) -> String {
    let response = within(
        http()
            .post(served.url("/api/v1/auth/session"))
            .basic_auth(USER, Some(SECRET))
            .send(),
    )
    .await
    .expect("the login reached the server");
    let body = body(response).await;
    body["token"]
        .as_str()
        .expect("a login that answers with a token")
        .to_string()
}

/// A browser sign-in, from the password to the `fbtt_session` value and the CSRF value a change needs.
async fn sign_in(served: &Served) -> (String, String) {
    let response = within(
        http()
            .post(served.url("/api/v1/auth/web-session"))
            .json(&json!({ "user": USER, "password": SECRET }))
            .send(),
    )
    .await
    .expect("the sign-in reached the server");
    let status = response.status();
    assert_eq!(status, StatusCode::OK, "a sign-in that did not succeed");
    let cookie = response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("an answer that sets a cookie")
        .split(';')
        .next()
        .expect("a cookie line")
        .to_string();
    let body = body(response).await;
    let csrf = body["csrf"].as_str().expect("a CSRF value").to_string();
    (cookie, csrf)
}

/// The JSON a response carries, which every route here answers in - errors included.
async fn body(response: Response) -> Value {
    within(response.json::<Value>()).await.expect("a JSON body")
}

/// The `{error: {code, message}}` every error here carries, so one parse handles the whole API.
async fn error_of(response: Response) -> (StatusCode, String, String) {
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

/// The `speed` half of a settings write: the running limits as a person states them, in KiB/s.
fn speed(
    download_kib: u64,
    upload_kib: u64,
    queue: (Option<usize>, Option<usize>, Option<usize>),
) -> Value {
    json!({
        "download_kib": download_kib,
        "upload_kib": upload_kib,
        "active_downloads": queue.0,
        "active_seeds": queue.1,
        "active_total": queue.2,
    })
}

/// The downloads section as the API takes it. `program` is the one key that may be absent: `None` leaves
/// it out (keep the program the file has), `Some(None)` sends `program: null` (run nothing), and
/// `Some(Some(_))` names one.
fn downloads_body(
    default_dir: &Path,
    unfinished_dir: Option<&Path>,
    watch_dir: Option<&Path>,
    start_paused: bool,
    program: Option<Option<&Path>>,
) -> Value {
    let mut body = json!({
        "default_dir": default_dir,
        "unfinished_dir": unfinished_dir,
        "start_paused": start_paused,
        "watch_dir": watch_dir,
    });
    if let Some(program) = program {
        body["on_complete"] = match program {
            Some(program) => json!({ "program": program }),
            None => json!({ "program": Value::Null }),
        };
    }
    body
}

/// The bytes of a file, for the tests that say a refused write left it exactly as it was.
fn bytes(path: &Path) -> Vec<u8> {
    fs::read(path).expect("the file is where it was")
}

// ---------------------------------------------------------------------------
// What the settings screen reads
// ---------------------------------------------------------------------------

/// `GET /api/v1/settings` answers the limits the engine is *running* under, not the last thing asked for:
/// the engine's own route sets them, and this one reads them back off the session. A screen that showed
/// what it last sent would drift the moment anything else changed them.
#[tokio::test(flavor = "multi_thread")]
async fn get_settings_reports_live_speed_and_downloads() {
    let served = Served::start("settings-get", with_alice).await;
    let client = http();
    let token = bearer(&served).await;

    // fbt-api's own route, in bytes per second: 100 KiB/s.
    let response = within(
        client
            .put(served.url("/api/v1/session/limits"))
            .bearer_auth(&token)
            .json(&json!({ "download": 100 * 1024, "upload": 0 }))
            .send(),
    )
    .await
    .expect("the server answered");
    assert!(
        response.status().is_success(),
        "the engine's own limits route did not answer: {}",
        response.status(),
    );

    let view = body(get_bearer(&client, &served.url("/api/v1/settings"), &token).await).await;
    assert_eq!(
        view["speed"]["download_kib"], 100,
        "the running download limit is read back from the session, in KiB/s: {view}",
    );
    assert_eq!(
        view["downloads"]["default_dir"],
        served.download_dir().to_string_lossy().as_ref(),
        "the downloads section reports the folder this run works in: {view}",
    );
    assert_eq!(
        view["downloads"]["on_complete"]["editable"], false,
        "a password session may not name a program to run on this machine: {view}",
    );

    served.stop().await;
}

/// A folder written relatively in `config.yaml` is a folder in the library directory once the run resolves
/// it, and the settings screen has to report the folder the run would work in rather than the spelling the
/// file happens to use. The file is read back on the first write, so that is where a stale unresolved copy
/// would surface: after a save, `GET` answers the resolved folder.
#[tokio::test(flavor = "multi_thread")]
async fn settings_report_a_relative_download_dir_resolved() {
    let served = Served::start("settings-relative", with_alice).await;
    let client = http();
    let token = bearer(&served).await;
    // A config a person wrote by hand, naming the download folder relatively: the run resolves that against
    // the library directory, and so must anything that reads the file back.
    let mut written = with_alice(served.dir.path());
    written.download_dir = PathBuf::from("downloads");
    served.save_config(&written);

    // A write is what reads the file back in; a speed save is the smallest one that does.
    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/speed"),
        &token,
        speed(64, 0, (None, None, None)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "a speed save");

    let expected = served.paths.data_dir.join("downloads");
    let view = body(get_bearer(&client, &served.url("/api/v1/settings"), &token).await).await;
    assert_eq!(
        view["downloads"]["default_dir"],
        expected.to_string_lossy().as_ref(),
        "a relative folder is reported as the absolute one this run would work in: {view}",
    );

    served.stop().await;
}

// ---------------------------------------------------------------------------
// Speed: applied and saved
// ---------------------------------------------------------------------------

/// A speed save does two things and the order matters: the file gets the limits, and then the engine is
/// put under them. Written to `config.yaml`, they are there again the next time this library is served.
#[tokio::test(flavor = "multi_thread")]
async fn put_speed_applies_and_survives_restart() {
    let served = Served::start("settings-speed", with_alice).await;
    let client = http();
    let token = bearer(&served).await;

    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/speed"),
        &token,
        speed(64, 32, (Some(2), Some(3), Some(4))),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a speed save answers the settings it made",
    );
    let view = body(response).await;
    assert_eq!(view["speed"]["download_kib"], 64, "{view}");

    // The engine itself is running under them: fbt-api's session route reads the live values, in bytes
    // per second.
    let session = body(get_bearer(&client, &served.url("/api/v1/session"), &token).await).await;
    assert_eq!(
        session["download_limit"],
        64 * 1024,
        "the running download limit is 64 KiB/s in bytes: {session}",
    );
    assert_eq!(
        session["upload_limit"],
        32 * 1024,
        "and the upload one is 32 KiB/s: {session}",
    );
    let queue = body(get_bearer(&client, &served.url("/api/v1/session/queue"), &token).await).await;
    assert_eq!(queue["active_downloads"], 2, "{queue}");
    assert_eq!(queue["active_seeds"], 3, "{queue}");
    assert_eq!(queue["active_limit"], 4, "{queue}");

    // Stop and serve the same library again, the config read back from the file: the limits survive.
    let served = served.restart().await;
    let client = http();
    let token = bearer(&served).await;
    let view = body(get_bearer(&client, &served.url("/api/v1/settings"), &token).await).await;
    assert_eq!(
        view["speed"]["download_kib"], 64,
        "the saved download limit is what the next run starts under: {view}",
    );
    assert_eq!(
        view["speed"]["upload_kib"], 32,
        "and the upload one: {view}",
    );

    served.stop().await;
}

// ---------------------------------------------------------------------------
// Downloads: persisted, and the program only with the secret
// ---------------------------------------------------------------------------

/// A downloads save writes this library's file, so everything else in it - the accounts, the device keys,
/// and any key this version does not know - is the person's, not the route's, and has to come through
/// untouched.
#[tokio::test(flavor = "multi_thread")]
async fn put_downloads_persists_and_keeps_users_devices_and_unknown_keys() {
    let served = Served::start("settings-downloads", with_alice).await;
    let client = http();
    let token = bearer(&served).await;

    // The file as a person leaves it: a device key, and a key from a newer version.
    let mut config = with_alice(served.dir.path());
    config
        .server
        .as_mut()
        .expect("a server section")
        .auth
        .devices
        .push(DeviceEntry {
            name: "phone".to_string(),
            public_key: PUBLIC_KEY.to_string(),
        });
    served.save_config(&config);
    let hash = config.server.as_ref().expect("a server section").auth.users[0]
        .password_hash
        .clone();
    let mut text = fs::read_to_string(served.file()).expect("the config file");
    text.push_str("future_key: 1\n");
    fs::write(served.file(), text).expect("add a key this version does not know");

    let unfinished = served.download_dir().join("incomplete");
    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/downloads"),
        &token,
        downloads_body(&served.download_dir(), Some(&unfinished), None, true, None),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "a downloads save");

    let after = Config::load_for_edit(&served.file())
        .expect("the config reads back")
        .expect("the file is still there");
    let server = after.server.as_ref().expect("a server section");
    assert_eq!(
        server
            .auth
            .users
            .iter()
            .map(|user| user.name.as_str())
            .collect::<Vec<_>>(),
        vec![USER],
        "the accounts are the person's, not the route's",
    );
    assert_eq!(
        server.auth.users[0].password_hash, hash,
        "and their hashes came through the save as the file had them",
    );
    assert_eq!(
        after
            .server
            .as_ref()
            .expect("a server section")
            .auth
            .devices,
        vec![DeviceEntry {
            name: "phone".to_string(),
            public_key: PUBLIC_KEY.to_string(),
        }],
        "and so are the device keys",
    );
    assert!(
        after.unknown_keys.contains_key("future_key"),
        "a key this version does not know survives the save: {:?}",
        after.unknown_keys,
    );
    assert!(
        fs::read_to_string(served.file())
            .expect("the config file")
            .contains("future_key: 1"),
        "and is written back as it was read",
    );
    assert_eq!(after.download_dir, served.download_dir(), "the new folder");
    assert_eq!(
        after.downloads.unfinished_dir,
        Some(unfinished.clone()),
        "and the unfinished folder beside it: {:?}",
        after.downloads,
    );
    assert!(after.downloads.start_paused, "{:?}", after.downloads);

    served.stop().await;
}

/// A folder outside the folders this server serves is refused before anything is written: the allowed
/// folders are the whole point of `server.allowed_roots`, and the answer says which line a person reads.
#[tokio::test(flavor = "multi_thread")]
async fn put_downloads_outside_roots_is_403() {
    let served = Served::start("settings-outside", with_alice).await;
    let client = http();
    let token = bearer(&served).await;

    let outside = served.dir.path().join("elsewhere");
    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/downloads"),
        &token,
        downloads_body(&outside, None, None, false, None),
    )
    .await;
    let (status, code, message) = error_of(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a folder outside the roots");
    assert_eq!(code, "outside_roots");
    assert_eq!(
        message, "Choose a folder inside the server's allowed folders.",
        "the refusal is the plan's own line: {message}",
    );
    assert!(
        !served.file().exists(),
        "nothing was written for a refused change",
    );

    served.stop().await;
}

/// A link inside the roots that leads out is outside them too. Its own spelling is below a root, so the
/// lexical check the plan makes accepts it; resolved, it points somewhere this server does not serve, so
/// the folder has to be refused before it is written down - otherwise a torrent would land wherever the
/// link leads, and a change that looked local would write outside the library.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn put_downloads_through_a_symlink_out_of_the_roots_is_403() {
    let served = Served::start("settings-symlink", with_alice).await;
    let client = http();
    let token = bearer(&served).await;
    // Somewhere this server does not serve, and a link to it inside the folder it does serve.
    let outside = served.dir.path().join("outside");
    fs::create_dir_all(&outside).expect("a folder outside the roots");
    let link = served.download_dir().join("out");
    std::os::unix::fs::symlink(&outside, &link).expect("a link inside the roots that leads out");
    // A config on disk, so a refusal can be told apart from "nothing had been written anyway".
    served.save_config(&with_alice(served.dir.path()));
    let before = bytes(&served.file());

    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/downloads"),
        &token,
        downloads_body(&link, None, None, false, None),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a link that leads out is outside the roots",
    );
    let (status, code, message) = error_of(response).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a link that leads out is outside the roots: {message}",
    );
    assert_eq!(code, "outside_roots");
    assert_eq!(
        bytes(&served.file()),
        before,
        "the file is exactly as it was: nothing was written for a refused change",
    );

    served.stop().await;
}

/// The deepest part of a folder that exists is what decides: a folder that is not there yet is judged by
/// the root above it, so a downloads change may name one before anything has made it. This is the other
/// half of the symlink rule - the check must not refuse a plain new folder just because it does not exist.
#[tokio::test(flavor = "multi_thread")]
async fn put_downloads_may_name_a_folder_that_is_not_there_yet() {
    let served = Served::start("settings-new-folder", with_alice).await;
    let client = http();
    let token = bearer(&served).await;

    let fresh = served.download_dir().join("not-made-yet");
    assert!(!fresh.exists(), "the folder really is not there");
    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/downloads"),
        &token,
        downloads_body(&fresh, None, None, false, None),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a folder below a root is accepted whether or not it is there yet",
    );

    served.stop().await;
}

/// A root reached through a link of its own has two spellings, and a client may arrive with either: the one
/// `config.yaml` names, or the one the file system resolves it to. On a machine where they differ - a link
/// in the download folder's own path, and by default on macOS, where `/var` is a link to `/private/var` -
/// a folder spelled the way the file system spells it is inside a root just as much as the other, so the
/// check is handed both. Refusing the resolved spelling would refuse a person the folder the server itself
/// had just shown them.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn put_downloads_accepts_a_root_spelled_the_way_the_file_system_resolves_it() {
    let served = Served::start("settings-root-spellings", |scratch| {
        let real = scratch.join("real");
        fs::create_dir_all(&real).expect("the folder the library really is in");
        let link = scratch.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("a link to it");
        let mut config = with_alice(scratch);
        // The library is served through the link, so the root this run serves has the link as its own
        // spelling and `real` as the form the file system resolves.
        config.download_dir = link;
        config
    })
    .await;
    let client = http();
    let token = bearer(&served).await;

    // The same root, spelled the way the file system resolves it: on this machine the scratch directory's
    // own prefix resolves too, which is what makes the two spellings differ even without a link of ours.
    let root_resolved = served
        .dir
        .path()
        .join("link")
        .canonicalize()
        .expect("the root resolves");
    let resolved = root_resolved.join("shows");
    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/downloads"),
        &token,
        downloads_body(&resolved, None, None, false, None),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the folder spelled the way the file system resolves the root is inside a root",
    );

    served.stop().await;
}
#[tokio::test(flavor = "multi_thread")]
async fn put_downloads_unfinished_equal_default_is_400() {
    let served = Served::start("settings-unfinished", with_alice).await;
    let client = http();
    let token = bearer(&served).await;

    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/downloads"),
        &token,
        downloads_body(
            &served.download_dir(),
            Some(&served.download_dir()),
            None,
            false,
            None,
        ),
    )
    .await;
    let (status, code, message) = error_of(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{message}");
    assert_eq!(code, "invalid_setting");
    assert_eq!(
        message, "The unfinished folder must be different from the default folder.",
        "the refusal is the plan's own line: {message}",
    );

    served.stop().await;
}

/// A password session - over a bearer token - may not name a program for this server to run: the program
/// lives in the server's own `config.yaml` and a client that merely visits the machine has no business
/// naming one. The refusal comes before anything is written, so the file is exactly as it was.
#[tokio::test(flavor = "multi_thread")]
async fn run_a_program_from_a_password_session_is_403() {
    let served = Served::start("settings-program-bearer", with_alice).await;
    let client = http();
    let token = bearer(&served).await;
    served.save_config(&with_alice(served.dir.path()));
    let before = bytes(&served.file());

    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/downloads"),
        &token,
        downloads_body(
            &served.download_dir(),
            None,
            None,
            false,
            Some(Some(&fake())),
        ),
    )
    .await;
    let (status, code, _) = error_of(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a password session");
    assert_eq!(code, "set_in_config_file");
    assert_eq!(
        bytes(&served.file()),
        before,
        "nothing was written for a refused change",
    );

    served.stop().await;
}

/// The same refusal for the browser's cookie session: the desktop engine that started this server took a
/// launch secret, so *a* client may name a program, but a page the browser signed in to is not that client.
/// The cookie is a real credential here - it reaches the route - and it must still not carry the secret.
#[tokio::test(flavor = "multi_thread")]
async fn run_a_program_from_the_web_cookie_session_is_403() {
    let served = Served::start_desktop("settings-program-cookie", true, with_alice).await;
    let client = http();
    let (cookie, csrf) = sign_in(&served).await;
    served.save_config(&with_alice(served.dir.path()));
    let before = bytes(&served.file());

    let response = put_cookie(
        &client,
        &served.url("/api/v1/settings/downloads"),
        &cookie,
        &csrf,
        downloads_body(
            &served.download_dir(),
            None,
            None,
            false,
            Some(Some(&fake())),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a cookie session may not name a program",
    );
    let (status, code, _) = error_of(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a cookie session");
    assert_eq!(code, "set_in_config_file");
    assert_eq!(
        bytes(&served.file()),
        before,
        "nothing was written for a refused change",
    );

    served.stop().await;
}

/// The desktop app's engine is the one thing that may name a program: the app made the secret for this
/// launch, and it is the app that runs the engine for the person sitting at this machine.
#[tokio::test(flavor = "multi_thread")]
async fn run_a_program_with_the_launch_secret_is_saved() {
    let served = Served::start_desktop("settings-program-secret", false, with_alice).await;
    let client = http();

    let response = within(
        client
            .put(served.url("/api/v1/settings/downloads"))
            .bearer_auth(LAUNCH_SECRET)
            .json(&downloads_body(
                &served.download_dir(),
                None,
                None,
                false,
                Some(Some(&fake())),
            ))
            .send(),
    )
    .await
    .expect("the server answered");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the launch secret names a program",
    );

    let after = Config::load_for_edit(&served.file())
        .expect("the config reads back")
        .expect("the file was written");
    assert_eq!(
        after.downloads.on_complete,
        Some(fake()),
        "the program is in the file",
    );

    served.stop().await;
}

/// A desktop engine that also serves others - `expose` - takes the configured logins as well, and the
/// launch secret beside them. The password account still may not name a program; the secret still may.
#[tokio::test(flavor = "multi_thread")]
async fn exposed_desktop_password_user_cannot_set_the_program() {
    let served = Served::start_desktop("settings-program-exposed", true, with_alice).await;
    let client = http();
    let token = bearer(&served).await;

    let body_with_program = downloads_body(
        &served.download_dir(),
        None,
        None,
        false,
        Some(Some(&fake())),
    );
    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/downloads"),
        &token,
        body_with_program.clone(),
    )
    .await;
    let (status, code, _) = error_of(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a password session");
    assert_eq!(code, "set_in_config_file");

    let response = within(
        client
            .put(served.url("/api/v1/settings/downloads"))
            .bearer_auth(LAUNCH_SECRET)
            .json(&body_with_program)
            .send(),
    )
    .await
    .expect("the server answered");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the launch secret names a program on an exposed engine too",
    );
    let after = Config::load_for_edit(&served.file())
        .expect("the config reads back")
        .expect("the file was written");
    assert_eq!(after.downloads.on_complete, Some(fake()));

    served.stop().await;
}

/// The launch secret is the `Authorization` header and nothing else. A request that carries it in the
/// query - a URL a log or a browser history keeps - is not the desktop app saying "this is my machine",
/// however well the guard treats the query token for reaching the API at all.
#[tokio::test(flavor = "multi_thread")]
async fn query_token_secret_does_not_count_as_launch_secret() {
    let served = Served::start_desktop("settings-program-query", true, with_alice).await;
    let client = http();
    let token = bearer(&served).await;

    let url = format!(
        "{}?token={LAUNCH_SECRET}",
        served.url("/api/v1/settings/downloads")
    );
    let response = put_bearer(
        &client,
        &url,
        &token,
        downloads_body(
            &served.download_dir(),
            None,
            None,
            false,
            Some(Some(&fake())),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "the query token never counts as the launch secret",
    );
    let (status, code, _) = error_of(response).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the query token never counts as the launch secret",
    );
    assert_eq!(code, "set_in_config_file");
    assert!(
        !served.file().exists(),
        "nothing was written for a refused change",
    );

    served.stop().await;
}

/// The same rule with no session at all: a `?token=<launch secret>` and nothing else is refused before any
/// handler runs, because the launch secret is only ever read from the `Authorization` header. The query
/// token is a credential for the file-stream and event routes and for nothing else, so on a settings route
/// there is nothing in it for the guard to accept and the answer is `401`, not a `200` and not a `403` from
/// inside the handler.
#[tokio::test(flavor = "multi_thread")]
async fn a_query_token_on_its_own_does_not_reach_the_settings_routes() {
    let served = Served::start_desktop("settings-query-alone", true, with_alice).await;
    let client = http();
    let url = format!(
        "{}?token={LAUNCH_SECRET}",
        served.url("/api/v1/settings/downloads")
    );

    let response = within(
        client
            .put(&url)
            .json(&downloads_body(
                &served.download_dir(),
                None,
                None,
                false,
                Some(Some(&fake())),
            ))
            .send(),
    )
    .await
    .expect("the server answered");
    assert_ne!(
        response.status(),
        StatusCode::OK,
        "the launch secret in a URL is not the desktop app naming a program",
    );
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "and it is not a credential on this route at all: it is refused before the handler runs",
    );
    assert!(
        !served.file().exists(),
        "nothing was written for a refused change",
    );

    served.stop().await;
}

/// A `GET` with the same query token and no header is refused the same way: the settings routes read no
/// credential from the URL, so the address alone is not enough to read a server's settings.
#[tokio::test(flavor = "multi_thread")]
async fn a_query_token_on_its_own_does_not_read_the_settings() {
    let served = Served::start_desktop("settings-query-get", true, with_alice).await;
    let client = http();
    let url = format!("{}?token={LAUNCH_SECRET}", served.url("/api/v1/settings"));

    let response = within(client.get(&url).send())
        .await
        .expect("the server answered");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a query token is not a session on the settings routes",
    );

    served.stop().await;
}

/// The view says whether *this* request may change the program, and shows the configured one either way:
/// the field is not hidden from a password session, it is shown as read-only, which is the difference
/// between "you cannot change this" and "there is nothing here".
#[tokio::test(flavor = "multi_thread")]
async fn get_reports_program_not_editable_without_the_secret() {
    let served = Served::start_desktop("settings-program-view", true, |scratch| {
        let mut config = with_alice(scratch);
        config.downloads = DownloadsConfig {
            on_complete: Some(fake()),
            ..DownloadsConfig::default()
        };
        config
    })
    .await;
    let client = http();
    let token = bearer(&served).await;

    let view = body(get_bearer(&client, &served.url("/api/v1/settings"), &token).await).await;
    assert_eq!(
        view["downloads"]["on_complete"]["program"],
        fake().to_string_lossy().as_ref(),
        "the configured program is shown to a password session: {view}",
    );
    assert_eq!(
        view["downloads"]["on_complete"]["editable"], false,
        "which may not change it: {view}",
    );

    let response = within(
        client
            .get(served.url("/api/v1/settings"))
            .bearer_auth(LAUNCH_SECRET)
            .send(),
    )
    .await
    .expect("the server answered");
    let view = body(response).await;
    assert_eq!(
        view["downloads"]["on_complete"]["program"],
        fake().to_string_lossy().as_ref(),
        "the secret sees the same value: {view}",
    );
    assert_eq!(
        view["downloads"]["on_complete"]["editable"], true,
        "and may change it: {view}",
    );

    served.stop().await;
}

// ---------------------------------------------------------------------------
// The allowed folders do not move, and two saves do not lose one another
// ---------------------------------------------------------------------------

/// An empty `server.allowed_roots` in the file means "the download folder", which `Config::resolve` fills
/// in at startup: the folders this run serves. Moving the download folder would then move the allowed
/// folders with it, so a change to the folder pins the roots this run serves into the file first.
#[tokio::test(flavor = "multi_thread")]
async fn changing_default_folder_pins_empty_roots() {
    let served = Served::start("settings-pin", with_alice).await;
    let client = http();
    let token = bearer(&served).await;
    // The file as a person wrote it: the roots unnamed, so resolving this file today means "the download
    // folder" and nothing else.
    served.save_config(&with_alice(served.dir.path()));
    let old = served.download_dir();

    let moved = old.join("shows");
    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/downloads"),
        &token,
        downloads_body(&moved, None, None, false, None),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "a downloads save");

    let after = Config::load_for_edit(&served.file())
        .expect("the config reads back")
        .expect("the file is still there");
    assert_eq!(
        after.download_dir, moved,
        "the new download folder is what was asked for",
    );
    assert_eq!(
        after
            .server
            .as_ref()
            .expect("a server section")
            .allowed_roots,
        vec![old.clone()],
        "and the folders this run serves are pinned, so they do not move with it",
    );

    served.stop().await;
}

/// Every config write goes through one lock, because each is a read-modify-write of the same file: two
/// that read before either writes leave one of them with a change nobody asked for and the other with
/// nothing at all. A speed save, a downloads save and a device add, all at once, all end up in the file.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_section_saves_keep_both() {
    let served = Served::start("settings-concurrent", with_alice).await;
    let client = http();
    let token = bearer(&served).await;

    let speed_url = served.url("/api/v1/settings/speed");
    let downloads_url = served.url("/api/v1/settings/downloads");
    let devices_url = served.url("/api/v1/auth/devices");
    let download_dir = served.download_dir();
    let (speed_response, downloads_response, device_response) = tokio::join!(
        put_bearer(
            &client,
            &speed_url,
            &token,
            speed(64, 32, (None, None, None))
        ),
        put_bearer(
            &client,
            &downloads_url,
            &token,
            downloads_body(&download_dir, None, None, true, None),
        ),
        post_bearer(
            &client,
            &devices_url,
            &token,
            json!({ "name": "phone", "public_key": PUBLIC_KEY }),
        ),
    );
    for (what, response) in [
        ("the speed save", speed_response),
        ("the downloads save", downloads_response),
        ("the device add", device_response),
    ] {
        assert!(
            response.status().is_success(),
            "{what} did not succeed: {}",
            response.status(),
        );
    }

    let after = Config::load_for_edit(&served.file())
        .expect("the config reads back")
        .expect("the file was written");
    assert_eq!(
        after.limits.download_kib, 64,
        "the speed save is in the file: {:?}",
        after.limits,
    );
    assert!(
        after.downloads.start_paused,
        "and so is the downloads save: {:?}",
        after.downloads,
    );
    assert!(
        after
            .server
            .as_ref()
            .expect("a server section")
            .auth
            .devices
            .iter()
            .any(|device| device.name == "phone"),
        "and the device add: {:?}",
        after
            .server
            .as_ref()
            .expect("a server section")
            .auth
            .devices,
    );

    served.stop().await;
}

/// Two speed saves at once: the file gets one, then the engine is put under it, both inside one lock, so
/// whichever save is written last is the one the engine ends up under. With the engine put under the
/// limits after the lock is released, the two applies can land in the reverse order and leave the running
/// engine on limits the file no longer holds - a client reads 128 KiB/s from the file and the engine is
/// still on 64.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_speed_saves_leave_the_engine_on_the_file() {
    let served = Served::start("settings-speed-race", with_alice).await;
    let client = http();
    let token = bearer(&served).await;
    let url = served.url("/api/v1/settings/speed");

    let (one, two) = tokio::join!(
        put_bearer(&client, &url, &token, speed(64, 8, (None, None, None))),
        put_bearer(&client, &url, &token, speed(128, 16, (None, None, None))),
    );
    for (what, response) in [
        ("the first speed save", one),
        ("the second speed save", two),
    ] {
        assert!(
            response.status().is_success(),
            "{what} did not succeed: {}",
            response.status(),
        );
    }

    let after = Config::load_for_edit(&served.file())
        .expect("the config reads back")
        .expect("the file was written");
    let in_the_file = after.limits.download_kib;
    assert!(
        in_the_file == 64 || in_the_file == 128,
        "one of the two saves is in the file: {in_the_file}",
    );
    let session = body(get_bearer(&client, &served.url("/api/v1/session"), &token).await).await;
    assert_eq!(
        session["download_limit"],
        in_the_file * 1024,
        "the engine is running under the limits the file holds, not the other save's: {session} against {in_the_file} KiB/s",
    );

    served.stop().await;
}

/// A device removed while another is added: each change reads what the last one wrote, so the list logins
/// check against is the list the file holds when both have answered - the removed key is gone from both.
/// With the list swapped after the lock is released, two swaps can land in the reverse order and leave a
/// key the file no longer has still accepted, which is the worst kind of stale: a revoked key that keeps
/// working.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_remove_racing_an_add_leaves_the_file_and_the_list_agreeing() {
    let served = Served::start("settings-device-race", with_alice).await;
    let client = http();
    let token = bearer(&served).await;
    let devices_url = served.url("/api/v1/auth/devices");

    // A key that is really there to begin with, so removing it is a removal rather than a no-op.
    let added = post_bearer(
        &client,
        &devices_url,
        &token,
        json!({ "name": "old", "public_key": PUBLIC_KEY }),
    )
    .await;
    assert_eq!(added.status(), StatusCode::CREATED, "a device to remove");

    let remove_url = format!("{devices_url}/old");
    let (removed, added_again) = tokio::join!(
        within(client.delete(&remove_url).bearer_auth(&token).send()),
        post_bearer(
            &client,
            &devices_url,
            &token,
            json!({ "name": "new", "public_key": OTHER_KEY }),
        ),
    );
    let removed = removed.expect("the server answered");
    assert_eq!(removed.status(), StatusCode::NO_CONTENT, "the remove");
    assert_eq!(
        added_again.status(),
        StatusCode::CREATED,
        "the add beside it",
    );

    let after = Config::load_for_edit(&served.file())
        .expect("the config reads back")
        .expect("the file was written");
    let mut in_the_file: Vec<String> = after
        .server
        .as_ref()
        .expect("a server section")
        .auth
        .devices
        .iter()
        .map(|device| device.name.clone())
        .collect();
    in_the_file.sort();
    assert!(
        !in_the_file.iter().any(|name| name == "old"),
        "the removal is in the file whatever order the two changes were written in: {in_the_file:?}",
    );

    let listed = body(get_bearer(&client, &devices_url, &token).await).await;
    let mut in_the_list: Vec<String> = listed
        .as_array()
        .expect("a list of devices")
        .iter()
        .map(|row| row["name"].as_str().expect("a name").to_string())
        .collect();
    in_the_list.sort();
    assert_eq!(
        in_the_list, in_the_file,
        "the list logins check against is the list the file holds, so a removed key is gone from both",
    );

    served.stop().await;
}

/// A file this server cannot read is not one to write over: the change is answered and nothing happens.
/// In particular the engine is not put under limits the file does not have, so the running session is
/// exactly where it was.
#[tokio::test(flavor = "multi_thread")]
async fn unreadable_config_is_500_and_applies_nothing() {
    let served = Served::start("settings-unreadable", with_alice).await;
    let client = http();
    let token = bearer(&served).await;
    let file = served.file();
    fs::create_dir_all(file.parent().expect("the config directory"))
        .expect("make the config directory");
    fs::write(&file, "server:\n  allowed_hosts: [bit.local\n").expect("write an unreadable config");

    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/speed"),
        &token,
        speed(64, 32, (None, None, None)),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a file this server cannot read is not its to repair",
    );
    let (status, code, _) = error_of(response).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a file this server cannot read is not its to repair",
    );
    assert_eq!(code, "config_unreadable");

    let session = body(get_bearer(&client, &served.url("/api/v1/session"), &token).await).await;
    assert_eq!(
        session["download_limit"], 0,
        "the limits the file does not have were not applied: {session}",
    );

    served.stop().await;
}

/// A config this server can read but not write is a different failure from one it cannot read, and gets a
/// different name: `500 config_unwritable`, saying what the operating system said, so whoever runs this
/// server can act on it instead of looking for a parse error that is not there. Like every other answer
/// here it names no path and no file name - that is what the log line is for - and the file is left exactly
/// as it was, with the engine not put under limits the file does not hold.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_config_that_cannot_be_written_is_500_config_unwritable() {
    use std::os::unix::fs::PermissionsExt;

    let served = Served::start("settings-unwritable", with_alice).await;
    let client = http();
    let token = bearer(&served).await;
    served.save_config(&with_alice(served.dir.path()));
    let before = bytes(&served.file());
    // The folder the config lives in, made read-only: the file can still be read, so the failure is the
    // save's and not the read's.
    let config_dir = served
        .file()
        .parent()
        .expect("the config directory")
        .to_path_buf();
    let original = fs::metadata(&config_dir)
        .expect("the config directory")
        .permissions();
    let mut read_only = original.clone();
    read_only.set_mode(0o555);
    fs::set_permissions(&config_dir, read_only).expect("make the config folder read-only");

    let response = put_bearer(
        &client,
        &served.url("/api/v1/settings/speed"),
        &token,
        speed(64, 0, (None, None, None)),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a config that cannot be written is not a client's mistake",
    );
    let (status, code, message) = error_of(response).await;
    // Put the folder back first, so the scratch directory can be removed even if an assertion below fails.
    fs::set_permissions(&config_dir, original).expect("make the config folder writable again");

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a config that cannot be written is not a client's mistake: {message}",
    );
    assert_eq!(code, "config_unwritable");
    assert!(
        message.contains("write its config file"),
        "the answer says which file failed: {message}",
    );
    assert!(
        message.to_lowercase().contains("permission"),
        "and says what the operating system said: {message}",
    );
    // The same silence every other answer here keeps: a caller is not told where the config lives or what
    // it is called, however useful that would be to an operator - they have the log line for it.
    let directory = config_dir.to_string_lossy().into_owned();
    assert!(
        !message.contains(&directory) && !message.contains("config.yaml"),
        "the answer names neither the path nor the file: {message}",
    );
    assert_eq!(
        bytes(&served.file()),
        before,
        "the file is exactly as it was: nothing was written",
    );
    let session = body(get_bearer(&client, &served.url("/api/v1/session"), &token).await).await;
    assert_eq!(
        session["download_limit"], 0,
        "and the limits the file does not have were not applied: {session}",
    );

    served.stop().await;
}

// ---------------------------------------------------------------------------
// The engine's own routes are untouched
// ---------------------------------------------------------------------------

/// fbt-api's `/api/v1/session/limits` still sets the running limits and still does not write anything:
/// the new routes are the ones that persist, and an older client keeps working the way it did.
#[tokio::test(flavor = "multi_thread")]
async fn engine_routes_still_work_and_do_not_persist() {
    let served = Served::start("settings-engine-routes", with_alice).await;
    let client = http();
    let token = bearer(&served).await;
    served.save_config(&with_alice(served.dir.path()));
    let before = bytes(&served.file());

    let response = within(
        client
            .put(served.url("/api/v1/session/limits"))
            .bearer_auth(&token)
            .json(&json!({ "download": 2048, "upload": 0 }))
            .send(),
    )
    .await
    .expect("the server answered");
    assert!(
        response.status().is_success(),
        "the engine's own limits route still works: {}",
        response.status(),
    );

    let view = body(get_bearer(&client, &served.url("/api/v1/settings"), &token).await).await;
    assert_eq!(
        view["speed"]["download_kib"], 2,
        "the engine is running under the new limit: {view}",
    );
    assert_eq!(
        bytes(&served.file()),
        before,
        "and the file was not touched: the engine's route does not persist",
    );

    served.stop().await;
}
