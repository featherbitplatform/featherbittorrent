//! The local backend end to end: a library in a temporary directory, sessions
//! on loopback with nothing that could leave the machine, and a 60 s deadline on
//! every wait so a backend that stops answering fails instead of hanging.

use std::fs;
use std::future::Future;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fbtt_core::scratch::ScratchDir;
use fbtt_core::{
    AddSource, Client, Command, Config, ConfigLoad, Connection, Detail, DetailTab, FilePriority,
    FileRow, LibraryLock, LiveLimits, LocalBackend, Notice, Paths, RowState, Snapshot, TorrentId,
    TorrentRow,
};
use tokio::task::JoinHandle;

/// How long a test waits for anything from the backend.
const DEADLINE: Duration = Duration::from_secs(60);
/// How long the engine's own status is polled at, when there is no snapshot to
/// wait on (the seed has none of ours).
const POLL: Duration = Duration::from_millis(50);
/// A 1 MiB download, big enough that pieces and rates mean something.
const MIB: usize = 1 << 20;

/// Runs `what` under the deadline, so a backend that never answers is a failure
/// with a message rather than a hung test job.
async fn within<T>(what: impl Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
}

/// Waits for the backend to publish a different snapshot; false once it has stopped.
async fn changed(client: &mut Client) -> bool {
    client.changed().changed().await.is_ok()
}

/// The settings every session in these tests runs on: loopback on a port the OS
/// picks, and no DHT, LSD, UPnP or NAT-PMP.
fn loopback(config: &Config) -> fbt::Settings {
    let mut settings = config.session_settings();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    settings
}

/// Deterministic file content, so a download can be compared with what was
/// seeded. A xorshift keeps it varied without a dependency.
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

/// Writes `bytes` to `<dir>/<name>`, making the directory first, and returns the
/// path written.
fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).expect("make a directory for a test file");
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write a test file");
    path
}

/// The `.torrent` for the file or directory at `path`, hashed off the task that
/// waits for it.
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

/// `path` with `suffix` on its file name: `session.state` becomes
/// `session.state.bad`.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or(path.as_os_str()).to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// The 20-byte hash a snapshot row's id stands for.
fn wire_of(id: &TorrentId) -> fbt::InfoHashV1 {
    id.0.parse().expect("a torrent id is 40 hex digits")
}

/// The id the backend gives a torrent with these info hashes.
fn id_of(info_hashes: &fbt::InfoHashes) -> TorrentId {
    TorrentId::from_wire_hash(info_hashes.wire_hash())
}

/// A library in its own temporary directory: `config/`, `data/` and the place
/// downloads go, plus the config that points at them. The directory goes when
/// the test that made it is done.
struct Library {
    dir: ScratchDir,
    paths: Paths,
    config: Config,
}

impl Library {
    fn new(tag: &str) -> Library {
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
    async fn start(&self) -> (Client, JoinHandle<()>, fbt::Session) {
        let lock = LibraryLock::acquire(&self.paths.lock_file()).expect("lock the library");
        within(LocalBackend::start_for_test(
            loopback(&self.config),
            self.config.clone(),
            &self.paths,
            lock,
        ))
        .await
        .expect("start the backend")
    }
}

/// Waits for a snapshot matching `ready`, and fails if the backend stops first.
async fn wait_for(client: &mut Client, ready: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
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
fn row_of(snapshot: &Snapshot, id: &TorrentId) -> Option<TorrentRow> {
    snapshot.torrents.iter().find(|row| row.id == *id).cloned()
}

/// The watched file list for `id`, when that is what the detail pane holds.
fn files_of(snapshot: &Snapshot, id: &TorrentId) -> Option<Vec<FileRow>> {
    match snapshot.detail.as_ref() {
        Some((watched, Detail::Files(rows))) if watched == id => Some(rows.clone()),
        _ => None,
    }
}

/// The next notice the backend sent. Notices arrive in the order they were
/// raised, so a test can read them one by one.
async fn notice(client: &mut Client) -> Notice {
    within(client.notice())
        .await
        .expect("the backend still has its notices open")
}

fn error_text(notice: Notice) -> String {
    match notice {
        Notice::Error(text) => text,
        other => panic!("expected an error notice, got {other:?}"),
    }
}

fn info_text(notice: Notice) -> String {
    match notice {
        Notice::Info(text) => text,
        other => panic!("expected an info notice, got {other:?}"),
    }
}

/// Asks the backend to stop and waits for it: its task only resolves once the
/// state file is written and the library lock is released.
async fn shutdown(client: Client, task: JoinHandle<()>) {
    assert!(client.send(Command::Shutdown), "the queue takes Shutdown");
    within(task)
        .await
        .expect("the backend stopped cleanly, with its state written");
}

/// Waits for `session` to have a torrent that is seeding: the seed must have its
/// data checked before it has anything to give a peer.
async fn wait_until_seeding(session: &fbt::Session) {
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
async fn seeding_library(tag: &str) -> (Library, Client, JoinHandle<()>, TorrentId, PathBuf) {
    let lib = Library::new(tag);
    let file = write_file(&lib.config.download_dir, "payload.bin", &sample(MIB));
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

/// A torrent added from a `.torrent` downloads the bytes a second session is
/// seeding, and the file on disk is what was seeded.
#[tokio::test(flavor = "multi_thread")]
async fn downloads_from_local_seed() {
    let lib = Library::new("local_seed");
    let seed_dir = lib.dir.path().join("seed");
    let seeded = write_file(&seed_dir, "payload.bin", &sample(MIB));
    let bytes = torrent_for(&seeded).await;
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    let id = id_of(&metainfo.info_hashes);

    let seed = fbt::Session::new(loopback(&lib.config))
        .await
        .expect("a session to seed with");
    seed.add_torrent(fbt::AddTorrent::new(metainfo, seed_dir))
        .await
        .expect("seed the torrent");
    wait_until_seeding(&seed).await;
    let seed_addr = seed.local_addr();

    let (mut client, task, engine) = lib.start().await;
    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes),
            dir: Some(lib.config.download_dir.clone()),
        }),
        "the queue takes the torrent"
    );
    assert_eq!(info_text(notice(&mut client).await), "Added payload.bin");

