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
use fbtt_core::{Config, ConfigLoad, LibraryLock, LockError, Paths, web_ui_env_override};
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
    // The lock comes before the questions, not after them. A wizard that writes
    // `config.yaml` while a TUI is running on it would replace what that run is
    // working from and only then find out it was never allowed to start; refused up
    // front, this command neither asks anything nor changes anything.
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

    // The wizard asks before any log line exists: a person answering questions has
    // nowhere to read a warning, and prompts belong on stdout anyway.
    let (config, startup_problem) = config_for_serve(paths)?;

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

/// Starts the server on the engine's settings: what the config says, with `--offline` applied in a build
/// that has that switch, and with `FBTT_WEB_UI` applied over the config's server section in every build.
/// There is one start path in every build, because taking the settings as an argument is not a test hook -
/// see [`Server::start_with_settings`] - so no feature that only tests ask for is linked into a release
/// binary here.
///
/// This is the one place `serve` reads the process environment; everything the override decides is in
/// [`apply_web_ui_env`], which takes the lookup as an argument.
async fn open(
    mut config: Config,
    paths: &Paths,
    lock: LibraryLock,
    debug: &ServeDebug,
) -> Result<Server, ServerError> {
    apply_web_ui_env(&mut config, &|name| std::env::var(name).ok());
    // A headless build has no browser UI to serve, so it says once what it is doing with a
    // setting that asked for one: heard, kept in the file, and not acted on. (The held log
    // writer keeps this behind `listening on ...`, where a script's contract says it belongs.)
    #[cfg(not(feature = "web-ui"))]
    if config
        .server
        .as_ref()
        .is_some_and(|server| server.web_ui.is_some())
    {
        tracing::warn!(
            "server.web_ui is set, but this is the headless build: there is no web UI to serve"
        );
    }
    #[cfg(debug_assertions)]
    let settings = offline_settings(&config, debug.offline());
    #[cfg(not(debug_assertions))]
    let settings = {
        // A release build has no switches to read, so the config's own settings are the answer.
        let _ = debug;
        config.settings_before_proxy()
    };
    Server::start_with_settings(settings, config, paths, lock).await
}

/// `FBTT_WEB_UI`, applied to the config's server section: when the environment names an answer it wins
/// over the file - even over a file that said `web_ui: true` - and when it names nothing, or a word
/// [`web_ui_env_override`] does not take, the file keeps its choice. A config with no server section at
/// all has nothing to override: `serve` only ever reaches this through a config that has one (see
/// [`serve_base_from_load`]), so that arm is a shape, not a case.
fn apply_web_ui_env(config: &mut Config, env: &dyn Fn(&str) -> Option<String>) {
    let Some(on) = web_ui_env_override(env) else {
        return;
    };
    if let Some(server) = config.server.as_mut() {
        server.web_ui = Some(on);
    }
}

