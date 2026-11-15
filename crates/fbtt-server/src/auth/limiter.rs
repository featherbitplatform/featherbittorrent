//! The login limiters: how many wrong answers one address gets before it has to wait, and how many
//! challenges it may ask for.
//!
//! Ten failures in a five-minute window lock an address out until the window clears, for passwords
//! and device keys alike - the same wall a person brute-forcing from another machine hits, and the
//! reason a stolen password list cannot turn into a guess per second. It counts logins, not requests:
//! an authenticated client calling the API all day is never touched, so this is not a rate limiter
//! for the rest of the server.
//!
//! A success clears the address's count, so one mistyped password on the way to a working login does
//! not leave someone half-blocked.
//!
//! The other limiter counts something else, and neither one feeds the other: asking for a challenge is
//! not an attempt at a login (nothing has been guessed yet), so it is capped by its own rate - thirty
//! a minute per address - rather than by the failure budget a wrong password spends. It is what stops
//! the challenge route, which answers strangers too, from being used to fill the challenge store.
//!
//! "One address" is deliberately a block rather than a socket: an IPv4-mapped IPv6 counts as its IPv4
//! address and other IPv6 addresses share a window per `/64`, so a client cannot get a fresh ten tries
//! by changing the low half of its address. Both limiters count a client by the same address bucket, so
//! a client is counted where the things it did were counted.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::auth::{Clock, locked, wall_clock};

/// How long a login failure is held against an address.
const WINDOW: Duration = Duration::from_secs(5 * 60);
/// How many failures inside that window lock the address out.
const MAX_FAILURES: usize = 10;
/// How long a challenge request is held against an address.
const ISSUE_WINDOW: Duration = Duration::from_secs(60);
/// How many challenges one address may ask for inside that minute. Half a second per nonce, which is
/// more than a client that logs in normally uses and less than one key can hold pending (eight), so
/// a hammering address cannot outrun the store's own per-key cap.
const MAX_ISSUES: usize = 30;
/// How many addresses are tracked at once, so a spray of source addresses cannot grow this table
/// without bound. The quietest one - the one whose last counted moment is oldest - is forgotten first,
/// and forgetting it only ever lets someone who had been kept busy try again sooner.
const MAX_ADDRESSES: usize = 4_096;

/// Whether a moment at `then` still counts when the clock reads `now`, over a window of `span`.
fn inside(then: Instant, now: Instant, span: Duration) -> bool {
    then.checked_add(span).is_some_and(|until| until > now)
}

/// The address a failure is counted against, so that two spellings of one client do not get two
/// windows and one client does not get a thousand.
///
/// `::ffff:203.0.113.7` is how the IPv4 client of an IPv6-listening server arrives, so it counts as
/// its IPv4 address ([`IpAddr::to_canonical`]). Everything else in IPv6 is keyed by its `/64`, which
/// is the block a customer is handed: someone rotating through addresses inside one prefix - what a
/// machine or a home connection does - shares one window instead of getting ten fresh tries per
/// address. Two prefixes two customers share would share a window, and that errs on the side of
/// refusing too much, which is the safe way for a limiter to be wrong.
fn bucket(address: IpAddr) -> IpAddr {
    match address.to_canonical() {
        address @ IpAddr::V4(_) => address,
        IpAddr::V6(v6) => {
            let [first, second, third, fourth, ..] = v6.segments();
            IpAddr::V6(Ipv6Addr::new(first, second, third, fourth, 0, 0, 0, 0))
        }
    }
}

/// Whole seconds between `now` and `until`, rounded up. A client told to wait zero seconds would be
/// back on the next tick, which is not what a `Retry-After` means.
fn seconds_until(until: Instant, now: Instant) -> u64 {
    let remaining = until - now;
    remaining.as_secs() + u64::from(remaining.subsec_nanos() != 0)
}

/// One address's recent counted moments - login failures, or the challenges it asked for - oldest
/// first, at most `cap` of them.
///
/// Only the newest `cap` can matter: a block ends when the oldest of those still held falls out of the
/// window, so anything older than that has already done its work.
#[derive(Debug)]
struct Events {
    at: VecDeque<Instant>,
    /// Where this address sits in [`Tracked::quietest_first`]: its newest moment, plus the number
    /// that keeps that unique when two addresses are counted on the same tick.
    place: (Instant, u64),
}

