//! The `downloads:` settings acted on: the unfinished folder a torrent works in until it is whole,
//! the watch folder that adds the `.torrent` files dropped into it, and the program a finished torrent
//! starts.
//!
//! These are the settings that make things happen on their own, so they happen in a task of their own,
//! beside the one that owns the engine and never inside it. Everything it does is either something the
//! engine already answers on its own (`add_torrent`, `move_storage`) or a file operation it keeps off
//! the runtime thread: reading a folder, reading a candidate's bytes, parsing them and renaming them all
//! happen on the blocking pool (the `watch` submodule in here), and the program is started and waited for
//! on a plain thread (the `program` submodule). Nothing here can hold the engine hostage, and nothing
//! here can hold a screen hostage either: notices go out on the sender the backend handed it, and a full
//! queue drops one rather than waiting.
//!
//! It wakes on three things: something the session said, the watch folder's clock ([`WATCH_EVERY`]), and
//! the handle saying stop. An event says what to do with one torrent; every wake then retries the moves
//! that are waiting and takes one look at the watch folder, which is what keeps both of them honest
//! without either needing a timer of its own.
//!
//! Two rules hold everywhere in here. A torrent file is only read once it has looked the same to two
//! scans in a row, because a copy that is still running is not a broken torrent. And nothing is thrown
//! away: a watched file is renamed `.added` or `.invalid`, a torrent's files are moved and never copied,
//! and a name somebody is using is stepped over rather than taken.

mod program;
mod watch;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use fbt::{AddTorrent, Event, EventMask, InfoHashes, MoveMode, Session, SessionError};
use tokio::sync::{mpsc, watch as signal};

use crate::client::Notice;
use crate::config::DownloadsConfig;
use crate::engine::map;

/// How often the watch folder is looked at, and with it how long a move the engine put off can take to
/// be asked about again: at most one of these periods after the engine is ready.
pub const WATCH_EVERY: Duration = Duration::from_secs(2);
/// The largest `.torrent` file a watch folder reads, in bytes. A torrent file is a few kilobytes; one
/// measured in megabytes is something else somebody renamed, and reading it into memory to find that out
/// is not worth it: the scan refuses one before reading any of its bytes.
pub const MAX_WATCHED_FILE: u64 = 16 * 1024 * 1024;
/// What goes beside the watch folder's field when the folder is not there, in the same words the
/// settings screen uses for a folder that cannot be used at all.
const NO_WATCH_FOLDER: &str = "No watch folder at that path.";

/// What the automation works on, as one value: the folder new and finished torrents go to, and the four
/// `downloads:` settings.
///
/// [`update`](AutomationHandle::update) replaces the whole thing at once rather than editing parts of
/// it, so a pass over the watch folder never acts on half of a saved change - a new unfinished folder
/// with the old default folder under it would move a finished torrent to the wrong place.
#[derive(Debug, Clone, Default)]
struct Settings {
    /// Where new torrents go and where finished ones come back to: the config's `download_dir`, already
    /// absolute (see [`crate::Config::resolve`]).
    default_dir: PathBuf,
    /// The four `downloads:` settings.
    config: DownloadsConfig,
}

impl Settings {
    /// The unfinished folder, `None` when there is nothing to move out of.
    ///
    /// A file edited by hand can name the same folder twice, which would make every move a move onto
    /// itself. Validated settings never do (see [`crate::SettingsProblem::UnfinishedIsDefault`]); this
    /// is where the difference is filtered out instead of paid for in renamed files.
    fn unfinished_dir(&self) -> Option<&Path> {
        self.config
            .unfinished_dir
            .as_deref()
            .filter(|dir| *dir != self.default_dir)
    }
}

/// Where the settings want a torrent's files, `None` when they are already where they belong.
///
/// `progress` and `save_path` are the torrent's own, read from it when the decision is made. The
/// default folder and the unfinished folder are the only two places this moves anything to, and a
/// torrent someone chose a folder for stays in the folder they chose: they put it there on purpose.
fn wanted(progress: f64, save_path: &Path, settings: &Settings) -> Option<PathBuf> {
    let unfinished = settings.unfinished_dir();
    if progress >= 1.0 {
        // Whole: it comes back to the default folder, and only if it is sitting in the unfinished one.
        (unfinished == Some(save_path)).then(|| settings.default_dir.clone())
    } else {
        // Still arriving: it works in the unfinished folder, and only if its files are where the add
        // put them, which is the default folder.
        unfinished
            .filter(|_| save_path == settings.default_dir)
            .map(Path::to_path_buf)
    }
}

