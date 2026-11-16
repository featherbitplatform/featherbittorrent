//! The dialogs over the main view: what each one answers, and what a person sees while it is up.
//!
//! These go through `Model::handle_key` rather than calling a dialog directly, because that is how
//! the binary uses them: a press in, actions out, and a screen to read afterwards.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fbtt_core::{
    AddSource, Command, Detail, DetailTab, DeviceCommand, FilePriority, LiveLimits, Notice,
    RowState, Snapshot,
};
use fbtt_tui::keys::KEYS;
use fbtt_tui::{Action, AddForm, Modal, Model};
use ratatui::style::Color;

mod support;
use support::*;

/// A directory under the system temp dir, removed when the test that made it is done. The crate
/// takes no `tempfile` dependency, so this is where a dialog test gets a real file to read.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "featherbittorrent-test-{tag}-{since_epoch}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Scratch(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Types a whole string one key at a time, the way a person types or pastes it into a field.
fn type_text(model: &mut Model, snap: &Snapshot, text: &str) {
    for c in text.chars() {
        model.handle_key(letter(c), snap);
    }
}

/// Presses one key `times` over, which is how a test empties a field whose length it knows.
fn repeat(model: &mut Model, snap: &Snapshot, code: KeyCode, times: usize) {
    for _ in 0..times {
        model.handle_key(key(code), snap);
    }
}

/// The add dialog that is open, read-only.
fn add_form(model: &Model) -> &AddForm {
    let Some(Modal::Add(form)) = &model.modal else {
        panic!("the add dialog is not open: {:?}", model.modal);
    };
    form
}

/// The snapshot the backend would have published after it did what `priority` says.
fn applied(snap: &Snapshot, file: usize, priority: FilePriority) -> Snapshot {
    let mut next = snap.clone();
    if let Some((_, Detail::Files(files))) = &mut next.detail {
        for row in files.iter_mut() {
            if row.index == file {
                row.priority = priority;
            }
        }
    }
    next
}

/// A magnet link that parses, used by the tests that follow an add through to its answer.
const MAGNET_LINK: &str = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=demo";

/// Opens the add dialog, types `text` into it and presses Enter, which is what every test
/// that follows an add starts from.
fn submit(snap: &Snapshot, text: &str) -> (Model, Vec<Action>) {
    let mut model = model_of(snap);
    model.handle_key(letter('a'), snap);
    type_text(&mut model, snap, text);
    let actions = model.handle_key(key(KeyCode::Enter), snap);
    (model, actions)
}

#[test]
fn add_magnet_sends_command() {
    let snap = library();
    let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=demo";

    let mut model = model_of(&snap);
    model.handle_key(letter('a'), &snap);
    type_text(&mut model, &snap, magnet);

    let form = add_form(&model);
    assert_eq!(
        form.source.text, magnet,
        "typing did not reach the source field"
    );
    assert_eq!(
        form.dir.text, "/downloads",
        "the folder field was not prefilled with the default"
    );

    let actions = model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Add {
            source: AddSource::Magnet(magnet.to_string()),
            dir: None,
        })],
        "the magnet was not sent as it was typed"
    );
    // An add is not finished when Enter was pressed. The dialog stays up for the backend's
    // answer, and says what it is waiting for; `duplicate_error_shows_in_dialog` and
    // `success_closes_dialog` follow that answer.
    assert!(
        matches!(model.modal, Some(Modal::Add(_))),
        "the dialog came down before its answer"
    );
    assert!(
        Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("Adding..."),
        "a dialog waiting for its add said nothing about it"
    );

    // A folder the person changed is sent; the default one is not, because nothing is moving.
    let mut model = model_of(&snap);
    model.handle_key(letter('a'), &snap);
    type_text(&mut model, &snap, magnet);
    model.handle_key(key(KeyCode::Tab), &snap);
    repeat(&mut model, &snap, KeyCode::Backspace, "/downloads".len());
    type_text(&mut model, &snap, "/tmp/elsewhere");

    let actions = model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Add {
            source: AddSource::Magnet(magnet.to_string()),
            dir: Some(PathBuf::from("/tmp/elsewhere")),
        })],
        "an edited folder was not sent"
    );

    // Esc closes without adding anything.
    let mut model = model_of(&snap);
    model.handle_key(letter('a'), &snap);
    type_text(&mut model, &snap, magnet);
    assert!(model.handle_key(key(KeyCode::Esc), &snap).is_empty());
    assert!(model.modal.is_none(), "escape left the dialog up");
}

