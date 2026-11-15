//! The destination picker: the folders this backend may name, walked one directory at a time.
//!
//! It is a dialog inside a dialog - the add box opens it over itself, and hands back the folder that was
//! chosen - because the answer comes from wherever the torrents are going to live: `Command::ListDirs`
//! on this machine's folders when the engine is local, and on a server's allowed folders when it is not.
//! The picker does not care which: it asks for a folder, draws what comes back, and never waits for it.
//!
//! What it draws comes from the newest snapshot rather than from its own copy, because the backend
//! replaces that listing whenever it answers again, and a picker holding its own would be showing last
//! answer's folders.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fbtt_core::{Command, Snapshot};

use crate::format;
use crate::modal::{Action, keys_hint};
use crate::theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The marker in front of the folder the cursor is on, the same one the torrent table uses.
const MARKER: &str = "▌";
/// How many folders `PageUp` and `PageDown` move the cursor, agreed rather than measured against the
/// screen: a picker inside a dialog box has no way to know how many rows it was given room for.
const PAGE_STEP: i32 = 10;

/// Where the person is in the folder tree, and which row of it they are on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderPicker {
    /// The folder being listed, or `None` for the list of places to start from.
    path: Option<PathBuf>,
    /// Which row of that listing the cursor is on.
    cursor: usize,
}

/// What one press did to the picker. It answers to the dialog that opened it rather than to the screen,
/// because choosing a folder fills a field in that dialog instead of closing anything.
#[derive(Debug, PartialEq)]
pub(crate) enum PickerAnswer {
    /// The picker stays up, with these going out (opening a folder asks the backend for its list).
    Stay(Vec<Action>),
    /// `s`: this folder goes into the field that asked for it, and the picker comes down.
    Chosen(PathBuf),
    /// Esc: back to the fields the picker was opened from.
    Back,
}

impl FolderPicker {
    /// Opens the picker at the list of places to start from, with the command that asks for it. The
    /// listing is not held here: it arrives in a snapshot, and this says which folder was asked about.
    pub(crate) fn open() -> (FolderPicker, Vec<Action>) {
        (
            FolderPicker::default(),
            vec![Action::Command(Command::ListDirs(None))],
        )
    }

    /// The rows on screen: the places to start from, or the folders inside the one being listed. `None`
    /// means the answer for this folder has not arrived yet.
    fn contents<'a>(&self, snap: &'a Snapshot) -> Option<Contents<'a>> {
        let listing = snap.dirs.as_ref()?;
        match self.path.as_ref() {
            // Every listing echoes the folders it may name, so going up one does not need to ask again.
            None => Some(Contents::Roots(&listing.roots)),
            Some(path) if listing.path.as_deref() == Some(path.as_path()) => {
                Some(Contents::Dirs(&listing.dirs))
            }
            _ => None,
        }
    }

    /// How many rows there are to move along.
    fn row_count(&self, snap: &Snapshot) -> usize {
        match self.contents(snap) {
            Some(Contents::Roots(roots)) => roots.len(),
            Some(Contents::Dirs(dirs)) => dirs.len(),
            None => 0,
        }
    }

    /// The row the cursor is on, kept inside the list: a folder that changed between two snapshots has
    /// no reason to leave a cursor pointing past its end.
    fn row(&self, snap: &Snapshot) -> usize {
        self.cursor.min(self.row_count(snap).saturating_sub(1))
    }

    /// Moves the cursor `delta` rows, stopping at either end like the torrent table does.
    fn step(&mut self, delta: i32, snap: &Snapshot) {
        let last = self.row_count(snap).saturating_sub(1);
        let from = i64::try_from(self.row(snap)).unwrap_or(i64::MAX);
        let to = (from + i64::from(delta)).clamp(0, i64::try_from(last).unwrap_or(i64::MAX));
        self.cursor = usize::try_from(to).unwrap_or(0);
    }

    /// Goes to a folder and asks for what is inside it.
    fn enter(&mut self, path: PathBuf) -> PickerAnswer {
        self.path = Some(path.clone());
        self.cursor = 0;
        PickerAnswer::Stay(vec![Action::Command(Command::ListDirs(Some(path)))])
    }
}

