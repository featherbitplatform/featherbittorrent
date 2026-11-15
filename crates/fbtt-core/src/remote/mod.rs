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

mod backend;
mod commands;
pub(crate) mod device;
pub(crate) mod dto;
pub(crate) mod gateway;
pub(crate) mod http;
pub(crate) mod login;
pub(crate) mod map;
mod sse;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use zeroize::Zeroizing;

use crate::client::{
    Client, Command, Connection, Detail, DetailTab, DeviceRow, DirListing, LinkState, LiveLimits,
    Notice, Snapshot, TorrentId, TorrentRow, Totals,
};
use crate::engine::map::clean;
use crate::ui_state::{LoginMethod, RemoteTarget};

use http::Api;

/// How often the table is rebuilt when nothing else wakes it. A second is what the local backend's half
/// second buys over HTTP: rates that look alive, a stalled column that ticks, and a server that is not
/// asked anything it does not need to answer.
const REFRESH: Duration = Duration::from_secs(1);

/// How often the queue counts (and with them the limits the header shows) are re-read. The table changes
/// every second; who is queued where moves when a torrent finishes.
const SLOW_REFRESH: Duration = Duration::from_secs(10);

/// The ladder of pauses between attempts at a server that is not answering, capped at half a minute so a
/// server that is down for the evening is asked about once every thirty seconds rather than once a second.
const BACKOFF: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
];

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

/// Drives the client interface over a server's HTTP API.
#[derive(Debug)]
pub struct RemoteBackend;

impl RemoteBackend {
    /// Starts a connection to `target` and hands back the handle a screen uses.
    ///
    /// Nothing here waits or fails: the address is dialed by the task this starts, so a server that is down
    /// says so on the client rather than to whoever called. A target and secret that do not go together - a
    /// password login with no user name, a gateway with no token endpoint - is reported the same way.
    pub fn start(target: RemoteTarget, secret: Secret) -> Client {
        let initial = Arc::new(Snapshot {
            connection: Connection::Remote {
                name: server_name(&target.url),
                secure: target.url.starts_with("https://"),
                auth: method_name(target.login).to_string(),
                state: LinkState::Connecting,
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
        });
        let (client, channels) = Client::paired(Arc::clone(&initial));

        match Api::try_new(&target, secret) {
            Err(problem) => {
                // Nothing can be asked of this address, so say that once and let the client answer
                // commands with nothing until it is dropped or asked to stop.
                let _ = channels.notices.try_send(Notice::Error(clean(&problem)));
                tokio::spawn(idle(channels.commands));
                client
            }
            Ok(api) => {
                let api = Arc::new(api);
                let (wake_tx, wake_rx) = mpsc::unbounded_channel::<()>();
                // The event stream's own end: dropping the sender at the end of `run` is what tells the
                // stream nobody is left to wake, so it cannot outlive this connection or wait on itself.
                let (stop_tx, stop_rx) = watch::channel(());
                sse::spawn(
                    Arc::clone(&api),
                    wake_tx.clone(),
                    channels.notices.clone(),
                    stop_rx,
                );
                let backend = Backend {
                    api,
                    name: server_name(&target.url),
                    secure: target.url.starts_with("https://"),
                    auth: method_name(target.login).to_string(),
                    roots: Vec::new(),
                    default_dir: PathBuf::new(),
                    watching: None,
                    dirs: None,
                    devices: None,
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
                    _wake_tx: wake_tx,
                    _stop: stop_tx,
                };
                tokio::spawn(backend.run(channels.commands, wake_rx));
                client
            }
        }
    }
}

/// A client that was never able to reach anywhere: it answers `Shutdown` and drops the rest, so a screen
/// that connected to a bad address can still close the connection.
async fn idle(mut commands: mpsc::Receiver<Command>) {
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
    /// A handle on the wake channel the event stream uses, held so the channel stays open for it. Never
    /// read: its only job is to exist until this struct does not.
    _wake_tx: mpsc::UnboundedSender<()>,
    /// Dropping this at the end of [`run`](Self::run) is what ends the event stream.
    _stop: watch::Sender<()>,
}

/// The pane's tab as the route that answers it is named.
fn tab_name(tab: DetailTab) -> &'static str {
    match tab {
        DetailTab::Files => "files",
        DetailTab::Peers => "peers",
        DetailTab::Trackers => "trackers",
        DetailTab::Pieces => "pieces",
    }
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

/// Percent-encodes one path or query component, so a device name with a space in it or a folder path with
/// a slash reaches the route that has to match it exactly. Everything outside the unreserved set becomes a
/// `%` escape, which is what a server decodes back to the byte it started as.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            // Spaces are the common case and every other byte needs no judgement either: this is one table.
            _ => {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("%{byte:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Secret, encode};
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

    /// The unreserved set and nothing more: `:` and `/` in an OAuth2 assertion type become escapes a
    /// gateway has to undo, which is why the fake one in `tests/remote.rs` decodes percent pairs rather
    /// than spaces alone.
    #[test]
    fn encode_leaves_only_the_unreserved_characters_bare() {
        assert_eq!(encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode("a b"), "a%20b");
        assert_eq!(encode("a/b"), "a%2Fb");
        assert_eq!(
            encode("urn:ietf:params:oauth:client-assertion-type:jwt-bearer"),
            "urn%3Aietf%3Aparams%3Aoauth%3Aclient-assertion-type%3Ajwt-bearer"
        );
    }
}
