//! A blocking question with a deadline: what the shell asks the session bus before the window exists.
//!
//! On Linux the keychain probe (the Secret Service) and the tray check (a StatusNotifier host) are calls
//! over the session bus that block, and they run on the main thread before the window is made. A wedged
//! bus would hang the app there with nothing on screen. [`answer_within`] runs such a call on a thread of
//! its own and stops waiting at the deadline: a call that has not answered by then counts as "no", and
//! its thread is left to finish (or not) on its own. This computer's start asks the keychain for the proxy
//! password the same way, on every system.

use std::sync::mpsc;
use std::time::Duration;

/// How long the shell waits for the session bus before it goes on without its answer.
pub const SESSION_BUS_DEADLINE: Duration = Duration::from_secs(2);

/// What `question` answers, when it answers within `deadline`; `None` when it does not (or its thread
/// could not start, or it panicked).
pub fn answer_within<T: Send + 'static>(
    deadline: Duration,
    question: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("session-bus-question".to_string())
        .spawn(move || {
            // The receiver may be gone by now: the deadline passed, and nobody wants the answer.
            let _ = send.send(question());
        })
        .ok()?;
    receive.recv_timeout(deadline).ok()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::answer_within;

    #[test]
    fn a_prompt_answer_is_kept() {
        assert_eq!(answer_within(Duration::from_secs(5), || 7), Some(7));
    }

    #[test]
    fn a_question_past_the_deadline_counts_as_no_answer() {
        let started = Instant::now();
        let answer = answer_within(Duration::from_millis(100), || {
            std::thread::sleep(Duration::from_secs(5));
            true
        });
        assert_eq!(answer, None);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the wait outlived its deadline"
        );
    }

    #[test]
    fn a_question_that_panics_counts_as_no_answer() {
        let answer: Option<bool> = answer_within(Duration::from_secs(5), || panic!("wedged"));
        assert_eq!(answer, None);
    }
}
