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
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use fbtt_core::proxy::ProxyHealth;
use fbtt_core::{
    Command, DownloadsChange, DownloadsView, LiveLimits, Notice, OnCompleteSet, PasswordChange,
    PasswordStorage, ProxyChange, ProxyType, ProxyView, SettingsChange, SettingsCommand,
    SettingsState, SettingsView, Snapshot,
};

use crate::modal::{Input, Masked, type_into, type_into_masked};

/// The refusal a server too old for the settings routes gets, in the plan's words.
pub(crate) const TOO_OLD: &str =
    "This server is too old to change its settings here. Update it to change them.";
/// Where the run-a-program setting lives when this screen may not change it.
pub(crate) const SET_IN_CONFIG: &str = "Set in the server's config file";

/// The sections the terminal has, in the order the web page lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiSection {
    /// Folders, the paused switch, the watch folder and the program run on completion.
    Downloads,
    /// The rates and the queue counts.
    Speed,
    /// The proxy: its type, address and login.
    Proxy,
}

/// Every section, top to bottom.
pub(crate) const SECTIONS: [TuiSection; 3] =
    [TuiSection::Downloads, TuiSection::Speed, TuiSection::Proxy];

/// What the proxy section says when the host is empty.
pub(crate) const NO_HOST: &str = "Enter the proxy's host name or address.";
/// What it says when the port is not a number from 1 to 65535.
pub(crate) const BAD_PORT: &str = "The port must be a number from 1 to 65535.";
/// Shown under the password field when the host or port was edited while a password is saved.
pub(crate) const NEW_ADDRESS: &str =
    "Enter the password again when the proxy's host or port changes.";
/// What a set proxy turns off.
pub(crate) const PROXY_HELP: &str = "While a proxy is set, every connection goes through it. Incoming connections, port forwarding and local peer discovery are off, so you may see fewer peers.";
/// What an HTTP proxy turns off on top of that.
pub(crate) const HTTP_HELP: &str =
    "An HTTP proxy carries TCP only, so the DHT, uTP and UDP trackers are off too.";
/// Shown when the proxy answers TCP but will not relay UDP.
pub(crate) const UDP_LINE: &str =
    "This proxy does not relay UDP, so the DHT and UDP trackers are off";

impl TuiSection {
    /// The title the list and the section's heading show.
    pub fn title(self) -> &'static str {
        match self {
            TuiSection::Downloads => "Downloads",
            TuiSection::Speed => "Speed",
            TuiSection::Proxy => "Proxy",
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

/// The values a save carried, kept until its answer comes back. What the section holds as saved
/// afterwards is exactly this, whatever was typed since.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SentValues {
    /// The five speed texts, and the limits themselves when they went out through `SetLimits`, which
    /// the backend does not acknowledge: the next snapshot that carries them is the answer.
    Speed([String; 5], Option<LiveLimits>),
    /// The downloads texts and the switch.
    Downloads([String; 4], bool),
    /// The proxy's texts, and what was done with the password. Never the password itself.
    Proxy(ProxyBase, PasswordSent),
}

/// What a proxy save did with the password, without holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasswordSent {
    Keep,
    Clear,
    Set,
}

/// A save that went out and has not been answered yet.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InFlight {
    section: TuiSection,
    sent: SentValues,
    /// What to do once the save is answered well.
    pending: Option<Pending>,
    /// The first clock reading after the send, which the timeout counts from. The screen has no
    /// clock of its own: the model's tick is its time.
    started: Option<Instant>,
}

/// How long a save may go unanswered before the screen gives up waiting for it.
const SAVE_TIMEOUT: Duration = Duration::from_secs(10);
/// What the screen says when that happens.
const NO_ANSWER: &str = "No answer from the server.";

/// The one notice a good save is answered with.
const SAVED: &str = "Saved.";

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

