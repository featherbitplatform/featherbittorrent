//! The remote backend end to end: a `Server` at one address, a client built by `fbtt-core`'s
//! [`RemoteBackend`] at another, and the fake servers a real one has not got yet.
//!
//! Everything here speaks plain http to loopback - no TLS, no trust store, nothing that could leave the
//! machine - so what is under test is this crate's own login routes plus fbt-api's, and what happens when
//! the thing at the other end of the socket is rude. The two fake servers (a hostile one and a gateway
//! one) exist because no featherbittorrent release answers with 17 MiB for a torrent list or hands out
//! OAuth2 bearer tokens yet; they are written with the same axum the real server runs on, so the requests
//! they see are the requests a client really sends.
//!
//! Every wait has a deadline, and every test ends by sending `Shutdown` and reading notices until the queue
//! closes, which is how a test knows the backend's task ended rather than merely stopped publishing.

use std::net::SocketAddr;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::MethodRouter, routing::get, routing::post, routing::put};
use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use fbtt_core::scratch::ScratchDir;
use fbtt_core::{
    AddSource, AuthConfig, Client, Command, Config, Connection, DeviceCommand, DeviceEntry,
    DirListing, LibraryLock, LinkState, LoginMethod, Notice, Paths, RemoteBackend, RemoteTarget,
    RowState, Secret, ServerConfig, SettingsChange, SettingsCommand, SettingsState, TorrentId,
    UserEntry,
};
use fbtt_server::Server;
use fbtt_server::auth::password;
use futures::StreamExt;
use serde_json::{Value, json};
use signature::Verifier;
use ssh_key::{
    Algorithm, EcdsaCurve, HashAlg, LineEnding, PrivateKey, PublicKey, Signature,
    rand_core::UnwrapErr,
};

/// How long a test waits for anything. A server start plus an engine check takes a moment; nothing here
/// should take a tenth of this.
const DEADLINE: Duration = Duration::from_secs(60);
/// How often a waiting test looks again after the snapshot went quiet, so a change it missed does not have
/// to wait for the next publish to be noticed.
const POLL: Duration = Duration::from_millis(250);
/// The payload every test seeds: a little over a piece, so a download is more than one round trip.
const MIB: usize = 1024 * 1024;

/// Runs `what` under the deadline, so a client or server that stops answering fails with a message instead
/// of hanging the test job.
async fn within<T>(what: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
}

/// The text of a notice, whichever kind it was. A refused sign-in is bad news about the connection like any
/// other error, so the tests below that only read words treat it the same way; the ones that care which kind
/// they were handed use [`notice_of_kind`] instead.
fn text(notice: Notice) -> String {
    match notice {
        Notice::Info(line) | Notice::Error(line) | Notice::SignInRefused(line) => line,
        // A file to play is not a line of text a test reads; these tests never hand one out.
        Notice::Play(_) => String::new(),
    }
}

// --------------------------------------------------------------------------------------------- harness

/// A library in a scratch directory that can be served, stopped and served again on the same port: what
/// `server_restart_reconnects` needs, and why this holds the config rather than the running server.
struct Library {
    dir: ScratchDir,
    paths: Paths,
    config: Config,
}

impl Library {
    /// A library with `users` and `devices` as its logins and auth on, downloading into the scratch
    /// directory. The download folder is the only allowed root, which is what [`Config::resolve`] makes it
    /// when the config names none - so a remote add without a folder has exactly one place to go.
    fn new(tag: &str, users: Vec<UserEntry>, devices: Vec<DeviceEntry>) -> Library {
        let dir = ScratchDir::new(tag);
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        let config = base_config(dir.path(), users, devices, Some(true), 0).resolve(&paths);
        Library { dir, paths, config }
    }

    /// The address of `path` on this library's server.
    fn url(&self, addr: SocketAddr, path: &str) -> String {
        format!("http://{addr}{path}")
    }

    /// Serves this library on a port the OS picks, and hands back the address it ended up on: pass that
    /// port to [`serve_on`](Self::serve_on) after stopping to be heard on the same one again.
    async fn serve(&self) -> (Server, SocketAddr) {
        self.serve_on(0).await
    }

    /// Serves this library with authentication off, on a port the OS picks: what a
    /// gateway test needs, because the gateway in front of the server is the credential,
    /// and a server with auth on would ask for one the test did not mean to hand it.
    async fn serve_with_auth_off(&self) -> (Server, SocketAddr) {
        let dir = self.dir.path();
        let paths = self.paths.clone();
        let config = base_config(dir, vec![], vec![], Some(false), 0).resolve(&paths);
        let lock = LibraryLock::acquire(&self.paths.lock_file()).expect("lock the library");
        let server = within(Server::start_with_settings(
            loopback(&config),
            config,
            &paths,
            lock,
        ))
        .await
        .expect("start the server with auth off");
        let addr = server.local_addr();
        (server, addr)
    }

    /// Serves this library again, on `port` (0: any port). The lock is taken here and goes with the
    /// server, which holds it until its last save - so a second run starts on the state the first left,
    /// never beside it.
    async fn serve_on(&self, port: u16) -> (Server, SocketAddr) {
        let mut config = self.config.clone();
        config
            .server
            .as_mut()
            .expect("this library's config chose to serve")
            .bind = format!("127.0.0.1:{port}")
            .parse()
            .expect("a loopback bind address");
        let lock = LibraryLock::acquire(&self.paths.lock_file()).expect("lock the library");
        let server = within(Server::start_with_settings(
            loopback(&config),
            config,
            &self.paths,
            lock,
        ))
        .await
        .expect("start the server");
        let addr = server.local_addr();
        (server, addr)
    }
}

/// The settings every engine here runs on: loopback only, with DHT, LSD, UPnP and NAT-PMP off, so nothing
/// a test starts can reach another machine.
fn loopback(config: &Config) -> fbt::Settings {
    let mut settings = config.session_settings();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    settings
}

/// A config that serves on `port` with auth on, `users` and `devices` as its logins, and downloads in
/// `<scratch>/downloads`.
fn base_config(
    scratch: &Path,
    users: Vec<UserEntry>,
    devices: Vec<DeviceEntry>,
    auth: Option<bool>,
    port: u16,
) -> Config {
    Config {
        download_dir: scratch.join("downloads"),
        // These tests are about serving, so their configs say so.
        server: Some(ServerConfig {
            bind: format!("127.0.0.1:{port}")
                .parse()
                .expect("a loopback bind address"),
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

/// The account every test that needs a password uses.
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

/// A fresh Ed25519 key pair.
fn keypair() -> PrivateKey {
    let mut rng = UnwrapErr(getrandom::SysRng);
    PrivateKey::random(&mut rng, Algorithm::Ed25519).expect("an ed25519 key")
}

/// A fresh ECDSA P-256 key pair, the kind `ssh-keygen -t ecdsa -b 256` makes: what a gateway that documents
/// a `private_key_jwt` grant hands out.
fn ecdsa_keypair() -> PrivateKey {
    let mut rng = UnwrapErr(getrandom::SysRng);
    PrivateKey::random(
        &mut rng,
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        },
    )
    .expect("a p256 key")
}

/// The one-line OpenSSH public key of `key`.
fn public_line(key: &PrivateKey) -> String {
    key.public_key().to_openssh().expect("the public key line")
}

/// Where a test's target points: this address, that method, nothing else.
fn target(
    addr: SocketAddr,
    login: LoginMethod,
    user: Option<&str>,
    key_path: Option<PathBuf>,
) -> RemoteTarget {
    RemoteTarget {
        url: format!("http://{addr}"),
        login,
        user: user.map(str::to_string),
        key_path,
        token_url: None,
        gateway_key_header: None,
        client_id: None,
    }
}

/// Writes `bytes` to `<dir>/<name>` and returns the path.
fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    std::fs::create_dir_all(dir).expect("make a directory for a test file");
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write a test file");
    path
}

/// Deterministic bytes, so a test can compare what arrived with what was seeded without a random seed.
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

/// The `.torrent` for the file at `path`, hashed off the blocking pool.
async fn torrent_for(path: &Path) -> Vec<u8> {
    let path = path.to_path_buf();
    within(tokio::task::spawn_blocking(move || {
        fbt::CreateTorrent::new(path)
            .build(&mut |_done, _total| ControlFlow::Continue(()))
            .expect("hash the test torrent")
    }))
    .await
    .expect("the hashing task ran")
}

/// The id a snapshot row carries for `metainfo`: the lowercase hex of the v1 hash, which is what fbt-api
/// puts in its URLs and in every DTO's `id`.
fn id_of(metainfo: &fbt::Metainfo) -> TorrentId {
    TorrentId::from_wire_hash(metainfo.info_hashes.wire_hash())
}

/// The client every direct request goes through: no redirect following, so a test sees the answer the
/// server gave. `.no_proxy()` keeps a test from routing a `127.0.0.1` request through a proxy a machine
/// happens to be set with.
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("an http client")
}

// ------------------------------------------------------------------------------------ waiting on a client

/// Waits until `look` finds something in a snapshot, and hands it back. A screen never waits, but a test
/// has nothing to draw while it waits, so it may.
async fn wait_for<T>(client: &mut Client, look: impl Fn(&fbtt_core::Snapshot) -> Option<T>) -> T {
    let until = tokio::time::Instant::now() + DEADLINE;
    loop {
        let snapshot = client.snapshot();
        if let Some(answer) = look(&snapshot) {
            return answer;
        }
        if tokio::time::Instant::now() >= until {
            panic!(
                "nothing waited for appeared within {} seconds; the snapshot says: {snapshot:?}",
                DEADLINE.as_secs()
            );
        }
        // Take the next publish when one comes, and look again shortly anyway: a backend that is stuck
        // publishing nothing must not turn into a hung test.
        let _ = tokio::time::timeout(POLL, client.changed().changed()).await;
    }
}

/// Waits until the link to the server says `want`, and hands back the snapshot that said it.
async fn wait_for_state(client: &mut Client, want: bool) -> fbtt_core::Snapshot {
    wait_for(client, |snapshot| {
        let connected = matches!(
            snapshot.connection,
            Connection::Remote {
                state: LinkState::Connected,
                ..
            }
        );
        (connected == want).then(|| snapshot.clone())
    })
    .await
}

/// The next notice, or `None` if none arrives before the deadline.
async fn one_notice(client: &mut Client) -> Option<Notice> {
    within(tokio::time::timeout(
        Duration::from_secs(30),
        client.notice(),
    ))
    .await
    .ok()
    .flatten()
}

/// Collects every notice that is waiting right now, with a moment to let late ones arrive.
async fn notices_so_far(client: &mut Client) -> Vec<String> {
    let mut lines = Vec::new();
    loop {
        match tokio::time::timeout(POLL * 8, client.notice()).await {
            Ok(Some(notice)) => lines.push(text(notice)),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    lines
}

/// Notices until `enough` is happy with what has arrived, or the deadline runs out. A fake that answers
/// differently on each call puts its trouble on different polls, so a test that wants a particular line
/// cannot ask for "everything so far after a moment" - it has to keep reading until it has it.
async fn notices_until(client: &mut Client, enough: impl Fn(&[String]) -> bool) -> Vec<String> {
    let mut lines = Vec::new();
    let until = tokio::time::Instant::now() + DEADLINE;
    while !enough(&lines) && tokio::time::Instant::now() < until {
        match within(tokio::time::timeout(POLL * 8, client.notice())).await {
            Ok(Some(notice)) => lines.push(text(notice)),
            // The backend stopped: whatever arrived is all there is, and the asserts say so.
            Ok(None) => break,
            // A quiet queue is not an answer: the backend may still be dialling.
            Err(_) => {}
        }
    }
    lines
}

/// Reads notices until one of them is the kind `is_one` describes, which is what a test needs when the *kind*
/// is the thing under test rather than its words: the binary takes a kept password out of its keychain on the
/// strength of [`fbtt_core::Notice::SignInRefused`] alone, so a backend that put that news in an ordinary
/// error line would leave it looking for something it is never sent.
async fn notice_that(client: &mut Client, is_one: impl Fn(&Notice) -> bool) -> Notice {
    let until = tokio::time::Instant::now() + DEADLINE;
    while tokio::time::Instant::now() < until {
        match within(tokio::time::timeout(POLL * 8, client.notice())).await {
            Ok(Some(notice)) if is_one(&notice) => return notice,
            // Other news about this connection is not what this helper was asked for, but it says the backend
            // is alive and talking. A quiet queue means the same, and is waited out like `notices_until` does.
            Ok(Some(_)) | Err(_) => {}
            // The backend stopped: nothing more will arrive.
            Ok(None) => break,
        }
    }
    panic!(
        "no notice of the kind asked for arrived within {} seconds",
        DEADLINE.as_secs()
    );
}

/// Ends the connection and proves it ended: `Shutdown` goes in, and notices are read until the queue
/// closes, which only happens when the backend's task has stopped.
async fn shutdown(client: &mut Client) {
    assert!(
        client.send(Command::Shutdown),
        "the queue takes the last command"
    );
    while let Some(notice) = within(client.notice()).await {
        eprintln!("notice on the way out: {}", text(notice));
    }
}

/// A session that is seeding `bytes` from `dir`, plus the address its torrent can be reached on.
async fn seed_from(dir: PathBuf, bytes: Vec<u8>) -> (fbt::Session, SocketAddr) {
    let file = write_file(&dir, "payload.bin", &sample(MIB));
    let torrent = torrent_for(&file).await;
    assert_eq!(torrent, bytes, "the same file hashes to the same torrent");
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    // The seed's own copy of the data has to be where its save path says it is, which is why the file was
    // written inside `dir` rather than beside it.
    // `Settings` is non-exhaustive across crates, so its fields are set one at a time on a default.
    let mut settings = fbt::Settings::default();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    let session = within(fbt::Session::new(settings))
        .await
        .expect("a session to seed with");
    session
        .add_torrent(fbt::AddTorrent::new(metainfo, dir))
        .await
        .expect("seed the torrent");
    within(async {
        loop {
            if session
                .torrents_status()
                .iter()
                .any(|status| status.state == fbt::TorrentState::Seeding)
            {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    let addr = session.local_addr();
    (session, addr)
}

/// Logs in over http the way a client does, and hands back the session token: what a test needs to call a
/// route on a server whose auth is on without going through the backend it is testing.
async fn login(addr: SocketAddr, name: &str, secret: &str) -> String {
    let response = within(
        http()
            .post(format!("http://{addr}/api/v1/auth/session"))
            .basic_auth(name, Some(secret))
            .send(),
    )
    .await
    .expect("the login request reached the server");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a matching password signs in"
    );
    let body: Value = within(response.json()).await.expect("a JSON answer");
    body.get("token")
        .and_then(Value::as_str)
        .expect("the answer names a token")
        .to_string()
}

// -------------------------------------------------------------------------------------- the real server

/// A password login gets a table, an add, a peer and a download: the whole remote path over the routes a
/// featherbittorrent server actually serves.
#[tokio::test(flavor = "multi_thread")]
async fn remote_password_end_to_end() {
    let lib = Library::new("remote_password", vec![alice()], vec![]);
    let (server, addr) = lib.serve().await;

    // A seed with the whole file already on disk, so the only thing the server's torrent is missing is a
    // peer to ask.
    let seed_dir = lib.dir.path().join("seed");
    let bytes = torrent_for(&write_file(&seed_dir, "payload.bin", &sample(MIB))).await;
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    let id = id_of(&metainfo);
    let (seed, seed_addr) = seed_from(seed_dir.clone(), bytes.clone()).await;

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::Password, Some("alice"), None),
        Secret::Password("hunter2".to_string().into()),
        None,
    );

    // The first thing a screen needs to know is whether the address answered at all, and where a torrent
    // goes when nobody names a folder: the server's own root.
    let connected = wait_for_state(&mut client, true).await;
    assert_eq!(connected.default_dir, lib.config.download_dir);
    let Connection::Remote { name, auth, .. } = &connected.connection else {
        panic!("a remote backend says where it came from");
    };
    assert_eq!(name, "127.0.0.1");
    assert_eq!(auth, "password");

    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes.clone()),
            dir: None,
        }),
        "the queue takes the torrent"
    );
    let added = one_notice(&mut client).await.expect("a line about the add");
    assert_eq!(text(added), "Added payload.bin");

    let row = wait_for(&mut client, |snapshot| {
        snapshot
            .torrents
            .iter()
            .find(|row| row.id == id && row.size > 0)
            .cloned()
    })
    .await;
    // The engine answers with the folder as it resolved it. On macOS that means `/private/var` where this
    // scratch directory was named `/var`, so both sides are compared after resolving links.
    assert_eq!(
        row.save_path
            .canonicalize()
            .expect("the save path is a folder"),
        lib.config
            .download_dir
            .canonicalize()
            .expect("the server made its download folder")
    );

    // The seed's address goes in through fbt-api's own add-peer route, called direct: the backend has no
    // reason to know about a peer this test invented.
    let token = login(addr, "alice", "hunter2").await;
    let response = within(
        http()
            .post(lib.url(addr, &format!("/api/v1/torrents/{}/peers", id.0)))
            .bearer_auth(&token)
            .json(&json!({ "addr": seed_addr.to_string() }))
            .send(),
    )
    .await
    .expect("the peer route answered");
    assert!(
        response.status().is_success(),
        "the server took the seed as a peer: {}",
        response.status()
    );

    let done = wait_for(&mut client, |snapshot| {
        snapshot
            .torrents
            .iter()
            .find(|row| row.id == id && row.state == RowState::Seeding)
            .cloned()
    })
    .await;
    assert_eq!(done.size, MIB as u64, "the whole file was wanted");
    assert_eq!(done.done, done.size, "every wanted byte arrived");
    assert_eq!(done.progress, 1.0);

    // The watched pane is what `Watch` asks for, and the files it shows are the server's own list.
    assert!(client.send(Command::Watch(Some((
        id.clone(),
        fbtt_core::DetailTab::Files
    )))));
    let detail = wait_for(&mut client, |snapshot| {
        snapshot.detail.clone().map(|(watched, detail)| {
            (watched == id).then(|| match detail {
                fbtt_core::Detail::Files(files) => files,
                other => panic!("the files pane was asked for, not {other:?}"),
            })
        })?
    })
    .await;
    assert_eq!(detail.len(), 1);
    assert_eq!(detail[0].path, "payload.bin");
    assert_eq!(detail[0].done, MIB as u64);

    // The folder answer a picker draws comes from the server's roots route.
    assert!(client.send(Command::ListDirs(None)));
    let dirs = wait_for(&mut client, |snapshot| snapshot.dirs.clone()).await;
    assert_eq!(dirs.roots, vec![lib.config.download_dir.clone()]);

    shutdown(&mut client).await;
    within(server.shutdown()).await.expect("the last save ran");
    seed.shutdown().await;
}

