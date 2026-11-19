//! The backend over a server's HTTP API: commands in, snapshots out, same as the local one.
//!
//! A screen cannot tell the two apart. It sends [`Command`]s and draws [`Snapshot`]s here exactly as it
//! does for [`crate::local`], because everything that is different about a server - that it answers over
//! HTTP, that its answer can be wrong or late or never come, that it can go away while somebody is looking
//! at it - is dealt with inside this module and turned into the two things a screen already understands:
//! a snapshot that says what was last known, and a line about what happened.
//!
//! One task owns one connection. It refreshes on a second's cadence, wakes early when the server's event
//! stream says something changed, keeps the last good table when the link goes down, and dials again with a
//! pause that doubles. Starting a connection therefore never fails: an address that is not one, or a
//! password login with no user name in it, arrives as [`LinkState::Lost`] and a notice, which is what the
//! connect screen needs in order to say why nothing appeared.
//!
//! Three things about this arrangement are load-bearing and worth saying out loud:
//!
//! - Only the refresh loop logs in. The event stream dials with whatever credential that loop already has,
//!   and only while it says the link is up, so a wrong password costs one refused login per backoff pause
//!   rather than two per second.
//! - Wakes coalesce. A burst of events during one refresh is one extra refresh, not one per event.
//! - `Shutdown` is answered by a task that is never busy (`relay`, below), so it interrupts a request that a
//!   silent server is still holding open instead of queueing behind it.

mod backend;
mod bodies;
mod commands;
pub(crate) mod device;
pub(crate) mod dto;
pub(crate) mod gateway;
pub(crate) mod http;
pub(crate) mod login;
pub(crate) mod map;
pub(crate) mod relay;
mod routes;
mod sse;
mod stopping;
mod stream;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, mpsc, watch};
use zeroize::Zeroizing;

use crate::client::{
    COMMAND_QUEUE, Client, Command, Connection, Detail, DetailTab, DeviceRow, DirListing,
    LinkState, LiveLimits, Notice, Snapshot, TorrentId, TorrentRow, Totals,
};
use crate::engine::map::clean;
use crate::ui_state::{LoginMethod, RemoteTarget};

use http::Api;
use stopping::Stopping;

// The gateway header rule the connect screen asks before it lets an answer go out, re-exported from the
// transport that enforces it: one list of names, asked in two places (see [`gateway_header_name_problem`]).
pub use http::{GatewayHeaderNameProblem, gateway_header_name_problem};

/// How often the table is rebuilt when nothing else wakes it. A second is what the local backend's half
/// second buys over HTTP: rates that look alive, a stalled column that ticks, and a server that is not
/// asked anything it does not need to answer.
const REFRESH: Duration = Duration::from_secs(1);

/// How often the queue counts (and with them the limits the header shows) are re-read. The table changes
/// every second; who is queued where moves when a torrent finishes.
const SLOW_REFRESH: Duration = Duration::from_secs(10);

/// The ladder of pauses between attempts at a server that is not answering, capped at half a minute so a
/// server that is down for the evening is asked about once every thirty seconds rather than once a second.
/// The event stream uses the same ladder for its own reconnects (see [`sse`]).
const BACKOFF: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
];

/// The words every sign-in refusal this client puts on a screen starts with.
///
/// Not the seam a screen's owner acts on any more - `Notice::SignInRefused` is that - but still the wording
/// every refusal shares, whether it arrives as that variant or folded into somebody else's sentence: the line
/// inside a folder listing that failed for this reason, the reason in a sentence about the event stream. Kept
/// here so no call site invents its own way of saying bad news about a credential.
pub(crate) const SIGN_IN_REFUSED: &str = "sign-in was refused";

