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
//!   refused is not worth keeping, and an address nobody reached is not worth pointing the next run at. A
//!   server this run only borrowed - the hand-off to one running on this library - is never written down at
//!   all (see [`Remembered`]).
//! - Nothing on the screen thread waits for anything outside it. A keychain call and a file write go to
//!   blocking threads whose lines come back on [`Work`]'s queue, which the loop reads whenever they arrive: a
//!   machine whose keychain is asleep costs a late line, never a screen that stopped drawing. The switch does
//!   wait - starting and stopping a backend is one step after another - but it happens between two turns of
//!   the loop, while the person is looking at the `Saving...` pane or at the frame before it, so nothing is
//!   caught halfway through an animation.
//! - The lock this library's engine needs is asked for before anything else is touched, so a switch that was
//!   never going to happen stops nothing (`plan_switch` carries that rule).
//!
//! The decisions that need no terminal, no keychain and no file system are pure: `plan_switch` and
//! `initial_target` in `backend.rs`, [`remembered_ui`] here. What has effects - starting, stopping, saving,
//! the keychain - is small, ordered, and takes its seams as arguments (`show`, `start_engine`) so the parts
//! worth testing can be tested without any of it.

use std::future::Future;
use std::path::PathBuf;
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
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use fbtt_core::client::ClientParts;
use fbtt_core::player::{self, SystemLookup};
use fbtt_core::ui_state::{LastUsed, RemoteTarget, UiState};
use fbtt_core::{
    Command, Config, ConfigLoad, Connection, LibraryLock, LinkState, Notice, Paths, PlayTarget,
    PlayerConfig, Secret, Snapshot,
};
use fbtt_tui::graphics::place::{AfterDraw, LogoView, Placement, Placer, PreparedLogos};
use fbtt_tui::{Action, ConnectRequest, ConnectTarget, Modal, Model, format, theme, view};

use crate::backend::{Backend, BackendKind, LockOutcome, SwitchPlan, plan_switch};
use crate::run::{cannot_lock, library_in_use};
use crate::secrets::{
    KeepPlans, SecretStore, apply_keep, forget_password, keep_plans, remote_secret,
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

/// Whether a connection is worth writing into `ui.yaml` once it signs in.
#[derive(Debug)]
enum Remembered {
    /// Somebody chose this server: a previous run wrote it into `ui.yaml`, or a person typed the address into
    /// the connect screen. The next run starts here too, so settling writes it down.
    Chosen,
    /// This run borrowed the server rather than chose it: the hand-off to the featherbittorrent running on
    /// this library. Not written down, because that server exists only while the other run is up. Pointing the
    /// next run at it would have it dial an address nobody answers while its own engine sat unused, and the
    /// offer about that server is something only a start on this library makes.
    Borrowed,
}

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
    /// What the keychain should hold once this connection has signed in: the credential it signs with and,
    /// when the target names a gateway header, the key for that header - see [`crate::secrets::keep_plans`].
    keep: KeepPlans,
    /// Whether this address belongs in `ui.yaml` once the sign-in answers - see [`Remembered`] and
    /// [`remembered_ui`].
    remembers: Remembered,
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
    /// says whether the credential it does sign with came out of there. A previous run already chose this
    /// address, so settling has nothing new to write about it.
    pub fn remembered(target: RemoteTarget, from_keychain: bool) -> Login {
        Login {
            target,
            keep: KeepPlans::nothing(),
            remembers: Remembered::Chosen,
            from_keychain,
            settled: false,
        }
    }

    /// A connection that came out of the connect screen. What the keychain should hold is what the person
    /// asked for - see [`crate::secrets::keep_plans`] - and a password they just typed is not one the keychain
    /// can be wrong about, so a refusal of it needs no repair. They chose this address, so it is what the next
    /// run starts on too.
    pub fn answered(target: RemoteTarget, keep: KeepPlans) -> Login {
        Login {
            target,
            keep,
            remembers: Remembered::Chosen,
            from_keychain: false,
            settled: false,
        }
    }

    /// The hand-off: this run connected to a server that another featherbittorrent is running on *this*
    /// library, because that other run offered it. The address was taken, not chosen, and it stops being real
    /// the moment that other run closes, so nothing about it is written down (see [`Remembered`]). It signs
    /// with what this run already holds: nobody was asked for a credential, so there is nothing new to keep
    /// and nothing the keychain could be wrong about.
    pub fn borrowed(target: RemoteTarget) -> Login {
        Login {
            target,
            keep: KeepPlans::nothing(),
            remembers: Remembered::Borrowed,
            from_keychain: false,
            settled: false,
        }
    }
}

/// What `ui.yaml` says once this connection has signed in, or `None` to leave the file exactly as it found
/// it. Pure because which address the next run should start on is a decision rather than an effect, and the
/// hand-off rule is the part of that decision worth testing without a file system to test against.
fn remembered_ui(login: &Login) -> Option<LastUsed> {
    match login.remembers {
        Remembered::Chosen => Some(LastUsed::Remote(login.target.clone())),
        Remembered::Borrowed => None,
    }
}

/// The work this run does away from the screen, and where its lines come back.
///
/// A keychain call and a file write are conversations with something that may take a while to answer - on
/// macOS a keychain can be waiting for somebody to unlock it - so neither happens on the thread that draws
/// the screen: each goes to this run's one worker thread (see [`Jobs`], [`send_work`]) and whatever it says
/// arrives on this queue, which the loop reads whenever it turns up (see [`run_loop`]). What a job needs -
/// this library's files, where its connect passwords live, and which media player a file opens in -
/// travels with the job because that is all this run ever sends away for. The config does not: a switch
/// reads `config.yaml` again (see [`local_config`]) because it needs the answer in hand before it can start
/// an engine on it, and that read happens between two turns of the loop, where nothing is drawing.
///
/// The worker thread holds the only sending end of that notice queue, which is what keeps it open for the
/// run's whole life: a queue with no senders left reads as a closed one, and the loop would be told there is
/// nothing to read on every single turn. It closes when this struct goes and the last job has answered - the
/// worker stops when the job queue empties with no senders left, and takes its sender with it - which is how
/// a test knows a save it asked for has finished: giving up its own `Work` leaves only the reading end of
/// that queue, and reading until it closes is reading everything the worker had to say.
pub struct Work<'a> {
    /// Where this library's files are: `config.yaml`, `ui.yaml`, the lock.
    pub paths: &'a Paths,
    /// Which media player this run opens a file in: the setting `config.yaml` had when this run started,
    /// which is not re-read on the way (`play_file` in this module starts the player with it).
    pub player: &'a PlayerConfig,
    /// Where a connect password lives between runs.
    pub store: &'a Arc<dyn SecretStore + Send + Sync>,
    rx: mpsc::Receiver<Notice>,
    /// The one worker every job of this run goes to (see [`Jobs`]).
    jobs: Jobs,
}

/// One piece of off-screen work: what to do away from the screen, and the lines it has to say when it is
/// done.
type Job = Box<dyn FnOnce() -> Vec<Notice> + Send>;

/// This run's one worker thread, reached through a queue nobody ever waits on.
///
/// A sign-in owes two writes - `ui.yaml` pointed at the server, and the password the person said to keep -
/// and a refusal of a kept password owes a delete that belongs beside them. Run on a pool of threads they
/// can be answered in any order, which is how a save lands after the delete that was meant to undo it, or a
/// remembered address overwrites a fresher one. One queue drained by one thread makes the order the person
/// did things in the order they happen in. Cloning this hands out another way of reaching that same thread;
/// the thread ends when the last handle goes, which is when a run lets go of its [`Work`].
#[derive(Clone)]
struct Jobs {
    tx: mpsc::UnboundedSender<Job>,
}

impl<'a> Work<'a> {
    /// The queue for one run's off-screen work, with its two ends wired together and the thread that does
    /// the work already started.
    pub fn new(
        paths: &'a Paths,
        player: &'a PlayerConfig,
        store: &'a Arc<dyn SecretStore + Send + Sync>,
    ) -> Work<'a> {
        // Eight lines is more than this run can have in flight at once - a sign-in's two writes, the delete
        // that follows a refusal, and the line about a file somebody asked to play. A queue that did fill up
        // would wait on the thread it was sent to, which is allowed to wait, rather than the screen, which
        // is not.
        let (tx, rx) = mpsc::channel(8);
        let (jobs_tx, mut waiting) = mpsc::unbounded_channel::<Job>();
        // A plain thread rather than a task: it spends its life waiting on another process, and being the
        // only one is what makes this run's writes happen in the order they were asked for. It holds the
        // notice queue's only sender, and ends when `jobs_tx` and every clone of it go - which is when the
        // run lets go of this struct.
        std::thread::spawn(move || {
            while let Some(job) = waiting.blocking_recv() {
                for notice in job() {
                    // This thread may wait for room; the screen never does. A queue nobody reads any more -
                    // a screen that went away mid-write - costs the line, not the write.
                    let _ = tx.blocking_send(notice);
                }
            }
        });
        Work {
            paths,
            player,
            store,
            rx,
            jobs: Jobs { tx: jobs_tx },
        }
    }

    /// Gives up this side of the work and hands back the reading end of the notice queue: what a job already
    /// answered still arrives, and the queue closes once the worker has drained what was queued to it and
    /// stopped. Only a test does this, to know that a save it asked for has finished; a run keeps its queue
    /// as long as its screen.
    #[cfg(test)]
    fn into_reader(self) -> mpsc::Receiver<Notice> {
        let Work { rx, jobs, .. } = self;
        // Dropping the last handle is what stops the worker, and it takes the queue's sender with it.
        drop(jobs);
        rx
    }
}

/// Hands one job to this run's worker (see [`Jobs`]). Nothing waits for the job, and nothing here can block:
/// the queue is unbounded, so a screen with three writes to make in one turn makes all three and carries on
/// drawing. What a job says back arrives on the queue [`Work`] reads whenever the worker gets an answer, and
/// jobs are answered in the order they were handed over - which is the whole reason there is one of them.
fn send_work(jobs: &Jobs, job: impl FnOnce() -> Vec<Notice> + Send + 'static) {
    // A send onto an unbounded queue cannot fail while its receiver is alive; the failure this ignores is a
    // worker that has already stopped, which means this run is on its way out.
    let _ = jobs.tx.send(Box::new(job));
}

/// Why the loop ended. `Failed` is a failure of this process - its line travels with it,
/// to be printed after the terminal is back, because stderr on the alternate screen is
/// not readable anywhere the person can look. A backend that could not save reports
/// itself when it is stopped instead.
pub enum Quit {
    Ok,
    Failed(String),
}

/// What one turn of the loop answered with. The request is boxed: it is a target plus two secrets, and every
/// turn that is not a quit hands one back, so a turn would move 232 bytes to say "somebody pressed Enter".
enum Outcome {
    /// The person is finished (or the terminal or the backend went away, which quits the same way).
    Quit(Quit),
    /// The connect screen's answer: run on that one instead. Acting on it is the loop above
    /// this one's job, so nothing else is sent to a backend that is about to be stopped.
    Connect(Box<ConnectRequest>),
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
    pub mark: &'a mut Mark<'a>,
    /// The state machine of the mark's prepares: what is wanted, what is in flight, and the
    /// protocol and scale every prepare of this run is made for.
    pub marks: MarkState,
}