/// Which folder a move was heading for, in the words the person set the field with.
fn folder_name(target: &Path, settings: &Settings) -> &'static str {
    if settings.unfinished_dir() == Some(target) {
        "unfinished folder"
    } else {
        "download folder"
    }
}

/// What the automation keeps between one wake and the next.
#[derive(Debug, Default)]
struct Shared {
    /// What it is allowed to do, which the handle replaces whole.
    settings: Settings,
    /// What the last look at the watch folder saw, which the next look compares against.
    scan: watch::Scan,
    /// Folders and files this run has already complained about out loud. A watch folder nobody can
    /// write into would otherwise say so every two seconds; the settings screen carries the complaint
    /// beside the field the whole time, so it is announced once. Kept down to what is still there.
    announced: HashSet<PathBuf>,
    /// A torrent whose files are not where the settings want them. The engine refuses a move while a
    /// torrent is checking its data, so this is tried again at the next wake until it works, or the
    /// torrent is gone, or the reason turns out not to be "busy".
    moves: HashMap<InfoHashes, PathBuf>,
    /// The latest complaint about the watch folder and about the completion program, which is what the
    /// settings screen shows beside each of those two fields.
    watch_problem: Option<String>,
    on_complete_problem: Option<String>,
}

/// The way in: [`spawn`](Self::spawn) starts the task, and everything it knows afterwards is reachable
/// through the handle it hands back.
#[derive(Debug)]
pub struct Automation;

/// What the started task runs as: the engine it acts on, the queue its lines go to, and the state the
/// handle shares with it.
#[derive(Debug)]
struct Worker {
    session: Session,
    notices: mpsc::Sender<Notice>,
    shared: Arc<Mutex<Shared>>,
}

impl Automation {
    /// Starts the automation beside `session`: it subscribes to `EventMask::STATUS` for added, removed
    /// and finished torrents, looks at `config.watch_dir` every [`WATCH_EVERY`], and runs
    /// `config.on_complete` when a torrent finishes.
    ///
    /// Its complaints go out on `notices`, the queue the backend reports on, and it never waits for that
    /// queue to be read. `default_dir` is where new torrents go and where finished ones come back to;
    /// `editable` is what a surface reading these settings is told about the program (see
    /// [`crate::OnCompleteView`]) - the automation itself runs whatever the config says either way.
    ///
    /// The task holds its own `Session` clone, so its events do not stop when the engine shuts down:
    /// [`AutomationHandle::shutdown`] is what ends it, and so is every owner dropping its handle.
    pub fn spawn(
        session: Session,
        default_dir: PathBuf,
        config: DownloadsConfig,
        editable: bool,
        notices: mpsc::Sender<Notice>,
    ) -> AutomationHandle {
        let shared = Arc::new(Mutex::new(Shared {
            settings: Settings {
                default_dir,
                config,
            },
            ..Shared::default()
        }));
        let (stop, stopped) = signal::channel(false);
        let handle = AutomationHandle {
            shared: Arc::clone(&shared),
            editable,
            stop,
        };
        // The engine's own status category: added, removed, state changes, metadata, finished. A move
        // this task asks for is answered by the `move_storage` it awaits, so nothing in the storage
        // category has a line here that is not already said - which is what keeps one move from being
        // reported twice.
        let stream = session.subscribe(EventMask::STATUS);
        let worker = Worker {
            session,
            notices,
            shared,
        };
        tokio::spawn(worker.run(stream, stopped));
        handle
    }
}