#[test]
fn add_dialog_shows_error() {
    let snap = library();
    let path = "/does/not/exist/x.torrent";
    // The operating system's own words for this path, which is what the dialog has to show.
    let os_message = std::fs::metadata(path)
        .expect_err("the test path should not exist")
        .to_string();

    let mut model = model_of(&snap);
    model.handle_key(letter('a'), &snap);
    type_text(&mut model, &snap, path);

    let actions = model.handle_key(key(KeyCode::Enter), &snap);
    assert!(actions.is_empty(), "a file that is not there was sent");
    assert!(
        matches!(model.modal, Some(Modal::Add(_))),
        "a failed read closed the dialog"
    );

    let form = add_form(&model);
    assert_eq!(form.error.as_deref(), Some(os_message.as_str()));

    // It is on the screen too, in the error colour, so the person can read it and correct the path.
    let screen = Screen::new(&model, &snap, 100, 30);
    let (x, y) = screen
        .find(&os_message)
        .expect("the reason for failing was not drawn");
    assert_eq!(screen.fg(x, y), Color::Rgb(0xed, 0x4a, 0x49));

    // Correcting the path clears it: the next try has to answer for itself.
    repeat(&mut model, &snap, KeyCode::Backspace, path.chars().count());
    assert_eq!(add_form(&model).error, None);
}

#[test]
fn add_file_over_cap_is_refused() {
    let scratch = Scratch::new("add-cap");
    let big = scratch.path().join("huge.torrent");
    let file = std::fs::File::create(&big).expect("a scratch file");
    // One byte past the cap, set rather than written: the check has to refuse this before the
    // bytes are read in.
    file.set_len(16 * 1024 * 1024 + 1)
        .expect("a file just over the cap");
    drop(file);

    let small = scratch.path().join("tiny.torrent");
    std::fs::write(&small, b"d5:hello").expect("a small torrent file");

    let snap = library();
    let mut model = model_of(&snap);
    model.handle_key(letter('a'), &snap);

    let big_text = big.to_string_lossy().to_string();
    type_text(&mut model, &snap, &big_text);
    assert!(
        model.handle_key(key(KeyCode::Enter), &snap).is_empty(),
        "an over-large file was sent to the backend"
    );
    assert_eq!(
        add_form(&model).error.as_deref(),
        Some("File is larger than 16 MiB"),
        "the cap did not say what it refused"
    );

    // A file under the cap is read here and goes as its bytes, which is what a local add needs:
    // the engine parses them, not this screen.
    repeat(
        &mut model,
        &snap,
        KeyCode::Backspace,
        big_text.chars().count(),
    );
    type_text(&mut model, &snap, &small.to_string_lossy());
    let actions = model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Add {
            source: AddSource::File(b"d5:hello".to_vec()),
            dir: None,
        })],
        "a small torrent file was not read"
    );
}

#[test]
#[cfg(unix)]
fn add_dialog_expands_a_leading_tilde() {
    let home = std::env::var("HOME").expect("a home directory on a unix host");
    let name = "featherbittorrent-no-such-torrent.torrent";

    let snap = library();
    let mut model = model_of(&snap);
    model.handle_key(letter('a'), &snap);
    type_text(&mut model, &snap, &format!("~/{name}"));
    assert!(
        model.handle_key(key(KeyCode::Enter), &snap).is_empty(),
        "a file that is not there was sent"
    );

    // The path that was looked for is the one under the home directory, not a directory called `~`.
    let expected = std::fs::metadata(PathBuf::from(home).join(name))
        .expect_err("the scratch name should not exist")
        .to_string();
    assert_eq!(add_form(&model).error.as_deref(), Some(expected.as_str()));
}

#[test]
fn malformed_magnet_stays_in_dialog() {
    let snap = library();
    // A `btih` that is neither 40 hex digits nor 32 base32 characters: the engine's own parser
    // refuses it, and so does this screen before anything is sent.
    let broken = "magnet:?xt=urn:btih:notavalidhash";

    let (mut model, actions) = submit(&snap, broken);
    assert!(
        actions.is_empty(),
        "a link that is not a magnet was sent to the backend"
    );
    let form = add_form(&model);
    assert_eq!(form.source.text, broken, "the typed link was not kept");
    let error = form.error.clone().expect("a refused link gave no reason");
    assert!(
        !error.is_empty(),
        "the reason for refusing the link was blank"
    );

    // It is on the screen too, in the error colour, where the person can read it and fix it.
    let screen = Screen::new(&model, &snap, 100, 30);
    let (x, y) = screen
        .find(&error)
        .expect("the reason for refusing the link was not drawn");
    assert_eq!(screen.fg(x, y), Color::Rgb(0xed, 0x4a, 0x49));

    // Correcting the hash and pressing Enter again sends it, and the dialog waits for its answer.
    repeat(
        &mut model,
        &snap,
        KeyCode::Backspace,
        broken.chars().count(),
    );
    type_text(&mut model, &snap, MAGNET_LINK);
    let actions = model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Add {
            source: AddSource::Magnet(MAGNET_LINK.to_string()),
            dir: None,
        })],
        "the corrected link was not sent"
    );
    assert_eq!(
        add_form(&model).error,
        None,
        "the old reason stayed on screen"
    );
}

