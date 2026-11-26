//! The scaffolding more than one integration test needs: a library in a temporary directory, sessions
//! on loopback with nothing that could leave the machine, a torrent hashed from a file of known bytes,
//! and the waits that fail with a message instead of hanging.
//!
//! `tests/local.rs` and `tests/downloads.rs` both start a backend this way, and the two ask the same
//! questions of it, so this module holds the shared parts and each test file keeps the helpers only it
//! uses. Each integration test target compiles its own copy of these (`mod support;`), which is why the
//! set may carry a helper one target has no use for. Everything here is written against a session on
//! `127.0.0.1:0` with the DHT, LSD, UPnP and NAT-PMP off: no test reaches the network.
#![allow(dead_code)] // each test binary uses part of the set, not the whole of it

use std::fs;
use std::future::Future;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fbtt_core::scratch::ScratchDir;
use fbtt_core::{
    AddSource, Client, Command, Config, LibraryLock, LocalBackend, Notice, Paths, RowState,
    Snapshot, TorrentId, TorrentRow,
};
use tokio::task::JoinHandle;

/// How long a test waits for anything from the backend.
pub const DEADLINE: Duration = Duration::from_secs(60);
/// How long the engine's own status is polled at, when there is no snapshot to
/// wait on (the seed has none of ours).
pub const POLL: Duration = Duration::from_millis(50);
/// A 1 MiB download, big enough that pieces and rates mean something.
pub const MIB: usize = 1 << 20;

/// Runs `what` under the deadline, so a backend that never answers is a failure
/// with a message rather than a hung test job.
pub async fn within<T>(what: impl Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
}

/// Waits for the backend to publish a different snapshot; false once it has stopped.
pub async fn changed(client: &mut Client) -> bool {
    client.changed().changed().await.is_ok()
}

/// The settings every session in these tests runs on: loopback on a port the OS
/// picks, and no DHT, LSD, UPnP or NAT-PMP. Before the proxy, as a run hands them in: the backend
/// applies the config's proxy itself and keeps these as what a proxy removed at runtime goes back to.
pub fn loopback(config: &Config) -> fbt::Settings {
    let mut settings = config.settings_before_proxy();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    // The engine holds local peers outside the global rate limit unless it is told otherwise, and every
    // peer any of these tests has is on loopback. So a test that throttles a download - which is what
    // `tests/downloads.rs` does to look at a torrent mid-download - gets no throttle at all without this.
    settings.rate_limit_local_peers = true;
    settings
}

/// Deterministic file content, so a download can be compared with what was
/// seeded. A xorshift keeps it varied without a dependency.
pub fn sample(len: usize) -> Vec<u8> {
    sample_for(len, "")
}

/// `sample`'s content shifted by `tag`: two libraries seeded under two tags hold two
/// different torrents, not one torrent in two folders.
///
/// The info hash is a hash of the content (and the names), so a payload every test shares gives
/// every test the same torrent - and a hook that fires per torrent, like the ones in
/// `crates/fbtt-core/src/downloads/test_support.rs`, then arms one test's torrent and fires on
/// another's. Folding the tag into the xorshift's starting state costs nothing and makes each
/// test's hashes its own.
pub fn sample_for(len: usize, tag: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state: u32 = 0x2545_f491;
    for byte in tag.as_bytes() {
        // The same mixing the generator below runs, applied once per tag byte: a different tag
        // is a different point on the same orbit, not a small offset from another's.
        state ^= u32::from(*byte);
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
    }
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Writes `bytes` to `<dir>/<name>`, making the directory first, and returns the
/// path written.
pub fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).expect("make a directory for a test file");
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write a test file");
    path
}

/// The `.torrent` for the file or directory at `path`, hashed off the task that
/// waits for it.
pub async fn torrent_for(path: &Path) -> Vec<u8> {
    let path = path.to_path_buf();
    within(tokio::task::spawn_blocking(move || {
        fbt::CreateTorrent::new(path)
            .build(&mut |_done, _total| ControlFlow::Continue(()))
            .expect("hash the test torrent")
    }))
    .await
    .expect("the hashing task ran")
}

/// The 20-byte hash a snapshot row's id stands for.
pub fn wire_of(id: &TorrentId) -> fbt::InfoHashV1 {
    id.0.parse().expect("a torrent id is 40 hex digits")
}

/// The id the backend gives a torrent with these info hashes.
pub fn id_of(info_hashes: &fbt::InfoHashes) -> TorrentId {
    TorrentId::from_wire_hash(info_hashes.wire_hash())
}

/// A library in its own temporary directory: `config/`, `data/` and the place
/// downloads go, plus the config that points at them. The directory goes when the
/// test that made it is done.
#[derive(Debug)]
pub struct Library {
    pub dir: ScratchDir,
    pub paths: Paths,
    pub config: Config,
}

impl Library {
    pub fn new(tag: &str) -> Library {
        let dir = ScratchDir::new(tag);
        Library {
            paths: Paths {
                config_dir: dir.path().join("config"),
                data_dir: dir.path().join("data"),
            },
            config: Config {
                download_dir: dir.path().join("downloads"),
                ..Config::default()
            },
            dir,
        }
    }

