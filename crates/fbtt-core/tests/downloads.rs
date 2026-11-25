//! The `downloads:` settings acted on: the unfinished folder a torrent works in and leaves when it is
//! whole, the watch folder that adds `.torrent` files as they appear, the program a finished torrent
//! runs, and new torrents starting paused.
//!
//! Every download here runs the way `tests/local.rs::downloads_from_local_seed` does - a second session
//! on loopback seeds the payload and the engine under test is told about it as a peer - and the program a
//! finished torrent runs is `fbtt-fake-player`, the stand-in this crate builds for its tests, which writes
//! down the arguments it was handed and exits. Nothing here needs mpv or VLC, a window, or a network
//! beyond the loopback interface.
//!
//! The watch folder is polled every [`WATCH_EVERY`] and a file is only read once two scans in a row have
//! seen the same size and modification time, so the tests that drop a file wait out two of those periods
//! plus the add, bounded as always by the shared 60 s deadline.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fbtt_core::downloads::WATCH_EVERY;
use fbtt_core::{
    AddSource, Client, Command, Config, DownloadsConfig, LiveLimits, Notice, RowState, TorrentId,
    TorrentRow,
};
use tokio::task::JoinHandle;

mod support;
use support::*;

/// The stand-in player this crate builds for these tests, and the program the two program tests set as
/// `downloads.on_complete`. It is handed only the torrent's name and its folder - exactly what a completion
/// program is given - and records what it was given in `<folder>/.fbtt-fake-player.log`.
const FAKE: &str = env!("CARGO_BIN_EXE_fbtt-fake-player");
/// Where the stand-in records when it was given no `--log`: beside the folder it was run for.
const FAKE_LOG: &str = ".fbtt-fake-player.log";
/// The download rate the tests that look at a torrent mid-download set: a 1 MiB payload takes about eight
/// seconds at it, long enough for a snapshot tick or two, and the unfinished folder's part file, to be
/// there to look at, and short enough to sit well inside the shared deadline. Without a limit a loopback
/// download of this size is whole between two looks, and "the file is under the unfinished folder while it
/// downloads" would be a thing the test hopes to catch rather than sees. It binds at all because
/// `support::loopback` tells the engine to limit local peers: every peer these tests have is one.
const LIMIT_KIB: u64 = 128;
/// The first words of the line about a completion program that would not start.
const COULD_NOT_START: &str = "Could not start the program run on completion: ";

/// The config for one of these tests: the library's own, with the downloads settings it chose, and with
/// the engine writing each piece as it arrives instead of holding them in its write cache.
///
/// `Config`'s 32 MiB cache means a 1 MiB download can finish with nothing written to disk until the last
/// piece, which would make "the torrent is working in the unfinished folder" something a test could only
/// infer from the row. A zero cache is the engine's write-through mode: what has arrived is on disk, in
/// the folder the torrent is saving to, at the moment it has arrived.
fn config_with(lib: &Library, downloads: DownloadsConfig) -> Config {
    Config {
        downloads,
        cache_size_mib: 0,
        ..lib.config.clone()
    }
}

/// A second featherbittorrent holding the payload complete, and seeding it to itself: every download here
/// is started with this as its only peer. Its own config chooses no downloads setting, so the seed is not a
/// second automation competing with the one under test.
struct Seed {
    lib: Library,
    client: Client,
    task: JoinHandle<Result<(), String>>,
    addr: SocketAddr,
    /// The `.torrent` for what it is seeding, for the leecher to add.
    bytes: Vec<u8>,
    id: TorrentId,
}

impl Seed {
    async fn start(tag: &str) -> Seed {
        let (lib, client, task, id, file) = seeding_library(&format!("{tag}_seed")).await;
        let addr = client
            .session()
            .expect("a backend started for a test carries its session")
            .local_addr();
        // Hashed again from the same bytes: the same torrent, and the leecher needs the `.torrent`.
        let bytes = torrent_for(&file).await;
        Seed {
            lib,
            client,
            task,
            addr,
            bytes,
            id,
        }
    }

