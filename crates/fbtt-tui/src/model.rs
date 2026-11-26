//! The screen's own state: what is selected, how the table is ordered, which pane is up.
//!
//! A model never talks to the engine and never waits for anything. A key press goes in
//! with the newest snapshot beside it, and either nothing changes or a list of [`Action`]s
//! comes out for the binary to act on - mostly commands for whatever backend is running, but
//! also "run on a different backend" and "finished". Reading a snapshot is what tells the model
//! what `p` means for the torrent under the cursor; deciding that the engine did it is not its
//! business, so a command that a peer keeps waiting for cannot freeze the screen.

use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use fbtt_core::{Command, DetailTab, Notice, RowState, Snapshot, TorrentId, TorrentRow};

use crate::keys;
use crate::modal::{ConnectRequest, ConnectTarget, Modal};
use crate::settings::SettingsScreen;
use fbtt_core::ui_state::RemoteTarget;

/// Which of the two screens is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The header, the torrent table and the detail pane.
    Full,
    /// One line per torrent, for a corner of the screen.
    Minimal,
}

/// The column a sort runs on, in the order `o` cycles through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// The torrent's name.
    Name,
    /// How much of it is on disk.
    Progress,
    /// Payload bytes per second coming in.
    Down,
    /// Payload bytes per second going out.
    Up,
    /// Connected peers.
    Peers,
    /// What it is doing: metadata first, then errors last.
    State,
    /// The order the backend holds them in, which is its queue. The default.
    Queue,
}

impl SortKey {
    /// The next column, wrapping back to the first after the last.
    pub(crate) const fn next(self) -> SortKey {
        match self {
            SortKey::Name => SortKey::Progress,
            SortKey::Progress => SortKey::Down,
            SortKey::Down => SortKey::Up,
            SortKey::Up => SortKey::Peers,
            SortKey::Peers => SortKey::State,
            SortKey::State => SortKey::Queue,
            SortKey::Queue => SortKey::Name,
        }
    }

    /// The state order this column sorts by: the states as fbtt-core declares them, so
    /// metadata comes first and a failed torrent last.
    fn state_rank(state: RowState) -> u8 {
        match state {
            RowState::Metadata => 0,
            RowState::Checking(_) => 1,
            RowState::Queued => 2,
            RowState::Downloading => 3,
            RowState::Stalled => 4,
            RowState::Seeding => 5,
            RowState::Paused => 6,
            RowState::Error => 7,
        }
    }
}

/// Which way a sort runs: `Descending(true)` puts the biggest or last first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descending(pub bool);

/// How long a notice stays on the footer. Long enough to read a line about something that just
/// failed, short enough that the screen is not still reporting it a minute later.
const NOTICE_TTL: Duration = Duration::from_secs(5);

/// The tabs of the detail pane, in the order `Tab` moves through them.
const TABS: [DetailTab; 4] = [
    DetailTab::Files,
    DetailTab::Peers,
    DetailTab::Trackers,
    DetailTab::Pieces,
];

/// What one key press asks of the program. A command is the common answer and goes to whatever backend
/// is running; the other two are about the run itself, which is why they are not commands: a screen cannot
/// ask a backend to start a different backend.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Something for the current backend to do.
    Command(Command),
    /// Stop this backend and run on another one: the connect screen's answer. The binary shuts the current
    /// backend down (which is how a local engine saves and frees its lock), starts the new one, and
    /// remembers the choice for the next run.
    Connect(ConnectRequest),
    /// The person is finished: save, restore the terminal, leave.
    Quit,
}

/// What a person is doing in the app, as one value.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// Which screen is on. `m` switches it.
    pub view: View,
    /// The torrent the table and the detail pane point at.
    ///
    /// It is an id rather than a row number so that a re-sort, a new snapshot or a
    /// torrent starting on its own keeps pointing at the same torrent.
    pub selected: Option<TorrentId>,
    /// How the table is ordered: the column, and whether it runs biggest first.
    pub sort: (SortKey, Descending),
    /// A name fragment that hides every torrent without it, matched ignoring case.
    pub filter: String,
    /// Which pane the area under the table shows.
    pub detail_tab: DetailTab,
    /// The dialog over the main view, when one is open.
    pub modal: Option<Modal>,
    /// The full-screen settings view, when it is open. It replaces the table while it is.
    pub settings: Option<SettingsScreen>,
    /// Whether the cursor is in the detail pane's file list, which `Enter` puts it in and `Esc`
    /// takes out again. While it is there, `j` and `k` mean files rather than torrents.
    pub files_focused: bool,
    /// Which file in that list the cursor is on. Every user of it clamps this to the list, so a
    /// snapshot with fewer files than the last one cannot leave it past the end.
    pub file_cursor: usize,
    /// `q` has been confirmed: the binary should stop.
    quit: bool,
    /// A `q` that found something still downloading and is waiting to be confirmed.
    quit_armed: bool,
    /// Where the selection last sat among the visible rows, so that a torrent going away
    /// leaves the selection in its place rather than at the top of the table.
    index_hint: usize,
    /// The last notice from the backend and when it arrived. The footer shows it until
    /// [`NOTICE_TTL`] has passed, which [`tick`](Self::tick) is what drops.
    notice: Option<(Notice, Instant)>,
    /// The target this run last connected to, which is what `c` prefills the connect screen with so that
    /// changing one thing about a server does not mean typing all of it again. The address and the method
    /// only: what a person typed into a password field stays in that field.
    last_target: Option<RemoteTarget>,
}

