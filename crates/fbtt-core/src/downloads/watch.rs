//! The decisions the watch folder makes about the files inside it, before any of them is handed to
//! the engine.
//!
//! A watch folder is somewhere a person drops files into, so everything here assumes the file is
//! still moving and that somebody else owns it. Three rules follow from that.
//!
//! * A file is only read once two scans in a row have seen the same size and the same modification time
//!   ([`settled`]): a copy that is still running leaves a growing file behind, and reading half of it
//!   would call a perfectly good torrent garbage. Those two looks are at a *name*, so what is read is
//!   checked again at the open ([`read_candidate`]): the name can have become a link, or a different
//!   file, or a bigger one in the microseconds since the look that accepted it, and the size limit is
//!   about bytes read into memory, so it is the size at the read that has to be the one asked about.
//! * A file is never thrown away - what has been dealt with is renamed with a suffix, `.added` for one
//!   the engine took and `.invalid` for one it could not - and [`free_name`] picks a suffix number
//!   nothing is using, so a second copy of a show never lands on the first one's name. A name is asked
//!   about by looking at the name itself (a dangling link is somebody's arrangement, not a gap), and the
//!   move refuses rather than replaces, so a name that appeared in the meantime is stepped over too.
//! * A file that has been read, offered to the engine and complained about is not read again while it
//!   still looks the way it did when it was dealt with ([`Unmoved`]): the rename that would have got it
//!   out of the way is no likelier to work at the next look, and reading and parsing it every two
//!   seconds buys a second copy of the same complaint.
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

/// The files one folder has already dealt with and could not move, keyed by path in name order, so one
/// pass hands its list to the next.
pub(crate) type UnmovedMap = BTreeMap<PathBuf, Unmoved>;

/// A watched file that was dealt with - read, parsed, offered to the engine, said about - and then could
/// not be renamed out of the way, so it is still sitting there as a candidate.
///
/// Its rename is not going to start working by itself: a folder whose `.added` numbers are all used up,
/// or one this user cannot write into. So the look it had when it was dealt with is remembered, and while
/// it still looks like that it is left alone instead of being read and parsed again on the next pass and
/// the one after that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unmoved {
    /// The size and modification time it had when it was dealt with, which is what the next pass compares
    /// against: a file that has been rewritten since then is a new question and gets read again.
    pub(crate) look: (u64, SystemTime),
    /// The whole line this run has already said about it. It is kept so that the settings screen shows
    /// the same words beside the field on every pass, rather than a paraphrase of them.
    pub(crate) line: String,
}

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
    /// Files that are not going to be added, each with the whole line to say about it: the ones renamed
    /// `.invalid` because their bytes are not a torrent or are bigger than [`MAX_WATCHED_FILE`], and the
    /// ones already dealt with whose rename failed ([`Unmoved`]) and which this pass left alone.
    pub(crate) refused: Vec<(PathBuf, String)>,
    /// What the next pass leaves alone for the same reason: this pass's files that are still there and
    /// still look the way they did when they were dealt with.
    pub(crate) unmoved: UnmovedMap,
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

/// The `<file><suffix>` name for one number: the plain name with the suffix for 1, then `.2`, `.3` and
/// on. A suffix number, never `.1`, because `show.torrent.added` *is* number 1 and a folder holding one
/// copy of a show should hold one name.
fn numbered(file: &str, suffix: &str, number: u32) -> String {
    if number == 1 {
        format!("{file}{suffix}")
    } else {
        format!("{file}{suffix}.{number}")
    }
}

/// Whether this name is somebody's already.
///
/// A link counts as taken even when what it stands for is gone: `Path::exists` follows links, so a
/// dangling one looks like a gap in the folder, and the file that lands on top of it takes somebody's
/// arrangement away - which is a deletion, in a folder that does not delete. A name that cannot be looked
/// at at all is treated as taken for the same reason: the safe answer to "would this throw something
/// away?" is to use the next number.
fn name_taken(path: &Path) -> bool {
    !matches!(
        fs::symlink_metadata(path),
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound
    )
}

/// The `<file><suffix>` name in `dir` that nothing is using, and its number, starting the search at
/// `first` and giving up after [`LAST_NUMBER`]: `None` means the folder holds them all.
///
/// Both renames go through here, so a second copy of a show gets its own `.added` instead of taking the
/// first one's name away. The file being renamed is never in its own way, which is why the question is
/// about the suffixed name only.
fn first_free_name(dir: &Path, file: &str, suffix: &str, first: u32) -> Option<(u32, PathBuf)> {
    (first..=LAST_NUMBER).find_map(|number| {
        let path = dir.join(numbered(file, suffix, number));
        (!name_taken(&path)).then_some((number, path))
    })
}

