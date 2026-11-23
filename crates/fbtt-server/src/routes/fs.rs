//! Browsing the folders this server may name: the roots, and the directories inside one of them.
//!
//! This is the destination picker's data source - the list a client walks to choose where a torrent goes -
//! so it answers with what a picker needs and nothing else: folder names, sorted, no files, no sizes,
//! nothing hidden. What it may name at all is `server.allowed_roots`, which after
//! [`fbtt_core::Config::resolve`] is a list of absolute folders (the download folder when the config named
//! none), and fbt-api enforces the same list on the paths its own routes take.
//!
//! What a listing then does - how many names it reports, which ones it skips, what it makes of a symlink,
//! what counts as outside the roots - is [`fbtt_core::fs_list`]'s, because the local backend answers the
//! same question about a person's own machine and one set of rules is the point: a folder that listed one
//! way over HTTP and another way in the picker would be two rules for somebody to keep apart. Decided here
//! is this API's shape only: the roots a request is confined to, spelled both ways ([`Root`], because a
//! request arrives in whichever spelling the person typed and on macOS `/var` is a link to `/private/var`,
//! so the two are not prefixes of one another), the guard that runs before the file system is touched at
//! all so a client cannot make this server stat - or, on Windows, authenticate to - something outside the
//! library, and the status and code each answer becomes. The listing reports at most
//! [`fbtt_core::fs_list::MAX_DIRS`] names.

// `pub(crate)` rather than private because [`Server`](crate::server::Server) prepares the allowed folders
// into these roots before fbt-api is handed any of them, and it has to name the type to do that.
pub(crate) use fbtt_core::fs_list::Root;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use fbtt_core::fs_list::{self, ListingError, inside_lexically};
use serde::{Deserialize, Serialize};

use super::{RouteError, Shared, clean};

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
/// The answer is [`fbtt_core::fs_list`]'s - names cleaned for display and sorted, files and hidden entries
/// left out, a link listed only where it stays inside a root. A path outside every root is `403`, whether
/// it got there by `..`, by being somewhere else entirely, or by being a symlink that leads out; one that
/// is not there is `404`; one that is not a folder, or that no operating system could take (a NUL byte in
/// the middle of it), is `400`. Refusing a path before anything on disk is looked at is this route's own
/// job: see [`inside_lexically`].
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
    // Lexical first, so a path this server would refuse anyway costs a comparison and not a stat - and no
    // trip to the blocking pool to say it.
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

/// The longest folder name [`mkdir`] takes, in bytes: what the common file systems (ext4, APFS, NTFS by
/// UTF-16 unit, which is never more than its UTF-8 bytes) allow for one component.
pub(super) const MAX_NAME_BYTES: usize = 255;

/// The most a `POST /api/v1/fs/mkdir` body may be: a parent path and one name, with room to spare. With
/// logins off this route answers whoever reaches the address, so a megabyte of JSON naming one folder is
/// not something worth buffering.
pub(super) const MAX_MKDIR_BODY: usize = 16 * 1024;

/// `POST /api/v1/fs/mkdir` with `{parent, name}`: create the one folder `name` directly inside `parent`,
/// answering `201 {path}` with the new folder spelled the way [`list`] spells paths (the parent as it was
/// given, joined with the name).
///
/// This writes to the server's disk over the network, so it is held to everything [`list`] is held to and
/// then some. The name is one path component and nothing else ([`name_problem`]): `400`. The parent has to
/// be inside a root by its spelling, checked before the disk is touched, and again once every link in it is
/// resolved ([`fs_list::resolve_folder`], the same rule the listing uses), so a link inside a root that leads
/// out of it is `403` like any other path outside. The folder is then made inside the *resolved* parent, with
/// `create_dir` and never `create_dir_all`, so nothing but the one folder is ever created, and an entry that
/// is already there under that name - a folder, a file, or a link, which `mkdir` does not follow - is `409`.
pub(super) async fn mkdir(
    State(shared): State<Arc<Shared>>,
    given: Result<Json<MkdirBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Created>), RouteError> {
    // A body that is not this shape comes back in this API's error shape rather than axum's plain-text one.
    let Json(body) = given?;
    if let Some(problem) = name_problem(&body.name) {
        return Err(RouteError::invalid_request(problem));
    }
    let asked = body.parent;
    // A NUL byte is where a path stops; one in the middle is no path at all, said at once rather than
    // however this operating system's error for it reads.
    if asked.contains('\0') {
        return Err(RouteError::invalid_request(format!(
            "invalid path {}",
            clean(&asked),
        )));
    }
    // Lexical first, so a parent this server would refuse anyway costs a comparison and not a stat.
    if !inside_lexically(&shared.roots, Path::new(&asked)) {
        return Err(RouteError::path_not_allowed(&asked));
    }
    let roots = Arc::clone(&shared.roots);
    let parent = PathBuf::from(&asked);
    let name = body.name.clone();
    tokio::task::spawn_blocking(move || create_inside(&roots, &parent, &name))
        .await
        .map_err(|problem| {
            RouteError::internal(format!("the folder could not be created ({problem})"))
        })??;
    let path = Path::new(&asked).join(&body.name);
    Ok((
        StatusCode::CREATED,
        Json(Created {
            path: path.to_string_lossy().into_owned(),
        }),
    ))
}