impl Model {
    /// A model as the app starts: the full view, sorted by queue, nothing selected until a
    /// snapshot arrives to select something in.
    pub fn new() -> Model {
        Model {
            view: View::Full,
            selected: None,
            sort: (SortKey::Queue, Descending(false)),
            filter: String::new(),
            detail_tab: DetailTab::Files,
            modal: None,
            settings: None,
            files_focused: false,
            file_cursor: 0,
            quit: false,
            quit_armed: false,
            index_hint: 0,
            notice: None,
            last_target: None,
        }
    }

    /// Applies one key press and says what the program should do about it.
    ///
    /// The snapshot comes along because the answer depends on what is on screen: `p` sends
    /// [`Command::Pause`] or [`Command::Resume`] depending on the state of the torrent under the
    /// selection. A key that only changes this model returns nothing. Quitting returns
    /// [`Action::Quit`] and also sets [`quit_requested`](Self::quit_requested), so a runtime that is
    /// watching for other things can stop as well; connecting returns [`Action::Connect`] with what the
    /// connect screen chose, which the binary acts on by changing backends.
    pub fn handle_key(&mut self, key: KeyEvent, snap: &Snapshot) -> Vec<Action> {
        self.sync_selection(snap);
        let actions = keys::handle(self, key, snap);
        // A connect answer is also the prefill for the next time the screen opens. What a person just
        // chose is the best guess at what they will want to change. Choosing this library's own engine
        // leaves the remembered server where it is: somebody stepping back to the local library is most
        // likely to want their server address waiting for them when they step out again.
        let chosen = actions.iter().find_map(|action| match action {
            Action::Connect(request) => Some(&request.target),
            Action::Command(_) | Action::Quit => None,
        });
        if let Some(ConnectTarget::Remote(target)) = chosen {
            self.last_target = Some(target.clone());
        }
        actions
    }

    /// What to prefill the connect screen with: the server this run last connected to, or `None` when it
    /// has never connected to one. Choosing the local engine does not clear it. A password is never part
    /// of it.
    pub fn last_target(&self) -> Option<&RemoteTarget> {
        self.last_target.as_ref()
    }

    /// Whether the person has finished: the binary should send [`Command::Shutdown`] and restore the
    /// terminal.
    pub const fn quit_requested(&self) -> bool {
        self.quit
    }

    /// What the detail pane is asking the backend to keep fresh, or nothing when no torrent
    /// is selected. The binary sends [`Command::Watch`] whenever this changes.
    pub fn watch(&self) -> Option<(TorrentId, DetailTab)> {
        self.selected.clone().map(|id| (id, self.detail_tab))
    }

    /// Records one line of feedback from the backend for the footer to show until it expires.
    ///
    /// The add dialog is the one notice that goes somewhere else, and only while it waits for
    /// the answer to the add it sent itself: success closes the dialog, failure shows the reason
    /// inside it. That flag is what keeps an unrelated line - a torrent that went away, a state
    /// file that would not write, a limit the engine refused - from reading as a refused magnet
    /// while somebody has the box open. `now` is a parameter rather than a clock reading so that
    /// a test can say when a notice arrived instead of waiting for one.
    pub fn push_notice(&mut self, notice: Notice, now: Instant) {
        // The settings screen is waiting on the answer to a save when one is out: a good answer
        // marks the section saved, a refusal is shown inside the screen, and a save asked for on the
        // way out finishes leaving.
        if let Some(screen) = self.settings.as_mut()
            && screen.on_notice(&notice, now)
        {
            self.settings = None;
        }
        match self.modal.take() {
            // This dialog asked the question this notice answers.
            Some(Modal::Add(mut form)) if form.awaiting_answer => match &notice {
                // A server that will not accept this client's credential failed this add the same way a
                // refused magnet did: the reason belongs inside the box that is waiting for an answer.
                Notice::Error(text) | Notice::SignInRefused(text) => {
                    form.error = Some(text.clone());
                    // The answer arrived, whatever it was: a later notice is about something else.
                    form.awaiting_answer = false;
                    self.modal = Some(Modal::Add(form));
                    return;
                }
                // Success: the dialog has done its job and comes down. The line still goes on
                // the footer, where the rest of the screen's news lands.
                Notice::Info(_) => {}
                // A file to play is never a dialog answer: it is a line the footer shows, nothing
                // that belongs inside the box that is waiting for an answer.
                Notice::Play(_) => {}
            },
            other => self.modal = other,
        }
        self.notice = Some((notice, now));
    }

