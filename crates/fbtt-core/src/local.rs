//! The backend over the embedded engine: one task owns the session.
//!
//! A `fbt::Session` is cheap to clone but is not something two places should drive at
//! once, so exactly one task holds it - through the [`Engine`] that owns the library and its
//! state file. That task takes commands off a queue, answers them, and publishes a snapshot of
//! what the session looks like afterwards. Nothing else touches the engine, so there is no lock
//! between the screen and the swarm, and a command that waits on a peer cannot hold a keystroke
//! hostage: the screen is not in this task at all.
//!
//! The task ends with [`Engine::shutdown`], which writes the state file and only then lets go of
//! the library lock. A second featherbittorrent therefore starts on either the old state or the
//! new one, never between them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use directories::UserDirs;
use fbt::{
    AddTorrent, InfoHashV1, InfoHashes, Magnet, Metainfo, Priority, RateLimit, RemoveMode, Session,
    SessionError, Settings, TorrentHandle,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::client::{
    AddSource, Client, Command, Connection, Detail, DetailTab, DirListing, FilePriority,
    LiveLimits, Notice, Snapshot, TorrentId, TorrentRow, Totals,
};
use crate::config::{Config, ConfigLoad, Limits};
use crate::engine::{Engine, SAVE_EVERY, map};
use crate::fs_list;
use crate::lock::LibraryLock;
use crate::paths::Paths;

/// How often the snapshot is rebuilt, and with it the watched pane. Short enough that
/// a rate looks alive, long enough that nobody pays for it.
const TICK: Duration = Duration::from_millis(500);
/// What the screen is told when an id names nothing this session holds. Ids come from
/// the screen (and, from milestone 2, from a server), so a stale or malformed one is an
/// ordinary answer rather than a failure.
const NO_TORRENT: &str = "the torrent is not in this library";
/// What the screen is told when it asks about devices: which keys may sign in to a featherbittorrent
/// server is that server's `config.yaml` to hold, and a library running its own engine has no list of
/// them - not an empty one, none at all.
const DEVICES_ON_A_SERVER: &str = "Devices are managed on a server";
/// What the screen is told when it asks to play a file, before this release can.
const NOT_YET: &str = "Streaming is not available yet";

/// The backend that runs the torrents in this process.
///
/// It has no state of its own: each [`start`](Self::start) spawns the task that owns one engine -
/// its session, its state file and its lock - and hands back the client that talks to it. What the
/// engine does not know is what a screen wants from it: the snapshot, the watched pane and the
/// config file are this backend's work.
#[derive(Debug)]
pub struct LocalBackend;

impl LocalBackend {
    /// Starts the engine on `paths`, restoring whatever its state file holds, on the settings
    /// [`Config::session_settings`] describes. A `config` whose download folder is relative gets
    /// it rooted in `paths` first ([`Config::resolve`]), which is what a run that read a
    /// hand-written config file hands in.
    ///
    /// `lock` is this library's, taken with [`LibraryLock::acquire`]: the backend holds it until
    /// its state is written. The returned handle resolves once `Shutdown` has been answered and
    /// that file is on disk, which is what a quit (or a test) waits on before starting again on
    /// the same directories. Its `Err` is one line about a final save that did not happen: the
    /// library on disk is then the one from before this run, and only the person running it can
    /// act on that, so the binary prints it after putting the terminal back. Whatever the state
    /// file could not be read as, or could not be kept, arrives as notices on the client.
    pub async fn start(
        config: Config,
        paths: &Paths,
        lock: LibraryLock,
    ) -> Result<(Client, JoinHandle<Result<(), String>>), SessionError> {
        let (client, task, _) = spawn(config.session_settings(), config, paths, lock).await?;
        Ok((client, task))
    }

    /// [`start`](Self::start) with the engine settings spelled out rather than derived
    /// from the config, which is how a test keeps every session on loopback.
    pub async fn start_with(
        settings: Settings,
        config: Config,
        paths: &Paths,
        lock: LibraryLock,
    ) -> Result<(Client, JoinHandle<Result<(), String>>), SessionError> {
        let (client, task, _) = spawn(settings, config, paths, lock).await?;
        Ok((client, task))
    }

    /// [`start_with`](Self::start_with), plus a clone of the session it started so that
    /// a test can hand the engine a peer of its own. Needs the `test-support` feature.
    #[cfg(feature = "test-support")]
    pub async fn start_for_test(
        settings: Settings,
        config: Config,
        paths: &Paths,
        lock: LibraryLock,
    ) -> Result<(Client, JoinHandle<Result<(), String>>, Session), SessionError> {
        spawn(settings, config, paths, lock).await
    }
}

/// Starts the engine and the task that owns it, `paths` and `lock`.
///
/// The engine's own problems with the state file - what it could not read, what it kept aside,
/// what it therefore refuses to write - come back with it as notices, one line each, and go out on
/// the new client before its first snapshot is anything a screen could act on. The engine clone
/// comes back with the client so a caller can drive the same session.
async fn spawn(
    settings: Settings,
    config: Config,
    paths: &Paths,
    lock: LibraryLock,
) -> Result<(Client, JoinHandle<Result<(), String>>, Session), SessionError> {
    // Whatever the config file named for downloads, this run works on an absolute
    // directory (`Config::resolve`), so a snapshot's default folder, an add with no
    // folder typed in, and the fallback a save writes are all the same place.
    let config = config.resolve(paths);
    let (mut engine, problems) = Engine::start(settings, paths, lock).await?;

    // The first snapshot is taken here rather than in the task, so a client never sees
    // an empty library that is about to fill up with restored torrents.
    let mut stalled = HashMap::new();
    let (torrents, totals) = rows(engine.session(), Instant::now(), &mut stalled);
    let initial = Arc::new(Snapshot {
        connection: Connection::Local,
        torrents,
        totals,
        limits: map::limits(engine.session()),
        detail: None,
        default_dir: config.download_dir.clone(),
        dirs: None,
        devices: None,
    });
    let (client, channels) = Client::paired(Arc::clone(&initial));
    // The screen may not be reading yet, so these wait in the queue, and past its room they are
    // dropped: that is what a notice always means. A restore reports one per torrent it could not
    // add, which is how many there can be.
    for notice in problems {
        let _ = channels.notices.try_send(notice);
    }
    // A save that leaves a torrent out says so torrent by torrent, and those lines belong on the
    // screen beside everything else this backend reports.
    engine.report_to(channels.notices.clone());

    let session = engine.session().clone();
    #[cfg(feature = "test-support")]
    let client = client.with_session(&session);

    let backend = Backend {
        engine,
        config,
        paths: paths.clone(),
        watching: None,
        dirs: None,
        stalled,
        published: initial,
        snapshot_tx: channels.snapshot,
        notices: channels.notices,
    };
    let task = tokio::spawn(backend.run(channels.commands));
    Ok((client, task, session))
}

/// The table and the header totals for the session as it is now.
///
/// `stalled` is updated as it goes, one entry per torrent the session holds: [`map::stalled_clock`]
/// keeps a torrent's count running only while it is downloading and taking nothing, and starts it
/// again from `now` at every other moment, so a download is only called stalled from the last time
/// it had a chance to make progress. A torrent no longer in the session loses its entry.
fn rows(
    session: &Session,
    now: Instant,
    stalled: &mut HashMap<TorrentId, Instant>,
) -> (Vec<TorrentRow>, Totals) {
    let seen = std::mem::take(stalled);
    let mut next = HashMap::with_capacity(seen.len());
    let mut down_rate = 0u64;
    let mut up_rate = 0u64;
    let torrents = session
        .torrents_status()
        .into_iter()
        .map(|status| {
            let id = TorrentId::from_wire_hash(status.info_hashes.wire_hash());
            let zero_since = map::stalled_clock(
                status.state,
                status.download_rate,
                seen.get(&id).copied(),
                now,
            );
            let row = map::row(&status, zero_since, now);
            next.insert(id, zero_since);
            down_rate = down_rate.saturating_add(status.download_rate);
            up_rate = up_rate.saturating_add(status.upload_rate);
            row
        })
        .collect();
    *stalled = next;
    let totals = Totals {
        down_rate,
        up_rate,
        dht_nodes: session
            .dht_stats()
            .map(|stats| stats.nodes_v4.saturating_add(stats.nodes_v6)),
    };
    (torrents, totals)
}

/// The folders a listing on this machine may name: the download folder this run saves to, and the
/// person's own home directory when their host has one.
///
/// Home is in the list because that is where everything else they own already is, and it is also the
/// bound that keeps a picker inside the two places a torrent plausibly goes rather than anywhere on the
/// file system. This costs no directory reads - which is why the backend's own thread may call it, while
/// everything that does read a directory happens in the blocking task ([`Backend::list_dirs"]). A host
/// with no home directory gets the download folder alone, which is the one thing a listing can say here.
fn browse_paths(download_dir: &Path) -> Vec<PathBuf> {
    let mut given = vec![download_dir.to_path_buf()];
    if let Some(dirs) = UserDirs::new() {
        let home = dirs.home_dir().to_path_buf();
        // The same folder named twice would show up twice in the list someone picks from.
        if !given.contains(&home) {
            given.push(home);
        }
    }
    given
}

/// One browse of this machine's folders, answered: the folders to start from when none was named, and
/// otherwise the directories inside the one that was, under the rules every featherbittorrent listing
/// uses ([`fs_list`]) - sorted, nothing hidden, names fit to draw, at most [`fs_list::MAX_DIRS`] of them,
/// a link only where it stays inside a root, and no file among them.
///
/// A folder that cannot be listed says so inside the listing rather than as a notice: the picker shows
/// that line beside the folder it failed on and can go back to `roots`, which a notice alone would not
/// have told it. Runs on a blocking thread, because this is where the directory reads happen.
fn listing_of(given: Vec<PathBuf>, path: Option<PathBuf>) -> DirListing {
    let Some(path) = path else {
        // Nothing named means nothing to read: the folders to start from were decided before anyone
        // asked, and there are no directories to report below them until someone names one.
        return DirListing {
            path: None,
            roots: given,
            dirs: Vec::new(),
            error: None,
        };
    };
    let allowed = fs_list::browse_roots(&given);
    let (dirs, error) = match fs_list::list_dirs(&allowed, &path) {
        Ok(dirs) => (dirs, None),
        Err(problem) => (Vec::new(), Some(problem.to_string())),
    };
    DirListing {
        path: Some(path),
        roots: given,
        dirs,
        error,
    }
}

/// The task's side of the library: the engine it drives, and what a screen wants built on top of
/// it - the snapshot, the watched pane, the config file the limits are saved to.
struct Backend {
    /// The engine this task drives alone: its session, its state file and this library's lock,
    /// which it releases after the last save.
    engine: Engine,
    /// What the run started with, its download folder already rooted in the library
    /// (see [`Config::resolve`]), kept current so a missing config file has something
    /// sensible to fall back to.
    config: Config,
    /// Where `config.yaml` and `session.state` are.
    paths: Paths,
    /// The pane to keep fresh, and for which torrent.
    watching: Option<(TorrentId, DetailTab)>,
    /// The last folder listing that was asked for, kept so that every snapshot after it still carries
    /// an answer the picker is drawn from: a screen reads snapshots, not replies to its own commands.
    /// `None` until one is asked for.
    dirs: Option<DirListing>,
    /// When each torrent's stalled count last started running: while it is downloading and
    /// taking nothing. See [`map::stalled_clock`].
    stalled: HashMap<TorrentId, Instant>,
    /// The snapshot the screen is looking at, so that only a change publishes.
    published: Arc<Snapshot>,
    /// Where snapshots go out.
    snapshot_tx: watch::Sender<Arc<Snapshot>>,
    /// Where one-line feedback goes out. The engine holds its own handle on the same queue (see
    /// [`Engine::report_to`]), so a torrent left out of a save reaches the screen too.
    notices: mpsc::Sender<Notice>,
}

impl Backend {
    /// Answers commands and keeps the snapshot fresh until asked to stop, then saves and releases
    /// the library. The lock goes with the engine at the end of that save, whatever path got here.
    ///
    /// The `Err` is one line about a final save that did not happen. Nothing in this process
    /// can fix that - the library on disk is the one from before this run - so it travels out
    /// of the task for the binary to print after restoring the terminal.
    async fn run(mut self, mut commands: mpsc::Receiver<Command>) -> Result<(), String> {
        // Both intervals are ready at once on their first tick. The snapshot's goes with
        // it; the save's would write a file that has not changed since it was read, so
        // that one is spent before the loop starts.
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut save = tokio::time::interval(SAVE_EVERY);
        save.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        save.tick().await;

        loop {
            // Which thing woke the task. A command and a tick do the same work afterwards
            // (rebuild the snapshot), so only the two kinds of work get names.
            enum Wake {
                Command(Command),
                Tick,
                Save,
            }
            let wake = tokio::select! {
                command = commands.recv() => match command {
                    // Every client gone is the same as one asking to stop: nobody is
                    // watching any more, and the library should be left tidy.
                    Some(command) => Wake::Command(command),
                    None => Wake::Command(Command::Shutdown),
                },
                _ = tick.tick() => Wake::Tick,
                _ = save.tick() => Wake::Save,
            };
            match wake {
                // The engine consumes itself here: the session goes with it, and so does the lock,
                // after the state file was written or failed to be. Another featherbittorrent may
                // start on the new state from that moment, never beside a half-written one.
                Wake::Command(Command::Shutdown) => return self.engine.shutdown().await,
                Wake::Command(command) => self.handle(command).await,
                Wake::Tick => {}
                Wake::Save => {
                    if let Some(problem) = self.engine.save_periodic().await {
                        self.notify(problem);
                    }
                }
            }
            self.publish().await;
        }
    }

    /// Rebuilds the snapshot and publishes it only when it differs from what is out
    /// there, so a screen woken by `changed()` always has something new to draw.
    async fn publish(&mut self) {
        let (torrents, totals) = rows(self.engine.session(), Instant::now(), &mut self.stalled);
        let detail = match self.watching.clone() {
            Some((id, tab)) => self.detail_for(&id, tab).await.map(|detail| (id, detail)),
            None => None,
        };
        let next = Arc::new(Snapshot {
            connection: Connection::Local,
            torrents,
            totals,
            limits: map::limits(self.engine.session()),
            detail,
            default_dir: self.config.download_dir.clone(),
            // The last listing stays with the snapshot until another one replaces it, and a library that
            // runs its own engine has no device list to publish: `Devices` answers that with a notice.
            dirs: self.dirs.clone(),
            devices: None,
        });
        if next == self.published {
            return;
        }
        self.published = Arc::clone(&next);
        // Fails only when every client is gone, which the closed command queue answers.
        let _ = self.snapshot_tx.send(next);
    }

    /// One command, answered. `Shutdown` never reaches here: the loop stops on it before
    /// doing any work.
    async fn handle(&mut self, command: Command) {
        match command {
            Command::Add { source, dir } => self.add(source, dir).await,
            Command::Pause(id) => match self.torrent(&id) {
                Some(torrent) => self.report(torrent.pause().await),
                None => self.no_torrent(),
            },
            Command::Resume(id) => match self.torrent(&id) {
                Some(torrent) => self.report(torrent.resume().await),
                None => self.no_torrent(),
            },
            Command::Remove { id, delete_files } => match self.torrent(&id) {
                Some(torrent) => {
                    let mode = if delete_files {
                        RemoveMode::DeleteData
                    } else {
                        RemoveMode::KeepData
                    };
                    self.report(torrent.remove(mode).await);
                }
                None => self.no_torrent(),
            },
            Command::SetFilePriority { id, file, priority } => {
                self.set_file_priority(&id, file, priority).await;
            }
            Command::SetLimits(limits) => self.set_limits(limits).await,
            // The next publish brings this pane, so the answer is already on its way.
            Command::Watch(target) => self.watching = target,
            // The listing is the answer, and it goes where a picker reads it: see [`Self::list_dirs`].
            Command::ListDirs(path) => self.list_dirs(path).await,
            // Which keys may sign in to a server is that server's `config.yaml` to hold. A library
            // running its own engine has no device list to change, and says so rather than pretending
            // to have an empty one.
            Command::Devices(_) => self.notify(Notice::Error(DEVICES_ON_A_SERVER.to_owned())),
            // Playing a file needs a path or a loopback stream, which arrive with the next tasks.
            Command::Stream { .. } => self.notify(Notice::Error(NOT_YET.to_owned())),
            Command::Shutdown => {}
        }
    }

    /// Adds a torrent, into `dir` or into the directory the config sends downloads to.
    /// Whatever went wrong - a magnet that is not one, a `.torrent` that does not parse,
    /// a torrent already here - comes back as a notice rather than stopping anything.
    async fn add(&mut self, source: AddSource, dir: Option<PathBuf>) {
        let save_path = dir.unwrap_or_else(|| self.config.download_dir.clone());
        let params = match source {
            AddSource::Magnet(uri) => Magnet::parse(&uri)
                .map(|magnet| AddTorrent::magnet(magnet, save_path))
                .map_err(|error| error.to_string()),
            AddSource::File(bytes) => Metainfo::from_bytes(&bytes)
                .map(|metainfo| AddTorrent::new(metainfo, save_path))
                .map_err(|error| error.to_string()),
        };
        let params = match params {
            Ok(params) => params,
            Err(problem) => return self.notify(Notice::Error(map::clean(&problem))),
        };
        match self.engine.session().add_torrent(params).await {
            // The name is the engine's own: for a magnet its `dn`, until peers confirm
            // what the torrent actually is. A magnet with no `dn` has nothing to call it
            // by yet, so its hash stands in for the name.
            Ok(torrent) => {
                let name = map::clean(&torrent.status().name);
                let shown = if name.is_empty() {
                    TorrentId::from_wire_hash(torrent.info_hashes().wire_hash()).0
                } else {
                    name
                };
                self.notify(Notice::Info(format!("Added {shown}")));
            }
            Err(error) => self.notify(Notice::Error(map::clean(&error.to_string()))),
        }
    }

    /// One file's priority. The engine wants one entry per file, so the list is read
    /// back, the named entry changed, and the whole thing sent again. Pad files stay in
    /// it: their indices are what the pane names rows by.
    async fn set_file_priority(&self, id: &TorrentId, file: usize, priority: FilePriority) {
        let Some(torrent) = self.torrent(id) else {
            return self.no_torrent();
        };
        let files = match torrent.files().await {
            Ok(files) => files,
            // A magnet whose metadata has not arrived has no file list to change.
            Err(error) => return self.notify(Notice::Error(map::clean(&error.to_string()))),
        };
        let mut priorities: Vec<Priority> = vec![Priority::Normal; files.len()];
        for entry in &files {
            if let Some(slot) = priorities.get_mut(entry.index) {
                *slot = entry.priority;
            }
        }
        match priorities.get_mut(file) {
            Some(slot) => *slot = map::priority(priority),
            // Ids and indices both come from the screen, and a pane that has not caught up
            // with a removal names an index that is no longer there.
            None => return self.notify(Notice::Error(format!("no file with index {file}"))),
        }
        self.report(torrent.set_file_priorities(priorities).await);
    }

    /// Changes the running rates and queue counts, then writes them to `config.yaml` so
    /// the next run starts under the same limits.
    async fn set_limits(&mut self, limits: LiveLimits) {
        let session = self.engine.session();
        session.set_rate_limit(RateLimit::new(
            map::rate_bytes(limits.download_kib),
            map::rate_bytes(limits.upload_kib),
        ));
        let mut queue = session.queue_settings();
        queue.active_downloads = limits.active_downloads;
        queue.active_seeds = limits.active_seeds;
        queue.active_limit = limits.active_total;
        // The counts are the engine's own to refuse, so it answers rather than us guessing.
        let outcome = session.set_queue_settings(queue);
        self.report(outcome);
        self.config.limits = Limits::from(limits);
        self.save_config().await;
    }

    /// Writes the config file with these limits, leaving everything else in it alone: it
    /// is read first, only `limits` changes. The write goes through a blocking thread
    /// because this task answers commands and should not wait on a disk.
    async fn save_config(&self) {
        let paths = self.paths.clone();
        let fallback = self.config.clone();
        let limits = self.config.limits;
        let outcome = tokio::task::spawn_blocking(move || {
            let file = paths.config_file();
            let mut config = match Config::load(&paths) {
                Ok(ConfigLoad::Loaded(config)) => config,
                // A file we could not parse is already kept aside as `.bad`; the
                // library's own defaults it came with are what to write on.
                Ok(ConfigLoad::Bad { config, .. }) => config,
                // The file was read and is left where it is: its settings, the player's at
                // its default, are what to write on.
                Ok(ConfigLoad::PlayerProblem { config, .. }) => config,
                // No file yet, or one that could not be opened at all: keep what this run
                // started with, which is what the file would have said - its download
                // folder included, already rooted in the library.
                Ok(ConfigLoad::Missing) | Err(_) => fallback,
            };
            config.limits = limits;
            config.save(&file).map_err(|error| error.to_string())
        })
        .await;
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(problem)) => self.notify(Notice::Error(map::clean(&problem))),
            Err(joined) => self.notify(Notice::Error(format!(
                "the config could not be written: {joined}"
            ))),
        }
    }

    /// One [`Command::ListDirs`] answered, on the blocking pool where reading directories belongs: this
    /// task answers commands and does not wait on a disk for any of them (see [`save_config`](Self::save_config)
    /// for the same arrangement). The answer is kept in `self.dirs` so the publish that follows this
    /// command carries it, and every publish after that keeps carrying it until another listing replaces
    /// it - a screen draws snapshots rather than replies.
    ///
    /// A folder that could not be listed is still an answer: its reason goes in the listing and nothing
    /// is sent to the notice queue, because a picker shows that reason beside the folder it failed on.
    async fn list_dirs(&mut self, path: Option<PathBuf>) {
        let asked = path.clone();
        // Which folders may be named at all is this backend's own rule, and reading it costs no directory
        // reads (see `browse_paths`); the task below does all of those.
        let browsable = browse_paths(&self.config.download_dir);
        let roots = browsable.clone();
        let outcome = tokio::task::spawn_blocking(move || listing_of(browsable, path)).await;
        self.dirs = Some(match outcome {
            Ok(listing) => listing,
            // A blocking task that was dropped takes its answer with it, which is the only way this
            // command fails outright. The reason goes where a picker reads reasons, and the root list
            // comes from the same rule the answer would have used.
            Err(joined) => DirListing {
                path: asked,
                roots,
                dirs: Vec::new(),
                error: Some(format!("the folder could not be listed: {joined}")),
            },
        });
    }

    /// The watched pane, or `None` when there is nothing to show for it: the torrent went
    /// away, or its detail cannot be taken yet (a magnet whose metadata has not arrived).
    async fn detail_for(&self, id: &TorrentId, tab: DetailTab) -> Option<Detail> {
        let torrent = self.torrent(id)?;
        match tab {
            DetailTab::Files => torrent
                .files()
                .await
                .ok()
                .map(|files| Detail::Files(map::file_rows(files))),
            DetailTab::Peers => torrent
                .peers()
                .await
                .ok()
                .map(|peers| Detail::Peers(map::peer_rows(peers))),
            DetailTab::Trackers => torrent
                .trackers()
                .await
                .ok()
                .map(|trackers| Detail::Trackers(map::tracker_rows(trackers))),
            // `pieces()` is the verified bitmap, which is exactly the row of squares.
            DetailTab::Pieces => torrent.pieces().await.ok().map(|pieces| Detail::Pieces {
                have: (0..pieces.len()).map(|piece| pieces.get(piece)).collect(),
            }),
        }
    }

    /// The session's handle for a row id. A row id is the v1 hash, or the first twenty
    /// bytes of a v2-only torrent's, and the session finds either by its wire hashes.
    fn torrent(&self, id: &TorrentId) -> Option<TorrentHandle> {
        let v1: InfoHashV1 = id.0.parse().ok()?;
        self.engine.session().torrent(&InfoHashes::from_v1(v1))
    }

    /// One line for the screen. It is dropped when the queue is full: a backend must not
    /// wait on a screen that has stopped reading.
    fn notify(&self, notice: Notice) {
        let _ = self.notices.try_send(notice);
    }

    /// Reports a command that the engine refused. Success needs no line: the snapshot it
    /// changes is the answer.
    fn report(&self, result: Result<(), SessionError>) {
        if let Err(error) = result {
            self.notify(Notice::Error(map::clean(&error.to_string())));
        }
    }

    fn no_torrent(&self) {
        self.notify(Notice::Error(NO_TORRENT.to_owned()));
    }
}
