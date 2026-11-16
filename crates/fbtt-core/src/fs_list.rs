//! The folders a person may pick, listed one way wherever featherbittorrent answers that question:
//! the server's `GET /api/v1/fs/list` and [`Command::ListDirs`](crate::client::Command::ListDirs) on a
//! person's own machine.
//!
//! This is the destination picker's data source, the list someone walks to choose where a torrent goes,
//! so it answers with what a picker needs and nothing else: folder names, sorted, no files, no sizes,
//! nothing hidden. Which folders may be named at all is the caller's: a server keeps
//! `server.allowed_roots` ([`Root::prepare`]), the local backend keeps its download folder and the
//! person's home directory ([`browse_roots`]). What a listing then does with that list is decided here,
//! once.
//!
//! Two spellings of every root are kept: as it was named, and canonicalised. A path is refused before the
//! file system is touched at all unless it is absolute, has no `..` in it and sits below a root in one of
//! those two spellings ([`inside_lexically`]) - the order fbt-api checks its own paths in, so nothing
//! dials this server or walks this file system towards something outside the library. Only then is the
//! path canonicalised, which is what resolves a symlink and lets the containment check refuse one that
//! leads out of every root. The same rule then applies to what a listing *contains*: a link inside a root
//! is offered only if wherever it points is inside a root too, because otherwise a picker could leave
//! through an alias one directory at a time and whatever handed it the way out would have looked like
//! part of the library. The answer reports the spelling that was asked for, not the resolved one: it is
//! the one the person typed, and the one that works when they ask again.

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use crate::engine::map::clean;

/// The most directories one listing reports. Enough to pick a folder from in any real library's tree,
/// and the reason a folder with half a million entries in it costs two thousand names rather than half a
/// million - whatever operating system this is running on, or whoever asked.
pub const MAX_DIRS: usize = 2_000;

/// One folder a listing may name, in both spellings it may be named in.
///
/// Both are needed because a path arrives in whichever spelling the person typed, and the two differ for
/// an ordinary folder on some platforms: on macOS `/var` is a link to `/private/var`, so a root below a
/// temp directory has two absolute spellings that are not prefixes of one another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// As it was named: what `GET /api/v1/fs/roots` reports, what [`DirListing::roots`](crate::client::DirListing::roots)
    /// echoes back, and what a listing's own answer spells itself as.
    pub given: PathBuf,
    /// Its canonical form, with every symlink in it resolved: what containment is checked against, so a
    /// folder reached through a symlink alias of the root itself still answers to its own root. A folder
    /// that was never made keeps its own spelling here ([`browse_roots`]).
    pub canonical: PathBuf,
}

impl Root {
    /// The folders a server serves, as directories it can confine requests to: each one made if it is not
    /// there yet, then canonicalised.
    ///
    /// Making them is what lets an API name a folder before the first download has created it - fbt-api
    /// refuses to start with an allowed root that does not exist, and a destination picker whose root is
    /// missing has nothing to pick from. The `Err` is one line naming the folder and what stopped it, fit
    /// for a startup message: at this point nobody is talking to the server yet, so whoever started it is
    /// the one who has to act on it.
    pub fn prepare(configured: &[PathBuf]) -> Result<Vec<Root>, String> {
        let mut roots = Vec::with_capacity(configured.len());
        for given in configured {
            std::fs::create_dir_all(given).map_err(|problem| {
                format!(
                    "cannot create the allowed folder {} ({problem})",
                    clean(&given.to_string_lossy()),
                )
            })?;
            let canonical = given.canonicalize().map_err(|problem| {
                format!(
                    "cannot use the allowed folder {} ({problem})",
                    clean(&given.to_string_lossy()),
                )
            })?;
            roots.push(Root {
                given: given.clone(),
                canonical,
            });
        }
        Ok(roots)
    }
}

/// The folders a listing browses on the machine it runs on, none of them made: each one in both
/// spellings, canonicalised where the file system can say what the folder points at and spelled as given
/// where it cannot.
///
/// A folder that is not there yet - the download folder before the first torrent has made it - stays in
/// the list, because a person picking somewhere to save still has to be told where their torrents go. It
/// confines nothing and hides nothing meanwhile: nothing below an absent folder resolves either, so its
/// own spelling is as far as containment can see. A server's roots are made first instead
/// ([`Root::prepare`]) because that is what its own API requires of them; browsing one's own machine has
/// no such requirement, and a command that only reports what is on disk should not go making folders.
pub fn browse_roots(given: &[PathBuf]) -> Vec<Root> {
    given
        .iter()
        .map(|given| Root {
            canonical: given.canonicalize().unwrap_or_else(|_| given.clone()),
            given: given.clone(),
        })
        .collect()
}

/// Whether `path` is absolute, has no `..` anywhere in it and lies below one of `roots` in either of its
/// spellings. Purely lexical: this is the guard that runs before anything on disk is looked at, and it is
/// part of [`list_dirs`] as well so that a caller cannot reach the file system by skipping it.
pub fn inside_lexically(roots: &[Root], path: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|c| matches!(c, Component::ParentDir))
        && roots
            .iter()
            .any(|root| path.starts_with(&root.given) || path.starts_with(&root.canonical))
}