    // The row appears first, so there is a torrent to hand the seed's address. Its size
    // fills in only once the engine has its layout ready, so the assertions on bytes
    // belong after the download instead of here.
    let started = wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    let row = row_of(&started, &id).expect("the added torrent");
    assert_eq!(row.save_path, lib.config.download_dir);
    drop(started);

    let torrent = engine
        .torrent(&fbt::InfoHashes::from_v1(wire_of(&id)))
        .expect("the session holds the torrent it added");
    let done = within(async {
        loop {
            if let Some(row) = row_of(&client.snapshot(), &id)
                && row.state == RowState::Seeding
                && row.progress == 1.0
            {
                break row;
            }
            torrent
                .add_peer(seed_addr)
                .await
                .expect("the seed is a peer");
            // Take the next snapshot when one comes, and look again shortly anyway: a
            // download that is getting nothing publishes nothing.
            let _ = tokio::time::timeout(POLL, changed(&mut client)).await;
        }
    })
    .await;

    assert_eq!(done.size, MIB as u64, "the whole file is wanted");
    assert_eq!(done.done, done.size, "every wanted byte arrived");
    // The peer count is not asserted: once we hold every piece, the engine drops a peer
    // that can only upload to us, so a finished row often shows no peers at all. That
    // the bytes came over the network rather than from data already on disk is shown by
    // what the session received, and by the file matching the seed's below.
    let status = torrent.status();
    assert!(
        status.total_downloaded > 0,
        "pieces arrived from the seed peer"
    );
    assert_eq!(status.info_hashes.wire_hash(), wire_of(&id).0);
    let snapshot = client.snapshot();
    assert_eq!(snapshot.connection, Connection::Local);
    assert_eq!(snapshot.default_dir, lib.config.download_dir);
    // The DHT is off in these tests, and the header shows that as no count.
    assert!(snapshot.totals.dht_nodes.is_none());
    drop(snapshot);

    let downloaded = done.save_path.join(&done.name);
    assert_eq!(
        fs::read(&downloaded).expect("the downloaded file"),
        sample(MIB),
        "the bytes on disk are the bytes that were seeded"
    );

    shutdown(client, task).await;
    seed.shutdown().await;
}

/// What one run added, the next run knows: the state file is the library's memory.
#[tokio::test(flavor = "multi_thread")]
async fn state_survives_restart() {
    let lib = Library::new("local_restart");
    let uri = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=restart-me";
    let magnet = fbt::Magnet::parse(uri).expect("a magnet with a made-up hash");
    let id = id_of(&magnet.info_hashes);

    let (mut client, task, _) = lib.start().await;
    assert!(
        client.send(Command::Add {
            source: AddSource::Magnet(uri.to_string()),
            dir: None,
        }),
        "the queue takes the magnet"
    );
    wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    shutdown(client, task).await;

    // The same directories, a second run: the lock is free because the first
    // backend's task resolved.
    let (mut client, task, _) = lib.start().await;
    let snapshot = wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    let row = row_of(&snapshot, &id).expect("the torrent is back");
    assert_eq!(row.name, "restart-me", "its name came back with it");
    drop(snapshot);
    shutdown(client, task).await;
}