/// A passphrase-protected key file logs in, and the token it earned is good for a route that needs one.
#[tokio::test(flavor = "multi_thread")]
async fn remote_key_file_end_to_end() {
    let key = keypair();
    let encrypted = {
        let mut rng = UnwrapErr(getrandom::SysRng);
        key.encrypt(&mut rng, "correct horse")
            .expect("protect the test key")
    };
    assert!(encrypted.is_encrypted());

    let lib = Library::new(
        "remote_key_file",
        vec![alice()],
        vec![device("test-laptop", &encrypted)],
    );
    let (server, addr) = lib.serve().await;
    let pem = encrypted.to_openssh(LineEnding::LF).expect("the key file");
    let key_file = write_file(lib.dir.path(), "id_ed25519", pem.as_bytes());

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::KeyFile, None, Some(key_file.clone())),
        Secret::KeyPassphrase(Some("correct horse".to_string().into())),
        None,
    );

    let connected = wait_for_state(&mut client, true).await;
    assert_eq!(connected.default_dir, lib.config.download_dir);
    let Connection::Remote { auth, secure, .. } = &connected.connection else {
        panic!("a remote backend says how it signed in");
    };
    assert_eq!(auth, "device key");
    assert!(!secure, "these tests speak plain http");

    // The device list is an authenticated route: seeing this test's own device in it proves the token came
    // from a signature this server accepted.
    assert!(client.send(Command::Devices(DeviceCommand::List)));
    let devices = wait_for(&mut client, |snapshot| snapshot.devices.clone()).await;
    assert_eq!(devices.len(), 1, "one device is configured");
    assert_eq!(devices[0].name, "test-laptop");
    assert!(devices[0].fingerprint.starts_with("SHA256:"));

    // An add over a key session works the same as one over a password session.
    let seed_dir = lib.dir.path().join("seed");
    let bytes = torrent_for(&write_file(&seed_dir, "payload.bin", &sample(MIB))).await;
    assert!(client.send(Command::Add {
        source: AddSource::File(bytes),
        dir: None,
    }));
    let added = one_notice(&mut client).await.expect("a line about the add");
    assert_eq!(text(added), "Added payload.bin");
    wait_for(&mut client, |snapshot| {
        (!snapshot.torrents.is_empty()).then_some(())
    })
    .await;

    shutdown(&mut client).await;
    within(server.shutdown()).await.expect("the last save ran");
}

/// A server that goes away is a `Lost` link with the last table still on it, and starting it again on the
/// same port brings both back.
#[tokio::test(flavor = "multi_thread")]
async fn server_restart_reconnects() {
    let lib = Library::new("remote_restart", vec![alice()], vec![]);
    let (server, addr) = lib.serve().await;

    let seed_dir = lib.dir.path().join("seed");
    let bytes = torrent_for(&write_file(&seed_dir, "payload.bin", &sample(MIB))).await;
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    let id = id_of(&metainfo);

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::Password, Some("alice"), None),
        Secret::Password("hunter2".to_string().into()),
        None,
    );
    wait_for_state(&mut client, true).await;
    assert!(client.send(Command::Add {
        source: AddSource::File(bytes),
        dir: None,
    }));
    let _ = one_notice(&mut client).await;
    wait_for(&mut client, |snapshot| {
        snapshot
            .torrents
            .iter()
            .any(|row| row.id == id)
            .then_some(())
    })
    .await;

    // The server stops answering: the link says so, and what the screen keeps drawing is the last table.
    let port = addr.port();
    within(server.shutdown()).await.expect("the last save ran");
    let lost = wait_for(&mut client, |snapshot| match snapshot.connection {
        Connection::Remote {
            state: LinkState::Lost { retry_in_secs },
            ..
        } => Some((retry_in_secs, snapshot.torrents.clone())),
        _ => None,
    })
    .await;
    assert!(
        lost.1.iter().any(|row| row.id == id),
        "the table on the screen is the last good one"
    );
    assert!(
        (1..=30).contains(&lost.0),
        "a lost link says when it will try again, not never"
    );

    // The same port, a new server: the row comes back out of the saved state on its own.
    let (server, again) = lib.serve_on(port).await;
    assert_eq!(
        again.port(),
        port,
        "the restarted server is where the client expects it"
    );
    wait_for_state(&mut client, true).await;
    wait_for(&mut client, |snapshot| {
        snapshot
            .torrents
            .iter()
            .any(|row| row.id == id)
            .then_some(())
    })
    .await;

    shutdown(&mut client).await;
    within(server.shutdown()).await.expect("the last save ran");
}

/// A wrong password is one line the person can act on, and a link that says it is down - not a panic, and
/// not a client that has stopped answering. It arrives as `Notice::SignInRefused` rather than as an ordinary
/// error that happens to use the word "refused": whoever wired this client keeps passwords for its servers,
/// and can only tell "the entry we kept is wrong" from "this request did not work" when the refusal is handed
/// to them as itself.
#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_is_a_notice() {
    let lib = Library::new("remote_wrong_password", vec![alice()], vec![]);
    let (server, addr) = lib.serve().await;

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::Password, Some("alice"), None),
        Secret::Password("not the password".to_string().into()),
        None,
    );

    let refusal = notice_that(&mut client, |notice| {
        matches!(notice, Notice::SignInRefused(_))
    })
    .await;
    assert!(
        matches!(&refusal, Notice::SignInRefused(reason) if reason.contains("sign-in was refused")),
        "a refused login arrives as a refusal somebody can act on, in its own words: {refusal:?}"
    );
    let snapshot = client.snapshot();
    assert!(
        matches!(
            snapshot.connection,
            Connection::Remote {
                state: LinkState::Lost { .. },
                ..
            }
        ),
        "with no credential there is no library to list"
    );

    // The client still answers its screen, including the request to stop.
    assert!(client.send(Command::ListDirs(None)));
    shutdown(&mut client).await;
    within(server.shutdown()).await.expect("the last save ran");
}

/// A target this client cannot use at all - a password login with no user name to go with it - is reported on
/// the client rather than thrown at whoever called `start`: one line saying what is missing, and a link that
/// admits it is down instead of one that keeps "connecting" while nothing can ever fix it.
#[tokio::test(flavor = "multi_thread")]
async fn a_target_that_cannot_be_used_says_the_link_is_down() {
    let mut client = RemoteBackend::start(
        RemoteTarget {
            // Nothing is dialled, so this address only has to look like one: what fails is the pairing of
            // method and secret, before any request is made.
            url: "http://127.0.0.1:1".to_string(),
            login: LoginMethod::Password,
            user: None,
            key_path: None,
            token_url: None,
            gateway_key_header: None,
            client_id: None,
        },
        Secret::Password("hunter2".to_string().into()),
        None,
    );

    let lines = notices_until(&mut client, |lines| !lines.is_empty()).await;
    assert!(
        lines.iter().any(|line| line.contains("user name")),
        "the line says what is missing: {lines:?}"
    );
    // Nothing was dialled, so the snapshot that `start` published is already final: an address this client
    // cannot put a request on is not one a later attempt would do better at, and a header that spun forever
    // would read as a server that is still being reached.
    let Connection::Remote { state, .. } = &client.snapshot().connection else {
        panic!("a remote backend says where it came from");
    };
    assert!(
        matches!(state, LinkState::Lost { retry_in_secs: 1 }),
        "a target nobody can dial says so rather than spinning: {state:?}"
    );

    // It still answers its screen, including the request to stop.
    assert!(client.send(Command::ListDirs(None)));
    shutdown(&mut client).await;
}

/// A connection that could not be started keeps both of its answer queues open.
///
/// The case this covers is a restart: `ui.yaml` points at a password target, the keychain no longer holds a
/// password for it, so `start` fails on the pairing of method and secret before anything is dialled. It still
/// owes the screen one line saying what is missing - but a queue with no senders left reads as a finished
/// backend, and a run loop that believes that quits before the person has read the line, which is exactly the
/// case that has to arrive at the connect screen with the address and method still filled in. So the sender
/// ends travel with the task that answers `Shutdown` and go only when it does.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_that_failed_before_dialling_keeps_the_queues_open() {
    let nowhere = "127.0.0.1:1".parse().expect("a port nothing listens on");
    let mut client = RemoteBackend::start(
        target(nowhere, LoginMethod::Password, Some("alice"), None),
        // A password login with no password in it: what a restart without a keychain entry hands over.
        Secret::None,
        None,
    );

    let lines = notices_until(&mut client, |lines| !lines.is_empty()).await;
    assert!(
        lines.iter().any(|line| line.contains("password")),
        "the line says what is missing: {lines:?}"
    );

    // The snapshot published by `start` is the last one this connection will have, and taking it marks that
    // change as seen - so asking for the next one is what shows whether the queue behind it is still open.
    within(client.changed().changed())
        .await
        .expect("the down state arrives");
    let Connection::Remote { state, .. } = &client.snapshot().connection else {
        panic!("a remote backend says where it came from");
    };
    assert!(
        matches!(state, LinkState::Lost { .. }),
        "nothing can be asked of this address: {state:?}"
    );

    // Both queues are quiet rather than closed. `Ok(None)` is the queue ending, which is what a run loop
    // takes as "this backend has stopped and everything it had to say has been read" - the thing that used to
    // end the screen too early. A timeout means nothing answered yet, which is the state a live connection is
    // in while its person decides what to do about the line above.
    match within(tokio::time::timeout(POLL * 4, client.notice())).await {
        Ok(None) => panic!("the notice queue closed while the connection was still open"),
        Ok(Some(extra)) => eprintln!("a second line arrived: {}", text(extra)),
        Err(_) => {}
    }
    match within(tokio::time::timeout(POLL * 4, client.changed().changed())).await {
        Ok(Ok(())) => {}
        Ok(Err(_gone)) => panic!("the snapshot queue closed while the connection was still open"),
        Err(_) => {}
    }

    // It answers the request to stop, and only then do the queues close - which is what ends this drain.
    assert!(client.send(Command::ListDirs(None)));
    shutdown(&mut client).await;
}

/// Takes back `port` after its server has gone. A listener can linger for a moment after the process that
/// held it stops, so this tries until it gets the port or the deadline says it never will.
async fn bind_black_hole(port: u16) -> tokio::net::TcpListener {
    within(async {
        loop {
            match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => break listener,
                Err(problem) if problem.kind() == std::io::ErrorKind::AddrInUse => {
                    tokio::time::sleep(POLL).await;
                }
                Err(problem) => panic!("cannot take port {port}: {problem}"),
            }
        }
    })
    .await
}

/// Pressing stop is not held up by a server that has stopped answering. The revoke that ends this client's own
/// session gets two seconds of its own rather than the fifteen a login is allowed, and a request already in
/// flight is given up on when the stop arrives rather than waited out: so the task ends within about those two
/// seconds whatever the server is doing with the connection.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_is_quick_when_server_is_gone() {
    let lib = Library::new("remote_quick_shutdown", vec![alice()], vec![]);
    let (server, addr) = lib.serve().await;
    let port = addr.port();

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::Password, Some("alice"), None),
        Secret::Password("hunter2".to_string().into()),
        None,
    );
    wait_for_state(&mut client, true).await;

    // The server goes away, and something that accepts connections and answers none of them takes its port.
    // From here every request this client makes waits for an answer that is not coming.
    within(server.shutdown()).await.expect("the last save ran");
    let black_hole = bind_black_hole(port).await;
    tokio::spawn(async move {
        // Held open and never written to. `forget` is what holds it: dropping the socket would close it, and
        // a close is an answer - this port is supposed to give none.
        loop {
            let (socket, _) = black_hole.accept().await.expect("the black hole accepts");
            std::mem::forget(socket);
        }
    });

    // The stop goes in immediately: whether or not a round of reads happens to be waiting on the black hole
    // at this moment, it is the same two seconds either way, which is the point of the test.
    let started = tokio::time::Instant::now();
    assert!(
        client.send(Command::Shutdown),
        "the queue takes the last command"
    );
    while let Some(notice) = within(client.notice()).await {
        eprintln!("notice on the way out: {}", text(notice));
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "the task ended in {:.1} s: the revoke gets two seconds and nothing else is waited on",
        elapsed.as_secs_f64()
    );
}

// --------------------------------------------------------------------------------------- the fake servers

/// What a fake server accepts as proof of who is asking, and what it remembers about the requests it saw.
struct Fake {
    mode: Mode,
    /// The folder its roots route names.
    root: String,
    /// Every `Authorization` value that reached an API route.
    seen: Mutex<Vec<String>>,
    /// Every assertion that reached the token endpoint, and how many tokens it handed out.
    assertions: Mutex<Vec<String>>,
    issued: AtomicUsize,
    /// The seconds a token this fake hands out lives for.
    token_ttl: u64,
    /// The device public key a `private_key_jwt` assertion has to be signed by.
    public_key: Option<PublicKey>,
    /// This installation's client id, which the assertion has to name.
    client_id: String,
    /// How many times the torrent list was asked for (the hostile server answers it differently each time).
    torrents_calls: AtomicUsize,
    /// Whether this fake answers its first three torrent lists rudely. `hostile_server_is_bounded` needs that;
    /// every other test wants the same answer every time so it can count what a client does.
    rude: bool,
    /// How many login requests arrived at `/api/v1/auth/session`, right or wrong. Counting these is how the
    /// backoff tests show a client is not hammering a refused password.
    logins: AtomicUsize,
    /// How many event streams were dialled. A client whose link is down should not be dialling any.
    event_dials: AtomicUsize,
    /// The session token `Mode::Session` accepts right now: empty until somebody logs in.
    session: Mutex<String>,
    /// The header a gateway standing in front of this fake reads its API key out of, and the value it insists
    /// on. `None` for every fake that is not behind one, which is most of them: this is what a request has to
    /// carry *in addition to* whatever `mode` asks for (see `docs/reverse-proxy.md`'s setup 2).
    gateway_header: Option<(String, String)>,
    /// Every value that reached that header, empty where a request carried none. Recorded before the
    /// credential is looked at, so "no request went out without it" can be answered for refused ones too.
    gateway_keys: Mutex<Vec<String>>,
    /// What reached the *token endpoint* under that header - a provider's own front door, which is not this
    /// fake's (`gateway_key_not_sent_to_token_endpoint` puts the two on separate addresses). Recorded only
    /// when something arrived, so an empty list is the answer "nothing went there".
    token_gateway_keys: Mutex<Vec<String>>,
    /// Set by a test to end every session this fake handed out - which is how a client finds out its token is
    /// dead the way it does with a real server: by being refused for it.
    revoked: AtomicBool,
    /// Set once `burst_events` has written its burst of frames, so a test knows when they are behind it.
    burst_written: AtomicBool,
    /// How many times the file list of the stream test's torrent was asked for: the file list is the
    /// one route a `Stream` answers from before it asks for a token, and counting it is how a test
    /// finds out the answer came from a list it wrote.
    files_calls: AtomicUsize,
    /// The body the file list answers with: the two files the largest-file test hashed.
    files: Mutex<Vec<String>>,
}

/// The credential a fake server insists on.
enum Mode {
    /// Nothing: every request is somebody.
    Open,
    /// A bearer token this fake issued, any one that has not been overtaken by a newer issue.
    Bearer,
    /// A user name and password on every request.
    Basic(String, String),
    /// A session token, earned at `/api/v1/auth/session` with `alice` and her password.
    Session,
}

impl Fake {
    /// A fake that insists on `mode` and is otherwise default: the temp folder as its one root, no token
    /// issued yet, and every route answering the same way every time.
    fn new(mode: Mode) -> Arc<Fake> {
        Arc::new(Fake {
            mode,
            root: std::env::temp_dir().to_string_lossy().into_owned(),
            seen: Mutex::new(Vec::new()),
            assertions: Mutex::new(Vec::new()),
            issued: AtomicUsize::new(0),
            token_ttl: 3_600,
            public_key: None,
            client_id: "featherbittorrent-test".to_string(),
            torrents_calls: AtomicUsize::new(0),
            rude: false,
            logins: AtomicUsize::new(0),
            event_dials: AtomicUsize::new(0),
            session: Mutex::new(String::new()),
            gateway_header: None,
            gateway_keys: Mutex::new(Vec::new()),
            token_gateway_keys: Mutex::new(Vec::new()),
            revoked: AtomicBool::new(false),
            burst_written: AtomicBool::new(false),
            files_calls: AtomicUsize::new(0),
            files: Mutex::new(Vec::new()),
        })
    }

    /// The hostile fake: nothing asked for, and a torrent list that is rude on its first three calls.
    fn open() -> Arc<Fake> {
        let mut fake =
            Arc::into_inner(Fake::new(Mode::Open)).expect("nobody else holds this test state");
        fake.rude = true;
        Arc::new(fake)
    }

    /// A fake that asks for nothing and answers everything the same way: for counting what a client does
    /// rather than testing how it copes with what it is handed.
    fn plain() -> Arc<Fake> {
        Fake::new(Mode::Open)
    }

    /// The session fake: `POST /api/v1/auth/session` with `alice` and her password earns a token that every
    /// other route then has to carry, and setting `revoked` ends the one it handed out.
    fn session() -> Arc<Fake> {
        Fake::new(Mode::Session)
    }

    /// The same server behind a gateway that checks a key of its own first: `header` has to arrive with
    /// `key` under it on every request, this fake's own login call included, and the password session is what
    /// opens once it does. Two credentials are asked for and neither one is the other's substitute.
    fn keyed_session(header: &str, key: &str) -> Arc<Fake> {
        let mut fake =
            Arc::into_inner(Fake::new(Mode::Session)).expect("nobody else holds this test state");
        fake.gateway_header = Some((header.to_string(), key.to_string()));
        Arc::new(fake)
    }

    /// The bearer fake for a device key, with the token's life spelled out. Which credential counts is
    /// decided by what the token endpoint issues, so this one is strict from the first request: a call
    /// that arrives without a bearer token has not earned one.
    fn gateway(public_key: PublicKey, ttl: u64) -> Arc<Fake> {
        let mut fake =
            Arc::into_inner(Fake::new(Mode::Bearer)).expect("nobody else holds this test state");
        fake.public_key = Some(public_key);
        fake.token_ttl = ttl;
        Arc::new(fake)
    }

    /// The gateway fake with a front door of its own: a bearer token bought from a provider, and an API key
    /// every request to *this* server has to carry as well. `header` and `key` are what this fake's routes
    /// insist on before they look at any credential; the token endpoint is not one of them - a test using
    /// this fake serves it itself, on its own address, which is the arrangement where "did the key follow the
    /// assertion out?" has an answer (see `gateway_key_not_sent_to_token_endpoint`).
    fn keyed_gateway(public_key: PublicKey, ttl: u64, header: &str, key: &str) -> Arc<Fake> {
        let mut fake = Arc::into_inner(Fake::gateway(public_key, ttl))
            .expect("nobody else holds this test state");
        fake.gateway_header = Some((header.to_string(), key.to_string()));
        Arc::new(fake)
    }

    fn basic(user: &str, password: &str) -> Arc<Fake> {
        Fake::new(Mode::Basic(user.to_string(), password.to_string()))
    }

    /// The front door, asked about before any credential: what a gateway standing in front of this fake looks
    /// for on its way in, and what it says to a request that did not bring one. Nothing below this runs for
    /// such a request, which is the whole arrangement setup 2 is - the gateway decides whether this is
    /// somebody before this server ever sees a bearer token.
    fn gate(&self, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
        let Some((name, want)) = self.gateway_header.as_ref() else {
            return Ok(());
        };
        let given = headers
            .get(name.as_str())
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        self.gateway_keys
            .lock()
            .expect("the test state")
            .push(given.clone());
        if &given != want {
            return Err((
                StatusCode::FORBIDDEN,
                "the gateway key is not this one".to_string(),
            ));
        }
        Ok(())
    }

    /// Notes what arrived at the token endpoint under this fake's gateway header, which is a different
    /// machine's door from [`Self::gate`]: nothing here refuses a request for it, because the question a test
    /// asks is whether this client sent one at all, and a refusal would take the rest of the connection with
    /// it whatever the answer.
    fn record_token_gate(&self, headers: &HeaderMap) {
        let Some((name, _)) = self.gateway_header.as_ref() else {
            return;
        };
        if let Some(value) = headers
            .get(name.as_str())
            .and_then(|value| value.to_str().ok())
        {
            self.token_gateway_keys
                .lock()
                .expect("the test state")
                .push(value.to_string());
        }
    }

