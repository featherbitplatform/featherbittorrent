//! The help screen: every key the model answers, and what pressing it does.
//!
//! The list comes from [`crate::keys`], which is also where the footer reads its hints, so a key
//! cannot be added to one and forgotten in the other.

use crossterm::event::{KeyCode, KeyEvent};

use crate::graphics::place::{LogoView, Prepared};
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
/// The keys, padded into one or two columns, and the key that puts the screen away. `area` is the
/// whole screen: a list of this length needs more rows than a laptop terminal has, so on a screen
/// too short for it in one column and wide enough for it in two, it is drawn in two.
///
/// The mark, when the box has room for it and for every key, stands at the top of the box, above
/// the keys: the list's own rows begin with `mark.rows + 1` rows left empty for it, and the mark's
/// rect (see [`crate::modal::help_placement`]) is drawn over exactly those cells, centred, with
/// the one empty row between it and the first key. The help never loses a key line to the mark:
/// a box too short for the mark and for every key draws the keys alone, as before.
pub(crate) fn lines(
    area: Rect,
    _logos: LogoView<'_>,
    mark: Option<&Prepared>,
) -> Vec<Line<'static>> {
    let mark_rows = mark.map(|prepared| usize::from(prepared.dims.rows));
    let half = KEYS.len().div_ceil(2);
    let (left, right) = KEYS.split_at(half);
    let widest = |rows: &[(&str, &str)]| rows.iter().map(|(key, _)| key.chars().count()).max();

    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(rows) = mark_rows {
        for _ in 0..rows + 1 {
            lines.push(Line::default());
        }
    }
    if !wants_two_columns(area, left, right, mark_rows) {
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
/// across. A list too short to need splitting never splits.
///
/// `mark_rows` is the rows the mark stands in above the keys: the box must hold the keys and the
/// mark together, which is what decides whether one column is tall enough at all. A screen where
/// one column fits the keys but not the mark and the keys together splits, so the mark can stand
/// above a shorter list.
fn wants_two_columns(
    area: Rect,
    left: &[(&str, &str)],
    right: &[(&str, &str)],
    mark_rows: Option<usize>,
) -> bool {
    if right.is_empty() {
        return false;
    }
    let mark = mark_rows.unwrap_or(0) + 1;
    // Two borders and the line that says how to close the screen.
    let rows_available = usize::from(area.height).saturating_sub(3);
    // One column is enough when the keys and the mark both fit in it.
    if rows_available >= KEYS.len() + mark {
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

    /// A stand-in mark of `rows` rows, for sizing the list the way `help_placement` sizes it.
    fn mark(rows: u16) -> Prepared {
        Prepared {
            size: crate::graphics::LogoSize::Large(rows),
            dims: crate::graphics::LogoDims {
                cols: 10,
                rows,
                px_w: 0,
                px_h: 0,
                img_w: 0,
                img_h: 0,
            },
            payload: std::sync::Arc::new([]),
        }
    }

    #[test]
    fn a_tall_screen_gets_one_column_and_a_short_wide_one_two() {
        let one = lines(Rect::new(0, 0, 100, 40), LogoView::text(), None);
        assert_eq!(
            one.len(),
            KEYS.len() + 1,
            "a tall screen did not list every key"
        );

        let two = lines(Rect::new(0, 0, 80, 24), LogoView::text(), None);
        assert_eq!(
            two.len(),
            KEYS.len().div_ceil(2) + 1,
            "a short screen did not split the list"
        );

        // Every action is on the screen either way, which is the point of splitting it.
        for name in ["quit", "raise the focused file's priority"] {
            for area in [Rect::new(0, 0, 100, 40), Rect::new(0, 0, 80, 24)] {
                let text = lines(area, LogoView::text(), None)
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
            let lines = lines(area, LogoView::text(), None);
            assert!(!lines.is_empty(), "no help was drawn at {area:?}");
        }
    }

    #[test]
    fn the_mark_takes_the_rows_left_at_the_top_and_no_key_line_is_lost() {
        // No mark: the list is the keys and the close hint.
        let plain = lines(Rect::new(0, 0, 100, 40), LogoView::text(), None);
        assert_eq!(plain.len(), KEYS.len() + 1);

        // With the ten-row mark: its rows plus the blank row above the keys, and every key still
        // listed - the mark took the rows the screen had to spare, not a key line.
        let with_mark = lines(Rect::new(0, 0, 100, 40), LogoView::text(), Some(&mark(10)));
        // Every key is still in the list, on the rows the mark did not take: the mark stood in
        // the rows the screen had to spare, not in a key's.
        let text = with_mark
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<String>>()
            .join("\n");
        for (key, _) in KEYS {
            assert!(
                text.contains(*key),
                "the mark took a key line: {key:?} is missing"
            );
        }
        assert!(
            text.contains("close"),
            "the mark took the close hint's line"
        );
    }
}
