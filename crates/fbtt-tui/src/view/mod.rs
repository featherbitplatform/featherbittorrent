//! Where each part of a screen goes, and what draws it.
//!
//! The lines are divided up here rather than by each widget asking for what it wants, so
//! that a terminal four rows tall gets a header and some torrents instead of five
//! overlapping panels nobody can read. A tall terminal gets the layout the design draws:
//! three header lines, the table, a rule, the detail pane, a rule and the key hints. What is
//! given up first as the terminal shrinks is written down in `full_areas`, and it is always
//! something other than a row of torrents until there is nothing else left to give.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Span;
use ratatui::widgets::Fill;

use fbtt_core::{RowState, Snapshot};

use crate::format;
use crate::model::{Model, View};
use crate::theme;

mod detail;
mod footer;
mod header;
mod minimal;
mod table;

/// The header's lines when there is room for them: the title and version, the connection,
/// and what is moving. A terminal shorter than [`TALL_HEIGHT`] keeps one line, which says
/// what this screen is and what is moving through it.
const HEADER_LINES: u16 = 3;
/// Below this many rows the header gives up two of its lines to the torrents.
const TALL_HEIGHT: u16 = 8;
/// A rule between two parts of the screen.
const RULE_LINES: u16 = 1;
/// The key hints along the bottom.
const FOOTER_LINES: u16 = 1;
/// The detail pane's preferred floor: its tabs and five lines of content.
const PREFERRED_DETAIL_LINES: u16 = 6;
/// Fewer lines than this and the pane is not drawn at all: a tabs line with two rows under
/// it says less about the torrent than the row in the table already does.
const MIN_DETAIL_LINES: u16 = 4;
/// What the table keeps for itself before the detail pane may take anything: its header row
/// and two torrents.
const MIN_TABLE_LINES: u16 = 3;

/// The parts of the full view, top to bottom.
#[derive(Debug)]
struct Areas {
    header: Rect,
    table: Rect,
    rule_top: Rect,
    detail: Rect,
    rule_bottom: Rect,
    footer: Rect,
}

/// Draws the whole screen into `frame`: the model says what it wants shown, the snapshot
/// says what is true, and neither of them is waited on.
pub fn draw(frame: &mut Frame, model: &Model, snap: &Snapshot) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }

    // The canvas goes down first. A terminal with a light default background would show
    // through every cell the widgets leave empty otherwise.
    frame.render_widget(Fill::new(" ").bg(theme::INK_950), area);

    match model.view {
        View::Full => draw_full(frame, area, model, snap),
        View::Minimal => minimal::draw(frame, area, model, snap),
    }

    // The dialog is drawn last so that it covers what it is meant to cover, and over either
    // view: the box a person gets for `a` in minimal mode is the same one they get anywhere.
    crate::modal::draw(frame, model);
}

fn draw_full(frame: &mut Frame, area: Rect, model: &Model, snap: &Snapshot) {
    let areas = full_areas(area);
    header::draw(frame, areas.header, snap);
    table::draw(frame, areas.table, model, snap);
    frame.render_widget(rule(), areas.rule_top);
    detail::draw(frame, areas.detail, model, snap);
    frame.render_widget(rule(), areas.rule_bottom);
    footer::draw(frame, areas.footer, model);
}

/// A rule: one line of the border colour.
fn rule() -> Fill<'static> {
    Fill::new("─").fg(theme::INK_600)
}