/// Why one folder could not be listed.
///
/// Each variant answers a different question, so each caller gives it the shape its own users read: the
/// server's route turns them into HTTP statuses and codes, the local backend puts the line this type
/// prints in [`DirListing::error`](crate::client::DirListing::error) beside the folder it failed on. Every
/// path in these lines went through `display_safe` on the way in, so they are fit to draw or to log as
/// they are.
#[derive(Debug, thiserror::Error)]
pub enum ListingError {
    /// Nothing is there: `404`.
    #[error("no folder {0}")]
    Missing(String),
    /// Not below any root - by its own spelling, or by where a link took it once that was resolved:
    /// `403 path_not_allowed`.
    #[error("{0} is outside the allowed folders")]
    Outside(String),
    /// A name no operating system could take: too long, reserved by this platform, a NUL in the middle.
    /// The request's mistake to fix, not this program's failure to answer: `400`.
    #[error("invalid path {0}")]
    Unusable(String),
    /// Something is there; it is not a folder, so nothing can be inside it: `400`.
    #[error("{0} is not a folder")]
    NotAFolder(String),
    /// Below a root and still refused: that is the file system's own answer to who may read it: `403`.
    #[error("{0} cannot be read")]
    ReadDenied(String),
    /// The resolved path could not be looked at: this program's problem, not the folder's: `500`.
    #[error("cannot check {shown} ({problem})")]
    CannotCheck { shown: String, problem: String },
    /// The folder is there and still could not be read: `500`.
    #[error("cannot read {shown} ({problem})")]
    CannotRead { shown: String, problem: String },
}

/// The directories directly inside `path`, spelled as `path` was spelled.
///
/// Names come back cleaned for display and sorted, files and hidden entries (the dot convention, on every
/// platform) are left out, and at most [`MAX_DIRS`] names are reported - the alphabetically first ones,
/// so the same folder answers the same way twice rather than in whatever order the file system happened
/// to hand them out. A path outside every root is [`ListingError::Outside`], whether it got there by
/// `..`, by being somewhere else entirely, or by being a symlink that leads out; one that is not there is
/// [`ListingError::Missing`]; one that is not a folder, or that no operating system could take, is
/// [`ListingError::Unusable`] or [`ListingError::NotAFolder`].
///
/// This blocks: it reads a directory. Call it from a blocking thread ([`tokio::task::spawn_blocking`])
/// when something else is waiting on the answer.
pub fn list_dirs(roots: &[Root], path: &Path) -> Result<Vec<String>, ListingError> {
    let shown = clean(&path.to_string_lossy());
    // Lexical first, so a path any caller would refuse anyway costs a comparison and not a stat - and so
    // that `..` is refused before anything has followed it somewhere else.
    if !inside_lexically(roots, path) {
        return Err(ListingError::Outside(shown));
    }
    let canonical = match path.canonicalize() {
        Ok(canonical) => canonical,
        Err(problem) if problem.kind() == ErrorKind::NotFound => {
            return Err(ListingError::Missing(shown));
        }
        // A name the operating system will not take at all: too long, or on Windows one it has reserved
        // for something else. Said this way rather than per platform: a NUL byte, the case that arrives
        // most easily through a percent-decoded query, is refused by the route before it gets here
        // because not every platform reports it as this error.
        Err(problem) if problem.kind() == ErrorKind::InvalidInput => {
            return Err(ListingError::Unusable(shown));
        }
        Err(problem) => {
            return Err(ListingError::CannotCheck {
                shown,
                problem: problem.to_string(),
            });
        }
    };
    // The links are resolved now, so a folder reached through one that leads outside every root is
    // refused here although its spelling was inside: this is the check that cannot be fooled by an alias.
    if !roots
        .iter()
        .any(|root| canonical.starts_with(&root.canonical))
    {
        return Err(ListingError::Outside(shown));
    }
    if !canonical.is_dir() {
        return Err(ListingError::NotAFolder(shown));
    }
    let entries = match std::fs::read_dir(&canonical) {
        Ok(entries) => entries,
        // Inside a root, and still refused: that is the file system's own answer to who may read it.
        Err(problem) if problem.kind() == ErrorKind::PermissionDenied => {
            return Err(ListingError::ReadDenied(shown));
        }
        // Removed between the check and the read.
        Err(problem) if problem.kind() == ErrorKind::NotFound => {
            return Err(ListingError::Missing(shown));
        }
        Err(problem) => {
            return Err(ListingError::CannotRead {
                shown,
                problem: problem.to_string(),
            });
        }
    };

    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        // A folder changing shape while we read it: whatever this entry turned out not to be, the answer
        // is still a listing of what was there.
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || !listed_dir(roots, &entry) {
            // hidden, or not a folder this listing may name: neither is somewhere a torrent can go
            continue;
        }
        let shown = clean(&name);
        if dirs.len() < MAX_DIRS {
            dirs.insert(shown);
            continue;
        }
        // Full. What makes the cut now is whatever sorts before the largest name kept, which then leaves:
        // the alphabetically first `MAX_DIRS`, decided by their order and not by the order the folder
        // happened to hand them out, and bounded in memory however many entries are inside.
        let Some(largest) = dirs.iter().next_back().cloned() else {
            continue;
        };
        if shown < largest {
            dirs.remove(&largest);
            dirs.insert(shown);
        }
    }
    Ok(dirs.into_iter().collect())
}

