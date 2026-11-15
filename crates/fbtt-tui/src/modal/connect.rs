//! The connect screen: which library this run drives, and how it signs in when that library is a
//! server's.
//!
//! It answers one question - where do the torrents come from - and hands the answer back as a
//! [`ConnectRequest`] for the binary to start a backend with. Nothing here connects: the screen never
//! waits, so a server that is down or an address with a typo in it is reported after this closes, by
//! the connection itself. The one thing it does look at is a device key file, and only to ask it
//! whether it needs a passphrase before asking the person for one.
//!
//! Secrets are the reason this dialog has its own field type: a password lives in a [`Masked`] and from
//! there into [`SecretInput`]'s zeroizing string, so no copy of it is left in the model, in a log line
//! or in what `Debug` prints.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use fbtt_core::ui_state::{LoginMethod, RemoteTarget};
use ssh_key::PrivateKey;
use zeroize::Zeroizing;

use crate::modal::add::expand_tilde;
use crate::modal::{
    Answer, Input, Masked, advice, caution, choice, field, keys_hint, masked_field, problem,
    toggle, type_into, type_into_masked,
};
use crate::model::Action;
use ratatui::text::Line;

/// The key path the screen offers before anybody types one: what OpenSSH uses for an Ed25519 identity,
/// which is the only key type a featherbittorrent server accepts.
const DEFAULT_KEY_PATH: &str = "~/.ssh/id_ed25519";

/// What the screen says about a password that is about to cross a network it cannot encrypt. It is not
/// a refusal - plenty of people run a server on a LAN behind nothing at all - so it stops for an answer
/// rather than explaining itself in a line somebody has already scrolled past.
const PLAIN_HTTP_WARNING: &str =
    "Password sent without encryption: use https through a reverse proxy.";

/// The words the login row cycles through, in the order `Left` and `Right` walk them. They name what
/// the person is being asked for, because that is what the rows under it turn out to be.
const METHODS: [(LoginMethod, &str); 6] = [
    (LoginMethod::None, "no login"),
    (LoginMethod::Password, "password"),
    (LoginMethod::KeyFile, "key file"),
    (LoginMethod::Agent, "ssh-agent"),
    (LoginMethod::GatewayKey, "gateway private key"),
    (LoginMethod::GatewayBasic, "gateway password"),
];

/// What the person is being asked about a gateway's token endpoint, in the order `Tab` walks it.
const TOKEN_URL_LABEL: &str = "Token URL";
const CLIENT_ID_LABEL: &str = "Client id";
/// The toggle that says which half of a key pair signs: this machine's file, or ssh-agent.
const AGENT_LABEL: &str = "Sign with ssh-agent";
/// The toggle that says whether the password goes to the system keychain as well as to the server.
const REMEMBER_LABEL: &str = "Remember in the system keychain";

/// How big a device key file may be. An OpenSSH private key is a few hundred bytes, or a few thousand
/// with its passphrase wrapper around it; something this big is somebody pointing at the wrong file,
/// and this read happens on the thread that draws the screen (the add dialog caps its read the same
/// way, and for the same reason).
const MAX_KEY_BYTES: u64 = 1 << 20;

/// Where the torrents come from, as the connect screen answered that question.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectTarget {
    /// The engine in this process, on this library.
    Local,
    /// A server, at the address and login the screen was given.
    Remote(RemoteTarget),
}

/// What the connect screen collected for the secret a target needs, if it needs one.
///
/// `Debug` is safe to print: a zeroizing string prints that it exists and not what is in it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SecretInput {
    /// The password, for a method that signs in with one. Overwritten when this value goes.
    pub password: Option<Zeroizing<String>>,
    /// The passphrase of the device key file, `None` when the file is not encrypted or when ssh-agent
    /// holds the key and its bytes are never read at all.
    pub passphrase: Option<Zeroizing<String>>,
    /// Whether the password should go into the system keychain as well, under this user name and URL.
    pub remember: bool,
}

/// One answer to the connect screen: a backend to run on, and what to sign in to it with.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectRequest {
    /// The engine or server to start on.
    pub target: ConnectTarget,
    /// What it takes to sign in there, if anything.
    pub secret: SecretInput,
}