/// The engine's settings, with `--offline` applied: every socket on loopback and
/// nothing that could leave the machine. A test uses this to hold a real server on
/// one address it chose, the way `fbtt-server`'s own tests do.
#[cfg(debug_assertions)]
fn offline_settings(config: &Config, offline: bool) -> fbt::Settings {
    let mut settings = config.settings_before_proxy();
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

/// What a loaded config means for `serve`: whether to serve it as read (no wizard)
/// or to start the wizard from it. The second value is the line about the file that
/// `Config::load` reported, kept until the first log line is out of the way.
#[derive(Debug)]
pub enum ServeBase {
    /// The file named a server: it has already been asked the questions the wizard
    /// asks, so serve it as read, with no prompting.
    Ready {
        /// The config to serve, resolved.
        config: Config,
        /// The one line to log before `listening on`, if the file needed saying something.
        problem: Option<String>,
    },
    /// No server section to serve as read: the wizard starts from this base and the
    /// file is rewritten with its answers.
    Wizard {
        /// The base the wizard starts from.
        base: Config,
        /// The one line to log before `listening on`, if the file needed saying something.
        problem: Option<String>,
    },
}

/// What a loaded config means for `serve`, decided without touching stdin or stdout:
/// the pure half of [`config_for_serve`], factored out so a test can check the
/// decision without running a server or prompting.
///
/// A file that carries a server section has already been asked the questions the
/// wizard asks, so it is served as read. A file that reads as this config but whose
/// only problem is the player's setting is in the same shape: the server section is
/// there and valid, the player's value is the one thing a run does not act on, so the
/// config is served as read (the player's at its default) and the one line about it is
/// kept to log. Every other shape is what the wizard starts from.
pub fn serve_base_from_load(loaded: ConfigLoad, paths: &Paths) -> ServeBase {
    match loaded {
        // A file that carries a server section has already been asked these questions; one that does
        // not, which is what a settings save from the TUI writes, never has. Presence is in the parsed
        // config: `Config` keeps no implicit server section to mistake for an answer.
        ConfigLoad::Loaded(config) if config.server.is_some() => ServeBase::Ready {
            config: config.resolve(paths),
            problem: None,
        },
        ConfigLoad::Loaded(config) => ServeBase::Wizard {
            base: config,
            problem: None,
        },
        // The file read as this config and named a server: the player's setting is the one thing a
        // run does not act on, so the config is served as read, the player's at its default, and the
        // one line about it is kept to log. This is the case a bad `player` value must not push into
        // the wizard: a server config with a player typo is still a server config.
        ConfigLoad::PlayerProblem { config, problem } if config.server.is_some() => {
            ServeBase::Ready {
                config,
                problem: Some(problem),
            }
        }
        // A player problem on a config with no server section is the same shape as a first run: the
        // wizard starts from the file's settings, the player's at its default.
        ConfigLoad::PlayerProblem { config, problem } => ServeBase::Wizard {
            base: config,
            problem: Some(problem),
        },
        // The unreadable bytes are already kept aside under `.bad`, so asking again loses nothing.
        ConfigLoad::Bad { config, problem } => ServeBase::Wizard {
            base: config,
            problem: Some(problem),
        },
        ConfigLoad::Missing => ServeBase::Wizard {
            base: Config::default_for(paths),
            problem: None,
        },
    }
}

/// The config to serve on: what the file says, or the wizard's answers when there
/// is no file, no server section in it, or bytes this run could not read. Called
/// with the library lock already held, so a wizard run cannot replace a file some
/// other featherbittorrent is working from (see [`run`]).
///
/// A server address and a login rule are two decisions nobody should have to look
/// up in a YAML file, and a config written by the TUI has never been asked them.
/// The second value is the line about the file that `Config::load` reported, which
/// becomes a warning once the first log line is out of the way.
fn config_for_serve(paths: &Paths) -> Result<(Config, Option<String>), Failure> {
    match serve_base_from_load(Config::load(paths)?, paths) {
        // A file that named a server is served as read: no prompting, no rewrite.
        ServeBase::Ready { config, problem } => Ok((config, problem)),
        // Everything else starts the wizard, which rewrites the file with its answers.
        ServeBase::Wizard { base, problem } => {
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
            Ok((config.resolve(paths), problem))
        }
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

/// There is no `--stop-file` in a release build, so [`ServeDebug::stop_file`] always answers `None`, this
/// future is never polled to completion, and nothing here polls for anything: it exists only so that
/// `wait_for_stop` selects over the same three futures in every build. A file named by a flag that does
/// not exist would have nothing to watch it, which is why this arm is a future that never wakes.
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

#[cfg(test)]
mod tests {
    use super::{ServeBase, apply_web_ui_env, serve_base_from_load};
    use fbtt_core::scratch::ScratchDir;
    use fbtt_core::{Config, Paths, ServerConfig};
    use std::fs;

    /// A scratch library: config side and data side in the same place, which is all
    /// `Config::load` and `serve_base_from_load` need to know.
    fn library(dir: &ScratchDir) -> Paths {
        Paths {
            config_dir: dir.path().to_path_buf(),
            data_dir: dir.path().to_path_buf(),
        }
    }

    /// A server config whose only problem is the player's setting is served as read, with the
    /// one line kept to log: it is not a first run, and the wizard must not be asked anything.
    /// This is the case a bad `player` value used to push into the wizard, which under systemd or
    /// Docker would stall or fail, and which, if the wizard completed, would rewrite the file.
    #[test]
    fn a_server_config_with_a_player_problem_is_served_not_wizarded() {
        let dir = ScratchDir::new("serve_player_problem");
        let paths = library(&dir);

        // A valid server section and a player whose program is empty: the one thing a run does
        // not act on, and nothing else wrong.
        fs::write(
            paths.config_file(),
            "server:\n  bind: 127.0.0.1:8099\nplayer:\n  program: \"\"\n",
        )
        .expect("write a server config with a bad player");

        let loaded = Config::load(&paths).expect("the file is read, not refused");
        let base = serve_base_from_load(loaded, &paths);

        // It is served as read, not started through the wizard.
        let ServeBase::Ready { config, problem } = base else {
            panic!(
                "a server config with a player problem is served as read, not wizarded: {base:?}"
            );
        };

        // The server section is exactly as the file has it.
        let bind = config
            .server
            .as_ref()
            .expect("the server section survives a bad player")
            .bind;
        assert_eq!(
            bind,
            "127.0.0.1:8099"
                .parse()
                .expect("the address the file named"),
            "the server's bind is kept as the file named it"
        );

        // The player's setting is at its default: the default program, no extra arguments.
        assert_eq!(
            config.player,
            Config::default().player,
            "only the player's is reset"
        );

        // The one line about the player's setting is kept to log before `listening on`.
        assert!(
            problem
                .as_ref()
                .expect("the problem line is kept to log")
                .lines()
                .count()
                == 1,
            "one line: {problem:?}"
        );
    }

    /// A config with a player problem and no server section is still a first run: the wizard
    /// starts from it. Only the server-section case skips the wizard.
    #[test]
    fn a_player_problem_without_a_server_still_wizards() {
        let dir = ScratchDir::new("serve_player_problem_no_server");
        let paths = library(&dir);

        fs::write(paths.config_file(), "player:\n  program: \"\"\n")
            .expect("write a player config with no server");

        let loaded = Config::load(&paths).expect("the file is read, not refused");
        let base = serve_base_from_load(loaded, &paths);

        // No server section: the wizard starts from it.
        let ServeBase::Wizard { base, problem } = base else {
            panic!("no server section, so the wizard starts: {base:?}");
        };
        assert!(problem.is_some(), "the problem line is kept to log");
        assert!(base.server.is_none(), "no server section to serve as read");
    }

    /// `FBTT_WEB_UI` wins over the file: the environment's word replaces what the config
    /// chose, and nothing in the environment is not an answer - it leaves the config in
    /// charge. The process environment is read only by `open`, so this test hands in its
    /// own lookup and never sets anything.
    #[test]
    fn the_environment_wins_over_the_config() {
        let mut chosen = Config {
            server: Some(ServerConfig {
                web_ui: Some(true),
                ..ServerConfig::default()
            }),
            ..Config::default()
        };
        apply_web_ui_env(&mut chosen, &|name| {
            (name == "FBTT_WEB_UI").then_some("0".to_string())
        });
        assert_eq!(
            chosen.server.expect("the section survives").web_ui,
            Some(false),
            "FBTT_WEB_UI=0 turns off a UI the config turned on"
        );

        // Nothing in the environment says nothing, and the config's choice stands.
        let mut silent = Config {
            server: Some(ServerConfig {
                web_ui: Some(true),
                ..ServerConfig::default()
            }),
            ..Config::default()
        };
        apply_web_ui_env(&mut silent, &|_name| None);
        assert_eq!(
            silent.server.expect("the section survives").web_ui,
            Some(true),
            "an unset FBTT_WEB_UI leaves the config in charge"
        );
    }
}
