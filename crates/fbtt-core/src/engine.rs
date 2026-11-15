//! The engine every featherbittorrent run drives: one `fbt::Session`, its state file, and the
//! library lock that keeps a second run off both.
//!
//! [`LocalBackend`](crate::local) and the server (`featherbittorrent serve`) do different jobs on
//! top of the same engine - one answers a terminal, the other an API - but the rules about the
//! library are identical, so they live here once:
//!
//! * The state file is replaced in one step: bytes go into a temp file beside it, are synced, and
//!   only then renamed over it, the way `config.yaml` is written. A crash leaves either the old
//!   state or the new one, never half of each.
//! * A state file this run cannot use is kept, never overwritten: bytes that do not decode move
//!   aside to `session.state.bad`, and a run that could not read what was under the name - or
//!   could not move it, or could not copy out resume data it was about to save over - writes no
//!   state at all for the rest of the run (`writes_blocked`). An empty library saved over bytes
//!   nobody read is worse than a library someone has to re-add by hand.
//! * The lock goes last, after the final save, so another featherbittorrent starts on either the
//!   old state or the new one and never beside a half-written file.
//!
//! What this module does not own is the snapshot: what the engine looks like to a screen belongs
//! to whoever shows it ([`crate::local`]), and the crate-internal `map` submodule inside here is
//! how the engine's words become table rows.

use std::time::Duration;

use fbt::{InfoHashes, Session, SessionError, SessionState, Settings};
use tokio::sync::mpsc;

use crate::client::Notice;
use crate::config::replace_file;
use crate::lock::LibraryLock;
use crate::paths::Paths;

/// What the engine reports about torrents: `TorrentStatus` to a table row, and the stalled clock
/// behind that column. Crate-internal: the local backend draws from it, a server has its own
/// shapes to fill.
pub(crate) mod map;
mod state;

/// How often a run is expected to call [`Engine::save_periodic`], so a crash costs no more than
/// re-checking what was written since the last save.
pub const SAVE_EVERY: Duration = Duration::from_secs(300);

/// The engine for one library: the session, the file its state goes to, and the lock held until
/// that file has been written.
///
/// Start one with [`Engine::start`], drive `session()`, call [`save_periodic`](Self::save_periodic)
/// every [`SAVE_EVERY`], and end it with [`shutdown`](Self::shutdown), which saves and releases the
/// library. It is not `Clone` and cannot be started twice on one library: the lock says no first.
#[derive(Debug)]
pub struct Engine {
    /// The only session of this library, and only its owner touches it.
    session: Session,
    /// Where `session.state` is.
    paths: Paths,
    /// Set when this run must not write `session.state` at all: something is under that name this
    /// run could not read, or could not move aside, or could not copy out before saving over it.
    /// Saving would replace bytes this run never read with a library of its own.
    writes_blocked: bool,
    /// Where one-line reports about individual torrents go (a torrent left out of a save): the
    /// screen's notice queue for the local backend, nothing at all for a caller that has no
    /// channel to put them in. See [`report_to`](Self::report_to).
    reporter: Option<mpsc::Sender<Notice>>,
    /// This library's lock, taken before the session started and never read: holding it is the
    /// whole job. [`shutdown`](Self::shutdown) drops it last, after the state file; a run that
    /// never gets there - killed, or panicked - drops it with the process, which is also safe
    /// because no file was half-written.
    _lock: LibraryLock,
}

