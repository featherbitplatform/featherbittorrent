//! Browsing the folders this server may name: the roots, and the directories inside one of them.
//!
//! This is the destination picker's data source - the list a client walks to choose where a torrent
//! goes - so it answers with what a picker needs and nothing else: folder names, sorted, no files, no
//! sizes, nothing hidden. What it may name at all is `server.allowed_roots`, which after
//! [`fbtt_core::Config::resolve`] is a list of absolute folders (the download folder when the config
//! named none), and fbt-api enforces the same list on the paths its own routes take.
//!
//! Two spellings of every root are kept: as `config.yaml` wrote it, and canonicalised. A request's path
//! is refused before the filesystem is touched at all unless it is absolute, has no `..` in it and sits
//! below a root in one of those two spellings - the same order fbt-api checks its own paths in, so a
//! client cannot make this server stat (or, on Windows, authenticate to) something outside the library.
//! Only then is the path canonicalised, which is what resolves a symlink and lets the containment check
//! refuse one that leads out of every root. The same rule then applies to what a listing *contains*: a link
//! inside a root is offered only if wherever it points is inside a root too, because otherwise a picker
//! could leave through an alias one directory at a time and this server would have handed it the way out.
//! The answer reports the spelling that was asked for, not the resolved one: it is the one the person typed,
//! and the one that works when they ask again.

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use super::{RouteError, Shared, clean};

/// The most directories one listing reports. Enough to pick a folder from in any real library's tree,
/// and the reason a folder with half a million entries in it costs this server two thousand names
/// rather than half a million.
const MAX_DIRS: usize = 2_000;

/// One allowed folder, in both spellings a request may use it in.
pub(crate) struct Root {
    /// As `config.yaml` named it, once [`fbtt_core::Config::resolve`] made it absolute: what
    /// `GET /api/v1/fs/roots` reports and what a listing echoes back.
    pub(crate) given: PathBuf,
    /// Its canonical form, with every symlink in it resolved: what containment is checked against, so a
    /// folder reached through a symlink alias of the root itself still answers to its own root.
    pub(crate) canonical: PathBuf,
}

