//! `featherbittorrent serve`: the library, its API, and nothing else.
//!
//! No terminal is taken over, so everything this run has to say goes to stderr as
//! logs: one line first that says where it is listening - so a script can wait for
//! it - then whatever the server and the engine find worth saying. Ctrl-C and
//! SIGTERM both mean stop: close the API, run the last save, release the library.
//!
//! The debug-only switches at the bottom are what its own tests use: a stop file
//! instead of a signal, and an offline mode that keeps every socket on loopback.

use std::future::{Future, pending};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Args;
use fbtt_core::{Config, ConfigLoad, LibraryLock, LockError, Paths};
use fbtt_server::{Server, ServerError};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

use crate::exit::Failure;
use crate::wizard;

/// A startup line that arrived before the first log could be written, kept until
/// it is safe to print without pushing `listening on ...` off the first line.
const HELD_LIMIT: usize = 256 * 1024;

/// Debug-only switches for `serve`. A release build has none of them, which is why
/// every reader below answers with "off" when compiled without debug assertions:
/// an unattended server should not be steered by flags a person did not ask for.
#[derive(Debug, Default, Args)]
pub struct ServeDebug {
    /// Stop as soon as this file exists, like Ctrl-C would. For tests and scripts.
    #[cfg(debug_assertions)]
    #[arg(long = "stop-file", value_name = "PATH", hide = true)]
    stop_file: Option<PathBuf>,

    /// Keep every socket on loopback and turn off DHT, LSD, UPnP and NAT-PMP, so a
    /// test run cannot reach the network at all. The config still decides the API's
    /// port; only where it listens changes.
    #[cfg(debug_assertions)]
    #[arg(long, hide = true)]
    offline: bool,
}

impl ServeDebug {
    /// The stop file, if this build has stop files at all.
    #[cfg(debug_assertions)]
    pub fn stop_file(&self) -> Option<&Path> {
        self.stop_file.as_deref()
    }

    /// A release build has no `--stop-file` flag to read.
    #[cfg(not(debug_assertions))]
    pub fn stop_file(&self) -> Option<&Path> {
        None
    }

    /// Whether the engine is kept on loopback, if this build has that switch. A
    /// release build has no `--offline` flag, and therefore nothing to ask about:
    /// the code that would read it is compiled out too (see `open`).
    #[cfg(debug_assertions)]
    pub fn offline(&self) -> bool {
        self.offline
    }
}

/// Serves this library until a signal says stop, then shuts it down in the order
/// that leaves the library whole. The problem it returns is what `main` prints and
/// exits with.
pub async fn run(paths: &Paths, debug: &ServeDebug) -> Result<(), Failure> {
    // The wizard asks before any log line exists: a person answering questions has
    // nowhere to read a warning, and prompts belong on stdout anyway.
    let (config, startup_problem) = config_for_serve(paths)?;

    // Before the listener: a held lock is a sentence to read, not a server to wait for.
    let lock = match LibraryLock::acquire(&paths.lock_file()) {
        Ok(lock) => lock,
        Err(LockError::Held) => {
            return Err(Failure::in_use(format!(
                "Another featherbittorrent is already using this library ({}).",
                paths.data_dir.display()
            )));
        }
        Err(LockError::Io(problem)) => {
            return Err(Failure::problem(format!(
                "Could not lock the library ({}): {problem}",
                paths.data_dir.display()
            )));
        }
    };

    // Logs start here, held back until the address is known: the contract is that
    // the first line a script sees is `listening on <addr>`, and the engine's own
    // complaints about a resume file would otherwise arrive before it.
    install_logs();
    let server = match open(config, paths, lock, debug).await {
        Ok(server) => server,
        Err(problem) => {
            release_logs();
            return Err(Failure::problem(problem.to_string()));
        }
    };

    log_first(&format!("listening on {}", server.local_addr()));
    if let Some(problem) = startup_problem {
        tracing::warn!("{problem}");
    }

    wait_for_stop(debug).await;
    tracing::info!("stopping");

    match server.shutdown().await {
        Ok(()) => Ok(()),
        Err(problem) => {
            // The library on disk is the one from before this run, so say what that
            // means rather than only passing the save's own words along.
            Err(Failure::problem(format!(
                "The last save did not complete: {problem}"
            )))
        }
    }
}