/// Reads the watch folder once, and does everything about it that does not need the engine: compare
/// against the previous scan, read and parse what has settled, and rename with `.invalid` the files that
/// will not be added - one bigger than [`MAX_WATCHED_FILE`], whose bytes are never read at all, one that
/// is not a torrent, and one that is not the same file it looked like to the look that accepted it.
///
/// `previous` is what the last pass saw, which is what makes a file settled. `unmoved` is what the last
/// pass dealt with and could not move: those files are left alone while they still look the way they did
/// then, and their line goes in `refused` so the field beside the watch folder keeps showing it.
///
/// `Err` is why the folder could not be read at all, which is what goes beside the watch folder's field -
/// its kind says whether the folder is simply not there, which the caller words the way the settings
/// screen does. A folder that is not there is an `Err` too.
pub(crate) fn sweep(
    dir: &Path,
    previous: &Scan,
    unmoved: &UnmovedMap,
) -> Result<Sweep, std::io::Error> {
    #[cfg(feature = "test-support")]
    super::test_support::park_the_pass();
    let entries = fs::read_dir(dir)?;
    let mut scan = Scan::new();
    let mut ready = Vec::new();
    let mut refused = Vec::new();
    let mut left = UnmovedMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some((size, modified)) = candidate(&path) else {
            continue;
        };
        let now = (size, modified);
        // Every candidate goes in the map, settled or not, dealt with or not: this is what the next pass
        // compares against, and a file that is still here has to be recognisable as the one it already
        // complained about.
        scan.insert(path.clone(), now);
        let file = entry.file_name().to_string_lossy().into_owned();
        if let Some(dealt_with) = unmoved.get(&path)
            && dealt_with.look == now
        {
            // Read, parsed, offered to the engine, complained about, and only the rename out of the
            // way failed - which is not going to start working on its own. Say the same thing about
            // it, and do not read it again.
            refused.push((path.clone(), dealt_with.line.clone()));
            left.insert(path, dealt_with.clone());
            continue;
        }
        if !settled(previous.get(&path).copied(), now) {
            // Somebody may still be copying it in. Wait for the next look.
            continue;
        }
        if size > MAX_WATCHED_FILE {
            let reason = format!(
                "the file is {size} bytes, over the {MAX_WATCHED_MIB} MiB a watch folder reads"
            );
            deal_with(dir, &file, &reason, now, &mut refused, &mut left);
            continue;
        }
        match read_candidate(&path, MAX_WATCHED_FILE).and_then(|bytes| {
            fbt::Metainfo::from_bytes(&bytes).map_err(|problem| problem.to_string())
        }) {
            Ok(metainfo) => ready.push(Ready { path, metainfo }),
            Err(problem) => deal_with(dir, &file, &problem, now, &mut refused, &mut left),
        }
    }
    Ok(Sweep {
        ready,
        scan,
        refused,
        unmoved: left,
    })
}

/// Renames `<dir><file>` `.invalid` - it is not going to be added - and records what the next pass needs:
/// the line to say about it, and the file itself in `left` when nothing moved, so it is not read again
/// every two seconds for the same reason.
fn deal_with(
    dir: &Path,
    file: &str,
    reason: &str,
    look: (u64, SystemTime),
    refused: &mut Vec<(PathBuf, String)>,
    left: &mut UnmovedMap,
) {
    let path = dir.join(file);
    let (line, still_there) = mark_invalid(dir, file, reason);
    refused.push((path, line.clone()));
    if still_there {
        left.insert(dir.join(file), Unmoved { look, line });
    }
}

/// Renames `<dir><file>` out of the candidates' way with the `.invalid` suffix, keeping the file, and
/// hands back the whole line the person should read - `reason`, wrapped in the folder's own words, plus
/// why nothing moved when nothing moved - and whether the file is still there.
///
/// The line is built here rather than by the caller so that the one said on the pass that found the file
/// and the one the field shows afterwards are the same words.
fn mark_invalid(dir: &Path, file: &str, reason: &str) -> (String, bool) {
    let started = format!("Could not add {file} from the watch folder: {reason}");
    match rename_to_free(dir, file, INVALID) {
        Ok(_) => (started, false),
        Err(problem) => (
            format!("{started}, and it could not be renamed out of the way: {problem}"),
            true,
        ),
    }
}