#[test]
fn uppercase_magnet_is_a_magnet() {
    let snap = library();
    // Links arrive from browsers and shell history spelled every which way. The scheme is read
    // ignoring case, so this goes to the backend as a magnet instead of being looked for on disk
    // as a file named `MAGNET:?...`.
    let upper = "MAGNET:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=demo";

    let (model, actions) = submit(&snap, upper);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Add {
            source: AddSource::Magnet(upper.to_string()),
            dir: None,
        })],
        "an upper-case scheme was not sent as the magnet it is"
    );
    let form = add_form(&model);
    assert_eq!(
        form.error, None,
        "a magnet was refused as if it were a path"
    );
    assert!(
        Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("Adding..."),
        "the upper-case link did not go as an add that is waiting for its answer"
    );
}

#[test]
fn unrelated_error_does_not_enter_add_dialog() {
    let snap = library();
    let at = Instant::now();
    let elsewhere = "the session state could not be written";

    // The dialog is open with nothing sent in it: a line about something else in the app has to
    // read as itself, on the footer, not as a refusal of what is being typed.
    let mut model = model_of(&snap);
    model.handle_key(letter('a'), &snap);
    model.push_notice(Notice::Error(elsewhere.to_string()), at);
    assert_eq!(
        add_form(&model).error,
        None,
        "an error that was not about this dialog went inside it"
    );
    assert_eq!(model.notice(), Some(&Notice::Error(elsewhere.to_string())));

    // Once a dialog has had its own answer, the next error is somebody else's news too.
    let (mut model, _) = submit(&snap, MAGNET_LINK);
    model.push_notice(
        Notice::Error("torrent already in this library".to_string()),
        at,
    );
    assert_eq!(
        add_form(&model).error.as_deref(),
        Some("torrent already in this library"),
        "the answer to this dialog did not reach it"
    );
    model.push_notice(
        Notice::Error(elsewhere.to_string()),
        at + Duration::from_secs(1),
    );
    assert_eq!(
        add_form(&model).error.as_deref(),
        Some("torrent already in this library"),
        "an unrelated error overwrote what this dialog was told"
    );
    assert_eq!(model.notice(), Some(&Notice::Error(elsewhere.to_string())));
}

#[test]
fn duplicate_error_shows_in_dialog() {
    let snap = library();
    let at = Instant::now();

    let (mut model, actions) = submit(&snap, MAGNET_LINK);
    assert_eq!(actions.len(), 1, "the add was not sent");
    assert!(
        matches!(model.modal, Some(Modal::Add(_))),
        "the dialog came down before its answer"
    );

    model.push_notice(
        Notice::Error("torrent already in this library".to_string()),
        at,
    );
    let form = add_form(&model);
    assert_eq!(
        form.error.as_deref(),
        Some("torrent already in this library"),
        "the refusal did not reach the dialog it belongs to"
    );
    assert!(model.notice().is_none(), "the same line was shown twice");

    // On the screen, inside the box, in the error colour.
    let screen = Screen::new(&model, &snap, 100, 30);
    let (x, y) = screen
        .find("torrent already in this library")
        .expect("the refusal was not drawn");
    assert_eq!(screen.fg(x, y), Color::Rgb(0xed, 0x4a, 0x49));

    // Esc still closes it, whatever it is waiting for.
    assert!(model.handle_key(key(KeyCode::Esc), &snap).is_empty());
    assert!(model.modal.is_none(), "escape left the dialog up");
}

#[test]
fn success_closes_dialog() {
    let snap = library();
    let at = Instant::now();

    let (mut model, actions) = submit(&snap, MAGNET_LINK);
    assert_eq!(actions.len(), 1, "the add was not sent");
    assert!(
        matches!(model.modal, Some(Modal::Add(_))),
        "the dialog came down before its answer"
    );

    model.push_notice(Notice::Info("Added demo".to_string()), at);
    assert!(
        model.modal.is_none(),
        "the dialog stayed open after the add worked"
    );
    assert_eq!(
        model.notice(),
        Some(&Notice::Info("Added demo".to_string())),
        "what worked did not reach the footer"
    );
}

#[test]
fn delete_keep_and_delete() {
    let snap = library();
    let buck = snap.torrents[0].id.clone();

    let mut model = model_of(&snap);
    model.handle_key(letter('d'), &snap);
    assert!(
        matches!(model.modal, Some(Modal::Delete { .. })),
        "d did not ask about the selected torrent"
    );

    // The question says which torrent and what each key means, in the words it is written in.
    let body = Screen::new(&model, &snap, 100, 30).text();
    assert!(
        body.contains("Remove \"Big.Buck.Bunny.4K\"?  k keep files   D delete files   Esc cancel"),
        "the question is not the designed copy:\n{body}"
    );

    // While it is up, `j` does not move the table behind it: the dialog has the keyboard.
    assert!(model.handle_key(letter('j'), &snap).is_empty());
    assert_eq!(model.watch().map(|(id, _)| id), Some(buck.clone()));

    let actions = model.handle_key(letter('k'), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Remove {
            id: buck.clone(),
            delete_files: false
        })],
        "k did not keep the files"
    );
    assert!(model.modal.is_none());

    let mut model = model_of(&snap);
    model.handle_key(letter('d'), &snap);
    let actions = model.handle_key(letter('D'), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Remove {
            id: buck.clone(),
            delete_files: true
        })],
        "D did not ask for the files to be deleted"
    );

    // Esc cancels: nothing goes to the backend, and the torrent is still listed.
    let mut model = model_of(&snap);
    model.handle_key(letter('d'), &snap);
    assert!(model.handle_key(key(KeyCode::Esc), &snap).is_empty());
    assert!(model.modal.is_none());

    // Nothing selected means no name to ask about, so `d` opens nothing.
    let empty = Snapshot {
        torrents: Vec::new(),
        ..library()
    };
    let mut model = model_of(&empty);
    model.handle_key(letter('d'), &empty);
    assert!(model.modal.is_none(), "d asked a question about nothing");
}

