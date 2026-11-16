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
use axum::{Json, Router, routing::MethodRouter, routing::get, routing::post};
use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use fbtt_core::scratch::ScratchDir;
use fbtt_core::{
    AddSource, AuthConfig, Client, Command, Config, Connection, DeviceCommand, DeviceEntry,
    DirListing, LibraryLock, LinkState, LoginMethod, Notice, Paths, RemoteBackend, RemoteTarget,
    RowState, Secret, ServerConfig, TorrentId, UserEntry,
};
use fbtt_server::Server;
use fbtt_server::auth::password;
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

/// The text of a notice, whichever kind it was.
fn text(notice: Notice) -> String {
    match notice {
        Notice::Info(line) | Notice::Error(line) => line,
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

    /// Serves this library again, on `port` (0: any port). The lock is taken here and goes with the
    /// server, which holds it until its last save - so a second run starts on the state the first left,
    /// never beside it.
    async fn serve_on(&self, port: u16) -> (Server, SocketAddr) {
        let mut config = self.config.clone();
        config.server.bind = format!("127.0.0.1:{port}")
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
        server: ServerConfig {
            bind: format!("127.0.0.1:{port}")
                .parse()
                .expect("a loopback bind address"),
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
/// server gave.
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
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
/// not a client that has stopped answering.
#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_is_a_notice() {
    let lib = Library::new("remote_wrong_password", vec![alice()], vec![]);
    let (server, addr) = lib.serve().await;

    let mut client = RemoteBackend::start(
        target(addr, LoginMethod::Password, Some("alice"), None),
        Secret::Password("not the password".to_string().into()),
    );

    let lines = notices_so_far(&mut client).await;
    assert!(
        lines
            .iter()
            .any(|line| line.contains("sign-in was refused")),
        "a refused login says so: {lines:?}"
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
            client_id: None,
        },
        Secret::Password("hunter2".to_string().into()),
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
    /// Set by a test to end every session this fake handed out - which is how a client finds out its token is
    /// dead the way it does with a real server: by being refused for it.
    revoked: AtomicBool,
    /// Set once `burst_events` has written its burst of frames, so a test knows when they are behind it.
    burst_written: AtomicBool,
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
            revoked: AtomicBool::new(false),
            burst_written: AtomicBool::new(false),
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

    fn basic(user: &str, password: &str) -> Arc<Fake> {
        Fake::new(Mode::Basic(user.to_string(), password.to_string()))
    }

    /// Records a request and says whether its credential is good enough. A request with no
    /// `Authorization` at all is what a server running with auth off gets, so it is only refused once this
    /// fake has decided somebody must prove who they are.
    fn check(&self, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
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

    let mut client =
        RemoteBackend::start(target(addr, LoginMethod::None, None, None), Secret::None);
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

    // The next round finds out and logs in again within itself. Waiting for the second login is also waiting
    // for the link to have carried on working, since a renewal that worked is not trouble.
    wait_for(&mut client, |snapshot| {
        (fake.logins.load(Ordering::SeqCst) == 2
            && matches!(
                snapshot.connection,
                Connection::Remote {
                    state: LinkState::Connected,
                    ..
                }
            ))
        .then_some(())
    })
    .await;

    // Both tokens were sent, and the second one is what it sends now.
    let seen = fake.seen.lock().expect("the test state").clone();
    assert!(
        seen.contains(&format!("Bearer {first}")),
        "the dead session was tried: {seen:?}"
    );
    let renewed = fake.session_token();
    assert_ne!(renewed, first, "the second login issued a different token");
    assert!(
        seen.contains(&format!("Bearer {renewed}")),
        "and it is the credential now in use: {seen:?}"
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

    let mut client =
        RemoteBackend::start(target(addr, LoginMethod::None, None, None), Secret::None);
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

    let mut client = RemoteBackend::start(remote, Secret::KeyPassphrase(None));
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

    let mut client = RemoteBackend::start(remote, Secret::KeyPassphrase(None));
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

    let mut client = RemoteBackend::start(remote, Secret::KeyPassphrase(None));
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

// ---------------------------------------------------------------------------------------- the token route

/// `POST /token`: one assertion checked, one token issued. The gateway fakes register this on top of
/// [`api_routes`], before that router is given its state.
async fn token(State(fake): State<Arc<Fake>>, body: axum::body::Bytes) -> Response {
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

/// An ES256 assertion checked the way a provider checks one: fixed-width `r || s`, against the curve point.
///
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