    /// Drops a notice that has been on the footer for `NOTICE_TTL`. The binary calls this with
    /// each frame's clock reading so a line about something goes away without a key press; a
    /// `now` earlier than the notice arrived counts as no time passed.
    pub fn tick(&mut self, now: Instant) {
        if let Some(screen) = self.settings.as_mut() {
            screen.tick(now);
        }
        let keep = self
            .notice
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) < NOTICE_TTL);
        if !keep {
            self.notice = None;
        }
    }

    /// The notice the footer shows, or `None` when there is nothing to say. It stops being shown
    /// by [`tick`](Self::tick), which is what knows how long it has been up.
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref().map(|(notice, _)| notice)
    }

    /// Points the selection at something the table is actually showing.
    ///
    /// The binary calls this whenever a new snapshot arrives, and anything that changes what
    /// the table shows (a filter, a sort) should call it too. Selection follows the id, so a
    /// torrent that is still listed keeps it however the rows are re-ordered. A torrent that
    /// is gone leaves the selection at the row now sitting in its place, clamped into the
    /// list, and an empty list leaves nothing selected at all.
    ///
    /// A torrent hidden by the filter counts as gone: the pane under the table describes the
    /// selection, so it should describe one of the torrents a person can see, and the backend
    /// should not be asked to keep a pane fresh for a row that is not on screen.
    ///
    /// A selection that moves or disappears takes the detail pane's file cursor with it: that
    /// cursor was put into one torrent's list, and once the pane describes another torrent the
    /// list it points into is not on the screen any more.
    pub fn sync_selection(&mut self, snap: &Snapshot) {
        // A new snapshot may carry settings the open screen was waiting for.
        if let Some(screen) = self.settings.as_mut() {
            screen.sync(snap);
            if screen.take_close() {
                self.settings = None;
            }
        }
        let rows = visible_rows(self, snap);
        if let Some(at) = self.selected_index(&rows) {
            self.index_hint = at;
            return;
        }

        let at = self.index_hint.min(rows.len().saturating_sub(1));
        self.index_hint = at;
        self.selected = rows.get(at).map(|row| row.id.clone());
        self.set_files_focused(false);
    }

    /// Selects the visible row at `index`, clamped into the list. Empty means nothing is
    /// selected.
    pub(crate) fn select_row(&mut self, index: usize, rows: &[&TorrentRow]) {
        let at = index.min(rows.len().saturating_sub(1));
        self.index_hint = at;
        self.selected = rows.get(at).map(|row| row.id.clone());
    }

    /// Moves the selection `delta` visible rows, stopping at either end instead of wrapping:
    /// walking off the bottom of a list should not put you somewhere unexpected.
    pub(crate) fn step_by(&mut self, delta: i32, rows: &[&TorrentRow]) {
        if rows.is_empty() {
            self.index_hint = 0;
            self.selected = None;
            return;
        }
        let from = self
            .selected_index(rows)
            .unwrap_or_else(|| self.index_hint.min(rows.len() - 1));
        let to = (from as i64 + i64::from(delta)).clamp(0, rows.len() as i64 - 1) as usize;
        self.select_row(to, rows);
    }

    /// Where the selected torrent sits among `rows`, or `None` when it is not among them.
    pub(crate) fn selected_index(&self, rows: &[&TorrentRow]) -> Option<usize> {
        let id = self.selected.as_ref()?;
        rows.iter().position(|row| &row.id == id)
    }

    /// The row the selection points at, or `None`.
    pub(crate) fn selected_row<'a>(&self, rows: &[&'a TorrentRow]) -> Option<&'a TorrentRow> {
        let id = self.selected.as_ref()?;
        rows.iter().find(|row| &row.id == id).copied()
    }

    /// Sorts by the next column.
    pub(crate) fn cycle_sort(&mut self) {
        self.sort = (self.sort.0.next(), self.sort.1);
    }

    /// Runs the current sort the other way.
    pub(crate) fn reverse_sort(&mut self) {
        self.sort = (self.sort.0, Descending(!self.sort.1.0));
    }

    /// Puts the cursor in the detail pane's file list, or takes it out again. Leaving it starts
    /// the next visit at the top of the list, which is where a person who just came back expects it.
    pub(crate) fn set_files_focused(&mut self, focused: bool) {
        self.files_focused = focused;
        if !focused {
            self.file_cursor = 0;
        }
    }

    /// Moves the file cursor within a list of `count` files, stopping at either end like the
    /// torrent selection does. An empty list leaves it at zero.
    pub(crate) fn step_file_cursor(&mut self, delta: i32, count: usize) {
        let last = count.saturating_sub(1);
        let from = self.file_cursor.min(last);
        self.file_cursor = (from as i64 + i64::from(delta)).clamp(0, last as i64) as usize;
    }

    /// Moves the detail pane one tab along, wrapping at either end.
    pub(crate) fn step_detail_tab(&mut self, forward: bool) {
        let current = match self.detail_tab {
            DetailTab::Files => 0,
            DetailTab::Peers => 1,
            DetailTab::Trackers => 2,
            DetailTab::Pieces => 3,
        };
        let count = TABS.len();
        let next = if forward {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
        self.detail_tab = TABS[next];
        // The focus belongs to one pane: moving on leaves the file list behind rather than
        // leaving a cursor in a list nobody is looking at.
        self.set_files_focused(false);
    }

    /// Switches between the full and the minimal screen.
    pub(crate) fn toggle_view(&mut self) {
        self.view = match self.view {
            View::Full => View::Minimal,
            View::Minimal => View::Full,
        };
    }

    /// A `q` that found something still downloading: it asks, and only a second `q` or a
    /// `y` answers. Any other key cancels the ask.
    pub(crate) fn arm_quit(&mut self) {
        self.quit_armed = true;
    }

    /// The footer shows the confirmation for as long as it is armed.
    pub(crate) const fn quit_armed(&self) -> bool {
        self.quit_armed
    }

    /// A pending `q` is dropped without quitting.
    pub(crate) fn cancel_quit(&mut self) {
        self.quit_armed = false;
    }

    /// Stops now: the model says it is finished and the backend is told to save and stop.
    pub(crate) fn quit_now(&mut self) -> Vec<Action> {
        self.quit = true;
        self.quit_armed = false;
        vec![Action::Quit]
    }
}

