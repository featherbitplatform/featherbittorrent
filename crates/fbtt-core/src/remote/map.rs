//! The shapes the server answers in, and how they become rows.
//!
//! These structs are ours, not fbt-api's types: a client that deserialises a server's own DTOs
//! inherits every rename and every added field that crate makes between releases, and a server newer
//! or older than this client then fails to parse at all. So each one is tolerant - `#[serde(default)]`
//! over the whole struct, nothing refused for being unknown - and only the fields a screen draws are
//! named at all. A response that answers a question this client did not ask is not a broken response.
//!
//! What they become is the same vocabulary the local backend uses: the state table and the stalled
//! clock come from [`crate::engine::map`], so a torrent says the same thing about itself whether it
//! was read out of this process's session or over HTTP from another machine.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::client::{
    DeviceRow, FilePriority, FileRow, LiveLimits, PeerRow, RowState, TorrentId, TorrentRow, Totals,
    TrackerRow,
};
use crate::engine::map::{clean, row_state, stalled_clock};

// The shapes themselves are in [`crate::remote::dto`]; re-exported here so that a caller of the
// mapping says `map::TorrentDto`, which is the one name for both halves.
pub(crate) use crate::remote::dto::*;

/// The units a limit is stated in on each side: the server counts bytes per second, the table KiB/s.
const KIB: u64 = 1024;

/// The most piece bitmap this client builds a row for. A million pieces is 125 KiB of server bitmap and a
/// million entries here - more than any torrent in this release's world - and `num_pieces` is a `u32` from
/// the server, so without a ceiling one hostile answer would ask this process for four gigabytes.
const MAX_PIECES: usize = 1_048_576;

/// What the table says about one torrent's state.
///
/// `state` and `checking_progress` are what the server said; `down_rate`, `zero_since` and `now` go
/// to [`row_state`], which is the same rule the local backend applies to its own engine.
fn row_state_of(
    state: &str,
    checking_progress: Option<f64>,
    progress: f64,
    down_rate: u64,
    zero_since: Instant,
    now: Instant,
) -> RowState {
    row_state(
        torrent_state(state, checking_progress),
        progress,
        down_rate,
        zero_since,
        now,
    )
}

/// The server's snake_case state name as the engine's own state: one spelling for both backends'
/// tables, and a name this release does not know is a torrent that is running.
fn torrent_state(state: &str, checking_progress: Option<f64>) -> fbt::TorrentState {
    match state {
        "downloading_metadata" => fbt::TorrentState::DownloadingMetadata,
        "queued_for_checking" => fbt::TorrentState::QueuedForChecking,
        "checking_files" => fbt::TorrentState::CheckingFiles {
            progress: checking_progress.unwrap_or(0.0),
        },
        "downloading" => fbt::TorrentState::Downloading,
        "seeding" => fbt::TorrentState::Seeding,
        "paused" => fbt::TorrentState::Paused,
        "queued" => fbt::TorrentState::Queued,
        "error" => fbt::TorrentState::Error,
        _ => fbt::TorrentState::Downloading,
    }
}

/// One row from one torrent's answer. `zero_since` is where [`stalled_clock`] left this torrent's
/// stalled count at the look that produced `dto`.
fn row(dto: &TorrentDto, zero_since: Instant, now: Instant) -> TorrentRow {
    let name = clean(&dto.name);
    TorrentRow {
        id: TorrentId(dto.id.clone()),
        // A magnet whose metadata has not arrived has no name yet; its hash is the only thing to call
        // it by, and that is what the local backend shows for the same torrent. It came from the server
        // like everything else here, so it gets cleaned on the way in too.
        name: if name.is_empty() {
            clean(&dto.id)
        } else {
            name
        },
        progress: dto.progress,
        state: row_state_of(
            &dto.state,
            dto.checking_progress,
            dto.progress,
            dto.download_rate,
            zero_since,
            now,
        ),
        down_rate: dto.download_rate,
        up_rate: dto.upload_rate,
        peers: dto.num_peers,
        seeds: dto.num_seeds,
        size: dto.total_wanted,
        done: dto.wanted_done,
        save_path: PathBuf::from(&dto.save_path),
        error: dto.error.as_deref().map(clean),
        queue_position: dto.queue_position,
    }
}