/// Creates `name` directly inside `parent`, once `parent` has been resolved and found inside a root. Runs on
/// the blocking pool: it asks the file system twice. The errors it logs carry the kind of failure only,
/// never a path, because a path here is the client's text.
fn create_inside(roots: &[Root], parent: &Path, name: &str) -> Result<(), RouteError> {
    let resolved = fs_list::resolve_folder(roots, parent).map_err(|problem| match problem {
        // A `500` logs its message, and the listing's says which path it could not check: this route logs
        // no path, so the server's own failure is said without one.
        ListingError::CannotCheck { .. } | ListingError::CannotRead { .. } => {
            RouteError::internal("the parent folder could not be checked")
        }
        problem => route_problem(problem),
    })?;
    let shown = clean(name);
    let created = resolved.join(name);
    match std::fs::create_dir(&created) {
        Ok(()) => confirm_inside(roots, &created, &shown),
        Err(problem) => {
            let kind = problem.kind();
            tracing::debug!(?kind, "a folder could not be created");
            Err(match kind {
                std::io::ErrorKind::AlreadyExists => {
                    RouteError::already_exists(format!("{shown} is already there"))
                }
                // The parent went away between the check and the create.
                std::io::ErrorKind::NotFound => {
                    RouteError::not_found(format!("no folder {}", clean(&parent.to_string_lossy())))
                }
                std::io::ErrorKind::PermissionDenied => {
                    RouteError::forbidden(format!("{shown} cannot be created here"))
                }
                std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidFilename => {
                    RouteError::invalid_request(format!("invalid folder name {shown}"))
                }
                // Logged as it is answered, so it names no folder: the kind above is what says why.
                _ => RouteError::internal("the folder could not be created"),
            })
        }
    }
}

/// Checks the folder [`create_inside`] just made is inside a root once resolved, and takes it back when it is
/// not: removed again, and `403 path_not_allowed`.
///
/// The parent was resolved and checked before the create, so this only matters when somebody changed the
/// path in between: replaced a folder on the way to it with a link that leads out. That needs write access
/// inside a root on this machine as well as a login, and this check narrows what it buys to an empty folder
/// that exists outside for a moment. It does not close the window: the swap could happen between the create
/// and this check too, or be swapped back. Closing it means creating relative to an open handle on the
/// parent (`mkdirat` with `O_NOFOLLOW`), which needs `rustix` in the workspace table and is Unix-only, so it
/// is not done here. `remove_dir` only removes an empty folder, and the folder was made a moment ago, so it
/// cannot take anything else with it.
fn confirm_inside(roots: &[Root], created: &Path, shown: &str) -> Result<(), RouteError> {
    let refused = || RouteError::path_not_allowed(shown);
    let resolved = match created.canonicalize() {
        Ok(resolved) => resolved,
        // Gone already, or unreadable: not somewhere this route can vouch for. Nothing to take back that it
        // can find.
        Err(problem) => {
            tracing::warn!(kind = ?problem.kind(), "a new folder could not be resolved after it was made");
            return Err(refused());
        }
    };
    if roots
        .iter()
        .any(|root| resolved.starts_with(&root.canonical))
    {
        return Ok(());
    }
    tracing::warn!("a new folder resolved outside the allowed folders and was removed again");
    // The path this request created, not the resolved one: `rmdir` does not follow a link in the last
    // component, so a name swapped for a link in the meantime makes this fail rather than remove a folder
    // the link points at.
    if let Err(problem) = std::fs::remove_dir(created) {
        tracing::warn!(kind = ?problem.kind(), "a new folder outside the allowed folders could not be removed");
    }
    Err(refused())
}