/// What a connection uses to prove who is asking.
///
/// `Debug` names the kind and nothing else: this is the one value in a remote connection that carries a
/// secret, and a target or a setting that gets logged must not repeat it.
pub enum Secret {
    /// No credential at all: the server runs with authentication switched off.
    None,
    /// A user name and password. The password is overwritten when this value is dropped.
    Password(Zeroizing<String>),
    /// The passphrase of a device key. `Some` opens the key file named by [`LoginMethod::KeyFile`]; `None`
    /// means an unencrypted file, or - with [`LoginMethod::Agent`] - that ssh-agent holds the key and its
    /// bytes are never read at all.
    KeyPassphrase(Option<Zeroizing<String>>),
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Which kind of credential, which is everything worth printing about one.
        f.write_str(match self {
            Secret::None => "Secret::None",
            Secret::Password(_) => "Secret::Password(<redacted>)",
            Secret::KeyPassphrase(_) => "Secret::KeyPassphrase(<redacted>)",
        })
    }
}

/// What the refresh loop tells the event stream about this connection.
///
/// It exists so that exactly one task logs in: the stream may not dial, and so may not ask for a
/// credential, until a refresh has got through to this server with one (see [`sse`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    /// The link is down, or has never been proved: the stream asks the server for nothing.
    Down,
    /// A refresh got through, so a credential is in hand and the stream may use it.
    Up,
}

/// Drives the client interface over a server's HTTP API.
#[derive(Debug)]
pub struct RemoteBackend;

impl RemoteBackend {
    /// Starts a connection to `target` and hands back the handle a screen uses.
    ///
    /// `secret` is what this connection signs in with; `gateway_key` is the value of the header named by
    /// `target.gateway_key_header`, for a server behind a gateway that checks one before the request reaches
    /// it. A target that names such a header needs its key here as well as there - neither half on its own
    /// is something this client can send - and that pairing, like everything else about a target that cannot
    /// work, is reported on the client rather than to whoever called.
    ///
    /// Nothing here waits or fails: the address is dialed by the task this starts, so a server that is down
    /// says so on the client rather than to whoever called. A target and secret that do not go together - a
    /// password login with no user name, a gateway with no token endpoint - is reported the same way.
    pub fn start(
        target: RemoteTarget,
        secret: Secret,
        gateway_key: Option<Zeroizing<String>>,
    ) -> Client {
        let name = server_name(&target.url);
        let secure = target.url.starts_with("https://");
        let auth = method_name(target.login).to_string();
        let initial = Arc::new(blank(
            name.clone(),
            secure,
            auth.clone(),
            LinkState::Connecting,
        ));
        let (client, channels) = Client::paired(Arc::clone(&initial));

        match Api::try_new(&target, secret, gateway_key, Arc::new(Stopping::default())) {
            Err(problem) => {
                // Nothing can be asked of this address, so say it once and let the client answer commands
                // with nothing until it is dropped or asked to stop. The link goes to `Lost` rather than
                // staying `Connecting`: there is nothing here that a later attempt could fix, and a header
                // that spins forever on somebody's typo reads as a server that is still being reached.
                let _ = channels.notices.try_send(Notice::Error(clean(&problem)));
                let retry_in_secs = BACKOFF[0].as_secs();
                let _ = channels.snapshot.send(Arc::new(blank(
                    name,
                    secure,
                    auth,
                    LinkState::Lost { retry_in_secs },
                )));
                tokio::spawn(idle(channels.commands, channels.snapshot, channels.notices));
                client
            }
            Ok(api) => {
                let api = Arc::new(api);
                // One permit is enough however many events arrive meanwhile: this is the difference between
                // a stream that keeps a screen fresh and a stream that makes the loop re-read once per event.
                let wake = Arc::new(Notify::new());
                // The stream's permission to dial, which only the refresh loop moves. Dropping the sender at
                // the end of `run` is what ends the stream, so it cannot outlive this connection.
                let (gate_tx, gate_rx) = watch::channel(Gate::Down);
                sse::spawn(
                    Arc::clone(&api),
                    Arc::clone(&wake),
                    channels.notices.clone(),
                    gate_rx,
                );
                let backend = Backend {
                    api,
                    name,
                    secure,
                    auth,
                    roots: Vec::new(),
                    default_dir: PathBuf::new(),
                    watching: None,
                    dirs: None,
                    devices: None,
                    // The relay starts on the first `Stream` a gateway target is asked
                    // for, behind a lock the command's start waits on: starting it is
                    // a bind, and the loop answers commands rather than racing two of
                    // them onto the loopback at once.
                    relay: Mutex::new(None),
                    torrents: Vec::new(),
                    totals: initial.totals,
                    limits: initial.limits,
                    detail: None,
                    queue: None,
                    want_limits: true,
                    stalled: HashMap::new(),
                    state: LinkState::Connecting,
                    backoff: 0,
                    complained: false,
                    published: initial,
                    snapshot_tx: channels.snapshot,
                    notices: channels.notices,
                    wake,
                    gate: gate_tx,
                };
                let (to_backend, commands) = mpsc::channel(COMMAND_QUEUE);
                tokio::spawn(relay(channels.commands, to_backend, backend.api.stopping()));
                tokio::spawn(backend.run(commands));
                client
            }
        }
    }
}

