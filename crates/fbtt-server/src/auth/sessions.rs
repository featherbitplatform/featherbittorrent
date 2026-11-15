//! Sessions and challenges: the two short-lived things a login moves through.
//!
//! A login - password or device key - ends with a session token, and everything after it carries
//! that token. A key login starts one step earlier, with a challenge: the server hands out 32 random
//! bytes for a key to sign, and those bytes are answerable once. Neither is worth writing to disk: a
//! restart costs every client a re-login, which is what an hour and a minute are for anyway.
//!
//! Only a token's SHA-256 is kept, so a copy of the store cannot be spent as a token. Both stores
//! are bounded, oldest-expiring entries giving way first, because a server that never ran out of
//! tokens would also never run out of memory.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::prelude::{BASE64_URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest as _, Sha256};

use crate::auth::{Clock, locked, wall_clock};

/// How long a session token stays good for after the last request that used it.
const SESSION_TTL: Duration = Duration::from_secs(60 * 60);
/// How many sessions may be live at once.
const MAX_SESSIONS: usize = 5_000;
/// How long a challenge nonce stays answerable.
const CHALLENGE_TTL: Duration = Duration::from_secs(60);
/// How many challenges may be pending at once.
const MAX_CHALLENGES: usize = 1_000;

/// What a session token is kept as: its SHA-256. The store never holds the token itself, so a copy of
/// it - in a log, a `Debug` printing, a memory dump - cannot be spent as one, and only the request
/// that presents the token learns whether it is good.
fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// `N` random bytes from the operating system's random source.
///
/// # Panics
/// If that source fails, which on every platform this project runs on it does not.
fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the operating system's random source");
    bytes
}

/// Whether `given` is the 32-byte `expected`, without leaking where they first differ.
///
/// The loop always runs over the whole expected value, so its length - and therefore its time - says
/// nothing about the guess except how long the guess was. A challenge nonce is the one secret here a
/// person with the public key could otherwise search for.
fn same(expected: &[u8; 32], given: &[u8]) -> bool {
    let mut difference = u8::from(given.len() != expected.len());
    for (expected, given) in expected.iter().zip(given) {
        difference |= expected ^ given;
    }
    difference == 0
}

/// One live session: whose it is, when its hour ends, and the number that keeps its place in the
/// expiry order unique even when two sessions end on the same tick.
#[derive(Debug)]
struct Session {
    subject: String,
    expires: Instant,
    sequence: u64,
}

impl Session {
    /// Where this session sits in [`Live::ending`].
    fn place(&self) -> (Instant, u64) {
        (self.expires, self.sequence)
    }
}

/// The sessions, looked up by token hash, and the same sessions ordered by when they end.
#[derive(Debug)]
struct Live {
    /// Token hash to session. Hashes only: a copy of this map is not a bag of tokens.
    by_token: HashMap<[u8; 32], Session>,
    /// `(when it ends, sequence)` to token hash, so the store can name the one ending soonest.
    ending: BTreeMap<(Instant, u64), [u8; 32]>,
    /// The number the next session gets.
    sequence: u64,
}

impl Live {
    /// Makes room for one more session by dropping the one whose hour ends first, which was the
    /// closest to being gone anyway.
    fn make_room(&mut self) {
        if self.by_token.len() < MAX_SESSIONS {
            return;
        }
        let Some((place, hash)) = self
            .ending
            .first_key_value()
            .map(|(place, hash)| (*place, *hash))
        else {
            return;
        };
        self.ending.remove(&place);
        self.by_token.remove(&hash);
    }
}

/// The sessions a signed-in client holds: token in, who it belongs to out.
pub struct Sessions {
    live: Mutex<Live>,
    clock: Clock,
}

impl Sessions {
    /// Sessions that expire an hour after they are last used, on the real clock.
    pub fn new() -> Self {
        Self::with_clock(wall_clock())
    }