impl Events {
    /// When this address stops being over its limit, if it is over it now.
    ///
    /// `cap` moments with the oldest of them still inside its window. Keep going and the answer moves
    /// forward with you: a hammering client cannot wear a block down to nothing.
    fn blocked_until(&self, now: Instant, cap: usize, span: Duration) -> Option<Instant> {
        if self.at.len() < cap {
            return None;
        }
        let oldest = (*self.at.front()?).checked_add(span)?;
        (oldest > now).then_some(oldest)
    }

    /// Records one more moment at `now`, dropping what has fallen out of the window and anything past
    /// the `cap` that can matter.
    fn add(&mut self, now: Instant, cap: usize, span: Duration) {
        self.at.retain(|then| inside(*then, now, span));
        self.at.push_back(now);
        // Past `cap`, the oldest stops mattering: a block ends when the `cap`-th newest moment falls
        // out of the window, so nothing older than that can change the answer.
        if self.at.len() > cap {
            self.at.pop_front();
        }
    }
}

/// The addresses with counted moments on their record, keyed by [`bucket`], and the same addresses
/// ordered by when they stop mattering. One window's worth of rules, which both limiters here use
/// with numbers of their own.
#[derive(Debug)]
struct Tracked {
    /// Bucket to its events.
    by_address: HashMap<IpAddr, Events>,
    /// `(newest moment, sequence)` to address: the entry a full table forgets first.
    quietest_first: BTreeMap<(Instant, u64), IpAddr>,
    /// The number the next counted moment gets.
    sequence: u64,
}

impl Tracked {
    /// Drops the address that has been quiet longest, making room for one more.
    ///
    /// That address is also the one whose block ends soonest - a block ends one window after its
    /// newest moment - so forgetting it forgets the least: at worst someone who had been kept busy
    /// gets to try again sooner than they should have.
    fn make_room(&mut self) {
        if self.by_address.len() < MAX_ADDRESSES {
            return;
        }
        let Some((place, address)) = self
            .quietest_first
            .first_key_value()
            .map(|(place, address)| (*place, *address))
        else {
            return;
        };
        self.quietest_first.remove(&place);
        self.by_address.remove(&address);
    }

    /// When `address` stops being over a limit of `cap` moments per `span`, if it is over it now.
    fn blocked_until(
        &self,
        address: IpAddr,
        now: Instant,
        cap: usize,
        span: Duration,
    ) -> Option<Instant> {
        self.by_address
            .get(&address)
            .and_then(|events| events.blocked_until(now, cap, span))
    }

    /// One more moment for `address`, making room in the address table first when it is new to it.
    fn add(&mut self, address: IpAddr, now: Instant, cap: usize, span: Duration) {
        if !self.by_address.contains_key(&address) {
            self.make_room();
        }
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        match self.by_address.get_mut(&address) {
            Some(events) => {
                events.add(now, cap, span);
                // Its place in the forgetting order moves with its newest moment.
                self.quietest_first.remove(&events.place);
                events.place = (now, sequence);
                self.quietest_first.insert(events.place, address);
            }
            None => {
                let events = Events {
                    at: VecDeque::from([now]),
                    place: (now, sequence),
                };
                self.quietest_first.insert(events.place, address);
                self.by_address.insert(address, events);
            }
        }
    }

    /// `address` starts from clean again.
    fn clear(&mut self, address: IpAddr) {
        if let Some(events) = self.by_address.remove(&address) {
            self.quietest_first.remove(&events.place);
        }
    }
}

/// Who may keep trying to log in, and who has to wait.
pub struct LoginLimiter {
    tracked: Mutex<Tracked>,
    clock: Clock,
}

impl LoginLimiter {
    /// Ten failures per address per five minutes, on the real clock.
    pub fn new() -> Self {
        Self::with_clock(wall_clock())
    }

    /// The same rule on a clock the caller moves, so a test does not wait out a window.
    pub fn with_clock(clock: Clock) -> Self {
        LoginLimiter {
            tracked: Mutex::new(Tracked {
                by_address: HashMap::new(),
                quietest_first: BTreeMap::new(),
                sequence: 0,
            }),
            clock,
        }
    }

    /// Whether `address` may try to log in now, or how many whole seconds it should wait first. The
    /// peer address as the connection reports it is fine: an IPv4-mapped IPv6 and a client rotating
    /// within its `/64` are counted where the failures they caused were counted.
    pub fn check(&self, address: IpAddr) -> Result<(), u64> {
        let now = (self.clock)();
        let address = bucket(address);
        let tracked = locked(&self.tracked);
        match tracked.blocked_until(address, now, MAX_FAILURES, WINDOW) {
            Some(until) => Err(seconds_until(until, now)),
            None => Ok(()),
        }
    }