/// What the person is choosing on the connect screen.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectForm {
    /// Whether a server is being described, or this library's own engine. The other rows only mean
    /// something for a server, so they come and go with this.
    server: bool,
    /// The server's address, scheme included.
    url: Input,
    /// Which of [`METHODS`] is selected.
    login: LoginMethod,
    /// The user name a password login signs in as.
    user: Input,
    /// The password, held only where it is typed.
    password: Masked,
    /// Whether the gateway private-key method signs with ssh-agent rather than with a key file.
    agent: bool,
    /// Where a gateway login gets its token.
    token_url: Input,
    /// The client id this installation is known to that gateway by.
    client_id: Input,
    /// The device key to sign with, or to look for in ssh-agent.
    key_path: Input,
    /// Whether the password goes to the system keychain too.
    remember: bool,
    /// Which row of the ones this state shows takes what is typed.
    focused: usize,
    /// The warning has been shown for this address and method: a second Enter accepts it. It is dropped
    /// when either changes, so a different server is asked about afresh.
    warned: bool,
    /// The key file whose passphrase is being asked for. Set when Enter found the file encrypted; while
    /// it is set there is a masked prompt that has to be answered before the connect goes out.
    asking: Option<PathBuf>,
    /// The passphrase typed into that prompt.
    passphrase: Masked,
    /// Why the last Enter did not go ahead, shown in red under the rows.
    error: Option<String>,
}

/// A row of the connect screen. Which ones are on it depends on the target and the login method, so a
/// person who chooses a key file is never asked for a password and a person who chooses no login is not
/// asked for anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// `Local engine` or `Server`.
    Target,
    /// The address.
    Url,
    /// Which login method is selected.
    Login,
    /// The user name.
    User,
    /// The password.
    Password,
    /// Whether the keychain keeps it.
    Remember,
    /// A gateway's token endpoint.
    TokenUrl,
    /// This installation's id at that gateway.
    ClientId,
    /// Whether ssh-agent signs instead of a key file.
    Agent,
    /// The device key to sign with.
    KeyPath,
    /// The passphrase of that key, while the screen knows it needs one.
    Passphrase,
}

impl ConnectForm {
    /// The screen as `c` opens it: pointed at a server, and prefilled with `last` when this library has
    /// been connected somewhere before (or is being offered the server running on its own library).
    pub fn new(last: Option<&RemoteTarget>) -> ConnectForm {
        let last_key_path = last.and_then(|target| target.key_path.as_ref());
        ConnectForm {
            server: true,
            url: Input::new(last.map_or("", |target| target.url.as_str())),
            login: last.map_or(LoginMethod::None, |target| target.login),
            user: Input::new(
                last.and_then(|target| target.user.as_deref())
                    .unwrap_or_default(),
            ),
            password: Masked::new(),
            // A gateway target with no key file named is one that signs through ssh-agent, which is the
            // only other thing this client can sign a gateway assertion with.
            agent: last.is_some_and(|target| {
                target.login == LoginMethod::GatewayKey && target.key_path.is_none()
            }),
            token_url: Input::new(
                last.and_then(|target| target.token_url.as_deref())
                    .unwrap_or_default(),
            ),
            client_id: Input::new(
                last.and_then(|target| target.client_id.as_deref())
                    .unwrap_or_default(),
            ),
            key_path: Input::new(match last_key_path {
                Some(path) => path.to_string_lossy().into_owned(),
                None => DEFAULT_KEY_PATH.to_string(),
            }),
            remember: false,
            focused: 0,
            warned: false,
            asking: None,
            passphrase: Masked::new(),
            error: None,
        }
    }

    /// Applies one press. `Tab` walks the rows this state shows, Enter asks to connect, and Left and
    /// Right move within typed text or change what is selected where there is nothing to type. Esc is
    /// the caller's to answer as closing, except while a key is waiting to be opened.
    pub(crate) fn on_key(&mut self, key: KeyEvent) -> Answer {
        match key.code {
            KeyCode::Tab => {
                self.cycle_focus(true);
                Answer::Stay
            }
            KeyCode::BackTab => {
                self.cycle_focus(false);
                Answer::Stay
            }
            KeyCode::Esc => {
                // Coming back out of a passphrase prompt is an answer too: the key path was the wrong
                // one, and closing the whole screen would be more than that mistake deserves.
                if self.asking.take().is_some() {
                    self.clamp_focus();
                    Answer::Stay
                } else {
                    Answer::Close(Vec::new())
                }
            }
            KeyCode::Enter => self.submit(),
            code => {
                if matches!(code, KeyCode::Left | KeyCode::Right) && self.focused_is_choice() {
                    self.step_choice(code == KeyCode::Right);
                    return Answer::Stay;
                }
                if self.type_field(key) {
                    // The reason describes text that is no longer there.
                    self.error = None;
                }
                Answer::Stay
            }
        }
    }