    /// Points the torrent `session` holds at this seed. The download then runs at whatever rate that
    /// session is under.
    async fn connect(&self, session: &fbt::Session) {
        let torrent = session
            .torrent(&fbt::InfoHashes::from_v1(wire_of(&self.id)))
            .expect("the session holds the torrent it added");
        torrent
            .add_peer(self.addr)
            .await
            .expect("the seed is a peer");
    }

    async fn stop(self) {
        let Seed {
            lib, client, task, ..
        } = self;
        shutdown(client, task).await;
        drop(lib);
    }
}

/// Adds `bytes` to the backend under test, into `dir` or into its default folder, and reads back the line
/// it reports.
async fn add(client: &mut Client, bytes: Vec<u8>, dir: Option<PathBuf>, name: &str) {
    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes),
            dir,
        }),
        "the queue takes the torrent"
    );
    assert_eq!(info_text(notice(client).await), format!("Added {name}"));
}

/// Throttles the download under test to [`LIMIT_KIB`] so that the middle of it lasts long enough to look
/// at.
fn slow_down(client: &mut Client) {
    assert!(
        client.send(Command::SetLimits(LiveLimits {
            download_kib: LIMIT_KIB,
            upload_kib: 0,
            active_downloads: None,
            active_seeds: None,
            active_total: None,
        })),
        "the queue takes the limits"
    );
}

/// Waits for the download of `id` to hold every byte: the same shape `downloads_from_local_seed` uses,
/// because a row only changes when the engine gets a piece.
async fn finish(
    client: &mut Client,
    session: &fbt::Session,
    id: &TorrentId,
    addr: SocketAddr,
) -> TorrentRow {
    let torrent = session
        .torrent(&fbt::InfoHashes::from_v1(wire_of(id)))
        .expect("the session holds the torrent it added");
    within(async {
        loop {
            if let Some(row) = row_of(&client.snapshot(), id)
                && row.state == RowState::Seeding
                && row.progress == 1.0
            {
                break row;
            }
            torrent.add_peer(addr).await.expect("the seed is a peer");
            let _ = tokio::time::timeout(POLL, changed(client)).await;
        }
    })
    .await
}

/// Waits for `path` to exist (or to stop existing), looking on the blocking pool where a directory or a
/// file's state is read: a rename the automation makes is not something this task should wait on in a
/// runtime thread.
async fn wait_for_file(path: PathBuf, want_exist: bool) {
    within(tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        loop {
            if path.exists() == want_exist {
                return;
            }
            assert!(
                started.elapsed() <= DEADLINE,
                "{} is still {} after {} seconds",
                path.display(),
                if want_exist { "absent" } else { "there" },
                DEADLINE.as_secs(),
            );
            std::thread::sleep(POLL);
        }
    }))
    .await
    .expect("the file check ran");
}

/// Waits for `dir` to hold something at all.
///
/// While a torrent downloads, the engine keeps what has arrived in its part file `.<info hash>.parts`
/// beside the torrent's own name, and only gives the file the name the torrent calls it once the pieces
/// are all there. So the question "which folder is this download working in?" is a question about a
/// folder's contents, not about a name a test would have to guess at.
async fn wait_for_entry(dir: PathBuf) {
    within(tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        loop {
            if !list_dir(&dir).is_empty() {
                return;
            }
            assert!(
                started.elapsed() <= DEADLINE,
                "{} stayed empty for {} seconds",
                dir.display(),
                DEADLINE.as_secs(),
            );
            std::thread::sleep(POLL);
        }
    }))
    .await
    .expect("the folder check ran");
}

/// What the stand-in player recorded, as the text it appended: one JSON line per program started.
async fn recorded(log: &Path) -> String {
    let log = log.to_path_buf();
    within(tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        loop {
            if let Ok(text) = fs::read_to_string(&log)
                && !text.trim().is_empty()
            {
                return text;
            }
            assert!(
                started.elapsed() <= DEADLINE,
                "the stand-in player never wrote its line to {}",
                log.display(),
            );
            std::thread::sleep(POLL);
        }
    }))
    .await
    .expect("the log reader ran")
}

/// The arguments one recording holds.
fn args_of(recording: &serde_json::Value) -> Vec<String> {
    recording["args"]
        .as_array()
        .expect("the recorded arguments")
        .iter()
        .map(|arg| arg.as_str().expect("an argument as text").to_string())
        .collect()
}