    /// Records a request and says whether its credential is good enough. A request with no
    /// `Authorization` at all is what a server running with auth off gets, so it is only refused once this
    /// fake has decided somebody must prove who they are.
    fn check(&self, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
        self.gate(headers)?;
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        self.seen
            .lock()
            .expect("the test state")
            .push(given.clone());
        match &self.mode {
            Mode::Open => {}
            Mode::Bearer => {
                // Any token this gateway issued counts, not only the newest: a renewal leaves the older one
                // good until its own expiry, and a client with a request of its still in flight is not doing
                // anything wrong.
                let number = match given.strip_prefix("Bearer token-") {
                    Some(number) => number.parse::<usize>().ok(),
                    None => None,
                };
                let issued = self.issued.load(Ordering::SeqCst);
                if !number.is_some_and(|number| number < issued) {
                    return Err((
                        StatusCode::UNAUTHORIZED,
                        "the bearer token is not one this gateway issued".to_string(),
                    ));
                }
            }
            // The token is whatever `session_login` last handed out - unless a test has said the session is
            // over, which is how a client finds its credential is dead: by being refused for it.
            Mode::Session => {
                let want = self.session.lock().expect("the test state").clone();
                if self.revoked.load(Ordering::SeqCst) || given != format!("Bearer {want}") {
                    return Err((
                        StatusCode::UNAUTHORIZED,
                        "this session is not one this server issued".to_string(),
                    ));
                }
            }
            Mode::Basic(user, secret) => {
                let wrong = || {
                    (
                        StatusCode::UNAUTHORIZED,
                        "the password credential is not this one".to_string(),
                    )
                };
                let Some(rest) = given.strip_prefix("Basic ") else {
                    return Err(wrong());
                };
                // A gateway spells the credential as standard base64 with padding, which the url-safe
                // decoder cannot read, so both spellings are tried: this fake is not testing one client
                // only.
                let decoded = BASE64_URL_SAFE_NO_PAD
                    .decode(rest)
                    .or_else(|_| base64::prelude::BASE64_STANDARD.decode(rest))
                    .unwrap_or_default();
                let text = String::from_utf8_lossy(&decoded).into_owned();
                if text != format!("{user}:{secret}") {
                    return Err(wrong());
                }
            }
        }
        Ok(())
    }

    /// The bearer token a request has to carry for `Mode::Session`: whatever `session_login` handed out, or
    /// an empty string nobody holds until somebody logs in.
    fn session_token(&self) -> String {
        self.session.lock().expect("the test state").clone()
    }
}

/// Serves `router` on a port the OS picks, in its own task, and hands back the address.
async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port for the test server");
    let addr = listener.local_addr().expect("the test server's address");
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("the test server stopped serving");
    });
    addr
}

/// The routes every fake answers: enough of `/api/v1` for a refresh to succeed, with `stream` behind
/// `/api/v1/events`. The hostile fake puts an endless line there; the gateway fakes answer with the one
/// line that ends. A gateway target adds `POST /token` itself, before this is given its state.
fn api_routes(stream: MethodRouter<Arc<Fake>>) -> Router<Arc<Fake>> {
    Router::new()
        .route("/api/v1/fs/roots", get(roots))
        .route("/api/v1/torrents", get(torrents))
        .route("/api/v1/session", get(session))
        .route("/api/v1/session/queue", get(queue))
        .route("/api/v1/events", stream)
}

async fn roots(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    match fake.check(&headers) {
        Ok(()) => Json(vec![fake.root.clone()]).into_response(),
        Err(problem) => problem.into_response(),
    }
}

/// The torrent list. The hostile fake answers its first three calls differently, which is how one test
/// exercises a body that is not JSON, a good round between them, and a body that is too big; every other
/// fake answers the same way every time, so a test can count the calls instead.
async fn torrents(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if let Err(problem) = fake.check(&headers) {
        return problem.into_response();
    }
    match fake.torrents_calls.fetch_add(1, Ordering::SeqCst) {
        0 if fake.rude => (
            [(header::CONTENT_TYPE, "application/json")],
            "{ \"not\": [\"a torrent list\"",
        )
            .into_response(),
        2 if fake.rude => {
            // 17 MiB crosses the 16 MiB cap this client reads: the test proves the
            // cap is enforced, and the over-cap answer costs the client a line, not
            // its memory.
            let big = vec![b'x'; 17 * 1024 * 1024];
            (
                [(header::CONTENT_TYPE, "application/json")],
                String::from_utf8(big).expect("ascii"),
            )
                .into_response()
        }
        _ => Json(Vec::<Value>::new()).into_response(),
    }
}

async fn session(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    match fake.check(&headers) {
        Ok(()) => {
            Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})).into_response()
        }
        Err(problem) => problem.into_response(),
    }
}

async fn queue(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    match fake.check(&headers) {
        Ok(()) => Json(json!({
            "active_downloads": 2,
            "active_seeds": null,
            "active_limit": null,
            "dont_count_slow_torrents": true,
            "inactive_down_rate": 1024,
            "inactive_up_rate": 1024,
            "startup_grace": 30.0,
            "interval": 1.0,
        }))
        .into_response(),
        Err(problem) => problem.into_response(),
    }
}

/// An event stream that says one thing and ends: the client reconnects, which is what an end means.
async fn events(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if let Err(problem) = fake.check(&headers) {
        return problem.into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        "event: ready\ndata: {}\n\n",
    )
        .into_response()
}

/// The hostile fake: a torrent list that is not JSON, then is, then is far too big; and an event stream of
/// one line with no end in it.
async fn endless_events(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if let Err(problem) = fake.check(&headers) {
        return problem.into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        "event: ready\n".to_string() + &"a".repeat(1024 * 1024 + 8),
    )
        .into_response()
}

/// `POST /api/v1/auth/session`: the one password login this fake knows, answered with a session token and
/// how long it lives. It counts every attempt - right or wrong - because counting those is how a test shows
/// a client is retrying on a ladder rather than hammering a refused password.
async fn session_login(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    // The gateway's door stands in front of this route as well: a key-auth gateway that only covered the
    // reading routes would let a login through without ever having checked its key. A refusal here is not a
    // refused password, so nothing is counted for it.
    if let Err(problem) = fake.gate(&headers) {
        return problem.into_response();
    }
    fake.logins.fetch_add(1, Ordering::SeqCst);
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    // Both base64 spellings are accepted, the same way `Fake::check` accepts them for a gateway: this route
    // is standing in for a server's, and it should not care how a client encoded the pair.
    let decoded = match given.strip_prefix("Basic ") {
        Some(rest) => BASE64_URL_SAFE_NO_PAD
            .decode(rest)
            .or_else(|_| base64::prelude::BASE64_STANDARD.decode(rest))
            .unwrap_or_default(),
        None => Vec::new(),
    };
    if String::from_utf8_lossy(&decoded).as_ref() != "alice:hunter2" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {"code": "unauthorized", "message": "sign-in refused: wrong password"}
            })),
        )
            .into_response();
    }
    let number = fake.issued.fetch_add(1, Ordering::SeqCst);
    let token = format!("session-{number}");
    *fake.session.lock().expect("the test state") = token.clone();
    // A login that worked means sessions are being served again, whatever a test revoked before it.
    fake.revoked.store(false, Ordering::SeqCst);
    (
        StatusCode::CREATED,
        Json(json!({ "token": token, "expires_in": 3_600 })),
    )
        .into_response()
}

/// The event stream for the tests that count: it records the dial - refused ones included, because a client
/// dialling at all is what those tests are about - then answers one frame and ends.
async fn counted_events(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    fake.event_dials.fetch_add(1, Ordering::SeqCst);
    if let Err(problem) = fake.check(&headers) {
        return problem.into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        "event: ready\ndata: {}\n\n",
    )
        .into_response()
}

/// How many frames `burst_events` writes at once: thousands, so that a client which woke once per event
/// would re-read the torrent list thousands of times if wakes were coalesced into one.
const EVENT_BURST: usize = 10_000;

/// A stream that answers its first call with [`EVENT_BURST`] frames at once and every later call with
/// nothing at all - not even headers, which is what a server with nothing to say looks like to this client.
/// The burst flag says when the frames are behind the server, so a test can measure what happened after them.
async fn burst_events(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    fake.event_dials.fetch_add(1, Ordering::SeqCst);
    if let Err(problem) = fake.check(&headers) {
        return problem.into_response();
    }
    // `compare_exchange` rather than read-and-set: a reconnect can overlap the first dial, and only one of
    // them is the first.
    if fake
        .burst_written
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        let mut body = String::with_capacity(EVENT_BURST * 32);
        for n in 0..EVENT_BURST {
            body.push_str(&format!("event: torrent-changed\ndata: {{\"n\":{n}}}\n\n"));
        }
        return ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response();
    }
    tokio::time::sleep(Duration::from_secs(3_600)).await;
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_server_is_bounded() {
    let fake = Fake::open();
    let addr = serve(api_routes(get(endless_events)).with_state(Arc::clone(&fake))).await;

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::None, None, None),
        Secret::None,
        None,
    );
    // The three rude answers land on different polls - this fake serves them in order, one per call - so
    // the test waits for all three lines instead of guessing how many polls a moment covers.
    let lines = notices_until(&mut client, |lines| {
        lines.iter().any(|line| line.contains("could not read"))
            && lines.iter().any(|line| line.contains("16 MiB"))
            && lines
                .iter()
                .any(|line| line.contains("event stream") && line.contains("longer than"))
    })
    .await;

    assert!(
        lines.iter().any(|line| line.contains("could not read")),
        "a body that is not JSON says so: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("16 MiB")),
        "a body over the cap says so: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("event stream") && line.contains("longer than")),
        "an endless event line says so: {lines:?}"
    );

    // None of it took the client out of service: the link stayed up (a rude answer is an answer) and the
    // screen can still ask for things.
    let snapshot = wait_for_state(&mut client, true).await;
    assert_eq!(snapshot.totals.down_rate, 0);
    assert!(client.send(Command::ListDirs(None)));
    let dirs = wait_for(&mut client, |snapshot| snapshot.dirs.clone()).await;
    let DirListing { roots, .. } = dirs;
    assert_eq!(roots, vec![PathBuf::from(&fake.root)]);

    shutdown(&mut client).await;
}

/// A refused password is retried on the backoff ladder rather than hammered, and the event stream adds no
/// logins of its own to that: it waits for the refresh loop to have a session instead of trying to earn one,
/// so while the link is down it asks this server for nothing at all. An Argon2 verify is the expensive part
/// of a login attempt, which is why the ladder exists.
#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_does_not_hammer_login() {
    let fake = Fake::session();
    let addr = serve(
        api_routes(get(counted_events))
            .route("/api/v1/auth/session", post(session_login))
            .with_state(Arc::clone(&fake)),
    )
    .await;

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::Password, Some("alice"), None),
        Secret::Password("not the password".to_string().into()),
        None,
    );
    // Counting starts once the first refusal is in: counting from before it would be measuring how long a
    // login takes rather than how often one is attempted.
    notices_until(&mut client, |lines| {
        lines
            .iter()
            .any(|line| line.contains("sign-in was refused"))
    })
    .await;

    let first = fake.logins.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let attempts = fake.logins.load(Ordering::SeqCst) - first;
    // The ladder's rungs inside five seconds are one, two and four: three more attempts, with room for a
    // fourth that was already on its way when the count started. A client that retried once a second would
    // be at five here, and one that retried twice a second - a login per stream dial as well - over ten.
    assert!(
        (1..=4).contains(&attempts),
        "five seconds of refused logins cost {attempts} attempts; the ladder allows four"
    );
    let dials = fake.event_dials.load(Ordering::SeqCst);
    assert_eq!(
        dials, 0,
        "the event stream dialled {dials} times while there was no session to send"
    );

    shutdown(&mut client).await;
}

/// A session that dies between two polls is renewed inside the call that found it dead: one further login,
/// the same link, and no lost connection. That is what an hour-old token on a server that restarted behind it
/// looks like to a person watching a download, and it should look like nothing happening.
#[tokio::test(flavor = "multi_thread")]
async fn mid_session_401_relogs_once() {
    let fake = Fake::session();
    let addr = serve(
        api_routes(get(counted_events))
            .route("/api/v1/auth/session", post(session_login))
            .with_state(Arc::clone(&fake)),
    )
    .await;

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::Password, Some("alice"), None),
        Secret::Password("hunter2".to_string().into()),
        None,
    );
    wait_for_state(&mut client, true).await;
    assert_eq!(
        fake.logins.load(Ordering::SeqCst),
        1,
        "one login got the session"
    );
    let first = fake.session_token();

    // The server ends this client's session without telling it: what a restart behind a long-lived token
    // does, and what signing this device out from another screen does.
    fake.revoked.store(true, Ordering::SeqCst);

    // The next round finds out and logs in again within itself, and its retry carries the new session.
    // The wait is on that retry having arrived, not on the login count: the fake counts a login as it
    // starts answering it, and the link says `Connected` all along (a renewal that worked is not trouble),
    // so "two logins and connected" can be true before the retry with the new token has reached this
    // server at all.
    let renewed = wait_for(&mut client, |snapshot| {
        let renewed = fake.session_token();
        let used = renewed != first
            && fake
                .seen
                .lock()
                .expect("the test state")
                .contains(&format!("Bearer {renewed}"));
        (used
            && matches!(
                snapshot.connection,
                Connection::Remote {
                    state: LinkState::Connected,
                    ..
                }
            ))
        .then_some(renewed)
    })
    .await;

    // Both tokens were sent, the second one is what it sends now, and one login earned it.
    let seen = fake.seen.lock().expect("the test state").clone();
    assert!(
        seen.contains(&format!("Bearer {first}")),
        "the dead session was tried: {seen:?}"
    );
    assert_eq!(
        fake.logins.load(Ordering::SeqCst),
        2,
        "one refused session cost one login: {seen:?}"
    );
    assert_eq!(
        fake.session_token(),
        renewed,
        "and the credential it earned is the one in use"
    );

    // And nothing about this was reported as a link going down and coming back: the line for that is only
    // ever said after a `Lost`, and there was none.
    let lines = notices_so_far(&mut client).await;
    assert!(
        !lines.iter().any(|line| line.contains("again")),
        "a renewal is not a reconnect: {lines:?}"
    );

    shutdown(&mut client).await;
}

/// Ten thousand events arriving while reads are under way become one further round of reads, not ten
/// thousand: the stream's wake is a permit rather than a message, so it cannot queue up behind itself. What
/// is measured is the quiet afterwards - the ordinary once-a-second cadence, with no backlog to work off.
#[tokio::test(flavor = "multi_thread")]
async fn a_burst_of_events_costs_one_refresh() {
    let fake = Fake::plain();
    let addr = serve(api_routes(get(burst_events)).with_state(Arc::clone(&fake))).await;

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::None, None, None),
        Secret::None,
        None,
    );
    wait_for_state(&mut client, true).await;

    // The frames are behind the server, and a moment to let this client have read them: over loopback a few
    // hundred kilobytes is milliseconds, and the stream reads as it arrives rather than at the end.
    within(async {
        while !fake.burst_written.load(Ordering::SeqCst) {
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let before = fake.torrents_calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(4)).await;
    let reads = fake.torrents_calls.load(Ordering::SeqCst) - before;
    assert!(
        reads <= 8,
        "four quiet seconds after the burst cost {reads} reads: the cadence is one a second, and wakes that \
         queued one per event would still be working off thousands here"
    );

    shutdown(&mut client).await;
}

/// A `private_key_jwt` assertion is checked the way RFC 7523 says it has to be: the signature against the
/// device's public key, and the claims a gateway relies on. The token it trades for is then sent on every
/// request, the event stream included.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_key_gets_and_uses_token() {
    let key = keypair();
    let public: PublicKey = key.public_key().clone();
    let fake = Fake::gateway(public.clone(), 3_600);
    let addr = serve(
        api_routes(get(events))
            .route("/token", post(token))
            .with_state(Arc::clone(&fake)),
    )
    .await;

    let pem = key.to_openssh(LineEnding::LF).expect("the key file");
    // The key lives in a scratch directory rather than loose in the temp folder: it is a private key, and a
    // test that fails halfway through should still take it with it.
    let dir = ScratchDir::new("gateway_key_token");
    let key_file = write_file(dir.path(), "id_ed25519", pem.as_bytes());
    let mut remote = target(addr, LoginMethod::GatewayKey, None, Some(key_file.clone()));
    remote.token_url = Some(format!("http://{addr}/token"));
    remote.client_id = Some(fake.client_id.clone());

    let mut client = RemoteBackend::start(remote, Secret::KeyPassphrase(None), None);
    wait_for_state(&mut client, true).await;

    let assertions = fake.assertions.lock().expect("the test state").clone();
    assert!(
        !assertions.is_empty(),
        "the assertion was posted to the token endpoint"
    );
    // The token endpoint verified the signature and the claims before it issued; a wrong one is a panic in
    // that handler, which this test would see as a 400 and no Connected.
    let seen = fake.seen.lock().expect("the test state").clone();
    assert!(
        seen.iter().all(|line| line.starts_with("Bearer token-")),
        "every API request carried the issued token: {seen:?}"
    );
    assert!(seen.len() > 1, "the event stream carried it too: {seen:?}");

    shutdown(&mut client).await;
}

/// A gateway that hands out ECDSA client keys gets an assertion it can actually check: the header says
/// `ES256`, and the signature is the two scalars at their fixed width rather than OpenSSH's mpints. The fake
/// verifies it with the `p256` crate, which is what a provider does - ssh-key would have taken either
/// spelling, so this is the test that proves the conversion went the right way.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_key_es256_gets_token() {
    let key = ecdsa_keypair();
    let public: PublicKey = key.public_key().clone();
    assert!(
        public.key_data().ecdsa().is_some(),
        "the fixture is a P-256 key"
    );
    let fake = Fake::gateway(public.clone(), 3_600);
    let addr = serve(
        api_routes(get(events))
            .route("/token", post(token))
            .with_state(Arc::clone(&fake)),
    )
    .await;

    let pem = key.to_openssh(LineEnding::LF).expect("the key file");
    // A scratch directory, as with the Ed25519 key: it is a private key, and a test that fails halfway
    // through should still take it with it.
    let dir = ScratchDir::new("gateway_key_es256");
    let key_file = write_file(dir.path(), "id_ecdsa_p256", pem.as_bytes());
    let mut remote = target(addr, LoginMethod::GatewayKey, None, Some(key_file.clone()));
    remote.token_url = Some(format!("http://{addr}/token"));
    remote.client_id = Some(fake.client_id.clone());

    let mut client = RemoteBackend::start(remote, Secret::KeyPassphrase(None), None);
    wait_for_state(&mut client, true).await;

    // The endpoint checked the signature before it issued anything, so what reached it is worth reading back:
    // a header naming the wrong algorithm, or an mpint pair where scalars belong, would have been a refusal
    // and no Connected rather than something this test gets to look at.
    let assertions = fake.assertions.lock().expect("the test state").clone();
    let Some(assertion) = assertions.first() else {
        panic!("the assertion never reached the token endpoint");
    };
    let (unsigned, signature) = assertion.rsplit_once('.').expect("a signed JWT");
    let header = BASE64_URL_SAFE_NO_PAD
        .decode(unsigned.split_once('.').expect("a header and claims").0)
        .expect("a base64url header");
    let header: Value = serde_json::from_slice(&header).expect("the header is JSON");
    assert_eq!(
        header.get("alg").and_then(Value::as_str),
        Some("ES256"),
        "a P-256 key signs ES256, and the gateway is told so before it checks anything"
    );
    let raw = BASE64_URL_SAFE_NO_PAD
        .decode(signature)
        .expect("a base64url signature");
    assert_eq!(raw.len(), 64, "the two scalars travel at their fixed width");
    // The check the endpoint ran is not a rubber stamp: the same bytes verify against this key, and one bit
    // changed in `r` does not. Without the first of these two, "no refusal" would only mean the fake said yes
    // to everything.
    assert!(
        verify_es256(&public, unsigned.as_bytes(), &raw),
        "the assertion verifies against the key it names"
    );
    let mut tampered = raw.clone();
    tampered[16] ^= 0x80;
    assert!(
        !verify_es256(&public, unsigned.as_bytes(), &tampered),
        "a signature is not a claim anybody can edit"
    );

    // And the token that trade bought is carried on every request afterwards, the event stream included.
    let seen = fake.seen.lock().expect("the test state").clone();
    assert!(
        seen.iter().all(|line| line.starts_with("Bearer token-")),
        "every API request carried the issued token: {seen:?}"
    );
    assert!(seen.len() > 1, "the event stream carried it too: {seen:?}");

    shutdown(&mut client).await;
}