/// The table for every torrent the server listed, in the order it listed them (its queue order), and
/// the header's rates summed over them.
///
/// `stalled` is updated as it goes exactly as the local backend updates its own: a torrent no longer
/// on the server loses its entry, and one first seen starts its stalled count now.
pub(crate) fn rows(
    torrents: &[TorrentDto],
    stalled: &mut HashMap<TorrentId, Instant>,
    now: Instant,
) -> (Vec<TorrentRow>, Totals) {
    let seen = std::mem::take(stalled);
    let mut next = HashMap::with_capacity(seen.len());
    let mut down_rate = 0u64;
    let mut up_rate = 0u64;
    let rows = torrents
        .iter()
        .map(|dto| {
            let id = TorrentId(dto.id.clone());
            let downloading = matches!(
                torrent_state(&dto.state, dto.checking_progress),
                fbt::TorrentState::Downloading
            );
            let zero_since = stalled_clock(
                if downloading {
                    fbt::TorrentState::Downloading
                } else {
                    fbt::TorrentState::Paused
                },
                dto.download_rate,
                seen.get(&id).copied(),
                now,
            );
            let row = row(dto, zero_since, now);
            next.insert(id.clone(), zero_since);
            down_rate = down_rate.saturating_add(dto.download_rate);
            up_rate = up_rate.saturating_add(dto.upload_rate);
            row
        })
        .collect();
    *stalled = next;
    (
        rows,
        Totals {
            down_rate,
            up_rate,
            // Filled in from the session route by whoever assembled the snapshot: these rows are
            // torrents, and a DHT count is not one.
            dht_nodes: None,
        },
    )
}

/// The file list for the pane. Pad files are left out and the indices stay the server's, so a
/// priority change can name a file by the row it is looking at.
pub(crate) fn file_rows(files: Vec<FileDto>) -> Vec<FileRow> {
    files
        .into_iter()
        .filter(|file| !file.pad)
        .map(|file| FileRow {
            index: file.index,
            path: clean(&file.path),
            size: file.len,
            done: file.downloaded,
            priority: priority_of(&file.priority),
        })
        .collect()
}

/// The pane's peer list. The server always answers a share of the pieces, so it is always a bar.
pub(crate) fn peer_rows(peers: Vec<PeerDto>) -> Vec<PeerRow> {
    peers
        .into_iter()
        .map(|peer| PeerRow {
            // An address is a peer's own text, and a peer on a hostile network can put anything in the
            // field this client prints.
            addr: clean(&peer.addr),
            client: peer.client.map(|client| clean(&client)).unwrap_or_default(),
            down_rate: peer.download_rate,
            up_rate: peer.upload_rate,
            progress: Some(peer.progress),
        })
        .collect()
}

/// The pane's tracker list. One that answered its last announce is `ok`; one that did not says the
/// reason it gave.
pub(crate) fn tracker_rows(trackers: Vec<TrackerDto>) -> Vec<TrackerRow> {
    trackers
        .into_iter()
        .map(|tracker| TrackerRow {
            url: clean(&tracker.url),
            tier: tracker.tier,
            status: tracker
                .last_error
                .as_deref()
                .map(clean)
                .unwrap_or_else(|| "ok".to_owned()),
            seeders: tracker.seeders,
            leechers: tracker.leechers,
        })
        .collect()
}

