use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
