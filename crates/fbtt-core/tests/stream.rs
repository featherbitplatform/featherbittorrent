//! The local streaming endpoint end to end: a library on loopback and the
//! file-stream route a play target names. A complete file plays as a path;
//! a file the leecher does not have yet plays as a stream, its ranges served
//! as the pieces arrive, and its token opens only the one file it was issued
//! for.

use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use fbtt_core::scratch::ScratchDir;
use fbtt_core::{
    AddSource, Client, Command, Config, LibraryLock, LocalBackend, Notice, Paths, PlaySource,
    PlayTarget, RowState, TorrentId,
};
use tokio::task::JoinHandle;

/// How long a test waits for anything from the backend.
const DEADLINE: Duration = Duration::from_secs(60);
/// A small file, so a piece or two is enough to start streaming.
const KB: usize = 1 << 10;
/// Two files for the torrent, so a token scoped to one can be shown to refuse the other.
const FILE_0: &str = "first.bin";
const FILE_1: &str = "second.bin";

/// Runs `what` under the deadline, so a backend that never answers is a failure
/// with a message rather than a hung test job.
async fn within<T>(what: impl Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
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

/// Deterministic file content, so a download can be compared with what was seeded.
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

/// Writes `bytes` to `<dir>/<name>`, making the directory first, and returns the path written.
fn write_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).expect("make a directory for a test file");
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write a test file");
    path
}

/// The `.torrent` for the file or directory at `path`, hashed off the task that waits for it.
async fn torrent_for(path: &std::path::Path) -> Vec<u8> {
    let path = path.to_path_buf();
    within(tokio::task::spawn_blocking(move || {
        fbt::CreateTorrent::new(path)
            .build(&mut |_done, _total| std::ops::ControlFlow::Continue(()))
            .expect("hash the test torrent")
    }))
    .await
    .expect("the hashing task ran")
}

/// The 20-byte hash a snapshot row's id stands for.
fn wire_of(id: &TorrentId) -> fbt::InfoHashV1 {
    id.0.parse().expect("a torrent id is 40 hex digits")
}

/// A library in its own temporary directory, plus the config that points at it.
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
    async fn start(&self) -> (Client, JoinHandle<Result<(), String>>, fbt::Session) {
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

/// The next notice the backend sent.
async fn notice(client: &mut Client) -> Notice {
    within(client.notice())
        .await
        .expect("the backend still has its notices open")
}

fn info_text(notice: Notice) -> String {
    match notice {
        Notice::Info(text) => text,
        other => panic!("expected an info notice, got {other:?}"),
    }
}

/// The `Error(text)` a command answers with, taken from the notice queue.
fn error_text(notice: Notice) -> String {
    match notice {
        Notice::Error(text) => text,
        other => panic!("expected an error notice, got {other:?}"),
    }
}

/// The row for `id`, if the snapshot lists it.
fn row_of(snapshot: &fbtt_core::Snapshot, id: &TorrentId) -> Option<fbtt_core::TorrentRow> {
    snapshot.torrents.iter().find(|row| row.id == *id).cloned()
}

/// Waits for a snapshot matching `ready`, and fails if the backend stops first.
async fn wait_for(
    client: &mut Client,
    ready: impl Fn(&fbtt_core::Snapshot) -> bool,
) -> std::sync::Arc<fbtt_core::Snapshot> {
    within(async {
        loop {
            let snapshot = client.snapshot();
            if ready(&snapshot) {
                return snapshot;
            }
            if client.changed().changed().await.is_err() {
                panic!("the backend stopped before the snapshot said what we waited for");
            }
        }
    })
    .await
}

/// Asks the backend to stop and waits for it.
async fn shutdown(client: Client, task: JoinHandle<Result<(), String>>) {
    assert!(client.send(Command::Shutdown), "the queue takes Shutdown");
    let outcome = within(task)
        .await
        .expect("the backend stopped cleanly, with its state written");
    assert_eq!(outcome, Ok(()), "the last save reported no problem");
}

/// The `Play(PlayTarget)` a `Stream` command answers with, taken from the notice queue.
async fn play_target(client: &mut Client) -> PlayTarget {
    loop {
        if let Notice::Play(target) = notice(client).await {
            return target;
        }
    }
}

/// A directory of two files, and the `.torrent` and id for it. The files are
/// in `<dir>/src/`, the torrent is made from `src/` (a multi-file torrent
/// named "src"), and the save path is `<dir>/` so the files land at
/// `<dir>/src/first.bin` and `<dir>/src/second.bin`.
async fn make_torrent(dir: &std::path::Path) -> (Vec<u8>, TorrentId, PathBuf, PathBuf) {
    let src = dir.join("src");
    let first = write_file(&src, FILE_0, &sample(32 * KB + 7));
    let second = write_file(&src, FILE_1, &sample(17 * KB + 11));
    let bytes = torrent_for(&src).await;
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    let id = TorrentId::from_wire_hash(metainfo.info_hashes.wire_hash());
    (bytes, id, first, second)
}

/// A complete local file plays as its path, not a stream: after `Seeding`, the
/// notice is a `PlaySource::Path` equal to the file on disk.
#[tokio::test(flavor = "multi_thread")]
async fn local_complete_file_plays_path() {
    let lib = Library::new("local_stream_path");
    // The files are in `download_dir/src/`, the torrent is made from `src/`,
    // and the save path is `download_dir/`.
    let (bytes, id, _, _) = make_torrent(&lib.config.download_dir).await;

    let (mut client, task, _) = lib.start().await;
    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes),
            dir: None,
        }),
        "the queue takes the torrent"
    );
    assert_eq!(info_text(notice(&mut client).await), "Added src");

    // The data is on disk, so the torrent seeds itself without a peer.
    wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    wait_for(&mut client, |snapshot| {
        row_of(snapshot, &id)
            .is_some_and(|row| row.state == RowState::Seeding && row.progress == 1.0)
    })
    .await;
    let row = row_of(&client.snapshot(), &id).expect("the added torrent");

    // The file is whole on disk now: a path, and the path of the file the torrent
    // saved. The torrent is named "src", so the file is at
    // `save_path/src/first.bin`.
    assert!(client.send(Command::Stream {
        id: id.clone(),
        file: Some(0),
    }));
    let target = play_target(&mut client).await;
    match target.source() {
        PlaySource::Path(path) => {
            let expected = row.save_path.join("src").join(FILE_0);
            assert_eq!(path, &expected, "the path is the file on disk");
            assert_eq!(
                fs::read(path).expect("the file is on disk"),
                sample(32 * KB + 7),
                "and it holds the bytes that were written"
            );
        }
        other => panic!("a complete file plays as a path, got {other:?}"),
    }

    shutdown(client, task).await;
}

