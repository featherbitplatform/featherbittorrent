//! The one question round the binary asks the terminal before the event loop exists.
//!
//! [`detect`] writes [`PROBE_QUERY`] once, reads what comes back byte by byte, and hands those bytes to the
//! pure decision table in `fbtt_tui::graphics` (decision 10). It runs after raw mode is on and before
//! `EventStream` exists because it owns the tty for those few milliseconds: crossterm's reader would take
//! stdin with its own buffer, and a keystroke typed while the queries are in flight has to survive for that
//! reader rather than be swallowed here. So the read is done with `rustix` (Unix only), one byte per call,
//! and it stops at the first complete DA1 answer - the one reply every terminal sends, which is why DA1 is
//! last in the query string (decision 9).
//!
//! Two deadlines bound the read, both measured from the write: 300 ms, extended to one second once an answer
//! has started flowing. Nothing else about this module is unbounded: at most 4096 bytes are read, no
//! allocation grows past that, a terminal that answers nothing costs the shorter deadline, and nothing is
//! ever printed to the terminal on the way in or out.

use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

use crossterm::terminal::WindowSize;
use fbtt_tui::graphics::{Graphics, PROBE_QUERY, choose, da1_complete, parse_replies};

/// The wait a terminal that has not started answering is given: decision 9's 300 ms. Long enough for a
/// local terminal to answer six short queries, short enough that a terminal which never answers - a pipe, a
/// serial console, a terminal with the responses switched off - costs less than a blink.
const BASE_BUDGET: Duration = Duration::from_millis(300);

/// The same deadline once at least one byte has arrived, extended to a full second: a terminal that is
/// answering is given time to finish rather than cut off mid-sequence, which would make its answer count as
/// no answer at all (see `caps::parse_replies`).
const EXTENDED_BUDGET: Duration = Duration::from_millis(1_000);

/// The most bytes read from the tty, whatever the terminal sends. A response is remote input like any other
/// (Global Constraints), and a terminal that answers the queries with noise must not make this run allocate
/// without bound or keep reading past its deadline.
const MAX_BYTES: usize = 4_096;

/// The reasons [`skip_reason`] answers with. They say why a run has no image logo, for whoever ends up
/// logging or printing them; on screen the effect is only that the text title stays where it is.
const SKIP_WINDOWS: &str = "windows";
const SKIP_LOGO_OFF: &str = "logo off";
const SKIP_NOT_A_TTY: &str = "stdin is not a terminal";
const SKIP_TMUX: &str = "tmux";
const SKIP_SCREEN: &str = "gnu screen";
const SKIP_DUMB: &str = "TERM=dumb";

/// The tty the probe talks to, as this module needs it: the queries in one write, and the answers out one
/// byte at a time with a wait that gives up.
///
/// It is a trait so the timing rules can be tested against a scripted clock instead of a real terminal:
/// every test here knows exactly when each answer "arrives" and never sleeps.
pub trait Tty {
    /// Puts the queries on the wire, and does not return until they are out: the wait for an answer is
    /// measured from this point, so a query still sitting in a buffer here would spend part of it.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Waits up to `wait` for the next byte and returns it, or `None` when nothing arrived within it.
    ///
    /// One byte per call on purpose: reading only what a response needs is what leaves a keystroke typed
    /// after the answers in the tty for `EventStream` instead of in a buffer nobody can hand back.
    fn read_byte(&mut self, wait: Duration) -> io::Result<Option<u8>>;
}

/// The process's own tty, read through `rustix` (Unix only).
#[cfg(unix)]
#[derive(Debug)]
struct StdinTty;

#[cfg(unix)]
impl Tty for StdinTty {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        // Brought in here rather than at the top of the file because only this implementation writes
        // anything to a tty, and a build with no Unix tty would have no use for it.
        use std::io::Write as _;

