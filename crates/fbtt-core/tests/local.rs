//! The local backend end to end: a library in a temporary directory, sessions
//! on loopback with nothing that could leave the machine, and a 60 s deadline on
//! every wait so a backend that stops answering fails instead of hanging.

use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use fbtt_core::{
    AddSource, Client, ClientParts, Command, Config, ConfigLoad, Connection, Detail, DetailTab,
    DeviceCommand, DirListing, DownloadsChange, FilePriority, FileRow, LiveLimits, Notice,
    OnCompleteSet, RowState, SettingsChange, SettingsCommand, SettingsState, Snapshot, TorrentId,
};

mod support;
use support::*;

/// The piece length the in-order test hashes its torrent with: 64 KiB, four 16 KiB blocks, so each of
/// its 1 MiB files is exactly 16 pieces. `CreateTorrent`'s own choice for a 3 MiB torrent is the 16 KiB
/// floor, which would make each file 64 pieces and the whole torrent 192 of them, so the test names the
/// length it wants instead of taking the automatic one.
const PIECE64: u32 = 64 * 1024;

/// The `.torrent` for the file or directory at `path`, hashed with an explicit piece length.
async fn torrent_with_piece_len(path: &Path, piece_len: u32) -> Vec<u8> {
    let path = path.to_path_buf();
    within(tokio::task::spawn_blocking(move || {
        fbt::CreateTorrent::new(path)
            .version(fbt::TorrentVersion::V1)
            .piece_len(piece_len)
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

/// The watched file list for `id`, when that is what the detail pane holds.
fn files_of(snapshot: &Snapshot, id: &TorrentId) -> Option<Vec<FileRow>> {
    match snapshot.detail.as_ref() {
        Some((watched, Detail::Files(rows))) if watched == id => Some(rows.clone()),
        _ => None,
    }
}

/// The folders a listing on this machine may name: the download folder, then this host's home directory
/// when it has one. Those two are where a torrent plausibly goes; anywhere else is somebody else's
/// business. A host with no home directory gets the download folder alone.
fn browsable_roots(download_dir: &Path) -> Vec<PathBuf> {
    let downloads = download_dir.to_path_buf();
    match directories::UserDirs::new() {
        Some(dirs) => {
            let home = dirs.home_dir().to_path_buf();
            // The same folder named twice would show up twice in the list someone picks from.
            if home == downloads {
                vec![downloads]
            } else {
                vec![downloads, home]
            }
        }
        None => vec![downloads],
    }
}

/// The listing a snapshot carries as the answer to [`Command::ListDirs`].
fn listing_in(snapshot: &Snapshot) -> DirListing {
    snapshot.dirs.clone().expect("an answer to ListDirs")
}

/// The directories inside one folder, spelled out in the order a listing reports them.
fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
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

/// What `Client::split` is for: every part sees every publish. The binary's loop holds
/// exactly these parts, and its screen once read snapshots from a channel the backend no
/// longer wrote to - the first snapshot shown forever. This is what would have caught it.
#[tokio::test(flavor = "multi_thread")]
async fn split_parts_see_live_snapshots() {
    let lib = Library::new("local_split");
    let (client, task, _) = lib.start().await;
    let ClientParts {
        commands,
        mut snapshots,
        mut notices,
    } = client.split();

    let uri = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=live-parts";
    let id = id_of(
        &fbt::Magnet::parse(uri)
            .expect("a magnet with a made-up hash")
            .info_hashes,
    );
    assert!(
        commands.send(Command::Add {
            source: AddSource::Magnet(uri.to_string()),
            dir: None,
        }),
        "the queue takes the magnet"
    );

    // The row is not there yet and arrives on a later wake: each `changed()` is followed
    // by taking *that* publish's value through `borrow_and_update`, so a receiver that
    // kept answering with its seed value would sit here until the deadline.
    assert!(row_of(&snapshots.borrow_and_update().clone(), &id).is_none());
    within(async {
        loop {
            if snapshots.changed().await.is_err() {
                panic!("the backend stopped before the row appeared");
            }
            if row_of(&snapshots.borrow_and_update().clone(), &id).is_some() {
                break;
            }
        }
    })
    .await;

    // The notice queue is live too, and it is a separate handle: dropping it does not
    // stop the backend, whose life follows the command queue.
    let added = within(notices.recv()).await.expect("a notice for the add");
    assert_eq!(info_text(added), "Added live-parts");

    // Shutdown goes through `commands`, and the join answers the way the binary reads it.
    assert!(commands.send(Command::Shutdown), "the queue takes Shutdown");
    drop(notices);
    drop(snapshots);
    let outcome = within(task).await.expect("the backend stopped cleanly");
    assert_eq!(outcome, Ok(()), "the last save reported no problem");
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

/// A state file this run cannot read at all - here a directory where the file belongs - does
/// not turn into an empty library: nothing is written over it while the run lasts, the notice
/// says both halves of that, and the engine still stops with the task rather than being left
/// running behind the released lock.
#[tokio::test(flavor = "multi_thread")]
async fn unreadable_state_is_never_overwritten() {
    let lib = Library::new("local_unreadable_state");
    fs::create_dir_all(&lib.paths.data_dir).expect("the data directory");
    fs::create_dir(lib.paths.state_file()).expect("a state path that is a directory");

    let (mut client, task, engine) = lib.start().await;
    let problem = error_text(notice(&mut client).await);
    assert!(
        problem.contains("could not be read") && problem.contains("will not save torrents"),
        "the notice says the file is unreadable and that this run keeps its hands off it: {problem}"
    );

    // Something worth saving, so that a backend which saved an empty state would be caught
    // here rather than looking correct.
    let uri = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=keep-off";
    assert!(client.send(Command::Add {
        source: AddSource::Magnet(uri.to_string()),
        dir: None,
    }));
    let id = id_of(&fbt::Magnet::parse(uri).expect("a valid magnet").info_hashes);
    wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;

    shutdown(client, task).await;

    assert!(
        lib.paths.state_file().is_dir(),
        "what could not be read is still there, not a file this run wrote over it"
    );
    // The engine stopped with the task rather than being left running behind a dropped lock:
    // a session that is only dropped closes its sockets and leaves every torrent task writing.
    assert!(
        engine.is_shut_down(),
        "the run shut the engine down on its way out"
    );
}

/// A second unreadable state file does not replace the first copy: `<name>.bad` keeps what run
/// one found and this run's goes to `<name>.bad.1`.
#[tokio::test(flavor = "multi_thread")]
async fn second_corruption_keeps_first_copy() {
    let lib = Library::new("local_corrupt_twice");
    fs::create_dir_all(&lib.paths.data_dir).expect("the data directory");
    let state = lib.paths.state_file();
    let first: &[u8] = b"first garbage";
    fs::write(&state, first).expect("a state file that is not one");

    let (mut client, task, _) = lib.start().await;
    let problem = error_text(notice(&mut client).await);
    assert!(
        problem.contains("session.state.bad"),
        "the notice names the copy: {problem}"
    );
    shutdown(client, task).await;
    let aside = suffixed(&state, ".bad");
    assert_eq!(fs::read(&aside).expect("the first copy"), first);

    // That run wrote a state file of its own. Make it unreadable too and start a third time:
    // the older copy stays where it is and this one goes to the next free name.
    fs::write(&state, b"second garbage").expect("a second state file that is not one");
    let (mut client, task, _) = lib.start().await;
    let problem = error_text(notice(&mut client).await);
    assert!(
        problem.contains("session.state.bad.1"),
        "the second copy takes the next free name: {problem}"
    );
    shutdown(client, task).await;

    assert_eq!(
        fs::read(&aside).expect("the first copy is still readable"),
        first,
        "a corruption from an earlier run is not overwritten by this one"
    );
    assert_eq!(
        fs::read(suffixed(&state, ".bad.1")).expect("the second copy"),
        b"second garbage"
    );
}

/// Resume data that will not come back is copied out from under the state file before anything
/// is saved: a run that restores half a library does not also delete the other half's bytes.
#[tokio::test(flavor = "multi_thread")]
async fn failed_restore_keeps_backup() {
    let lib = Library::new("local_restore_backup");
    fs::create_dir_all(&lib.paths.data_dir).expect("the data directory");
    // A state file that decodes, holding one entry that is not resume data at all.
    let mut saved = fbt::SessionState::default();
    saved.torrents.push(b"not resume data".to_vec());
    let bytes = saved.to_bytes();
    fs::write(lib.paths.state_file(), &bytes).expect("a state file with one bad entry");

    let (mut client, task, _) = lib.start().await;
    let problem = error_text(notice(&mut client).await);
    assert!(
        problem.contains("could not be restored") && problem.contains("session.state.bak"),
        "the notice names the backup the bytes went to: {problem}"
    );
    assert_eq!(
        fs::read(suffixed(&lib.paths.state_file(), ".bak")).expect("the copy of the state file"),
        bytes,
        "the file as it was read is kept, so the entry that failed survives in it"
    );

    shutdown(client, task).await;
}

/// A run that cannot keep its copy of what it could not restore writes no state at all: the save
/// that follows would erase those torrents' only bytes, so they stay under their own name instead.
/// Unix-only, because making a directory refuse a new file is a permission there.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn failed_backup_blocks_state_writes() {
    use fbtt_core::{LibraryLock, LocalBackend};
    use std::os::unix::fs::PermissionsExt;

    let lib = Library::new("local_backup_blocked");
    fs::create_dir_all(&lib.paths.data_dir).expect("the data directory");
    let mut saved = fbt::SessionState::default();
    saved.torrents.push(b"not resume data".to_vec());
    let bytes = saved.to_bytes();
    fs::write(lib.paths.state_file(), &bytes).expect("a state file with one bad entry");

    // The lock first, because from here on nothing may be created in the data directory - and
    // the copy of the state file is the thing that has to fail. The engine's own files go to the
    // download directory, which stays writable, and a held flock needs no room in it either.
    let lock = LibraryLock::acquire(&lib.paths.lock_file()).expect("lock the library");
    fs::set_permissions(&lib.paths.data_dir, fs::Permissions::from_mode(0o500))
        .expect("make the data directory read-only");

    let (mut client, task, engine, _automation) = within(LocalBackend::start_for_test(
        loopback(&lib.config),
        lib.config.clone(),
        &lib.paths,
        lock,
    ))
    .await
    .expect("start the backend on a library it cannot copy");

    let problem = error_text(notice(&mut client).await);
    assert!(
        problem.contains("could not be kept") && problem.contains("will not save torrents"),
        "the notice names the copy that failed and says nothing will be saved: {problem}"
    );

    // Something worth saving, so that a backend which saved anyway would be caught here.
    let uri = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=no-copy";
    assert!(client.send(Command::Add {
        source: AddSource::Magnet(uri.to_string()),
        dir: None,
    }));
    let id = id_of(&fbt::Magnet::parse(uri).expect("a valid magnet").info_hashes);
    wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;

    shutdown(client, task).await;
    fs::set_permissions(&lib.paths.data_dir, fs::Permissions::from_mode(0o700))
        .expect("let the scratch directory be removed again");

    assert!(
        !suffixed(&lib.paths.state_file(), ".bak").exists(),
        "the copy that could not be made left nothing behind"
    );
    assert_eq!(
        fs::read(lib.paths.state_file()).expect("the state file"),
        bytes,
        "a run that could not copy did not save over the bytes it could not restore"
    );
    assert!(
        engine.is_shut_down(),
        "the run shut the engine down on its way out"
    );
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

    match Config::load(&lib.paths).expect("read the config back") {
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

/// What a settings change saves on a library that never chose to serve - which is every library whose
/// `config.yaml` came out of the TUI: rates and a folder, and no `server` key at all. That section is how
/// another run knows an address to offer for this library, so writing one here would have the library claim
/// a server it never started (see `named_server` in the binary, which reads exactly this absence).
#[tokio::test(flavor = "multi_thread")]
async fn tui_save_writes_no_server_section() {
    let lib = Library::new("local_settings_save");
    // The harness' own config names no server, like a run that has only ever driven this library.
    assert_eq!(lib.config.server, None);
    let (mut client, task, _) = lib.start().await;

    let mut limits = LiveLimits::from(lib.config.limits);
    limits.download_kib = 512;
    assert!(client.send(Command::SetLimits(limits)));
    wait_for(&mut client, |snapshot| snapshot.limits.download_kib == 512).await;

    let text = fs::read_to_string(lib.paths.config_file()).expect("the limits save wrote a file");
    assert!(
        text.contains("download_kib: 512"),
        "the setting the person changed is in the file: {text}"
    );
    assert!(
        !text
            .lines()
            .any(|line| line.trim_start().starts_with("server:")),
        "and no server section appears beside it: {text}"
    );

    // The parsed answer, which is what a caller editing the file - and the binary's `named_server` - acts
    // on: no section means no address to offer anybody.
    let ConfigLoad::Loaded(written) = Config::load(&lib.paths).expect("read the saved config back")
    else {
        panic!("a settings save writes a readable file");
    };
    assert_eq!(written.server, None);
    assert_eq!(
        Config::load_for_edit(&lib.paths.config_file())
            .expect("the file reads as a config")
            .expect("the file is there")
            .server,
        None,
        "reading it to edit it invents no section either"
    );

    shutdown(client, task).await;
}

/// The limits save writes the config this run resolved, and the roots default has to survive that:
/// `allowed_roots` stays unnamed in the file, because "no roots named" *is* the download folder. A
/// file that pinned it would leave the server's root behind the day the person moves `download_dir`.
/// The library here is one whose config chose to serve - what a `serve` run leaves behind - because a
/// config with no server section has no roots to pin (see `a_save_without_a_server_section_writes_none`
/// in the config module).
#[tokio::test(flavor = "multi_thread")]
async fn limits_save_keeps_allowed_roots_default() {
    let lib = Library::new("local_limits_roots");
    let mut served = lib.config.clone();
    served.server = Some(fbtt_core::ServerConfig::default());
    let (mut client, task, _) = lib.start_on(served).await;

    let mut limits = LiveLimits::from(lib.config.limits);
    limits.download_kib = 250;
    assert!(client.send(Command::SetLimits(limits)));
    wait_for(&mut client, |snapshot| snapshot.limits.download_kib == 250).await;

    let ConfigLoad::Loaded(written) =
        Config::load(&lib.paths).expect("the limits save wrote a file")
    else {
        panic!("the config the limits save wrote is readable");
    };
    assert_eq!(written.limits.download_kib, 250, "the limit is in the file");
    assert_eq!(
        written.download_dir, lib.config.download_dir,
        "so is the folder this run works on"
    );
    let server = written
        .server
        .as_ref()
        .expect("a save keeps the server this library chose");
    assert!(
        server.allowed_roots.is_empty(),
        "the roots default is not pinned to a directory: {:?}",
        server.allowed_roots
    );

    // The text says the same thing: no `allowed_roots` line, or an empty list.
    let text = fs::read_to_string(lib.paths.config_file()).expect("read the file as text");
    let roots_line = text
        .lines()
        .find(|line| line.trim_start().starts_with("allowed_roots:"));
    assert!(
        roots_line.is_none_or(|line| line.trim() == "allowed_roots: []"),
        "the file leaves the roots unnamed: {text}"
    );

    // And resolving what was written still means the download folder - and moves with it, which is
    // the point of leaving the roots unnamed.
    let downloads = lib.config.download_dir.clone();
    let resolved = written.clone().resolve(&lib.paths);
    assert_eq!(
        resolved
            .server
            .expect("resolving keeps the section")
            .allowed_roots,
        [downloads],
        "unnamed roots mean the download folder"
    );

    let elsewhere = lib.dir.path().join("elsewhere");
    let mut moved = written;
    moved.download_dir = elsewhere.clone();
    let moved = moved.resolve(&lib.paths);
    assert_eq!(
        moved.server.expect("and keeps it").allowed_roots,
        [elsewhere],
        "and the root follows a folder the person moves"
    );

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

/// Playing a file puts it first: the `s` key on one file of a three-file torrent raises that file to High
/// and leaves the other two exactly as they were, the skipped one still skipped.
#[tokio::test(flavor = "multi_thread")]
async fn local_play_raises_the_file() {
    let lib = Library::new("local_play_priority");
    let source = lib.dir.path().join("payload");
    write_file(&source, "a.bin", &sample(64 * 1024 + 7));
    write_file(&source, "b.bin", &sample(33 * 1024 + 5));
    write_file(&source, "c.bin", &sample(12 * 1024 + 3));
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
    assert_eq!(info_text(notice(&mut client).await), "Added payload");
    assert!(client.send(Command::Watch(Some((id.clone(), DetailTab::Files)))));
    let snapshot = wait_for(&mut client, |snapshot| {
        files_of(snapshot, &id).is_some_and(|rows| rows.len() == 3)
    })
    .await;
    let rows = files_of(&snapshot, &id).expect("the file list");
    drop(snapshot);
    let index_of = |name: &str| {
        rows.iter()
            .find(|row| row.path.ends_with(name))
            .unwrap_or_else(|| panic!("{name} is not in the file list: {rows:?}"))
            .index
    };
    let (other, played, skipped) = (index_of("a.bin"), index_of("b.bin"), index_of("c.bin"));

    assert!(client.send(Command::SetFilePriority {
        id: id.clone(),
        file: skipped,
        priority: FilePriority::Skip,
    }));
    wait_for(&mut client, |snapshot| {
        files_of(snapshot, &id).is_some_and(|rows| {
            rows.iter()
                .any(|row| row.index == skipped && row.priority == FilePriority::Skip)
        })
    })
    .await;

    // `s` on the file the person picked, with the torrent nowhere near finished.
    assert!(client.send(Command::Stream {
        id: id.clone(),
        file: Some(played),
    }));
    let target = notice(&mut client).await;
    assert!(
        matches!(target, Notice::Play(_)),
        "pressing Play did not produce a play target: {target:?}"
    );

    let snapshot = wait_for(&mut client, |snapshot| {
        files_of(snapshot, &id).is_some_and(|rows| {
            rows.iter()
                .any(|row| row.index == played && row.priority == FilePriority::High)
        })
    })
    .await;
    let rows = files_of(&snapshot, &id).expect("the file list");
    drop(snapshot);
    let priority_of = |index: usize| {
        rows.iter()
            .find(|row| row.index == index)
            .unwrap_or_else(|| panic!("file {index} is not in the file list: {rows:?}"))
            .priority
    };
    assert_eq!(
        priority_of(played),
        FilePriority::High,
        "the played file is not the one the engine is asked for first: {rows:?}"
    );
    assert_eq!(
        priority_of(other),
        FilePriority::Normal,
        "playing one file changed another: {rows:?}"
    );
    assert_eq!(
        priority_of(skipped),
        FilePriority::Skip,
        "playing one file un-skipped another: {rows:?}"
    );

    shutdown(client, task).await;
}

/// The pieces of the file a play picked arrive in order.
///
/// Two sessions on loopback: one seeding a three-file torrent, one downloading it with nothing on disk.
/// The two files nobody plays are skipped, so the only pieces this downloader wants are the 16 of the
/// played file, all of them the same priority and all of them held by the one peer it dials. That is the
/// state the picker's tie-break decides in: with no file marked in order it takes a random starting point
/// inside that bucket of equal pieces, with the played file marked it starts at the front of it. The
/// torrent is hashed at 64 KiB pieces so each file owns exactly 16 of them, and the seed is connected
/// only after the play, so every piece this torrent asks for is asked for after the file was raised.
#[tokio::test(flavor = "multi_thread")]
async fn played_file_downloads_its_pieces_in_order() {
    let lib = Library::new("local_play_in_order");
    let seed_dir = lib.dir.path().join("seed");
    let source = seed_dir.join("payload");
    for name in ["a.bin", "b.bin", "c.bin"] {
        write_file(&source, name, &sample(MIB));
    }
    let bytes = torrent_with_piece_len(&source, PIECE64).await;
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    let id = id_of(&metainfo.info_hashes);
    let info = &metainfo.info;
    assert_eq!(
        info.piece_len, PIECE64,
        "the torrent was not hashed at the piece length the test asks for"
    );
    assert_eq!(
        info.files.len(),
        3,
        "three files and no pad file is what the piece arithmetic below assumes: {:?}",
        info.files
    );
    // A v1 torrent lays its files end to end in torrent order, so file 1 - the 1 MiB `b.bin`, a whole
    // number of pieces on each side - owns pieces 16 to 31.
    let first = (info.files[0].len / u64::from(PIECE64)) as u32;
    let count = (info.files[1].len / u64::from(PIECE64)) as u32;
    assert!(
        count > 1,
        "the played file has to span pieces for an order to be a fact about it"
    );

    let seed = fbt::Session::new(loopback(&lib.config))
        .await
        .expect("a session to seed with");
    seed.add_torrent(fbt::AddTorrent::new(metainfo.clone(), seed_dir))
        .await
        .expect("seed the torrent");
    wait_until_seeding(&seed).await;
    let seed_addr = seed.local_addr();

    // One piece's worth of block requests in flight to the one peer, and no more. The order the engine
    // reports pieces *finished* in is the order their requests were answered and then hashed, and the
    // engine hashes on a pool of threads: with the default of hundreds of requests in flight, all 16
    // pieces are asked for at once and a loaded machine finishes them in any order (a run under load saw
    // 17 first, another 27 before 25). With a single piece in flight the next piece is only asked for
    // after the previous one's blocks have arrived, so what the test reads - the first piece to finish,
    // and a bounded inversion - is a fact about the order the picker chose, not about thread scheduling.
    let mut settings = loopback(&lib.config);
    settings.max_outstanding_requests =
        usize::try_from(PIECE64 / (16 * 1024)).expect("a small count");
    let lock = fbtt_core::LibraryLock::acquire(&lib.paths.lock_file()).expect("lock the library");
    let (mut client, task, engine, _automation) = within(fbtt_core::LocalBackend::start_for_test(
        settings,
        lib.config.clone(),
        &lib.paths,
        lock,
    ))
    .await
    .expect("start the backend");
    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes),
            dir: Some(lib.config.download_dir.clone()),
        }),
        "the queue takes the torrent"
    );
    assert_eq!(info_text(notice(&mut client).await), "Added payload");
    assert!(client.send(Command::Watch(Some((id.clone(), DetailTab::Files)))));
    let snapshot = wait_for(&mut client, |snapshot| {
        files_of(snapshot, &id).is_some_and(|rows| rows.len() == 3)
    })
    .await;
    let rows = files_of(&snapshot, &id).expect("the file list");
    drop(snapshot);
    let index_of = |name: &str| {
        rows.iter()
            .find(|row| row.path.ends_with(name))
            .unwrap_or_else(|| panic!("{name} is not in the file list: {rows:?}"))
            .index
    };
    let played = index_of("b.bin");

    // Nothing but the played file is wanted: the other two files are skipped, so the only pieces this
    // run asks for are the ones whose order the test reads.
    for file in [index_of("a.bin"), index_of("c.bin")] {
        assert!(client.send(Command::SetFilePriority {
            id: id.clone(),
            file,
            priority: FilePriority::Skip,
        }));
    }
    wait_for(&mut client, |snapshot| {
        files_of(snapshot, &id).is_some_and(|rows| {
            rows.iter()
                .filter(|row| row.priority == FilePriority::Skip)
                .count()
                == 2
        })
    })
    .await;

    let torrent = engine
        .torrent(&fbt::InfoHashes::from_v1(wire_of(&id)))
        .expect("the session holds the torrent it added");
    let hashes = torrent.info_hashes();
    let mut pieces = engine.subscribe(fbt::EventMask::PIECE);

    assert!(client.send(Command::Stream {
        id: id.clone(),
        file: Some(played),
    }));
    let target = notice(&mut client).await;
    assert!(
        matches!(target, Notice::Play(_)),
        "pressing Play did not produce a play target: {target:?}"
    );
    // The priority is what the engine was told before the first piece was asked for, and waiting for it
    // here is also what keeps the peer connection - the thing that starts the requests - behind it.
    wait_for(&mut client, |snapshot| {
        files_of(snapshot, &id).is_some_and(|rows| {
            rows.iter()
                .any(|row| row.index == played && row.priority == FilePriority::High)
        })
    })
    .await;

    torrent
        .add_peer(seed_addr)
        .await
        .expect("the seed is a peer");
    let order = within(async {
        let mut order = Vec::new();
        while (order.len() as u32) < count {
            let event = pieces
                .recv()
                .await
                .expect("the engine kept its events open");
            if let fbt::Event::PieceFinished { torrent, piece } = event
                && torrent == hashes
                && (first..first + count).contains(&piece)
            {
                order.push(piece);
            }
        }
        order
    })
    .await;
    assert_eq!(
        order.first(),
        Some(&first),
        "the first piece of the played file to arrive was not its first: {order:?}"
    );
    // Blocks arrive in the order they were asked for on the one connection, so how far the completion
    // order can stray from the ask order is the number of pieces in flight at once. fbt-lib's own
    // in-order test, on this same geometry, allows eight.
    let inversion = order
        .windows(2)
        .map(|pair| {
            if pair[0] > pair[1] {
                usize::try_from(pair[0] - pair[1]).expect("a piece index fits")
            } else {
                0
            }
        })
        .max()
        .unwrap_or(0);
    assert!(
        inversion <= 8,
        "the played file's pieces came in out of order by more than the pipeline allows: {order:?}"
    );
    assert_eq!(
        torrent.files().await.expect("the file list")[played].priority,
        fbt::Priority::High,
        "the played file was not the one the engine asked for first"
    );

    seed.shutdown().await;
    shutdown(client, task).await;
}

/// The answer for one named folder, asked and taken back: the listing that reports that same path.
async fn listed(client: &mut Client, path: PathBuf) -> DirListing {
    assert!(
        client.send(Command::ListDirs(Some(path.clone()))),
        "the queue takes ListDirs",
    );
    let snapshot = wait_for(client, |snapshot| {
        snapshot
            .dirs
            .as_ref()
            .is_some_and(|dirs| dirs.path.as_deref() == Some(path.as_path()))
    })
    .await;
    let listing = listing_in(&snapshot);
    drop(snapshot);
    listing
}

/// A local folder listing: the folders to start from, and the directories inside one of them. Sorted by
/// their own order rather than whatever the disk handed out, nothing hidden, no files, and a link only
/// where it stays inside a root - the same rules `GET /api/v1/fs/list` answers with, because a person who
/// browses a server's folders and their own should not get two different answers for one folder.
#[tokio::test(flavor = "multi_thread")]
async fn local_list_dirs_roots_and_child() {
    let lib = Library::new("local_list_dirs");
    // Newest name first, so that the order these came off the disk in is not the order they sort into, and
    // with a hidden folder, a file, a link out of every root, and a folder below one of them to walk on.
    for name in ["zeta", ".hidden", "beta", "alpha"] {
        fs::create_dir_all(lib.config.download_dir.join(name)).expect("make a test folder");
    }
    fs::write(lib.config.download_dir.join("a-file"), b"not a folder").expect("write a test file");
    fs::create_dir_all(lib.dir.path().join("outside")).expect("make the folder the link points at");
    fs::create_dir_all(lib.config.download_dir.join("alpha").join("nested"))
        .expect("make a deeper folder");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        lib.dir.path().join("outside"),
        lib.config.download_dir.join("link-out"),
    )
    .expect("make a link that leaves every root");

    let (mut client, task, _) = lib.start().await;

    // Nothing named: the folders to start from, and no read of anything.
    assert!(
        client.send(Command::ListDirs(None)),
        "the queue takes ListDirs"
    );
    let snapshot = wait_for(&mut client, |snapshot| snapshot.dirs.is_some()).await;
    let listing = listing_in(&snapshot);
    drop(snapshot);
    assert_eq!(listing.path, None, "no folder was named");
    assert_eq!(listing.error, None, "naming nothing is not a failure");
    assert_eq!(
        listing.roots,
        browsable_roots(&lib.config.download_dir),
        "the download folder and this host's home directory are where a torrent may go",
    );
    assert!(
        listing.dirs.is_empty(),
        "the list to start from is `roots`, so there is nothing in `dirs` yet: {:?}",
        listing.dirs,
    );

    // One of those folders named: the directories directly inside it, with the hidden folder, the file and
    // the link that leaves every root left out.
    let downloads = lib.config.download_dir.clone();
    let listing = listed(&mut client, downloads.clone()).await;
    assert_eq!(
        listing.error, None,
        "the download folder is there and readable"
    );
    assert_eq!(
        listing.roots,
        browsable_roots(&downloads),
        "every answer comes with the folders it was confined to",
    );
    assert_eq!(
        listing.dirs,
        names(&["alpha", "beta", "zeta"]),
        "the three folders a torrent could go into, in their own order: {:?}",
        listing.dirs,
    );

    // And one step further down, which is how a picker walks.
    let alpha = downloads.join("alpha");
    let listing = listed(&mut client, alpha).await;
    assert_eq!(listing.error, None);
    assert_eq!(
        listing.dirs,
        names(&["nested"]),
        "a folder below a root is inside it",
    );

    shutdown(client, task).await;
}

/// A folder that cannot be listed says so inside the listing rather than as a notice: a picker shows that
/// line beside the folder it failed on and hands back the roots to go instead to, which is more than one
/// line about a failure would tell it. The wording is the one the server's route answers with for the same
/// kind of question, because both are the same listing.
#[tokio::test(flavor = "multi_thread")]
async fn local_list_dirs_error_is_in_listing() {
    let lib = Library::new("local_list_dirs_error");
    fs::create_dir_all(lib.config.download_dir.join("alpha")).expect("make a folder");
    fs::write(lib.config.download_dir.join("a-file"), b"not a folder").expect("write a test file");
    let elsewhere = lib.dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("make a folder outside every root");

    let (mut client, task, _) = lib.start().await;

    // Not there.
    let missing = lib.config.download_dir.join("not-here");
    let listing = listed(&mut client, missing.clone()).await;
    assert_eq!(
        listing.path.as_deref(),
        Some(missing.as_path()),
        "the answer is about the folder that was asked for",
    );
    assert_eq!(
        listing.dirs,
        names(&[]),
        "a folder that is not there has no names to report",
    );
    let error = listing
        .error
        .expect("the reason for a failure travels with the answer");
    assert!(
        error.contains("no folder"),
        "it says what is wrong: {error}"
    );
    assert_eq!(
        listing.roots,
        browsable_roots(&lib.config.download_dir),
        "and it still hands back the folders to go instead to",
    );
    assert!(
        client.try_notice().is_none(),
        "a failed listing is said once, where a person can act on it"
    );

    // Somewhere else entirely: below this scratch library, outside both roots.
    let error = listed(&mut client, elsewhere.clone())
        .await
        .error
        .expect("a folder outside every root is refused");
    assert!(
        error.contains("outside the allowed folders"),
        "it says which rule it broke: {error}",
    );

    // There, and readable, but not a folder, so nothing can be inside it.
    let file = lib.config.download_dir.join("a-file");
    let listing = listed(&mut client, file).await;
    let error = listing.error.expect("a file is nowhere a torrent goes");
    assert!(
        error.contains("is not a folder"),
        "it says what it found: {error}"
    );

    // A good folder after three bad ones answers normally: a failure is one answer, not the end of the
    // listing, and it leaves nothing behind that the next question inherits.
    let listing = listed(&mut client, lib.config.download_dir.clone()).await;
    assert_eq!(listing.error, None, "the download folder still lists");
    assert_eq!(
        listing.dirs,
        names(&["alpha"]),
        "only the folder is somewhere to go"
    );

    shutdown(client, task).await;
}

/// Devices are a server's list to keep: which keys may sign in lives in that server's `config.yaml`, and a
/// library running its own engine has no device table to read, add to or remove from. It says so for every
/// question of that shape - including one whose key it would not have accepted - rather than answering with
/// an empty list somebody would read as "no devices".
#[tokio::test(flavor = "multi_thread")]
async fn local_devices_is_a_notice() {
    let lib = Library::new("local_devices");
    let (mut client, task, _) = lib.start().await;

    for command in [
        DeviceCommand::List,
        // Not a key this server would parse: refusing devices comes before looking at the key.
        DeviceCommand::Add {
            name: "laptop".to_string(),
            public_key: "ssh-ed25519 AAAA-not-a-key alice@laptop".to_string(),
        },
        DeviceCommand::Remove("laptop".to_string()),
    ] {
        assert!(
            client.send(Command::Devices(command.clone())),
            "the queue takes {command:?}",
        );
        assert_eq!(
            error_text(notice(&mut client).await),
            "Devices are managed on a server",
            "{command:?} got the answer a device command gets here",
        );
    }

    let snapshot = client.snapshot();
    assert!(
        snapshot.devices.is_none(),
        "a library with no server has no device list, not an empty one",
    );
    drop(snapshot);

    shutdown(client, task).await;
}

/// The settings the snapshot carries, when they have been loaded.
fn loaded(snapshot: &Snapshot) -> Option<fbtt_core::SettingsView> {
    match &snapshot.settings {
        SettingsState::Loaded(view) => Some(view.clone()),
        _ => None,
    }
}

/// A downloads change that keeps what the library has, going to `default_dir`.
fn downloads_to(default_dir: PathBuf) -> DownloadsChange {
    DownloadsChange {
        default_dir,
        unfinished_dir: None,
        start_paused: false,
        watch_dir: None,
        on_complete: None,
    }
}

/// `Load` fills the settings in, and a speed save changes the running view, says `Saved.` and writes the
/// limits to the file.
#[tokio::test(flavor = "multi_thread")]
async fn local_settings_load_and_save_speed_persist() {
    let lib = Library::new("local_settings_speed");
    let (mut client, task, _) = lib.start().await;
    assert_eq!(client.snapshot().settings, SettingsState::NotLoaded);

    assert!(client.send(Command::Settings(SettingsCommand::Load)));
    let first = wait_for(&mut client, |snapshot| loaded(snapshot).is_some()).await;
    let view = loaded(&first).expect("loaded");
    assert_eq!(view.speed, LiveLimits::from(lib.config.limits));
    assert_eq!(view.downloads.default_dir, lib.config.download_dir);
    drop(first);

    let mut limits = LiveLimits::from(lib.config.limits);
    limits.download_kib = 123;
    assert!(client.send(Command::Settings(SettingsCommand::Save(
        SettingsChange::Speed(limits)
    ))));
    assert_eq!(info_text(notice(&mut client).await), "Saved.");
    let saved = wait_for(&mut client, |snapshot| {
        loaded(snapshot).is_some_and(|view| view.speed.download_kib == 123)
    })
    .await;
    assert_eq!(saved.limits.download_kib, 123);
    drop(saved);

    match Config::load(&lib.paths).expect("read the config back") {
        ConfigLoad::Loaded(config) => assert_eq!(config.limits.download_kib, 123),
        other => panic!("the new limits are in the config file, got {other:?}"),
    }
    shutdown(client, task).await;
}

/// A downloads save moves the default folder: the snapshot says so, and a torrent added with no folder
/// lands there. The file holds it too.
#[tokio::test(flavor = "multi_thread")]
async fn local_downloads_save_changes_default_dir_for_new_adds() {
    let lib = Library::new("local_settings_default_dir");
    let (mut client, task, _) = lib.start().await;
    let elsewhere = lib.dir.path().join("elsewhere");

    let mut change = downloads_to(elsewhere.clone());
    change.start_paused = true;
    assert!(client.send(Command::Settings(SettingsCommand::Save(
        SettingsChange::Downloads(change)
    ))));
    assert_eq!(info_text(notice(&mut client).await), "Saved.");
    let snapshot = wait_for(&mut client, |snapshot| snapshot.default_dir == elsewhere).await;
    assert_eq!(
        loaded(&snapshot)
            .expect("a save publishes the view")
            .downloads
            .default_dir,
        elsewhere
    );
    drop(snapshot);

    let uri = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=lands-there";
    let id = id_of(&fbt::Magnet::parse(uri).expect("a magnet").info_hashes);
    assert!(client.send(Command::Add {
        source: AddSource::Magnet(uri.to_string()),
        dir: None,
    }));
    let added = wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    assert_eq!(
        row_of(&added, &id).expect("the row").save_path,
        elsewhere,
        "an add with no folder goes where the save said"
    );
    drop(added);

    match Config::load(&lib.paths).expect("read the config back") {
        ConfigLoad::Loaded(config) => {
            assert_eq!(config.download_dir, elsewhere);
            assert!(config.downloads.start_paused);
        }
        other => panic!("the downloads section is in the file, got {other:?}"),
    }
    shutdown(client, task).await;
}

/// The program is the TUI's own to set, because the engine runs in this process.
#[tokio::test(flavor = "multi_thread")]
async fn local_program_is_editable() {
    let lib = Library::new("local_settings_program");
    let (mut client, task, _) = lib.start().await;
    assert!(client.send(Command::Settings(SettingsCommand::Load)));
    let snapshot = wait_for(&mut client, |snapshot| loaded(snapshot).is_some()).await;
    let view = loaded(&snapshot).expect("loaded");
    assert!(view.downloads.on_complete.editable);
    assert_eq!(view.downloads.on_complete.program, None);
    drop(snapshot);

    // A program that exists and is never run: validation looks, and the engine is not asked to finish
    // anything here.
    let program = std::env::current_exe().expect("this test's own binary");
    let mut change = downloads_to(lib.config.download_dir.clone());
    change.on_complete = Some(OnCompleteSet {
        program: Some(program.clone()),
    });
    assert!(client.send(Command::Settings(SettingsCommand::Save(
        SettingsChange::Downloads(change)
    ))));
    assert_eq!(info_text(notice(&mut client).await), "Saved.");
    let snapshot = wait_for(&mut client, |snapshot| {
        loaded(snapshot).is_some_and(|view| view.downloads.on_complete.program.is_some())
    })
    .await;
    assert_eq!(
        loaded(&snapshot)
            .expect("loaded")
            .downloads
            .on_complete
            .program,
        Some(program)
    );
    drop(snapshot);
    shutdown(client, task).await;
}

/// A change that fails validation says why and changes nothing: not the snapshot, not the file, and not
/// where the next add goes.
#[tokio::test(flavor = "multi_thread")]
async fn local_invalid_downloads_is_an_error_and_changes_nothing() {
    let lib = Library::new("local_settings_invalid");
    let (mut client, task, _) = lib.start().await;
    assert!(client.send(Command::Settings(SettingsCommand::Load)));
    wait_for(&mut client, |snapshot| loaded(snapshot).is_some()).await;

    // The unfinished folder is the default folder, which is a refusal on every host.
    let elsewhere = lib.dir.path().join("elsewhere");
    let mut change = downloads_to(elsewhere.clone());
    change.unfinished_dir = Some(elsewhere.clone());
    assert!(client.send(Command::Settings(SettingsCommand::Save(
        SettingsChange::Downloads(change)
    ))));
    assert_eq!(
        error_text(notice(&mut client).await),
        "The unfinished folder must be different from the default folder."
    );

    // A command after it is answered after it, so by the time this snapshot is here the refused one has
    // been handled.
    assert!(client.send(Command::Settings(SettingsCommand::Load)));
    let snapshot = wait_for(&mut client, |snapshot| loaded(snapshot).is_some()).await;
    assert_eq!(snapshot.default_dir, lib.config.download_dir);
    assert_eq!(
        loaded(&snapshot).expect("loaded").downloads.default_dir,
        lib.config.download_dir
    );
    assert_eq!(
        loaded(&snapshot).expect("loaded").downloads.unfinished_dir,
        None
    );
    drop(snapshot);
    assert!(
        !lib.paths.config_file().exists(),
        "a refused save writes no file"
    );
    shutdown(client, task).await;
}

/// The order is validate, write, apply: a write that fails applies nothing.
#[tokio::test(flavor = "multi_thread")]
async fn local_failed_write_applies_nothing() {
    let lib = Library::new("local_settings_unwritable");
    // A folder where the file belongs: it cannot be replaced by a file.
    fs::create_dir_all(lib.paths.config_file()).expect("a folder in the file's place");
    let (mut client, task, _) = lib.start().await;
    let elsewhere = lib.dir.path().join("elsewhere");

    assert!(client.send(Command::Settings(SettingsCommand::Save(
        SettingsChange::Downloads(downloads_to(elsewhere))
    ))));
    let line = error_text(notice(&mut client).await);
    assert!(!line.is_empty());

    assert!(client.send(Command::Settings(SettingsCommand::Load)));
    let snapshot = wait_for(&mut client, |snapshot| loaded(snapshot).is_some()).await;
    assert_eq!(
        snapshot.default_dir, lib.config.download_dir,
        "nothing was applied for a save that was not written"
    );
    drop(snapshot);
    shutdown(client, task).await;
}