#[test]
fn settings_rejects_text() {
    let snap = library();
    let mut model = model_of(&snap);
    model.handle_key(letter('S'), &snap);

    // The field is prefilled with `0`, so this makes it not a number without clearing anything.
    type_text(&mut model, &snap, "abc");
    let actions = model.handle_key(key(KeyCode::Enter), &snap);
    assert!(actions.is_empty(), "a field of text was sent as a limit");
    assert!(
        matches!(model.modal, Some(Modal::Settings(_))),
        "a refused field closed the dialog"
    );

    let screen = Screen::new(&model, &snap, 100, 30);
    let (x, y) = screen.find("0abc").expect("the field was not drawn");
    assert_eq!(
        screen.fg(x, y),
        Color::Rgb(0xed, 0x4a, 0x49),
        "the refused field was not marked red"
    );

    // Tab moves the cursor to the next field, which is where typing goes after it.
    model.handle_key(key(KeyCode::Tab), &snap);
    type_text(&mut model, &snap, "x");
    let Modal::Settings(form) = model.modal.as_ref().expect("the dialog closed") else {
        panic!("a different dialog is open: {:?}", model.modal);
    };
    assert_eq!(form.fields[1].text, "0x", "Tab did not move the cursor");
    assert_eq!(form.fields[0].text, "0abc");
}

#[test]
fn settings_sends_limits() {
    let snap = library(); // every limit unlimited: two `0`s and three empty fields
    let want = ["2048", "1024", "3", "", "5"];
    let prefilled = [1, 1, 0, 0, 0];

    let mut model = model_of(&snap);
    model.handle_key(letter('S'), &snap);
    for (index, text) in want.iter().enumerate() {
        if index > 0 {
            model.handle_key(key(KeyCode::Tab), &snap);
        }
        repeat(&mut model, &snap, KeyCode::Backspace, prefilled[index]);
        type_text(&mut model, &snap, text);
    }

    let actions = model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::SetLimits(LiveLimits {
            download_kib: 2048,
            upload_kib: 1024,
            active_downloads: Some(3),
            // An empty queue field is no limit, which is what the dialog says it means.
            active_seeds: None,
            active_total: Some(5),
        }))],
        "the five fields were not sent as one change"
    );
    assert!(
        model.modal.is_none(),
        "the dialog stayed open after applying"
    );

    // Esc cancels without sending anything, whatever has been typed.
    let mut model = model_of(&snap);
    model.handle_key(letter('S'), &snap);
    type_text(&mut model, &snap, "999");
    assert!(model.handle_key(key(KeyCode::Esc), &snap).is_empty());
    assert!(model.modal.is_none());
}

#[test]
fn filter_live_and_escape() {
    let snap = library();
    let mut model = model_of(&snap);
    model.handle_key(letter('/'), &snap);
    assert!(matches!(model.modal, Some(Modal::Filter)));

    type_text(&mut model, &snap, "deb");
    assert_eq!(model.filter, "deb", "typing did not change the filter");

    // The table, the pane and what the binary is watching all follow what has been typed, not
    // only what has been accepted: a hidden torrent is not something to keep describing.
    assert_eq!(visible_rows_len(&model, &snap), 1);
    assert_eq!(
        model.watch().map(|(id, _)| id),
        Some(snap.torrents[1].id.clone()),
        "the pane did not follow the filter"
    );

    let screen = Screen::new(&model, &snap, 100, 30);
    let last = screen.lines().last().expect("a footer line").clone();
    assert!(
        last.contains("filter deb"),
        "the filter was not typed where a person can see it: {last:?}"
    );
    assert!(
        !screen.text().contains("Big.Buck.Bunny.4K"),
        "a filtered-out torrent was still drawn"
    );

    // Enter keeps what was typed and puts the field away.
    model.handle_key(key(KeyCode::Enter), &snap);
    assert!(model.modal.is_none());
    assert_eq!(model.filter, "deb");

    // Esc throws it away, and the whole library is back.
    model.handle_key(letter('/'), &snap);
    type_text(&mut model, &snap, "buck");
    model.handle_key(key(KeyCode::Esc), &snap);
    assert!(model.modal.is_none());
    assert_eq!(model.filter, "", "escape left the filter behind");
    assert_eq!(visible_rows_len(&model, &snap), 2);

    // A key that means something else in the table is still only a letter here.
    let mut model = model_of(&snap);
    model.handle_key(letter('/'), &snap);
    model.handle_key(letter('q'), &snap);
    assert_eq!(model.filter, "q");
    assert!(!model.quit_requested(), "q quit while it was being typed");
}

