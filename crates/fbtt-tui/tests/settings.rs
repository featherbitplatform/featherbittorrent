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
        view.downloads.watch_problem = Some("watch   problem".to_string());
        view.downloads.on_complete_problem = Some("program   problem".to_string());
        view.downloads.on_complete.program = Some(PathBuf::from("run\u{1b}   this"));
    }
    let model = open(&snap);
    let text = Screen::new(&model, &snap, 140, 30).text();
    // Display-safe text has its control characters gone and its runs of spaces collapsed, which is
    // what tells a path drawn through `display_safe` from one drawn raw. Each of the four lines
    // carries its own marker, so dropping the call from any one of them fails here.
    for marker in ["EVIL TAIL", "watch problem", "program problem", "run this"] {
        assert!(
            text.contains(marker),
            "{marker:?} was not drawn safely:\n{text}"
        );
    }
    assert!(
        !text.contains('\u{1b}'),
        "a control character reached the screen"
    );
}

fn unsupported() -> Snapshot {
    let mut snap = library();
    snap.settings = SettingsState::Unsupported;
    snap.limits.download_kib = 100;
    snap
}

/// Esc asks when the open section has unsaved changes and leaves at once when it has none.
fn is_dirty(model: &mut Model, snap: &Snapshot) -> bool {
    press(model, snap, KeyCode::Esc);
    let stayed = model.settings.is_some();
    if stayed {
        // Put the question away again.
        press(model, snap, KeyCode::Esc);
    }
    stayed
}

#[test]
fn unsupported_set_limits_failure_is_shown_and_form_stays_dirty() {
    let snap = unsupported();
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");
    model.handle_key(ctrl_s(), &snap);
    model.push_notice(
        Notice::Error("the engine refused".to_string()),
        Instant::now(),
    );
    let text = Screen::new(&model, &snap, 120, 30).text();
    assert!(text.contains("the engine refused"), "{text}");
    assert!(
        is_dirty(&mut model, &snap),
        "a refused save marked the form saved"
    );
    // The snapshot's old limits do not revert what was typed.
    model.sync_selection(&snap);
    assert!(Screen::new(&model, &snap, 120, 30).text().contains("64"));

    // A save with no answer but the limits showing up in the next snapshot is a good one.
    model.handle_key(ctrl_s(), &snap);
    assert!(
        is_dirty(&mut model, &snap),
        "saved before the limits arrived"
    );
    let mut newer = snap.clone();
    newer.limits.download_kib = 64;
    model.sync_selection(&newer);
    assert!(!is_dirty(&mut model, &newer));
}

#[test]
fn typing_during_a_save_keeps_the_form_dirty() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");
    model.handle_key(ctrl_s(), &snap);
    replace_field(&mut model, &snap, "65");
    model.push_notice(Notice::Info("Saved.".to_string()), Instant::now());
    assert!(
        is_dirty(&mut model, &snap),
        "an edit made during the save was marked saved"
    );
}

#[test]
fn an_unrelated_notice_does_not_answer_the_save() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");
    model.handle_key(ctrl_s(), &snap);
    model.push_notice(Notice::Info("Torrent added".to_string()), Instant::now());
    assert!(
        is_dirty(&mut model, &snap),
        "another line answered the save"
    );
    model.push_notice(Notice::Info("Saved.".to_string()), Instant::now());
    assert!(!is_dirty(&mut model, &snap));
}

#[test]
fn ctrl_s_while_saving_sends_once() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");
    assert_eq!(model.handle_key(ctrl_s(), &snap).len(), 1);
    assert!(
        model.handle_key(ctrl_s(), &snap).is_empty(),
        "a second save went out"
    );
    assert!(
        Screen::new(&model, &snap, 120, 30)
            .text()
            .contains("saving")
    );
    // A refusal frees the Save row again.
    model.push_notice(Notice::Error("no".to_string()), Instant::now());
    assert_eq!(model.handle_key(ctrl_s(), &snap).len(), 1);
}

