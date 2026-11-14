//! One engine per library: the lock that keeps a second featherbittorrent off
//! files two engines would corrupt.
//!
//! The lock is an OS-level lock on `library.lock` in the library's data
//! directory ([`Paths::lock_file`](crate::Paths::lock_file)), taken with
//! [`File::try_lock`] rather than waited for: a competing instance is something
//! to tell the user about, not something to sit behind, so a start answers
//! straight away with [`LockError::Held`].
//!
//! The kernel releases the lock when the last handle to the file closes, which
//! happens on the guard's drop and on any exit, panic or kill. A crashed
//! featherbittorrent therefore frees its library by itself: there is no stale
//! lock to detect, timestamp or break, and no reason to ever delete the file.
//! The empty file stays behind between runs, which is fine; only the lock on it
//! counts.
//!
//! A second handle in the same process conflicts with the first exactly like a
//! second process does, on all three platforms, so the tests here need no child
//! process to check what two instances do.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

use thiserror::Error;

/// What can go wrong taking the library lock.
#[derive(Debug, Error)]
pub enum LockError {
    /// Another featherbittorrent is running on this library right now. This is
    /// the expected answer to a second start, not a failure: say so and either
    /// open another library or connect to the engine that is already here.
    #[error("another featherbittorrent holds the library lock")]
    Held,
    /// The lock file could not be created or locked for a reason that is not a
    /// competing holder: no permission, a file where a directory should be, a
    /// filesystem that cannot hold a lock at all.
    #[error("library lock: {0}")]
    Io(#[from] io::Error),
}

/// The library lock, held for as long as this guard is alive.
///
/// Take one with [`LibraryLock::acquire`] and keep it in scope for the whole
/// run of the engine: dropping it (or exiting) releases the library. It is not
/// `Clone` and cannot be taken twice on one path, because two guards would be
/// two engines on one set of resume data.
#[derive(Debug)]
pub struct LibraryLock {
    /// The file the lock is held on. The lock lives in this handle: keeping it
    /// open keeps the lock, closing it releases the lock.
    file: File,
}

impl LibraryLock {
    /// Takes the exclusive lock on `path`, making the file and its parent
    /// directories first if they are not there yet.
    ///
    /// The lock is tried, never waited for, so this cannot hang behind another
    /// instance. A holder is [`LockError::Held`]; anything that stops the file
    /// being created or opened is [`LockError::Io`] with the OS's own reason.
    ///
    /// The file is opened read-write and never truncated: taking the lock leaves
    /// its contents alone. Read-write is not decoration, it is what Windows
    /// requires of a handle that wants to take an exclusive lock.
    pub fn acquire(path: &Path) -> Result<LibraryLock, LockError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(LibraryLock { file }),
            // The only thing that locks this file is this function, so a holder
            // we cannot have is another featherbittorrent.
            Err(TryLockError::WouldBlock) => Err(LockError::Held),
            Err(TryLockError::Error(error)) => Err(LockError::Io(error)),
        }
    }
}

impl Drop for LibraryLock {
    /// Releases the lock. Closing the handle would release it on its own;
    /// unlocking first makes the release a step of ours, and an unlock that
    /// fails still leaves the close behind it.
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::{LibraryLock, LockError};
    use crate::scratch::ScratchDir;
    use std::fs;

    /// A second featherbittorrent on one library is refused instead of sharing
    /// the resume data with the first.
    #[test]
    fn second_lock_is_refused() {
        let dir = ScratchDir::new("lock_refused");
        // The directory below the lock file does not exist yet, which is how a
        // first run finds it: acquiring makes room for the file.
        let path = dir.path().join("data").join("library.lock");

        let first = LibraryLock::acquire(&path).expect("the first acquire wins");
        assert!(path.exists(), "acquiring creates the lock file");

        match LibraryLock::acquire(&path) {
            Err(LockError::Held) => {}
            other => panic!("a second acquire on one path is refused, got {other:?}"),
        }
        // The winner still holds it after the loser tried.
        assert!(
            matches!(LibraryLock::acquire(&path), Err(LockError::Held)),
            "the first guard keeps holding the lock"
        );
        drop(first);
    }

    /// The lock belongs to the guard, not to the file: dropping it frees the
    /// library, so a process that exits, panics or is killed leaves nothing
    /// locked.
    #[test]
    fn lock_is_released_on_drop() {
        let dir = ScratchDir::new("lock_drop");
        let path = dir.path().join("library.lock");

        let first = LibraryLock::acquire(&path).expect("the first acquire wins");
        assert!(
            matches!(LibraryLock::acquire(&path), Err(LockError::Held)),
            "a guard holds the lock while it lives"
        );
        drop(first);

        // The same release an exit, a panic or a kill gets for us.
        let again = LibraryLock::acquire(&path).expect("dropping the guard frees the lock");
        assert!(
            matches!(LibraryLock::acquire(&path), Err(LockError::Held)),
            "the second guard holds it too"
        );
        drop(again);
        LibraryLock::acquire(&path).expect("and dropping that one frees it again");
    }

    /// A lock file that cannot be opened is an IO error, not a holder: "cannot
    /// tell" is not the same answer as "someone else is here".
    #[test]
    fn a_parent_that_is_a_file_is_an_io_error() {
        let dir = ScratchDir::new("lock_io");
        let in_the_way = dir.path().join("not_a_directory");
        fs::write(&in_the_way, "a file where a directory belongs")
            .expect("write a file in place of a directory");

        match LibraryLock::acquire(&in_the_way.join("library.lock")) {
            Err(LockError::Io(error)) => assert!(
                !error.to_string().is_empty(),
                "the io error says what went wrong: {error}"
            ),
            other => panic!("a file in the way is an io error, got {other:?}"),
        }
    }

    /// Taking the lock does not rewrite the file it sits on.
    #[test]
    fn acquiring_does_not_empty_the_lock_file() {
        let dir = ScratchDir::new("lock_no_truncate");
        let path = dir.path().join("library.lock");
        fs::write(&path, "written before the lock was taken").expect("write a lock file");

        drop(LibraryLock::acquire(&path).expect("take the lock"));
        assert_eq!(
            fs::read_to_string(&path).expect("read the lock file back"),
            "written before the lock was taken"
        );
    }
}
