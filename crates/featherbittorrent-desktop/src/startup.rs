//! This computer's start, off the main thread, and what the rest of the shell waits on.
//!
//! The setup spawns the start on Tauri's async runtime and returns at once, so the main thread keeps
//! pumping its event loop: a second launch forwarding its links (which on Windows waits on this thread
//! with a timeout) is answered while the engine is still starting, and nothing modal ever runs inside the
//! loop's own dispatch (on Linux a native message box there waits on the loop it is blocking).
//!
//! [`Startup`] is the state the rest reads: starting, running, failed, or stopped. The page's `connect`
//! for This computer waits for it ([`Startup::endpoint`], at most [`START_WAIT`]); a quit waits for it too
//! ([`Startup::take_when_settled`]), so a quit while starting still stops the engine and runs its final
//! save. A failed start is not said in a native dialog: it is kept ([`StartProblem`]), the window is
//! shown, and the page asks for it with [`startup_problem`] (again while the start is still running) and
//! draws it with a Quit button. Quitting then exits with [`StartProblem::exit_code`]: 3 when the library
//! is held by another run, as the terminal client does, 1 otherwise; so does a system exit (logout,
//! shutdown) after a failed start.
//!
//! A serving restart ([`crate::lifecycle::Lifecycle::restart_this_computer`]) goes through the same
//! state: it waits for the first start to settle, takes the engine out ([`Startup::begin_restart`], which
//! makes the phase starting again so a connect waits for the new engine), and puts the new one in
//! ([`Startup::finish_restart`]).

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::Notify;

use crate::lifecycle;
use crate::local::{self, ThisComputer};

/// The longest the page's connect, or the question about a failed start, waits for This computer.
pub const START_WAIT: Duration = Duration::from_secs(60);

/// What `connect` answers when This computer took longer than [`START_WAIT`].
pub const START_TOO_SLOW: &str = "This computer did not start in time.";

/// The exit code for any other failed start.
pub const EXIT_PROBLEM: i32 = 1;

/// Why This computer did not start: the line to show, and the code a quit then exits with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartProblem {
    pub message: String,
    #[serde(skip)]
    pub exit_code: i32,
}

impl StartProblem {
    /// The problem `local::start_with` reported. The library held by another run is the one with its
    /// own code.
    pub fn from_start_error(message: String) -> Self {
        let exit_code = if message == local::ALREADY_RUNNING {
            local::EXIT_ALREADY_RUNNING
        } else {
            EXIT_PROBLEM
        };
        StartProblem { message, exit_code }
    }
}

/// What the page's [`startup_problem`] answers: the start is still under way (ask again), it went fine,
/// or it failed with the line to show. Serialized as `{ state: "starting" }`, `{ state: "fine" }` or
/// `{ state: "problem", message }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum StartupStatus {
    Starting,
    Fine,
    Problem { message: String },
}

enum Phase {
    Starting,
    Running(ThisComputer),
    Failed(StartProblem),
    /// Taken out to stop; nothing runs any more.
    Stopped,
}

/// This computer's start and, once it finished, the engine or why there is none.
pub struct Startup {
    phase: Mutex<Phase>,
    settled: Notify,
}

impl std::fmt::Debug for Startup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Startup").finish_non_exhaustive()
    }
}

impl Startup {
    /// A start under way.
    pub fn starting() -> Self {
        Startup {
            phase: Mutex::new(Phase::Starting),
            settled: Notify::new(),
        }
    }

    /// The start finished: the engine, or the problem. Wakes everything waiting on it. A start that
    /// finishes after a quit already gave up on it is stopped by the caller, never kept.
    pub fn finish(&self, result: Result<ThisComputer, StartProblem>) -> Option<ThisComputer> {
        let mut phase = self.lock();
        let leftover = match (&*phase, result) {
            (Phase::Starting, Ok(this)) => {
                *phase = Phase::Running(this);
                None
            }
            (Phase::Starting, Err(problem)) => {
                *phase = Phase::Failed(problem);
                None
            }
            (_, Ok(this)) => Some(this),
            (_, Err(_)) => None,
        };
        drop(phase);
        self.settled.notify_waiters();
        leftover
    }

