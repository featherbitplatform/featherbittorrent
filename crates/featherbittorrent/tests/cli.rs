use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use fbtt_core::{Config, LibraryLock};

#[test]
fn version_prints_name_and_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_featherbittorrent"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        format!("featherbittorrent {}", env!("CARGO_PKG_VERSION"))
    );
}

/// A second featherbittorrent on a locked library answers with its own sentence and
/// exit code 3, before the terminal is touched. The lock is held here, in this process:
/// a second handle conflicts exactly like a second process does, which is what the
/// lock's own tests prove, so no child has to hold it for us.
#[test]
fn lock_held_message() {
    let scratch = Scratch::new("lock_held");
    // The guard outlives the child: the binary must find the lock held while it runs.
    let _held = LibraryLock::acquire(&scratch.data_dir.join("library.lock")).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_featherbittorrent"))
        .env("FBTT_CONFIG_DIR", &scratch.config_dir)
        .env("FBTT_DATA_DIR", &scratch.data_dir)
        // Piped, so the run sees no TTY on either stream and still answers cleanly.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(3), "a held lock is exit code 3");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("already using this library"),
        "stderr says who holds the library: {stderr}"
    );
    assert!(
        stderr.contains(&scratch.data_dir.display().to_string()),
        "the message names the data directory the lock lives in: {stderr}"
    );
}

/// The same locked library, found by a second run whose config came out of its own settings save. This
/// is the case `[server]` presence decides end to end: a config the TUI wrote says nothing about serving,
/// so this run has no address to offer and leaves with the same sentence and exit code as a run that
/// found no file at all - rather than offering to connect to a server nobody started. The decision
/// itself is tested in `backend::tests::second_tui_on_locked_library_exits_3`; this is the whole run.
#[test]
fn settings_saved_library_offers_no_server_when_locked() {
    let scratch = Scratch::new("locked_after_settings_save");
    // What a settings change writes: rates and a folder, and no `server` section.
    let config_path = scratch.config_dir.join("config.yaml");
    let mut saved = Config::default();
    saved.limits.download_kib = 512;
    saved.save(&config_path).expect("save a settings config");
    let written = fs::read_to_string(&config_path).expect("read the file back");
    assert!(
        !written.contains("server"),
        "a settings save wrote no server section: {written}"
    );

    // The guard outlives the child, as in `lock_held_message`.
    let _held = LibraryLock::acquire(&scratch.data_dir.join("library.lock")).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_featherbittorrent"))
        .env("FBTT_CONFIG_DIR", &scratch.config_dir)
        .env("FBTT_DATA_DIR", &scratch.data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(3),
        "a library somebody else is driving is exit code 3: {out:?}"
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("already using this library"),
        "the run says who holds the library: {stderr}"
    );
    // And it offers no address: nothing in this file says a server runs here.
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        !stdout.contains("127.0.0.1"),
        "no server was invented for this library: {stdout}"
    );
}

/// When the terminal cannot be set up, the binary must say so and leave - without ever starting
/// the engine, which is the bug this test exists for. A run with no tty on any stream is how that
/// happens in CI. `listen_port: 0` goes in the config first so that an engine which did start
/// would take a port the OS hands out rather than one another test run is holding. Where this
/// process has a controlling terminal, crossterm reaches it through `/dev/tty` whatever our
/// stdios are and setup cannot fail at all; there the test says that plainly instead of pretending
/// to have tested the failure.
#[test]
fn terminal_setup_failure_exits() {
    // Same session, so what this process finds is what the child will find.
    #[cfg(unix)]
    let tty_reachable = fs::File::open("/dev/tty").is_ok();
    #[cfg(not(unix))]
    let tty_reachable = true;
    if tty_reachable {
        eprintln!(
            "terminal_setup_failure_exits: skipped - a terminal is reachable, so setup \
             cannot fail here"
        );
        return;
    }

    let scratch = Scratch::new("terminal_failure");
    fs::write(scratch.config_dir.join("config.yaml"), "listen_port: 0\n")
        .expect("a config on a port the OS picks");
    // stderr goes to a file, not a pipe: nothing to drain, and no way for a full pipe
    // buffer to look like a hung binary.
    let err_path = scratch.root.join("stderr.txt");

    let mut child = Command::new(env!("CARGO_BIN_EXE_featherbittorrent"))
        .env("FBTT_CONFIG_DIR", &scratch.config_dir)
        .env("FBTT_DATA_DIR", &scratch.data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            File::create(&err_path).expect("a place for stderr"),
        ))
        .spawn()
        .expect("the binary starts even with no terminal to show");

    // `try_wait` does not block, so the deadline can live right here: a run that will not
    // end is killed and failed, not left hanging CI on a blocking `wait`.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // SIGKILL on Unix, TerminateProcess on Windows; then reap the child so
                    // it is gone before the scratch directories are removed.
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "the binary is still running after 20 seconds - it waited on a \
                         backend it never told to stop"
                    );
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("waiting for the binary failed: {error}"),
        }
    };

    let stderr = fs::read_to_string(&err_path).unwrap_or_default();
    assert!(
        !status.success(),
        "a run that never got a terminal is not a success: {stderr}"
    );
    assert!(
        stderr.contains("Could not set up the terminal"),
        "the binary says what stopped it: {stderr}"
    );

    // The engine never ran, which `session.state` proves. The data directory exists - taking the
    // lock makes it, and that comes before the terminal - but only a run that drove a session
    // writes that file into it, and this one had no screen to run an engine for.
    let left_behind: Vec<_> = fs::read_dir(&scratch.data_dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !left_behind.iter().any(|name| name == "session.state"),
        "a run with no terminal started the engine anyway: {left_behind:?}"
    );
}

