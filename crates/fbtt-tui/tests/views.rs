//! The main screens and the model's keys, tested the way the binary uses them: keys in,
//! actions out, and a buffer to read what a person would have seen.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fbtt_core::{Command, DetailTab, LinkState, RowState, Snapshot, TorrentId};
use fbtt_tui::graphics::caps::{CellPx, Graphics, Protocol};
use fbtt_tui::graphics::kitty;
use fbtt_tui::graphics::place::{LogoSize, LogoView, Placement, PreparedLogos};
use fbtt_tui::{Action, Descending, SortKey, theme, visible_rows};
use ratatui::buffer::CellDiffOption;
use ratatui::style::Color;

mod support;
use support::*;

/// A drawn screen and the marks the frame placed: `fbtt_tui::view::draw_with` the way the binary
/// does, with the prepared logos the terminal it pretends to be.
///
/// `buffer` is the frame's own buffer, not the backend's: ratatui's diff skips the cells the
/// mark covers, so the backend only holds what the frame actually wrote, while the frame's
/// buffer is where the view left its placeholder and skip marks.
struct Drawn<'a> {
    screen: Screen,
    buffer: ratatui::buffer::Buffer,
    placements: Vec<Placement>,
    _logos: &'a PreparedLogos,
}

/// The marks for a terminal whose protocol and cell size a test spells out: the case the binary's
/// blocking pool answers with. `cell` is the device pixel size of one cell.
fn logos(protocol: Protocol, cell: CellPx) -> PreparedLogos {
    fbtt_tui::graphics::prepare(Graphics {
        protocol,
        cell: Some(cell),
        scale: 1,
    })
}

/// A screen drawn with the marks `logos` has. The frame's placements are captured on the way
/// through, the way the binary's draw callback captures them before the frame is handed to the
/// placer.
fn drawn_with<'a>(
    model: &fbtt_tui::Model,
    snap: &Snapshot,
    logos: &'a PreparedLogos,
    width: u16,
    height: u16,
) -> Drawn<'a> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("a test terminal");
    let mut placements = Vec::new();
    let frame = terminal
        .draw(|frame| {
            placements =
                fbtt_tui::view::draw_with(frame, model, snap, LogoView { logos: Some(logos) });
        })
        .expect("a drawn frame");
    // The frame's buffer is the one that carries the view's marks: the diff the backend saw
    // wrote only what changed, and skipped cells are exactly the ones it did not.
    let buffer = frame.buffer.clone();
    let screen = Screen::from_terminal(terminal);
    Drawn {
        screen,
        buffer,
        placements,
        _logos: logos,
    }
}

#[test]
fn full_view_shows_rows_and_header() {
    let snap = library();
    let model = model_of(&snap);
    let screen = Screen::new(&model, &snap, 100, 30);
    let lines = screen.lines();

    // Three header lines: what this is, what it is driven by, and what is moving.
    let header = format!("{} {} {}", lines[0], lines[1], lines[2]);
    assert!(
        header.contains("featherbittorrent"),
        "the title is missing from {header:?}"
    );
    assert!(
        header.contains(env!("CARGO_PKG_VERSION")),
        "the version is missing from {header:?}"
    );
    assert!(header.contains("Local"), "the connection is missing");
    assert!(header.contains("DHT 312 nodes"), "the DHT count is missing");

    let body = screen.text();
    for want in [
        "Big.Buck.Bunny.4K",
        "debian-13.1-amd64.iso",
        "62%",
        "4.2 MiB/s",
        "downloading",
        "seeding",
    ] {
        assert!(body.contains(want), "{want:?} is missing from:\n{body}");
    }

    // The pane under the table belongs to the selected torrent: its tabs and its files.
    for want in ["Files", "Peers", "Trackers", "Pieces"] {
        assert!(body.contains(want), "the tab {want:?} is missing");
    }
    assert!(
        body.contains("bbb_sunflower_2160p_60fps.mp4"),
        "the file list is missing from:\n{body}"
    );
}

