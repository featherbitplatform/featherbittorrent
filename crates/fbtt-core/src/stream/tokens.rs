//! The tokens that open one file's stream, and the rules they die by.
//!
//! A token is 32 random bytes, base64url without padding. Only its SHA-256 is kept; the
//! token itself is returned once, on issue, and is a credential from then on. A token
//! opens exactly one file: it is scoped to the one address it was issued for, checked
//! byte for byte, and the address it was issued for is the very one its URL was built
//! from. A token that has never been used dies young; one that is used stays alive as
//! long as a film keeps seeking it, and never beyond eight hours.
//!
//! Everything here is behind one [`std::sync::Mutex`] and nothing inside it waits, so a
//! request that asks to open a file never stalls on the store.

use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::client::Clock;

/// The most a token may be asked to open before fbt-api would not even offer a route a
/// query token: a request's `?token=` is at most 4096 bytes there, and one longer than
/// that is refused here as well.
const MAX_TOKEN_BYTES: usize = 4096;
/// The random bytes behind one token, base64url without padding: 43 characters.
const TOKEN_BYTES: usize = 32;

/// How long a token may go without a first use before it dies.
pub const FIRST_USE: Duration = Duration::from_secs(60);
/// How long a used token may go without a request before it dies.
pub const IDLE: Duration = Duration::from_secs(30 * 60);
/// How long a token lives at most, from its issue, whatever it keeps being used for.
pub const MAX_AGE: Duration = Duration::from_secs(8 * 60 * 60);

/// One token a store hands out, as the store keeps it.
struct Entry {
    /// The token's SHA-256, the only copy of it the store holds.
    hash: [u8; 32],
    /// The one address this token opens, kept to compare byte for byte.
    scope: String,
    /// When it was issued; the eight-hour cap and the first-use window both run from here.
    issued: Instant,
    /// When the idle clock started running: `None` until the first use, then the last request
    /// that was still inside the limit. A check that is inside the limit renews it; one past
    /// it is refused and leaves the clock running, so the token dies of the silence that ended.
    idle_since: Option<Instant>,
}

/// The tokens that open the streams of one endpoint: at most `cap` live, oldest evicted.
///
/// A token is a secret while it lives, so the store's `Debug` counts the tokens in it and
/// says nothing about any of them.
pub struct StreamTokens {
    inner: Mutex<Inner>,
    cap: usize,
    clock: Clock,
}

struct Inner {
    /// The live tokens, oldest first: the head is what a new token evicts when the store
    /// is full.
    entries: std::collections::VecDeque<Entry>,
}

impl StreamTokens {
    /// A store for `cap` live tokens, on the wall clock.
    pub fn new(cap: usize) -> Self {
        Self::with_clock(cap, std::sync::Arc::new(Instant::now))
    }

    /// A store for `cap` live tokens, on `clock`: the process's own in a run, a counter a
    /// test moves by hand in one.
    pub fn with_clock(cap: usize, clock: Clock) -> Self {
        StreamTokens {
            inner: Mutex::new(Inner {
                entries: std::collections::VecDeque::new(),
            }),
            cap: cap.max(1),
            clock,
        }
    }

    /// Issues a token that opens `scope` and returns it: the one copy a caller keeps.
    ///
    /// Before deciding whether the store is full, every expired entry is dropped: a token
    /// that has already died by its first-use window, its idle limit, or its eight-hour cap
    /// does not count against the cap, so a live token is never evicted to make room for
    /// one that is already gone.
    pub fn issue(&self, scope: String) -> String {
        let bytes = fresh_token_bytes();
        let token = BASE64_URL_SAFE_NO_PAD.encode(bytes);
        // The store keeps the token's hash, and a check re-derives it from the token string it is
        // handed, so the hash is of the token string, not of the bytes behind it.
        let hash = sha256(token.as_bytes());
        let now = self.now();
        let mut inner = self.locked();
        // Drop whatever has died before counting against the cap: the same rule `check` uses.
        inner.entries.retain(|entry| !expired(entry, now));
        if inner.entries.len() >= self.cap {
            inner.entries.pop_front();
        }
        inner.entries.push_back(Entry {
            hash,
            scope,
            issued: now,
            idle_since: None,
        });
        token
    }

