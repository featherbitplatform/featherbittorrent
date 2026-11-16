//! The binary's own work: bring one library up, run its screen, put everything back.
//!
//! Setup happens in order - paths, config, which backend to start on, lock, terminal, backend - because
//! every step answers with its own line before the terminal is touched, and a second instance should hear
//! "close the other one" (or be offered the server running on that library) rather than see a blank screen
//! blink. The screen goes up before the engine does: a run that cannot show anything has no reason to start
//! an engine, and no resume data to write for a library nobody showed. The loop then waits on four things at
//! once (keys, snapshots, notices, the frame clock), never on a backend reply: it draws the newest snapshot
//! when something changed, at most thirty frames a second, and quitting is a command out and a join, not a
//! conversation. Connecting somewhere else is that same quit with a start after it - stop this backend, wait
//! for what its stop owes, run on the one that was named, and write the choice into `ui.yaml` so the next
//! run starts there too.

use std::future::Future;
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream};
// `std::stream::Stream` is unstable, so the stable one comes from the same futures
// tree crossterm's own `EventStream` implements it from.
use futures_core::Stream;
use ratatui::DefaultTerminal;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::widgets::{Fill, Paragraph};
use tokio::sync::mpsc;

use fbtt_core::client::ClientParts;
use fbtt_core::ui_state::{LastUsed, RemoteTarget, UiState};
use fbtt_core::{Command, Config, ConfigLoad, LibraryLock, Notice, Paths, Snapshot};
use fbtt_tui::{Action, ConnectRequest, ConnectTarget, Modal, Model, format, theme, view};

use crate::backend::{
    Backend, BackendKind, LockOutcome, Start, SwitchPlan, initial_target, named_server, plan_switch,
};
use crate::secrets::{
    Keychain, SecretStore, StartSecret, keep_password, remote_secret, start_secret_for,
};
use crate::terminal::Terminal;

/// The frame clock: redraws land at most this far apart, and only over a `dirty` flag.
const FRAME_TICK: Duration = Duration::from_millis(33);
/// What the screen says while the last save runs. A run that was told it will not save
/// shows its notice here instead, so a quit never promises more than the engine kept.
const SAVING: &str = "Saving...";
/// What the footer says when a key press produced a command the backend would not take:
/// its queue was full, so that press did nothing and the person should press it again.
const BUSY: &str = "Busy, try again";
/// What the footer says when the connect screen asks for the library this run already drives: the answer is
/// that nothing changes, so the press needs a word rather than silence.
const ALREADY_ON_THIS_LIBRARY: &str = "This library is already running its own engine";
/// The offer made to a run whose library another featherbittorrent has taken while running a server on it.
/// One line, answered with one letter, asked before this run takes the screen out from under that one.
const OFFERS_LOCAL_SERVER: &str =
    "A featherbittorrent server is running on this library. Connect to it? y/n";