    /// Waits until the start is no longer under way, for at most `limit`. True when it settled.
    pub async fn settled(&self, limit: Duration) -> bool {
        let wait = async {
            loop {
                let notified = self.settled.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if !matches!(*self.lock(), Phase::Starting) {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(limit, wait).await.is_ok()
    }

    /// This computer's base address and secret, once it runs; the line saying why not otherwise. Waits
    /// for a start under way, at most `limit`.
    pub async fn endpoint(&self, limit: Duration) -> Result<(String, String), String> {
        if !self.settled(limit).await {
            return Err(START_TOO_SLOW.to_string());
        }
        match &*self.lock() {
            Phase::Running(this) => Ok(this.endpoint()),
            Phase::Failed(problem) => Err(problem.message.clone()),
            Phase::Starting => Err(START_TOO_SLOW.to_string()),
            Phase::Stopped => Err("This computer is not running.".to_string()),
        }
    }

    /// Why the start failed, once it settled within `limit`; `None` when it runs (or is still starting).
    /// The tests' view; the page asks [`Startup::status`].
    #[cfg(test)]
    pub async fn problem(&self, limit: Duration) -> Option<StartProblem> {
        self.settled(limit).await;
        match &*self.lock() {
            Phase::Failed(problem) => Some(problem.clone()),
            _ => None,
        }
    }

    /// What the page is told about the start after waiting for it at most `limit`: still starting (the
    /// page asks again), fine, or the problem. A start that takes longer than one wait is not a problem.
    pub async fn status(&self, limit: Duration) -> StartupStatus {
        self.settled(limit).await;
        match &*self.lock() {
            Phase::Starting => StartupStatus::Starting,
            Phase::Failed(problem) => StartupStatus::Problem {
                message: problem.message.clone(),
            },
            Phase::Running(_) | Phase::Stopped => StartupStatus::Fine,
        }
    }

    /// `read` applied to the engine while it runs; `None` while it starts, restarts, failed or stopped.
    pub fn with_running<T>(&self, read: impl FnOnce(&ThisComputer) -> T) -> Option<T> {
        match &*self.lock() {
            Phase::Running(this) => Some(read(this)),
            _ => None,
        }
    }

    /// The start of a restart (Serve the web UI changed): the running engine is taken out to be stopped
    /// and the phase is starting again, so a connect meanwhile waits for the new engine rather than
    /// answering the old secret. `None` when there was no engine to take (a restart that could not start
    /// one before). A failed first start is not restarted: its line is the answer. The caller has waited
    /// for the first start to settle and holds the engine lock ([`crate::lifecycle::Lifecycle`]).
    pub fn begin_restart(&self) -> Result<Option<ThisComputer>, String> {
        let mut phase = self.lock();
        match std::mem::replace(&mut *phase, Phase::Starting) {
            Phase::Running(this) => Ok(Some(this)),
            Phase::Stopped => Ok(None),
            Phase::Failed(problem) => {
                let message = problem.message.clone();
                *phase = Phase::Failed(problem);
                Err(message)
            }
            Phase::Starting => Err(START_TOO_SLOW.to_string()),
        }
    }

    /// The end of a restart: the new engine runs, or there is none (stopped). Wakes everything waiting.
    pub fn finish_restart(&self, running: Option<ThisComputer>) {
        *self.lock() = match running {
            Some(this) => Phase::Running(this),
            None => Phase::Stopped,
        };
        self.settled.notify_waiters();
    }

    /// Takes This computer out to stop it, waiting for a start under way (at most `limit`): a quit while
    /// starting still stops the engine and saves. `None` when there is none to stop. The code the app
    /// exits with is [`Startup::exit_code`], which a take leaves as it was.
    pub async fn take_when_settled(&self, limit: Duration) -> Option<ThisComputer> {
        self.settled(limit).await;
        let mut phase = self.lock();
        match std::mem::replace(&mut *phase, Phase::Stopped) {
            Phase::Running(this) => Some(this),
            Phase::Failed(problem) => {
                *phase = Phase::Failed(problem);
                None
            }
            _ => None,
        }
    }

    /// The code a quit exits with: the failed start's, 0 otherwise.
    pub fn exit_code(&self) -> i32 {
        match &*self.lock() {
            Phase::Failed(problem) => problem.exit_code,
            _ => 0,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Phase> {
        self.phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Starts This computer on the async runtime and returns at once. A failure is kept for the page and
/// the window is shown, whatever the launch asked for, so the line is seen.
pub fn spawn<R: Runtime>(
    app: &AppHandle<R>,
    start: impl Future<Output = Result<ThisComputer, String>> + Send + 'static,
) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = start.await;
        let failed = result.is_err();
        if let Err(problem) = &result {
            eprintln!("{problem}");
        }
        let leftover = app
            .state::<crate::commands::DesktopState>()
            .startup()
            .finish(result.map_err(StartProblem::from_start_error));
        if let Some(this_computer) = leftover {
            this_computer.stop().await;
        }
        if failed {
            lifecycle::show_window(&app);
        }
    });
}

/// How This computer's start went, after waiting for it at most [`START_WAIT`]: still starting (the page
/// asks again), fine, or the problem the page shows with Quit.
#[tauri::command]
pub async fn startup_problem(
    state: tauri::State<'_, crate::commands::DesktopState>,
) -> Result<StartupStatus, String> {
    Ok(state.startup().status(START_WAIT).await)
}

/// Quits the app: what the page's Quit does on the failed-start screen. The exit code is the failed
/// start's ([`StartProblem::exit_code`]).
#[tauri::command]
pub fn quit_app(app: AppHandle) {
    lifecycle::quit(&app);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use fbtt_core::scratch::ScratchDir;

    use super::{START_TOO_SLOW, StartProblem, Startup, StartupStatus};
    use crate::local::tests::{scratch_paths, start_offline};
    use crate::local::{ALREADY_RUNNING, EXIT_ALREADY_RUNNING};

    const LONG: Duration = Duration::from_secs(30);

    /// A connect waits for the start under way and answers with the engine once it runs; one that waits
    /// past its limit says so rather than hanging.
    #[tokio::test(flavor = "multi_thread")]
    async fn connect_waits_for_the_start() {
        let dir = ScratchDir::new("desktop-startup-waits");
        let paths = scratch_paths(&dir);
        let startup = Arc::new(Startup::starting());

        assert_eq!(
            startup.endpoint(Duration::from_millis(50)).await,
            Err(START_TOO_SLOW.to_string()),
            "a start that has not finished is not waited on past the limit"
        );

        let waiting = {
            let startup = startup.clone();
            tokio::spawn(async move { startup.endpoint(LONG).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "connect waits while starting");

        let this = start_offline(&paths).await.expect("This computer starts");
        let expected = this.endpoint();
        assert!(startup.finish(Ok(this)).is_none());
        assert_eq!(waiting.await.expect("joins"), Ok(expected));
        assert_eq!(startup.exit_code(), 0);

        startup
            .take_when_settled(LONG)
            .await
            .expect("running")
            .stop()
            .await;
    }

    /// A failed start is kept with its line and its exit code: 3 for the library another run holds,
    /// 1 for anything else. Connect answers the line.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_start_is_kept_with_its_exit_code() {
        let held = Startup::starting();
        held.finish(Err(StartProblem::from_start_error(
            ALREADY_RUNNING.to_string(),
        )));
        assert_eq!(held.endpoint(LONG).await, Err(ALREADY_RUNNING.to_string()));
        let problem = held.problem(LONG).await.expect("a problem");
        assert_eq!(problem.message, ALREADY_RUNNING);
        assert_eq!(held.exit_code(), EXIT_ALREADY_RUNNING);
        assert!(held.take_when_settled(LONG).await.is_none());
        assert_eq!(
            held.exit_code(),
            EXIT_ALREADY_RUNNING,
            "a quit keeps the code"
        );

        let other = Startup::starting();
        other.finish(Err(StartProblem::from_start_error(
            "This computer could not start: no port".to_string(),
        )));
        assert_eq!(other.exit_code(), 1);
        let json = serde_json::to_value(other.problem(LONG).await).expect("serializes");
        assert_eq!(json["message"], "This computer could not start: no port");

        let running = Startup::starting();
        assert!(running.problem(Duration::from_millis(20)).await.is_none());
    }

    /// The page's question has three answers: still starting after its wait (it asks again rather than
    /// giving up), fine once it runs, and the problem with its line once a start failed.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_startup_status_says_starting_fine_or_problem() {
        let dir = ScratchDir::new("desktop-startup-status");
        let paths = scratch_paths(&dir);
        let startup = Startup::starting();
        assert_eq!(
            startup.status(Duration::from_millis(20)).await,
            StartupStatus::Starting
        );
        let json = serde_json::to_value(StartupStatus::Starting).expect("serializes");
        assert_eq!(json, serde_json::json!({ "state": "starting" }));

        let this = start_offline(&paths).await.expect("This computer starts");
        startup.finish(Ok(this));
        assert_eq!(startup.status(LONG).await, StartupStatus::Fine);
        assert_eq!(
            serde_json::to_value(StartupStatus::Fine).expect("serializes"),
            serde_json::json!({ "state": "fine" })
        );
        startup
            .take_when_settled(LONG)
            .await
            .expect("running")
            .stop()
            .await;

        let failed = Startup::starting();
        failed.finish(Err(StartProblem::from_start_error(
            ALREADY_RUNNING.to_string(),
        )));
        let status = failed.status(LONG).await;
        assert_eq!(
            status,
            StartupStatus::Problem {
                message: ALREADY_RUNNING.to_string()
            }
        );
        assert_eq!(
            serde_json::to_value(status).expect("serializes"),
            serde_json::json!({ "state": "problem", "message": ALREADY_RUNNING })
        );
    }

    /// A quit while This computer is starting waits for the start, then gets the engine to stop: its final
    /// save runs and the library is free again afterwards.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_quit_while_starting_waits_and_saves() {
        let dir = ScratchDir::new("desktop-startup-quit");
        let paths = scratch_paths(&dir);
        let startup = Arc::new(Startup::starting());

        let quitting = {
            let startup = startup.clone();
            tokio::spawn(async move {
                match startup.take_when_settled(LONG).await {
                    Some(this) => {
                        this.stop().await;
                        true
                    }
                    None => false,
                }
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!quitting.is_finished(), "the quit waits for the start");

        let this = start_offline(&paths).await.expect("This computer starts");
        startup.finish(Ok(this));
        assert!(
            quitting.await.expect("joins"),
            "the quit stopped the engine"
        );

        // Stopped means the lock is let go: the same library starts again.
        let again = start_offline(&paths)
            .await
            .expect("the library is free after the stop");
        again.stop().await;

        // A start that finishes after the quit took everything is handed back to be stopped.
        let late = Startup::starting();
        assert!(
            late.take_when_settled(Duration::from_millis(20))
                .await
                .is_none()
        );
        let this = start_offline(&paths).await.expect("starts");
        let leftover = late.finish(Ok(this)).expect("handed back, not kept");
        leftover.stop().await;
    }
}