    /// The rows this state shows, in the order `Tab` walks them.
    fn shown(&self) -> Vec<Field> {
        let mut fields = vec![Field::Target];
        if !self.server {
            return fields;
        }
        fields.push(Field::Url);
        fields.push(Field::Login);
        match self.login {
            LoginMethod::None => {}
            LoginMethod::Password | LoginMethod::GatewayBasic => {
                fields.extend([Field::User, Field::Password, Field::Remember]);
            }
            // An agent-held key is found by its public half, which lives beside it as `.pub`, so the path
            // is still worth asking for even though its bytes are never read.
            LoginMethod::KeyFile | LoginMethod::Agent => fields.push(Field::KeyPath),
            LoginMethod::GatewayKey => {
                fields.extend([Field::TokenUrl, Field::ClientId, Field::Agent]);
                if !self.agent {
                    fields.push(Field::KeyPath);
                }
            }
        }
        if self.asking.is_some() {
            fields.push(Field::Passphrase);
        }
        fields
    }

    /// The row the cursor is on.
    fn focused_field(&self) -> Field {
        *self.shown().get(self.focused).unwrap_or(&Field::Target)
    }

    /// Whether this row is chosen with Left and Right rather than typed into.
    fn focused_is_choice(&self) -> bool {
        matches!(
            self.focused_field(),
            Field::Target | Field::Login | Field::Remember | Field::Agent
        )
    }

    /// Moves the cursor to the next row, wrapping at either end: a form this short should not need a
    /// person to hold Shift to get back to the top of it.
    fn cycle_focus(&mut self, forward: bool) {
        let count = self.shown().len();
        if count == 0 {
            self.focused = 0;
            return;
        }
        self.focused = if forward {
            (self.focused + 1) % count
        } else {
            (self.focused + count - 1) % count
        };
    }

    /// Puts the cursor on `field`, which is where a row that only appears in answer to something needs
    /// it: a passphrase prompt nobody can see is not a prompt.
    fn focus_on(&mut self, field: Field) {
        if let Some(at) = self.shown().iter().position(|shown| *shown == field) {
            self.focused = at;
        }
    }

    /// Keeps the cursor on a row that still exists, after rows above it have come or gone.
    fn clamp_focus(&mut self) {
        let last = self.shown().len().saturating_sub(1);
        self.focused = self.focused.min(last);
    }

    /// Left and Right on a row that is chosen rather than typed.
    fn step_choice(&mut self, forward: bool) {
        match self.focused_field() {
            Field::Target => {
                self.server = !self.server;
                self.error = None;
                self.asking = None;
            }
            Field::Login => {
                self.login = next_method(self.login, forward);
                // A different method may not send a password at all, and a different address may encrypt
                // it: either way the warning has to be earned again.
                self.warned = false;
                self.error = None;
                self.asking = None;
                self.clamp_focus();
            }
            Field::Remember => self.remember = !self.remember,
            Field::Agent => {
                self.agent = !self.agent;
                self.error = None;
                self.asking = None;
                self.clamp_focus();
            }
            // Not a choice row: the caller let Left and Right through to the field.
            _ => {}
        }
    }

    /// Types into the row that has the cursor. False means the key belonged to the dialog.
    fn type_field(&mut self, key: KeyEvent) -> bool {
        match self.focused_field() {
            Field::Url => {
                let before = self.url.text.clone();
                if !type_into(&mut self.url, key) {
                    return false;
                }
                // A different address is a different answer about encryption, so the warning starts over.
                if self.url.text != before {
                    self.warned = false;
                }
                true
            }
            Field::User => type_into(&mut self.user, key),
            Field::TokenUrl => type_into(&mut self.token_url, key),
            Field::ClientId => type_into(&mut self.client_id, key),
            Field::KeyPath => {
                let before = self.key_path.text.clone();
                if !type_into(&mut self.key_path, key) {
                    return false;
                }
                // The prompt was about the file that used to be named here.
                if self.key_path.text != before {
                    self.asking = None;
                }
                true
            }
            Field::Password => type_into_masked(&mut self.password, key),
            Field::Passphrase => type_into_masked(&mut self.passphrase, key),
            Field::Target | Field::Login | Field::Remember | Field::Agent => false,
        }
    }