/// Reads notices until one's text satisfies `wanted`, and hands back that text. Anything else this backend
/// could report is this test's failure too, so it is raised as one.
async fn notice_where(client: &mut Client, wanted: impl Fn(&str) -> bool) -> String {
    loop {
        let text = match within(client.notice()).await {
            Some(Notice::Info(text) | Notice::Error(text)) => text,
            Some(Notice::SignInRefused(text)) => text,
            Some(Notice::Play(target)) => panic!("a play target from these settings: {target:?}"),
            None => panic!("the backend closed its notices"),
        };
        if wanted(&text) {
            return text;
        }
    }
}

/// Waits longer than the watch folder's settle rule can take: two scans of an unchanged file, plus the add
/// and the rename that follow them.
async fn wait_for_settle() {
    tokio::time::sleep(4 * WATCH_EVERY).await;
}

/// The names directly inside a folder, sorted: what a test compares against when it says a folder holds
/// one particular file, or nothing.
fn list_dir(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A `.torrent` in this library's own seed folder for a small file of known bytes, and the id it names:
/// the file a watch folder test drops, and the torrent that add is expected to put in the library.
async fn torrent_in_seed_dir(lib: &Library, tag: &str, name: &str) -> (Vec<u8>, TorrentId) {
    let payload = write_file(
        &lib.dir.path().join(format!("seed_{tag}")),
        name,
        &sample(64 * 1024),
    );
    let bytes = torrent_for(&payload).await;
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    (bytes, id_of(&metainfo.info_hashes))
}

/// A torrent added into its default folder works in the unfinished folder until it is whole, and is moved
/// back to the default folder the moment it is: while it downloads the file is under the unfinished folder
/// and nowhere else, and afterwards it is under the default folder with nothing left behind.
#[tokio::test(flavor = "multi_thread")]
async fn unfinished_then_moved_to_default_on_finish() {
    let seed = Seed::start("dl_unfinished").await;
    let lib = Library::new("dl_unfinished_leech");
    let unfinished = lib.dir.path().join("incomplete");
    let default = lib.config.download_dir.clone();
    let config = config_with(
        &lib,
        DownloadsConfig {
            unfinished_dir: Some(unfinished.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, engine) = lib.start_on(config).await;
    slow_down(&mut client);
    add(&mut client, seed.bytes.clone(), None, "payload.bin").await;

    // No peer yet, so the only thing that can move this torrent is the automation reacting to the add.
    let moved = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &seed.id).is_some_and(|row| row.save_path == unfinished)
    })
    .await;
    let row = row_of(&moved, &seed.id).expect("the added torrent");
    assert!(
        row.progress < 1.0 && row.state != RowState::Seeding,
        "it was moved before it was whole: {row:?}"
    );
    assert!(
        !default.join("payload.bin").exists(),
        "nothing has arrived in the default folder yet"
    );
    drop(moved);

    seed.connect(&engine).await;
    // The payload is still coming, and it is coming into the unfinished folder. Which folder that is
    // cannot be asked of a file name here: while a torrent downloads, the engine keeps what has arrived
    // in its part file `.<info hash>.parts` beside the torrent's own name, so what a test can look at is
    // which folder holds something.
    wait_for_entry(unfinished.clone()).await;
    let downloading = within(async {
        loop {
            let snapshot = client.snapshot();
            if let Some(row) = row_of(&snapshot, &seed.id)
                && row.done > 0
                && row.progress < 1.0
            {
                break row;
            }
            drop(snapshot);
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    assert_eq!(
        downloading.save_path, unfinished,
        "it is still working in the unfinished folder"
    );
    assert!(
        list_dir(&default).is_empty(),
        "and not in the default folder: {:?}",
        list_dir(&default)
    );
    drop(downloading);

    let done = finish(&mut client, &engine, &seed.id, seed.addr).await;
    drop(done);
    let moved_back = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &seed.id).is_some_and(|row| row.save_path == default)
    })
    .await;
    let row = row_of(&moved_back, &seed.id).expect("the moved torrent");
    assert_eq!(row.state, RowState::Seeding, "it seeds from there");
    assert_eq!(row.progress, 1.0, "with the whole torrent with it");
    drop(moved_back);

    assert_eq!(
        fs::read(default.join("payload.bin")).expect("the finished file"),
        sample(MIB),
        "the bytes on disk are the bytes that were seeded, in the default folder"
    );
    assert!(
        list_dir(&unfinished).is_empty(),
        "the unfinished folder kept nothing behind, the part file the engine wrote there included: {:?}",
        list_dir(&unfinished)
    );

    shutdown(client, task).await;
    seed.stop().await;
}

