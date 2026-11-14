//! The backend over the embedded engine: one task owns the session.
//!
//! A `fbt::Session` is cheap to clone but is not something two places should drive at
//! once, so exactly one task holds it. That task takes commands off a queue, answers
//! them, and publishes a snapshot of what the session looks like afterwards. Nothing
//! else touches the engine, so there is no lock between the screen and the swarm, and
//! a command that waits on a peer cannot hold a keystroke hostage: the screen is not
//! in this task at all.
//!
//! The task owns the library lock too, and drops it only after the state file is
//! written. A second featherbittorrent therefore starts on either the old state or the
//! new one, never between them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fbt::{
    AddTorrent, InfoHashV1, InfoHashes, Magnet, Metainfo, Priority, RateLimit, RemoveMode, Session,
    SessionError, SessionState, Settings, TorrentHandle,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::client::{
    AddSource, Client, Command, Connection, Detail, DetailTab, FilePriority, LiveLimits, Notice,
    Snapshot, TorrentId, TorrentRow, Totals,
};
use crate::config::{Config, ConfigLoad, Limits, replace_file};
use crate::lock::LibraryLock;
use crate::paths::Paths;

mod map;
mod state;

/// How often the snapshot is rebuilt, and with it the watched pane. Short enough that
/// a rate looks alive, long enough that nobody pays for it.
const TICK: Duration = Duration::from_millis(500);
/// How often the session state is written while the app runs, so a crash costs no more
/// than re-checking what was written since the last save.
const SAVE_EVERY: Duration = Duration::from_secs(300);
/// What the screen is told when an id names nothing this session holds. Ids come from
/// the screen (and, from milestone 2, from a server), so a stale or malformed one is an
/// ordinary answer rather than a failure.
const NO_TORRENT: &str = "the torrent is not in this library";

/// The backend that runs the torrents in this process.
///
/// It has no state of its own: each [`start`](Self::start) spawns the task that owns
/// one session, this library's files and its lock, and hands back the client that talks
/// to it.
#[derive(Debug)]
pub struct LocalBackend;