/// One library's whole run: setup, the loop, the quit. The exit code is the answer.
pub async fn run() -> ExitCode {
    let paths = match Paths::resolve() {
        Ok(paths) => paths,
        Err(error) => return fail(&error),
    };

    // Where a connect password lives between runs. Making this touches nothing: the keychain is only
    // reached by the calls below, and only for a target that signs in with a password.
    let store: Arc<dyn SecretStore + Send + Sync> = Arc::new(Keychain);

    // A missing config is written with this library's defaults; a broken one came back
    // from `Config::load` as those defaults plus the line saying what was wrong (the file
    // itself is kept aside under `.bad`), and that line becomes the first notice. Every
    // branch is resolved: a config file names its download folder by hand, so it may say
    // `dl`, and downloads then belong below this library, not below whatever directory
    // the process happened to be started in.
    let mut first_notice = None;
    let config = match Config::load(&paths) {
        Ok(ConfigLoad::Loaded(config)) => config.resolve(&paths),
        Ok(ConfigLoad::Missing) => {
            let config = Config::default_for(&paths).resolve(&paths);
            if let Err(error) = config.save(&paths.config_file()) {
                return fail(&error);
            }
            config
        }
        Ok(ConfigLoad::Bad { config, problem }) => {
            first_notice = Some(Notice::Error(problem));
            config.resolve(&paths)
        }
        Err(error) => return fail(&error),
    };

    // Which backend the last run used is what decides everything after this: `ui.yaml`, beside
    // `config.yaml`. A missing file, or bytes this run could not make sense of, already answered "the local
    // engine" when it was read, so there is always an answer to start from.
    let ui = UiState::load(&paths);

    // Only a run the file points at this library asks the library whether it is free. A connection needs
    // nothing from these files - they belong to wherever the server runs - so taking the lock for one would
    // only make an unrelated featherbittorrent refuse to start for no reason.
    let (lock, outcome) = match &ui.last_used {
        LastUsed::Remote(_) => (None, LockOutcome::Taken),
        LastUsed::Local => {
            let acquired = LibraryLock::acquire(&paths.lock_file());
            let outcome = LockOutcome::of(&acquired);
            (acquired.ok(), outcome)
        }
    };

    // Before the terminal: a library somebody else is driving is a sentence to read, not a screen to see,
    // and so is the offer about the server running on it - that other run may be drawing on this terminal
    // right now. A keychain is consulted here too, for the same reason the engine is started after the
    // screen: nothing a person has to read should happen behind an alternate screen they cannot see yet.
    let opening = match initial_target(&ui, outcome, named_server(&paths)) {
        Start::Engine => match lock {
            Some(lock) => Opening::Engine {
                // The engine task owns its config; this run keeps a copy of its own, because switching back
                // to the local engine later starts it on the same one.
                config: config.clone(),
                lock,
            },
            // `Taken` is only ever reported by holding this library, so a run told to drive an engine has
            // the guard with it. This is a caller's mistake rather than a thing a person can do, so it gets
            // a line rather than a panic.
            None => return fail_line(&cannot_lock(&paths, "the lock went missing")),
        },
        Start::Server(target) => connect(Arc::clone(&store), target, false).await,
        // The offer was taken: run on the server this library is already running. Its address is this
        // program's own guess and its login method is not something this run can know, so the connect screen
        // goes up prefilled rather than leaving a connection that may need a login nobody was asked for.
        Start::AskAboutServer(target) => match take_the_offer() {
            Offer::Taken => connect(Arc::clone(&store), target, true).await,
            Offer::Declined => return leave(&library_in_use(&paths)),
        },
        Start::LibraryInUse => return leave(&library_in_use(&paths)),
        Start::LockFailed(problem) => return fail_line(&cannot_lock(&paths, &problem)),
    };

    // The screen comes up before the engine does. A run with no terminal to draw on has
    // nothing to show and no reason to start a session: it says its line and leaves, with
    // the library as it found it - no `session.state`, and the lock released on the way out.
    let mut terminal = match Terminal::setup() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("Could not set up the terminal: {error}");
            return ExitCode::FAILURE;
        }
    };

    // Listened for before anything else can go wrong from here on: a listener made once
    // buffers a signal that lands between two turns of the loop, where one made per turn
    // would miss it entirely.
    let mut signal = ShutdownSignal::install();

    let (backend, ask, notice) = match opening {
        Opening::Engine { config, lock } => {
            match Backend::start_local(config, &paths, lock).await {
                Ok(backend) => (backend, None, None),
                Err(error) => {
                    // The terminal goes back first: a line printed onto the alternate screen is
                    // not readable anywhere the person can look. Nothing was written to the
                    // library, so there is no save to wait for; the lock leaves with `lock`.
                    drop(terminal);
                    return fail(&error);
                }
            }
        }
        Opening::Server {
            target,
            start,
            connect_screen,
        } => {
            // A connection starts even with something missing from its credential: a server it cannot sign
            // in to says so once through the client, which is what the empty table beside the connect
            // screen is for. The screen itself collects whatever was missing - and goes up on top of a target
            // this run named by itself, whose login method nobody has chosen yet.
            let (secret, ask, notice) = match start {
                StartSecret::Ready(secret) => (
                    secret,
                    if connect_screen {
                        Some(target.clone())
                    } else {
                        None
                    },
                    None,
                ),
                StartSecret::Ask { secret, notice } => (secret, Some(target.clone()), notice),
            };
            (Backend::start_remote(target.clone(), secret), ask, notice)
        }
    };

    let mut model = Model::new();
    if let Some(notice) = first_notice {
        model.push_notice(notice, Instant::now());
    }
    if let Some(problem) = notice {
        model.push_notice(Notice::Error(problem), Instant::now());
    }
    if let Some(target) = &ask {
        model.modal = Some(Modal::connect(Some(target)));
    }
    // The value the watch already holds. The backend builds a snapshot before it
    // publishes one, so taking it is a clone of an `Arc`, never a wait.
    let mut snap = backend.initial_snapshot();
    model.sync_selection(&snap);
    start_watching(&backend, &model);

    // The event stream is made once for the whole screen and let go before the last save: its reader owns
    // stdin, and switching backends changes which library the screen talks to, not who owns the keyboard.
    let (quit, running) = {
        let mut stream = EventStream::new();
        run_session(
            terminal.get_mut(),
            &mut model,
            &mut snap,
            backend,
            &Library {
                paths: &paths,
                config: &config,
                store: &store,
            },
            &mut stream,
            &mut signal,
        )
        .await
    };

    // The backend is told once: by a command the loop sent or, if the queue was full, by its channels going
    // out of scope. No timeout - resume data is worth the wait. `Gone` is only reached when the last thing
    // this run tried to start failed, and there is nothing left to tell.
    let stopped = running
        .stop(&mut model, |model| draw_saving(terminal.get_mut(), model))
        .await;
    drop(terminal); // The terminal goes back before anything is printed to it.
    if let Err(problem) = &stopped {
        eprintln!("{problem}");
    }
    if let Quit::Failed(problem) = quit {
        eprintln!("{problem}");
        return ExitCode::FAILURE;
    }
    match stopped {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Why the loop ended. `Failed` is a failure of this process - its line travels with it,
/// to be printed after the terminal is back, because stderr on the alternate screen is
/// not readable anywhere the person can look. A backend that could not save reports
/// itself when it is stopped instead.
enum Quit {
    Ok,
    Failed(String),
}

/// What the run starts on, once `ui.yaml`, this library and the keychain have been asked about.
enum Opening {
    /// Drive this library's engine, on `config`, holding its lock.
    Engine { config: Config, lock: LibraryLock },
    /// Connect to `target`. `start` says what it signs in with and whether something still has to be asked
    /// for it on the connect screen; `connect_screen` opens that screen anyway, prefilled, for a target this
    /// run named itself - the server running on this library - where the address is known but how to sign in
    /// to it is not.
    Server {
        target: RemoteTarget,
        start: StartSecret,
        connect_screen: bool,
    },
}

/// Asks the keychain what to sign in to `target` with, which is the one thing about a remembered server
/// this run cannot read out of `ui.yaml` (see [`crate::secrets`]).
async fn connect(
    store: Arc<dyn SecretStore + Send + Sync>,
    target: RemoteTarget,
    connect_screen: bool,
) -> Opening {
    let start = start_secret_for(store, target.clone()).await;
    Opening::Server {
        target,
        start,
        connect_screen,
    }
}

/// Whether the person took the offer about the server running on this library.
///
/// Asked only where it can be answered: with no terminal in front of this run there is nobody to ask, and a
/// question written into a pipe nobody reads would hang a run that milestone 1 lets through with exit 3.
fn take_the_offer() -> Offer {
    if !std::io::stdin().is_terminal() {
        return Offer::Declined;
    }
    println!("{OFFERS_LOCAL_SERVER}");
    // A question still sitting in this process's buffer when the answer is wanted reads as a run that
    // printed nothing at all.
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    match std::io::stdin().lock().read_line(&mut answer) {
        // Nothing to read - the end of a pipe, or a stdin that will not answer: the offer stands and this
        // run leaves, which is what milestone 1 does with a held library.
        Ok(0) | Err(_) => Offer::Declined,
        Ok(_) if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") => Offer::Taken,
        Ok(_) => Offer::Declined,
    }
}

/// Whether `take_the_offer` got an answer, and which one.
enum Offer {
    /// Connect to the server running on this library instead of driving it here.
    Taken,
    /// Say the sentence milestone 1 says about a held library and leave with exit 3: declined, or nobody
    /// was there to ask.
    Declined,
}

/// Prints one line about a run that will not start, and gives the code for a taken library: this is where
/// the offer ends when it is not taken, and where a held library with nothing else to offer ends too.
fn leave(problem: &str) -> ExitCode {
    eprintln!("{problem}");
    ExitCode::from(3)
}

/// The sentence milestone 1 answers a held library with - still the answer when this run has nowhere else
/// to go, and the one the offer about that library leaves off with when it is declined.
fn library_in_use(paths: &Paths) -> String {
    format!(
        "Another featherbittorrent is already using this library ({}). Close it first.",
        paths.data_dir.display()
    )
}

/// The line for a library this run could not make sense of at all, which is not the same thing as a holder
/// and so does not deserve exit 3.
fn cannot_lock(paths: &Paths, problem: &str) -> String {
    format!(
        "Could not lock the library ({}): {problem}",
        paths.data_dir.display()
    )
}

/// What switching a screen from one backend to another may need from the run itself: where the library is,
/// what it runs on, and where a connect password lives.
struct Library<'a> {
    paths: &'a Paths,
    config: &'a Config,
    store: &'a Arc<dyn SecretStore + Send + Sync>,
}