/// The mark's bookkeeping for one screen: what the screen last drew, and the placer that remembers
/// it. Kept apart from [`Screen`] so the `Saving...` frame a switch draws on its way - which is a
/// frame like any other and goes through the same logo path - can borrow it while the screen it
/// belongs to is busy elsewhere.
pub struct Mark<'a> {
    pub logos: &'a mut Option<Arc<PreparedLogos>>,
    pub placer: &'a mut Placer,
}

/// What a prepare's answer leads to, as [`MarkState::answered`] decides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    /// The answer is the right size: swap the logos in.
    Swap,
    /// The answer is stale: drop it and prepare for this.
    Prepare(fbtt_tui::graphics::caps::Graphics),
}

/// The mark's prepare state machine, kept apart from the loop that drives it so every transition
/// is testable without a terminal: the startup, a resize, and the answer of a prepare.
#[derive(Debug, Clone)]
pub struct MarkState {
    /// The latest cell size estimate that counted as a change: the size the next prepare is for,
    /// remembered whether or not logos exist yet (a resize during the first prepare is not lost).
    pub wanted: Option<(u16, u16)>,
    /// The cell size the running prepare was made for, `None` when none is running. A startup
    /// prepare whose cell size was never probed is in flight for `(0, 0)`: a size that always
    /// counts as a change against a real one, but that is still a prepare running, so a resize
    /// does not start a second one beside it.
    pub in_flight: Option<(u16, u16)>,
    /// The image protocol the screen was probed with: every prepare this run makes is for this
    /// protocol, so a re-prepare keeps the protocol the terminal can draw, not a `None` the mark
    /// would die on.
    pub protocol: fbtt_tui::graphics::caps::Protocol,
    /// The cell scale the screen was probed with, kept beside the protocol for the same reason.
    pub scale: u16,
}

impl MarkState {
    /// A state with nothing wanted and nothing in flight, for a screen probed with `protocol`
    /// and `scale`.
    pub fn new(protocol: fbtt_tui::graphics::caps::Protocol, scale: u16) -> Self {
        MarkState {
            wanted: None,
            in_flight: None,
            protocol,
            scale,
        }
    }

    /// A state for the probed `graphics` verdict: its protocol and scale, so every prepare this
    /// run makes - the startup one and the re-prepares a resize or a stale answer starts - is
    /// made for the protocol the terminal can draw and the scale it reports, not for a
    /// `Protocol::None` the mark would die on.
    #[must_use]
    pub fn for_graphics(graphics: &fbtt_tui::graphics::caps::Graphics) -> Self {
        MarkState::new(graphics.protocol, graphics.scale)
    }

    /// The startup prepare is running, for the probed `cell` (or for none known).
    pub fn start(&mut self, cell: Option<(u16, u16)>) {
        self.in_flight = Some(cell.unwrap_or((0, 0)));
    }

    /// What a prepare for `cell` is made of: the probed protocol and scale, and that size.
    fn graphics_for(&self, cell: (u16, u16)) -> fbtt_tui::graphics::caps::Graphics {
        fbtt_tui::graphics::caps::Graphics {
            protocol: self.protocol,
            cell: Some(fbtt_tui::graphics::caps::CellPx {
                w: cell.0,
                h: cell.1,
            }),
            scale: self.scale,
        }
    }

    /// A resize whose cell estimate is `estimate`, with logos on screen made for `logos_cell`.
    /// Returns the graphics to prepare when a prepare is to be started; the state then has it in
    /// flight.
    pub fn resize(
        &mut self,
        logos_cell: Option<(u16, u16)>,
        estimate: (u16, u16),
    ) -> Option<fbtt_tui::graphics::caps::Graphics> {
        match on_resize(logos_cell, self.wanted, self.in_flight, estimate) {
            ResizeAction::Nothing => None,
            ResizeAction::SetWanted { cell, spawn } => {
                self.wanted = Some(cell);
                if spawn {
                    self.in_flight = Some(cell);
                    Some(self.graphics_for(cell))
                } else {
                    None
                }
            }
        }
    }

    /// A terminal resize that answered with a cell size `estimate`, with the screen's `placer`.
    ///
    /// The estimate is compared against the size the screen is currently preparing toward by
    /// [`MarkState::resize`]: within the rounding tolerance nothing moves, and a real change
    /// is remembered in `wanted` and, with no prepare in flight, answered by a fresh prepare
    /// for the probed protocol and scale (which is the graphics to start).
    ///
    /// A resize never invalidates `placer`: the bitmaps it has sent are still the right size
    /// for a screen whose cells did not move, and only a change the frame itself cannot see -
    /// a new cell size - is owed a fresh bitmap. The parameter is the resize's, not the
    /// decision's, so that the rule is where it can be broken and pinned.
    pub fn on_terminal_resize(
        &mut self,
        logos_cell: Option<(u16, u16)>,
        estimate: (u16, u16),
        placer: &mut fbtt_tui::graphics::place::Placer,
    ) -> Option<fbtt_tui::graphics::caps::Graphics> {
        let _ = placer; // a resize never invalidates the placer (see above)
        self.resize(logos_cell, estimate)
    }

    /// The running prepare answered, with logos made for `logos_cell` (`None` when they carry no
    /// cell size). A stale answer starts the prepare for `wanted` and the state has it in flight;
    /// otherwise nothing is in flight any more.
    pub fn answered(&mut self, logos_cell: Option<(u16, u16)>) -> Answer {
        let answered_for = self.in_flight.or(logos_cell).unwrap_or((0, 0));
        match on_prepared(self.wanted, self.in_flight, answered_for) {
            PrepareAction::Swap => {
                self.in_flight = None;
                Answer::Swap
            }
            PrepareAction::Prepare { cell } => {
                self.in_flight = Some(cell);
                Answer::Prepare(self.graphics_for(cell))
            }
        }
    }
}

/// Runs `prepare` on the blocking pool and gives back the receiver its answer arrives on.
fn spawn_prepare(
    graphics: fbtt_tui::graphics::caps::Graphics,
) -> tokio::sync::oneshot::Receiver<PreparedLogos> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result =
            tokio::task::spawn_blocking(move || fbtt_tui::graphics::prepare(graphics)).await;
        if let Ok(logos) = result {
            let _ = tx.send(logos);
        }
    });
    rx
}

/// The startup prepare, already running on the blocking pool as `handle`: remembered as in flight
/// for the probed `cell`, so a resize that arrives before it answers is matched against that size
/// rather than starting a second prepare, and the answer is what the screen was set up to wait for.
fn adopt_startup_prepare(
    state: &mut MarkState,
    handle: Option<tokio::task::JoinHandle<PreparedLogos>>,
    cell: Option<(u16, u16)>,
) -> Option<tokio::sync::oneshot::Receiver<PreparedLogos>> {
    handle.map(|handle| {
        state.start(cell);
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result = handle.await;
            if let Ok(logos) = result {
                let _ = tx.send(logos);
            }
        });
        rx
    })
}

