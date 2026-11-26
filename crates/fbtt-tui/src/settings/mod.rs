//! The full-screen settings view: a list of sections on the left and one section's fields on the
//! right, as the web page lays it out.
//!
//! The screen holds what is being typed and nothing else of the engine's: it reads the settings out of
//! the snapshot ([`SettingsState`]) and answers with a [`Command`] for the runtime to send, so it never
//! waits on the network or the disk. Each section has its own `Save`. A form is dirty when what it holds
//! differs from the values it was filled with, and a dirty form is never refilled from a snapshot, so a
//! refresh cannot take away what somebody is in the middle of typing.
//!
//! Paths in a [`SettingsView`] are raw, because they go back to the server as they came. The forms keep
//! them raw; the drawing code in `view/settings.rs` passes every one through `fbt::display_safe`.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use fbtt_core::{
    Command, DownloadsChange, DownloadsView, LiveLimits, Notice, OnCompleteSet, SettingsChange,
    SettingsCommand, SettingsState, SettingsView, Snapshot,
};

use crate::modal::{Input, type_into};

/// The refusal a server too old for the settings routes gets, in the plan's words.
pub(crate) const TOO_OLD: &str =
    "This server is too old to change its settings here. Update it to change them.";
/// Where the run-a-program setting lives when this screen may not change it.
pub(crate) const SET_IN_CONFIG: &str = "Set in the server's config file";

/// The sections the terminal has, in the order the web page lists them. The proxy joins later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiSection {
    /// Folders, the paused switch, the watch folder and the program run on completion.
    Downloads,
    /// The rates and the queue counts.
    Speed,
}

/// Every section, top to bottom.
pub(crate) const SECTIONS: [TuiSection; 2] = [TuiSection::Downloads, TuiSection::Speed];

impl TuiSection {
    /// The title the list and the section's heading show.
    pub fn title(self) -> &'static str {
        match self {
            TuiSection::Downloads => "Downloads",
            TuiSection::Speed => "Speed",
        }
    }
}

/// What a press asks of the model.
#[derive(Debug, PartialEq)]
pub(crate) enum ScreenAnswer {
    /// Nothing leaves the screen.
    Stay,
    /// Back to the torrents.
    Close,
    /// Something for the backend; the screen stays up for the answer.
    Send(Command),
    /// Something for the backend, then back to the torrents: a save answered `s` to the leave question
    /// on a path the backend does not acknowledge.
    SendThenClose(Command),
}

/// Which half of the screen takes the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The section list.
    List,
    /// The fields of the open section.
    Fields,
}

/// What a person leaving with unsaved changes was about to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// Go back to the torrents.
    Leave,
    /// Open another section.
    Section(TuiSection),
}

/// One row of the downloads section a cursor can stand on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DownloadsRow {
    Default,
    Unfinished,
    Paused,
    Watch,
    Program,
    Save,
}

/// The rates and queue counts, as five fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpeedForm {
    /// Download, upload, active downloads, active seeds, active total.
    pub(crate) fields: [Input; 5],
    /// The texts the fields were filled with.
    base: [String; 5],
    /// The field refused last, drawn red.
    pub(crate) invalid: Option<usize>,
    /// The row the cursor is on: the five fields, then `Save`.
    pub(crate) row: usize,
}

/// Which row of the speed section is `Save`.
pub(crate) const SPEED_SAVE_ROW: usize = 5;

impl SpeedForm {
    fn new(limits: &LiveLimits) -> SpeedForm {
        let count = |count: Option<usize>| count.map_or(String::new(), |count| count.to_string());
        let texts = [
            limits.download_kib.to_string(),
            limits.upload_kib.to_string(),
            count(limits.active_downloads),
            count(limits.active_seeds),
            count(limits.active_total),
        ];
        SpeedForm {
            fields: texts.clone().map(Input::new),
            base: texts,
            invalid: None,
            row: 0,
        }
    }