impl LocalBackend {
    /// Starts the engine on `paths`, restoring whatever `session.state` holds, on the
    /// settings [`Config::session_settings`] describes.
    ///
    /// `lock` is this library's, taken with [`LibraryLock::acquire`]: the backend holds
    /// it until its state is written. The returned handle resolves once `Shutdown` has
    /// been answered and that file is on disk, which is what a quit (or a test) waits on
    /// before starting again on the same directories. Its `Err` is one line about a final
    /// save that did not happen: the library on disk is then the one from before this run,
    /// and only the person running it can act on that, so the binary prints it after
    /// putting the terminal back.
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

/// Starts the session and the task that owns it, `paths` and `lock`.
///
/// A state file that cannot be read is left where it is and this run stops writing it; one
/// that does not decode is kept aside under another name. Resume data that will not come back
/// is copied out from under it before any save happens. All of that arrives as notices. The
/// engine clone comes back with the client so a caller can drive the same session.
async fn spawn(
    settings: Settings,
    config: Config,
    paths: &Paths,
    lock: LibraryLock,
) -> Result<(Client, JoinHandle<Result<(), String>>, Session), SessionError> {
    let reading = state::read_state(&paths.state_file());
    let (session, failed) = match &reading.state {
        Some(loaded) => Session::restore(settings, loaded).await?,
        None => (Session::new(settings).await?, Vec::new()),
    };
    let engine = session.clone();

    // One line each for what went wrong with the saved state. The screen may not be
    // reading yet, so these wait in the queue, and past its room they are dropped:
    // that is what a notice always means. A restore reports one per torrent it could
    // not add, which is how many there can be.
    let mut problems = reading.notices;
    if let Some(loaded) = &reading.state {
        // Torrents that did not come back deserve kept bytes: the save this run makes describes
        // only what it has, so without a copy those torrents would leave the library twice over
        // - once unseen by the session and once overwritten in the file that described them.
        let kept = if failed.is_empty() {
            None
        } else {
            match state::keep_copy(&paths.state_file(), &reading.bytes) {
                Ok(copy) => Some(copy),
                Err(problem) => {
                    problems.push(Notice::Error(format!(
                        "the saved state could not be copied aside ({problem}); what could not \
                         be restored will not come back"
                    )));
                    None
                }
            }
        };
        problems.extend(failed.into_iter().map(|(index, error)| {
            Notice::Error(state::restored_problem(
                loaded,
                index,
                &error,
                kept.as_deref(),
            ))
        }));
    }

    // The first snapshot is taken here rather than in the task, so a client never sees
    // an empty library that is about to fill up with restored torrents.
    let mut stalled = HashMap::new();
    let (torrents, totals) = rows(&session, Instant::now(), &mut stalled);
    let initial = Arc::new(Snapshot {
        connection: Connection::Local,
        torrents,
        totals,
        limits: map::limits(&session),
        detail: None,
        default_dir: config.download_dir.clone(),
    });
    let (client, channels) = Client::paired(Arc::clone(&initial));
    for notice in problems {
        let _ = channels.notices.try_send(notice);
    }
    #[cfg(feature = "test-support")]
    let client = client.with_session(&engine);

    let backend = Backend {
        session,
        config,
        paths: paths.clone(),
        watching: None,
        stalled,
        writes_blocked: reading.writes_blocked,
        published: initial,
        snapshot_tx: channels.snapshot,
        notices: channels.notices,
    };
    let task = tokio::spawn(backend.run(channels.commands, lock));
    Ok((client, task, engine))
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

/// The task's side of the library: the session it drives, the files it writes, and what
/// it last published.
struct Backend {
    /// The only session in this process, and only this task touches it.
    session: Session,
    /// What the run started with, kept current so a missing config file has something
    /// sensible to fall back to.
    config: Config,
    /// Where `config.yaml` and `session.state` are.
    paths: Paths,
    /// The pane to keep fresh, and for which torrent.
    watching: Option<(TorrentId, DetailTab)>,
    /// When each torrent's stalled count last started running: while it is downloading and
    /// taking nothing. See [`map::stalled_clock`].
    stalled: HashMap<TorrentId, Instant>,
    /// Set when this run could not read what was under `session.state`, or could not move an
    /// unreadable file out of the way: no state write happens at all while it is set, because
    /// saving would replace bytes this run never read with a library of its own.
    writes_blocked: bool,
    /// The snapshot the screen is looking at, so that only a change publishes.
    published: Arc<Snapshot>,
    /// Where snapshots go out.
    snapshot_tx: watch::Sender<Arc<Snapshot>>,
    /// Where one-line feedback goes out.
    notices: mpsc::Sender<Notice>,
}

impl Backend {
    /// Answers commands and keeps the snapshot fresh until asked to stop, then saves and
    /// releases the library. The lock goes with the last line here, after the state file is
    /// written.
    ///
    /// The `Err` is one line about a final save that did not happen. Nothing in this process
    /// can fix that - the library on disk is the one from before this run - so it travels out
    /// of the task for the binary to print after restoring the terminal.
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        _lock: LibraryLock,
    ) -> Result<(), String> {
        // Both intervals are ready at once on their first tick. The snapshot's goes with
        // it; the save's would write a file that has not changed since it was read, so
        // that one is spent before the loop starts.
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut save = tokio::time::interval(SAVE_EVERY);
        save.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        save.tick().await;

        let outcome = loop {
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
                Wake::Command(Command::Shutdown) => break self.save_and_stop().await,
                Wake::Command(command) => self.handle(command).await,
                Wake::Tick => {}
                Wake::Save => self.periodic_save().await,
            }
            self.publish().await;
        };
        // `_lock` is released on the way out of here, after the state file was written or
        // failed to be: another featherbittorrent may start on the new state, never beside a
        // half-written one.
        outcome
    }

