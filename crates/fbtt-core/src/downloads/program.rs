//! The program a finished torrent starts, and the two arguments it always gets.
//!
//! The person names a program - in `config.yaml`, or from the TUI or the desktop app on a library they
//! run themselves - and featherbit torrent starts it with the torrent's name and the folder its files
//! finished in. Two separate arguments through [`std::process::Command`], never a shell: a name holding
//! a space, a quote or `$(...)` is one argument to the program, not a command to run.
//!
//! It is started and left alone. It is not waited for (a program that plays something for an hour would
//! hold the automation), and it is not killed when this program forgets it (nobody wants quitting the
//! app to stop a film). The only thing anybody here needs to know is whether it *started*: that is the
//! caller's notice. Its exit status goes in the log, and the wait for it happens on a plain thread,
//! where blocking for as long as the program runs costs a thread and nothing else.

use std::ffi::OsString;
// The platform-side ways to start a program quietly: no process group this app's Ctrl-C reaches, and
// on Windows no console window behind it.
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// The Windows creation flag that keeps a program from being handed a console window of its own behind
/// the one this app may be holding: it brings its own, if it has one.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The two arguments a completion program is given: the torrent's name, then the folder its files
/// finished in.
///
/// Two values, not one line of shell: nothing here quotes, splits or expands anything, because nothing
/// here runs a shell.
pub(crate) fn completion_args(name: &str, folder: &Path) -> [OsString; 2] {
    [name.into(), folder.as_os_str().to_os_string()]
}

/// Starts `program` for a torrent that finished, and lets go of it.
///
/// `Err` is a program that would not start at all - no file there, not an executable, a folder in the
/// way - which is the only thing about it anybody outside the log is told. Once it is running its exit
/// status is the log's business: it is waited for on its own thread so the caller never blocks on it.
///
/// [`std::process::Command`] rather than [`tokio::process::Command`] (which is what
/// [`crate::player`] uses to start a media player): there is nothing to tidy up after this one, so the
/// wait needs no runtime, and a plain thread is the cheapest place to put one.
pub(crate) fn run(program: &Path, name: &str, folder: &Path) -> Result<(), std::io::Error> {
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

    let mut child = command.spawn()?;
    let started = program.to_path_buf();
    std::thread::spawn(move || match child.wait() {
        Ok(status) => tracing::debug!(
            program = %started.display(),
            status = ?status,
            "the program run on completion exited",
        ),
        Err(problem) => tracing::debug!(
            program = %started.display(),
            problem = %problem,
            "the program run on completion could not be waited for",
        ),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::completion_args;
    use std::ffi::OsString;
    use std::path::Path;

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
}
