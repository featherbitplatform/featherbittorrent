//! The pane under the table: what the selected torrent is made of, one tab at a time.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState};

use fbtt_core::{
    Detail, DetailTab, FilePriority, FileRow, PeerRow, RowState, Snapshot, TorrentRow, TrackerRow,
};

use crate::format;
use crate::model::Model;
use crate::theme;
use crate::view;

/// The tabs, in the order `Tab` walks them, with the word drawn for each.
const TABS: [(DetailTab, &str); 4] = [
    (DetailTab::Files, "Files"),
    (DetailTab::Peers, "Peers"),
    (DetailTab::Trackers, "Trackers"),
    (DetailTab::Pieces, "Pieces"),
];
/// Room between tab words.
const TAB_GAP: &str = "  ";

/// Cells of a bar in the file list at its widest.
const WIDEST_FILE_BAR: u16 = 12;
/// A percent beside such a bar, and beside a peer's or a file's progress anywhere else.
const PERCENT_CELLS: u16 = 4;
/// The longest file priority word, `normal`.
const PRIORITY_COLUMN: u16 = 6;
/// A rate in the pane, as in the table.
const RATE_COLUMN: u16 = 11;
/// How much of a peer has arrived, as `100%` or a dash.
const HAVE_COLUMN: u16 = 5;
/// A tracker's tier, which is a small number.
const TIER_COLUMN: u16 = 4;
/// Seeders and leechers together, as `12/34`.
const SWARM_COLUMN: u16 = 11;
/// Room between columns in the pane's lists, one cell rather than the table's two: these lists
/// are read a line at a time, and a pane is usually narrower than the table above it.
const COLUMN_SPACING: u16 = 1;

/// Draws the tabs line, what it says about the torrent beside it, and the pane's content.
pub(crate) fn draw(frame: &mut Frame, area: Rect, model: &Model, snap: &Snapshot) {
    if area.is_empty() {
        return;
    }

    let [tabs_area, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let tabs = tabs_line(model.detail_tab);
    // The tabs on the left of that line, and the torrent they are about on the right, so the
    // pane never shows a file list for a torrent nobody asked about. The gap between them is
    // laid out rather than written into either side, so a name long enough to reach the tabs
    // stops at its own edge instead of running into them.
    let tabs_width = u16::try_from(tabs.width()).unwrap_or(area.width);
    let [tabs_left, tabs_right] =
        Layout::horizontal([Constraint::Length(tabs_width), Constraint::Fill(1)])
            .spacing(COLUMN_SPACING)
            .areas(tabs_area);
    frame.render_widget(Paragraph::new(tabs), tabs_left);
    frame.render_widget(
        Paragraph::new(summary_line(model, snap)).alignment(Alignment::Right),
        tabs_right,
    );

    content(frame, body, model, snap);
}

/// The tab words, the active one in the lifted accent so it is obvious which pane is up.
fn tabs_line(active: DetailTab) -> Line<'static> {
    let mut spans = Vec::with_capacity(TABS.len() * 2);
    for (index, (tab, label)) in TABS.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(TAB_GAP));
        }
        let style = if *tab == active {
            Style::new().fg(theme::VIOLET_400)
        } else {
            Style::new().fg(theme::INK_400)
        };
        spans.push(Span::styled(*label, style));
    }
    Line::from(spans)
}

/// What the pane is describing: the torrent's name and size, and why it stopped when it
/// stopped for a reason. Whatever does not fit beside the tabs is cut from the end, so the
/// name survives and the figures beside it are what go.
fn summary_line(model: &Model, snap: &Snapshot) -> Line<'static> {
    let muted = Style::new().fg(theme::INK_400);
    let Some(row) = selected_row(model, snap) else {
        return Line::from(Span::styled("no torrent selected", muted));
    };

    let mut spans = vec![
        Span::styled(format::safe(&row.name), muted),
        Span::styled(format!(" · {}", format::size(row.size)), muted),
    ];
    if let Some(error) = &row.error {
        spans.push(Span::styled(
            format!(" · {}", format::safe(error)),
            Style::new().fg(theme::RED_500),
        ));
    }
    Line::from(spans)
}

