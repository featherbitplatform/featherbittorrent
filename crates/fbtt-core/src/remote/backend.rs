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
    Command, Connection, Detail, DetailTab, LinkState, Notice, Snapshot, TorrentId, Totals,
};
use crate::engine::map::clean;
use crate::remote::http::ApiError;
use crate::remote::{BACKOFF, Backend, REFRESH, SLOW_REFRESH, map, tab_name};

impl Backend {
    /// Answers commands and keeps the snapshot fresh until asked to stop, or until nobody is watching any
    /// more, which are the same thing here as they are for the local backend.
    pub(crate) async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        mut wake: mpsc::UnboundedReceiver<()>,
    ) {
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
                    // The event stream: something changed, so ask at once instead of on the next tick.
                    _ = wake.recv() => Wake::Refresh,
                    _ = slow.tick() => Wake::Slow,
                },
            };
            match wake {
                Wake::Command(Command::Shutdown) => {
                    // End this client's own session before going: a device that is not connected any more
                    // should stop being listed as one.
                    self.api.log_out().await;
                    break;
                }
                Wake::Command(command) => self.handle(command).await,
                Wake::Refresh => self.refresh().await,
                Wake::Slow => {
                    self.want_limits = true;
                    self.refresh().await;
                }
            }
            self.publish();
        }
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
        match self.poll().await {
            Ok(()) => {
                if matches!(self.state, LinkState::Lost { .. }) {
                    self.notify(Notice::Info(format!("reached {} again", self.name)));
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
                    self.notify(Notice::Error(clean(&problem.notice())));
                }
            }
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
                Err(ApiError::Answer(_)) => self.limits = map::rate_limits(&session),
                Err(problem) => return Err(problem),
            }
            self.want_limits = false;
        }

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

    /// The watched pane, or `None` when there is nothing to show for it: the torrent went away, or this
    /// release cannot read what the server answers for that tab. A failure of one pane is not a failure of
    /// the round - the table came through, and the pane comes back on the next tick.
    pub(crate) async fn read_detail(&self, id: &TorrentId, tab: DetailTab) -> Option<Detail> {
        let path = format!("/api/v1/torrents/{}/{}", id.0, tab_name(tab));
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
            self.notify(Notice::Error(clean(&problem.notice())));
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
        });
        if next == self.published {
            return;
        }
        self.published = Arc::clone(&next);
        // Fails only when every client is gone, which the closed command queue answers.
        let _ = self.snapshot_tx.send(next);
    }

    /// One line for the screen. It is dropped when the queue is full: a backend must not wait on a screen
    /// that has stopped reading.
    pub(crate) fn notify(&self, notice: Notice) {
        let _ = self.notices.try_send(notice);
    }
}
