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
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fbtt_core::downloads::{AutomationHandle, WATCH_EVERY, test_support};
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
    /// The bytes it is seeding, read back from the seed's own file: what a test that wants to compare
    /// the downloaded file's content compares against.
    payload: Vec<u8>,
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
            payload: fs::read(&file).expect("the seed's own payload, read back for comparisons"),
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
    // The `Added` line is what a successful add says. It is *waited for* rather than demanded as
    // the next notice in line because a mutation that runs the completion program on the add wakes
    // the program's own complaint beside it, and which of the two queues drains first is the
    // runtime's choice, not the backend's: a proof run should fail on the added-whole assertion,
    // not on whichever line happened to arrive a poll earlier. Other helpers (`first_error`) keep
    // asking what the first line was, which is well defined in runs where nothing else is talking.
    let line = notice_where(client, |text| text == format!("Added {name}")).await;
    assert_eq!(line, format!("Added {name}"));
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

/// What the stand-in recorded once it holds `want` runs, waiting on the blocking pool.
///
/// A test that compares two recordings cannot use [`recorded`]: the log is already non-empty from the
/// first run, and the run still to come would be invisible in it.
async fn recorded_lines(log: &Path, want: usize) -> String {
    let log = log.to_path_buf();
    within(tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        loop {
            if let Ok(text) = fs::read_to_string(&log) {
                let held = text.lines().filter(|line| !line.trim().is_empty()).count();
                if held >= want {
                    return text;
                }
            }
            assert!(
                started.elapsed() <= DEADLINE,
                "the stand-in player never wrote {want} lines to {} after {} seconds",
                log.display(),
                DEADLINE.as_secs(),
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
        seed.payload,
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
        seed.payload,
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
    drop(done);
    // The row says whole a moment before the move back lands (the download is not slowed here), so the
    // folder is waited for rather than read at the instant the last piece arrives.
    let back = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &seed.id).is_some_and(|row| row.save_path == default)
    })
    .await;
    drop(back);

    let log = default.join(FAKE_LOG);
    // A run owed to this torrent has had every chance to happen by now: the answer to the move back, and
    // three watch folder passes on top of the second the torrent has been whole for. A second run would
    // be in the log by now too.
    tokio::time::sleep(3 * WATCH_EVERY).await;
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
    assert!(
        line.contains(&missing.display().to_string()),
        "the line names the program nobody could start, which is the thing to fix: {line}"
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

/// A library whose unfinished folder already holds a complete torrent's files, with the `.torrent` that
/// names them and the id they are the same thing as.
///
/// What the automation finds sitting whole in the unfinished folder is its fastest way to a completion
/// program: there is no download to wait for, because handing the torrent back to the default folder *is*
/// the finish. The default folder is made here too, since a move to a folder that is not there is a
/// different problem from the one a test is asking about.
async fn whole_in_unfinished(lib: &Library, name: &str, unfinished: &Path) -> (Vec<u8>, TorrentId) {
    fs::create_dir_all(&lib.config.download_dir).expect("the default folder");
    fs::create_dir_all(unfinished).expect("the unfinished folder");
    let payload = write_file(unfinished, name, &sample(64 * 1024));
    let bytes = torrent_for(&payload).await;
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    (bytes, id_of(&metainfo.info_hashes))
}

/// Waits until `handle`'s automation is parked in a watch folder pass, which is the moment a test knows
/// the loop is in the middle of a wake and cannot act on anything the session says in the meantime.
async fn wait_until_parked(handle: &AutomationHandle) {
    within(async {
        loop {
            if handle.watch_folder_is_parked() {
                return;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
}

/// The reason a refused program is told with: the words that name the program itself, which is what a
/// person has to see to fix the line they wrote.
const NOT_ABSOLUTE: &str = "not an absolute path";

/// A program named by a relative path is refused before anything is started, with the same line and the
/// same field as a program that would not start.
///
/// `Config` leaves `on_complete` exactly as it was written - it names a program, not a folder to root in
/// the library - and a server reads its own `config.yaml`, which a person edits by hand between two
/// starts with nothing validating it. A name like this one means "below whatever directory this process
/// was started in", which is not where the person meant and not the same run twice.
#[tokio::test(flavor = "multi_thread")]
async fn relative_program_is_refused_at_run_time() {
    let lib = Library::new("dl_relative_program");
    let unfinished = lib.dir.path().join("incomplete");
    let default = lib.config.download_dir.clone();
    let program = PathBuf::from("hooks/notify-torrent");
    let config = config_with(
        &lib,
        DownloadsConfig {
            unfinished_dir: Some(unfinished.clone()),
            on_complete: Some(program.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (bytes, id) = whole_in_unfinished(&lib, "payload.bin", &unfinished).await;
    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    add(&mut client, bytes, Some(unfinished.clone()), "payload.bin").await;

    // The torrent is whole and in the unfinished folder, so handing it back is the finish, and the
    // program is what the settings asked for after it.
    let placed = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &id).is_some_and(|row| row.save_path == default)
    })
    .await;
    let row = row_of(&placed, &id).expect("the placed torrent");
    assert_eq!(
        row.state,
        RowState::Seeding,
        "and it is whole where it belongs: {row:?}"
    );
    drop(placed);
    wait_for_settle().await;

    assert!(
        !fs::exists(default.join(FAKE_LOG)).unwrap_or(false),
        "a program that is not an absolute path is never started, so the stand-in never wrote its record: {:?}",
        list_dir(&default),
    );

    let line = notice_where(&mut client, |text| text.starts_with(COULD_NOT_START)).await;
    assert!(
        line.contains(NOT_ABSOLUTE),
        "the reason is that the name is not a path from the root of the filesystem: {line}"
    );
    assert!(
        line.contains(&program.display().to_string()),
        "and it names the program it refused: {line}"
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
}

/// A bare program name is refused too, for the same reason in a different shape: with no path in it, the
/// name is looked up on `PATH` - which on a server is whatever the service user's `PATH` happens to be,
/// not the folder the person was thinking of, and a torrent finishing is not the time to guess.
#[tokio::test(flavor = "multi_thread")]
async fn bare_program_name_is_refused_at_run_time() {
    let lib = Library::new("dl_bare_program");
    let unfinished = lib.dir.path().join("incomplete");
    let default = lib.config.download_dir.clone();
    let program = PathBuf::from("notify-torrent-not-on-any-path");
    let config = config_with(
        &lib,
        DownloadsConfig {
            unfinished_dir: Some(unfinished.clone()),
            on_complete: Some(program.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (bytes, id) = whole_in_unfinished(&lib, "payload.bin", &unfinished).await;
    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    add(&mut client, bytes, Some(unfinished.clone()), "payload.bin").await;

    let placed = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &id).is_some_and(|row| row.save_path == default)
    })
    .await;
    drop(placed);
    wait_for_settle().await;

    assert!(
        !fs::exists(default.join(FAKE_LOG)).unwrap_or(false),
        "a name with no path in it is refused rather than searched for on PATH: {:?}",
        list_dir(&default),
    );

    let line = notice_where(&mut client, |text| text.starts_with(COULD_NOT_START)).await;
    assert!(
        line.contains(NOT_ABSOLUTE),
        "the reason is the name, not an operating system's search: {line}"
    );
    assert!(
        line.contains(&program.display().to_string()),
        "and it names the program it refused: {line}"
    );
    assert_eq!(handle.problems().1.as_deref(), Some(line.as_str()));
    assert_eq!(handle.problems().0, None);

    shutdown(client, task).await;
}

/// On Windows a `.bat` or a `.cmd` is refused as well, because Windows starts those through `cmd.exe`:
/// the remote-controlled torrent name would arrive as an argument to a shell rather than to a program.
/// Everywhere else a `.bat` is an executable file like any other and is run, which is why this test is
/// Windows-only while the extension rule itself is checked on every host in `settings.rs`.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn batch_program_is_refused_at_run_time() {
    let lib = Library::new("dl_batch_program");
    let unfinished = lib.dir.path().join("incomplete");
    let default = lib.config.download_dir.clone();
    // An absolute path to a file that really is there: the extension is the whole problem, so nothing
    // else may be wrong with it.
    let program = write_file(
        &lib.dir.path().join("hooks"),
        "notify.bat",
        b"@echo off\r\n",
    );
    let config = config_with(
        &lib,
        DownloadsConfig {
            unfinished_dir: Some(unfinished.clone()),
            on_complete: Some(program.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (bytes, id) = whole_in_unfinished(&lib, "payload.bin", &unfinished).await;
    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    add(&mut client, bytes, Some(unfinished.clone()), "payload.bin").await;

    let placed = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &id).is_some_and(|row| row.save_path == default)
    })
    .await;
    drop(placed);
    wait_for_settle().await;

    assert!(
        !fs::exists(default.join(FAKE_LOG)).unwrap_or(false),
        "the batch file is not started, so nothing ran it: {:?}",
        list_dir(&default),
    );

    let line = notice_where(&mut client, |text| text.starts_with(COULD_NOT_START)).await;
    assert!(
        line.contains("batch file"),
        "the reason is what a batch file is on Windows: {line}"
    );
    assert!(
        line.contains(&program.display().to_string()),
        "and it names the program it refused: {line}"
    );
    assert_eq!(handle.problems().1.as_deref(), Some(line.as_str()));

    shutdown(client, task).await;
}

/// A torrent added already whole gets no completion program: it never finished here, it arrived finished,
/// and the settings say one run per finish.
///
/// The park matters: the session says "added" before the torrent's actor even exists, so the only way this
/// task ever sees such a torrent as whole is by being busy while the engine checked its data. Parked, the
/// torrent is checked and reported whole with the loop still holding its last wake; released, the loop
/// takes the add it has been holding and looks at a torrent that was whole from the first.
#[tokio::test(flavor = "multi_thread")]
async fn program_does_not_run_for_a_torrent_added_whole() {
    let lib = Library::new("dl_added_whole");
    let watch = lib.dir.path().join("watch");
    let default = lib.config.download_dir.clone();
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            on_complete: Some(PathBuf::from(FAKE)),
            ..DownloadsConfig::default()
        },
    );

    let (bytes, id) = {
        let payload = write_file(&default, "held.bin", &sample(64 * 1024));
        let bytes = torrent_for(&payload).await;
        let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
        (bytes, id_of(&metainfo.info_hashes))
    };

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    fs::create_dir_all(&watch).expect("the watch folder");

    handle.park_the_watch_folder();
    wait_until_parked(&handle).await;
    add(&mut client, bytes, None, "held.bin").await;

    // The engine has checked it: the files were already here, so it is whole before the loop is let go.
    let whole = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &id)
            .is_some_and(|row| row.state == RowState::Seeding && row.progress == 1.0)
    })
    .await;
    let row = row_of(&whole, &id).expect("the added torrent");
    assert_eq!(
        row.save_path, default,
        "and it is where it was added: {row:?}"
    );
    drop(whole);
    handle.release_the_watch_folder();

    // A few passes over everything the settings do, in case a program was owed to it by some other route.
    wait_for_settle().await;
    let last = client.snapshot();
    assert_eq!(
        row_of(&last, &id).expect("the torrent is still here").state,
        RowState::Seeding,
        "the torrent is here and whole: {:?}",
        last.torrents
    );
    drop(last);
    assert!(
        !fs::exists(default.join(FAKE_LOG)).unwrap_or(false),
        "a torrent that arrived whole never finished here, so its program does not run: {:?}",
        list_dir(&default),
    );
    assert_eq!(
        handle.problems(),
        (None, None),
        "and nothing went wrong on the way"
    );

    shutdown(client, task).await;
}

/// A move back that fails for a reason the next wake will not fix - a file of somebody else's already at
/// the target - is still one warning line, and the program still runs, with the folder the files are
/// actually in.
///
/// Leaving a finished torrent without its program because a *move* went wrong would be the app punishing
/// the person for the torrent they finished: the files are whole where they are, and a program that plays
/// or notifies has no interest in which folder it was asked from.
#[tokio::test(flavor = "multi_thread")]
async fn program_runs_in_place_when_the_move_back_fails() {
    let seed = Seed::start("dl_move_failed").await;
    let lib = Library::new("dl_move_failed_leech");
    let unfinished = lib.dir.path().join("incomplete");
    let default = lib.config.download_dir.clone();
    // Somebody else's file, wearing the name this torrent's payload would take in the default folder: the
    // engine is told never to replace what is at the target, so this move cannot work.
    let other = write_file(&default, "payload.bin", b"not this torrent's file\n");
    let config = config_with(
        &lib,
        DownloadsConfig {
            unfinished_dir: Some(unfinished.clone()),
            on_complete: Some(PathBuf::from(FAKE)),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, engine, handle) = lib.start_with_automation(config).await;
    slow_down(&mut client);
    add(
        &mut client,
        seed.bytes.clone(),
        Some(unfinished.clone()),
        "payload.bin",
    )
    .await;

    let done = finish(&mut client, &engine, &seed.id, seed.addr).await;
    assert_eq!(
        done.save_path, unfinished,
        "it finished where it was downloading: {done:?}"
    );
    drop(done);

    let line = notice_where(&mut client, |text| text.starts_with("Could not move ")).await;
    assert!(
        line.contains("payload.bin"),
        "the line names the torrent whose move failed: {line}"
    );
    assert!(
        line.contains("download folder"),
        "and the folder it could not get into: {line}"
    );

    // The program runs, once, with the folder the files are really in.
    let log = unfinished.join(FAKE_LOG);
    let text = recorded(&log).await;
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "one run for one finish, wherever it was: {text}"
    );
    let recording: serde_json::Value =
        serde_json::from_str(lines[0]).expect("the stand-in writes one JSON line");
    assert_eq!(
        args_of(&recording),
        ["payload.bin".to_string(), unfinished.display().to_string(),],
        "the name, and the folder the files are actually in"
    );

    assert_eq!(
        fs::read(&other).expect("the other person's file"),
        b"not this torrent's file\n",
        "a failed move wrote nothing over anything"
    );
    assert!(
        !fs::exists(default.join(FAKE_LOG)).unwrap_or(false),
        "and the record is beside the folder the program was run for: {:?}",
        list_dir(&default),
    );
    assert_eq!(
        handle.problems().1,
        None,
        "a program that ran has nothing to complain about, even beside a move that failed"
    );

    shutdown(client, task).await;
    seed.stop().await;
}

/// A lost event does not lose a torrent its finish.
///
/// The session drops events for a subscriber that cannot keep up - a `TorrentFinished` among them, which
/// is the one thing this task cannot do without - and the recovery is to look at what the session holds
/// now and finish the torrents that clearly finished in the gap: move them back, then run their programs.
/// The hook here drops the finish itself, which is what an overflowed queue does to it, and the 1 MiB
/// download runs under its own throttle while it is gone.
#[tokio::test(flavor = "multi_thread")]
async fn lagged_events_still_finish_and_run_the_program() {
    let seed = Seed::start("dl_lagged").await;
    let lib = Library::new("dl_lagged_leech");
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
    slow_down(&mut client);
    add(&mut client, seed.bytes.clone(), None, "payload.bin").await;
    // The torrent's own info hashes, which are what the event carries: a torrent made by these tests is a
    // hybrid, so a v1-only hash would name a different torrent and the queue would lose nothing at all.
    let hashes = fbt::Metainfo::from_bytes(&seed.bytes)
        .expect("the seed's torrent")
        .info_hashes;
    // Armed while the download is still about eight seconds from its last piece: this torrent's finish is
    // the event the queue is about to lose.
    test_support::lag_when_it_finishes(hashes);

    let moved = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &seed.id).is_some_and(|row| row.save_path == unfinished)
    })
    .await;
    drop(moved);

    let done = finish(&mut client, &engine, &seed.id, seed.addr).await;
    assert_eq!(done.progress, 1.0, "the last piece landed: {done:?}");
    drop(done);
    assert!(
        test_support::lag_was_injected(&hashes),
        "this test is about a finish that never arrived, so the hook has to have dropped *this* \
         torrent's finish, and no other test's",
    );

    // A few wakes' worth of grace, which is what the recovery needs: it is a look at what the session holds,
    // and it happens on the next wake, not on the drop of the event.
    wait_for_settle().await;
    wait_for_settle().await;

    // Nothing was listening for the finish, so the whole files come back on the recovery's say-so and on
    // nobody else's: the unfinished folder is empty of them, and the default folder has them.
    let left_behind = list_dir(&unfinished);
    assert!(
        !left_behind.iter().any(|name| name == "payload.bin"),
        "a lost event did not leave the finished files where they were downloaded: {left_behind:?}"
    );

    let placed = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &seed.id).is_some_and(|row| row.save_path == default)
    })
    .await;
    let row = row_of(&placed, &seed.id).expect("the placed torrent");
    assert_eq!(
        row.progress, 1.0,
        "whole, and back where it belongs: {row:?}"
    );
    drop(placed);

    let log = default.join(FAKE_LOG);
    let text = recorded(&log).await;
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "the missed finish is run once, not once for the recovery and once for a later event: {text}"
    );
    let recording: serde_json::Value =
        serde_json::from_str(lines[0]).expect("the stand-in writes one JSON line");
    assert_eq!(
        args_of(&recording),
        ["payload.bin".to_string(), default.display().to_string(),],
        "the name, and the folder the recovery moved it into"
    );
    assert!(
        !fs::exists(unfinished.join(FAKE_LOG)).unwrap_or(false),
        "the record is beside the folder it was run for: {:?}",
        list_dir(&unfinished),
    );
    assert_eq!(
        handle.problems(),
        (None, None),
        "and a recovered finish reports nothing wrong"
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

    wait_for_file(watch.join("watched.torrent"), false).await;
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
    wait_for_file(watch.join("growing.torrent"), false).await;
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
    // The field is read the moment the line arrives, before anything else this test waits for: it only
    // carries one pass's complaint, and the next pass, two watch periods after this one began, finds the
    // name already renamed away, has nothing left to say and clears it. A test thread that was descheduled
    // for a couple of seconds between the line and this look would find it empty, which says nothing
    // about the watch folder.
    assert_eq!(
        handle.problems().0.as_deref(),
        Some(line.as_str()),
        "the same line is what the watch folder's field shows"
    );
    wait_for_file(watch.join("junk.torrent"), false).await;
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
    wait_for_file(watch.join("huge.torrent"), false).await;
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

/// A candidate that is a link by the time its bytes are read is refused, and nothing is read from behind
/// it.
///
/// The look that accepted this name saw a regular file; between that look and the read something put a
/// link in its place, and a read that follows links would take whatever the link stands for - a file in
/// another folder, `/dev/zero`, a FIFO nobody is writing to - into memory, or never finish at all. What
/// the folder is told to add is the file it looked at, not whatever answers to that name a moment later.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn watch_folder_refuses_a_symlink_swapped_in() {
    let lib = Library::new("watch_swapped");
    let watch = lib.dir.path().join("watch");
    let (bytes, id) = torrent_in_seed_dir(&lib, "swapped", "swapped.bin").await;
    // A torrent the watch folder was never told about, in a folder of its own, which is what the link
    // stands for by the time the read comes.
    let target = write_file(&lib.dir.path().join("elsewhere"), "other.torrent", &bytes);
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    write_file(&watch, "swapped.torrent", &bytes);
    test_support::when_the_watch_folder_reads(
        watch.join("swapped.torrent"),
        test_support::AtRead::SwapToSymlink {
            target: target.clone(),
        },
    );

    // The complaint is caught the moment it appears. The field only carries the folder's current complaint
    // - a pass after the refusal finds the name already renamed out of the way, has nothing left to say,
    // and clears it - so "this same line is what the field shows" can only be asked while the refusal is
    // what happened this very pass, which is the moment the notice goes out.
    let line = notice_where(&mut client, |text| text.contains("swapped.torrent")).await;
    assert!(
        line.contains("changed between the look and the read"),
        "the reason is that the name stands for a different file than the one that was looked at: {line}"
    );
    assert_eq!(
        handle.problems().0.as_deref(),
        Some(line.as_str()),
        "the same line is what the watch folder's field shows"
    );
    wait_for_file(watch.join("swapped.torrent"), false).await;

    let snapshot = client.snapshot();
    assert!(
        row_of(&snapshot, &id).is_none(),
        "the torrent behind the link was not added: {:?}",
        snapshot.torrents
    );
    drop(snapshot);
    assert_eq!(
        fs::metadata(&target)
            .expect("the file the link stands for is still where it was")
            .len(),
        bytes.len() as u64,
        "and nothing was read out of it or moved with it"
    );
    assert_eq!(
        handle.problems().0.as_deref(),
        Some(line.as_str()),
        "the same line is what the watch folder's field shows"
    );

    shutdown(client, task).await;
}

