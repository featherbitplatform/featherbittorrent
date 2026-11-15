//! The torrent table: one row per torrent, and only as many of them as the terminal shows.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Cell, HighlightSpacing, Row, Table, TableState};

use fbtt_core::{RowState, Snapshot, TorrentRow};

use crate::format;
use crate::model::{Model, visible_rows};
use crate::theme;
use crate::view;

/// Cells of progress in the bar at its widest. The design's mockup draws thirteen, which an
/// eighty-column terminal cannot hold beside a readable name, so thirteen is only one of the
/// shapes below.
const WIDEST_BAR: u16 = 13;
/// A percent, right aligned in its own cells beside the bar.
const PERCENT_CELLS: u16 = 4;
/// A rate, which is widest at something like `123.4 MiB/s` or `1023.9 KiB/s`.
const RATE_COLUMN: u16 = 11;
/// Seeds and peers together, as `18/42`.
const PEERS_COLUMN: u16 = 7;
/// Room between columns.
const COLUMN_SPACING: u16 = 2;
/// What stands in the gutter before the selected row. It is always there, so a column does
/// not slide sideways when the selection moves, and it costs the table a cell of width.
const MARKER: &str = "▌";
const MARKER_CELLS: u16 = 1;

/// How the table lays itself out across a terminal of a given width.
#[derive(Debug, Clone, Copy)]
struct Shape {
    /// Cells of progress in the bar. Zero draws the percent on its own, because a number says
    /// exactly what a short row of blocks only suggests.
    bar: u16,
    /// The download rate column.
    down: bool,
    /// The upload rate column.
    up: bool,
    /// The seeds and peers column.
    peers: bool,
}

impl Shape {
    /// Cells the progress column takes: the bar, a space, and the percent. A barless shape
    /// keeps the number alone.
    const fn progress_cells(self) -> u16 {
        if self.bar == 0 {
            PERCENT_CELLS
        } else {
            self.bar + 1 + PERCENT_CELLS
        }
    }

    /// Cells everything but the name takes, its own gaps and the selection gutter included.
    const fn fixed_cells(self) -> u16 {
        let mut cells = self.progress_cells() + view::STATE_COLUMN;
        // The number of gaps is the number of columns beside the name.
        let mut columns = 2;
        if self.down {
            cells += RATE_COLUMN;
            columns += 1;
        }
        if self.up {
            cells += RATE_COLUMN;
            columns += 1;
        }
        if self.peers {
            cells += PEERS_COLUMN;
            columns += 1;
        }
        cells + columns * COLUMN_SPACING + MARKER_CELLS
    }

    /// Cells the name gets, which is what is left of a `width` column terminal. It can go below
    /// [`view::NAME_CELLS`] in a terminal too narrow for even the leanest shape, where it shows
    /// as a fragment rather than disappearing. Only the tests ask: the drawing code gives the
    /// leftover to ratatui and lets it clip.
    #[cfg(test)]
    const fn name_cells(self, width: u16) -> u16 {
        width.saturating_sub(self.fixed_cells())
    }

    /// Cells this shape wants, its one name included.
    const fn cells_needed(self) -> u16 {
        self.fixed_cells() + view::NAME_CELLS
    }
}

/// The shapes, richest first. The order they give columns up is the argument: an upload rate
/// says least about a torrent that is still missing pieces, then who is in the swarm, then the
/// download rate; after those, the bar shrinks, and last it gives way to its number.
const SHAPES: [Shape; 7] = [
    Shape {
        bar: WIDEST_BAR,
        down: true,
        up: true,
        peers: true,
    },
    Shape {
        bar: 10,
        down: true,
        up: true,
        peers: true,
    },
    Shape {
        bar: 10,
        down: true,
        up: false,
        peers: true,
    },
    Shape {
        bar: 10,
        down: true,
        up: false,
        peers: false,
    },
    Shape {
        bar: 6,
        down: true,
        up: false,
        peers: false,
    },
    Shape {
        bar: 0,
        down: true,
        up: false,
        peers: false,
    },
    Shape {
        bar: 0,
        down: false,
        up: false,
        peers: false,
    },
];

/// The richest shape a terminal `width` cells across can show a name in. Below the last shape
/// nothing more can be given up, so the name takes whatever is left.
fn shape_for(width: u16) -> Shape {
    *view::richest_fitting(&SHAPES, width, |shape| shape.cells_needed())
}

/// Draws the table into `area`: the header row plus the torrents that fit under it.
pub(crate) fn draw(frame: &mut Frame, area: Rect, model: &Model, snap: &Snapshot) {
    let rows = visible_rows(model, snap);
    let selected_at = model.selected_index(&rows);
    let shape = shape_for(area.width);

    // One line is the header; the rest are torrents. Only those rows are built at all, so a
    // library of a thousand torrents does not allocate a thousand rows thirty times a second.
    let height = usize::from(area.height.saturating_sub(1));
    let start = view::window_start(rows.len(), area.height.saturating_sub(1), selected_at);
    let end = (start + height).min(rows.len());
    let window = &rows[start..end];

    let body: Vec<Row<'static>> = window
        .iter()
        .map(|row| torrent_row(row, model.selected.as_ref() == Some(&row.id), shape))
        .collect();

    // The table is given only the rows it can draw, so its own offset stays at zero and the
    // selection is counted from the first row of the window.
    let mut state = TableState::default();
    state.select(selected_at.and_then(|at| at.checked_sub(start)));

    let table = Table::new(body, column_widths(shape))
        .header(header_row(shape))
        .column_spacing(COLUMN_SPACING)
        .highlight_symbol(Text::from(Line::from(Span::styled(
            MARKER,
            Style::new().fg(theme::VIOLET_500),
        ))))
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(table, area, &mut state);
}

