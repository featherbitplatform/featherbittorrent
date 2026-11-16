//! The state file, read: what came back, and what to say about what did not.
//!
//! `session.state` is bytes the engine wrote on an earlier run, so it is hostile input like
//! anything from a peer: it may be missing, truncated, unreadable, or from a version that no
//! longer understands it. None of those stop the run. What cannot be used is kept under
//! another name rather than overwritten, and when even that is impossible this run stops
//! writing the file at all: an empty library saved over bytes nobody read is worse than a
//! library that has to be re-added by hand.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use fbt::{AddTorrent, SessionState, TorrentSource};

use crate::client::{Notice, TorrentId};
use crate::config::suffixed;

use super::map;

/// A state file that does not decode is kept under this suffix, the convention `config.yaml`
/// follows as well: never overwrite what could not be read.
const BAD_SUFFIX: &str = ".bad";
/// Resume data that came back short of the library it describes is copied out from under the
/// state file, which the next save would replace, under this suffix.
const BACKUP_SUFFIX: &str = ".bak";
/// `<name>.bad` and `<name>.bad.1` through `<name>.bad.99`, and the same for `.bak`: enough
/// names that a copy from two runs ago survives the one from this run, few enough that a
/// directory nobody can clean cannot grow without bound.
const LAST_TRY: u32 = 99;

/// What `session.state` gave this run: what to restore, what to say before anything starts,
/// and whether writing that file is allowed at all.
pub(super) struct Reading {
    /// The state to restore, `None` when there was nothing usable.
    pub(super) state: Option<SessionState>,
    /// The bytes it came from, so a run that could not put every torrent back can keep them.
    pub(super) bytes: Vec<u8>,
    /// One line each for what went wrong, in the order they happened.
    pub(super) notices: Vec<Notice>,
    /// True when this run must not write `session.state` at all: something is under that name
    /// this run could not read, and saving would replace it with a library of its own.
    pub(super) writes_blocked: bool,
}

/// Reads and decodes `session.state`, keeping aside whatever it cannot use.
///
/// There being no such file is ordinary: a library nobody has run yet. Anything else that this
/// run cannot turn into a library is either moved out of the way (a file that does not decode)
/// or left exactly where it is with writing turned off (a file it cannot read, or one it cannot
/// move): "cannot tell" is not the same answer as "not worth keeping", and only the second one
/// justifies overwriting.
pub(super) fn read_state(path: &Path) -> Reading {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Reading {
                state: None,
                bytes: Vec::new(),
                notices: Vec::new(),
                writes_blocked: false,
            };
        }
        // Something is there - a directory, a file on a mount that has gone, a permission this
        // process does not have - and nothing can be made of it. Start empty, but do not write:
        // the next periodic save would otherwise turn an unreadable library into an empty one.
        Err(source) => {
            let problem = format!("{}: {}", name_of(path), map::clean(&source.to_string()));
            return Reading {
                state: None,
                bytes: Vec::new(),
                notices: vec![blocked(&problem)],
                writes_blocked: true,
            };
        }
    };
    match SessionState::from_bytes(&bytes) {
        Ok(state) => Reading {
            state: Some(state),
            bytes,
            notices: Vec::new(),
            writes_blocked: false,
        },
        Err(_) => match keep_aside(path) {
            Ok(aside) => Reading {
                state: None,
                bytes,
                notices: vec![Notice::Error(format!(
                    "Saved torrents could not be read; the file was kept as {}",
                    name_of(&aside)
                ))],
                writes_blocked: false,
            },
            // It could not be moved out of the way either, so it stays under its own name and
            // this run leaves it alone. Next run reads the same bytes and says the same thing.
            Err(problem) => Reading {
                state: None,
                bytes,
                notices: vec![blocked(&problem)],
                writes_blocked: true,
            },
        },
    }
}