/// A file that grew past the size limit after it settled is refused by the size the read found, not by the
/// size the settling look saw.
///
/// Two looks saying the same thing is the folder's whole answer to "is somebody still writing this?", and
/// it is a look at a name, not at the bytes: a file appended to between the look and the read is bigger
/// than the folder decided it was, and the limit is about how much is read into memory, so it is the size
/// at the read that has to be the one asked about.
#[tokio::test(flavor = "multi_thread")]
async fn watch_folder_refuses_a_file_grown_past_the_limit_after_settling() {
    use fbtt_core::downloads::MAX_WATCHED_FILE;

    let lib = Library::new("watch_grown");
    let watch = lib.dir.path().join("watch");
    let (bytes, id) = torrent_in_seed_dir(&lib, "grown", "grown.bin").await;
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    write_file(&watch, "grown.torrent", &bytes);
    // Appended to, after the two looks agreed and while the read of it is what is left to do: the look
    // that accepted this candidate still says it is a few hundred bytes.
    test_support::when_the_watch_folder_reads(
        watch.join("grown.torrent"),
        test_support::AtRead::GrowTo {
            bytes: MAX_WATCHED_FILE + 1024 * 1024,
        },
    );

    let line = notice_where(&mut client, |text| text.contains("grown.torrent")).await;
    assert!(
        line.contains("bytes now"),
        "the reason is the size there at the read, not the size the look saw: {line}"
    );
    assert!(
        line.contains("16 MiB"),
        "named in the units the limit is set in: {line}"
    );
    wait_for_file(watch.join("grown.torrent"), false).await;

    let snapshot = client.snapshot();
    assert!(
        row_of(&snapshot, &id).is_none(),
        "nothing was added from it: {:?}",
        snapshot.torrents
    );
    drop(snapshot);
    assert_eq!(handle.problems().0.as_deref(), Some(line.as_str()));

    shutdown(client, task).await;
}

