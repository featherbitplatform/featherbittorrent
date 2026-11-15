use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fbtt_core::LibraryLock;

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

/// When the terminal cannot be set up, the binary must say so and leave - not wait on a
/// backend it never told to stop, which is the bug this test exists for. A run with no
/// tty on any stream is how that happens in CI. `listen_port: 0` goes in the config
/// first so the engine takes a port the OS hands out rather than one another test run
/// is holding. Where this process has a controlling terminal, crossterm reaches it
/// through `/dev/tty` whatever our stdios are and setup cannot fail at all; there the
/// test says that plainly instead of pretending to have tested the failure.
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
    let err_path = scratch
        .config_dir
        .parent()
        .expect("a scratch root")
        .join("stderr.txt");

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

    // `wait` blocks, so the deadline lives in a second thread and the main one times out
    // on a channel instead.
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait()));
    let status = match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => panic!("waiting for the binary failed: {error}"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the waiter thread died"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // std cannot signal a child it has moved away: ask by number, then reap.
            let _ = Command::new("/bin/kill").arg(pid.to_string()).status();
            let _ = rx.recv();
            panic!(
                "the binary is still running after 20 seconds - it waited on a backend \
                 it never told to stop"
            );
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
}

/// A scratch pair of directories, removed when the test that made it is done. The
/// workspace takes no `tempfile` dependency, so tests make their own scratch space.
struct Scratch {
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
        match fs::remove_dir_all(self.config_dir.parent().unwrap()) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            other => other.unwrap(),
        }
    }
}