/// The columns: the name takes whatever the rest leave. A wide terminal shows a whole name and
/// a narrow one cuts it, rather than hiding the numbers nobody can read without them.
fn column_widths(shape: Shape) -> Vec<Constraint> {
    // A terminal too narrow for even the leanest shape gives the name one cell rather than
    // none, so a row always says which torrent it is.
    let mut widths = vec![
        Constraint::Min(1),
        Constraint::Length(shape.progress_cells()),
    ];
    if shape.down {
        widths.push(Constraint::Length(RATE_COLUMN));
    }
    if shape.up {
        widths.push(Constraint::Length(RATE_COLUMN));
    }
    if shape.peers {
        widths.push(Constraint::Length(PEERS_COLUMN));
    }
    widths.push(Constraint::Length(view::STATE_COLUMN));
    widths
}

/// The column titles, which are the columns the shape kept. They are upper case and muted: the
/// table's own shape says what the columns are, so the words only have to confirm it. A barless
/// shape calls the progress column `%`, because all it holds is the number.
fn header_row(shape: Shape) -> Row<'static> {
    let mut cells = vec![
        Cell::from("NAME"),
        Cell::from(if shape.bar == 0 {
            view::right("%".to_string(), shape.progress_cells())
        } else {
            "PROGRESS".to_string()
        }),
    ];
    if shape.down {
        cells.push(Cell::from(view::right("DOWN".to_string(), RATE_COLUMN)));
    }
    if shape.up {
        cells.push(Cell::from(view::right("UP".to_string(), RATE_COLUMN)));
    }
    if shape.peers {
        cells.push(Cell::from(view::right("PEERS".to_string(), PEERS_COLUMN)));
    }
    cells.push(Cell::from(view::right(
        "STATE".to_string(),
        view::STATE_COLUMN,
    )));
    Row::new(cells).style(Style::new().fg(theme::INK_400))
}

/// One torrent: its name, how far it has got, what is moving, who it is talking to, and what
/// it is doing. The selected row is lifted onto the surface colour with strong text; the parts
/// that carry their own meaning keep their own colour through it.
fn torrent_row(row: &TorrentRow, selected: bool, shape: Shape) -> Row<'static> {
    let colour = theme::state_colour(row.state);
    // A torrent that is hash-checking reports its fraction in the state rather than in
    // `progress`, which for one is about to be checked over again says nothing useful.
    let fraction = match row.state {
        RowState::Checking(part) => part,
        _ => row.progress,
    };

    let style = if selected {
        Style::new().bg(theme::INK_850).fg(theme::INK_50)
    } else {
        Style::new().fg(theme::INK_300)
    };

    let percent = view::right(format::percent(fraction), PERCENT_CELLS);
    // Without room for the blocks, the number carries the state colour instead of the bar.
    let progress = if shape.bar == 0 {
        Line::from(Span::styled(percent, Style::new().fg(colour)))
    } else {
        let mut spans = view::bar_spans(fraction, shape.bar, colour);
        spans.push(Span::raw(format!(" {percent}")));
        Line::from(spans)
    };

    let mut cells = vec![Cell::from(format::safe(&row.name)), Cell::from(progress)];
    if shape.down {
        cells.push(Cell::from(view::right(
            format::rate(row.down_rate),
            RATE_COLUMN,
        )));
    }
    if shape.up {
        cells.push(Cell::from(view::right(
            format::rate(row.up_rate),
            RATE_COLUMN,
        )));
    }
    if shape.peers {
        cells.push(Cell::from(view::right(
            format!("{}/{}", row.seeds, row.peers),
            PEERS_COLUMN,
        )));
    }
    cells.push(Cell::from(Line::from(Span::styled(
        view::state_label(row.state),
        Style::new().fg(colour),
    ))));

    Row::new(cells).style(style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_stays_readable_at_every_terminal_width() {
        // From the narrowest terminal worth laying out at all, some shape keeps the name its
        // minimum; below that the shapes are exhausted and only clipping is left.
        for width in 32..=200u16 {
            let name = shape_for(width).name_cells(width);
            assert!(
                name >= view::NAME_CELLS,
                "a name only gets {name} cells at {width} columns"
            );
        }
        // The richest shape is for a terminal wide enough to be called one, and the design's
        // thirteen-cell bar is not what an eighty-column terminal gets.
        assert_eq!(shape_for(120).bar, WIDEST_BAR);
        assert_eq!(shape_for(80).bar, 10);
        assert!(!shape_for(64).up);
    }

    #[test]
    fn each_shape_gives_up_less_than_the_one_after_it() {
        // The ladder has to actually shrink, or `shape_for` would hand a narrow terminal the
        // same columns as a wide one and clip the name to nothing.
        for pair in SHAPES.windows(2) {
            assert!(
                pair[0].fixed_cells() > pair[1].fixed_cells(),
                "{:?} is no wider than {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn a_shape_counts_the_cells_it_really_draws() {
        // The arithmetic in `fixed_cells` is the difference between a name that fits and one
        // that does not, so it is checked against one shape worked out by hand: the bar, its
        // percent, two rates, seeds and peers, the state word, five gaps and the gutter.
        let shape = SHAPES[1];
        assert_eq!(shape.progress_cells(), 15);
        assert_eq!(shape.fixed_cells(), 15 + 11 + 11 + 7 + 11 + 10 + 1);
        assert_eq!(shape.name_cells(100), 34);
    }
}
