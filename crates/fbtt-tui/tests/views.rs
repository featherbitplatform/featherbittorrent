//! The main screens and the model's keys, tested the way the binary uses them: keys in,
//! commands out, and a buffer to read what a person would have seen.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fbtt_core::{Command, DetailTab, RowState, Snapshot, TorrentId};
use fbtt_tui::{Descending, SortKey, visible_rows};
use ratatui::style::Color;

mod support;
use support::*;

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
    let commands = model.handle_key(letter('p'), &snap);
    assert_eq!(commands, vec![Command::Pause(snap.torrents[0].id.clone())]);

    // Two steps down reaches the paused one, where `p` starts it again.
    model.handle_key(letter('j'), &snap);
    model.handle_key(letter('j'), &snap);
    let commands = model.handle_key(letter('p'), &snap);
    assert_eq!(commands, vec![Command::Resume(snap.torrents[2].id.clone())]);

    // Nothing selected means nothing to stop.
    let empty = Snapshot {
        torrents: Vec::new(),
        ..library()
    };
    let mut model = model_of(&empty);
    assert_eq!(model.handle_key(letter('p'), &empty), Vec::new());
}

#[test]
fn quit_asks_twice_when_something_downloads() {
    let snap = library();
    let mut model = model_of(&snap);

    // A torrent is downloading: the first `q` only asks.
    let commands = model.handle_key(letter('q'), &snap);
    assert!(commands.is_empty(), "the first q quit without asking");
    assert!(!model.quit_requested());
    assert!(
        Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("q again"),
        "nothing told the person that q needs pressing again"
    );

    // A second `q` confirms.
    let commands = model.handle_key(letter('q'), &snap);
    assert_eq!(commands, vec![Command::Shutdown]);
    assert!(model.quit_requested());

    // Any other key cancels the ask instead.
    let mut model = model_of(&snap);
    model.handle_key(letter('q'), &snap);
    model.handle_key(letter('j'), &snap);
    assert!(!model.quit_requested());
    let commands = model.handle_key(letter('q'), &snap);
    assert!(commands.is_empty(), "a cancelled q quit at once");

    // `y` confirms too.
    model.handle_key(letter('y'), &snap);
    assert!(model.quit_requested());

    // With nothing downloading, one `q` is enough.
    let idle = Snapshot {
        torrents: vec![torrent("a", "quiet.torrent", RowState::Paused)],
        ..library()
    };
    let mut model = model_of(&idle);
    let commands = model.handle_key(letter('q'), &idle);
    assert_eq!(commands, vec![Command::Shutdown]);
    assert!(model.quit_requested());
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