/// A link whose target is not there is a taken name, and the added file goes on to the next number instead
/// of replacing it.
///
/// `Path::exists` follows a link, so a dangling one looks like nobody is using the name - and the file
/// that arrives would then be renamed *over* it, which is a deletion in a folder that does not delete. The
/// name is somebody's arrangement, dangling or not: what is there is asked about with a look at the name
/// itself, and the rename fails rather than replacing what it finds.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn dangling_symlink_name_is_not_overwritten() {
    let lib = Library::new("watch_dangling");
    let watch = lib.dir.path().join("watch");
    let (bytes, id) = torrent_in_seed_dir(&lib, "dangling", "dangling.bin").await;
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    fs::create_dir_all(&watch).expect("the watch folder");
    // The name the added file would have taken, worn by a link to a file that is not there.
    let link = watch.join("show.torrent.added");
    let nowhere = lib.dir.path().join("nowhere.torrent");
    std::os::unix::fs::symlink(&nowhere, &link).expect("a dangling link where the added name goes");
    write_file(&watch, "show.torrent", &bytes);

    let added = wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    assert_eq!(
        row_of(&added, &id).expect("the added torrent").save_path,
        lib.config.download_dir,
        "the torrent was added, which is all this folder is for"
    );
    drop(added);
    // The candidate has been moved out of the way, which is the moment the name it left behind is the
    // question: `Path::exists` follows the dangling link, so a folder that trusts it thinks this name is
    // free and renames straight onto it.
    wait_for_file(watch.join("show.torrent"), false).await;

    let kept = fs::symlink_metadata(&link).expect("the link is still where it was");
    assert!(
        kept.file_type().is_symlink(),
        "the dangling link was not replaced by the added file: {kept:?}"
    );
    assert_eq!(
        fs::read_link(&link).expect("the link still points where its owner pointed it"),
        nowhere,
        "and it still points at the same missing file"
    );
    wait_for_file(watch.join("show.torrent.added.2"), true).await;
    assert_eq!(
        list_dir(&watch),
        vec![
            "show.torrent.added".to_string(),
            "show.torrent.added.2".to_string()
        ],
        "the added file stepped on to the next number"
    );
    assert_eq!(
        handle.problems(),
        (None, None),
        "stepping over a name is not a problem to report"
    );

    shutdown(client, task).await;
}