#[test]
fn selection_follows_id_after_resort() {
    let snap = library();
    let mut model = model_of(&snap);

    // The first row (Big Buck Bunny) is selected to start with; one step down selects
    // the debian image, which sorts second by queue but first by name.
    assert_eq!(
        model.watch().map(|(id, _)| id),
        Some(snap.torrents[0].id.clone())
    );
    model.handle_key(letter('j'), &snap);
    let debian = snap.torrents[1].id.clone();
    assert_eq!(model.watch().map(|(id, _)| id), Some(debian.clone()));

    // Reversing the sort moves it from second to first. A selection that remembered a row
    // number rather than a torrent would be on Big Buck Bunny now.
    model.handle_key(letter('O'), &snap);
    assert_eq!(model.sort, (SortKey::Queue, Descending(true)));
    let rows = visible_rows(&model, &snap);
    assert_eq!(
        rows[0].id, debian,
        "reversing the sort did not reverse the rows"
    );
    assert_eq!(
        model.watch().map(|(id, _)| id),
        Some(debian.clone()),
        "the selection followed the row instead of the torrent"
    );

    // Cycling to the name column and running it the other way round moves the same torrent
    // back down the table: `big.buck...` sorts before `debian-...`.
    model.handle_key(letter('o'), &snap);
    assert_eq!(model.sort, (SortKey::Name, Descending(true)));
    model.handle_key(letter('O'), &snap);
    assert_eq!(model.sort, (SortKey::Name, Descending(false)));
    let rows = visible_rows(&model, &snap);
    assert_eq!(
        rows[1].id, debian,
        "the name sort put the wrong torrent first"
    );
    assert_eq!(model.watch().map(|(id, _)| id), Some(debian));
}

#[test]
fn filter_hides_rows() {
    let snap = library();
    let mut model = model_of(&snap);
    model.filter = "deb".to_string();
    // The binary keeps the selection pointing at something on screen, which is what a new
    // filter needs too: Big Buck Bunny is hidden now, so the pane follows the torrent that
    // is still listed.
    model.sync_selection(&snap);

    let rows = visible_rows(&model, &snap);
    assert_eq!(rows.len(), 1, "the filter kept the wrong number of rows");
    assert_eq!(rows[0].name, "debian-13.1-amd64.iso");
    assert_eq!(model.watch().map(|(id, _)| id), Some(rows[0].id.clone()));

    let screen = Screen::new(&model, &snap, 100, 30);
    let body = screen.text();
    assert!(body.contains("debian-13.1-amd64.iso"));
    assert!(
        !body.contains("Big.Buck.Bunny.4K"),
        "a filtered-out torrent was drawn:\n{body}"
    );
}

#[test]
fn filter_ignores_case() {
    let snap = library();
    let mut model = model_of(&snap);
    model.filter = "BUCK".to_string();

    let rows = visible_rows(&model, &snap);
    assert_eq!(rows.len(), 1, "an upper case filter matched nothing");
    assert_eq!(rows[0].name, "Big.Buck.Bunny.4K");
}

#[test]
fn minimal_view_toggle() {
    let snap = library();
    let mut model = model_of(&snap);
    let full = Screen::new(&model, &snap, 100, 30);
    assert!(full.text().contains("Peers"), "the tabs line is missing");

    model.handle_key(letter('m'), &snap);
    let minimal = Screen::new(&model, &snap, 100, 30);
    let body = minimal.text();

    // The detail pane and the table header are gone; the bars and the names are not.
    assert!(
        !body.contains("Peers"),
        "the detail tabs are still drawn:\n{body}"
    );
    assert!(
        !body.contains("Trackers"),
        "the detail tabs are still drawn"
    );
    assert!(body.contains('█') || body.contains('░'), "no bar was drawn");
    assert!(body.contains("Big.Buck.Bunny.4K"));
    assert!(body.contains("seeding"));
    assert!(minimal.lines()[0].contains("featherbittorrent"));

    // And back again.
    model.handle_key(letter('m'), &snap);
    assert!(Screen::new(&model, &snap, 100, 30).text().contains("Peers"));
}

#[test]
fn seeding_row_is_green() {
    let snap = library();
    let model = model_of(&snap);
    let screen = Screen::new(&model, &snap, 100, 30);
    // The label is the whole of the state cell that starts it, so its colour is the
    // state's colour and not a bar or a name that happens to be green.
    let (x, y) = screen.find("seeding").expect("no seeding state was drawn");
    assert_eq!(
        screen.fg(x, y),
        Color::Rgb(0x2b, 0xbb, 0x71),
        "seeding is not the design's green"
    );

    // Downloading wears the accent instead.
    let (x, y) = screen.find("downloading").expect("no downloading state");
    assert_eq!(screen.fg(x, y), Color::Rgb(0x86, 0x3b, 0xff));
}