/// The pane's content, or one line saying why there is none.
fn content(frame: &mut Frame, area: Rect, model: &Model, snap: &Snapshot) {
    if area.is_empty() {
        return;
    }
    // A library with nothing in it and a library where nothing is chosen are different things:
    // one needs the add key, the other only an arrow key.
    let Some(row) = selected_row(model, snap) else {
        let note = if snap.torrents.is_empty() {
            "no torrents yet"
        } else {
            "nothing selected"
        };
        draw_note(frame, area, note);
        return;
    };

    // What is drawn is whatever the backend is keeping fresh for the selected torrent. While
    // it cannot answer (its metadata has not arrived, or it was just removed), say so rather
    // than draw an empty list that reads as a torrent with no files.
    let watched = match &snap.detail {
        Some((id, detail)) if *id == row.id => Some(detail),
        _ => None,
    };
    match (model.detail_tab, watched) {
        (DetailTab::Files, Some(Detail::Files(files))) => files_table(frame, area, row, files),
        (DetailTab::Peers, Some(Detail::Peers(peers))) => peers_table(frame, area, peers),
        (DetailTab::Trackers, Some(Detail::Trackers(trackers))) => {
            trackers_table(frame, area, trackers);
        }
        (DetailTab::Pieces, Some(Detail::Pieces { have })) => pieces(frame, area, row, have),
        (_, None) => draw_note(frame, area, note_for(row.state)),
        // A pane that got an answer for a different tab than the one asked for: the next
        // snapshot will fix it, so there is nothing to show this frame.
        _ => draw_note(frame, area, "nothing to show yet"),
    }
}

/// The selected torrent's row in the snapshot, or `None` when nothing is selected.
fn selected_row<'a>(model: &Model, snap: &'a Snapshot) -> Option<&'a TorrentRow> {
    let id = model.selected.as_ref()?;
    snap.torrents.iter().find(|row| &row.id == id)
}

/// Why a pane has nothing in it yet.
fn note_for(state: RowState) -> &'static str {
    match state {
        RowState::Metadata => "metadata has not arrived yet",
        _ => "nothing to show yet",
    }
}

/// One muted line in the pane.
fn draw_note(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(
        Paragraph::new(text).style(Style::new().fg(theme::INK_400)),
        area,
    );
}

/// The file list's columns at a given width.
#[derive(Debug, Clone, Copy)]
struct FileShape {
    /// Cells of progress in the bar; zero draws the percent on its own.
    bar: u16,
    /// Whether the priority word is drawn beside the bar.
    priority: bool,
}

impl FileShape {
    /// The cells the bar and its percent take.
    const fn progress_cells(self) -> u16 {
        if self.bar == 0 {
            PERCENT_CELLS
        } else {
            self.bar + 1 + PERCENT_CELLS
        }
    }

    /// Cells this shape wants, including the shortest path worth reading.
    const fn cells_needed(self) -> u16 {
        let mut cells = self.progress_cells();
        // The gap between the path and the bar, plus another when the word is there.
        let mut gaps = 1;
        if self.priority {
            cells += PRIORITY_COLUMN;
            gaps += 1;
        }
        cells + gaps * COLUMN_SPACING + view::NAME_CELLS
    }
}

/// The shapes, richest first. The priority word goes first: until Task 6 puts a focus in this
/// list it is something a person reads about a file they cannot change, and the path is what
/// tells them which file that is. Then the bar shrinks, and last it gives way to its percent.
const FILE_SHAPES: [FileShape; 4] = [
    FileShape {
        bar: WIDEST_FILE_BAR,
        priority: true,
    },
    FileShape {
        bar: WIDEST_FILE_BAR,
        priority: false,
    },
    FileShape {
        bar: 8,
        priority: false,
    },
    FileShape {
        bar: 0,
        priority: false,
    },
];

