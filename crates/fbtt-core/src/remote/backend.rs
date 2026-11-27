//! The remote connection's own loop: refresh on a cadence, wake early when an event says something
//! changed, and publish a snapshot whenever anything actually did.
//!
//! This is the half of [`crate::remote`]'s task that talks to the server and rebuilds what a screen draws;
//! the commands it answers are in [`crate::remote::commands`], and the state both of them work on is the
//! [`Backend`] they share, which stays in the module that starts it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::client::{
    Command, Connection, Detail, DetailTab, LinkState, Notice, SettingsCommand, Snapshot,
    TorrentId, Totals,
};
use crate::engine::map::clean;
use crate::proxy::ProxyHealth;
use crate::remote::bodies::ApiError;
use crate::remote::routes::{tab_path, torrent_route};
use crate::remote::{BACKOFF, Backend, Gate, REFRESH, SLOW_REFRESH, map};

/// How often a connected backend asks the server for its proxy health.
const PROXY_EVERY: Duration = Duration::from_secs(5);

impl Backend {
    /// Answers commands and keeps the snapshot fresh until asked to stop, or until nobody is watching any
    /// more, which are the same thing here as they are for the local backend.
    pub(crate) async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let mut tick = tokio::time::interval(REFRESH);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut slow = tokio::time::interval(SLOW_REFRESH);
        slow.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Both are ready at once on their first tick. The table's goes with it - the first read is what
        // fills an empty screen in; the queue's would re-read counts the same round already reads.
        slow.tick().await;

        loop {
            // Which thing woke the task. While the link is down the only things worth waiting on are the
            // retry and whatever the screen asks for, so the refresh timers stay out of that select.
            enum Wake {
                Command(Command),
                Refresh,
                Slow,
            }
            let wake = match self.pause() {
                Some(pause) => tokio::select! {
                    _ = tokio::time::sleep(pause) => Wake::Refresh,
                    command = commands.recv() => match command {
                        Some(command) => Wake::Command(command),
                        None => Wake::Command(Command::Shutdown),
                    },
                },
                None => tokio::select! {
                    command = commands.recv() => match command {
                        Some(command) => Wake::Command(command),
                        // Every client gone is the same as one asking to stop.
                        None => Wake::Command(Command::Shutdown),
                    },
                    _ = tick.tick() => Wake::Refresh,
                    // The event stream: something changed, so ask at once instead of on the next tick. One
                    // permit covers any number of events that arrived while the last round was running, which
                    // is what keeps a busy server from turning this loop into a re-read per event.
                    _ = self.wake.notified() => Wake::Refresh,
                    _ = slow.tick() => Wake::Slow,
                },
            };
            match wake {
                Wake::Command(Command::Shutdown) => break,
                Wake::Command(command) => self.handle(command).await,
                Wake::Refresh => self.refresh().await,
                Wake::Slow => {
                    self.want_limits = true;
                    self.refresh().await;
                }
            }
            // A round that gave up because somebody pressed stop (see `crate::remote`'s relay) has nothing new
            // to show and its failure is not trouble worth a line: go straight to ending the session rather
            // than publishing a snapshot about a server nobody is waiting for any more.
            if self.api.asked_to_stop() {
                break;
            }
            self.publish();
        }