/// The screen over `backend`, through every backend this run switches to, until the person is finished.
///
/// The backend is taken in and handed back so that switching can move it whole: stopping one and starting
/// the next is the same pair of steps a quit takes, and it cannot happen inside the loop below, where the
/// channels that backend owns are borrowed by `select!`.
async fn run_session(
    terminal: &mut DefaultTerminal,
    model: &mut Model,
    snap: &mut Arc<Snapshot>,
    mut backend: Backend,
    library: &Library<'_>,
    stream: &mut EventStream,
    signal: &mut ShutdownSignal,
) -> (Quit, Last) {
    loop {
        let (mut parts, task) = backend.into_parts();
        let outcome = run_loop(terminal, model, snap, &mut parts, stream, signal).await;
        backend = Backend::from_parts(parts, task);
        match outcome {
            Outcome::Quit(quit) => return (quit, Last::Running(backend)),
            Outcome::Connect(request) => {
                match switch(library, backend, request, snap, model, terminal).await {
                    Switched::Now(next, notices) => {
                        backend = next;
                        for notice in notices {
                            model.push_notice(notice, Instant::now());
                        }
                    }
                    Switched::Kept(current, notice) => {
                        backend = current;
                        model.push_notice(notice, Instant::now());
                    }
                    // Nothing is running any more: the only way a switch gets here is an engine that would
                    // not start after its predecessor had already saved and let go.
                    Switched::Ended(problem) => return (Quit::Failed(problem), Last::Gone),
                }
            }
        }
    }
}