/// A torrent added into a folder the person chose works there and stays there: the unfinished folder is
/// about the default folder only, and a chosen folder is the person's own arrangement.
#[tokio::test(flavor = "multi_thread")]
async fn chosen_folder_is_not_moved() {
    let seed = Seed::start("dl_chosen").await;
    let lib = Library::new("dl_chosen_leech");
    let unfinished = lib.dir.path().join("incomplete");
    let other = lib.dir.path().join("elsewhere");
    let default = lib.config.download_dir.clone();
    let config = config_with(
        &lib,
        DownloadsConfig {
            unfinished_dir: Some(unfinished.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, engine) = lib.start_on(config).await;
    add(
        &mut client,
        seed.bytes.clone(),
        Some(other.clone()),
        "payload.bin",
    )
    .await;

    let started = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &seed.id).is_some_and(|row| row.save_path == other)
    })
    .await;
    assert_eq!(
        row_of(&started, &seed.id)
            .expect("the added torrent")
            .save_path,
        other,
        "it went where it was told to"
    );
    drop(started);

    let done = finish(&mut client, &engine, &seed.id, seed.addr).await;
    assert_eq!(
        done.save_path, other,
        "it finished where it was put: {done:?}"
    );
    drop(done);
    // A few more passes over the automation's work: a torrent it wrongly thought belonged elsewhere would
    // have been moved by now.
    wait_for_settle().await;

    assert_eq!(
        fs::read(other.join("payload.bin")).expect("the finished file"),
        sample(MIB),
        "the chosen folder has the bytes"
    );
    assert!(
        list_dir(&unfinished).is_empty(),
        "the unfinished folder was never involved: {:?}",
        list_dir(&unfinished)
    );
    assert!(
        list_dir(&default).is_empty(),
        "nor was the default folder: {:?}",
        list_dir(&default)
    );
    let last = client.snapshot();
    assert_eq!(
        row_of(&last, &seed.id).expect("the torrent").save_path,
        other,
        "and its row still names the chosen folder"
    );
    drop(last);

    shutdown(client, task).await;
    seed.stop().await;
}

/// The completion program is started with the torrent's name and the folder its files ended up in, as two
/// separate arguments, once, and only after the unfinished folder has handed the files back to the default
/// folder.
#[tokio::test(flavor = "multi_thread")]
async fn program_gets_name_and_final_folder() {
    let seed = Seed::start("dl_program").await;
    let lib = Library::new("dl_program_leech");
    let unfinished = lib.dir.path().join("incomplete");
    let default = lib.config.download_dir.clone();
    let config = config_with(
        &lib,
        DownloadsConfig {
            unfinished_dir: Some(unfinished.clone()),
            on_complete: Some(PathBuf::from(FAKE)),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, engine, handle) = lib.start_with_automation(config).await;
    add(&mut client, seed.bytes.clone(), None, "payload.bin").await;

    let moved = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &seed.id).is_some_and(|row| row.save_path == unfinished)
    })
    .await;
    drop(moved);
    let done = finish(&mut client, &engine, &seed.id, seed.addr).await;
    assert_eq!(done.save_path, default, "moved back before anything ran");
    drop(done);

    let log = default.join(FAKE_LOG);
    let text = recorded(&log).await;
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "one run for one finished torrent, not one per event: {text}"
    );
    let recording: serde_json::Value =
        serde_json::from_str(lines[0]).expect("the stand-in writes one JSON line");
    assert_eq!(
        args_of(&recording),
        ["payload.bin".to_string(), default.display().to_string(),],
        "the torrent's name, and the folder it finished in, as two arguments"
    );
    assert_eq!(
        handle.problems(),
        (None, None),
        "a program that ran has nothing to complain about"
    );
    assert!(
        !fs::exists(unfinished.join(FAKE_LOG)).unwrap_or(false),
        "the log is beside the folder it was run for, so the program ran after the move"
    );

    shutdown(client, task).await;
    seed.stop().await;
}

