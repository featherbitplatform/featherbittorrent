//! Which backend this run drives, and what it takes to change to another one.
//!
//! One backend runs at a time: either the engine in this process or a connection to a server, never
//! both, because the screen draws one library. A switch is not a conversation with the running one -
//! it is the ordered steps the person already trusts from a quit: tell it to stop, wait for what its
//! stop owes (an engine writes its resume file and only then lets go of the library), start the one
//! that was asked for, and remember the choice in `ui.yaml` so the next run starts where this one ended.
//!
//! Two decisions about that are written as pure functions over plain values rather than as code inside
//! the async loop, because both are about a file system and a terminal the tests would rather not have:
//! what to start on ([`initial_target`]) and whether a switch may replace what is running
//! ([`plan_switch`]). The rule they carry - ask the library lock before stopping anything - is what keeps
//! a refused switch from leaving the person with no backend at all.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use fbtt_core::client::{ClientParts, Command, Commands};
use fbtt_core::ui_state::{LastUsed, RemoteTarget, UiState};
use fbtt_core::{
    Config, LibraryLock, LocalBackend, LockError, Paths, RemoteBackend, Secret, ServerConfig,
    Snapshot,
};
use fbtt_tui::{ConnectTarget, Model};
use tokio::task::JoinHandle;

/// The one backend this run drives: the channels the loop waits on, and what stopping it has to wait for.
#[derive(Debug)]
pub enum Backend {
    /// The engine in this process, on this library. Its task owns the session, the resume file and the
    /// library lock; its handle answers once the last save is on disk and the lock is let go.
    Local {
        parts: ClientParts,
        task: JoinHandle<Result<(), String>>,
    },
    /// A server over the network. Its tasks answer `Shutdown` (or the command queue going away) by
    /// themselves, so nothing here has a handle to join: a connection owes no file to write.
    Remote { parts: ClientParts },
}

impl Backend {
    /// Starts this library's engine on `config`, holding `lock` until its last save is written.
    ///
    /// The `Err` is the engine's own line about why it could not start - a data directory it may not
    /// write, a resume file it cannot read and may not overwrite - which is worth printing rather than
    /// retrying, because nothing in this process can fix it.
    pub async fn start_local(
        config: Config,
        paths: &Paths,
        lock: LibraryLock,
    ) -> Result<Backend, fbt::SessionError> {
        let (client, task) = LocalBackend::start(config, paths, lock).await?;
        Ok(Backend::Local {
            parts: client.split(),
            task,
        })
    }

    /// Connects to `target`, signed in with `secret`.
    ///
    /// Nothing here waits or fails: [`RemoteBackend::start`] reports an unusable address or a credential
    /// that does not go with the method through the client, which is what the screen draws.
    pub fn start_remote(target: RemoteTarget, secret: Secret) -> Backend {
        Backend::Remote {
            parts: RemoteBackend::start(target, secret).split(),
        }
    }

    /// Which kind of backend this is, as [`plan_switch`] wants it.
    pub fn kind(&self) -> BackendKind {
        match self {
            Backend::Local { .. } => BackendKind::Local,
            Backend::Remote { .. } => BackendKind::Remote,
        }
    }

    /// The way in to this backend's queue, for a command that belongs to the backend as a whole rather than
    /// to one turn of the loop - the watch that tells a newly adopted backend what the screen is looking at.
    pub fn commands(&self) -> &Commands {
        match self {
            Backend::Local { parts, .. } | Backend::Remote { parts } => &parts.commands,
        }
    }

    /// The snapshot this backend starts from: what it built before it published anything, so taking it
    /// is a clone of an `Arc` and never a wait.
    pub fn initial_snapshot(&self) -> Arc<Snapshot> {
        let parts = match self {
            Backend::Local { parts, .. } | Backend::Remote { parts } => parts,
        };
        parts.snapshots.borrow().clone()
    }

    /// Takes the channels out so the loop can hold each one on its own, and the handle beside them.
    ///
    /// A method on the whole backend borrows it, and one `select!` branch cannot hold that borrow while
    /// another sends on the same backend - the same reason `Client::split` exists.
    pub fn into_parts(self) -> (ClientParts, Option<JoinHandle<Result<(), String>>>) {
        match self {
            Backend::Local { parts, task } => (parts, Some(task)),
            Backend::Remote { parts } => (parts, None),
        }
    }