    /// Takes the library lock and starts a backend on it. The returned session is
    /// the same engine the backend drives, so a test can point it at a peer.
    pub async fn start(&self) -> (Client, JoinHandle<Result<(), String>>, fbt::Session) {
        let (client, task, session, _) = self.start_with_automation(self.config.clone()).await;
        (client, task, session)
    }

    /// The same, on a config the caller changed first: which settings a run works on is what its saves
    /// write back, and some tests want a library whose config chose to serve (see
    /// `limits_save_keeps_allowed_roots_default`).
    pub async fn start_on(
        &self,
        config: Config,
    ) -> (Client, JoinHandle<Result<(), String>>, fbt::Session) {
        let (client, task, session, _) = self.start_with_automation(config).await;
        (client, task, session)
    }

    /// [`start_on`](Self::start_on), plus the handle on the downloads automation the backend started
    /// beside its engine: what `tests/downloads.rs` changes with [`AutomationHandle::update`] and reads
    /// complaints back from. The backend holds its own handle, so dropping this one changes nothing.
    ///
    /// [`AutomationHandle::update`]: fbtt_core::downloads::AutomationHandle::update
    pub async fn start_with_automation(
        &self,
        config: Config,
    ) -> (
        Client,
        JoinHandle<Result<(), String>>,
        fbt::Session,
        fbtt_core::downloads::AutomationHandle,
    ) {
        let lock = LibraryLock::acquire(&self.paths.lock_file()).expect("lock the library");
        within(LocalBackend::start_for_test(
            loopback(&config),
            config,
            &self.paths,
            lock,
        ))
        .await
        .expect("start the backend")
    }
}

/// Waits for a snapshot matching `ready`, and fails if the backend stops first.
pub async fn wait_for(client: &mut Client, ready: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    within(async {
        loop {
            let snapshot = client.snapshot();
            if ready(&snapshot) {
                return snapshot;
            }
            if !changed(client).await {
                panic!("the backend stopped before the snapshot said what we waited for");
            }
        }
    })
    .await
}

/// The row for `id`, if the snapshot lists it.
pub fn row_of(snapshot: &Snapshot, id: &TorrentId) -> Option<TorrentRow> {
    snapshot.torrents.iter().find(|row| row.id == *id).cloned()
}

/// The next notice the backend sent. Notices arrive in the order they were
/// raised, so a test can read them one by one.
pub async fn notice(client: &mut Client) -> Notice {
    within(client.notice())
        .await
        .expect("the backend still has its notices open")
}

pub fn error_text(notice: Notice) -> String {
    match notice {
        Notice::Error(text) => text,
        other => panic!("expected an error notice, got {other:?}"),
    }
}

pub fn info_text(notice: Notice) -> String {
    match notice {
        Notice::Info(text) => text,
        other => panic!("expected an info notice, got {other:?}"),
    }
}

/// Asks the backend to stop and waits for it: its task only resolves once the state file is
/// written and the library lock is released. Its `Err` is the message the binary would print,
/// so a test that expected the save to work fails on it rather than on a missing file later.
pub async fn shutdown(client: Client, task: JoinHandle<Result<(), String>>) {
    assert!(client.send(Command::Shutdown), "the queue takes Shutdown");
    let outcome = within(task)
        .await
        .expect("the backend stopped cleanly, with its state written");
    assert_eq!(outcome, Ok(()), "the last save reported no problem");
}

/// Waits for `session` to have a torrent that is seeding: the seed must have its
/// data checked before it has anything to give a peer.
pub async fn wait_until_seeding(session: &fbt::Session) {
    within(async {
        loop {
            if session
                .torrents_status()
                .iter()
                .any(|status| status.state == fbt::TorrentState::Seeding)
            {
                return;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await
}

/// A library with one complete torrent already on disk: it seeds itself without
/// a peer, so the backend's check is the only thing to wait for. Returns the
/// row id and the file's path.
pub async fn seeding_library(
    tag: &str,
) -> (
    Library,
    Client,
    JoinHandle<Result<(), String>>,
    TorrentId,
    PathBuf,
) {
    let lib = Library::new(tag);
    // The tag names the payload too: a library seeded under this tag holds a torrent nobody else's
    // hooks are armed for (see [`sample_for`]).
    let file = write_file(
        &lib.config.download_dir,
        "payload.bin",
        &sample_for(MIB, tag),
    );
    let bytes = torrent_for(&file).await;
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    let id = id_of(&metainfo.info_hashes);
    let (mut client, task, _) = lib.start().await;
    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes),
            dir: None,
        }),
        "the queue takes the torrent"
    );
    assert_eq!(info_text(notice(&mut client).await), "Added payload.bin");
    wait_for(&mut client, |snapshot| {
        row_of(snapshot, &id).is_some_and(|row| row.state == RowState::Seeding)
    })
    .await;
    (lib, client, task, id, file)
}