/// A credential is replaced before it expires rather than after - and not on every call. An eight-second
/// token renews at four seconds (half its life: see `fbtt_core`'s gateway module), so within seconds a second
/// token appears, and the first one carries many requests on the way there.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_token_refreshed_before_expiry() {
    let key = keypair();
    let public: PublicKey = key.public_key().clone();
    let fake = Fake::gateway(public, 8);
    let addr = serve(
        api_routes(get(events))
            .route("/token", post(token))
            .with_state(Arc::clone(&fake)),
    )
    .await;

    let pem = key.to_openssh(LineEnding::LF).expect("the key file");
    let dir = ScratchDir::new("gateway_token_ttl");
    let key_file = write_file(dir.path(), "id_ed25519", pem.as_bytes());
    let mut remote = target(addr, LoginMethod::GatewayKey, None, Some(key_file.clone()));
    remote.token_url = Some(format!("http://{addr}/token"));
    remote.client_id = Some(fake.client_id.clone());

    let mut client = RemoteBackend::start(remote, Secret::KeyPassphrase(None), None);
    wait_for_state(&mut client, true).await;

    // Two different tokens on two requests means the first was replaced while it was still valid: a client
    // that waited for a 401 would have had to be refused first.
    let second = within(async {
        loop {
            let seen = fake.seen.lock().expect("the test state").clone();
            if let Some(index) = (1..seen.len()).find(|index| seen[*index] != seen[0]) {
                break seen[index].clone();
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    assert!(second.starts_with("Bearer token-"), "{second}");
    let issued = fake.issued.load(Ordering::SeqCst);
    assert_eq!(
        issued, 2,
        "two assertions, two tokens: the first was renewed, not refused"
    );

    // And the renewal is on a clock rather than per request: one token served a run of polls, which is what
    // a short-lived credential needs or every second of this client's life would be a signature and a POST.
    let seen = fake.seen.lock().expect("the test state").clone();
    let shared = seen.iter().filter(|line| **line == seen[0]).count();
    assert!(
        shared >= 3,
        "one token carried {shared} requests before it was replaced: {seen:?}"
    );

    shutdown(&mut client).await;
}

/// A gateway that takes the password on every request gets it on every request: no login call, and no
/// request sent without one.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_basic_sent_on_every_request() {
    let fake = Fake::basic("alice", "hunter2");
    let addr = serve(api_routes(get(events)).with_state(Arc::clone(&fake))).await;

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::GatewayBasic, Some("alice"), None),
        Secret::Password("hunter2".to_string().into()),
        None,
    );
    wait_for_state(&mut client, true).await;

    let seen = fake.seen.lock().expect("the test state").clone();
    assert!(seen.len() > 1, "more than one request went out: {seen:?}");
    assert!(
        seen.iter().all(|line| line.starts_with("Basic ")),
        "every request carried the password: {seen:?}"
    );
    let Connection::Remote { auth, .. } = &client.snapshot().connection else {
        panic!("a gateway target says how it signed in");
    };
    assert_eq!(auth, "gateway password");

    shutdown(&mut client).await;
}

/// Setup 2 from `docs/reverse-proxy.md`: a gateway checking a key of its own in front of a featherbittorrent
/// server whose login is a password. Both halves have to arrive on every request this client makes - the key
/// under the header that gateway reads it from, and this client's own bearer under `Authorization` - and the
/// login call is one of those requests, because the gateway stands in front of it too.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_key_header_sent_with_password_login() {
    const HEADER: &str = "X-API-Key";
    const KEY: &str = "gateway-secret";
    let fake = Fake::keyed_session(HEADER, KEY);
    let addr = serve(
        api_routes(get(counted_events))
            .route("/api/v1/auth/session", post(session_login))
            .with_state(Arc::clone(&fake)),
    )
    .await;

    let mut client = RemoteBackend::start(
        RemoteTarget {
            // Only the name is what `ui.yaml` keeps for a target like this; the value comes out of the
            // keychain, and this argument is what that hands over (see `featherbittorrent`'s secrets).
            gateway_key_header: Some(HEADER.to_string()),
            ..target(addr, LoginMethod::Password, Some("alice"), None)
        },
        Secret::Password("hunter2".to_string().into()),
        Some(KEY.to_string().into()),
    );

    let connected = wait_for_state(&mut client, true).await;
    let Connection::Remote { auth, .. } = &connected.connection else {
        panic!("a remote backend says how it signed in");
    };
    assert_eq!(
        auth, "password",
        "a gateway key changed which login this connection is"
    );

    // The stream is a request like every other one here, and the last thing to go out after a sign-in:
    // waiting for its dial is what makes "every request" cover it rather than cover whatever happened to be
    // queued when the link came up.
    within(async {
        while fake.event_dials.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(POLL).await;
        }
    })
    .await;

    // The login itself went through this front door: the fake answers 403 before it looks at a password, so
    // a client that forgot the header would have no session to be `Connected` on at all.
    assert_eq!(
        fake.logins.load(Ordering::SeqCst),
        1,
        "one login earned the session"
    );

    // Everything after it carried the key too. The value is recorded once per request, refusals included, so
    // an empty entry is a request that went out without one and a short list is a front door only some of
    // the requests were dialled through.
    let keys = fake.gateway_keys.lock().expect("the test state").clone();
    assert!(
        keys.len() >= 4,
        "the header arrived at the login and hardly anywhere else: {keys:?}"
    );
    assert!(
        keys.iter().all(|value| value == KEY),
        "a request went out without the gateway key: {keys:?}"
    );

    // Neither half stands in for the other. This fake's own bearer, with no header in front of it:
    let token = fake.session_token();
    let roots = format!("http://{addr}/api/v1/fs/roots");
    let alone = within(http().get(&roots).bearer_auth(&token).send())
        .await
        .expect("the request reached the fake");
    assert_eq!(
        alone.status(),
        StatusCode::FORBIDDEN,
        "the bearer alone does not open this door"
    );

    // and the key alone, which is a gateway's credential and not this server's:
    let ours = within(http().get(&roots).header(HEADER, KEY).send())
        .await
        .expect("the request reached the fake");
    assert_eq!(
        ours.status(),
        StatusCode::UNAUTHORIZED,
        "a gateway key was taken for a featherbittorrent session"
    );

    // and the two together, which is what this client sends.
    let both = within(
        http()
            .get(&roots)
            .bearer_auth(&token)
            .header(HEADER, KEY)
            .send(),
    )
    .await
    .expect("the request reached the fake");
    assert!(
        both.status().is_success(),
        "the two halves together did not open the door: {}",
        both.status()
    );

    shutdown(&mut client).await;
}

/// The other combination setup 2 allows: a server behind a gateway checking an API key, signing in to a
/// provider on another address with an assertion of its own. A key that opens this server says nothing to
/// that provider - it has never seen this client's server, and to the person who set the key up it is
/// somebody else's host - so the token request goes out carrying only what it signed, while every request
/// that reaches this server still carries the key.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_key_not_sent_to_token_endpoint() {
    const HEADER: &str = "X-API-Key";
    const KEY: &str = "gateway-secret";
    let key = keypair();
    let public: PublicKey = key.public_key().clone();
    let fake = Fake::keyed_gateway(public, 3_600, HEADER, KEY);

    // Two addresses on purpose. Served from one host, "the key did not follow the assertion" and "there was
    // no separate door to tell them apart" are the same observation; served from two, only the first of them
    // can be true, which is what makes the assertion below worth anything.
    let provider = serve(
        Router::new()
            .route("/token", post(token))
            .with_state(Arc::clone(&fake)),
    )
    .await;
    let addr = serve(api_routes(get(counted_events)).with_state(Arc::clone(&fake))).await;

    let pem = key.to_openssh(LineEnding::LF).expect("the key file");
    let dir = ScratchDir::new("gateway_key_token_origin");
    let key_file = write_file(dir.path(), "id_ed25519", pem.as_bytes());
    let mut remote = target(addr, LoginMethod::GatewayKey, None, Some(key_file));
    remote.token_url = Some(format!("http://{provider}/token"));
    remote.client_id = Some(fake.client_id.clone());
    remote.gateway_key_header = Some(HEADER.to_string());

    let mut client = RemoteBackend::start(
        remote,
        Secret::KeyPassphrase(None),
        Some(KEY.to_string().into()),
    );
    wait_for_state(&mut client, true).await;

    // The stream is a request like every other one here, and the last thing to go out once a token is in
    // hand: waiting for its dial is what makes "every request" below cover it rather than whatever happened
    // to be queued when the link came up.
    within(async {
        while fake.event_dials.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(POLL).await;
        }
    })
    .await;

    // The provider was dialled, and dialled as a provider: the assertion it is there to check arrived, and
    // this connection's server-side key did not.
    let assertions = fake.assertions.lock().expect("the test state").clone();
    assert!(
        !assertions.is_empty(),
        "the assertion never reached the token endpoint, so nothing below proves anything"
    );
    let at_the_provider = fake
        .token_gateway_keys
        .lock()
        .expect("the test state")
        .clone();
    assert!(
        at_the_provider.is_empty(),
        "the gateway key went out to the token endpoint, which is another host's: {at_the_provider:?}"
    );

    // It did its job here, though: every request that reached this server carried it - the event stream
    // included, because its dial is what was just waited for - and what each of them signed with was the
    // token the provider issued without it.
    let keys = fake.gateway_keys.lock().expect("the test state").clone();
    assert!(
        keys.len() > 1,
        "hardly any request came through this server's front door: {keys:?}"
    );
    assert!(
        keys.iter().all(|value| value == KEY),
        "a request went out without the gateway key: {keys:?}"
    );
    let seen = fake.seen.lock().expect("the test state").clone();
    assert!(
        seen.iter().all(|line| line.starts_with("Bearer token-")),
        "an API request did not carry the token the provider issued: {seen:?}"
    );

    // And the recorder is not blind, which is what makes the empty list above an answer rather than an
    // artefact of nothing ever being recorded: a request that does bring the key is noticed. The endpoint
    // refuses this one because it carries no assertion to check - printed as it always prints a refusal -
    // and noting the header happens before any of that.
    let probe = within(
        http()
            .post(format!("http://{provider}/token"))
            .header(HEADER, KEY)
            .form(&[("grant_type", "client_credentials")])
            .send(),
    )
    .await
    .expect("the request reached the provider");
    assert_eq!(
        probe.status(),
        StatusCode::BAD_REQUEST,
        "the fixture's endpoint stopped behaving"
    );
    let recorded = fake
        .token_gateway_keys
        .lock()
        .expect("the test state")
        .clone();
    assert_eq!(
        recorded,
        vec![KEY.to_string()],
        "a key that did arrive at the token endpoint was not noticed"
    );

    shutdown(&mut client).await;
}

// ---------------------------------------------------------------------------------------- the token route

/// `POST /token`: one assertion checked, one token issued. The gateway fakes register this on top of
/// [`api_routes`], before that router is given its state. It notes what arrived under the fake's gateway
/// header on the way in (see [`Fake::record_token_gate`]): a provider's endpoint has no reason to be told
/// about a key that opens somebody else's server, and this is how a test finds out whether it was.
async fn token(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    fake.record_token_gate(&headers);
    let form = String::from_utf8_lossy(&body).into_owned();
    let bad = |problem: &str| {
        // A refusal here is the only way these tests find out their own gateway said no, so it is printed
        // as well as answered: without it a failing gateway test sees only a link that went `Lost`.
        eprintln!("the token endpoint refused the assertion: {problem}");
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"code": "invalid_request", "message": problem}})),
        )
            .into_response()
    };
    if form_value(&form, "grant_type").as_deref() != Some("client_credentials") {
        return bad("grant_type must be client_credentials");
    }
    if form_value(&form, "client_assertion_type").as_deref()
        != Some("urn:ietf:params:oauth:client-assertion-type:jwt-bearer")
    {
        return bad("the assertion type is not the jwt-bearer one");
    }
    let Some(assertion) = form_value(&form, "client_assertion") else {
        return bad("no client_assertion");
    };
    let Some((unsigned, signature)) = assertion.rsplit_once('.') else {
        return bad("the assertion is not a JWT");
    };
    let Some((header, claims)) = unsigned.split_once('.') else {
        return bad("the assertion has no header");
    };
    let Ok(header) = BASE64_URL_SAFE_NO_PAD.decode(header) else {
        return bad("the header is not base64url");
    };
    let Ok(claims) = BASE64_URL_SAFE_NO_PAD.decode(claims) else {
        return bad("the claims are not base64url");
    };
    let Ok(signature) = BASE64_URL_SAFE_NO_PAD.decode(signature) else {
        return bad("the signature is not base64url");
    };

    // The signature first, because everything else about the assertion is somebody's word until it holds -
    // except the header's `alg`, which is what says how it has to be held.
    let Some(public) = &fake.public_key else {
        return bad("this gateway issues no tokens");
    };
    let Ok(header) = serde_json::from_slice::<Value>(&header) else {
        return bad("the header is not JSON");
    };
    match header.get("alg").and_then(Value::as_str) {
        // `Verifier` and not `PublicKey::verify`: a JWT carries a bare Ed25519 signature rather than an
        // `SSHSIG`, and this is the trait ssh-key implements for exactly that shape.
        Some("EdDSA") => {
            let Ok(signature) = Signature::new(Algorithm::Ed25519, signature) else {
                return bad("the signature is not an Ed25519 one");
            };
            if Verifier::verify(public, unsigned.as_bytes(), &signature).is_err() {
                return bad("the assertion is not signed by the key it names");
            }
        }
        Some("ES256") => {
            if !verify_es256(public, unsigned.as_bytes(), &signature) {
                return bad("the assertion is not an ES256 signature this key made");
            }
        }
        // `alg: none` is in here too, because a gateway that fell through to "no signature needed" would be
        // handing out tokens to anybody who could spell a client id.
        _ => return bad("alg is neither EdDSA nor ES256"),
    }

    let Ok(claims) = serde_json::from_slice::<Value>(&claims) else {
        return bad("the claims are not JSON");
    };
    let claim = |name: &str| claims.get(name).and_then(Value::as_str).unwrap_or_default();
    if header
        .get("kid")
        .and_then(Value::as_str)
        .unwrap_or_default()
        != public.fingerprint(HashAlg::Sha256).to_string()
    {
        return bad("kid is not this key's fingerprint");
    }
    if claim("iss") != fake.client_id || claim("sub") != fake.client_id {
        return bad("iss and sub must be the client id");
    }
    if !claim("aud").ends_with("/token") {
        // The audience is this endpoint's own URL, which carries the port this fake was handed and cannot
        // know from inside a handler: what can be checked here is that the assertion names this endpoint.
        return bad("aud must name this endpoint");
    }
    if claim("jti").is_empty() {
        return bad("a fresh assertion needs a fresh jti");
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock")
        .as_secs();
    if claims.get("exp").and_then(Value::as_u64).unwrap_or(0) <= now {
        return bad("the assertion has expired");
    }
    // Two assertions in the same second must not carry the same id, or a gateway could be shown one twice.
    if fake
        .assertions
        .lock()
        .expect("the test state")
        .iter()
        .any(|earlier| {
            earlier
                .rsplit_once('.')
                .and_then(|(unsigned, _)| unsigned.split_once('.'))
                .and_then(|(_, claims)| BASE64_URL_SAFE_NO_PAD.decode(claims).ok())
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|claims| {
                    claims.get("jti").and_then(Value::as_str) == Some(claim("jti"))
                })
        })
    {
        return bad("this jti has been used already");
    }

    fake.assertions
        .lock()
        .expect("the test state")
        .push(assertion.clone());
    let number = fake.issued.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        Json(json!({
            "access_token": format!("token-{number}"),
            "expires_in": fake.token_ttl,
            "token_type": "bearer",
        })),
    )
        .into_response()
}

/// The source of a `Play` notice, if the notice is one: what a test that wants the
/// address the player would open reads out of the notice rather than of a snapshot.
fn play_source(notice: &Notice) -> Option<fbtt_core::PlaySource> {
    match notice {
        Notice::Play(target) => Some(target.source().clone()),
        _ => None,
    }
}

/// The `PlaySource` a `Play` notice carries: what a test that wants the address the
/// player would open needs out of a notice, panicking if the notice is not a play.
fn source_of(notice: Notice) -> fbtt_core::PlaySource {
    match notice {
        Notice::Play(target) => target.source().clone(),
        other => panic!("a play notice was asked for, and {other:?} is not one"),
    }
}

/// The same routes every fake answers, with a token route that is rude on purpose: the
/// first answer is not JSON of the shape this client reads, the second is a body that
/// is over the cap. What a test asks of it is the one line the client says for each,
/// and nothing more: a `Play` that came out of either would be a player opening what
/// the server said it was not.
fn api_routes_with_rude_tokens(stream: MethodRouter<Arc<Fake>>) -> Router<Arc<Fake>> {
    api_routes(stream)
        .route("/api/v1/torrents/{id}/files", get(file_list))
        .route("/api/v1/stream-tokens", post(rude_token_route))
}

/// The file list the stream test's torrent has: the entries the test wrote, one per line as JSON. The
/// token route is rude either way, so the list is only there for the choice to be made from.
async fn file_list(State(fake): State<Arc<Fake>>, _headers: HeaderMap) -> Response {
    fake.files_calls.fetch_add(1, Ordering::SeqCst);
    let files = fake.files.lock().expect("the test state").clone();
    let body = files.join(",");
    Json(serde_json::from_str::<Vec<serde_json::Value>>(&body).unwrap_or_default()).into_response()
}

/// `POST /api/v1/stream-tokens`, rude in the two ways the test asks of it: not JSON on
/// the first call, over the cap on the second.
async fn rude_token_route() -> Response {
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    match CALLS.fetch_add(1, Ordering::SeqCst) {
        0 => (
            [(header::CONTENT_TYPE, "application/json")],
            "{ \"not\": [\"a stream answer\"",
        )
            .into_response(),
        _ => {
            // 17 MiB crosses the 16 MiB cap this client reads: the test proves the
            // cap is enforced, and the over-cap answer costs the client a line, not
            // its memory.
            let big = vec![b'x'; 17 * 1024 * 1024];
            (
                [(header::CONTENT_TYPE, "application/json")],
                String::from_utf8(big).expect("ascii"),
            )
                .into_response()
        }
    }
}