/// The file list: a path, how much of it is on disk, and what is being downloaded of it.
///
/// Task 6 gives this list a focus of its own so priorities can be changed from the keyboard;
/// until then it shows what fits from the top.
fn files_table(frame: &mut Frame, area: Rect, row: &TorrentRow, files: &[FileRow]) {
    let colour_of = theme::state_colour(row.state);
    let shape = *view::richest_fitting(&FILE_SHAPES, area.width, |shape| shape.cells_needed());

    let body: Vec<Row<'static>> = files
        .iter()
        .take(usize::from(area.height))
        .map(|file| {
            let progress = share(file.done, file.size);
            // A file that is whole is green even while its torrent is still downloading the
            // rest, which is the point of listing them separately.
            let colour = if file.done >= file.size {
                theme::GREEN_500
            } else {
                colour_of
            };
            let percent = view::right(format::percent(progress), PERCENT_CELLS);
            let bar = if shape.bar == 0 {
                Line::from(Span::styled(percent, Style::new().fg(colour)))
            } else {
                let mut spans = view::bar_spans(progress, shape.bar, colour);
                spans.push(Span::raw(format!(" {percent}")));
                Line::from(spans)
            };

            let mut cells = vec![Cell::from(format::safe(&file.path)), Cell::from(bar)];
            if shape.priority {
                cells.push(
                    Cell::from(view::right(
                        priority_label(file.priority).to_string(),
                        PRIORITY_COLUMN,
                    ))
                    .style(Style::new().fg(theme::INK_400)),
                );
            }
            Row::new(cells).style(Style::new().fg(theme::INK_300))
        })
        .collect();

    let mut widths = vec![
        Constraint::Min(1),
        Constraint::Length(shape.progress_cells()),
    ];
    if shape.priority {
        widths.push(Constraint::Length(PRIORITY_COLUMN));
    }

    let table = Table::new(body, widths).column_spacing(COLUMN_SPACING);
    frame.render_stateful_widget(table, area, &mut TableState::default());
}

/// The peer list's columns at a given width.
#[derive(Debug, Clone, Copy)]
struct PeerShape {
    /// What the peer says it is, in whatever room is left after the address.
    client: bool,
    /// What is going out to it.
    up: bool,
    /// How much of the torrent it has.
    have: bool,
}

impl PeerShape {
    /// Cells the columns with a width of their own take, their gaps included. The address and
    /// the client name share whatever that leaves.
    const fn fixed_cells(self) -> u16 {
        let mut cells = RATE_COLUMN;
        // The address and the download rate, plus one column for each optional one.
        let mut columns = 2;
        if self.up {
            cells += RATE_COLUMN;
            columns += 1;
        }
        if self.have {
            cells += HAVE_COLUMN;
            columns += 1;
        }
        if self.client {
            columns += 1;
        }
        cells + (columns - 1) * COLUMN_SPACING
    }

    /// Cells this shape wants, including the shortest address and client name worth reading.
    const fn cells_needed(self) -> u16 {
        let one_name = self.fixed_cells() + view::NAME_CELLS;
        if self.client {
            one_name + view::NAME_CELLS
        } else {
            one_name
        }
    }

    /// The columns: an address capped at half of what the numbers leave, so a forty-character
    /// host does not push the client name into the rates beside it.
    fn widths(self, width: u16) -> Vec<Constraint> {
        let left = width.saturating_sub(self.fixed_cells());
        let mut widths = Vec::new();
        if self.client {
            widths.push(Constraint::Max((left / 2).max(view::NAME_CELLS)));
            widths.push(Constraint::Min(1));
        } else {
            widths.push(Constraint::Min(1));
        }
        widths.push(Constraint::Length(RATE_COLUMN));
        if self.up {
            widths.push(Constraint::Length(RATE_COLUMN));
        }
        if self.have {
            widths.push(Constraint::Length(HAVE_COLUMN));
        }
        widths
    }
}