/// How many rows the table is showing, counted rather than named so the test reads as a count.
fn visible_rows_len(model: &Model, snap: &Snapshot) -> usize {
    fbtt_tui::visible_rows(model, snap).len()
}

#[test]
fn help_lists_every_key() {
    let snap = library();
    let mut model = model_of(&snap);
    model.handle_key(letter('?'), &snap);
    assert!(matches!(model.modal, Some(Modal::Help)));

    // Every row of the key table is on the screen: in a tall terminal, and in a short one, where
    // the list is drawn in two columns so that nothing has to be scrolled for.
    for (width, height) in [(100u16, 40u16), (80, 24)] {
        let lines = Screen::new(&model, &snap, width, height).lines();
        for (name, action) in KEYS {
            let shown = lines.iter().any(|line| match line.find(action) {
                // The key is drawn before its action on the same line, in either shape.
                Some(at) => line[..at].contains(name),
                None => false,
            });
            assert!(
                shown,
                "the row for {name:?} is missing at {width}x{height}:\n{}",
                lines.join("\n")
            );
        }
    }

    // Reading the keys does not move the table, and Esc puts the help away.
    assert!(model.handle_key(letter('j'), &snap).is_empty());
    assert_eq!(
        model.watch().map(|(id, _)| id),
        Some(snap.torrents[0].id.clone())
    );
    model.handle_key(key(KeyCode::Esc), &snap);
    assert!(model.modal.is_none(), "escape left the help up");
}

#[test]
fn file_priority_keys() {
    let snap = library();
    let buck = snap.torrents[0].id.clone();
    let mut model = model_of(&snap);
    assert_eq!(model.watch(), Some((buck.clone(), DetailTab::Files)));

    // Enter on the Files tab puts the cursor in the list.
    model.handle_key(key(KeyCode::Enter), &snap);
    assert!(model.files_focused, "Enter did not focus the file list");

    // From here `j` means a file: the table and the pane keep pointing at the same torrent.
    assert!(model.handle_key(letter('j'), &snap).is_empty());
    assert_eq!(model.watch().map(|(id, _)| id), Some(buck.clone()));
    assert_eq!(
        model.file_cursor, 1,
        "the cursor did not move down the list"
    );

    // `+` asks for the file under the cursor to come first.
    let actions = model.handle_key(letter('+'), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::SetFilePriority {
            id: buck.clone(),
            file: 1,
            priority: FilePriority::High,
        })],
        "+ did not raise the focused file"
    );

    // The word only changes once the backend has done it, and from there `-` walks the ladder
    // back down: high, normal, low.
    let high = applied(&snap, 1, FilePriority::High);
    let actions = model.handle_key(letter('-'), &high);
    assert_eq!(
        actions,
        vec![Action::Command(Command::SetFilePriority {
            id: buck.clone(),
            file: 1,
            priority: FilePriority::Normal,
        })],
        "- did not lower a high file one step"
    );
    let normal = applied(&snap, 1, FilePriority::Normal);
    let actions = model.handle_key(letter('-'), &normal);
    assert_eq!(
        actions,
        vec![Action::Command(Command::SetFilePriority {
            id: buck.clone(),
            file: 1,
            priority: FilePriority::Low,
        })],
        "- did not lower a normal file to low"
    );

    // At the bottom of the ladder there is nothing to send, so holding the key does nothing.
    let skip = applied(&snap, 1, FilePriority::Skip);
    assert!(model.handle_key(letter('-'), &skip).is_empty());

    // The focused row is lifted onto the surface colour, and the footer says what these keys do.
    let screen = Screen::new(&model, &snap, 100, 30);
    let (x, y) = screen.find("bbb.srt").expect("the file list was not drawn");
    assert_eq!(
        screen.bg(x, y),
        Color::Rgb(0x15, 0x15, 0x18),
        "the focused file was not lifted"
    );
    let body = screen.text();
    assert!(
        body.contains("Esc leave the file list"),
        "the footer did not say how to leave the list:\n{body}"
    );

    // Esc leaves, and `j` means a torrent again.
    model.handle_key(key(KeyCode::Esc), &snap);
    assert!(!model.files_focused);
    model.handle_key(letter('j'), &snap);
    assert_eq!(
        model.watch().map(|(id, _)| id),
        Some(snap.torrents[1].id.clone())
    );

    // A gap in the list is the engine's, not something this screen invents: the indices are the ones
    // the metadata carries, and a command has to name the one it came with. Here index 1 is a pad
    // file that is not listed at all, so the second row down holds index 2 and `+` on it says 2.
    let mut padded = snap.clone();
    if let Some((_, Detail::Files(files))) = &mut padded.detail {
        files[1].index = 2;
    }
    let mut model = model_of(&padded);
    model.handle_key(key(KeyCode::Enter), &padded);
    model.handle_key(letter('j'), &padded);
    assert_eq!(
        model.file_cursor, 1,
        "the cursor did not reach the second listed file"
    );
    assert_eq!(
        model.handle_key(letter('+'), &padded),
        vec![Action::Command(Command::SetFilePriority {
            id: buck.clone(),
            file: 2,
            priority: FilePriority::High,
        })],
        "the command named a position in the list instead of the engine's index"
    );
}