/// The names Windows keeps for devices, refused on every platform so a folder made here can be copied to
/// any machine: `CON.txt` is the device as much as `CON` is.
/// Windows also takes the superscript digits one to three and zero as port numbers, and `CONIN$` and
/// `CONOUT$` as the console.
const RESERVED_NAMES: [&str; 32] = [
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "CONIN$",
    "CONOUT$",
    "COM0",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT0",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
];

/// What is wrong with `name` as the name of one new folder, or `None` when it is one: a single, ordinary
/// path component. Not empty, `.` or `..`; no `/` or `\` (either platform's separator, whichever this is
/// running on, so a name means the same thing on every server); no NUL or other control character; at most
/// [`MAX_NAME_BYTES`] bytes; exactly what `clean` would show; not starting with a dot; not ending in a dot
/// or a space, and not one of Windows' device names ([`RESERVED_NAMES`]) on any platform. And it has to
/// parse as exactly one normal component on this platform, which is what refuses whatever else a platform reads as more than a name - a drive prefix such as `C:` on Windows,
/// which `join` would otherwise take as a whole new path.
pub(super) fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("a folder name cannot be empty".to_string());
    }
    if name == "." || name == ".." {
        return Some(format!("{name} is not a folder name"));
    }
    // `fs/list` leaves dot entries out, so a folder named this way could never be seen or picked again.
    if name.starts_with('.') {
        return Some(format!(
            "a folder name cannot start with a dot: {}",
            clean(name),
        ));
    }
    if name.contains(['/', '\\']) {
        return Some(format!(
            "a folder name cannot contain a separator: {}",
            clean(name)
        ));
    }
    if name.chars().any(char::is_control) {
        return Some(format!(
            "a folder name cannot contain a control character: {}",
            clean(name),
        ));
    }
    if name.len() > MAX_NAME_BYTES {
        return Some(format!(
            "a folder name is at most {MAX_NAME_BYTES} bytes, this one is {}",
            name.len(),
        ));
    }
    // A name has to read exactly as it is shown, because `fs/list` and every picker show it cleaned: an
    // invisible format character (U+200B, U+202E), a name of spaces, a leading or trailing space or a run of
    // them would let `Show` and a second folder that only looks like `Show` sit side by side.
    if clean(name) != name {
        return Some(format!(
            "a folder name has to read the same as it is shown: {}",
            clean(name),
        ));
    }
    // Windows drops a trailing dot or space from a name, so the folder made would not be the one asked for;
    // refused everywhere so a library moves between machines intact.
    if name.ends_with(['.', ' ']) {
        return Some(format!(
            "a folder name cannot end in a dot or a space: {}",
            clean(name),
        ));
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    if RESERVED_NAMES
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return Some(format!("{} is a name Windows reserves", clean(name)));
    }
    let mut parts = Path::new(name).components();
    if !matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(part)), None) if part == name
    ) {
        return Some(format!("{} is not one folder name", clean(name)));
    }
    None
}

/// The body of `POST /api/v1/fs/mkdir`: the folder to create in, and the one new name to create there.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MkdirBody {
    parent: String,
    name: String,
}

/// What `POST /api/v1/fs/mkdir` answers: the folder it created.
#[derive(Debug, Serialize)]
pub(super) struct Created {
    path: String,
}

/// The directories inside `path`, in this API's answer for them. [`fbtt_core::fs_list::list_dirs`] does the
/// work and says what went wrong in the words every featherbittorrent listing uses - only the status and
/// code a client of this API reads are picked here. Runs on the blocking pool because it reads a directory.
fn list_dirs(roots: &[Root], path: &Path) -> Result<Vec<String>, RouteError> {
    fs_list::list_dirs(roots, path).map_err(route_problem)
}