/// The proxy fields as texts: what a form was filled with and what a save carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProxyBase {
    pub(crate) kind: ProxyType,
    pub(crate) host: String,
    pub(crate) port: String,
    pub(crate) username: String,
}

/// One row of the proxy section a cursor can stand on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProxyRow {
    Type,
    Host,
    Port,
    User,
    Password,
    ClearPassword,
    Save,
}

/// The proxy section: a type, an address, a login. The password lives only in a masked field and is
/// never filled in from a view: the view says whether one is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProxyForm {
    pub(crate) kind: ProxyType,
    pub(crate) host: Input,
    pub(crate) port: Input,
    pub(crate) username: Input,
    pub(crate) password: Masked,
    /// The stored password is to be removed with the next save.
    pub(crate) clear: bool,
    /// Whether a password is stored, and where it is kept.
    pub(crate) stored: bool,
    pub(crate) storage: PasswordStorage,
    base: ProxyBase,
    /// The field refused last, drawn red.
    pub(crate) invalid: Option<ProxyRow>,
    /// The row the cursor is on, as an index into [`ProxyForm::rows`].
    pub(crate) row: usize,
}

/// The proxy type after `kind`, in the order the form lists them.
fn next_kind(kind: &ProxyType) -> ProxyType {
    match kind {
        ProxyType::None => ProxyType::Http,
        ProxyType::Http => ProxyType::Socks5,
        _ => ProxyType::None,
    }
}

impl ProxyForm {
    fn new(view: &ProxyView) -> ProxyForm {
        let kind = match view.kind {
            ProxyType::Http => ProxyType::Http,
            ProxyType::Socks5 => ProxyType::Socks5,
            _ => ProxyType::None,
        };
        let set = kind != ProxyType::None;
        let base = ProxyBase {
            host: if set {
                view.host.clone()
            } else {
                String::new()
            },
            port: if set && view.port != 0 {
                view.port.to_string()
            } else {
                String::new()
            },
            username: if set {
                view.username.clone().unwrap_or_default()
            } else {
                String::new()
            },
            kind: kind.clone(),
        };
        ProxyForm {
            kind,
            host: Input::new(base.host.clone()),
            port: Input::new(base.port.clone()),
            username: Input::new(base.username.clone()),
            password: Masked::new(),
            clear: false,
            stored: set && view.password_saved,
            storage: view.password_storage,
            base,
            invalid: None,
            row: 0,
        }
    }

    /// The rows the cursor visits: with no proxy only the type and Save.
    pub(crate) fn rows(&self) -> Vec<ProxyRow> {
        let mut rows = vec![ProxyRow::Type];
        if self.kind != ProxyType::None {
            rows.extend([
                ProxyRow::Host,
                ProxyRow::Port,
                ProxyRow::User,
                ProxyRow::Password,
            ]);
            if self.stored && !self.clear {
                rows.push(ProxyRow::ClearPassword);
            }
        }
        rows.push(ProxyRow::Save);
        rows
    }

    fn current(&self) -> ProxyRow {
        let rows = self.rows();
        rows[self.row.min(rows.len() - 1)]
    }

    fn texts(&self) -> ProxyBase {
        ProxyBase {
            kind: self.kind.clone(),
            host: self.host.text.clone(),
            port: self.port.text.clone(),
            username: self.username.text.clone(),
        }
    }

    /// Whether the host or port differs from what the proxy was filled with.
    pub(crate) fn address_changed(&self) -> bool {
        self.host.text.trim() != self.base.host || self.port.text.trim() != self.base.port
    }

    /// The stored password cannot be kept for a new address: it has to be typed again or cleared.
    pub(crate) fn needs_password(&self) -> bool {
        self.stored
            && self.kind != ProxyType::None
            && self.address_changed()
            && self.password.is_empty()
            && !self.clear
    }

    /// Whether the placeholder `saved` belongs in the password field.
    pub(crate) fn shows_saved(&self) -> bool {
        self.stored && !self.clear && !self.address_changed()
    }