        // The relay goes before the session ends: its streams read through this client's
        // credential, so the relay must be done before the credential is gone, the same
        // order the local endpoint and its engine go in (see `crate::local`). A relay
        // that was never started is nothing, and its stop signal is the one this
        // connection already asked to stop on.
        if let Some(relay) = self.relay.take() {
            relay.shutdown().await;
        }
        // End this client's own session before going: a device that is not connected any more should stop
        // being listed as one. This is the only thing on the way out with a timeout of its own - two seconds,
        // and then the task ends whatever the server did with it (see `Api::log_out`).
        self.api.log_out().await;
    }

    /// How long to wait before the next attempt at a server that last refused to answer, or `None` when the
    /// link is up and there is nothing to wait for.
    pub(crate) fn pause(&self) -> Option<Duration> {
        match self.state {
            LinkState::Lost { retry_in_secs } => Some(
                BACKOFF[self.backoff.min(BACKOFF.len() - 1)]
                    .min(Duration::from_secs(retry_in_secs)),
            ),
            _ => None,
        }
    }

    /// Reads the table once and takes it in, then says how the link stands.
    pub(crate) async fn refresh(&mut self) {
        // What this round has to say, kept and said after the snapshot it is about. A screen takes the
        // notice and the snapshot from two different channels, so the order they are sent in is the order
        // they can be seen in: the state the line describes has to be out there before the line arrives.
        let mut line: Option<Notice> = None;
        match self.poll().await {
            Ok(()) => {
                if matches!(self.state, LinkState::Lost { .. }) {
                    line = Some(Notice::Info(format!("reached {} again", self.name)));
                }
                self.state = LinkState::Connected;
                self.backoff = 0;
                self.complained = false;
            }
            Err(problem) => {
                // A server that answers "no such torrent", or sends a body this client cannot read, has not
                // gone anywhere: the link stays up and the last table stays on the screen. Only failing to
                // reach it, or a credential it will not accept, is a lost link.
                if problem.breaks_the_link() {
                    let pause = BACKOFF[self.backoff.min(BACKOFF.len() - 1)];
                    self.backoff = (self.backoff + 1).min(BACKOFF.len() - 1);
                    self.state = LinkState::Lost {
                        retry_in_secs: pause.as_secs(),
                    };
                }
                // Said once per run of trouble rather than once per attempt: a server that stays broken for
                // an hour is one line about being broken, not three thousand. A good round clears it, so the
                // next trouble is news again.
                if !self.complained {
                    self.complained = true;
                    line = Some(problem.notice());
                }
            }
        }
        // A round that gave up because somebody pressed stop (see `crate::remote`'s relay) has nothing new
        // to show: the link did not go somewhere, this client did. Publishing the `Lost` this round worked
        // out would put a lost link on the screen of a server nobody is waiting for any more, which is what
        // the loop behind this already refuses to do - so this round stops here, before the snapshot, before
        // the line, and before the stream's gate.
        if self.api.asked_to_stop() {
            return;
        }
        // The snapshot before the line, always: whatever the round ended up saying about the link, the
        // state it says it about is already published, so a notice and the snapshot that agrees with it are
        // seen together rather than the line arriving against the state from before this round.
        self.publish();
        if let Some(line) = line {
            self.notify(line);
        }
        self.publish_gate();
    }

    /// Tells the event stream whether it may dial, which follows the link exactly: the stream never logs in
    /// itself, so it dials only once a round of reads has proved this client can talk to this server with a
    /// credential it already holds (see [`crate::remote::sse`]).
    fn publish_gate(&self) {
        let next = if matches!(self.state, LinkState::Connected) {
            Gate::Up
        } else {
            Gate::Down
        };
        // Only on a change. The stream races its own attempt against this channel, so sending once a second
        // whether it changed or not would look to it like the link went down every second.
        if *self.gate.borrow() != next {
            let _ = self.gate.send(next);
        }
    }

    /// One round of reads: the roots once, then the table, the session row, and the watched pane. Any one
    /// of them failing is reported to the caller, which decides whether that means the link is gone.
    pub(crate) async fn poll(&mut self) -> Result<(), ApiError> {
        if self.roots.is_empty() {
            let roots: Vec<String> = self.api.json(self.api.get("/api/v1/fs/roots")).await?;
            self.roots = roots.iter().map(PathBuf::from).collect();
            // The first root is where a torrent goes when nobody says otherwise, which is the rule the
            // server's own add route follows too.
            self.default_dir = self.roots.first().cloned().unwrap_or_default();
        }
        // Once, on connecting: the server's current default folder, so a screen that did not change it
        // still adds to the right one. An old server, or any error, leaves the roots' first as it was.
        if !self.settings_asked {
            self.settings_asked = true;
            self.settings_round(SettingsCommand::Load, true).await;
        }
        let torrents: Vec<map::TorrentDto> =
            self.api.json(self.api.get("/api/v1/torrents")).await?;
        let session: map::SessionDto = self.api.json(self.api.get("/api/v1/session")).await?;

        if self.want_limits {
            match self
                .api
                .json::<map::QueueDto>(self.api.get("/api/v1/session/queue"))
                .await
            {
                Ok(queue) => {
                    self.limits = map::live_limits(&session, &queue);
                    self.queue = Some(queue);
                }
                // A server that will not answer its queue route still has rates to show: the counts stay
                // unknown rather than being invented as unlimited.
                Err(ApiError::Answer(_) | ApiError::NotFound(_)) => {
                    self.limits = map::rate_limits(&session)
                }
                Err(problem) => return Err(problem),
            }
            self.want_limits = false;
        }

        self.poll_proxy(Instant::now()).await;

        let (torrents, totals) = map::rows(&torrents, &mut self.stalled, Instant::now());
        self.torrents = torrents;
        self.totals = Totals {
            down_rate: totals.down_rate,
            up_rate: totals.up_rate,
            dht_nodes: session
                .dht_nodes
                .map(|nodes| usize::try_from(nodes).unwrap_or(usize::MAX)),
        };
        self.detail = match self.watching.clone() {
            Some((id, tab)) => self.read_detail(&id, tab).await.map(|detail| (id, detail)),
            None => None,
        };
        Ok(())
    }

    /// Asks the server how its proxy is doing, at most once in [`PROXY_EVERY`], and only from a round that
    /// has just read the table, so it is only ever asked while the link is up. Nothing here fails the
    /// round or says anything: a network error keeps the last value, a `404` means an older server, which
    /// is `Off` and is not asked again on this connection, and an answer this client cannot read is kept
    /// out. It is a call inside the loop, not a task of its own, so nothing is left running when the
    /// backend stops.
    pub(crate) async fn poll_proxy(&mut self, now: Instant) {
        if self.proxy_old
            || self
                .proxy_polled
                .is_some_and(|at| now.duration_since(at) < PROXY_EVERY)
        {
            return;
        }
        self.proxy_polled = Some(now);
        match self
            .api
            .json::<ProxyHealth>(self.api.get("/api/v1/settings/proxy/state"))
            .await
        {
            Ok(health) => {
                // The reason is the server's text, drawn on a screen: it is cleaned like every other.
                self.proxy = match health {
                    ProxyHealth::Unreachable { reason } => ProxyHealth::Unreachable {
                        reason: clean(&reason),
                    },
                    other => other,
                };
            }
            Err(ApiError::NotFound(_)) => {
                self.proxy_old = true;
                self.proxy = ProxyHealth::Off;
            }
            Err(_) => {}
        }
    }

    /// The watched pane, or `None` when there is nothing to show for it: the torrent went away, or this
    /// release cannot read what the server answers for that tab. A failure of one pane is not a failure of
    /// the round - the table came through, and the pane comes back on the next tick.
    pub(crate) async fn read_detail(&self, id: &TorrentId, tab: DetailTab) -> Option<Detail> {
        let path = torrent_route(&id.0, tab_path(tab));
        match tab {
            DetailTab::Files => self
                .api
                .json::<Vec<map::FileDto>>(self.api.get(&path))
                .await
                .ok()
                .map(map::file_rows)
                .map(Detail::Files),
            DetailTab::Peers => self
                .api
                .json::<Vec<map::PeerDto>>(self.api.get(&path))
                .await
                .ok()
                .map(map::peer_rows)
                .map(Detail::Peers),
            DetailTab::Trackers => self
                .api
                .json::<Vec<map::TrackerDto>>(self.api.get(&path))
                .await
                .ok()
                .map(map::tracker_rows)
                .map(Detail::Trackers),
            DetailTab::Pieces => self
                .api
                .json::<map::PiecesDto>(self.api.get(&path))
                .await
                .ok()
                .map(|dto| Detail::Pieces {
                    have: map::piece_flags(&dto),
                }),
        }
    }

    /// Reports what the server said about a command. Success needs no line: the snapshot it changes is the
    /// answer, exactly as it is for the local backend.
    pub(crate) fn report(&self, outcome: Result<(), ApiError>) {
        if let Err(problem) = outcome {
            self.notify(problem.notice());
        }
    }

    /// Rebuilds the snapshot and publishes it only when it differs from what is out there, so a screen
    /// woken by `changed()` always has something new to draw.
    pub(crate) fn publish(&mut self) {
        let next = Arc::new(Snapshot {
            connection: Connection::Remote {
                name: self.name.clone(),
                secure: self.secure,
                auth: self.auth.clone(),
                state: self.state,
            },
            torrents: self.torrents.clone(),
            totals: self.totals,
            limits: self.limits,
            detail: self.detail.clone(),
            default_dir: self.default_dir.clone(),
            dirs: self.dirs.clone(),
            devices: self.devices.clone(),
            settings: self.settings.clone(),
            proxy: self.proxy.clone(),
        });
        if next == self.published {
            return;
        }
        self.published = Arc::clone(&next);
        // Fails only when every client is gone, which the closed command queue answers.
        let _ = self.snapshot_tx.send(next);
    }

    /// One line for the screen. It is dropped when the queue is full: a backend must not wait on a screen
    /// that has stopped reading. And a round that gave up because somebody pressed stop says nothing at all:
    /// "the server could not be reached" would be a complaint about a server nobody was waiting for any more.
    pub(crate) fn notify(&self, notice: Notice) {
        if self.api.asked_to_stop() {
            return;
        }
        let _ = self.notices.try_send(notice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{LiveLimits, SettingsState};
    use crate::remote::Secret;
    use crate::remote::blank;
    use crate::remote::http::Api;
    use crate::remote::stopping::Stopping;
    use crate::ui_state::{LoginMethod, RemoteTarget};
    use std::collections::HashMap;
    use tokio::sync::{Notify, mpsc, watch};

    /// A round that gave up because somebody pressed stop has nothing to publish: the link did not go
    /// anywhere, this client did. Putting the `Lost` it worked out on the screen is the snapshot the loop
    /// behind it already refuses to publish - news about a server nobody is waiting for any more.
    #[tokio::test]
    async fn a_round_given_up_for_a_stop_publishes_nothing() {
        let stopping = Arc::new(Stopping::default());
        // Nothing has to answer on the other end: the stop is already in by the time the round runs, so
        // every call the round makes ends on the stop rather than on a socket.
        let target = RemoteTarget {
            url: "http://127.0.0.1:1".to_string(),
            login: LoginMethod::None,
            user: None,
            key_path: None,
            token_url: None,
            gateway_key_header: None,
            client_id: None,
        };
        let api = Arc::new(
            Api::try_new(&target, Secret::None, None, Arc::clone(&stopping))
                .expect("an address that is one"),
        );
        let initial = Arc::new(blank(
            "a server".to_string(),
            false,
            "open".to_string(),
            LinkState::Connected,
        ));
        let (snapshot_tx, mut snapshots) = watch::channel(Arc::clone(&initial));
        let (notices, _lines) = mpsc::channel(1);
        let (gate, _up) = watch::channel(Gate::Down);
        let mut backend = Backend {
            api,
            name: "a server".to_string(),
            secure: false,
            auth: "open".to_string(),
            roots: Vec::new(),
            default_dir: PathBuf::new(),
            watching: None,
            dirs: None,
            devices: None,
            settings: SettingsState::NotLoaded,
            settings_asked: false,
            proxy: ProxyHealth::Off,
            proxy_old: false,
            proxy_polled: None,
            relay: None,
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
            queue: None,
            want_limits: false,
            stalled: HashMap::new(),
            state: LinkState::Connected,
            backoff: 0,
            complained: false,
            published: Arc::clone(&initial),
            snapshot_tx,
            notices,
            wake: Arc::new(Notify::new()),
            gate,
        };

        // The stop lands while the round is reading, which is what leaves `asked_to_stop` set by the time the
        // round would publish: it must put nothing on the screen at all.
        stopping.stop();
        backend.refresh().await;

        assert!(
            !snapshots
                .has_changed()
                .expect("the round left the snapshot queue open"),
            "a round given up for a stop publishes nothing"
        );
        assert_eq!(
            *snapshots.borrow_and_update(),
            initial,
            "the screen keeps the last good snapshot, not a lost link worked out on the way out"
        );
    }

    /// What the stand-in server answers the proxy state route with: a status and a body, or `None` to
    /// drop the connection, and how many times it was asked.
    #[derive(Default)]
    struct Answer {
        reply: std::sync::Mutex<Option<(u16, String)>>,
        asked: std::sync::atomic::AtomicUsize,
    }

    /// A one-route HTTP server on loopback that answers every request from `answer`, and a backend
    /// pointed at it. Nothing leaves the machine.
    async fn backend_against(answer: Arc<Answer>) -> Backend {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback listener");
        let url = format!("http://{}", listener.local_addr().expect("its address"));
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let answer = Arc::clone(&answer);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    answer
                        .asked
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let reply = answer.reply.lock().expect("the reply").clone();
                    let Some((status, body)) = reply else {
                        return;
                    };
                    let head = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(body.as_bytes()).await;
                });
            }
        });
        let target = RemoteTarget {
            url,
            login: LoginMethod::None,
            user: None,
            key_path: None,
            token_url: None,
            gateway_key_header: None,
            client_id: None,
        };
        let api = Arc::new(
            Api::try_new(&target, Secret::None, None, Arc::new(Stopping::default()))
                .expect("an address that is one"),
        );
        let initial = Arc::new(blank(
            "a server".to_string(),
            false,
            "open".to_string(),
            LinkState::Connected,
        ));
        let (snapshot_tx, _snapshots) = watch::channel(Arc::clone(&initial));
        let (notices, _lines) = mpsc::channel(1);
        let (gate, _up) = watch::channel(Gate::Down);
        Backend {
            api,
            name: "a server".to_string(),
            secure: false,
            auth: "open".to_string(),
            roots: Vec::new(),
            default_dir: PathBuf::new(),
            watching: None,
            dirs: None,
            devices: None,
            settings: SettingsState::NotLoaded,
            settings_asked: false,
            proxy: ProxyHealth::Off,
            proxy_old: false,
            proxy_polled: None,
            relay: None,
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
            queue: None,
            want_limits: false,
            stalled: HashMap::new(),
            state: LinkState::Connected,
            backoff: 0,
            complained: false,
            published: Arc::clone(&initial),
            snapshot_tx,
            notices,
            wake: Arc::new(Notify::new()),
            gate,
        }
    }

    fn asked(answer: &Answer) -> usize {
        answer.asked.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The server's answer is taken in, its reason cleaned, and the route is asked at most every five
    /// seconds, not every round; an error keeps the last value and says nothing.
    #[tokio::test]
    async fn remote_polls_proxy_state_every_five_seconds() {
        let answer = Arc::new(Answer::default());
        *answer.reply.lock().expect("reply") = Some((
            200,
            r#"{"state":"unreachable","reason":"\u001b[31mdown"}"#.to_string(),
        ));
        let mut backend = backend_against(Arc::clone(&answer)).await;
        let start = Instant::now();

        backend.poll_proxy(start).await;
        let ProxyHealth::Unreachable { reason } = backend.proxy.clone() else {
            panic!("the server's answer is taken in: {:?}", backend.proxy);
        };
        assert!(!reason.contains('\u{1b}'), "{reason:?}");
        assert_eq!(asked(&answer), 1);

        // Within five seconds: not asked again.
        backend.poll_proxy(start + Duration::from_secs(4)).await;
        assert_eq!(asked(&answer), 1, "asked again inside five seconds");

        // After five seconds: asked, and a change is taken in.
        *answer.reply.lock().expect("reply") = Some((200, r#"{"state":"ok"}"#.to_string()));
        backend.poll_proxy(start + Duration::from_secs(6)).await;
        assert_eq!(asked(&answer), 2);
        assert_eq!(backend.proxy, ProxyHealth::Ok);

        // The connection drops: the last value stays, and nothing is raised.
        *answer.reply.lock().expect("reply") = None;
        backend.poll_proxy(start + Duration::from_secs(12)).await;
        assert_eq!(asked(&answer), 3);
        assert_eq!(backend.proxy, ProxyHealth::Ok, "a network error keeps it");
        assert!(!backend.proxy_old);
    }

    /// A `404` is an older server: `Off`, and this connection never asks again.
    #[tokio::test]
    async fn old_server_is_off_and_not_asked_again() {
        let answer = Arc::new(Answer::default());
        *answer.reply.lock().expect("reply") = Some((
            404,
            r#"{"error":"not_found","message":"no such route"}"#.to_string(),
        ));
        let mut backend = backend_against(Arc::clone(&answer)).await;
        backend.proxy = ProxyHealth::LoginRefused;
        let start = Instant::now();

        backend.poll_proxy(start).await;
        assert_eq!(backend.proxy, ProxyHealth::Off);
        assert_eq!(asked(&answer), 1);

        backend.poll_proxy(start + Duration::from_secs(6)).await;
        backend.poll_proxy(start + Duration::from_secs(60)).await;
        assert_eq!(asked(&answer), 1, "an older server is not asked again");
    }
}
