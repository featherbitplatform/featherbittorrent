//! The one worker that talks to the keychain the proxy password lives in.
//!
//! A system keychain can take its time: a Secret Service that hangs, a collection waiting on an unlock
//! prompt. A caller waits at most [`KEYCHAIN_DEADLINE`] for its answer, but the call itself cannot be
//! taken back, so it may still land later. If every call ran on a thread of its own, late calls would land
//! in any order: a rollback could land before the write it undoes. So one thread owns the entry and runs
//! every `get`, `set` and `clear` in the order they were handed in, the desktop app's start and the
//! server's routes alike. A rollback queued behind a write that hung lands after it.
//!
//! Nothing here logs or returns the password, and [`KeychainWorker`]'s `Debug` never prints it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::ProxyPasswordStore;

/// How long a caller waits for the keychain before it goes on without its answer: the desktop app's own
/// deadline for the session bus.
pub const KEYCHAIN_DEADLINE: Duration = Duration::from_secs(2);

/// What a caller says when its keychain call ran past [`KEYCHAIN_DEADLINE`]: the call is still queued or
/// running, and will land when the keychain answers.
pub const KEYCHAIN_TIMED_OUT: &str =
    "The system keychain did not answer in time, so the saved proxy password may still change.";

/// What a proxy save answers while the worker still has calls from an earlier request.
pub const KEYCHAIN_BUSY: &str =
    "The system keychain is still busy with the last change. Try again in a moment.";

/// Why a keychain call has no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeychainCallError {
    /// No answer within [`KEYCHAIN_DEADLINE`]; the call stays in the queue and may still land.
    TimedOut,
    /// The worker is gone (its thread could not start, or a call panicked).
    Gone,
    /// The keychain answered that it would not: its own line, which carries no password.
    Refused(String),
}

impl KeychainCallError {
    /// The line to show.
    pub fn message(&self) -> String {
        match self {
            KeychainCallError::TimedOut => KEYCHAIN_TIMED_OUT.to_string(),
            KeychainCallError::Gone => "the keychain could not be asked".to_string(),
            KeychainCallError::Refused(problem) => problem.clone(),
        }
    }
}

/// One call, as the worker runs it. A password in it is never printed: this type has no `Debug`.
enum Op {
    Get,
    Set(String),
    Clear,
}

/// What a call answers.
enum Answer {
    Get(Option<String>),
    Done(Result<(), String>),
}

/// A call and where its answer goes.
struct Job {
    op: Op,
    reply: mpsc::SyncSender<Answer>,
}

/// The worker: one thread, a FIFO queue, and what the last answer said about whether a password is saved.
pub struct KeychainWorker {
    store: Arc<dyn ProxyPasswordStore>,
    queue: Mutex<Option<mpsc::Sender<Job>>>,
    /// Calls handed in and not finished yet.
    pending: Arc<AtomicUsize>,
    /// Whether a password was saved, as the last finished call left it; `None` before any answer.
    saved: Arc<Mutex<Option<bool>>>,
}