    fn values(&self) -> [String; 5] {
        self.fields.clone().map(|field| field.text)
    }

    fn dirty(&self) -> bool {
        self.values() != self.base
    }

    fn saved(&mut self) {
        self.base = self.values();
    }

    fn discard(&mut self) {
        let row = self.row;
        for (field, base) in self.fields.iter_mut().zip(&self.base) {
            *field = Input::new(base.clone());
        }
        self.invalid = None;
        self.row = row;
    }

    /// The five numbers as one change, or `None` with the field that is not a number marked.
    fn limits(&mut self) -> Option<LiveLimits> {
        let mut values = [0u64; 5];
        for (index, field) in self.fields.iter().enumerate() {
            let Some(value) = number(&field.text) else {
                self.invalid = Some(index);
                self.row = index;
                return None;
            };
            values[index] = value;
        }
        Some(LiveLimits {
            download_kib: values[0],
            upload_kib: values[1],
            active_downloads: queue_count(values[2]),
            active_seeds: queue_count(values[3]),
            active_total: queue_count(values[4]),
        })
    }
}

/// The downloads section: three folders, a switch and a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DownloadsForm {
    pub(crate) default_dir: Input,
    pub(crate) unfinished_dir: Input,
    pub(crate) watch_dir: Input,
    /// The program, as typed. Only a field when `editable`.
    pub(crate) program: Input,
    pub(crate) paused: bool,
    /// Whether this surface may change the program at all.
    pub(crate) editable: bool,
    /// The latest complaints about the watch folder and the program, for display.
    pub(crate) watch_problem: Option<String>,
    pub(crate) on_complete_problem: Option<String>,
    /// The values the form was filled with: default, unfinished, watch, program, paused.
    base: ([String; 4], bool),
    /// The row the cursor is on, as an index into [`DownloadsForm::rows`].
    pub(crate) row: usize,
}

fn raw(path: &Option<PathBuf>) -> String {
    path.as_ref()
        .map_or(String::new(), |path| path.to_string_lossy().into_owned())
}

