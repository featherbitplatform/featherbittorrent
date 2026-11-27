//! The library and the drawn screen that every test in this crate reads.
//!
//! Each integration test target compiles its own copy of these (`mod support;`), which is why the
//! set may carry a helper one target has no use for.
#![allow(dead_code)] // each test binary uses part of the set, not the whole of it

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fbtt_core::{
    Connection, Detail, DeviceRow, DirListing, FilePriority, FileRow, LinkState, LiveLimits,
    RowState, Snapshot, TorrentId, TorrentRow, Totals,
};
use fbtt_tui::Model;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::CellWidth;
use ratatui::style::Color;

/// A key press with no modifiers, as a terminal reports it.
pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// A plain letter press.
pub fn letter(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

/// A row with everything a test does not care about set to something sane.
pub fn torrent(id: &str, name: &str, state: RowState) -> TorrentRow {
    TorrentRow {
        id: TorrentId(id.to_string()),
        name: name.to_string(),
        progress: 0.0,
        state,
        down_rate: 0,
        up_rate: 0,
        peers: 0,
        seeds: 0,
        size: 3_298_534_880,
        done: 0,
        save_path: PathBuf::from("/downloads"),
        error: None,
        queue_position: 0,
    }
}

/// A downloading torrent, a seeding one, and the files of the first: what the screens are for.
/// Sorted by queue, which is the model's own default order. Every limit is unlimited, which is
/// what a settings dialog opened on this library has to show.
pub fn library() -> Snapshot {
    let mut buck = torrent(
        &TorrentId::from_wire_hash([0x11; 20]).0,
        "Big.Buck.Bunny.4K",
        RowState::Downloading,
    );
    buck.progress = 0.62;
    // 4.2 MiB/s exactly, as `format::rate` spells it.
    buck.down_rate = 4_404_019;
    buck.up_rate = 317_440;
    buck.peers = 42;
    buck.seeds = 18;
    buck.done = 2_045_091_628;

    let mut debian = torrent(
        &TorrentId::from_wire_hash([0x22; 20]).0,
        "debian-13.1-amd64.iso",
        RowState::Seeding,
    );
    debian.progress = 1.0;
    debian.up_rate = 1_153_434;
    debian.peers = 7;
    debian.seeds = 0;
    debian.done = debian.size;
    debian.queue_position = 1;

    Snapshot {
        connection: Connection::Local,
        torrents: vec![buck.clone(), debian.clone()],
        totals: Totals {
            down_rate: buck.down_rate,
            up_rate: buck.up_rate + debian.up_rate,
            dht_nodes: Some(312),
        },
        limits: LiveLimits {
            download_kib: 0,
            upload_kib: 0,
            active_downloads: None,
            active_seeds: None,
            active_total: None,
        },
        detail: Some((
            buck.id.clone(),
            Detail::Files(vec![
                FileRow {
                    index: 0,
                    path: "bbb_sunflower_2160p_60fps.mp4".to_string(),
                    size: 3_006_542_368,
                    done: 1_924_187_115,
                    priority: FilePriority::Normal,
                },
                FileRow {
                    index: 1,
                    path: "bbb.srt".to_string(),
                    size: 88_741,
                    done: 88_741,
                    priority: FilePriority::Normal,
                },
            ]),
        )),
        default_dir: PathBuf::from("/downloads"),
        dirs: None,
        devices: None,
        settings: fbtt_core::SettingsState::NotLoaded,
        proxy: fbtt_core::proxy::ProxyHealth::Off,
    }
}

/// The whole UI at a size, as the terminal would show it.
pub struct Screen {
    terminal: Terminal<TestBackend>,
}

impl Screen {
    pub fn new(model: &Model, snap: &Snapshot, width: u16, height: u16) -> Screen {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("a test terminal");
        terminal
            .draw(|frame| fbtt_tui::draw(frame, model, snap))
            .expect("a drawn frame");
        Screen { terminal }
    }

    /// A screen from a terminal a test drew into itself, for a frame drawn with something `new`
    /// cannot - the prepared logos the binary gives it.
    pub fn from_terminal(terminal: Terminal<TestBackend>) -> Screen {
        Screen { terminal }
    }

    /// Every buffer line as text, left to right and top to bottom.
    pub fn lines(&self) -> Vec<String> {
        let buffer = self.terminal.backend().buffer();
        let area = *buffer.area();
        (0..area.height)
            .map(|y| {
                let mut line = String::new();
                // A cell that a wider character has drawn over is left out, the same way the
                // library's own debug view of a buffer leaves it out.
                let mut covered = 0u16;
                for x in 0..area.width {
                    let cell = &buffer[(x, y)];
                    if covered == 0 {
                        line.push_str(cell.symbol());
                    }
                    covered = covered.max(cell.cell_width()).saturating_sub(1);
                }
                line
            })
            .collect()
    }

    /// The whole screen as one string.
    pub fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// Where a needle first appears, as its column and row, or `None`.
    pub fn find(&self, needle: &str) -> Option<(usize, usize)> {
        self.lines().iter().enumerate().find_map(|(y, line)| {
            line.find(needle)
                .map(|byte| (line[..byte].chars().count(), y))
        })
    }

    /// The foreground colour of the cell at a column and row.
    pub fn fg(&self, x: usize, y: usize) -> Color {
        self.cell(x, y).fg
    }

    /// The background colour of the cell at a column and row, which is how a lifted row or a
    /// drawn cursor is told apart from text that happens to be the same colour.
    pub fn bg(&self, x: usize, y: usize) -> Color {
        self.cell(x, y).bg
    }

    fn cell(&self, x: usize, y: usize) -> &ratatui::buffer::Cell {
        let buffer = self.terminal.backend().buffer();
        &buffer[(x as u16, y as u16)]
    }

    /// The width of the terminal this was drawn into.
    pub fn width(&self) -> usize {
        usize::from(self.terminal.backend().buffer().area().width)
    }
}

/// The folders a backend says it may name, spelled as they were configured: where a destination picker
/// starts and where going back up comes back to.
pub const ROOTS: [&str; 2] = ["/downloads", "/home/alice"];

/// The library with one folder listing in it, which is what a destination picker draws. `inside` is the
/// folder that was asked about - `None` for the list of places to start from - and `dirs` are what is
/// directly inside it, in the order the backend sorted them.
pub fn folders(inside: Option<&str>, dirs: &[&str]) -> Snapshot {
    let mut snap = library();
    snap.dirs = Some(DirListing {
        path: inside.map(PathBuf::from),
        roots: ROOTS.iter().map(|root| PathBuf::from(*root)).collect(),
        dirs: dirs.iter().map(|dir| (*dir).to_string()).collect(),
        error: None,
    });
    snap
}

/// The library when its torrents come from a server, with the link in the state `state`: that state is
/// what a header test is about, and everything else stays the library the other tests read. A server's
/// snapshot reports what that server reported, so this client's own DHT count is not in it.
pub fn server(state: LinkState) -> Snapshot {
    let mut snap = library();
    snap.totals.dht_nodes = None;
    snap.connection = Connection::Remote {
        name: "bit.example".to_string(),
        secure: false,
        auth: "password".to_string(),
        state,
    };
    snap
}

/// The same, with the device list that server gave: each row a name and the fingerprint a person checks
/// the name against before trusting it.
pub fn server_with_devices(devices: &[(&str, &str)]) -> Snapshot {
    let mut snap = server(LinkState::Connected);
    snap.devices = Some(
        devices
            .iter()
            .map(|(name, fingerprint)| DeviceRow {
                name: (*name).to_string(),
                fingerprint: (*fingerprint).to_string(),
            })
            .collect(),
    );
    snap
}

/// A model looking at a snapshot the way the binary leaves it after every new one.
pub fn model_of(snap: &Snapshot) -> Model {
    let mut model = Model::new();
    model.sync_selection(snap);
    model
}
