//! The key map: which press means what, and the one list of actions that the footer and
//! the help screen both read from.
//!
//! A key is an action, not a shortcut for a widget: `p` means "stop the torrent under the
//! selection" wherever the cursor happens to be. The table here is the whole map, so the
//! help screen cannot drift from what the keys actually do and the footer cannot promise a
//! key that does nothing.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use fbtt_core::{
    Command, Detail, DetailTab, FilePriority, FileRow, RowState, Snapshot, TorrentRow,
};

use crate::modal::{self, Modal};
use crate::model::{Model, visible_rows};

/// Every key the model answers, and what pressing it does. The help screen lists all of
/// them; [`FOOTER_KEYS`] chooses which of them a person needs without looking, so those
/// wordings have to fit one line while the rest can be a phrase.
pub const KEYS: &[(&str, &str)] = &[
    ("j", "next torrent"),
    ("k", "previous torrent"),
    ("g", "first torrent"),
    ("G", "last torrent"),
    ("End", "last torrent"),
    ("Page Down", "down a page"),
    ("Page Up", "up a page"),
    ("Tab", "next detail tab"),
    ("Shift+Tab", "previous detail tab"),
    ("o", "sort by the next column"),
    ("O", "reverse the sort"),
    ("p", "pause/resume"),
    ("/", "filter"),
    ("m", "minimal"),
    ("a", "add"),
    ("d", "delete"),
    ("S", "settings"),
    ("?", "help"),
    // These answer inside a dialog or in the file list rather than in the table, and they are in
    // this one list so that the help screen has to mention them too.
    ("Enter", "focus the file list"),
    ("+", "raise the focused file's priority"),
    ("-", "lower the focused file's priority"),
    ("Esc", "close a dialog"),
    ("y", "confirm quit"),
    ("q", "quit"),
];

/// The hints the footer has room for, in the order it shows them. Each one is a key of
/// [`KEYS`].
pub const FOOTER_KEYS: &[&str] = &["a", "d", "p", "/", "m", "?", "q"];

/// The hints the footer gives instead while the cursor is in the detail pane's file list, where
/// `j` and `k` mean files rather than torrents, as the key and what it does there. Every key of it
/// is a key of [`KEYS`] too, which a test keeps true; the words beside them are this list's own,
/// because they describe where the cursor is: `Esc` in the list leaves the list, it does not close
/// a dialog.
pub const FILE_LIST_HINTS: &[(&str, &str)] = &[
    ("+", "raise the focused file's priority"),
    ("-", "lower the focused file's priority"),
    ("Esc", "leave the file list"),
];

/// How far `PageDown` and `PageUp` move the selection. The model does not know how tall
/// the terminal is, so this is a page by agreement rather than a screenful.
const PAGE_STEP: i32 = 10;

