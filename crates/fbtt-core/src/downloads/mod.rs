//! The `downloads:` settings acted on: the unfinished folder a torrent works in until it is whole,
//! the watch folder that adds the `.torrent` files dropped into it, and the program a finished torrent
//! starts.
//!
//! These are the settings that make things happen on their own, so they happen in a task of their own,
//! beside the one that owns the engine and never inside it. Everything it does is either something the
//! engine already answers on its own (`add_torrent`, `move_storage`) or a file operation it keeps off the
//! runtime thread: reading a folder, reading a candidate's bytes, parsing them and renaming them all happen
//! on the blocking pool (the `watch` submodule in here), the program is started and waited for on a plain
//! thread (the `program` submodule), and a move is a task of its own whose answer comes back on a channel
//! rather than being awaited - a folder on another filesystem can copy for minutes, and the watch folder,
//! the session's events and every other torrent must not sit behind it. Nothing here can hold the engine
//! hostage, and nothing here can hold a screen hostage either: notices go out on the sender the backend
//! handed it, and a full queue drops one rather than waiting.
//!
//! It wakes on three things: something the session said, the watch folder's clock ([`WATCH_EVERY`]), and the
//! handle saying stop. An event says what to do with one torrent; every wake then looks at what the session
//! holds *now*, picks up the move answers that have come in and starts the ones that are waiting, and takes
//! one look at the watch folder. That middle look is what makes a lost event survivable: the session drops
//! events for a subscriber that cannot keep up, and a finish that was dropped looks exactly like a torrent
//! that has not finished, so nothing is trusted to have been told. Whole files sitting in the unfinished
//! folder are found by looking, and so is a torrent this run saw arrive that is now whole and where it
//! belongs.
//!
//! Four rules hold everywhere in here. A torrent file is only read once it has looked the same to two scans
//! in a row, because a copy that is still running is not a broken torrent - and what is read is checked
//! again at the open, because two looks at a *name* say nothing about what answers to it by the time the
//! bytes are taken. A torrent's program is started once per finish, claimed before it is started, so a
//! finish that arrives twice does not play the film twice. And nothing is thrown away: a watched file is
//! renamed `.added` or `.invalid`, a torrent's files are moved and never copied, and a name somebody is
//! using is stepped over rather than taken.

mod program;
#[cfg(feature = "test-support")]
pub mod test_support;
mod watch;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

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

/// Whether the engine says this torrent is taking pieces right now.
///
/// The claim "this run watched this torrent arrive" is a claim about downloading, not about one sampled
/// progress number: a torrent whose files were already whole when it was added reports a progress below
/// one while the engine checks them, then seeds, and never downloads at all. Only the downloading
/// states mean a finish is coming that this run has a right to expect - and to go looking for, the way
/// [`Worker::place_what_is_here`] does, if the event queue ever drops it.
fn is_downloading(status: &fbt::TorrentStatus) -> bool {
    matches!(
        status.state,
        fbt::TorrentState::Downloading | fbt::TorrentState::DownloadingMetadata
    )
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
    /// Watched files that were read, offered to the engine and complained about, and could not be renamed
    /// out of the way: while one still looks the way it did then, the folder leaves it alone rather than
    /// reading and parsing it again every [`WATCH_EVERY`] for the same reason.
    unmoved: watch::UnmovedMap,
    /// Torrents whose files are not where the settings want them, and what to do about each: the folder to
    /// move them to, whether the completion program is owed afterwards, whether a move is running now, and
    /// whether the last refusal was one that is not going to fix itself.
    moves: HashMap<InfoHashes, Move>,
    /// Torrents this run has seen downloading: the ones whose finishes this run can expect to hear about,
    /// which is what makes a missing one worth looking for. A torrent that was whole before the engine
    /// ever started on it is not one of these, however its progress number read while being checked - it
    /// has no finish this run missed (see [`is_downloading`]).
    seen_unfinished: HashSet<InfoHashes>,
    /// Torrents whose finish this run has already handled - the program started, or refused, or lost to a
    /// program that would not start, or nothing configured to run at all - which is what keeps one finish
    /// to one run. "Nothing configured" counts as handled on purpose: a person who sets a program after a
    /// dozen torrents finished does not get twelve runs for finishes this run already saw and answered.
    /// A torrent leaves the set when it leaves the session (see [`Worker::place_what_is_here`]), so the
    /// same torrent re-added later is new to this run and earns its program.
    finish_handled: HashSet<InfoHashes>,
    /// The latest complaint about the watch folder and about the completion program, which is what the
    /// settings screen shows beside each of those two fields.
    watch_problem: Option<String>,
    on_complete_problem: Option<String>,
}

/// One torrent's move, and what it is there for.
#[derive(Debug)]
struct Move {
    /// Where the files are being taken.
    target: PathBuf,
    /// Whether the completion program is owed once the files arrive: true for a torrent that was whole when
    /// the move was queued, which is the only shape in which a move hands finished files to the folder they
    /// finished in.
    program_after: bool,
    /// Whether a `move_storage` for this torrent is running now, so the next wake does not start a second.
    in_flight: bool,
    /// Whether this move has been refused for a reason that is not going to change on its own - a file of
    /// somebody else's at the target, a folder nobody can write into - which is said once and then left
    /// alone until the settings change, the torrent goes, or the torrent goes somewhere else.
    given_up: bool,
}

