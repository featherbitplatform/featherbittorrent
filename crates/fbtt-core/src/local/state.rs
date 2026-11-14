//! The state file, read: what came back, and what to say about what did not.
//!
//! `session.state` is bytes the engine wrote on an earlier run, so it is hostile input
//! like anything from a peer: it may be missing, truncated, or from a version that no
//! longer understands it. None of those stop the run. What cannot be understood is kept
//! under another name rather than overwritten, because the next save would otherwise
//! erase bytes we never read.

use std::path::Path;

use fbt::{AddTorrent, SessionState, TorrentSource};

use crate::client::{Notice, TorrentId};
use crate::config::suffixed;

use super::map;

/// A state file that does not decode is kept under this suffix, the convention
/// `config.yaml` follows as well: never overwrite what could not be read.
const BAD_SUFFIX: &str = ".bad";

/// What `session.state` gave us, and what to tell the user about it.
///
/// There being no such file is ordinary: a library nobody has run yet. A file that does
/// not decode is renamed to `<file>.bad` and left where it was. A file that cannot be
/// opened at all is reported too, but left alone: "cannot tell" is not the same answer as
/// "not worth keeping".
pub(super) fn read_state(path: &Path) -> (Option<SessionState>, Vec<Notice>) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return (None, Vec::new());
        }
        Err(source) => {
            return (
                None,
                vec![Notice::Error(format!(
                    "cannot read {}: {source}",
                    path.display()
                ))],
            );
        }
    };
    match SessionState::from_bytes(&bytes) {
        Ok(state) => (Some(state), Vec::new()),
        Err(_) => {
            let aside = suffixed(path, BAD_SUFFIX);
            match std::fs::rename(path, &aside) {
                Ok(()) => (
                    None,
                    vec![Notice::Error(format!(
                        "Saved torrents could not be read; the file was kept as {}",
                        name_of(&aside)
                    ))],
                ),
                Err(source) => (
                    None,
                    vec![Notice::Error(format!(
                        "Saved torrents could not be read, and {} could not be moved aside: {source}",
                        name_of(path)
                    ))],
                ),
            }
        }
    }
}

/// One line about a torrent the saved state could not put back. Its resume bytes are
/// decoded once more, only to name it: the failure came either from that decode or from
/// the add that followed it, and an id is something a person can point at.
pub(super) fn restored_problem(
    state: &SessionState,
    index: usize,
    error: &fbt::SessionError,
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
    format!(
        "{} could not be restored: {}",
        named.unwrap_or_else(|| format!("torrent {index} of the saved state")),
        map::clean(&error.to_string())
    )
}

/// A file's own name, for a line that makes sense without reading a whole path.
fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