/// What was running when the person finished, which is what the last save is told to stop.
enum Last {
    Running(Backend),
    /// Nothing: the last switch ended with a backend that could not start. There is no save to wait for,
    /// and the line about why is already on its way out with the quit.
    Gone,
}

impl Last {
    /// Stops what is left, with the `Saving...` screen up over it. See [`Backend::stop`].
    async fn stop(self, model: &mut Model, show: impl FnMut(&mut Model)) -> Result<(), String> {
        match self {
            Last::Running(backend) => backend.stop(model, show).await,
            Last::Gone => Ok(()),
        }
    }
}

/// What one turn of the loop answered with.
enum Outcome {
    /// The person is finished (or the terminal or the backend went away, which quits the same way).
    Quit(Quit),
    /// The connect screen's answer: stop this backend and run on that one. Acting on it is the loop above
    /// this one's job, so nothing else is sent to a backend that is about to be stopped.
    Connect(ConnectRequest),
}

/// Keys, snapshots, notices and the frame clock, waited on all at once. Nothing here awaits a backend
/// reply: each branch takes what has arrived and returns. The channels arrive as separate parts so that no
/// borrow of one can fight a use of another in the same `select!`.
async fn run_loop(
    terminal: &mut DefaultTerminal,
    model: &mut Model,
    snap: &mut Arc<Snapshot>,
    parts: &mut ClientParts,
    stream: &mut EventStream,
    signal: &mut ShutdownSignal,
) -> Outcome {
    // Nothing has been drawn yet, so the first tick draws.
    let mut dirty = true;
    let mut last_watch = model.watch();
    let mut frame = tokio::time::interval(FRAME_TICK);
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            // Every branch is a `once`: each wakes on one event, and re-arming the next
            // is what an iteration of this loop does anyway.
            // A stream polled by hand: `OnceStream` turns one item into one future,
            // which is what a branch that re-arms itself every iteration should be.
            event = once_stream(&mut *stream) => match event {
                Some(Ok(Event::Key(key))) => {
                    for action in model.handle_key(key, snap) {
                        match action {
                            // A full queue drops this command: the backend handles one at a
                            // time and every one is short, so 64 deep is more than a person can
                            // type, and what is dropped is a keystroke they will press again. Say
                            // so rather than let the screen look like it ignored them - unless
                            // this was the quit itself, which the `Saving...` screen answers.
                            Action::Command(command) => {
                                if !parts.commands.send(command) && !model.quit_requested() {
                                    model.push_notice(Notice::Error(BUSY.to_string()), Instant::now());
                                }
                            }
                            // Quitting is not a command to this backend: the `Saving...` screen
                            // below stops it on the way out, which is how an engine gets the
                            // chance to save and free its lock.
                            Action::Quit => {}
                            // The connect screen's answer means "run on that instead". Shutting
                            // this backend down, starting the one that was named and remembering
                            // the choice are the loop above this one's steps, so it stops here.
                            Action::Connect(request) => return Outcome::Connect(request),
                        }
                    }
                    dirty = true;
                }
                Some(Ok(Event::Resize(_, _))) => dirty = true,
                // A closed event stream is the terminal going away: quit like any other.
                Some(Err(_)) | None => return Outcome::Quit(Quit::Ok),
                Some(Ok(_)) => {}
            },
            // `changed()` is a method on the receiver this loop owns, and its body reads
            // that same receiver - no branch here borrows anything another uses. The value
            // comes from `borrow_and_update`, which takes the snapshot that woke this wait;
            // reading through the client instead was the old bug: it answered from a
            // channel the backend no longer writes to.
            wake = parts.snapshots.changed() => match wake {
                Ok(()) => {
                    *snap = parts.snapshots.borrow_and_update().clone();
                    model.sync_selection(snap);
                    dirty = true;
                }
                // The backend is gone; nothing more will change on this screen.
                Err(_) => return Outcome::Quit(Quit::Ok),
            },
            notice = once(parts.notices.recv()) => match notice {
                Some(notice) => {
                    model.push_notice(notice, Instant::now());
                    dirty = true;
                    drain_notices(model, &mut parts.notices, |_| {});
                }
                // The backend stopped and the queue is read out: the screen has all it gets.
                None => return Outcome::Quit(Quit::Ok),
            },
            _ = frame.tick() => {
                // Notices expire on the clock rather than on a key press. `tick` reports
                // nothing, so before and after say whether the footer needs a redraw.
                let notice_before = model.notice().is_some();
                model.tick(Instant::now());
                dirty |= notice_before && model.notice().is_none();
                if !dirty {
                    continue;
                }
                dirty = false;
                let watch = model.watch();
                if watch != last_watch {
                    parts.commands.send(Command::Watch(watch.clone()));
                    last_watch = watch;
                }
                if let Err(error) = terminal.draw(|frame| view::draw(frame, model, snap)) {
                    // The screen is the thing that failed: carry the line out of here and
                    // print it once the terminal is back.
                    return Outcome::Quit(Quit::Failed(format!("the terminal failed: {error}")));
                }
            },
            _ = once(signal.wait.as_mut()) => return Outcome::Quit(Quit::Ok),
        }
        if model.quit_requested() {
            return Outcome::Quit(Quit::Ok);
        }
    }
}

