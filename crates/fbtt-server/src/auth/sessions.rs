//! Sessions and challenges: the two short-lived things a login moves through.
//!
//! A login - password or device key - ends with a session token, and everything after it carries
//! that token. A key login starts one step earlier, with a challenge: the server hands out 32 random
//! bytes for a key to sign, and those bytes are answerable once. Neither is worth writing to disk: a
//! restart costs every client a re-login, which is what an hour and a minute are for anyway.
//!
//! Only a token's SHA-256 is kept, so a copy of the store cannot be spent as a token. A challenge is
//! found by `(fingerprint, nonce)` rather than by fingerprint alone, so asking for a nonce never
//! cancels an answer that is still on its way. Both stores are bounded - sessions by how many may be
//! live, challenges per key and in total - with the oldest-expiring entries giving way first, because
//! a server that never ran out of tokens would also never run out of memory.

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
/// How many of them one key may have outstanding, so that asking again cannot cancel an answer that
/// is still on its way while keeping one busy key from filling the store.
const MAX_PER_KEY: usize = 8;

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

/// One pending challenge: when it stops being answerable, and the number that keeps its place in the
/// expiry order unique even when two challenges end on the same tick. The nonce itself is not a field:
/// it is part of how the challenge is found, as [`Pending::by_nonce`]'s key.
#[derive(Debug, Clone, Copy)]
struct Challenge {
    expires: Instant,
    sequence: u64,
}

impl Challenge {
    /// Where this challenge sits in [`Pending::ending`].
    fn place(&self) -> (Instant, u64) {
        (self.expires, self.sequence)
    }
}

/// Which challenge an answer is for: the key it was handed out for, and the nonce we gave it.
type PendingKey = (String, [u8; 32]);

/// The challenges waiting for an answer.
#[derive(Debug)]
struct Pending {
    /// `(fingerprint, nonce)` to how long it has left. A fingerprint can have several outstanding
    /// challenges - only someone holding the nonce can name one, so asking again cannot cancel a
    /// different attempt that is still in flight.
    by_nonce: HashMap<PendingKey, Challenge>,
    /// `(when it ends, sequence)` to `(fingerprint, nonce)`, so the store can name the challenge
    /// ending soonest: the one to forget when the whole store is full, and the front of the queue a
    /// sweep walks until it reaches something still open.
    ending: BTreeMap<(Instant, u64), PendingKey>,
    /// Fingerprint to the places of its own challenges, oldest first, so that one key's overflow
    /// evicts that key's oldest and nobody else's. An entry is dropped as soon as it is empty, or
    /// this map would grow with every key that ever asked for a nonce.
    per_key: HashMap<String, BTreeSet<(Instant, u64)>>,
    /// The number the next challenge gets.
    sequence: u64,
}

impl Pending {
    /// Drops the challenge at `place` from all three views of the store.
    fn drop_at(&mut self, place: (Instant, u64)) {
        let Some(key) = self.ending.remove(&place) else {
            return;
        };
        self.by_nonce.remove(&key);
        let Some(places) = self.per_key.get_mut(&key.0) else {
            return;
        };
        places.remove(&place);
        let empty = places.is_empty();
        if empty {
            self.per_key.remove(&key.0);
        }
    }

    /// Clears out the challenges whose minute is up.
    ///
    /// `ending` is ordered by when each challenge ends, so this stops at the first one still open and
    /// no challenge is ever removed twice: the sweep costs what it removes and nothing more. Without
    /// it a store that nobody answers would keep every expired nonce until it overflowed.
    fn sweep_expired(&mut self, now: Instant) {
        while let Some(place) = self.ending.first_key_value().map(|(place, _)| *place) {
            if place.0 > now {
                return;
            }
            // The borrow above ended when the place was copied out, so this may take the map again.
            self.drop_at(place);
        }
    }