        let mut out = std::io::stdout();
        out.write_all(bytes)?;
        // The flush is part of "on the wire": without it the queries could still be in the process's own
        // buffer while this run waits for an answer that will only be produced by what they say.
        out.flush()
    }

    fn read_byte(&mut self, wait: Duration) -> io::Result<Option<u8>> {
        use rustix::event::{PollFd, PollFlags, Timespec};

        // `Timespec` because `poll`'s timeout is relative to now. The wait can only ever be a second or
        // less (see `probe`), so the conversion cannot overflow; anything longer is refused rather than
        // silently truncated to something shorter.
        let Ok(timeout) = Timespec::try_from(wait) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the wait is too long to poll for",
            ));
        };
        // Never `std::io::stdin()`: the standard library would buffer a whole block of what the terminal
        // wrote back, and crossterm's reader - which owns stdin from `EventStream::new()` onwards - would
        // never see the bytes behind it. This is fd 0 read raw, one byte at a time (decision 9).
        let stdin = rustix::stdio::stdin();
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        // `0` is the wait running out with nothing on the wire. An error here - a signal interrupting the
        // wait, a tty that went away - goes out as an error, which ends the round with whatever was already
        // read rather than restarting it: `probe` owns the deadline, not this call.
        if rustix::event::poll(&mut fds, Some(&timeout))? == 0 {
            return Ok(None);
        }
        let mut buf = [0u8; 1];
        // `stdin` is a borrowed handle and `Copy`, so this takes it by value: the read does not own fd 0.
        match rustix::io::read(stdin, &mut buf)? {
            // The tty is closed: there are no answers, and nothing left to wait for.
            0 => Ok(None),
            _ => Ok(Some(buf[0])),
        }
    }
}

/// The tty the probe uses where stdin is not a byte stream: writes go nowhere and nothing ever arrives.
///
/// `skip_reason` never lets a Windows run as far as this (decision 9), so it is never actually asked
/// anything; it exists so that one `probe` compiles on every platform instead of two.
#[cfg(not(unix))]
#[derive(Debug)]
struct IdleTty;

#[cfg(not(unix))]
impl Tty for IdleTty {
    fn write_all(&mut self, _bytes: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn read_byte(&mut self, _wait: Duration) -> io::Result<Option<u8>> {
        Ok(None)
    }
}

/// Writes the queries once and reads what comes back, stopping at the first complete DA1 answer, at 4096
/// bytes, or at the deadline - whichever happens first.
///
/// `now` is the clock this reads its deadline from rather than `Instant::now` itself: production passes the
/// real one, and a test passes a fake it advances by exactly what was asked for.
pub fn probe(tty: &mut dyn Tty, now: impl Fn() -> Instant) -> Vec<u8> {
    let written_at = match tty.write_all(PROBE_QUERY) {
        Ok(()) => now(),
        // A tty that will not take the queries has nothing to answer with either.
        Err(_) => return Vec::new(),
    };
    let mut answers = Vec::new();
    loop {
        // The three ways this ends. DA1 is last in `PROBE_QUERY` and every terminal answers it, so a
        // complete DA1 is the end of the conversation; 4096 bytes is what a terminal that answers with
        // noise costs this run. Checking before reading is what keeps the round from taking one byte more
        // than it needs.
        if answers.len() >= MAX_BYTES || da1_complete(&answers) {
            break;
        }
        // The deadline, asked for rather than assumed: 300 ms from the write, or a full second once an
        // answer has started flowing. A terminal that is answering gets the time to finish - a response
        // cut in half counts as no response at all - and one that is not costs only the shorter wait.
        let budget = if answers.is_empty() {
            BASE_BUDGET
        } else {
            EXTENDED_BUDGET
        };
        let left = budget.saturating_sub(now().saturating_duration_since(written_at));
        if left.is_zero() {
            break;
        }
        match tty.read_byte(left) {
            Ok(Some(byte)) => answers.push(byte),
            // The wait ran out, or the read went wrong: a signal interrupted it, or the tty closed. Either
            // way what is in hand is all there is, and this does not repeat a read that failed.
            Ok(None) | Err(_) => break,
        }
    }
    answers
}

/// Whether this run should leave the text title where it is, and why; `None` means probe the terminal.
///
/// Decision 9's skip list, in the order it is worth knowing about: a Windows console, `logo: off`, stdin
/// that is not a terminal at all, tmux and GNU screen - which rewrite or swallow the responses, so waiting
/// for them costs the deadline for nothing - a `TERM` naming one of them anyway, which is the same
/// situation reached from a side where `TMUX` or `STY` did not survive into this environment, and
/// `TERM=dumb`.
///
/// `env` is a lookup rather than `std::env::var` so a test can name the environment it wants without putting
/// anything in this process's real one.
pub fn skip_reason(
    env: &dyn Fn(&str) -> Option<String>,
    stdin_is_tty: bool,
    logo_off: bool,
) -> Option<&'static str> {
    // A Windows console is not a byte stream crossterm leaves to us, so there is nothing here for the
    // probe to read (decision 9). The checks below still compile and are what a Unix run answers.
    if cfg!(not(unix)) {
        return Some(SKIP_WINDOWS);
    }
    // The person's own way out, asked before anything is spent finding out what the terminal can do.
    if logo_off {
        return Some(SKIP_LOGO_OFF);
    }
    // A run reading from a pipe or a file has no terminal to answer at all: this is also the case that
    // milestone 1 lets through with exit 3 for a taken library, and it must not pay a deadline for an
    // answer that will never come.
    if !stdin_is_tty {
        return Some(SKIP_NOT_A_TTY);
    }
    // Inside tmux or GNU screen the answers belong to the multiplexer, which rewrites or swallows them, so
    // waiting costs the deadline for nothing (`STY` is what a detached screen exports; `TMUX` is what tmux
    // sets in every shell it runs). A `TERM` naming one of them is the same situation from the side where
    // those variables did not survive into this environment.
    if env("TMUX").is_some() {
        return Some(SKIP_TMUX);
    }
    if env("STY").is_some() {
        return Some(SKIP_SCREEN);
    }
    let term = env("TERM").unwrap_or_default();
    if term == "dumb" {
        return Some(SKIP_DUMB);
    }
    if term.starts_with("tmux") {
        return Some(SKIP_TMUX);
    }
    if term.starts_with("screen") {
        return Some(SKIP_SCREEN);
    }
    None
}