/// Acts on the connect screen's answer: stop what is running, start what was asked for, remember it.
///
/// The lock is asked about before anything is stopped (see [`plan_switch`]), because a person who cannot
/// have this library should not lose the connection they already have for a switch that could not happen.
/// What the answer says about the keychain is done on the way in, so its line belongs to the screen it
/// describes rather than arriving after somebody has already moved on.
async fn switch(
    library: &Library<'_>,
    backend: Backend,
    request: ConnectRequest,
    snap: &mut Arc<Snapshot>,
    model: &mut Model,
    terminal: &mut DefaultTerminal,
) -> Switched {
    // The lock check comes first, and only of the switch that needs it: a server connection asks nothing of
    // this library, and a run already driving its own engine holds its lock (asking would only report a
    // holder that is itself).
    let (new_lock, outcome) = match (&request.target, backend.kind()) {
        (ConnectTarget::Local, BackendKind::Remote) => {
            let acquired = LibraryLock::acquire(&library.paths.lock_file());
            let outcome = LockOutcome::of(&acquired);
            (acquired.ok(), outcome)
        }
        _ => (None, LockOutcome::Taken),
    };
    match plan_switch(&request.target, backend.kind(), outcome) {
        SwitchPlan::AlreadyHere => {
            Switched::Kept(backend, Notice::Info(ALREADY_ON_THIS_LIBRARY.to_string()))
        }
        SwitchPlan::LibraryInUse => {
            Switched::Kept(backend, Notice::Error(library_in_use(library.paths)))
        }
        SwitchPlan::LockFailed(problem) => {
            Switched::Kept(backend, Notice::Error(cannot_lock(library.paths, &problem)))
        }
        SwitchPlan::Go => {
            switch_onward(library, backend, request, new_lock, snap, model, terminal).await
        }
    }
}

