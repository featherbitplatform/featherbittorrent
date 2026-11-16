//! Running a screen over one backend, and moving it to another: what switching is made of.
//!
//! The binary's setup (`run.rs`) decides what to start on and hands the result here. From that point this
//! module owns the screen's loop - keys, snapshots, notices, the frame clock - and everything that happens
//! when the person asks the screen to run somewhere else instead:
//!
//! - A switch starts what was named *before* it stops what is running. A connection cannot fail to start; an
//!   engine can, and an engine that will not start must not cost anybody the connection they already have.
//! - Nothing is written down about a server until its own snapshot says the sign-in worked. `ui.yaml` and the
//!   keychain both wait for that answer, which is what [`Login`] and [`settle`] are for: a password the server
//!   refused is not worth keeping, and an address nobody reached is not worth pointing the next run at.
//! - The lock this library's engine needs is asked for before anything else is touched, so a switch that was
//!   never going to happen stops nothing (`plan_switch` carries that rule).
//!
//! The two decisions that need no terminal and no file system are pure (`plan_switch` and
//! `initial_target` live in `backend.rs`); what has effects - starting, stopping, saving, the keychain - is
//! small, ordered, and takes its seams as arguments (`show`, `start_engine`) so the parts worth testing can
//! be tested without any of it.

use std::future::Future;
use std::pin::Pin;
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

use fbtt_core::client::ClientParts;
use fbtt_core::remote::SIGN_IN_REFUSED;
use fbtt_core::ui_state::{LastUsed, RemoteTarget, UiState};
use fbtt_core::{
    Command, Config, ConfigLoad, Connection, LibraryLock, LinkState, Notice, Paths, Secret,
    Snapshot,
};
use fbtt_tui::{Action, ConnectRequest, ConnectTarget, Modal, Model, format, theme, view};

use crate::backend::{Backend, BackendKind, LockOutcome, SwitchPlan, plan_switch};
use crate::run::{cannot_lock, library_in_use};
use crate::secrets::{
    KEYCHAIN_UNAVAILABLE, KeepPlan, SecretStore, apply_keep, forget_password, keep_plan,
    remote_secret,
};

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
/// What the footer says when the server refused the password that was kept for it. It names what happens
/// next - the screen is opened with the address already in it - because the person has to type it again,
/// and knowing why keeps them from typing the same wrong thing a third time.
const SAVED_PASSWORD_REFUSED: &str = "The saved password was refused; enter it again.";

/// What a connection was signed in as, and what this run still owes it.
///
/// A server is not remembered, and its password is not kept, the moment a connection is made: until that
/// connection's own snapshot says the sign-in worked, nobody knows whether this address and this credential
/// are worth writing down. So everything a switch would otherwise write waits here, and [`settle`] pays it on
/// the first snapshot that says the link is up - or lets it go, if the person moves on before then.
///
/// `from_keychain` is the other half of what makes a refusal worth acting on: only a password this run read
/// out of the keychain means "what we kept was wrong", which is a thing to take out and to ask again about
/// (see [`stored_password_refused`]).
#[derive(Debug)]
pub struct Login {
    /// Where this connection goes: what `ui.yaml` is pointed at once it is in, and what the connect screen
    /// is prefilled with when its password turns out to be wrong.
    target: RemoteTarget,
    /// What the keychain should hold once this connection has signed in - see [`crate::secrets::keep_plan`].
    keep: KeepPlan,
    /// Whether the credential it signs with came out of the keychain.
    from_keychain: bool,
    /// Whether the first sign-in has been paid for. Once it has, nothing here is owed again, so a snapshot
    /// arriving every second costs nothing.
    settled: bool,
}

impl Login {
    /// A connection this run started from what it remembered: what `ui.yaml` named, signed with whatever the
    /// keychain held for it - or with nothing yet, when what is missing is about to be asked for on the
    /// connect screen. Nothing was collected from anybody, so there is nothing new to keep; `from_keychain`
    /// says whether the credential it does sign with came out of there.
    pub fn remembered(target: RemoteTarget, from_keychain: bool) -> Login {
        Login {
            target,
            keep: KeepPlan::Nothing,
            from_keychain,
            settled: false,
        }
    }

    /// A connection that came out of the connect screen. What the keychain should hold is what the person
    /// asked for - see [`keep_plan`] - and a password they just typed is not one the keychain can be wrong
    /// about, so a refusal of it needs no repair.
    pub fn answered(target: RemoteTarget, keep: KeepPlan) -> Login {
        Login {
            target,
            keep,
            from_keychain: false,
            settled: false,
        }
    }
}