/// What the picker is listing.
enum Contents<'a> {
    /// The folders this backend may name at all, in the order it named them.
    Roots(&'a [PathBuf]),
    /// The directories inside the folder being listed, as the backend sorted them.
    Dirs(&'a [String]),
}

/// Applies one press to the picker. `Enter` opens a folder, `Backspace` goes up, `s` chooses, and `j` and
/// `k` move along the list; everything else is ignored rather than closing anything, because a person
/// pressing another key meant to stay in the picker.
pub(crate) fn on_key(picker: &mut FolderPicker, key: KeyEvent, snap: &Snapshot) -> PickerAnswer {
    match key.code {
        KeyCode::Esc => PickerAnswer::Back,
        // The same key that opened the picker closes it again, which is what a person who opened it by
        // accident looks for.
        KeyCode::Char('o') if key.modifiers == KeyModifiers::CONTROL => PickerAnswer::Back,
        KeyCode::Char('j') | KeyCode::Down => {
            picker.step(1, snap);
            PickerAnswer::Stay(Vec::new())
        }
        KeyCode::Char('k') | KeyCode::Up => {
            picker.step(-1, snap);
            PickerAnswer::Stay(Vec::new())
        }
        KeyCode::PageDown => {
            picker.step(PAGE_STEP, snap);
            PickerAnswer::Stay(Vec::new())
        }
        KeyCode::PageUp => {
            picker.step(-PAGE_STEP, snap);
            PickerAnswer::Stay(Vec::new())
        }
        KeyCode::Enter => match picker.contents(snap) {
            Some(Contents::Roots(roots)) => match roots.get(picker.row(snap)) {
                Some(root) => picker.enter(root.clone()),
                None => PickerAnswer::Stay(Vec::new()),
            },
            Some(Contents::Dirs(dirs)) => {
                match (dirs.get(picker.row(snap)), picker.path.as_ref()) {
                    (Some(dir), Some(path)) => picker.enter(path.join(dir)),
                    _ => PickerAnswer::Stay(Vec::new()),
                }
            }
            None => PickerAnswer::Stay(Vec::new()),
        },
        KeyCode::Char('s') => match picker.contents(snap) {
            // Inside a folder, `s` chooses the folder being listed: that is the one named on the line
            // above the list, and the one a person standing in it means by "this folder".
            Some(Contents::Dirs(_)) => match picker.path.clone() {
                Some(path) => PickerAnswer::Chosen(path),
                None => PickerAnswer::Stay(Vec::new()),
            },
            // At the places to start from there is no folder being listed, so the highlighted one is what
            // choosing has to mean.
            Some(Contents::Roots(roots)) => match roots.get(picker.row(snap)) {
                Some(root) => PickerAnswer::Chosen(root.clone()),
                None => PickerAnswer::Stay(Vec::new()),
            },
            None => PickerAnswer::Stay(Vec::new()),
        },
        KeyCode::Backspace => {
            let Some(path) = picker.path.clone() else {
                // The list of places to start from has nothing above it, so there is nothing to do.
                return PickerAnswer::Stay(Vec::new());
            };
            match parent_inside(roots_of(snap), &path) {
                Some(parent) => {
                    picker.path = Some(parent.clone());
                    picker.cursor = 0;
                    PickerAnswer::Stay(vec![Action::Command(Command::ListDirs(Some(parent)))])
                }
                // The step out of this folder would leave every folder this backend may name, which is the
                // picker's top: it shows the places to start from rather than the reason that step was
                // refused, because the answer to "where do I go back to" is that list.
                None => {
                    picker.path = None;
                    picker.cursor = 0;
                    PickerAnswer::Stay(Vec::new())
                }
            }
        }
        _ => PickerAnswer::Stay(Vec::new()),
    }
}

/// The folders every listing reports, which is what says whether a step up is somewhere this backend may
/// name at all. Nothing has been asked yet means nothing is known about containment.
fn roots_of(snap: &Snapshot) -> &[PathBuf] {
    match snap.dirs.as_ref() {
        Some(listing) => &listing.roots,
        None => &[],
    }
}

/// The folder above `path` that is still inside one of `roots`, or `None` when there is none: nothing
/// above it may be named, so going up means the list of roots rather than a listing that would fail.
///
/// Lexical on purpose. Every path the picker reaches was made by joining folder names onto a root it was
/// given, so a parent is inside a root exactly when it starts with one - and a server's answer about a
/// link that leads out belongs beside the folder it came from, not guessed at here.
fn parent_inside(roots: &[PathBuf], path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    roots
        .iter()
        .any(|root| parent.starts_with(root))
        .then(|| parent.to_path_buf())
}

/// The picker's lines: where in the tree this is, what is inside it, why it could not be listed when it
/// could not be, and what each key does here.
pub(crate) fn lines(picker: &FolderPicker, snap: &Snapshot) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme::INK_400);
    let at = picker.row(snap);
    let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
        format::safe(&location(picker)),
        if picker.path.is_some() {
            Style::new().fg(theme::INK_300)
        } else {
            muted
        },
    ))];

    match picker.contents(snap) {
        // The answer for this folder has not arrived: the line that says where we are stays, and the list
        // is replaced by what is happening to it.
        None => lines.push(Line::from(Span::styled("reading folders...", muted))),
        Some(Contents::Roots(roots)) => {
            for (index, root) in roots.iter().enumerate() {
                lines.push(row(index == at, &root.to_string_lossy()));
            }
        }
        Some(Contents::Dirs(dirs)) => {
            let listing = snap
                .dirs
                .as_ref()
                .filter(|listing| listing.path.as_deref() == picker.path.as_deref());
            if let Some(error) = listing.and_then(|listing| listing.error.as_deref()) {
                lines.push(Line::from(Span::styled(
                    format::safe(error),
                    Style::new().fg(theme::RED_500),
                )));
            }
            if dirs.is_empty() && listing.is_some() {
                lines.push(Line::from(Span::styled("no folders inside", muted)));
            }
            for (index, dir) in dirs.iter().enumerate() {
                lines.push(row(index == at, dir));
            }
        }
    }

    lines.push(keys_hint(&[
        ("Enter", "open"),
        ("Backspace", "up"),
        ("s", "choose"),
        ("Esc", "back"),
    ]));
    lines
}