    /// The same sessions on a clock the caller moves, so a test does not wait for an hour.
    pub fn with_clock(clock: Clock) -> Self {
        Sessions {
            live: Mutex::new(Live {
                by_token: HashMap::new(),
                ending: BTreeMap::new(),
                sequence: 0,
            }),
            clock,
        }
    }

    /// A new session for `subject`: the token to hand the client and how long it lives.
    ///
    /// The token is 32 random bytes in base64url without padding. What the store keeps is its
    /// SHA-256, so a request has to present the token itself to get anything back.
    pub fn issue(&self, subject: String) -> (String, Duration) {
        let mut live = locked(&self.live);
        live.make_room();
        let token = BASE64_URL_SAFE_NO_PAD.encode(random_bytes::<32>());
        let hash = token_hash(&token);
        let sequence = live.sequence;
        live.sequence = live.sequence.wrapping_add(1);
        let session = Session {
            subject,
            expires: (self.clock)() + SESSION_TTL,
            sequence,
        };
        let place = session.place();
        live.ending.insert(place, hash);
        live.by_token.insert(hash, session);
        (token, SESSION_TTL)
    }

    /// Who `token` belongs to, renewing it for another hour, or `None` if it is not a session any
    /// more - never issued, revoked, or an hour since anyone used it.
    pub fn check(&self, token: &str) -> Option<String> {
        let now = (self.clock)();
        let hash = token_hash(token);
        let mut live = locked(&self.live);
        // Taking the session out is the only way to move it in the expiry order: remove it from both
        // maps, and put it back with its new hour. An expired session goes through the same removal
        // and simply does not come back, which is all the sweeping this store ever needs.
        let mut session = live.by_token.remove(&hash)?;
        live.ending.remove(&session.place());
        if session.expires <= now {
            return None;
        }
        session.expires = now + SESSION_TTL;
        let subject = session.subject.clone();
        live.ending.insert(session.place(), hash);
        live.by_token.insert(hash, session);
        Some(subject)
    }

    /// Ends `token` now, so it stops working before its hour is up. Logging out, and the last thing
    /// a client does before it forgets it.
    pub fn revoke(&self, token: &str) {
        let hash = token_hash(token);
        let mut live = locked(&self.live);
        if let Some(session) = live.by_token.remove(&hash) {
            live.ending.remove(&session.place());
        }
    }
}

impl Default for Sessions {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Sessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // How many, not who: an account name belongs in a log line written on purpose, not in every
        // `Debug` printing that ends up next to a panic.
        f.debug_struct("Sessions")
            .field("live", &locked(&self.live).by_token.len())
            .finish()
    }
}

/// One pending challenge: the nonce handed out, whose key it was handed out for (as a fingerprint),
/// when it stops being answerable, and its place in the expiry order.
#[derive(Debug, Clone, Copy)]
struct Challenge {
    nonce: [u8; 32],
    expires: Instant,
    sequence: u64,
}

impl Challenge {
    /// Where this challenge sits in [`Pending::ending`].
    fn place(&self) -> (Instant, u64) {
        (self.expires, self.sequence)
    }
}

/// The challenges waiting for an answer.
#[derive(Debug)]
struct Pending {
    /// Fingerprint to challenge: one pending challenge per key, the newest wins.
    by_fingerprint: HashMap<String, Challenge>,
    /// `(when it ends, sequence)` to fingerprint.
    ending: BTreeMap<(Instant, u64), String>,
    /// The number the next challenge gets.
    sequence: u64,
}

impl Pending {
    /// Makes room for one more challenge by dropping the one ending first.
    fn make_room(&mut self) {
        if self.by_fingerprint.len() < MAX_CHALLENGES {
            return;
        }
        let Some((place, fingerprint)) = self
            .ending
            .first_key_value()
            .map(|(place, fingerprint)| (*place, fingerprint.clone()))
        else {
            return;
        };
        self.ending.remove(&place);
        self.by_fingerprint.remove(&fingerprint);
    }
}