/// A password login, a torrent seeded into it, and a stream URL that plays a range of
/// it: the whole remote path a player uses, from the command to the bytes.
#[tokio::test(flavor = "multi_thread")]
async fn remote_stream_url_plays_range() {
    let lib = Library::new("remote_stream_range", vec![alice()], vec![]);
    let (server, addr) = lib.serve().await;

    // The file in the server's own download folder: the server's copy is what the
    // stream route serves, and a torrent that already has its pieces on disk
    // answers a read from storage once its check finishes.
    let bytes = torrent_for(&write_file(
        lib.dir.path().join("downloads").as_path(),
        "payload.bin",
        &sample(MIB),
    ))
    .await;
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    let id = id_of(&metainfo);

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::Password, Some("alice"), None),
        Secret::Password("hunter2".to_string().into()),
        None,
    );
    wait_for_state(&mut client, true).await;
    assert!(client.send(Command::Add {
        source: AddSource::File(bytes),
        dir: None,
    }));
    wait_for(&mut client, |snapshot| {
        (!snapshot.torrents.is_empty()).then_some(())
    })
    .await;

    // The command the screen sends, and the answer a player gets: a URL on the server,
    // with its token, and the range a player's first request asks for.
    assert!(client.send(Command::Stream { id, file: None }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = source else {
        panic!("a remote stream is a URL, not a path");
    };
    let url = url.as_str();
    assert!(
        url.starts_with(&format!("http://{addr}")),
        "the stream URL is the server's own: {url}"
    );
    assert!(url.contains("token="), "the stream URL carries its token");
    assert!(
        url.contains("/files/0/stream"),
        "the stream URL names the file it was issued for: {url}"
    );

    // The player's first request: a range of the file it was handed.
    let first = within(http().get(url).header("Range", "bytes=0-15").send())
        .await
        .expect("the stream answered");
    assert_eq!(
        first.status(),
        StatusCode::PARTIAL_CONTENT,
        "a range is a range: {}",
        first.status()
    );
    let body = within(first.bytes()).await.expect("the range arrived");
    let wanted = sample(MIB);
    assert_eq!(
        body.as_ref(),
        &wanted[0..16],
        "the bytes are the file's, from the server's copy"
    );

    shutdown(&mut client).await;
    within(server.shutdown()).await.expect("the last save ran");
}

/// `file: None` plays the largest non-pad file: the server's own list is what the
/// choice is made from, and the index the URL names is the larger of the two.
#[tokio::test(flavor = "multi_thread")]
async fn remote_stream_picks_largest_file() {
    const ID: &str = "0123456789abcdef0123456789abcdef01234567";
    const STREAM: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/2/stream?token=t";
    // Three files in the list, the last one the largest, a pad between the other two:
    // the choice is the last, and the answer the token route gives names it.
    let files_calls = Arc::new(AtomicUsize::new(0));
    let files = [
        r#"{"index":0,"path":"small.bin","len":65536,"pad":false}"#.to_string(),
        r#"{"index":1,"path":"pad","len":512,"pad":true}"#.to_string(),
        r#"{"index":2,"path":"large.bin","len":262144,"pad":false}"#.to_string(),
    ];
    let files_body = format!("[{}]", files.join(","));
    let upstream = Router::new()
        .route(
            "/api/v1/fs/roots",
            get(|| async { Json(vec!["/tmp".to_string()]) }),
        )
        .route(
            "/api/v1/torrents",
            get(|| async { Json(Vec::<Value>::new()) }),
        )
        .route(
            "/api/v1/session",
            get(|| async { Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})) }),
        )
        .route(
            "/api/v1/session/queue",
            get(|| async {
                Json(json!({
                    "active_downloads": 1,
                    "active_seeds": null,
                    "active_limit": null,
                    "dont_count_slow_torrents": true,
                    "inactive_down_rate": 1024,
                    "inactive_up_rate": 1024,
                    "startup_grace": 30.0,
                    "interval": 1.0,
                }))
            }),
        )
        .route(
            "/api/v1/events",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: ready\ndata: {}\n\n",
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/{id}/files",
            get({
                let calls = Arc::clone(&files_calls);
                let body = files_body.clone();
                move |id: axum::extract::Path<String>| async move {
                    let _ = id;
                    calls.fetch_add(1, Ordering::SeqCst);
                    ([(header::CONTENT_TYPE, "application/json")], body.clone()).into_response()
                }
            }),
        )
        .route(
            "/api/v1/stream-tokens",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({ "path": STREAM, "expires_in": 60 })),
                )
                    .into_response()
            }),
        );
    let addr = serve(upstream).await;

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::None, None, None),
        Secret::None,
        None,
    );
    wait_for_state(&mut client, true).await;

    assert!(client.send(Command::Stream {
        id: TorrentId(ID.to_string()),
        file: None
    }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("a remote stream is a URL, not a path: {source:?}");
    };
    // The largest non-pad file is index 2: the URL the server answered names it, and the
    // choice was made from the server's list, not from the token's answer.
    assert!(
        url.as_str().contains("/files/2/stream"),
        "the larger file is the one the URL names: {}",
        url.as_str()
    );
    // The file list was asked for: the choice came from the server's list.
    assert!(
        files_calls.load(Ordering::SeqCst) > 0,
        "the file list was read before the choice was made"
    );

    shutdown(&mut client).await;
}

/// A hostile file list: the server's `index` field does not line up with the list's
/// positions. The choice looks up by `index` field, not by position, so a list with
/// `[{"index":7,...}]` does not panic. The result is one error notice, not a dead
/// backend task.
#[tokio::test(flavor = "multi_thread")]
async fn hostile_file_list_does_not_panic() {
    const ID: &str = "0123456789abcdef0123456789abcdef01234567";
    const STREAM: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/7/stream?token=t";
    // A one-element file list whose `index` field is 7: the server's own value, not
    // the position. The choice looks up by `index` field and finds it.
    let files_body = r#"[{"index":7,"path":"a.bin","len":10,"pad":false}]"#;
    let upstream = Router::new()
        .route(
            "/api/v1/fs/roots",
            get(|| async { Json(vec!["/tmp".to_string()]) }),
        )
        .route(
            "/api/v1/torrents",
            get(|| async { Json(Vec::<Value>::new()) }),
        )
        .route(
            "/api/v1/session",
            get(|| async { Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})) }),
        )
        .route(
            "/api/v1/session/queue",
            get(|| async {
                Json(json!({
                    "active_downloads": 1,
                    "active_seeds": null,
                    "active_limit": null,
                    "dont_count_slow_torrents": true,
                    "inactive_down_rate": 1024,
                    "inactive_up_rate": 1024,
                    "startup_grace": 30.0,
                    "interval": 1.0,
                }))
            }),
        )
        .route(
            "/api/v1/events",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: ready\ndata: {}\n\n",
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/{id}/files",
            get({
                let body = files_body.to_string();
                move |_id: axum::extract::Path<String>| async move {
                    ([(header::CONTENT_TYPE, "application/json")], body.clone()).into_response()
                }
            }),
        )
        .route(
            "/api/v1/stream-tokens",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({ "path": STREAM, "expires_in": 60 })),
                )
                    .into_response()
            }),
        );
    let addr = serve(upstream).await;

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::None, None, None),
        Secret::None,
        None,
    );
    wait_for_state(&mut client, true).await;

    // The stream command: the file list has one file at `index` 7, the token route
    // answers with the matching path. The choice looks up by `index` field and finds
    // it; the URL the server answered names it. No panic.
    assert!(client.send(Command::Stream {
        id: TorrentId(ID.to_string()),
        file: None
    }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("a remote stream is a URL, not a path: {source:?}");
    };
    // The file at `index` 7 is the one the URL names: the choice was made by the
    // `index` field, not by position.
    assert!(
        url.as_str().contains("/files/7/stream"),
        "the file at index 7 is the one the URL names: {}",
        url.as_str()
    );

    shutdown(&mut client).await;
}

/// A gateway that checks `Authorization: Basic` in front of a server with auth off:
/// the stream URL a player gets is the relay's, not the server's - a player cannot
/// send the gateway's credential, and the relay is the thing that does.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_stream_goes_through_relay() {
    const USER: &str = "gw";
    const PASSWORD: &str = "gw-pass";
    // The credential as the gateway will see it: standard base64 of `user:password`.
    let basic = {
        use base64::Engine as _;
        format!(
            "Basic {}",
            base64::prelude::BASE64_STANDARD.encode("gw:gw-pass")
        )
    };

    // The server: auth off, so the only thing between a request and the stream is the
    // gateway in front of it.
    let lib = Library::new("remote_stream_gateway", vec![], vec![]);
    let (server, server_addr) = lib.serve_with_auth_off().await;

    // The file in the server's own download folder, so its copy is what the stream route serves.
    let bytes = torrent_for(&write_file(
        lib.dir.path().join("downloads").as_path(),
        "payload.bin",
        &sample(MIB),
    ))
    .await;

    // The gateway: a proxy that refuses anything without the credential, and records
    // what it saw, so the test can answer "did the relay send it?"
    let saw = Arc::new(Mutex::new(Vec::new()));
    let gateway = {
        let saw = Arc::clone(&saw);
        let forward_to = format!("http://{server_addr}");
        let expected = basic.clone();
        Router::new().fallback(move |request: axum::extract::Request| {
            let saw = Arc::clone(&saw);
            let forward_to = forward_to.clone();
            let expected = expected.clone();
            async move {
                let authorization = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                saw.lock()
                    .expect("the test state")
                    .push(authorization.clone());
                if authorization != expected {
                    return (StatusCode::UNAUTHORIZED, "the gateway sees no credential")
                        .into_response();
                }
                // Forward to the server, keeping the method, the path with its query
                // (the stream URL's token is in the query, and it has to go through),
                // and every header except the host and the credential the relay adds:
                // what the server sees is what the relay sent it.
                let method = request.method().clone();
                let uri = request.uri().to_string();
                let mut headers = header::HeaderMap::new();
                for (name, value) in request.headers() {
                    if name.as_str() != header::HOST.as_str()
                        && name.as_str() != header::AUTHORIZATION.as_str()
                    {
                        headers.append(name, value.clone());
                    }
                }
                let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
                    .await
                    .unwrap_or_default();
                let response = within(
                    http()
                        .request(method, format!("{forward_to}{uri}"))
                        .headers(headers)
                        .body(body.to_vec())
                        .send(),
                )
                .await
                .expect("the server answered the relay");
                let status = response.status();
                let body = within(response.bytes()).await.expect("the server's body");
                (status, body).into_response()
            }
        })
    };
    let gateway_addr = serve(gateway).await;

    // The target: the gateway's address, a basic login, and a gateway key header, which is
    // what tells the client this target signs to a gateway: a player cannot send that
    // credential itself, and the relay is the one that does.
    let mut remote = target(gateway_addr, LoginMethod::GatewayBasic, Some(USER), None);
    remote.gateway_key_header = Some("x-gateway".to_string());
    let mut client = RemoteBackend::start(
        remote,
        Secret::Password(PASSWORD.to_string().into()),
        Some("gw-key".to_string().into()),
    );
    wait_for_state(&mut client, true).await;

    // The torrent goes through the gateway too: the add and the token route both need
    // the credential, and both go with the same call the refresh loop makes.
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    let id = id_of(&metainfo);
    assert!(client.send(Command::Add {
        source: AddSource::File(bytes),
        dir: None,
    }));
    wait_for(&mut client, |snapshot| {
        (!snapshot.torrents.is_empty()).then_some(())
    })
    .await;

    assert!(client.send(Command::Stream { id, file: None }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("a remote stream is a URL, not a path: {source:?}");
    };
    let url = url.as_str();
    // The URL is the relay's: loopback, the relay's path, and nothing of the password
    // in it - the credential is a header the relay adds, not a string the player
    // carries.
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "a gateway stream is the relay's URL: {url}"
    );
    assert!(
        url.contains("/relay/"),
        "a gateway stream is the relay's path: {url}"
    );
    assert!(
        !url.contains(PASSWORD) && !url.contains("Basic"),
        "the player's URL carries no credential: {url}"
    );
    assert!(
        !url.contains(&server_addr.to_string()),
        "the player's URL is not the server's: {url}"
    );

    // The player's first request through the relay: a range of the file, and the
    // gateway in front of the server sees the credential the relay added.
    let first = within(http().get(url).header("Range", "bytes=0-15").send())
        .await
        .expect("the relay answered");
    assert_eq!(
        first.status(),
        StatusCode::PARTIAL_CONTENT,
        "the relay served the range: {}",
        first.status()
    );
    let body = within(first.bytes()).await.expect("the range arrived");
    let wanted = sample(MIB);
    assert_eq!(
        body.as_ref(),
        &wanted[0..16],
        "the bytes are the file's, through the relay"
    );
    // The gateway saw the credential on the relay's way through.
    let saw = saw.lock().expect("the test state").clone();
    assert!(
        saw.iter().any(|seen| seen == &basic),
        "the gateway saw the credential the relay sent: {saw:?}"
    );

    shutdown(&mut client).await;
    within(server.shutdown()).await.expect("the last save ran");
}
/// The check comes from the `p256` crate and not from ssh-key on purpose: ssh-key would have accepted the
/// mpint spelling of the same signature, so verifying with it would only prove that one library agrees with
/// itself about how to read its own bytes. What a gateway actually receives is the other spelling, and this
/// is the only way in this tree to say whether that one arrived.
fn verify_es256(public: &PublicKey, unsigned: &[u8], signature: &[u8]) -> bool {
    let Some(ecdsa) = public.key_data().ecdsa() else {
        return false;
    };
    let Ok(key) = p256::ecdsa::VerifyingKey::try_from(ecdsa) else {
        return false;
    };
    // Sixty-four bytes or nothing: `from_slice` is the spelling that says so instead of guessing.
    let Ok(signature) = p256::ecdsa::Signature::from_slice(signature) else {
        return false;
    };
    key.verify(unsigned, &signature).is_ok()
}

/// One value out of an `application/x-www-form-urlencoded` body. This fake parses it by hand because axum's
/// `Form` extractor is a feature nobody in this tree has turned on.
fn form_value(form: &str, name: &str) -> Option<String> {
    form.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| decode_percent(value))
    })
}