    /// One more failure for `address`.
    pub fn failure(&self, address: IpAddr) {
        let now = (self.clock)();
        let address = bucket(address);
        let tracked = &mut *locked(&self.tracked);
        tracked.add(address, now, MAX_FAILURES, WINDOW);
    }

    /// A login worked: the address starts from clean again.
    pub fn success(&self, address: IpAddr) {
        let address = bucket(address);
        let tracked = &mut *locked(&self.tracked);
        tracked.clear(address);
    }
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for LoginLimiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // How many addresses have failures on their record, not which ones or what they tried.
        f.debug_struct("LoginLimiter")
            .field("tracked", &locked(&self.tracked).by_address.len())
            .finish()
    }
}

/// How many challenges one address may ask for in a minute: thirty, half a second apiece.
///
/// The challenge route answers every well-formed key, configured or not - anything else would tell a
/// passer-by which devices are authorised - so without a cap of its own that route is free work for
/// an attacker: it costs no login failures (asking has guessed nothing yet), and each answer makes the
/// server hold a nonce. Nobody logging in normally gets near the cap, and it is slow enough that one
/// address cannot outrun the eight challenges a single key may hold pending. Its own count, not shared with
/// the failure budget: a client that has been guessing passwords still gets its full minute of nonces, and
/// a client that has been asking for nonces has not yet failed anything.
pub struct ChallengeLimiter {
    tracked: Mutex<Tracked>,
    clock: Clock,
}

impl ChallengeLimiter {
    /// Thirty challenges per address per minute, on the real clock.
    pub fn new() -> Self {
        Self::with_clock(wall_clock())
    }

    /// The same rule on a clock the caller moves, so a test does not wait out a minute.
    pub fn with_clock(clock: Clock) -> Self {
        ChallengeLimiter {
            tracked: Mutex::new(Tracked {
                by_address: HashMap::new(),
                quietest_first: BTreeMap::new(),
                sequence: 0,
            }),
            clock,
        }
    }

    /// Whether `address` may ask for another challenge now, or how many whole seconds it should wait
    /// first - the oldest of the nonces in its minute is answerable for that long again.
    pub fn check(&self, address: IpAddr) -> Result<(), u64> {
        let now = (self.clock)();
        let address = bucket(address);
        let tracked = locked(&self.tracked);
        match tracked.blocked_until(address, now, MAX_ISSUES, ISSUE_WINDOW) {
            Some(until) => Err(seconds_until(until, now)),
            None => Ok(()),
        }
    }

    /// One more nonce handed to `address`. Only an address that was answered is counted: a request
    /// refused before it got a nonce asked for nothing.
    pub fn record(&self, address: IpAddr) {
        let now = (self.clock)();
        let address = bucket(address);
        let tracked = &mut *locked(&self.tracked);
        tracked.add(address, now, MAX_ISSUES, ISSUE_WINDOW);
    }
}