/// The challenges a device key login starts with.
pub struct Challenges {
    pending: Mutex<Pending>,
    clock: Clock,
}

impl Challenges {
    /// Challenges that stay answerable for a minute, on the real clock.
    pub fn new() -> Self {
        Self::with_clock(wall_clock())
    }

    /// The same challenges on a clock the caller moves.
    pub fn with_clock(clock: Clock) -> Self {
        Challenges {
            pending: Mutex::new(Pending {
                by_fingerprint: HashMap::new(),
                ending: BTreeMap::new(),
                sequence: 0,
            }),
            clock,
        }
    }

    /// 32 random bytes for the key `fingerprint` to sign, good for a minute.
    ///
    /// A key can only have one challenge outstanding: asking again replaces the first, so a client
    /// that lost its nonce asks instead of waiting out the minute.
    pub fn issue(&self, fingerprint: String) -> Vec<u8> {
        let mut pending = locked(&self.pending);
        // One outstanding challenge per key: asking again replaces the answer we gave first, which is
        // what a client that lost its nonce needs, and it keeps the store from growing one entry per
        // attempt.
        if let Some(old) = pending.by_fingerprint.remove(&fingerprint) {
            pending.ending.remove(&old.place());
        } else {
            pending.make_room();
        }
        let nonce = random_bytes::<32>();
        let sequence = pending.sequence;
        pending.sequence = pending.sequence.wrapping_add(1);
        let challenge = Challenge {
            nonce,
            expires: (self.clock)() + CHALLENGE_TTL,
            sequence,
        };
        let place = challenge.place();
        pending.ending.insert(place, fingerprint.clone());
        pending.by_fingerprint.insert(fingerprint, challenge);
        nonce.to_vec()
    }

    /// Whether `nonce` is the outstanding challenge for `fingerprint`, spending it.
    ///
    /// Right answer, once: presenting the nonce spends the challenge whether or not what follows
    /// turns out well, so a challenge buys one attempt at a signature. A wrong nonce leaves it
    /// answerable, and an expired one is gone either way. The fingerprint is in the open - anyone
    /// holding the public key can compute it - so the nonce is what has to be guessed, and it is
    /// compared without leaking where it first differs.
    pub fn take(&self, fingerprint: &str, nonce: &[u8]) -> bool {
        let now = (self.clock)();
        let mut pending = locked(&self.pending);
        // 48 bytes copied out of the map rather than a borrow that would have to be released before
        // the entry could be spent.
        let Some(challenge) = pending.by_fingerprint.get(fingerprint).copied() else {
            return false;
        };
        if challenge.expires <= now {
            // Its minute is up: it goes here, and there was nothing to answer.
            pending.ending.remove(&challenge.place());
            pending.by_fingerprint.remove(fingerprint);
            return false;
        }
        if !same(&challenge.nonce, nonce) {
            // A guess does not spend the challenge: whoever asked for it still has their minute.
            return false;
        }
        pending.ending.remove(&challenge.place());
        pending.by_fingerprint.remove(fingerprint);
        true
    }
}

impl Default for Challenges {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Challenges {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // How many are outstanding, not which keys they are for.
        f.debug_struct("Challenges")
            .field("pending", &locked(&self.pending).by_fingerprint.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{Challenges, MAX_CHALLENGES, MAX_SESSIONS, Sessions};
    use crate::auth::TestClock;
    use std::time::{Duration, Instant};

    /// A token works until its hour has passed without being used, and using it moves the hour.
    #[test]
    fn session_expires_and_slides() {
        let (clock, handle) = TestClock::starting_at(Instant::now());
        let sessions = Sessions::with_clock(handle);
        let (token, ttl) = sessions.issue("alice".to_string());
        assert_eq!(ttl, Duration::from_secs(60 * 60));
        assert_eq!(token.len(), 43, "32 bytes in base64url with no padding");
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
            "url-safe alphabet: {token}"
        );

        clock.advance(Duration::from_secs(30 * 60));
        assert_eq!(sessions.check(&token).as_deref(), Some("alice"));
        clock.advance(Duration::from_secs(59 * 60));
        assert_eq!(
            sessions.check(&token).as_deref(),
            Some("alice"),
            "the hour slides with each use",
        );
        clock.advance(Duration::from_secs(60 * 60 + 1));
        assert_eq!(
            sessions.check(&token),
            None,
            "an unused hour is an expired one"
        );

        // The hour ends on the dot, and revoking is immediate.
        let (fresh, _) = sessions.issue("bob".to_string());
        clock.advance(Duration::from_secs(60 * 60));
        assert_eq!(sessions.check(&fresh), None, "no grace period");
        let (kept, _) = sessions.issue("carol".to_string());
        sessions.revoke(&kept);
        assert_eq!(sessions.check(&kept), None, "revoked");
        assert_eq!(sessions.check("nobody issued this"), None);
    }