impl Worker {
    /// Acts on events, scans the watch folder and retries moves until the session goes away or the
    /// handle says stop.
    async fn run(self, mut stream: fbt::EventStream, mut stopped: signal::Receiver<bool>) {
        self.place_what_is_here();
        let mut ticker = tokio::time::interval(WATCH_EVERY);
        // A runtime that had nothing to do for a minute does not get fifty scans at once when it wakes
        // up.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // Whichever way this wake started, it ends the same way: the moves that are waiting, then a
            // look at the watch folder.
            let work = tokio::select! {
                event = stream.recv() => match event {
                    Some(event) => Work::Event(event),
                    // The session is gone, so there is nothing left to act on.
                    None => Work::Stop,
                },
                _ = ticker.tick() => Work::Pass,
                changed = stopped.changed() => match (changed, *stopped.borrow_and_update()) {
                    (Ok(_), false) => Work::Pass,
                    // The handle said stop, or every owner dropped it and nobody is left who could
                    // change these settings.
                    _ => Work::Stop,
                },
            };
            match work {
                Work::Stop => break,
                Work::Event(event) => self.act_on(&event),
                Work::Pass => {}
            }
            self.drain_moves().await;
            self.scan_watch_folder().await;
        }
        tracing::debug!("the downloads automation stopped");
    }

    /// What one thing the session said means for these settings.
    fn act_on(&self, event: &Event) {
        match event {
            Event::TorrentAdded { torrent } | Event::TorrentFinished { torrent } => {
                let Some(torrent) = self.session.torrent(torrent) else {
                    return; // gone between the event and this look
                };
                let status = torrent.status();
                let settings = self.settings();
                match wanted(status.progress, &status.save_path, &settings) {
                    Some(target) => {
                        self.lock().moves.insert(torrent.info_hashes(), target);
                    }
                    None if status.progress >= 1.0 => {
                        // Whole and already where it belongs: there is no move to run the program
                        // after, so the program runs here.
                        self.run_program(&status.name, &status.save_path);
                    }
                    None => {}
                }
            }
            Event::TorrentRemoved { torrent } => {
                self.lock().moves.remove(torrent);
            }
            // Everything else this subscription can see - a state change, metadata arriving, a queue
            // this fell behind in - is something the next wake retries anyway, or nothing to do at all.
            _ => {}
        }
    }

    /// Puts the torrents the session already holds where the settings want them: a library restored from
    /// its state file, or one whose settings changed while nothing was arriving, comes round to what
    /// `config.yaml` now says instead of waiting for an event that is never going to arrive.
    fn place_what_is_here(&self) {
        let settings = self.settings();
        let mut shared = self.lock();
        for torrent in self.session.torrents() {
            let status = torrent.status();
            if let Some(target) = wanted(status.progress, &status.save_path, &settings) {
                shared.moves.insert(torrent.info_hashes(), target);
            }
        }
    }

    /// Retries every move that is waiting, and runs the completion program for a move that put a whole
    /// torrent in the folder it finished in.
    ///
    /// The engine refuses a move while a torrent is checking its data (`Busy`), which is ordinary for a
    /// torrent that has just been added: that one stays in the map and is asked about again at the next
    /// wake. Any other refusal is said once and the files stay where they are - a folder that cannot be
    /// moved into does not get asked again every two seconds, and the program waits for the next time
    /// this torrent finishes.
    async fn drain_moves(&self) {
        let waiting: Vec<InfoHashes> = { self.lock().moves.keys().copied().collect() };
        for id in waiting {
            let Some(torrent) = self.session.torrent(&id) else {
                self.lock().moves.remove(&id);
                continue;
            };
            let settings = self.settings();
            let status = torrent.status();
            let Some(target) = wanted(status.progress, &status.save_path, &settings) else {
                // It is where it belongs, or the settings changed underneath it and want something else
                // from it, which the next thing the session says will pick up.
                self.lock().moves.remove(&id);
                continue;
            };
            match torrent
                .move_storage(target.clone(), MoveMode::FailIfExists)
                .await
            {
                Ok(()) => {
                    self.lock().moves.remove(&id);
                    if status.progress >= 1.0 {
                        // The folder this move put the files in is the folder they finished in, and
                        // that is what the program is told. Asking the engine again here could read a
                        // folder somebody else moved them into in between.
                        self.run_program(&status.name, &target);
                    }
                }
                // It is checking its data: ask again at the next wake.
                Err(SessionError::Busy) => {}
                Err(problem) => {
                    self.lock().moves.remove(&id);
                    let line = format!(
                        "Could not move {} into the {}: {}",
                        map::clean(&torrent.status().name),
                        folder_name(&target, &settings),
                        map::clean(&problem.to_string())
                    );
                    self.notify(Notice::Error(map::clean(&line)));
                }
            }
        }
    }

    /// One pass over the watch folder. Everything about it that does not need the engine - the folder
    /// listing, a candidate's bytes, the parse, the renames - happens on the blocking pool, and the
    /// engine taking the torrent is the only thing awaited.
    async fn scan_watch_folder(&self) {
        let Some(dir) = self.settings().config.watch_dir.clone() else {
            return;
        };
        let previous = { self.lock().scan.clone() };
        let swept = {
            let dir = dir.clone();
            match tokio::task::spawn_blocking(move || watch::sweep(&dir, &previous)).await {
                Ok(swept) => swept,
                // The blocking pool is going away, which means this process is going away too.
                Err(problem) => {
                    tracing::debug!(problem = %problem, "the watch folder could not be looked at");
                    return;
                }
            }
        };
        let swept = match swept {
            Ok(swept) => swept,
            // A folder the person deleted while the app was running: the same line the settings screen
            // gives for a folder that cannot be used, and the field shows it from here on.
            Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {
                self.complain(&dir, NO_WATCH_FOLDER.to_owned());
                return;
            }
            Err(problem) => {
                let line = format!("Could not read the watch folder: {problem}");
                self.complain(&dir, map::clean(&line));
                return;
            }
        };

        // The field shows what went wrong the last time this folder was read, so a folder that can be
        // read again has nothing to show: the complaint is dropped here and put back below if this pass
        // found a file it could not add.
        self.lock().watch_problem = None;

        // Files that are not going to be added, already renamed `.invalid`, each with the reason: their
        // bytes were never read because of their size, or they are not a torrent at all.
        for (file, reason) in &swept.refused {
            let path = dir.join(file);
            let line = format!(
                "Could not add {} from the watch folder: {reason}",
                map::clean(file)
            );
            self.complain(&path, map::clean(&line));
        }

        for watch::Ready { path, metainfo } in swept.ready {
            let settings = self.settings();
            let file = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            let mut params = AddTorrent::new(metainfo, settings.default_dir.clone());
            // A watch folder's torrents obey the switch like every other way in: nothing arrives until
            // somebody starts this one.
            params.paused = settings.config.start_paused;
            match self.session.add_torrent(params).await {
                Ok(torrent) => {
                    // Renamed out of the candidates' way, which is what keeps it from being added
                    // again - across a restart too, because the rename is the record.
                    self.mark_added(&path, &file).await;
                    let status = torrent.status();
                    let shown = if status.name.is_empty() {
                        file
                    } else {
                        status.name
                    };
                    self.notify(Notice::Info(format!("Added {}", map::clean(&shown))));
                }
                // Somebody else put this torrent in the library first: the same file under another name,
                // or the same torrent from the screen. It is in the library, which is all this folder
                // was for, so it gets the same rename as any other added file and no line about it.
                Err(SessionError::DuplicateTorrent) => {
                    self.mark_added(&path, &file).await;
                }
                // The session is going away underneath this one, on its way out. The file stays a
                // candidate for the next run to add, and there is nothing to complain about on the way
                // out of a process.
                Err(SessionError::Shutdown) => {}
                // A torrent the engine will not take: its reason, said with the file's name, and renamed
                // `.invalid` so it is not asked again.
                Err(problem) => {
                    let mut reason = map::clean(&problem.to_string());
                    if let Err(problem) = rename_out(&path, watch::INVALID).await {
                        reason = format!(
                            "{reason}, and it could not be renamed out of the way: {problem}"
                        );
                    }
                    let line = format!("Could not add {file} from the watch folder: {reason}");
                    self.complain(&path, map::clean(&line));
                }
            }
        }

        // What the next look compares against, and the complaints to keep quiet about, down to the files
        // still in the folder: one that was renamed away or deleted cannot complain again, and there is
        // no reason to remember a complaint about a file that is gone.
        let still_there: HashSet<PathBuf> = swept
            .scan
            .keys()
            .cloned()
            .chain(swept.refused.iter().map(|(file, _)| dir.join(file)))
            .collect();
        let mut shared = self.lock();
        shared.scan = swept.scan;
        shared.announced.retain(|path| still_there.contains(path));
    }

    /// Renames a settled candidate `.added` now that the engine has it, and says so if the rename is
    /// what failed: the torrent is in the library, but the file is still where it was and will be
    /// offered up again - which the engine answers with "already here", so nothing is added twice
    /// either way.
    async fn mark_added(&self, path: &Path, file: &str) {
        if let Err(problem) = rename_out(path, watch::ADDED).await {
            let line = format!(
                "Could not finish adding {file} from the watch folder: it is in the library, but it \
                 could not be renamed out of the way: {problem}"
            );
            self.complain(path, map::clean(&line));
        }
    }

    /// The complaint about a folder, or about a file in it: always the latest one the settings screen
    /// shows, and out loud only the first time this run hears it.
    fn complain(&self, path: &Path, line: String) {
        let first = {
            let mut shared = self.lock();
            let first = shared.announced.insert(path.to_path_buf());
            shared.watch_problem = Some(line.clone());
            first
        };
        if first {
            self.notify(Notice::Error(line));
        }
    }

    /// Starts the program this config names for a finished torrent, and says so only if it would not
    /// start. What it did after that - how long it ran, what it exited with - is in the log and is
    /// nobody's business here.
    fn run_program(&self, name: &str, folder: &Path) {
        let Some(program) = self.settings().config.on_complete.clone() else {
            return;
        };
        if let Err(problem) = program::run(&program, name, folder) {
            let line = format!(
                "Could not start the program run on completion: {}",
                map::clean(&problem.to_string())
            );
            let changed = {
                let mut shared = self.lock();
                let changed = shared.on_complete_problem.as_deref() != Some(line.as_str());
                shared.on_complete_problem = Some(line.clone());
                changed
            };
            // One line per torrent that finished and could not run its program: two torrents, two
            // lines. The field beside the program shows the last of them.
            if changed {
                self.notify(Notice::Error(line));
            }
        }
    }

    /// The settings as they are right now: cloned out of the lock, so a pass works on one value and
    /// nothing is held across an await.
    fn settings(&self) -> Settings {
        self.lock().settings.clone()
    }

    /// One line for the screen, dropped when the queue is full: this task must not wait on a surface
    /// that has stopped reading.
    fn notify(&self, notice: Notice) {
        let _ = self.notices.try_send(notice);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        // Every write here is one field at a time, so a panic inside the lock leaves nothing half
        // applied for the next pass to trip over.
        self.shared
            .lock()
            .expect("the downloads state is not poisoned")
    }
}