/// The half of a switch that anything may replace: the old backend is stopped and waited for - which is how
/// an engine writes its resume file and lets go of this library, and what a switch into the engine needs
/// free - then the one that was named starts, and `ui.yaml` remembers it.
async fn switch_onward(
    library: &Library<'_>,
    backend: Backend,
    request: ConnectRequest,
    new_lock: Option<LibraryLock>,
    snap: &mut Arc<Snapshot>,
    model: &mut Model,
    terminal: &mut DefaultTerminal,
) -> Switched {
    // What the keychain should hold after this answer, decided from what was collected while it is still
    // beside everything it describes, and carried out off the thread that draws the screen.
    let mut notices = Vec::new();
    if let ConnectTarget::Remote(target) = &request.target {
        let keep = keep_password(
            Arc::clone(library.store),
            target.clone(),
            request.secret.clone(),
        );
        if let Err(problem) = keep.await {
            notices.push(Notice::Error(problem));
        }
    }
    // The credential the new connection signs with is chosen before anything is stopped, so a target and a
    // secret that do not go together never cost anybody their running backend.
    let secret = match &request.target {
        ConnectTarget::Remote(target) => remote_secret(target, &request.secret),
        // This library's engine needs no credential: the lock was the thing it needed, and that was asked
        // for before anything here moved.
        ConnectTarget::Local => fbtt_core::Secret::None,
    };

    match backend
        .stop(model, |model| draw_saving(terminal, model))
        .await
    {
        Ok(()) => {}
        // An engine's last save is the one thing its stop owes us, and it belongs on the screen beside
        // everything else this run has to say about the library it just let go of.
        Err(problem) => notices.push(Notice::Error(problem)),
    }

    let (next, remembered) = match request.target {
        ConnectTarget::Remote(target) => {
            let remembered = LastUsed::Remote(target.clone());
            // A connection cannot fail to start: an address this run cannot dial says so through the client.
            (Backend::start_remote(target, secret), remembered)
        }
        ConnectTarget::Local => {
            // The guard came with the lock answer that said this switch could go ahead; a `Go` without one
            // is a caller's mistake rather than a thing a person can do.
            let Some(lock) = new_lock else {
                return Switched::Ended(cannot_lock(library.paths, "the lock went missing"));
            };
            match Backend::start_local(library.config.clone(), library.paths, lock).await {
                Ok(next) => (next, LastUsed::Local),
                // The engine that was running already saved and let go of the library, so there is nothing
                // left to draw with: this ends the run the way a failed start before the screen does.
                Err(problem) => return Switched::Ended(problem.to_string()),
            }
        }
    };

    // The screen shows what this backend knows about its library, not what the one being stopped saw: a
    // fresh engine or connection starts with an empty list and fills it in on its own.
    adopt(&next, model, snap);
    // A backend that has never driven this screen does not know what the screen is looking at, and the loop
    // below only sends a watch once the selection moves.
    start_watching(&next, model);

    // Where this run ended up is what the next one starts on. A library that cannot be written does not cost
    // anybody the connection they just asked for: say so on the footer and carry on.
    let state_to_save = UiState {
        last_used: remembered,
    };
    if let Err(problem) = state_to_save.save(library.paths) {
        notices.push(Notice::Error(format!(
            "Could not save {}: {problem}",
            library.paths.ui_file().display()
        )));
    }

    Switched::Now(next, notices)
}

