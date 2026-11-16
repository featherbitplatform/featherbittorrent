//! The client interface: commands go in, immutable snapshots come out.
//!
//! Nothing here knows what runs the torrents. A screen sends a [`Command`] and
//! never waits for it; what it draws is the newest [`Snapshot`], which the backend
//! replaces whole. Notices are a third stream, one line at a time, because telling
//! the user that a magnet was refused is not the same job as listing torrents: a
//! dropped notice is a missed toast, while a dropped snapshot would draw the wrong
//! library.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use crate::config::Limits;

/// Commands the backend queues before [`Client::send`] starts refusing them. It
/// handles one at a time and each is short, so 64 is more than a person can type.
pub(crate) const COMMAND_QUEUE: usize = 64;
/// Notices queued for the screen before they are dropped. A screen that reads one
/// per frame cannot fall behind by more than this many.
pub(crate) const NOTICE_QUEUE: usize = 64;

/// One torrent, named by the hash every peer knows it by: the lowercase hex of
/// [`fbt::InfoHashes::wire_hash`](fbt::InfoHashes::wire_hash), 40 digits, the same
/// spelling fbt-api uses in its URLs. A remote backend from milestone 2 gets ids
/// over HTTP, so ids stay strings here rather than a hash type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TorrentId(pub String);

impl TorrentId {
    /// The id of the torrent known by `hash`.
    pub fn from_wire_hash(hash: [u8; 20]) -> TorrentId {
        let mut text = String::with_capacity(hash.len() * 2);
        for byte in hash {
            // Writing to a String does not fail.
            let _ = write!(&mut text, "{byte:02x}");
        }
        TorrentId(text)
    }
}

/// What a torrent is doing, as the table shows it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RowState {
    /// A magnet whose metadata has not arrived from peers yet.
    Metadata,
    /// Existing data is being hash-checked; the fraction is for its bar.
    Checking(f64),
    /// Stopped and waiting for a place in the session's queue.
    Queued,
    /// Running, with pieces still missing.
    Downloading,
    /// [`Downloading`](Self::Downloading) at zero download rate for at least 30 s:
    /// it is running, but nothing is arriving.
    Stalled,
    /// Every wanted byte is on disk.
    Seeding,
    /// Stopped by the user.
    Paused,
    /// Stopped by a storage failure or bad metadata; see [`TorrentRow::error`].
    Error,
}

/// One row of the torrent table.
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentRow {
    /// The torrent's id.
    pub id: TorrentId,
    /// Its name, already fit to draw.
    pub name: String,
    /// Bytes done over bytes wanted, `0.0..=1.0`.
    pub progress: f64,
    /// What it is doing.
    pub state: RowState,
    /// Payload bytes per second coming in.
    pub down_rate: u64,
    /// Payload bytes per second going out.
    pub up_rate: u64,
    /// Connected peers.
    pub peers: u32,
    /// Connected peers that have every piece.
    pub seeds: u32,
    /// Bytes wanted by this torrent's file priorities.
    pub size: u64,
    /// Of those, bytes verified and on disk.
    pub done: u64,
    /// The directory its files are below.
    pub save_path: PathBuf,
    /// Why it stopped with [`RowState::Error`], fit to draw.
    pub error: Option<String>,
    /// Its place in the session's queue, from 0.
    pub queue_position: usize,
}

/// The rates and DHT count the header shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    /// Payload bytes per second coming in, over every torrent.
    pub down_rate: u64,
    /// Payload bytes per second going out, over every torrent.
    pub up_rate: u64,
    /// Nodes the DHT routing tables hold, `None` when no DHT runs.
    pub dht_nodes: Option<usize>,
}

/// The limits the running session is under, in the units a person states them in.
///
/// A rate of 0 KiB/s is unlimited and a queue count of `None` is unlimited, as in
/// [`Limits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveLimits {
    /// Download rate in KiB/s (0: unlimited).
    pub download_kib: u64,
    /// Upload rate in KiB/s (0: unlimited).
    pub upload_kib: u64,
    /// Torrents downloading at once.
    pub active_downloads: Option<usize>,
    /// Torrents seeding at once.
    pub active_seeds: Option<usize>,
    /// Torrents running at once, downloading or seeding.
    pub active_total: Option<usize>,
}

impl From<Limits> for LiveLimits {
    fn from(limits: Limits) -> Self {
        LiveLimits {
            download_kib: limits.download_kib,
            upload_kib: limits.upload_kib,
            active_downloads: limits.active_downloads,
            active_seeds: limits.active_seeds,
            active_total: limits.active_total,
        }
    }
}

impl From<LiveLimits> for Limits {
    fn from(limits: LiveLimits) -> Self {
        Limits {
            download_kib: limits.download_kib,
            upload_kib: limits.upload_kib,
            active_downloads: limits.active_downloads,
            active_seeds: limits.active_seeds,
            active_total: limits.active_total,
        }
    }
}

