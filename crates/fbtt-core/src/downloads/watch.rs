//! The decisions the watch folder makes about the files inside it, before any of them is handed to
//! the engine.
//!
//! A watch folder is somewhere a person drops files into, so everything here assumes the file is
//! still moving and that somebody else owns it. Two rules follow from that. A file is only read once
//! two scans in a row have seen the same size and the same modification time ([`settled`]): a copy
//! that is still running leaves a growing file behind, and reading half of it would call a perfectly
//! good torrent garbage. And a file is never thrown away - what has been dealt with is renamed with a
//! suffix, `.added` for one the engine took and `.invalid` for one it could not - and [`free_name`]
//! picks a suffix number nothing is using, so a second copy of a show never lands on the first one's
//! name.
//!
//! The reading, the parsing and the renaming all happen here, on the blocking pool ([`sweep`]). The
//! only thing left for the caller is handing the parsed torrent to the engine, which is a call it has
//! to await anyway.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::MAX_WATCHED_FILE;

/// The suffix of a torrent file the watch folder added, and of the copy it renamed out of the way.
pub(crate) const ADDED: &str = ".added";
/// The suffix of a file the engine could not take, whose reason is said out loud.
pub(crate) const INVALID: &str = ".invalid";
/// The highest suffix number [`free_name`] uses: `<name>.torrent.added.999` is the last name it
/// tries, and a folder holding a thousand copies of one show is that person's own arrangement.
const LAST_NUMBER: u32 = 999;
/// What a candidate's name has to end with, matched whatever its case because a camera writing
/// `SHOW.TORRENT` onto a card is holding a torrent too.
const TORRENT_SUFFIX: &str = ".torrent";
/// The same limit in the units the setting and the notice are stated in.
const MAX_WATCHED_MIB: u64 = MAX_WATCHED_FILE / (1024 * 1024);

/// What one scan saw of one candidate - its bytes and its modification time - and, keyed by path in
/// a `BTreeMap`, what a whole scan saw, in name order. [`settled`] compares two scans with it.
pub(crate) type Scan = BTreeMap<PathBuf, (u64, SystemTime)>;

/// A torrent the scan read and parsed, for the caller to hand to the engine.
pub(crate) struct Ready {
    /// The file, still where it was found: it is renamed once the engine has it.
    pub(crate) path: PathBuf,
    /// What it parsed into, ready for [`fbt::AddTorrent::new`].
    pub(crate) metainfo: fbt::Metainfo,
}

/// What one pass over the watch folder produced.
pub(crate) struct Sweep {
    /// Settled torrents that parsed, in name order.
    pub(crate) ready: Vec<Ready>,
    /// What the next pass compares against: the candidates still worth comparing, settled or not.
    pub(crate) scan: Scan,
    /// Files that are not going to be added, already renamed `.invalid`: the name to say in the
    /// notice, and the reason to say with it.
    pub(crate) refused: Vec<(String, String)>,
}

/// Whether `now` looks exactly the way this file looked to the previous scan - the same bytes and the
/// same modification time, which is as close as this gets to "nobody is still writing it".
///
/// A file's first look is never settled (`None`), so a `.torrent` sits in the folder through two scan
/// periods before anything reads it.
pub(crate) fn settled(previous: Option<(u64, SystemTime)>, now: (u64, SystemTime)) -> bool {
    matches!(previous, Some(seen) if seen == now)
}

/// What the watch folder makes of `path`: `None` for something it does not take - a folder, a link, a
/// name it hid itself, a name that does not end `.torrent` in any spelling - and otherwise the two
/// things [`settled`] needs, from one read of its metadata.
///
/// A link is not a candidate whatever it points at: renaming it would move somebody's link, the file
/// at the other end would stay where it is, and it would be added again.
fn candidate(path: &Path) -> Option<(u64, SystemTime)> {
    let name = path.file_name()?.to_str()?;
    if is_hidden_or_not_a_torrent(name) {
        return None;
    }
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some((meta.len(), meta.modified().ok()?))
}