/// A typed folder as a path: nothing typed is no folder.
fn typed_path(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

impl DownloadsForm {
    fn new(view: &DownloadsView) -> DownloadsForm {
        let texts = [
            view.default_dir.to_string_lossy().into_owned(),
            raw(&view.unfinished_dir),
            raw(&view.watch_dir),
            raw(&view.on_complete.program),
        ];
        DownloadsForm {
            default_dir: Input::new(texts[0].clone()),
            unfinished_dir: Input::new(texts[1].clone()),
            watch_dir: Input::new(texts[2].clone()),
            program: Input::new(texts[3].clone()),
            paused: view.start_paused,
            editable: view.on_complete.editable,
            watch_problem: view.watch_problem.clone(),
            on_complete_problem: view.on_complete_problem.clone(),
            base: (texts, view.start_paused),
            row: 0,
        }
    }

    /// The rows the cursor visits: the program is one only when it can be changed here.
    pub(crate) fn rows(&self) -> Vec<DownloadsRow> {
        let mut rows = vec![
            DownloadsRow::Default,
            DownloadsRow::Unfinished,
            DownloadsRow::Paused,
            DownloadsRow::Watch,
        ];
        if self.editable {
            rows.push(DownloadsRow::Program);
        }
        rows.push(DownloadsRow::Save);
        rows
    }

    fn current(&self) -> DownloadsRow {
        let rows = self.rows();
        rows[self.row.min(rows.len() - 1)]
    }

    fn input_mut(&mut self, row: DownloadsRow) -> Option<&mut Input> {
        match row {
            DownloadsRow::Default => Some(&mut self.default_dir),
            DownloadsRow::Unfinished => Some(&mut self.unfinished_dir),
            DownloadsRow::Watch => Some(&mut self.watch_dir),
            DownloadsRow::Program => Some(&mut self.program),
            DownloadsRow::Paused | DownloadsRow::Save => None,
        }
    }

    fn values(&self) -> ([String; 4], bool) {
        (
            [
                self.default_dir.text.clone(),
                self.unfinished_dir.text.clone(),
                self.watch_dir.text.clone(),
                self.program.text.clone(),
            ],
            self.paused,
        )
    }

    fn dirty(&self) -> bool {
        self.values() != self.base
    }

    fn saved(&mut self) {
        self.base = self.values();
    }

    fn discard(&mut self) {
        let row = self.row;
        let ([default, unfinished, watch, program], paused) = self.base.clone();
        self.default_dir = Input::new(default);
        self.unfinished_dir = Input::new(unfinished);
        self.watch_dir = Input::new(watch);
        self.program = Input::new(program);
        self.paused = paused;
        self.row = row;
    }

    /// The whole section as a change. The program is named only when this surface may change it and
    /// the person did: leaving it out keeps what the server has.
    fn change(&self) -> DownloadsChange {
        let on_complete = if self.editable && self.program.text != self.base.0[3] {
            Some(OnCompleteSet {
                program: typed_path(&self.program.text),
            })
        } else {
            None
        };
        DownloadsChange {
            default_dir: PathBuf::from(self.default_dir.text.trim()),
            unfinished_dir: typed_path(&self.unfinished_dir.text),
            start_paused: self.paused,
            watch_dir: typed_path(&self.watch_dir.text),
            on_complete,
        }
    }
}

/// The settings screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsScreen {
    /// The open section.
    pub(crate) section: TuiSection,
    /// Which half takes the keys.
    pub(crate) focus: Focus,
    pub(crate) speed: SpeedForm,
    pub(crate) downloads: DownloadsForm,
    /// Whether the downloads values have arrived.
    pub(crate) downloads_loaded: bool,
    /// Whether the speed values have arrived.
    pub(crate) speed_loaded: bool,
    /// The server has no settings routes: Speed saves through `SetLimits`, the rest is read-only.
    pub(crate) unsupported: bool,
    /// A text field is taking typed characters.
    pub(crate) editing: bool,
    /// The question asked when leaving with unsaved changes, and what it was about.
    confirm: Option<Pending>,
    /// A save has gone out and its answer has not come back.
    saving: bool,
    /// What to do once that save is answered well.
    pending: Option<Pending>,
    /// The line the last refused save came back with.
    pub(crate) error: Option<String>,
}

impl SettingsScreen {
    /// The screen as `S` opens it, from what the snapshot knows so far. It is on the list, with
    /// Downloads open.
    pub fn new(state: &SettingsState) -> SettingsScreen {
        let mut screen = SettingsScreen {
            section: TuiSection::Downloads,
            focus: Focus::List,
            speed: SpeedForm::new(&zero_limits()),
            downloads: DownloadsForm::new(&empty_downloads()),
            downloads_loaded: false,
            speed_loaded: false,
            unsupported: false,
            editing: false,
            confirm: None,
            saving: false,
            pending: None,
            error: None,
        };
        screen.refresh(state);
        screen
    }

    /// The screen filled from a whole snapshot: what [`refresh`](Self::refresh) does, and the running
    /// limits for a server that has no settings routes.
    pub(crate) fn open(snap: &Snapshot) -> SettingsScreen {
        let mut screen = SettingsScreen::new(&snap.settings);
        screen.sync(snap);
        screen
    }

    /// Takes in a new snapshot: refreshes the settings, and fills Speed from the running limits when
    /// the server has no settings routes to read them from.
    pub(crate) fn sync(&mut self, snap: &Snapshot) {
        self.refresh(&snap.settings);
        if self.unsupported {
            self.fill_speed(&snap.limits);
        }
    }

    /// Refills the sections that are not dirty from `state`. A form with unsaved changes keeps what
    /// is typed in it.
    pub(crate) fn refresh(&mut self, state: &SettingsState) {
        match state {
            SettingsState::NotLoaded => {}
            SettingsState::Unsupported => self.unsupported = true,
            SettingsState::Loaded(view) => {
                self.unsupported = false;
                self.fill(view);
            }
        }
    }