/// How much of a file to download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilePriority {
    /// Not downloaded.
    Skip,
    /// After everything else.
    Low,
    /// The default.
    Normal,
    /// First.
    High,
}

/// One file of a torrent. Pad files the engine keeps for piece alignment are not
/// listed: they have no content of their own.
#[derive(Debug, Clone, PartialEq)]
pub struct FileRow {
    /// Its index in the torrent's file list, pad files counted. This is what
    /// [`Command::SetFilePriority`] names.
    pub index: usize,
    /// Its path below the torrent's directory, fit to draw.
    pub path: String,
    /// Its length in bytes.
    pub size: u64,
    /// Bytes of it inside verified pieces.
    pub done: u64,
    /// What is being downloaded of it.
    pub priority: FilePriority,
}

/// One connected peer of a torrent.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerRow {
    /// Its address, ours or theirs depending on who dialled.
    pub addr: String,
    /// What it says it is, fit to draw; empty when it says nothing.
    pub client: String,
    /// Payload bytes per second coming from it.
    pub down_rate: u64,
    /// Payload bytes per second going to it.
    pub up_rate: u64,
    /// The share of the torrent's pieces it has, `None` when it is not known yet.
    pub progress: Option<f64>,
}

/// One tracker of a torrent under one announced hash.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackerRow {
    /// Its announce URL, fit to draw.
    pub url: String,
    /// Its tier, from 0.
    pub tier: usize,
    /// What the last announce did, fit to draw: `ok`, or the tracker's own reason.
    pub status: String,
    /// Seeders in its last reply, when it said.
    pub seeders: Option<u32>,
    /// Leechers in its last reply, when it said.
    pub leechers: Option<u32>,
}

/// The pane under the table.
#[derive(Debug, Clone, PartialEq)]
pub enum Detail {
    /// The torrent's files.
    Files(Vec<FileRow>),
    /// Its connected peers.
    Peers(Vec<PeerRow>),
    /// Its trackers.
    Trackers(Vec<TrackerRow>),
    /// One entry per piece, set when we have it verified.
    Pieces {
        /// Which pieces are on disk, in piece order.
        have: Vec<bool>,
    },
}

/// Which pane to watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetailTab {
    /// The file list.
    Files,
    /// The peer list.
    Peers,
    /// The tracker list.
    Trackers,
    /// The piece bitmap.
    Pieces,
}

/// Where the torrents are coming from.
#[derive(Debug, Clone, PartialEq)]
pub enum Connection {
    /// The engine in this process.
    Local,
    /// A server reached over the network (milestone 2).
    Remote {
        /// What to call it in the header.
        name: String,
        /// Whether the connection is encrypted.
        secure: bool,
        /// How we are signed in, fit to draw.
        auth: String,
        /// Whether the link holds, right now.
        state: LinkState,
    },
}

/// How a link to a server stands, as the header shows it.
///
/// A server that goes away does not stop the screen: the last snapshot stays on it and this says why, so
/// a person knows whether the numbers they are looking at are current or remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// Nothing has come back from that address yet.
    Connecting,
    /// The last exchange worked.
    Connected,
    /// The server went away and this client is dialling again: the snapshot on the screen is whatever
    /// was last good.
    Lost {
        /// Seconds until the next attempt, which doubles from 1 while it keeps failing.
        retry_in_secs: u64,
    },
}

/// The folders a destination may be picked from, as one answer to [`Command::ListDirs`].
///
/// With `path` `None` this is only the list of places to start from; naming a folder asks what is directly
/// inside it. A failure belongs in `error` rather than in a notice: a picker shows the reason beside the
/// folder it failed on, and it needs `roots` to fall back on while it does - one line about the failure
/// alone would say the same thing twice and take the folders out of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirListing {
    /// The folder that was asked for; `None` for the list of places to start from.
    pub path: Option<PathBuf>,
    /// The folders this backend may name at all, spelled as they were configured: what a picker starts
    /// from, and where it goes back to.
    pub roots: Vec<PathBuf>,
    /// The directories directly inside `path`: sorted, nothing hidden, names fit to draw, at most two
    /// thousand of them, and no file among them. Empty when `path` is `None`, where `roots` is the list
    /// to show.
    pub dirs: Vec<String>,
    /// Why that folder could not be listed, fit to draw; `None` when it could.
    pub error: Option<String>,
}

/// What the screen asks about the devices a server lets sign in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceCommand {
    /// The devices this server knows, as [`Snapshot::devices`].
    List,
    /// Let `name` sign in with `public_key`, spelled as an OpenSSH public key line.
    Add {
        /// What to call this device.
        name: String,
        /// The key it signs logins with.
        public_key: String,
    },
    /// Stop the device called `name` from signing in.
    Remove(String),
}