/// Starts the server: with the debug switches in a debug build (where the tests
/// run), and straight from the config in a release build.
async fn open(
    config: Config,
    paths: &Paths,
    lock: LibraryLock,
    debug: &ServeDebug,
) -> Result<Server, ServerError> {
    #[cfg(debug_assertions)]
    {
        let settings = offline_settings(&config, debug.offline());
        Server::start_with_settings(settings, config, paths, lock).await
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = debug;
        Server::start(config, paths, lock).await
    }
}

/// The engine's settings, with `--offline` applied: every socket on loopback and
/// nothing that could leave the machine. A test uses this to hold a real server on
/// one address it chose, the way `fbtt-server`'s own tests do.
#[cfg(debug_assertions)]
fn offline_settings(config: &Config, offline: bool) -> fbt::Settings {
    let mut settings = config.session_settings();
    if offline {
        settings.listen_addrs = vec![std::net::SocketAddr::new(
            std::net::Ipv4Addr::LOCALHOST.into(),
            config.listen_port,
        )];
        settings.dht_enabled = false;
        settings.enable_lsd = false;
        settings.enable_upnp = false;
        settings.enable_natpmp = false;
    }
    settings
}

/// The config to serve on: what the file says, or the wizard's answers when there
/// is no file, no server section in it, or bytes this run could not read.
///
/// A server address and a login rule are two decisions nobody should have to look
/// up in a YAML file, and a config written by the TUI has never been asked them.
/// The second value is the line about the state file that `Config::load` reported,
/// which becomes a warning once the first log line is out of the way.
fn config_for_serve(paths: &Paths) -> Result<(Config, Option<String>), Failure> {
    let startup_problem;
    let base = match Config::load(paths)? {
        ConfigLoad::Loaded(config) if has_server_section(paths) => {
            return Ok((config.resolve(paths), None));
        }
        ConfigLoad::Loaded(config) => {
            startup_problem = None;
            config
        }
        ConfigLoad::Bad { config, problem } => {
            // The unreadable bytes are already kept aside under `.bad`, so asking
            // again loses nothing.
            startup_problem = Some(problem);
            config
        }
        ConfigLoad::Missing => {
            startup_problem = None;
            Config::default_for(paths)
        }
    };

    let stdin = std::io::stdin();
    let secret_input = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    let config = wizard::run(&base, &mut input, &mut out, secret_input)?;
    drop(input);
    config.save(&paths.config_file())?;
    writeln!(out, "Wrote {}", paths.config_file().display())?;
    writeln!(
        out,
        "Add a device key later with: featherbittorrent device add"
    )?;
    let _ = out.flush();
    Ok((config.resolve(paths), startup_problem))
}

/// Whether `config.yaml` already says something about the server. A file written by
/// the TUI does not: it has no reason to name an address it never listens on, and
/// `Config::save` drops a server section that only holds defaults.
fn has_server_section(paths: &Paths) -> bool {
    let text = match std::fs::read_to_string(paths.config_file()) {
        Ok(text) => text,
        Err(_) => return true,
    };
    match serde_norway::from_str::<serde_norway::Value>(&text) {
        Ok(value) => value.get("server").is_some(),
        // Not YAML at all: `Config::load` has its own answer for that, and this
        // function only decides whether to ask questions.
        Err(_) => true,
    }
}

/// Waits for the cue to stop: Ctrl-C, or SIGTERM on Unix, or (debug builds) the
/// stop file appearing. All three are listened for once, here, before anything can
/// go wrong from here on - a listener made per poll would miss a signal that
/// arrives between two polls.
///
/// This is not the TUI's `ShutdownSignal`: that one listens for SIGTERM only,
/// because in raw mode Ctrl-C arrives as a key press. A server takes no terminal,
/// so Ctrl-C is what it always was, a signal this process has to catch itself.
async fn wait_for_stop(debug: &ServeDebug) {
    let ctrl_c = tokio::signal::ctrl_c();
    let terminate = terminate_signal();
    let stop_file = match debug.stop_file() {
        Some(path) => Box::pin(wait_for_stop_file(path.to_path_buf()))
            as Pin<Box<dyn Future<Output = ()> + Send>>,
        None => Box::pin(pending::<()>()) as Pin<Box<dyn Future<Output = ()> + Send>>,
    };
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
        _ = stop_file => {}
    }
}