/// Whether `entry` belongs in a listing: a folder, and one this listing may name.
///
/// `file_type` does not follow links on any platform, so an ordinary directory is decided without looking
/// at the disk again - and an ordinary directory inside a folder that has already been canonicalised
/// cannot lead anywhere else. A link is the interesting case: it belongs in the list only if wherever it
/// points is a folder inside one of `roots` too, which is the same rule the listing's own path is held
/// to. Without it a destination picker could leave a root one directory at a time - list the alias, step
/// into it, list what is behind it - and every answer would look like part of the library. A link that
/// resolves to nothing (dangling, or pointing through somewhere this program may not read) is left out:
/// it is not somewhere a torrent can go, whatever its spelling suggests.
fn listed_dir(roots: &[Root], entry: &std::fs::DirEntry) -> bool {
    match entry.file_type() {
        Ok(kind) if kind.is_dir() => true,
        Ok(kind) if !kind.is_symlink() => false,
        // A link, or a platform that will not say: ask the path, which resolves it.
        _ => match entry.path().canonicalize() {
            Ok(target) => {
                target.is_dir() && roots.iter().any(|root| target.starts_with(&root.canonical))
            }
            Err(_) => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Root, browse_roots, inside_lexically, list_dirs};
    use crate::scratch::ScratchDir;
    use std::fs;
    use std::path::PathBuf;

    /// The roots a person's own machine is browsed with, and the ones a server is started with, differ in
    /// one thing: browsing does not make anything. A download folder that no torrent has made yet is
    /// still where torrents go, so it stays in the list, and its own spelling is all containment has to
    /// go on - which is also what keeps a real folder's canonical form, the one a resolved path is
    /// compared against.
    #[test]
    fn browsing_keeps_what_is_there_and_tolerates_what_is_not() {
        let dir = ScratchDir::new("fs_list_browse_roots");
        let there = dir.path().join("downloads");
        let absent = dir.path().join("not-yet");
        fs::create_dir_all(&there).expect("make the folder");

        let roots = browse_roots(&[there.clone(), absent.clone()]);
        assert_eq!(roots.len(), 2, "naming two folders gives two roots");
        assert_eq!(roots[0].given, there, "the spelling asked for is kept");
        assert_eq!(
            roots[0].canonical,
            there.canonicalize().expect("the folder is there"),
            "and so is the form a resolved path has to match",
        );
        assert_eq!(roots[1].given, absent);
        assert_eq!(
            roots[1].canonical, absent,
            "a folder that is not there keeps its own spelling: nothing below it resolves either"
        );
        assert!(
            !absent.exists(),
            "and listing what is browsed does not make the folders it names"
        );

        // A server's roots are made first, because fbt-api will not start without them.
        let prepared = Root::prepare(&[dir.path().join("served")]).expect("make the served folder");
        assert!(
            dir.path().join("served").is_dir(),
            "preparing a folder is what puts it on disk"
        );
        assert_eq!(prepared[0].given, dir.path().join("served"));
    }

    /// `..` is refused by its spelling, before anything asks where it would have led: the guard is part
    /// of [`list_dirs`] and not only something callers remember to run first.
    #[test]
    fn a_path_that_walks_out_is_refused_before_the_disk_is_read() {
        let dir = ScratchDir::new("fs_list_lexical");
        let given = dir.path().join("root");
        fs::create_dir_all(given.join("alpha")).expect("make the root and a folder in it");
        let roots = browse_roots(std::slice::from_ref(&given));
        let canonical = given.canonicalize().expect("the root is there");

        for inside in [
            given.clone(),
            given.join("alpha"),
            canonical.join("alpha"),
            given.join("deep").join("deeper"),
        ] {
            assert!(
                inside_lexically(&roots, &inside),
                "{} is below the root",
                inside.display(),
            );
        }

        // `<root>/../root/alpha` ends up inside the root and is still refused: a listing answers for the
        // folder it was given, not for wherever that spelling happens to resolve.
        let walked_out = given.join("..").join("root").join("alpha");
        assert!(!inside_lexically(&roots, &walked_out));
        let problem =
            list_dirs(&roots, &walked_out).expect_err("`..` is refused however tidy it resolves");
        assert!(
            problem.to_string().contains("outside the allowed folders"),
            "the answer says which rule it broke: {problem}"
        );

        // Somewhere else entirely is the same answer, and a relative path is too: a listing does not
        // answer for whatever directory this program happened to be started in.
        assert!(list_dirs(&roots, &dir.path().join("outside")).is_err());
        assert!(list_dirs(&roots, PathBuf::from("alpha").as_path()).is_err());
    }
}