/// What one running move answered, on the channel rather than to the wake that started it.
#[derive(Debug)]
struct Moved {
    /// The torrent.
    id: InfoHashes,
    /// Where its files were being taken. The answer is read against the move the entry wants now, and
    /// this is the only way to tell this move's answer from the answer of the one that replaced it; the
    /// folder is also what the completion program is told when the files arrive.
    target: PathBuf,
    /// The torrent's name as it was when the move started, for the line about a move that failed and for
    /// the program's first argument.
    name: String,
    /// What the engine said: `Ok` for the files having arrived, `Busy` for asking while it is still
    /// checking that torrent's data, and anything else for a move that is not going to work.
    outcome: Result<(), SessionError>,
}

/// What an answer to a running move says about the entry table, once [`answer_for`] has applied it:
/// `Landed` means the entry is gone, `Busy` and `Refused` keep it, and `Stale` means the entry was about
/// something else entirely.
#[derive(Debug)]
enum Answered {
    /// The answer's folder is not the folder the entry wants, or the torrent has no entry: the answer is
    /// about a superseded move, and nothing outside the entry's in-flight claim changed.
    Stale,
    /// The files arrived where the entry wanted them; the entry is removed. `true` is a completion
    /// program owed to the torrent that just got its files home.
    Landed { program_after: bool },
    /// The engine was not ready for this torrent yet; the entry waits, claim cleared, for the next ask.
    Busy,
    /// The move is not going to work; the entry stays and gives up. `problem` is the engine's reason for
    /// the line, and `true` is a completion program owed to a torrent that will keep its files where
    /// they are.
    Refused {
        problem: SessionError,
        program_after: bool,
    },
}

/// The way in: [`spawn`](Self::spawn) starts the task, and everything it knows afterwards is reachable
/// through the handle it hands back.
#[derive(Debug)]
pub struct Automation;

/// What the started task runs as: the engine it acts on, the queue its lines go to, the state the handle
/// shares with it, and the other end of the channel its running moves answer on.
#[derive(Debug)]
struct Worker {
    session: Session,
    notices: mpsc::Sender<Notice>,
    shared: Arc<Mutex<Shared>>,
    /// Where running moves report, and where the next wake reads them. Unbounded on purpose: one result
    /// per move attempt, and an attempt is only started by a wake that has room for its answer.
    moves_tx: mpsc::UnboundedSender<Moved>,
    moves_rx: mpsc::UnboundedReceiver<Moved>,
    /// The flags of this automation's test park, shared with its handle: per automation, so a test parks
    /// the loop of the handle it holds and not the loop of whichever test happened to ask last.
    #[cfg(feature = "test-support")]
    park: Arc<test_support::ParkFlags>,
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
        #[cfg(feature = "test-support")]
        let park = Arc::new(test_support::ParkFlags::default());
        #[cfg(feature = "test-support")]
        let handle = AutomationHandle {
            shared: Arc::clone(&shared),
            editable,
            stop,
            park: Arc::clone(&park),
        };
        #[cfg(not(feature = "test-support"))]
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
        // One result per move attempt, so a queue this long is a queue this task wrote itself.
        let (moves_tx, moves_rx) = mpsc::unbounded_channel();
        let worker = Worker {
            session,
            notices,
            shared,
            moves_tx,
            moves_rx,
            #[cfg(feature = "test-support")]
            park,
        };
        tokio::spawn(worker.run(stream, stopped));
        handle
    }
}

