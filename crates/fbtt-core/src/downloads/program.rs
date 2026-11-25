//! The program a finished torrent starts, the names that are refused before anything is started, and the
//! two arguments it always gets.
//!
//! The person names a program - in `config.yaml`, or from the TUI or the desktop app on a library they
//! run themselves - and featherbit torrent starts it with the torrent's name and the folder its files
//! finished in. Two separate arguments through [`std::process::Command`], never a shell: a name holding
//! a space, a quote or `$(...)` is one argument to the program, not a command to run.
//!
//! A name is checked before it is started, because a server runs from a directory nobody chose: a
//! program named as `hooks/notify.sh` means "below whatever directory this process happened to be started
//! in", and a bare name means a search of `PATH`, which on a service is not the `PATH` of the person who
//! wrote the line. Both are refused with a line naming the program, rather than started somewhere
//! surprising - or, on Windows, handed to `cmd.exe`, which is what a `.bat` is started with and which
//! would read the torrent's name as its own argument.
//!
//! It is started and left alone. It is not waited for (a program that plays something for an hour would
//! hold the automation), and it is not killed when this program forgets it (nobody wants quitting the
//! app to stop a film). The only thing anybody here needs to know is whether it *started*: that is the
//! caller's notice. Its exit status goes in the log, and the wait for it happens on a plain thread of
//! its own, where blocking for as long as the program runs costs a thread and nothing else.

use std::ffi::OsString;
// The platform-side ways to start a program quietly: no process group this app's Ctrl-C reaches, and
// on Windows no console window behind it.
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use thiserror::Error;

use crate::settings;

/// The Windows creation flag that keeps a program from being handed a console window of its own behind
/// the one this app may be holding: it brings its own, if it has one.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The name given to the thread that waits for a completion program, so that a log line or a stack
/// dump says what it is waiting for.
const WAITER: &str = "fbtt-completion-program";

/// The two arguments a completion program is given: the torrent's name, then the folder its files
/// finished in.
///
/// Two values, not one line of shell: nothing here quotes, splits or expands anything, because nothing
/// here runs a shell. The name comes first, and a torrent named `-torrent` by its author is a perfectly
/// ordinary name: a program that parses its own arguments sees an option there, which is the program's
/// business and not something this run can rename around.
pub(crate) fn completion_args(name: &str, folder: &Path) -> [OsString; 2] {
    [name.into(), folder.as_os_str().to_os_string()]
}

/// Why this config's program is not started, worded to be read straight after the program's own path:
/// `Could not start the program run on completion: {path} {this}`.
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum Refused {
    /// A name with no leading `/`, no drive letter and no UNC root: it means either "below whatever
    /// directory this process was started in" or "look it up on `PATH`", and neither is the folder the
    /// person was thinking of.
    #[error("is not an absolute path: name the program by the whole path to it")]
    NotAbsolute,
    /// A `.bat` or a `.cmd`: Windows starts those through `cmd.exe`, so the two arguments this run
    /// passes would arrive at a shell, where a name holding a space or a quote stops being one argument.
    #[error(
        "is a Windows batch file, which Windows starts through a shell: name a program to start directly"
    )]
    BatchFile,
    /// Nothing to start at that path, or something there that is not a program this run can start.
    #[error("would not start: {reason}")]
    WouldNotStart {
        /// What the operating system said when asked to start it.
        reason: String,
    },
}

/// Whether `program` is a name this run refuses, without touching the filesystem and without starting
/// anything.
///
/// `windows` is a parameter rather than a `cfg` so that the batch rule - a rule about what *Windows*
/// does with a `.bat` - can be checked from any host by saying which platform to answer for. The path
/// rule comes first: a relative batch file is refused for being relative, which is the more immediate
/// mistake and the one that has nothing to do with the file's type.
pub(crate) fn refusal(program: &Path, windows: bool) -> Option<Refused> {
    if !program.is_absolute() {
        return Some(Refused::NotAbsolute);
    }
    (windows && settings::batch_file(program)).then_some(Refused::BatchFile)
}

/// Starts `program` for a torrent that finished, and lets go of it.
///
/// `Err` is why it was not started - a name that is refused (see [`refusal`]), or nothing there to
/// start - which is the only thing about it anybody outside the log is told. Once it is running its
/// exit status is the log's business: it is waited for on its own thread so the caller never blocks on
/// it.
///
/// [`std::process::Command`] rather than [`tokio::process::Command`] (which is what
/// [`crate::player`] uses to start a media player): there is nothing to tidy up after this one, so the
/// wait needs no runtime, and a plain thread is the cheapest place to put one.
pub(crate) fn run(program: &Path, name: &str, folder: &Path) -> Result<(), Refused> {
    if let Some(refused) = refusal(program, cfg!(windows)) {
        return Err(refused);
    }

    let args = completion_args(name, folder);
    let mut command = Command::new(program);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    // Its own process group, so a Ctrl-C at this app's terminal is not delivered to it too.
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let child = command.spawn().map_err(|problem| Refused::WouldNotStart {
        reason: problem.to_string(),
    })?;
    watch_exit(child, program);
    Ok(())
}

