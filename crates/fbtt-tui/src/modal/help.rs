//! The help screen: every key the model answers, and what pressing it does.
//!
//! The list comes from [`crate::keys`], which is also where the footer reads its hints, so a key
//! cannot be added to one and forgotten in the other.

use crossterm::event::{KeyCode, KeyEvent};

use crate::keys::KEYS;
use crate::modal::{Answer, keys_hint};
use crate::theme;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// Cells between a key and what it does.
const KEY_GAP: usize = 1;
/// Cells between the two columns, used only when the list is drawn in two.
const COLUMN_GAP: usize = 2;

/// The keys, padded into one or two columns, and the key that puts the screen away. `area` is the
/// whole screen: a list of this length needs more rows than a laptop terminal has, so on a screen
/// too short for it in one column and wide enough for it in two, it is drawn in two.
pub(crate) fn lines(area: Rect) -> Vec<Line<'static>> {
    let half = KEYS.len().div_ceil(2);
    let (left, right) = KEYS.split_at(half);
    let widest = |rows: &[(&str, &str)]| rows.iter().map(|(key, _)| key.chars().count()).max();

    if !wants_two_columns(area, left, right) {
        let width = widest(KEYS).unwrap_or_default();
        let mut lines: Vec<Line<'static>> = KEYS
            .iter()
            .map(|(key, action)| Line::from(row(key, action, width)))
            .collect();
        lines.push(close_hint());
        return lines;
    }

    let (left_width, right_width) = (widest(left), widest(right));
    let mut lines: Vec<Line<'static>> = left
        .iter()
        .enumerate()
        .map(|(index, (key, action))| {
            let mut spans = row(key, action, left_width.unwrap_or_default());
            spans.push(Span::raw(" ".repeat(COLUMN_GAP)));
            if let Some((key, action)) = right.get(index) {
                spans.extend(row(key, action, right_width.unwrap_or_default()));
            }
            Line::from(spans)
        })
        .collect();
    lines.push(close_hint());
    lines
}

/// Whether this screen is better served by two columns: one would run off the bottom, and two fit
/// across. A list too short to need splitting never splits.
fn wants_two_columns(area: Rect, left: &[(&str, &str)], right: &[(&str, &str)]) -> bool {
    if right.is_empty() {
        return false;
    }
    // Two borders and the line that says how to close the screen.
    let rows_available = usize::from(area.height).saturating_sub(3);
    if rows_available >= KEYS.len() {
        return false;
    }
    // The widest two-column row pairs the widest left action with the widest right action:
    // the left key column and the right key column are each padded to their own widest key,
    // so the row that is widest is the one that carries both widest actions.
    let cells = |rows: &[(&str, &str)]| {
        rows.iter()
            .map(|(_, action)| action.chars().count())
            .max()
            .unwrap_or_default()
    };
    let widest_key = |rows: &[(&str, &str)]| {
        rows.iter()
            .map(|(key, _)| key.chars().count())
            .max()
            .unwrap_or_default()
    };
    let two_col_width = widest_key(left)
        + KEY_GAP
        + cells(left)
        + COLUMN_GAP
        + widest_key(right)
        + KEY_GAP
        + cells(right)
        + 4;
    usize::from(area.width) >= two_col_width
}

/// One row: the key in the accent, padded so the actions line up, and what it does beside it.
fn row(key: &str, action: &str, width: usize) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{key:<width$}"), Style::new().fg(theme::VIOLET_400)),
        Span::raw(" ".repeat(KEY_GAP)),
        Span::styled(action.to_string(), Style::new().fg(theme::INK_300)),
    ]
}

/// The list is its own screen, so it says how to leave it.
fn close_hint() -> Line<'static> {
    keys_hint(&[("Esc", "close")])
}

/// Esc puts the help away. Every other key stays out of the table: a person reading the keys is
/// not moving the selection.
pub(crate) fn on_key(key: KeyEvent) -> Answer {
    match key.code {
        KeyCode::Esc | KeyCode::Char('?') => Answer::Close(Vec::new()),
        _ => Answer::Stay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tall_screen_gets_one_column_and_a_short_wide_one_two() {
        let one = lines(Rect::new(0, 0, 100, 40));
        assert_eq!(
            one.len(),
            KEYS.len() + 1,
            "a tall screen did not list every key"
        );

        let two = lines(Rect::new(0, 0, 80, 24));
        assert_eq!(
            two.len(),
            KEYS.len().div_ceil(2) + 1,
            "a short screen did not split the list"
        );

        // Every action is on the screen either way, which is the point of splitting it.
        for name in ["quit", "raise the focused file's priority"] {
            for area in [Rect::new(0, 0, 100, 40), Rect::new(0, 0, 80, 24)] {
                let text = lines(area)
                    .iter()
                    .map(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>()
                    })
                    .collect::<Vec<String>>()
                    .join("\n");
                assert!(text.contains(name), "{name:?} is missing at {area:?}");
            }
        }
    }

    #[test]
    fn a_screen_too_small_for_both_shapes_still_draws_what_fits() {
        for area in [Rect::new(0, 0, 20, 5), Rect::new(0, 0, 1, 1)] {
            let lines = lines(area);
            assert!(!lines.is_empty(), "no help was drawn at {area:?}");
        }
    }
}
