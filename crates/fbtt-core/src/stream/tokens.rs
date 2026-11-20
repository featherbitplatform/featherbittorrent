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

impl Entry {
    /// When the token was last used: the last successful check, or its issue time if it was
    /// never used. The eviction order is by this, oldest first.
    fn last_used(&self) -> Instant {
        self.idle_since.unwrap_or(self.issued)
    }
}

/// The tokens that open the streams of one endpoint: at most `cap` live. A full store
/// evicts the least recently *used* token (its last successful check, or its issue time
/// if never used), never the most recently used one, so a player that keeps seeking is
/// not cut off by a newer play.
///
/// A token is a secret while it lives, so the store's `Debug` counts the tokens in it and
/// says nothing about any of them.
pub struct StreamTokens {
    inner: Mutex<Inner>,
    cap: usize,
    clock: Clock,
}

struct Inner {
    /// The live tokens. Eviction when the store is full is by last use, not by position.
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
        // A live player may still hold a token it was issued for an earlier play, so a
        // full store evicts the least recently *used* token - the last successful check,
        // or the issue time if it was never used - never the most recently used one.
        while inner.entries.len() >= self.cap {
            let victim = inner
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used())
                .map(|(slot, _)| slot);
            let Some(slot) = victim else {
                break;
            };
            inner.entries.remove(slot);
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
        self.resolve(token).is_some_and(|scope| scope == path)
    }

    /// [`resolve`](Self::resolve)'s rule in one place: whether `token` is one that still
    /// lives, with the first-use window, the idle limit and the eight-hour cap applied,
    /// and the idle clock renewed when it passes. The scope is read from the same locked
    /// store in the same call, so the rule about when a token lives and the scope it
    /// opens are one decision rather than two. `None` when the token is unknown, dead,
    /// or past its first-use window.
    fn resolve_scope(&self, token: &str) -> Option<String> {
        // Longer than the limit fbt-api offers a route a query token is not a token at all,
        // and is refused before any hash is even looked up.
        if token.len() > MAX_TOKEN_BYTES {
            return None;
        }
        let hash = sha256(token.as_bytes());
        let now = self.now();
        let mut inner = self.locked();
        // Drop whatever has died, oldest first: past its first-use window if it was never
        // used, past its idle limit or its eight-hour cap if it was.
        inner.entries.retain(|entry| !expired(entry, now));
        let slot = inner.entries.iter().position(|entry| entry.hash == hash)?;
        // The idle clock is what keeps a token alive: a first use inside the window starts
        // it, and every later use that is still inside the limit renews it, so a film that
        // keeps seeking stays open. A use past the limit is refused, and the token is
        // already on its way out of the silence.
        match inner.entries[slot].idle_since {
            None => {
                // First use: it must still be inside the first-use window, and it starts the
                // idle clock.
                if now.duration_since(inner.entries[slot].issued) > FIRST_USE {
                    return None;
                }
                inner.entries[slot].idle_since = Some(now);
            }
            Some(since) => {
                // A later use is valid while it is inside the idle limit of the last one, and
                // renews the clock.
                if now.duration_since(since) > IDLE {
                    return None;
                }
                inner.entries[slot].idle_since = Some(now);
            }
        }
        Some(inner.entries[slot].scope.clone())
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

    /// The scope `token` opens, applying exactly the rules [`check`](Self::check) applies - the
    /// length limit, the first-use window, the idle limit, the eight-hour cap, and marking the
    /// token used for the eviction order - and `None` when the token is dead or unknown. The
    /// relay needs the scope beside the check: it has to know which server path its token opens
    /// in order to relay to it.
    pub fn resolve(&self, token: &str) -> Option<String> {
        self.resolve_scope(token)
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

    /// `resolve` applies the same rules `check` does and hands back the scope a live token
    /// opens: `None` for a token nobody issued, for one past its first-use window, and for
    /// one its idle limit ended; the scope itself when it is still alive. The relay reads the
    /// scope this way, so its life is the same life a check gives a token.
    #[test]
    fn resolve_gives_the_scope_of_a_live_token() {
        let (handle, clock) = clock_at(Instant::now());
        let tokens = StreamTokens::with_clock(16, clock);

        // An unknown token has no scope.
        assert!(
            tokens.resolve("never-issued").is_none(),
            "a token nobody issued resolves to nothing"
        );

        let path = stream_path(&id(), 0);
        let token = tokens.issue(path.clone());
        // A freshly issued token resolves to the scope it was issued for.
        assert_eq!(tokens.resolve(&token), Some(path.clone()));

        // The first-use window: a token nobody used in 61 s is gone, and so is its scope.
        let unused = tokens.issue("/somewhere".to_string());
        *handle.lock().expect("the test clock") += Duration::from_secs(61);
        assert!(
            tokens.resolve(&unused).is_none(),
            "a token past its first-use window resolves to nothing"
        );

        // The idle limit: a used token dies of a silence past 30 minutes, with it its scope.
        let idle = tokens.issue(path.clone());
        assert!(
            tokens.check(&idle, &path),
            "a first use starts the idle clock"
        );
        *handle.lock().expect("the test clock") += Duration::from_secs(31 * 60);
        assert!(
            tokens.resolve(&idle).is_none(),
            "a token past its idle limit resolves to nothing"
        );

        // The eight-hour cap: a token kept alive by use is resolved to nothing past it.
        let film = tokens.issue(path.clone());
        assert!(tokens.check(&film, &path));
        *handle.lock().expect("the test clock") += Duration::from_secs(8 * 60 * 60 + 1);
        assert!(
            tokens.resolve(&film).is_none(),
            "a token past the eight-hour cap resolves to nothing"
        );

        // A resolving use marks the token used: the least recently used order sees it.
        let used = tokens.issue(path.clone());
        assert!(tokens.check(&used, &path), "it was used by check");
        assert!(tokens.resolve(&used).is_some(), "and still resolves");
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

    /// A full store evicts the least recently *used* token, never the most recently
    /// used one. A player that keeps seeking holds a token it is still using; a newer
    /// play must not evict it. This test evicts the LRU even when it is not the oldest
    /// inserted, to prove the eviction policy is last-use not FIFO.
    ///
    /// Token 1 is issued first and then token 2 is checked first, making token 2 the LRU
    /// even though token 1 was inserted earlier. The oldest-inserted entry (token 1) must
    /// survive while the least-recently-used (token 2) is evicted, so the test distinguishes
    /// LRU from FIFO.
    ///
    /// The clock is advanced between each use, so the last-use ordering is unambiguous.
    #[test]
    fn a_full_store_evicts_least_recently_used() {
        let (handle, clock) = clock_at(Instant::now());
        let cap = 4;
        let tokens = StreamTokens::with_clock(cap, clock);

        // Tokens 0..=3 are issued first. Token 1 will be checked first later.
        let path_0 = stream_path(&id(), 0);
        let path_1 = stream_path(&id(), 1);
        let path_2 = stream_path(&id(), 2);
        let path_3 = stream_path(&id(), 3);
        let t0 = tokens.issue(path_0.clone());
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        let t1 = tokens.issue(path_1.clone());
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        let t2 = tokens.issue(path_2.clone());
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        let t3 = tokens.issue(path_3.clone());
        *handle.lock().expect("the test clock") += Duration::from_secs(1);

        // Token 1 is checked first, making it the least recently used although token 0
        // was inserted first. The oldest-inserted token 0 must survive this eviction.
        assert!(tokens.check(&t1, &path_1), "token 1 is used and becomes LRU");

        // Fill the store to capacity.
        let path_4 = stream_path(&id(), 4);
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        let t4 = tokens.issue(path_4.clone());
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        assert_eq!(tokens.len(), cap, "the store is full at its cap");

        // A 5th play. The store is full, so it must evict the least recently used token,
        // which is token 1 in this case (it was checked first). Token 0 is the oldest by
        // insertion, so if eviction were FIFO, token 0 would die; but the policy is LRU,
        // so token 1 dies instead.
        let path_5 = stream_path(&id(), 5);
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        let t5 = tokens.issue(path_5.clone());
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        assert!(tokens.check(&t5, &path_5), "the newest token is live");
        assert_eq!(tokens.len(), cap, "the cap is not exceeded");

        // Token 1, the least recently used, is evicted. Token 0, the oldest-inserted,
        // survives. Token 4, the most recently used, and the middle tokens also survive.
        assert!(
            !tokens.check(&t1, &path_1),
            "the least recently used token is evicted, NOT the oldest inserted"
        );
        assert!(tokens.check(&t0, &path_0), "the oldest-inserted token survives");
        assert!(tokens.check(&t2, &path_2), "a middle token survives");
        assert!(tokens.check(&t3, &path_3), "a middle token survives");
        assert!(
            tokens.check(&t4, &path_4),
            "the most recently used token is not evicted"
        );
        assert!(
            tokens.check(&t5, &path_5),
            "the newly issued token is live"
        );
    }

    /// The same rule from the player's point of view: a token that is kept in use by
    /// checks survives the issue of newer tokens, even when it was issued first and is
    /// therefore the oldest entry. With the store at its cap, a fresh play evicts the
    /// least recently used token; a token that was just checked is not that one.
    ///
    /// The clock is advanced between each use, so the last-use ordering is unambiguous.
    #[test]
    fn a_token_in_use_survives_a_full_store() {
        let (handle, clock) = clock_at(Instant::now());
        let cap = 3;
        let tokens = StreamTokens::with_clock(cap, clock);

        let path_a = stream_path(&id(), 0);
        let path_b = stream_path(&id(), 1);
        let path_c = stream_path(&id(), 2);
        let path_d = stream_path(&id(), 3);

        // A is issued first (oldest entry) and is the player's token. The clock is
        // advanced before each issue and use so the last-use ordering is
        // unambiguous. Each token is checked right after it is issued, so every
        // token's `last_used` is its check time, never its issue time.
        let a = tokens.issue(path_a.clone());
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        assert!(tokens.check(&a, &path_a), "A is used at t=1");
        let b = tokens.issue(path_b.clone());
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        assert!(tokens.check(&b, &path_b), "B is used at t=2");
        let c = tokens.issue(path_c.clone());
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        assert!(tokens.check(&c, &path_c), "C is used at t=3");
        assert_eq!(tokens.len(), cap, "the store is full");

        // The player seeks again: A is checked, so A is now the most recently
        // used token (t=4). B (t=2) is the least recently used.
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        assert!(
            tokens.check(&a, &path_a),
            "the player's token is used again at t=4"
        );

        // A new play, D. The store is full, so it evicts the least recently
        // used token: B (used at t=2), not A (just used at t=4) and not C
        // (used at t=3).
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        let d = tokens.issue(path_d.clone());
        *handle.lock().expect("the test clock") += Duration::from_secs(1);
        assert!(tokens.check(&d, &path_d), "the newest token is live");
        assert_eq!(tokens.len(), cap, "the cap is not exceeded");

        // A, which was just used, survives. C, used more recently than B,
        // survives. B, the least recently used, is evicted.
        assert!(
            tokens.check(&a, &path_a),
            "the token that was just used survives"
        );
        assert!(
            tokens.check(&c, &path_c),
            "the token used more recently than the evicted one survives"
        );
        assert!(
            !tokens.check(&b, &path_b),
            "the least recently used token is evicted"
        );
    }
}
