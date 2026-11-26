//! A media player for the tests to start instead of a real one: it writes down what it was handed,
//! holds on if asked to, and exits. No window, no codec, nothing over the network.
//!
//! `fbtt-fake-player [--log <file>] [--hold-ms <n>] <media...>` appends one JSON line to `<file>`: the
//! arguments this process was given, and - when the last of them is an `.m3u` file - what was in it.
//! Those are the two questions a play test has to ask about a launch: what reached the process, where
//! another user could read it, and what reached it only through the private file.
//!
//! With no `--log` it is standing in for the program a finished torrent runs, which is handed the
//! torrent's name and its folder and nothing else: the line then goes to `.fbtt-fake-player.log` in the
//! folder it was run for, which is the only place a completion test can look without the program being
//! told where to write.

// Its stderr is for whoever runs it by hand: these tests close it.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

/// Where the record goes when nobody named a file: beside the folder this was run for.
const RECORD: &str = ".fbtt-fake-player.log";

fn main() -> ExitCode {
    // `args_os` rather than `args`: a scratch path on a Unix box need not be valid UTF-8, and a panic
    // here would read as a play that went wrong rather than as a broken stand-in. What is recorded is
    // the lossy text, which is enough to search in.
    let given: Vec<String> = env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let mut log: Option<String> = None;
    let mut hold_ms: u64 = 0;
    // Where the media starts: everything after the first argument that is not one of ours.
    let mut media = 0;
    while media < given.len() {
        match given[media].as_str() {
            "--log" if media + 1 < given.len() => {
                log = Some(given[media + 1].clone());
                media += 2;
            }
            "--hold-ms" if media + 1 < given.len() => {
                hold_ms = given[media + 1].parse().unwrap_or(0);
                media += 2;
            }
            _ => break,
        }
    }

    // Nothing named to write to: this is the completion program standing in for a real one, so the
    // record goes beside the folder it was run for. A last argument that is not a folder is a wrong
    // command line, not a run with no log to keep.
    let log = match log {
        Some(log) => log,
        None => match given.last() {
            Some(folder) if Path::new(folder).is_dir() => Path::new(folder)
                .join(RECORD)
                .to_string_lossy()
                .into_owned(),
            _ => {
                eprintln!(
                    "usage: fbtt-fake-player [--log <file>] [--hold-ms <n>] <media...> \
                     (with no --log, the last argument must be a folder)"
                );
                return ExitCode::from(2);
            }
        },
    };

    if let Err(problem) = append(&log, &record(&given, playlist_contents(&given[media..]))) {
        eprintln!("could not write {log}: {problem}");
        return ExitCode::FAILURE;
    }

    // Held open so a test can look at the state while this player is still alive.
    if hold_ms > 0 {
        std::thread::sleep(Duration::from_millis(hold_ms));
    }
    ExitCode::SUCCESS
}

/// One JSON line: every argument this process was given, and what was in the last one when that was a
/// playlist file (`null` when it was not).
fn record(given: &[String], playlist: Option<String>) -> String {
    let args = given
        .iter()
        .map(|arg| json_string(arg))
        .collect::<Vec<String>>()
        .join(", ");
    let playlist = match playlist {
        Some(contents) => json_string(&contents),
        None => "null".to_string(),
    };
    format!("{{ \"args\": [{args}], \"playlist\": {playlist} }}\n")
}

/// The contents of the last argument, when that is a playlist file: what the player read.
fn playlist_contents(media: &[String]) -> Option<String> {
    let last = media.last()?;
    let is_playlist = Path::new(last)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u"));
    // Unreadable counts as nothing to report: the test that wanted those contents says so itself.
    is_playlist.then(|| fs::read_to_string(last).ok()).flatten()
}

/// Appends one line, creating the file when there is none yet.
fn append(log: &str, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(log)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// `s` as a JSON string: quotes and backslashes escaped, and every control character spelled out so
/// one argument is still one line of the log.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