    /// Puts the channels and the handle back together, which is what makes a backend stoppable again.
    pub fn from_parts(parts: ClientParts, task: Option<JoinHandle<Result<(), String>>>) -> Backend {
        match task {
            Some(task) => Backend::Local { parts, task },
            None => Backend::Remote { parts },
        }
    }

    /// Stops this backend and waits for what its stop owes us.
    ///
    /// `show` runs with the model after every step, so the caller can keep the `Saving...` screen up and
    /// let it carry the lines the stopping backend reports on its way out. The `Err` is one line about a
    /// save that did not happen, which only the person running this can act on; a connection has nothing
    /// to write and so cannot fail this way.
    pub async fn stop(
        self,
        model: &mut Model,
        mut show: impl FnMut(&mut Model),
    ) -> Result<(), String> {
        let (mut parts, task) = self.into_parts();
        // Told once: by this command or, if the queue was full, by `parts` going out of scope below. No
        // timeout - resume data is worth the wait, and a connection that will not answer is not waiting
        // for anything of ours.
        parts.commands.send(Command::Shutdown);
        show(model);
        while let Ok(notice) = parts.notices.try_recv() {
            model.push_notice(notice, Instant::now());
            show(model);
        }
        // Every channel gone is the same as `Shutdown` to a backend that never got one.
        drop(parts);
        match task {
            None => Ok(()),
            Some(task) => match task.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(problem)) => Err(problem),
                Err(joined) => Err(format!("the engine task stopped before saving: {joined}")),
            },
        }
    }
}

/// Which kind of backend is running, as the switch decision sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// This library's own engine, in this process.
    Local,
    /// A server over the network.
    Remote,
}

/// Whether anything stands between this run and driving this library's engine, spelled as plain values so
/// both decisions can be tested without a file system.
///
/// [`LibraryLock::acquire`] answers `Held` or `Failed`. `Taken` is what a caller passes when it *has* the
/// library - because it just took the lock, or because its own engine already holds it - and also what a
/// run that asks nothing of this library passes: connecting to a server needs no lock here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockOutcome {
    /// Nothing is in the way: this run holds the library, or needs nothing from it.
    Taken,
    /// Another featherbittorrent holds it.
    Held,
    /// It could not be taken for a reason that is not a competing holder; the line is the operating
    /// system's own.
    Failed(String),
}

impl LockOutcome {
    /// What [`LibraryLock::acquire`] answered, as these decisions see it. The guard itself stays with the
    /// caller: taking a lock has effects a decision does not.
    pub fn of(acquired: &Result<LibraryLock, LockError>) -> LockOutcome {
        match acquired {
            Ok(_) => LockOutcome::Taken,
            Err(LockError::Held) => LockOutcome::Held,
            Err(LockError::Io(problem)) => LockOutcome::Failed(problem.to_string()),
        }
    }
}

/// What the run starts on, as decided before anything was started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Drive this library's engine. The lock is in hand: `Taken` was answered by taking it.
    Engine,
    /// Connect to `target`: no engine, and no lock on this library either.
    Server(RemoteTarget),
    /// Another featherbittorrent holds this library and its config runs a server on it, so there is a
    /// connection this run could make instead of leaving: ask the person whether to make it, opening the
    /// connect screen prefilled with `target`.
    AskAboutServer(RemoteTarget),
    /// This library belongs to another featherbittorrent and offers nothing else: say so and leave.
    LibraryInUse,
    /// The library could not be told anything about - not held by somebody, simply unreadable. The line
    /// is the operating system's own.
    LockFailed(String),
}