/// A state file that does not decode does not stop the run: it is kept aside
/// under a name that says so, and the library starts empty.
#[tokio::test(flavor = "multi_thread")]
async fn corrupt_state_is_kept_aside() {
    let lib = Library::new("local_corrupt");
    fs::create_dir_all(&lib.paths.data_dir).expect("the data directory");
    fs::write(lib.paths.state_file(), b"garbage").expect("write a state file that is not one");

    let (mut client, task, _) = lib.start().await;
    let problem = error_text(notice(&mut client).await);
    assert!(
        problem.contains("session.state.bad"),
        "the notice says which file was kept: {problem}"
    );
    let aside = suffixed(&lib.paths.state_file(), ".bad");
    assert!(aside.exists(), "the unreadable state is kept aside");
    assert!(!lib.paths.state_file().exists(), "it moves out of the way");
    assert!(
        client.snapshot().torrents.is_empty(),
        "nothing comes back from a file we could not read"
    );

    // The run itself is ordinary: it shuts down and writes a state file of its own.
    shutdown(client, task).await;
    assert!(lib.paths.state_file().exists(), "the new run starts fresh");
}

/// A bad magnet, then the same good magnet twice: both are refused with a notice
/// that says why, and the backend still takes commands afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn add_errors_become_notices() {
    let lib = Library::new("local_add_errors");
    let (mut client, task, _) = lib.start().await;

    assert!(
        client.send(Command::Add {
            source: AddSource::Magnet("magnet:?xt=nope".to_string()),
            dir: None,
        }),
        "a command that will fail is still queued"
    );
    let problem = error_text(notice(&mut client).await);
    assert!(!problem.is_empty(), "the bad magnet says what is wrong");

    let uri = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=twice";
    assert!(client.send(Command::Add {
        source: AddSource::Magnet(uri.to_string()),
        dir: None,
    }));
    assert_eq!(info_text(notice(&mut client).await), "Added twice");

    assert!(client.send(Command::Add {
        source: AddSource::Magnet(uri.to_string()),
        dir: None,
    }));
    let duplicate = error_text(notice(&mut client).await);
    assert!(!duplicate.is_empty(), "the second add is refused");

    let magnet = fbt::Magnet::parse(uri).expect("a valid magnet");
    let id = id_of(&magnet.info_hashes);
    assert!(client.send(Command::Pause(id.clone())));
    let snapshot = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &id).is_some_and(|row| row.state == RowState::Paused)
    })
    .await;
    drop(snapshot);

    shutdown(client, task).await;
}

/// Removing with `delete_files` takes the torrent's own file with it.
#[tokio::test(flavor = "multi_thread")]
async fn remove_with_files_deletes_them() {
    let (lib, mut client, task, id, file) = seeding_library("local_remove_delete").await;
    assert!(file.exists(), "the torrent's data was there to begin with");

    assert!(client.send(Command::Remove {
        id: id.clone(),
        delete_files: true,
    }));
    wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_none()).await;

    assert!(!file.exists(), "the removal took the file with it");
    let left: Vec<PathBuf> = fs::read_dir(&lib.config.download_dir)
        .expect("read the download directory")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert!(left.is_empty(), "nothing of the torrent is left: {left:?}");

    shutdown(client, task).await;
}

/// Removing without it leaves the data alone: a library can forget a torrent and
/// keep what it downloaded.
#[tokio::test(flavor = "multi_thread")]
async fn remove_keeps_files() {
    let (lib, mut client, task, id, file) = seeding_library("local_remove_keep").await;
    let content = fs::read(&file).expect("the torrent's file");

    assert!(client.send(Command::Remove {
        id: id.clone(),
        delete_files: false,
    }));
    wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_none()).await;

    assert!(file.exists(), "the file stays where it was");
    assert_eq!(fs::read(&file).expect("read it back"), content);
    assert!(
        lib.config.download_dir.exists(),
        "and so does the directory holding it"
    );

    shutdown(client, task).await;
}