    fn fill(&mut self, view: &SettingsView) {
        self.fill_speed(&view.speed);
        let fresh = DownloadsForm::new(&view.downloads);
        // Unchanged values leave the form alone, so a snapshot every second does not move the cursor
        // out from under somebody typing.
        let same = fresh.values() == self.downloads.base
            && fresh.editable == self.downloads.editable
            && fresh.watch_problem == self.downloads.watch_problem
            && fresh.on_complete_problem == self.downloads.on_complete_problem;
        if !self.downloads.dirty() && (!self.downloads_loaded || !same) {
            self.downloads = DownloadsForm {
                row: self.downloads.row,
                ..fresh
            };
        }
        self.downloads_loaded = true;
    }

    fn fill_speed(&mut self, limits: &LiveLimits) {
        let fresh = SpeedForm::new(limits);
        if !self.speed.dirty() && (!self.speed_loaded || fresh.base != self.speed.base) {
            self.speed = SpeedForm {
                row: self.speed.row,
                ..fresh
            };
        }
        self.speed_loaded = true;
    }

    /// Whether the open section has changes that were not saved.
    pub(crate) fn dirty(&self) -> bool {
        match self.section {
            TuiSection::Downloads => self.downloads.dirty(),
            TuiSection::Speed => self.speed.dirty(),
        }
    }

    /// Whether the question about unsaved changes is on screen, and what it names.
    pub(crate) fn asking(&self) -> bool {
        self.confirm.is_some()
    }

    /// Whether the open section has fields to show.
    pub(crate) fn section_ready(&self) -> bool {
        match self.section {
            TuiSection::Downloads => self.downloads_loaded && !self.unsupported,
            TuiSection::Speed => self.speed_loaded,
        }
    }

