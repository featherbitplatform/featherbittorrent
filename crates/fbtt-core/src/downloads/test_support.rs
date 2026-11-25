//! The questions a fixed two-second clock cannot ask, put where the automation can be interrupted.
//!
//! Production builds compile none of this: the `test-support` feature the `local`, `play`, `stream` and
//! `downloads` test targets already require is what makes it exist at all. Every knob is armed by the test
//! that needs it, fires once, and on one torrent or one file only, so a test that uses one changes nothing
//! about the tests running beside it in the same binary. The event knobs are keyed by info hashes for the
//! same reason: the tests share one process, and a torrent belongs to whichever test made it.
//!
//! * [`lag_when_it_finishes`] turns one torrent's `TorrentFinished` into the `Lagged` the session sends
//!   when a subscriber falls behind. The overflow itself needs a session busy enough to drop events into a
//!   queue 4096 deep, which no test can arrange and time.
//! * [`lose_the_removal`] does the same for a `TorrentRemoved`: the removal arrives as a lag and is gone,
//!   which is how a lost removal leaves claims behind that a re-added torrent then trips over.
//! * The watch-folder park ([`AutomationHandle::park_the_watch_folder`], on the handle) holds one of *that
//!   automation's* passes where the test can see it, which is how an event is made to arrive while the loop
//!   is busy rather than between two wakes. The session says "added" before the torrent's actor exists, so
//!   the only way the automation ever sees an added torrent as whole is by being busy while the engine
//!   checked its data.
//! * [`when_the_watch_folder_reads`] changes a candidate between the look that accepted it and the read of
//!   its bytes, which in one pass are microseconds apart.
//!
//! [`AutomationHandle::park_the_watch_folder`]: super::AutomationHandle::park_the_watch_folder

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fbt::{Event, InfoHashes};

/// How long a parked pass looks at whether it has been let go.
const PARK_POLL: Duration = Duration::from_millis(10);

/// A file the watch folder is about to read, changed under it.
#[derive(Debug)]
pub enum AtRead {
    /// Replace the file with a link to `target`, which is what a link swapped in looks like: the name is
    /// the one the folder accepted, and the bytes behind it belong to another file.
    SwapToSymlink {
        /// The file the link stands for, which the folder never looked at.
        target: PathBuf,
    },
    /// Grow the file to `bytes`, which is what appending to a settled file looks like to the read that was
    /// about to take it: the look that accepted it still says it is small.
    GrowTo {
        /// How big the file is by the time it is opened.
        bytes: u64,
    },
    /// Replace the file with a FIFO (a named pipe), which is what a name that was a regular file at the
    /// look and a pipe at the open looks like - and what a watch folder must survive without waiting: an
    /// ordinary read-mode open of a FIFO with no writer does not come back at all.
    #[cfg(unix)]
    SwapToFifo,
}

/// The torrents whose finish has not been turned into a lag yet.
static LAG_FOR: Mutex<Vec<InfoHashes>> = Mutex::new(Vec::new());
/// The torrents whose finish has been swapped for a lag: keyed by torrent, because each test asserts about
/// *its* event going missing, and a global flag would let one test's lag answer another test's question.
static LAGS_LOST: Mutex<Vec<InfoHashes>> = Mutex::new(Vec::new());
/// The torrents whose removal has not been lost yet.
static REMOVAL_FOR: Mutex<Vec<InfoHashes>> = Mutex::new(Vec::new());
/// The torrents whose removal [`lose_the_removal`] has eaten so far.
static REMOVALS_LOST: Mutex<Vec<InfoHashes>> = Mutex::new(Vec::new());
/// The changes the next reads of candidates are about to find, each armed for its own file. More than one
/// waits at a time because the tests in one binary run at the same moment, and each arms the file it is
/// watching; a single slot would let a test that armed last erase the one armed before it.
static AT_READ: Mutex<Vec<(PathBuf, AtRead)>> = Mutex::new(Vec::new());

/// The two flags one automation's park runs on, held by both the worker and its handle: the request and
/// the release cross the lock by atomics, and because the flags belong to one automation, parking *this*
/// handle parks *this* test's loop and nobody else's.
#[derive(Debug, Default)]
pub(crate) struct ParkFlags {
    /// Whether the next pass of this automation's watch folder should park itself.
    asked: AtomicBool,
    /// Whether a pass is parked now, which is how the test knows the loop is busy.
    parked: AtomicBool,
    /// Whether a parked pass may carry on.
    released: AtomicBool,
}

fn armed<T>(what: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    what.lock().expect("the test hooks are not poisoned")
}

/// Asks this automation's next watch folder pass to park itself, [`pass_is_parked`] saying when it has.
pub(crate) fn ask_for_park(flags: &ParkFlags) {
    flags.asked.store(true, Ordering::SeqCst);
}

/// Whether this automation's pass is parked right now: while it is, the loop is in the middle of a wake
/// and cannot act on anything the session says.
pub(crate) fn pass_is_parked(flags: &ParkFlags) -> bool {
    flags.parked.load(Ordering::SeqCst)
}