/// The graphics verdict for the terminal this process is attached to: [`skip_reason`] first, then one probe
/// round, then the decision table over what was answered and this terminal's own sizes.
///
/// Nothing here draws anything: the queries go out through stdout, only the answers come back through stdin,
/// and the two `crossterm` size calls are ioctls that write nothing. Whatever the answer is, it costs at
/// most a second of startup and it never touches the screen.
pub fn detect(logo_off: bool) -> Graphics {
    let env = |name: &str| std::env::var(name).ok();
    if skip_reason(&env, std::io::stdin().is_terminal(), logo_off).is_some() {
        return Graphics::none();
    }
    // The grid and the window size are what `choose` divides when no query answered a cell size directly
    // (decision 11); a terminal that answers neither of these ioctls gets `(0, 0)`, which chooses nothing.
    let grid = crossterm::terminal::size().unwrap_or((0, 0));
    let pixels = crossterm::terminal::window_size()
        .ok()
        .as_ref()
        .and_then(window_pixels);
    choose(&parse_replies(&ask()), grid, pixels)
}

/// The probe round on this process's own tty: the queries out through stdout, the answers back in through
/// fd 0. Unix only; `IdleTty` is the twin this compiles against everywhere else.
#[cfg(unix)]
fn ask() -> Vec<u8> {
    let mut tty = StdinTty;
    probe(&mut tty, Instant::now)
}

/// Windows console input is not a byte stream crossterm leaves to us, so `skip_reason` has already said skip
/// by the time this could run (decision 9); an empty answer is what keeps one `detect` compiling here.
#[cfg(not(unix))]
fn ask() -> Vec<u8> {
    let mut tty = IdleTty;
    probe(&mut tty, Instant::now)
}