/// A file the folder could not move is read once, not once every two seconds.
///
/// A file whose rename failed has been read, parsed, offered to the engine and complained about already,
/// and the rename is not going to work any better at the next look: a folder whose `.added` numbers are
/// all used up, or one nobody can write into. Reading and parsing it again every two seconds buys nothing,
/// so the folder remembers what it looked like when it was read and leaves it while it still looks like
/// that - the same size and the same modification time - and says the same thing about it.
#[tokio::test(flavor = "multi_thread")]
async fn unrenameable_file_is_not_reread_every_scan() {
    let lib = Library::new("watch_unrenameable");
    let watch = lib.dir.path().join("watch");
    // Every name this file could be renamed to is already taken, so the rename that would keep it from
    // being offered again is the one that cannot work.
    write_file(&watch, "show.torrent.added", b"copy");
    for number in 2..=999 {
        write_file(&watch, &format!("show.torrent.added.{number}"), b"copy");
    }
    let (bytes, id) = torrent_in_seed_dir(&lib, "sticky", "sticky.bin").await;
    let (second, second_id) = torrent_in_seed_dir(&lib, "swapped_in", "second.bin").await;
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    let path = write_file(&watch, "show.torrent", &bytes);

    let added = wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    assert_eq!(
        row_of(&added, &id).expect("the torrent it held").save_path,
        lib.config.download_dir,
        "the engine took it, which is why it was read at all"
    );
    drop(added);
    // Looks enough that this file is on the list of ones the folder has already dealt with.
    wait_for_settle().await;

    // A different torrent, the same size, with the modification time put back: the look that made this
    // file a candidate is unchanged, so the only way `second_id` gets into the library is by the bytes
    // being read again.
    assert_eq!(
        second.len(),
        bytes.len(),
        "the two torrents have to be the same size for the look to match"
    );
    let look = fs::metadata(&path).expect("the candidate's look");
    let modified = look.modified().expect("the candidate's modification time");
    fs::write(&path, &second).expect("put another torrent in the same file");
    let back = fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open it to put its time back");
    back.set_modified(modified)
        .expect("put the modification time back");

    wait_for_settle().await;
    wait_for_settle().await;

    let last = client.snapshot();
    assert!(
        row_of(&last, &id).is_some(),
        "the torrent that was added from it is still in the library: {:?}",
        last.torrents
    );
    assert!(
        row_of(&last, &second_id).is_none(),
        "and its bytes were not read a second time: {:?}",
        last.torrents
    );
    drop(last);
    assert!(
        fs::exists(&path).unwrap_or(false),
        "the file is still where it was, which is what the failed rename means"
    );
    assert_eq!(
        list_dir(&watch).len(),
        1000,
        "all 999 taken names, and the file that could not be renamed into one of them"
    );
    assert!(
        handle.problems().0.is_some(),
        "the folder still has the same thing to say about that file"
    );

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
    wait_for_file(watch.join("first.torrent"), false).await;
    wait_for_file(watch.join("second.torrent"), false).await;

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
    wait_for_file(new_watch.join("late.torrent"), false).await;
    assert_eq!(
        list_dir(&old_watch),
        Vec::<String>::new(),
        "the folder that stopped being watched was left alone"
    );
    assert_eq!(handle.problems(), (None, None));

    shutdown(client, task).await;
}