#[test]
fn hostile_names_render_cleanly() {
    let mut snap = library();
    // A bidi override that would reverse everything after it, and a name no column holds.
    snap.torrents[0].name = format!("evil\u{202e}gpj.exe{}", "x".repeat(10_000));
    let model = model_of(&snap);
    let screen = Screen::new(&model, &snap, 80, 24);

    let body = screen.text();
    assert!(
        !body.contains('\u{202e}'),
        "a bidi override reached the screen"
    );

    let (_, y) = screen
        .find("evilgpj.exe")
        .expect("the name was not drawn at all");
    let line = &screen.lines()[y];
    // A long name stops at its own column: the bar, the percent and the state label are
    // still where they belong on the same row.
    assert!(
        line.contains('%'),
        "the percent column was pushed away:\n{line}"
    );
    assert!(
        line.contains("downloading"),
        "the state column was pushed off the row:\n{line}"
    );
    for drawn in screen.lines() {
        assert!(
            drawn.chars().count() <= screen.width(),
            "a row overflowed: {drawn:?}"
        );
    }

    // The override is gone, so the name reads the way it was spelled. Only the selection
    // marker and blank cells may stand in front of it.
    let before = line.find("evilgpj.exe").expect("the name");
    assert!(
        line[..before]
            .chars()
            .all(|cell| cell == '▌' || cell == ' '),
        "something was drawn before the name: {line:?}"
    );
}

#[test]
fn tiny_terminal_does_not_panic() {
    let snap = library();
    for (width, height) in [(20u16, 5u16), (1u16, 1u16)] {
        let mut model = model_of(&snap);
        let screen = Screen::new(&model, &snap, width, height);
        assert_eq!(screen.lines().len(), usize::from(height));

        model.handle_key(letter('m'), &snap);
        let screen = Screen::new(&model, &snap, width, height);
        assert_eq!(screen.lines().len(), usize::from(height));

        // Keys still work when nothing fits.
        model.handle_key(letter('j'), &snap);
        model.handle_key(letter('p'), &snap);
        model.handle_key(letter('O'), &snap);
    }
}

#[test]
fn p_sends_pause_or_resume() {
    let mut snap = library();
    snap.torrents.push(torrent(
        &TorrentId::from_wire_hash([0x33; 20]).0,
        "archlinux-2026.11.iso",
        RowState::Paused,
    ));
    snap.torrents[2].queue_position = 2;

    let mut model = model_of(&snap);
    // The downloading torrent is selected first: `p` stops it.
    let actions = model.handle_key(letter('p'), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Pause(snap.torrents[0].id.clone()))]
    );

    // Two steps down reaches the paused one, where `p` starts it again.
    model.handle_key(letter('j'), &snap);
    model.handle_key(letter('j'), &snap);
    let actions = model.handle_key(letter('p'), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Resume(
            snap.torrents[2].id.clone()
        ))]
    );

    // Nothing selected means nothing to stop.
    let empty = Snapshot {
        torrents: Vec::new(),
        ..library()
    };
    let mut model = model_of(&empty);
    assert_eq!(model.handle_key(letter('p'), &empty), Vec::new());
}

#[test]
fn p_on_queued_pauses_and_on_error_resumes() {
    let mut snap = library();
    // A torrent waiting for its place in the session's queue, and one a storage failure stopped.
    let mut queued = torrent(
        &TorrentId::from_wire_hash([0x33; 20]).0,
        "archlinux-2026.11.iso",
        RowState::Queued,
    );
    queued.queue_position = 2;
    let mut errored = torrent(
        &TorrentId::from_wire_hash([0x44; 20]).0,
        "broken.partition.iso",
        RowState::Error,
    );
    errored.queue_position = 3;
    errored.error = Some("no space left on device".to_string());
    snap.torrents.push(queued);
    snap.torrents.push(errored);

    let mut model = model_of(&snap);

    // Two rows down is the queued one. It has nothing running to lose, and stopping it hands its
    // place in the queue to another torrent, so `p` stops rather than starts.
    model.handle_key(letter('j'), &snap);
    model.handle_key(letter('j'), &snap);
    assert_eq!(
        model.watch().map(|(id, _)| id),
        Some(snap.torrents[2].id.clone()),
        "the cursor did not reach the queued torrent"
    );
    assert_eq!(
        model.handle_key(letter('p'), &snap),
        vec![Action::Command(Command::Pause(snap.torrents[2].id.clone()))],
        "p on a queued torrent did not stop it"
    );

    // One more row down is the errored one. `resume` is what retries it: the engine clears the
    // storage error that stopped it as it starts the torrent again.
    model.handle_key(letter('j'), &snap);
    assert_eq!(
        model.watch().map(|(id, _)| id),
        Some(snap.torrents[3].id.clone()),
        "the cursor did not reach the errored torrent"
    );
    assert_eq!(
        model.handle_key(letter('p'), &snap),
        vec![Action::Command(Command::Resume(
            snap.torrents[3].id.clone()
        ))],
        "p on an errored torrent did not try it again"
    );
}