#[test]
fn refused_load_is_shown() {
    let snap = library();
    let mut model = open(&snap);
    assert!(
        Screen::new(&model, &snap, 120, 30)
            .text()
            .contains("Loading settings.")
    );
    model.push_notice(
        Notice::Error("cannot read the settings".to_string()),
        Instant::now(),
    );
    let text = Screen::new(&model, &snap, 120, 30).text();
    assert!(text.contains("cannot read the settings"), "{text}");
    assert!(!text.contains("Loading settings."));
    press(&mut model, &snap, KeyCode::Esc);
    assert!(model.settings.is_none(), "Esc did not leave");
}

#[test]
fn speed_fields_all_convert() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    for text in ["2048", "1024", "3", "", "5"] {
        replace_field(&mut model, &snap, text);
        press(&mut model, &snap, KeyCode::Down);
    }
    let actions = model.handle_key(ctrl_s(), &snap);
    assert_eq!(
        actions,
        vec![Action::Command(Command::Settings(SettingsCommand::Save(
            SettingsChange::Speed(LiveLimits {
                download_kib: 2048,
                upload_kib: 1024,
                active_downloads: Some(3),
                active_seeds: None,
                active_total: Some(5),
            })
        )))]
    );

    // A field that is not a number is marked red and nothing goes out.
    let mut model = open_speed(&snap);
    press(&mut model, &snap, KeyCode::Enter);
    type_text(&mut model, &snap, "abc");
    press(&mut model, &snap, KeyCode::Enter);
    assert!(model.handle_key(ctrl_s(), &snap).is_empty());
    let screen = Screen::new(&model, &snap, 120, 30);
    let (x, y) = screen.find("0abc").expect("the field was not drawn");
    assert_eq!(
        screen.fg(x, y),
        ratatui::style::Color::Rgb(0xed, 0x4a, 0x49)
    );
}

#[test]
fn a_long_value_keeps_the_caret_in_view() {
    let snap = loaded(false);
    let mut model = open(&snap);
    press(&mut model, &snap, KeyCode::Tab);
    press(&mut model, &snap, KeyCode::Enter);
    type_text(&mut model, &snap, &"z".repeat(200));
    type_text(&mut model, &snap, "END");
    let text = Screen::new(&model, &snap, 80, 20).text();
    assert!(
        text.contains("zzEND"),
        "the end of a long value is not in view:\n{text}"
    );
}

/// Edits Speed, sends it, edits again, and discards the change by moving to Downloads: the first
/// send is then given up on and the screen is on Downloads.
fn discard_with_a_save_out(model: &mut Model, snap: &Snapshot) {
    replace_field(model, snap, "64");
    assert_eq!(model.handle_key(ctrl_s(), snap).len(), 1);
    replace_field(model, snap, "65");
    press(model, snap, KeyCode::Tab);
    press(model, snap, KeyCode::Up);
    model.handle_key(letter('d'), snap);
}

#[test]
fn discard_during_a_save_then_save_again_sends() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    discard_with_a_save_out(&mut model, &snap);
    assert!(
        Screen::new(&model, &snap, 120, 30)
            .text()
            .contains("Default folder")
    );
    press(&mut model, &snap, KeyCode::Tab);
    let actions = model.handle_key(ctrl_s(), &snap);
    assert_eq!(actions.len(), 1, "Ctrl+S stayed dead after a discard");
}

#[test]
fn a_late_answer_to_a_discarded_save_is_ignored() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    discard_with_a_save_out(&mut model, &snap);
    // Back to Speed and a second save.
    press(&mut model, &snap, KeyCode::Down);
    press(&mut model, &snap, KeyCode::Tab);
    replace_field(&mut model, &snap, "66");
    assert_eq!(model.handle_key(ctrl_s(), &snap).len(), 1);
    // The first save's answer arrives now: it is not the second's.
    model.push_notice(Notice::Info("Saved.".to_string()), Instant::now());
    assert!(
        is_dirty(&mut model, &snap),
        "a late answer completed a newer save"
    );
    // The second's own answer does.
    model.push_notice(Notice::Info("Saved.".to_string()), Instant::now());
    assert!(!is_dirty(&mut model, &snap));
}