/// A torrent that finished while no program was set has had its finish handled - by nothing, which is
/// still an answer - and a program set afterwards through `update` is for the next finish, not a bell
/// rung over every finish that already happened with nothing to run.
///
/// The reading this test refuses is the tempting one - "run the program when a finish is found and a
/// program is set" - because it is only wrong in the accumulation: finish a dozen torrents quietly,
/// then set a program, and twelve players wake at once for finishes the person watched go by with
/// nothing playing them. The claim is taken before the settings are even consulted, which is the only
/// way "handled once" means something to a person who changes their mind about the program.
#[tokio::test(flavor = "multi_thread")]
async fn a_program_set_after_a_finish_does_not_replay_it() {
    let seed = Seed::start("dl_late_program").await;
    let lib = Library::new("dl_late_program_leech");
    let default = lib.config.download_dir.clone();
    let config = config_with(&lib, DownloadsConfig::default());

    let (mut client, task, engine, handle) = lib.start_with_automation(config.clone()).await;
    slow_down(&mut client);
    add(&mut client, seed.bytes.clone(), None, "payload.bin").await;

    let done = finish(&mut client, &engine, &seed.id, seed.addr).await;
    assert_eq!(
        done.state,
        RowState::Seeding,
        "the download finished with no program set: {done:?}"
    );
    drop(done);
    // Every wake the completion program had to find this finish in - the finish's own wake among them.
    wait_for_settle().await;
    assert!(
        !fs::exists(default.join(FAKE_LOG)).unwrap_or(false),
        "with nothing configured, nothing ran: {:?}",
        list_dir(&default),
    );

    // Only now does the person set a program.
    handle.update(
        default.clone(),
        DownloadsConfig {
            on_complete: Some(PathBuf::from(FAKE)),
            ..DownloadsConfig::default()
        },
    );
    wait_for_settle().await;

    assert!(
        !fs::exists(default.join(FAKE_LOG)).unwrap_or(false),
        "the torrent finished while nothing was set, so this run has no finish left for a late \
         program to replay: {:?}",
        list_dir(&default),
    );
    let last = client.snapshot();
    assert_eq!(
        row_of(&last, &seed.id)
            .expect("the torrent is still here")
            .state,
        RowState::Seeding,
        "and it is whole where it finished: {:?}",
        last.torrents,
    );
    drop(last);
    assert_eq!(
        handle.problems(),
        (None, None),
        "nothing went wrong, so nothing is being complained about",
    );

    shutdown(client, task).await;
    seed.stop().await;
}