/// The percent-decoding a form body needs: `%XX` escapes, and the `+` that stands for a space. A gateway
/// compares the assertion type against its literal spelling, so this fake has to undo what a client's form
/// encoder did to it - which is where `:` turning into `%3A` and `/` turning into `%2F` come from.
fn decode_percent(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escape = match bytes[at] {
            b'+' => Some((b' ', 1)),
            // A `%` with two hex digits behind it stands for one byte. A lone `%`, or one with anything
            // that is not hex after it, is somebody's typo, and the only honest reading of a typo is to
            // take it as it comes.
            b'%' => percent_pair(&bytes[at + 1..]).map(|byte| (byte, 3)),
            byte => Some((byte, 1)),
        };
        match escape {
            Some((byte, used)) => {
                out.push(byte);
                at += used;
            }
            None => {
                out.push(b'%');
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The byte a `%XX` escape at the front of `rest` stands for. Nothing when those two characters are not
/// hex digits, which is what keeps a stray `%` a stray `%`.
fn percent_pair(rest: &[u8]) -> Option<u8> {
    let digits = rest.get(..2)?;
    std::str::from_utf8(digits)
        .ok()
        .and_then(|digits| u8::from_str_radix(digits, 16).ok())
}

// --------------------------------------------------------------------------------- the relay's bound

/// The relay's bound, one part at a time: a request for a host it does not serve is
/// refused; an unknown token is refused; nine concurrent requests against an upstream
/// that holds its body open get eight through and one `503` with a second to wait; a
/// 300-byte `Range` reaches the upstream as no `Range` at all; and a response copies
/// back only the status and five headers.
#[tokio::test(flavor = "multi_thread")]
async fn relay_is_bounded() {
    const ID: &str = "0123456789abcdef0123456789abcdef01234567";
    const STREAM_PATH: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream";
    const STREAM: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream?token=t";

    // The upstream the relay forwards to: it answers the backend's own calls so the
    // connection comes up, and its stream route is what the relay's requests reach.
    // The stream route records what it saw, so "what did the relay send?" has an
    // answer; the `hold` flag is how a test makes it hold a body open.
    let hold = Arc::new(AtomicBool::new(false));
    let hold_clone = Arc::clone(&hold);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen);
    // The held-open bodies: each request that arrives with `hold` set gets its own
    // mpsc channel. The test holds the sending ends in `held_senders`; dropping
    // one of them ends that one body, which is how the permit-release assertion
    // proves the slot is released when the body ends and not earlier.
    let held_senders: Arc<Mutex<Vec<tokio::sync::mpsc::Sender<axum::body::Bytes>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let held_senders_clone = Arc::clone(&held_senders);
    let upstream = Router::new()
        .route(
            "/api/v1/fs/roots",
            get(|| async { Json(vec!["/tmp".to_string()]) }),
        )
        .route(
            "/api/v1/torrents",
            get(|| async { Json(Vec::<Value>::new()) }),
        )
        .route(
            "/api/v1/session",
            get(|| async { Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})) }),
        )
        .route(
            "/api/v1/session/queue",
            get(|| async {
                Json(json!({
                    "active_downloads": 1,
                    "active_seeds": null,
                    "active_limit": null,
                    "dont_count_slow_torrents": true,
                    "inactive_down_rate": 1024,
                    "inactive_up_rate": 1024,
                    "startup_grace": 30.0,
                    "interval": 1.0,
                }))
            }),
        )
        .route(
            "/api/v1/events",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: ready\ndata: {}\n\n",
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/{id}/files",
            get(|| async {
                Json(json!([{"index":0,"path":"payload.bin","len":16,"pad":false}])).into_response()
            }),
        )
        .route(
            "/api/v1/stream-tokens",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({ "path": STREAM, "expires_in": 60 })),
                )
                    .into_response()
            }),
        )
        .route(
            STREAM_PATH,
            get({
                let hold = Arc::clone(&hold_clone);
                let seen = Arc::clone(&seen_clone);
                let held_senders = Arc::clone(&held_senders_clone);
                move |request: axum::extract::Request| {
                    let hold = Arc::clone(&hold);
                    let seen = Arc::clone(&seen);
                    let held_senders = Arc::clone(&held_senders);
                    async move {
                        let range = request
                            .headers()
                            .get(header::RANGE)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string)
                            .unwrap_or_default();
                        seen.lock().expect("the test state").push(range);
                        if hold.load(Ordering::SeqCst) {
                            // A held-open body: one chunk, then the body stays open
                            // until the test drops this request's mpsc sender. The
                            // mpsc channel is per-request, so each body is independent.
                            // Dropping the sender closes the channel, which ends the
                            // body stream, which releases the relay's permit.
                            let (tx, mut rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(1);
                            // Hold the sender: the body stays open until the test
                            // drops it.
                            held_senders.lock().expect("the test state").push(tx);
                            // The first chunk is sent immediately; the body stays
                            // open until the channel is closed (the sender is dropped).
                            let first_chunk = futures::stream::once(async {
                                Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
                                    b"0123456789abcdef",
                                ))
                            });
                            let rest = futures::stream::poll_fn(move |cx| {
                                // `poll_recv` returns `Poll<Option<Bytes>>`; wrap it in
                                // `Ok` to match the first chunk's `Result<Bytes, io::Error>`.
                                match rx.poll_recv(cx) {
                                    std::task::Poll::Ready(Some(chunk)) => {
                                        std::task::Poll::Ready(Some(Ok(chunk)))
                                    }
                                    std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
                                    std::task::Poll::Pending => std::task::Poll::Pending,
                                }
                            });
                            let stream = first_chunk.chain(rest);
                            return (
                                [
                                    (header::CONTENT_TYPE, "application/octet-stream"),
                                    (header::ACCEPT_RANGES, "bytes"),
                                ],
                                axum::body::Body::from_stream(stream),
                            )
                                .into_response();
                        }
                        (
                            [
                                (header::CONTENT_TYPE, "application/octet-stream"),
                                (header::CONTENT_RANGE, "bytes 0-15/16"),
                                (header::ACCEPT_RANGES, "bytes"),
                                (header::CONTENT_DISPOSITION, "inline"),
                                (header::SET_COOKIE, "a=secret"),
                                (header::HeaderName::from_static("x-foo"), "bar"),
                            ],
                            "0123456789abcdef".to_string(),
                        )
                            .into_response()
                    }
                }
            }),
        );
    let addr = serve(upstream).await;

    // The target signs to a gateway: a basic login with a gateway key header, so the
    // client routes streams through the relay rather than handing the player the
    // server's own URL.
    let mut remote = target(addr, LoginMethod::GatewayBasic, Some("relay-user"), None);
    remote.gateway_key_header = Some("x-gw".to_string());
    let mut client = RemoteBackend::start(
        remote,
        Secret::Password("relay-pass".to_string().into()),
        Some("relay-key".to_string().into()),
    );
    wait_for_state(&mut client, true).await;

    // The relay starts on the first `Stream` it is asked for, and its own address is
    // what the player opens: loopback, the relay's path, no token of the server's in
    // it - the relay's token is its own, and it is the only one the relay knows.
    let id = TorrentId(ID.to_string());
    assert!(client.send(Command::Stream { id, file: None }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("the relay's URL: {source:?}");
    };
    let url = url.as_str();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "the relay's URL is loopback: {url}"
    );
    assert!(
        url.contains("/relay/"),
        "the relay's URL is the relay's path: {url}"
    );

    // The relay does not serve a host it does not name: a request for another host is
    // refused before the token is looked up, so it learns nothing about the tokens.
    // The request goes to the relay's own address with a wrong `Host` header.
    let refused_host = within(http().get(url).header("Host", "evil.example").send())
        .await
        .expect("the relay answered the other host");
    assert_eq!(
        refused_host.status(),
        StatusCode::FORBIDDEN,
        "a request for another host is refused: {}",
        refused_host.status()
    );

    // An unknown token is refused before a slot is taken: a stranger's guesses do not
    // count against the bound, and a `401` is what a player sees of them.
    let relay_host = url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("");
    let unknown = format!("http://{}/relay/{}", relay_host, "a-token-nobody-issued");
    let unknown = within(http().get(&unknown).send())
        .await
        .expect("the relay answered the unknown token");
    assert_eq!(
        unknown.status(),
        StatusCode::UNAUTHORIZED,
        "an unknown token is refused: {}",
        unknown.status()
    );

    // A range of the file through the relay: the five headers the relay copies back,
    // and the two it does not.
    let first = within(http().get(url).header("Range", "bytes=0-15").send())
        .await
        .expect("the relay answered");
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "the relay served the stream: {}",
        first.status()
    );
    let headers = first.headers().clone();
    let body = within(first.bytes()).await.expect("the body arrived");
    assert_eq!(
        body.as_ref(),
        b"0123456789abcdef".as_slice(),
        "the relay streamed the upstream's chunk"
    );
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
        header::CONTENT_DISPOSITION,
    ] {
        assert!(headers.get(&name).is_some(), "the relay copied {name}");
    }
    assert!(
        headers.get(header::SET_COOKIE).is_none(),
        "the relay did not copy a cookie"
    );
    assert!(
        headers.get("x-foo").is_none(),
        "the relay did not copy a header it was not told to"
    );

    // A 300-byte `Range` reaches the upstream as no `Range` at all: the limit is 256,
    // and a range past it is not a range a player would send.
    let long_range = format!("bytes=0-{}", "9".repeat(292));
    assert_eq!(long_range.len(), 300, "the test's range is 300 bytes");
    let long = within(http().get(url).header("Range", &long_range).send())
        .await
        .expect("the relay answered the long range");
    assert_eq!(
        long.status(),
        StatusCode::OK,
        "a range past the limit is served: {}",
        long.status()
    );
    let seen = seen.lock().expect("the test state").clone();
    assert!(
        seen.iter().any(|range| range.is_empty()),
        "the 300-byte range reached the upstream as no range: {seen:?}"
    );

    // Nine concurrent requests against an upstream that holds its body open: the slots
    // are held for the whole body, not the head, so eight in flight hold eight slots,
    // and the ninth is refused before it reaches the upstream.
    hold.store(true, Ordering::SeqCst);
    let requests: Vec<_> = (0..9)
        .map(|_| {
            let url = url.to_string();
            async move { http().get(url).send().await }
        })
        .collect();
    let results: Vec<Result<reqwest::Response, reqwest::Error>> =
        within(futures::future::join_all(requests)).await;
    let statuses: Vec<_> = results
        .iter()
        .filter_map(|result| result.as_ref().ok().map(|response| response.status()))
        .collect();
    assert_eq!(
        statuses.len(),
        9,
        "all nine requests got an answer: {statuses:?}"
    );
    // Eight of the nine are `200` (held open), one is `503` (refused). Separate them:
    // the eight player-side responses are read a first chunk from and held here, so the
    // permit-release assertion can drop one (the player hanging up); the refused one is
    // checked for its `Retry-After`.
    let mut held_responses: Vec<reqwest::Response> = Vec::new();
    let mut refused: Option<reqwest::Response> = None;
    for result in results {
        let response = result.expect("all nine requests were answered");
        if response.status() == StatusCode::OK {
            held_responses.push(response);
        } else if response.status() == StatusCode::SERVICE_UNAVAILABLE {
            refused = Some(response);
        } else {
            panic!(
                "an unexpected status in the nine concurrent requests: {}",
                response.status()
            );
        }
    }
    assert_eq!(
        held_responses.len(),
        8,
        "eight of nine concurrent requests hold a slot: {statuses:?}"
    );
    let Some(refused) = refused else {
        panic!("exactly one of nine concurrent requests should be refused: {statuses:?}");
    };
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::SERVICE_UNAVAILABLE)
            .count(),
        1,
        "one of nine concurrent requests is refused, not two: {statuses:?}"
    );
    assert_eq!(
        refused
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("1"),
        "the refusal says when to wait"
    );
    // Each of the eight held responses received its first bytes: the body is held open
    // by the upstream, so the relay's slot is held for the whole body, not the head.
    for held in &mut held_responses {
        let Ok(Some(chunk)) = within(held.chunk()).await else {
            panic!("the held request's first chunk was not delivered");
        };
        assert!(
            !chunk.is_empty(),
            "the held request received its first bytes: {chunk:?}"
        );
    }
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::SERVICE_UNAVAILABLE)
            .count(),
        1,
        "one of nine concurrent requests is refused, not two: {statuses:?}"
    );
    assert_eq!(
        refused
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("1"),
        "the refusal says when to wait"
    );

    // The player hangs up: drop one of the eight player-side responses, keeping every
    // upstream sender alive. The relay must notice the client's EOF while its body poll
    // is pending, end the body, and release the permit. Poll the tenth request for up
    // to two seconds until it is no longer a `503`; this removes the race between the
    // release travelling through reqwest and the relay and the tenth request arriving.
    held_responses.remove(0); // the first player hangs up
    let url_owned = url.to_string();
    let mut tenth = None;
    for _ in 0..20 {
        let response = within(http().get(&url_owned).send())
            .await
            .expect("the tenth request was answered");
        if response.status() != StatusCode::SERVICE_UNAVAILABLE {
            tenth = Some(response);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let Some(tenth) = tenth else {
        panic!("the tenth request never stopped being a 503: the permit was not released");
    };
    assert_eq!(
        tenth.status(),
        StatusCode::OK,
        "the tenth request succeeds after the player hangs up: {}",
        tenth.status()
    );
    drop(tenth);

    // Quitting while a player streams: seven bodies are still held open. Measuring
    // `shutdown` with an instant proves it returns in well under the two-second abort:
    // the stop signal ends every relayed body at once, so the graceful shutdown does
    // not wait for the seven held-open connections.
    let started = std::time::Instant::now();
    shutdown(&mut client).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "shutdown with held-open bodies returned in under 1 s: {elapsed:?}"
    );
}

/// After `shutdown` with a held-open body in flight, the relayed response's body ends:
/// the player's next chunk read returns within a second, rather than blocking for as
/// long as the upstream holds the body open. This is the per-connection half of the
/// stop: the relay's own stop signal ends every relayed body, not just the listener.
#[tokio::test(flavor = "multi_thread")]
async fn relay_shutdown_ends_a_held_body() {
    const ID: &str = "0123456789abcdef0123456789abcdef01234567";
    const STREAM: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream?token=t";

    // The upstream holds its body open: one chunk, then the body never ends until the
    // relay's stop signal does. The sender is kept in test state for the whole test, so
    // the body is genuinely held open and does not end by itself.
    let sender: Arc<Mutex<Option<tokio::sync::mpsc::Sender<axum::body::Bytes>>>> =
        Arc::new(Mutex::new(None));
    let sender_clone = Arc::clone(&sender);
    let upstream = Router::new()
        .route(
            "/api/v1/fs/roots",
            get(|| async { Json(vec!["/tmp".to_string()]) }),
        )
        .route(
            "/api/v1/torrents",
            get(|| async { Json(Vec::<Value>::new()) }),
        )
        .route(
            "/api/v1/session",
            get(|| async { Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})) }),
        )
        .route(
            "/api/v1/session/queue",
            get(|| async {
                Json(json!({
                    "active_downloads": 1,
                    "active_seeds": null,
                    "active_limit": null,
                    "dont_count_slow_torrents": true,
                    "inactive_down_rate": 1024,
                    "inactive_up_rate": 1024,
                    "startup_grace": 30.0,
                    "interval": 1.0,
                }))
            }),
        )
        .route(
            "/api/v1/events",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: ready\ndata: {}\n\n",
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/{id}/files",
            get(|| async {
                Json(json!([{"index":0,"path":"payload.bin","len":16,"pad":false}])).into_response()
            }),
        )
        .route(
            "/api/v1/stream-tokens",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({ "path": STREAM, "expires_in": 60 })),
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream",
            get({
                let sender = Arc::clone(&sender_clone);
                move |_request: axum::extract::Request| {
                    let sender = Arc::clone(&sender);
                    async move {
                        // One chunk, then the body is held open by a channel the test never
                        // feeds: the sender is kept in test state for the whole test, so the
                        // body never ends until the relay's stop signal ends it.
                        let (tx, mut rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(1);
                        *sender.lock().expect("the test state") = Some(tx);
                        let first = futures::stream::once(async {
                            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
                                b"0123456789abcdef",
                            ))
                        });
                        let rest = futures::stream::poll_fn(move |cx| match rx.poll_recv(cx) {
                            std::task::Poll::Ready(Some(chunk)) => {
                                std::task::Poll::Ready(Some(Ok(chunk)))
                            }
                            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
                            std::task::Poll::Pending => std::task::Poll::Pending,
                        });
                        (
                            [
                                (header::CONTENT_TYPE, "application/octet-stream"),
                                (header::ACCEPT_RANGES, "bytes"),
                            ],
                            axum::body::Body::from_stream(first.chain(rest)),
                        )
                            .into_response()
                    }
                }
            }),
        );
    let addr = serve(upstream).await;

    let mut remote = target(addr, LoginMethod::GatewayBasic, Some("quit-user"), None);
    remote.gateway_key_header = Some("x-gw".to_string());
    let mut client = RemoteBackend::start(
        remote,
        Secret::Password("quit-pass".to_string().into()),
        Some("quit-key".to_string().into()),
    );
    wait_for_state(&mut client, true).await;

    let id = TorrentId(ID.to_string());
    assert!(client.send(Command::Stream { id, file: None }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("the relay's URL: {source:?}");
    };
    let url = url.as_str();

    // The player's request: the first chunk arrives, then the body is held open.
    let mut response = within(http().get(url).send())
        .await
        .expect("the relay answered");
    let Ok(Some(first)) = within(response.chunk()).await else {
        panic!("the first chunk was not delivered");
    };
    assert_eq!(
        first.as_ref(),
        b"0123456789abcdef".as_slice(),
        "the player received the first chunk"
    );

    // Quitting *first*: the relay's stop signal ends the held-open body, so the player's
    // next chunk read ends within a second rather than blocking for as long as the
    // upstream holds on. The read has to come after `shutdown`: a read before it would
    // block for the whole `DEADLINE`, because nothing has yet told the body to end.
    shutdown(&mut client).await;
    let started = std::time::Instant::now();
    let next = tokio::time::timeout(Duration::from_secs(1), response.chunk())
        .await
        .expect("the held body ended within 1 s of quit");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the held body ended within 1 s of quit: {:?}",
        started.elapsed()
    );
    assert!(
        !matches!(next, Ok(Some(_))),
        "the held body ended instead of delivering more: {next:?}"
    );
}

/// An upstream that accepts the connection and never sends a response head: the relay
/// answers `504 Gateway Timeout` within the head timeout (1 s under test), not `502`.
/// The body the relay sends is a fixed sentence, never the reqwest error's text (which
/// holds the URL with the stream token in it).
///
/// The upstream's stream handler awaits forever: the head is never sent, so the
/// relay's `tokio::time::timeout` is the only thing that ends the wait.
#[tokio::test(flavor = "multi_thread")]
async fn relay_head_timeout_is_504() {
    const ID: &str = "0123456789abcdef0123456789abcdef01234567";
    const STREAM: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream?token=t";

    // The upstream: the stream handler awaits forever, so the response head is never
    // sent. The relay's `tokio::time::timeout` is the only thing that ends the wait.
    let upstream = Router::new()
        .route(
            "/api/v1/fs/roots",
            get(|| async { Json(vec!["/tmp".to_string()]) }),
        )
        .route(
            "/api/v1/torrents",
            get(|| async { Json(Vec::<Value>::new()) }),
        )
        .route(
            "/api/v1/session",
            get(|| async { Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})) }),
        )
        .route(
            "/api/v1/session/queue",
            get(|| async {
                Json(json!({
                    "active_downloads": 1,
                    "active_seeds": null,
                    "active_limit": null,
                    "dont_count_slow_torrents": true,
                    "inactive_down_rate": 1024,
                    "inactive_up_rate": 1024,
                    "startup_grace": 30.0,
                    "interval": 1.0,
                }))
            }),
        )
        .route(
            "/api/v1/events",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: ready\ndata: {}\n\n",
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/{id}/files",
            get(|| async {
                Json(json!([{"index":0,"path":"payload.bin","len":16,"pad":false}])).into_response()
            }),
        )
        .route(
            "/api/v1/stream-tokens",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({ "path": STREAM, "expires_in": 60 })),
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream",
            get(|| async {
                // The handler awaits forever: the response is never returned, so the
                // head is never sent. The relay's head timeout is the only thing that
                // ends the wait.
                futures::future::pending::<()>().await
            }),
        );
    let addr = serve(upstream).await;

    let mut remote = target(addr, LoginMethod::GatewayBasic, Some("timeout-user"), None);
    remote.gateway_key_header = Some("x-gw".to_string());
    let mut client = RemoteBackend::start(
        remote,
        Secret::Password("timeout-pass".to_string().into()),
        Some("timeout-key".to_string().into()),
    );
    wait_for_state(&mut client, true).await;

    let id = TorrentId(ID.to_string());
    assert!(client.send(Command::Stream { id, file: None }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("the relay's URL: {source:?}");
    };
    let url = url.as_str();

    // The relay answers 504 within 3 s (head timeout is 1 s under test).
    let started = std::time::Instant::now();
    let response = within(http().get(url).send())
        .await
        .expect("the relay answered");
    let elapsed = started.elapsed();
    assert_eq!(
        response.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "a silent upstream gets 504, not 502: {}",
        response.status()
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "the 504 came within 3 s: {elapsed:?}"
    );
    // The body is a fixed sentence, never the reqwest error's text.
    let body = within(response.text()).await.expect("the body arrived");
    assert!(
        !body.contains(url),
        "the 504 body does not hold the relay's URL: {body}"
    );
    assert!(
        !body.contains("token"),
        "the 504 body does not name a token: {body}"
    );

    shutdown(&mut client).await;
}

/// A Password login with a `gateway_key_header`, streaming through the relay: the
/// upstream records the headers it saw, and there is no `Authorization: Bearer` in
/// them. The relay sends only the gateway's credential (the key header), never the
/// server's session bearer.
#[tokio::test(flavor = "multi_thread")]
async fn relay_does_not_send_session_bearer() {
    const ID: &str = "0123456789abcdef0123456789abcdef01234567";
    const STREAM: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream?token=t";

    // The upstream records every `Authorization` and `x-gw-key` header it sees on the
    // stream route, and answers the login and stream routes.
    let saw_auth = Arc::new(Mutex::new(Vec::new()));
    let saw_auth_clone = Arc::clone(&saw_auth);
    let saw_key = Arc::new(Mutex::new(Vec::new()));
    let saw_key_clone = Arc::clone(&saw_key);
    let upstream = Router::new()
        .route(
            "/api/v1/fs/roots",
            get(|| async { Json(vec!["/tmp".to_string()]) }),
        )
        .route(
            "/api/v1/torrents",
            get(|| async { Json(Vec::<Value>::new()) }),
        )
        .route(
            "/api/v1/session",
            get(|| async { Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})) }),
        )
        .route(
            "/api/v1/session/queue",
            get(|| async {
                Json(json!({
                    "active_downloads": 1,
                    "active_seeds": null,
                    "active_limit": null,
                    "dont_count_slow_torrents": true,
                    "inactive_down_rate": 1024,
                    "inactive_up_rate": 1024,
                    "startup_grace": 30.0,
                    "interval": 1.0,
                }))
            }),
        )
        .route(
            "/api/v1/events",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: ready\ndata: {}\n\n",
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/{id}/files",
            get(|| async {
                Json(json!([{"index":0,"path":"payload.bin","len":16,"pad":false}])).into_response()
            }),
        )
        .route(
            "/api/v1/stream-tokens",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({ "path": STREAM, "expires_in": 60 })),
                )
                    .into_response()
            }),
        )
        // The login route: a Password login needs it. The test checks that the relay
        // does not send the session bearer it earns here.
        .route(
            "/api/v1/auth/session",
            post(|| async { Json(json!({"token": "session-token-123", "expires_in": 3600})) }),
        )
        .route(
            "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream",
            get({
                let saw_auth = Arc::clone(&saw_auth_clone);
                let saw_key = Arc::clone(&saw_key_clone);
                move |request: axum::extract::Request| {
                    let saw_auth = Arc::clone(&saw_auth);
                    let saw_key = Arc::clone(&saw_key);
                    async move {
                        let auth = request
                            .headers()
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string)
                            .unwrap_or_default();
                        let key = request
                            .headers()
                            .get("x-gw-key")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string)
                            .unwrap_or_default();
                        saw_auth.lock().expect("the test state").push(auth);
                        saw_key.lock().expect("the test state").push(key);
                        ("0123456789abcdef".to_string()).into_response()
                    }
                }
            }),
        );
    let addr = serve(upstream).await;

    // A Password login (server session) with a `gateway_key_header` and a key: the
    // client signs to a gateway, so it routes streams through the relay. The relay must
    // send the gateway key header, not the session bearer.
    let mut remote = target(addr, LoginMethod::Password, Some("bearer-user"), None);
    remote.gateway_key_header = Some("x-gw-key".to_string());
    let mut client = RemoteBackend::start(
        remote,
        Secret::Password("bearer-pass".to_string().into()),
        Some("gw-key-value".to_string().into()),
    );
    wait_for_state(&mut client, true).await;

    let id = TorrentId(ID.to_string());
    assert!(client.send(Command::Stream { id, file: None }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("the relay's URL: {source:?}");
    };
    let url = url.as_str();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "the relay's URL is loopback: {url}"
    );

    // The player's request through the relay: the upstream records the `Authorization`
    // header it saw, and there is no `Bearer` in it.
    let first = within(http().get(url).send())
        .await
        .expect("the relay answered");
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "the relay served the stream: {}",
        first.status()
    );
    let _ = within(first.bytes()).await.expect("the body arrived");

    // The upstream saw no `Authorization: Bearer` header: the relay sent only the
    // gateway key header, never the server's session bearer. The list is not empty:
    // `all` on an empty list passes, so assert the request actually arrived.
    let saw = saw_auth.lock().expect("the test state").clone();
    assert!(!saw.is_empty(), "the stream route was reached: {saw:?}");
    assert!(
        saw.iter().all(|auth| !auth.starts_with("Bearer")),
        "the relay did not send the session bearer: {saw:?}"
    );
    // The gateway key header went on its own, with the value the client was given: a
    // relay that sent no credentials at all would not have it.
    let saw_key = saw_key.lock().expect("the test state").clone();
    assert!(
        !saw_key.is_empty(),
        "the stream route saw the key header: {saw_key:?}"
    );
    assert!(
        saw_key.iter().all(|key| key == "gw-key-value"),
        "the gateway key header reached the upstream with its value: {saw_key:?}"
    );

    shutdown(&mut client).await;
}

