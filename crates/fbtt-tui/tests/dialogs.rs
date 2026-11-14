//! The dialogs over the main view: what each one answers, and what a person sees while it is up.
//!
//! These go through `Model::handle_key` rather than calling a dialog directly, because that is how
//! the binary uses them: a press in, commands out, and a screen to read afterwards.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::KeyCode;
use fbtt_core::{
    AddSource, Command, Detail, DetailTab, FilePriority, LiveLimits, Notice, RowState, Snapshot,
};
use fbtt_tui::keys::KEYS;
use fbtt_tui::{AddForm, Modal, Model};
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

    let commands = model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        commands,
        vec![Command::Add {
            source: AddSource::Magnet(magnet.to_string()),
            dir: None,
        }],
        "the magnet was not sent as it was typed"
    );
    assert!(
        model.modal.is_none(),
        "the dialog stayed open once submitted"
    );

    // A folder the person changed is sent; the default one is not, because nothing is moving.
    let mut model = model_of(&snap);
    model.handle_key(letter('a'), &snap);
    type_text(&mut model, &snap, magnet);
    model.handle_key(key(KeyCode::Tab), &snap);
    repeat(&mut model, &snap, KeyCode::Backspace, "/downloads".len());
    type_text(&mut model, &snap, "/tmp/elsewhere");

    let commands = model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        commands,
        vec![Command::Add {
            source: AddSource::Magnet(magnet.to_string()),
            dir: Some(PathBuf::from("/tmp/elsewhere")),
        }],
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

    let commands = model.handle_key(key(KeyCode::Enter), &snap);
    assert!(commands.is_empty(), "a file that is not there was sent");
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
    let commands = model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        commands,
        vec![Command::Add {
            source: AddSource::File(b"d5:hello".to_vec()),
            dir: None,
        }],
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

    let commands = model.handle_key(letter('k'), &snap);
    assert_eq!(
        commands,
        vec![Command::Remove {
            id: buck.clone(),
            delete_files: false
        }],
        "k did not keep the files"
    );
    assert!(model.modal.is_none());

    let mut model = model_of(&snap);
    model.handle_key(letter('d'), &snap);
    let commands = model.handle_key(letter('D'), &snap);
    assert_eq!(
        commands,
        vec![Command::Remove {
            id: buck.clone(),
            delete_files: true
        }],
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
    let commands = model.handle_key(key(KeyCode::Enter), &snap);
    assert!(commands.is_empty(), "a field of text was sent as a limit");
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

    let commands = model.handle_key(key(KeyCode::Enter), &snap);
    assert_eq!(
        commands,
        vec![Command::SetLimits(LiveLimits {
            download_kib: 2048,
            upload_kib: 1024,
            active_downloads: Some(3),
            // An empty queue field is no limit, which is what the dialog says it means.
            active_seeds: None,
            active_total: Some(5),
        })],
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
    let commands = model.handle_key(letter('+'), &snap);
    assert_eq!(
        commands,
        vec![Command::SetFilePriority {
            id: buck.clone(),
            file: 1,
            priority: FilePriority::High,
        }],
        "+ did not raise the focused file"
    );

    // The word only changes once the backend has done it, and from there `-` walks the ladder
    // back down: high, normal, low.
    let high = applied(&snap, 1, FilePriority::High);
    let commands = model.handle_key(letter('-'), &high);
    assert_eq!(
        commands,
        vec![Command::SetFilePriority {
            id: buck.clone(),
            file: 1,
            priority: FilePriority::Normal,
        }],
        "- did not lower a high file one step"
    );
    let normal = applied(&snap, 1, FilePriority::Normal);
    let commands = model.handle_key(letter('-'), &normal);
    assert_eq!(
        commands,
        vec![Command::SetFilePriority {
            id: buck.clone(),
            file: 1,
            priority: FilePriority::Low,
        }],
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
        vec![Command::SetFilePriority {
            id: buck.clone(),
            file: 2,
            priority: FilePriority::High,
        }],
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

    // An error that answers the add dialog goes in the dialog rather than under it.
    let mut model = model_of(&snap);
    model.handle_key(letter('a'), &snap);
    model.push_notice(Notice::Error("duplicate torrent".to_string()), at);
    assert_eq!(add_form(&model).error.as_deref(), Some("duplicate torrent"));
    assert!(model.notice().is_none(), "the same error was shown twice");

    // Something that worked still goes on the footer even with the dialog up.
    model.push_notice(Notice::Info("added".to_string()), at);
    assert_eq!(model.notice(), Some(&Notice::Info("added".to_string())));
}