/// A torrent this run lost the removal event for, and later got back under the same hash, is new to
/// this run: the claims its first life left behind are the departed torrent's, not the returning
/// one's, and spending them against the newcomer would deny it a program for a finish that has never
/// happened yet.
///
/// The claims are tidied by the same look that tidies everything else the session says: the torrent
/// is not in the session, so whatever the loop had filed under its hash went with it. The re-added
/// torrent arrives whole inside the unfinished folder, where the settings' own hand-back *is* the
/// finish - the second program run is that hand-back's, and only that.
#[tokio::test(flavor = "multi_thread")]
async fn a_readded_torrent_is_new_to_a_run_that_lost_its_removal() {
    let seed = Seed::start("dl_lost_removal").await;
    let lib = Library::new("dl_lost_removal_leech");
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
    slow_down(&mut client);
    add(&mut client, seed.bytes.clone(), None, "payload.bin").await;
    let hashes = fbt::Metainfo::from_bytes(&seed.bytes)
        .expect("the seed's torrent")
        .info_hashes;

    let moved = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &seed.id).is_some_and(|row| row.save_path == unfinished)
    })
    .await;
    drop(moved);
    let done = finish(&mut client, &engine, &seed.id, seed.addr).await;
    assert_eq!(done.progress, 1.0, "the download finished: {done:?}");
    drop(done);

    // Its first life, ordinary: hand-back to the default folder, then one program run.
    let placed = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &seed.id).is_some_and(|row| row.save_path == default)
    })
    .await;
    drop(placed);
    let log = default.join(FAKE_LOG);
    let first = recorded(&log).await;
    assert_eq!(
        first.lines().filter(|line| !line.trim().is_empty()).count(),
        1,
        "the first finish earned exactly one run: {first}",
    );

    // The torrent is removed - files left where they are - and the event saying so is dropped, which
    // is what a queue this task fell behind in does.
    test_support::lose_the_removal(hashes);
    assert!(
        client.send(Command::Remove {
            id: seed.id.clone(),
            delete_files: false,
        }),
        "the queue takes the removal",
    );
    wait_for(&mut client, |snapshot| row_of(snapshot, &seed.id).is_none()).await;
    wait_for_settle().await;
    assert!(
        test_support::removal_was_lost(&hashes),
        "this test is about a removal nobody was told about, so the hook has to have eaten it",
    );

    // The very same torrent comes back whole, whole inside the unfinished folder. The settings'
    // hand-back is its finish, and the program belongs to this arrival, not to the claims of the
    // torrent that left.
    fs::rename(default.join("payload.bin"), unfinished.join("payload.bin"))
        .expect("put the whole file where the re-added torrent will find it");
    add(
        &mut client,
        seed.bytes.clone(),
        Some(unfinished.clone()),
        "payload.bin",
    )
    .await;

    let back = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &seed.id).is_some_and(|row| row.save_path == default)
    })
    .await;
    let row = row_of(&back, &seed.id).expect("the re-added torrent");
    assert_eq!(
        row.state,
        RowState::Seeding,
        "whole where it belongs: {row:?}"
    );
    drop(back);
    wait_for_settle().await;

    let text = recorded_lines(&log, 2).await;
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        2,
        "the returning torrent is new to this run, so its finish earns its program: {text}",
    );
    let recording: serde_json::Value =
        serde_json::from_str(lines[1]).expect("the stand-in writes one JSON line");
    assert_eq!(
        args_of(&recording),
        ["payload.bin".to_string(), default.display().to_string(),],
        "the second recording is the re-added torrent's hand-back, with its name and where it landed",
    );
    assert_eq!(
        handle.problems(),
        (None, None),
        "a re-added torrent that got its program has nothing to complain about",
    );

    shutdown(client, task).await;
    seed.stop().await;
}