    /// Whether `token` opens `path`: the token's hash, then its scope compared with `path`
    /// byte for byte. A check that passes marks the token used and renews its idle clock;
    /// one that fails changes nothing.
    pub fn check(&self, token: &str, path: &str) -> bool {
        // Longer than the limit fbt-api offers a route a query token is not a token at all,
        // and is refused before any hash is even looked up.
        if token.len() > MAX_TOKEN_BYTES {
            return false;
        }
        let hash = sha256(token.as_bytes());
        let now = self.now();
        let mut inner = self.locked();
        // Drop whatever has died, oldest first: past its first-use window if it was never
        // used, past its idle limit or its eight-hour cap if it was.
        inner.entries.retain(|entry| !expired(entry, now));
        let Some(slot) = inner.entries.iter().position(|entry| entry.hash == hash) else {
            return false;
        };
        let entry = &inner.entries[slot];
        if entry.scope != path {
            return false;
        }
        // It opens this file. The idle clock is what keeps it alive: a first use inside the
        // window starts it, and every later use that is still inside the limit renews it, so a
        // film that keeps seeking stays open. A use past the limit is refused, and the token is
        // already on its way out of the silence.
        match entry.idle_since {
            None => {
                // First use: it must still be inside the first-use window, and it starts the
                // idle clock.
                if now.duration_since(entry.issued) <= FIRST_USE {
                    inner.entries[slot].idle_since = Some(now)
                } else {
                    return false;
                }
            }
            Some(since) => {
                // A later use is valid while it is inside the idle limit of the last one, and
                // renews the clock.
                if now.duration_since(since) > IDLE {
                    return false;
                }
                inner.entries[slot].idle_since = Some(now);
            }
        }
        true
    }

    /// How many live tokens the store holds: expired entries are dropped before counting,
    /// so the number is the one that matters for the cap.
    pub fn len(&self) -> usize {
        let now = self.now();
        let mut inner = self.locked();
        inner.entries.retain(|entry| !expired(entry, now));
        inner.entries.len()
    }

    /// Whether the store holds no live tokens.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The store's reading of the time, on the clock it was handed.
    fn now(&self) -> Instant {
        (self.clock)()
    }
}

impl fmt::Debug for StreamTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamTokens")
            .field("live", &self.len())
            .finish()
    }
}

/// A random token's bytes, or the store has no way to open a file.
fn fresh_token_bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).expect("the platform has a random source");
    bytes
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out: [u8; 32] = hasher.finalize().into();
    out
}

