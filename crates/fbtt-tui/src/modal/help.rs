//! The help screen: every key the model answers, and what pressing it does.
//!
//! The list comes from [`crate::keys`], which is also where the footer reads its hints, so a key
//! cannot be added to one and forgotten in the other.

use crossterm::event::{KeyCode, KeyEvent};

use crate::graphics::place::LogoView;
use crate::keys::KEYS;
use crate::modal::{Answer, keys_hint};
use crate::theme;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// Cells between a key and what it does.
const KEY_GAP: usize = 2;
/// Cells between the two columns, used only when the list is drawn in two.
const COLUMN_GAP: usize = 4;

/// The keys, padded into one or two columns, and the key that puts the screen away. `area` is the
/// whole screen: a list of this length needs more rows than a laptop terminal has, so on a screen
/// too short for it in one column and wide enough for it in two, it is drawn in two.
///
/// The mark is not part of the list: it stands above the keys in rows the box leaves for it, and
/// [`crate::modal::help_placement`] decides whether the box can afford that. The list itself never
/// changes shape because of the mark, which is what keeps the box `draw_box` draws and the rect
/// the mark is placed over from disagreeing about where the keys begin.
pub(crate) fn lines(area: Rect, _logos: LogoView<'_>) -> Vec<Line<'static>> {
    let half = KEYS.len().div_ceil(2);
    let (left, right) = KEYS.split_at(half);
    let widest = |rows: &[(&str, &str)]| rows.iter().map(|(key, _)| key.chars().count()).max();

    let mut lines: Vec<Line<'static>> = Vec::new();
    if !columns(area, left, right) {
        let width = widest(KEYS).unwrap_or_default();
        for (key, action) in KEYS {
            lines.push(Line::from(row(key, action, width)));
        }
        lines.push(close_hint());
        return lines;
    }

    let (left_width, right_width) = (widest(left), widest(right));
    for (index, (key, action)) in left.iter().enumerate() {
        let mut spans = row(key, action, left_width.unwrap_or_default());
        spans.push(Span::raw(" ".repeat(COLUMN_GAP)));
        if let Some((key, action)) = right.get(index) {
            spans.extend(row(key, action, right_width.unwrap_or_default()));
        }
        lines.push(Line::from(spans));
    }
    lines.push(close_hint());
    lines
}

/// Whether this screen is better served by two columns: one would run off the bottom, and two fit
/// across. A list too short to need splitting never splits. This is the only place the decision
/// is made: both [`lines`] and [`crate::modal::help_placement`] ask it, so the box the list is
/// drawn in and the mark that stands above it can never disagree about the list's shape.
pub(crate) fn columns(area: Rect, left: &[(&str, &str)], right: &[(&str, &str)]) -> bool {
    if right.is_empty() {
        return false;
    }
    // Two borders and the line that says how to close the screen.
    let rows_available = usize::from(area.height).saturating_sub(3);
    if rows_available >= KEYS.len() {
        return false;
    }
    let cells = |rows: &[(&str, &str)]| {
        rows.iter()
            .map(|(key, action)| key.chars().count() + KEY_GAP + action.chars().count())
            .max()
            .unwrap_or_default()
    };
    usize::from(area.width) >= cells(left) + COLUMN_GAP + cells(right) + 4
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
        let one = lines(Rect::new(0, 0, 100, 40), LogoView::text());
        assert_eq!(
            one.len(),
            KEYS.len() + 1,
            "a tall screen did not list every key"
        );

        let two = lines(Rect::new(0, 0, 80, 24), LogoView::text());
        assert_eq!(
            two.len(),
            KEYS.len().div_ceil(2) + 1,
            "a short screen did not split the list"
        );

        // Every action is on the screen either way, which is the point of splitting it.
        for name in ["quit", "raise the focused file's priority"] {
            for area in [Rect::new(0, 0, 100, 40), Rect::new(0, 0, 80, 24)] {
                let text = lines(area, LogoView::text())
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
            let lines = lines(area, LogoView::text());
            assert!(!lines.is_empty(), "no help was drawn at {area:?}");
        }
    }

    #[test]
    fn the_columns_decision_never_splits_when_the_keys_fit_one_column() {
        // A screen tall enough for the whole list in one column never splits, whatever the
        // width, and one too short for it splits when the two columns fit across.
        assert!(!columns(Rect::new(0, 0, 100, 40), KEYS, KEYS));
        assert!(columns(Rect::new(0, 0, 80, 24), KEYS, KEYS));
    }
}