/// What the picker calls the folder it is listing.
fn location(picker: &FolderPicker) -> String {
    match &picker.path {
        Some(path) => path.display().to_string(),
        None => "the folders you may start from".to_string(),
    }
}

/// One folder: the marker in front of the one the cursor is on, and its name in body text. Folder names
/// came from somebody's disk, so they go through `safe` like everything else that was not typed here.
fn row(focused: bool, name: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            if focused { MARKER } else { " " },
            Style::new().fg(theme::VIOLET_500),
        ),
        Span::raw(" "),
        Span::styled(format::safe(name), Style::new().fg(theme::INK_300)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbtt_core::{Connection, DirListing, LiveLimits, Totals};

    /// A snapshot with nothing in it but the folder listing a test is about.
    fn snapshot(dirs: Option<DirListing>) -> Snapshot {
        Snapshot {
            connection: Connection::Local,
            torrents: Vec::new(),
            totals: Totals {
                down_rate: 0,
                up_rate: 0,
                dht_nodes: None,
            },
            limits: LiveLimits {
                download_kib: 0,
                upload_kib: 0,
                active_downloads: None,
                active_seeds: None,
                active_total: None,
            },
            detail: None,
            default_dir: PathBuf::from("/downloads"),
            dirs,
            devices: None,
        }
    }

    fn roots() -> Vec<PathBuf> {
        vec![PathBuf::from("/downloads"), PathBuf::from("/home/alice")]
    }

    /// A listing of `/downloads` with two folders inside it, which is what every test here stands on.
    fn listing() -> DirListing {
        DirListing {
            path: Some(PathBuf::from("/downloads")),
            roots: roots(),
            dirs: vec!["shows".to_string(), "tv".to_string()],
            error: None,
        }
    }

    #[test]
    fn going_up_stays_inside_the_folders_that_may_be_named() {
        // Inside a root, the parent is another listing.
        assert_eq!(
            parent_inside(&roots(), Path::new("/downloads/shows")),
            Some(PathBuf::from("/downloads"))
        );
        assert_eq!(
            parent_inside(&roots(), Path::new("/home/alice/downloads/deep")),
            Some(PathBuf::from("/home/alice/downloads"))
        );

        // A root's own parent is nowhere this backend may name, so there is no folder to go to: the caller
        // shows the list of roots instead of that reason.
        assert_eq!(parent_inside(&roots(), Path::new("/downloads")), None);
        assert_eq!(parent_inside(&roots(), Path::new("/home/alice")), None);

        // A root that is a whole filesystem root does have somewhere above it, and that place is itself.
        assert_eq!(
            parent_inside(&[PathBuf::from("/")], Path::new("/downloads")),
            Some(PathBuf::from("/"))
        );

        // Nothing known about containment yet means no answer about it either.
        assert_eq!(parent_inside(&[], Path::new("/downloads/x")), None);
    }

    #[test]
    fn a_cursor_stays_inside_the_folder_being_listed() {
        let snap = snapshot(Some(listing()));
        let mut picker = FolderPicker {
            path: Some(PathBuf::from("/downloads")),
            cursor: 9,
        };
        assert_eq!(picker.row(&snap), 1, "the cursor rode past the end");

        picker.step(5, &snap);
        assert_eq!(picker.cursor, 1, "moving down past the last row wrapped");
        picker.step(-5, &snap);
        assert_eq!(picker.cursor, 0, "moving up past the first row stopped");

        // A folder whose answer has not arrived has no rows, and a cursor there reads as zero.
        let elsewhere = FolderPicker {
            path: Some(PathBuf::from("/not-listed-yet")),
            cursor: 3,
        };
        assert_eq!(elsewhere.row(&snap), 0);
        assert!(elsewhere.contents(&snap).is_none());
    }

    #[test]
    fn going_up_from_a_root_shows_the_roots_instead_of_the_refusal() {
        let snap = snapshot(Some(listing()));
        // Standing in the root itself: the step out would leave every folder this backend may name.
        let mut picker = FolderPicker {
            path: Some(PathBuf::from("/downloads")),
            cursor: 0,
        };
        assert!(matches!(
            on_key(&mut picker, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), &snap),
            PickerAnswer::Stay(actions) if actions.is_empty()
        ));
        assert_eq!(
            picker.path, None,
            "going up from the root did not reach the roots"
        );

        // And there is nothing above the roots themselves: Backspace does nothing rather than asking for
        // a parent that would be refused.
        assert!(matches!(
            on_key(&mut picker, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), &snap),
            PickerAnswer::Stay(actions) if actions.is_empty()
        ));
        assert_eq!(picker.path, None);
    }

    #[test]
    fn opening_a_folder_asks_for_its_listing_and_s_hands_the_folder_back() {
        let snap = snapshot(Some(listing()));
        let mut picker = FolderPicker {
            path: Some(PathBuf::from("/downloads")),
            cursor: 1,
        };

        // Enter opens the highlighted folder, and says so to the backend.
        assert_eq!(
            on_key(
                &mut picker,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &snap
            ),
            PickerAnswer::Stay(vec![Action::Command(Command::ListDirs(Some(
                PathBuf::from("/downloads/tv")
            )))])
        );
        assert_eq!(picker.path, Some(PathBuf::from("/downloads/tv")));
        assert_eq!(
            picker.cursor, 0,
            "the new folder opened with a cursor somewhere else"
        );

        // The answer arrives, and `s` chooses the folder being listed rather than one inside it.
        let inside = snapshot(Some(DirListing {
            path: Some(PathBuf::from("/downloads/tv")),
            roots: roots(),
            dirs: vec!["season-one".to_string()],
            error: None,
        }));
        assert_eq!(
            on_key(
                &mut picker,
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
                &inside
            ),
            PickerAnswer::Chosen(PathBuf::from("/downloads/tv"))
        );

        // At the places to start from, choosing means the highlighted root.
        let mut at_roots = FolderPicker {
            path: None,
            cursor: 1,
        };
        assert_eq!(
            on_key(
                &mut at_roots,
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
                &snap
            ),
            PickerAnswer::Chosen(PathBuf::from("/home/alice"))
        );
    }
}