impl Engine {
    /// Starts the engine on `paths`, restoring whatever `session.state` holds, on `settings`.
    ///
    /// `lock` is this library's, taken with [`LibraryLock::acquire`]: the engine holds it until
    /// [`shutdown`](Self::shutdown) writes the state. A state file that cannot be read is left
    /// where it is and this run stops writing it; one that does not decode is kept aside under
    /// another name. Resume data that will not come back is copied out from under it before any
    /// save happens, and a run that cannot make that copy writes no state either. All of that
    /// arrives as the returned notices, one line each, because whoever starts the engine has the
    /// channel they belong to and this does not.
    pub async fn start(
        settings: Settings,
        paths: &Paths,
        lock: LibraryLock,
    ) -> Result<(Engine, Vec<Notice>), SessionError> {
        let reading = state::read_state(&paths.state_file());
        let (session, failed) = match &reading.state {
            Some(loaded) => Session::restore(settings, loaded).await?,
            None => (Session::new(settings).await?, Vec::new()),
        };

        // One line each for what went wrong with the saved state. A restore reports one per
        // torrent it could not add, which is how many there can be.
        let mut problems = reading.notices;
        // A run with nothing it is allowed to write about: set by a state file this run could not
        // read, by one it could not move aside, and by a copy of the bytes it could not restore
        // that it failed to keep. All three mean the same thing - writing would replace bytes
        // nobody has.
        let mut writes_blocked = reading.writes_blocked;
        if let Some(loaded) = &reading.state {
            // Torrents that did not come back deserve kept bytes: the save this run makes describes
            // only what it has, so without a copy those torrents would leave the library twice
            // over - once unseen by the session and once overwritten in the file that described
            // them.
            let kept = if failed.is_empty() {
                None
            } else {
                match state::keep_copy(&paths.state_file(), &reading.bytes) {
                    Ok(copy) => Some(copy),
                    Err(problem) => {
                        // The save that follows would erase the only bytes those torrents have,
                        // and this run could not keep them: same answer as a state file it could
                        // not read.
                        writes_blocked = true;
                        problems.push(Notice::Error(format!(
                            "the copy of what could not be restored could not be kept ({problem}); \
                             this run will not save torrents"
                        )));
                        None
                    }
                }
            };
            problems.extend(failed.into_iter().map(|(index, error)| {
                Notice::Error(state::restored_problem(
                    loaded,
                    index,
                    &error,
                    kept.as_deref(),
                ))
            }));
        }

        Ok((
            Engine {
                session,
                paths: paths.clone(),
                writes_blocked,
                reporter: None,
                _lock: lock,
            },
            problems,
        ))
    }

    /// The session this engine drives: the one place a torrent handle comes from. Cloning it is
    /// cheap; driving it from two places is not what it is for, which is why the engine that owns
    /// the library owns this.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Sends this engine's reports about individual torrents - one line per torrent a save left
    /// out - to `notices`, instead of dropping them.
    ///
    /// The methods' own return values carry one line each: what the *save* did. A save can say more
    /// than that about the torrents it could not take resume data for, and those lines belong on
    /// the screen a person is watching. They are dropped when the queue is full or gone: a backend
    /// must not wait on a screen that has stopped reading.
    pub fn report_to(&mut self, notices: mpsc::Sender<Notice>) {
        self.reporter = Some(notices);
    }

    /// Writes the state while the run goes on, so a crash costs a re-check and nothing else. A run
    /// that could not read what was under the state file writes nothing at all: its empty library
    /// is not evidence about what the file held.
    ///
    /// Call it every [`SAVE_EVERY`]. The `Option` is this save's own failure - resume data the
    /// session would not give, or bytes that did not reach the file - as one line already fit to
    /// print; `None` means the save itself worked. What the save left *out*, torrent by torrent,
    /// goes to the reporter attached with [`report_to`](Self::report_to).
    pub async fn save_periodic(&mut self) -> Option<Notice> {
        if self.writes_blocked {
            return None;
        }
        match self.session.save_state().await {
            Ok((state, left_out)) => {
                self.report_left_out(&left_out);
                match self.write_state(&state).await {
                    Ok(()) => None,
                    Err(problem) => Some(Notice::Error(problem)),
                }
            }
            Err(error) => Some(Notice::Error(map::clean(&error.to_string()))),
        }
    }

    /// The last save, and the engine's answer. `shutdown_and_save` stops every torrent with its
    /// bytes written first, so what is saved here is trusted next run and nothing has to be
    /// re-checked. A failure here cannot be fixed from in here - the library on disk stays the one
    /// from before this run - so it goes out as the message whoever started the engine prints.
    ///
    /// The engine is consumed: its session goes with it, and the lock goes last, after the state
    /// file was written or failed to be. Another featherbittorrent may start on the new state from
    /// that moment, never beside a half-written one. Torrents a save left out are reported to the
    /// reporter as they are in [`save_periodic`](Self::save_periodic).
    pub async fn shutdown(self) -> Result<(), String> {
        if self.writes_blocked {
            // Nothing to write, but the torrents still have to be stopped: dropping a session
            // closes its sockets and leaves every torrent task running with its cached writes
            // unflushed, and this is where the library lock goes out - the next featherbittorrent
            // would find one library being written by two. `shutdown` stops each torrent with its
            // bytes flushed and does not produce resume data this run has nowhere to put. Whatever
            // is under `session.state` stays exactly as found.
            self.session.shutdown().await;
            return Ok(());
        }
        let (state, left_out) = self.session.shutdown_and_save().await;
        self.report_left_out(&left_out);
        // The engine - its session and this library's lock - goes when this returns, which is
        // after the state file was written or failed to be.
        self.write_state(&state).await
    }