#[test]
fn quit_asks_twice_when_something_downloads() {
    let snap = library();
    let mut model = model_of(&snap);

    // A torrent is downloading: the first `q` only asks.
    let actions = model.handle_key(letter('q'), &snap);
    assert!(actions.is_empty(), "the first q quit without asking");
    assert!(!model.quit_requested());
    assert!(
        Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("q again"),
        "nothing told the person that q needs pressing again"
    );

    // A second `q` confirms.
    let actions = model.handle_key(letter('q'), &snap);
    assert_eq!(actions, vec![Action::Quit]);
    assert!(model.quit_requested());

    // Any other key cancels the ask instead.
    let mut model = model_of(&snap);
    model.handle_key(letter('q'), &snap);
    model.handle_key(letter('j'), &snap);
    assert!(!model.quit_requested());
    let actions = model.handle_key(letter('q'), &snap);
    assert!(actions.is_empty(), "a cancelled q quit at once");

    // `y` confirms too.
    model.handle_key(letter('y'), &snap);
    assert!(model.quit_requested());

    // Work that has not become bytes on disk yet is still work in progress: waiting for metadata,
    // checking what is on disk, and running with nothing arriving are all worth asking about.
    for state in [
        RowState::Metadata,
        RowState::Checking(0.4),
        RowState::Stalled,
    ] {
        let busy = Snapshot {
            torrents: vec![torrent("a", "work.in.progress.iso", state)],
            ..library()
        };
        let mut model = model_of(&busy);
        assert!(
            model.handle_key(letter('q'), &busy).is_empty(),
            "the first q quit over {state:?} without asking"
        );
        assert_eq!(
            model.handle_key(letter('q'), &busy),
            vec![Action::Quit],
            "the second q did not confirm over {state:?}"
        );
    }

    // With nothing in progress, one `q` is enough: stopped, waiting for a place in the queue,
    // finished, and stopped by a failure each have nothing this run would leave half-done.
    for state in [
        RowState::Paused,
        RowState::Queued,
        RowState::Seeding,
        RowState::Error,
    ] {
        let idle = Snapshot {
            torrents: vec![torrent("a", "quiet.torrent", state)],
            ..library()
        };
        let mut model = model_of(&idle);
        assert_eq!(
            model.handle_key(letter('q'), &idle),
            vec![Action::Quit],
            "one q did not quit a library whose only torrent was {state:?}"
        );
    }
}

#[test]
fn ctrl_c_quits_the_way_q_does() {
    // Ctrl-C is a modified press the map would otherwise drop; it has to mean quit, with
    // the same confirmation `q` asks for while bytes are still coming down.
    let snap = library();
    let mut model = model_of(&snap);
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

    let actions = model.handle_key(ctrl_c, &snap);
    assert!(actions.is_empty(), "the first Ctrl-C quit without asking");
    assert!(!model.quit_requested());
    let actions = model.handle_key(ctrl_c, &snap);
    assert_eq!(actions, vec![Action::Quit]);
    assert!(model.quit_requested());

    // Running with nothing arriving asks too: it is still a download in progress.
    let stalled = Snapshot {
        torrents: vec![torrent("a", "nothing.arriving.iso", RowState::Stalled)],
        ..library()
    };
    let mut model = model_of(&stalled);
    assert!(
        model.handle_key(ctrl_c, &stalled).is_empty(),
        "Ctrl-C quit a stalled download without asking"
    );

    // With nothing downloading it quits at once, and `q` confirms an armed Ctrl-C too.
    let idle = Snapshot {
        torrents: vec![torrent("a", "quiet.torrent", RowState::Paused)],
        ..library()
    };
    let mut model = model_of(&idle);
    assert_eq!(
        model.handle_key(ctrl_c, &idle),
        vec![Action::Quit],
        "Ctrl-C did not quit an idle library"
    );

    let mut model = model_of(&snap);
    model.handle_key(ctrl_c, &snap);
    assert_eq!(
        model.handle_key(letter('y'), &snap),
        vec![Action::Quit],
        "y did not confirm the Ctrl-C ask"
    );
}

#[test]
fn tab_moves_through_the_detail_panes() {
    let snap = library();
    let mut model = model_of(&snap);
    let id = snap.torrents[0].id.clone();
    assert_eq!(model.watch(), Some((id.clone(), DetailTab::Files)));

    model.handle_key(key(KeyCode::Tab), &snap);
    assert_eq!(model.watch(), Some((id.clone(), DetailTab::Peers)));
    model.handle_key(key(KeyCode::Tab), &snap);
    assert_eq!(model.watch(), Some((id.clone(), DetailTab::Trackers)));

    // Shift+Tab walks back, and the ends wrap rather than stop.
    let shift_tab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    model.handle_key(shift_tab, &snap);
    assert_eq!(model.watch(), Some((id.clone(), DetailTab::Peers)));

    let mut model = model_of(&snap);
    for _ in 0..5 {
        model.handle_key(key(KeyCode::Tab), &snap);
    }
    assert_eq!(model.watch(), Some((id, DetailTab::Peers)));
}