    /// Makes room for one more challenge for `fingerprint`.
    ///
    /// That key's own overflow goes first, and only its own: past [`MAX_PER_KEY`] challenges for one
    /// key the oldest of them gives way (it has been waiting longest, and an answer still on its way
    /// is not that one's). The store-wide cap then takes whatever is ending soonest, which was the
    /// closest to being gone anyway.
    fn make_room(&mut self, fingerprint: &str) {
        let oldest_of_this_key = self
            .per_key
            .get(fingerprint)
            .filter(|places| places.len() >= MAX_PER_KEY)
            .and_then(|places| places.first())
            .copied();
        if let Some(place) = oldest_of_this_key {
            self.drop_at(place);
            return;
        }
        while self.by_nonce.len() >= MAX_CHALLENGES {
            let Some(place) = self.ending.first_key_value().map(|(place, _)| *place) else {
                return;
            };
            self.drop_at(place);
        }
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
                by_nonce: HashMap::new(),
                ending: BTreeMap::new(),
                per_key: HashMap::new(),
                sequence: 0,
            }),
            clock,
        }
    }

    /// 32 random bytes for the key `fingerprint` to sign, good for a minute.
    ///
    /// Asking again adds another challenge; it does not cancel one already outstanding. A device's
    /// public key is in the open - anyone with it can compute the fingerprint - so if asking replaced
    /// what was asked before, a person watching that key could keep it from ever logging in by asking
    /// for a new nonce faster than the device answers. Eight of them stand at once, so an answer still
    /// on its way stays answerable, and a client that lost its nonce has only to ask.
    pub fn issue(&self, fingerprint: String) -> Vec<u8> {
        let now = (self.clock)();
        let mut pending = locked(&self.pending);
        pending.sweep_expired(now);
        pending.make_room(&fingerprint);
        let nonce = random_bytes::<32>();
        let sequence = pending.sequence;
        pending.sequence = pending.sequence.wrapping_add(1);
        let challenge = Challenge {
            expires: now + CHALLENGE_TTL,
            sequence,
        };
        let place = challenge.place();
        pending
            .by_nonce
            .insert((fingerprint.clone(), nonce), challenge);
        pending.ending.insert(place, (fingerprint.clone(), nonce));
        pending
            .per_key
            .entry(fingerprint)
            .or_default()
            .insert(place);
        nonce.to_vec()
    }

    /// Whether `nonce` is an outstanding challenge for `fingerprint`, spending it.
    ///
    /// Right answer, once: presenting the nonce spends that challenge whether or not what follows
    /// turns out well, so a challenge buys one attempt at a signature. A nonce we did not hand this
    /// key finds nothing and spends nothing - whoever asked still has their minute - and an expired
    /// one is gone either way. The fingerprint is in the open, so the nonce is what has to be guessed:
    /// being part of the entry's key, a wrong guess misses it outright instead of grazing it.
    pub fn take(&self, fingerprint: &str, nonce: &[u8]) -> bool {
        let now = (self.clock)();
        let Ok(nonce) = <[u8; 32]>::try_from(nonce) else {
            return false; // nothing we handed out is another length
        };
        let mut pending = locked(&self.pending);
        pending.sweep_expired(now);
        let key = (fingerprint.to_string(), nonce);
        // Sweeping first means whatever is still here is answerable, so finding it and spending it are
        // the same question.
        let Some(challenge) = pending.by_nonce.get(&key).copied() else {
            return false;
        };
        pending.drop_at(challenge.place());
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
            .field("pending", &locked(&self.pending).by_nonce.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{Challenges, MAX_CHALLENGES, MAX_PER_KEY, MAX_SESSIONS, Sessions};
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
        assert!(
            !challenges.take("SHA256:aaaa", &other),
            "another key's challenge"
        );
        assert!(!challenges.take("SHA256:bbbb", &[9u8; 32]), "a guess");
        assert!(challenges.take("SHA256:bbbb", &other));
        assert!(!challenges.take("SHA256:bbbb", &other), "one answer each");

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
    }

    /// Asking for a nonce again adds a challenge. It does not cancel one already outstanding, which is
    /// what stops someone who knows a device's public key - and anyone with it can work out the
    /// fingerprint - from keeping that device out by hammering the challenge route.
    #[test]
    fn new_challenge_does_not_cancel_pending() {
        let (_clock, handle) = TestClock::starting_at(Instant::now());
        let challenges = Challenges::with_clock(handle);

        let first = challenges.issue("SHA256:eeee".to_string());
        let second = challenges.issue("SHA256:eeee".to_string());
        assert_ne!(first, second, "two nonces are two challenges");
        assert!(challenges.take("SHA256:eeee", &second), "the newest");
        assert!(
            challenges.take("SHA256:eeee", &first),
            "the first is still answerable"
        );
        assert!(!challenges.take("SHA256:eeee", &first), "and spent once");

        // A challenge for another key in between changes nothing either.
        let third = challenges.issue("SHA256:eeee".to_string());
        let neighbour = challenges.issue("SHA256:ffff".to_string());
        assert!(challenges.take("SHA256:ffff", &neighbour));
        assert!(challenges.take("SHA256:eeee", &third));
    }

    /// A key past [`MAX_PER_KEY`] challenges loses its own oldest and only that one: a device failing
    /// to answer nine times in a row cannot push an unrelated client's challenge out of the store.
    #[test]
    fn per_key_cap_evicts_only_that_key() {
        let (_clock, handle) = TestClock::starting_at(Instant::now());
        let challenges = Challenges::with_clock(handle);

        // Asked for before any of the noisy key's, so a store-wide eviction would drop this one first.
        let neighbour = challenges.issue("SHA256:neighbour".to_string());
        let oldest = challenges.issue("SHA256:noisy".to_string());
        for _ in 1..MAX_PER_KEY {
            challenges.issue("SHA256:noisy".to_string());
        }
        let newest = challenges.issue("SHA256:noisy".to_string());

        assert!(!challenges.take("SHA256:noisy", &oldest), "its own oldest");
        assert!(challenges.take("SHA256:noisy", &newest));
        assert!(
            challenges.take("SHA256:neighbour", &neighbour),
            "the neighbour keeps its minute",
        );
    }

    /// A challenge whose minute is up stops counting, so nonces nobody answered cannot fill the store
    /// and evict a live one.
    #[test]
    fn expired_challenges_are_swept() {
        let (clock, handle) = TestClock::starting_at(Instant::now());
        let challenges = Challenges::with_clock(handle);

        for index in 0..MAX_CHALLENGES {
            challenges.issue(format!("SHA256:user-{index}"));
        }
        clock.advance(Duration::from_secs(61));

        // Nobody answered them. The next question finds the store empty instead of full, so one live
        // challenge is all that is outstanding rather than a thousand dead ones plus it.
        let live = challenges.issue("SHA256:live".to_string());
        assert_eq!(
            format!("{challenges:?}"),
            "Challenges { pending: 1 }",
            "expired nonces stop taking up room",
        );
        assert!(challenges.take("SHA256:live", &live));
    }

    /// A full pending list drops the challenge ending first, like the sessions do. Every one here is
    /// for a different key, so it is the store-wide cap doing the work, not [`MAX_PER_KEY`].
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
