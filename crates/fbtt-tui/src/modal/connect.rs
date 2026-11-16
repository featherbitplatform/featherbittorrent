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
use url::{Host, Url};
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

/// What the screen says about an address it cannot read as one this client can dial. It names the shape
/// that works rather than repeating the parser's complaint, because what has to happen next is typing.
const NOT_AN_ADDRESS: &str = "Enter a server address such as https://host:port";

/// What the screen says about `https://alice:secret@bit.example`. The fields beside this one are where a
/// user name and its password belong: an address that carries them would keep a password in the URL, in
/// what `Debug` prints of the last connection, and in whatever dials it.
const USER_IN_ADDRESS: &str = "Put the user name in its own field, not in the address";

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
/// The header a gateway standing in front of this server reads its key out of, and the key itself. Only the
/// name is written to `ui.yaml`; the value goes to the system keychain beside the passwords.
const GATEWAY_HEADER_LABEL: &str = "Gateway key header";
const GATEWAY_KEY_LABEL: &str = "Gateway key";
/// What the gateway rows say while the header row is empty, which is the one thing about them a label does
/// not answer: that empty means no front door at all, and what to type when there is one. The name suggested
/// is the one this project's own gateway example configures its key-auth plugin to read (see
/// `docs/reverse-proxy.md`).
const GATEWAY_HINT: &str =
    "Empty sends no header. A key-auth gateway that reads X-API-Key wants exactly that name.";

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
    /// The value of the gateway header this target names, for a server behind a gateway that checks one
    /// before it looks at any credential. It rides on every request beside this client's own proof, which is
    /// why it comes back here rather than in the target: `ui.yaml` keeps only the header's NAME (see
    /// [`fbtt_core::ui_state::RemoteTarget`]).
    pub gateway_key: Option<Zeroizing<String>>,
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
    /// Which header a gateway in front of this server reads its key out of, empty for a server reached
    /// directly. Only this name is remembered in `ui.yaml`.
    gateway_header: Input,
    /// The key that gateway checks, held only where it is typed.
    gateway_key: Masked,
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
    /// Which header a gateway in front of this server reads its key out of. Empty means there is no front
    /// door, which is every server reached directly.
    GatewayHeader,
    /// The key that gateway checks, typed into a masked row.
    GatewayKey,
}

impl ConnectForm {
    /// The screen as `c` opens it: pointed at a server, and prefilled with `last` when this library has
    /// been connected somewhere before (or is being offered the server running on its own library).
    ///
    /// Prefilled with a password target starts the cursor on the password row: that is the one thing this run
    /// could not get hold of by itself, and it is what has to be typed before Enter means anything here.
    pub fn new(last: Option<&RemoteTarget>) -> ConnectForm {
        let last_key_path = last.and_then(|target| target.key_path.as_ref());
        let mut form = ConnectForm {
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
            // The header name is what `ui.yaml` kept; the key itself was never in that file, so this row
            // starts empty whatever the last run signed in with (see `crate::secrets`'s gateway account).
            gateway_header: Input::new(
                last.and_then(|target| target.gateway_key_header.as_deref())
                    .unwrap_or_default(),
            ),
            gateway_key: Masked::new(),
            remember: false,
            focused: 0,
            warned: false,
            asking: None,
            passphrase: Masked::new(),
            error: None,
        };
        // A run that restarted onto a saved password server, whose keychain entry is gone, arrives here with
        // the address and the method filled in and nothing to sign in with. The password is the one piece
        // this screen cannot supply from what it was given (see [`ConnectForm::submit`]: what the keychain
        // holds is never retyped from it), so the cursor starts there rather than at the top of the form. A
        // target that signs with a key, or with nothing, has no such gap and keeps the first row.
        if form.signs_with_a_password() && form.password.is_empty() {
            form.focus_on(Field::Password);
        }
        form
    }

    /// Whether this login signs in with a user name and a password: the pair this screen asks for together,
    /// and refuses to send with either of them blank.
    fn signs_with_a_password(&self) -> bool {
        matches!(
            self.login,
            LoginMethod::Password | LoginMethod::GatewayBasic
        )
    }