/// Decides what to start on.
///
/// `ui.yaml` is the first word: which backend the last run used is a convenience, and remembering it is
/// the whole reason the file exists. Only a run it points at this library asks the library whether it is
/// free, so only such a run has a `lock` to report; a run pointed at a server passes [`LockOutcome::Taken`]
/// because nothing here stands in its way.
///
/// `local_server` is the address of a server this library's own config says runs here
/// ([`named_server`]), which is what turns "the library is taken" from a dead end into a question.
pub fn initial_target(ui: &UiState, lock: LockOutcome, local_server: Option<SocketAddr>) -> Start {
    match &ui.last_used {
        // What the last run connected to is what this one connects to, and no lock on this library is
        // asked for on the way there.
        LastUsed::Remote(target) => Start::Server(target.clone()),
        LastUsed::Local => match (lock, local_server) {
            (LockOutcome::Taken, _) => Start::Engine,
            // Somebody else drives this library - and runs a server on it that this run could reach over
            // loopback. That is worth one question; the answer to it is a connection, not an engine.
            (LockOutcome::Held, Some(bind)) => Start::AskAboutServer(loopback_target(bind)),
            (LockOutcome::Held, None) => Start::LibraryInUse,
            // "cannot tell" is not the same answer as "someone else is here", so it is not a question
            // either: it is this library being unusable, which is what exit 1 is for.
            (LockOutcome::Failed(problem), _) => Start::LockFailed(problem.clone()),
        },
    }
}

/// What to do about the connect screen's answer, decided before anything running is touched.
///
/// A switch that cannot end well must not cost the person the backend they already have: an engine saves
/// and frees its lock on the way out, and a library that is somebody else's would be re-taken by nobody.
/// So the lock is asked about first, and `LibraryInUse` means nothing was stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchPlan {
    /// Stop what is running and start the one that was asked for. For a local engine, `Taken` says the
    /// lock needed for it is already in hand.
    Go,
    /// This library's own engine is already what is running: keep it and say so. Asking this run for its
    /// own library would only report a holder that is itself.
    AlreadyHere,
    /// The library belongs to another featherbittorrent: keep the connection this run has and say so.
    LibraryInUse,
    /// The library could not be told anything about; the line is the operating system's own. Keep what
    /// is running.
    LockFailed(String),
}

/// `current` is what is running now, and `lock` what this library's lock answered - asked only of a
/// switch that would need it, which is a switch to a local engine while a server connection runs. A
/// switch to a server needs nothing from this library, and a run already driving its own engine holds
/// its lock, so both pass [`LockOutcome::Taken`].
pub fn plan_switch(asked: &ConnectTarget, current: BackendKind, lock: LockOutcome) -> SwitchPlan {
    match (asked, current, lock) {
        // A server connection needs nothing from this library, whatever is running now.
        (ConnectTarget::Remote(_), _, _) => SwitchPlan::Go,
        (ConnectTarget::Local, BackendKind::Local, _) => SwitchPlan::AlreadyHere,
        (ConnectTarget::Local, BackendKind::Remote, LockOutcome::Taken) => SwitchPlan::Go,
        // Stopping this connection to lose the library to somebody else would leave nothing running.
        (ConnectTarget::Local, BackendKind::Remote, LockOutcome::Held) => SwitchPlan::LibraryInUse,
        (ConnectTarget::Local, BackendKind::Remote, LockOutcome::Failed(problem)) => {
            SwitchPlan::LockFailed(problem.clone())
        }
    }
}

/// The target for a server running on this library, reached over loopback.
///
/// A server bound to a network address (`0.0.0.0:8090`) is still reached from this machine over
/// loopback, so the address dialed is this machine's with that port: `http://0.0.0.0:8090` is not an
/// address to dial. Which login to use is what the connect screen asks for - this run cannot know
/// whether that server signs people in - so the target starts with none.
fn loopback_target(bind: SocketAddr) -> RemoteTarget {
    let ip = if bind.ip().is_loopback() {
        bind.ip()
    } else {
        Ipv4Addr::LOCALHOST.into()
    };
    RemoteTarget {
        url: format!("http://{}", SocketAddr::new(ip, bind.port())),
        ..RemoteTarget::default()
    }
}

/// Whether this library's `config.yaml` names a server of its own, and the address it says it listens on.
///
/// The parsed [`Config`] cannot answer this: every one of its fields has a default, so a config written
/// by the TUI - which never listens on anything - still reads back with a full `server` section. What
/// decides is whether the file ever said anything about a server (`serve` writes one, and
/// `Config::save` drops one that only holds defaults), which is the same question `serve.rs` asks before
/// it runs its wizard. Bytes that are not YAML answer `None`: a config this run could not read cannot
/// have told anybody about a server either.
pub fn named_server(paths: &Paths) -> Option<SocketAddr> {
    let text = std::fs::read_to_string(paths.config_file()).ok()?;
    server_address_of(&text)
}

