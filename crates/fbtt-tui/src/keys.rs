//! The key map: which press means what, and the one list of actions that the footer and
//! the help screen both read from.
//!
//! A key is an action, not a shortcut for a widget: `p` means "stop the torrent under the
//! selection" wherever the cursor happens to be. The table here is the whole map, so the
//! help screen cannot drift from what the keys actually do and the footer cannot promise a
//! key that does nothing.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use fbtt_core::{Command, RowState, Snapshot, TorrentRow};

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
    ("y", "confirm quit"),
    ("?", "help"),
    ("q", "quit"),
];

/// The hints the footer has room for, in the order it shows them. Each one is a key of
/// [`KEYS`].
pub const FOOTER_KEYS: &[&str] = &["a", "d", "p", "/", "m", "?", "q"];

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
                // Add, delete, settings, filter and help open dialogs: Task 6 gives them
                // their keys back. Reserved for later milestones: `c` connects, `s` streams.
                KeyCode::Char('/')
                | KeyCode::Char('a')
                | KeyCode::Char('d')
                | KeyCode::Char('S')
                | KeyCode::Char('?') => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_keys_are_in_the_key_table() {
        for key in FOOTER_KEYS {
            assert!(
                KEYS.iter().any(|(name, _)| name == key),
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
