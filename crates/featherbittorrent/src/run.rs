//! The binary's own work: bring one library up, run its screen, put everything back.
//!
//! Setup happens in order - paths, config, lock, backend - because every step answers
//! with its own line before the terminal is touched, and a second instance should hear
//! "close the other one" rather than see a blank screen blink. The loop then waits on
//! four things at once (keys, snapshots, notices, the frame clock), never on a backend
//! reply: it draws the newest snapshot when something changed, at most thirty frames a
//! second, and quitting is a command out and a join, not a conversation.

use std::future::Future;
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
use tokio::task::JoinHandle;

use fbtt_core::{
    Client, Command, Config, ConfigLoad, LibraryLock, LocalBackend, LockError, Notice, Paths,
    Snapshot,
};
use fbtt_tui::{Model, format, theme, view};

use crate::terminal::Terminal;

/// The frame clock: redraws land at most this far apart, and only over a `dirty` flag.
const FRAME_TICK: Duration = Duration::from_millis(33);
/// What the screen says while the last save runs. A run that was told it will not save
/// shows its notice here instead, so a quit never promises more than the engine kept.
const SAVING: &str = "Saving...";

/// One library's whole run: setup, the loop, the quit. The exit code is the answer.
pub async fn run() -> ExitCode {
    let paths = match Paths::resolve() {
        Ok(paths) => paths,
        Err(error) => return fail(&error),
    };

    // A missing config is written with this library's defaults; a broken one came back
    // from `Config::load` as the defaults plus the line saying what was wrong (the file
    // itself is kept aside under `.bad`), and that line becomes the first notice.
    let mut first_notice = None;
    let config = match Config::load(&paths.config_file()) {
        Ok(ConfigLoad::Loaded(config)) => config,
        Ok(ConfigLoad::Missing) => {
            let config = Config::default_for(&paths);
            if let Err(error) = config.save(&paths.config_file()) {
                return fail(&error);
            }
            config
        }
        Ok(ConfigLoad::Bad { config, problem }) => {
            first_notice = Some(Notice::Error(problem));
            config
        }
        Err(error) => return fail(&error),
    };

    // Before the terminal: a held lock is a sentence to read, not a screen to see.
    let lock = match LibraryLock::acquire(&paths.lock_file()) {
        Ok(lock) => lock,
        Err(LockError::Held) => {
            eprintln!(
                "Another featherbittorrent is already using this library ({}). Close it first.",
                paths.data_dir.display()
            );
            return ExitCode::from(3);
        }
        Err(LockError::Io(error)) => {
            eprintln!(
                "Could not lock the library ({}): {error}",
                paths.data_dir.display()
            );
            return ExitCode::FAILURE;
        }
    };

    let (mut client, task) = match LocalBackend::start(config, &paths, lock).await {
        Ok(started) => started,
        Err(error) => return fail(&error),
    };

    let mut terminal = match Terminal::setup() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("Could not set up the terminal: {error}");
            // Dropping `client` closes the queue, which the backend answers with its
            // save; waiting for it keeps a second start off a half-written file.
            let (code, problem) = await_backend(task).await;
            if let Some(problem) = problem {
                eprintln!("{problem}");
            }
            return code;
        }
    };

    let mut model = Model::new();
    let mut snap = client.snapshot();
    model.sync_selection(&snap);
    if let Some(notice) = first_notice {
        model.push_notice(notice, Instant::now());
    }

    let quit = {
        // The change-wait leaves `client` by value (see `Client::take_changed`): a
        // borrow of it could not sit in the loop beside the sends and the notice read.
        let mut watch_rx = client.take_changed();
        let mut stream = EventStream::new();
        // Both drop at the end of this block: the event reader lets go of stdin before
        // the last save.
        run_loop(
            terminal.get_mut(),
            &mut model,
            &mut snap,
            &mut client,
            &mut stream,
            &mut watch_rx,
        )
        .await
    };

    // The backend is told once: by this command or, if the queue was full, by the
    // client going out of scope below. No timeout - resume data is worth the wait.
    client.send(Command::Shutdown);
    draw_saving(terminal.get_mut(), &model);
    drain_notices(&mut model, &mut client, |model| {
        draw_saving(terminal.get_mut(), model)
    });
    drop(client);

    let (code, problem) = await_backend(task).await;
    drop(terminal); // The terminal goes back before anything is printed to it.
    if let Some(problem) = problem {
        eprintln!("{problem}");
    }
    if quit == Quit::Failed {
        return ExitCode::FAILURE;
    }
    code
}