/// One device a server lets sign in: what it was named, and the short form of the key behind that name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    /// The name this server's person gave it.
    pub name: String,
    /// Its key's fingerprint, `SHA256:<base64>`: what a person checks before trusting the name.
    pub fingerprint: String,
}

/// Everything one screen draws, as one value.
///
/// A snapshot is replaced whole rather than edited, so a screen can hold one while
/// the backend builds the next, and comparing two says whether anything changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// The engine or server these torrents come from.
    pub connection: Connection,
    /// Every torrent, in the order the backend holds them (its queue order).
    pub torrents: Vec<TorrentRow>,
    /// Rates and DHT count for the header.
    pub totals: Totals,
    /// The limits the session is running under.
    pub limits: LiveLimits,
    /// The pane under the table: which torrent it describes and what it holds.
    /// `None` when nothing is watched, and while a watched torrent's detail cannot
    /// be taken (its metadata has not arrived, or it was removed).
    pub detail: Option<(TorrentId, Detail)>,
    /// The directory new torrents go to unless the dialog names another.
    pub default_dir: PathBuf,
    /// The last folder listing that was asked for with [`Command::ListDirs`], and the answer to it;
    /// `None` until one is asked for. See [`DirListing`].
    pub dirs: Option<DirListing>,
    /// The devices a server lets sign in, from the last [`Command::Devices`]; `None` until one is asked
    /// for, which is also what a library with no server to manage them keeps showing.
    pub devices: Option<Vec<DeviceRow>>,
}

/// What a torrent is being added from.
#[derive(Debug, Clone, PartialEq)]
pub enum AddSource {
    /// A magnet link: the metadata comes from peers (BEP 9).
    Magnet(String),
    /// The bytes of a `.torrent` file.
    File(Vec<u8>),
}

/// What the screen asks the backend to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Add a torrent, into `dir` or into [`Snapshot::default_dir`] when it is `None`.
    Add {
        /// The magnet or `.torrent` to add.
        source: AddSource,
        /// The directory its files go below.
        dir: Option<PathBuf>,
    },
    /// Stop a torrent: no peers, no announces.
    Pause(TorrentId),
    /// Undo [`Pause`](Command::Pause).
    Resume(TorrentId),
    /// Forget a torrent, and its files too when asked.
    Remove {
        /// The torrent to forget.
        id: TorrentId,
        /// Delete the bytes it downloaded.
        delete_files: bool,
    },
    /// Change what is downloaded of one file.
    SetFilePriority {
        /// The torrent whose file changes.
        id: TorrentId,
        /// The [`FileRow::index`] of the file.
        file: usize,
        /// What to download of it now.
        priority: FilePriority,
    },
    /// Change the running rates and queue counts, and save them.
    SetLimits(LiveLimits),
    /// Which pane to keep fresh, or nothing.
    Watch(Option<(TorrentId, DetailTab)>),
    /// What folders may be named as a destination: the places to start from when `None`, otherwise the
    /// directories directly inside that folder. The answer is [`Snapshot::dirs`].
    ListDirs(Option<PathBuf>),
    /// The devices a server lets sign in, and what to do with them. The answer is
    /// [`Snapshot::devices`]; a library running its own engine has no device list to give and says so.
    Devices(DeviceCommand),
    /// Save everything and stop. Dropping every [`Client`] is the same as this.
    Shutdown,
}

/// One line for the user.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// Something worked.
    Info(String),
    /// Something did not, with the reason fit to draw.
    Error(String),
    /// A sign-in was refused with the credential this client signs with, drawn as an error is (it is bad
    /// news about the same thing) but kept apart because somebody has to *do* something about it: a run that
    /// read that password out of its own keychain now knows the entry is wrong, which is a different answer
    /// from "this request did not work". What it carries is the reason, fit to draw.
    SignInRefused(String),
}

/// The backend's half of the channels a [`Client`] is built from.
pub(crate) struct Channels {
    /// Commands the screen sent.
    pub(crate) commands: mpsc::Receiver<Command>,
    /// Where the backend publishes snapshots.
    pub(crate) snapshot: watch::Sender<Arc<Snapshot>>,
    /// Where the backend reports one-line feedback.
    pub(crate) notices: mpsc::Sender<Notice>,
}

/// The sending half of the command queue, so a runtime can hold it beside the two
/// receivers without holding a whole [`Client`]. Cloning it is cloning the queue's
/// handle: every clone must go before the backend sees an empty queue.
#[derive(Clone, Debug)]
pub struct Commands {
    commands: mpsc::Sender<Command>,
}