/// The pure half of [`named_server`]: the address a config file's bytes name, `None` when they name no
/// server at all. A section that names no address means the default ([`Config`]'s own), because that is
/// what the server that wrote the section listened on.
fn server_address_of(text: &str) -> Option<SocketAddr> {
    let value = serde_norway::from_str::<serde_norway::Value>(text).ok()?;
    let server = value.get("server")?;
    match server.get("bind") {
        Some(serde_norway::Value::String(text)) => {
            Some(text.parse::<SocketAddr>().ok().unwrap_or_else(default_bind))
        }
        // A section with nothing under it, or an address spelled in a shape this run cannot read: the
        // default is what `serve` would have listened on anyway.
        _ => Some(default_bind()),
    }
}

/// The address a server with no `bind` in its config listens on.
fn default_bind() -> SocketAddr {
    ServerConfig::default().bind
}

#[cfg(test)]
mod tests {
    use super::{
        BackendKind, ConnectTarget, LockOutcome, Start, SwitchPlan, initial_target,
        loopback_target, named_server, plan_switch, server_address_of,
    };
    use fbtt_core::Paths;
    use fbtt_core::scratch::ScratchDir;
    use fbtt_core::ui_state::{LastUsed, LoginMethod, RemoteTarget, UiState};
    use std::fs;
    use std::net::{Ipv4Addr, SocketAddr};

    /// What the last run used decides what this one starts on: a remembered server wins over the local
    /// engine, and only a run that wants its own engine is answered by the lock.
    #[test]
    fn ui_state_decides_start() {
        let server = RemoteTarget {
            url: "https://bit.example".to_string(),
            login: LoginMethod::Password,
            user: Some("alice".to_string()),
            ..RemoteTarget::default()
        };
        let remote = UiState {
            last_used: LastUsed::Remote(server.clone()),
        };
        let local = UiState::default();

        // A remembered server needs nothing from this library, so it is connected to whatever the lock
        // would have said - including that the library is somebody else's, which is not this run's
        // business.
        for lock in [
            LockOutcome::Taken,
            LockOutcome::Held,
            LockOutcome::Failed("permission denied".to_string()),
        ] {
            assert_eq!(
                initial_target(&remote, lock.clone(), None),
                Start::Server(server.clone()),
                "a remembered server is start-on-{lock:?}"
            );
        }

        // The local engine is what a free library runs.
        assert_eq!(
            initial_target(&local, LockOutcome::Taken, None),
            Start::Engine
        );
        assert_eq!(
            initial_target(&local, LockOutcome::Taken, some_bind("127.0.0.1:8090")),
            Start::Engine,
            "a server running here is not worth asking about while the library is free"
        );

        // A library this run cannot make sense of is exit 1 with the operating system's line, not a
        // question and not the local engine.
        assert_eq!(
            initial_target(
                &local,
                LockOutcome::Failed("permission denied".to_string()),
                None
            ),
            Start::LockFailed("permission denied".to_string())
        );

        // A `ui.yaml` this run could not read is `UiState::default`, so it decides like the local engine.
        assert_eq!(
            initial_target(&UiState::default(), LockOutcome::Taken, None),
            Start::Engine
        );
    }

    /// A held library that runs a server of its own is a question, not a dead end; a held library with no
    /// server to offer is the sentence milestone 1 prints.
    #[test]
    fn lock_held_offers_local_server() {
        let local = UiState::default();

        assert_eq!(
            initial_target(&local, LockOutcome::Held, some_bind("127.0.0.1:8090")),
            Start::AskAboutServer(loopback_target(bind("127.0.0.1:8090"))),
            "a server running on this library is worth one question"
        );
        assert_eq!(
            initial_target(&local, LockOutcome::Held, None),
            Start::LibraryInUse,
            "no server section means nothing to connect to"
        );

        // The address dialed is loopback even when the server listens everywhere, because
        // `http://0.0.0.0:8090` is not an address to dial; the port is the one its config named.
        let asked = initial_target(&local, LockOutcome::Held, some_bind("0.0.0.0:9000"));
        let Start::AskAboutServer(target) = asked else {
            panic!("a held library with a server asks about it, got {asked:?}");
        };
        assert_eq!(target.url, "http://127.0.0.1:9000");
        assert_eq!(
            target.login,
            LoginMethod::None,
            "this run cannot know whether that server signs people in"
        );

        // An IPv6 loopback keeps the brackets a URL needs around its colons.
        let Start::AskAboutServer(target) =
            initial_target(&local, LockOutcome::Held, some_bind("[::1]:8090"))
        else {
            panic!("a held library with a server asks about it");
        };
        assert_eq!(target.url, "http://[::1]:8090");
    }