    /// Applies one press.
    pub(crate) fn on_key(&mut self, key: KeyEvent, snap: &Snapshot) -> ScreenAnswer {
        if key.kind != KeyEventKind::Press {
            return ScreenAnswer::Stay;
        }
        self.sync(snap);
        if self.confirm.is_some() {
            return self.confirm_key(key);
        }
        if key.modifiers == KeyModifiers::CONTROL {
            return match key.code {
                KeyCode::Char('s') => self.save(None),
                _ => ScreenAnswer::Stay,
            };
        }
        if !matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) {
            return ScreenAnswer::Stay;
        }
        match self.focus {
            Focus::List => self.list_key(key),
            Focus::Fields if self.editing => self.editing_key(key),
            Focus::Fields => self.fields_key(key),
        }
    }

    /// The answer to the question about unsaved changes.
    fn confirm_key(&mut self, key: KeyEvent) -> ScreenAnswer {
        let Some(pending) = self.confirm else {
            return ScreenAnswer::Stay;
        };
        match key.code {
            KeyCode::Char('s') => {
                self.confirm = None;
                self.save(Some(pending))
            }
            KeyCode::Char('d') => {
                self.confirm = None;
                self.discard();
                self.proceed(pending)
            }
            KeyCode::Esc => {
                self.confirm = None;
                ScreenAnswer::Stay
            }
            _ => ScreenAnswer::Stay,
        }
    }

    fn discard(&mut self) {
        self.error = None;
        self.editing = false;
        match self.section {
            TuiSection::Downloads => self.downloads.discard(),
            TuiSection::Speed => self.speed.discard(),
        }
    }

    /// Does what was asked once nothing unsaved stands in the way.
    fn proceed(&mut self, pending: Pending) -> ScreenAnswer {
        match pending {
            Pending::Leave => ScreenAnswer::Close,
            Pending::Section(section) => {
                self.section = section;
                self.editing = false;
                self.error = None;
                ScreenAnswer::Stay
            }
        }
    }

    /// Asks first when there is something unsaved, and otherwise does it.
    fn request(&mut self, pending: Pending) -> ScreenAnswer {
        if self.dirty() {
            self.confirm = Some(pending);
            ScreenAnswer::Stay
        } else {
            self.proceed(pending)
        }
    }

    fn list_key(&mut self, key: KeyEvent) -> ScreenAnswer {
        let at = SECTIONS
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or(0);
        match key.code {
            KeyCode::Up if at > 0 => self.request(Pending::Section(SECTIONS[at - 1])),
            KeyCode::Down if at + 1 < SECTIONS.len() => {
                self.request(Pending::Section(SECTIONS[at + 1]))
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Enter | KeyCode::Right => {
                if self.section_ready() {
                    self.focus = Focus::Fields;
                }
                ScreenAnswer::Stay
            }
            KeyCode::Esc => self.request(Pending::Leave),
            _ => ScreenAnswer::Stay,
        }
    }

    /// How many rows the open section has.
    fn row_count(&self) -> usize {
        match self.section {
            TuiSection::Downloads => self.downloads.rows().len(),
            TuiSection::Speed => SPEED_SAVE_ROW + 1,
        }
    }

    fn row_mut(&mut self) -> &mut usize {
        match self.section {
            TuiSection::Downloads => &mut self.downloads.row,
            TuiSection::Speed => &mut self.speed.row,
        }
    }

    fn move_row(&mut self, delta: i32) {
        let last = self.row_count() - 1;
        let row = self.row_mut();
        *row = (i64::try_from(*row).unwrap_or(0) + i64::from(delta)).clamp(0, last as i64) as usize;
    }

    fn fields_key(&mut self, key: KeyEvent) -> ScreenAnswer {
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = Focus::List;
                ScreenAnswer::Stay
            }
            KeyCode::Up => {
                self.move_row(-1);
                ScreenAnswer::Stay
            }
            KeyCode::Down => {
                self.move_row(1);
                ScreenAnswer::Stay
            }
            KeyCode::Esc => self.request(Pending::Leave),
            KeyCode::Char(' ') => {
                self.toggle();
                ScreenAnswer::Stay
            }
            KeyCode::Enter => self.activate(),
            _ => ScreenAnswer::Stay,
        }
    }

    /// Space: flips the switch when the cursor is on it.
    fn toggle(&mut self) {
        if self.section == TuiSection::Downloads && self.downloads.current() == DownloadsRow::Paused
        {
            self.downloads.paused = !self.downloads.paused;
            self.error = None;
        }
    }

    /// Enter: edits a text field, flips the switch, or saves, by the row it is on.
    fn activate(&mut self) -> ScreenAnswer {
        match self.section {
            TuiSection::Downloads => match self.downloads.current() {
                DownloadsRow::Save => return self.save(None),
                DownloadsRow::Paused => self.toggle(),
                _ => self.editing = true,
            },
            TuiSection::Speed => {
                if self.speed.row == SPEED_SAVE_ROW {
                    return self.save(None);
                }
                self.editing = true;
            }
        }
        ScreenAnswer::Stay
    }

    fn editing_key(&mut self, key: KeyEvent) -> ScreenAnswer {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.editing = false,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                self.editing = false;
                return self.fields_key(key);
            }
            _ => {
                let section = self.section;
                let typed = match section {
                    TuiSection::Downloads => {
                        let row = self.downloads.current();
                        self.downloads
                            .input_mut(row)
                            .is_some_and(|field| type_into(field, key))
                    }
                    TuiSection::Speed => {
                        let at = self.speed.row;
                        let typed = self
                            .speed
                            .fields
                            .get_mut(at)
                            .is_some_and(|field| type_into(field, key));
                        if typed && self.speed.invalid == Some(at) {
                            self.speed.invalid = None;
                        }
                        typed
                    }
                };
                if typed {
                    self.error = None;
                }
            }
        }
        ScreenAnswer::Stay
    }

    /// Saves the open section. `then` is what to do once the save is answered well.
    fn save(&mut self, then: Option<Pending>) -> ScreenAnswer {
        self.editing = false;
        self.error = None;
        let command = match self.section {
            TuiSection::Downloads => {
                if self.unsupported || !self.downloads_loaded {
                    return ScreenAnswer::Stay;
                }
                Command::Settings(SettingsCommand::Save(SettingsChange::Downloads(
                    self.downloads.change(),
                )))
            }
            TuiSection::Speed => {
                let Some(limits) = self.speed.limits() else {
                    return ScreenAnswer::Stay;
                };
                if self.unsupported {
                    // A server with no settings routes takes its limits the way it always did, and
                    // answers nothing the screen could wait for, so this save counts as made.
                    self.speed.saved();
                    return match then {
                        Some(Pending::Leave) => {
                            ScreenAnswer::SendThenClose(Command::SetLimits(limits))
                        }
                        Some(pending) => {
                            self.proceed(pending);
                            ScreenAnswer::Send(Command::SetLimits(limits))
                        }
                        None => ScreenAnswer::Send(Command::SetLimits(limits)),
                    };
                }
                Command::Settings(SettingsCommand::Save(SettingsChange::Speed(limits)))
            }
        };
        self.saving = true;
        self.pending = then;
        ScreenAnswer::Send(command)
    }

    /// What the backend said about the save that went out. True when the screen should close.
    pub(crate) fn on_notice(&mut self, notice: &Notice) -> bool {
        if !self.saving {
            return false;
        }
        match notice {
            Notice::Info(_) => {
                self.saving = false;
                match self.section {
                    TuiSection::Downloads => self.downloads.saved(),
                    TuiSection::Speed => self.speed.saved(),
                }
                match self.pending.take() {
                    Some(Pending::Leave) => return true,
                    Some(pending) => {
                        self.proceed(pending);
                    }
                    None => {}
                }
            }
            Notice::Error(text) | Notice::SignInRefused(text) => {
                self.saving = false;
                self.pending = None;
                self.error = Some(text.clone());
            }
            Notice::Play(_) => {}
        }
        false
    }

    /// The unsaved-changes question's text, when it is being asked.
    pub(crate) fn question(&self) -> Option<String> {
        self.confirm
            .map(|_| format!("You have unsaved changes in {}.", self.section.title()))
    }
}