/// What switching a screen from one backend to another may need from the run itself: where this library's
/// files are, and where a connect password lives. The config is not here because a switch does not reuse
/// startup's: it reads `config.yaml` again (see [`local_config`]).
pub struct Library<'a> {
    pub paths: &'a Paths,
    pub store: &'a Arc<dyn SecretStore + Send + Sync>,
}

/// Why the loop ended. `Failed` is a failure of this process - its line travels with it,
/// to be printed after the terminal is back, because stderr on the alternate screen is
/// not readable anywhere the person can look. A backend that could not save reports
/// itself when it is stopped instead.
pub enum Quit {
    Ok,
    Failed(String),
}

/// What one turn of the loop answered with.
enum Outcome {
    /// The person is finished (or the terminal or the backend went away, which quits the same way).
    Quit(Quit),
    /// The connect screen's answer: run on that one instead. Acting on it is the loop above
    /// this one's job, so nothing else is sent to a backend that is about to be stopped.
    Connect(ConnectRequest),
}

/// SIGTERM on Unix, Ctrl-C elsewhere (where raw mode does not swallow it), listened for
/// once at startup: tokio keeps the signal's own queue between polls, so a listener made
/// once cannot lose a signal that lands mid-loop, while a fresh listener per iteration
/// can. A system that will not hand over a listener runs without that signal; on Unix
/// Ctrl-C arrives as a key press instead, and the model's map answers it. Both quit
/// without asking - a signal is not a person who can answer a confirmation.
pub struct ShutdownSignal {
    wait: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl ShutdownSignal {
    pub fn install() -> ShutdownSignal {
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

/// The screen's own state: what to draw, and the snapshot it is drawn from. A frame needs both, and a switch
/// replaces one with the other's, so they travel as one thing rather than as two more arguments for every
/// function that touches the screen.
pub struct Screen<'a> {
    pub model: &'a mut Model,
    pub snap: &'a mut Arc<Snapshot>,
}

/// The screen over `backend`, through every backend this run switches to, until the person is finished.
///
/// The backend is taken in and handed back because it cannot be moved about inside the loop below, where the
/// channels it owns are borrowed by `select!`. `login` travels with it: what the connection it is running on
/// was signed in as, and what is still owed for it. A switch replaces both together; a quit drops whatever
/// was still owed, because an answer that never signed in is not worth remembering - which is the whole
/// reason it was waited for.
pub async fn run_session(
    terminal: &mut DefaultTerminal,
    mut screen: Screen<'_>,
    mut backend: Backend,
    mut login: Option<Login>,
    library: &Library<'_>,
    stream: &mut EventStream,
    signal: &mut ShutdownSignal,
) -> (Quit, Backend) {
    loop {
        let (mut parts, task) = backend.into_parts();
        let outcome = run_loop(
            terminal,
            &mut screen,
            &mut parts,
            login.as_mut(),
            library,
            stream,
            signal,
        )
        .await;
        // Whatever it answered with, this is still what is running: `Connect` means something else runs
        // *next*, not that this one has stopped.
        backend = Backend::from_parts(parts, task);
        match outcome {
            Outcome::Quit(quit) => return (quit, backend),
            Outcome::Connect(request) => {
                match switch(library, backend, request, &mut screen, &mut |model| {
                    draw_saving(terminal, model)
                })
                .await
                {
                    Switched::Now {
                        backend: next,
                        login: next_login,
                        notices,
                    } => {
                        backend = next;
                        login = next_login;
                        for notice in notices {
                            screen.model.push_notice(notice, Instant::now());
                        }
                    }
                    Switched::Kept(current, notice) => {
                        backend = current;
                        screen.model.push_notice(notice, Instant::now());
                    }
                }
            }
        }
    }
}

/// Keys, snapshots, notices and the frame clock, waited on all at once. Nothing here awaits a backend
/// reply: each branch takes what has arrived and returns. The channels arrive as separate parts so that no
/// borrow of one can fight a use of another in the same `select!`.
///
/// Two things read what arrives rather than only drawing it: the first snapshot that says this connection
/// signed in pays for it (see [`settle`]), and a line saying the kept password was refused takes that entry
/// out and asks again (see [`stored_password_refused`]). Both are once-per-connection, and both leave the
/// backend's channels alone.
async fn run_loop(
    terminal: &mut DefaultTerminal,
    screen: &mut Screen<'_>,
    parts: &mut ClientParts,
    mut login: Option<&mut Login>,
    library: &Library<'_>,
    stream: &mut EventStream,
    signal: &mut ShutdownSignal,
) -> Outcome {
    // The screen's two halves are used apart all through the loop - a key press reads one and writes the
    // other - so they are taken out here, once, rather than reached through `screen` in every branch.
    let model = &mut *screen.model;
    let snap = &mut *screen.snap;
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
                            // The connect screen's answer means "run on that instead". Starting
                            // the one that was named, stopping this one and remembering the choice
                            // are the loop above this one's steps, so it stops here.
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
                    // The first snapshot that says this connection is in is what its password and its
                    // place in `ui.yaml` were waited for. Nobody else can answer that question: this run
                    // did not dial the server, this backend did.
                    let owed = match login.as_deref_mut() {
                        Some(login) if signed_in(snap) => {
                            settle(login, Arc::clone(library.store), library.paths).await
                        }
                        _ => Vec::new(),
                    };
                    for notice in owed {
                        model.push_notice(notice, Instant::now());
                    }
                }
                // The backend is gone; nothing more will change on this screen.
                Err(_) => return Outcome::Quit(Quit::Ok),
            },
            notice = once(parts.notices.recv()) => match notice {
                Some(first) => {
                    dirty = true;
                    // A backend that is unhappy says so once per run of trouble, so what arrives at all is
                    // one story rather than one line: take the rest of it too, and read every piece, because
                    // the refusal of a kept password may well be second in the queue rather than first.
                    let mut lines = vec![first];
                    while let Ok(next) = parts.notices.try_recv() {
                        lines.push(next);
                    }
                    let mut refused = None;
                    if let Some(login) = login.as_deref_mut() {
                        for notice in &lines {
                            if stored_password_refused(login, notice) {
                                refused = Some(login.target.clone());
                                break;
                            }
                        }
                    }
                    for notice in lines {
                        model.push_notice(notice, Instant::now());
                    }
                    if let Some(target) = refused {
                        // The keychain holds a password this server said no to. It goes, so the next run asks
                        // instead of refusing the same thing again; and the screen that asks is opened with
                        // the address already in it, because this run chose that address and the person did
                        // not type it. Where a dialog is already up, that dialog keeps the keyboard: what a
                        // person is half-way through typing outranks our question about it.
                        model.push_notice(Notice::Error(SAVED_PASSWORD_REFUSED.to_string()), Instant::now());
                        if model.modal.is_none() {
                            model.modal = Some(Modal::connect(Some(&target)));
                        }
                        // A keychain that will not answer has nothing we can take out of it, and the line
                        // above already says what happens next: this failure needs no second notice.
                        let _ = forget_password(Arc::clone(library.store), target).await;
                    }
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

/// Acts on the connect screen's answer: start what was named, then stop what was running, then remember it.
///
/// The lock is asked about before anything else (see [`plan_switch`]), because a person who cannot have this
/// library should not lose the connection they already have for a switch that could not happen; and the new
/// backend starts before the old one is stopped, because an engine's start is the one thing here that can
/// fail. A `Kept` therefore means nothing was stopped at all.
async fn switch(
    library: &Library<'_>,
    backend: Backend,
    request: ConnectRequest,
    screen: &mut Screen<'_>,
    show: &mut dyn FnMut(&mut Model),
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
            switch_onward(
                library,
                backend,
                request,
                new_lock,
                screen,
                show,
                start_engine,
            )
            .await
        }
    }
}

/// The half of a switch that may replace what is running: start the one that was named, stop the one that
/// was not, and put the screen on the new one.
///
/// `show` is what the caller wants drawn while the old backend is stopped - the `Saving...` screen in a run,
/// nothing at all in a test - and `start_engine` is the engine start itself, handed in so that the case this
/// function exists to get right (an engine that will not start) can be tested without an engine.
async fn switch_onward<F>(
    library: &Library<'_>,
    backend: Backend,
    request: ConnectRequest,
    new_lock: Option<LibraryLock>,
    screen: &mut Screen<'_>,
    show: &mut dyn FnMut(&mut Model),
    start_engine: impl FnOnce(Config, Paths, LibraryLock) -> F,
) -> Switched
where
    F: Future<Output = Result<Backend, String>>,
{
    let model = &mut *screen.model;
    let snap = &mut *screen.snap;
    // The credential the new backend signs with is chosen from the answer before either backend is touched,
    // so a target and a secret that do not go together never cost anybody their running backend. What the
    // keychain should hold is decided here too but *done* later: nothing is stored until this connection has
    // signed in (see [`settle`]), because a password the server refused is not worth keeping.
    let secret = match &request.target {
        ConnectTarget::Remote(target) => remote_secret(target, &request.secret),
        // This library's engine needs no credential: the lock was the thing it needed, and that was asked
        // for before anything here moved.
        ConnectTarget::Local => Secret::None,
    };

    let (next, login, mut notices) = match request.target {
        ConnectTarget::Remote(target) => {
            let keep = keep_plan(&target, &request.secret);
            let login = Login::answered(target.clone(), keep);
            // A connection cannot fail to start: an address this run cannot dial says so through the client,
            // which is what the empty table beside the connect screen is for. Nothing is remembered about it
            // yet - not the address, not the password - because nobody has reached it yet.
            (
                Backend::start_remote(target, secret),
                Some(login),
                Vec::new(),
            )
        }
        ConnectTarget::Local => {
            // The guard came with the lock answer that said this switch could go ahead; a `Go` without one
            // is a caller's mistake rather than a thing a person can do. Nothing has been stopped yet, so it
            // is an ordinary "keep what you have" answer rather than the end of a run.
            let Some(lock) = new_lock else {
                return Switched::Kept(
                    backend,
                    Notice::Error(cannot_lock(library.paths, "the lock went missing")),
                );
            };
            // The config is read again now rather than reused from startup: limits changed during this run
            // are in that file and nowhere else (the engine writes them there), and an engine started on
            // what was in memory would quietly put them back.
            let mut lines = Vec::new();
            let (config, problem) = match local_config(library.paths) {
                Ok(both) => both,
                Err(problem) => return Switched::Kept(backend, Notice::Error(problem)),
            };
            if let Some(problem) = problem {
                lines.push(Notice::Error(problem));
            }
            let next = match start_engine(config, library.paths.clone(), lock).await {
                Ok(next) => next,
                // Nothing was stopped to get here, so nothing has to be put back: this run keeps the
                // connection it had and reads the engine's own reason for refusing on the footer.
                Err(problem) => return Switched::Kept(backend, Notice::Error(problem)),
            };
            // This library's own engine needs nothing proved about it - it is running here, on this
            // library - so `ui.yaml` can say so now. A connection waits for its first sign-in.
            let remembered = UiState {
                last_used: LastUsed::Local,
            };
            if let Err(problem) = remembered.save(library.paths) {
                lines.push(Notice::Error(could_not_save(
                    library.paths,
                    &problem.to_string(),
                )));
            }
            (next, None, lines)
        }
    };

    // Only now is what was running told to stop: an engine's last save - and with it this library's lock -
    // arrives while its replacement is already answering commands. A connection owes no file, so stopping it
    // cannot fail and there is nothing to report about it.
    match backend.stop(model, show).await {
        Ok(()) => {}
        // An engine's last save is the one thing its stop owes us, and it belongs on the screen beside
        // everything else this run has to say about the library it just let go of.
        Err(problem) => notices.push(Notice::Error(problem)),
    }

    // The screen shows what this backend knows about its library, not what the one being stopped saw: a
    // fresh engine or connection starts with an empty list and fills it in on its own.
    adopt(&next, model, snap);
    // A backend that has never driven this screen does not know what the screen is looking at, and the loop
    // below only sends a watch once the selection moves.
    start_watching(&next, model);

    Switched::Now {
        backend: next,
        login,
        notices,
    }
}

/// What an answer on the connect screen did to what is running (see [`switch`]).
///
/// There is no answer that leaves nothing running: the new backend starts before the old one stops, so a
/// switch that cannot go ahead keeps what was there and says why on the footer.
#[derive(Debug)]
enum Switched {
    /// The switch went ahead: this is the backend to drive from here, what its sign-in is still owed, and
    /// the lines its start and the stop before it owe the person - a config that could not be read, an
    /// engine whose last save failed, a keychain that would not write. The screen has already been pointed
    /// at it.
    Now {
        backend: Backend,
        login: Option<Login>,
        notices: Vec<Notice>,
    },
    /// Nothing was stopped, because the switch could not happen or was not needed: keep what is running and
    /// put the reason where the person can see it.
    Kept(Backend, Notice),
}

/// `Backend::start_local` in the shape [`switch_onward`] takes: the engine's own error becomes its one line,
/// and `paths` comes by value so a test can hand in a starter of its own - one that fails on purpose - with
/// no engine and no terminal anywhere near it.
async fn start_engine(config: Config, paths: Paths, lock: LibraryLock) -> Result<Backend, String> {
    Backend::start_local(config, &paths, lock)
        .await
        .map_err(|problem| problem.to_string())
}

/// The config this library's engine starts on, read again from `config.yaml`.
///
/// A run changes limits with `+` and `-` while it runs, and the engine writes them to that file; an engine
/// started from what startup read would quietly put the old ones back. The same three answers `Config::load`
/// gives at startup: loaded, missing (this library's defaults, written back out), and broken (the defaults
/// plus the line saying what was wrong, which becomes a notice rather than ending the run). `Err` is one line
/// about there being no config to start an engine on at all.
fn local_config(paths: &Paths) -> Result<(Config, Option<String>), String> {
    match Config::load(paths) {
        Ok(ConfigLoad::Loaded(config)) => Ok((config.resolve(paths), None)),
        Ok(ConfigLoad::Missing) => {
            let config = Config::default_for(paths).resolve(paths);
            // A file that went away mid-run comes back with this library's defaults, the way a first run
            // writes it. A directory that will not take it is worth a line, not a quit: the engine still has
            // its data directory to write to.
            let problem = config
                .save(&paths.config_file())
                .err()
                .map(|problem| problem.to_string());
            Ok((config, problem))
        }
        Ok(ConfigLoad::Bad { config, problem }) => Ok((config.resolve(paths), Some(problem))),
        Err(problem) => Err(problem.to_string()),
    }
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
pub fn start_watching(backend: &Backend, model: &Model) {
    let _ = backend.commands().send(Command::Watch(model.watch()));
}

/// Whether this snapshot says a connection signed in: the header's `Connected`, which is the first thing
/// worth remembering a password for. An engine's own library answers `Local` and is never asked.
fn signed_in(snap: &Snapshot) -> bool {
    matches!(
        snap.connection,
        Connection::Remote {
            state: LinkState::Connected,
            ..
        }
    )
}

/// The first snapshot that says a connection signed in pays what starting it borrowed: `ui.yaml` pointed at
/// this server, and the password the person said to keep.
///
/// Both wait for the backend's own answer because until then nobody knows whether this address and this
/// credential are worth remembering - a run that reached nothing should not leave its next run pointed at
/// nothing, and a password a server refused is not a password to keep. A keychain that will not write and a
/// file that will not save come back as lines for the footer; the connection stands either way. Once it has
/// been paid for it is never owed again, so the snapshot that arrives every second costs nothing.
async fn settle(
    login: &mut Login,
    store: Arc<dyn SecretStore + Send + Sync>,
    paths: &Paths,
) -> Vec<Notice> {
    if login.settled {
        return Vec::new();
    }
    login.settled = true;
    let keep = std::mem::replace(&mut login.keep, KeepPlan::Nothing);
    let ui = UiState {
        last_used: LastUsed::Remote(login.target.clone()),
    };
    let paths = paths.clone();
    // A file write and a call to another process, neither of which belongs on the thread that draws the
    // screen. Whichever of them answers badly is said in the words the person can act on; a blocking task
    // that was dropped mid-flight gets the same line as one that refused, because to the person they are
    // the same answer.
    let lines = tokio::task::spawn_blocking(move || {
        let mut lines = Vec::new();
        if let Err(problem) = ui.save(&paths) {
            lines.push(could_not_save(&paths, &problem.to_string()));
        }
        if let Err(problem) = apply_keep(&*store, &keep) {
            lines.push(problem);
        }
        lines
    })
    .await
    .unwrap_or_else(|_| vec![KEYCHAIN_UNAVAILABLE.to_string()]);
    lines.into_iter().map(Notice::Error).collect()
}

/// Whether one line from a connection says the password this run read out of the keychain was refused.
///
/// The two things a backend gives a screen - a snapshot and a line - cannot say "refused" except in words,
/// so this is where `fbtt_core::remote::SIGN_IN_REFUSED` is read back out of one. That coupling is why only
/// a password that came out of the keychain is acted on: what somebody just typed needs no repair (they are
/// looking at the screen they typed it into), and once this connection has signed in, a later refusal is
/// about something else entirely - a token that expired, a server that changed its users.
///
/// Fires once: `from_keychain` goes with the answer, so a server that keeps refusing does not reopen the
/// question every backoff pause.
fn stored_password_refused(login: &mut Login, notice: &Notice) -> bool {
    if !login.from_keychain || login.settled {
        return false;
    }
    let refused = matches!(notice, Notice::Error(text) if text.starts_with(SIGN_IN_REFUSED));
    if refused {
        login.from_keychain = false;
    }
    refused
}

/// The line for a library whose `ui.yaml` would not take what this run learned from it.
fn could_not_save(paths: &Paths, problem: &str) -> String {
    format!("Could not save {}: {problem}", paths.ui_file().display())
}

/// The `Saving...` screen, with the newest notice still on its footer: a run that was
/// told it will not save must not read as one that did.
pub fn draw_saving(terminal: &mut DefaultTerminal, model: &Model) {
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

#[cfg(test)]
mod tests {
    use super::{
        Library, Login, Screen, Switched, could_not_save, local_config, settle,
        stored_password_refused, switch_onward,
    };
    use crate::backend::{Backend, BackendKind};
    use crate::fake_store::FakeStore;
    use crate::secrets::{KeepPlan, SecretStore};
    use fbtt_core::scratch::ScratchDir;
    use fbtt_core::ui_state::{LastUsed, LoginMethod, RemoteTarget, UiState};
    use fbtt_core::{Config, LibraryLock, Notice, Paths, Secret};
    use fbtt_tui::{ConnectRequest, ConnectTarget, Model, SecretInput};
    use std::fs;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use zeroize::Zeroizing;

    /// The two directories of one scratch library, made real. The `ScratchDir` is what cleans them up, so a
    /// test keeps it alive for as long as it uses the paths.
    fn scratch_library(name: &str) -> (ScratchDir, Paths) {
        let dir = ScratchDir::new(name);
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        fs::create_dir_all(&paths.config_dir).expect("the config directory");
        fs::create_dir_all(&paths.data_dir).expect("the data directory");
        (dir, paths)
    }

    /// A server nothing is listening at, which is enough for a switch test: a connection never fails to
    /// start, and what it says about the address afterwards is `fbtt-core`'s tested business, not this one's.
    fn nowhere() -> RemoteTarget {
        RemoteTarget {
            url: "http://127.0.0.1:1".to_string(),
            login: LoginMethod::None,
            ..RemoteTarget::default()
        }
    }

    /// An engine start, in the shape `switch_onward` takes it: boxed, so that a test can hand in a starter
    /// of its own - one that fails on purpose - where production hands in `Backend::start_local`.
    type Starter = Pin<Box<dyn Future<Output = Result<Backend, String>>>>;

    /// An engine start that works, standing in for the real one: only which config it was handed matters to
    /// the tests below, so it hands back a connection rather than an engine.
    fn started(what: RemoteTarget) -> impl FnOnce(Config, Paths, LibraryLock) -> Starter {
        move |_config, _paths, _lock| {
            let backend = Backend::start_remote(what.clone(), Secret::None);
            Box::pin(async move { Ok(backend) })
        }
    }

    /// An engine start that refuses, with a line of its own to be reported.
    fn refused(problem: &str) -> impl FnOnce(Config, Paths, LibraryLock) -> Starter {
        let problem = problem.to_string();
        move |_config, _paths, _lock| Box::pin(async move { Err(problem.clone()) })
    }

    /// A switch back to the local engine starts it on what `config.yaml` says *now*, not on what startup read
    /// at the beginning of the run: limits changed during a run live in that file and nowhere else.
    #[tokio::test]
    async fn a_switch_back_to_the_engine_starts_on_the_config_as_it_is_now() {
        let (_dir, paths) = scratch_library("switch_fresh_config");
        fs::write(paths.config_file(), "limits:\n  download_kib: 4321\n")
            .expect("a config with new limits");
        let lock = LibraryLock::acquire(&paths.lock_file()).expect("a scratch library is free");

        // What startup read an hour ago, with no limits in it.
        // What startup read an hour ago, with no limits in it: the config a switch must *not* start the
        // engine on. It is not passed to `switch_onward` at all - the file is what it reads.
        let fake = Arc::new(FakeStore::new());
        let store: Arc<dyn SecretStore + Send + Sync> = fake.clone();
        let library = Library {
            paths: &paths,
            store: &store,
        };

        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        let record = move |config: Config, paths: Paths, lock: LibraryLock| {
            recorded
                .lock()
                .expect("no panic inside the lock")
                .push(config.limits.download_kib);
            let backend = Backend::start_remote(nowhere(), Secret::None);
            drop((paths, lock));
            Box::pin(async move { Ok(backend) })
        };

        // The connection this run is running on, which the switch below replaces. Its own first snapshot is
        // what the screen carries in, so no test has to invent one.
        let running = Backend::start_remote(nowhere(), Secret::None);
        let mut snap = running.initial_snapshot();
        let mut model = Model::new();
        let mut screen = Screen {
            model: &mut model,
            snap: &mut snap,
        };
        let switched = switch_onward(
            &library,
            running,
            ConnectRequest {
                target: ConnectTarget::Local,
                secret: SecretInput::default(),
            },
            Some(lock),
            &mut screen,
            &mut |_| {},
            record,
        )
        .await;

        let handed_on = seen.lock().expect("no panic inside the lock").clone();
        assert_eq!(
            handed_on,
            vec![4321],
            "the engine was started on what startup read, not on the file"
        );
        let Switched::Now {
            backend,
            login,
            notices,
            ..
        } = switched
        else {
            panic!("the engine started, got {switched:?}");
        };
        assert_eq!(
            backend.kind(),
            BackendKind::Remote,
            "a stand-in for the engine"
        );
        assert!(login.is_none(), "this library's engine signs in to nothing");
        assert!(notices.is_empty(), "nothing went wrong: {notices:?}");
        // An engine on this library needs nothing proved about it, so `ui.yaml` says so right away.
        assert_eq!(UiState::load(&paths).last_used, LastUsed::Local);
    }

    /// The case the ordering exists for: an engine that will not start must not cost the person the
    /// connection they were on, because that connection was never stopped.
    #[tokio::test]
    async fn a_failed_engine_start_keeps_the_connection_it_was_replacing() {
        let (_dir, paths) = scratch_library("switch_engine_fails");
        fs::write(paths.config_file(), "listen_port: 6881\n").expect("a config");
        // Where this run is running now, so a switch that gives up can be seen to have changed nothing.
        let before = RemoteTarget {
            url: "https://bit.example".to_string(),
            ..RemoteTarget::default()
        };
        UiState {
            last_used: LastUsed::Remote(before.clone()),
        }
        .save(&paths)
        .expect("a ui.yaml pointing at this run's server");
        let lock = LibraryLock::acquire(&paths.lock_file()).expect("a scratch library is free");

        let fake = Arc::new(FakeStore::new());
        let store: Arc<dyn SecretStore + Send + Sync> = fake.clone();
        let library = Library {
            paths: &paths,
            store: &store,
        };

        let running = Backend::start_remote(before.clone(), Secret::None);
        let mut snap = running.initial_snapshot();
        let mut model = Model::new();
        let mut screen = Screen {
            model: &mut model,
            snap: &mut snap,
        };
        let switched = switch_onward(
            &library,
            running,
            ConnectRequest {
                target: ConnectTarget::Local,
                secret: SecretInput::default(),
            },
            Some(lock),
            &mut screen,
            &mut |_| {},
            refused("the library would not open"),
        )
        .await;

        let Switched::Kept(kept, notice) = switched else {
            panic!(
                "an engine that would not start should keep this run where it was, got {switched:?}"
            );
        };
        assert_eq!(
            kept.kind(),
            BackendKind::Remote,
            "the connection is still running"
        );
        assert_eq!(
            notice,
            Notice::Error("the library would not open".to_string())
        );
        // Nothing was written about a switch that did not happen...
        assert_eq!(UiState::load(&paths).last_used, LastUsed::Remote(before));
        // ...and the library this run took for the engine that never started is free again.
        LibraryLock::acquire(&paths.lock_file())
            .expect("the lock taken for the failed start comes back with it");
    }

    /// A switch to a server remembers nothing until that server says the sign-in worked: neither the address
    /// in `ui.yaml` nor the password in the keychain, which is what makes a refused password cheap.
    #[tokio::test]
    async fn a_server_is_remembered_only_after_it_signs_in() {
        let (_dir, paths) = scratch_library("switch_remembers_later");
        fs::write(paths.config_file(), "listen_port: 6881\n").expect("a config");
        let fake = Arc::new(FakeStore::new());
        let store: Arc<dyn SecretStore + Send + Sync> = fake.clone();
        let library = Library {
            paths: &paths,
            store: &store,
        };

        let server = RemoteTarget {
            url: "https://bit.example".to_string(),
            login: LoginMethod::Password,
            user: Some("alice".to_string()),
            ..RemoteTarget::default()
        };
        let running = Backend::start_remote(nowhere(), Secret::None);
        let mut snap = running.initial_snapshot();
        let mut model = Model::new();
        let mut screen = Screen {
            model: &mut model,
            snap: &mut snap,
        };
        let switched = switch_onward(
            &library,
            running,
            ConnectRequest {
                target: ConnectTarget::Remote(server.clone()),
                secret: SecretInput {
                    password: Some(Zeroizing::new("hunter2".to_string())),
                    remember: true,
                    ..SecretInput::default()
                },
            },
            None,
            &mut screen,
            &mut |_| {},
            started(nowhere()),
        )
        .await;

        let Switched::Now { login, notices, .. } = switched else {
            panic!("the connection started, got {switched:?}");
        };
        assert!(notices.is_empty(), "nothing went wrong: {notices:?}");
        assert!(
            !paths.ui_file().exists(),
            "the next run is not pointed at this server yet"
        );
        assert!(
            fake.asked().is_empty(),
            "the keychain was written to already"
        );

        // The answer arrives, and what was held back is paid for: the address remembered, the password kept.
        let mut login = login.expect("a connection comes with its sign-in");
        let notices = settle(&mut login, Arc::clone(&store), &paths).await;
        assert!(notices.is_empty(), "both went in: {notices:?}");
        assert_eq!(UiState::load(&paths).last_used, LastUsed::Remote(server));
        assert_eq!(
            fake.kept("alice@https://bit.example"),
            Some("hunter2".to_string())
        );

        // A snapshot arrives every second. Only the first one that says this connection is in owes anything.
        let notices = settle(&mut login, Arc::clone(&store), &paths).await;
        assert!(notices.is_empty());
        assert_eq!(fake.asked(), vec!["set".to_string()]);
    }

    /// A refusal of the password that was kept is worth acting on once, and only when it was this run that
    /// kept it: what somebody just typed needs no repair, and a server nobody can reach makes nothing wrong.
    #[test]
    fn a_refusal_of_the_saved_password_is_only_worth_acting_on_once() {
        let refusal = Notice::Error("sign-in was refused: the password was wrong".to_string());
        let mut saved = Login::remembered(nowhere(), true);
        assert!(stored_password_refused(&mut saved, &refusal));
        assert!(
            !stored_password_refused(&mut saved, &refusal),
            "a server that keeps refusing does not get to ask the person twice"
        );

        let mut typed = Login::answered(nowhere(), KeepPlan::Nothing);
        assert!(
            !stored_password_refused(&mut typed, &refusal),
            "what somebody just typed is not the keychain's mistake"
        );

        let mut saved = Login::remembered(nowhere(), true);
        assert!(
            !stored_password_refused(
                &mut saved,
                &Notice::Error("the server could not be reached: connection refused".to_string())
            ),
            "a server nobody can reach has not refused anybody's password"
        );

        // Once this connection signed in, a later refusal is about something else (a session that ended, a
        // user that was taken out), and the kept password stays where it is.
        let mut settled = Login::remembered(nowhere(), true);
        settled.settled = true;
        assert!(
            !stored_password_refused(&mut settled, &refusal),
            "this run already knows the password was good once"
        );
    }

    /// A library whose config file went away mid-run starts its engine on this library's defaults, and a
    /// broken one says what is wrong with it instead of refusing to start at all.
    #[test]
    fn a_config_that_is_not_there_or_not_readable_still_starts_an_engine() {
        let (_dir, paths) = scratch_library("local_config_answers");

        let (config, problem) =
            local_config(&paths).expect("no file is an answer: this library's defaults");
        assert_eq!(config.limits.download_kib, 0);
        assert_eq!(problem, None);
        assert!(
            paths.config_file().exists(),
            "a config that was not there comes back with the defaults"
        );

        fs::write(paths.config_file(), "limits:\n  download_kib: 4321\n").expect("a config");
        let (config, problem) = local_config(&paths).expect("a readable config");
        assert_eq!(config.limits.download_kib, 4321);
        assert_eq!(problem, None, "nothing to complain about");

        fs::write(paths.config_file(), "limits: [unterminated\n")
            .expect("bytes that are not a config");
        let (_, problem) = local_config(&paths).expect("a broken config still says what to run on");
        assert!(
            problem.is_some_and(|problem| !problem.is_empty()),
            "a broken config comes with the line startup would have shown"
        );
    }

    /// The line for a `ui.yaml` that would not write names the file, so a person can fix it.
    #[test]
    fn a_ui_yaml_that_would_not_write_names_the_file() {
        let (_dir, paths) = scratch_library("could_not_save");
        let line = could_not_save(&paths, "Read-only file system");
        assert!(line.starts_with("Could not save "));
        assert!(line.contains("ui.yaml"));
        assert!(line.ends_with(": Read-only file system"));
    }
}