/// The piece bitmap as one entry per piece.
///
/// `have` is the hex of the server's own packed bitmap: two characters to a byte, eight pieces to a byte,
/// piece 0 in the top bit of the first byte (BEP 3 order, which is how fbt-api packs it too). The count is
/// clamped to the bits that were actually sent - `num_pieces` is a `u32` from a server this client does not
/// trust - and to [`MAX_PIECES`] on top of that, so an answer that claims four billion pieces costs a
/// bounded vector rather than gigabytes. Bits past the last piece are spare bits of the final byte, which the
/// server leaves zero and this never reads.
pub(crate) fn piece_flags(pieces: &PiecesDto) -> Vec<bool> {
    let bytes = pieces.have.len() / 2;
    let claimed = usize::try_from(pieces.num_pieces).unwrap_or(usize::MAX);
    let count = claimed.min(bytes.saturating_mul(8)).min(MAX_PIECES);
    (0..count)
        .map(|piece| {
            let index = piece / 8;
            let Some(byte) = pieces.have.get(index * 2..index * 2 + 2) else {
                return false;
            };
            let Ok(byte) = u8::from_str_radix(byte, 16) else {
                return false;
            };
            // Piece 0 is the top bit of the first byte: most significant first.
            byte & (1 << (7 - piece % 8)) != 0
        })
        .collect()
}

/// The limits the server is running under, from its session and queue answers, in KiB/s. A limit that
/// is not a whole KiB shows as the KiB below it, as it does for the local backend.
pub(crate) fn live_limits(session: &SessionDto, queue: &QueueDto) -> LiveLimits {
    LiveLimits {
        download_kib: session.download_limit / KIB,
        upload_kib: session.upload_limit / KIB,
        active_downloads: queue.active_downloads,
        active_seeds: queue.active_seeds,
        active_total: queue.active_limit,
    }
}

/// The rates alone, for a server that answers its session but not its queue. The counts are `None`, which
/// is what the table shows an unknown limit as - not invented as unlimited, which would tell a person their
/// downloads are uncapped when this client simply was not told.
pub(crate) fn rate_limits(session: &SessionDto) -> LiveLimits {
    LiveLimits {
        download_kib: session.download_limit / KIB,
        upload_kib: session.upload_limit / KIB,
        active_downloads: None,
        active_seeds: None,
        active_total: None,
    }
}

/// The pane's priority as the API spells it.
fn priority_of(priority: &str) -> FilePriority {
    match priority {
        "skip" => FilePriority::Skip,
        "low" => FilePriority::Low,
        "high" => FilePriority::High,
        // `normal`, the default, and anything a later server invents.
        _ => FilePriority::Normal,
    }
}

/// What this client sends back for one priority.
fn priority_name(priority: FilePriority) -> &'static str {
    match priority {
        FilePriority::Skip => "skip",
        FilePriority::Low => "low",
        FilePriority::Normal => "normal",
        FilePriority::High => "high",
    }
}

/// The whole file list's priorities with one entry changed, ready for `PUT .../file-priorities`.
///
/// The server wants one priority per file and refuses a list that does not match its own count, so
/// this is built from what it just reported - pad files included, in its order - and `file` names an
/// index in that same list. `None` when no entry has that index: the pane was looking at a file that
/// is not there any more.
pub(crate) fn priorities_with(
    files: &[FileDto],
    file: usize,
    priority: FilePriority,
) -> Option<PrioritiesBody<'static>> {
    let mut names: Vec<&'static str> = files
        .iter()
        .map(|file| priority_name(priority_of(&file.priority)))
        .collect();
    let slot = names.get_mut(file)?;
    *slot = priority_name(priority);
    Some(PrioritiesBody { priorities: names })
}

