//! What closing the window does, how the app quits, and how a launch at login starts.
//!
//! The decisions are pure functions ([`on_close_requested`], [`initial_window_state`]) tested without a
//! webview; the rest of this file applies them to the app. Every way out of the app ends in [`quit`],
//! which stops This computer (the engine's final save) before the process exits: the tray's Quit, a close
//! that means quit, the page's own answer to the first-close question, a quit signal on Unix, and an exit
//! request Tauri reports while This computer runs. The system's own quit (Cmd+Q, logging out, shutting
//! down) reaches [`handle_run_event`] as `RunEvent::Exit` with nothing before it, and the process ends
//! when that handler returns: so the handler stops This computer itself when no quit has started, and
//! waits for the save of the quit that has ([`on_exit`], [`QUIT_WAIT`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use fbtt_desktop_core::{CloseAction, DesktopSettings};
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime};
use tauri_plugin_autostart::ManagerExt;

use crate::commands::DesktopState;

/// The argument the login item launches the app with: start without showing the window.
pub const BACKGROUND_FLAG: &str = "--background";

/// The event the page gets when closing the window needs its answer first.
pub const CLOSE_REQUESTED_EVENT: &str = "close-requested";

/// The label of the app's one window, as `tauri.conf.json` names it.
pub const MAIN_WINDOW: &str = "main";

/// What a request to close the window turns into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    /// Stop This computer and exit.
    Quit,
    /// Hide the window; the app keeps running in the tray.
    HideToTray,
    /// Keep the window, and ask the page what closing should do.
    AskPage,
}

/// What closing the window does, given the saved choice and whether this system gave the app a tray.
///
/// Without a tray there is nowhere to keep running, so both `Ask` and `Tray` quit: the question is only
/// worth asking when both of its answers can happen.
pub fn on_close_requested(settings: &DesktopSettings, tray_available: bool) -> CloseOutcome {
    match (settings.close, tray_available) {
        (CloseAction::Quit, _) | (_, false) => CloseOutcome::Quit,
        (CloseAction::Ask, true) => CloseOutcome::AskPage,
        (CloseAction::Tray, true) => CloseOutcome::HideToTray,
    }
}

/// How the window starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    /// Shown and focused: every launch a person makes.
    Shown,
    /// Not shown at all; the tray's Show brings it up.
    Hidden,
    /// Shown minimized: a background launch on a system with no tray, where a hidden window could not be
    /// brought back.
    Minimized,
}

/// How the window starts for a launch with `args` (the program name included or not).
///
/// A launch with [`BACKGROUND_FLAG`] (the login item's) keeps the window out of the way: hidden with a
/// tray, minimized without one.
pub fn initial_window_state<I, S>(args: I, tray_available: bool) -> WindowState
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let background = args.into_iter().any(|arg| arg.as_ref() == BACKGROUND_FLAG);
    match (background, tray_available) {
        (false, _) => WindowState::Shown,
        (true, true) => WindowState::Hidden,
        (true, false) => WindowState::Minimized,
    }
}

/// The page's spelling of a close answer: `quit` or `tray`. `ask` is not an answer, it is the question.
pub fn parse_close_action(action: &str) -> Result<CloseAction, String> {
    match action {
        "quit" => Ok(CloseAction::Quit),
        "tray" => Ok(CloseAction::Tray),
        other => Err(format!(
            "Closing the window cannot be set to {}.",
            fbt::display_safe(other, fbt::MAX_DISPLAY_CHARS)
        )),
    }
}

/// Changes one thing in the settings file at `path` and saves it, keeping everything else in the file as
/// it is on disk now (other screens save their own settings into the same file). A file that cannot be
/// read is left alone and the change is refused with its line.
pub fn update_settings(
    path: &Path,
    change: impl FnOnce(&mut DesktopSettings),
) -> Result<(), String> {
    let mut settings = DesktopSettings::load(path).map_err(|problem| problem.to_string())?;
    change(&mut settings);
    settings.save(path).map_err(|problem| problem.to_string())
}

/// The settings closing the window reads: the file as it is now, or the defaults when it cannot be read
/// (which ask, or quit without a tray).
fn current_settings(path: &Path) -> DesktopSettings {
    DesktopSettings::load(path).unwrap_or_else(|problem| {
        eprintln!("{problem}");
        DesktopSettings::default()
    })
}