#[test]
fn selection_falls_back_when_the_row_disappears() {
    let snap = library();
    let mut model = model_of(&snap);
    model.handle_key(letter('j'), &snap);
    let second = snap.torrents[1].id.clone();
    assert_eq!(model.watch().map(|(id, _)| id), Some(second));

    // The selected torrent goes away: the row now in its place takes the selection.
    let mut next = library();
    next.torrents.remove(1);
    next.torrents.push(torrent(
        &TorrentId::from_wire_hash([0x44; 20]).0,
        "Sintel.2010.1080p",
        RowState::Queued,
    ));
    next.torrents[1].queue_position = 1;
    model.sync_selection(&next);
    assert_eq!(
        model.watch().map(|(id, _)| id),
        Some(next.torrents[1].id.clone())
    );

    // An empty library leaves nothing selected.
    let none = Snapshot {
        torrents: Vec::new(),
        ..library()
    };
    model.sync_selection(&none);
    assert_eq!(model.watch(), None);
}

#[test]
fn sorting_orders_by_the_chosen_column_and_reverses() {
    let mut snap = library();
    snap.torrents.push(torrent(
        &TorrentId::from_wire_hash([0x55; 20]).0,
        "Tears.of.Steel.v2",
        RowState::Stalled,
    ));
    snap.torrents[2].progress = 0.88;
    snap.torrents[2].queue_position = 2;

    let model = model_of(&snap);
    let queue: Vec<String> = visible_rows(&model, &snap)
        .iter()
        .map(|r| r.name.clone())
        .collect();
    assert_eq!(
        queue,
        vec![
            "Big.Buck.Bunny.4K",
            "debian-13.1-amd64.iso",
            "Tears.of.Steel.v2"
        ],
        "the default sort is not the queue order"
    );

    let mut by_progress = model.clone();
    by_progress.sort = (SortKey::Progress, Descending(false));
    let names: Vec<String> = visible_rows(&by_progress, &snap)
        .iter()
        .map(|r| r.name.clone())
        .collect();
    assert_eq!(
        names,
        vec![
            "Big.Buck.Bunny.4K",
            "Tears.of.Steel.v2",
            "debian-13.1-amd64.iso"
        ],
        "the progress sort is wrong"
    );

    // The same column the other way round.
    by_progress.sort = (SortKey::Progress, Descending(true));
    let reversed: Vec<String> = visible_rows(&by_progress, &snap)
        .iter()
        .map(|r| r.name.clone())
        .collect();
    let mut expect = names;
    expect.reverse();
    assert_eq!(
        reversed, expect,
        "reversing the sort did not reverse the rows"
    );

    // `O` is the same thing from a keyboard.
    let mut model = model_of(&snap);
    model.handle_key(letter('o'), &snap); // queue -> name
    model.handle_key(letter('o'), &snap); // name -> progress
    assert_eq!(model.sort, (SortKey::Progress, Descending(false)));
    model.handle_key(letter('O'), &snap);
    assert_eq!(model.sort, (SortKey::Progress, Descending(true)));
}

#[test]
fn a_narrow_terminal_trades_columns_and_not_the_name() {
    // Both views give up columns as the terminal narrows, and what they never trade away is
    // enough of a name to tell one torrent from another. These are the widths where each shape
    // change lands, so a regression in the ladder shows up as a missing prefix.
    let snap = library();
    let names = ["Big.Buck.Bun", "debian-13.1-"];

    for (minimal, width) in [
        (false, 32u16),
        (false, 60),
        (false, 80),
        (true, 17),
        (true, 40),
        (true, 80),
    ] {
        let mut model = model_of(&snap);
        if minimal {
            model.handle_key(letter('m'), &snap);
        }
        let screen = Screen::new(&model, &snap, width, 14);
        let body = screen.text();
        for name in names {
            assert!(
                body.contains(name),
                "at {width} columns the {:?} view cut {:?} short:\n{body}",
                if minimal { "minimal" } else { "full" },
                name
            );
        }
    }
}

