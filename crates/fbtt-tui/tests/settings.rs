//! The full-screen settings view: keys in, commands out, and a buffer to read what a person sees.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fbtt_core::{
    Command, DownloadsChange, DownloadsView, LiveLimits, Notice, OnCompleteSet, OnCompleteView,
    SettingsChange, SettingsCommand, SettingsState, SettingsView, Snapshot,
};
use fbtt_tui::{Action, Model};

mod support;
use support::*;

/// A folder under the system temp dir: a path that is absolute on every platform CI runs on.
fn dir(name: &str) -> PathBuf {
    std::env::temp_dir().join("fbtt-a7-settings").join(name)
}

fn view(editable: bool) -> SettingsView {
    SettingsView {
        speed: LiveLimits {
            download_kib: 0,
            upload_kib: 0,
            active_downloads: None,
            active_seeds: None,
            active_total: None,
        },
        downloads: DownloadsView {
            default_dir: dir("downloads"),
            unfinished_dir: None,
            start_paused: false,
            watch_dir: None,
            on_complete: OnCompleteView {
                program: Some(dir("tools").join("done")),
                editable,
            },
            watch_problem: None,
            on_complete_problem: None,
        },
    }
}

fn loaded(editable: bool) -> Snapshot {
    let mut snap = library();
    snap.settings = SettingsState::Loaded(view(editable));
    snap
}

fn ctrl_s() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)
}

fn press(model: &mut Model, snap: &Snapshot, code: KeyCode) -> Vec<Action> {
    model.handle_key(key(code), snap)
}

fn type_text(model: &mut Model, snap: &Snapshot, text: &str) {
    for c in text.chars() {
        model.handle_key(letter(c), snap);
    }
}

/// `S`, then the section list is on screen with Downloads open.
fn open(snap: &Snapshot) -> Model {
    let mut model = model_of(snap);
    model.handle_key(letter('S'), snap);
    model
}

/// Opens Speed with the cursor on its first field.
fn open_speed(snap: &Snapshot) -> Model {
    let mut model = open(snap);
    press(&mut model, snap, KeyCode::Down);
    press(&mut model, snap, KeyCode::Tab);
    model
}

/// Types `text` into the field the cursor is on, replacing what is there.
fn replace_field(model: &mut Model, snap: &Snapshot, text: &str) {
    press(model, snap, KeyCode::Enter);
    for _ in 0..200 {
        press(model, snap, KeyCode::Backspace);
    }
    type_text(model, snap, text);
    press(model, snap, KeyCode::Enter);
}

#[test]
fn s_opens_full_screen_and_loads() {
    let snap = loaded(false);
    let mut model = model_of(&snap);
    let actions = model.handle_key(letter('S'), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Settings(SettingsCommand::Load))]
    );
    assert!(model.settings.is_some());

    let screen = Screen::new(&model, &snap, 100, 30);
    let text = screen.text();
    assert!(text.contains("Downloads"), "no section list:\n{text}");
    assert!(text.contains("Speed"), "no section list:\n{text}");
    assert!(text.contains("Default folder"), "no fields:\n{text}");
    assert!(
        !text.contains("Name") && !text.contains("Big.Buck.Bunny"),
        "the torrent table is still drawn:\n{text}"
    );
}

#[test]
fn tab_moves_between_list_and_fields() {
    let snap = loaded(false);
    let mut model = open(&snap);
    // On the list, Down opens the next section and Up the one before.
    press(&mut model, &snap, KeyCode::Down);
    assert!(
        Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("Download limit KiB/s")
    );
    press(&mut model, &snap, KeyCode::Up);
    assert!(
        Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("Default folder")
    );

    // Tab goes to the fields, where Down moves between them and the list does not move.
    press(&mut model, &snap, KeyCode::Tab);
    press(&mut model, &snap, KeyCode::Down);
    assert!(
        Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("Default folder")
    );
    let screen = Screen::new(&model, &snap, 100, 30);
    let (_, unfinished_row) = screen.find("Unfinished folder").expect("the field");
    assert!(
        screen.lines()[unfinished_row].contains("> "),
        "the cursor is not on the second field:\n{}",
        screen.text()
    );

    // Tab goes back to the list, where Down changes section again.
    press(&mut model, &snap, KeyCode::Tab);
    press(&mut model, &snap, KeyCode::Down);
    assert!(
        Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("Download limit KiB/s")
    );
}