/// The shapes, richest first. What a peer calls itself says least about it; then how much of
/// the torrent it has; the rate coming in is last to go because it is why the pane is open.
const PEER_SHAPES: [PeerShape; 4] = [
    PeerShape {
        client: true,
        up: true,
        have: true,
    },
    PeerShape {
        client: false,
        up: true,
        have: true,
    },
    PeerShape {
        client: false,
        up: false,
        have: true,
    },
    PeerShape {
        client: false,
        up: false,
        have: false,
    },
];

/// The peers: who is sending and receiving, and how much of the torrent they have.
fn peers_table(frame: &mut Frame, area: Rect, peers: &[PeerRow]) {
    let shape = *view::richest_fitting(&PEER_SHAPES, area.width, |shape| shape.cells_needed());

    let body: Vec<Row<'static>> = peers
        .iter()
        .take(usize::from(area.height.saturating_sub(1)))
        .map(|peer| {
            let mut cells = vec![Cell::from(format::safe(&peer.addr))];
            if shape.client {
                cells.push(Cell::from(format::safe(&peer.client)));
            }
            cells.push(Cell::from(view::right(
                format::rate(peer.down_rate),
                RATE_COLUMN,
            )));
            if shape.up {
                cells.push(Cell::from(view::right(
                    format::rate(peer.up_rate),
                    RATE_COLUMN,
                )));
            }
            if shape.have {
                cells.push(Cell::from(view::right(
                    match peer.progress {
                        Some(progress) => format::percent(progress),
                        // A peer that has not told us its pieces yet: a dash, not a zero,
                        // because 0% would be a claim we have no reason to make.
                        None => "-".to_string(),
                    },
                    HAVE_COLUMN,
                )));
            }
            Row::new(cells).style(Style::new().fg(theme::INK_300))
        })
        .collect();

    // The columns are in the order the widths function lays them out: name, then numbers.
    let mut header = vec![Cell::from("ADDRESS")];
    if shape.client {
        header.insert(1, Cell::from("CLIENT"));
    }
    header.push(Cell::from(view::right("DOWN".to_string(), RATE_COLUMN)));
    if shape.up {
        header.push(Cell::from(view::right("UP".to_string(), RATE_COLUMN)));
    }
    if shape.have {
        header.push(Cell::from(view::right("HAVE".to_string(), HAVE_COLUMN)));
    }

    let table = Table::new(body, shape.widths(area.width))
        .header(Row::new(header).style(Style::new().fg(theme::INK_400)))
        .column_spacing(COLUMN_SPACING);
    frame.render_stateful_widget(table, area, &mut TableState::default());
}

/// The tracker list's columns at a given width.
#[derive(Debug, Clone, Copy)]
struct TrackerShape {
    /// Seeders and leechers from the last reply.
    swarm: bool,
    /// Which tier the tracker is in.
    tier: bool,
}

impl TrackerShape {
    /// Cells the columns with a width of their own take, their gaps included. The URL and the
    /// status share whatever that leaves.
    const fn fixed_cells(self) -> u16 {
        let mut cells = 0;
        // The URL and the status, plus one column for each optional one.
        let mut columns = 2;
        if self.swarm {
            cells += SWARM_COLUMN;
            columns += 1;
        }
        if self.tier {
            cells += TIER_COLUMN;
            columns += 1;
        }
        cells + (columns - 1) * COLUMN_SPACING
    }

    /// Cells this shape wants, including the shortest URL and status worth reading.
    const fn cells_needed(self) -> u16 {
        self.fixed_cells() + 2 * view::NAME_CELLS
    }

    /// The columns, in the order they are drawn: the URL capped at half of what the numbers
    /// leave so a tracker's own reason for refusing an announce gets room to be read.
    fn widths(self, width: u16) -> Vec<Constraint> {
        let left = width.saturating_sub(self.fixed_cells());
        let mut widths = vec![Constraint::Max((left / 2).max(view::NAME_CELLS))];
        if self.tier {
            widths.push(Constraint::Length(TIER_COLUMN));
        }
        widths.push(Constraint::Min(1));
        if self.swarm {
            widths.push(Constraint::Length(SWARM_COLUMN));
        }
        widths
    }
}