#[test]
fn file_list_focus_goes_with_the_torrent_it_was_in() {
    let snap = library(); // buck selected, its two files listed
    let mut model = model_of(&snap);
    model.handle_key(key(KeyCode::Enter), &snap);
    assert!(model.files_focused, "Enter did not focus the file list");
    model.handle_key(letter('j'), &snap);
    assert_eq!(model.file_cursor, 1);

    // A third torrent so that there is somewhere for `j` to go afterwards, and the selected one
    // taken out of the snapshot: what a delete the backend agreed to looks like from here.
    let mut next = snap.clone();
    let mut third = torrent("t3", "Third.Torrent", RowState::Seeding);
    third.queue_position = 2;
    next.torrents.push(third);
    next.torrents.remove(0);
    next.detail = None; // its files went with it

    model.sync_selection(&next);
    assert!(
        !model.files_focused,
        "the cursor stayed in a list that left with its torrent"
    );
    assert_eq!(
        model.file_cursor, 0,
        "the cursor was not put back at the top of the list"
    );
    // The selection took the row the gone torrent left behind, which is what it always does.
    assert_eq!(
        model.watch().map(|(id, _)| id),
        Some(snap.torrents[1].id.clone())
    );

    // `j` moves the table again rather than a cursor nobody can see.
    model.handle_key(letter('j'), &next);
    assert_eq!(
        model.watch().map(|(id, _)| id),
        Some(next.torrents[1].id.clone()),
        "the key went to a file list that is not there any more"
    );
}

#[test]
fn a_focused_file_list_keeps_the_priority_word() {
    // At this width an unfocused pane gives up the priority word to keep the bar readable. A
    // focused one cannot give it up: `+` and `-` change exactly that word.
    let snap = library();

    let model = model_of(&snap);
    let unfocused = Screen::new(&model, &snap, 34, 16).text();
    assert!(
        !unfocused.contains("normal"),
        "the pane kept a column it gives up at this width:\n{unfocused}"
    );

    let mut model = model_of(&snap);
    model.handle_key(key(KeyCode::Enter), &snap);
    let focused = Screen::new(&model, &snap, 34, 16).text();
    assert!(
        focused.contains("normal"),
        "the focused list dropped the word it exists to change:\n{focused}"
    );
}

#[test]
fn notice_expires_after_5s() {
    let snap = library();
    let mut model = model_of(&snap);
    let at = Instant::now();

    model.push_notice(Notice::Error("the torrent was refused".to_string()), at);
    let screen = Screen::new(&model, &snap, 100, 30);
    let (x, y) = screen
        .find("the torrent was refused")
        .expect("the footer did not show what the backend said");
    assert_eq!(
        screen.fg(x, y),
        Color::Rgb(0xed, 0x4a, 0x49),
        "an error is not red"
    );

    model.tick(at + Duration::from_secs(4));
    assert!(model.notice().is_some(), "the notice went away early");
    model.tick(at + Duration::from_secs(5));
    assert_eq!(
        model.notice(),
        None,
        "the notice outstayed its five seconds"
    );
    assert!(
        !Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("the torrent was refused"),
        "an expired notice was still on the screen"
    );

    // An error while the add dialog is open, with nothing in it waiting for an answer, is
    // somebody else's news: it stays on the footer rather than reading as a refused magnet.
    let mut model = model_of(&snap);
    model.handle_key(letter('a'), &snap);
    model.push_notice(
        Notice::Error("the session state could not be written".to_string()),
        at,
    );
    assert_eq!(
        add_form(&model).error,
        None,
        "an error that was not about the dialog went inside it"
    );
    assert!(
        model.notice().is_some(),
        "an unrelated error never reached the footer"
    );

    // Something that worked still goes on the footer even with the dialog up.
    model.push_notice(Notice::Info("added".to_string()), at);
    assert_eq!(model.notice(), Some(&Notice::Info("added".to_string())));
}

/// `Ctrl+O`, the one modified key that reaches a dialog: its whole job is to put a folder into the field
/// of the dialog it is pressed in.
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