impl Default for ChallengeLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ChallengeLimiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // How many addresses were asked for a nonce recently, not which ones.
        f.debug_struct("ChallengeLimiter")
            .field("tracked", &locked(&self.tracked).by_address.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{ChallengeLimiter, LoginLimiter, MAX_FAILURES, MAX_ISSUES};
    use crate::auth::TestClock;
    use std::net::IpAddr;
    use std::time::{Duration, Instant};

    /// Ten failures in the window lock an address out until the window clears; a success wipes it.
    #[test]
    fn limiter_blocks_after_10_failures() {
        let (clock, handle) = TestClock::starting_at(Instant::now());
        let limiter = LoginLimiter::with_clock(handle);
        let address: IpAddr = "203.0.113.7".parse().expect("an address");

        assert_eq!(limiter.check(address), Ok(()));
        for _ in 0..MAX_FAILURES - 1 {
            limiter.failure(address);
        }
        assert_eq!(
            limiter.check(address),
            Ok(()),
            "nine failures leave one try",
        );
        limiter.failure(address);

        let Err(retry_after) = limiter.check(address) else {
            panic!("ten failures should block the address");
        };
        assert_eq!(retry_after, 300, "blocked until the window clears");
        clock.advance(Duration::from_secs(299));
        assert!(
            limiter.check(address).is_err(),
            "still inside the window of the oldest failure"
        );
        clock.advance(Duration::from_secs(2));
        assert_eq!(limiter.check(address), Ok(()), "the window has cleared");

        // Failing while blocked only pushes the block forward, it does not wear it down.
        for _ in 0..MAX_FAILURES {
            limiter.failure(address);
        }
        let Err(retry_after) = limiter.check(address) else {
            panic!("ten fresh failures block again");
        };
        assert_eq!(retry_after, 300);

        // A login that worked clears the count: nine failures do not carry over to the next attempt.
        let other: IpAddr = "203.0.113.8".parse().expect("an address");
        for _ in 0..MAX_FAILURES - 1 {
            limiter.failure(other);
        }
        assert_eq!(limiter.check(other), Ok(()));
        limiter.success(other);
        assert_eq!(limiter.check(other), Ok(()));
    }

    /// A failure follows the client, not the spelling its address arrived in: `::ffff:a.b.c.d` is that
    /// IPv4 address, and every address inside one `/64` shares one window.
    #[test]
    fn limiter_groups_v6_by_64_and_maps_v4() {
        let (_clock, handle) = TestClock::starting_at(Instant::now());
        let limiter = LoginLimiter::with_clock(handle);

        // One IPv4 address, arrived two ways.
        let mapped: IpAddr = "::ffff:203.0.113.9"
            .parse()
            .expect("a v6 spelling of a v4 address");
        let plain: IpAddr = "203.0.113.9".parse().expect("an address");
        for _ in 0..MAX_FAILURES - 1 {
            limiter.failure(mapped);
        }
        assert_eq!(limiter.check(plain), Ok(()), "nine failures leave one try");
        limiter.failure(plain);
        assert!(
            limiter.check(mapped).is_err(),
            "the tenth failure came in as the other spelling",
        );
        limiter.success(plain);
        assert_eq!(
            limiter.check(mapped),
            Ok(()),
            "clearing one spelling clears the other",
        );

        // Many addresses, one customer: a `/64` is what a network hands out.
        let first: IpAddr = "2001:db8:1:2::5".parse().expect("an address");
        for _ in 0..MAX_FAILURES {
            limiter.failure(first);
        }
        for sibling in [
            "2001:db8:1:2::ffff:1",
            "2001:db8:1:2:dead:beef:cafe:f00d",
            "2001:db8:1:2:0:0:0:0",
        ] {
            let address: IpAddr = sibling.parse().expect("an address");
            assert!(
                limiter.check(address).is_err(),
                "{sibling} is the same /64 as 2001:db8:1:2::5",
            );
        }

        // The next /64 over is somebody else.
        let neighbour: IpAddr = "2001:db8:1:3::5".parse().expect("an address");
        assert_eq!(
            limiter.check(neighbour),
            Ok(()),
            "a different /64 keeps its own window",
        );
    }

    /// Thirty nonces a minute per address, the block moving with a client that keeps asking. Asking
    /// spends nothing from the failure budget, and one address's minute is nobody else's.
    #[test]
    fn challenge_issuance_is_capped_per_minute() {
        let (clock, handle) = TestClock::starting_at(Instant::now());
        let limiter = ChallengeLimiter::with_clock(handle);
        let address: IpAddr = "203.0.113.21".parse().expect("an address");

        for _ in 0..MAX_ISSUES {
            assert_eq!(limiter.check(address), Ok(()));
            limiter.record(address);
        }
        let Err(retry_after) = limiter.check(address) else {
            panic!("{MAX_ISSUES} nonces should fill the minute");
        };
        assert_eq!(retry_after, 60, "blocked until the oldest nonce ages out");

        clock.advance(Duration::from_secs(59));
        assert!(
            limiter.check(address).is_err(),
            "still inside the minute of the oldest one"
        );
        clock.advance(Duration::from_secs(2));
        assert_eq!(
            limiter.check(address),
            Ok(()),
            "the oldest nonce has aged out, so room is left again",
        );

        // The other spelling of the same address is the same address here too.
        let mapped: IpAddr = "::ffff:203.0.113.21"
            .parse()
            .expect("a v6 spelling of a v4 address");
        for _ in 0..MAX_ISSUES {
            limiter.record(mapped);
        }
        assert!(
            limiter.check(address).is_err(),
            "the minute was filled in the other spelling",
        );

        // Nobody else pays for it.
        let neighbour: IpAddr = "203.0.113.22".parse().expect("an address");
        assert_eq!(
            limiter.check(neighbour),
            Ok(()),
            "a different address keeps its own minute",
        );
    }
}