impl Worker {
    /// Acts on events, places what the session holds, moves what needs moving, and looks at the watch
    /// folder, until the session goes away or the handle says stop.
    async fn run(mut self, mut stream: fbt::EventStream, mut stopped: signal::Receiver<bool>) {
        let mut ticker = tokio::time::interval(WATCH_EVERY);
        // A runtime that had nothing to do for a minute does not get fifty scans at once when it wakes
        // up.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // Whichever way this wake started, it goes on the same way: what the session holds now, the
            // moves that are waiting on it, then a look at the watch folder.
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
                Work::Event(event) => {
                    #[cfg(feature = "test-support")]
                    // A test's stand-in for a queue this task fell behind in, which is what a session
                    // busy enough to overflow one actually does to a finish - or to a removal.
                    let event =
                        test_support::remove_instead_of(test_support::lag_instead_of(event));
                    self.act_on(&event);
                }
                Work::Pass => {}
            }
            // Then, however the wake started, a look at what the session holds *now*. This is what makes a
            // lost event survivable: the session drops events for a subscriber that cannot keep up, and a
            // drop that swallows a `TorrentFinished` is indistinguishable, from here, from a torrent that
            // never finished - so nothing is trusted to have arrived. The torrents that are whole and where
            // they belong are looked at directly, and the ones that finished while nothing was listening are
            // found that way rather than waited for.
            self.place_what_is_here();
            self.ask_moves();
            self.scan_watch_folder().await;
        }
        tracing::debug!("the downloads automation stopped");
    }

    /// What one thing the session said means for these settings.
    ///
    /// `TorrentAdded` and `TorrentFinished` are kept apart because they mean different things about the
    /// completion program. A finish is a torrent that became whole here, and it earns its program. An add is
    /// a torrent this run met already whole, or not: arriving is not finishing, so an add only ever says
    /// where the files should go. A torrent added whole *into the unfinished folder* still gets its program,
    /// but from the hand-back the settings require of it - the move runs it when the files land (see
    /// [`place_what_is_here`]), not the add.
    fn act_on(&self, event: &Event) {
        match event {
            Event::TorrentAdded { torrent } => {
                let Some(torrent) = self.session.torrent(torrent) else {
                    return; // gone between the event and this look
                };
                let status = torrent.status();
                let id = torrent.info_hashes();
                if is_downloading(&status) {
                    // Something this run watched arriving in earnest - pieces on their way - so its
                    // finish is a thing this run can expect, and a thing whose absence means something
                    // if the queue drops it. A torrent whose files were already whole checks and seeds
                    // without ever downloading: its progress number reads below one while the engine
                    // checks, but it never arrived unfinished here, and it does not join that list.
                    self.lock().seen_unfinished.insert(id);
                }
                self.queue_move(
                    &id,
                    status.progress,
                    &status.save_path,
                    status.progress >= 1.0,
                );
            }
            Event::TorrentFinished { torrent } => {
                let Some(torrent) = self.session.torrent(torrent) else {
                    return; // gone between the event and this look
                };
                let status = torrent.status();
                let id = torrent.info_hashes();
                self.lock().seen_unfinished.insert(id);
                let settings = self.settings();
                match wanted(status.progress, &status.save_path, &settings) {
                    Some(target) => self.queue_move_to(&id, target, true),
                    None => {
                        // Whole and already where it belongs, so there is no move to run the program after:
                        // the program runs for the folder the files are in.
                        self.run_program(&id, &status.name, &status.save_path);
                    }
                }
            }
            Event::TorrentRemoved { torrent } => {
                let mut shared = self.lock();
                shared.moves.remove(torrent);
                shared.seen_unfinished.remove(torrent);
                shared.finish_handled.remove(torrent);
            }
            // The queue this task subscribes to dropped events, a `TorrentFinished` among them: nothing to
            // catch up on, because every wake re-reads what the session holds, and that is where a torrent
            // that finished while nobody was listening is found again. Worth a log line; the torrents
            // themselves say what they have to say.
            Event::Lagged { missed } => {
                tracing::debug!(
                    missed,
                    "the downloads automation fell behind in the session's events and is re-reading \
                     what it holds",
                );
            }
            // Everything else this subscription can see - a state change, metadata arriving, a seed limit
            // reached - is something every wake looks at anyway, or nothing to do at all.
            _ => {}
        }
    }

    /// Puts the torrents the session already holds where the settings want them, and finds the finishes no
    /// event carried.
    ///
    /// This happens on every wake, not only at the start, for two reasons. A library restored from its state
    /// file, and a torrent whose settings changed while nothing was arriving, have no event coming for them:
    /// without this look a finished torrent would sit in the unfinished folder for the rest of the run. And
    /// the session drops events for a subscriber that cannot keep up, so a finish this task never got cannot
    /// be allowed to lose a torrent its hand-back or its program: whole files sitting in the unfinished
    /// folder are found here, and so is a torrent this run saw arrive that is now whole and where it belongs.
    ///
    /// Whole and where it belongs does not owe a program on its own. A torrent that arrived already whole
    /// never finished here, and this is the one place that could mistake it for having.
    fn place_what_is_here(&self) {
        let settings = self.settings();
        let mut present: HashSet<InfoHashes> = HashSet::new();
        for torrent in self.session.torrents() {
            let status = torrent.status();
            let id = torrent.info_hashes();
            present.insert(id);
            let whole = status.progress >= 1.0;
            if !whole && is_downloading(&status) {
                // A torrent this run catches with pieces on their way is one whose finish it has come
                // to expect - whether or not the add itself happened to be sampled mid-check - so the
                // recovery below knows to go looking for that finish if its event is ever dropped.
                // One gap this look does not close, and does not try to: a torrent whose entire stay in
                // the downloading states fell between two wakes - checking, then downloading, then
                // whole, all in one gap - and whose `TorrentFinished` was dropped along with it, was
                // never seen downloading by any wake here and so is not enrolled to be grieved for.
                self.lock().seen_unfinished.insert(id);
            }
            match wanted(status.progress, &status.save_path, &settings) {
                // Whole files in the unfinished folder are owed their move back, and the move runs the
                // program when they land: this is the startup rule for a torrent that finished while the
                // server was stopped, and the recovery for one whose finish went missing mid-run.
                Some(target) => self.queue_move_to(&id, target, whole),
                None if whole => {
                    // The only shape that owes this torrent a program is a torrent this run saw arriving
                    // whose program has not run: a finish whose event never made it.
                    let saw_it_arriving = self.lock().seen_unfinished.contains(&id);
                    if saw_it_arriving {
                        self.run_program(&id, &status.name, &status.save_path);
                    }
                }
                None => {}
            }
        }
        // A removal this task never got - the queue dropped it, which is the same event loss every
        // other rule here plans around - has to lose its claims here too, because the session says the
        // torrent is gone: a `seen_unfinished` or a `finish_handled` outliving the torrent it describes
        // would tell the *same torrent re-added later* that its program had run already, which is a
        // finish the person never got. Whatever the session still holds keeps its claims; everything
        // else was removed while nobody was listening.
        let mut shared = self.lock();
        shared.seen_unfinished.retain(|id| present.contains(id));
        shared.finish_handled.retain(|id| present.contains(id));
    }

    /// Queues what the settings want done with one torrent's files, and starts it if nothing is doing it
    /// already. `program_after` is whether the completion program is owed once the files land, which is
    /// only ever true for a torrent that is whole now: the only shape in which a move hands finished files
    /// to the folder they finished in.
    fn queue_move(&self, id: &InfoHashes, progress: f64, save_path: &Path, program_after: bool) {
        let settings = self.settings();
        if let Some(target) = wanted(progress, save_path, &settings) {
            self.queue_move_to(id, target, program_after);
        }
    }

    /// Records what the settings now want from one torrent's files, and says whether this look should
    /// try to start the move (`start_move` asks the entry itself whether it is still waiting).
    ///
    /// A move already queued for the same folder is left as it is, a refused one included: its reason has
    /// not changed and asking again every two seconds is what `given_up` exists to stop. A queued move
    /// heading for a *different* folder is replaced - the settings changed underneath this torrent and
    /// the new folder is what they want now - but its in-flight claim carries over: the move that is
    /// running now is one `move_storage` the engine is copying under its old goal, and a second asked for
    /// mid-copy is the engine refusing its own work. The running move's answer clears the claim (see
    /// [`answer_for`]), and the entry's own move starts then, one wake at most away.
    fn record_move(
        shared: &mut Shared,
        id: &InfoHashes,
        target: PathBuf,
        program_after: bool,
    ) -> bool {
        match shared.moves.get_mut(id) {
            Some(entry) if entry.target == target => {
                // A torrent that became whole while its move was running is owed its program when the
                // move lands, so this is where a move queued for an arriving torrent is upgraded.
                entry.program_after |= program_after;
            }
            Some(entry) => {
                entry.target = target;
                entry.program_after = program_after;
                // A new folder is a new attempt: the old one's dead end says nothing about this one.
                entry.given_up = false;
            }
            None => {
                shared.moves.insert(
                    *id,
                    Move {
                        target,
                        program_after,
                        in_flight: false,
                        given_up: false,
                    },
                );
            }
        }
        let entry = shared.moves.get(id).expect("recorded just above");
        !entry.in_flight && !entry.given_up
    }

    /// Records one torrent's move and starts it, unless something is already doing it.
    ///
    /// [`record_move`] decides from the entry table; this turns a "yes, start" into the engine call,
    /// outside the lock, the way every start here happens.
    fn queue_move_to(&self, id: &InfoHashes, target: PathBuf, program_after: bool) {
        let start_now = {
            let mut shared = self.lock();
            Self::record_move(&mut shared, id, target, program_after)
        };
        if start_now {
            self.start_move(id);
        }
    }

    /// Starts one waiting move, if it is waiting, not already running, and still what the settings want.
    ///
    /// The answer is not awaited. A folder on another filesystem can take minutes to copy, and the watch
    /// folder, the session's events and every other torrent's move cannot wait behind it: the answer comes
    /// back on the channel and is dealt with at the next wake, which is at most [`WATCH_EVERY`] away.
    fn start_move(&self, id: &InfoHashes) {
        let Some(torrent) = self.session.torrent(id) else {
            self.lock().moves.remove(id);
            return;
        };
        let settings = self.settings();
        let status = torrent.status();
        // Claimed as running in the same look that decides it may start, so a wake that comes round while
        // this one is running does not start a second copy of it.
        let claimed = {
            let mut shared = self.lock();
            let Some(queued) = shared.moves.get_mut(id) else {
                return;
            };
            if queued.in_flight || queued.given_up {
                return;
            }
            let still_wanted = wanted(status.progress, &status.save_path, &settings)
                .as_deref()
                .is_some_and(|target| target == queued.target.as_path());
            if !still_wanted {
                // It is where it belongs, or the settings want somewhere else now: this entry is a leftover,
                // and the look that queued what is wanted now has had its own say.
                shared.moves.remove(id);
                return;
            }
            queued.in_flight = true;
            Some(queued.target.clone())
        };
        let Some(target) = claimed else {
            return;
        };
        let id = *id;
        let name = status.name.clone();
        let moves = self.moves_tx.clone();
        tokio::spawn(async move {
            let outcome = torrent
                .move_storage(target.clone(), MoveMode::FailIfExists)
                .await;
            // A receiver that has gone away means this task is on its way out; the engine has the files
            // where they were asked to go either way.
            let _ = moves.send(Moved {
                id,
                target,
                name,
                outcome,
            });
        });
    }

    /// Picks up the answers of the moves that were running, then starts the ones that are not.
    ///
    /// `Busy` - the engine is still checking that torrent's data, which is ordinary just after an add -
    /// leaves the move queued, so this wake or the next one, at most [`WATCH_EVERY`] away, asks again. Any
    /// other refusal is about a folder that is not going to start working on its own: it is said once, the
    /// files stay where they are, and a whole torrent's program still runs, for the folder they are in. A
    /// finished torrent does not go without its program because a *move* could not be done, and the folder it
    /// did finish in is the folder a program wants to be told about.
    fn ask_moves(&mut self) {
        while let Ok(moved) = self.moves_rx.try_recv() {
            self.answer_move(moved);
        }
        let waiting: Vec<InfoHashes> = {
            let shared = self.lock();
            shared
                .moves
                .iter()
                .filter(|(_, queued)| !queued.in_flight && !queued.given_up)
                .map(|(id, _)| *id)
                .collect()
        };
        for id in waiting {
            self.start_move(&id);
        }
    }

    /// What one move's answer means for the entry table, applied to it, and what it means outside the
    /// table: see [`Answered`].
    ///
    /// The answer carries the folder *that move* was heading for. If the entry wants a different one -
    /// the settings moved the goal while the copy was running - the answer is about a move nobody wants
    /// any more: it neither completes the new entry nor condemns it, and all it may do is release the
    /// in-flight claim the new entry is still wearing from the move it replaced, so the entry's own move
    /// can start. An answer for a torrent with no entry is the same shape from the other side: the
    /// torrent left the library mid-move, and with it went everything it owed.
    fn answer_for(
        shared: &mut Shared,
        id: &InfoHashes,
        target: &Path,
        outcome: Result<(), SessionError>,
    ) -> Answered {
        let Some(entry) = shared.moves.get_mut(id) else {
            return Answered::Stale;
        };
        if entry.target != target {
            entry.in_flight = false;
            return Answered::Stale;
        }
        // The entry is what has been tracking this torrent since before the move started; whether its
        // program is owed is its fact, not something the answer carries. A torrent that became whole
        // mid-move is owed a program its queued move never heard about.
        let program_after = entry.program_after;
        match outcome {
            Ok(()) => {
                shared.moves.remove(id);
                Answered::Landed { program_after }
            }
            // Still checking this torrent's data: ask again at the next wake.
            Err(SessionError::Busy) => {
                entry.in_flight = false;
                Answered::Busy
            }
            Err(problem) => {
                // A target that is somebody else's file, or a folder that cannot be moved into, is not
                // going to change because this task asked again: said once, and left alone until the
                // settings change, the torrent goes, or the torrent ends up somewhere else.
                entry.in_flight = false;
                entry.given_up = true;
                Answered::Refused {
                    problem,
                    program_after,
                }
            }
        }
    }

    /// What one move's answer means: the program for files that arrived, another try for files the engine
    /// was not ready to move, one line plus a program run where the files are for a move that is not
    /// going to work, and nothing at all for an answer the entry table no longer wants.
    ///
    /// `Busy` - the engine is still checking that torrent's data, which is ordinary just after an add -
    /// leaves the move queued, so this wake or the next one, at most [`WATCH_EVERY`] away, asks again. Any
    /// other refusal is about a folder that is not going to start working on its own: it is said once, the
    /// files stay where they are, and a whole torrent's program still runs, for the folder they are in. A
    /// finished torrent does not go without its program because a *move* could not be done, and the folder it
    /// did finish in is the folder a program wants to be told about.
    fn answer_move(&self, moved: Moved) {
        let Moved {
            id,
            target,
            name,
            outcome,
        } = moved;
        let answered = {
            let mut shared = self.lock();
            Self::answer_for(&mut shared, &id, &target, outcome)
        };
        match answered {
            Answered::Stale => {
                tracing::debug!(
                    torrent = %map::clean(&name),
                    target = %map::clean(&target.display().to_string()),
                    "a move this automation no longer wants answered; the entry's own move will report",
                );
            }
            Answered::Landed { program_after } => {
                if program_after {
                    // The folder this move put the files in is the folder they finished in, and that is what
                    // the program is told. Asking the engine again here could read a folder somebody else
                    // moved them into in between.
                    self.run_program(&id, &name, &target);
                }
            }
            // The entry stays queued; `ask_moves` starts it again, this wake or the next.
            Answered::Busy => {}
            Answered::Refused {
                problem,
                program_after,
            } => {
                let settings = self.settings();
                let line = format!(
                    "Could not move {} into the {}: {}",
                    map::clean(&name),
                    folder_name(&target, &settings),
                    map::clean(&problem.to_string()),
                );
                self.notify(Notice::Error(line));
                if program_after {
                    let here = self.session.torrent(&id).map(|t| t.status().save_path);
                    if let Some(here) = here {
                        self.run_program(&id, &name, &here);
                    }
                }
            }
        }
    }

    /// One pass over the watch folder. Everything about it that does not need the engine - the folder
    /// listing, a candidate's bytes, the parse, the renames - happens on the blocking pool, and the
    /// engine taking the torrent is the only thing awaited.
    async fn scan_watch_folder(&self) {
        #[cfg(feature = "test-support")]
        // A park this automation's test asked for, if it asked: the wake holds where it is, asleep in
        // the runtime rather than on a thread, until the test lets it go.
        test_support::park_the_pass(&self.park).await;
        let Some(dir) = self.settings().config.watch_dir.clone() else {
            return;
        };
        // What the last pass saw, and what it dealt with and could not move: both go to the blocking pool
        // with the folder, and both come back as what the next pass works on.
        let (previous, dealt_with) = {
            let shared = self.lock();
            (shared.scan.clone(), shared.unmoved.clone())
        };
        let swept = {
            let dir = dir.clone();
            match tokio::task::spawn_blocking(move || watch::sweep(&dir, &previous, &dealt_with))
                .await
            {
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

        // Files that are not going to be added, each with the whole line to say about it: bytes never read
        // because of their size, bytes that are not a torrent, and files already dealt with whose rename
        // failed. Those last ones carry the line they were filed under, so the field shows the same words on
        // every pass instead of a paraphrase of them.
        for (path, line) in &swept.refused {
            self.complain(path, map::clean(line));
        }

        // What this pass dealt with and could not move, as only the engine's answers could find out: the
        // sweep keeps records of its own, and these join them at the end.
        let mut learned: watch::UnmovedMap = watch::UnmovedMap::new();
        for watch::Ready { path, metainfo } in swept.ready {
            let settings = self.settings();
            let file = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            // The look this pass read it at, which is what the next pass compares against before it reads
            // the same file again.
            let look = swept.scan.get(&path).copied();
            let mut params = AddTorrent::new(metainfo, settings.default_dir.clone());
            // A watch folder's torrents obey the switch like every other way in: nothing arrives until
            // somebody starts this one.
            params.paused = settings.config.start_paused;
            match self.session.add_torrent(params).await {
                Ok(torrent) => {
                    // Renamed out of the candidates' way, which is what keeps it from being added again -
                    // across a restart too, because the rename is the record.
                    if let Some(record) = self.mark_added(&path, &file, look).await {
                        learned.insert(path.clone(), record);
                    }
                    let status = torrent.status();
                    let shown = if status.name.is_empty() {
                        file
                    } else {
                        status.name
                    };
                    self.notify(Notice::Info(format!("Added {}", map::clean(&shown))));
                }
                // Somebody else put this torrent in the library first: the same file under another name,
                // or the same torrent from the screen. It is in the library, which is all this folder was
                // for, so it gets the same rename as any other added file and no line about it.
                Err(SessionError::DuplicateTorrent) => {
                    if let Some(record) = self.mark_added(&path, &file, look).await {
                        learned.insert(path.clone(), record);
                    }
                }
                // The session is going away underneath this one, on its way out. The file stays a
                // candidate for the next run to add, and there is nothing to complain about on the way out
                // of a process.
                Err(SessionError::Shutdown) => {}
                // A torrent the engine will not take: its reason, said with the file's name, and renamed
                // `.invalid` so it is not asked again.
                Err(problem) => {
                    let mut reason = map::clean(&problem.to_string());
                    let renamed = rename_out(&path, watch::INVALID).await;
                    let still_there = renamed.is_err();
                    if let Err(problem) = renamed {
                        reason = format!(
                            "{reason}, and it could not be renamed out of the way: {problem}"
                        );
                    }
                    let line = map::clean(&format!(
                        "Could not add {file} from the watch folder: {reason}"
                    ));
                    self.complain(&path, line.clone());
                    // Still there, and the engine is not going to change its mind about bytes that have not
                    // changed: the record that stops the next passes reading it for the same answer.
                    if still_there && let Some(look) = look {
                        learned.insert(path.clone(), watch::Unmoved { look, line });
                    }
                }
            }
        }

        // What the next look compares against, what it leaves alone, and the complaints to keep quiet about,
        // down to the files still in the folder: one that was renamed away or deleted cannot complain again,
        // and a record of a file that is gone is worth nothing.
        let mut still_there: HashSet<PathBuf> = swept.scan.keys().cloned().collect();
        still_there.extend(swept.refused.iter().map(|(path, _)| path.clone()));
        still_there.extend(learned.keys().cloned());
        let mut shared = self.lock();
        shared.scan = swept.scan;
        // What the sweep found, plus what only the engine's answers could find - which happened after the
        // sweep, and is the later word on any file both of them have something to say about.
        let mut unmoved = swept.unmoved;
        unmoved.extend(learned);
        shared.unmoved = unmoved;
        shared.announced.retain(|path| still_there.contains(path));
    }

    /// Renames a settled candidate `.added` now that the engine has it, and says so if the rename is what
    /// failed.
    ///
    /// `None` is the ordinary answer. `Some(record)` is the file still sitting where it was, already in the
    /// library, with the line said about it and the look it was read at: while it still looks like that the
    /// folder leaves it alone, because offering the engine the same bytes every two seconds answers "already
    /// here" and renews a complaint nobody is going to act on twice more.
    async fn mark_added(
        &self,
        path: &Path,
        file: &str,
        look: Option<(u64, SystemTime)>,
    ) -> Option<watch::Unmoved> {
        let Err(problem) = rename_out(path, watch::ADDED).await else {
            return None;
        };
        let line = map::clean(&format!(
            "Could not finish adding {file} from the watch folder: it is in the library, but it could \
             not be renamed out of the way: {problem}"
        ));
        self.complain(path, line.clone());
        look.map(|look| watch::Unmoved { look, line })
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

    /// Records the finish and starts the program this config names, saying nothing unless the program
    /// would not start. What it did after that - how long it ran, what it exited with - is in the log and
    /// is nobody's business here.
    ///
    /// One torrent gets one attempt at its program, however many wakes find it finished: the claim is
    /// taken before anything is looked at - including before the settings are asked whether a program is
    /// configured at all - so a finish that arrives twice, or is found again on the next look, does not
    /// play the film twice, and a finish that found nothing configured is *handled*, not merely skipped.
    /// A person who sets a program afterwards gets it for the next finish, not a chorus of runs for
    /// every finish this run already watched pass without one.
    fn run_program(&self, id: &InfoHashes, name: &str, folder: &Path) {
        if !self.lock().finish_handled.insert(*id) {
            let program = self.settings().config.on_complete;
            tracing::debug!(
                program = %program
                    .as_ref()
                    .map_or_else(String::new, |p| p.display().to_string()),
                "this torrent's finish has been handled already",
            );
            return;
        }
        let Some(program) = self.settings().config.on_complete.clone() else {
            return;
        };
        if let Err(problem) = program::run(&program, name, folder) {
            // The program's own path goes in the line: a name that is refused is fixed by changing that
            // line in `config.yaml`, and a person who is told only "would not start" has to guess which of
            // the things they own is the one that would not.
            let line = format!(
                "Could not start the program run on completion: {} {problem}",
                map::clean(&program.display().to_string())
            );
            let changed = {
                let mut shared = self.lock();
                let changed = shared.on_complete_problem.as_deref() != Some(line.as_str());
                shared.on_complete_problem = Some(line.clone());
                changed
            };
            // One line per torrent that finished and could not run its program: two torrents, two lines.
            // The field beside the program shows the last of them.
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
    /// The park flags this automation's worker also holds; see [`park_the_watch_folder`](Self::park_the_watch_folder).
    #[cfg(feature = "test-support")]
    park: Arc<test_support::ParkFlags>,
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

    /// Holds this automation's next watch folder pass in the middle of its wake:
    /// [`watch_folder_is_parked`](Self::watch_folder_is_parked) says when it has, and nothing the session
    /// says afterwards is acted on until [`release_the_watch_folder`](Self::release_the_watch_folder).
    ///
    /// Only this automation is parked - the flags belong to it - so a test holding two handles (or a
    /// test running beside another test's automation in the same process) parks exactly the loop it is
    /// testing. Test-support builds only; see [`crate::downloads::test_support`] for what a test does
    /// with a wake it can hold.
    #[cfg(feature = "test-support")]
    pub fn park_the_watch_folder(&self) {
        test_support::ask_for_park(&self.park);
    }

    /// Whether this automation's pass is parked right now: while it is, the loop is in the middle of a
    /// wake. Test-support builds only.
    #[cfg(feature = "test-support")]
    pub fn watch_folder_is_parked(&self) -> bool {
        test_support::pass_is_parked(&self.park)
    }

    /// Lets this automation's parked pass finish its look at the folder. Test-support builds only.
    #[cfg(feature = "test-support")]
    pub fn release_the_watch_folder(&self) {
        test_support::release_the_park(&self.park);
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
    use super::{Answered, InfoHashes, Move, Settings, Shared, Worker, wanted};
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

    /// A made-up torrent id, distinguishable from every other one a test here builds.
    fn torrent_number(n: u8) -> InfoHashes {
        let hex = format!("{:0>40x}", n);
        fbt::InfoHashes::from_v1(hex.parse().expect("a made-up 40-hex info hash"))
    }

    /// One entry table, holding one entry, exactly as the worker would hold it.
    fn one_entry(shared: &mut Shared, id: &InfoHashes, entry: Move) {
        shared.moves.insert(*id, entry);
    }

    /// Re-pointing a move that is already running replaces the entry - the settings moved the goal -
    /// but does not start a second `move_storage` behind the first: the in-flight claim carries over,
    /// and only the running move's own answer, once it is answered, clears it.
    #[test]
    fn replacing_a_running_move_waits_for_the_running_one() {
        let id = torrent_number(7);
        let old = PathBuf::from("/first");
        let new = PathBuf::from("/second");
        let mut shared = Shared::default();
        one_entry(
            &mut shared,
            &id,
            Move {
                target: old.clone(),
                program_after: false,
                in_flight: true,
                given_up: false,
            },
        );

        assert!(
            !Worker::record_move(&mut shared, &id, new.clone(), true),
            "a copy asked for while the engine is still copying under the old goal waits, it does not start: \
             the engine would only refuse the second one mid-copy",
        );
        let queued = shared.moves.get(&id).expect("the new goal is recorded");
        assert_eq!(
            queued.target, new,
            "the entry now points where the settings want"
        );
        assert!(
            queued.program_after,
            "and the entry, not the running copy, is what carries the program"
        );
        assert!(!queued.given_up, "a new folder is a new attempt");
        assert!(
            queued.in_flight,
            "the in-flight claim carries over: the superseded move has to answer first"
        );

        // The superseded move answers, naming its own folder. It may release the claim and nothing else:
        // it does not complete the new entry, does not condemn it, and does not speak for its program.
        let answered = Worker::answer_for(&mut shared, &id, &old, Ok(()));
        assert!(
            matches!(answered, Answered::Stale),
            "an answer for a folder the entry no longer wants is spent quietly: {answered:?}"
        );
        let queued = shared
            .moves
            .get(&id)
            .expect("the superseded answer does not remove the entry");
        assert_eq!(
            queued.target, new,
            "the entry keeps the folder the settings want"
        );
        assert!(
            queued.program_after,
            "and keeps the program the settings made it owed"
        );
        assert!(
            !queued.given_up,
            "a superseded landing is not this entry's failure"
        );
        assert!(
            !queued.in_flight,
            "but the claim the entry borrowed from the superseded move is released"
        );

        // With the claim released the entry is startable, and an ask for the same folder only ever
        // upgrades its program flag.
        assert!(
            Worker::record_move(&mut shared, &id, new.clone(), false),
            "with the claim gone the entry is startable again",
        );
        assert!(
            shared
                .moves
                .get(&id)
                .expect("still the same entry")
                .program_after,
            "and asking for the folder it already wants never clears the program it owes",
        );
    }

    /// An answer is taken for what the entry wants now: a matching folder completes or condemns the
    /// entry and its own `program_after`; a folder the entry does not want, or a torrent that has no
    /// entry at all (it left the library mid-move), is stale.
    #[test]
    fn an_answer_is_read_against_the_entry_not_against_the_move() {
        let id = torrent_number(12);
        let here = PathBuf::from("/downloads");

        // The move lands where the entry wants, on a torrent the entry owes a program to: the entry is
        // done and the program is owed.
        let mut shared = Shared::default();
        one_entry(
            &mut shared,
            &id,
            Move {
                target: here.clone(),
                program_after: true,
                in_flight: true,
                given_up: false,
            },
        );
        let answered = Worker::answer_for(&mut shared, &id, &here, Ok(()));
        assert!(
            matches!(
                answered,
                Answered::Landed {
                    program_after: true
                }
            ),
            "files home on a torrent owed its program: {answered:?}"
        );
        assert!(
            shared.moves.is_empty(),
            "the entry that landed is gone: {:?}",
            shared.moves
        );

        // The engine is not ready: the entry stays, claim released, for the next ask.
        let mut shared = Shared::default();
        one_entry(
            &mut shared,
            &id,
            Move {
                target: here.clone(),
                program_after: true,
                in_flight: true,
                given_up: false,
            },
        );
        let answered = Worker::answer_for(&mut shared, &id, &here, Err(fbt::SessionError::Busy));
        assert!(
            matches!(answered, Answered::Busy),
            "not ready yet: {answered:?}"
        );
        let queued = shared.moves.get(&id).expect("a busy answer waits");
        assert!(
            !queued.in_flight,
            "the claim is released so the next ask can take it"
        );
        assert!(!queued.given_up, "and nothing was given up over a busy");

        // A folder nobody can move into: the entry gives up once, and its program is still owed -
        // somewhere else, for the folder the files are in, which is the caller's to look up.
        let mut shared = Shared::default();
        one_entry(
            &mut shared,
            &id,
            Move {
                target: here.clone(),
                program_after: true,
                in_flight: true,
                given_up: false,
            },
        );
        let answered =
            Worker::answer_for(&mut shared, &id, &here, Err(fbt::SessionError::Shutdown));
        let Answered::Refused {
            problem,
            program_after,
        } = answered
        else {
            panic!("a refusal is a refusal: {answered:?}");
        };
        assert_eq!(problem.to_string(), fbt::SessionError::Shutdown.to_string());
        assert!(
            program_after,
            "a dead move does not cancel the owed program"
        );
        let queued = shared.moves.get(&id).expect("a given-up entry stays put");
        assert!(queued.given_up, "the reason is not going to fix itself");
        assert!(
            !queued.in_flight,
            "and no second asking burns the same dead end"
        );

        // A torrent with no entry - gone from the library while its move ran - has nothing left for an
        // answer to do.
        let mut shared = Shared::default();
        let answered = Worker::answer_for(&mut shared, &id, &here, Ok(()));
        assert!(
            matches!(answered, Answered::Stale),
            "a gone torrent went with everything it owed: {answered:?}"
        );
    }
}
