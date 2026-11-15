//! The lines above the table: what this is, what is running the torrents, and what is
//! moving right now.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use fbtt_core::{Connection, Snapshot};

use crate::format;
use crate::theme;

/// What the program calls itself, which is also the name of the binary.
const APP_NAME: &str = "featherbittorrent";

/// The title and its version, for the first line of the full view. The logo that replaces
/// it in a terminal that draws images arrives with milestone 3.
fn title_line() -> Line<'static> {
    Line::from(vec![
        title_span(),
        Span::styled(
            format!(" {}", env!("CARGO_PKG_VERSION")),
            Style::new().fg(theme::INK_400),
        ),
    ])
}

/// The title on its own, for the single line the minimal view has room for.
fn title_span() -> Span<'static> {
    Span::styled(
        APP_NAME,
        Style::new().fg(theme::INK_50).add_modifier(Modifier::BOLD),
    )
}

/// What is running the torrents: the engine in this process, or a server reached over the
/// network with milestone 2. The name came from that server, so it goes through `safe`.
fn connection_line(snap: &Snapshot) -> Line<'static> {
    let muted = Style::new().fg(theme::INK_400);
    let mut spans = vec![Span::styled("connected to ", muted)];
    match &snap.connection {
        Connection::Local => spans.push(Span::styled("Local", Style::new().fg(theme::CYAN_400))),
        Connection::Remote { name, secure, auth } => {
            spans.push(Span::styled(
                format::safe(name),
                Style::new().fg(theme::CYAN_400),
            ));
            let scheme = if *secure { "https" } else { "http" };
            spans.push(Span::styled(
                format!(" (remote · {scheme} · {})", format::safe(auth)),
                muted,
            ));
        }
    }
    Line::from(spans)
}

/// The total rates: the arrow and its unit muted, the number itself strong, so the eye
/// lands on the figure and not on the decoration around it.
fn rate_spans(snap: &Snapshot) -> Vec<Span<'static>> {
    let muted = Style::new().fg(theme::INK_400);
    let strong = Style::new().fg(theme::INK_50);
    vec![
        Span::styled("↓ ", muted),
        Span::styled(format::rate(snap.totals.down_rate), strong),
        Span::styled("  ↑ ", muted),
        Span::styled(format::rate(snap.totals.up_rate), strong),
    ]
}

/// The rates, plus how many DHT nodes are known and how many torrents the library holds.
/// A host with no DHT running gets no node count rather than a count of nothing.
fn totals_line(snap: &Snapshot) -> Line<'static> {
    let muted = Style::new().fg(theme::INK_400);
    let mut spans = rate_spans(snap);
    let counts = match snap.totals.dht_nodes {
        Some(nodes) => format!("  DHT {nodes} nodes · {} torrents", snap.torrents.len()),
        None => format!("  {} torrents", snap.torrents.len()),
    };
    spans.push(Span::styled(counts, muted));
    Line::from(spans)
}

/// The full view's header: the design's three lines on a tall terminal, and on a short one
/// whatever of them still matters. A single line carries the name and the rates, because what
/// is moving says more than the words around it; two lines drop the connection, which nobody
/// changes from this screen.
pub(crate) fn draw(frame: &mut Frame, area: Rect, snap: &Snapshot) {
    let lines: Vec<Line<'static>> = match area.height {
        0 => return,
        1 => vec![fallback_line(snap)],
        2 => vec![title_line(), totals_line(snap)],
        _ => vec![title_line(), connection_line(snap), totals_line(snap)],
    };
    frame.render_widget(Paragraph::new(lines), area);
}

/// The name and the rates on one line: the header both views fall back to.
fn fallback_line(snap: &Snapshot) -> Line<'static> {
    let mut spans = vec![title_span(), Span::raw("  ")];
    spans.extend(rate_spans(snap));
    Line::from(spans)
}

/// Below this many columns the minimal view drops its own title, because the rates are the
/// reason a person put a terminal in that corner and the name is what gets cut in half.
const MINIMAL_TITLE_WIDTH: u16 = 26;

/// The minimal view's header: one line, with the name left out where it would not fit.
pub(crate) fn draw_minimal(frame: &mut Frame, area: Rect, snap: &Snapshot) {
    let line = if area.width >= MINIMAL_TITLE_WIDTH {
        fallback_line(snap)
    } else {
        Line::from(rate_spans(snap))
    };
    frame.render_widget(Paragraph::new(line), area);
}