#[test]
fn folder_picker_navigates_and_selects() {
    // The picker starts at the folders this backend may name. Every listing echoes that list, which is why
    // going back up does not have to ask anybody anything.
    let roots = folders(None, &[]);
    let mut model = model_of(&roots);
    model.handle_key(letter('a'), &roots);

    let actions = model.handle_key(ctrl('o'), &roots);
    assert_eq!(
        actions,
        vec![Action::Command(Command::ListDirs(None))],
        "the picker opened without asking the backend what it may name"
    );
    let text = Screen::new(&model, &roots, 100, 30).text();
    assert!(
        text.contains("/downloads") && text.contains("/home/alice"),
        "the places to start from were not listed: {text:?}"
    );
    assert!(
        text.contains("s choose"),
        "the picker did not say how to choose a folder: {text:?}"
    );

    // There is nothing above the list of places to start from, so Backspace does nothing rather than asking
    // for a parent that would only be refused.
    assert!(
        model.handle_key(key(KeyCode::Backspace), &roots).is_empty(),
        "going up from the top of the tree sent a folder anyway"
    );

    // Enter opens the highlighted one and asks what is inside it. The answer arrives as the next snapshot:
    // the picker draws that, not a copy it took for itself.
    let downloads = folders(Some("/downloads"), &["movies", "shows"]);
    assert_eq!(
        model.handle_key(key(KeyCode::Enter), &roots),
        vec![Action::Command(Command::ListDirs(Some(PathBuf::from(
            "/downloads"
        ))))],
        "opening a folder did not ask the backend for it"
    );

    // `j` and `k` move along the list, and nothing goes out until a folder is opened or chosen.
    assert!(
        model.handle_key(letter('j'), &downloads).is_empty(),
        "moving the cursor sent something to the backend"
    );
    let text = Screen::new(&model, &downloads, 100, 30).text();
    assert!(
        text.contains("movies") && text.contains("shows"),
        "the folders inside were not listed: {text:?}"
    );

    // Backspace from a root shows the places to start from instead of the reason that step was refused:
    // the answer to "where do I go back to" is that list, and it is already in hand.
    assert!(
        model
            .handle_key(key(KeyCode::Backspace), &downloads)
            .is_empty(),
        "stepping out of a root asked for a folder outside the allowed ones"
    );
    let text = Screen::new(&model, &downloads, 100, 30).text();
    assert!(
        text.contains("/home/alice") && !text.contains("movies"),
        "stepping out of a root did not show the places to start from: {text:?}"
    );

    // Down again, along the list, and `s` chooses the folder being listed - the one named above the list,
    // not one inside it. That goes into the field as text and the picker comes down.
    model.handle_key(key(KeyCode::Enter), &downloads);
    model.handle_key(letter('j'), &downloads);
    let shows = folders(Some("/downloads/shows"), &["season-one"]);
    assert_eq!(
        model.handle_key(key(KeyCode::Enter), &downloads),
        vec![Action::Command(Command::ListDirs(Some(PathBuf::from(
            "/downloads/shows"
        ))))],
        "the cursor moved but Enter opened the wrong row"
    );
    assert!(
        model.handle_key(letter('s'), &shows).is_empty(),
        "choosing a folder sent something to the backend: it is only text in a field"
    );
    assert_eq!(
        add_form(&model).dir.text,
        "/downloads/shows",
        "the chosen folder did not go into the destination field"
    );
    let text = Screen::new(&model, &shows, 100, 30).text();
    assert!(
        text.contains("Torrent or magnet") && !text.contains("season-one"),
        "the picker was still over the dialog after a folder was chosen: {text:?}"
    );

    // The same key that opened the picker closes it, which is what a person who pressed it by mistake
    // looks for; the fields underneath are theirs again and nothing was changed.
    model.handle_key(ctrl('o'), &shows);
    let text = Screen::new(&model, &shows, 100, 30).text();
    assert!(
        !text.contains("Torrent or magnet") && text.contains("s choose"),
        "the picker did not come over the dialog when it was opened again: {text:?}"
    );
    model.handle_key(key(KeyCode::Esc), &shows);
    let text = Screen::new(&model, &shows, 100, 30).text();
    assert!(
        text.contains("Torrent or magnet"),
        "Esc did not bring the fields back: {text:?}"
    );
}

