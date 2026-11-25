//! The questions a fixed two-second clock cannot ask, put where the automation can be interrupted.
//!
//! Production builds compile none of this: the `test-support` feature the `local`, `play`, `stream` and
//! `downloads` test targets already require is what makes it exist at all. Every knob is armed by the test
//! that needs it, fires once, and on one torrent or one file only, so a test that uses one changes nothing
//! about the tests running beside it in the same binary.
//!
//! * [`lag_when_it_finishes`] turns one torrent's `TorrentFinished` into the `Lagged` the session sends
//!   when a subscriber falls behind. The overflow itself needs a session busy enough to drop events into a
//!   queue 4096 deep, which no test can arrange and time.
//! * [`park_the_watch_folder`] holds one watch folder pass where the test can see it, which is how an event
//!   is made to arrive while the loop is busy rather than between two wakes. The session says "added"
//!   before the torrent's actor exists, so the only way the automation ever sees an added torrent as whole
//!   is by being busy while the engine checked its data.
//! * [`when_the_watch_folder_reads`] changes a candidate between the look that accepted it and the read of
//!   its bytes, which in one pass are microseconds apart.

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
}

/// The torrents whose finish has not been turned into a lag yet.
static LAG_FOR: Mutex<Vec<InfoHashes>> = Mutex::new(Vec::new());
/// Whether a hook has yet swapped a finish for a lag, which is how a test knows it is testing the
/// recovery rather than the ordinary path beside it.
static LAG_INJECTED: AtomicBool = AtomicBool::new(false);
/// The changes the next reads of candidates are about to find, each armed for its own file. More than one
/// waits at a time because the tests in one binary run at the same moment, and each arms the file it is
/// watching; a single slot would let a test that armed last erase the one armed before it.
static AT_READ: Mutex<Vec<(PathBuf, AtRead)>> = Mutex::new(Vec::new());
/// Whether the next watch folder pass should park itself.
static PARK_ASKED: AtomicBool = AtomicBool::new(false);
/// Whether a pass is parked now, which is how the test knows the loop is busy.
static PARK_PARKED: AtomicBool = AtomicBool::new(false);
/// Whether a parked pass may carry on.
static PARK_RELEASED: AtomicBool = AtomicBool::new(false);

fn armed<T>(what: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    what.lock().expect("the test hooks are not poisoned")
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
    LAG_INJECTED.store(true, Ordering::SeqCst);
    tracing::debug!("the test hook dropped this torrent's finish from the event queue");
    Event::Lagged { missed: 1 }
}

/// Whether a finish has been swapped for a lag by [`lag_when_it_finishes`] since this process started: a
/// test that waits for a recovery wants to know the event it is recovering from really went missing.
pub fn lag_was_injected() -> bool {
    LAG_INJECTED.load(Ordering::SeqCst)
}

/// Parks the next watch folder pass: it stops there, [`watch_folder_is_parked`] says so, and it does not
/// move on until [`release_the_watch_folder`] is called.
pub fn park_the_watch_folder() {
    PARK_RELEASED.store(false, Ordering::SeqCst);
    PARK_ASKED.store(true, Ordering::SeqCst);
}

/// Whether a watch folder pass is parked right now: while it is, the automation is in the middle of a
/// wake and cannot act on anything the session says.
pub fn watch_folder_is_parked() -> bool {
    PARK_PARKED.load(Ordering::SeqCst)
}

/// Lets a parked pass finish its look at the folder.
pub fn release_the_watch_folder() {
    PARK_RELEASED.store(true, Ordering::SeqCst);
}

/// The park a test asked for, if it asked: parked, then waited for, then let go, once.
pub(crate) fn park_the_pass() {
    if !PARK_ASKED.swap(false, Ordering::SeqCst) {
        return;
    }
    PARK_PARKED.store(true, Ordering::SeqCst);
    while !PARK_RELEASED.swap(false, Ordering::SeqCst) {
        std::thread::sleep(PARK_POLL);
    }
    PARK_PARKED.store(false, Ordering::SeqCst);
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
    }
}