/// Whether `name` is one the folder leaves alone: hidden (a leading dot, which is how `.added` and
/// `.invalid` renames hide themselves, and the `._show.torrent` a macOS download leaves beside the
/// real file), or too short to hold a name, or not ending `.torrent`.
///
/// The tail is cut by bytes: the suffix is ASCII, so cutting eight bytes from the end of a name is a
/// character boundary whatever came before it.
fn is_hidden_or_not_a_torrent(name: &str) -> bool {
    let Some(before) = name.len().checked_sub(TORRENT_SUFFIX.len()) else {
        return true;
    };
    name.as_bytes()[0] == b'.' || !name[before..].eq_ignore_ascii_case(TORRENT_SUFFIX)
}

/// A name for `<file><suffix>` in `dir` that nothing is using yet: that name itself, then
/// `<file><suffix>.2`, `.3` and on to `.999`.
///
/// Both renames go through here, so a second copy of the same show gets its own `.added` instead of
/// taking the first one's name away. The file being renamed is never in its own way, which is why the
/// question is about the suffixed name only.
///
/// A folder that holds all 999 names hands back the last one, and that one is taken: the caller looks
/// before it renames, and says so rather than replacing somebody's file.
pub(crate) fn free_name(dir: &Path, file: &str, suffix: &str) -> PathBuf {
    for number in 1..=LAST_NUMBER {
        let name = if number == 1 {
            format!("{file}{suffix}")
        } else {
            format!("{file}{suffix}.{number}")
        };
        let path = dir.join(name);
        if !path.exists() {
            return path;
        }
    }
    let last = LAST_NUMBER;
    dir.join(format!("{file}{suffix}.{last}"))
}

/// Reads the watch folder once, and does everything about it that does not need the engine: compare
/// against the previous scan, read and parse what has settled, and rename with `.invalid` the files
/// that will not be added - a torrent bigger than [`MAX_WATCHED_FILE`], whose bytes are never read at
/// all, and one that is not a torrent.
///
/// `Err` is why the folder could not be read at all, which is what goes beside the watch folder's
/// field - its kind says whether the folder is simply not there, which the caller words the way the
/// settings screen does. A folder that is not there is an `Err` too.
pub(crate) fn sweep(dir: &Path, previous: &Scan) -> Result<Sweep, std::io::Error> {
    let entries = fs::read_dir(dir)?;
    let mut scan = Scan::new();
    let mut ready = Vec::new();
    let mut refused = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some((size, modified)) = candidate(&path) else {
            continue;
        };
        let now = (size, modified);
        // Every candidate goes in the map, settled or not: this is what the next pass compares against,
        // and a file whose rename failed has to be recognisable as the one it already complained about.
        scan.insert(path.clone(), now);
        if !settled(previous.get(&path).copied(), now) {
            // Somebody may still be copying it in. Wait for the next look.
            continue;
        }
        let file = entry.file_name().to_string_lossy().into_owned();
        if size > MAX_WATCHED_FILE {
            let reason = format!(
                "the file is {size} bytes, over the {MAX_WATCHED_MIB} MiB a watch folder reads"
            );
            refused.push((file.clone(), mark_invalid(dir, &file, &reason)));
            continue;
        }
        let parsed = fs::read(&path)
            .map_err(|problem| problem.to_string())
            .and_then(|bytes| {
                fbt::Metainfo::from_bytes(&bytes).map_err(|problem| problem.to_string())
            });
        match parsed {
            Ok(metainfo) => ready.push(Ready { path, metainfo }),
            Err(problem) => {
                refused.push((file.clone(), mark_invalid(dir, &file, &problem)));
            }
        }
    }
    Ok(Sweep {
        ready,
        scan,
        refused,
    })
}