/// The window's device pixel size in the shape `choose` wants, or `None` when either side is zero.
///
/// A terminal that reports a zero-width or zero-height window is reporting that it does not know, and a
/// cell size divided out of "does not know" is 0, which would put an image nobody can see in the header.
fn window_pixels(size: &WindowSize) -> Option<(u16, u16)> {
    (size.width != 0 && size.height != 0).then_some((size.width, size.height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The answer a kitty sends to the graphics query.
    const KITTY_OK: &[u8] = b"\x1b_Gi=4242;OK\x1b\\";
    /// DA1's answer, listing `4`: the reply every terminal gives, and therefore the probe's stop condition.
    const DA1: &[u8] = b"\x1b[?62;4;22c";

    /// A tty whose answers arrive at scripted times, so no test here waits for anything real.
    ///
    /// Each segment lands whole at its offset from the start, and `read_byte` moves the shared clock the way
    /// a real wait would: to the moment the next answer is due when that falls inside `wait`, and by the
    /// whole `wait` when nothing does. A byte already due at the current instant is handed over without
    /// moving the clock, which is how every byte of a burst that has arrived gets read.
    struct FakeTty {
        /// The instant every offset is measured from.
        start: Instant,
        /// The clock `probe` reads through [`FakeTty::clock`] and moves through `read_byte`.
        clock: Rc<RefCell<Instant>>,
        /// What is left to answer with: `(ms from the start, bytes)`, in order.
        script: Vec<(u64, &'static [u8])>,
        /// Which segment is next, and how far into it the reads have got.
        segment: usize,
        offset: usize,
        /// The byte a terminal that never stops answering keeps handing out once the script runs out.
        endless: Option<u8>,
        /// What was written to this tty, one entry per write.
        writes: Vec<Vec<u8>>,
        /// Every `wait` this tty was asked for, in order.
        waits: Vec<Duration>,
        /// How many bytes were read.
        reads: usize,
    }

    impl FakeTty {
        /// A tty that answers with these segments and then goes quiet.
        fn new(script: Vec<(u64, &'static [u8])>) -> Self {
            Self::build(script, None)
        }

        /// A tty that answers nothing from a script and then keeps answering with one byte forever.
        fn endless(byte: u8) -> Self {
            Self::build(Vec::new(), Some(byte))
        }

        fn build(script: Vec<(u64, &'static [u8])>, endless: Option<u8>) -> Self {
            let start = Instant::now();
            FakeTty {
                clock: Rc::new(RefCell::new(start)),
                start,
                script,
                segment: 0,
                offset: 0,
                endless,
                writes: Vec::new(),
                waits: Vec::new(),
                reads: 0,
            }
        }

        /// The clock to hand `probe`: reading it never moves real time. It owns its own handle on this
        /// fake's time rather than borrowing `self`, which is what lets a test pass it to `probe` while it
        /// holds the tty mutably, and still read the same clock back afterwards.
        fn clock(&self) -> impl Fn() -> Instant + use<> {
            let clock = Rc::clone(&self.clock);
            move || *clock.borrow()
        }

        /// How far this fake's clock has moved since the queries were written, in ms.
        fn elapsed_ms(&self) -> u128 {
            (*self.clock.borrow() - self.start).as_millis()
        }

        /// The bytes this tty still holds unread: what a probe that stopped early left for `EventStream`.
        fn pending(&self) -> Vec<u8> {
            let mut out = Vec::new();
            if let Some((_, bytes)) = self.script.get(self.segment) {
                out.extend_from_slice(&bytes[self.offset..]);
            }
            if let Some(rest) = self.script.get(self.segment + 1..) {
                for &(_, bytes) in rest {
                    out.extend_from_slice(bytes);
                }
            }
            out
        }

        /// The next byte due at `elapsed` ms from the start, taking it; `None` when the next one is still in
        /// the future or nothing more will ever arrive.
        fn take_ready(&mut self, elapsed: u128) -> Option<u8> {
            loop {
                let Some((at_ms, bytes)) = self.script.get(self.segment).copied() else {
                    // Nothing scripted left: a terminal that never stops answering keeps giving this byte.
                    return self.endless;
                };
                if u128::from(at_ms) > elapsed {
                    return None;
                }
                match bytes.get(self.offset) {
                    Some(&byte) => {
                        self.offset += 1;
                        return Some(byte);
                    }
                    // This segment is spent, so the next one gets its turn at the same instant.
                    None => {
                        self.segment += 1;
                        self.offset = 0;
                    }
                }
            }
        }
    }

    impl Tty for FakeTty {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.writes.push(bytes.to_vec());
            Ok(())
        }

        fn read_byte(&mut self, wait: Duration) -> io::Result<Option<u8>> {
            self.waits.push(wait);
            let elapsed = self.elapsed_ms();
            if let Some(byte) = self.take_ready(elapsed) {
                self.reads += 1;
                return Ok(Some(byte));
            }
            // The next answer is due inside this wait: move the clock to it and hand over its first byte.
            // `take_ready` skipped the spent and empty segments above, so there is a byte here to take.
            if let Some((at_ms, _)) = self.script.get(self.segment).copied()
                && u128::from(at_ms) <= elapsed + wait.as_millis()
            {
                *self.clock.borrow_mut() = self.start + Duration::from_millis(at_ms);
                self.reads += 1;
                return Ok(Some(
                    self.take_ready(self.elapsed_ms())
                        .expect("an answer due at the instant the clock just moved to"),
                ));
            }
            // Nothing was due inside it: the whole wait is spent waiting, which is what a real `poll` does
            // too. This is the only way a test can show that a deadline was actually reached.
            *self.clock.borrow_mut() += wait;
            Ok(None)
        }
    }

    /// An environment for [`skip_reason`]: the pairs it answers with, and nothing else.
    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn probe_writes_query_once() {
        let mut fake = FakeTty::new(vec![(0, DA1)]);
        let clock = fake.clock();
        let answers = probe(&mut fake, clock);
        // One write of the whole query string: six queries in one go, so a terminal that answers them all
        // can do it in one burst (decision 9).
        assert_eq!(fake.writes, vec![PROBE_QUERY.to_vec()]);
        assert!(
            da1_complete(&answers),
            "the answer scripted at once is read"
        );
    }

    #[test]
    fn probe_stops_at_da1() {
        // A kitty that answers the graphics query, then DA1, then a keystroke typed while the queries were
        // in flight.
        let mut fake = FakeTty::new(vec![(0, KITTY_OK), (50, DA1), (80, b"q")]);
        let clock = fake.clock();
        let answers = probe(&mut fake, clock);
        assert_eq!(answers, [KITTY_OK, DA1].concat());
        // The keystroke is still the event loop's to read: this stopped at DA1's final `c` and never looked
        // past it (Review Focus 5).
        assert_eq!(fake.pending(), b"q");
    }

    #[test]
    fn probe_times_out() {
        // A terminal that never answers: nothing scripted, and nothing endless either.
        let mut fake = FakeTty::new(Vec::new());
        let clock = fake.clock();
        let answers = probe(&mut fake, clock);
        assert!(
            answers.is_empty(),
            "a terminal that never answers has no bytes"
        );
        // The budget is what was asked for and never more: each call may wait only for whatever the
        // deadline still has left, so a round cannot overshoot 300 ms by re-asking for a full one.
        let mut spent = Duration::ZERO;
        for wait in &fake.waits {
            assert!(
                *wait <= BASE_BUDGET.saturating_sub(spent),
                "asked to wait {wait:?} with only {:?} of the budget left",
                BASE_BUDGET.saturating_sub(spent),
            );
            spent += *wait;
        }
        // And it was waited out in full: the fake clock passing 300 ms is what ends the round.
        assert!(spent >= BASE_BUDGET, "the round gave up after {spent:?}");
        assert!(fake.elapsed_ms() >= 300);
    }

    #[test]
    fn probe_extends_when_answers_are_flowing() {
        // The first byte arrives at 250 ms, inside the 300 ms budget; DA1 only lands at 700 ms. Without
        // the extension the round would have ended at 300 ms with a truncated answer set.
        let mut fake = FakeTty::new(vec![(250, KITTY_OK), (700, DA1)]);
        let clock = fake.clock();
        let answers = probe(&mut fake, clock);
        assert_eq!(answers, [KITTY_OK, DA1].concat());
        // The waits say the extension happened: the first call gets the 300 ms base budget, and the next
        // one the remaining 750 ms of the extended second. (A 300 ms deadline would have asked for 50.)
        assert_eq!(fake.waits[0], Duration::from_millis(300));
        assert_eq!(fake.waits[1], Duration::from_millis(750));
    }

    #[test]
    fn probe_caps_bytes() {
        // A terminal that answers with noise forever: never a complete DA1, so only the byte cap ends it.
        let mut fake = FakeTty::endless(b'a');
        let clock = fake.clock();
        let answers = probe(&mut fake, clock);
        assert_eq!(answers.len(), MAX_BYTES);
        assert!(answers.iter().all(|byte| *byte == b'a'));
        // The cap is a stop rather than an overshoot: one read per byte and no 4097th.
        assert_eq!(fake.reads, MAX_BYTES);
    }

    #[test]
    fn skip_in_tmux_screen_dumb_and_off() {
        let tmux = env_of(&[
            ("TERM", "tmux-256color"),
            ("TMUX", "/tmp/tmux-1000/default,1234,0"),
        ]);
        let tmux_term_only = env_of(&[("TERM", "tmux-256color")]);
        let screen = env_of(&[("TERM", "screen-256color"), ("STY", "8640.pseudo-tty.0")]);
        let screen_term_only = env_of(&[("TERM", "screen.xterm")]);
        let dumb = env_of(&[("TERM", "dumb")]);
        let plain = env_of(&[("TERM", "xterm-256color")]);

        // Every case decision 9 says to skip: a multiplexer in front of the terminal, a stdin that is not
        // one at all, and the setting that turns the logo off.
        for env in [&tmux, &tmux_term_only, &screen, &screen_term_only, &dumb] {
            assert!(skip_reason(env, true, false).is_some());
        }
        assert!(
            skip_reason(&plain, false, false).is_some(),
            "stdin is a pipe"
        );
        assert!(skip_reason(&plain, true, true).is_some(), "logo: off");

        // Which reason each one says. Unix only: on Windows the platform rule answers first whatever the
        // environment says.
        #[cfg(unix)]
        {
            assert_eq!(skip_reason(&tmux, true, false), Some(SKIP_TMUX));
            assert_eq!(skip_reason(&tmux_term_only, true, false), Some(SKIP_TMUX));
            assert_eq!(skip_reason(&screen, true, false), Some(SKIP_SCREEN));
            assert_eq!(
                skip_reason(&screen_term_only, true, false),
                Some(SKIP_SCREEN)
            );
            assert_eq!(skip_reason(&dumb, true, false), Some(SKIP_DUMB));
            assert_eq!(skip_reason(&plain, false, false), Some(SKIP_NOT_A_TTY));
            assert_eq!(skip_reason(&plain, true, true), Some(SKIP_LOGO_OFF));
        }

        // The one case that gets probed. On Windows `skip_reason` always skips (decision 9), so the same
        // call answers differently there and this says so instead of pretending otherwise.
        assert_eq!(skip_reason(&plain, true, false).is_none(), cfg!(unix));
    }

    #[test]
    fn zero_window_pixels_are_no_cell_size() {
        // A window with no pixels on one side is the terminal saying it does not know its own size, and a
        // cell size divided out of that would be 0 (decision 11).
        let unknown = WindowSize {
            rows: 24,
            columns: 80,
            width: 0,
            height: 768,
        };
        assert_eq!(window_pixels(&unknown), None);
        let unknown = WindowSize {
            rows: 24,
            columns: 80,
            width: 1_024,
            height: 0,
        };
        assert_eq!(window_pixels(&unknown), None);
        let known = WindowSize {
            rows: 24,
            columns: 80,
            width: 1_024,
            height: 768,
        };
        assert_eq!(window_pixels(&known), Some((1_024, 768)));
    }
}