/// A completion program that cannot be started is said out loud, and the same line stays available beside
/// the field it was set from.
#[tokio::test(flavor = "multi_thread")]
async fn program_that_does_not_exist_is_a_notice() {
    let seed = Seed::start("dl_missing").await;
    let lib = Library::new("dl_missing_leech");
    let missing = lib.dir.path().join("no-such-program");
    let config = config_with(
        &lib,
        DownloadsConfig {
            on_complete: Some(missing.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, engine, handle) = lib.start_with_automation(config).await;
    assert!(
        !missing.exists(),
        "this test needs no program at {missing:?}"
    );
    add(&mut client, seed.bytes.clone(), None, "payload.bin").await;
    let done = finish(&mut client, &engine, &seed.id, seed.addr).await;
    assert_eq!(done.state, RowState::Seeding, "the download finished");
    drop(done);

    let line = notice_where(&mut client, |text| text.starts_with(COULD_NOT_START)).await;
    assert!(
        line.len() > COULD_NOT_START.len(),
        "the line carries a reason: {line}"
    );
    assert_eq!(
        handle.problems().1.as_deref(),
        Some(line.as_str()),
        "the settings screen shows the same line beside the field"
    );
    assert_eq!(
        handle.problems().0,
        None,
        "nothing else went wrong, so nothing else is blamed"
    );

    shutdown(client, task).await;
    seed.stop().await;
}

/// A `.torrent` dropped into the watch folder is added to the default folder and renamed out of the way
/// with `.added`, so it is never added twice and never thrown away.
#[tokio::test(flavor = "multi_thread")]
async fn watch_folder_adds_and_renames_to_added() {
    let lib = Library::new("watch_added");
    let watch = lib.dir.path().join("watch");
    let (bytes, id) = torrent_in_seed_dir(&lib, "added", "watched.bin").await;
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    write_file(&watch, "watched.torrent", &bytes);

    let added = wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    let row = row_of(&added, &id).expect("the torrent the watch folder added");
    assert_eq!(
        row.save_path, lib.config.download_dir,
        "a watch folder's torrent goes to the default folder"
    );
    drop(added);

    wait_for_file(watch.join("watched.torrent.added"), true).await;
    assert_eq!(
        list_dir(&watch),
        vec!["watched.torrent.added".to_string()],
        "renamed in place: not copied, and not deleted"
    );
    assert_eq!(
        handle.problems(),
        (None, None),
        "an add that worked leaves nothing to complain about"
    );

    shutdown(client, task).await;
}

/// A file that is still being copied in is not read: no scan takes it until two scans in a row have seen
/// the same size and modification time, so a half-written `.torrent` is neither added nor blamed for being
/// garbage.
#[tokio::test(flavor = "multi_thread")]
async fn watch_folder_waits_for_a_growing_file() {
    let lib = Library::new("watch_growing");
    let watch = lib.dir.path().join("watch");
    let (bytes, id) = torrent_in_seed_dir(&lib, "growing", "growing.bin").await;
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    fs::create_dir_all(&watch).expect("the watch folder");
    let path = watch.join("growing.torrent");

    // Written in pieces, one every eighth of a scan period, so no two scans can see the same file: this is
    // a copy that is still running. Each piece is looked at before the next is written, because a scan that
    // had read this file would have renamed it away already - added, or `.invalid` for the truncated bytes.
    for piece in 1..=16 {
        let taken = bytes.len() * piece / 16;
        fs::write(&path, &bytes[..taken.max(1)]).expect("write the next piece");
        assert!(
            fs::exists(&path).unwrap_or(false),
            "{path:?} was renamed away before it was complete, so it was read too early: {:?}",
            list_dir(&watch),
        );
        assert!(
            row_of(&client.snapshot(), &id).is_none(),
            "an incomplete file was added: {:?}",
            list_dir(&watch),
        );
        tokio::time::sleep(WATCH_EVERY / 8).await;
    }

    // Whole now, and left alone: two scans see the same file and it is read.
    let added = wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    assert_eq!(
        row_of(&added, &id).expect("the added torrent").save_path,
        lib.config.download_dir,
        "the completed file went to the default folder"
    );
    drop(added);
    wait_for_file(watch.join("growing.torrent.added"), true).await;
    assert_eq!(
        list_dir(&watch),
        vec!["growing.torrent.added".to_string()],
        "the completed file was added once and renamed"
    );
    assert_eq!(handle.problems(), (None, None), "and nothing went wrong");

    shutdown(client, task).await;
}

/// A file that is not a torrent is renamed with `.invalid` and said out loud with its reason, and nothing
/// is added from it. It is kept: a file the app cannot read is still somebody's file.
#[tokio::test(flavor = "multi_thread")]
async fn watch_folder_marks_garbage_invalid() {
    let lib = Library::new("watch_garbage");
    let watch = lib.dir.path().join("watch");
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    write_file(&watch, "junk.torrent", &sample(4096));

    let line = notice_where(&mut client, |text| text.contains("junk.torrent")).await;
    assert!(
        line.contains("junk.torrent"),
        "the line names the file: {line}"
    );
    wait_for_file(watch.join("junk.torrent.invalid"), true).await;
    assert_eq!(
        list_dir(&watch),
        vec!["junk.torrent.invalid".to_string()],
        "renamed, not deleted"
    );
    let snapshot = client.snapshot();
    assert!(
        snapshot.torrents.is_empty(),
        "nothing was added from it: {:?}",
        snapshot.torrents
    );
    drop(snapshot);
    assert_eq!(
        handle.problems().0.as_deref(),
        Some(line.as_str()),
        "the same line is what the watch folder's field shows"
    );

    shutdown(client, task).await;
}

/// A file past the size a watch folder will read is refused by its size, before any of its bytes are read
/// into memory, and renamed `.invalid` like anything else that cannot be added.
#[tokio::test(flavor = "multi_thread")]
async fn watch_folder_refuses_a_file_past_the_size_limit() {
    use fbtt_core::downloads::MAX_WATCHED_FILE;

    let lib = Library::new("watch_too_big");
    let watch = lib.dir.path().join("watch");
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    // One byte over the limit, and the first byte is `d`, which is what a bencoded dictionary starts with:
    // the reason has to be the size, because these bytes are never read at all.
    let mut huge = vec![b'\0'; MAX_WATCHED_FILE as usize + 1];
    huge[0] = b'd';
    write_file(&watch, "huge.torrent", &huge);

    let line = notice_where(&mut client, |text| text.contains("huge.torrent")).await;
    assert!(
        line.contains("16 MiB"),
        "the reason is the size, named in the units the setting is stated in: {line}"
    );
    wait_for_file(watch.join("huge.torrent.invalid"), true).await;
    assert_eq!(
        list_dir(&watch),
        vec!["huge.torrent.invalid".to_string()],
        "renamed, not deleted"
    );
    let snapshot = client.snapshot();
    assert!(
        snapshot.torrents.is_empty(),
        "nothing was added from it: {:?}",
        snapshot.torrents
    );
    drop(snapshot);
    assert_eq!(handle.problems().0.as_deref(), Some(line.as_str()));

    shutdown(client, task).await;
}

/// The same torrent under two names is added once: the second file finds the torrent already in the
/// library, gets its `.added` rename like the first, and does not put a second copy in the library.
#[tokio::test(flavor = "multi_thread")]
async fn watch_folder_duplicate_is_added_once() {
    let lib = Library::new("watch_duplicate");
    let watch = lib.dir.path().join("watch");
    let (bytes, id) = torrent_in_seed_dir(&lib, "duplicate", "twice.bin").await;
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    write_file(&watch, "first.torrent", &bytes);
    write_file(&watch, "second.torrent", &bytes);

    let added = wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    assert_eq!(
        added.torrents.len(),
        1,
        "one torrent for two files: {:?}",
        added.torrents
    );
    drop(added);
    wait_for_file(watch.join("first.torrent.added"), true).await;
    wait_for_file(watch.join("second.torrent.added"), true).await;

    assert_eq!(
        list_dir(&watch),
        vec![
            "first.torrent.added".to_string(),
            "second.torrent.added".to_string(),
        ],
        "both files are renamed, neither kept as a candidate and neither deleted"
    );
    let snapshot = client.snapshot();
    assert_eq!(
        snapshot.torrents.len(),
        1,
        "one torrent, not two: {:?}",
        snapshot.torrents
    );
    assert_eq!(
        handle.problems(),
        (None, None),
        "a torrent already in the library is not a failure"
    );
    drop(snapshot);

    shutdown(client, task).await;
}

/// A watch folder's torrents obey `start_paused` like every other way a torrent gets added: nothing
/// arrives until somebody starts it.
#[tokio::test(flavor = "multi_thread")]
async fn watch_folder_respects_start_paused() {
    let lib = Library::new("watch_paused");
    let watch = lib.dir.path().join("watch");
    let (bytes, id) = torrent_in_seed_dir(&lib, "paused", "paused.bin").await;
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            start_paused: true,
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    write_file(&watch, "paused.torrent", &bytes);

    let paused = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &id).is_some_and(|row| row.state == RowState::Paused)
    })
    .await;
    let row = row_of(&paused, &id).expect("the added torrent");
    assert_eq!(
        row.state,
        RowState::Paused,
        "a watch folder's torrent starts paused when the setting says so"
    );
    assert_eq!(
        row.save_path, lib.config.download_dir,
        "in the default folder, paused"
    );
    assert_eq!(row.progress, 0.0, "and nothing arrived: {row:?}");
    drop(paused);
    assert_eq!(handle.problems(), (None, None), "and nothing went wrong");

    shutdown(client, task).await;
}