impl Default for Model {
    fn default() -> Self {
        Model::new()
    }
}

/// The rows the table draws: every torrent whose name contains the filter, ordered by the
/// current sort. With an empty filter this is the whole library in sorted order.
pub fn visible_rows<'a>(model: &Model, snap: &'a Snapshot) -> Vec<&'a TorrentRow> {
    let needle = model.filter.to_lowercase();
    let mut rows: Vec<&TorrentRow> = snap
        .torrents
        .iter()
        .filter(|row| needle.is_empty() || row.name.to_lowercase().contains(&needle))
        .collect();
    sort_rows(&mut rows, model.sort);
    rows
}

/// Orders the rows by one column. Two torrents that tie on it keep the backend's queue
/// order, so a redraw cannot swap two rows for no reason.
fn sort_rows(rows: &mut Vec<&TorrentRow>, (key, descending): (SortKey, Descending)) {
    match key {
        // The name is folded to lower case once per row rather than once per comparison.
        SortKey::Name => {
            rows.sort_by_cached_key(|row| (row.name.to_lowercase(), row.queue_position))
        }
        SortKey::Progress => rows.sort_by(|a, b| {
            a.progress
                .total_cmp(&b.progress)
                .then_with(|| a.queue_position.cmp(&b.queue_position))
        }),
        SortKey::Down => sort_by_rate(rows, |row| row.down_rate),
        SortKey::Up => sort_by_rate(rows, |row| row.up_rate),
        SortKey::Peers => rows.sort_by(|a, b| {
            a.peers
                .cmp(&b.peers)
                .then_with(|| a.queue_position.cmp(&b.queue_position))
        }),
        SortKey::State => rows.sort_by(|a, b| {
            SortKey::state_rank(a.state)
                .cmp(&SortKey::state_rank(b.state))
                .then_with(|| a.queue_position.cmp(&b.queue_position))
        }),
        SortKey::Queue => rows.sort_by_key(|row| row.queue_position),
    }
    // Reversing flips everything, ties included: `O` is meant to read as "this list, the
    // other way round", not as a second ordering rule.
    if descending.0 {
        rows.reverse();
    }
}

/// Orders by one of a row's counters, keeping the queue order for equal values.
fn sort_by_rate(rows: &mut Vec<&TorrentRow>, value: impl Fn(&TorrentRow) -> u64) {
    rows.sort_by(|a, b| {
        value(a)
            .cmp(&value(b))
            .then_with(|| a.queue_position.cmp(&b.queue_position))
    });
}