/// The shapes, richest first. A tracker's reason for refusing an announce is why the pane is
/// open, so it keeps its room ahead of a swarm count, and a tier is inferable from the order
/// the list is already in.
const TRACKER_SHAPES: [TrackerShape; 3] = [
    TrackerShape {
        swarm: true,
        tier: true,
    },
    TrackerShape {
        swarm: false,
        tier: true,
    },
    TrackerShape {
        swarm: false,
        tier: false,
    },
];

/// The trackers: what the last announce to each one did, and the swarm it reported.
fn trackers_table(frame: &mut Frame, area: Rect, trackers: &[TrackerRow]) {
    let shape = *view::richest_fitting(&TRACKER_SHAPES, area.width, |shape| shape.cells_needed());

    let body: Vec<Row<'static>> = trackers
        .iter()
        .take(usize::from(area.height.saturating_sub(1)))
        .map(|tracker| {
            // `ok` is what a tracker that accepted the announce says; anything else is that
            // tracker's own reason, and worth seeing in the error colour.
            let status_style = if tracker.status == "ok" {
                Style::new().fg(theme::INK_300)
            } else {
                Style::new().fg(theme::RED_500)
            };
            let mut cells = vec![Cell::from(format::safe(&tracker.url))];
            if shape.tier {
                cells.push(Cell::from(view::right(
                    tracker.tier.to_string(),
                    TIER_COLUMN,
                )));
            }
            cells.push(Cell::from(Line::from(Span::styled(
                format::safe(&tracker.status),
                status_style,
            ))));
            if shape.swarm {
                cells.push(Cell::from(view::right(
                    format!("{}/{}", count(tracker.seeders), count(tracker.leechers)),
                    SWARM_COLUMN,
                )));
            }
            Row::new(cells).style(Style::new().fg(theme::INK_300))
        })
        .collect();

    let mut header = vec![Cell::from("TRACKER")];
    if shape.tier {
        header.push(Cell::from(view::right("TIER".to_string(), TIER_COLUMN)));
    }
    header.push(Cell::from("STATUS"));
    if shape.swarm {
        header.push(Cell::from(view::right(
            "SEEDS/LEECH".to_string(),
            SWARM_COLUMN,
        )));
    }

    let table = Table::new(body, shape.widths(area.width))
        .header(Row::new(header).style(Style::new().fg(theme::INK_400)))
        .column_spacing(COLUMN_SPACING);
    frame.render_stateful_widget(table, area, &mut TableState::default());
}

/// The piece bitmap: one cell per verified run of pieces.
///
/// A pane cannot hold one cell per piece for a torrent with ten thousand of them, so each cell
/// stands for as many pieces as it takes to fit, and is only filled when every piece in that
/// run is on disk. A cell that is empty might mean half of its run is there; the count above it
/// is the exact number.
fn pieces(frame: &mut Frame, area: Rect, row: &TorrentRow, have: &[bool]) {
    let verified = have.iter().filter(|have| **have).count();
    let [counts_area, grid] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let counts = if have.is_empty() {
        "no pieces yet".to_string()
    } else {
        format!(
            "{} of {} pieces verified · {}",
            verified,
            have.len(),
            format::percent(share(verified as u64, have.len() as u64))
        )
    };
    frame.render_widget(
        Paragraph::new(counts).style(Style::new().fg(theme::INK_400)),
        counts_area,
    );

    let per_line = usize::from(grid.width);
    let lines = usize::from(grid.height);
    if per_line == 0 || lines == 0 || have.is_empty() {
        return;
    }

    let cells = per_line * lines;
    let per_cell = have.len().div_ceil(cells).max(1);
    let colour = theme::state_colour(row.state);
    let empty = Style::new().fg(theme::INK_600);

    let mut drawn: Vec<Line<'static>> = Vec::with_capacity(lines);
    for line in 0..lines {
        let start = line * per_line * per_cell;
        if start >= have.len() {
            break;
        }
        let mut spans = Vec::with_capacity(per_line);
        for cell in 0..per_line {
            let at = start + cell * per_cell;
            if at >= have.len() {
                break;
            }
            let run = &have[at..(at + per_cell).min(have.len())];
            if run.iter().all(|have| *have) {
                spans.push(Span::styled("█", Style::new().fg(colour)));
            } else {
                spans.push(Span::styled("░", empty));
            }
        }
        drawn.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(drawn), grid);
}

