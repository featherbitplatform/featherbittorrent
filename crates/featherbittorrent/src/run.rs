//! The binary's own work: bring one library up, hand the screen over, put everything back.
//!
//! Setup happens in order - paths, config, which backend to start on, lock, terminal, backend - because
//! every step answers with its own line before the terminal is touched, and a second instance should hear
//! "close the other one" (or be offered the server running on that library) rather than see a blank screen
//! blink. The screen goes up before the engine does: a run that cannot show anything has no reason to start
//! an engine, and no resume data to write for a library nobody showed. From there the screen is
//! `switch.rs`'s: it owns the loop, and every backend this run moves to after the first one. What is left
//! here is the part that cannot happen twice - choosing what to start on, and putting the last backend down.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use crossterm::event::EventStream;

use fbtt_core::ui_state::{LastUsed, RemoteTarget, UiState};
use fbtt_core::{Config, ConfigLoad, LibraryLock, LogoSetting, Notice, Paths};
use fbtt_tui::{Modal, Model};

use crate::backend::{Backend, LockOutcome, Start, initial_target, named_server};
use crate::secrets::{Keychain, SecretStore, StartSecret, start_secret_for};
use crate::switch::{Login, Quit, ShutdownSignal, Work, draw_saving, run_session, start_watching};
use crate::terminal::Terminal;

/// The offer made to a run whose library another featherbittorrent has taken while running a server on it.
/// One line, answered with one letter, asked before this run takes the screen out from under that one.
const OFFERS_LOCAL_SERVER: &str =
    "A featherbittorrent server is running on this library. Connect to it? y/n";