/// A `.torrent` whose only file is a BEP 47 pad, so the torrent has no playable
/// file. The pad occupies one 16 KiB piece of zeros (a pad holds no content, so
/// its piece hashes to 16 KiB of zero bytes) and is never written to disk. The
/// name is given so the torrent's notice can be recognised.
fn pad_torrent(name: &str) -> (Vec<u8>, TorrentId) {
    let pad_len: u64 = 16_384;
    // A multi-file torrent whose only file is a BEP 47 pad: the `files` list
    // has one entry with `attr: p`, so the parser marks it a pad. The pad's
    // piece is 16 KiB of zeros (a pad holds no content), so `pieces` is one
    // 20-byte digest of zeros. The file is never written to disk.
    let mut file_entry = String::new();
    file_entry.push('d');
    file_entry.push_str("6:length");
    file_entry.push_str(&format!("i{pad_len}e"));
    file_entry.push_str("4:path");
    file_entry.push('l');
    file_entry.push_str(&format!("{}:{name}", name.len()));
    file_entry.push_str("4:.pad");
    file_entry.push('e');
    file_entry.push_str("4:attr1:p");
    file_entry.push('e');
    let files_list = format!("l{file_entry}e");
    let mut info = String::new();
    info.push('d');
    info.push_str("5:files");
    info.push_str(&files_list);
    info.push_str("4:name");
    info.push_str(&format!("{}:{name}", name.len()));
    info.push_str("12:piece length");
    info.push_str(&format!("i{pad_len}e"));
    info.push_str("6:pieces20:");
    info.extend(std::iter::repeat_n('\0', 20));
    info.push('e');
    let mut torrent = String::new();
    torrent.push('d');
    torrent.push_str("4:info");
    torrent.push_str(&info);
    torrent.push('e');
    let metainfo = fbt::Metainfo::from_bytes(torrent.as_bytes()).expect("the pad torrent parses");
    let id = TorrentId::from_wire_hash(metainfo.info_hashes.wire_hash());
    (torrent.into_bytes(), id)
}