/// Waits for `child` somewhere that can afford to be blocked for as long as it runs, and logs what it
/// exited with.
///
/// A plain named thread first: it is the cheapest place to put a wait of unknown length, and a program
/// that plays a film for an hour must not occupy a slot of the runtime's blocking pool for the whole of
/// that. If the operating system will not give us a thread - the only way this fails - the runtime's
/// own blocking pool is asked, since a run of this crate is inside a runtime; and if there is not even
/// one of those, nothing is waited for at all. The program is already running by then, so what is lost
/// in every fallback is the log line about how it ended, never the program.
///
/// Dropping the thread's handle is deliberate: this task must not join on a program it has been told
/// not to wait for, and a thread that is still waiting when this process ends is left with it.
fn watch_exit(child: Child, program: &Path) {
    // Shared rather than moved into the first thread, so the fallbacks below still have it to wait on.
    let child = Arc::new(Mutex::new(child));
    let started = program.to_path_buf();
    let wait = {
        let (child, started) = (Arc::clone(&child), started.clone());
        move || log_exit(&child, &started)
    };
    match thread::Builder::new().name(WAITER.to_owned()).spawn(wait) {
        Ok(_) => {}
        Err(problem) => {
            tracing::debug!(
                program = %started.display(),
                problem = %problem,
                "no thread to wait for the program run on completion on",
            );
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    let child = Arc::clone(&child);
                    handle.spawn_blocking(move || log_exit(&child, &started));
                }
                // Not inside a runtime either, and blocking here would hold up the caller that started
                // the program: leave it unwaited for, which is what leaving it un-killed already means.
                Err(_) => {
                    drop(child);
                }
            }
        }
    }
}

/// The one log line a completion program earns: how it ended, or why that could not be found out.
fn log_exit(child: &Arc<Mutex<Child>>, program: &Path) {
    let waited = child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .wait();
    match waited {
        Ok(status) => tracing::debug!(
            program = %program.display(),
            status = ?status,
            "the program run on completion exited",
        ),
        Err(problem) => tracing::debug!(
            program = %program.display(),
            problem = %problem,
            "the program run on completion could not be waited for",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{Refused, completion_args, refusal};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    /// An absolute path to a program, in the spelling of the host running this test: the batch rule is
    /// about the file's type, so the folder it is in must not be the thing that differs.
    fn program(named: &str) -> PathBuf {
        let dir = if cfg!(windows) {
            Path::new(r"C:\srv\hooks")
        } else {
            Path::new("/srv/hooks")
        };
        dir.join(named)
    }

    #[test]
    fn completion_args_are_two_separate_arguments() {
        let name = "a b; $(rm -rf ~)";
        let folder = "/d l";
        let args = completion_args(name, Path::new(folder));

        assert_eq!(
            args,
            [OsString::from(name), OsString::from(folder)],
            "the name and the folder, each exactly as it was"
        );
        assert_eq!(args.len(), 2, "two arguments, never one shell string");
        assert_eq!(
            args[0],
            OsString::from("a b; $(rm -rf ~)"),
            "the first is the torrent's name, with the shell in it left alone"
        );
        assert_eq!(
            args[1],
            OsString::from("/d l"),
            "the second is the folder it finished in, with its space left alone"
        );
    }

    /// A relative name means "below wherever this process was started", which on a server is a directory
    /// nobody chose, and a bare name means `PATH`, which on a service is not the `PATH` the person writing
    /// the line was using. Both are refused before anything is started, on every host.
    #[test]
    fn a_program_named_by_a_relative_path_is_refused() {
        for name in ["hooks/notify.sh", "./notify.sh", "notify.sh"] {
            let named = Path::new(name);
            for windows in [false, true] {
                assert_eq!(
                    refusal(named, windows).map(|refused| refused.to_string()),
                    Some(Refused::NotAbsolute.to_string()),
                    "`{name}` is not a path from the root of the filesystem on any host, \
                     whichever platform is being answered for",
                );
            }
        }
        assert_eq!(
            refusal(&program("notify.sh"), false),
            None,
            "a whole path to an ordinary program is a program to start"
        );
    }

    /// The batch rule is a rule about what Windows does with the file, so it is asked and answered here
    /// for both platforms instead of being compiled down to one: a `.bat` is refused for Windows and run
    /// everywhere else, and a relative batch file is refused for its path first.
    #[test]
    fn a_batch_file_is_refused_for_windows_and_run_everywhere_else() {
        for name in ["notify.bat", "NIGHTLY.CMD", "notify.bat."] {
            let named = program(name);
            assert!(
                named.is_absolute(),
                "this test is about the extension, so the path must not be the problem: {named:?}"
            );
            assert_eq!(
                refusal(&named, true).map(|refused| refused.to_string()),
                Some(Refused::BatchFile.to_string()),
                "`{name}` is a script for `cmd.exe` on Windows",
            );
            assert_eq!(
                refusal(&named, false),
                None,
                "and an executable file like any other everywhere else, which is why this is not \
                     a rule about the file but about the platform",
            );
        }
        assert_eq!(
            refusal(Path::new("hooks/notify.bat"), true),
            Some(Refused::NotAbsolute),
            "a batch file with no path in it is refused for the path, which is the nearer mistake",
        );
    }

    /// The three lines are what the notice is built from, so they are stated once and checked once, here
    /// rather than from a test that has to start a process to read one.
    #[test]
    fn each_refusal_names_what_is_wrong_with_the_program() {
        assert_eq!(
            Refused::NotAbsolute.to_string(),
            "is not an absolute path: name the program by the whole path to it"
        );
        assert_eq!(
            Refused::BatchFile.to_string(),
            "is a Windows batch file, which Windows starts through a shell: name a program to start \
             directly"
        );
        assert_eq!(
            Refused::WouldNotStart {
                reason: "No such file or directory (os error 2)".to_owned(),
            }
            .to_string(),
            "would not start: No such file or directory (os error 2)",
            "the operating system's own reason, which is the thing a person can act on",
        );
    }
}
