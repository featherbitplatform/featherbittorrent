//! Mapping between what the engine reports and what the table shows.
//!
//! The engine counts bytes per second and names its states in its own vocabulary;
//! the table wants KiB/s, a stalled column of its own, and only the text a person
//! can read. Everything crossing here is cut and cleaned on the way.

use std::time::{Duration, Instant};

use fbt::{
    FileStatus, InfoHashes, PeerStatus, Priority, Session, SessionError, TorrentState,
    TorrentStatus, TrackerStatus,
};

use crate::client::{
    FilePriority, FileRow, LiveLimits, PeerRow, RowState, TorrentId, TorrentRow, TrackerRow,
};

/// One KiB, the unit rates are stated in. The engine counts bytes per second.
const KIB: u64 = 1024;
/// How long a download may take nothing before the table calls it stalled. Long
/// enough that a slow tracker reply or a handshake is not "stalled".
pub(super) const STALL_AFTER: Duration = Duration::from_secs(30);

/// Text from a torrent file, a peer, a tracker or an error, cut and cleaned the way
/// every remote string is before it is drawn.
pub(super) fn clean(text: &str) -> String {
    fbt::display_safe(text, fbt::MAX_DISPLAY_CHARS)
}

/// KiB/s as the engine counts it. A value too large to convert caps out instead of
/// wrapping into a small limit.
pub(super) fn rate_bytes(kib: u64) -> u64 {
    kib.saturating_mul(KIB)
}

/// The limits the session is running under, read back from it rather than from what
/// was last asked for, in KiB/s. A limit that is not a whole KiB shows as the KiB
/// below it.
pub(super) fn limits(session: &Session) -> LiveLimits {
    let rate = session.rate_limit();
    let queue = session.queue_settings();
    LiveLimits {
        download_kib: rate.download / KIB,
        upload_kib: rate.upload / KIB,
        active_downloads: queue.active_downloads,
        active_seeds: queue.active_seeds,
        active_total: queue.active_limit,
    }
}

/// One row from one torrent's status.
///
/// `last_rate` is when this torrent was last seen taking anything, `None` the first
/// time it appears in a snapshot. A download that has taken nothing since before
/// [`STALL_AFTER`] is stalled: it is running, but nothing is arriving, and a person
/// scanning the table needs to see that rather than a bar stuck at 40 %.
pub(super) fn row(status: &TorrentStatus, last_rate: Option<Instant>, now: Instant) -> TorrentRow {
    let state = match status.state {
        TorrentState::DownloadingMetadata => RowState::Metadata,
        // Waiting for a check slot stops a torrent exactly like waiting for a place
        // in the queue, so the table shows one thing for both.
        TorrentState::QueuedForChecking | TorrentState::Queued => RowState::Queued,
        TorrentState::CheckingFiles { progress } => RowState::Checking(progress),
        TorrentState::Downloading => {
            let waiting_too_long = last_rate
                .and_then(|since| now.checked_duration_since(since))
                .is_some_and(|waiting| waiting >= STALL_AFTER);
            if status.download_rate == 0 && status.progress < 1.0 && waiting_too_long {
                RowState::Stalled
            } else {
                RowState::Downloading
            }
        }
        TorrentState::Seeding => RowState::Seeding,
        TorrentState::Paused => RowState::Paused,
        TorrentState::Error => RowState::Error,
        // A state newer than this mapping is a torrent that is running: show it as
        // one rather than pretending it stopped.
        _ => RowState::Downloading,
    };
    TorrentRow {
        id: TorrentId::from_wire_hash(status.info_hashes.wire_hash()),
        name: clean(&status.name),
        progress: status.progress,
        state,
        down_rate: status.download_rate,
        up_rate: status.upload_rate,
        peers: status.num_peers,
        seeds: status.num_seeds,
        size: status.total_wanted,
        done: status.wanted_done,
        save_path: status.save_path.clone(),
        error: status.error.as_deref().map(clean),
        queue_position: status.queue_position,
    }
}

/// The file list for the detail pane.
///
/// Pad files are left out: they hold the part of a piece that a real file needs, and
/// there is nothing for a person to choose about them. The indices stay the engine's,
/// so [`Command::SetFilePriority`](crate::Command::SetFilePriority) can name a file by
/// the row it is looking at.
pub(super) fn file_rows(files: Vec<FileStatus>) -> Vec<FileRow> {
    files
        .into_iter()
        .filter(|file| !file.pad)
        .map(|file| FileRow {
            index: file.index,
            path: clean(&file.path.to_string_lossy()),
            size: file.len,
            done: file.downloaded,
            priority: file_priority(file.priority),
        })
        .collect()
}

/// The engine's priority as the pane shows it. A priority newer than this mapping
/// shows as the default.
fn file_priority(priority: Priority) -> FilePriority {
    match priority {
        Priority::Skip => FilePriority::Skip,
        Priority::Low => FilePriority::Low,
        Priority::Normal => FilePriority::Normal,
        Priority::High => FilePriority::High,
        _ => FilePriority::Normal,
    }
}

/// The pane's priority as the engine takes it.
pub(super) fn priority(priority: FilePriority) -> Priority {
    match priority {
        FilePriority::Skip => Priority::Skip,
        FilePriority::Low => Priority::Low,
        FilePriority::Normal => Priority::Normal,
        FilePriority::High => Priority::High,
    }
}

/// The peer list. `progress` is the share of the torrent's pieces the peer has: it
/// is always known once the metadata is, and 0.0 before that, so a bar can be drawn.
pub(super) fn peer_rows(peers: Vec<PeerStatus>) -> Vec<PeerRow> {
    peers
        .into_iter()
        .map(|peer| PeerRow {
            addr: peer.addr.to_string(),
            client: peer.client.map(|client| clean(&client)).unwrap_or_default(),
            down_rate: peer.download_rate,
            up_rate: peer.upload_rate,
            progress: Some(peer.progress),
        })
        .collect()
}

/// The tracker list. A tracker that answered its last announce is `ok`; one that did
/// not says the reason it gave.
pub(super) fn tracker_rows(trackers: Vec<TrackerStatus>) -> Vec<TrackerRow> {
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

/// One line about a torrent the session could not take resume data for, naming it by
/// the hash its row carries.
pub(super) fn left_out(hashes: &InfoHashes, error: &SessionError) -> String {
    format!(
        "{} was left out of the saved state: {}",
        TorrentId::from_wire_hash(hashes.wire_hash()).0,
        clean(&error.to_string())
    )
}