/// Applies one press to `model` and returns what the backend should be told.
pub(crate) fn handle(model: &mut Model, key: KeyEvent, snap: &Snapshot) -> Vec<Command> {
    // Only presses count. Windows reports a release for every press, and a key held down
    // repeats: one row at a time either way.
    if key.kind != KeyEventKind::Press {
        return Vec::new();
    }
    // Ctrl, Alt and the window key mean something else, and M1 has no binding for them.
    // Terminals report an upper case letter as the character they typed, so a shift press
    // is only accepted where it is the whole of the modifier.
    if !matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) {
        return Vec::new();
    }
    // A dialog owns the keyboard while it is up. A `q` in a magnet field is a letter, a `j` in
    // the delete question is an answer, and nothing here moves the table behind it.
    if model.modal.is_some() {
        return modal::handle(model, key, snap);
    }

    let rows = visible_rows(model, snap);
    match key.code {
        // Quitting is the one action that may need a second press, so it comes first and
        // answers before any of the rest of the map runs.
        KeyCode::Char('q') => {
            if model.quit_armed() {
                model.quit_now()
            } else if any_downloading(snap) {
                model.arm_quit();
                Vec::new()
            } else {
                model.quit_now()
            }
        }
        KeyCode::Char('y') => {
            if model.quit_armed() {
                model.quit_now()
            } else {
                Vec::new()
            }
        }
        code => {
            // Any other key answers the pending confirmation by not confirming it.
            model.cancel_quit();
            if model.files_focused {
                return file_list_keys(model, code, snap);
            }
            match code {
                KeyCode::Char('j') | KeyCode::Down => model.step_by(1, &rows),
                KeyCode::Char('k') | KeyCode::Up => model.step_by(-1, &rows),
                KeyCode::Char('g') => model.select_row(0, &rows),
                KeyCode::Char('G') | KeyCode::End => {
                    model.select_row(rows.len().saturating_sub(1), &rows);
                }
                KeyCode::PageDown => model.step_by(PAGE_STEP, &rows),
                KeyCode::PageUp => model.step_by(-PAGE_STEP, &rows),
                KeyCode::Char('p') => return pause_or_resume(model.selected_row(&rows)),
                KeyCode::Tab => model.step_detail_tab(true),
                // Shift+Tab arrives as its own key on every terminal crossterm supports.
                KeyCode::BackTab => model.step_detail_tab(false),
                KeyCode::Char('o') => model.cycle_sort(),
                KeyCode::Char('O') => model.reverse_sort(),
                KeyCode::Char('m') => model.toggle_view(),
                // These five open a dialog, which then answers every key itself. `c` and `s` are
                // reserved for later milestones: connect, and stream.
                KeyCode::Char('/') => model.modal = Some(Modal::Filter),
                KeyCode::Char('a') => model.modal = Some(Modal::add(snap)),
                KeyCode::Char('S') => model.modal = Some(Modal::settings(snap)),
                KeyCode::Char('?') => model.modal = Some(Modal::Help),
                KeyCode::Char('d') => {
                    // The question names the torrent it is about, so there has to be one.
                    if let Some(row) = model.selected_row(&rows) {
                        model.modal = Some(Modal::delete(row));
                    }
                }
                KeyCode::Enter => focus_file_list(model, snap),
                _ => {}
            }
            Vec::new()
        }
    }
}

/// `p` for the selected row: a stopped torrent is started and anything else is stopped.
/// Nothing selected sends nothing.
fn pause_or_resume(row: Option<&TorrentRow>) -> Vec<Command> {
    let Some(row) = row else {
        return Vec::new();
    };
    let command = match row.state {
        RowState::Paused | RowState::Queued => Command::Resume(row.id.clone()),
        // Running, waiting for metadata, or stopped by a failure: the screen offers the key
        // and the backend decides what stopping an errored torrent means.
        _ => Command::Pause(row.id.clone()),
    };
    vec![command]
}

/// Whether something is still pulling bytes, which is what makes quitting worth asking about.
fn any_downloading(snap: &Snapshot) -> bool {
    snap.torrents
        .iter()
        .any(|row| matches!(row.state, RowState::Downloading))
}

/// `Enter` on the Files tab puts the cursor in the file list, where `j` and `k` move it and `+`
/// and `-` change what is downloaded of the file under it. A list with nothing in it is not worth
/// moving a cursor into: there would be nothing to change, and no way to tell it was focused.
fn focus_file_list(model: &mut Model, snap: &Snapshot) {
    if model.detail_tab != DetailTab::Files {
        return;
    }
    if watched_files(model, snap).is_some_and(|files| !files.is_empty()) {
        model.set_files_focused(true);
    }
}