/// One library's whole run: setup, the screen, the last stop. The exit code is the answer.
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
        // The file was read and is left where it is: the run keeps everything it named but the
        // player's, which comes back at its default, and the line saying why is the first notice.
        Ok(ConfigLoad::PlayerProblem { config, problem }) => {
            first_notice = Some(Notice::Error(problem));
            config
        }
        Ok(ConfigLoad::Bad { config, problem }) => {
            first_notice = Some(Notice::Error(problem));
            config
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

    // The logo's capability probe (see `crate::probe`): one write of the queries and the answers that come
    // back, asked here because it owns stdin until it is done and `EventStream` cannot share it. The logo
    // setting is read from the config before it moves into `Opening`.
    let logo_off = config.logo == LogoSetting::Off;
    let graphics = crate::probe::detect(logo_off);

    // Before the terminal: a library somebody else is driving is a sentence to read, not a screen to see,
    // and so is the offer about the server running on it - that other run may be drawing on this terminal
    // right now. A keychain is consulted here too, for the same reason the engine is started after the
    // screen: nothing a person has to read should happen behind an alternate screen they cannot see yet.
    let opening = match initial_target(&ui, outcome, named_server(&paths)) {
        Start::Engine => match lock {
            Some(lock) => Opening::Engine {
                // The engine task owns its config. This is the one start that reads it here: a switch back
                // to the local engine later reads `config.yaml` again, because limits changed during a run
                // live in that file and nowhere else (see `crate::switch::local_config`).
                config,
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

    // The backend to run on, and what its sign-in is still owed (see `crate::switch::Login`): an engine on
    // this library owes nothing, because it signs in to nobody.
    let (backend, login, ask, notice) = match opening {
        Opening::Engine { config, lock } => {
            match Backend::start_local(config, &paths, lock).await {
                Ok(backend) => (backend, None, None, None),
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
            let (secret, gateway_key, from_keychain, ask, notice) = match start {
                StartSecret::Ready {
                    secret,
                    gateway_key,
                    from_keychain,
                } => (
                    secret,
                    gateway_key,
                    from_keychain,
                    if connect_screen {
                        Some(target.clone())
                    } else {
                        None
                    },
                    None,
                ),
                StartSecret::Ask {
                    secret,
                    gateway_key,
                    notice,
                } => (secret, gateway_key, false, Some(target.clone()), notice),
            };
            // Nothing was collected from anybody to start this connection, so nothing new is kept for it; what
            // the keychain already holds stays there until this server says whether it is right. The hand-off is
            // a smaller promise than a remembered server: this run took an address another run offered it, and
            // that server only exists while that run is up, so nothing about it is written down even once it has
            // signed in (see `crate::switch::Remembered`).
            let login = if connect_screen {
                Login::borrowed(target.clone())
            } else {
                Login::remembered(target.clone(), from_keychain)
            };
            (
                Backend::start_remote(target.clone(), secret, gateway_key),
                Some(login),
                ask,
                notice,
            )
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

    // The prepared logos are CPU work (area-averaging a 511x853 PNG into every size the terminal needs,
    // plus quantization for the image protocols), so they go to the blocking pool. The screen draws the
    // text title until they arrive; a terminal with no image protocol at all gets nothing to wait for.
    let prepare_fut = if graphics.protocol != fbtt_tui::graphics::Protocol::None {
        Some(tokio::task::spawn_blocking(move || {
            fbtt_tui::graphics::prepare(graphics)
        }))
    } else {
        None
    };

    // The queue for work this run must not wait on: what a sign-in owes (a `ui.yaml` write and a keychain
    // write), and the delete that follows a refusal of a kept password. It is made here, before the screen,
    // because it lives as long as the screen does - every backend this run switches to sends its work on this
    // same queue, and the loop reads the answers whenever they turn up.
    let mut work = Work::new(&paths, &store);

    // The event stream is made once for the whole screen and let go before the last save: its reader owns
    // stdin, and switching backends changes which library the screen talks to, not who owns the keyboard.
    // The mark's bookkeeping is here, beside the terminal, because it is the terminal's own: what the
    // screen last drew is what the exit has to clean up, and the `Saving...` frame it draws has to
    // repaint the cells a removed mark used to cover. The screen borrows the mark while it runs and
    // hands it back when it is done, and what it drew last is what the exit knows.
    let mut stream = EventStream::new();
    let mut logos: Option<std::sync::Arc<fbtt_tui::graphics::place::PreparedLogos>> = None;
    let mut placer = fbtt_tui::graphics::place::Placer::new(graphics.protocol);
    let mut mark = crate::switch::Mark {
        logos: &mut logos,
        placer: &mut placer,
    };
    let startup_cell = graphics.cell.map(|cell| (cell.w, cell.h));
    let screen = crate::switch::Screen {
        model: &mut model,
        snap: &mut snap,
        mark: &mut mark,
        wanted: None,
        in_flight: None,
        protocol: graphics.protocol,
        scale: graphics.scale,
    };
    let (quit, screen, running) = run_session(
        terminal.get_mut(),
        screen,
        backend,
        login,
        &mut work,
        &mut stream,
        &mut signal,
        prepare_fut,
        startup_cell,
    )
    .await;

    // The last backend is told once: by a command the loop sent or, if the queue was full, by its channels
    // going out of scope. No timeout - resume data is worth the wait. Whatever it owes - an engine's last
    // save, and with it this library's lock - arrives here, on a screen that says what is happening while it
    // does. A run always ends with a backend under it: a switch that could not go ahead kept the one it had.
    // The `Saving...` screen is a frame like any other: it goes through the same logo path, so the cells
    // a removed mark used to cover are repainted rather than left as a ghost under the last words.
    let stopped = running
        .stop(screen.model, |model| {
            let logos = screen.mark.logos.as_deref();
            draw_saving(terminal.get_mut(), model, logos, screen.mark.placer)
        })
        .await;
    // The mark's ids are the placer's to clean up: kitty's images live in the terminal after this process
    // is gone, so the ids are deleted on the way out; the other protocols write nothing on exit, because
    // their images are the cells the screen is about to leave, and those go with it.
    let exit_bytes = placer.on_exit();
    if !exit_bytes.is_empty() {
        use std::io::Write as _;
        let _ = terminal.get_mut().backend_mut().write_all(&exit_bytes);
        let _ = terminal.get_mut().backend_mut().flush();
    }
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

/// What the run starts on, once `ui.yaml`, this library and the keychain have been asked about.
enum Opening {
    /// Drive this library's engine, on `config`, holding its lock.
    Engine { config: Config, lock: LibraryLock },
    /// Connect to `target`. `start` says what it signs in with and whether something still has to be asked
    /// for it on the connect screen; `connect_screen` opens that screen anyway, prefilled, for a target this
    /// run named itself - the server running on this library - where the address is known but how to sign in
    /// to it is not. Such a connection is borrowed rather than chosen, so nothing about it is written down
    /// even after it signs in (see `crate::switch::Remembered`).
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
pub fn library_in_use(paths: &Paths) -> String {
    format!(
        "Another featherbittorrent is already using this library ({}). Close it first.",
        paths.data_dir.display()
    )
}

/// The line for a library this run could not make sense of at all, which is not the same thing as a holder
/// and so does not deserve exit 3. Also the one a switch answers with when this library will not come free
/// for its engine (see `crate::switch`).
pub fn cannot_lock(paths: &Paths, problem: &str) -> String {
    format!(
        "Could not lock the library ({}): {problem}",
        paths.data_dir.display()
    )
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