    /// A full store makes room for a new session by dropping the one whose hour ends first.
    #[test]
    fn session_cap_evicts_oldest() {
        let (_clock, handle) = TestClock::starting_at(Instant::now());
        let sessions = Sessions::with_clock(handle);
        let oldest = sessions.issue("oldest".to_string()).0;
        for index in 1..MAX_SESSIONS {
            sessions.issue(format!("user-{index}"));
        }
        let newest = sessions.issue("newest".to_string()).0;
        assert_eq!(sessions.check(&oldest), None, "the store is capped");
        assert_eq!(sessions.check(&newest).as_deref(), Some("newest"));
    }

    /// A challenge is answerable once, only by the key it was issued for, and only for a minute.
    #[test]
    fn challenge_single_use_and_expiry() {
        let (clock, handle) = TestClock::starting_at(Instant::now());
        let challenges = Challenges::with_clock(handle);

        let nonce = challenges.issue("SHA256:aaaa".to_string());
        assert_eq!(nonce.len(), 32);
        assert!(challenges.take("SHA256:aaaa", &nonce));
        assert!(!challenges.take("SHA256:aaaa", &nonce), "one answer each");

        // A wrong nonce does not spend the challenge, and a nonce is only good for its own key.
        let other = challenges.issue("SHA256:bbbb".to_string());
        assert!(!challenges.take("SHA256:bbbb", &[9u8; 32]), "a guess");
        assert!(challenges.take("SHA256:bbbb", &other));
        assert!(
            !challenges.take("SHA256:aaaa", &other),
            "another key's challenge"
        );

        // A minute is the whole life of a challenge.
        let live = challenges.issue("SHA256:cccc".to_string());
        clock.advance(Duration::from_secs(59));
        assert!(
            challenges.take("SHA256:cccc", &live),
            "still open a second before"
        );
        let expiring = challenges.issue("SHA256:dddd".to_string());
        clock.advance(Duration::from_secs(60));
        assert!(
            !challenges.take("SHA256:dddd", &expiring),
            "the minute is up"
        );

        // Asking again replaces a key's challenge, so the newest nonce is the answerable one.
        let replaced = challenges.issue("SHA256:eeee".to_string());
        let fresh = challenges.issue("SHA256:eeee".to_string());
        assert!(!challenges.take("SHA256:eeee", &replaced), "superseded");
        assert!(challenges.take("SHA256:eeee", &fresh));
    }

    /// A full pending list drops the challenge ending first, like the sessions do.
    #[test]
    fn challenge_cap_evicts_oldest() {
        let (_clock, handle) = TestClock::starting_at(Instant::now());
        let challenges = Challenges::with_clock(handle);
        let oldest = challenges.issue("SHA256:oldest".to_string());
        for index in 1..MAX_CHALLENGES {
            challenges.issue(format!("SHA256:user-{index}"));
        }
        let newest = challenges.issue("SHA256:newest".to_string());
        assert!(!challenges.take("SHA256:oldest", &oldest), "capped");
        assert!(challenges.take("SHA256:newest", &newest));
    }
}