#[test]
fn the_picker_takes_an_answer_named_differently_from_the_folder_it_asked_for() {
    // A backend that canonicalises paths answers `/downloads` with the folder that path actually is. That
    // has to read as the answer to the picker's own ask - waiting for the name it typed left it saying
    // `reading folders...` over an answer it had already got - and the name the backend gave is what goes
    // into the field, because that is the folder that exists.
    let roots = folders(None, &[]);
    let mut model = model_of(&roots);
    model.handle_key(letter('a'), &roots);
    model.handle_key(ctrl('o'), &roots);
    assert_eq!(
        model.handle_key(key(KeyCode::Enter), &roots),
        vec![Action::Command(Command::ListDirs(Some(PathBuf::from(
            "/downloads"
        ))))],
        "the picker did not ask for the folder it opened"
    );

    // The listing that arrives is not the one that was on screen when the ask went out, so it is the
    // answer, and the folder on screen is the one it names rather than the one that was typed.
    let canonical = folders(Some("/private/downloads"), &["shows"]);
    let text = Screen::new(&model, &canonical, 100, 30).text();
    assert!(
        text.contains("/private/downloads") && !text.contains("reading folders"),
        "a canonicalised answer was not taken as an answer: {text:?}"
    );

    // Choosing hands that spelling to the field.
    assert!(
        model.handle_key(letter('s'), &canonical).is_empty(),
        "choosing a folder sent something to the backend: it is only text in a field"
    );
    assert_eq!(
        add_form(&model).dir.text,
        "/private/downloads",
        "the folder that was asked for went into the field instead of the folder that exists"
    );
}

#[test]
fn devices_add_and_remove_send_commands() {
    // A device list belongs to a server, so the key that opens one is answered by the connection rather
    // than by the table: on a library running its own engine there is no list to show.
    let local = library();
    let mut model = model_of(&local);
    assert!(
        model.handle_key(letter('K'), &local).is_empty(),
        "K sent something to a library that has no server to ask"
    );
    assert!(
        model.modal.is_none(),
        "K opened a device list on a library that runs its own engine"
    );

    let snap = server_with_devices(&[
        (
            "laptop",
            "SHA256:7hV9c1XkP2r8mZ3wQaB4nYtLdKfJhS6uE0pRiOxNmvA",
        ),
        (
            "phone",
            "SHA256:1kT9pQ2sLdMn4bV8cXzR7eHjKgF3uYoA6WiN0CxYbdE",
        ),
    ]);
    let mut model = model_of(&snap);

    // Opening the list asks the server for it, because a list nobody asked for is an empty screen until
    // somebody presses a key inside it. The rows are what the server said, name and fingerprint together.
    assert_eq!(
        model.handle_key(letter('K'), &snap),
        vec![Action::Command(Command::Devices(DeviceCommand::List))],
        "the device list was opened without asking for it"
    );
    let text = Screen::new(&model, &snap, 100, 30).text();
    assert!(
        text.contains("laptop") && text.contains("SHA256:7hV9c1Xk"),
        "the list did not show what the server reported: {text:?}"
    );

    // An add needs both halves of a device before anything goes out, and says which.
    model.handle_key(letter('a'), &snap);
    assert!(
        model.handle_key(key(KeyCode::Enter), &snap).is_empty(),
        "an add with nothing typed into it went to the server"
    );
    let text = Screen::new(&model, &snap, 100, 30).text();
    assert!(
        text.contains("A device needs a name and its public key."),
        "the refused add did not say what was missing: {text:?}"
    );

    type_text(&mut model, &snap, "desk");
    model.handle_key(key(KeyCode::Tab), &snap);
    type_text(
        &mut model,
        &snap,
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA desk@laptop",
    );
    assert_eq!(
        model.handle_key(key(KeyCode::Enter), &snap),
        vec![Action::Command(Command::Devices(DeviceCommand::Add {
            name: "desk".to_string(),
            public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA desk@laptop".to_string(),
        }))],
        "the device that was typed did not go out as it was typed"
    );
    // It stays up for the answer, which arrives as the list changing in a snapshot rather than as this
    // dialog editing its own rows.
    assert!(
        matches!(model.modal, Some(Modal::Devices(_))),
        "the add came down before the server had answered it"
    );

    // `x` asks about the device under the cursor, by name, and only `y` confirms it.
    assert!(
        model.handle_key(letter('x'), &snap).is_empty(),
        "x removed a device before anybody said yes"
    );
    let text = Screen::new(&model, &snap, 100, 30).text();
    assert!(
        text.contains("Remove \"laptop\"?"),
        "the question did not name the device it was about: {text:?}"
    );
    assert!(
        model.handle_key(key(KeyCode::Esc), &snap).is_empty(),
        "canceling the question sent a removal anyway"
    );
    let text = Screen::new(&model, &snap, 100, 30).text();
    assert!(
        !text.contains("Remove \"laptop\"?"),
        "the question stayed up after it was answered no: {text:?}"
    );

    // Answering yes sends the removal, and the list is still the server's to redraw.
    model.handle_key(letter('x'), &snap);
    assert_eq!(
        model.handle_key(letter('y'), &snap),
        vec![Action::Command(Command::Devices(DeviceCommand::Remove(
            "laptop".to_string()
        )))],
        "confirming did not ask for the removal of the device that was named"
    );
    assert!(
        matches!(model.modal, Some(Modal::Devices(_))),
        "the dialog came down before its removal was answered"
    );

    // Esc closes it, and sends nothing: the list is already what the server last said.
    assert!(
        model.handle_key(key(KeyCode::Esc), &snap).is_empty(),
        "closing the device list sent something"
    );
    assert!(model.modal.is_none(), "the device list stayed up after Esc");
}