/// A torrent with no playable file says so, and a stream of a pad file serves
/// nothing. The torrent's only file is a BEP 47 pad, so `choose_file` finds no
/// non-pad file and the `Stream` command answers with an error, not a play
/// target.
#[tokio::test(flavor = "multi_thread")]
async fn local_all_pad_torrent_reports_no_playable_file() {
    let lib = Library::new("local_stream_pad");
    let (bytes, id) = pad_torrent("pads");

    let (mut client, task, _) = lib.start().await;
    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes),
            dir: None,
        }),
        "the queue takes the torrent"
    );
    assert_eq!(info_text(notice(&mut client).await), "Added pads");

    // The torrent is in the snapshot now: the metadata is parsed, and the
    // pad file is recognised as a pad. The torrent may not reach Seeding
    // (a pad is never written to disk), so we wait only for the row.
    wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;

    // No playable file: an error, and the error says the torrent has no playable file.
    assert!(client.send(Command::Stream {
        id: id.clone(),
        file: None,
    }));
    assert_eq!(
        error_text(notice(&mut client).await),
        "the torrent has no playable file",
        "a pad-only torrent says it has no playable file"
    );

    shutdown(client, task).await;
}

/// A file the leecher does not have yet plays as a stream, and the stream serves
/// its ranges as the pieces arrive: `206` for a range, `416` for one past the end,
/// and the first bytes of the file once they come.
///
/// The torrent is added without its data on disk (the download directory is
/// empty), so the engine does not have the file. A separate seed session
/// provides the pieces; the leecher streams while it downloads.
#[tokio::test(flavor = "multi_thread")]
async fn local_partial_file_streams_ranges() {
    let lib = Library::new("local_stream_ranges");
    // Write the files to a separate directory for the seed, not to the
    // download directory the leecher uses.
    let seed_dir = lib.dir.path().join("seed");
    let (bytes, id, _, _) = make_torrent(&seed_dir).await;

    // The seed session: loopback, no DHT, seeds the torrent from seed_dir.
    let seed_settings = loopback(&lib.config);
    let seed = fbt::Session::new(seed_settings)
        .await
        .expect("a session to seed with");
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent");
    seed.add_torrent(fbt::AddTorrent::new(metainfo, seed_dir))
        .await
        .expect("seed the torrent");
    within(async {
        loop {
            if seed
                .torrents_status()
                .iter()
                .any(|status| status.state == fbt::TorrentState::Seeding)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let seed_addr = seed.local_addr();

    let (mut client, task, engine) = lib.start().await;
    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes),
            dir: None,
        }),
        "the queue takes the torrent"
    );
    assert_eq!(info_text(notice(&mut client).await), "Added src");

    wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;

    // Before the leecher has the data: a stream URL, not a path. The stream is
    // asked for first, while the leecher still has no piece of the file, so the
    // answer is a URL rather than a path to a file that would not exist.
    assert!(client.send(Command::Stream {
        id: id.clone(),
        file: Some(0),
    }));
    let target = play_target(&mut client).await;
    let url = match target.source() {
        PlaySource::Url(url) => url.as_str().to_owned(),
        other => panic!("a partial file plays as a stream, got {other:?}"),
    };
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "the stream is on loopback: {url}"
    );
    assert!(url.contains("token="), "the token is in the query: {url}");

    let len = 32 * KB + 7;
    // The range request goes out while the leecher has no piece of the file:
    // it blocks until the first piece arrives, which is what makes this a
    // stream of bytes as the pieces do, not a read of a file that is already
    // complete. The request is answered once the piece does.
    let range_client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("a loopback client");
    let response_fut = range_client.get(&url).header("Range", "bytes=0-15").send();
    let response_task = tokio::spawn(within(response_fut));

    // Only now does the leecher learn the seed, so the download starts after the
    // range request is already waiting.
    let torrent = engine
        .torrent(&fbt::InfoHashes::from_v1(wire_of(&id)))
        .expect("the session holds the torrent it added");
    let _ = torrent.add_peer(seed_addr).await;

    let response = response_task
        .await
        .expect("the range request's task ran")
        .expect("the range request is answered");
    assert_eq!(response.status(), 206, "a satisfiable range is `206`");
    let expected_range = format!("bytes 0-15/{len}");
    assert_eq!(
        response
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok()),
        Some(expected_range.as_str()),
        "the range says what it serves"
    );
    let first = within(response.bytes()).await.expect("the range body");
    assert_eq!(first.len(), 16, "the range's sixteen bytes");
    assert_eq!(
        first,
        sample(32 * KB + 7)[..16],
        "the bytes are the seed's, once the piece arrived"
    );

    // A range past the end: `416`.
    let response = within(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("a loopback client")
            .get(&url)
            .header("Range", format!("bytes={len}-"))
            .send(),
    )
    .await
    .expect("the oversized range is answered");
    assert_eq!(response.status(), 416, "a range past the end is `416`");

    // A 10 000-byte `Range` header: the whole file. The header fits inside
    // hyper's HTTP/1 read-buffer cap (about 408 KiB), so a 10 000-byte value is
    // not refused with `431`; fbt-api answers `200` for a `Range` that does not
    // parse to one range.
    let big_range: String = "0".repeat(10_000);
    let response = within(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("a loopback client")
            .get(&url)
            .header("Range", format!("bytes={big_range}"))
            .send(),
    )
    .await
    .expect("the oversized header is answered");
    assert_eq!(
        response.status(),
        200,
        "a `Range` that is not one range is the whole file: {}",
        response.status()
    );

    shutdown(client, task).await;
    seed.shutdown().await;
}

