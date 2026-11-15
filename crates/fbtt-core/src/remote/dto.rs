//! What the server's routes answer, as those answers are shaped.
//!
//! Every struct here is `#[serde(default)]` with no required fields: a server that stops sending a field
//! costs this client one column of the table, not the whole refresh. That is only safe because each field
//! has an obvious empty value and [`crate::remote::map`] knows what an empty one means.

use serde::{Deserialize, Serialize};

/// One torrent, as `GET /api/v1/torrents` and `GET /api/v1/torrents/{id}` answer it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct TorrentDto {
    /// The id every other route names this torrent by: the 40-hex wire hash.
    pub(crate) id: String,
    /// Its name; a magnet's `dn` before its metadata arrives, and empty when it has none yet.
    pub(crate) name: String,
    /// `downloading_metadata`, `queued_for_checking`, `checking_files`, `downloading`, `seeding`,
    /// `paused`, `queued` or `error`.
    pub(crate) state: String,
    /// The share of the file check done, while `state` says it is checking.
    pub(crate) checking_progress: Option<f64>,
    /// Bytes done over bytes wanted.
    pub(crate) progress: f64,
    /// Payload bytes per second coming in and going out.
    pub(crate) download_rate: u64,
    /// ...and the other direction.
    pub(crate) upload_rate: u64,
    /// Connected peers, and of those the ones that have every piece.
    pub(crate) num_peers: u32,
    /// ...the seeds.
    pub(crate) num_seeds: u32,
    /// Bytes this torrent's file priorities want, and of those what is verified on disk.
    pub(crate) total_wanted: u64,
    /// ...what is done.
    pub(crate) wanted_done: u64,
    /// Why it stopped with `error`.
    pub(crate) error: Option<String>,
    /// The directory its files are below.
    pub(crate) save_path: String,
    /// Its place in the server's queue, from 0.
    pub(crate) queue_position: usize,
}

/// One file of a torrent, as `GET .../files` answers it. Pad files stay in the list here: their
/// indices are what a priority change names rows by, and they leave only on the way to the pane.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct FileDto {
    /// Its index in the file list, pad files counted.
    pub(crate) index: usize,
    /// Its path below the torrent's directory.
    pub(crate) path: String,
    /// Its length in bytes, and what is inside verified pieces.
    pub(crate) len: u64,
    /// ...what is done.
    pub(crate) downloaded: u64,
    /// A BEP 47 pad file, which holds part of a piece and has no content of its own.
    pub(crate) pad: bool,
    /// `skip`, `low`, `normal` or `high`.
    pub(crate) priority: String,
}

/// One connected peer, as `GET .../peers` answers it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct PeerDto {
    /// Its address.
    pub(crate) addr: String,
    /// What it says it is, when it says anything.
    pub(crate) client: Option<String>,
    /// Payload bytes per second in each direction.
    pub(crate) download_rate: u64,
    /// ...the other way.
    pub(crate) upload_rate: u64,
    /// The share of the torrent's pieces it has.
    pub(crate) progress: f64,
}

/// One tracker of a torrent, as `GET .../trackers` answers it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct TrackerDto {
    /// Its announce URL.
    pub(crate) url: String,
    /// Its tier, from 0.
    pub(crate) tier: usize,
    /// What the last announce said went wrong, when it said anything.
    pub(crate) last_error: Option<String>,
    /// Seeders and leechers in its last reply, when it gave them.
    pub(crate) seeders: Option<u32>,
    /// ...the leechers.
    pub(crate) leechers: Option<u32>,
}

/// Which pieces a torrent has, as `GET .../pieces` answers it: the bitfield as hex, piece 0 in the
/// top bit of the first byte (BEP 3 order).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct PiecesDto {
    /// How many pieces there are.
    pub(crate) num_pieces: u32,
    /// The bitmap, lowercase hex.
    pub(crate) have: String,
}

/// What the session is doing and under what rates, as `GET /api/v1/session` answers it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SessionDto {
    /// Nodes in the server's DHT routing tables; null when no DHT runs there.
    pub(crate) dht_nodes: Option<u64>,
    /// Session-wide download and upload limits in bytes per second; 0 is unlimited.
    pub(crate) download_limit: u64,
    /// ...the upload one.
    pub(crate) upload_limit: u64,
}

/// The server's queue counts, as `GET`/`PUT /api/v1/session/queue` spells them. A `PUT` replaces the
/// whole set and refuses a field it does not know, so this is read back and sent again with only the
/// three counts changed - which is why every field is here even though the table draws four numbers.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct QueueDto {
    /// Torrents downloading at once; null is unlimited.
    pub(crate) active_downloads: Option<usize>,
    /// Torrents seeding at once.
    pub(crate) active_seeds: Option<usize>,
    /// Torrents running at once.
    pub(crate) active_limit: Option<usize>,
    /// Whether a slow torrent may run beside the limits.
    pub(crate) dont_count_slow_torrents: bool,
    /// The rates below which a torrent may count as slow.
    pub(crate) inactive_down_rate: u64,
    /// ...the upload one.
    pub(crate) inactive_up_rate: u64,
    /// How long a torrent runs before it may count as slow, and how often the queue is revisited, in
    /// seconds.
    pub(crate) startup_grace: f64,
    /// ...the interval.
    pub(crate) interval: f64,
}

/// One device a server lets sign in.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct DeviceDto {
    /// What its person named it.
    pub(crate) name: String,
    /// The short form of the key behind that name, `SHA256:<base64>`.
    pub(crate) fingerprint: String,
}

/// The directories inside one folder, as `GET /api/v1/fs/list` answers them.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ListingDto {
    /// The folder that was asked for, spelled as it was asked for.
    pub(crate) path: String,
    /// The directories directly inside it: names, not paths.
    pub(crate) dirs: Vec<String>,
}

/// The body of `POST /api/v1/torrents` when the torrent comes from a magnet link.
#[derive(Debug, Serialize)]
pub(crate) struct MagnetBody<'a> {
    /// The `magnet:` URI.
    pub(crate) magnet: &'a str,
    /// The directory its files go below, which this API insists on naming.
    pub(crate) save_path: &'a str,
}

/// The body of `PUT /api/v1/torrents/{id}/file-priorities`: one priority per file, pad files
/// included, in file order.
#[derive(Debug, Serialize)]
pub(crate) struct PrioritiesBody<'a> {
    /// The priorities, spelled the way the API spells them.
    pub(crate) priorities: Vec<&'a str>,
}

/// The body of `PUT /api/v1/session/limits`: bytes per second, 0 unlimited.
#[derive(Debug, Serialize)]
pub(crate) struct LimitsBody {
    /// The download limit.
    pub(crate) download: u64,
    /// The upload limit.
    pub(crate) upload: u64,
}

/// The body of `POST /api/v1/auth/devices`: a name and the one-line OpenSSH key behind it.
#[derive(Debug, Serialize)]
pub(crate) struct NewDeviceBody<'a> {
    /// What to call this device.
    pub(crate) name: &'a str,
    /// The key it signs logins with.
    pub(crate) public_key: &'a str,
}