    fn dirty(&self) -> bool {
        self.texts() != self.base || !self.password.is_empty() || self.clear
    }

    fn discard(&mut self) {
        let row = self.row;
        let view_stored = self.stored;
        let storage = self.storage;
        let base = self.base.clone();
        *self = ProxyForm {
            kind: base.kind.clone(),
            host: Input::new(base.host.clone()),
            port: Input::new(base.port.clone()),
            username: Input::new(base.username.clone()),
            password: Masked::new(),
            clear: false,
            stored: view_stored,
            storage,
            base,
            invalid: None,
            row,
        };
    }

    fn cycle_kind(&mut self) {
        self.kind = next_kind(&self.kind);
        self.invalid = None;
    }

    /// The form as a change, or `None` with the refusal and the field it is about.
    fn change(&mut self) -> Result<(ProxyChange, PasswordSent), &'static str> {
        if self.kind == ProxyType::None {
            return Ok((
                ProxyChange {
                    kind: ProxyType::None,
                    host: String::new(),
                    port: 0,
                    username: None,
                    password: PasswordChange::Clear,
                },
                PasswordSent::Clear,
            ));
        }
        let host = self.host.text.trim().to_string();
        if host.is_empty() {
            self.invalid = Some(ProxyRow::Host);
            self.row = self
                .rows()
                .iter()
                .position(|r| *r == ProxyRow::Host)
                .unwrap_or(0);
            return Err(NO_HOST);
        }
        let port = self.port.text.trim();
        let port = if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
            port.parse::<u16>().ok().filter(|port| *port != 0)
        } else {
            None
        };
        let Some(port) = port else {
            self.invalid = Some(ProxyRow::Port);
            self.row = self
                .rows()
                .iter()
                .position(|r| *r == ProxyRow::Port)
                .unwrap_or(0);
            return Err(BAD_PORT);
        };
        let username = self.username.text.trim();
        let (password, sent) = if self.clear {
            (PasswordChange::Clear, PasswordSent::Clear)
        } else if !self.password.is_empty() {
            (
                PasswordChange::Set(self.password.text().to_string()),
                PasswordSent::Set,
            )
        } else {
            (PasswordChange::Keep, PasswordSent::Keep)
        };
        Ok((
            ProxyChange {
                kind: self.kind.clone(),
                host,
                port,
                username: (!username.is_empty()).then(|| username.to_string()),
                password,
            },
            sent,
        ))
    }
}

