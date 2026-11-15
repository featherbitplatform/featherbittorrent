//! The one bit that says this client is on its way out.
//!
//! It lives apart from both halves it connects because it belongs to neither: the relay task in
//! [`crate::remote`] sets it when a `Shutdown` arrives, and everything in [`crate::remote::http`] that is
//! waiting on a server stops waiting because of it. Without it, pressing stop means queueing behind a
//! request that a silent server is still holding open for its whole timeout - thirty seconds of somebody
//! else's patience between pressing a key and the connection closing.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// The line every interrupted wait reports. It is a transport failure, because that is the shape the refresh
/// loop already knows what to do with: stop waiting on this server, and keep the last snapshot as it is.
pub(crate) const STOPPED: &str = "this client is stopping";

/// Set once, when this client starts stopping, so that nothing still waiting on a server keeps waiting.
///
/// A flag and a `Notify` rather than a flag alone: a caller already parked inside a `select!` has to be
/// woken, and a caller that begins waiting afterwards has to see the answer. `notify_waiters` reaches only
/// the waiters there are at the moment it is called, so from then on the flag carries it.
#[derive(Debug, Default)]
pub(crate) struct Stopping {
    stopped: AtomicBool,
    wake: Notify,
}

impl Stopping {
    /// Says, once and for everything waiting on this connection, that it is stopping.
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.wake.notify_waiters();
    }

    /// Whether somebody has already asked to stop. Cheap enough to ask between chunks of a body.
    pub(crate) fn stopping(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Completes once this client is stopping, and immediately when it already is.
    pub(crate) async fn waited(&self) {
        let wait = self.wake.notified();
        tokio::pin!(wait);
        // Interest is registered before the last look at the flag, so a stop that lands between the two
        // cannot be missed: `enable` is what makes a `notified` future interested without polling it.
        wait.as_mut().enable();
        if self.stopping() {
            return;
        }
        wait.await;
    }
}

#[cfg(test)]
mod tests {
    use super::Stopping;
    use std::time::Duration;

    /// The two orders a stop can arrive in - while somebody is parked, and before they start waiting - both
    /// have to be answered, because the first is woken by the `Notify` and the second only by the flag.
    #[tokio::test]
    async fn a_stop_is_answered_whenever_it_arrives() {
        let stopping = std::sync::Arc::new(Stopping::default());
        let parked = {
            let stopping = std::sync::Arc::clone(&stopping);
            tokio::spawn(async move { stopping.waited().await })
        };
        // A moment to park: `waited` on a flag nobody has set waits for a wake, so getting ahead of this
        // task is the case under test rather than a thing to arrange afterwards.
        tokio::time::sleep(Duration::from_millis(20)).await;
        stopping.stop();
        tokio::time::timeout(Duration::from_secs(1), parked)
            .await
            .expect("a stop wakes whoever is waiting")
            .expect("the waiter ran");

        // A caller that arrives after the fact does not park at all.
        let late = tokio::time::timeout(Duration::from_millis(50), stopping.waited()).await;
        assert!(
            late.is_ok(),
            "a stop already given is a wait already answered"
        );
    }
}