    /// Enter: check what was typed, ask a key file whether it needs a passphrase, and only then send the
    /// request. Whatever stopped it, the screen stays up with what was typed kept and the reason in it.
    fn submit(&mut self) -> Answer {
        if !self.server {
            return Answer::Close(vec![Action::Connect(ConnectRequest {
                target: ConnectTarget::Local,
                secret: SecretInput::default(),
            })]);
        }

        let url = self.url.text.trim().to_string();
        if server_host(&url).is_none() {
            self.error = Some("Use an http:// or https:// address.".to_string());
            return Answer::Stay;
        }

        // Each method is asked for its own pieces before any of them goes out: a target that cannot be
        // connected to is the screen's mistake to catch, not something to hand to a backend.
        let needs_user = matches!(
            self.login,
            LoginMethod::Password | LoginMethod::GatewayBasic
        );
        if needs_user && self.user.text.trim().is_empty() {
            self.error = Some("This login needs a user name.".to_string());
            return Answer::Stay;
        }
        if needs_user && self.password.is_empty() {
            self.error = Some("This login needs a password.".to_string());
            return Answer::Stay;
        }
        if self.login == LoginMethod::GatewayKey {
            if self.token_url.text.trim().is_empty() {
                self.error = Some("This login needs the provider's token URL.".to_string());
                return Answer::Stay;
            }
            if self.client_id.text.trim().is_empty() {
                self.error =
                    Some("This login needs the client id this installation uses.".to_string());
                return Answer::Stay;
            }
        }

        // A password that is about to cross an unencrypted hop gets asked about once, and only once per
        // address: a person who has read it and pressed Enter again has said they accept.
        if needs_user && !self.warned && password_crosses_the_network(&url) {
            self.warned = true;
            self.error = None;
            return Answer::Stay;
        }

        // The key file is the one thing here this screen looks at, because it decides whether there is a
        // question left to ask. It is a small file on this machine, read with the same cap the add dialog
        // gives a torrent file.
        let uses_key_file = self.login == LoginMethod::KeyFile
            || (self.login == LoginMethod::GatewayKey && !self.agent);
        let passphrase = if uses_key_file {
            match self.key_path_text() {
                None => {
                    self.error = Some("This login needs the path of a device key.".to_string());
                    return Answer::Stay;
                }
                Some(path) => match self.asking.take() {
                    // The prompt is up: ask the file whether what was typed opens it.
                    Some(asked) => {
                        if self.passphrase.is_empty() {
                            self.asking = Some(asked);
                            return Answer::Stay;
                        }
                        match open_device_key(&asked, self.passphrase.text()) {
                            Ok(()) => Some(self.passphrase.take()),
                            Err(reason) => {
                                self.asking = Some(asked);
                                self.error = Some(reason);
                                return Answer::Stay;
                            }
                        }
                    }
                    None => match device_key_needs_a_passphrase(&path) {
                        Ok(true) => {
                            self.asking = Some(path);
                            self.focus_on(Field::Passphrase);
                            return Answer::Stay;
                        }
                        Ok(false) => None,
                        Err(reason) => {
                            self.error = Some(reason);
                            return Answer::Stay;
                        }
                    },
                },
            }
        } else {
            None
        };

        Answer::Close(vec![Action::Connect(self.request(passphrase))])
    }

    /// The device key path as typed, `None` when the row is empty.
    fn key_path_text(&self) -> Option<PathBuf> {
        let text = self.key_path.text.trim();
        (!text.is_empty()).then(|| expand_tilde(text))
    }

