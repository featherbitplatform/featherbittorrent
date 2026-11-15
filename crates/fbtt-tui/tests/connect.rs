//! The connect screen: which library runs this run's torrents, and how it signs in when that library is
//! somewhere else. Tested the way the binary uses it - keys in, an action out - because nothing here
//! connects.
//!
//! What Enter produces is a [`ConnectRequest`] for the binary to start a backend with, so these tests
//! check the answer and the screen that collected it. A server that is down, or an address with a typo in
//! it, is not this screen's to find out about: it reports itself after the screen has closed. The one
//! thing the screen does look at is a device key file, and only to ask it whether it needs a passphrase.

use crossterm::event::{KeyCode, KeyEvent};
use fbtt_core::Snapshot;
use fbtt_core::ui_state::{LoginMethod, RemoteTarget};
use fbtt_tui::{Action, ConnectRequest, ConnectTarget, Modal, Model, SecretInput};
use zeroize::Zeroizing;

mod support;
use support::*;

/// The address one test connects to and the other refuses to warn about: a server somewhere else, reached
/// over plain http from a machine that is not it.
const SERVER_URL: &str = "https://bit.example";

/// Types a whole string one key at a time, the way a person types or pastes it into a field.
fn type_text(model: &mut Model, snap: &Snapshot, text: &str) {
    for c in text.chars() {
        model.handle_key(letter(c), snap);
    }
}

/// Presses one key, which reads better in a screen whose keys are mostly single presses of letters.
fn press(model: &mut Model, snap: &Snapshot, key: KeyEvent) {
    model.handle_key(key, snap);
}

/// Walks the connect screen from the row it opens on to the password field: the address, the method that
/// signs in with a password, the user name, and the password itself. Returns nothing: what the rows were
/// filled with is in the actions the caller asks about next.
fn fill_password_login(model: &mut Model, snap: &Snapshot, url: &str, user: &str, password: &str) {
    press(model, snap, key(KeyCode::Tab)); // Connect to -> URL
    type_text(model, snap, url);
    press(model, snap, key(KeyCode::Tab)); // URL -> Login
    press(model, snap, key(KeyCode::Right)); // no login -> password
    press(model, snap, key(KeyCode::Tab)); // Login -> User
    type_text(model, snap, user);
    press(model, snap, key(KeyCode::Tab)); // User -> Password
    type_text(model, snap, password);
}

/// The one request this library would start a remote backend with, as the connect screen answered it for
/// `url` signed in to as `alice` with a password.
fn request(url: &str, password: &str) -> ConnectRequest {
    ConnectRequest {
        target: ConnectTarget::Remote(RemoteTarget {
            url: url.to_string(),
            login: LoginMethod::Password,
            user: Some("alice".to_string()),
            key_path: None,
            token_url: None,
            client_id: None,
        }),
        secret: SecretInput {
            password: Some(Zeroizing::new(password.to_string())),
            passphrase: None,
            remember: false,
        },
    }
}

#[test]
fn connect_screen_builds_request() {
    let snap = library();
    let mut model = model_of(&snap);

    // `c` opens the screen and sends nothing: it is a question, and a question does not talk to a backend.
    assert!(
        model.handle_key(letter('c'), &snap).is_empty(),
        "opening the connect screen sent something to the backend"
    );
    assert!(
        matches!(model.modal, Some(Modal::Connect(_))),
        "c did not open the connect screen: {:?}",
        model.modal
    );

    // It opens pointed at a server, so there is nothing to accept yet. Enter says so instead of connecting
    // to an address that was never typed.
    assert!(
        model.handle_key(key(KeyCode::Enter), &snap).is_empty(),
        "an empty address was accepted as a server to dial"
    );
    let screen = Screen::new(&model, &snap, 100, 30);
    assert!(
        screen
            .text()
            .contains("Use an http:// or https:// address."),
        "the screen refused the address without saying why: {:?}",
        screen.text()
    );
    assert!(
        matches!(model.modal, Some(Modal::Connect(_))),
        "the screen came down on a press it had refused"
    );

    fill_password_login(&mut model, &snap, SERVER_URL, "alice", "hunter2");

    // A password is drawn as the length of itself and nothing more: what a person looking over a shoulder
    // gets is a row of stars, and so does whatever prints the screen for a test.
    let screen = Screen::new(&model, &snap, 100, 30);
    let text = screen.text();
    assert!(
        text.contains("*******"),
        "the password field drew nothing where the password is: {text:?}"
    );
    assert!(
        !text.contains("hunter2"),
        "the password was drawn as itself"
    );

    // An https address needs no answer from anybody besides the person typing: Enter is the end of the
    // screen, and what comes out of it is one action naming the backend to run on.
    let actions = model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        actions,
        vec![Action::Connect(request(SERVER_URL, "hunter2"))]
    );
    assert!(
        model.modal.is_none(),
        "the screen stayed up after it had answered"
    );

    // The same screen answering "this library's own engine" needs nothing besides the choosing of it: no
    // address, no login, and no secret at all.
    let mut model = model_of(&snap);
    press(&mut model, &snap, letter('c'));
    press(&mut model, &snap, key(KeyCode::Left)); // Server -> Local engine
    assert_eq!(
        model.handle_key(key(KeyCode::Enter), &snap),
        vec![Action::Connect(ConnectRequest {
            target: ConnectTarget::Local,
            secret: SecretInput::default(),
        })],
        "choosing this library's own engine did not answer the screen"
    );

    // What was chosen is what the screen opens with next time, because changing one thing about a server
    // should not mean typing all of it again. A password is not part of that: it stays something a person
    // types when they mean to send it.
    assert_eq!(
        model_of(&snap).last_target(),
        None,
        "a run that has not connected anywhere still offered an address"
    );
    let mut model = model_of(&snap);
    press(&mut model, &snap, letter('c'));
    fill_password_login(&mut model, &snap, SERVER_URL, "alice", "hunter2");
    model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        model.last_target(),
        Some(&RemoteTarget {
            url: SERVER_URL.to_string(),
            login: LoginMethod::Password,
            user: Some("alice".to_string()),
            ..RemoteTarget::default()
        }),
        "the screen forgot the server it had just been told about"
    );
    press(&mut model, &snap, letter('c'));
    let screen = Screen::new(&model, &snap, 100, 30);
    assert!(
        screen.text().contains(SERVER_URL),
        "reopening the screen did not offer back the address that was just used: {:?}",
        screen.text()
    );
}