/// The longest the system's exit waits for a quit's final save before it lets the process end anyway.
pub const QUIT_WAIT: Duration = Duration::from_secs(30);

/// Where the app is on its way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitPhase {
    /// This computer runs; nothing has asked to quit.
    Running,
    /// A quit has taken This computer out and is waiting for its final save.
    Quitting,
    /// The final save is done (or there was nothing to save): the process may end.
    Stopped,
}

/// What an exit request (`RunEvent::ExitRequested`) gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitRequestAnswer {
    /// Hold it back and start the app's own quit, which ends in the exit once the save is done.
    HoldAndQuit,
    /// Hold it back: a quit is already saving and will exit when it is done.
    Hold,
    /// Let it through: it is the quit's own final exit.
    Allow,
}

/// What an exit request gets in `phase`.
pub fn on_exit_requested(phase: QuitPhase) -> ExitRequestAnswer {
    match phase {
        QuitPhase::Running => ExitRequestAnswer::HoldAndQuit,
        QuitPhase::Quitting => ExitRequestAnswer::Hold,
        QuitPhase::Stopped => ExitRequestAnswer::Allow,
    }
}

/// What the event loop's last event (`RunEvent::Exit`) does before the process ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitAnswer {
    /// Nothing has stopped This computer: stop it here, and wait for the save.
    StopHere,
    /// A quit is saving: wait for it (at most [`QUIT_WAIT`]).
    WaitForQuit,
    /// The save is done.
    Nothing,
}

/// What the last event does in `phase`.
pub fn on_exit(phase: QuitPhase) -> ExitAnswer {
    match phase {
        QuitPhase::Running => ExitAnswer::StopHere,
        QuitPhase::Quitting => ExitAnswer::WaitForQuit,
        QuitPhase::Stopped => ExitAnswer::Nothing,
    }
}

/// What the shell keeps for closing and quitting.
#[derive(Debug)]
pub struct Lifecycle {
    settings_file: PathBuf,
    tray_available: AtomicBool,
    phase: Mutex<QuitPhase>,
    stopped: Condvar,
}

impl Lifecycle {
    /// The lifecycle of a run whose settings live at `settings_file`. No tray until [`set_tray_available`]
    /// says one was built.
    ///
    /// [`set_tray_available`]: Lifecycle::set_tray_available
    pub fn new(settings_file: PathBuf) -> Self {
        Lifecycle {
            settings_file,
            tray_available: AtomicBool::new(false),
            phase: Mutex::new(QuitPhase::Running),
            stopped: Condvar::new(),
        }
    }

    /// Records whether the tray builder gave the app a tray.
    pub fn set_tray_available(&self, available: bool) {
        self.tray_available.store(available, Ordering::SeqCst);
    }

    /// Whether the app has a tray to keep running in.
    pub fn tray_available(&self) -> bool {
        self.tray_available.load(Ordering::SeqCst)
    }

    /// What closing the window does now.
    pub fn close_outcome(&self) -> CloseOutcome {
        on_close_requested(
            &current_settings(&self.settings_file),
            self.tray_available(),
        )
    }

    /// Where the app is on its way out.
    pub fn phase(&self) -> QuitPhase {
        *self
            .phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Moves from running to quitting. True the first time, false every time after: one quit runs, the
    /// rest wait for it.
    fn begin_quit(&self) -> bool {
        let mut phase = self
            .phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *phase != QuitPhase::Running {
            return false;
        }
        *phase = QuitPhase::Quitting;
        true
    }

    /// The final save is done: wakes every exit waiting for it.
    fn finish_quit(&self) {
        *self
            .phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = QuitPhase::Stopped;
        self.stopped.notify_all();
    }

    /// Waits until a quit in progress has finished its save, for at most `limit`. True when it finished
    /// (or none was in progress), false when the wait ran out.
    fn wait_for_quit(&self, limit: Duration) -> bool {
        let phase = self
            .phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (phase, _) = self
            .stopped
            .wait_timeout_while(phase, limit, |phase| *phase == QuitPhase::Quitting)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *phase != QuitPhase::Quitting
    }
}

/// Shows the main window, brings it out of the Dock or taskbar, and focuses it.
pub fn show_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Puts the window in the state a launch asked for. The window starts invisible (`tauri.conf.json`), so
/// every launch passes through here.
pub fn apply_initial_state<R: Runtime>(app: &AppHandle<R>, state: WindowState) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    match state {
        WindowState::Shown => {
            let _ = window.show();
            let _ = window.set_focus();
        }
        WindowState::Hidden => {}
        WindowState::Minimized => {
            let _ = window.show();
            let _ = window.minimize();
        }
    }
}