/// Lets a parked pass finish its look at the folder.
pub(crate) fn release_the_park(flags: &ParkFlags) {
    flags.released.store(true, Ordering::SeqCst);
}

/// The park this automation was asked for, if it was asked for: parked, then waited for, then let go,
/// once. The wait is the runtime's own ten-millisecond step, so the wake is held where it is without
/// standing on a thread.
pub(crate) async fn park_the_pass(flags: &ParkFlags) {
    if !flags.asked.swap(false, Ordering::SeqCst) {
        return;
    }
    flags.parked.store(true, Ordering::SeqCst);
    while !flags.released.swap(false, Ordering::SeqCst) {
        tokio::time::sleep(PARK_POLL).await;
    }
    flags.parked.store(false, Ordering::SeqCst);
}

/// Makes the next `TorrentFinished` for `torrent` arrive as the `Lagged` the session sends when a
/// subscriber's queue overflows: the finish itself is gone, exactly as it is on a busy session.
pub fn lag_when_it_finishes(torrent: InfoHashes) {
    armed(&LAG_FOR).push(torrent);
}

/// The event the loop acts on: the one it was sent, except for the finish [`lag_when_it_finishes`] named,
/// which arrives as a lag instead and is then spent.
pub(crate) fn lag_instead_of(event: Event) -> Event {
    let Event::TorrentFinished { torrent } = &event else {
        return event;
    };
    let waiting = {
        let armed = armed(&LAG_FOR);
        armed.iter().position(|seen| seen == torrent)
    };
    let Some(at) = waiting else {
        return event;
    };
    let _ = armed(&LAG_FOR).remove(at);
    armed(&LAGS_LOST).push(*torrent);
    tracing::debug!("the test hook dropped this torrent's finish from the event queue");
    Event::Lagged { missed: 1 }
}

/// Whether *this* torrent's finish has been swapped for a lag by [`lag_when_it_finishes`]: a test that
/// waits for a recovery wants to know the event it is recovering from really went missing - and only its
/// own, which a flag shared by the whole process cannot tell it.
pub fn lag_was_injected(torrent: &InfoHashes) -> bool {
    armed(&LAGS_LOST).iter().any(|lost| lost == torrent)
}

/// Makes the next `TorrentRemoved` for `torrent` arrive as the `Lagged` an overflowed queue would: the
/// removal is gone, exactly as it is on a busy session, and whatever the loop had filed under this
/// torrent keeps standing after the torrent itself has gone.
pub fn lose_the_removal(torrent: InfoHashes) {
    armed(&REMOVAL_FOR).push(torrent);
}

/// The event the loop acts on, once [`lag_instead_of`] has had its say: the removal [`lose_the_removal`]
/// named arrives as a lag instead and is then spent.
pub(crate) fn remove_instead_of(event: Event) -> Event {
    let Event::TorrentRemoved { torrent } = &event else {
        return event;
    };
    let waiting = {
        let armed = armed(&REMOVAL_FOR);
        armed.iter().position(|seen| seen == torrent)
    };
    let Some(at) = waiting else {
        return event;
    };
    let _ = armed(&REMOVAL_FOR).remove(at);
    armed(&REMOVALS_LOST).push(*torrent);
    tracing::debug!("the test hook dropped this torrent's removal from the event queue");
    Event::Lagged { missed: 1 }
}

/// Whether *this* torrent's removal was lost by [`lose_the_removal`]: the test asking has to know the
/// event whose absence it is testing really never arrived.
pub fn removal_was_lost(torrent: &InfoHashes) -> bool {
    armed(&REMOVALS_LOST).iter().any(|lost| lost == torrent)
}

/// Changes `path` the next time the watch folder is about to read it.
pub fn when_the_watch_folder_reads(path: PathBuf, what: AtRead) {
    armed(&AT_READ).push((path, what));
}

/// The change a test armed for this file, applied and spent. Anything else the folder reads is left
/// exactly as it is.
pub(crate) fn before_read(path: &Path) {
    let what = {
        let mut armed = armed(&AT_READ);
        let at = armed.iter().position(|(wanted, _)| *wanted == path);
        at.map(|index| armed.remove(index).1)
    };
    let Some(what) = what else { return };
    match what {
        AtRead::SwapToSymlink { target } => {
            fs::remove_file(path).expect("the test file was there to swap out");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, path).expect("make the link the test swaps in");
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(&target, path)
                .expect("make the link the test swaps in");
        }
        AtRead::GrowTo { bytes } => {
            let file = fs::OpenOptions::new()
                .write(true)
                .open(path)
                .expect("the test file was there to grow");
            file.set_len(bytes).expect("grow the test file");
        }
        #[cfg(unix)]
        AtRead::SwapToFifo => {
            fs::remove_file(path).expect("the test file was there to swap out");
            let made = std::process::Command::new("mkfifo")
                .arg(path)
                .status()
                .expect("run mkfifo to make the pipe the test swaps in");
            assert!(made.success(), "mkfifo made the pipe the test swaps in");
        }
    }
}