/// A `401` on the relay's forward is a bad stream token, not a refused session: the
/// relay answers `502`, and the server's session-login counter does not increase. A
/// `Password` login behind a key header has a `Kind::Server` session the relay never
/// sent, so a `401` cannot be about it - and it must not be thrown away.
#[tokio::test(flavor = "multi_thread")]
async fn relay_401_does_not_drop_the_session() {
    const ID: &str = "0123456789abcdef0123456789abcdef01234567";
    const STREAM: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream?token=t";

    // The upstream counts its session logins and answers the stream route with `401`:
    // a bad stream token, the case the ruling is about.
    let logins = Arc::new(AtomicUsize::new(0));
    let logins_clone = Arc::clone(&logins);
    let upstream = Router::new()
        .route(
            "/api/v1/fs/roots",
            get(|| async { Json(vec!["/tmp".to_string()]) }),
        )
        .route(
            "/api/v1/torrents",
            get(|| async { Json(Vec::<Value>::new()) }),
        )
        .route(
            "/api/v1/session",
            get(|| async { Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})) }),
        )
        .route(
            "/api/v1/session/queue",
            get(|| async {
                Json(json!({
                    "active_downloads": 1,
                    "active_seeds": null,
                    "active_limit": null,
                    "dont_count_slow_torrents": true,
                    "inactive_down_rate": 1024,
                    "inactive_up_rate": 1024,
                    "startup_grace": 30.0,
                    "interval": 1.0,
                }))
            }),
        )
        .route(
            "/api/v1/events",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: ready\ndata: {}\n\n",
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/{id}/files",
            get(|| async {
                Json(json!([{"index":0,"path":"payload.bin","len":16,"pad":false}])).into_response()
            }),
        )
        .route(
            "/api/v1/stream-tokens",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({ "path": STREAM, "expires_in": 60 })),
                )
                    .into_response()
            }),
        )
        // The login route: counts the session logins and answers `401` from the second
        // call on. The first call is the initial session; if the relay's `401` dropped
        // the session, the next call would re-login and answer `401` - a sign the session
        // was thrown away for a bad stream token.
        .route(
            "/api/v1/auth/session",
            post({
                let logins = Arc::clone(&logins_clone);
                move || async move {
                    let count = logins.fetch_add(1, Ordering::SeqCst) + 1;
                    if count == 1 {
                        Json(json!({"token": "session-token-123", "expires_in": 3600}))
                            .into_response()
                    } else {
                        (StatusCode::UNAUTHORIZED, "the session was dropped").into_response()
                    }
                }
            }),
        )
        .route(
            "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream",
            get(|| async { (StatusCode::UNAUTHORIZED, "the stream token is bad").into_response() }),
        );
    let addr = serve(upstream).await;

    // A Password login (server session) with a `gateway_key_header` and a key: the
    // client signs to a gateway, so it routes streams through the relay.
    let mut remote = target(addr, LoginMethod::Password, Some("token-user"), None);
    remote.gateway_key_header = Some("x-gw-key".to_string());
    let mut client = RemoteBackend::start(
        remote,
        Secret::Password("token-pass".to_string().into()),
        Some("gw-key-value".to_string().into()),
    );
    wait_for_state(&mut client, true).await;

    let id = TorrentId(ID.to_string());
    assert!(client.send(Command::Stream { id, file: None }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("the relay's URL: {source:?}");
    };
    let url = url.as_str();

    // The player's request: the upstream answers `401` (a bad stream token), and the
    // relay maps it to a `502` for the player.
    let first = within(http().get(url).send())
        .await
        .expect("the relay answered");
    assert_eq!(
        first.status(),
        StatusCode::BAD_GATEWAY,
        "a 401 on a non-gateway login is a 502 for the player, not a retry: {}",
        first.status()
    );
    let _ = within(first.bytes()).await.expect("the body arrived");

    // The relay logged in once to start (the refresh loop's session). The `401` on the
    // relay's forward must not add another: a bad stream token is not a dropped session,
    // and throwing the TUI's session away for it is a login storm. Poll the counter for
    // a moment; if the session had been invalidated, the next ordinary call would re-login
    // and the counter would climb (and, on the second login, answer `401` too).
    let before = logins.load(Ordering::SeqCst);
    assert_eq!(
        before, 1,
        "the initial session login happened exactly once: {before}"
    );
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let after = logins.load(Ordering::SeqCst);
        if after != before {
            panic!("a 401 on the relay logged the session in again: before={before} after={after}");
        }
    }
    assert_eq!(
        logins.load(Ordering::SeqCst),
        1,
        "a 401 on the relay did not log the session in again"
    );

    shutdown(&mut client).await;
}

/// A `401` on a gateway login is the one the relay retries: the upstream answers `401`
/// once (the gateway token is no good) and `200` on the retry, and the relay answers
/// `200`. The token endpoint is asked for exactly one new token for the retry: the
/// initial login already earned one, and the `401` renewal is the second.
#[tokio::test(flavor = "multi_thread")]
async fn relay_401_renews_the_gateway_token() {
    const ID: &str = "0123456789abcdef0123456789abcdef01234567";
    const STREAM: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream?token=t";

    // The upstream answers the stream route `401` once, then `200`: the first attempt's
    // gateway token is the one that is no good, the retry's is new.
    let stream_hits = Arc::new(AtomicUsize::new(0));
    let stream_hits_clone = Arc::clone(&stream_hits);
    // The token endpoint counts how many times the gateway asked for a token.
    let token_hits = Arc::new(AtomicUsize::new(0));
    let token_hits_clone = Arc::clone(&token_hits);

    let key = keypair();
    let pem = key.to_openssh(LineEnding::LF).expect("the key file");
    let dir = ScratchDir::new("relay_401_renew");
    let key_file = write_file(dir.path(), "id_ed25519", pem.as_bytes());

    let upstream = Router::new()
        .route(
            "/api/v1/fs/roots",
            get(|| async { Json(vec!["/tmp".to_string()]) }),
        )
        .route(
            "/api/v1/torrents",
            get(|| async { Json(Vec::<Value>::new()) }),
        )
        .route(
            "/api/v1/session",
            get(|| async { Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})) }),
        )
        .route(
            "/api/v1/session/queue",
            get(|| async {
                Json(json!({
                    "active_downloads": 1,
                    "active_seeds": null,
                    "active_limit": null,
                    "dont_count_slow_torrents": true,
                    "inactive_down_rate": 1024,
                    "inactive_up_rate": 1024,
                    "startup_grace": 30.0,
                    "interval": 1.0,
                }))
            }),
        )
        .route(
            "/api/v1/events",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: ready\ndata: {}\n\n",
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/{id}/files",
            get(|| async {
                Json(json!([{"index":0,"path":"payload.bin","len":16,"pad":false}])).into_response()
            }),
        )
        .route(
            "/api/v1/stream-tokens",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({ "path": STREAM, "expires_in": 60 })),
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream",
            get({
                let stream_hits = Arc::clone(&stream_hits_clone);
                move |request: axum::extract::Request| {
                    let stream_hits = Arc::clone(&stream_hits);
                    async move {
                        let _ = request;
                        let count = stream_hits.fetch_add(1, Ordering::SeqCst) + 1;
                        if count == 1 {
                            (StatusCode::UNAUTHORIZED, "the token is no good").into_response()
                        } else {
                            (
                                [(header::CONTENT_TYPE, "application/octet-stream")],
                                "0123456789abcdef".to_string(),
                            )
                                .into_response()
                        }
                    }
                }
            }),
        )
        // The gateway's token endpoint: issues a numbered token on every call, and counts
        // the calls. The client's refresh loop earns token 1 at start; the `401` renewal
        // earns token 2.
        .route(
            "/token",
            post({
                let token_hits = Arc::clone(&token_hits_clone);
                move |_request: axum::extract::Request| {
                    let token_hits = Arc::clone(&token_hits);
                    async move {
                        let count = token_hits.fetch_add(1, Ordering::SeqCst) + 1;
                        // The assertion is not verified here: the test is about the relay's
                        // retry, not the gateway's signature check. The token is issued
                        // regardless, numbered so the relay's first and second tokens
                        // differ.
                        Json(json!({"access_token": format!("token-{count}"), "expires_in": 3600}))
                            .into_response()
                    }
                }
            }),
        );
    let addr = serve(upstream).await;

    // A `GatewayKey` login: the client earns a bearer token from the gateway's token
    // endpoint, and the relay sends it on a stream forward.
    let mut remote = target(addr, LoginMethod::GatewayKey, None, Some(key_file.clone()));
    remote.token_url = Some(format!("http://{addr}/token"));
    remote.client_id = Some("featherbittorrent-test".to_string());
    let mut client = RemoteBackend::start(remote, Secret::KeyPassphrase(None), None);
    wait_for_state(&mut client, true).await;

    let id = TorrentId(ID.to_string());
    assert!(client.send(Command::Stream { id, file: None }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("the relay's URL: {source:?}");
    };
    let url = url.as_str();

    // The player's request: the upstream answers 401 once, the relay renews the gateway
    // token, and the retry gets 200. The relay answers 200 to the player.
    let first = within(http().get(url).send())
        .await
        .expect("the relay answered");
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "the relay answered 200 after the 401 renewal: {}",
        first.status()
    );
    let _ = within(first.bytes()).await.expect("the body arrived");

    // The upstream was reached twice: the first attempt (401) and the retry (200).
    assert_eq!(
        stream_hits.load(Ordering::SeqCst),
        2,
        "the upstream was reached once for the 401 and once for the retry"
    );
    // The token endpoint was asked for exactly one new token for the retry: the initial
    // login earned token 1, and the 401 renewal earned token 2.
    assert_eq!(
        token_hits.load(Ordering::SeqCst),
        2,
        "the gateway token was earned once at start and once for the 401 renewal"
    );

    shutdown(&mut client).await;
}

/// One gateway token expiry seen by several relayed requests at once costs one token
/// fetch. Four player requests all send `token-1`, the upstream holds them until all four
/// have arrived and then answers each `401`; every retry must succeed, and the token
/// endpoint is asked exactly twice in all: the login, and one renewal shared by the four.
#[tokio::test(flavor = "multi_thread")]
async fn relay_concurrent_401s_share_one_token_refresh() {
    const ID: &str = "0123456789abcdef0123456789abcdef01234567";
    const STREAM: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream?token=t";

    // The upstream answers the stream route `401` once, then `200`: the first attempt's
    // gateway token is the one that is no good, the retry's is new.
    let stream_hits = Arc::new(AtomicUsize::new(0));
    let stream_hits_clone = Arc::clone(&stream_hits);
    const PLAYERS: usize = 4;
    let barrier = Arc::new(tokio::sync::Barrier::new(PLAYERS));
    // The token endpoint counts how many times the gateway asked for a token.
    let token_hits = Arc::new(AtomicUsize::new(0));
    let token_hits_clone = Arc::clone(&token_hits);

    let key = keypair();
    let pem = key.to_openssh(LineEnding::LF).expect("the key file");
    let dir = ScratchDir::new("relay_401_concurrent");
    let key_file = write_file(dir.path(), "id_ed25519", pem.as_bytes());

    let upstream = Router::new()
        .route(
            "/api/v1/fs/roots",
            get(|| async { Json(vec!["/tmp".to_string()]) }),
        )
        .route(
            "/api/v1/torrents",
            get(|| async { Json(Vec::<Value>::new()) }),
        )
        .route(
            "/api/v1/session",
            get(|| async { Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})) }),
        )
        .route(
            "/api/v1/session/queue",
            get(|| async {
                Json(json!({
                    "active_downloads": 1,
                    "active_seeds": null,
                    "active_limit": null,
                    "dont_count_slow_torrents": true,
                    "inactive_down_rate": 1024,
                    "inactive_up_rate": 1024,
                    "startup_grace": 30.0,
                    "interval": 1.0,
                }))
            }),
        )
        .route(
            "/api/v1/events",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: ready\ndata: {}\n\n",
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/{id}/files",
            get(|| async {
                Json(json!([{"index":0,"path":"payload.bin","len":16,"pad":false}])).into_response()
            }),
        )
        .route(
            "/api/v1/stream-tokens",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({ "path": STREAM, "expires_in": 60 })),
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream",
            get({
                let stream_hits = Arc::clone(&stream_hits_clone);
                let barrier = Arc::clone(&barrier);
                move |request: axum::extract::Request| {
                    let stream_hits = Arc::clone(&stream_hits);
                    let barrier = Arc::clone(&barrier);
                    async move {
                        stream_hits.fetch_add(1, Ordering::SeqCst);
                        let bearer = request
                            .headers()
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        if bearer == "Bearer token-1" {
                            // The expired token: hold every request carrying it until all
                            // of them are in flight, so each one gets its own `401`.
                            barrier.wait().await;
                            (StatusCode::UNAUTHORIZED, "the token is no good").into_response()
                        } else {
                            (
                                [(header::CONTENT_TYPE, "application/octet-stream")],
                                "0123456789abcdef".to_string(),
                            )
                                .into_response()
                        }
                    }
                }
            }),
        )
        // The gateway's token endpoint: issues a numbered token on every call, and counts
        // the calls. The client's refresh loop earns token 1 at start; the `401` renewal
        // earns token 2.
        .route(
            "/token",
            post({
                let token_hits = Arc::clone(&token_hits_clone);
                move |_request: axum::extract::Request| {
                    let token_hits = Arc::clone(&token_hits);
                    async move {
                        let count = token_hits.fetch_add(1, Ordering::SeqCst) + 1;
                        // The assertion is not verified here: the test is about the relay's
                        // retry, not the gateway's signature check. The token is issued
                        // regardless, numbered so the relay's first and second tokens
                        // differ.
                        Json(json!({"access_token": format!("token-{count}"), "expires_in": 3600}))
                            .into_response()
                    }
                }
            }),
        );
    let addr = serve(upstream).await;

    // A `GatewayKey` login: the client earns a bearer token from the gateway's token
    // endpoint, and the relay sends it on a stream forward.
    let mut remote = target(addr, LoginMethod::GatewayKey, None, Some(key_file.clone()));
    remote.token_url = Some(format!("http://{addr}/token"));
    remote.client_id = Some("featherbittorrent-test".to_string());
    let mut client = RemoteBackend::start(remote, Secret::KeyPassphrase(None), None);
    wait_for_state(&mut client, true).await;

    let id = TorrentId(ID.to_string());
    assert!(client.send(Command::Stream { id, file: None }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("the relay's URL: {source:?}");
    };
    let url = url.as_str();

    // Four concurrent player requests, all sent with the expired token.
    let mut players = Vec::new();
    for _ in 0..PLAYERS {
        let url = url.to_string();
        players.push(tokio::spawn(async move {
            let response = http().get(url).send().await.expect("the relay answered");
            let status = response.status();
            let _ = response.bytes().await.expect("the body arrived");
            status
        }));
    }
    for player in players {
        let status = within(player).await.expect("the player task");
        assert_eq!(
            status,
            StatusCode::OK,
            "the relay answered 200 after the renewal"
        );
    }

    // Four first attempts (401) and four retries (200).
    assert_eq!(stream_hits.load(Ordering::SeqCst), 2 * PLAYERS);
    // The login earned token 1; one renewal served all four requests.
    assert_eq!(
        token_hits.load(Ordering::SeqCst),
        2,
        "one expiry costs one token fetch, however many requests saw the 401"
    );

    shutdown(&mut client).await;
}

/// A `401` on a basic login is a bad stream token, not a refused credential: the relay
/// answers `502` with no retry. The upstream is reached exactly once.
#[tokio::test(flavor = "multi_thread")]
async fn relay_401_basic_login_no_retry() {
    const ID: &str = "0123456789abcdef0123456789abcdef01234567";
    const STREAM: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream?token=t";

    let stream_hits = Arc::new(AtomicUsize::new(0));
    let stream_hits_clone = Arc::clone(&stream_hits);
    let upstream = Router::new()
        .route(
            "/api/v1/fs/roots",
            get(|| async { Json(vec!["/tmp".to_string()]) }),
        )
        .route(
            "/api/v1/torrents",
            get(|| async { Json(Vec::<Value>::new()) }),
        )
        .route(
            "/api/v1/session",
            get(|| async { Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})) }),
        )
        .route(
            "/api/v1/session/queue",
            get(|| async {
                Json(json!({
                    "active_downloads": 1,
                    "active_seeds": null,
                    "active_limit": null,
                    "dont_count_slow_torrents": true,
                    "inactive_down_rate": 1024,
                    "inactive_up_rate": 1024,
                    "startup_grace": 30.0,
                    "interval": 1.0,
                }))
            }),
        )
        .route(
            "/api/v1/events",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: ready\ndata: {}\n\n",
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/{id}/files",
            get(|| async {
                Json(json!([{"index":0,"path":"payload.bin","len":16,"pad":false}])).into_response()
            }),
        )
        .route(
            "/api/v1/stream-tokens",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({ "path": STREAM, "expires_in": 60 })),
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream",
            get({
                let stream_hits = Arc::clone(&stream_hits_clone);
                move |_request: axum::extract::Request| {
                    let stream_hits = Arc::clone(&stream_hits);
                    async move {
                        stream_hits.fetch_add(1, Ordering::SeqCst);
                        (StatusCode::UNAUTHORIZED, "the token is no good").into_response()
                    }
                }
            }),
        );
    let addr = serve(upstream).await;

    // A `GatewayBasic` login: the relay sends its `Authorization` header on a stream
    // forward, and a `401` is a bad stream token, not a refused credential.
    let mut remote = target(addr, LoginMethod::GatewayBasic, Some("basic-user"), None);
    remote.gateway_key_header = Some("x-gw".to_string());
    let mut client = RemoteBackend::start(
        remote,
        Secret::Password("basic-pass".to_string().into()),
        Some("basic-key".to_string().into()),
    );
    wait_for_state(&mut client, true).await;

    let id = TorrentId(ID.to_string());
    assert!(client.send(Command::Stream { id, file: None }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("the relay's URL: {source:?}");
    };
    let url = url.as_str();

    // The player's request: the upstream answers 401, and the relay maps it to a 502
    // with no retry. The upstream is reached exactly once.
    let first = within(http().get(url).send())
        .await
        .expect("the relay answered");
    assert_eq!(
        first.status(),
        StatusCode::BAD_GATEWAY,
        "a 401 on a basic login is a 502 with no retry: {}",
        first.status()
    );
    let _ = within(first.bytes()).await.expect("the body arrived");
    assert_eq!(
        stream_hits.load(Ordering::SeqCst),
        1,
        "the upstream was reached exactly once: no retry on a basic login's 401"
    );

    shutdown(&mut client).await;
}

