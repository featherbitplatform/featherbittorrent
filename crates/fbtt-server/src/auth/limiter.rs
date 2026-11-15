//! The login limiter: how many wrong answers one address gets before it has to wait.
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
//! "One address" is deliberately a block rather than a socket: an IPv4-mapped IPv6 counts as its IPv4
//! address and other IPv6 addresses share a window per `/64`, so a client cannot get a fresh ten tries
//! by changing the low half of its address.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::auth::{Clock, locked, wall_clock};

/// How long a failure is held against an address.
const WINDOW: Duration = Duration::from_secs(5 * 60);
/// How many failures inside that window lock the address out.
const MAX_FAILURES: usize = 10;
/// How many addresses are tracked at once, so a spray of source addresses cannot grow this table
/// without bound. The quietest one - the one whose last failure is oldest - is forgotten first, and
/// forgetting it only ever lets someone who had been failing quietly try again.
const MAX_ADDRESSES: usize = 4_096;

/// Whether a failure at `then` still counts when the clock reads `now`.
fn inside(then: Instant, now: Instant) -> bool {
    then.checked_add(WINDOW).is_some_and(|until| until > now)
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

/// One address's recent failures, oldest first, at most [`MAX_FAILURES`] of them.
///
/// Only the newest ten can matter: a block ends when the oldest of the ten it still holds falls out
/// of the window, so anything older than that has already done its work.
#[derive(Debug)]
struct Failures {
    at: VecDeque<Instant>,
    /// Where this address sits in [`Tracked::quietest_first`]: its newest failure, plus the number
    /// that keeps that unique when two addresses fail on the same tick.
    place: (Instant, u64),
}

impl Failures {
    /// When this address stops being locked out, if it is locked out now.
    ///
    /// Ten failures with the oldest of them still inside the window. Keep failing and the answer
    /// moves forward with you: a hammering client cannot wear a block down to nothing.
    fn blocked_until(&self, now: Instant) -> Option<Instant> {
        if self.at.len() < MAX_FAILURES {
            return None;
        }
        let oldest = (*self.at.front()?).checked_add(WINDOW)?;
        (oldest > now).then_some(oldest)
    }

    /// Records one more failure at `now`, dropping what has fallen out of the window and anything
    /// past the ten that can matter.
    fn add(&mut self, now: Instant) {
        self.at.retain(|then| inside(*then, now));
        self.at.push_back(now);
        // Past ten, the oldest stops mattering: a block ends when the tenth-newest failure falls out
        // of the window, so nothing older than that can change the answer.
        if self.at.len() > MAX_FAILURES {
            self.at.pop_front();
        }
    }
}

/// The addresses with failures on their record, keyed by [`bucket`], and the same addresses ordered by
/// when they stop mattering.
#[derive(Debug)]
struct Tracked {
    /// Bucket to its failures.
    by_address: HashMap<IpAddr, Failures>,
    /// `(newest failure, sequence)` to address: the entry a full table forgets first.
    quietest_first: BTreeMap<(Instant, u64), IpAddr>,
    /// The number the next failure gets.
    sequence: u64,
}

impl Tracked {
    /// Drops the address that has been quiet longest, making room for one more.
    ///
    /// That address is also the one whose block ends soonest - a block ends five minutes after its
    /// newest failure - so forgetting it forgets the least: at worst someone who had been failing
    /// quietly gets to try again sooner than they should have.
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
        match tracked
            .by_address
            .get(&address)
            .and_then(|failures| failures.blocked_until(now))
        {
            Some(until) => Err(seconds_until(until, now)),
            None => Ok(()),
        }
    }

    /// One more failure for `address`.
    pub fn failure(&self, address: IpAddr) {
        let now = (self.clock)();
        let address = bucket(address);
        let tracked = &mut *locked(&self.tracked);
        if !tracked.by_address.contains_key(&address) {
            tracked.make_room();
        }
        let sequence = tracked.sequence;
        tracked.sequence = tracked.sequence.wrapping_add(1);
        match tracked.by_address.get_mut(&address) {
            Some(failures) => {
                failures.add(now);
                // Its place in the forgetting order moves with its newest failure.
                tracked.quietest_first.remove(&failures.place);
                failures.place = (now, sequence);
                tracked.quietest_first.insert(failures.place, address);
            }
            None => {
                let failures = Failures {
                    at: VecDeque::from([now]),
                    place: (now, sequence),
                };
                tracked.quietest_first.insert(failures.place, address);
                tracked.by_address.insert(address, failures);
            }
        }
    }

    /// A login worked: the address starts from clean again.
    pub fn success(&self, address: IpAddr) {
        let address = bucket(address);
        let tracked = &mut *locked(&self.tracked);
        if let Some(failures) = tracked.by_address.remove(&address) {
            tracked.quietest_first.remove(&failures.place);
        }
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

#[cfg(test)]
mod tests {
    use super::{LoginLimiter, MAX_FAILURES};
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
}