/// The snapshot a connection shows before it has asked the server anything.
fn blank(name: String, secure: bool, auth: String, state: LinkState) -> Snapshot {
    Snapshot {
        connection: Connection::Remote {
            name,
            secure,
            auth,
            state,
        },
        torrents: Vec::new(),
        totals: Totals {
            down_rate: 0,
            up_rate: 0,
            dht_nodes: None,
        },
        limits: LiveLimits {
            download_kib: 0,
            upload_kib: 0,
            active_downloads: None,
            active_seeds: None,
            active_total: None,
        },
        detail: None,
        default_dir: PathBuf::new(),
        dirs: None,
        devices: None,
    }
}

/// Forwards commands to the loop that answers them, and says "stop waiting on the server" the moment one of
/// them is `Shutdown` - or the moment the screen has gone away altogether, which means the same thing.
///
/// It has to be a task of its own because the loop may be deep inside a request when the command arrives,
/// and a server that has gone quiet holds a request open for its whole timeout: somebody pressing stop
/// should not have to wait out that. The command itself still goes through, so the loop is the one that
/// ends the session and decides what the last snapshot says.
async fn relay(
    mut from_screen: mpsc::Receiver<Command>,
    to_backend: mpsc::Sender<Command>,
    stopping: Arc<Stopping>,
) {
    while let Some(command) = from_screen.recv().await {
        if matches!(command, Command::Shutdown) {
            stopping.stop();
        }
        // The loop is gone: nobody left to answer.
        if to_backend.send(command).await.is_err() {
            return;
        }
    }
    stopping.stop();
}

/// A client that was never able to reach anywhere: it answers `Shutdown` and drops the rest, so a screen
/// that connected to a bad address can still close the connection.
///
/// The two sender ends come along and are dropped only when this does. Left in [`RemoteBackend::start`] they
/// would go as soon as it returned, and a queue with no senders left reads to the run loop as "the backend
/// stopped and the queue is read out" - the screen would exit instead of showing the one line saying why
/// nothing appeared. Holding them here keeps that line reachable and the connection open until its screen
/// lets go or asks it to stop.
async fn idle(
    mut commands: mpsc::Receiver<Command>,
    _snapshot: watch::Sender<Arc<Snapshot>>,
    _notices: mpsc::Sender<Notice>,
) {
    while let Some(command) = commands.recv().await {
        if matches!(command, Command::Shutdown) {
            return;
        }
    }
}