    /// What was chosen, as the value the binary starts a backend with. The secret pieces are moved out
    /// of the fields rather than copied, so this is the last place they exist.
    fn request(&mut self, passphrase: Option<Zeroizing<String>>) -> ConnectRequest {
        let password_method = matches!(
            self.login,
            LoginMethod::Password | LoginMethod::GatewayBasic
        );
        let names_key_path = matches!(
            self.login,
            LoginMethod::KeyFile | LoginMethod::Agent | LoginMethod::GatewayKey
        ) && (!self.agent || self.login == LoginMethod::Agent);

        let target = RemoteTarget {
            url: self.url.text.trim().to_string(),
            login: self.login,
            user: non_empty(&self.user),
            // A gateway login that signs through ssh-agent names no file: which identity to use is the
            // agent's answer, and `None` is what says so to a backend. An ssh-agent login for the server
            // itself keeps its path because that is how the right identity is found among several.
            key_path: if names_key_path {
                non_empty(&self.key_path).map(|text| expand_tilde(&text))
            } else {
                None
            },
            token_url: (self.login == LoginMethod::GatewayKey)
                .then(|| non_empty(&self.token_url))
                .flatten(),
            client_id: (self.login == LoginMethod::GatewayKey)
                .then(|| non_empty(&self.client_id))
                .flatten(),
        };

        ConnectRequest {
            target: ConnectTarget::Remote(target),
            secret: SecretInput {
                password: (password_method && !self.password.is_empty())
                    .then(|| self.password.take()),
                passphrase,
                remember: password_method && self.remember,
            },
        }
    }
}

/// A field's text as `Some`, or `None` when nothing was typed. An empty user name and no user name are
/// the same thing to a backend, so the screen does not send one.
fn non_empty(field: &Input) -> Option<String> {
    let text = field.text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The next login method, wrapping at either end.
fn next_method(current: LoginMethod, forward: bool) -> LoginMethod {
    let at = METHODS
        .iter()
        .position(|(method, _)| *method == current)
        .unwrap_or(0);
    let count = METHODS.len();
    let next = if forward {
        (at + 1) % count
    } else {
        (at + count - 1) % count
    };
    METHODS[next].0
}

/// The words beside the login row.
fn method_words(login: LoginMethod) -> &'static str {
    METHODS
        .iter()
        .find(|(method, _)| *method == login)
        .map_or("login", |(_, words)| words)
}

/// The host an address names, `None` when it does not name one at all: no scheme this client can use, or
/// a scheme with nothing after it. Whether the rest of it is a real address is the connection's business;
/// this is only the question of what to call it and whether it needs encrypting.
fn server_host(url: &str) -> Option<&str> {
    let without_scheme = match (strip_scheme(url, "http://"), strip_scheme(url, "https://")) {
        (Some((_, rest)), _) | (None, Some((_, rest))) => rest,
        (None, None) => return None,
    };
    // The authority ends where the path, the query or a fragment begins, and starts after whatever
    // user name was spelled into it.
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = match authority.strip_prefix('[') {
        // A literal IPv6 address keeps its brackets and puts the port after them.
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    (!host.is_empty()).then_some(host)
}

/// `url` without `scheme`, when it starts with it. Terminals report what a person typed, and people type
/// `HTTP://` as readily as `http://`.
fn strip_scheme<'a>(url: &'a str, scheme: &str) -> Option<(&'a str, &'a str)> {
    let (head, rest) = url.split_at(scheme.len().min(url.len()));
    head.eq_ignore_ascii_case(scheme).then_some((head, rest))
}

/// Whether this address would carry a password over a network it cannot encrypt: plain `http://` to a
/// host that is not this machine. Loopback needs no encryption because nothing leaves the machine, and
/// the screen says so about everything else rather than refusing it - plenty of servers run on a LAN
/// behind a proxy that has not been set up yet, and some people mean that.
fn password_crosses_the_network(url: &str) -> bool {
    match (strip_scheme(url, "http://"), server_host(url)) {
        (Some(_), Some(host)) => !is_loopback(host),
        _ => false,
    }
}

/// Whether a host means this machine: `localhost` and its aliases, anything in `127.0.0.0/8`, and the
/// IPv6 loopback address. Written out rather than resolved because this decides a warning, not a route,
/// and a name that resolves to loopback by way of `/etc/hosts` is still worth telling somebody about.
fn is_loopback(host: &str) -> bool {
    // A trailing dot is the same host spelled absolutely, and people type it when they mean "this exact
    // name and no search domain".
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    if host == "::1" || host == "0:0:0:0:0:0:0:1" {
        return true;
    }
    let mut octets = host.split('.');
    let Some(first) = octets.next() else {
        return false;
    };
    first.parse::<u8>().is_ok_and(|first| first == 127)
        && octets.clone().count() == 3
        && octets.all(|octet| octet.parse::<u8>().is_ok())
}