#[test]
fn http_password_warning_needs_second_enter() {
    let snap = library();
    let mut model = model_of(&snap);
    press(&mut model, &snap, letter('c'));
    fill_password_login(&mut model, &snap, "http://bit.example", "alice", "hunter2");

    // The first Enter stops rather than connecting: this password is about to cross a network this client
    // cannot encrypt, and that is worth one look before it goes.
    assert!(
        model.handle_key(key(KeyCode::Enter), &snap).is_empty(),
        "the press that warned also connected"
    );
    let screen = Screen::new(&model, &snap, 100, 30);
    assert!(
        screen
            .text()
            .contains("Password sent without encryption: use https through a reverse proxy."),
        "the screen did not say what is wrong with this address: {:?}",
        screen.text()
    );
    assert!(
        matches!(model.modal, Some(Modal::Connect(_))),
        "the warning closed the screen instead of waiting for an answer to it"
    );

    // The second Enter is that answer. Nothing else about the screen has changed, so what goes out is the
    // same request the first press would have sent.
    let actions = model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        actions,
        vec![Action::Connect(request("http://bit.example", "hunter2"))],
        "the second Enter did not send what the first one stopped"
    );
    assert!(
        model.modal.is_none(),
        "the screen stayed up after it answered"
    );

    // On a loopback address the same press goes straight through: nothing leaves the machine, so there is
    // nothing to warn about and nobody should be asked to read a line that does not apply.
    let mut model = model_of(&snap);
    press(&mut model, &snap, letter('c'));
    fill_password_login(
        &mut model,
        &snap,
        "http://127.0.0.1:8090",
        "alice",
        "hunter2",
    );
    assert_eq!(
        model.handle_key(key(KeyCode::Enter), &snap),
        vec![Action::Connect(request("http://127.0.0.1:8090", "hunter2"))],
        "a password to this machine was warned about as if it were crossing a network"
    );

    // And the warning is about this address: typing a different one earns it again, so a person who typed
    // their way from http to https is not asked to accept something they have already changed.
    let mut model = model_of(&snap);
    press(&mut model, &snap, letter('c'));
    fill_password_login(&mut model, &snap, "http://bit.example", "alice", "hunter2");
    press(&mut model, &snap, key(KeyCode::Enter)); // the warning
    press(&mut model, &snap, key(KeyCode::Tab)); // Password -> Remember
    press(&mut model, &snap, key(KeyCode::Tab)); // Remember -> Connect to, the rows wrapping
    press(&mut model, &snap, key(KeyCode::Tab)); // Connect to -> URL
    repeat(
        &mut model,
        &snap,
        KeyCode::Backspace,
        "http://bit.example".len(),
    );
    type_text(&mut model, &snap, "https://bit.example");
    assert_eq!(
        model.handle_key(key(KeyCode::Enter), &snap),
        vec![Action::Connect(request("https://bit.example", "hunter2"))],
        "an address made encrypted was still held at the warning it had outgrown"
    );
}

/// Presses one key `times` over, which is how a test empties a field whose length it knows.
fn repeat(model: &mut Model, snap: &Snapshot, code: KeyCode, times: usize) {
    for _ in 0..times {
        model.handle_key(key(code), snap);
    }
}