/// The task's side of one connection: the server it talks to, and what a screen wants built on top of the
/// answers - the table, the watched pane, the folder list, the device list.
struct Backend {
    /// The server, its address and its credential. Shared with the event stream so a re-login on either
    /// half is a re-login on both.
    api: Arc<Api>,
    /// What the header calls this connection, whether it is encrypted, and how it is signed in.
    name: String,
    secure: bool,
    auth: String,
    /// The folders this server lets a torrent go into, spelled as its own roots route spells them.
    roots: Vec<PathBuf>,
    /// Where a torrent goes when the add dialog names nowhere.
    default_dir: PathBuf,
    /// The pane to keep fresh, and for which torrent.
    watching: Option<(TorrentId, DetailTab)>,
    /// The last folder listing that was asked for, kept so every later snapshot still carries an answer:
    /// a screen draws snapshots, not replies to its own commands.
    dirs: Option<DirListing>,
    /// The devices this server lets sign in, from the last `Devices` command; `None` until one is asked for.
    devices: Option<Vec<DeviceRow>>,
    /// The loopback relay a gateway target streams through: started on the first
    /// `Stream` that needs it and kept for the run, the same arrangement as the local
    /// endpoint (see [`crate::stream::local::LocalEndpoint`]). A `Mutex` rather than a
    /// plain field because the start is a bind the command's own start waits on: the
    /// lock is held only for the bind itself, never across an await.
    relay: Mutex<Option<relay::Relay>>,
    /// What the last good round of reads said, reused whole when the link is down so the screen keeps
    /// showing something rather than nothing.
    torrents: Vec<TorrentRow>,
    totals: Totals,
    limits: LiveLimits,
    detail: Option<(TorrentId, Detail)>,
    /// The server's queue counts as it last answered them, which is what a limits change is built on top
    /// of: its `PUT` replaces the whole set and refuses a field this client left out.
    queue: Option<map::QueueDto>,
    /// Whether the next round should re-read the queue counts.
    want_limits: bool,
    /// When each torrent's stalled count last started running; see [`crate::engine::map::stalled_clock`].
    stalled: HashMap<TorrentId, Instant>,
    /// How the link stands right now, and which rung of [`BACKOFF`] this client is waiting on.
    state: LinkState,
    backoff: usize,
    /// Whether the current run of failures has already been said out loud: a server that stays down for an
    /// hour is one line about being down, not three thousand.
    complained: bool,
    /// The snapshot the screen is looking at, so that only a change publishes.
    published: Arc<Snapshot>,
    snapshot_tx: watch::Sender<Arc<Snapshot>>,
    notices: mpsc::Sender<Notice>,
    /// Wakes this loop early when an event says something changed. Held by the event stream too, which is
    /// why it is shared rather than a channel this task owns.
    wake: Arc<Notify>,
    /// Tells the event stream whether the link is up (see [`Gate`]). Dropping this at the end of
    /// [`run`](Self::run) is what ends the stream.
    gate: watch::Sender<Gate>,
}

/// What the header calls a server: the host its address names, without the port.
fn server_name(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| clean(url))
}

/// How a connection is signed in, spelled for the header. No secret is in it: this says which kind of
/// credential is in use, never what it is.
fn method_name(login: LoginMethod) -> &'static str {
    match login {
        LoginMethod::None => "open",
        LoginMethod::Password => "password",
        LoginMethod::KeyFile => "device key",
        LoginMethod::Agent => "ssh-agent",
        LoginMethod::GatewayKey => "gateway token",
        LoginMethod::GatewayBasic => "gateway password",
    }
}

#[cfg(test)]
mod tests {
    use super::Secret;
    use zeroize::Zeroizing;

    /// A secret's `Debug` says which kind of credential it is and never what it is: the target a screen
    /// logs on its way to a server carries a user name and an address, and this is the one value beside
    /// them that somebody could repeat by accident.
    #[test]
    fn a_secret_prints_its_kind_and_not_its_value() {
        let cases = [
            (Secret::None, "Secret::None"),
            (
                Secret::Password(Zeroizing::new("hunter2".to_string())),
                "Secret::Password(<redacted>)",
            ),
            (
                Secret::KeyPassphrase(Some(Zeroizing::new("correct horse".to_string()))),
                "Secret::KeyPassphrase(<redacted>)",
            ),
            (
                Secret::KeyPassphrase(None),
                "Secret::KeyPassphrase(<redacted>)",
            ),
        ];
        for (secret, expected) in cases {
            assert_eq!(format!("{secret:?}"), expected);
        }
    }
}