impl Commands {
    /// Hands `command` to the backend without waiting, with exactly
    /// [`Client::send`](Client::send)'s contract: false means the queue was full or
    /// the backend has gone.
    pub fn send(&self, command: Command) -> bool {
        self.commands.try_send(command).is_ok()
    }
}

/// A [`Client`] taken apart by [`split`](Client::split): the fields a runtime waits on
/// and sends through, so each lives on its own and no borrow of one fights another in
/// the same `tokio::select!`.
#[derive(Debug)]
pub struct ClientParts {
    /// Commands out to the backend. Clonable, and every clone has to go - with the
    /// two receivers below - before the backend notices nobody is listening.
    pub commands: Commands,
    /// The snapshots, newest first: `changed()` wakes on a publish and
    /// `borrow_and_update()` takes the value that woke it.
    pub snapshots: watch::Receiver<Arc<Snapshot>>,
    /// The notices, oldest first. Dropping this one does not stop the backend; its
    /// queue is separate from the commands that keep it alive.
    pub notices: mpsc::Receiver<Notice>,
}

/// The screen's side of a backend: commands out, snapshots and notices in.
///
/// None of its methods waits on the engine, so the thread that draws with them can
/// keep drawing. Dropping it closes the command queue, which is how a backend finds
/// out that nobody is watching any more.
#[derive(Debug)]
pub struct Client {
    commands: mpsc::Sender<Command>,
    snapshot: watch::Receiver<Arc<Snapshot>>,
    notices: mpsc::Receiver<Notice>,
    /// The engine this client's backend drives, for the tests that need to hand it
    /// a peer. Only present with the `test-support` feature.
    #[cfg(feature = "test-support")]
    session: Option<fbt::Session>,
}

impl Client {
    /// Makes the two halves of a connection to a backend, with `initial` as the
    /// snapshot until the backend publishes its first one.
    pub(crate) fn paired(initial: Arc<Snapshot>) -> (Client, Channels) {
        let (commands_tx, commands_rx) = mpsc::channel(COMMAND_QUEUE);
        let (snapshot_tx, snapshot_rx) = watch::channel(initial);
        let (notices_tx, notices_rx) = mpsc::channel(NOTICE_QUEUE);
        (
            Client {
                commands: commands_tx,
                snapshot: snapshot_rx,
                notices: notices_rx,
                #[cfg(feature = "test-support")]
                session: None,
            },
            Channels {
                commands: commands_rx,
                snapshot: snapshot_tx,
                notices: notices_tx,
            },
        )
    }

    /// The engine this client's backend drives. Test support only, and `None` for a
    /// client from [`LocalBackend::start`](crate::LocalBackend::start).
    #[cfg(feature = "test-support")]
    pub(crate) fn with_session(mut self, session: &fbt::Session) -> Self {
        self.session = Some(session.clone());
        self
    }

    /// The engine this client's backend drives, for handing it a peer in a test.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn session(&self) -> Option<&fbt::Session> {
        self.session.as_ref()
    }

    /// Hands `command` to the backend without waiting.
    ///
    /// False means the queue was full or the backend has gone: the screen should
    /// not try again until something changes, but nothing is broken and no command
    /// already accepted is undone.
    pub fn send(&self, command: Command) -> bool {
        self.commands.try_send(command).is_ok()
    }

    /// The newest snapshot. This cannot block: the backend builds a snapshot before
    /// it publishes, so taking one is a clone of an [`Arc`].
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.borrow().clone()
    }

    /// The receiver behind [`snapshot`](Self::snapshot), for a runtime that wants to
    /// act only when something changed: its `changed()` resolves once the backend
    /// has published a different snapshot.
    pub fn changed(&mut self) -> &mut watch::Receiver<Arc<Snapshot>> {
        &mut self.snapshot
    }

    /// Takes the client's three channels apart. A runtime that selects on snapshots
    /// beside keys and notices needs the receivers as fields it owns: a method on
    /// `self` borrows the whole client, and one `select!` branch cannot hold that
    /// borrow while another sends on the same client. The client is consumed; letting
    /// every part go is what tells the backend nobody is listening, exactly as
    /// dropping the client did.
    pub fn split(self) -> ClientParts {
        let Client {
            commands,
            snapshot,
            notices,
            ..
        } = self;
        ClientParts {
            commands: Commands { commands },
            snapshots: snapshot,
            notices,
        }
    }

    /// The next notice, if there is one waiting. A screen that draws every frame
    /// takes notices this way so it never waits on the engine.
    pub fn try_notice(&mut self) -> Option<Notice> {
        self.notices.try_recv().ok()
    }

    /// Waits for the next notice; `None` once the backend has stopped and every
    /// notice is read.
    pub async fn notice(&mut self) -> Option<Notice> {
        self.notices.recv().await
    }
}