/// One listing problem in this API's error shape: the same answer fbt-api gives for the same kind of
/// question, with the listing's own line as its message. The variant says which question was answered;
/// the line it prints is what a client reads, and both come from the listing so they cannot drift apart.
fn route_problem(problem: ListingError) -> RouteError {
    match &problem {
        ListingError::Missing(_) => RouteError::not_found(problem.to_string()),
        // Its own code, because a client that walks this list to pick a folder has to be able to tell
        // "somewhere else" from "not allowed here": the first is a mistake in what it asked for, the second
        // is a limit of this server.
        ListingError::Outside(_) => RouteError::new(
            StatusCode::FORBIDDEN,
            "path_not_allowed",
            problem.to_string(),
        ),
        ListingError::Unusable(_) | ListingError::NotAFolder(_) => {
            RouteError::invalid_request(problem.to_string())
        }
        ListingError::ReadDenied(_) => RouteError::forbidden(problem.to_string()),
        ListingError::CannotCheck { .. } | ListingError::CannotRead { .. } => {
            RouteError::internal(problem.to_string())
        }
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
    use super::{MAX_NAME_BYTES, Root, confirm_inside, inside_lexically, list_dirs, name_problem};
    use axum::http::StatusCode;
    use fbtt_core::fs_list::MAX_DIRS;
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

    /// A folder name is one ordinary path component: anything that would name another place, or no place,
    /// or that no person typed on purpose, is refused before the parent is even looked at.
    #[test]
    fn a_folder_name_is_one_component() {
        let longest = "a".repeat(MAX_NAME_BYTES);
        // Multi-byte text is measured in bytes: 85 three-byte characters is 255 bytes, one more is too many.
        let wide = "\u{3042}".repeat(85);
        for good in [
            "New Show",
            "Season 1",
            "a.b",
            "x.hidden",
            "COM10",
            "LPT\u{b4}",
            "CONIN",
            "Show (2024) - Part 2",
            "CONSOLE",
            "e\u{301}t\u{e9}",
            "caf\u{e9}",
            longest.as_str(),
            wide.as_str(),
        ] {
            assert_eq!(name_problem(good), None, "{good:?} is one folder name");
        }

        let too_long = "a".repeat(MAX_NAME_BYTES + 1);
        let too_wide = "\u{3042}".repeat(86);
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "/etc",
            "\\etc",
            "../x",
            "x/..",
            "x\u{0}y",
            "x\ny",
            "x\ty",
            "x\u{7f}y",
            "x\u{85}y",
            too_long.as_str(),
            too_wide.as_str(),
            // Shown differently from what it is: invisible format characters, spaces that `clean` trims or
            // folds, whitespace that is not a plain space.
            "Show\u{200B}",
            "\u{202E}wohS",
            "Sh\u{FEFF}ow",
            " ",
            "   ",
            " Show",
            "Show ",
            "New  Show",
            "New\u{a0}Show",
            "\u{301}Show",
            // What Windows would change or take for a device.
            "...",
            "Show.",
            "CON",
            "con",
            "Con.txt",
            "NUL.tar.gz",
            "prn",
            "AUX",
            "COM1",
            "com9.log",
            "LPT1",
            "Lpt9",
            "CON .txt",
            "COM0",
            "lpt0.txt",
            "COM\u{b9}",
            "com\u{b2}.log",
            "COM\u{b3}",
            "LPT\u{b9}",
            "lpt\u{b2}",
            "LPT\u{b3}.txt",
            "CONIN$",
            "conout$",
            "ConOut$.txt",
            // `fs/list` hides dot entries, so a folder named this way could never be picked.
            ".hidden",
            ".config",
        ] {
            assert!(name_problem(bad).is_some(), "{bad:?} is refused");
        }

        // A drive prefix is a whole new path to `join` on Windows; elsewhere it is an ordinary name.
        #[cfg(windows)]
        assert!(name_problem("C:").is_some(), "a drive prefix is refused");
    }

    /// A folder that turns out, once made, to resolve outside every root - the parent was swapped for a link
    /// out between the check and the create - is removed again and answered as outside the roots. The race
    /// itself cannot be timed in a test, so this hands [`confirm_inside`] the outcome directly: a folder made
    /// outside, named through a link inside the root.
    #[cfg(unix)]
    #[test]
    fn a_folder_made_outside_is_taken_back() {
        let tree = Tree::new("fs_taken_back");
        let roots = tree.roots();
        std::os::unix::fs::symlink(&tree.outside, tree.given.join("swapped"))
            .expect("make a link out of the root");
        let made = tree.given.join("swapped").join("escaped");
        fs::create_dir(&made).expect("make the folder the race would have made");
        assert!(tree.outside.join("escaped").is_dir());

        let problem =
            confirm_inside(&roots, &made, "escaped").expect_err("a folder outside is not kept");
        assert_eq!(problem.status, StatusCode::FORBIDDEN);
        assert_eq!(problem.code, "path_not_allowed");
        assert!(
            !tree.outside.join("escaped").exists(),
            "the folder made outside was removed again",
        );

        // One that is where it should be stays.
        let kept = tree.given.join("kept");
        fs::create_dir(&kept).expect("make a folder inside");
        assert!(confirm_inside(&roots, &kept, "kept").is_ok());
        assert!(kept.is_dir(), "a folder inside the root is left alone");
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
        // The lexical rule stops it at the `..`, and asking anyway gets the same answer from the listing -
        // which holds the path to the same rule, so a caller cannot reach the disk by skipping it.
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
