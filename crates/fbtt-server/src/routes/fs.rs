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
//! refuse one that leads out of every root. The answer reports the spelling that was asked for, not the
//! resolved one: it is the one the person typed, and the one that works when they ask again.

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
/// folder is `400`.
pub(super) async fn list(
    State(shared): State<Arc<Shared>>,
    given: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Listing>, RouteError> {
    // A query that is not this shape - no `path` at all, or one that cannot be read as text - comes back
    // in this API's error shape rather than axum's plain-text one.
    let Query(query) = given?;
    // The path as it arrived, kept whole: it is what an error names and what the answer reports back.
    let asked = query.path;
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
        if name.starts_with('.') || !is_directory(&entry) {
            continue; // hidden, or not a folder: neither is somewhere a torrent can go
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

/// Whether `entry` names a directory, following a symlink. A link to a folder inside a root is a folder
/// one can open, so it belongs in the list; a link out of every root is refused by the containment check
/// when someone asks for what it points at. `file_type` does not follow links on any platform, so only
/// the entries that could be links cost an extra look.
fn is_directory(entry: &std::fs::DirEntry) -> bool {
    match entry.file_type() {
        Ok(kind) if kind.is_dir() => true,
        Ok(kind) if !kind.is_symlink() => false,
        // A link, or a platform that will not say: ask the path, which resolves it.
        _ => entry.path().is_dir(),
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