#[test]
fn renders_only_visible_rows_with_10k_torrents() {
    // A library this big is what the table is for: only the rows that fit are built and drawn, so
    // one redraw costs the same whatever the backend is holding. Ten thousand names in a screen
    // that shows ten of them would be a list nobody can read and a redraw nobody can afford.
    let mut snap = library();
    snap.torrents = (0..10_000)
        .map(|index| {
            torrent(
                &format!("t{index}"),
                &format!("torrent-{index:05}"),
                RowState::Seeding,
            )
        })
        .collect();
    for (index, row) in snap.torrents.iter_mut().enumerate() {
        row.queue_position = index;
    }
    snap.detail = None;

    let model = model_of(&snap);
    let screen = Screen::new(&model, &snap, 80, 24);
    let body = screen.text();

    // The table's header row plus the ten torrents that fit under it. The eleventh is not there,
    // which is what says a window was cut rather than a long name clipped by its own column.
    assert!(
        body.contains("torrent-00000"),
        "the first row was not drawn:\n{body}"
    );
    assert!(
        body.contains("torrent-00009"),
        "the last row with room was not drawn"
    );
    assert!(
        !body.contains("torrent-00010"),
        "a row below the fold was drawn:\n{body}"
    );
    assert!(
        !body.contains("torrent-09999"),
        "the last of ten thousand rows was drawn:\n{body}"
    );
}

#[test]
fn header_shows_lost_link() {
    // A server that goes away does not stop the screen: the last snapshot stays on it and the header says
    // why. A person who cannot tell a lost link from a slow one reads frozen numbers as a stalled torrent.
    let lost = server(LinkState::Lost { retry_in_secs: 4 });
    let model = model_of(&lost);
    let screen = Screen::new(&model, &lost, 100, 30);
    let text = screen.text();
    assert!(
        text.contains("connection lost, retrying in 4s"),
        "a server that went away was reported as nothing: {text:?}"
    );
    assert!(
        text.contains("bit.example"),
        "the line did not say which server went away: {text:?}"
    );

    // Amber is the one colour on this screen that means "this is not working", so it goes on the fact
    // rather than on the name: what changed is the link, not the server.
    let (column, row) = screen
        .find("connection lost")
        .expect("the header said nothing about the link");
    assert_eq!(
        screen.fg(column, row),
        theme::AMBER_500,
        "a lost link was not drawn as the failure it is"
    );

    // A link still dialling says exactly that, so a table with nothing in it yet reads as waiting rather
    // than as empty, and it still names the server it is trying to reach.
    let connecting = server(LinkState::Connecting);
    let text = Screen::new(&model_of(&connecting), &connecting, 100, 30).text();
    assert!(
        text.contains("connecting... bit.example (http · password)"),
        "a link that has not answered yet said nothing about itself: {text:?}"
    );

    // A held link says what it is signed in with, which is the thing a person checks when the numbers on
    // the screen are not the ones they expected to see.
    let held = server(LinkState::Connected);
    let text = Screen::new(&model_of(&held), &held, 100, 30).text();
    assert!(
        text.contains("connected to bit.example (http · password)"),
        "a remote connection did not say how it is signed in: {text:?}"
    );

    // And this library's own engine still reads as itself: milestone 2 added a second kind of connection
    // without changing the words for the first.
    let local = library();
    let text = Screen::new(&model_of(&local), &local, 100, 30).text();
    assert!(
        text.contains("connected to Local"),
        "the local engine's header changed: {text:?}"
    );
}

/// A terminal with no image protocol at all: the header is the text title, as it has always been.
#[test]
fn no_graphics_shows_text_title() {
    let snap = library();
    let model = model_of(&snap);
    let screen = Screen::new(&model, &snap, 100, 30);
    // The title line is the mark's space, which the text title fills.
    let title_col = screen
        .find("featherbittorrent")
        .expect("the title is drawn");
    assert_eq!(
        title_col.0, 0,
        "no mark is placed, so the title starts the header"
    );
    assert!(
        !screen.text().contains('\u{10EEEE}'),
        "no placeholders on a terminal that cannot draw images"
    );

    // The same for a `PreparedLogos` with `Protocol::None`: a terminal that proved nothing. Zero
    // placements, and the text title where the mark would be.
    let none = PreparedLogos {
        graphics: fbtt_tui::graphics::Graphics::none(),
        small: None,
        large: Vec::new(),
    };
    let drawn = drawn_with(&model, &snap, &none, 100, 30);
    assert!(
        drawn.placements.is_empty(),
        "a terminal with no protocol places no marks: {:?}",
        drawn.placements
    );
    assert!(
        drawn.screen.text().contains("featherbittorrent"),
        "the text title is still drawn"
    );
}