    /// Rebuilds the snapshot and publishes it only when it differs from what is out
    /// there, so a screen woken by `changed()` always has something new to draw.
    async fn publish(&mut self) {
        let (torrents, totals) = rows(&self.session, Instant::now(), &mut self.stalled);
        let detail = match self.watching.clone() {
            Some((id, tab)) => self.detail_for(&id, tab).await.map(|detail| (id, detail)),
            None => None,
        };
        let next = Arc::new(Snapshot {
            connection: Connection::Local,
            torrents,
            totals,
            limits: map::limits(&self.session),
            detail,
            default_dir: self.config.download_dir.clone(),
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
        match self.session.add_torrent(params).await {
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
        self.session.set_rate_limit(RateLimit::new(
            map::rate_bytes(limits.download_kib),
            map::rate_bytes(limits.upload_kib),
        ));
        let mut queue = self.session.queue_settings();
        queue.active_downloads = limits.active_downloads;
        queue.active_seeds = limits.active_seeds;
        queue.active_limit = limits.active_total;
        // The counts are the engine's own to refuse, so it answers rather than us guessing.
        self.report(self.session.set_queue_settings(queue));
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
            let mut config = match Config::load(&file) {
                Ok(ConfigLoad::Loaded(config)) => config,
                // A file we could not parse is already kept aside as `.bad`; the defaults
                // it came with are what to write on.
                Ok(ConfigLoad::Bad { config, .. }) => config,
                // No file yet, or one that could not be opened at all: keep what this run
                // started with, which is what the file would have said.
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

    /// Writes the state while the app runs, so a crash costs a re-check and nothing else.
    /// A run that could not read what was under the state file writes nothing at all: its
    /// empty library is not evidence about what the file held.
    async fn periodic_save(&self) {
        if self.writes_blocked {
            return;
        }
        match self.session.save_state().await {
            Ok((state, left_out)) => {
                self.report_left_out(&left_out);
                if let Err(problem) = self.write_state(&state).await {
                    self.notify(Notice::Error(problem));
                }
            }
            Err(error) => self.notify(Notice::Error(map::clean(&error.to_string()))),
        }
    }

    /// The last save, and the task's answer. `shutdown_and_save` stops every torrent with its
    /// bytes written first, so what is saved here is trusted next run and nothing has to be
    /// re-checked. A failure here cannot be fixed from in here - the library on disk stays the
    /// one from before this run - so it goes out as the message the binary prints.
    async fn save_and_stop(&self) -> Result<(), String> {
        if self.writes_blocked {
            // Nothing to write, so nothing to fail and no reason to stop the torrents for it:
            // whatever is under `session.state` stays exactly as this run found it.
            return Ok(());
        }
        let (state, left_out) = self.session.shutdown_and_save().await;
        self.report_left_out(&left_out);
        self.write_state(&state).await
    }

    /// A torrent whose resume data could not be taken is out of the saved state: it will
    /// not be here next run, which is worth saying.
    fn report_left_out(&self, left_out: &[(InfoHashes, SessionError)]) {
        for (hashes, error) in left_out {
            self.notify(Notice::Error(map::left_out(hashes, error)));
        }
    }

    /// Puts the state where the next run will find it. The bytes are written beside the
    /// file, synced and renamed over it on a blocking thread, so a crash leaves either
    /// the old state or the new one and never half of either. The `Err` is one line already
    /// fit to print: whoever asked for this save either reports it as a notice or carries it
    /// out of the task for the binary.
    async fn write_state(&self, state: &SessionState) -> Result<(), String> {
        let path = self.paths.state_file();
        let bytes = state.to_bytes();
        let outcome = tokio::task::spawn_blocking(move || {
            replace_file(&path, &bytes)
                .map_err(|source| format!("cannot write {}: {source}", path.display()))
        })
        .await;
        match outcome {
            Ok(outcome) => outcome.map_err(|problem| map::clean(&problem)),
            Err(joined) => Err(format!("the session state could not be written: {joined}")),
        }
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
        self.session.torrent(&InfoHashes::from_v1(v1))
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