    /// Puts the state where the next run will find it. The bytes are written beside the file,
    /// synced and renamed over it on a blocking thread, so a crash leaves either the old state or
    /// the new one and never half of either. The `Err` is one line already fit to print: whoever
    /// asked for this save either reports it as a notice or carries it out for the person running
    /// it.
    async fn write_state(&self, state: &SessionState) -> Result<(), String> {
        let path = self.paths.state_file();
        let bytes = state.to_bytes();
        let outcome = tokio::task::spawn_blocking(move || {
            replace_file(&path, &bytes)
                .map_err(|source| format!("cannot write {}: {source}", path.display()))
        })
        .await;
        match outcome {
            Ok(outcome) => outcome.map_err(|problem| map::clean(&problem)),
            Err(joined) => Err(format!("the session state could not be written: {joined}")),
        }
    }

    /// A torrent whose resume data could not be taken is out of the saved state: it will not be
    /// here next run, which is worth saying. One line each goes to the reporter when one was
    /// attached with [`report_to`](Self::report_to); a caller that attached none does not hear
    /// them, so `None` from [`save_periodic`](Self::save_periodic) or `Ok` from
    /// [`shutdown`](Self::shutdown) means only that the save itself worked.
    fn report_left_out(&self, left_out: &[(InfoHashes, SessionError)]) {
        let Some(reporter) = &self.reporter else {
            return;
        };
        for (hashes, error) in left_out {
            let _ = reporter.try_send(Notice::Error(map::left_out(hashes, error)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Engine;
    use crate::lock::LibraryLock;
    use crate::paths::Paths;
    use crate::scratch::ScratchDir;
    use fbt::{AddTorrent, Magnet};

    /// The settings an engine test runs on: loopback on a port the OS picks, and nothing that
    /// could leave the machine.
    fn loopback() -> fbt::Settings {
        let mut settings = fbt::Settings::default();
        settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
        settings.dht_enabled = false;
        settings.enable_lsd = false;
        settings.enable_upnp = false;
        settings.enable_natpmp = false;
        settings
    }

    /// What one run's engine added, the next run's engine knows. `start` restores what is under
    /// the state file, `save_periodic` and `shutdown` write it, and the lock goes last: the
    /// second start can only take the library once the first run's file is on disk.
    #[tokio::test(flavor = "multi_thread")]
    async fn engine_round_trip() {
        let dir = ScratchDir::new("engine_round_trip");
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        let download_dir = dir.path().join("downloads");
        std::fs::create_dir_all(&download_dir).expect("a download directory");

        let uri =
            "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=engine-round-trip";
        let magnet = Magnet::parse(uri).expect("a magnet with a made-up hash");
        let wire = magnet.info_hashes.wire_hash();

        let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
        let (mut engine, notices) = Engine::start(loopback(), &paths, lock)
            .await
            .expect("start an engine on a library nobody has used");
        assert!(
            notices.is_empty(),
            "a fresh library has nothing to complain about: {notices:?}"
        );

        // The session is the engine's own, and adding through it is what a command does. Nothing
        // is written until something asks for a save.
        engine
            .session()
            .add_torrent(AddTorrent::magnet(magnet, download_dir))
            .await
            .expect("add a torrent to the engine's session");
        let state_file = paths.state_file();
        assert!(
            !state_file.exists(),
            "starting an engine does not write a state file"
        );

        let quiet = engine.save_periodic().await;
        assert!(
            quiet.is_none(),
            "a save that worked has nothing to say: {quiet:?}"
        );
        assert!(state_file.exists(), "the periodic save wrote the state");

        engine
            .shutdown()
            .await
            .expect("the last save ran without a problem");
        assert!(state_file.exists(), "shutdown leaves a state file behind");

        // The same directories, a second engine: the lock is free because `shutdown` released it,
        // and the torrent is back out of the file.
        let lock = LibraryLock::acquire(&paths.lock_file()).expect("the library came free");
        let (engine, notices) = Engine::start(loopback(), &paths, lock)
            .await
            .expect("start again on the saved state");
        assert!(
            notices.is_empty(),
            "nothing was left out of the state this run wrote: {notices:?}"
        );
        let restored = engine
            .session()
            .torrents_status()
            .into_iter()
            .find(|status| status.info_hashes.wire_hash() == wire)
            .expect("the torrent the first run added is back");
        assert_eq!(restored.name, "engine-round-trip", "its name came with it");

        engine
            .shutdown()
            .await
            .expect("the second run saved without a problem too");
    }
}