/// The gateway `401` retry must not hold the auth lock across the retry's upstream
/// request. The upstream answers the first stream request `401` and parks the second
/// (the retry) on a gate the test controls, so the stall outlasts the measurement. Only
/// once the upstream has seen the second request does the test send an ordinary TUI
/// call that needs the auth lock (`Devices`), and it asserts that call's own request
/// reaches the upstream within 300 ms. With the lock held across the retry it cannot:
/// the call waits for the gate.
#[tokio::test(flavor = "multi_thread")]
async fn relay_retry_does_not_hold_the_auth_lock() {
    const ID: &str = "0123456789abcdef0123456789abcdef01234567";
    const STREAM: &str =
        "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream?token=t";

    // The upstream answers the stream route `401` once, then parks the second (the
    // retry) on a gate the test controls: it says the retry arrived, and waits on
    // the gate until the test opens it. The stall is the case the finding is
    // about: a slow or hostile upstream on the retry's path, held open for as
    // long as the test needs to say it holds nothing.
    let stream_hits = Arc::new(AtomicUsize::new(0));
    let stream_hits_clone = Arc::clone(&stream_hits);
    // The test's controls: the upstream says when the retry has arrived, and parks it on
    // `gate` until the test opens it. `devices_hit` says the TUI call's request arrived.
    let retry_arrived = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let (devices_hit_tx, mut devices_hit) = tokio::sync::mpsc::unbounded_channel::<()>();
    let key = keypair();
    let pem = key.to_openssh(LineEnding::LF).expect("the key file");
    let dir = ScratchDir::new("relay_retry_no_lock");
    let key_file = write_file(dir.path(), "id_ed25519", pem.as_bytes());

    let upstream = Router::new()
        .route(
            "/api/v1/fs/roots",
            get(|| async { Json(vec!["/tmp".to_string()]) }),
        )
        .route(
            "/api/v1/torrents",
            get(|| async { Json(Vec::<Value>::new()) }),
        )
        .route(
            "/api/v1/session",
            get(|| async { Json(json!({"version": "test", "dht_nodes": null, "torrents": 0})) }),
        )
        .route(
            "/api/v1/session/queue",
            get(|| async {
                Json(json!({
                    "active_downloads": 1,
                    "active_seeds": null,
                    "active_limit": null,
                    "dont_count_slow_torrents": true,
                    "inactive_down_rate": 1024,
                    "inactive_up_rate": 1024,
                    "startup_grace": 30.0,
                    "interval": 1.0,
                }))
            }),
        )
        .route(
            "/api/v1/events",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "event: ready\ndata: {}\n\n",
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/auth/devices",
            get(move || {
                let devices_hit_tx = devices_hit_tx.clone();
                async move {
                    let _ = devices_hit_tx.send(());
                    Json(Vec::<Value>::new())
                }
            }),
        )
        .route(
            "/api/v1/torrents/{id}/files",
            get(|| async {
                Json(json!([{"index":0,"path":"payload.bin","len":16,"pad":false}])).into_response()
            }),
        )
        .route(
            "/api/v1/stream-tokens",
            post(|| async {
                (
                    StatusCode::CREATED,
                    Json(json!({ "path": STREAM, "expires_in": 60 })),
                )
                    .into_response()
            }),
        )
        .route(
            "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream",
            get({
                let stream_hits = Arc::clone(&stream_hits_clone);
                let retry_arrived = Arc::clone(&retry_arrived);
                let gate = Arc::clone(&gate);
                move |_request: axum::extract::Request| {
                    let stream_hits = Arc::clone(&stream_hits);
                    let retry_arrived = Arc::clone(&retry_arrived);
                    let gate = Arc::clone(&gate);
                    async move {
                        let count = stream_hits.fetch_add(1, Ordering::SeqCst) + 1;
                        if count == 1 {
                            (StatusCode::UNAUTHORIZED, "the token is no good").into_response()
                        } else {
                            // The retry: say it arrived, then park until the test opens
                            // the gate. A client that holds the auth lock across this
                            // request keeps every TUI call blocked for as long as it waits.
                            retry_arrived.notify_one();
                            gate.notified().await;
                            (
                                [(header::CONTENT_TYPE, "application/octet-stream")],
                                "0123456789abcdef".to_string(),
                            )
                                .into_response()
                        }
                    }
                }
            }),
        )
        .route(
            "/token",
            post(|| async {
                Json(json!({"access_token": "token-1", "expires_in": 3600})).into_response()
            }),
        );
    let addr = serve(upstream).await;

    // A `GatewayKey` login: the client earns a bearer token from the gateway's token
    // endpoint, and the relay sends it on a stream forward.
    let mut remote = target(addr, LoginMethod::GatewayKey, None, Some(key_file.clone()));
    remote.token_url = Some(format!("http://{addr}/token"));
    remote.client_id = Some("featherbittorrent-test".to_string());
    let mut client = RemoteBackend::start(remote, Secret::KeyPassphrase(None), None);
    wait_for_state(&mut client, true).await;

    let id = TorrentId(ID.to_string());
    assert!(client.send(Command::Stream { id, file: None }));
    let source = source_of(notice_that(&mut client, |notice| play_source(notice).is_some()).await);
    let fbtt_core::PlaySource::Url(url) = &source else {
        panic!("the relay's URL: {source:?}");
    };
    let url = url.as_str();

    // The player's request runs in its own task, so the test can act while the retry is
    // parked at the upstream.
    let player_url = url.to_string();
    let player = tokio::spawn(async move {
        let response = http()
            .get(player_url)
            .send()
            .await
            .expect("the relay answered");
        let status = response.status();
        let _ = response.bytes().await.expect("the body arrived");
        status
    });

    // The retry is provably in flight once the upstream has seen the second stream request.
    within(retry_arrived.notified()).await;
    assert_eq!(
        stream_hits.load(Ordering::SeqCst),
        2,
        "the 401 and the retry were both seen"
    );

    // An ordinary TUI call that needs the auth lock. Its own request must reach the
    // upstream while the retry is still parked: this call's request, not any snapshot.
    let sent = tokio::time::Instant::now();
    assert!(client.send(Command::Devices(DeviceCommand::List)));
    let reached = tokio::time::timeout(Duration::from_millis(300), devices_hit.recv()).await;
    let elapsed = sent.elapsed();
    // Open the gate whatever happened, so the player and the backend can finish.
    gate.notify_one();
    assert!(
        matches!(reached, Ok(Some(()))),
        "the TUI call did not reach the upstream within 300 ms while the retry was parked: the relay held the auth lock across the retry ({elapsed:?})"
    );

    let status = within(player).await.expect("the player task");
    // The lock assert above is what this test is about; the status only needs to say the retry was answered
    // at all. The gate can open up to about 300 ms after the retry arrives, and in a test build the relay's
    // head cap is one second: on a descheduled runner the retry's answer is a 504, and the lock was still not
    // held across it, so either status is what the test proves.
    assert!(
        matches!(status, StatusCode::OK | StatusCode::GATEWAY_TIMEOUT),
        "the relay answered its own 200 or 504 after the 401 renewal: {status}"
    );

    shutdown(&mut client).await;
}

/// A server that answers the token route with garbage: one error notice, no `Play`,
/// and a body this client cannot read is a body this client does not read whole - a
/// 100 MiB answer costs the client the cap and a line, not its memory.
#[tokio::test(flavor = "multi_thread")]
async fn hostile_server_stream_answer() {
    let fake = Fake::open();
    let addr =
        serve(api_routes_with_rude_tokens(get(counted_events)).with_state(Arc::clone(&fake))).await;

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::None, None, None),
        Secret::None,
        None,
    );
    wait_for_state(&mut client, true).await;

    let id = TorrentId("0123456789abcdef0123456789abcdef01234567".to_string());
    // First stream: the garbage answer, one error, no play.
    assert!(client.send(Command::Stream {
        id: id.clone(),
        file: None
    }));
    let lines = notices_until(&mut client, |lines| {
        lines.iter().any(|line| line.contains("could not read"))
    })
    .await;
    assert!(
        lines.iter().any(|line| line.contains("could not read")),
        "a body that is not JSON says so: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.is_empty()),
        "a garbage answer is not a play: {lines:?}"
    );

    // Second stream: the body over the cap, one line about it, no play.
    assert!(client.send(Command::Stream { id, file: None }));
    let lines = notices_until(&mut client, |lines| {
        lines.iter().any(|line| line.contains("MiB"))
    })
    .await;
    assert!(
        lines.iter().any(|line| line.contains("MiB")),
        "a body over the cap says so: {lines:?}"
    );

    // The link is up: a rude answer is an answer, and the client is still answering
    // its screen.
    wait_for_state(&mut client, true).await;
    shutdown(&mut client).await;
}

// ------------------------------------------------------------------------------------------- settings

/// `Load` then `Save(Speed)` against a real server: the answer becomes `Loaded` with the new numbers, and
/// the server's own `config.yaml` has them.
#[tokio::test(flavor = "multi_thread")]
async fn remote_settings_round_trip() {
    let lib = Library::new("remote_settings", vec![alice()], vec![]);
    let (server, addr) = lib.serve().await;
    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::Password, Some("alice"), None),
        Secret::Password("hunter2".to_string().into()),
        None,
    );
    wait_for_state(&mut client, true).await;

    assert!(client.send(Command::Settings(SettingsCommand::Load)));
    let first = wait_for(&mut client, |snapshot| match &snapshot.settings {
        SettingsState::Loaded(view) => Some(view.clone()),
        _ => None,
    })
    .await;
    assert_eq!(first.downloads.default_dir, lib.config.download_dir);

    let mut limits = first.speed;
    limits.download_kib = 321;
    limits.upload_kib = 123;
    assert!(client.send(Command::Settings(SettingsCommand::Save(
        SettingsChange::Speed(limits)
    ))));
    let saved = wait_for(&mut client, |snapshot| match &snapshot.settings {
        SettingsState::Loaded(view) if view.speed.download_kib == 321 => Some(view.clone()),
        _ => None,
    })
    .await;
    assert_eq!(saved.speed.upload_kib, 123);
    let lines = notices_until(&mut client, |lines| {
        lines.iter().any(|line| line == "Saved.")
    })
    .await;
    assert!(lines.iter().any(|line| line == "Saved."), "{lines:?}");

    match Config::load(&lib.paths).expect("read the server's config back") {
        fbtt_core::ConfigLoad::Loaded(config) => {
            assert_eq!(config.limits.download_kib, 321);
            assert_eq!(config.limits.upload_kib, 123);
        }
        other => panic!("the server wrote its limits, got {other:?}"),
    }

    shutdown(&mut client).await;
    within(server.shutdown()).await.expect("the last save ran");
}

/// The settings a stub server answers: a view whose program is shown and not editable, as a real server
/// answers every request that does not carry the launch secret.
fn stub_view(program: &Path) -> Value {
    json!({
        "speed": {
            "download_kib": 0,
            "upload_kib": 0,
            "active_downloads": 3,
            "active_seeds": null,
            "active_total": null
        },
        "downloads": {
            "default_dir": std::env::temp_dir(),
            "unfinished_dir": null,
            "start_paused": false,
            "watch_dir": null,
            "on_complete": { "program": program, "editable": false },
            "watch_problem": null,
            "on_complete_problem": null
        }
    })
}

/// A server's program is its config file's to hold: the view says it is not editable, and a downloads save
/// from the remote backend never sends the `on_complete` key, whatever the screen handed in.
#[tokio::test(flavor = "multi_thread")]
async fn remote_program_is_not_editable() {
    let program = std::env::current_exe().expect("this test's own binary");
    let bodies: Arc<Mutex<Vec<Value>>> = Arc::default();
    let view = stub_view(&program);
    let get_view = view.clone();
    let put_view = view.clone();
    let seen = Arc::clone(&bodies);
    let fake = Fake::plain();
    let addr = serve(
        api_routes(get(events))
            .route(
                "/api/v1/settings",
                get(move || {
                    let body = get_view.clone();
                    async move { Json(body) }
                }),
            )
            .route(
                "/api/v1/settings/downloads",
                put(move |Json(body): Json<Value>| {
                    let seen = Arc::clone(&seen);
                    let answer = put_view.clone();
                    async move {
                        seen.lock().expect("the test state").push(body);
                        Json(answer)
                    }
                }),
            )
            .with_state(fake),
    )
    .await;
    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::None, None, None),
        Secret::None,
        None,
    );
    wait_for_state(&mut client, true).await;

    assert!(client.send(Command::Settings(SettingsCommand::Load)));
    let loaded = wait_for(&mut client, |snapshot| match &snapshot.settings {
        SettingsState::Loaded(view) => Some(view.clone()),
        _ => None,
    })
    .await;
    assert!(!loaded.downloads.on_complete.editable);
    assert_eq!(loaded.downloads.on_complete.program, Some(program.clone()));

    // The screen hands in a program anyway.
    assert!(client.send(Command::Settings(SettingsCommand::Save(
        SettingsChange::Downloads(fbtt_core::DownloadsChange {
            default_dir: std::env::temp_dir(),
            unfinished_dir: None,
            start_paused: true,
            watch_dir: None,
            on_complete: Some(fbtt_core::OnCompleteSet {
                program: Some(program),
            }),
        })
    ))));
    let lines = notices_until(&mut client, |lines| {
        lines.iter().any(|line| line == "Saved.")
    })
    .await;
    assert!(lines.iter().any(|line| line == "Saved."), "{lines:?}");

    let bodies = bodies.lock().expect("the test state").clone();
    assert_eq!(bodies.len(), 1, "one save, one request: {bodies:?}");
    assert_eq!(bodies[0]["start_paused"], json!(true), "the section went");
    assert!(
        bodies[0].get("on_complete").is_none(),
        "a remote client never names the program: {:?}",
        bodies[0]
    );

    shutdown(&mut client).await;
}

/// A server older than the settings routes answers `404`: the settings are `Unsupported`, nothing spins,
/// and the limits still save through the engine routes as they always did.
#[tokio::test(flavor = "multi_thread")]
async fn old_server_settings_are_unsupported_and_limits_still_save() {
    let limits_bodies: Arc<Mutex<Vec<Value>>> = Arc::default();
    let seen = Arc::clone(&limits_bodies);
    let fake = Fake::plain();
    let addr = serve(
        api_routes(get(events))
            .route(
                "/api/v1/session/limits",
                put(move |Json(body): Json<Value>| {
                    let seen = Arc::clone(&seen);
                    async move {
                        seen.lock().expect("the test state").push(body);
                        StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/api/v1/session/queue",
                put(|Json(_): Json<Value>| async { StatusCode::NO_CONTENT }),
            )
            .with_state(fake),
    )
    .await;
    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::None, None, None),
        Secret::None,
        None,
    );
    wait_for_state(&mut client, true).await;

    assert!(client.send(Command::Settings(SettingsCommand::Load)));
    wait_for(&mut client, |snapshot| {
        (snapshot.settings == SettingsState::Unsupported).then_some(())
    })
    .await;
    // Still connected: a missing route is not a lost link.
    wait_for_state(&mut client, true).await;

    // A save says the server is too old.
    assert!(client.send(Command::Settings(SettingsCommand::Save(
        SettingsChange::Speed(fbtt_core::LiveLimits {
            download_kib: 50,
            upload_kib: 0,
            active_downloads: None,
            active_seeds: None,
            active_total: None,
        })
    ))));
    let lines = notices_until(&mut client, |lines| {
        lines.iter().any(|line| line.contains("too old"))
    })
    .await;
    assert!(
        lines.iter().any(|line| line
            == "This server is too old to change its settings here. Update it to change them."),
        "{lines:?}"
    );

    // And the old path works.
    assert!(client.send(Command::SetLimits(fbtt_core::LiveLimits {
        download_kib: 50,
        upload_kib: 0,
        active_downloads: None,
        active_seeds: None,
        active_total: None,
    })));
    within(async {
        loop {
            if !limits_bodies.lock().expect("the test state").is_empty() {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    let bodies = limits_bodies.lock().expect("the test state").clone();
    assert_eq!(bodies.len(), 1, "{bodies:?}");

    shutdown(&mut client).await;
}

/// A Default folder changed from a remote screen is where the next add with no folder goes: the server's
/// roots stay where they were, so the backend follows the answer of the save, not the first root.
#[tokio::test(flavor = "multi_thread")]
async fn remote_add_without_dir_follows_a_changed_default_folder() {
    let lib = Library::new("remote_default_follow", vec![alice()], vec![]);
    let (server, addr) = lib.serve().await;
    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::Password, Some("alice"), None),
        Secret::Password("hunter2".to_string().into()),
        None,
    );
    let connected = wait_for_state(&mut client, true).await;
    assert_eq!(connected.default_dir, lib.config.download_dir);

    // A folder inside the one root the server serves.
    let new = lib.config.download_dir.join("elsewhere");
    assert!(client.send(Command::Settings(SettingsCommand::Save(
        SettingsChange::Downloads(fbtt_core::DownloadsChange {
            default_dir: new.clone(),
            unfinished_dir: None,
            start_paused: false,
            watch_dir: None,
            on_complete: None,
        })
    ))));
    let shown = wait_for(&mut client, |snapshot| {
        (snapshot.default_dir == new).then(|| snapshot.default_dir.clone())
    })
    .await;
    assert_eq!(shown, new, "the snapshot shows the new default");

    let uri = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=follows";
    let id = TorrentId::from_wire_hash(
        fbt::Magnet::parse(uri)
            .expect("a magnet")
            .info_hashes
            .wire_hash(),
    );
    assert!(client.send(Command::Add {
        source: AddSource::Magnet(uri.to_string()),
        dir: None,
    }));
    let row = wait_for(&mut client, |snapshot| {
        snapshot.torrents.iter().find(|row| row.id == id).cloned()
    })
    .await;
    assert!(
        row.save_path.starts_with(&new) || row.save_path.ends_with("elsewhere"),
        "the add landed in the new default folder: {:?}",
        row.save_path
    );

    shutdown(&mut client).await;
    within(server.shutdown()).await.expect("the last save ran");
}
