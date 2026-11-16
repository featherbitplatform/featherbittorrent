//! The destination picker: the folders this backend may name, walked one directory at a time.
//!
//! It is a dialog inside a dialog - the add box opens it over itself, and hands back the folder that was
//! chosen - because the answer comes from wherever the torrents are going to live: `Command::ListDirs`
//! on this machine's folders when the engine is local, and on a server's allowed folders when it is not.
//! The picker does not care which: it asks for a folder, draws what comes back, and never waits for it.
//!
//! What it draws comes from the newest snapshot rather than from its own copy, because the backend
//! replaces that listing whenever it answers again, and a picker holding its own would be showing last
//! answer's folders. An arriving listing is recognised as the answer to an ask by being *different* from
//! what was on screen when the ask went out, rather than by naming the folder that was asked for: a server
//! that canonicalises paths answers `/downloads/shows` with `/private/downloads/shows`, and a picker that
//! only accepted exact names would sit reading `reading folders...` over an answer it had already got.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fbtt_core::{Command, DirListing, Snapshot};

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

/// Where the person is in the folder tree, which ask is still outstanding, and which row of the listing
/// they are on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderPicker {
    /// The folder whose contents are on screen, spelled as the backend spelled it, or `None` for the list
    /// of places to start from. What the backend named is what gets chosen: a server that canonicalises
    /// paths answers with a different spelling from the one it was given, and that spelling names the
    /// folder that actually exists.
    standing: Option<PathBuf>,
    /// Which row of that listing the cursor is on.
    cursor: usize,
    /// The `ListDirs` that went out last: the folder it named, and the listing that was on screen when it
    /// left. Its answer is the first listing to arrive that differs from the one that was there, or one
    /// that names the asked folder exactly - which is how re-opening the picker over a listing already on
    /// screen gets an answer instead of waiting for a change nobody was going to make.
    waiting: Option<Ask>,
}

/// One ask, and what the screen was showing when it went out.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ask {
    /// The folder that was asked about, `None` for the list of places to start from.
    folder: Option<PathBuf>,
    /// The listing on screen at the time, which is what an answer has to be different from.
    before: Option<DirListing>,
}

impl Ask {
    /// Whether `listing` answers this ask: something arrived after it went out, or it names exactly the
    /// folder that was asked for.
    fn answered_by(&self, listing: &DirListing) -> bool {
        self.before.as_ref() != Some(listing) || listing.path == self.folder
    }
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
    /// listing is not held here - it arrives in a snapshot - but opening the picker is an ask, and an ask
    /// is recognised by what its answer changes, so `snap` has to come along to be remembered as the
    /// before.
    pub(crate) fn open(snap: &Snapshot) -> (FolderPicker, Vec<Action>) {
        (
            FolderPicker {
                standing: None,
                cursor: 0,
                waiting: Some(Ask {
                    folder: None,
                    before: snap.dirs.clone(),
                }),
            },
            vec![Action::Command(Command::ListDirs(None))],
        )
    }