impl std::fmt::Debug for KeychainWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeychainWorker")
            .field("available", &self.store.available())
            .field("pending", &self.pending.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl KeychainWorker {
    /// The worker for `store`, with its thread started.
    pub fn new(store: Arc<dyn ProxyPasswordStore>) -> Arc<Self> {
        let (send, receive) = mpsc::channel::<Job>();
        let pending = Arc::new(AtomicUsize::new(0));
        let saved = Arc::new(Mutex::new(None));
        let running = Arc::clone(&store);
        let (done, remembered) = (Arc::clone(&pending), Arc::clone(&saved));
        let started = std::thread::Builder::new()
            .name("proxy-keychain".to_string())
            .spawn(move || {
                while let Ok(job) = receive.recv() {
                    // A store that panics answers nobody (its caller sees the worker gone) and the queue
                    // goes on, so one bad call cannot leave every later save refused as busy.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run(running.as_ref(), job, &remembered);
                    }));
                    done.fetch_sub(1, Ordering::SeqCst);
                }
            });
        let queue = match started {
            Ok(_) => Some(send),
            Err(problem) => {
                tracing::warn!(problem = %problem, "the keychain worker did not start");
                None
            }
        };
        Arc::new(KeychainWorker {
            store,
            queue: Mutex::new(queue),
            pending,
            saved,
        })
    }

    /// Whether this system has a keychain to save in at all (a value the store keeps, not a call).
    pub fn available(&self) -> bool {
        self.store.available()
    }

    /// Whether calls handed in earlier have not finished yet.
    pub fn busy(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0
    }

    /// Whether a password was saved, as the last finished call left it: what a view shows while the
    /// keychain is busy. `false` before any answer.
    pub fn last_known_saved(&self) -> bool {
        self.saved
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .unwrap_or(false)
    }

    /// The password saved, waiting at most [`KEYCHAIN_DEADLINE`] on this thread.
    pub fn get_blocking(&self) -> Result<Option<String>, KeychainCallError> {
        match wait_blocking(self.submit(Op::Get)?)? {
            Answer::Get(password) => Ok(password),
            Answer::Done(_) => Err(KeychainCallError::Gone),
        }
    }

    /// Saves `password`, waiting at most [`KEYCHAIN_DEADLINE`] on this thread.
    pub fn set_blocking(&self, password: &str) -> Result<(), KeychainCallError> {
        done(wait_blocking(self.submit(Op::Set(password.to_string()))?)?)
    }

    /// The password saved, waiting at most [`KEYCHAIN_DEADLINE`] without holding a runtime thread.
    pub async fn get(&self) -> Result<Option<String>, KeychainCallError> {
        match wait(self.submit(Op::Get)?).await? {
            Answer::Get(password) => Ok(password),
            Answer::Done(_) => Err(KeychainCallError::Gone),
        }
    }

    /// Saves `password` (or takes it out, for `None`), waiting at most [`KEYCHAIN_DEADLINE`].
    pub async fn put(&self, password: Option<&str>) -> Result<(), KeychainCallError> {
        let op = match password {
            Some(password) => Op::Set(password.to_string()),
            None => Op::Clear,
        };
        done(wait(self.submit(op)?).await?)
    }

    /// Hands `op` to the worker's queue, behind every call already there.
    fn submit(&self, op: Op) -> Result<mpsc::Receiver<Answer>, KeychainCallError> {
        let (reply, answer) = mpsc::sync_channel(1);
        let queue = self
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(queue) = queue.as_ref() else {
            return Err(KeychainCallError::Gone);
        };
        self.pending.fetch_add(1, Ordering::SeqCst);
        if queue.send(Job { op, reply }).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            return Err(KeychainCallError::Gone);
        }
        Ok(answer)
    }
}

/// Runs one call on the worker's thread, and remembers whether it left a password saved.
fn run(store: &dyn ProxyPasswordStore, job: Job, saved: &Mutex<Option<bool>>) {
    let answer = match job.op {
        Op::Get => {
            let password = store.get().filter(|password| !password.is_empty());
            remember(saved, password.is_some());
            Answer::Get(password)
        }
        Op::Set(password) => {
            let written = store.set(&password);
            if written.is_ok() {
                remember(saved, true);
            }
            Answer::Done(written)
        }
        Op::Clear => {
            let cleared = store.clear();
            if cleared.is_ok() {
                remember(saved, false);
            }
            Answer::Done(cleared)
        }
    };
    // The caller may have stopped waiting: the answer then has nobody to go to, and the call has landed.
    let _ = job.reply.send(answer);
}

fn remember(saved: &Mutex<Option<bool>>, value: bool) {
    *saved
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(value);
}

fn wait_blocking(answer: mpsc::Receiver<Answer>) -> Result<Answer, KeychainCallError> {
    answer
        .recv_timeout(KEYCHAIN_DEADLINE)
        .map_err(|problem| match problem {
            mpsc::RecvTimeoutError::Timeout => KeychainCallError::TimedOut,
            mpsc::RecvTimeoutError::Disconnected => KeychainCallError::Gone,
        })
}

/// [`wait_blocking`] on the blocking pool, so a runtime thread is never held; it returns within the
/// deadline, so the pool is never held longer either.
async fn wait(answer: mpsc::Receiver<Answer>) -> Result<Answer, KeychainCallError> {
    tokio::task::spawn_blocking(move || wait_blocking(answer))
        .await
        .map_err(|_| KeychainCallError::Gone)?
}

fn done(answer: Answer) -> Result<(), KeychainCallError> {
    match answer {
        Answer::Done(Ok(())) => Ok(()),
        Answer::Done(Err(problem)) => Err(KeychainCallError::Refused(problem)),
        Answer::Get(_) => Err(KeychainCallError::Gone),
    }
}