/// The keys the file list answers while it has the cursor: moving along it, changing the priority
/// of the file under the cursor, and leaving. Everything else is ignored rather than falling
/// through to the table, because the person pressing it meant the list. Quit is the exception, and
/// it was answered before any of this ran.
fn file_list_keys(model: &mut Model, code: KeyCode, snap: &Snapshot) -> Vec<Command> {
    match code {
        KeyCode::Esc => {
            model.set_files_focused(false);
            Vec::new()
        }
        KeyCode::Char('j') | KeyCode::Down => {
            model.step_file_cursor(1, file_count(model, snap));
            Vec::new()
        }
        KeyCode::Char('k') | KeyCode::Up => {
            model.step_file_cursor(-1, file_count(model, snap));
            Vec::new()
        }
        // Changing pane is another way of leaving this one, and the key that does it should keep
        // working while the cursor is here.
        KeyCode::Tab => {
            model.set_files_focused(false);
            model.step_detail_tab(true);
            Vec::new()
        }
        KeyCode::BackTab => {
            model.set_files_focused(false);
            model.step_detail_tab(false);
            Vec::new()
        }
        KeyCode::Char('+') | KeyCode::Char('-') => {
            change_priority(model, snap, code == KeyCode::Char('+'))
        }
        _ => Vec::new(),
    }
}

/// `+` and `-` on the file under the cursor. Only the command is sent: the model does not assume
/// it worked, so the word in the list changes when the next snapshot says it did. A file already
/// at the top or the bottom of the ladder sends nothing.
fn change_priority(model: &Model, snap: &Snapshot, raise: bool) -> Vec<Command> {
    let Some(files) = watched_files(model, snap) else {
        return Vec::new();
    };
    let at = model.file_cursor.min(files.len().saturating_sub(1));
    let Some(file) = files.get(at) else {
        return Vec::new();
    };
    let next = if raise {
        higher_priority(file.priority)
    } else {
        lower_priority(file.priority)
    };
    if next == file.priority {
        return Vec::new();
    }
    let Some(id) = &model.selected else {
        return Vec::new();
    };
    vec![Command::SetFilePriority {
        id: id.clone(),
        file: file.index,
        priority: next,
    }]
}

/// The file list the snapshot holds for the selected torrent, `None` when it holds none: while a
/// torrent's metadata has not arrived there are no files to change.
fn watched_files<'a>(model: &Model, snap: &'a Snapshot) -> Option<&'a [FileRow]> {
    let id = model.selected.as_ref()?;
    match &snap.detail {
        Some((watched, Detail::Files(files))) if watched == id => Some(files),
        _ => None,
    }
}

/// How many files the focused list holds, which is what stops the cursor at the last one.
fn file_count(model: &Model, snap: &Snapshot) -> usize {
    watched_files(model, snap).map_or(0, |files| files.len())
}

/// The next priority up, or the same one at the top of the ladder.
const fn higher_priority(priority: FilePriority) -> FilePriority {
    match priority {
        FilePriority::Skip => FilePriority::Low,
        FilePriority::Low => FilePriority::Normal,
        FilePriority::Normal | FilePriority::High => FilePriority::High,
    }
}

/// The next priority down, or the same one at the bottom of it.
const fn lower_priority(priority: FilePriority) -> FilePriority {
    match priority {
        FilePriority::Skip | FilePriority::Low => FilePriority::Skip,
        FilePriority::Normal => FilePriority::Low,
        FilePriority::High => FilePriority::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hint_the_footer_gives_is_a_key_the_model_answers() {
        // The file-list hints carry their own wording, so only the keys are checked against the
        // table here: what they say is this list's business.
        let names = FOOTER_KEYS
            .iter()
            .copied()
            .chain(FILE_LIST_HINTS.iter().map(|(key, _)| *key));
        for key in names {
            assert!(
                KEYS.iter().any(|(name, _)| *name == key),
                "the footer hints at {key:?}, which the model does not answer"
            );
        }
    }

    #[test]
    fn every_action_is_something_to_read() {
        for (key, action) in KEYS {
            assert!(!key.is_empty(), "a key with no name");
            assert!(!action.is_empty(), "{key:?} has no description");
        }
    }
}