    /// The listing on screen and the folder it is a listing of: `None` while the answer to the last ask has
    /// not arrived. See [`Ask::answered_by`] for what counts as an answer.
    fn here<'a>(&self, snap: &'a Snapshot) -> Option<(&'a DirListing, Option<PathBuf>)> {
        let listing = snap.dirs.as_ref()?;
        match &self.waiting {
            // Nothing outstanding: what is on screen lists the folder this picker last stood in.
            None => Some((listing, self.standing.clone())),
            Some(ask) if ask.answered_by(listing) => Some((listing, listing.path.clone())),
            Some(_) => None,
        }
    }

    /// Takes an answer that has arrived: the folder on screen becomes the one the backend named - which is
    /// how a canonicalising path lands in the field rather than the spelling that was typed - and the ask
    /// stops being outstanding. `here` has already said whether an answer arrived, so the ask is taken out
    /// once rather than asked about twice.
    fn commit(&mut self, snap: &Snapshot) {
        let answered = self
            .here(snap)
            .and_then(|(_, folder)| self.waiting.take().map(|_| folder));
        if let Some(folder) = answered {
            self.standing = folder;
        }
    }

    /// The rows on screen: the places to start from, or the folders inside the one being listed. `None`
    /// means the answer for this folder has not arrived yet.
    fn contents<'a>(&self, snap: &'a Snapshot) -> Option<Contents<'a>> {
        let (listing, folder) = self.here(snap)?;
        Some(match folder {
            // Every listing echoes the folders it may name, so going up one does not need to ask again.
            None => Contents::Roots(&listing.roots),
            Some(_) => Contents::Dirs {
                dirs: &listing.dirs,
                error: listing.error.as_deref(),
            },
        })
    }

    /// How many rows there are to move along.
    fn row_count(&self, snap: &Snapshot) -> usize {
        match self.contents(snap) {
            Some(Contents::Roots(roots)) => roots.len(),
            Some(Contents::Dirs { dirs, .. }) => dirs.len(),
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

    /// Goes to a folder and asks for what is inside it. `snap` comes along because the answer has to be
    /// recognised by what it changes: the listing on screen now is the one it must not turn out to be.
    fn enter(&mut self, path: PathBuf, snap: &Snapshot) -> PickerAnswer {
        self.standing = Some(path.clone());
        self.cursor = 0;
        self.waiting = Some(Ask {
            folder: Some(path.clone()),
            before: snap.dirs.clone(),
        });
        PickerAnswer::Stay(vec![Action::Command(Command::ListDirs(Some(path)))])
    }

    /// Goes back to the list of places to start from, which needs no ask: every listing already names them.
    fn show_roots(&mut self) {
        self.standing = None;
        self.cursor = 0;
        self.waiting = None;
    }
}

/// What the picker is listing.
enum Contents<'a> {
    /// The folders this backend may name at all, in the order it named them.
    Roots(&'a [PathBuf]),
    /// The directories inside the folder on screen, as the backend sorted them, and why that folder could
    /// not be listed when it could not.
    Dirs {
        dirs: &'a [String],
        error: Option<&'a str>,
    },
}

/// Applies one press to the picker. `Enter` opens a folder, `Backspace` goes up, `s` chooses, and `j` and
/// `k` move along the list; everything else is ignored rather than closing anything, because a person
/// pressing another key meant to stay in the picker.
pub(crate) fn on_key(picker: &mut FolderPicker, key: KeyEvent, snap: &Snapshot) -> PickerAnswer {
    // Whatever has arrived since the last press is taken first, so that every key below acts on the folder
    // that is actually on screen rather than on the one this picker was standing in when it asked.
    picker.commit(snap);
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
        KeyCode::Enter => {
            // The folder to open, worked out before the picker is moved: an answer that arrived with this
            // press names the folder its own way, and joining onto that is what keeps the path real.
            let next = match picker.here(snap) {
                Some((listing, None)) => listing.roots.get(picker.row(snap)).cloned(),
                Some((listing, Some(path))) => {
                    listing.dirs.get(picker.row(snap)).map(|dir| path.join(dir))
                }
                None => None,
            };
            match next {
                Some(path) => picker.enter(path, snap),
                None => PickerAnswer::Stay(Vec::new()),
            }
        }
        KeyCode::Char('s') => match picker.here(snap) {
            // Inside a folder, `s` chooses the folder being listed: that is the one named on the line
            // above the list, and the one a person standing in it means by "this folder".
            Some((_, Some(path))) => PickerAnswer::Chosen(path),
            // At the places to start from there is no folder being listed, so the highlighted one is what
            // choosing has to mean.
            Some((listing, None)) => match listing.roots.get(picker.row(snap)) {
                Some(root) => PickerAnswer::Chosen(root.clone()),
                None => PickerAnswer::Stay(Vec::new()),
            },
            // Nothing to choose until an answer arrives.
            None => PickerAnswer::Stay(Vec::new()),
        },
        KeyCode::Backspace => match picker.here(snap) {
            // The list of places to start from has nothing above it, so there is nothing to do.
            Some((_, None)) => PickerAnswer::Stay(Vec::new()),
            Some((listing, Some(path))) => match parent_inside(&listing.roots, &path) {
                Some(parent) => picker.enter(parent, snap),
                // The step out of this folder would leave every folder this backend may name, which is the
                // picker's top: it shows the places to start from rather than the reason that step was
                // refused, because the answer to "where do I go back to" is that list.
                None => {
                    picker.show_roots();
                    PickerAnswer::Stay(Vec::new())
                }
            },
            // Nothing has arrived yet to go up from.
            None => PickerAnswer::Stay(Vec::new()),
        },
        _ => PickerAnswer::Stay(Vec::new()),
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
    // While an ask is outstanding the line names the folder that was asked for, because that is what the
    // wait is about; once an answer is on screen it names the folder the answer named.
    let place = match location(picker, snap) {
        Some(path) => (path.display().to_string(), true),
        None => ("the folders you may start from".to_string(), false),
    };
    let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
        format::safe(&place.0),
        if place.1 {
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
        Some(Contents::Dirs { dirs, error }) => {
            if let Some(error) = error {
                lines.push(Line::from(Span::styled(
                    format::safe(error),
                    Style::new().fg(theme::RED_500),
                )));
            }
            if dirs.is_empty() {
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

/// What the picker calls the folder it is listing: the one on screen when there is one, and otherwise the
/// one its outstanding ask is waiting for. `None` is the places to start from.
fn location(picker: &FolderPicker, snap: &Snapshot) -> Option<PathBuf> {
    match picker.here(snap) {
        Some((_, folder)) => folder,
        None => picker.waiting.as_ref().and_then(|ask| ask.folder.clone()),
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
    use fbtt_core::{Connection, LiveLimits, Totals};

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

    /// `/downloads`, with two folders inside it and nothing else on the snapshot.
    fn listing() -> DirListing {
        DirListing {
            path: Some(PathBuf::from("/downloads")),
            roots: roots(),
            dirs: vec!["movies".to_string(), "tv".to_string()],
            error: None,
        }
    }

    /// The places to start from, which is what a listing with no folder in it says.
    fn root_listing() -> DirListing {
        DirListing {
            path: None,
            roots: roots(),
            dirs: Vec::new(),
            error: None,
        }
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Standing in `path` with nothing outstanding, which is where the picker is once an answer to its
    /// last ask has arrived.
    fn standing_at(path: Option<PathBuf>) -> FolderPicker {
        FolderPicker {
            standing: path,
            cursor: 0,
            waiting: None,
        }
    }

    #[test]
    fn going_up_stays_inside_the_folders_that_may_be_named() {
        assert_eq!(
            parent_inside(&roots(), Path::new("/downloads/shows")),
            Some(PathBuf::from("/downloads"))
        );
        assert_eq!(parent_inside(&roots(), Path::new("/downloads")), None);

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
        let mut picker = standing_at(Some(PathBuf::from("/downloads")));
        picker.cursor = 9;
        assert_eq!(picker.row(&snap), 1, "the cursor rode past the end");

        picker.step(5, &snap);
        assert_eq!(picker.cursor, 1, "moving down past the last row wrapped");
        picker.step(-5, &snap);
        assert_eq!(
            picker.cursor, 0,
            "moving up past the first row went below it"
        );

        // A folder whose listing has not arrived has no rows at all, so the cursor cannot sit anywhere in
        // it: what is on screen is the wait, not a list.
        let waiting = FolderPicker {
            standing: Some(PathBuf::from("/downloads/shows")),
            cursor: 3,
            waiting: Some(Ask {
                folder: Some(PathBuf::from("/downloads/shows")),
                before: Some(listing()),
            }),
        };
        assert_eq!(waiting.row(&snap), 0);
        assert!(waiting.contents(&snap).is_none());
    }

    #[test]
    fn going_up_from_a_root_shows_the_roots_instead_of_the_refusal() {
        let snap = snapshot(Some(listing()));
        let mut picker = standing_at(Some(PathBuf::from("/downloads")));

        // `/downloads` is one of the roots, so there is nowhere above it to ask for: the picker shows the
        // places to start from, which every listing already names.
        assert!(matches!(
            on_key(&mut picker, press(KeyCode::Backspace), &snap),
            PickerAnswer::Stay(actions) if actions.is_empty()
        ));
        assert_eq!(picker.standing, None);
        assert!(
            matches!(picker.contents(&snap), Some(Contents::Roots(_))),
            "the step out of a root left the refused folder on screen"
        );

        // And there is nothing above the roots themselves: Backspace does nothing rather than asking for
        // a parent that would be refused.
        assert!(matches!(
            on_key(&mut picker, press(KeyCode::Backspace), &snap),
            PickerAnswer::Stay(actions) if actions.is_empty()
        ));
        assert_eq!(picker.standing, None);
    }

    #[test]
    fn opening_a_folder_asks_for_its_listing_and_s_hands_the_folder_back() {
        let snap = snapshot(Some(listing()));
        let mut picker = standing_at(Some(PathBuf::from("/downloads")));
        picker.cursor = 1;

        // Enter opens the highlighted folder, and says so to the backend.
        assert_eq!(
            on_key(&mut picker, press(KeyCode::Enter), &snap),
            PickerAnswer::Stay(vec![Action::Command(Command::ListDirs(Some(
                PathBuf::from("/downloads/tv")
            )))])
        );
        assert_eq!(picker.standing, Some(PathBuf::from("/downloads/tv")));
        assert_eq!(
            picker.cursor, 0,
            "the new folder opened with a cursor somewhere else"
        );

        // The same listing as the one that was there when the ask went out is not an answer: the picker
        // says it is reading rather than pretending the folder has been listed.
        assert!(picker.contents(&snap).is_none());

        // The answer arrives, and `s` chooses the folder being listed rather than one inside it.
        let inside = snapshot(Some(DirListing {
            path: Some(PathBuf::from("/downloads/tv")),
            roots: roots(),
            dirs: vec!["season-one".to_string()],
            error: None,
        }));
        assert_eq!(
            on_key(&mut picker, press(KeyCode::Char('s')), &inside),
            PickerAnswer::Chosen(PathBuf::from("/downloads/tv"))
        );

        // At the places to start from, choosing means the highlighted root.
        let mut at_roots = FolderPicker {
            standing: None,
            cursor: 1,
            waiting: None,
        };
        assert_eq!(
            on_key(&mut at_roots, press(KeyCode::Char('s')), &snap),
            PickerAnswer::Chosen(PathBuf::from("/home/alice"))
        );
    }

    #[test]
    fn opening_the_picker_over_a_root_listing_starts_there() {
        // Re-opening the picker is an ask too, so it has to be answered - and a listing of the places to
        // start from already on screen answers it without waiting for somebody to change it.
        let snap = snapshot(Some(root_listing()));
        let (mut picker, actions) = FolderPicker::open(&snap);
        assert_eq!(actions, vec![Action::Command(Command::ListDirs(None))]);
        assert!(matches!(picker.contents(&snap), Some(Contents::Roots(_))));

        // Opening over a folder instead: the roots answer has not arrived, so the picker waits rather than
        // drawing the folder it is no longer standing in.
        let (from_a_folder, _) = FolderPicker::open(&snapshot(Some(listing())));
        assert!(from_a_folder.contents(&snap).is_some());
        assert_eq!(
            from_a_folder.standing, None,
            "a picker opened at the roots drew somebody else's folder as its own"
        );

        // Nothing on screen at all is a wait, not an empty list of folders.
        let (fresh, _) = FolderPicker::open(&snapshot(None));
        assert!(fresh.contents(&snapshot(None)).is_none());
        picker.commit(&snap);
        assert_eq!(
            picker.waiting, None,
            "an answer that was already there stayed outstanding"
        );
    }

    #[test]
    fn a_folder_answered_under_another_name_is_still_an_answer() {
        // The case the picker was rewritten for: a backend that canonicalises paths answers with a
        // different spelling from the one it was given. Matching on exact equality left the picker reading
        // `reading folders...` over an answer it had already got, and no folder ever reached the field.
        let at_roots = snapshot(Some(root_listing()));
        let (mut picker, _) = FolderPicker::open(&at_roots);

        // Standing in `/downloads`, spelled as the roots named it.
        assert!(matches!(
            on_key(&mut picker, press(KeyCode::Enter), &at_roots),
            PickerAnswer::Stay(actions) if matches!(actions.as_slice(),
                [Action::Command(Command::ListDirs(Some(path)))] if path == &PathBuf::from("/downloads"))
        ));

        // The answer comes back as `/private/downloads`, which is what that path really is over here.
        let canonical = snapshot(Some(DirListing {
            path: Some(PathBuf::from("/private/downloads")),
            roots: vec![
                PathBuf::from("/private/downloads"),
                PathBuf::from("/home/alice"),
            ],
            dirs: vec!["shows".to_string(), "tv".to_string()],
            error: None,
        }));
        assert!(
            picker.contents(&canonical).is_some(),
            "a canonicalised answer was not taken as an answer"
        );
        // The folder on screen is the one the backend named, not the one that was asked for.
        assert_eq!(
            location(&picker, &canonical),
            Some(PathBuf::from("/private/downloads"))
        );

        // Choosing hands that spelling to the field, so what gets saved is a folder that exists.
        assert_eq!(
            on_key(&mut picker, press(KeyCode::Char('s')), &canonical),
            PickerAnswer::Chosen(PathBuf::from("/private/downloads"))
        );

        // And going into a folder inside it joins onto the name the backend gave, so the next ask is one
        // this server can answer.
        let (mut picker, _) = FolderPicker::open(&at_roots);
        on_key(&mut picker, press(KeyCode::Enter), &at_roots);
        picker.cursor = 0;
        assert!(matches!(
            on_key(&mut picker, press(KeyCode::Enter), &canonical),
            PickerAnswer::Stay(actions) if matches!(actions.as_slice(),
                [Action::Command(Command::ListDirs(Some(path)))] if path == &PathBuf::from("/private/downloads/shows"))
        ));
    }

    #[test]
    fn a_folder_that_could_not_be_listed_says_so_beside_the_folders_it_keeps() {
        let refused = snapshot(Some(DirListing {
            error: Some("not a folder this server may name".to_string()),
            ..listing()
        }));
        let picker = standing_at(Some(PathBuf::from("/downloads")));
        let drawn: Vec<String> = lines(&picker, &refused)
            .iter()
            .map(|line| line.to_string())
            .collect();
        assert!(
            drawn
                .iter()
                .any(|line| line == "not a folder this server may name"),
            "the reason was not drawn: {drawn:?}"
        );
        // The roots and the folders it did name stay on screen, because the picker has to have somewhere
        // to go.
        assert!(
            drawn.iter().any(|line| line.contains("movies")),
            "the refusal took the folders out of view: {drawn:?}"
        );

        // An empty folder says that rather than drawing nothing at all.
        let empty = snapshot(Some(DirListing {
            dirs: Vec::new(),
            ..listing()
        }));
        let drawn: Vec<String> = lines(&picker, &empty)
            .iter()
            .map(|line| line.to_string())
            .collect();
        assert!(
            drawn.iter().any(|line| line == "no folders inside"),
            "an empty folder looked like a missing answer: {drawn:?}"
        );

        // And while the answer has not arrived, that is what the list says.
        let (waiting, _) = FolderPicker::open(&snapshot(Some(listing())));
        let drawn: Vec<String> = lines(&waiting, &snapshot(None))
            .iter()
            .map(|line| line.to_string())
            .collect();
        assert!(
            drawn.iter().any(|line| line == "reading folders..."),
            "the wait was not drawn: {drawn:?}"
        );
    }
}