#[test]
fn save_speed_sends_the_change() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");
    let actions = model.handle_key(ctrl_s(), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Settings(SettingsCommand::Save(
            SettingsChange::Speed(LiveLimits {
                download_kib: 64,
                upload_kib: 0,
                active_downloads: None,
                active_seeds: None,
                active_total: None,
            })
        )))]
    );

    // Not a number: nothing goes out and the field is marked.
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "abc");
    assert!(model.handle_key(ctrl_s(), &snap).is_empty());
}

#[test]
fn esc_with_unsaved_changes_asks_then_discards() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");

    // Esc asks instead of leaving.
    assert!(press(&mut model, &snap, KeyCode::Esc).is_empty());
    assert!(model.settings.is_some(), "Esc left with unsaved changes");
    let text = Screen::new(&model, &snap, 100, 30).text();
    assert!(
        text.contains("You have unsaved changes in Speed."),
        "{text}"
    );
    assert!(text.contains("Save changes") && text.contains("Discard changes"));

    // Esc again keeps editing.
    press(&mut model, &snap, KeyCode::Esc);
    assert!(model.settings.is_some());
    assert!(
        !Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("You have unsaved")
    );

    // `d` throws the change away and goes back to the torrents.
    press(&mut model, &snap, KeyCode::Esc);
    assert!(model.handle_key(letter('d'), &snap).is_empty());
    assert!(model.settings.is_none());

    // Reopening shows the saved value again, and Esc leaves at once.
    let mut model = open_speed(&snap);
    assert!(
        Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("Download limit KiB/s")
    );
    press(&mut model, &snap, KeyCode::Esc);
    assert!(model.settings.is_none());
}

#[test]
fn switching_section_with_unsaved_changes_asks_and_save_goes_out() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");
    press(&mut model, &snap, KeyCode::Tab);
    press(&mut model, &snap, KeyCode::Up);
    assert!(
        Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("You have unsaved changes in Speed.")
    );
    let actions = model.handle_key(letter('s'), &snap);
    assert!(matches!(
        actions.as_slice(),
        [Action::Command(Command::Settings(SettingsCommand::Save(
            SettingsChange::Speed(_)
        )))]
    ));
    // The save is answered well: the section is saved and the one asked for opens.
    model.push_notice(Notice::Info("Saved.".to_string()), Instant::now());
    assert!(
        Screen::new(&model, &snap, 100, 30)
            .text()
            .contains("Default folder")
    );
}

#[test]
fn remote_settings_show_set_in_server_config_file_for_run_a_program() {
    let snap = loaded(false);
    let mut model = open(&snap);
    let text = Screen::new(&model, &snap, 120, 30).text();
    assert!(text.contains("Set in the server's config file"), "{text}");

    // The cursor never lands on the program: Default, Unfinished, Paused, Watch, Save.
    press(&mut model, &snap, KeyCode::Tab);
    for _ in 0..10 {
        press(&mut model, &snap, KeyCode::Down);
    }
    // Change another field so the save carries something, then save.
    press(&mut model, &snap, KeyCode::Up);
    press(&mut model, &snap, KeyCode::Up);
    press(&mut model, &snap, KeyCode::Char(' ')); // Start torrents paused
    let actions = model.handle_key(ctrl_s(), &snap);
    let [
        Action::Command(Command::Settings(SettingsCommand::Save(SettingsChange::Downloads(
            change,
        )))),
    ] = actions.as_slice()
    else {
        panic!("not a downloads save: {actions:?}");
    };
    assert!(change.start_paused);
    assert_eq!(
        change.on_complete, None,
        "a program was sent from a surface that may not change it"
    );
}