/// A name in the watch folder that is a FIFO (a named pipe) by the time its bytes are due is refused
/// and kept, and the folder carries on adding the files beside it.
///
/// An ordinary read-mode open of a FIFO with no writer does not come back until a writer appears -
/// and in a folder a person drops files into, the writer may never appear - so a folder that opens
/// without that flag does not delay this file: it delays every file after it in the folder and every
/// torrent behind the pass. The proof is the file *beside* the pipe: it is added if and only if the
/// pass moved on past the open.
#[cfg(unix)]
#[test]
fn watch_folder_adds_the_real_file_beside_a_fifo() {
    // The runtime is the test's own, not the macro's, because of what a mutated build does: a sweep
    // whose candidate `open()` blocks parks a tokio blocking-pool thread, and dropping a runtime
    // drains that pool - a wait that never ends when one of its jobs never returns. So the runtime
    // is dropped the ordinary way only when the body finished; after a panic it is deliberately
    // leaked (its pool may be holding a thread stuck in `open()`) and the process exits right there,
    // with the failure printed. A test that hangs on its bug proves nothing; this one fails.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime for the watch folder test");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(watch_folder_adds_the_real_file_beside_a_fifo_body())
    }));
    if let Err(problem) = outcome {
        // Leak, do not drop: after a panic the pool may hold the very thread the failure was about,
        // and `Drop` would wait for it forever. Leaked threads die with the process, and the panic
        // carries on past this function so the harness reports it the usual way - the message the
        // failing assertion raised is what ends up in the log.
        std::mem::forget(runtime);
        std::panic::resume_unwind(problem);
    }
}

#[cfg(unix)]
async fn watch_folder_adds_the_real_file_beside_a_fifo_body() {
    let lib = Library::new("watch_fifo");
    let watch = lib.dir.path().join("watch");
    let (bytes, id) = torrent_in_seed_dir(&lib, "fifo", "shown.bin").await;
    let config = config_with(
        &lib,
        DownloadsConfig {
            watch_dir: Some(watch.clone()),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    fs::create_dir_all(&watch).expect("the watch folder");
    // A name the pass will settle on and try to read, swapped into a pipe between the read's own look
    // and its open - the shape that would otherwise wait inside the open forever.
    write_file(&watch, "blocked.torrent", &sample(4096));
    test_support::when_the_watch_folder_reads(
        watch.join("blocked.torrent"),
        test_support::AtRead::SwapToFifo,
    );
    // The file the folder was actually for, dropped beside it.
    write_file(&watch, "show.torrent", &bytes);

    // The pipe's name is refused as not-the-file-that-was-looked-at, said once, and kept like
    // anything else the folder cannot add - renamed `.invalid`, still a pipe. The complaint is caught
    // the moment it appears, before a later pass - which finds the name already renamed out of the way,
    // has nothing left to say, and clears the field - goes by: the watch folder's field only carries
    // this pass's complaint, which is the moment the notice goes out.
    // The wait is bounded, and that is what makes a poisoned sweep *fail* this test instead of
    // hanging it: a pass whose `open()` blocks on the swapped-in pipe never produces this line, and
    // a deadline is the only witness that will ever say so. The bound is the suite's own `DEADLINE`,
    // not a multiple of what a healthy pass takes (two watch periods and a wake): that figure is a
    // fact about an idle machine, and on a busy one the pass is late by however long the machine
    // kept it waiting. Working code still gets there with a wide margin; a sweep stuck inside
    // `open()` is named, out loud, within the same minute every other wait in this file allows.
    let line = tokio::time::timeout(
        DEADLINE,
        notice_where(&mut client, |text| text.contains("blocked.torrent")),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the watch sweep never came back from blocked.torrent within {} seconds: the pass is \
             sitting inside the candidate open() - without O_NONBLOCK a FIFO with no writer stops \
             the whole scan, and the real torrent beside it waits behind that open too",
            DEADLINE.as_secs()
        )
    });
    assert!(
        line.contains("changed between the look and the read"),
        "the reason is that the name stopped being the file that was looked at: {line}"
    );
    assert_eq!(
        handle.problems().0.as_deref(),
        Some(line.as_str()),
        "the same line is what the watch folder's field shows"
    );
    wait_for_file(watch.join("blocked.torrent"), false).await;

    // And the point of the test: the file beside the pipe was added anyway, because the pass moved on
    // past the open instead of waiting inside it.
    let added = wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    assert_eq!(
        row_of(&added, &id)
            .expect("the torrent added beside the pipe")
            .save_path,
        lib.config.download_dir,
        "a pipe in the folder did not stop a real torrent from being added",
    );
    drop(added);
    wait_for_file(watch.join("show.torrent"), false).await;
    let kept = fs::metadata(watch.join("blocked.torrent.invalid")).expect("a refused name is kept");
    assert!(
        kept.file_type().is_fifo(),
        "what the folder refused really was the pipe the name was swapped into: {kept:?}",
    );
    assert_eq!(
        list_dir(&watch),
        vec![
            "blocked.torrent.invalid".to_string(),
            "show.torrent.added".to_string(),
        ],
        "both files were dealt with and neither was deleted"
    );

    shutdown(client, task).await;
}