/// The screen over `backend`, through every backend this run switches to, until the person is finished.
///
/// The backend is taken in and handed back because it cannot be moved about inside the loop below, where the
/// channels it owns are borrowed by `select!`. `login` travels with it: what the connection it is running on
/// was signed in as, and what is still owed for it. A switch replaces both together; a quit drops whatever
/// was still owed, because an answer that never signed in is not worth remembering - which is the whole
/// reason it was waited for.
#[allow(clippy::too_many_arguments)]
pub async fn run_session<'a>(
    terminal: &mut DefaultTerminal,
    mut screen: Screen<'a>,
    mut backend: Backend,
    mut login: Option<Login>,
    work: &mut Work<'a>,
    stream: &mut EventStream,
    signal: &mut ShutdownSignal,
    prepare_fut: Option<tokio::task::JoinHandle<PreparedLogos>>,
    startup_cell: Option<(u16, u16)>,
) -> (Quit, Screen<'a>, Backend) {
    // The prepared logos arrive on this oneshot; the screen draws the text title until they do.
    // The startup prepare is remembered as in flight for the cell it was probed for.
    let mut logos_rx = adopt_startup_prepare(&mut screen.marks, prepare_fut, startup_cell);
    loop {
        let (mut parts, task) = backend.into_parts();
        let outcome = run_loop(
            terminal,
            &mut screen,
            &mut parts,
            login.as_mut(),
            work,
            stream,
            signal,
            &mut logos_rx,
        )
        .await;
        // Whatever it answered with, this is still what is running: `Connect` means something else runs
        // *next*, not that this one has stopped.
        backend = Backend::from_parts(parts, task);
        match outcome {
            Outcome::Quit(quit) => return (quit, screen, backend),
            Outcome::Connect(request) => {
                // The `Saving...` screen the switch draws on its way is the same logo path as every
                // other frame: the cells a removed mark used to cover are repainted rather than left
                // as a ghost. The show borrows the screen's mark, so the switch's frame is not the
                // one frame the screen does not clean up after.
                let mut show = |model: &mut Model| {
                    let logos = screen.mark.logos.as_deref();
                    draw_saving(terminal, model, logos, screen.mark.placer)
                };
                let result = switch(
                    work,
                    backend,
                    *request,
                    screen.model,
                    screen.snap,
                    &mut show,
                )
                .await;
                match result {
                    Switched::Now {
                        backend: next,
                        login: next_login,
                        notices,
                    } => {
                        backend = next;
                        login = next_login.map(|login| *login);
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

/// Keys, snapshots, notices and the frame clock, waited on all at once. Nothing here awaits: every branch
/// takes what has arrived and returns, and the only work that talks to another process or a disk is sent away
/// (see [`Work`]), so a keychain that is asleep cannot hold this screen still. The channels arrive as separate
/// parts so that no borrow of one can fight a use of another in the same `select!`.
///
/// Four things read what arrives rather than only drawing it: the first snapshot that says this connection
/// signed in sends off what that sign-in owes (see [`settle`]), a line saying the kept password was refused
/// takes that entry out and asks again (see [`stored_password_refused`]), a request to open something hands
/// it to the media player (see [`play_file`]), and the lines those jobs answer with go on the footer like
/// any other news. The first two are once per connection, and neither touches the backend's channels.
#[allow(clippy::too_many_arguments)]
async fn run_loop(
    terminal: &mut DefaultTerminal,
    screen: &mut Screen<'_>,
    parts: &mut ClientParts,
    mut login: Option<&mut Login>,
    work: &mut Work<'_>,
    stream: &mut EventStream,
    signal: &mut ShutdownSignal,
    logos_rx: &mut Option<tokio::sync::oneshot::Receiver<PreparedLogos>>,
) -> Outcome {
    // The screen's two halves are used apart all through the loop - a key press reads one and writes the
    // other - so they are taken out here, once, rather than reached through `screen` in every branch.
    // The same for the queue's two ends: this loop reads one while the worker it sends jobs to writes the
    // other, and a branch cannot hold the reading end open if an arm body is reaching through `work` at the
    // same time. What a job needs travels as its own handle for the same reason.
    let model = &mut *screen.model;
    let snap = &mut *screen.snap;
    let jobs = work.jobs.clone();
    let rx = &mut work.rx;
    let paths = work.paths;
    let player = work.player;
    let store = Arc::clone(work.store);
    // The runtime this loop's off-screen work reaches back in through: starting a media player is a task,
    // and the job that starts one waits for its one line from the thread it was sent to (see
    // [`play_file`]). This function only ever runs inside one.
    let runtime = Handle::current();
    // Nothing has been drawn yet, so the first tick draws.
    let mut dirty = true;
    let mut last_watch = model.watch();
    // The placements the last frame put on the screen: the reference `before_draw` compares
    // against before a new frame is drawn, so the pixels a frame's placeholders reference are
    // already transmitted when the diff writes them.
    let mut last_placements: Vec<Placement> = Vec::new();
    let mut frame = tokio::time::interval(FRAME_TICK);
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        // The prepared logos answer here, whenever the blocking pool gets done with them. The screen
        // has been drawing the text title until now; from this frame on it draws the mark. A non-blocking
        // check: the oneshot is either ready (the prepare finished) or it is not, and either way we move
        // on to the next turn of the loop.
        if let Some(rx) = logos_rx.as_mut()
            && let Ok(logos) = rx.try_recv()
        {
            // The answer is matched against the latest estimate that counted as a change, not
            // against whatever the window says now: a prepare in flight is worth its own size,
            // and a resize that happened while it was running is already remembered in
            // `wanted`. The decision is `MarkState::answered`, tested without a terminal.
            let logos_cell = logos.graphics.cell.map(|cell| (cell.w, cell.h));
            match screen.marks.answered(logos_cell) {
                Answer::Swap => {
                    *screen.mark.logos = Some(Arc::new(logos));
                    screen.mark.placer.invalidate();
                    dirty = true;
                }
                Answer::Prepare(graphics) => {
                    // The answer is stale: drop it, and prepare the size `wanted` remembers.
                    // The screen keeps the mark it is showing until the fresh prepare answers.
                    *logos_rx = Some(spawn_prepare(graphics));
                }
            }
        }
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
                            Action::Connect(request) => {
                                return Outcome::Connect(Box::new(request))
                            }
                        }
                    }
                    dirty = true;
                }
                Some(Ok(Event::Resize(cols, rows))) => {
                    dirty = true;
                    // A resize may have changed the cell's pixel size: the window's pixel size
                    // divided by the new grid. What that division answers is an estimate, and
                    // the estimate wobbles by a pixel or two as the window is dragged; only a
                    // real change, a font or zoom that moved the cells, is remembered in
                    // `wanted`. The decision, and the rule that a resize never invalidates
                    // the placer by itself, is `MarkState::on_terminal_resize`, tested
                    // without a terminal.
                    if cols > 0
                        && rows > 0
                        && let Ok(ws) = crossterm::terminal::window_size()
                        && ws.width > 0
                        && ws.height > 0
                    {
                        let w = ws.width / cols;
                        let h = ws.height / rows;
                        if w > 0 && h > 0 {
                            let logos_cell = screen
                                .mark
                                .logos
                                .as_ref()
                                .and_then(|logos| logos.graphics.cell)
                                .map(|cell| (cell.w, cell.h));
                            if let Some(graphics) = screen.marks.on_terminal_resize(
                                logos_cell,
                                (w, h),
                                screen.mark.placer,
                            ) {
                                // Nothing was in flight: a prepare for the new size, for the
                                // protocol and scale the screen was probed with.
                                *logos_rx = Some(spawn_prepare(graphics));
                            }
                        }
                    }
                }
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
                    match login.as_deref_mut() {
                        // The first snapshot that says this connection is in is what its password and its
                        // place in `ui.yaml` were waited for. Nobody else can answer that question: this run
                        // did not dial the server, this backend did. What it owes goes off to a thread that may
                        // wait for a keychain; whatever it answers with arrives on the queue below.
                        Some(login) if signed_in(snap) => settle(login, &store, paths, &jobs),
                        _ => {}
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
                        match notice {
                            // A file to play is not a line this screen draws: it is a job. Starting a player
                            // is a conversation with another process, so it goes to this run's worker like
                            // every other piece of off-screen work, and what comes back on the queue below is
                            // the line about what was opened.
                            Notice::Play(target) => {
                                play_file(
                                    &jobs,
                                    runtime.clone(),
                                    player.clone(),
                                    paths.play_dir(),
                                    target,
                                );
                            }
                            other => model.push_notice(other, Instant::now()),
                        }
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
                        // The entry goes, on a thread that may have to wait for the keychain to answer. This
                        // screen has already said what happens next, so it does not also wait for the delete:
                        // whether the old entry came out is not something the person can act on any differently
                        // right now, and a keychain that will not delete anything says so on the queue below.
                        forget(target, &store, &jobs);
                    }
                }
                // The backend stopped and the queue is read out: the screen has all it gets.
                None => return Outcome::Quit(Quit::Ok),
            },
            // Whatever was sent off the screen answers here, whenever its thread gets an answer: a `ui.yaml`
            // that would not write, or a keychain that would not. Nothing waits for any of it - a run that ends
            // first loses a line about work it is no longer doing, which is the deal this queue makes. A closed
            // queue cannot happen while this loop runs: `work` keeps a sender of its own open between jobs, and
            // a queue with a live sender is never closed - so there is nothing to report when nothing arrives.
            done = once(rx.recv()) => if let Some(notice) = done {
                model.push_notice(notice, Instant::now());
                dirty = true;
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
                if let Err(problem) = draw_frame(
                    terminal,
                    model,
                    snap,
                    screen.mark.logos.as_deref(),
                    screen.mark.placer,
                    &mut last_placements,
                ) {
                    return Outcome::Quit(Quit::Failed(problem));
                }
            },
            _ = once(signal.wait.as_mut()) => return Outcome::Quit(Quit::Ok),
        }
        if model.quit_requested() {
            return Outcome::Quit(Quit::Ok);
        }
    }
}