    /// The switch to a local engine is refused before anything is stopped, so a person who cannot have
    /// the library still has the connection they had.
    #[test]
    fn switch_checks_lock_first() {
        let local = ConnectTarget::Local;
        let remote = ConnectTarget::Remote(RemoteTarget {
            url: "https://bit.example".to_string(),
            ..RemoteTarget::default()
        });

        // A server connection needs nothing from this library, so it always goes ahead.
        for current in [BackendKind::Local, BackendKind::Remote] {
            assert_eq!(
                plan_switch(&remote, current, LockOutcome::Taken),
                SwitchPlan::Go
            );
        }

        // Asking this run for a library its own engine already holds would report a holder that is just
        // us, so it is answered without asking.
        assert_eq!(
            plan_switch(&local, BackendKind::Local, LockOutcome::Taken),
            SwitchPlan::AlreadyHere
        );

        // A server connection gives up only once the replacement is in hand: a held library keeps it.
        assert_eq!(
            plan_switch(&local, BackendKind::Remote, LockOutcome::Taken),
            SwitchPlan::Go
        );
        assert_eq!(
            plan_switch(&local, BackendKind::Remote, LockOutcome::Held),
            SwitchPlan::LibraryInUse,
            "the lock was asked about before the connection was stopped"
        );
        assert_eq!(
            plan_switch(
                &local,
                BackendKind::Remote,
                LockOutcome::Failed("permission denied".to_string())
            ),
            SwitchPlan::LockFailed("permission denied".to_string())
        );
    }

    /// Only a config file that ever said something about a server counts as one, because every field of
    /// the parsed `Config` has a default.
    #[test]
    fn only_a_config_that_names_a_server_offers_one() {
        assert_eq!(server_address_of("listen_port: 6881\n"), None);
        assert_eq!(
            server_address_of(
                "download_dir: /home/alice/Downloads\nlimits:\n  download_kib: 512\n"
            ),
            None,
            "a config written by the TUI names no server"
        );
        assert_eq!(
            server_address_of("server:\n  bind: \"0.0.0.0:9000\"\n"),
            Some(bind("0.0.0.0:9000"))
        );
        // A section with nothing under it, or an address this run cannot read, is the default address:
        // that is what a server started from this file would listen on anyway.
        assert_eq!(server_address_of("server:\n"), Some(default_bind()));
        assert_eq!(
            server_address_of("server:\n  bind: not-an-address\n"),
            Some(default_bind())
        );
        assert_eq!(
            server_address_of("server: [unterminated\n"),
            None,
            "not YAML at all"
        );
    }

    /// The file half of the same question, on a scratch library.
    #[test]
    fn named_server_reads_the_config_file() {
        let dir = ScratchDir::new("named_server");
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        assert_eq!(
            named_server(&paths),
            None,
            "no config file yet names no server"
        );

        fs::create_dir_all(&paths.config_dir).expect("the config directory");
        fs::write(paths.config_file(), "listen_port: 6881\n").expect("write a config");
        assert_eq!(named_server(&paths), None);

        fs::write(paths.config_file(), "server:\n  bind: \"127.0.0.1:8090\"\n")
            .expect("write a config naming a server");
        assert_eq!(named_server(&paths), Some(bind("127.0.0.1:8090")));
    }

    fn bind(text: &str) -> SocketAddr {
        text.parse().expect("{text} is an address")
    }

    fn some_bind(text: &str) -> Option<SocketAddr> {
        Some(bind(text))
    }

    /// The address a server with no `bind` in its config listens on, as the decision above uses it.
    fn default_bind() -> SocketAddr {
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 8090)
    }
}