/// Divides `area` into the full view's six parts.
///
/// A tall terminal gets the header's three lines and a detail pane of two fifths of what is
/// between it and the footer, never less than [`PREFERRED_DETAIL_LINES`] when it can have
/// them. A short one gives things up in this order: two of the header's lines at
/// [`TALL_HEIGHT`], the rule above the key hints, the detail pane once it would be smaller
/// than [`MIN_DETAIL_LINES`], the key hints themselves, and only then a torrent.
fn full_areas(area: Rect) -> Areas {
    let height = area.height;
    let header_lines = if height >= TALL_HEIGHT {
        HEADER_LINES
    } else {
        1
    };
    let rest = height.saturating_sub(header_lines);

    // The footer comes off the bottom next, because key hints are worth more than the rule
    // above them and than a detail pane nobody has room to read. At four rows or fewer even
    // they give up, since a screen that shows no torrents is not worth looking at.
    let footer_lines = if rest <= MIN_TABLE_LINES {
        0
    } else {
        rest.min(FOOTER_LINES)
    };
    let inner = rest - footer_lines;
    // That rule gives up its line before the table does.
    let rule_below = u16::from(inner >= MIN_TABLE_LINES + RULE_LINES);
    let inner = inner - rule_below;

    let share = u16::try_from(u32::from(inner) * 2 / 5).unwrap_or(inner);
    let want = share.max(PREFERRED_DETAIL_LINES);
    // The table has to keep its minimum, and the rule between them costs a line too.
    let room = inner.saturating_sub(MIN_TABLE_LINES + RULE_LINES);
    // A pane smaller than this is not worth its rule, so the lines go to the table instead.
    let detail_lines = if want.min(room) >= MIN_DETAIL_LINES {
        want.min(room)
    } else {
        0
    };
    let rule_between = u16::from(detail_lines > 0);

    // Every line is accounted for: `detail_lines` never takes more than `room`, so the
    // table keeps its minimum whenever a detail pane is drawn at all.
    debug_assert!(u32::from(detail_lines) + u32::from(rule_between) <= u32::from(inner));
    let table_lines = inner - detail_lines - rule_between;

    let [header, table, rule_top, detail, rule_bottom, footer] = Layout::vertical([
        Constraint::Length(header_lines),
        Constraint::Length(table_lines),
        Constraint::Length(rule_between),
        Constraint::Length(detail_lines),
        Constraint::Length(rule_below),
        Constraint::Length(footer_lines),
    ])
    .areas(area);

    Areas {
        header,
        table,
        rule_top,
        detail,
        rule_bottom,
        footer,
    }
}

/// The first row of the `height` rows that show `selected`, so a selection at either end of
/// a long list is on screen rather than scrolled out of view.
pub(crate) fn window_start(len: usize, height: u16, selected: Option<usize>) -> usize {
    let Some(selected) = selected else {
        return 0;
    };
    let height = usize::from(height).max(1);
    if selected < height {
        return 0;
    }
    (selected - height + 1).min(len.saturating_sub(1))
}

/// The two spans of a progress bar `width` cells wide: the part that is done in `colour`,
/// the rest as empty blocks in the rule colour.
pub(crate) fn bar_spans(progress: f64, width: u16, colour: Color) -> Vec<Span<'static>> {
    let filled = usize::from(format::filled_cells(progress, width));
    let empty = usize::from(width) - filled;
    vec![
        Span::styled("█".repeat(filled), Style::new().fg(colour)),
        Span::styled("░".repeat(empty), Style::new().fg(theme::INK_600)),
    ]
}

/// Text padded out to `width` cells on the right, so a column of numbers lines up. Text
/// longer than `width` is left as it is and cut by its own column instead of pushing the
/// columns beside it.
pub(crate) fn right(text: String, width: u16) -> String {
    let width = usize::from(width);
    format!("{text:>width$}")
}

/// `cells`, or nothing, depending on whether a shape is drawing that column. Counting what a
/// shape stopped paying for is most of what decides which one a narrow terminal gets.
pub(crate) const fn cost(shown: bool, cells: u16) -> u16 {
    if shown { cells } else { 0 }
}

/// Cells the state word is drawn in, shared by both views so a torrent reads the same way
/// in either of them. It holds the longest label, `downloading`.
pub(crate) const STATE_COLUMN: u16 = 11;

/// Cells a torrent's name needs before it stops being worth reading, which is what decides how
/// many columns either view keeps at a given width. Below this a name shows as a fragment of
/// itself, which still beats a screen full of numbers with no way to tell which torrent they
/// describe.
pub(crate) const NAME_CELLS: u16 = 12;

/// The first shape in `shapes` that fits a terminal `width` cells across, richest first.
///
/// Each view lists its shapes from the one with the most columns to the one with the fewest,
/// and each says how many cells it needs including the names it draws; this takes the first
/// that fits. The leanest shape is the answer when none of them do: by then there is nothing
/// left to give up, and a name cut to three cells still says more than no name at all.
pub(crate) fn richest_fitting<S>(shapes: &[S], width: u16, needed: impl Fn(&S) -> u16) -> &S {
    shapes
        .iter()
        .find(|shape| needed(shape) <= width)
        .unwrap_or_else(|| shapes.last().expect("a view needs at least one shape"))
}