/// A kitty terminal: the header's cells are placeholders, the image the pixels that will be
/// there, and the title stands one column to the right of the mark's blank column.
#[test]
fn kitty_header_cells_are_placeholders() {
    let snap = library();
    let model = model_of(&snap);
    let logos = logos(Protocol::Kitty, CellPx { w: 8, h: 16 });
    let drawn = drawn_with(&model, &snap, &logos, 100, 30);
    let placement = drawn
        .placements
        .iter()
        .find(|placement| placement.size == LogoSize::Small)
        .expect("the header's mark is placed");

    // The mark's cells hold the placeholder: the first of them is the character for row 0, column 0.
    let cell = &drawn.buffer[(0, 0)];
    assert!(
        cell.symbol().starts_with('\u{10EEEE}'),
        "the first header cell is a placeholder, found {:?}",
        cell.symbol()
    );
    assert_eq!(
        cell.symbol(),
        kitty::placeholder(0, 0).as_str(),
        "row 0, column 0 of the mark"
    );
    assert_eq!(
        cell.fg,
        kitty::id_colour(kitty::SMALL_ID),
        "the placeholder's foreground is the id, the one colour that is not a theme token"
    );

    // The title starts one column to the right of the mark: the mark's blank column stands
    // between them. The frame buffer holds it as a cell, not as characters, so the column is
    // a cell count, not a character count.
    let title_start = usize::from(placement.rect.width) + 1;
    let title_cell = &drawn.buffer[(title_start as u16, 0)];
    assert!(
        title_cell.symbol().starts_with('f'),
        "the title starts at cell {title_start} (the mark's columns plus its blank column), \
         found {:?}",
        title_cell.symbol()
    );

    // The corner of the mark: its last cell holds the character for its last row and column.
    let last = &drawn.buffer[(placement.rect.right() - 1, placement.rect.bottom() - 1)];
    assert_eq!(
        last.symbol(),
        kitty::placeholder(2, placement.rect.width - 1).as_str(),
        "the last cell of the mark"
    );
}

/// A sixel terminal: the mark's cells are skipped, so the frame writes nothing over the image
/// the placer writes after the frame, and the placement the frame returns is where it will be
/// written.
#[test]
fn sixel_header_cells_are_skipped() {
    let snap = library();
    let model = model_of(&snap);
    let logos = logos(Protocol::Sixel, CellPx { w: 8, h: 16 });
    let drawn = drawn_with(&model, &snap, &logos, 100, 30);
    let placement = drawn
        .placements
        .iter()
        .find(|placement| placement.size == LogoSize::Small)
        .expect("the header's mark is placed");
    assert_eq!(placement.size, LogoSize::Small);
    assert_eq!(
        drawn.placements.len(),
        1,
        "the full view at this size places exactly the header's mark: {:?}",
        drawn.placements
    );

    // Every cell the mark covers is skipped: the frame never writes it.
    for y in 0..placement.rect.height {
        for x in 0..placement.rect.width {
            let cell = &drawn.buffer[(placement.rect.x + x, placement.rect.y + y)];
            assert_eq!(
                cell.diff_option,
                CellDiffOption::Skip,
                "the cell at {x}, {y} of the mark must be skipped"
            );
        }
    }
}

/// The help screen on a 100 x 30 screen: the mark stands at the top of the help box, centred
/// above the keys, when the key list and the mark both fit. The rect it is placed over is the
/// mark's own cells - no more, no less - and only those cells are the ones the frame skips.
#[test]
fn help_shows_large_logo_when_it_fits() {
    let snap = library();
    let mut model = model_of(&snap);
    model.modal = Some(fbtt_tui::modal::Modal::Help);
    let logos = logos(Protocol::Sixel, CellPx { w: 8, h: 16 });
    let drawn = drawn_with(&model, &snap, &logos, 100, 30);
    let large = drawn
        .placements
        .iter()
        .find(|placement| matches!(placement.size, LogoSize::Large(_)));
    assert!(
        large.is_some(),
        "a 30-row screen has room for the mark and for every key"
    );
    let large = large.expect("the mark is there");
    // The rect is exactly the mark's own cells: the bitmap the view placed, no padding around it.
    let prepared = logos
        .large
        .iter()
        .find(|prepared| prepared.dims.rows == large.dims.rows)
        .expect("the rows the view placed");
    assert_eq!(
        (large.rect.width, large.rect.height),
        (prepared.dims.cols, prepared.dims.rows),
        "the placement is the mark's own cells, not a rect the mark happens to fit in"
    );
    assert_eq!(large.dims, prepared.dims);
    // Centred above the keys: the mark is roughly in the middle of the screen, with the same
    // (or nearly the same) space on either side. The exact centring depends on the box's width,
    // which is the widest line plus padding, but the mark must be within a few columns of the
    // screen's centre.
    let centre = 100u16 / 2;
    let mark_centre = large.rect.x + large.rect.width / 2;
    assert!(
        mark_centre.abs_diff(centre) <= 5,
        "the mark is roughly centred in the screen: mark at x={}",
        large.rect.x
    );
    // And it is inside the box, at its top: the first row of the box holds the mark, not the
    // key that would be there without it.
    let text = drawn.screen.text();
    assert!(
        text.contains("quit") && text.contains("sort by the next column"),
        "every key is still listed beside the mark"
    );
}