/// Keeps a copy of `bytes` beside the state file, under `<name>.bak` or the next free name
/// after it, and leaves the original where it is. Used when some torrents came back and others
/// did not: the save that follows describes only the ones that did, so these bytes live nowhere
/// else afterwards - which is why the copy is synced, and why a run that cannot make one stops
/// writing state at all.
///
/// Returns the name the copy went to, or the reason it did not go anywhere.
pub(super) fn keep_copy(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let Some(copy) = free_name(path, BACKUP_SUFFIX) else {
        return Err(names_taken(path, BACKUP_SUFFIX));
    };
    match write_synced(&copy, bytes) {
        Ok(()) => Ok(copy),
        Err(source) => Err(format!(
            "{}: {}",
            name_of(&copy),
            map::clean(&source.to_string())
        )),
    }
}

/// Writes a file and asks the device to have it before returning. `std::fs::write` hands the
/// bytes to the OS and calls that done; this copy is the only place its bytes exist by the time
/// the next save lands, so it is not done until [`sync_all`](std::fs::File::sync_all) says the
/// device has them.
fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// The line that says both halves of what happened: the library could not be read, and this run
/// will not be the one that replaces it.
fn blocked(problem: &str) -> Notice {
    Notice::Error(format!(
        "Saved torrents could not be read ({problem}); this run will not save torrents"
    ))
}

/// Moves an unreadable state file out of the way, to the first free `<name>.bad` name.
fn keep_aside(path: &Path) -> Result<PathBuf, String> {
    let Some(aside) = free_name(path, BAD_SUFFIX) else {
        return Err(names_taken(path, BAD_SUFFIX));
    };
    std::fs::rename(path, &aside)
        .map(|()| aside)
        .map_err(|source| {
            format!(
                "{} could not be moved aside: {}",
                name_of(path),
                map::clean(&source.to_string())
            )
        })
}

/// The first of `<file><suffix>`, `<file><suffix>.1` … `<file><suffix>.99` that nothing is
/// using, so a copy from an earlier run stays where it is. `None` when all hundred are taken.
fn free_name(path: &Path, suffix: &str) -> Option<PathBuf> {
    (0..=LAST_TRY).find_map(|index| {
        let candidate = numbered(path, suffix, index);
        match candidate.try_exists() {
            Ok(true) => None,
            // A name this process cannot even look at is not one to write into. There are
            // ninety-nine others, and the next run may be able to read this one.
            Err(_) => None,
            Ok(false) => Some(candidate),
        }
    })
}

/// `<file><suffix>` for index 0 - the plain name is the first one tried - and
/// `<file><suffix>.<index>` for every index after it.
fn numbered(path: &Path, suffix: &str, index: u32) -> PathBuf {
    if index == 0 {
        suffixed(path, suffix)
    } else {
        suffixed(path, &format!("{suffix}.{index}"))
    }
}

/// The reason a copy could not be made when every name is already used.
fn names_taken(path: &Path, suffix: &str) -> String {
    format!(
        "every copy name from {} to {} is taken",
        name_of(&numbered(path, suffix, 0)),
        name_of(&numbered(path, suffix, LAST_TRY))
    )
}

/// One line about a torrent the saved state could not put back. Its resume bytes are decoded
/// once more, only to name it: the failure came either from that decode or from the add that
/// followed it, and an id is something a person can point at. `kept` is where the file those
/// bytes came from was copied, so the data is not simply gone.
pub(super) fn restored_problem(
    state: &SessionState,
    index: usize,
    error: &fbt::SessionError,
    kept: Option<&Path>,
) -> String {
    let named = state
        .torrents
        .get(index)
        .and_then(|bytes| AddTorrent::from_resume(bytes).ok())
        .and_then(|params| match params.source {
            TorrentSource::Metainfo(ref metainfo) => Some(metainfo.info_hashes),
            TorrentSource::Magnet(ref magnet) => Some(magnet.info_hashes),
            _ => None,
        })
        .map(|hashes| {
            format!(
                "torrent {}",
                TorrentId::from_wire_hash(hashes.wire_hash()).0
            )
        });
    let kept = match kept {
        Some(path) => format!("; its bytes are kept as {}", name_of(path)),
        None => String::new(),
    };
    format!(
        "{} could not be restored: {}{}",
        named.unwrap_or_else(|| format!("torrent {index} of the saved state")),
        map::clean(&error.to_string()),
        kept,
    )
}

/// A file's own name, for a line that makes sense without reading a whole path.
fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