/// Whether the device key at `path` is encrypted, which is the question that decides whether there is a
/// passphrase to ask for. Reading it is bounded: the size is asked for first, and the read itself stops
/// one byte past [`MAX_KEY_BYTES`] so that a path pointing at something endless cannot stop the thread
/// that draws the screen (the same shape `add::read_torrent` gives a `.torrent`).
fn device_key_needs_a_passphrase(path: &Path) -> Result<bool, String> {
    read_device_key(path).map(|key| key.is_encrypted())
}

/// Whether `passphrase` opens the device key at `path`. The screen asks before connecting so that a wrong
/// passphrase is corrected here rather than reported by a connection that never got as far as using it.
fn open_device_key(path: &Path, passphrase: &str) -> Result<(), String> {
    read_device_key(path)?
        .decrypt(passphrase)
        .map(|_| ())
        .map_err(|_| format!("That passphrase did not open {}", path.display()))
}

/// The private key at `path`, or one line saying why there is not one to be had: the operating system's
/// own words for a path that is not there, and ours for a file that is too big to be a key or is not a
/// key at all.
fn read_device_key(path: &Path) -> Result<PrivateKey, String> {
    let not_a_key = || format!("{} is not an OpenSSH private key", path.display());
    let size = std::fs::metadata(path)
        .map_err(|problem| problem.to_string())?
        .len();
    if size > MAX_KEY_BYTES {
        return Err(format!("{} is too big to be a device key", path.display()));
    }
    let file = std::fs::File::open(path).map_err(|problem| problem.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_KEY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|problem| problem.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_KEY_BYTES {
        return Err(format!("{} is too big to be a device key", path.display()));
    }
    PrivateKey::read_openssh(&mut std::io::Cursor::new(&bytes)).map_err(|_| not_a_key())
}

/// The connect screen's lines: the rows this state has, what stopped the last Enter, what accepting the
/// unencrypted password would mean, and what each key does here.
pub(crate) fn lines(form: &ConnectForm) -> Vec<Line<'static>> {
    let fields = form.shown();
    // The label column is as wide as the widest *field* label: a toggle's own label follows its box, so
    // it does not push every field on the screen to the right.
    let width = fields
        .iter()
        .filter(|field| !matches!(field, Field::Remember | Field::Agent))
        .map(|field| label(field).chars().count())
        .max()
        .unwrap_or_default();

    let mut lines: Vec<Line<'static>> = fields
        .iter()
        .enumerate()
        .map(|(index, field)| line(form, *field, index == form.focused, width))
        .collect();

    if let Some(error) = &form.error {
        lines.push(problem(error));
    }
    if form.warned {
        lines.push(caution(PLAIN_HTTP_WARNING));
    }
    lines.push(advice(
        "Left and Right move in typed text, or change a choice",
    ));
    if form.asking.is_some() {
        lines.push(keys_hint(&[
            ("Tab", "field"),
            ("Enter", "open the key"),
            ("Esc", "back"),
        ]));
    } else {
        lines.push(keys_hint(&[
            ("Tab", "field"),
            ("Enter", "connect"),
            ("Esc", "cancel"),
        ]));
    }
    lines
}

/// One row.
fn line(form: &ConnectForm, row: Field, focused: bool, width: usize) -> Line<'static> {
    match row {
        Field::Target => choice(
            label(&row),
            if form.server {
                "Server"
            } else {
                "Local engine"
            },
            width,
            focused,
        ),
        Field::Login => choice(label(&row), method_words(form.login), width, focused),
        Field::Remember => toggle(REMEMBER_LABEL, form.remember, focused),
        Field::Agent => toggle(AGENT_LABEL, form.agent, focused),
        Field::Url => field(label(&row), &form.url, width, false),
        Field::User => field(label(&row), &form.user, width, false),
        Field::TokenUrl => field(label(&row), &form.token_url, width, false),
        Field::ClientId => field(label(&row), &form.client_id, width, false),
        Field::KeyPath => field(label(&row), &form.key_path, width, false),
        Field::Password => masked_field(label(&row), &form.password, width),
        Field::Passphrase => masked_field(label(&row), &form.passphrase, width),
    }
}

