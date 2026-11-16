//! Minimal mode: one line per torrent, for a corner of the screen or a narrow split.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Row, Table, TableState};

use fbtt_core::{RowState, Snapshot, TorrentRow};

use crate::format;
use crate::model::{Model, visible_rows};
use crate::theme;
use crate::view;

/// Cells of the bar at its widest: the width the design fixes for this view, wide enough to
/// read a percentage off without looking at the number beside it.
const WIDEST_BAR: u16 = 24;
/// A percent beside the bar.
const PERCENT_CELLS: u16 = 4;
/// Room between the parts of a line.
const COLUMN_SPACING: u16 = 1;

/// How a line is laid out across a corner of a given width.
#[derive(Debug, Clone, Copy)]
struct Shape {
    /// Cells of progress in the bar. Zero draws the percent on its own.
    bar: u16,
    /// The state word, which a narrow corner gives up first: the line is already coloured by
    /// state, so dropping the word does not drop the information.
    state: bool,
}

impl Shape {
    /// Cells everything but the name takes, its gap included.
    const fn fixed_cells(self) -> u16 {
        let progress = if self.bar == 0 {
            PERCENT_CELLS
        } else {
            self.bar + 1 + PERCENT_CELLS
        };
        let words = view::cost(self.state, view::STATE_COLUMN);
        // One gap beside the name, and another before the state word when it is drawn.
        let gaps = COLUMN_SPACING + view::cost(self.state, COLUMN_SPACING);
        progress + words + gaps
    }

    /// Cells the name gets out of a corner `width` cells across. Only the tests ask: the drawing
    /// code gives the leftover to ratatui and lets it clip.
    #[cfg(test)]
    const fn name_cells(self, width: u16) -> u16 {
        width.saturating_sub(self.fixed_cells())
    }

    /// Cells this shape wants, its one name included.
    const fn cells_needed(self) -> u16 {
        self.fixed_cells() + view::NAME_CELLS
    }
}

/// The shapes, richest first: the bar shrinks until it is too short to read as a bar, then the
/// state word goes, and last the bar gives way to its number.
const SHAPES: [Shape; 6] = [
    Shape {
        bar: WIDEST_BAR,
        state: true,
    },
    Shape {
        bar: 18,
        state: true,
    },
    Shape {
        bar: 12,
        state: true,
    },
    Shape {
        bar: 8,
        state: true,
    },
    Shape {
        bar: 8,
        state: false,
    },
    Shape {
        bar: 0,
        state: false,
    },
];

/// The richest shape a corner `width` cells across can show a name in.
fn shape_for(width: u16) -> Shape {
    *view::richest_fitting(&SHAPES, width, |shape| shape.cells_needed())
}

/// Draws the one-line header and a line per torrent that fits under it.
pub(crate) fn draw(frame: &mut Frame, area: Rect, model: &Model, snap: &Snapshot) {
    let [header_area, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    view::header::draw_minimal(frame, header_area, snap);

    let rows = visible_rows(model, snap);
    let selected_at = model.selected_index(&rows);
    let height = usize::from(body.height);
    let start = view::window_start(rows.len(), body.height, selected_at);
    let end = (start + height).min(rows.len());
    let shape = shape_for(area.width);

    let lines: Vec<Row<'static>> = rows[start..end]
        .iter()
        .map(|row| torrent_line(row, model.selected.as_ref() == Some(&row.id), shape))
        .collect();

    // No header row and no marker: this view is a bar, a percent, a name and a state, and the
    // selected line is the one lifted onto the surface colour.
    let table = Table::new(lines, column_widths(shape)).column_spacing(COLUMN_SPACING);
    frame.render_stateful_widget(table, body, &mut TableState::default());
}

/// The columns, in the shape the corner has room for.
fn column_widths(shape: Shape) -> Vec<Constraint> {
    let mut widths = vec![if shape.bar == 0 {
        Constraint::Length(PERCENT_CELLS)
    } else {
        Constraint::Length(shape.bar + 1 + PERCENT_CELLS)
    }];
    // A corner too narrow for even the leanest shape gives the name one cell rather than none.
    widths.push(Constraint::Min(1));
    if shape.state {
        widths.push(Constraint::Length(view::STATE_COLUMN));
    }
    widths
}

/// One torrent on one line.
fn torrent_line(row: &TorrentRow, selected: bool, shape: Shape) -> Row<'static> {
    let colour = theme::state_colour(row.state);
    let fraction = match row.state {
        RowState::Checking(part) => part,
        _ => row.progress,
    };

    let percent = view::right(format::percent(fraction), PERCENT_CELLS);
    let progress = if shape.bar == 0 {
        Line::from(Span::styled(percent, Style::new().fg(colour)))
    } else {
        let mut bar = view::bar_spans(fraction, shape.bar, colour);
        bar.push(Span::raw(format!(" {percent}")));
        Line::from(bar)
    };

    let style = if selected {
        Style::new().bg(theme::INK_850).fg(theme::INK_50)
    } else {
        Style::new().fg(theme::INK_300)
    };

    let mut cells = vec![Cell::from(progress), Cell::from(format::safe(&row.name))];
    if shape.state {
        cells.push(Cell::from(view::state_label(row.state)));
    }
    Row::new(cells).style(style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_narrow_corner_keeps_the_name_and_loses_the_rest() {
        for width in 17..=200u16 {
            let name = shape_for(width).name_cells(width);
            assert!(
                name >= view::NAME_CELLS,
                "a name only gets {name} cells in a {width} column corner"
            );
        }
        // The design's bar is for a wide split; a corner the size of a notebook column reads
        // the name instead of the state word.
        assert_eq!(shape_for(100).bar, WIDEST_BAR);
        assert_eq!(shape_for(48).bar, 18);
        assert!(!shape_for(30).state);
    }

    #[test]
    fn each_shape_gives_up_less_than_the_one_after_it() {
        for pair in SHAPES.windows(2) {
            assert!(
                pair[0].fixed_cells() > pair[1].fixed_cells(),
                "{:?} is no narrower than {:?}",
                pair[0],
                pair[1]
            );
        }
    }
}