/// A token scoped to one file opens only that file: the same token on another
/// file's index is `401`.
#[tokio::test(flavor = "multi_thread")]
async fn local_token_cannot_open_another_file() {
    let lib = Library::new("local_stream_token");
    // Write the files to the download directory so the torrent seeds itself,
    // The leecher has no data on disk, so it plays a stream URL.
    let seed_dir = lib.dir.path().join("seed");
    let (bytes, id, _, _) = make_torrent(&seed_dir).await;

    // The seed session.
    let seed_settings = loopback(&lib.config);
    let seed = fbt::Session::new(seed_settings)
        .await
        .expect("a session to seed with");
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent");
    seed.add_torrent(fbt::AddTorrent::new(metainfo, seed_dir))
        .await
        .expect("seed the torrent");
    within(async {
        loop {
            if seed
                .torrents_status()
                .iter()
                .any(|status| status.state == fbt::TorrentState::Seeding)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

    let (mut client, task, _) = lib.start().await;
    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes),
            dir: None,
        }),
        "the queue takes the torrent"
    );
    assert_eq!(info_text(notice(&mut client).await), "Added src");

    wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    assert!(client.send(Command::Stream {
        id: id.clone(),
        file: Some(0),
    }));
    let target = play_target(&mut client).await;
    let url = match target.source() {
        PlaySource::Url(url) => url.as_str().to_owned(),
        other => panic!("a file with no data plays as a stream, got {other:?}"),
    };

    // The URL is for file 0; the same token on file 1 is refused.
    let other = url.replacen("/files/0/stream", "/files/1/stream", 1);
    assert_ne!(other, url, "file 1's path is different");
    let response = within(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("a loopback client")
            .get(&other)
            .send(),
    )
    .await
    .expect("the other file is answered");
    assert_eq!(
        response.status(),
        401,
        "the token does not open another file"
    );

    shutdown(client, task).await;
    seed.shutdown().await;
}

/// The endpoint stops with the engine: after `Shutdown`, the port refuses
/// connections.
#[tokio::test(flavor = "multi_thread")]
async fn local_endpoint_stops_with_engine() {
    let lib = Library::new("local_stream_stop");
    // Write to a separate directory so the leecher has no data and plays a stream.
    let seed_dir = lib.dir.path().join("seed");
    let (bytes, id, _, _) = make_torrent(&seed_dir).await;

    let seed_settings = loopback(&lib.config);
    let seed = fbt::Session::new(seed_settings)
        .await
        .expect("a session to seed with");
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent");
    seed.add_torrent(fbt::AddTorrent::new(metainfo, seed_dir))
        .await
        .expect("seed the torrent");
    within(async {
        loop {
            if seed
                .torrents_status()
                .iter()
                .any(|status| status.state == fbt::TorrentState::Seeding)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

    let (mut client, task, _) = lib.start().await;
    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes),
            dir: None,
        }),
        "the queue takes the torrent"
    );
    assert_eq!(info_text(notice(&mut client).await), "Added src");

    wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    assert!(client.send(Command::Stream {
        id: id.clone(),
        file: Some(0),
    }));
    let target = play_target(&mut client).await;
    let url = match target.source() {
        PlaySource::Url(url) => url.as_str().to_owned(),
        other => panic!("a file with no data plays as a stream, got {other:?}"),
    };

    // The endpoint is up now: a connection to its port is taken.
    let addr: std::net::SocketAddr = url
        .strip_prefix("http://")
        .expect("the URL's address")
        .split('/')
        .next()
        .expect("the URL's host")
        .parse()
        .expect("the endpoint's address");
    let connected = within(tokio::net::TcpStream::connect(addr))
        .await
        .expect("the endpoint is up");
    drop(connected);

    // After the engine stops, the port refuses connections.
    shutdown(client, task).await;
    let refused = within(tokio::net::TcpStream::connect(addr)).await;
    assert!(
        refused.is_err(),
        "the port refuses connections after shutdown"
    );
    seed.shutdown().await;
}