/// Whether `entry` is past its life at `now`.
fn expired(entry: &Entry, now: Instant) -> bool {
    // The eight-hour cap runs from the issue for every token, used or not, and nothing can
    // keep a token alive past it.
    if now.duration_since(entry.issued) > MAX_AGE {
        return true;
    }
    match entry.idle_since {
        // Never used: it dies at the end of its first-use window.
        None => now.duration_since(entry.issued) > FIRST_USE,
        // Used: it lives while a request keeps coming within the idle limit of its last one.
        Some(since) => now.duration_since(since) > IDLE,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::StreamTokens;
    use crate::client::TorrentId;
    use crate::stream::stream_path;

    /// The one id a test streams: a wire hash in its 40 lowercase hex digits.
    fn id() -> TorrentId {
        TorrentId("0123456789abcdef0123456789abcdef01234567".to_string())
    }

    /// A clock that stands still until a test moves it, and the handle to hand a store.
    /// The same shape [`fbtt-server`]'s login stores are tested with.
    fn clock_at(start: Instant) -> (Arc<Mutex<Instant>>, crate::client::Clock) {
        let handle = Arc::new(Mutex::new(start));
        let shared = Arc::clone(&handle);
        let clock: crate::client::Clock = Arc::new(move || *shared.lock().expect("the test clock"));
        (handle, clock)
    }

    /// Everything a token may be asked to open, and everything it may not: one file, one
    /// spelling of it, and a life that is short when unused and long but bounded when used.
    #[test]
    fn stream_token_rules() {
        let (handle, clock) = clock_at(Instant::now());
        let tokens = StreamTokens::with_clock(64, clock);

        let path = stream_path(&id(), 0);
        let token = tokens.issue(path.clone());
        assert!(
            tokens.check(&token, &path),
            "a token opens the file it was issued for"
        );
        assert!(
            !tokens.check(&token, "/api/v1/torrents/"),
            "it does not open a route it was not issued for"
        );
        // The scope is the address itself, byte for byte: a leading zero in the index, or
        // the id upper-cased, is another file to the token.
        let leading_zero =
            "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/00/stream";
        assert!(
            !tokens.check(&token, leading_zero),
            "a leading zero in the index is refused"
        );
        let upper = "/api/v1/torrents/0123456789ABCDEF0123456789ABCDEF01234567/files/0/stream";
        assert!(!tokens.check(&token, upper), "an upper-cased id is refused");

        // An unused token dies in the first-use window: one second past it, it is gone.
        let unused = tokens.issue("/api/v1/torrents/".to_string());
        *handle.lock().expect("the test clock") += Duration::from_secs(61);
        assert!(
            !tokens.check(&unused, "/api/v1/torrents/"),
            "a token no one used in 61 s is dead"
        );

        // A used token is renewed by its use: a request inside the idle limit keeps it alive,
        // and a silence past the limit ends it. The same token that passed at 29 minutes must
        // fail at 31: this is what pins the idle limit itself.
        let alive = tokens.issue(path.clone());
        assert!(tokens.check(&alive, &path), "first use counts as one");
        *handle.lock().expect("the test clock") += Duration::from_secs(29 * 60);
        assert!(
            tokens.check(&alive, &path),
            "29 minutes of silence is inside the limit"
        );
        *handle.lock().expect("the test clock") += Duration::from_secs(31 * 60);
        assert!(
            !tokens.check(&alive, &path),
            "31 minutes of silence is past the limit"
        );

        // The eight-hour cap runs from the issue, not from the last use: a film that seeks
        // every ten minutes keeps its token alive by the idle limit, and still ends at eight
        // hours. At 7 h 50 m from the issue the token is still inside the cap; at 8 h 1 m
        // it is past it, even though the film kept seeking.
        let film = tokens.issue(path.clone());
        // Advance minute by minute to 7 h 50 m, seeking every ten minutes. Each seeking check
        // renews the idle clock, so the only thing that can end the token is the cap itself.
        for minute in 0..470 {
            if minute % 10 == 0 {
                assert!(
                    tokens.check(&film, &path),
                    "still inside the cap at {minute} minutes"
                );
            }
            *handle.lock().expect("the test clock") += Duration::from_secs(60);
        }
        // Exactly 7 h 50 m from the issue: inside the cap, still alive.
        assert!(
            tokens.check(&film, &path),
            "7 h 50 m is inside the eight-hour cap"
        );
        // Advance the remaining 11 minutes to 8 h 1 m. The film seeks once more at 7 h 50 m
        // + 10 m = 8 h 0 m, which renews the idle clock, but the cap is checked first and
        // ends the token.
        for _ in 0..11 {
            *handle.lock().expect("the test clock") += Duration::from_secs(60);
        }
        // 8 h 1 m from the issue: past the cap, even though the film kept seeking.
        assert!(
            !tokens.check(&film, &path),
            "8 h 1 m is past the eight-hour cap"
        );

        // A store is bounded: the 65th token evicts the oldest, and a token longer than the
        // 4096-byte limit fbt-api offers a route is refused without a panic.
        let first = tokens.issue("/first".to_string());
        for slot in 1..64 {
            tokens.issue(format!("/filler/{slot}"));
        }
        assert_eq!(tokens.len(), 64, "the store is full at its cap");
        tokens.issue("/sixty-fourth".to_string());
        assert_eq!(tokens.len(), 64, "the cap is not exceeded");
        assert!(
            !tokens.check(&first, "/first"),
            "the oldest token is evicted to make room"
        );
        let huge = "x".repeat(4097);
        assert!(
            !tokens.check(&huge, &huge),
            "a 4097-byte token is refused without a panic"
        );

        // `Debug` counts the store; it says nothing about the tokens in it.
        let shown = format!("{tokens:?}");
        assert!(!shown.contains(&path), "the scope is not printed: {shown}");
        assert!(shown.contains("64"), "the count is: {shown}");
    }

    /// `issue` prunes expired entries before counting against the cap, so a live token is
    /// never evicted to make room for one that is already gone. With a cap of 2, B is issued
    /// and used first; A is issued after and never used, so A is the newest entry and
    /// expires after 61 s. Issuing C then must not evict B: without the pruning in
    /// `issue`, the store counts the expired A against the cap and evicts B, the oldest.
    ///
    /// The test does not call `check` or `len` between the expiry and the `issue`: both of
    /// those prune expired entries on their own, and a `check` before the `issue` would
    /// remove A first and let the test pass even if `issue` had no pruning of its own.
    #[test]
    fn an_expired_entry_does_not_count_against_the_cap() {
        let (handle, clock) = clock_at(Instant::now());
        let tokens = StreamTokens::with_clock(2, clock);

        let path_a = stream_path(&id(), 0);
        let path_b = stream_path(&id(), 1);

        // Issue B first and use it right away so its idle clock starts and it is not in the
        // fragile first-use window. Then issue A after B: A is the newest entry, so a cap
        // eviction would evict B, the oldest, not A.
        let b = tokens.issue(path_b.clone());
        assert!(
            tokens.check(&b, &path_b),
            "B is used and its idle clock starts"
        );
        let _a = tokens.issue(path_a.clone());
        assert_eq!(tokens.len(), 2, "the store is full at its cap");

        // Advance 61 s: A, which was never used, is now past its first-use window and is
        // expired. B was used at the issue, so 61 s is well inside its 30-minute idle
        // limit. No `check` or `len` runs in between, so A is still in the store, merely
        // expired: `issue` is the next thing to prune it.
        *handle.lock().expect("the test clock") += Duration::from_secs(61);

        // Issuing C must not evict B: `issue` prunes A first, sees one live entry, and has
        // room for C. Without the pruning in `issue`, the store counts A against the cap
        // and evicts B, the oldest, to make room.
        let c = tokens.issue(path_a.clone());
        assert_eq!(tokens.len(), 2, "A is gone, B and C are live");
        assert!(tokens.check(&b, &path_b), "B survived the issue of C");
        assert!(
            tokens.check(&c, &path_a),
            "C opens the file it was issued for"
        );
    }

    /// `len` counts only live entries: expired ones are dropped before the count. The test
    /// calls `len` with no `check` in between: a `check` would prune the expired entry on
    /// its own and let the test pass even if `len` counted stored rather than live entries.
    #[test]
    fn len_counts_live_entries_only() {
        let (handle, clock) = clock_at(Instant::now());
        let tokens = StreamTokens::with_clock(16, clock);

        let path_a = stream_path(&id(), 0);
        let path_b = stream_path(&id(), 1);
        // Issue A (never used) and B (used right away, so it is not in the first-use window).
        let _a = tokens.issue(path_a);
        let b = tokens.issue(path_b.clone());
        assert!(
            tokens.check(&b, &path_b),
            "B is used and its idle clock starts"
        );
        assert_eq!(tokens.len(), 2, "two tokens issued, two live");

        // 61 s later: A, which was never used, is past its first-use window. No `check` runs
        // in between, so A is still in the store, merely expired: `len` is the next thing
        // to drop it.
        *handle.lock().expect("the test clock") += Duration::from_secs(61);
        // `len` drops A and counts only B.
        assert_eq!(tokens.len(), 1, "only B is live");
        assert!(tokens.check(&b, &path_b), "B is still alive");
    }
}