/// `Command::Add` obeys `start_paused` too: the setting is about how torrents get added, whichever way
/// they arrived.
#[tokio::test(flavor = "multi_thread")]
async fn local_add_respects_start_paused() {
    let lib = Library::new("add_paused");
    let (bytes, id) = torrent_in_seed_dir(&lib, "add_paused", "held.bin").await;
    let config = config_with(
        &lib,
        DownloadsConfig {
            start_paused: true,
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine) = lib.start_on(config).await;
    add(&mut client, bytes, None, "held.bin").await;

    let paused = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &id).is_some_and(|row| row.state == RowState::Paused)
    })
    .await;
    let row = row_of(&paused, &id).expect("the added torrent");
    assert_eq!(
        row.state,
        RowState::Paused,
        "the add honoured start_paused: {row:?}"
    );
    assert_eq!(row.progress, 0.0, "and nothing arrived: {row:?}");
    drop(paused);

    shutdown(client, task).await;
}

/// `AutomationHandle::update` takes effect on the next scan: a new watch folder starts being watched
/// without restarting anything, and the folder it stopped watching is left alone.
#[tokio::test(flavor = "multi_thread")]
async fn update_takes_effect_without_restart() {
    let lib = Library::new("watch_update");
    let old_watch = lib.dir.path().join("watch_first");
    let new_watch = lib.dir.path().join("watch_second");
    let (bytes, id) = torrent_in_seed_dir(&lib, "update", "moved_rule.bin").await;
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(old_watch.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config.clone()).await;
    // A file in the folder this run is not watching yet, dropped there before the update: so the update
    // is what started watching it, not a file that was already waiting.
    write_file(&new_watch, "late.torrent", &bytes);
    wait_for_settle().await;
    let before = client.snapshot();
    assert!(
        row_of(&before, &id).is_none(),
        "the second folder is not being watched yet: {:?}",
        before.torrents
    );
    drop(before);

    handle.update(
        lib.config.download_dir.clone(),
        DownloadsConfig {
            watch_dir: Some(new_watch.clone()),
            ..config.downloads.clone()
        },
    );

    let added = wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    assert_eq!(
        row_of(&added, &id).expect("the added torrent").save_path,
        lib.config.download_dir,
        "added into the default folder the update named"
    );
    drop(added);
    wait_for_file(new_watch.join("late.torrent.added"), true).await;
    assert_eq!(
        list_dir(&old_watch),
        Vec::<String>::new(),
        "the folder that stopped being watched was left alone"
    );
    assert_eq!(handle.problems(), (None, None));

    shutdown(client, task).await;
}