/// SIGTERM, listened for once. A system that will not hand over a listener waits
/// on a future that never wakes instead: Ctrl-C still works, and a server with no
/// terminal in front of it is not the place to refuse to start.
#[cfg(unix)]
fn terminate_signal() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut listener) => Box::pin(async move {
            let _ = listener.recv().await;
        }),
        Err(_) => Box::pin(pending::<()>()),
    }
}

/// There is no SIGTERM to listen for outside Unix, and Ctrl-C covers the signal
/// that exists there.
#[cfg(not(unix))]
fn terminate_signal() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(pending::<()>())
}

/// `--stop-file`: the file appearing is the cue, checked once a second. A test
/// makes the file instead of finding this process's pid and sending it a signal.
#[cfg(debug_assertions)]
async fn wait_for_stop_file(path: PathBuf) {
    let mut clock = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        clock.tick().await;
        if path.exists() {
            return;
        }
    }
}

/// The same, for a build that has no stop file: a future nothing ever wakes.
#[cfg(not(debug_assertions))]
async fn wait_for_stop_file(_path: PathBuf) {}

/// Writes the first log line, then lets everything held back before it through.
fn log_first(line: &str) {
    OPEN.store(true, Ordering::SeqCst);
    tracing::info!("{line}");
    release_logs();
}

/// Installs the log writer: `RUST_LOG` decides what is logged (info when it is
/// unset), stderr is where it goes, and colour only when stderr is a terminal -
/// these lines end up in a file far more often than on a screen.
///
/// A line is its message and nothing else: no timestamp, level or module in front
/// of it. That is what lets the first line be exactly `listening on <addr>`, which
/// is what a script waits for, and these lines have no reader who needs to tell an
/// info from a warning at a glance.
fn install_logs() {
    let ansi = std::io::stderr().is_terminal();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(tracing::Level::INFO.into())
                .from_env_lossy(),
        )
        .with_ansi(ansi)
        .with_writer(HeldMakeWriter)
        .event_format(
            tracing_subscriber::fmt::format()
                .without_time()
                .with_level(false)
                .with_target(false),
        )
        .init();
}

/// Startup lines written before [`log_first`], oldest first, and whether a line
/// still needs holding. A process runs one server, so this is the whole state a
/// second thread could race over: a log record can arrive from an engine thread
/// while this one is still starting up.
static HELD: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static OPEN: AtomicBool = AtomicBool::new(false);

/// Writes what was held back and leaves the stream open for everything after.
fn release_logs() {
    let held = std::mem::take(&mut *HELD.lock().expect("the log buffer"));
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(&held);
    let _ = stderr.flush();
}

/// The writer the formatter pulls out for each record: the real stderr once `OPEN`
/// is set, the buffer before that. A stream too long for the buffer stops being
/// held rather than stopping the server, so a chatty engine cannot exhaust it.
struct HeldWriter;

impl Write for HeldWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if OPEN.load(Ordering::SeqCst) {
            return std::io::stderr().write(buf);
        }
        let mut buffer = HELD.lock().expect("the log buffer");
        if buffer.len() + buf.len() > HELD_LIMIT {
            drop(buffer);
            OPEN.store(true, Ordering::SeqCst);
            return std::io::stderr().write(buf);
        }
        buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    /// A held line is not waiting for anything, so there is nothing to push out.
    /// Once the stream is open, the formatter's own flush reaches it.
    fn flush(&mut self) -> std::io::Result<()> {
        if OPEN.load(Ordering::SeqCst) {
            return std::io::stderr().flush();
        }
        Ok(())
    }
}

/// The same writer, handed out as many times as the formatter wants it.
struct HeldMakeWriter;

impl<'w> MakeWriter<'w> for HeldMakeWriter {
    type Writer = HeldWriter;

    fn make_writer(&'w self) -> HeldWriter {
        HeldWriter
    }
}