/// What a wake was for.
enum Work {
    /// The session said something about a torrent.
    Event(Event),
    /// The standing work: the moves that are waiting, and the watch folder.
    Pass,
    /// End this task.
    Stop,
}

/// Renames a watched file out of the candidates' way with `suffix`, on the blocking pool, where a rename
/// belongs in a task that must not hold up a runtime thread.
async fn rename_out(path: &Path, suffix: &str) -> Result<(), String> {
    let path = path.to_path_buf();
    let suffix = suffix.to_owned();
    tokio::task::spawn_blocking(move || watch::mark(&path, &suffix))
        .await
        .unwrap_or_else(|problem| Err(format!("the rename could not run: {problem}")))
}

/// The handle on a running [`Automation`]: what changes what it does, and what reads back what went
/// wrong.
///
/// Dropping it does not stop the automation: the task runs beside the session it was started on until
/// [`shutdown`](Self::shutdown), or until every owner has dropped its handle.
#[derive(Clone, Debug)]
pub struct AutomationHandle {
    shared: Arc<Mutex<Shared>>,
    editable: bool,
    stop: signal::Sender<bool>,
}

impl AutomationHandle {
    /// Applies new settings. The next thing the session says, or the next look at the watch folder,
    /// whichever comes first, works on them - at most [`WATCH_EVERY`] away.
    ///
    /// The two complaints belong to the settings they were filed under, so choosing a different watch
    /// folder clears the watch folder's (the new one has had no chance to go wrong yet) and choosing a
    /// different program clears the program's. A folder or a program left as it was keeps its complaint:
    /// a watch folder that is still missing is still missing.
    pub fn update(&self, default_dir: PathBuf, config: DownloadsConfig) {
        let mut shared = self.lock();
        let moved_watch = shared.settings.config.watch_dir != config.watch_dir
            || shared.settings.default_dir != default_dir;
        let changed_program = shared.settings.config.on_complete != config.on_complete;
        shared.settings = Settings {
            default_dir,
            config,
        };
        if moved_watch {
            // A different folder is a different set of files: the scan it is compared against starts
            // again, and so do the complaints this run has already said something about.
            shared.scan = watch::Scan::new();
            shared.announced.clear();
            shared.watch_problem = None;
        }
        if changed_program {
            shared.on_complete_problem = None;
        }
    }