fn empty_proxy() -> ProxyView {
    ProxyView::default()
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
    pub(crate) proxy: ProxyForm,
    /// How the proxy is doing, as the last snapshot said.
    pub(crate) health: ProxyHealth,
    /// Whether the proxy values have arrived.
    pub(crate) proxy_loaded: bool,
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
    in_flight: Option<InFlight>,
    /// The sends that were discarded and may still be answered, oldest first, each with the moment its
    /// answer stops being waited for (its send time plus [`SAVE_TIMEOUT`]; `None` until the next tick
    /// or notice says what time it is). Answers arrive in send order, so while one is inside its
    /// deadline the next answer is its and is ignored rather than taken for a newer save's. A send
    /// that timed out is never listed: it is already past its deadline.
    abandoned: Vec<Option<Instant>>,
    /// A save answered well on the way out: the model closes the screen.
    close_requested: bool,
    /// What a refused Load said, drawn in place of the loading line.
    pub(crate) load_error: Option<String>,
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
            proxy: ProxyForm::new(&empty_proxy()),
            health: ProxyHealth::Off,
            proxy_loaded: false,
            downloads_loaded: false,
            speed_loaded: false,
            unsupported: false,
            editing: false,
            confirm: None,
            in_flight: None,
            abandoned: Vec::new(),
            close_requested: false,
            load_error: None,
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
        // The header's `S: Proxy` hint is shown while the proxy cannot be reached: that is the
        // section the key goes to then.
        if snap.proxy.headline().is_some() {
            screen.section = TuiSection::Proxy;
        }
        screen
    }

    /// Takes in a new snapshot: refreshes the settings, and fills Speed from the running limits when
    /// the server has no settings routes to read them from.
    pub(crate) fn sync(&mut self, snap: &Snapshot) {
        self.refresh(&snap.settings);
        self.health = snap.proxy.clone();
        if self.unsupported {
            // `SetLimits` has no acknowledgement: the snapshot that carries the numbers sent is it.
            if let Some(InFlight {
                sent: SentValues::Speed(_, Some(limits)),
                ..
            }) = &self.in_flight
                && *limits == snap.limits
                && self.complete_save()
            {
                self.close_requested = true;
            }
            self.fill_speed(&snap.limits);
        }
    }

    /// Whether a save is out and waiting for its answer.
    pub(crate) fn saving(&self) -> bool {
        self.in_flight.is_some()
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
        let fresh = ProxyForm::new(&view.proxy);
        if !self.proxy.dirty()
            && (!self.proxy_loaded
                || fresh.base != self.proxy.base
                || fresh.stored != self.proxy.stored
                || fresh.storage != self.proxy.storage)
        {
            self.proxy = ProxyForm {
                row: self.proxy.row,
                ..fresh
            };
        }
        self.proxy_loaded = true;
        self.load_error = None;
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
        self.load_error = None;
    }

    /// Whether the open section has changes that were not saved.
    pub(crate) fn dirty(&self) -> bool {
        match self.section {
            TuiSection::Downloads => self.downloads.dirty(),
            TuiSection::Speed => self.speed.dirty(),
            TuiSection::Proxy => self.proxy.dirty(),
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
            TuiSection::Proxy => self.proxy_loaded && !self.unsupported,
        }
    }

    /// Applies one press.
    pub(crate) fn on_key(&mut self, key: KeyEvent, snap: &Snapshot) -> ScreenAnswer {
        if key.kind != KeyEventKind::Press {
            return ScreenAnswer::Stay;
        }
        self.sync(snap);
        if self.take_close() {
            return ScreenAnswer::Close;
        }
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

    /// Gives up on the save that is out, if any: its answer, when it comes, is not about what the form
    /// holds now.
    ///
    /// `may_answer` is true for a discard, whose answer can still arrive inside the deadline, and false
    /// for a timeout, whose deadline has passed.
    fn abandon_save(&mut self, may_answer: bool) {
        if let Some(flight) = self.in_flight.take() {
            // A send through `SetLimits` has no answer to wait for or to skip.
            if may_answer && !matches!(flight.sent, SentValues::Speed(_, Some(_))) {
                self.abandoned
                    .push(flight.started.map(|at| at + SAVE_TIMEOUT));
            }
        }
    }

    /// Settles the abandoned sends against the clock: those with no deadline yet get one, and those
    /// past it are forgotten.
    fn settle_abandoned(&mut self, now: Instant) {
        for deadline in &mut self.abandoned {
            deadline.get_or_insert(now + SAVE_TIMEOUT);
        }
        self.abandoned
            .retain(|deadline| deadline.is_some_and(|at| at > now));
    }

    fn discard(&mut self) {
        self.abandon_save(true);
        self.error = None;
        self.editing = false;
        match self.section {
            TuiSection::Downloads => self.downloads.discard(),
            TuiSection::Speed => self.speed.discard(),
            TuiSection::Proxy => self.proxy.discard(),
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
            TuiSection::Proxy => self.proxy.rows().len(),
        }
    }

    fn row_mut(&mut self) -> &mut usize {
        match self.section {
            TuiSection::Downloads => &mut self.downloads.row,
            TuiSection::Speed => &mut self.speed.row,
            TuiSection::Proxy => &mut self.proxy.row,
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
            KeyCode::Left | KeyCode::Right if self.on_proxy_type() => {
                self.proxy.cycle_kind();
                self.error = None;
                ScreenAnswer::Stay
            }
            KeyCode::Enter => self.activate(),
            _ => ScreenAnswer::Stay,
        }
    }

    /// Whether the cursor is on the proxy type.
    fn on_proxy_type(&self) -> bool {
        self.section == TuiSection::Proxy && self.proxy.current() == ProxyRow::Type
    }

    /// Space: flips the switch when the cursor is on it, or the proxy type or `Clear password`.
    fn toggle(&mut self) {
        if self.section == TuiSection::Proxy {
            match self.proxy.current() {
                ProxyRow::Type => {
                    self.proxy.cycle_kind();
                    self.error = None;
                }
                ProxyRow::ClearPassword => self.clear_password(),
                _ => {}
            }
            return;
        }
        if self.section == TuiSection::Downloads && self.downloads.current() == DownloadsRow::Paused
        {
            self.downloads.paused = !self.downloads.paused;
            self.error = None;
        }
    }

    /// Chooses `Clear password`: the stored password is removed with the next save.
    fn clear_password(&mut self) {
        self.proxy.clear = true;
        self.proxy.password.clear();
        self.error = None;
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
            TuiSection::Proxy => match self.proxy.current() {
                ProxyRow::Save => return self.save(None),
                ProxyRow::Type | ProxyRow::ClearPassword => self.toggle(),
                _ => self.editing = true,
            },
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
                    TuiSection::Proxy => {
                        let row = self.proxy.current();
                        let typed = match row {
                            ProxyRow::Host => type_into(&mut self.proxy.host, key),
                            ProxyRow::Port => type_into(&mut self.proxy.port, key),
                            ProxyRow::User => type_into(&mut self.proxy.username, key),
                            ProxyRow::Password => {
                                let typed = type_into_masked(&mut self.proxy.password, key);
                                if typed {
                                    // Typing a password replaces a request to clear it.
                                    self.proxy.clear = false;
                                }
                                typed
                            }
                            _ => false,
                        };
                        if typed && self.proxy.invalid == Some(row) {
                            self.proxy.invalid = None;
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

    /// Saves the open section. `then` is what to do once the save is answered well. While a save is
    /// out nothing is sent again.
    fn save(&mut self, then: Option<Pending>) -> ScreenAnswer {
        if let Some(flight) = self.in_flight.as_mut() {
            // The question's answer is for after this save: it goes on waiting for it.
            if then.is_some() {
                flight.pending = then;
            }
            return ScreenAnswer::Stay;
        }
        self.editing = false;
        self.error = None;
        let (command, sent) = match self.section {
            TuiSection::Downloads => {
                if self.unsupported || !self.downloads_loaded {
                    return ScreenAnswer::Stay;
                }
                let (texts, paused) = self.downloads.values();
                (
                    Command::Settings(SettingsCommand::Save(SettingsChange::Downloads(
                        self.downloads.change(),
                    ))),
                    SentValues::Downloads(texts, paused),
                )
            }
            TuiSection::Speed => {
                let Some(limits) = self.speed.limits() else {
                    return ScreenAnswer::Stay;
                };
                if self.unsupported {
                    // A server with no settings routes takes its limits the way it always did.
                    (
                        Command::SetLimits(limits),
                        SentValues::Speed(self.speed.values(), Some(limits)),
                    )
                } else {
                    (
                        Command::Settings(SettingsCommand::Save(SettingsChange::Speed(limits))),
                        SentValues::Speed(self.speed.values(), None),
                    )
                }
            }
            TuiSection::Proxy => {
                if self.unsupported || !self.proxy_loaded {
                    return ScreenAnswer::Stay;
                }
                // Save stays off until the stored password is typed again or cleared.
                if self.proxy.needs_password() {
                    return ScreenAnswer::Stay;
                }
                match self.proxy.change() {
                    Ok((change, sent)) => (
                        Command::Settings(SettingsCommand::Save(SettingsChange::Proxy(change))),
                        SentValues::Proxy(self.proxy.texts(), sent),
                    ),
                    Err(refusal) => {
                        self.error = Some(refusal.to_string());
                        return ScreenAnswer::Stay;
                    }
                }
            }
        };
        self.in_flight = Some(InFlight {
            section: self.section,
            sent,
            pending: then,
            started: None,
        });
        ScreenAnswer::Send(command)
    }

    /// A save was answered well: what it carried is what the section has saved now, and the form stays
    /// dirty if it was edited since. Returns whether the screen should close.
    fn complete_save(&mut self) -> bool {
        let Some(flight) = self.in_flight.take() else {
            return false;
        };
        match flight.sent {
            SentValues::Speed(values, _) => self.speed.base = values,
            SentValues::Downloads(texts, paused) => self.downloads.base = (texts, paused),
            SentValues::Proxy(base, password) => {
                self.proxy.base = base;
                match password {
                    PasswordSent::Keep => {}
                    PasswordSent::Set => {
                        self.proxy.stored = true;
                        self.proxy.password.clear();
                    }
                    PasswordSent::Clear => {
                        self.proxy.stored = false;
                        self.proxy.clear = false;
                        self.proxy.password.clear();
                    }
                }
            }
        }
        // Edited since: the section still has unsaved changes, so the question that was waiting on
        // this save is asked again rather than answered.
        let clean = match flight.section {
            TuiSection::Downloads => !self.downloads.dirty(),
            TuiSection::Speed => !self.speed.dirty(),
            TuiSection::Proxy => !self.proxy.dirty(),
        };
        match flight.pending {
            Some(Pending::Leave) if clean => return true,
            Some(Pending::Section(section)) if clean => {
                self.proceed(Pending::Section(section));
            }
            _ => {}
        }
        false
    }

    /// Counts the time a save has been waiting. After [`SAVE_TIMEOUT`] without an answer the screen
    /// says so and frees the Save row; the form stays as typed, still unsaved.
    pub(crate) fn tick(&mut self, now: Instant) {
        self.settle_abandoned(now);
        let Some(flight) = self.in_flight.as_mut() else {
            return;
        };
        let started = *flight.started.get_or_insert(now);
        if now.saturating_duration_since(started) >= SAVE_TIMEOUT {
            self.abandon_save(false);
            self.error = Some(NO_ANSWER.to_string());
        }
    }

    /// Whether a good save asked for the screen to close, and forgets it.
    pub(crate) fn take_close(&mut self) -> bool {
        std::mem::take(&mut self.close_requested)
    }

    /// What the backend said. True when the screen should close.
    ///
    /// While a save is out, the first `Saved.` or the first error is its answer and nothing else is:
    /// a line about another torrent is not about this form. A refused Load is shown where the loading
    /// line was.
    pub(crate) fn on_notice(&mut self, notice: &Notice, now: Instant) -> bool {
        // An answer to a send that was given up on is not about the one that is out now.
        let answer = matches!(notice, Notice::Error(_) | Notice::SignInRefused(_))
            || matches!(notice, Notice::Info(text) if text == SAVED);
        self.settle_abandoned(now);
        if answer && !self.abandoned.is_empty() {
            self.abandoned.remove(0);
            return false;
        }
        if self.in_flight.is_none() {
            if let Notice::Error(text) = notice
                && !self.speed_loaded
                && !self.proxy_loaded
                && !self.downloads_loaded
                && !self.unsupported
            {
                self.load_error = Some(text.clone());
            }
            return false;
        }
        match notice {
            Notice::Info(text) if text == SAVED => return self.complete_save(),
            Notice::Error(text) | Notice::SignInRefused(text) => {
                self.in_flight = None;
                self.error = Some(text.clone());
            }
            Notice::Info(_) | Notice::Play(_) => {}
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