/// Does what closing the window means now. The close itself has already been held back by the caller,
/// so the window only goes away through here.
pub fn close_window<R: Runtime>(app: &AppHandle<R>) {
    match app.state::<Lifecycle>().close_outcome() {
        CloseOutcome::Quit => quit(app),
        CloseOutcome::HideToTray => {
            if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                let _ = window.hide();
            }
        }
        CloseOutcome::AskPage => {
            if let Err(problem) = app.emit(CLOSE_REQUESTED_EVENT, ()) {
                eprintln!("The window could not ask what closing should do: {problem}");
            }
        }
    }
}

/// Quits the app: the window goes away at once, This computer stops (its final save), then the process
/// exits. A second quit while the first is stopping does nothing, so it cannot exit before the save.
pub fn quit<R: Runtime>(app: &AppHandle<R>) {
    if !app.state::<Lifecycle>().begin_quit() {
        return;
    }
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.hide();
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let this_computer = app.state::<DesktopState>().take();
        if let Some(this_computer) = this_computer {
            this_computer.stop().await;
        }
        // Stopped before the exit, so the exit request it causes is let through.
        app.state::<Lifecycle>().finish_quit();
        app.exit(0);
    });
}

/// The app's answer to the event loop's own events: exit requests, the last event, and the Dock icon.
pub fn handle_run_event<R: Runtime>(app: &AppHandle<R>, event: RunEvent) {
    match event {
        RunEvent::ExitRequested { api, .. } => {
            match on_exit_requested(app.state::<Lifecycle>().phase()) {
                ExitRequestAnswer::HoldAndQuit => {
                    api.prevent_exit();
                    quit(app);
                }
                ExitRequestAnswer::Hold => api.prevent_exit(),
                ExitRequestAnswer::Allow => {}
            }
        }
        // The Dock icon clicked while the window is hidden in the tray.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_window(app),
        RunEvent::Exit => {
            let lifecycle = app.state::<Lifecycle>();
            match on_exit(lifecycle.phase()) {
                ExitAnswer::StopHere => {
                    if lifecycle.begin_quit() {
                        if let Some(this_computer) = app.state::<DesktopState>().take() {
                            tauri::async_runtime::block_on(this_computer.stop());
                        }
                        lifecycle.finish_quit();
                    } else if !lifecycle.wait_for_quit(QUIT_WAIT) {
                        eprintln!("The final save did not finish in time; quitting without it.");
                    }
                }
                ExitAnswer::WaitForQuit => {
                    if !lifecycle.wait_for_quit(QUIT_WAIT) {
                        eprintln!("The final save did not finish in time; quitting without it.");
                    }
                }
                ExitAnswer::Nothing => {}
            }
        }
        _ => {}
    }
}

/// Turns a quit signal (SIGTERM, SIGINT, SIGHUP) into [`quit`], so a stop by `kill` or a session ending
/// still runs the final save. Windows has no such signals for a windowed app; its logout and shutdown
/// arrive as the app's exit.
#[cfg(unix)]
pub fn quit_on_signals<R: Runtime>(app: &AppHandle<R>) {
    use tokio::signal::unix::{SignalKind, signal};

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let streams = [
            SignalKind::terminate(),
            SignalKind::interrupt(),
            SignalKind::hangup(),
        ]
        .map(signal);
        let [Ok(mut terminate), Ok(mut interrupt), Ok(mut hangup)] = streams else {
            eprintln!(
                "The quit signals could not be watched; a stop by signal skips the final save."
            );
            return;
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
            _ = hangup.recv() => {}
        }
        quit(&app);
    });
}