/// Acts on the connect screen's answer: start what was named, then stop what was running. What the answer is
/// worth writing down comes along with it - `ui.yaml` for this library's own engine, and what a server
/// connection still owes in [`Login`] - because nothing about a server is remembered before its sign-in answers.
///
/// The lock is asked about before anything else (see [`plan_switch`]), because a person who cannot have this
/// library should not lose the connection they already have for a switch that could not happen; and the new
/// backend starts before the old one is stopped, because an engine's start is the one thing here that can
/// fail. A `Kept` therefore means nothing was stopped at all.
async fn switch(
    work: &Work<'_>,
    backend: Backend,
    request: ConnectRequest,
    model: &mut Model,
    snap: &mut Arc<Snapshot>,
    show: &mut dyn FnMut(&mut Model),
) -> Switched {
    // The lock check comes first, and only of the switch that needs it: a server connection asks nothing of
    // this library, and a run already driving its own engine holds its lock (asking would only report a
    // holder that is itself).
    let (new_lock, outcome) = match (&request.target, backend.kind()) {
        (ConnectTarget::Local, BackendKind::Remote) => {
            let acquired = LibraryLock::acquire(&work.paths.lock_file());
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
            Switched::Kept(backend, Notice::Error(library_in_use(work.paths)))
        }
        SwitchPlan::LockFailed(problem) => {
            Switched::Kept(backend, Notice::Error(cannot_lock(work.paths, &problem)))
        }
        SwitchPlan::Go => {
            switch_onward(
                work,
                backend,
                request,
                new_lock,
                model,
                snap,
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
#[allow(clippy::too_many_arguments)]
async fn switch_onward<F>(
    work: &Work<'_>,
    backend: Backend,
    request: ConnectRequest,
    new_lock: Option<LibraryLock>,
    model: &mut Model,
    snap: &mut Arc<Snapshot>,
    show: &mut dyn FnMut(&mut Model),
    start_engine: impl FnOnce(Config, Paths, LibraryLock) -> F,
) -> Switched
where
    F: Future<Output = Result<Backend, String>>,
{
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
    // The gateway key comes out of the answer too, and not out of the target: `ui.yaml` keeps only which header
    // a gateway reads, so the value is this answer's to carry (see `crate::secrets`'s gateway account). Taken
    // here, before the answer is pulled apart, so both halves of what a keyed front door needs are in hand
    // before either backend is touched.
    let gateway_key = match &request.target {
        ConnectTarget::Remote(_) => request.secret.gateway_key.clone(),
        ConnectTarget::Local => None,
    };

    let (next, login, mut notices) = match request.target {
        ConnectTarget::Remote(target) => {
            let keep = keep_plans(&target, &request.secret);
            let login = Login::answered(target.clone(), keep);
            // A connection cannot fail to start: an address this run cannot dial says so through the client,
            // which is what the empty table beside the connect screen is for. Nothing is remembered about it
            // yet - not the address, not the password - because nobody has reached it yet.
            (
                Backend::start_remote(target, secret, gateway_key),
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
                    Notice::Error(cannot_lock(work.paths, "the lock went missing")),
                );
            };
            // The config is read again now rather than reused from startup: limits changed during this run
            // are in that file and nowhere else (the engine writes them there), and an engine started on
            // what was in memory would quietly put them back.
            let mut lines = Vec::new();
            let (config, problem) = match local_config(work.paths) {
                Ok(both) => both,
                Err(problem) => return Switched::Kept(backend, Notice::Error(problem)),
            };
            if let Some(problem) = problem {
                lines.push(Notice::Error(problem));
            }
            let next = match start_engine(config, work.paths.clone(), lock).await {
                Ok(next) => next,
                // Nothing was stopped to get here, so nothing has to be put back: this run keeps the
                // connection it had and reads the engine's own reason for refusing on the footer.
                Err(problem) => return Switched::Kept(backend, Notice::Error(problem)),
            };
            // This library's own engine needs nothing proved about it - it is running here, on this library -
            // so `ui.yaml` can say so now, where a connection waits for its first sign-in. The write goes off
            // the screen as every file write in this run does; if the file would not take it, the line saying
            // so arrives on the queue beside the rest of this switch's news.
            let ui = UiState {
                last_used: LastUsed::Local,
            };
            let paths = work.paths.clone();
            send_work(&work.jobs, move || {
                ui.save(&paths)
                    .err()
                    .map(|problem| {
                        vec![Notice::Error(could_not_save(&paths, &problem.to_string()))]
                    })
                    .unwrap_or_default()
            });
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
        login: login.map(Box::new),
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
        /// What this connection's sign-in still owes, boxed: a switch hands one of these back per answer on
        /// the connect screen and the loop passes it straight to the next turn, so `Kept` - a backend and
        /// one line - should not pay for a plan nobody has run yet.
        login: Option<Box<Login>>,
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
        // The file was read and is left where it is: everything it named is kept but the player's, which
        // is at its default, and the line saying why is the one the screen shows.
        Ok(ConfigLoad::PlayerProblem { config, problem }) => Ok((config, Some(problem))),
        Ok(ConfigLoad::Bad { config, problem }) => Ok((config, Some(problem))),
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

/// What the first snapshot that says a connection signed in owes, sent away from the screen rather than
/// waited on: `ui.yaml` pointed at this server - unless this run only borrowed it, which is what
/// [`remembered_ui`] decides - and whatever the person said to keep: the password, the gateway key, or both.
///
/// Both wait for the backend's own answer because until then nobody knows whether this address and this
/// credential are worth remembering: a run that reached nothing should not leave its next run pointed at
/// nothing, and a password a server refused is not a password to keep. A keychain that will not write and a
/// file that will not save come back as lines on the screen's queue; the connection stands either way. Once a
/// connection has been paid for it is never owed again, so the snapshot that arrives every second after it
/// costs nothing.
fn settle(
    login: &mut Login,
    store: &Arc<dyn SecretStore + Send + Sync>,
    paths: &Paths,
    jobs: &Jobs,
) {
    if login.settled {
        return;
    }
    login.settled = true;
    let keep = std::mem::replace(&mut login.keep, KeepPlans::nothing());
    // Decided here, on the screen's thread, because it is a decision and not an effect: which address the next
    // run should start on. What follows is only the writing of it.
    let ui = remembered_ui(login).map(|last_used| UiState { last_used });
    let paths = paths.clone();
    let store = Arc::clone(store);
    send_work(jobs, move || {
        let mut lines = Vec::new();
        if let Some(ui) = ui
            && let Err(problem) = ui.save(&paths)
        {
            lines.push(Notice::Error(could_not_save(&paths, &problem.to_string())));
        }
        // The credential first and the front-door key after it, in one job on one worker: the order they were
        // asked for is the order they happen in, which is what keeps a refused "do not keep this" from being
        // undone by the other half landing late (see [`Jobs`]). A plan that asks for nothing never reaches the
        // keychain, so a plain server costs one call as it always did.
        for plan in [keep.login, keep.gateway_key] {
            if let Err(problem) = apply_keep(&*store, &plan) {
                lines.push(Notice::Error(problem));
            }
        }
        lines
    });
}

/// Takes this server's stored password out of the keychain, on a thread that may have to wait for it.
///
/// The line telling the person what to do about the refusal is already on the footer by the time this is sent,
/// so nothing here waits for the answer. The one thing worth saying back is that the keychain would not delete,
/// which arrives on the queue like any other news: an entry left behind is not fatal, because the next run
/// reads it, is refused the same way, and asks the person again.
fn forget(target: RemoteTarget, store: &Arc<dyn SecretStore + Send + Sync>, jobs: &Jobs) {
    let store = Arc::clone(store);
    send_work(jobs, move || {
        forget_password(&*store, &target)
            .err()
            .map(|problem| vec![Notice::Error(problem)])
            .unwrap_or_default()
    });
}

/// Ask the media player to open one target, and send back the one line about it.
///
/// [`fbtt_core::player`] finds mpv or VLC, writes a private playlist when the file is an address rather
/// than a file, and starts the player without that address ever reaching its command line (decision 3 of
/// the milestone 3 plan). The player config is the one this run loaded at start, not the one a switch
/// re-reads for its limits: one run does not change players halfway through.
///
/// This screen waits for none of it. A job on this run's worker thread is handed to that thread as
/// `'static`, so the job carries the config, the target and a handle to the runtime rather than borrowing
/// them, and `block_on` there drives the spawn: finding a player and writing a file cost about what
/// reading a torrent file from the same queue already costs. The line back is what was opened, or the
/// reason nothing was; a player that starts keeps running after this run ends.
fn play_file(jobs: &Jobs, runtime: Handle, player: PlayerConfig, dir: PathBuf, target: PlayTarget) {
    send_work(jobs, move || {
        let opened = runtime.block_on(player::play(&player, target, &dir, &SystemLookup));
        match opened {
            Ok(line) => vec![Notice::Info(line)],
            Err(problem) => vec![Notice::Error(problem.to_string())],
        }
    });
}

/// Whether one line from a connection says the password this run read out of the keychain was refused.
///
/// The variant is the whole test: the remote backend puts `Notice::SignInRefused` on the queue when it is the
/// credential that the server said no to (see `fbtt_core::remote`), so nothing here has to pick a refusal out
/// of words another failure might use too. What this run still has to decide for itself is which refusals are
/// its business: only a password that came out of the keychain means "what we kept was wrong", which is a thing
/// to take out and to ask again about, while what somebody just typed needs no repair (they are looking at the
/// screen they typed it into), and once this connection has signed in a later refusal is about something else
/// entirely - a token that expired, a server that changed its users.
///
/// Fires once: `from_keychain` goes with the answer, so a server that keeps refusing does not reopen the
/// question every backoff pause.
fn stored_password_refused(login: &mut Login, notice: &Notice) -> bool {
    if !login.from_keychain || login.settled {
        return false;
    }
    let refused = matches!(notice, Notice::SignInRefused(_));
    if refused {
        login.from_keychain = false;
    }
    refused
}

/// The line for a library whose `ui.yaml` would not take what this run learned from it.
fn could_not_save(paths: &Paths, problem: &str) -> String {
    format!("Could not save {}: {problem}", paths.ui_file().display())
}

/// Whether a new cell size estimate is a real change from the current one: a font or zoom change,
/// not the rounding noise of a window pixel size divided by a grid. A dimension that differs by
/// more than 10 percent is a change; within that, the terminal draws the same cell and the
/// bitmaps are still the right size.
fn cell_size_changed(current: (u16, u16), estimated: (u16, u16)) -> bool {
    let (cw, ch) = current;
    let (ew, eh) = estimated;
    if cw == 0 || ch == 0 {
        return true;
    }
    // |a - b| / min(a, b) > 10% means a change: the smaller of the two is the baseline, so a
    // cell that is 10 percent bigger than the current one is a change, and so is one that is 10
    // percent smaller. Using the smaller as the denominator means a cell that is 10 percent of
    // the current one (a tenth of the size) is not a change, which is right: that is rounding.
    // Widened to u32 before the multiplication: a cell estimate a terminal answers can differ
    // from the current one by more than 655 pixels, and a u16 multiplication overflows (a panic
    // in debug, a wrong decision in release).
    let w_diff = u32::from(cw.abs_diff(ew));
    let w_min = u32::from(cw.min(ew));
    let h_diff = u32::from(ch.abs_diff(eh));
    let h_min = u32::from(ch.min(eh));
    (w_min > 0 && w_diff * 100 > w_min * 10) || (h_min > 0 && h_diff * 100 > h_min * 10)
}

/// One frame of the screen, in the order the terminal needs its bytes: the pixels kitty must already
/// hold, the frame's own cells, and then whatever the frame could only decide by being drawn.
///
/// 1. Before the draw, [`Placer::before_draw`] gets the placements of the *previous* frame
///    (`last_placements`): for kitty that sends the small mark, and the large mark when the last
///    frame placed one, so the placeholder cells the diff is about to write find their pixels.
/// 2. `terminal.draw` writes the view's cells; the placements this frame chose come back.
/// 3. After the draw, [`Placer::after_draw`] gets this frame's placements. For kitty it transmits a
///    large mark that is newly chosen or resized in this frame (the help just opened, or a smaller
///    size picked), which was not known before the draw; the terminal draws the placeholders it
///    just received as soon as the pixels arrive, in the same flush. The image protocols write
///    their image here, and the cells a removed placement covered are repainted.
///
/// `last_placements` is replaced with this frame's. An error is the terminal failing, worded for
/// the person.
fn draw_frame<B>(
    terminal: &mut ratatui::Terminal<B>,
    model: &Model,
    snap: &Snapshot,
    logos: Option<&PreparedLogos>,
    placer: &mut Placer,
    last_placements: &mut Vec<Placement>,
) -> Result<(), String>
where
    B: ratatui::backend::Backend + std::io::Write,
    B::Error: std::fmt::Display,
{
    let logos_view = logos
        .map(|logos| LogoView { logos: Some(logos) })
        .unwrap_or(LogoView::text());
    if let Some(logos) = logos {
        let pre_bytes = placer.before_draw(logos, last_placements);
        if !pre_bytes.is_empty() {
            let _ = std::io::Write::write_all(terminal.backend_mut(), &pre_bytes);
        }
    }
    let mut placements = Vec::new();
    let done = terminal
        .draw(|frame| {
            placements = view::draw_with(frame, model, snap, logos_view);
        })
        .map_err(|error| format!("the terminal failed: {error}"))?;
    let area = done.area.into();
    if let Some(logos) = logos {
        // The repaint cells are copied out of the frame's buffer before the terminal is written
        // to, because the buffer is borrowed from it; a frame with nothing to repaint copies
        // nothing.
        let after = placer.after_draw(&placements, area, logos);
        let cells = repaint_cells(done.buffer, &after);
        write_logo_bytes(terminal, &cells, &after.bytes);
    }
    *last_placements = placements;
    Ok(())
}

/// The logo's bytes after a frame is drawn: the cells a removed placement used to cover, repainted
/// from the frame's own buffer, and what [`Placer::after_draw`] answered with. Every frame that
/// leaves a mark behind, and every frame that takes one down - the frame the `Saving...` screen
/// draws is one of those - goes through this, so the screen never has the ghost of a mark its last
/// frame did not draw.
///
/// The bytes are the image the image protocols write over the skipped cells, and for kitty the
/// pixels of a large mark this frame newly chose (see [`draw_frame`]). What kitty needs before the
/// frame is transmitted before it, by [`Placer::before_draw`].
///
/// `repaint_cells` are the cells a removed placement used to cover, copied out of the frame's
/// buffer by the caller, which is where the decision of which cells are owed a repaint lives - the
/// frame's buffer is borrowed from the terminal, so the cells are read out before the terminal is
/// written to. The buffer is never cloned for the frame as a whole: only the cells that are owed a
/// repaint are copied, and a frame with nothing to repaint copies nothing.
fn write_logo_bytes<B>(
    terminal: &mut ratatui::Terminal<B>,
    repaint_cells: &[(u16, u16, ratatui::buffer::Cell)],
    bytes: &[u8],
) where
    B: ratatui::backend::Backend + std::io::Write,
{
    // The image protocols write the image over the skipped cells, and the cells a removed placement
    // used to cover are repainted from the buffer the frame just drew - before the image, so a cell
    // that is text again shows the text and not the ghost of an image.
    if !repaint_cells.is_empty() {
        let backend = terminal.backend_mut();
        let _ = backend.draw(
            repaint_cells
                .iter()
                .map(|(x, y, c)| (*x, *y, c as &ratatui::buffer::Cell)),
        );
    }
    if !bytes.is_empty() {
        let backend = terminal.backend_mut();
        let _ = std::io::Write::write_all(backend, bytes);
    }
    let _ = std::io::Write::flush(terminal.backend_mut());
}

/// The cells a removed placement used to cover, copied out of the frame's buffer: the answer to
/// "which cells are owed a repaint" comes from [`Placer::after_draw`], and the cells themselves are
/// read out of the buffer before the terminal is written to, because the buffer is borrowed from it.
/// A frame with nothing to repaint copies nothing.
fn repaint_cells(
    buffer: &ratatui::buffer::Buffer,
    after: &AfterDraw,
) -> Vec<(u16, u16, ratatui::buffer::Cell)> {
    if after.repaint.is_empty() {
        return Vec::new();
    }
    let mut cells = Vec::new();
    for rect in &after.repaint {
        for y in rect.top()..rect.bottom() {
            for x in rect.left()..rect.right() {
                cells.push((x, y, buffer[(x, y)].clone()));
            }
        }
    }
    cells
}

/// The `Saving...` screen, with the newest notice still on its footer: a run that was
/// told it will not save must not read as one that did.
pub fn draw_saving(
    terminal: &mut DefaultTerminal,
    model: &Model,
    logos: Option<&PreparedLogos>,
    placer: &mut Placer,
) {
    // The saving screen is a frame like any other: the cells it leaves the image covering are
    // repainted over, so the screen it is drawn on is a screen the run's next frame, if it has one,
    // will not have to clean up.
    let done = match terminal.draw(|frame| {
        let area = frame.area();
        if area.is_empty() {
            return;
        }
        frame.render_widget(Fill::new(" ").bg(theme::INK_950), area);
        // A refused sign-in is bad news about the connection like any other error: the same colour and the
        // same words, so a run that was told its password was wrong does not read as one that is quietly saving.
        let line = match model.notice() {
            Some(Notice::Error(text)) | Some(Notice::SignInRefused(text)) => {
                format::safe(text).fg(theme::RED_500)
            }
            Some(Notice::Info(text)) => format::safe(text).fg(theme::INK_300),
            // A file to play is not a line the saving screen shows either: it is a job the run does.
            Some(Notice::Play(_)) => SAVING.fg(theme::INK_300),
            None => SAVING.fg(theme::INK_300),
        };
        let [_, middle, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(line).centered(), middle);
    }) {
        Ok(done) => done,
        Err(_) => return,
    };
    if let Some(logos) = logos {
        let area = done.area.into();
        // The saving screen draws no mark, so the placements are empty: any mark the last frame
        // put on the screen is owed a repaint, and the image protocols write nothing new.
        let after = placer.after_draw(&[], area, logos);
        let cells = repaint_cells(done.buffer, &after);
        write_logo_bytes(terminal, &cells, &after.bytes);
    }
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

/// What a prepare's answer is worth: the state machine behind the mark's resize, in one pure
/// function so the sequences it answers are tested without a terminal, a blocking pool or a
/// window.
///
/// A prepare answers for the cell size it was made for. The screen remembers the latest estimate
/// that counted as a change (`wanted`) and the size the running prepare was made for
/// (`in_flight`): when the answer arrives, it is swapped in when `wanted` is `None` or the answer
/// is within ten percent of `wanted` (the rounding noise of a pixel size divided by a grid is not
/// a change), and discarded for a fresh prepare of `wanted` otherwise. A resize only ever sets
/// `wanted` and starts a prepare when none is in flight: it never invalidates the placer on its
/// own, which is the screen-size rule's to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrepareAction {
    /// The answer is the right size: swap the logos in.
    Swap,
    /// The answer is stale: drop it, and prepare `wanted` instead.
    Prepare { cell: (u16, u16) },
}

/// The decision a prepare's answer leads to: `state` is `(wanted, in_flight)`, and `answered_for`
/// is the cell size the answer was made for. `None` for `in_flight` is the first prepare, asked
/// for before any resize, which is never stale.
pub(crate) fn on_prepared(
    wanted: Option<(u16, u16)>,
    in_flight: Option<(u16, u16)>,
    answered_for: (u16, u16),
) -> PrepareAction {
    let _ = in_flight; // the answer's own size is what it is worth, not what it was asked for
    match wanted {
        None => PrepareAction::Swap,
        // `(0, 0)` is the size of a prepare that had no probed cell: never the size wanted.
        Some(w) if answered_for != (0, 0) && !cell_size_changed(w, answered_for) => {
            PrepareAction::Swap
        }
        Some(w) => PrepareAction::Prepare { cell: w },
    }
}

/// What a resize is worth: the other half of the mark's resize state machine, beside
/// [`on_prepared`]. A resize only ever sets `wanted` and starts a prepare when none is in flight;
/// it never invalidates the placer on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResizeAction {
    /// Nothing changed: the estimate is within ten percent of the reference.
    Nothing,
    /// Set `wanted` to the estimate, and start a prepare for it when none is in flight.
    SetWanted { cell: (u16, u16), spawn: bool },
}

/// The decision a resize leads to. The reference the estimate is compared against is the size the
/// screen is currently preparing toward: `wanted` when a prepare is in flight or `wanted` is set,
/// otherwise the cell the logos on screen were made for, and with neither, the in-flight size.
/// Comparing against `wanted` while a prepare is in flight is what makes a zoom-in-then-zoom-back
/// out count as a change, so the stale answer is discarded and re-prepared for the size the window
/// is back at.
pub(crate) fn on_resize(
    logos_cell: Option<(u16, u16)>,
    wanted: Option<(u16, u16)>,
    in_flight: Option<(u16, u16)>,
    estimate: (u16, u16),
) -> ResizeAction {
    let reference = wanted.or(logos_cell).or(in_flight).unwrap_or((0, 0));
    if !cell_size_changed(reference, estimate) {
        return ResizeAction::Nothing;
    }
    ResizeAction::SetWanted {
        cell: estimate,
        spawn: in_flight.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Login, Mark, PrepareAction, ResizeAction, Screen, Switched, Work, cell_size_changed,
        could_not_save, forget, local_config, on_prepared, on_resize, play_file, remembered_ui,
        settle, stored_password_refused, switch_onward,
    };
    use crate::backend::{Backend, BackendKind};
    use crate::fake_store::FakeStore;
    use crate::secrets::{KeepPlan, KeepPlans, SecretStore};
    use fbtt_core::scratch::ScratchDir;
    use fbtt_core::ui_state::{LastUsed, LoginMethod, RemoteTarget, UiState};
    use fbtt_core::{
        Config, LibraryLock, Notice, Paths, PlaySource, PlayTarget, PlayerConfig, Secret,
    };
    use fbtt_tui::{ConnectRequest, ConnectTarget, Model, SecretInput};
    use std::fs;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tokio::runtime::Handle;
    use tokio::sync::mpsc;
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

    #[test]
    fn a_cell_size_moved_by_rounding_is_not_a_change() {
        // A cell read back from a window whose pixel size wobbled by one: the same cell, which
        // the mark's bitmap is already the right size for.
        assert!(
            !cell_size_changed((16, 34), (17, 34)),
            "one pixel in the width is rounding"
        );
        assert!(
            !cell_size_changed((16, 34), (16, 35)),
            "one pixel in the height is rounding"
        );
        // Two pixels on a double-height cell is still inside ten percent of the baseline.
        assert!(
            !cell_size_changed((8, 16), (8, 17)),
            "one pixel of a 16-pixel cell is rounding"
        );
    }

    #[test]
    fn a_cell_size_moved_by_a_font_change_is_a_change() {
        // A zoom from 16 to 18 points wide is eleven percent: a different font size, a new bitmap.
        assert!(
            cell_size_changed((16, 34), (18, 34)),
            "an eleven-percent move is a change"
        );
        // And the same move in the other direction, a zoom out.
        assert!(
            cell_size_changed((18, 34), (16, 34)),
            "zooming out is a change too"
        );
        // A move that only the height answers.
        assert!(
            cell_size_changed((16, 34), (16, 38)),
            "a taller cell is a change"
        );
        // A new answer after no answer at all is a change, whatever it says.
        assert!(
            cell_size_changed((0, 0), (16, 34)),
            "a first answer is a change"
        );
    }

    #[test]
    fn a_zoom_leads_to_a_swap() {
        // A zoom changes `wanted`; the prepare it starts answers for that size, and the answer
        // is within ten percent of `wanted`, so it is swapped in.
        let wanted = (18, 34);
        assert_eq!(
            on_prepared(Some(wanted), Some(wanted), wanted),
            PrepareAction::Swap
        );
        // The first prepare, asked for before any resize, is never stale: `wanted` is `None`.
        assert_eq!(
            on_prepared(None, Some((16, 34)), (16, 34)),
            PrepareAction::Swap
        );
    }

    #[test]
    fn a_second_zoom_during_a_prepare_leads_to_a_discard_then_a_swap() {
        // A zoom to 18 starts a prepare. A second zoom to 24 (forty percent, a real change)
        // changes `wanted` to 24 while the 18-prepare is in flight. When the 18-prepare
        // answers, it is stale: discard it, and prepare 24 instead.
        let first = (18, 34);
        let second = (24, 34);
        // The first prepare answers for 18, but `wanted` is now 24: a change, so discard.
        assert_eq!(
            on_prepared(Some(second), Some(first), first),
            PrepareAction::Prepare { cell: second }
        );
        // The 24-prepare answers for 24, which is `wanted`: swap it in.
        assert_eq!(
            on_prepared(Some(second), Some(second), second),
            PrepareAction::Swap
        );
    }

    #[test]
    fn rounding_noise_during_a_prepare_still_leads_to_a_swap() {
        // A prepare in flight for 16x34; a resize answers 17x34, which is one pixel, not a
        // change. `wanted` becomes 17x34, and the 16x34-prepare answers for 16x34: within ten
        // percent of `wanted`, so swap it in.
        let in_flight = (16, 34);
        let wanted = (17, 34);
        assert_eq!(
            on_prepared(Some(wanted), Some(in_flight), in_flight),
            PrepareAction::Swap
        );
    }

    #[test]
    fn a_resize_during_the_first_prepare_is_remembered() {
        // No logos yet: the first prepare is in flight, `wanted` is `None`. A resize sets
        // `wanted`. When the first prepare answers, it is matched against `wanted`, not
        // dropped as stale: a resize during the first prepare is not lost.
        let in_flight = (16, 34);
        // A resize to 18 (a real change) sets `wanted`.
        let wanted = (18, 34);
        assert!(
            cell_size_changed(in_flight, wanted),
            "the resize is a real change"
        );
        // The first prepare answers for 16, but `wanted` is 18: a change, so prepare 18.
        assert_eq!(
            on_prepared(Some(wanted), Some(in_flight), in_flight),
            PrepareAction::Prepare { cell: wanted }
        );
        // And a resize that is only rounding does not make the first prepare stale.
        let wanted_noise = (17, 34);
        assert_eq!(
            on_prepared(Some(wanted_noise), Some(in_flight), in_flight),
            PrepareAction::Swap
        );
    }

    #[test]
    fn a_huge_cell_estimate_does_not_overflow_the_change_check() {
        // A terminal that answers a whole-screen pixel size against a tiny grid produces a
        // cell estimate that differs from the current one by more than 655 pixels. The
        // comparison must widen to u32 before multiplying, or it panics in debug and wraps
        // in release.
        assert!(
            cell_size_changed((16, 34), (800, 34)),
            "a cell a hundred times wider is a change"
        );
        assert!(
            cell_size_changed((u16::MAX, u16::MAX), (1, 1)),
            "a cell from the maximum to the minimum is a change"
        );
        // And the other direction: a tiny current cell against a huge estimate.
        assert!(
            cell_size_changed((1, 1), (u16::MAX, u16::MAX)),
            "a tiny cell against a huge estimate is a change"
        );
    }

    #[test]
    fn a_resize_before_the_first_answer_leads_to_a_prepare_for_the_new_size() {
        // The first prepare is in flight for 16x34, `wanted` is `None`. A resize to 18x34
        // (a real change) sets `wanted` to 18 and does not spawn (the first prepare is in
        // flight). When the first prepare answers for 16, it is stale against `wanted` 18:
        // discard it and prepare 18 instead.
        let in_flight = (16, 34);
        let estimate = (18, 34);
        // The resize: no logos yet, `wanted` is `None`, the first prepare is in flight.
        assert_eq!(
            on_resize(None, None, Some(in_flight), estimate),
            ResizeAction::SetWanted {
                cell: estimate,
                spawn: false
            }
        );
        // The first prepare answers for 16, but `wanted` is now 18: a change, so prepare 18.
        let wanted = estimate;
        assert_eq!(
            on_prepared(Some(wanted), Some(in_flight), in_flight),
            PrepareAction::Prepare { cell: wanted }
        );
    }

    #[test]
    fn a_zoom_in_and_back_out_during_a_prepare_leads_to_a_prepare_for_the_original() {
        // Logos at 16x34. A zoom to 18 starts a prepare (18 in flight, `wanted` 18). Before
        // it answers, a zoom back to 16: the reference is `wanted` 18 (a prepare is in
        // flight), so 16 is a change, and `wanted` returns to 16. When the 18-prepare
        // answers, it is stale against `wanted` 16: discard it and prepare 16.
        let logos = (16, 34);
        // Zoom in to 18: a change from the logos, and nothing in flight yet, so spawn.
        assert_eq!(
            on_resize(Some(logos), None, None, (18, 34)),
            ResizeAction::SetWanted {
                cell: (18, 34),
                spawn: true
            }
        );
        // The 18-prepare is now in flight; `wanted` is 18. Zoom back to 16: the reference is
        // `wanted` 18, so 16 is a change, and no spawn (the 18-prepare is in flight).
        assert_eq!(
            on_resize(Some(logos), Some((18, 34)), Some((18, 34)), (16, 34)),
            ResizeAction::SetWanted {
                cell: (16, 34),
                spawn: false
            }
        );
        // The 18-prepare answers for 18, but `wanted` is now 16: a change, so prepare 16.
        assert_eq!(
            on_prepared(Some((16, 34)), Some((18, 34)), (18, 34)),
            PrepareAction::Prepare { cell: (16, 34) }
        );
    }

    #[test]
    fn a_resize_with_no_real_change_before_the_first_answer_changes_nothing() {
        // The first prepare is in flight for 16x34, `wanted` is `None`. A resize that only
        // wobbles by one pixel is not a change: `wanted` stays `None` and nothing spawns.
        let in_flight = (16, 34);
        assert_eq!(
            on_resize(None, None, Some(in_flight), (17, 34)),
            ResizeAction::Nothing
        );
        // And the first prepare answers for 16, with `wanted` still `None`: swap it in.
        assert_eq!(
            on_prepared(None, Some(in_flight), in_flight),
            PrepareAction::Swap
        );
    }

    /// An engine start, in the shape `switch_onward` takes it: boxed, so that a test can hand in a starter
    /// of its own - one that fails on purpose - where production hands in `Backend::start_local`.
    type Starter = Pin<Box<dyn Future<Output = Result<Backend, String>>>>;

    /// An engine start that works, standing in for the real one: only which config it was handed matters to
    /// the tests below, so it hands back a connection rather than an engine.
    fn started(what: RemoteTarget) -> impl FnOnce(Config, Paths, LibraryLock) -> Starter {
        move |_config, _paths, _lock| {
            let backend = Backend::start_remote(what.clone(), Secret::None, None);
            Box::pin(async move { Ok(backend) })
        }
    }

    /// An engine start that refuses, with a line of its own to be reported.
    fn refused(problem: &str) -> impl FnOnce(Config, Paths, LibraryLock) -> Starter {
        let problem = problem.to_string();
        move |_config, _paths, _lock| Box::pin(async move { Err(problem.clone()) })
    }

    /// Gives up the sending end of a work queue (which is what closes it: nothing else holds a sender) and reads
    /// everything the jobs in flight answer before it closes. On a run where nothing went wrong, that is nothing
    /// at all - which is also the point: a test cannot assert about a `ui.yaml` or a keychain until the job that
    /// writes them has finished, and this is what finishing means.
    async fn answered(mut reader: mpsc::Receiver<Notice>) -> Vec<Notice> {
        let mut lines = Vec::new();
        while let Some(notice) = reader.recv().await {
            lines.push(notice);
        }
        lines
    }

    /// A request to open something is answered with the one line about what happened, and the request itself
    /// is never drawn: it is how the screen asks for a player, not news for the footer. The name here carries
    /// a path separator, so it fails before anything is started - which is what makes it testable on a
    /// machine with no mpv and no VLC installed, where starting something real would not be.
    #[tokio::test]
    async fn a_play_request_answers_with_the_line_about_it() {
        let (_dir, paths) = scratch_library("work_play_request");
        let store: Arc<dyn SecretStore + Send + Sync> = Arc::new(FakeStore::new());
        let missing = paths.data_dir.join("no-such-player");
        let player = PlayerConfig {
            program: missing.display().to_string(),
            ..PlayerConfig::default()
        };
        let work = Work::new(&paths, &player, &store);

        play_file(
            &work.jobs,
            Handle::current(),
            player.clone(),
            paths.play_dir(),
            PlayTarget::new("A Film", PlaySource::Path(missing.clone())),
        );

        assert_eq!(
            answered(work.into_reader()).await,
            vec![Notice::Error(format!(
                "The player in config.yaml was not found: {}",
                missing.display()
            ))],
            "a play request answers with the line about what it could not open"
        );
    }

    /// The two writes one sign-in owes, and the delete that can follow them, land in the order they were
    /// asked for - whichever way round they were asked. On a pool of threads a save could arrive after the
    /// delete that was meant to undo it, leaving kept the very password the person had just refused; one
    /// worker means whatever was asked for last is what the keychain ends holding.
    #[tokio::test]
    async fn off_screen_work_runs_in_the_order_it_was_queued() {
        let account = "alice@https://bit.example";
        let server = RemoteTarget {
            url: "https://bit.example".to_string(),
            login: LoginMethod::Password,
            user: Some("alice".to_string()),
            ..RemoteTarget::default()
        };
        let keep = || KeepPlans {
            login: KeepPlan::Keep {
                account: account.to_string(),
                secret: Zeroizing::new("hunter2".to_string()),
            },
            gateway_key: KeepPlan::Nothing,
        };

        // The keep first and the refusal's delete after it: the delete is last, so nothing is kept.
        let (_dir, paths) = scratch_library("work_keep_then_forget");
        let fake = Arc::new(FakeStore::new());
        let store: Arc<dyn SecretStore + Send + Sync> = fake.clone();
        let player = PlayerConfig::default();
        let work = Work::new(&paths, &player, &store);
        let mut login = Login::answered(server.clone(), keep());
        settle(&mut login, work.store, work.paths, &work.jobs);
        forget(server.clone(), work.store, &work.jobs);
        assert!(
            answered(work.into_reader()).await.is_empty(),
            "neither job had anything to report"
        );
        assert_eq!(
            fake.asked(),
            ["set", "delete"],
            "the delete ran after the save it was asked after, not before it"
        );
        assert_eq!(fake.kept(account), None, "the account ends empty");
        assert_eq!(
            UiState::load(&paths).last_used,
            LastUsed::Remote(server.clone()),
            "and the address the same sign-in chose was written down"
        );

        // Asked the other way round - a delete, then a keep - the keep is what stands at the end. What is
        // being tested is the order, not which one wins: nothing may overtake anything.
        let (_dir, paths) = scratch_library("work_forget_then_keep");
        let fake = Arc::new(FakeStore::new());
        fake.put(account, "stale");
        let store: Arc<dyn SecretStore + Send + Sync> = fake.clone();
        let player = PlayerConfig::default();
        let work = Work::new(&paths, &player, &store);
        forget(server.clone(), work.store, &work.jobs);
        let mut login = Login::answered(server.clone(), keep());
        settle(&mut login, work.store, work.paths, &work.jobs);
        assert!(
            answered(work.into_reader()).await.is_empty(),
            "neither job had anything to report"
        );
        assert_eq!(
            fake.asked(),
            ["delete", "set"],
            "the delete ran before the save that came after it"
        );
        assert_eq!(
            fake.kept(account).as_deref(),
            Some("hunter2"),
            "what was asked for last is what the keychain holds"
        );
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
        let player = PlayerConfig::default();
        let work = Work::new(&paths, &player, &store);

        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        let record = move |config: Config, paths: Paths, lock: LibraryLock| {
            recorded
                .lock()
                .expect("no panic inside the lock")
                .push(config.limits.download_kib);
            let backend = Backend::start_remote(nowhere(), Secret::None, None);
            drop((paths, lock));
            Box::pin(async move { Ok(backend) })
        };

        // The connection this run is running on, which the switch below replaces. Its own first snapshot is
        // what the screen carries in, so no test has to invent one.
        let running = Backend::start_remote(nowhere(), Secret::None, None);
        let mut snap = running.initial_snapshot();
        let mut model = Model::new();
        let mut logos: Option<std::sync::Arc<fbtt_tui::graphics::place::PreparedLogos>> = None;
        let mut placer = fbtt_tui::graphics::place::Placer::new(fbtt_tui::graphics::Protocol::None);
        let mut mark = Mark {
            logos: &mut logos,
            placer: &mut placer,
        };
        let screen = Screen {
            model: &mut model,
            snap: &mut snap,
            mark: &mut mark,
            marks: super::MarkState::new(fbtt_tui::graphics::caps::Protocol::None, 1),
        };
        let switched = switch_onward(
            &work,
            running,
            ConnectRequest {
                target: ConnectTarget::Local,
                secret: SecretInput::default(),
            },
            Some(lock),
            screen.model,
            screen.snap,
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
        // An engine on this library needs nothing proved about it, so `ui.yaml` says so - sent off the screen,
        // so the queue is closed and read before anything is asserted about the file. A save that went in says
        // nothing at all.
        assert!(
            answered(work.into_reader()).await.is_empty(),
            "a save that went in had nothing to report"
        );
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
        let player = PlayerConfig::default();
        let work = Work::new(&paths, &player, &store);

        let running = Backend::start_remote(before.clone(), Secret::None, None);
        let mut snap = running.initial_snapshot();
        let mut model = Model::new();
        let mut logos: Option<std::sync::Arc<fbtt_tui::graphics::place::PreparedLogos>> = None;
        let mut placer = fbtt_tui::graphics::place::Placer::new(fbtt_tui::graphics::Protocol::None);
        let mut mark = Mark {
            logos: &mut logos,
            placer: &mut placer,
        };
        let screen = Screen {
            model: &mut model,
            snap: &mut snap,
            mark: &mut mark,
            marks: super::MarkState::new(fbtt_tui::graphics::caps::Protocol::None, 1),
        };
        let switched = switch_onward(
            &work,
            running,
            ConnectRequest {
                target: ConnectTarget::Local,
                secret: SecretInput::default(),
            },
            Some(lock),
            screen.model,
            screen.snap,
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
        // Nothing was written about a switch that did not happen: no work was even sent off the screen...
        assert!(
            answered(work.into_reader()).await.is_empty(),
            "a switch that kept its backend had nothing to send away"
        );
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
        let player = PlayerConfig::default();
        let work = Work::new(&paths, &player, &store);

        let server = RemoteTarget {
            url: "https://bit.example".to_string(),
            login: LoginMethod::Password,
            user: Some("alice".to_string()),
            ..RemoteTarget::default()
        };
        let running = Backend::start_remote(nowhere(), Secret::None, None);
        let mut snap = running.initial_snapshot();
        let mut model = Model::new();
        let mut logos: Option<std::sync::Arc<fbtt_tui::graphics::place::PreparedLogos>> = None;
        let mut placer = fbtt_tui::graphics::place::Placer::new(fbtt_tui::graphics::Protocol::None);
        let mut mark = Mark {
            logos: &mut logos,
            placer: &mut placer,
        };
        let screen = Screen {
            model: &mut model,
            snap: &mut snap,
            mark: &mut mark,
            marks: super::MarkState::new(fbtt_tui::graphics::caps::Protocol::None, 1),
        };
        let switched = switch_onward(
            &work,
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
            screen.model,
            screen.snap,
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

        // The answer arrives, and what was held back is paid for. It is sent away from the screen rather than
        // waited on, so the queue is closed and read before anything is asserted about the file or the keychain:
        // that is how a test knows the writes have finished. A second snapshot - which is what arrives every
        // second after this one - owes nothing, so it sends nothing.
        let mut login = login.expect("a connection comes with its sign-in");
        settle(&mut login, work.store, work.paths, &work.jobs);
        settle(&mut login, work.store, work.paths, &work.jobs);
        assert!(
            answered(work.into_reader()).await.is_empty(),
            "neither write had anything to report"
        );
        assert_eq!(UiState::load(&paths).last_used, LastUsed::Remote(server));
        assert_eq!(
            fake.kept("alice@https://bit.example"),
            Some("hunter2".to_string())
        );
        // Only the first sign-in paid anything: a connection is not written down again on every snapshot.
        assert_eq!(fake.asked(), vec!["set".to_string()]);
    }

    /// A refusal of the password that was kept is worth acting on once, and only when it was this run that
    /// kept it: what somebody just typed needs no repair, and a server nobody can reach makes nothing wrong.
    #[test]
    fn a_refusal_of_the_saved_password_is_only_worth_acting_on_once() {
        let refusal =
            Notice::SignInRefused("sign-in was refused: the password was wrong".to_string());
        let mut saved = Login::remembered(nowhere(), true);
        assert!(stored_password_refused(&mut saved, &refusal));
        assert!(
            !stored_password_refused(&mut saved, &refusal),
            "a server that keeps refusing does not get to ask the person twice"
        );

        let mut typed = Login::answered(nowhere(), KeepPlans::nothing());
        assert!(
            !stored_password_refused(&mut typed, &refusal),
            "what somebody just typed is not the keychain's mistake"
        );

        // Which *kind* of notice it was is the signal, not which words it used: a line that says the same thing
        // about something else (the event stream giving up on this credential, say) is drawn and left alone.
        let mut saved = Login::remembered(nowhere(), true);
        assert!(
            !stored_password_refused(
                &mut saved,
                &Notice::Error("sign-in was refused: the password was wrong".to_string())
            ),
            "an error that happens to use those words is not this run's business"
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

    /// Which address the next run starts on is decided from the connection alone: a server somebody chose goes
    /// into `ui.yaml`, and the hand-off to a server running on this library does not. That one exists only while
    /// the other run that offered it is up, so pointing the next run at it would have it dial an address nobody
    /// answers while its own engine sat unused - and the offer about that server is something only a start on
    /// this library makes.
    #[test]
    fn a_borrowed_server_is_not_the_one_the_next_run_starts_on() {
        let server = RemoteTarget {
            url: "https://bit.example".to_string(),
            ..RemoteTarget::default()
        };
        assert_eq!(
            remembered_ui(&Login::remembered(server.clone(), false)),
            Some(LastUsed::Remote(server.clone())),
            "a previous run chose this address, so the next one starts on it too"
        );
        assert_eq!(
            remembered_ui(&Login::answered(server.clone(), KeepPlans::nothing())),
            Some(LastUsed::Remote(server)),
            "somebody typed this address into the connect screen"
        );
        assert_eq!(
            remembered_ui(&Login::borrowed(nowhere())),
            None,
            "the hand-off is not written down: `ui.yaml` stays where it was"
        );
    }

    /// A run that took the hand-off leaves `ui.yaml` exactly as it found it, even after its connection signs in:
    /// still this library. That is what makes the next run drive its own engine, and be offered the server again
    /// if the other run is still up - which a start pointed at somebody else's address would not be.
    #[tokio::test]
    async fn a_borrowed_connection_leaves_ui_yaml_where_it_found_it() {
        let (_dir, paths) = scratch_library("switch_borrowed_server");
        fs::write(paths.config_file(), "listen_port: 6881\n").expect("a config");
        UiState {
            last_used: LastUsed::Local,
        }
        .save(&paths)
        .expect("a ui.yaml pointing at this library");

        let fake = Arc::new(FakeStore::new());
        let store: Arc<dyn SecretStore + Send + Sync> = fake.clone();
        let player = PlayerConfig::default();
        let work = Work::new(&paths, &player, &store);
        let borrowed = RemoteTarget {
            url: "http://127.0.0.1:8090".to_string(),
            ..RemoteTarget::default()
        };

        // The sign-in answer arrives for a connection nobody chose and nobody typed a password into.
        let mut login = Login::borrowed(borrowed);
        settle(&mut login, work.store, work.paths, &work.jobs);
        assert!(
            answered(work.into_reader()).await.is_empty(),
            "nothing went wrong: there was nothing to report"
        );

        assert_eq!(
            UiState::load(&paths).last_used,
            LastUsed::Local,
            "the next run drives this library and is offered the server again"
        );
        assert!(
            fake.asked().is_empty(),
            "a borrowed server has no password of ours to keep"
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
    // ---- the frame's byte order, driven through the function the loop calls ----

    use super::{Answer, MarkState, adopt_startup_prepare, draw_frame};
    use fbtt_tui::graphics::caps::{CellPx, Graphics, Protocol};
    use fbtt_tui::graphics::kitty;
    use fbtt_tui::graphics::place::{LogoSize, Placement, Placer, PreparedLogos};
    use ratatui::backend::{Backend as RatatuiBackend, ClearType, CrosstermBackend, WindowSize};
    use ratatui::layout::{Position, Rect, Size};
    use ratatui::{Terminal, TerminalOptions, Viewport};

    /// A writer that keeps every byte it is handed, in order.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Recorder {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("the recording").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The crossterm backend over a recording writer, with every question a terminal would be asked
    /// answered from a fixed size instead: crossterm's own `size()`, `window_size()` and
    /// `get_cursor_position()` read the real tty, which a test never does (and which fails with
    /// `WouldBlock` where there is none). Everything that writes goes to the inner backend, so the
    /// recording holds the exact bytes the real backend would send.
    // The size is fixed at construction, so after `terminal.resize` a clear sees a stale size; that
    // only affects the cleared bytes, which the tests discard.
    struct FixedBackend {
        inner: CrosstermBackend<Recorder>,
        size: Size,
    }

    impl std::io::Write for FixedBackend {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            std::io::Write::write(&mut self.inner, buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            std::io::Write::flush(&mut self.inner)
        }
    }

    impl RatatuiBackend for FixedBackend {
        type Error = std::io::Error;

        fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
        {
            self.inner.draw(content)
        }
        fn hide_cursor(&mut self) -> std::io::Result<()> {
            self.inner.hide_cursor()
        }
        fn show_cursor(&mut self) -> std::io::Result<()> {
            self.inner.show_cursor()
        }
        fn get_cursor_position(&mut self) -> std::io::Result<Position> {
            Ok(Position::new(0, 0))
        }
        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
            self.inner.set_cursor_position(position)
        }
        fn clear(&mut self) -> std::io::Result<()> {
            self.inner.clear()
        }
        fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
            self.inner.clear_region(clear_type)
        }
        fn append_lines(&mut self, n: u16) -> std::io::Result<()> {
            self.inner.append_lines(n)
        }
        fn size(&self) -> std::io::Result<Size> {
            Ok(self.size)
        }
        fn window_size(&mut self) -> std::io::Result<WindowSize> {
            Ok(WindowSize {
                columns_rows: self.size,
                pixels: Size::new(0, 0),
            })
        }
        fn flush(&mut self) -> std::io::Result<()> {
            RatatuiBackend::flush(&mut self.inner)
        }
    }

    /// A terminal of `height` rows over a recording writer, with a fixed viewport and a backend
    /// that answers its own size and cursor questions, so nothing on the test path asks a real tty.
    /// The recording is read through the returned handle.
    fn recording_terminal(height: u16) -> (Terminal<FixedBackend>, Recorder) {
        let recorder = Recorder::default();
        let backend = FixedBackend {
            inner: CrosstermBackend::new(recorder.clone()),
            size: Size::new(100, height),
        };
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 100, height)),
            },
        )
        .expect("a recording terminal");
        (terminal, recorder)
    }

    /// Every byte written since the last call.
    fn take(recorder: &Recorder) -> Vec<u8> {
        std::mem::take(&mut *recorder.0.lock().expect("the recording"))
    }

    /// The offset of the first occurrence of `needle` in `haystack`.
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn snapshot() -> fbtt_core::Snapshot {
        fbtt_core::Snapshot {
            connection: fbtt_core::Connection::Local,
            torrents: Vec::new(),
            totals: fbtt_core::Totals {
                down_rate: 0,
                up_rate: 0,
                dht_nodes: None,
            },
            limits: fbtt_core::LiveLimits {
                download_kib: 0,
                upload_kib: 0,
                active_downloads: None,
                active_seeds: None,
                active_total: None,
            },
            detail: None,
            default_dir: std::path::PathBuf::new(),
            dirs: None,
            devices: None,
        }
    }

    fn kitty_logos() -> PreparedLogos {
        fbtt_tui::graphics::prepare(Graphics {
            protocol: Protocol::Kitty,
            cell: Some(CellPx { w: 8, h: 16 }),
            scale: 1,
        })
    }

    /// The first byte of a kitty placeholder cell: U+10EEEE, which nothing else on the screen is.
    fn placeholder_bytes() -> Vec<u8> {
        let first = kitty::placeholder(0, 0)
            .chars()
            .next()
            .expect("a placeholder has a character");
        first.to_string().into_bytes()
    }

    /// The transmit's own opening, which carries the id: how the bytes of one image are told from
    /// another's.
    fn transmit_of(id: u32) -> Vec<u8> {
        format!("\x1b_Ga=t,q=2,f=32,i={id},").into_bytes()
    }

    /// The first frame, and the first frame after `invalidate()` (on a cleared screen, as the diff
    /// then rewrites every cell): the pixels of the mark the placeholders reference are in the
    /// bytes before the first placeholder cell, because that is the order a terminal needs.
    #[test]
    fn the_frame_transmits_kitty_pixels_before_its_first_placeholder() {
        let logos = kitty_logos();
        let snap = snapshot();
        let model = Model::new();
        let mut placer = Placer::new(Protocol::Kitty);
        let mut last: Vec<Placement> = Vec::new();
        let (mut terminal, recorder) = recording_terminal(40);

        for frame in ["the first frame", "the first frame after invalidate()"] {
            if frame != "the first frame" {
                placer.invalidate();
                terminal
                    .resize(Rect::new(0, 0, 100, 40))
                    .expect("a cleared screen");
            }
            take(&recorder);
            draw_frame(
                &mut terminal,
                &model,
                &snap,
                Some(&logos),
                &mut placer,
                &mut last,
            )
            .expect("a drawn frame");
            let bytes = take(&recorder);
            let transmit = find(&bytes, &transmit_of(kitty::SMALL_ID))
                .unwrap_or_else(|| panic!("{frame}: the small mark is transmitted"));
            let placeholder = find(&bytes, &placeholder_bytes())
                .unwrap_or_else(|| panic!("{frame}: the header's placeholders are written"));
            assert!(
                transmit < placeholder,
                "{frame}: the transmit (at {transmit}) comes before the first placeholder cell \
                 (at {placeholder})"
            );
        }
    }

    /// A large mark the frame chooses itself - the help just opened, then a smaller size on a
    /// shorter screen - is transmitted in that same frame, after the draw, and a frame that places
    /// the same size again sends nothing.
    #[test]
    fn a_newly_chosen_large_mark_is_transmitted_in_the_same_frame() {
        let logos = kitty_logos();
        let snap = snapshot();
        let mut model = Model::new();
        let mut placer = Placer::new(Protocol::Kitty);
        let mut last: Vec<Placement> = Vec::new();
        let (mut terminal, recorder) = recording_terminal(40);
        let rows = |placements: &[Placement]| {
            placements
                .iter()
                .find_map(|placement| match placement.size {
                    LogoSize::Large(rows) => Some(rows),
                    LogoSize::Small => None,
                })
        };

        // A frame without help: the large mark is nowhere.
        draw_frame(
            &mut terminal,
            &model,
            &snap,
            Some(&logos),
            &mut placer,
            &mut last,
        )
        .expect("a drawn frame");
        assert_eq!(rows(&last), None);
        let before = take(&recorder);
        assert!(find(&before, &transmit_of(kitty::LARGE_ID)).is_none());

        // The help opens: its mark is placed by this frame, so it is transmitted by this frame,
        // and the transmit follows the draw (the placeholders it references are already out).
        model.modal = Some(fbtt_tui::Modal::Help);
        draw_frame(
            &mut terminal,
            &model,
            &snap,
            Some(&logos),
            &mut placer,
            &mut last,
        )
        .expect("a drawn frame");
        let tall = rows(&last).expect("the help places its mark on a 40-row screen");
        let bytes = take(&recorder);
        let transmit = find(&bytes, &transmit_of(kitty::LARGE_ID))
            .expect("the help's mark is transmitted in the frame that places it");
        let placeholder = find(&bytes, &placeholder_bytes()).expect("the placeholders are written");
        assert!(
            placeholder < transmit,
            "a newly chosen mark's transmit follows the draw, as it could not be known before it"
        );

        // The same help again: nothing is sent twice.
        draw_frame(
            &mut terminal,
            &model,
            &snap,
            Some(&logos),
            &mut placer,
            &mut last,
        )
        .expect("a drawn frame");
        let steady = take(&recorder);
        assert!(
            find(&steady, b"\x1b_G").is_none(),
            "a steady frame transmits nothing"
        );

        // A shorter screen makes the help pick a smaller size: its pixels are transmitted in the
        // frame that picks it, the bitmap of that size and not the taller one's.
        let mut found = None;
        for height in (10..40).rev() {
            terminal
                .resize(Rect::new(0, 0, 100, height))
                .expect("a resized screen");
            take(&recorder);
            draw_frame(
                &mut terminal,
                &model,
                &snap,
                Some(&logos),
                &mut placer,
                &mut last,
            )
            .expect("a drawn frame");
            let bytes = take(&recorder);
            if let Some(small) = rows(&last)
                && small != tall
            {
                found = Some((small, bytes));
                break;
            }
        }
        let (small, bytes) = found.expect("a shorter screen picks a smaller mark");
        let prepared = logos
            .large
            .iter()
            .find(|prepared| prepared.dims.rows == small)
            .expect("the mark of the chosen size");
        assert!(
            find(&bytes, &prepared.payload).is_some(),
            "the {small}-row mark's own pixels go out in the frame that picks it"
        );
    }

    /// The startup prepare is remembered as in flight for the probed cell, so a resize before it
    /// answers does not start a second prepare; and when it does start one later, it keeps the
    /// probed protocol and scale.
    #[tokio::test]
    async fn the_startup_prepare_is_in_flight_and_a_re_prepare_keeps_the_protocol() {
        let mut state = MarkState::new(Protocol::Kitty, 2);
        let handle = tokio::task::spawn_blocking(|| fbtt_tui::graphics::prepare(Graphics::none()));
        let rx = adopt_startup_prepare(&mut state, Some(handle), Some((16, 34)));
        assert!(rx.is_some(), "the startup prepare has a receiver");
        assert_eq!(
            state.in_flight,
            Some((16, 34)),
            "the startup prepare is in flight for the probed cell"
        );

        // A real zoom before it answers: remembered, and no second prepare beside the first.
        assert_eq!(state.resize(None, (24, 34)), None);
        assert_eq!(state.wanted, Some((24, 34)));
        // The answer (for 16x34) is stale against what was wanted: prepare 24x34, for the
        // protocol and scale the screen was probed with.
        match state.answered(None) {
            Answer::Prepare(graphics) => {
                assert_eq!(graphics.protocol, Protocol::Kitty);
                assert_eq!(graphics.scale, 2);
                assert_eq!(graphics.cell, Some(CellPx { w: 24, h: 34 }));
            }
            Answer::Swap => panic!("a stale answer is not swapped in"),
        }
        assert_eq!(state.in_flight, Some((24, 34)));
        // And its answer is the one that is swapped in.
        assert_eq!(state.answered(None), Answer::Swap);
        assert_eq!(state.in_flight, None);

        // With nothing in flight, a resize starts a prepare that keeps the protocol too.
        let graphics = state
            .resize(Some((24, 34)), (16, 34))
            .expect("nothing in flight: a prepare starts");
        assert_eq!((graphics.protocol, graphics.scale), (Protocol::Kitty, 2));
    }

    /// With no probed cell size, the startup prepare is still a prepare running: the first resize
    /// remembers the size it wants and does not start a second prepare beside the first.
    #[tokio::test]
    async fn a_first_resize_without_a_probed_cell_does_not_restart_the_prepare() {
        let mut state = MarkState::new(Protocol::Sixel, 1);
        let handle = tokio::task::spawn_blocking(|| fbtt_tui::graphics::prepare(Graphics::none()));
        let _rx = adopt_startup_prepare(&mut state, Some(handle), None);
        assert!(state.in_flight.is_some(), "a startup prepare is in flight");
        assert_eq!(
            state.resize(None, (16, 34)),
            None,
            "the first resize does not start a second prepare"
        );
        assert_eq!(
            state.wanted,
            Some((16, 34)),
            "but it remembers the size wanted"
        );
        // The startup prepare had no cell size to be made for, so its answer is never the wanted one.
        assert!(matches!(state.answered(None), Answer::Prepare(_)));
    }

    /// The probed verdict is what the state machine is made from: the protocol and scale the
    /// terminal answered with, so a re-prepare a resize or a stale answer starts is made for
    /// the protocol the terminal can draw, not for a `Protocol::None` the mark would die on.
    #[test]
    fn the_mark_state_is_made_from_the_probed_graphics() {
        let graphics = Graphics {
            protocol: Protocol::Kitty,
            cell: Some(CellPx { w: 8, h: 16 }),
            scale: 2,
        };
        let mut state = MarkState::for_graphics(&graphics);
        assert_eq!(state.protocol, Protocol::Kitty, "the probed protocol");
        assert_eq!(state.scale, 2, "the probed scale");

        // A real zoom with nothing in flight starts a re-prepare, and the prepare is made for
        // the protocol and scale the screen was probed with.
        let reprepare = state
            .resize(Some((8, 16)), (16, 34))
            .expect("nothing in flight: a prepare starts");
        assert_eq!(reprepare.protocol, Protocol::Kitty);
        assert_eq!(reprepare.scale, 2);
        assert_eq!(reprepare.cell, Some(CellPx { w: 16, h: 34 }));
    }

    /// The help screen's mark at `rows` of `logos`, centred in a 100-column box: the shape the
    /// view's `help_placement` builds, so a test that drives the placer with it is driving it
    /// with a placement the view actually makes.
    fn large_placement(logos: &PreparedLogos, rows: u16) -> Placement {
        let prepared = logos
            .large
            .iter()
            .find(|prepared| prepared.dims.rows == rows)
            .expect("the help mark at these rows")
            .clone();
        let x = 100u16.saturating_sub(prepared.dims.cols) / 2;
        Placement {
            size: LogoSize::Large(rows),
            rect: Rect::new(x, 4, prepared.dims.cols, rows),
            dims: prepared.dims,
            prepared: Some(std::sync::Arc::new(prepared)),
        }
    }

    /// A resize whose cell estimate is within the rounding tolerance changes nothing on the
    /// mark: no prepare starts, and the placer keeps the bitmaps it sent, so the frame after
    /// the resize sends no pixels at all. Only a new cell size - which invalidates the placer,
    /// not a resize - is owed a fresh bitmap.
    #[test]
    fn a_resize_without_a_cell_change_keeps_the_sent_marks() {
        let logos = kitty_logos(); // 8 x 16 cells
        let mut marks = MarkState::new(Protocol::Kitty, 1);
        let mut placer = Placer::new(Protocol::Kitty);

        // A frame with the help open at the ten-row mark: the small and the large bitmaps are
        // on their way, and the placer remembers both as sent for the 8 x 16 cell.
        let small_payload = logos
            .small
            .as_ref()
            .expect("the header's mark")
            .payload
            .as_ref();
        let ten = large_placement(&logos, 10);
        assert!(
            !placer
                .before_draw(&logos, std::slice::from_ref(&ten))
                .is_empty()
        );
        assert_eq!(
            placer.sent(),
            (Some(small_payload), Some(10)),
            "the small and the ten-row mark are sent"
        );

        // A window drag that only moves the pixels by a pixel or two: the estimate is within
        // ten percent of the cell the logos were made for, so nothing is owed, and the placer
        // keeps the bitmaps it sent.
        let graphics = marks.on_terminal_resize(Some((8, 16)), (8, 17), &mut placer);
        assert!(
            graphics.is_none(),
            "a one-pixel move is rounding, not a new prepare"
        );
        assert_eq!(
            marks.resize(Some((8, 16)), (8, 16)),
            None,
            "an unchanged estimate is no change at all"
        );
        assert_eq!(
            placer.sent(),
            (Some(small_payload), Some(10)),
            "the resize invalidated the placer's sent marks"
        );
    }
}