/// Why the loop ended. `Failed` is a failure of this process (a draw that could not be
/// written); a backend that could not save reports itself through the join instead.
#[derive(PartialEq, Eq)]
enum Quit {
    Ok,
    Failed,
}

/// Keys, snapshots, notices and the frame clock, waited on all at once. Nothing here
/// awaits a backend reply: each branch takes what has arrived and returns.
async fn run_loop(
    terminal: &mut DefaultTerminal,
    model: &mut Model,
    snap: &mut Arc<Snapshot>,
    client: &mut Client,
    stream: &mut EventStream,
    watch_rx: &mut tokio::sync::watch::Receiver<Arc<Snapshot>>,
) -> Quit {
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
                    for command in model.handle_key(key, snap) {
                        client.send(command);
                    }
                    dirty = true;
                }
                Some(Ok(Event::Resize(_, _))) => dirty = true,
                // A closed event stream is the terminal going away: quit like any other.
                Some(Err(_)) | None => return Quit::Ok,
                Some(Ok(_)) => {}
            },
            // The change-wait taken from `client` before the loop: it resolves when the
            // backend publishes, and taking the snapshot after it is a clone of an
            // `Arc`, never a wait. (`select!` polls a watch receiver directly; a borrow
            // of `client` in this branch would fight the sends in the other handlers.)
            wake = watch_rx.changed() => match wake {
                Ok(()) => {
                    *snap = client.snapshot();
                    model.sync_selection(snap);
                    dirty = true;
                }
                // The backend is gone; nothing more will change on this screen.
                Err(_) => return Quit::Ok,
            },
            notice = once(client.notice()) => match notice {
                Some(notice) => {
                    model.push_notice(notice, Instant::now());
                    dirty = true;
                    drain_notices(model, client, |_| {});
                }
                // The backend stopped and the queue is read out: the screen has all it gets.
                None => return Quit::Ok,
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
                    client.send(Command::Watch(watch.clone()));
                    last_watch = watch;
                }
                if let Err(error) = terminal.draw(|frame| view::draw(frame, model, snap)) {
                    eprintln!("the terminal failed: {error}");
                    return Quit::Failed;
                }
            },
            _ = once(signal()) => return Quit::Ok,
        }
        if model.quit_requested() {
            return Quit::Ok;
        }
    }
}

/// The one shutdown signal this build listens for that is not a key: SIGTERM on Unix,
/// Ctrl-C elsewhere, where raw mode does not swallow it. Both quit without asking -
/// a signal is not a person who can answer a confirmation. On Unix Ctrl-C arrives as a
/// key press instead, and the model's own map answers it.
async fn signal() {
    #[cfg(unix)]
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut term) => {
            term.recv().await;
        }
        // A system that will not hand us a SIGTERM listener runs without that signal.
        Err(_) => std::future::pending::<()>().await,
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
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
fn drain_notices(model: &mut Model, client: &mut Client, mut show: impl FnMut(&mut Model)) {
    while let Some(notice) = client.try_notice() {
        model.push_notice(notice, Instant::now());
        show(model);
    }
}

/// The backend's answer, joined with no timeout. Its line - a final save that did not
/// happen - is returned rather than printed here, so the caller can put the terminal
/// back first: stderr after restore is readable, stderr onto the alternate screen is not.
async fn await_backend(task: JoinHandle<Result<(), String>>) -> (ExitCode, Option<String>) {
    let joined = task.await;
    match joined {
        Ok(Ok(())) => (ExitCode::SUCCESS, None),
        Ok(Err(problem)) => (ExitCode::FAILURE, Some(problem)),
        Err(error) => (
            ExitCode::FAILURE,
            Some(format!("the engine task stopped before saving: {error}")),
        ),
    }
}

/// A line for a setup failure, printed before any terminal exists.
fn fail(error: &dyn std::error::Error) -> ExitCode {
    eprintln!("{error}");
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