/// The devices a server lets sign in, as its route answers them.
pub(crate) fn device_rows(devices: Vec<DeviceDto>) -> Vec<DeviceRow> {
    devices
        .into_iter()
        .map(|device| DeviceRow {
            name: clean(&device.name),
            // A fingerprint is a hash rendered in base64, which survives this untouched; it is still the
            // server's text rather than this client's, so it goes through the same door.
            fingerprint: clean(&device.fingerprint),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{MAX_PIECES, PiecesDto, TorrentDto, piece_flags, row_state_of};
    use crate::client::RowState;
    use std::time::{Duration, Instant};

    /// The state table the server's names go through, including the one a screen cares about most:
    /// a download taking nothing for half a minute is stalled, and one that just started is not.
    #[test]
    fn states_map_to_the_same_table_as_the_local_backend() {
        let now = Instant::now();
        let long_ago = now - Duration::from_secs(40);
        assert_eq!(
            row_state_of("checking_files", Some(0.25), 0.1, 0, now, now),
            RowState::Checking(0.25),
        );
        assert_eq!(
            row_state_of("seeding", None, 1.0, 0, long_ago, now),
            RowState::Seeding,
        );
        assert_eq!(
            row_state_of("downloading", None, 0.4, 0, long_ago, now),
            RowState::Stalled,
            "running, taking nothing, for longer than the stall window",
        );
        assert_eq!(
            row_state_of("downloading", None, 0.4, 1024, long_ago, now),
            RowState::Downloading,
            "bytes arriving end the count",
        );
        assert_eq!(
            row_state_of("something_newer", None, 0.4, 0, now, now),
            RowState::Downloading,
            "a state this release does not know is a torrent that is running",
        );
    }

    /// Eight pieces to a byte, most significant first: a pattern across three bytes reads back as the pieces
    /// it names, not as nibbles wandering across the wrong bytes.
    #[test]
    fn pieces_read_in_bep_3_order() {
        // `81` is 1000_0001: pieces 0 and 7. `42` is 0100_0010: the second byte's bits 6 and 1, which are
        // pieces 14 and 9. `00` says nothing about pieces 16 through 19.
        let dto = PiecesDto {
            num_pieces: 20,
            have: "814200".to_string(),
        };
        let flags = piece_flags(&dto);
        assert_eq!(flags.len(), 20, "one entry per piece the server named");
        let had: Vec<usize> = flags
            .iter()
            .enumerate()
            .filter_map(|(piece, have)| have.then_some(piece))
            .collect();
        assert_eq!(had, [0, 7, 9, 14]);

        // A bitmap that covers more pieces than it was sent with reports the ones it covers.
        let five = PiecesDto {
            num_pieces: 5,
            have: "a0".to_string(),
        };
        assert_eq!(
            piece_flags(&five),
            [true, false, true, false, false],
            "`a0` is 1010_0000",
        );
    }

    /// A `u32` from a hostile server is not a size to trust; the bitmap that arrived is. Even a count the
    /// bitmap does cover has a ceiling, because a vector this client builds is memory somebody pays for.
    #[test]
    fn a_huge_piece_count_is_clamped() {
        // One byte of bitmap speaks for eight pieces, whatever the count claims.
        let one_byte = PiecesDto {
            num_pieces: u32::MAX,
            have: "c0".to_string(),
        };
        assert_eq!(
            piece_flags(&one_byte),
            [true, true, false, false, false, false, false, false],
            "`c0` is 1100_0000, and eight pieces is all one byte can speak for",
        );

        // A count the bitmap does cover still stops at the ceiling rather than allocating on the server's
        // say-so: this one claims four billion and sends enough bits for a million and eight.
        let beyond = PiecesDto {
            num_pieces: u32::MAX,
            have: "ff".repeat(MAX_PIECES / 8 + 1),
        };
        assert_eq!(piece_flags(&beyond).len(), MAX_PIECES);

        // Nothing sent claims nothing had, rather than a bar for every piece nobody described.
        let empty = PiecesDto {
            num_pieces: 4,
            have: String::new(),
        };
        assert_eq!(piece_flags(&empty).len(), 0);
    }

    /// A torrent with no name yet - a magnet whose metadata has not arrived - is listed by its hash.
    #[test]
    fn an_unnamed_torrent_is_listed_by_its_hash() {
        let dto = TorrentDto {
            id: "aa".repeat(20),
            ..TorrentDto::default()
        };
        let row = super::row(&dto, Instant::now(), Instant::now());
        assert_eq!(row.name, dto.id);
    }
}