fn zero_limits() -> LiveLimits {
    LiveLimits {
        download_kib: 0,
        upload_kib: 0,
        active_downloads: None,
        active_seeds: None,
        active_total: None,
    }
}

fn empty_downloads() -> DownloadsView {
    DownloadsView {
        default_dir: PathBuf::new(),
        unfinished_dir: None,
        start_paused: false,
        watch_dir: None,
        on_complete: fbtt_core::OnCompleteView {
            program: None,
            editable: false,
        },
        watch_problem: None,
        on_complete_problem: None,
    }
}

/// One field as a number: nothing typed, or only spaces, is the same as `0`, which every one of the
/// five means as "no limit". Anything else has to be digits and nothing more.
fn number(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    text.parse().ok()
}

/// A queue count: `0` is no limit at all, which [`LiveLimits`] spells `None`. A number too big for
/// this platform's `usize` is the biggest one it has rather than a wrapped-around small one.
fn queue_count(value: u64) -> Option<usize> {
    if value == 0 {
        return None;
    }
    Some(usize::try_from(value).unwrap_or(usize::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_and_zero_are_the_same_number() {
        assert_eq!(number(""), Some(0));
        assert_eq!(number(" 2048 "), Some(2048));
        assert_eq!(number("1.5"), None);
        assert_eq!(number("abc"), None);
        assert_eq!(number("-3"), None);
    }

    #[test]
    fn an_unlimited_queue_is_no_queue() {
        assert_eq!(queue_count(0), None);
        assert_eq!(queue_count(5), Some(5));
    }
}