/// The hidden `generate-assets` subcommand writes what the packages install: man pages and
/// completions built from the same `Command` that prints `--help`, so they cannot drift from the
/// flags. This is the whole command on a scratch directory, seen the way the packaging scripts see
/// it. The binary under test is a debug build, which means it also has the two hidden `serve`
/// flags; nothing hidden may show up in a file somebody installs, because a release build does not
/// accept those flags at all.
#[test]
fn generate_assets_writes_man_and_completions() {
    let scratch = Scratch::new("generate_assets");

    let out = Command::new(env!("CARGO_BIN_EXE_featherbittorrent"))
        .arg("generate-assets")
        .arg(&scratch.root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        out.status.success(),
        "assets written into a writable directory is exit 0: {stderr}"
    );

    // One page for the command and one for each subcommand, named by joining the command path
    // with `-`, plus the four shells the packaging scripts copy out of this directory.
    const SHIPPED: [&str; 8] = [
        "man/featherbittorrent.1",
        "man/featherbittorrent-serve.1",
        "man/featherbittorrent-user-set.1",
        "man/featherbittorrent-device-add.1",
        "completions/featherbittorrent.bash",
        "completions/_featherbittorrent",
        "completions/featherbittorrent.fish",
        "completions/_featherbittorrent.ps1",
    ];
    for shipped in SHIPPED {
        let path = scratch.root.join(shipped);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|problem| panic!("{} was not written: {problem}", path.display()));
        assert!(!text.trim().is_empty(), "{} came out empty", path.display());
        // The command prints every path it wrote, which is how the packaging scripts know what
        // to gzip and copy.
        assert!(
            stdout.contains(&path.display().to_string()),
            "stdout lists every file written, and not this one: {stdout}"
        );
    }

    // A man page that `man` can render, and a top page that says the subcommands exist.
    let top_page = fs::read_to_string(scratch.root.join("man/featherbittorrent.1")).unwrap();
    assert!(
        top_page.contains(".TH"),
        "the page carries the roff macros man reads: {top_page}"
    );
    assert!(
        top_page.contains("serve"),
        "the top page names the subcommands: {top_page}"
    );

    // A completion script that knows the subcommands, which is what a person typing expects.
    let bash = fs::read_to_string(scratch.root.join("completions/featherbittorrent.bash")).unwrap();
    assert!(
        bash.contains("serve") && bash.contains("device"),
        "the bash completion knows the subcommands: {bash}"
    );

    // The hidden subcommand and the debug-only `serve` flags belong to this build only, so no
    // shipped file may mention them.
    for dir in ["man", "completions"] {
        let entries = fs::read_dir(scratch.root.join(dir))
            .unwrap_or_else(|problem| panic!("{} is missing: {problem}", dir));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            let text = fs::read_to_string(&path).expect("a generated file reads as text");
            for hidden in ["generate-assets", "stop-file", "offline"] {
                assert!(
                    !text.contains(hidden),
                    "{} mentions the hidden `{hidden}`, which a release build does not have: {}",
                    path.display(),
                    // The line that got it wrong, so the failure says what to filter.
                    text.lines()
                        .find(|line| line.contains(hidden))
                        .unwrap_or("")
                        .trim()
                );
            }
        }
    }
}

/// An unwritable directory is a problem like any other: one line on stderr and exit 1, not a panic
/// and not clap's usage error. The scratch root is made, then taken away, so the path exists as a
/// file where no directory can be written.
#[test]
fn generate_assets_reports_an_unwritable_directory() {
    let scratch = Scratch::new("generate_assets_unwritable");
    let blocked = scratch.root.join("blocked");
    fs::write(&blocked, "not a directory\n").expect("a file where the assets were asked for");

    let out = Command::new(env!("CARGO_BIN_EXE_featherbittorrent"))
        .arg("generate-assets")
        .arg(&blocked)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(1),
        "a directory that cannot be written is exit 1: {:?}",
        String::from_utf8(out.stderr).unwrap()
    );
}

/// `generate-assets` exists for the packaging scripts, not for a person at a prompt, so it is not in
/// `--help`, and completions built from the same command do not offer it.
#[test]
fn generate_assets_is_hidden() {
    let out = Command::new(env!("CARGO_BIN_EXE_featherbittorrent"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        !stdout.contains("generate-assets"),
        "the help a person reads does not list the packaging subcommand: {stdout}"
    );
}

/// The man page and the terminal see the same text, so what a person needs to know about where the
/// library lives, what the environment variables do, and what the exit codes mean is in `--help`.
#[test]
fn help_names_files_and_exit_codes() {
    let out = Command::new(env!("CARGO_BIN_EXE_featherbittorrent"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    for expected in ["FBTT_CONFIG_DIR", "config.yaml", "EXIT STATUS"] {
        assert!(
            stdout.contains(expected),
            "--help says {expected}, and did not: {stdout}"
        );
    }
}

/// A scratch set of directories, removed when the test that made it is done. The
/// workspace takes no `tempfile` dependency, so tests make their own scratch space.
struct Scratch {
    /// The directory the config and data directories are made inside, which is also where a test
    /// that needs somewhere to write files puts them.
    root: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "featherbittorrent-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0),
        ));
        let scratch = Scratch {
            root: dir.clone(),
            config_dir: dir.join("config"),
            data_dir: dir.join("data"),
        };
        fs::create_dir_all(&scratch.config_dir).unwrap();
        scratch
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort, and a directory already gone is not a failure.
        match fs::remove_dir_all(&self.root) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            other => other.unwrap(),
        }
    }
}