#[test]
fn a_save_with_no_answer_times_out_and_can_be_retried() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");
    model.handle_key(ctrl_s(), &snap);
    let start = Instant::now();
    model.tick(start);
    model.tick(start + std::time::Duration::from_secs(9));
    assert!(
        model.handle_key(ctrl_s(), &snap).is_empty(),
        "timed out too early"
    );
    model.tick(start + std::time::Duration::from_secs(11));
    let text = Screen::new(&model, &snap, 120, 30).text();
    assert!(text.contains("No answer from the server."), "{text}");
    assert!(
        is_dirty(&mut model, &snap),
        "a timeout marked the form saved"
    );
    assert_eq!(
        model.handle_key(ctrl_s(), &snap).len(),
        1,
        "Save stayed dead after the timeout"
    );
}

#[test]
fn s_while_a_save_is_out_waits_and_then_leaves() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");
    assert_eq!(model.handle_key(ctrl_s(), &snap).len(), 1);
    // Back out while it is out: the form still differs from what was saved.
    press(&mut model, &snap, KeyCode::Esc);
    assert!(
        model.handle_key(letter('s'), &snap).is_empty(),
        "a second save went out"
    );
    assert!(model.settings.is_some());
    model.push_notice(Notice::Info("Saved.".to_string()), Instant::now());
    assert!(model.settings.is_none(), "the leave was forgotten");

    // A refusal keeps the screen and shows the line.
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");
    model.handle_key(ctrl_s(), &snap);
    press(&mut model, &snap, KeyCode::Esc);
    model.handle_key(letter('s'), &snap);
    model.push_notice(Notice::Error("refused".to_string()), Instant::now());
    assert!(model.settings.is_some());
    assert!(
        Screen::new(&model, &snap, 120, 30)
            .text()
            .contains("refused")
    );
}

#[test]
fn a_wide_value_keeps_the_caret_in_view() {
    let snap = loaded(false);
    let mut model = open(&snap);
    press(&mut model, &snap, KeyCode::Tab);
    press(&mut model, &snap, KeyCode::Enter);
    type_text(&mut model, &snap, &"\u{6f22}".repeat(60));
    type_text(&mut model, &snap, "END");
    let text = Screen::new(&model, &snap, 80, 20).text();
    assert!(
        text.contains("END"),
        "the end of a wide value is not in view:\n{text}"
    );
}

#[test]
fn a_timed_out_save_then_a_retry_is_confirmed() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    replace_field(&mut model, &snap, "64");
    model.handle_key(ctrl_s(), &snap);
    let start = Instant::now();
    model.tick(start);
    model.tick(start + std::time::Duration::from_secs(11));
    assert_eq!(
        model.handle_key(ctrl_s(), &snap).len(),
        1,
        "the retry did not go out"
    );
    // The retry is answered: that answer is the retry's, not a leftover of the timed-out send.
    model.push_notice(
        Notice::Info("Saved.".to_string()),
        start + std::time::Duration::from_secs(12),
    );
    assert!(
        !is_dirty(&mut model, &snap),
        "the retry's answer was swallowed"
    );
}

#[test]
fn a_discarded_save_answer_after_its_deadline_is_not_swallowed() {
    let snap = loaded(false);
    let mut model = open_speed(&snap);
    discard_with_a_save_out(&mut model, &snap);
    let start = Instant::now();
    model.tick(start);
    model.tick(start + std::time::Duration::from_secs(11));
    // A new save, answered well after the discarded one could still be answered.
    press(&mut model, &snap, KeyCode::Down);
    press(&mut model, &snap, KeyCode::Tab);
    replace_field(&mut model, &snap, "66");
    assert_eq!(model.handle_key(ctrl_s(), &snap).len(), 1);
    model.push_notice(
        Notice::Info("Saved.".to_string()),
        start + std::time::Duration::from_secs(12),
    );
    assert!(
        !is_dirty(&mut model, &snap),
        "an expired stale entry swallowed the answer"
    );
}