/// Renames `path` to a free `<file><suffix>` name in the folder it is in: how a torrent file that has
/// been dealt with stops being a candidate, and the only way one ever leaves the watch folder.
///
/// `Err` is why nothing moved, which is what the caller says out loud. The file is never copied and
/// never deleted: the bytes end up under the new name, or stay under the old one with a line about it.
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

/// Reads `path` as a settled candidate and hands back its bytes, or the reason to say about it.
///
/// The two questions asked here are the two the settling looks could not answer, because both of them
/// look at a *name* and the answer can change between the look and the read:
///
/// * Is this still the same file? The name is looked at as itself first (a link is not the file that was
///   agreed to), then the file is opened - following links on the way in, on every platform except
///   Windows, where the open is asked for the name itself - and the open file's own identity is compared
///   with what the name looked like a moment before. A link swapped in, or a file renamed away and
///   another written in its place, answers with a different device and inode, and is refused: what the
///   folder was told to add is the file it looked at, not whatever answers to that name now.
/// * Are these still the bytes that were agreed to? The size limit is about how much is read into
///   memory, so it is asked of the open file's own length, and again while reading - a file being
///   appended to can be longer by the last byte than it was at the open.
///
/// A file that is unreadable in itself - its name gone between the look and here, a folder it cannot
/// open - is refused with the operating system's reason, which is what the `.invalid` rename is filed
/// under.
fn read_candidate(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let looked_at = fs::symlink_metadata(path).map_err(|problem| problem.to_string())?;
    if !looked_at.is_file() {
        return Err(NOT_THE_SAME.to_owned());
    }
    #[cfg(feature = "test-support")]
    super::test_support::before_read(path);

    let mut open = fs::File::options();
    open.read(true);
    #[cfg(windows)]
    // `FILE_FLAG_OPEN_REPARSE_POINT`: open the name, not the file it stands for, so that an open cannot
    // follow a link that was not there when the name was looked at. std has no name for this flag, and
    // the crate does not carry a Windows crate to get one.
    open.custom_flags(0x0020_0000);
    let mut file = open.open(path).map_err(|problem| problem.to_string())?;

    let opened = file.metadata().map_err(|problem| problem.to_string())?;
    if !opened.is_file() {
        return Err(NOT_THE_SAME.to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // The same file has the same address on the same device. A link opened normally arrives as
        // whatever it stands for, which is a different address - so this one question catches both that
        // shape and the file that was swapped out from under the name.
        if opened.dev() != looked_at.dev() || opened.ino() != looked_at.ino() {
            return Err(format!(
                "it is a different file than the one that was looked at: {NOT_THE_SAME}"
            ));
        }
    }
    let bytes_now = opened.len();
    if bytes_now > cap {
        return Err(format!(
            "{bytes_now} bytes now, over the {MAX_WATCHED_MIB} MiB a watch folder reads"
        ));
    }
    match read_capped(&mut file, cap) {
        Ok(Some(bytes)) => Ok(bytes),
        // Longer at the last byte than it was at the open, which is the same question the size check
        // above asks, asked again because the file is not obliged to hold still in between.
        Ok(None) => Err(format!(
            "it grew while it was being read, past the {MAX_WATCHED_MIB} MiB a watch folder reads"
        )),
        Err(problem) => Err(problem.to_string()),
    }
}

/// The reason for every answer to "is this still the file I looked at?", which is one question whether it
/// became a link, a folder or somebody else's file.
const NOT_THE_SAME: &str = "it changed between the look and the read";

/// Takes at most `cap + 1` bytes from `file`: `None` is the answer "it is longer than that", found
/// without reading a whole huge file into memory to discover it, and `Err` is the file not being readable
/// in the first place.
///
/// `None` rather than a truncated `Ok`, because the caller's business is a torrent file's bytes: half of
/// one is not a shorter version of the answer, it is a parse error waiting to be misread as one.
fn read_capped(file: &mut dyn std::io::Read, cap: u64) -> std::io::Result<Option<Vec<u8>>> {
    // One byte more than the cap is all the question needs an answer to, so that is all that is taken:
    // a reader that returns everything it is asked for stops here, and a file being appended to cannot
    // run away from the question by being long.
    let want = usize::try_from(cap.saturating_add(1)).unwrap_or(usize::MAX);
    let mut bytes = vec![0u8; want.min(64 * 1024)];
    let mut filled = 0;
    while filled < want {
        if filled == bytes.len() {
            bytes.resize((filled * 2).max(bytes.len() * 2).min(want), 0);
        }
        match file.read(&mut bytes[filled..])? {
            // The end of the file, which is the other way a read comes back inside the limit.
            0 => break,
            read => filled += read,
        }
    }
    bytes.truncate(filled);
    // The limit is a limit on the bytes the caller gets, and the one byte more than the cap was taken
    // only to find out whether there was one.
    Ok((filled as u64 <= cap).then_some(bytes))
}

/// Moves `<dir><file>` under `target`, without replacing anything that is in the way.
///
/// A hard link rather than a rename, because of what each does to a name that turns out to be taken:
/// `rename` moves the file onto it and the thing that was there is gone - a deletion, in a folder that
/// does not delete - while `hard_link` answers `AlreadyExists` and the caller steps on to the next
/// number. A link's own name is moved with a plain rename: a hard link would follow it, so the link's
/// owner would find a file where their link was and their target's bytes duplicated in the watch folder.
///
/// A filesystem with no hard links - some network mounts and some FUSE readers answer `Unsupported` -
/// gets the plain rename, which is the only move that folder has. It can replace a name that appeared in
/// the microseconds between the look and the move; the failure is reported rather than swallowed, and a
/// folder that cannot link is not a folder that can promise not to.
///
/// The link and the name it came from are in the same folder, so a link never crosses a filesystem.
fn move_name(from: &Path, target: &Path, is_link: bool) -> Result<(), std::io::Error> {
    if is_link {
        return fs::rename(from, target);
    }
    match fs::hard_link(from, target) {
        Ok(()) => match fs::remove_file(from) {
            Ok(()) => Ok(()),
            // The bytes are safely under the new name and the old name is still pointing at them, which
            // means the file is still a candidate: undo the link and say what went wrong instead of
            // leaving two names for one torrent file behind.
            Err(problem) => {
                let _ = fs::remove_file(target);
                Err(problem)
            }
        },
        // A filesystem with no hard links at all: the plain rename is the only move this folder has.
        Err(problem) if problem.kind() == std::io::ErrorKind::Unsupported => {
            fs::rename(from, target)
        }
        Err(problem) => Err(problem),
    }
}

/// Puts `<dir><file>` under the first `<file><suffix>` name in its own folder that nothing is using, and
/// hands back the name it used.
///
/// [`first_free_name`] does the looking and [`move_name`] does the moving - and they are done again
/// together when the move answers "already exists", because a name that was free when the folder looked
/// and is taken by the time the move runs is exactly the case the search already steps over. Out of
/// numbers, the reason says what is in the way.
fn rename_to_free(dir: &Path, file: &str, suffix: &str) -> Result<PathBuf, std::io::Error> {
    let from = dir.join(file);
    let is_link = fs::symlink_metadata(&from).is_ok_and(|meta| meta.file_type().is_symlink());
    let mut first = 1;
    while let Some((number, target)) = first_free_name(dir, file, suffix, first) {
        match move_name(&from, &target, is_link) {
            Ok(()) => return Ok(target),
            Err(problem)
                if problem.kind() == std::io::ErrorKind::AlreadyExists && number < LAST_NUMBER =>
            {
                first = number + 1;
            }
            Err(problem) => return Err(problem),
        }
    }
    let last = LAST_NUMBER;
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("{file}{suffix} and its {last} numbered copies are all taken"),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        ADDED, INVALID, LAST_NUMBER, NOT_THE_SAME, Scan, Unmoved, UnmovedMap, candidate,
        first_free_name, name_taken, numbered, read_candidate, read_capped, settled, sweep,
    };
    use crate::scratch::ScratchDir;
    use std::fs;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// The name a pass would use, without its number: the first free one, or - when the folder holds all
    /// of them - a taken one, which the caller reports rather than renames onto.
    fn free_name(dir: &Path, file: &str, suffix: &str) -> PathBuf {
        first_free_name(dir, file, suffix, 1)
            .map(|(_, path)| path)
            .unwrap_or_else(|| dir.join(numbered(file, suffix, LAST_NUMBER)))
    }

    /// A candidate is `candidate` answering something about the file.
    fn is_candidate(path: &Path) -> bool {
        candidate(path).is_some()
    }

    /// A candidate's look: its bytes and the time they were last written. Two of these with the same
    /// bytes and the same time are the same look, which is the whole of `settled`.
    fn look(size: u64, modified: u64) -> (u64, SystemTime) {
        (size, UNIX_EPOCH + Duration::from_secs(modified))
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

    /// The names directly inside a folder, sorted: what the rename tests compare against when they say a
    /// file was moved rather than copied or thrown away.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("the folder reads")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A link whose target is not there is somebody's arrangement with a name on it. `Path::exists` follows
    /// links, so it looks like a gap in the folder - and the file that lands on top of it takes the
    /// arrangement away, which is a deletion in a folder that does not delete.
    #[cfg(unix)]
    #[test]
    fn a_dangling_link_is_a_taken_name() {
        let dir = ScratchDir::new("watch_dangling_names");
        let d = dir.path();
        let nowhere = d.join("nowhere");
        let dangling = d.join("show.torrent.added");
        std::os::unix::fs::symlink(&nowhere, &dangling)
            .expect("a link to a file that is not there");

        assert!(
            !nowhere.exists(),
            "this test is about a link whose target is not there"
        );
        assert!(
            name_taken(&dangling),
            "a dangling link is a name in use, not a gap the folder can step into"
        );
        assert_eq!(
            free_name(d, "show.torrent", ADDED),
            d.join("show.torrent.added.2"),
            "so the added file steps over it rather than replacing it"
        );

        // And it is still a name in use once its target arrives: the question is about the name, not about
        // whatever happens to answer to it.
        fs::write(&nowhere, b"arrived").expect("the target arrives");
        assert!(name_taken(&dangling));
    }

    /// The cap is asked of the bytes taken, not of a length somebody was told about: one byte over it is
    /// enough to answer "longer than that", and answering must not cost reading the rest of the file.
    #[test]
    fn read_capped_takes_the_limit_and_no_more() {
        let bytes: &[u8] = b"0123456789abcdef";

        assert_eq!(
            read_capped(&mut Cursor::new(bytes), 8).expect("a reader that is not broken"),
            None,
            "twelve bytes are over a cap of eight, and they are not truncated into an answer"
        );
        assert_eq!(
            read_capped(&mut Cursor::new(bytes), 16)
                .expect("a reader that is not broken")
                .expect("exactly the cap is the cap")
                .len(),
            16,
            "a file of exactly the cap fits it"
        );
        assert_eq!(
            read_capped(&mut Cursor::new(bytes), 64)
                .expect("a reader that is not broken")
                .expect("a cap wider than the file")
                .len(),
            16,
            "and a wider cap takes what is there and stops at the end of it"
        );
        assert_eq!(
            read_capped(&mut Cursor::new(&[] as &[u8]), 0)
                .expect("an empty file is not over a cap"),
            Some(Vec::new()),
            "an empty file fits any cap, including none"
        );
    }

    /// The size limit is asked of the file as it is when it is opened, not as it looked to the look that
    /// accepted it: two looks agreeing is the folder's answer to "is somebody still writing this?", and it
    /// is a look at a name.
    #[test]
    fn a_file_bigger_at_the_open_than_the_cap_is_refused_by_its_new_size() {
        let dir = ScratchDir::new("watch_bigger_at_the_open");
        let path = dir.path().join("grown.torrent");
        fs::write(&path, b"a torrent file of twenty-eight bytes").expect("write the file");

        let reason =
            read_candidate(&path, 8).expect_err("twenty-eight bytes are over a cap of eight");
        assert!(
            reason.contains("bytes now") && reason.contains("over the"),
            "the reason is the size there at the read, not the size that was agreed to: {reason}"
        );
        assert_eq!(
            read_candidate(&path, 1_000).expect("the same file fits a wider cap"),
            b"a torrent file of twenty-eight bytes",
            "and nothing about the file itself was the problem"
        );
    }

    /// A link put where the file was - after the look that accepted it, before the open of it - is refused,
    /// and nothing is read from behind it. The test-support hook is what splits those two moments apart,
    /// which is why this test needs the feature the `downloads` test target already requires.
    #[cfg(all(unix, feature = "test-support"))]
    #[test]
    fn a_link_swapped_in_before_the_open_is_refused() {
        let dir = ScratchDir::new("watch_link_before_open");
        let d = dir.path();
        let path = d.join("swapped.torrent");
        let target = d.join("other.torrent");
        fs::write(&path, b"d8:announce...").expect("the file the folder looked at");
        fs::write(
            &target,
            b"somebody else's file, which the folder never looked at",
        )
        .expect("what is behind it");
        crate::downloads::test_support::when_the_watch_folder_reads(
            path.clone(),
            crate::downloads::test_support::AtRead::SwapToSymlink {
                target: target.clone(),
            },
        );

        let reason =
            read_candidate(&path, 1_000).expect_err("the name is a link by the time it is opened");

        assert!(
            reason.contains(NOT_THE_SAME) && reason.contains("different file"),
            "the reason is that this is not the file that was looked at: {reason}"
        );
        assert!(
            fs::symlink_metadata(&path)
                .expect("the file behind the name is untouched")
                .file_type()
                .is_symlink(),
            "and refusing it means nothing was read from behind the link and nothing was moved"
        );
    }

    /// A file that was dealt with and could not be moved is left alone while it still looks the way it did
    /// then: its bytes are not read again, its rename is not tried again, and the line already said about
    /// it is the line the field goes on showing.
    #[test]
    fn a_dealt_with_file_is_left_alone_while_it_looks_the_same() {
        let dir = ScratchDir::new("watch_dealt_with");
        let path = dir.path().join("show.torrent");
        fs::write(&path, b"not a torrent at all").expect("write the file");
        let meta = fs::symlink_metadata(&path).expect("look at it");
        let look = (meta.len(), meta.modified().expect("its time"));
        let line =
            "Could not add show.torrent from the watch folder: it is in the library, but it \
                    could not be renamed out of the way: Read-only file system (os error 30)"
                .to_owned();
        // Settled: the last pass saw exactly this. The only thing left to do with it was the rename, and \
        // that failed, and it is the rename that is not going to start working by itself.
        let mut previous = Scan::new();
        previous.insert(path.clone(), look);
        let mut unmoved = UnmovedMap::new();
        unmoved.insert(
            path.clone(),
            Unmoved {
                look,
                line: line.clone(),
            },
        );

        let swept = sweep(dir.path(), &previous, &unmoved).expect("the folder reads");

        assert!(
            swept.ready.is_empty(),
            "its bytes were not read and parsed again, so nothing new can be added from them"
        );
        assert_eq!(
            swept.refused,
            vec![(path.clone(), line)],
            "the same line, word for word, is what this pass reports about it"
        );
        assert_eq!(
            swept.unmoved, unmoved,
            "and it is still owed the same treatment next pass, while it looks the same"
        );
        assert_eq!(
            names(dir.path()),
            vec!["show.torrent".to_owned()],
            "and the folder still holds it under the name its owner left it at"
        );
    }

    /// The same file, rewritten, is a new question: the record of having been dealt with goes with the look
    /// it was dealt with at, so a person who edits the file and leaves it there gets it read again.
    #[test]
    fn a_dealt_with_file_that_has_been_rewritten_is_read_again() {
        let dir = ScratchDir::new("watch_dealt_with_rewritten");
        let path = dir.path().join("show.torrent");
        fs::write(&path, b"not a torrent at all").expect("write it once");
        let meta = fs::symlink_metadata(&path).expect("look at it");
        let dealt_with_at = (meta.len(), meta.modified().expect("its time"));
        let remembered =
            "Could not add show.torrent from the watch folder: not a torrent".to_owned();
        let mut unmoved = UnmovedMap::new();
        unmoved.insert(
            path.clone(),
            Unmoved {
                look: dealt_with_at,
                line: remembered.clone(),
            },
        );
        // Rewritten, bigger, and settled again since: two passes on the new bytes, which is what makes it
        // worth a fresh look.
        fs::write(&path, b"not a torrent at all, and longer now").expect("write it again");
        let meta = fs::symlink_metadata(&path).expect("look at it again");
        let mut previous = Scan::new();
        previous.insert(
            path.clone(),
            (meta.len(), meta.modified().expect("its new time")),
        );

        let swept = sweep(dir.path(), &previous, &unmoved).expect("the folder reads");

        assert!(
            swept.unmoved.is_empty(),
            "the record was of a file that is no longer there in that shape: {:?}",
            swept.unmoved
        );
        assert_eq!(
            swept.refused.len(),
            1,
            "and this pass has something new to say about it: {:?}",
            swept.refused
        );
        assert!(
            swept.refused[0].1 != remembered,
            "what it says is the reason this pass found, not the one it remembered: {:?}",
            swept.refused
        );
        assert_eq!(
            names(dir.path()),
            vec!["show.torrent.invalid".to_owned()],
            "and it was read, refused and renamed once more, as a new file is"
        );
    }
}