/// Live limits change the running session and land in `config.yaml`, so the next
/// run starts with them.
#[tokio::test(flavor = "multi_thread")]
async fn set_limits_applies_and_persists() {
    let lib = Library::new("local_limits");
    let (mut client, task, _) = lib.start().await;
    let first = client.snapshot();
    assert_eq!(first.connection, Connection::Local);
    assert_eq!(first.default_dir, lib.config.download_dir);
    assert_eq!(first.limits, LiveLimits::from(lib.config.limits));
    drop(first);

    let mut limits = LiveLimits::from(lib.config.limits);
    limits.download_kib = 100;
    assert!(client.send(Command::SetLimits(limits)));

    let snapshot = wait_for(&mut client, |snapshot| snapshot.limits.download_kib == 100).await;
    assert_eq!(snapshot.limits.upload_kib, lib.config.limits.upload_kib);
    drop(snapshot);

    match Config::load(&lib.paths.config_file()).expect("read the config back") {
        ConfigLoad::Loaded(config) => {
            assert_eq!(config.limits.download_kib, 100);
            assert_eq!(config.limits.upload_kib, lib.config.limits.upload_kib);
            assert_eq!(config.limits.active_total, lib.config.limits.active_total);
            assert_eq!(
                config.download_dir, lib.config.download_dir,
                "the rest of the file is untouched"
            );
        }
        other => panic!("the new limits are in the config file, got {other:?}"),
    }

    shutdown(client, task).await;
}

/// Skipping one file of a two-file torrent shows up in the watched file list:
/// pad files are not listed, and only the named file changes.
#[tokio::test(flavor = "multi_thread")]
async fn set_file_priority_skip() {
    let lib = Library::new("local_priorities");
    // The torrent is made from its own directory and downloads elsewhere, so
    // nothing is on disk yet and no peer is needed.
    let source = lib.dir.path().join("payload");
    write_file(&source, "a.bin", &sample(64 * 1024 + 7));
    write_file(&source, "b.bin", &sample(33 * 1024 + 5));
    let bytes = torrent_for(&source).await;
    let id = id_of(
        &fbt::Metainfo::from_bytes(&bytes)
            .expect("the torrent")
            .info_hashes,
    );

    let (mut client, task, _) = lib.start().await;
    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes),
            dir: Some(lib.config.download_dir.clone()),
        }),
        "the queue takes the torrent"
    );
    assert_eq!(
        info_text(notice(&mut client).await),
        "Added payload",
        "a multi-file torrent is named for its directory"
    );

    assert!(client.send(Command::Watch(Some((id.clone(), DetailTab::Files)))));
    let snapshot = wait_for(&mut client, |snapshot| {
        files_of(snapshot, &id).is_some_and(|rows| rows.len() == 2)
    })
    .await;
    let rows = files_of(&snapshot, &id).expect("the file list");
    drop(snapshot);
    assert_eq!(
        rows.len(),
        2,
        "the torrent's two files, and no pad file: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.path.ends_with("a.bin")),
        "both real files are listed: {rows:?}"
    );
    let skipped = rows
        .iter()
        .find(|row| row.path.ends_with("b.bin"))
        .expect("the second file is listed");
    assert_eq!(skipped.size, 33 * 1024 + 5);
    assert_eq!(skipped.done, 0, "nothing of it is here yet");
    assert_eq!(skipped.priority, FilePriority::Normal);
    let index = skipped.index;

    assert!(client.send(Command::SetFilePriority {
        id: id.clone(),
        file: index,
        priority: FilePriority::Skip,
    }));
    let snapshot = wait_for(&mut client, |snapshot| {
        files_of(snapshot, &id).is_some_and(|rows| {
            rows.len() == 2
                && rows
                    .iter()
                    .any(|row| row.index == index && row.priority == FilePriority::Skip)
                && rows
                    .iter()
                    .any(|row| row.index != index && row.priority == FilePriority::Normal)
        })
    })
    .await;
    drop(snapshot);

    // The other tabs answer too, on the same watch mechanism.
    for tab in [DetailTab::Peers, DetailTab::Trackers, DetailTab::Pieces] {
        assert!(client.send(Command::Watch(Some((id.clone(), tab)))));
        let snapshot = wait_for(&mut client, |snapshot| match snapshot.detail.as_ref() {
            Some((watched, detail)) if *watched == id => match detail {
                Detail::Peers(_) | Detail::Trackers(_) => true,
                Detail::Pieces { have } => !have.is_empty(),
                Detail::Files(_) => false,
            },
            _ => false,
        })
        .await;
        drop(snapshot);
    }

    shutdown(client, task).await;
}