/// What an answer on the connect screen did to what is running (see [`switch`]).
enum Switched {
    /// The switch went ahead: this is the backend to drive from here, with the lines its start and the stop
    /// before it owe - a keychain that would not write, an engine whose last save failed. The screen has
    /// already been pointed at it.
    Now(Backend, Vec<Notice>),
    /// Nothing was stopped, because the switch could not happen or was not needed: keep what is running and
    /// put the reason where the person can see it.
    Kept(Backend, Notice),
    /// The run cannot go on: an engine would not start after its predecessor had already saved and let go of
    /// this library, so there is nothing left to draw with. This line leaves with the quit, which is how a
    /// start that failed before the screen came up is reported too.
    Ended(String),
}

/// Points the screen at what `backend` knows about its library, so a switch does not leave the previous
/// backend's list and detail pane up while the new one fills its own in. The selection survives a switch onto
/// the same library, which still has that torrent; anywhere else it starts where a run that just began starts.
fn adopt(backend: &Backend, model: &mut Model, snap: &mut Arc<Snapshot>) {
    *snap = backend.initial_snapshot();
    model.sync_selection(snap);
}

/// Tells `backend` what the screen is looking at. Its own loop only sends a watch when the selection moves,
/// and this backend has never driven this screen: without this the detail pane stays empty until something is
/// pressed, which is not what the person is looking at. A full queue drops it, and the next change sends again.
fn start_watching(backend: &Backend, model: &Model) {
    let _ = backend.commands().send(Command::Watch(model.watch()));
}