/// `done` of `size`, as a fraction, with an empty thing counted as whole.
fn share(done: u64, size: u64) -> f64 {
    if size == 0 {
        1.0
    } else {
        done as f64 / size as f64
    }
}

/// What a file priority is called in the list.
fn priority_label(priority: FilePriority) -> &'static str {
    match priority {
        FilePriority::Skip => "skip",
        FilePriority::Low => "low",
        FilePriority::Normal => "normal",
        FilePriority::High => "high",
    }
}

/// A count a tracker may not have reported: a dash rather than a zero.
fn count(value: Option<u32>) -> String {
    match value {
        Some(count) => count.to_string(),
        None => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_priority_word_fits_its_column() {
        for priority in [
            FilePriority::Skip,
            FilePriority::Low,
            FilePriority::Normal,
            FilePriority::High,
        ] {
            let label = priority_label(priority);
            assert!(
                label.len() <= usize::from(PRIORITY_COLUMN),
                "{label} is too wide"
            );
        }
    }

    #[test]
    fn every_pane_keeps_two_readable_columns_at_its_narrowest() {
        // Each list gives up columns until two readable ones are left, and from that width up
        // the shape it picks really has room for what it draws.
        for width in 26..=200u16 {
            let files = *view::richest_fitting(&FILE_SHAPES, width, |shape| shape.cells_needed());
            assert!(
                files.cells_needed() <= width || matches!(files.bar, 0) && !files.priority,
                "the file list at {width} columns is out of room"
            );
            let peers = *view::richest_fitting(&PEER_SHAPES, width, |shape| shape.cells_needed());
            assert!(
                peers.cells_needed() <= width || !(peers.up || peers.have || peers.client),
                "the peer list at {width} columns is out of room"
            );
            let trackers =
                *view::richest_fitting(&TRACKER_SHAPES, width, |shape| shape.cells_needed());
            assert!(
                trackers.cells_needed() <= width || !(trackers.swarm || trackers.tier),
                "the tracker list at {width} columns is out of room"
            );
        }
    }

    #[test]
    fn a_wide_pane_shares_itself_between_a_name_and_its_numbers() {
        // The address and the URL are capped so the second name in each list gets room too: an
        // IPv6 host used to take the whole pane and push the client name against the rates.
        let peers = PEER_SHAPES[0].widths(100);
        assert!(matches!(peers[0], Constraint::Max(width) if width <= 40));
        assert!(matches!(peers[1], Constraint::Min(_)));

        let trackers = TRACKER_SHAPES[0].widths(100);
        assert!(matches!(trackers[0], Constraint::Max(width) if width <= 50));
        assert!(matches!(trackers[2], Constraint::Min(_)));
    }

    #[test]
    fn an_empty_thing_counts_as_whole() {
        assert_eq!(share(0, 0), 1.0);
        assert_eq!(share(50, 100), 0.5);
        assert_eq!(count(None), "-");
        assert_eq!(count(Some(3)), "3");
    }

    #[test]
    fn the_tab_list_covers_every_pane_once() {
        // The pane and the key that walks it have to agree, or Tab shows the wrong list.
        let every = [
            DetailTab::Files,
            DetailTab::Peers,
            DetailTab::Trackers,
            DetailTab::Pieces,
        ];
        for tab in every {
            assert_eq!(
                TABS.iter().filter(|(listed, _)| *listed == tab).count(),
                1,
                "a pane is missing from the tabs line or listed twice"
            );
        }
    }
}