/// A torrent added whole into the unfinished folder is moving to the default folder with its program
/// owed when the person names another default folder mid-copy. The first move lands in a folder nobody
/// wants any more, its answer comes back stale, and the entry for the new folder finds the torrent
/// whole where it landed with nothing left to move: the program it owed still runs, once, for the
/// folder the files are in.
///
/// The move is held before the engine is asked, which is what a copy under way looks like to everything
/// else: the torrent still says it is in the unfinished folder, so the settings change is seen against
/// that and replaces the entry.
#[tokio::test(flavor = "multi_thread")]
async fn a_stale_move_still_runs_the_program_it_owed() {
    let lib = Library::new("dl_stale_owed");
    let unfinished = lib.dir.path().join("incomplete");
    let other = lib.dir.path().join("other_default");
    let default = lib.config.download_dir.clone();
    let on_complete = DownloadsConfig {
        unfinished_dir: Some(unfinished.clone()),
        on_complete: Some(PathBuf::from(FAKE)),
        ..DownloadsConfig::default()
    };
    let config = config_with(&lib, on_complete.clone());
    let (bytes, id) = whole_in_unfinished(&lib, "payload.bin", &unfinished).await;
    fs::create_dir_all(&other).expect("the other default folder");

    let (mut client, task, _engine, handle) = lib.start_with_automation(config).await;
    handle.hold_the_next_move();
    add(&mut client, bytes, Some(unfinished.clone()), "payload.bin").await;
    within(async {
        loop {
            if handle.moves_are_held() {
                return;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await;

    // The default folder changes while the first move is claimed and not yet copying.
    handle.update(other.clone(), on_complete);
    wait_for_settle().await;
    handle.release_the_move();

    let landed = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &id).is_some_and(|row| row.save_path == default)
    })
    .await;
    drop(landed);

    let text = recorded(&default.join(FAKE_LOG)).await;
    wait_for_settle().await;
    let text_later = recorded(&default.join(FAKE_LOG)).await;
    assert_eq!(text, text_later, "and it ran once, not again later");
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 1, "one run for one finish: {text}");
    let recording: serde_json::Value =
        serde_json::from_str(lines[0]).expect("the stand-in writes one JSON line");
    assert_eq!(
        args_of(&recording),
        ["payload.bin".to_string(), default.display().to_string()],
        "with the name, and the folder the files landed in",
    );
    assert_eq!(handle.problems(), (None, None), "nothing went wrong");

    shutdown(client, task).await;
}

/// A torrent that finished, lost its data to a forced recheck and downloads again, finishes a second
/// time, and that finish owes its program as the first did: two runs in all.
///
/// The claim a finish leaves behind is what stops a program set later from replaying finishes; it is
/// let go when the torrent is seen downloading again, because that is a finish still to come.
#[tokio::test(flavor = "multi_thread")]
async fn a_torrent_that_finishes_again_runs_its_program_again() {
    let seed = Seed::start("dl_finish_twice").await;
    let lib = Library::new("dl_finish_twice_leech");
    let default = lib.config.download_dir.clone();
    let config = config_with(
        &lib,
        DownloadsConfig {
            on_complete: Some(PathBuf::from(FAKE)),
            ..DownloadsConfig::default()
        },
    );

    let (mut client, task, engine, handle) = lib.start_with_automation(config).await;
    slow_down(&mut client);
    add(&mut client, seed.bytes.clone(), None, "payload.bin").await;
    let done = finish(&mut client, &engine, &seed.id, seed.addr).await;
    drop(done);

    let log = default.join(FAKE_LOG);
    let first = recorded_lines(&log, 1).await;
    assert_eq!(first.lines().filter(|l| !l.trim().is_empty()).count(), 1);

    // The data goes bad and the torrent is checked again: pieces are missing, and it downloads them.
    let file = default.join("payload.bin");
    let size = fs::metadata(&file).expect("the downloaded file").len();
    fs::write(
        &file,
        vec![0u8; usize::try_from(size).expect("a small file")],
    )
    .expect("overwrite the payload with zeros");
    let torrent = engine
        .torrent(&fbt::InfoHashes::from_v1(wire_of(&seed.id)))
        .expect("the session holds the torrent");
    torrent.force_recheck().await.expect("the recheck starts");
    let incomplete = wait_for(&mut client, |snapshot| {
        row_of(snapshot, &seed.id).is_some_and(|row| row.progress < 1.0)
    })
    .await;
    drop(incomplete);

    let again = finish(&mut client, &engine, &seed.id, seed.addr).await;
    drop(again);
    wait_for_settle().await;

    let text = recorded_lines(&log, 2).await;
    wait_for_settle().await;
    let text = {
        let later = recorded_lines(&log, 2).await;
        assert_eq!(text, later, "no third run follows");
        later
    };
    assert_eq!(
        text.lines().filter(|l| !l.trim().is_empty()).count(),
        2,
        "each finish ran the program once: {text}",
    );
    assert_eq!(handle.problems(), (None, None), "nothing went wrong");

    shutdown(client, task).await;
    seed.stop().await;
}
