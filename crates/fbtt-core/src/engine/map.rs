//! Mapping between what the engine reports and what the table shows.
//!
//! The engine counts bytes per second and names its states in its own vocabulary;
//! the table wants KiB/s, a stalled column of its own, and only the text a person
//! can read. Everything crossing here is cut and cleaned on the way.
//!
//! This stays inside the crate: it is how [`crate::local`]'s snapshot is built, and an HTTP
//! server answers in its own shapes rather than these rows.

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
pub(crate) const STALL_AFTER: Duration = Duration::from_secs(30);

/// Text from a torrent file, a peer, a tracker or an error, cut and cleaned the way
/// every remote string is before it is drawn.
pub(crate) fn clean(text: &str) -> String {
    fbt::display_safe(text, fbt::MAX_DISPLAY_CHARS)
}

/// KiB/s as the engine counts it. A value too large to convert caps out instead of
/// wrapping into a small limit.
pub(crate) fn rate_bytes(kib: u64) -> u64 {
    kib.saturating_mul(KIB)
}

/// The limits the session is running under, read back from it rather than from what
/// was last asked for, in KiB/s. A limit that is not a whole KiB shows as the KiB
/// below it.
pub(crate) fn limits(session: &Session) -> LiveLimits {
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

/// Where the stalled clock stands after one more look at a torrent.
///
/// It keeps running only while the torrent is downloading and taking nothing. Every other
/// moment - stopped, queued, checking, waiting for metadata, or actually taking bytes -
/// resets it to `now`, so the count measures how long a *running* download has been getting
/// nothing and not how long since the torrent was last moving in some wider sense. A torrent
/// resumed after an hour paused therefore starts from zero, which is what someone reading the
/// stalled column expects: `previous` is the value from the last look, `None` the first time
/// this torrent appears in a snapshot.
pub(crate) fn stalled_clock(
    state: TorrentState,
    down_rate: u64,
    previous: Option<Instant>,
    now: Instant,
) -> Instant {
    if state == TorrentState::Downloading && down_rate == 0 {
        previous.unwrap_or(now)
    } else {
        now
    }
}

/// What the table says a torrent is doing.
///
/// `zero_since` comes from [`stalled_clock`]: since when this torrent has been downloading
/// while taking nothing. A download that has done that for [`STALL_AFTER`] is stalled - it is
/// running, but nothing is arriving, and a person scanning the table needs to see that rather
/// than a bar stuck at 40 %.
pub(crate) fn row_state(
    state: TorrentState,
    progress: f64,
    down_rate: u64,
    zero_since: Instant,
    now: Instant,
) -> RowState {
    match state {
        TorrentState::DownloadingMetadata => RowState::Metadata,
        // Waiting for a check slot stops a torrent exactly like waiting for a place
        // in the queue, so the table shows one thing for both.
        TorrentState::QueuedForChecking | TorrentState::Queued => RowState::Queued,
        TorrentState::CheckingFiles { progress } => RowState::Checking(progress),
        TorrentState::Downloading => {
            let waiting = now.checked_duration_since(zero_since);
            // A rate above zero means the clock was reset this very tick, and everything at
            // 100 % is about to be seeding: neither belongs in the stalled column.
            if down_rate == 0
                && progress < 1.0
                && waiting.is_some_and(|waiting| waiting >= STALL_AFTER)
            {
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
    }
}

/// One row from one torrent's status.
///
/// `zero_since` is where [`stalled_clock`] left this torrent's stalled count at the look that
/// produced `status`, so the row and the map agree about what the torrent has been doing.
pub(crate) fn row(status: &TorrentStatus, zero_since: Instant, now: Instant) -> TorrentRow {
    TorrentRow {
        id: TorrentId::from_wire_hash(status.info_hashes.wire_hash()),
        name: clean(&status.name),
        progress: status.progress,
        state: row_state(
            status.state,
            status.progress,
            status.download_rate,
            zero_since,
            now,
        ),
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
pub(crate) fn file_rows(files: Vec<FileStatus>) -> Vec<FileRow> {
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
pub(crate) fn priority(priority: FilePriority) -> Priority {
    match priority {
        FilePriority::Skip => Priority::Skip,
        FilePriority::Low => Priority::Low,
        FilePriority::Normal => Priority::Normal,
        FilePriority::High => Priority::High,
    }
}

/// The peer list. `progress` is the share of the torrent's pieces the peer has: it
/// is always known once the metadata is, and 0.0 before that, so a bar can be drawn.
pub(crate) fn peer_rows(peers: Vec<PeerStatus>) -> Vec<PeerRow> {
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
pub(crate) fn tracker_rows(trackers: Vec<TrackerStatus>) -> Vec<TrackerRow> {
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
pub(crate) fn left_out(hashes: &InfoHashes, error: &SessionError) -> String {
    format!(
        "{} was left out of the saved state: {}",
        TorrentId::from_wire_hash(hashes.wire_hash()).0,
        clean(&error.to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::{STALL_AFTER, row_state, stalled_clock};
    use crate::client::RowState;
    use fbt::TorrentState;
    use std::time::{Duration, Instant};

    /// One step of test time: long enough to stand for a tick, short enough that the 30 s
    /// rule is a handful of steps rather than a wait.
    const STEP: Duration = Duration::from_secs(1);

    /// A torrent seen now has no clock, one running at zero rate keeps whatever clock it had,
    /// and the column turns stalled only once the gap reaches 30 s.
    #[test]
    fn stalled_after_30s_zero_rate() {
        let started = Instant::now();
        assert_eq!(
            stalled_clock(TorrentState::Downloading, 0, Some(started), started + STEP),
            started,
            "the count keeps running while a download takes nothing"
        );

        let seen_now = stalled_clock(TorrentState::Downloading, 0, None, started);
        assert_eq!(
            seen_now, started,
            "a torrent first seen starts its own count"
        );
        assert_eq!(
            row_state(TorrentState::Downloading, 0.4, 0, seen_now, started + STEP),
            RowState::Downloading,
            "one tick of nothing is not a stall"
        );

        assert_eq!(
            row_state(
                TorrentState::Downloading,
                0.4,
                0,
                started,
                started + STALL_AFTER - STEP
            ),
            RowState::Downloading,
            "29 s is a slow tracker reply, not a stalled download"
        );
        assert_eq!(
            row_state(
                TorrentState::Downloading,
                0.4,
                0,
                started,
                started + STALL_AFTER
            ),
            RowState::Stalled
        );
    }

    /// The count does not carry over across the moments when the torrent was not downloading:
    /// resume must never look stalled because it was paused for an hour.
    #[test]
    fn not_stalled_right_after_resume() {
        let stopped_at = Instant::now();
        let mut zero_since = stopped_at;
        for step in 0u32..60 {
            let now = stopped_at + step * STEP;
            for state in [
                TorrentState::Paused,
                TorrentState::Queued,
                TorrentState::QueuedForChecking,
                TorrentState::DownloadingMetadata,
                TorrentState::CheckingFiles { progress: 0.5 },
            ] {
                zero_since = stalled_clock(state, 0, Some(zero_since), now);
                assert_eq!(zero_since, now, "{state:?} does not count towards a stall");
            }
        }

        // A minute after the torrent entered the library and one tick after it resumed: the
        // minute it spent stopped is not this download's history.
        let resumed_at = stopped_at + 61 * STEP;
        assert_eq!(
            row_state(TorrentState::Downloading, 0.4, 0, zero_since, resumed_at),
            RowState::Downloading,
            "a resumed download starts the count again"
        );

        assert_eq!(
            stalled_clock(
                TorrentState::Downloading,
                4096,
                Some(stopped_at),
                resumed_at
            ),
            resumed_at,
            "bytes arriving end the count"
        );
    }

    /// Everything at 100 % and a zero rate is an ordinary moment, not a stall, and no clock
    /// changes what the other states say.
    #[test]
    fn not_stalled_when_complete() {
        let now = Instant::now();
        let long_ago = now - 2 * STALL_AFTER;

        assert_eq!(
            row_state(TorrentState::Downloading, 1.0, 0, long_ago, now),
            RowState::Downloading,
            "a download that has everything is about to seed, not stalled"
        );
        assert_eq!(
            row_state(TorrentState::Downloading, 0.4, 1, long_ago, now),
            RowState::Downloading,
            "a torrent taking bytes is not stalled however long the gap before it"
        );

        assert_eq!(
            row_state(TorrentState::Paused, 0.4, 0, long_ago, now),
            RowState::Paused
        );
        assert_eq!(
            row_state(TorrentState::Seeding, 1.0, 0, long_ago, now),
            RowState::Seeding
        );
        assert_eq!(
            row_state(TorrentState::DownloadingMetadata, 0.0, 0, long_ago, now),
            RowState::Metadata
        );
        assert_eq!(
            row_state(TorrentState::Queued, 0.4, 0, long_ago, now),
            RowState::Queued
        );
        assert_eq!(
            row_state(
                TorrentState::CheckingFiles { progress: 0.25 },
                0.1,
                0,
                long_ago,
                now
            ),
            RowState::Checking(0.25)
        );
        assert_eq!(
            row_state(TorrentState::Error, 0.4, 0, long_ago, now),
            RowState::Error
        );
    }
}