/// Renames `<dir><file>` out of the candidates' way with the `.invalid` suffix, keeping the file, and
/// hands back what the person should read: `reason` for the file that moved, or, when nothing moved,
/// why not.
fn mark_invalid(dir: &Path, file: &str, reason: &str) -> String {
    match rename_to_free(dir, file, INVALID) {
        Ok(_) => reason.to_owned(),
        Err(problem) => format!("{reason}, and it could not be renamed out of the way: {problem}"),
    }
}

/// Renames `path` to a free `<file><suffix>` name in the folder it is in: how a torrent file that has
/// been dealt with stops being a candidate, and the only way one ever leaves the watch folder.
///
/// `Err` is why nothing moved, which is what the caller says out loud. The file is never copied and
/// never deleted: this is one `rename`, so a marker is written for a file that is still there.
pub(crate) fn mark(path: &Path, suffix: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("the watched file has no folder")?;
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("the watched file has no name to mark")?;
    rename_to_free(dir, file, suffix)
        .map(|_| ())
        .map_err(|problem| problem.to_string())
}

/// The rename [`mark`] and [`mark_invalid`] both end up doing, and the name it used.
///
/// A name [`free_name`] hands back when the folder is out of numbers is a taken one, and renaming onto
/// it would replace somebody's file - which is a deletion, and this folder does not delete. So it is
/// an error instead, and the reason says what is in the way.
fn rename_to_free(dir: &Path, file: &str, suffix: &str) -> Result<PathBuf, std::io::Error> {
    let target = free_name(dir, file, suffix);
    if target.exists() {
        let last = LAST_NUMBER;
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{file}{suffix} and its {last} numbered copies are all taken"),
        ));
    }
    fs::rename(dir.join(file), &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::{ADDED, INVALID, candidate, free_name, settled};
    use crate::scratch::ScratchDir;
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// A candidate's look: its bytes and the time they were last written. Two of these with the same
    /// bytes and the same time are the same look, which is the whole of `settled`.
    fn look(size: u64, modified: u64) -> (u64, SystemTime) {
        (size, UNIX_EPOCH + Duration::from_secs(modified))
    }

    /// A candidate is `candidate` answering something about the file.
    fn is_candidate(path: &Path) -> bool {
        candidate(path).is_some()
    }

    #[test]
    fn settled_needs_two_equal_scans() {
        let first = look(4_096, 1_700_000_000);
        assert!(
            !settled(None, first),
            "a file seen for the first time is still arriving"
        );
        assert!(
            settled(Some(first), first),
            "the same bytes at the same time is a file nobody is writing"
        );

        let mut grew = first;
        grew.0 += 1;
        assert!(
            !settled(Some(first), grew),
            "the same file that got bigger is still being copied in"
        );

        let mut touched = first;
        touched.1 = UNIX_EPOCH + Duration::from_secs(1_700_000_001);
        assert!(
            !settled(Some(first), touched),
            "the same bytes written again is a file somebody is still working on"
        );
    }

    #[test]
    fn free_name_skips_taken_names() {
        let dir = ScratchDir::new("watch_free_name");
        let d = dir.path();

        // The file about to be renamed is here, and it is not in its own way.
        fs::write(d.join("x.torrent"), b"kept").unwrap();
        assert_eq!(free_name(d, "x.torrent", ADDED), d.join("x.torrent.added"));

        fs::write(d.join("x.torrent.added"), b"first").unwrap();
        assert_eq!(
            free_name(d, "x.torrent", ADDED),
            d.join("x.torrent.added.2"),
            "the plain name is taken, so the next copy is `.2`, not `.1`"
        );

        fs::write(d.join("x.torrent.added.2"), b"second").unwrap();
        assert_eq!(
            free_name(d, "x.torrent", ADDED),
            d.join("x.torrent.added.3"),
            "and on up the numbers"
        );

        // A number somebody freed is used again: the run of names has no gaps in it.
        fs::remove_file(d.join("x.torrent.added.2")).unwrap();
        assert_eq!(
            free_name(d, "x.torrent", ADDED),
            d.join("x.torrent.added.2"),
            "a name somebody deleted is free again"
        );

        // Each suffix is asked about on its own: a show's added copies do not block its invalid ones.
        assert_eq!(
            free_name(d, "x.torrent", INVALID),
            d.join("x.torrent.invalid"),
            "an added copy does not hold the invalid name"
        );
        fs::write(d.join("x.torrent.invalid"), b"junk").unwrap();
        assert_eq!(
            free_name(d, "x.torrent", INVALID),
            d.join("x.torrent.invalid.2")
        );

        // `.999` is the last name it tries, and there it stops: with every name taken it hands back a
        // taken one, and the caller says so instead of renaming over somebody's file.
        for number in 1..=999 {
            fs::write(d.join(format!("x.torrent.invalid.{number}")), b"copy").unwrap();
        }
        let last = free_name(d, "x.torrent", INVALID);
        assert_eq!(last, d.join("x.torrent.invalid.999"));
        assert!(
            last.exists(),
            "the name it hands back when there is none free is a taken one: {:?}",
            last
        );
    }

    #[test]
    fn only_plain_torrent_files_are_candidates() {
        let dir = ScratchDir::new("watch_candidates");
        let d = dir.path();
        fs::create_dir(d.join("season.torrent")).unwrap();
        fs::write(d.join("show.torrent"), b"ok").unwrap();
        fs::write(d.join("show.torrent.added"), b"added").unwrap();
        fs::write(d.join("show.torrent.invalid"), b"junk").unwrap();
        fs::write(d.join("show.added"), b"marker").unwrap();
        fs::write(d.join("notes.txt"), b"no").unwrap();
        fs::write(d.join(".torrent"), b"no").unwrap();
        fs::write(d.join("._show.torrent"), b"resource fork").unwrap();
        fs::write(d.join("notes.txt.torrent"), b"ok").unwrap();
        fs::write(d.join("SHOW.TORRENT"), b"ok").unwrap();
        fs::write(d.join("big.torrent"), b"its size is not the question").unwrap();
        let gone = d.join("gone.torrent");
        fs::write(&gone, b"here").unwrap();
        fs::remove_file(&gone).unwrap();
        fs::write(d.join("target.torrent"), b"real").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(d.join("target.torrent"), d.join("link.torrent")).unwrap();

        assert!(
            !is_candidate(&d.join("season.torrent")),
            "a folder named season.torrent is not a torrent file"
        );
        assert!(
            is_candidate(&d.join("show.torrent")),
            "a plain torrent file is"
        );
        assert!(
            !is_candidate(&d.join("show.torrent.added")),
            "a file already renamed `.added` is not a candidate again"
        );
        assert!(
            !is_candidate(&d.join("show.torrent.invalid")),
            "nor one renamed `.invalid`"
        );
        assert!(
            !is_candidate(&d.join("show.added")),
            "nor a marker of any other shape"
        );
        assert!(
            !is_candidate(&d.join("notes.txt")),
            "nor a plain file that is not a torrent"
        );
        assert!(
            !is_candidate(&d.join(".torrent")),
            "a file named .torrent has no name to add it under"
        );
        assert!(
            !is_candidate(&d.join("._show.torrent")),
            "a name the folder hid is left alone whatever it ends with"
        );
        assert!(
            is_candidate(&d.join("notes.txt.torrent")),
            "a dot in the middle of a name is still one torrent file"
        );
        assert!(
            is_candidate(&d.join("SHOW.TORRENT")),
            "the suffix is matched whatever its case: a camera card writes it in capitals"
        );
        assert!(
            is_candidate(&d.join("big.torrent")),
            "how big it is is not part of being a candidate: the scan says a big file is too big"
        );
        assert!(
            !is_candidate(&gone),
            "a file that is not there is not a candidate"
        );
        #[cfg(unix)]
        assert!(
            !is_candidate(&d.join("link.torrent")),
            "a link is somebody else's arrangement, and renaming it would not move the file"
        );
        assert!(
            is_candidate(&d.join("target.torrent")),
            "while the file the link stands for is still a candidate in its own right"
        );
    }
}
