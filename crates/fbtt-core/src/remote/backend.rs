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
use crate::remote::bodies::ApiError;
use crate::remote::routes::{tab_path, torrent_route};
use crate::remote::{BACKOFF, Backend, Gate, REFRESH, SLOW_REFRESH, map};

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
                    self.notify(problem.notice());
                }
            }
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