    /// Whether this screen has anything the keychain could keep: a password, or the key for a gateway header.
    ///
    /// A device key is never kept there - its bytes stay in their file or in ssh-agent - so a key login with
    /// no front door offers nothing to remember and shows no row for it. The header name is what decides for a
    /// key login because that is the setting that says this server has a gateway key at all.
    fn offers_the_keychain(&self) -> bool {
        self.signs_with_a_password() || !self.gateway_header.text.trim().is_empty()
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
                // one, and closing the whole screen would be more than that mistake deserves. What was
                // typed into the prompt goes with it - the next prompt is about a different file, and a
                // secret sitting in a field nobody can see is a secret kept for no reason.
                if self.asking.take().is_some() {
                    self.passphrase.clear();
                    self.error = None;
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
                fields.extend([Field::User, Field::Password]);
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
        // The front door comes after the credential, and on every server target: what a gateway in front of
        // this server checks before it looks at any credential is not something to guess from a login method
        // - an open server sits behind a keyed gateway as easily as a passworded one does - and leaving both
        // rows empty is what says there is no gateway at all.
        fields.extend([Field::GatewayHeader, Field::GatewayKey]);
        // The keychain is then offered for whatever this screen could collect for it: a password, or the key
        // for a header (see [`ConnectForm::offers_the_keychain`]).
        if self.offers_the_keychain() {
            fields.push(Field::Remember);
        }
        // The passphrase prompt is last because it appears in answer to something else: the key file saying it
        // is encrypted, which is only known once everything above has been filled in.
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
            Field::GatewayHeader => type_into(&mut self.gateway_header, key),
            Field::GatewayKey => type_into_masked(&mut self.gateway_key, key),
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

        // Read the address with the parser this client's HTTP library uses. Deciding the encryption warning
        // from a spelling of our own is how `http://evil.example\@localhost/` came to be called loopback:
        // WHATWG ends the authority at that backslash, and reqwest dials `evil.example`.
        let parsed = match address(&self.url.text) {
            Ok(parsed) => parsed,
            Err(reason) => {
                self.error = Some(reason);
                return Answer::Stay;
            }
        };

        // Login text spelled into the address is refused rather than dialled, so that a password never ends
        // up in the URL, in `last_target`, or in whatever prints it.
        if !parsed.username().is_empty() || parsed.password().is_some() {
            self.error = Some(USER_IN_ADDRESS.to_string());
            return Answer::Stay;
        }

        // Each method is asked for its own pieces before any of them goes out: a target that cannot be
        // connected to is the screen's mistake to catch, not something to hand to a backend.
        let needs_user = self.signs_with_a_password();
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

        // A front door is named by its header, and the two halves go together: a name with no key is a
        // connection a gateway turns away before it ever sees our credential, and a key with no name is one
        // this client has nowhere to send. Which header a gateway was configured to read is that gateway's
        // setting, so nothing here checks the spelling beyond pairing it (see `fbtt_core::remote`'s transport,
        // which says once when a name is not a header name at all).
        let names_a_header = !self.gateway_header.text.trim().is_empty();
        if names_a_header && self.gateway_key.is_empty() {
            self.error = Some("A gateway key header needs the key to send under it.".to_string());
            return Answer::Stay;
        }
        if !names_a_header && !self.gateway_key.is_empty() {
            self.error = Some("Name the header this gateway key is sent under.".to_string());
            return Answer::Stay;
        }

        // A password that is about to cross an unencrypted hop gets asked about once, and only once per
        // address: a person who has read it and pressed Enter again has said they accept.
        if needs_user && !self.warned && password_crosses_the_network(&parsed) {
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
            // The header's name is the half that belongs in `ui.yaml`; the value it opens with comes back in
            // the secret below, because a file somebody cats is not where a key goes.
            gateway_key_header: non_empty(&self.gateway_header),
        };

        let keeps_a_secret = self.offers_the_keychain();
        ConnectRequest {
            target: ConnectTarget::Remote(target),
            secret: SecretInput {
                password: (password_method && !self.password.is_empty())
                    .then(|| self.password.take()),
                passphrase,
                gateway_key: (!self.gateway_key.is_empty()).then(|| self.gateway_key.take()),
                remember: keeps_a_secret && self.remember,
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

/// What was typed, read as an address: `Ok` holds it when this client could dial it. The parser is
/// reqwest's own - WHATWG rules, so a backslash inside an `http://` URL ends the authority exactly where a
/// slash does - which is the point: the screen's question about encryption has to be answered against the
/// host the connection will actually reach, not against a spelling of ours.
fn address(url: &str) -> Result<Url, String> {
    let parsed = Url::parse(url.trim()).map_err(|_| NOT_AN_ADDRESS.to_string())?;
    // Only the two schemes this client can dial. `ftp://bit.example` and `mailto:alice` parse into something
    // with a host and mean nothing here.
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(NOT_AN_ADDRESS.to_string());
    }
    // A scheme with nothing after it (`https://`) has no host to dial, which is what this line is about.
    // `https:///x` does have one: WHATWG skips the extra slash and reads `x` as the host, so a person who
    // typed that dials a host called `x`, and whether it exists is the connection's thing to report.
    // Terminals report what a person typed, and people type `HTTP://` as readily as `http://`; the parser
    // reads both, having lowercased the scheme.
    parsed.host().ok_or_else(|| NOT_AN_ADDRESS.to_string())?;
    Ok(parsed)
}

/// Whether the password typed into this address would cross a network it cannot encrypt: plain `http://` to
/// a host that is not this machine. Loopback needs no encryption because nothing leaves the machine, and
/// the screen says so about everything else rather than refusing it - plenty of servers run on a LAN behind
/// a proxy that has not been set up yet, and some people mean that.
fn password_crosses_the_network(address: &Url) -> bool {
    address.scheme() == "http" && !is_loopback(address)
}

/// Whether this address names this machine: `localhost` and its subdomains, anything the parser read as an
/// address in `127.0.0.0/8`, and the IPv6 loopback address. Names are not resolved here - a host that
/// reaches loopback by way of `/etc/hosts` is still somebody else's name on the wire, and a person who put
/// it in an `http://` address should still be told their password goes in the clear.
fn is_loopback(address: &Url) -> bool {
    match address.host() {
        Some(Host::Ipv4(at)) => at.is_loopback(),
        Some(Host::Ipv6(at)) => at.is_loopback(),
        // The port and any login text are already off this, and so is the case: a trailing dot is the same
        // host spelled absolutely, which is what a person types when they mean "this exact name and no
        // search domain".
        Some(Host::Domain(name)) => {
            let name = name.trim_end_matches('.');
            name == "localhost" || name.ends_with(".localhost")
        }
        None => false,
    }
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
    // The one row whose label cannot answer its own question: what to type there, and what leaving it empty
    // means. Said only while that row has the cursor, so a screen about a server reached directly does not
    // spend a line on a gateway nobody is using.
    if form.focused_field() == Field::GatewayHeader && form.gateway_header.text.trim().is_empty() {
        lines.push(advice(GATEWAY_HINT));
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
        Field::GatewayHeader => field(label(&row), &form.gateway_header, width, false),
        Field::GatewayKey => masked_field(label(&row), &form.gateway_key, width),
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
        Field::GatewayHeader => GATEWAY_HEADER_LABEL,
        Field::GatewayKey => GATEWAY_KEY_LABEL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host spelled as an address, so the loopback question gets asked the way it is asked in the form.
    fn host_address(host: &str) -> Url {
        let spelling = if host.contains(':') {
            format!("http://[{host}]")
        } else {
            format!("http://{host}")
        };
        Url::parse(&spelling).unwrap_or_else(|why| panic!("{spelling:?} is not an address: {why}"))
    }

    #[test]
    fn an_address_that_is_not_one_is_refused() {
        // No scheme at all, a scheme this client cannot dial, or an authority the parser will not accept.
        for url in [
            "bit.example",
            "127.0.0.1:8090",
            "ftp://bit.example",
            "mailto:alice@bit.example",
            "https://",
            // Digits and dots that are not an address: refused rather than dialled, because a person who
            // typed this meant one of the four numbers below.
            "http://127.0.0.1.5",
            "",
            "   ",
        ] {
            assert_eq!(
                address(url),
                Err(NOT_AN_ADDRESS.to_string()),
                "{url:?} is not an address"
            );
        }

        // Either scheme, in either case, with or without a port and a path. `https:///x` counts: WHATWG
        // skips the extra slash and reads `x` as the host, which is exactly what the connection dials, so
        // there is nothing for the screen to say about it.
        for url in [
            "https://bit.example",
            "HTTP://10.0.0.5:8090",
            "http://[::1]:8090/list",
            "  http://localhost:8090  ",
            "https:///x",
        ] {
            assert!(address(url).is_ok(), "{url:?} is an address");
        }
    }

    #[test]
    fn the_warning_follows_the_host_the_http_library_would_dial() {
        // The case this was rewritten for. WHATWG ends the authority at a backslash, so reqwest dials
        // `evil.example` and takes the password with it; splitting on `@` first read this as loopback and
        // let the password go without a word.
        assert!(password_crosses_the_network(
            &address("http://evil.example\\@localhost/").unwrap()
        ));

        // The same reading for a user name spelled into an address whose host is somebody else: the host
        // is `evil`, so the password leaves this machine whatever the user name looks like. (The form
        // refuses login text in the address before it gets here; what this line protects is the decision.)
        assert!(password_crosses_the_network(
            &address("http://localhost@evil").unwrap()
        ));

        // Loopback needs no warning, and neither does anything already encrypted.
        for url in [
            "http://[::1]:8090",
            "http://127.0.0.1:8090",
            "http://localhost:8090",
            "https://bit.example",
        ] {
            assert!(
                !password_crosses_the_network(&address(url).unwrap()),
                "{url:?} was reported as unencrypted off the machine"
            );
        }
    }

    #[test]
    fn a_password_only_needs_asking_about_off_this_machine() {
        assert!(password_crosses_the_network(
            &address("http://bit.example").unwrap()
        ));
        assert!(password_crosses_the_network(
            &address("HTTP://10.0.0.5:8090").unwrap()
        ));
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
                !password_crosses_the_network(&address(url).unwrap()),
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
            "0:0:0:0:0:0:0:1",
            "127.0.0.1",
            "127.255.255.254",
        ] {
            assert!(is_loopback(&host_address(host)), "{host:?} is this machine");
        }
        for host in [
            "bit.example",
            "10.0.0.1",
            "128.0.0.1",
            "::2",
            "localhost.localdomain",
            // The parser reads a one-part address as `0.0.0.127`, so the digits at the front are not what
            // decides this. (Five parts never get here: `http://127.0.0.1.5` is refused as an address.)
            "127",
        ] {
            assert!(
                !is_loopback(&host_address(host)),
                "{host:?} is not this machine"
            );
        }
    }

    #[test]
    fn the_rows_follow_the_target_and_the_login_method() {
        let mut form = ConnectForm::new(None);
        let rows = form.shown();
        assert!(rows.contains(&Field::Url), "a server asked for no address");
        // No login asks for nothing but the front door, which every server target gets and which most people
        // leave empty: two rows that say "no gateway" by being blank cost less than a screen that cannot ask
        // about one at all.
        form.login = LoginMethod::None;
        assert_eq!(
            form.shown(),
            vec![
                Field::Target,
                Field::Url,
                Field::Login,
                Field::GatewayHeader,
                Field::GatewayKey
            ]
        );

        // A password asks for its pair, and the keychain row comes last: it is about what to do with
        // everything above it.
        form.login = LoginMethod::Password;
        assert_eq!(
            form.shown(),
            vec![
                Field::Target,
                Field::Url,
                Field::Login,
                Field::User,
                Field::Password,
                Field::GatewayHeader,
                Field::GatewayKey,
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
                Field::KeyPath,
                Field::GatewayHeader,
                Field::GatewayKey
            ]
        );
        form.agent = true;
        assert!(
            !form.shown().contains(&Field::KeyPath),
            "ssh-agent was asked for a key file as well"
        );

        // A device key with no front door has nothing the keychain could hold - its bytes stay in their file -
        // so it gets no keychain row until there is a gateway key worth keeping.
        form.login = LoginMethod::KeyFile;
        assert!(
            !form.shown().contains(&Field::Remember),
            "a device key was offered the keychain for a key the keychain never holds"
        );
        form.gateway_header = Input::new("X-API-Key".to_string());
        assert!(
            form.shown().contains(&Field::Remember),
            "a gateway key was collected with no way to say whether to keep it"
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
                gateway_key_header: None,
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
                gateway_key_header: None,
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

    #[test]
    fn escaping_a_passphrase_prompt_lets_go_of_what_was_typed_into_it() {
        use crossterm::event::KeyModifiers;
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

        let mut form = ConnectForm::new(None);
        form.url = Input::new("https://bit.example".to_string());
        form.login = LoginMethod::KeyFile;
        // The prompt the screen puts up when the key file says it is encrypted. A test has no
        // passphrase-protected key to open, so the state goes in the way the screen would have put it.
        form.asking = Some(PathBuf::from("/home/alice/.ssh/id_ed25519"));
        form.focus_on(Field::Passphrase);
        type_into_masked(
            &mut form.passphrase,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );

        assert_eq!(form.on_key(esc), Answer::Stay, "the prompt was open");
        assert!(form.asking.is_none(), "and Esc put it away");
        assert!(
            form.passphrase.is_empty(),
            "a secret typed into a prompt that has been closed is kept for nobody"
        );

        // With no prompt up, Esc is still the caller's to answer as closing the screen.
        assert_eq!(form.on_key(esc), Answer::Close(Vec::new()));
    }

    /// A restart onto a saved password server whose keychain entry is gone reaches this screen with the
    /// address and method filled in and nothing to sign in with, so the cursor starts on the row that has to
    /// be answered before Enter can mean anything. Before this, it started at the top of the form and the
    /// person had to find the password row themselves - after a run that exited on the way to the screen,
    /// which is not a thing anybody expects to have to Tab through.
    #[test]
    fn a_password_target_opens_on_the_password_row() {
        use crossterm::event::KeyModifiers;

        let saved = RemoteTarget {
            url: "https://bit.example".to_string(),
            login: LoginMethod::Password,
            user: Some("alice".to_string()),
            ..RemoteTarget::default()
        };
        let mut form = ConnectForm::new(Some(&saved));
        assert_eq!(form.url.text, "https://bit.example", "the address was kept");
        assert_eq!(form.user.text, "alice", "and so was the user name");
        assert_eq!(
            form.focused_field(),
            Field::Password,
            "a saved password target with nothing typed starts on the piece that is missing"
        );

        // What is typed goes where the cursor is: one letter through the real press handler, and the answer
        // this run gets holds a password rather than nothing.
        assert_eq!(
            form.on_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)),
            Answer::Stay
        );
        let request = form.request(None);
        assert_eq!(
            request.secret.password.as_deref().map(|p| &**p),
            Some("h"),
            "the row the screen opened on is the row that took what was typed"
        );

        // A gateway password login is the same kind of credential, so it opens the same way.
        let gateway = RemoteTarget {
            url: "https://bit.example".to_string(),
            login: LoginMethod::GatewayBasic,
            user: Some("alice".to_string()),
            ..RemoteTarget::default()
        };
        assert_eq!(
            ConnectForm::new(Some(&gateway)).focused_field(),
            Field::Password,
            "a gateway password was left to hunt for its own password too"
        );

        // Nothing else arrives with a piece missing in this way: a key login, a server that needs no
        // credential, and a screen opened with nothing saved all start at the top.
        for login in [
            LoginMethod::None,
            LoginMethod::KeyFile,
            LoginMethod::Agent,
            LoginMethod::GatewayKey,
        ] {
            let target = RemoteTarget {
                url: "https://bit.example".to_string(),
                login,
                ..RemoteTarget::default()
            };
            assert_eq!(
                ConnectForm::new(Some(&target)).focused_field(),
                Field::Target,
                "{login:?} was sent to a row it does not sign with"
            );
        }
        assert_eq!(
            ConnectForm::new(None).focused_field(),
            Field::Target,
            "a screen with nothing saved has no missing piece to point at"
        );
    }
}