/// SIGTERM on Unix, Ctrl-C elsewhere (where raw mode does not swallow it), listened for
/// once at startup: tokio keeps the signal's own queue between polls, so a listener made
/// once cannot lose a signal that lands mid-loop, while a fresh listener per iteration
/// can. A system that will not hand over a listener runs without that signal; on Unix
/// Ctrl-C arrives as a key press instead, and the model's map answers it. Both quit
/// without asking - a signal is not a person who can answer a confirmation.
struct ShutdownSignal {
    wait: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl ShutdownSignal {
    fn install() -> ShutdownSignal {
        #[cfg(unix)]
        let wait: Pin<Box<dyn Future<Output = ()> + Send>> =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut term) => Box::pin(async move {
                    term.recv().await;
                }),
                Err(_) => Box::pin(std::future::pending::<()>()),
            };
        #[cfg(not(unix))]
        let wait: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async {
            // Registered once, like the Unix listener; Ctrl-C is not a key here.
            let _ = tokio::signal::ctrl_c().await;
        });
        ShutdownSignal { wait }
    }
}

/// The `Saving...` screen, with the newest notice still on its footer: a run that was
/// told it will not save must not read as one that did.
fn draw_saving(terminal: &mut DefaultTerminal, model: &Model) {
    let _ = terminal.draw(|frame| {
        let area = frame.area();
        if area.is_empty() {
            return;
        }
        frame.render_widget(Fill::new(" ").bg(theme::INK_950), area);
        let line = match model.notice() {
            Some(Notice::Error(text)) => format::safe(text).fg(theme::RED_500),
            Some(Notice::Info(text)) => format::safe(text).fg(theme::INK_300),
            None => SAVING.fg(theme::INK_300),
        };
        let [_, middle, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(line).centered(), middle);
    });
}

/// Every notice already queued, taken without waiting: these are the answers a quitting
/// person should still see, and past their room in the queue they mean nothing new.
fn drain_notices(
    model: &mut Model,
    notices: &mut mpsc::Receiver<Notice>,
    mut show: impl FnMut(&mut Model),
) {
    while let Ok(notice) = notices.try_recv() {
        model.push_notice(notice, Instant::now());
        show(model);
    }
}

/// A line for a setup failure, printed before any terminal exists.
fn fail(error: &dyn std::error::Error) -> ExitCode {
    eprintln!("{error}");
    ExitCode::FAILURE
}

/// The same for a line this run wrote itself rather than got from an error type.
fn fail_line(problem: &str) -> ExitCode {
    eprintln!("{problem}");
    ExitCode::FAILURE
}

/// Polls `f` exactly once and answers right away when it is ready, which is what
/// `select!` wants from a branch that re-arms itself every iteration of the loop.
fn once<F: Future>(f: F) -> Once<F> {
    Once { inner: Box::pin(f) }
}

struct Once<F> {
    // Boxed so the wrapper is `Unpin` (what `select!` wants) whatever `F` is; being a
    // future, it reaches `select!`'s `IntoFuture` through std's blanket impl.
    inner: Pin<Box<F>>,
}

impl<F: Future> Future for Once<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<F::Output> {
        self.inner.as_mut().poll(cx)
    }
}

/// Polls `s` exactly once, reporting a finished stream as `Ready(None)` and a still-
/// sleeping one as `Pending`, so a stream joins the same `select!` as the futures above.
fn once_stream<S: Stream>(s: S) -> OnceStream<S> {
    OnceStream { inner: Box::pin(s) }
}

struct OnceStream<S> {
    // Boxed like `Once`: a future, so `select!`'s `IntoFuture` reaches it through the
    // blanket impl, and boxing keeps that path free of an `S: Unpin` bound.
    inner: Pin<Box<S>>,
}

impl<S: Stream> Future for OnceStream<S> {
    type Output = Option<S::Item>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        self.inner.as_mut().poll_next(cx)
    }
}