impl Root {
    /// The allowed folders as directories this server can confine requests to: each one made if it is
    /// not there yet, then canonicalised.
    ///
    /// Making them is what lets the API name a folder before the first download has created it - fbt-api
    /// refuses to start with an allowed root that does not exist, and a destination picker whose root is
    /// missing has nothing to pick from. The `Err` is one line naming the folder and what stopped it, fit
    /// for a startup message: at this point nobody is talking to the server yet, so whoever started it is
    /// the one who has to act on it.
    pub(crate) fn prepare(configured: &[PathBuf]) -> Result<Vec<Root>, String> {
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

/// `GET /api/v1/fs/roots`: the folders this server may name, spelled as `config.yaml` spells them.
pub(super) async fn roots(State(shared): State<Arc<Shared>>) -> Json<Vec<String>> {
    Json(
        shared
            .roots
            .iter()
            .map(|root| root.given.to_string_lossy().into_owned())
            .collect(),
    )
}

/// `GET /api/v1/fs/list?path=<absolute>`: the directories directly inside `path`, as `{path, dirs}`.
///
/// Names come back cleaned for display and sorted, files and hidden entries (the dot convention, on every
/// platform) are left out, and at most [`MAX_DIRS`] names are reported - the alphabetically first ones,
/// so the same folder answers the same way twice rather than in whatever order the filesystem happened to
/// hand them out. A path outside every root is `403`, whether it got there by `..`, by being somewhere
/// else entirely, or by being a symlink that leads out; one that is not there is `404`; one that is not a
/// folder, or that no operating system could take (a NUL byte in the middle of it), is `400`.
pub(super) async fn list(
    State(shared): State<Arc<Shared>>,
    given: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Listing>, RouteError> {
    // A query that is not this shape - no `path` at all, or one that cannot be read as text - comes back
    // in this API's error shape rather than axum's plain-text one.
    let Query(query) = given?;
    // The path as it arrived, kept whole: it is what an error names and what the answer reports back.
    let asked = query.path;
    // A NUL byte is where a path stops, so one in the middle is not a path anything could resolve - a
    // percent-decoded `%00` is how it gets here. Said at once rather than discovered as an error from the
    // operating system, which reports it differently depending on which one this is running on.
    if asked.contains('\0') {
        return Err(RouteError::invalid_request(format!(
            "invalid path {}",
            clean(&asked),
        )));
    }
    // Lexical first, so a path this server would refuse anyway costs a comparison and not a stat.
    if !inside_lexically(&shared.roots, Path::new(&asked)) {
        return Err(RouteError::path_not_allowed(&asked));
    }
    let roots = Arc::clone(&shared.roots);
    let target = PathBuf::from(&asked);
    let dirs = tokio::task::spawn_blocking(move || list_dirs(&roots, &target))
        .await
        .map_err(|problem| {
            RouteError::internal(format!("the folder could not be listed ({problem})"))
        })??;
    Ok(Json(Listing { path: asked, dirs }))
}

/// Whether `path` is absolute, has no `..` anywhere in it and lies below one of `roots` in either of its
/// spellings. Purely lexical: this is the guard that runs before anything on disk is looked at.
fn inside_lexically(roots: &[Root], path: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|c| matches!(c, Component::ParentDir))
        && roots
            .iter()
            .any(|root| path.starts_with(&root.given) || path.starts_with(&root.canonical))
}

/// The directories inside `path`, on the blocking pool: canonicalise it, check containment again with any
/// symlinks resolved, and read the folder. A folder that is not there is `404`; one this server may not
/// name - including a link out of every root - is `403`; one that is not a folder is `400`.
fn list_dirs(roots: &[Root], path: &Path) -> Result<Vec<String>, RouteError> {
    let shown = clean(&path.to_string_lossy());
    let canonical = match path.canonicalize() {
        Ok(canonical) => canonical,
        Err(problem) if problem.kind() == ErrorKind::NotFound => {
            return Err(RouteError::not_found(format!("no folder {shown}")));
        }
        // A name the operating system will not take at all: too long, or on Windows one it has reserved for
        // something else. That is the request's problem to fix, not a failure of this server to answer - and
        // a NUL byte, the case that arrives most easily through a percent-decoded query, was refused earlier
        // because not every platform reports this one the same way.
        Err(problem) if problem.kind() == ErrorKind::InvalidInput => {
            return Err(RouteError::invalid_request(format!("invalid path {shown}")));
        }
        Err(problem) => {
            return Err(RouteError::internal(format!(
                "cannot check {shown} ({problem})",
            )));
        }
    };
    // The links are resolved now, so a folder reached through one that leads outside every root is
    // refused here although its spelling was inside: this is the check that cannot be fooled by an alias.
    if !roots
        .iter()
        .any(|root| canonical.starts_with(&root.canonical))
    {
        return Err(RouteError::path_not_allowed(&path.to_string_lossy()));
    }
    if !canonical.is_dir() {
        return Err(RouteError::invalid_request(format!(
            "{shown} is not a folder"
        )));
    }
    let entries = match std::fs::read_dir(&canonical) {
        Ok(entries) => entries,
        // Inside a root, and still refused: that is the file system's own answer to who may read it.
        Err(problem) if problem.kind() == ErrorKind::PermissionDenied => {
            return Err(RouteError::forbidden(format!("{shown} cannot be read")));
        }
        // Removed between the check and the read.
        Err(problem) if problem.kind() == ErrorKind::NotFound => {
            return Err(RouteError::not_found(format!("no folder {shown}")));
        }
        Err(problem) => {
            return Err(RouteError::internal(format!(
                "cannot read {shown} ({problem})",
            )));
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
            // hidden, or not a folder this server may name: neither is somewhere a torrent can go
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

/// Whether `entry` belongs in a listing: a folder, and one this server may name.
///
/// `file_type` does not follow links on any platform, so an ordinary directory is decided without looking at
/// the disk again - and an ordinary directory inside a folder that has already been canonicalised cannot
/// lead anywhere else. A link is the interesting case: it belongs in the list only if wherever it points is
/// a folder inside one of `roots` too, which is the same rule the listing's own path is held to. Without it
/// a destination picker could leave a root one directory at a time - list the alias, step into it, list
/// what is behind it - and every answer would look like part of the library. A link that resolves to
/// nothing (dangling, or pointing through somewhere this server may not read) is left out: it is not
/// somewhere a torrent can go, whatever its spelling suggests.
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

/// The query of `GET /api/v1/fs/list`: the folder to look inside. An absolute path is what the route can
/// answer for; anything else is refused as outside the roots, like anywhere else it is not welcome.
#[derive(Debug, Deserialize)]
pub(super) struct ListQuery {
    path: String,
}

/// What a listing answers: the folder that was asked for, spelled as it was asked for, and the
/// directories directly inside it.
#[derive(Debug, Serialize)]
pub(super) struct Listing {
    path: String,
    dirs: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{MAX_DIRS, Root, inside_lexically, list_dirs};
    use axum::http::StatusCode;
    use fbtt_core::scratch::ScratchDir;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// One allowed root, the folder beside it that is not one, and the scratch directory both are in: what
    /// these tests check containment against. The root is kept in both spellings because a request arrives
    /// in whichever one the person typed, and on macOS those two differ for a temp directory (`/var` is a
    /// link to `/private/var`).
    struct Tree {
        /// Removed when this goes down: everything here is inside it. Never read - its whole job is being
        /// dropped at the end of the test.
        _dir: ScratchDir,
        /// The root as `config.yaml` would name it.
        given: PathBuf,
        /// ...and as the filesystem spells it, which is what containment is checked against.
        canonical: PathBuf,
        /// Below the root's parent: inside the tree somebody drew, outside every root.
        outside: PathBuf,
    }

    impl Tree {
        fn new(tag: &str) -> Tree {
            let dir = ScratchDir::new(tag);
            let given = dir.path().join("root");
            let outside = dir.path().join("outside");
            fs::create_dir_all(&given).expect("make the root");
            fs::create_dir_all(&outside).expect("make the folder beside it");
            let canonical = given.canonicalize().expect("canonicalise the root");
            Tree {
                _dir: dir,
                given,
                canonical,
                outside,
            }
        }

        /// The one root, spelled both ways, as the routes hold it.
        fn roots(&self) -> [Root; 1] {
            [Root {
                given: self.given.clone(),
                canonical: self.canonical.clone(),
            }]
        }

        /// The directories `list_dirs` reports for `path`.
        fn list(&self, path: &Path) -> Vec<String> {
            let roots = self.roots();
            list_dirs(&roots, path).expect("a listing")
        }
    }

    /// The guard that runs before the disk is touched: absolute, no `..`, and below a root in one of its two
    /// spellings. A relative path and a `..` are refused however tidy they look - a request may not name a
    /// folder by where this server happened to be started - and a name that merely starts where a root does
    /// is somebody else's folder.
    #[test]
    fn containment_is_lexical_before_it_is_real() {
        let tree = Tree::new("fs_lexical");
        let roots = tree.roots();

        for inside in [
            tree.given.clone(),
            tree.given.join("alpha"),
            tree.given.join("deep").join("deeper"),
            tree.canonical.clone(),
            tree.canonical.join("alpha"),
        ] {
            assert!(
                inside_lexically(&roots, &inside),
                "{} is below the root",
                inside.display(),
            );
        }

        // `<root>/../outside` is the case that looks like it belongs and is not: the `..` is refused
        // lexically, without asking the filesystem where it would have led.
        for outside in [
            PathBuf::from("/"),
            tree.outside.clone(),
            tree.given.join("..").join("outside"),
            PathBuf::from("alpha"),
        ] {
            assert!(
                !inside_lexically(&roots, &outside),
                "{} is not below the root",
                outside.display(),
            );
        }

        // `/library-two` does not live under `/library`, however much the text suggests it: containment is
        // about components, not prefixes.
        let neighbour = tree.given.with_file_name("root-2");
        fs::create_dir_all(&neighbour).expect("make the neighbour");
        assert!(
            !inside_lexically(&roots, &neighbour),
            "{} is a sibling",
            neighbour.display(),
        );

        // `<root>/../outside` is what M5 asks about end to end: the route refuses it as outside the roots.
        // The lexical rule stops it at the `..`, and asking anyway gets the same answer from the resolved
        // path - which is the only rule that would have caught a link doing the same thing.
        let problem = list_dirs(&roots, &tree.given.join("..").join("outside"))
            .expect_err("a path that leaves the root through `..` is not inside it");
        assert_eq!(problem.status, StatusCode::FORBIDDEN);
        assert_eq!(problem.code, "path_not_allowed");
    }

    /// A link inside a root belongs in a listing only if wherever it points is inside a root too. Without
    /// that rule a destination picker could leave through an alias one directory at a time - list the alias,
    /// step into it, list what is behind it - with every answer still spelling itself as part of the
    /// library. A link to nothing at all is left out as well: it is not somewhere a torrent can go.
    #[cfg(unix)]
    #[test]
    fn a_link_out_is_not_a_folder_in_the_listing() {
        let tree = Tree::new("fs_links");
        fs::create_dir_all(tree.given.join("alpha")).expect("make a folder");
        fs::write(tree.given.join("a-file"), b"not a folder").expect("write a file");
        std::os::unix::fs::symlink("../outside", tree.given.join("link-out"))
            .expect("make a link out of the root");
        std::os::unix::fs::symlink("alpha", tree.given.join("link-in"))
            .expect("make a link inside the root");
        std::os::unix::fs::symlink("../not-here", tree.given.join("link-dangling"))
            .expect("make a dangling link");

        assert_eq!(
            tree.list(&tree.given),
            ["alpha", "link-in"],
            "a folder and a link to one are somewhere a torrent can go; a link out, a dangling link and a \
             file are not",
        );

        // Asking for the link out by name is refused once it resolves: the same rule, from the other side.
        let roots = tree.roots();
        let problem = list_dirs(&roots, &tree.given.join("link-out"))
            .expect_err("a link that leads out of every root is not inside one");
        assert_eq!(problem.status, StatusCode::FORBIDDEN);
        assert_eq!(problem.code, "path_not_allowed");

        // A dangling link resolves to nothing, so it is nowhere a picker can go: `404`, like any folder that
        // is not there.
        let problem = list_dirs(&roots, &tree.given.join("link-dangling"))
            .expect_err("a dangling link is not a folder");
        assert_eq!(problem.status, StatusCode::NOT_FOUND);
    }

    /// The listing's own bound: the alphabetically first [`MAX_DIRS`] names, whatever order the folder handed
    /// them out in, and nothing past them - so a folder with half a million entries costs this server two
    /// thousand names rather than half a million.
    #[test]
    fn listing_keeps_the_alphabetically_first_names() {
        let tree = Tree::new("fs_cap");
        // `MAX_DIRS` plus ten, made newest-name-first so that the order they arrived in and the order they
        // sort into are not the same thing, and zero-padded so that sorting them as text is sorting them by
        // number.
        for number in (0..MAX_DIRS + 10).rev() {
            fs::create_dir_all(tree.given.join(format!("dir-{number:04}")))
                .expect("make a test folder");
        }

        let dirs = tree.list(&tree.given);
        assert_eq!(
            dirs.len(),
            MAX_DIRS,
            "the cap is the answer, not an estimate"
        );
        assert_eq!(dirs.first().expect("a name"), "dir-0000");
        assert_eq!(
            dirs.last().expect("a name").as_str(),
            format!("dir-{:04}", MAX_DIRS - 1).as_str(),
            "what is kept is the alphabetically first `MAX_DIRS`, not whatever came off the disk first"
        );
        assert!(
            !dirs.contains(&format!("dir-{MAX_DIRS:04}")),
            "the name at the cut-off is out, along with everything after it",
        );
    }

    /// A path no operating system could take is the request's mistake, answered as one: `400`, and a message
    /// that says which path. It used to arrive as a `500`, which blamed this server for a name somebody else
    /// invented - and `%00` in a query string is how a client sends one by accident.
    #[test]
    fn an_unusable_path_is_a_bad_request() {
        let tree = Tree::new("fs_invalid");
        let roots = tree.roots();
        let bad = tree.given.join(format!("a{}b", char::from(0u8)));

        let problem = list_dirs(&roots, &bad).expect_err("no filesystem takes a NUL in a name");
        assert_eq!(problem.status, StatusCode::BAD_REQUEST);
        assert_eq!(problem.code, "invalid_request");
        assert!(
            problem.message.contains("invalid path"),
            "the message says what is wrong with it: {}",
            problem.message,
        );

        // A folder that is not there is still `404`, and something that is not a folder is still `400` - but
        // with a different message from the one above, because they answer different questions.
        let problem = list_dirs(&roots, &tree.given.join("not-here")).expect_err("nothing there");
        assert_eq!(problem.status, StatusCode::NOT_FOUND);
        fs::write(tree.given.join("a-file"), b"not a folder").expect("write a file");
        let problem =
            list_dirs(&roots, &tree.given.join("a-file")).expect_err("a file is not a folder");
        assert_eq!(problem.status, StatusCode::BAD_REQUEST);
        assert!(
            !problem.message.contains("invalid path"),
            "a file is a usable name in the wrong shape: {}",
            problem.message,
        );
    }
}