/// What a torrent is doing, as one lower-case word. A checking fraction belongs in its bar,
/// not in the label beside it.
pub(crate) const fn state_label(state: RowState) -> &'static str {
    match state {
        RowState::Metadata => "metadata",
        RowState::Checking(_) => "checking",
        RowState::Queued => "queued",
        RowState::Downloading => "downloading",
        RowState::Stalled => "stalled",
        RowState::Seeding => "seeding",
        RowState::Paused => "paused",
        RowState::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_reads_as_one_word_that_fits_its_column() {
        let states = [
            RowState::Metadata,
            RowState::Checking(0.5),
            RowState::Queued,
            RowState::Downloading,
            RowState::Stalled,
            RowState::Seeding,
            RowState::Paused,
            RowState::Error,
        ];
        for state in states {
            let label = state_label(state);
            assert!(!label.is_empty(), "a state with no label");
            assert!(
                label.len() <= usize::from(STATE_COLUMN),
                "{label} is wider than its column"
            );
            // Every state also has a colour, or the bar beside it would be unreadable.
            let _ = theme::state_colour(state);
        }
    }

    #[test]
    fn a_window_starts_where_the_selection_stops_falling_off_the_top() {
        // A short list needs no scrolling at all.
        assert_eq!(window_start(3, 10, None), 0);
        assert_eq!(window_start(3, 10, Some(2)), 0);
        // A selection below the fold pulls the window down with it, and stops one row short
        // of putting the last row at the top of the table.
        assert_eq!(window_start(100, 10, Some(30)), 21);
        assert_eq!(window_start(100, 10, Some(99)), 90);
        // An empty list has no window.
        assert_eq!(window_start(0, 10, Some(0)), 0);
    }

    #[test]
    fn the_parts_tile_the_screen_at_every_height_and_keep_a_torrent_visible() {
        for height in 1..=60u16 {
            let areas = full_areas(Rect::new(0, 0, 80, height));
            // Nothing overlaps and nothing is left over: every line of the terminal is one of
            // the six parts.
            let mut y = 0;
            for part in [
                areas.header,
                areas.table,
                areas.rule_top,
                areas.detail,
                areas.rule_bottom,
                areas.footer,
            ] {
                assert_eq!(part.y, y, "a gap or an overlap at height {height}");
                assert_eq!(part.width, 80);
                y += part.height;
            }
            assert_eq!(y, height, "lines were lost at height {height}");

            // The header is the design's three lines or the one line that replaces them.
            assert_eq!(
                areas.header.height,
                if height >= TALL_HEIGHT {
                    HEADER_LINES
                } else {
                    1
                },
                "the header took the wrong share at height {height}"
            );
            // A pane is drawn only when it has something to say: its tabs and three rows.
            assert!(
                areas.detail.is_empty() || areas.detail.height >= MIN_DETAIL_LINES,
                "a detail pane of {} lines at height {height}",
                areas.detail.height
            );
            assert_eq!(
                areas.rule_top.is_empty(),
                areas.detail.is_empty(),
                "a rule with no pane at height {height}"
            );
            // From three rows up, the screen is still a torrent list: a header row and at
            // least one torrent under it.
            if height >= 3 {
                assert!(
                    areas.table.height >= 2,
                    "no room for a torrent at height {height}"
                );
            }
        }
    }

    #[test]
    fn a_wide_terminal_gets_the_detail_pane_the_design_draws() {
        let areas = full_areas(Rect::new(0, 0, 120, 30));
        assert_eq!(areas.header.height, HEADER_LINES);
        assert_eq!(areas.footer.height, FOOTER_LINES);
        // Two fifths of the twenty-five lines between them, and the table keeps the rest.
        assert_eq!(areas.detail.height, 10);
        assert_eq!(areas.table.height, 14);
    }

    #[test]
    fn a_bar_never_exceeds_the_width_it_is_given() {
        for progress in [0.0, 0.33, 0.5, 0.62, 1.0, f64::NAN] {
            let spans = bar_spans(progress, 13, theme::VIOLET_500);
            let cells: usize = spans.iter().map(|span| span.width()).sum();
            assert_eq!(cells, 13, "a bar for {progress} is the wrong width");
        }
    }
}