    /// The latest complaint about the watch folder, and the latest about the completion program: the two
    /// lines to show beside those fields, `None` for nothing to say.
    pub fn problems(&self) -> (Option<String>, Option<String>) {
        let shared = self.lock();
        (
            shared.watch_problem.clone(),
            shared.on_complete_problem.clone(),
        )
    }

    /// Whether the surface reading these settings may change the program at all: see
    /// [`crate::OnCompleteView`]. A server answers `false` because it reads the program from its own
    /// `config.yaml`, and a browser has no business naming a program to run on a machine it is only
    /// visiting; a library this app runs itself answers `true`.
    pub fn editable(&self) -> bool {
        self.editable
    }

    /// Stops the task, after whatever pass it is in.
    ///
    /// A completion program that is already running keeps running: it was started and left alone, and
    /// this app letting go of it is not a reason to stop a film somebody is watching.
    pub fn shutdown(&self) {
        // This cannot fail in a way that matters: the sender holds one value, so it never fills up, and
        // an error means the task already ended.
        let _ = self.stop.send(true);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared
            .lock()
            .expect("the downloads state is not poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::{Settings, wanted};
    use crate::config::DownloadsConfig;
    use std::path::{Path, PathBuf};

    fn settings(unfinished: Option<&Path>) -> Settings {
        Settings {
            default_dir: PathBuf::from("/downloads"),
            config: DownloadsConfig {
                unfinished_dir: unfinished.map(Path::to_path_buf),
                ..DownloadsConfig::default()
            },
        }
    }

    /// The whole rule of the unfinished folder in one place: where a torrent's files should go, given
    /// how far along it is and where they are now. `None` means leave them where they are.
    #[test]
    fn files_go_to_the_unfinished_folder_and_come_back_when_whole() {
        let incomplete = Path::new("/downloads/incomplete");
        let chosen = Path::new("/other/series");
        let with = settings(Some(incomplete));
        let without = settings(None);

        assert_eq!(
            wanted(0.4, Path::new("/downloads"), &with),
            Some(incomplete.to_path_buf()),
            "a torrent arriving in the default folder works in the unfinished one"
        );
        assert_eq!(
            wanted(1.0, incomplete, &with),
            Some(PathBuf::from("/downloads")),
            "and comes back to the default folder the moment it is whole"
        );
        assert_eq!(
            wanted(1.0, Path::new("/downloads"), &with),
            None,
            "whole in the default folder is already where it belongs"
        );
        assert_eq!(
            wanted(0.4, chosen, &with),
            None,
            "a folder the person chose for this torrent is theirs: nothing moves it"
        );
        assert_eq!(
            wanted(1.0, chosen, &with),
            None,
            "and it finished where it was put"
        );
        assert_eq!(
            wanted(0.4, Path::new("/downloads"), &without),
            None,
            "no unfinished folder, no move"
        );
        assert_eq!(
            wanted(1.0, incomplete, &without),
            None,
            "and nothing to move a torrent that is sitting in a folder nobody set"
        );

        // A file edited by hand can name the same folder twice. That is one folder named twice, and a
        // move onto it would be a move for nothing.
        let twice = settings(Some(Path::new("/downloads")));
        assert_eq!(wanted(0.4, Path::new("/downloads"), &twice), None);
        assert_eq!(wanted(1.0, Path::new("/downloads"), &twice), None);
    }
}