/// A row's label. The two toggles answer themselves in `line`.
fn label(row: &Field) -> &'static str {
    match row {
        Field::Target => "Connect to",
        Field::Url => "URL",
        Field::Login => "Login",
        Field::User => "User",
        Field::Password => "Password",
        Field::Remember => REMEMBER_LABEL,
        Field::TokenUrl => TOKEN_URL_LABEL,
        Field::ClientId => CLIENT_ID_LABEL,
        Field::Agent => AGENT_LABEL,
        Field::KeyPath => "Key path",
        Field::Passphrase => "Passphrase",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_names_a_host_or_does_not_connect() {
        assert_eq!(server_host("https://bit.example"), Some("bit.example"));
        assert_eq!(server_host("http://127.0.0.1:8090"), Some("127.0.0.1"));
        // The path, the query and a user name spelled into the authority are none of the host.
        assert_eq!(
            server_host("https://alice@bit.example/torrents?x=1"),
            Some("bit.example")
        );
        assert_eq!(server_host("http://[::1]:8090/list"), Some("::1"));
        // No scheme, or a scheme with nothing after it, is not an address this client can dial.
        for url in [
            "bit.example",
            "ftp://bit.example",
            "https://",
            "",
            "https:///x",
        ] {
            assert_eq!(server_host(url), None, "{url:?} names no host");
        }
    }

    #[test]
    fn a_password_only_needs_asking_about_off_this_machine() {
        assert!(password_crosses_the_network("http://bit.example"));
        assert!(password_crosses_the_network("HTTP://10.0.0.5:8090"));
        // Loopback in any of its spellings, and anything encrypted, needs no warning at all.
        for url in [
            "http://127.0.0.1:8090",
            "http://localhost:8090",
            "http://LOCALHOST.",
            "http://[::1]:8090",
            "http://127.5.5.5",
            "https://bit.example",
        ] {
            assert!(
                !password_crosses_the_network(url),
                "{url:?} was reported as unencrypted off the machine"
            );
        }
    }

    #[test]
    fn a_loopback_host_is_only_what_a_loopback_address_looks_like() {
        for host in [
            "localhost",
            "LOCALHOST",
            "a.localhost",
            "::1",
            "127.0.0.1",
            "127.255.255.254",
        ] {
            assert!(is_loopback(host), "{host:?} is this machine");
        }
        for host in [
            "bit.example",
            "10.0.0.1",
            "128.0.0.1",
            "::2",
            "localhost.localdomain",
            "127",
            "127.0.0.1.5",
        ] {
            assert!(!is_loopback(host), "{host:?} is not this machine");
        }
    }

    #[test]
    fn the_rows_follow_the_target_and_the_login_method() {
        let mut form = ConnectForm::new(None);
        let rows = form.shown();
        assert!(rows.contains(&Field::Url), "a server asked for no address");
        // No login asks for nothing else.
        form.login = LoginMethod::None;
        assert_eq!(form.shown(), vec![Field::Target, Field::Url, Field::Login]);

        form.login = LoginMethod::Password;
        assert_eq!(
            form.shown(),
            vec![
                Field::Target,
                Field::Url,
                Field::Login,
                Field::User,
                Field::Password,
                Field::Remember
            ]
        );

        // A gateway private key asks for its endpoint and client id, and for a key file only when ssh-agent
        // is not the thing signing.
        form.login = LoginMethod::GatewayKey;
        form.agent = false;
        assert_eq!(
            form.shown(),
            vec![
                Field::Target,
                Field::Url,
                Field::Login,
                Field::TokenUrl,
                Field::ClientId,
                Field::Agent,
                Field::KeyPath
            ]
        );
        form.agent = true;
        assert!(
            !form.shown().contains(&Field::KeyPath),
            "ssh-agent was asked for a key file as well"
        );

        // The local engine asks for nothing but the choice itself.
        let mut local = ConnectForm::new(None);
        local.server = false;
        assert_eq!(local.shown(), vec![Field::Target]);
    }

    #[test]
    fn the_method_row_walks_every_method_and_wraps() {
        let mut seen = Vec::new();
        let mut method = LoginMethod::None;
        for _ in 0..METHODS.len() {
            seen.push(method);
            method = next_method(method, true);
        }
        assert_eq!(
            seen,
            vec![
                LoginMethod::None,
                LoginMethod::Password,
                LoginMethod::KeyFile,
                LoginMethod::Agent,
                LoginMethod::GatewayKey,
                LoginMethod::GatewayBasic
            ],
            "the cycle does not go through the methods in order"
        );
        assert_eq!(
            next_method(LoginMethod::None, false),
            LoginMethod::GatewayBasic
        );
    }

    #[test]
    fn the_answer_names_no_key_file_when_ssh_agent_signs() {
        // Which identity a gateway assertion is signed with is ssh-agent's answer when the agent is doing
        // the signing, so the target carries no file at all: `None` is what says so to a backend, which is
        // the same rule `fbtt_core::remote::device` reads a target by.
        let mut form = ConnectForm::new(None);
        form.url = Input::new("https://bit.example".to_string());
        form.login = LoginMethod::GatewayKey;
        form.agent = true;
        form.token_url = Input::new("https://auth.example/token".to_string());
        form.client_id = Input::new("featherbittorrent".to_string());
        form.key_path = Input::new("~/.ssh/id_ed25519".to_string());
        assert_eq!(
            form.request(None).target,
            ConnectTarget::Remote(RemoteTarget {
                url: "https://bit.example".to_string(),
                login: LoginMethod::GatewayKey,
                user: None,
                key_path: None,
                token_url: Some("https://auth.example/token".to_string()),
                client_id: Some("featherbittorrent".to_string()),
            }),
            "ssh-agent was given a key file to sign with"
        );

        // The same method without the agent names the file, and `~` is expanded on the way out so that a
        // backend never sees a tilde it would have to know about.
        let mut form = ConnectForm::new(None);
        form.url = Input::new("https://bit.example".to_string());
        form.login = LoginMethod::GatewayKey;
        form.agent = false;
        form.token_url = Input::new("https://auth.example/token".to_string());
        form.client_id = Input::new("featherbittorrent".to_string());
        form.key_path = Input::new("~/.ssh/id_ed25519".to_string());
        assert_eq!(
            form.request(None).target,
            ConnectTarget::Remote(RemoteTarget {
                url: "https://bit.example".to_string(),
                login: LoginMethod::GatewayKey,
                user: None,
                key_path: Some(expand_tilde("~/.ssh/id_ed25519")),
                token_url: Some("https://auth.example/token".to_string()),
                client_id: Some("featherbittorrent".to_string()),
            }),
            "a gateway key file was sent as it was typed"
        );

        // An ssh-agent login for the server itself keeps its path: among several identities that is how the
        // right one is found.
        let mut form = ConnectForm::new(None);
        form.url = Input::new("https://bit.example".to_string());
        form.login = LoginMethod::Agent;
        form.key_path = Input::new("~/.ssh/id_ed25519".to_string());
        assert_eq!(
            form.request(None).target,
            ConnectTarget::Remote(RemoteTarget {
                key_path: Some(expand_tilde("~/.ssh/id_ed25519")),
                ..form_target("https://bit.example", LoginMethod::Agent)
            }),
            "the agent was left without the identity it was pointed at"
        );

        // A method that signs in with nothing takes no secret, and `remember` means nothing to send when
        // there is no password to keep.
        let mut form = ConnectForm::new(None);
        form.url = Input::new("https://bit.example".to_string());
        form.login = LoginMethod::None;
        form.remember = true;
        assert_eq!(form.request(None).secret, SecretInput::default());
    }

    /// The target one test expects back from the connect screen: `url` signed in to by `login`, with no
    /// user name and no gateway pieces, which is every method but a gateway's.
    fn form_target(url: &str, login: LoginMethod) -> RemoteTarget {
        RemoteTarget {
            url: url.to_string(),
            login,
            ..RemoteTarget::default()
        }
    }

    #[test]
    fn a_key_file_says_whether_it_needs_a_passphrase() {
        // A file that is not there answers with the operating system's own words, which is what the dialog
        // has to show rather than a guess of ours.
        let missing = std::fs::metadata("/definitely/not/here/id_ed25519")
            .expect_err("the test path should not exist")
            .to_string();
        assert_eq!(
            device_key_needs_a_passphrase(Path::new("/definitely/not/here/id_ed25519"))
                .expect_err("a path that is not there should not answer"),
            missing
        );
    }
}