/// The help screen on a short screen: no rows for the mark, so the keys take them.
#[test]
fn help_drops_logo_when_short() {
    let snap = library();
    let mut model = model_of(&snap);
    model.modal = Some(fbtt_tui::modal::Modal::Help);
    let logos = logos(Protocol::Sixel, CellPx { w: 8, h: 16 });
    let drawn = drawn_with(&model, &snap, &logos, 100, 20);
    assert!(
        !drawn
            .placements
            .iter()
            .any(|placement| matches!(placement.size, LogoSize::Large(_))),
        "a 20-row screen has no rows for the mark"
    );
    // The keys are still all there: the box just took the rows the mark would have stood in.
    let text = drawn.screen.text();
    for want in ["quit", "sort by the next column"] {
        assert!(text.contains(want), "the keys lost {want:?}: {text:?}");
    }
}

/// A dialog over the header: the mark is not placed this frame, because the box it would be
/// drawn under is the box drawn over it. At 10 rows by 10 columns the settings box is
/// centred at (0, 0) and covers the three header rows and the mark's columns.
///
/// The help screen's mark is not dropped by the same rule: it belongs to the help box, and a
/// box that is its own does not stand over it.
#[test]
fn help_placement_is_not_dropped_by_its_own_box() {
    let snap = library();
    let mut model = model_of(&snap);
    model.modal = Some(fbtt_tui::modal::Modal::Help);
    let logos = logos(Protocol::Sixel, CellPx { w: 8, h: 16 });
    let drawn = drawn_with(&model, &snap, &logos, 100, 30);
    assert!(
        drawn
            .placements
            .iter()
            .any(|placement| matches!(placement.size, LogoSize::Large(_))),
        "the help box's own mark is not dropped by the drop rule"
    );
}
#[test]
fn modal_over_header_drops_small_placement() {
    let snap = library();
    let mut model = model_of(&snap);
    model.modal = Some(fbtt_tui::modal::Modal::Settings(
        fbtt_tui::modal::SettingsForm::new(&snap.limits),
    ));
    let logos = logos(Protocol::Kitty, CellPx { w: 8, h: 16 });
    let drawn = drawn_with(&model, &snap, &logos, 10, 10);
    assert!(
        !drawn
            .placements
            .iter()
            .any(|placement| placement.size == LogoSize::Small),
        "a dialog over the header takes the mark off the screen"
    );
}

/// The minimal view never has a mark: its one line per torrent is all it is, at any size.
#[test]
fn minimal_view_has_no_logo() {
    let snap = library();
    let mut model = model_of(&snap);
    model.view = fbtt_tui::View::Minimal;
    let logos = logos(Protocol::Kitty, CellPx { w: 8, h: 16 });
    let drawn = drawn_with(&model, &snap, &logos, 100, 30);
    assert!(
        drawn.placements.is_empty(),
        "the minimal view does not place a mark"
    );
}

/// A header of one row: the mark wants three, so it is not placed and the one line of text
/// takes the header it has. At height 7 the header gives up two of its three rows to the
/// torrents, leaving one row for the name and the rates.
///
/// The layout only ever makes a header of three rows or one: at eight rows or more it is the
/// design's three lines, and below that it is the one line that replaces them. There is no
/// two-row header to test, so this is what the short case actually is.
#[test]
fn one_row_header_has_no_logo() {
    let snap = library();
    let model = model_of(&snap);
    let logos = logos(Protocol::Kitty, CellPx { w: 8, h: 16 });
    let drawn = drawn_with(&model, &snap, &logos, 100, 7);
    assert!(
        !drawn
            .placements
            .iter()
            .any(|placement| placement.size == LogoSize::Small),
        "a one-row header has no mark"
    );
    // The one line it has: the name and what is moving.
    let text = drawn.screen.text();
    assert!(
        text.contains("featherbittorrent"),
        "the title is still there"
    );
    assert!(text.contains("MiB/s"), "the rates are still there");
}