#[test]
fn local_program_is_editable_and_sent() {
    let snap = loaded(true);
    let mut model = open(&snap);
    let text = Screen::new(&model, &snap, 120, 30).text();
    assert!(!text.contains("Set in the server's config file"), "{text}");

    // Saving without touching the program leaves it out, which keeps the one the file has.
    press(&mut model, &snap, KeyCode::Tab);
    let actions = model.handle_key(ctrl_s(), &snap);
    let [
        Action::Command(Command::Settings(SettingsCommand::Save(SettingsChange::Downloads(
            change,
        )))),
    ] = actions.as_slice()
    else {
        panic!("not a downloads save: {actions:?}");
    };
    assert_eq!(change.on_complete, None);
    model.push_notice(Notice::Info("Saved.".to_string()), Instant::now());

    // Typing a new program names it.
    let program = dir("tools").join("other");
    for _ in 0..4 {
        press(&mut model, &snap, KeyCode::Down);
    }
    replace_field(&mut model, &snap, &program.to_string_lossy());
    let actions = model.handle_key(ctrl_s(), &snap);
    let [
        Action::Command(Command::Settings(SettingsCommand::Save(SettingsChange::Downloads(
            change,
        )))),
    ] = actions.as_slice()
    else {
        panic!("not a downloads save: {actions:?}");
    };
    assert_eq!(
        *change,
        DownloadsChange {
            default_dir: dir("downloads"),
            unfinished_dir: None,
            start_paused: false,
            watch_dir: None,
            on_complete: Some(OnCompleteSet {
                program: Some(program)
            }),
        }
    );
}

#[test]
fn unsupported_server_shows_too_old_and_speed_uses_set_limits() {
    let mut snap = library();
    snap.settings = SettingsState::Unsupported;
    snap.limits.download_kib = 100;
    let mut model = open(&snap);
    let text = Screen::new(&model, &snap, 120, 30).text();
    assert!(
        text.contains(
            "This server is too old to change its settings here. Update it to change them."
        ),
        "{text}"
    );

    press(&mut model, &snap, KeyCode::Down);
    press(&mut model, &snap, KeyCode::Tab);
    assert!(Screen::new(&model, &snap, 120, 30).text().contains("100"));
    replace_field(&mut model, &snap, "64");
    let actions = model.handle_key(ctrl_s(), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::SetLimits(LiveLimits {
            download_kib: 64,
            ..snap.limits
        }))]
    );
}

#[test]
fn refresh_does_not_overwrite_a_dirty_form() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");

    // The server's numbers changed under us; the typed value stays.
    let mut newer = loaded(false);
    if let SettingsState::Loaded(view) = &mut newer.settings {
        view.speed.download_kib = 999;
        view.speed.upload_kib = 7;
    }
    model.sync_selection(&newer);
    let text = Screen::new(&model, &newer, 120, 30).text();
    assert!(text.contains("64") && !text.contains("999"), "{text}");

    // A clean form does follow the server.
    let mut model = open_speed(&snap);
    model.sync_selection(&newer);
    let text = Screen::new(&model, &newer, 120, 30).text();
    assert!(text.contains("999"), "{text}");
}

#[test]
fn a_refused_save_stays_on_screen_with_the_line() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");
    model.handle_key(ctrl_s(), &snap);
    model.push_notice(
        Notice::Error("Choose a folder inside the server's allowed folders.".to_string()),
        Instant::now(),
    );
    let text = Screen::new(&model, &snap, 120, 30).text();
    assert!(
        text.contains("Choose a folder inside the server's allowed folders."),
        "{text}"
    );
    // What was typed is still there, and still unsaved.
    assert!(text.contains("64"));
    press(&mut model, &snap, KeyCode::Esc);
    assert!(model.settings.is_some());
}

#[test]
fn paths_are_drawn_through_display_safe() {
    let mut snap = loaded(false);
    if let SettingsState::Loaded(view) = &mut snap.settings {
        view.downloads.default_dir =
            PathBuf::from(format!("{}\u{1b}[31mEVIL   TAIL", dir("d").display()));
        view.downloads.watch_problem = Some("bad\u{7}bell".to_string());
        view.downloads.on_complete.program = Some(PathBuf::from("run\u{202e}this"));
    }
    let model = open(&snap);
    let screen = Screen::new(&model, &snap, 140, 30);
    let text = screen.text();
    // Display-safe text has its control characters gone and its runs of spaces collapsed, which is
    // what tells a path drawn through `display_safe` from one drawn raw.
    assert!(
        text.contains("EVIL TAIL"),
        "the path was not drawn safely:\n{text}"
    );
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{7}') && !text.contains('\u{202e}'),
        "a control or format character reached the screen"
    );
}