/// Without Unix signals there is nothing to watch.
#[cfg(not(unix))]
pub fn quit_on_signals<R: Runtime>(_app: &AppHandle<R>) {}

/// Saves what closing the window does from now on: `quit` or `tray`, the page's answer to the first-close
/// question (and later the Settings switch).
#[tauri::command]
pub fn set_close_action(action: String, state: tauri::State<'_, Lifecycle>) -> Result<(), String> {
    let action = parse_close_action(&action)?;
    update_settings(&state.settings_file, |settings| settings.close = action)
}

/// Closes the window the way the saved choice says: what the page calls once it has saved an answer.
#[tauri::command]
pub fn close_window_now(app: AppHandle) {
    close_window(&app);
}

/// Whether this system gave the app a tray, so the page offers `Keep running in the tray` only when it can
/// happen.
#[tauri::command]
pub fn tray_available(state: tauri::State<'_, Lifecycle>) -> bool {
    state.tray_available()
}

/// Turns Start at login on or off: the login item is written or removed first, and the setting is saved
/// only once that worked, so the file never says what the system does not do.
#[tauri::command]
pub fn set_start_at_login(
    on: bool,
    app: AppHandle,
    state: tauri::State<'_, Lifecycle>,
) -> Result<(), String> {
    let autolaunch = app.autolaunch();
    let changed = if on {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };
    changed.map_err(|problem| format!("Start at login could not be changed: {problem}"))?;
    update_settings(&state.settings_file, |settings| {
        settings.start_at_login = on
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use fbtt_core::scratch::ScratchDir;
    use fbtt_desktop_core::{CloseAction, DesktopSettings};

    use super::{
        CloseOutcome, ExitAnswer, ExitRequestAnswer, Lifecycle, QuitPhase, WindowState,
        initial_window_state, on_close_requested, on_exit, on_exit_requested, parse_close_action,
        update_settings,
    };

    fn closing(close: CloseAction) -> DesktopSettings {
        DesktopSettings {
            close,
            ..DesktopSettings::default()
        }
    }

    #[test]
    fn ask_with_a_tray_asks_the_page() {
        assert_eq!(
            on_close_requested(&closing(CloseAction::Ask), true),
            CloseOutcome::AskPage
        );
    }

    #[test]
    fn ask_without_a_tray_quits() {
        assert_eq!(
            on_close_requested(&closing(CloseAction::Ask), false),
            CloseOutcome::Quit
        );
    }

    #[test]
    fn tray_without_a_tray_quits() {
        assert_eq!(
            on_close_requested(&closing(CloseAction::Tray), false),
            CloseOutcome::Quit
        );
        assert_eq!(
            on_close_requested(&closing(CloseAction::Tray), true),
            CloseOutcome::HideToTray
        );
    }

    #[test]
    fn quit_quits() {
        for tray in [true, false] {
            assert_eq!(
                on_close_requested(&closing(CloseAction::Quit), tray),
                CloseOutcome::Quit
            );
        }
    }

    #[test]
    fn background_flag_hides_or_minimizes() {
        let plain = ["featherbittorrent-desktop"];
        let background = ["featherbittorrent-desktop", "--background"];
        assert_eq!(initial_window_state(plain, true), WindowState::Shown);
        assert_eq!(initial_window_state(plain, false), WindowState::Shown);
        assert_eq!(initial_window_state(background, true), WindowState::Hidden);
        assert_eq!(
            initial_window_state(background, false),
            WindowState::Minimized
        );
        assert_eq!(
            initial_window_state(["featherbittorrent-desktop", "--backgrounds"], true),
            WindowState::Shown,
            "only the exact flag counts"
        );
    }

    #[test]
    fn close_answers_are_quit_or_tray() {
        assert_eq!(parse_close_action("quit"), Ok(CloseAction::Quit));
        assert_eq!(parse_close_action("tray"), Ok(CloseAction::Tray));
        assert!(parse_close_action("ask").is_err());
        assert!(parse_close_action("").is_err());
    }

    /// Saving the close answer changes that one setting and keeps the rest of the file, and the next
    /// close reads it.
    #[test]
    fn the_saved_answer_is_what_the_next_close_does() {
        let dir = ScratchDir::new("desktop-close-answer");
        let path = dir.path().join("desktop.yaml");
        let before = DesktopSettings {
            system_title_bar: true,
            ..DesktopSettings::default()
        };
        before.save(&path).expect("write the settings");

        let lifecycle = Lifecycle::new(path.clone());
        assert_eq!(lifecycle.close_outcome(), CloseOutcome::Quit, "no tray yet");
        lifecycle.set_tray_available(true);
        assert_eq!(lifecycle.close_outcome(), CloseOutcome::AskPage);

        update_settings(&path, |settings| settings.close = CloseAction::Tray).expect("saves");
        assert_eq!(lifecycle.close_outcome(), CloseOutcome::HideToTray);
        let after = DesktopSettings::load(&path).expect("reads");
        assert!(after.system_title_bar, "the other settings were kept");
    }

    /// A settings file that does not parse is left as it is: the change is refused, not written over it.
    #[test]
    fn an_unreadable_file_is_not_overwritten() {
        let dir = ScratchDir::new("desktop-close-unreadable");
        let path = dir.path().join("desktop.yaml");
        std::fs::write(&path, "close: [not, a, choice\n").expect("write");
        assert!(update_settings(&path, |settings| settings.close = CloseAction::Quit).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "close: [not, a, choice\n"
        );
    }

    #[test]
    fn only_the_first_quit_runs() {
        let lifecycle = Lifecycle::new("unused.yaml".into());
        assert!(lifecycle.begin_quit());
        assert!(!lifecycle.begin_quit());
        lifecycle.finish_quit();
        assert!(
            !lifecycle.begin_quit(),
            "a finished quit does not start again"
        );
    }

    /// Exit requests are held back while This computer runs or saves, and only the quit's own final
    /// exit, after the save, goes through.
    #[test]
    fn exit_requests_are_held_until_the_save_is_done() {
        assert_eq!(
            on_exit_requested(QuitPhase::Running),
            ExitRequestAnswer::HoldAndQuit
        );
        assert_eq!(
            on_exit_requested(QuitPhase::Quitting),
            ExitRequestAnswer::Hold
        );
        assert_eq!(
            on_exit_requested(QuitPhase::Stopped),
            ExitRequestAnswer::Allow
        );
        assert_eq!(on_exit(QuitPhase::Running), ExitAnswer::StopHere);
        assert_eq!(on_exit(QuitPhase::Quitting), ExitAnswer::WaitForQuit);
        assert_eq!(on_exit(QuitPhase::Stopped), ExitAnswer::Nothing);
    }

    /// The tray's Quit has started and is saving; Cmd+Q (the system's exit, with no request first)
    /// arrives: the exit waits for the save and only then lets the process end.
    #[test]
    fn a_system_exit_during_a_quit_waits_for_the_save() {
        let lifecycle = Arc::new(Lifecycle::new("unused.yaml".into()));
        assert!(lifecycle.begin_quit(), "the tray's Quit starts");
        assert_eq!(on_exit(lifecycle.phase()), ExitAnswer::WaitForQuit);

        let saved = Arc::new(AtomicBool::new(false));
        let saver = {
            let lifecycle = Arc::clone(&lifecycle);
            let saved = Arc::clone(&saved);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                saved.store(true, Ordering::SeqCst);
                lifecycle.finish_quit();
            })
        };
        assert!(lifecycle.wait_for_quit(Duration::from_secs(10)));
        assert!(
            saved.load(Ordering::SeqCst),
            "the exit went on before the save finished"
        );
        assert_eq!(
            on_exit_requested(lifecycle.phase()),
            ExitRequestAnswer::Allow,
            "the quit's own final exit goes through"
        );
        saver.join().expect("the saver thread");
    }

    /// A save that never finishes does not hold the exit forever.
    #[test]
    fn the_wait_for_a_save_is_bounded() {
        let lifecycle = Lifecycle::new("unused.yaml".into());
        assert!(
            lifecycle.wait_for_quit(Duration::from_millis(10)),
            "no quit, no wait"
        );
        assert!(lifecycle.begin_quit());
        assert!(!lifecycle.wait_for_quit(Duration::from_millis(50)));
        assert_eq!(lifecycle.phase(), QuitPhase::Quitting);
    }
}
