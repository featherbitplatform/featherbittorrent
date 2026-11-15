//! Which backend this library's runs use: `ui.yaml`, next to `config.yaml`.
//!
//! This is the only thing featherbittorrent remembers about how it was last used: the local engine,
//! or one server. It is a convenience - starting on the local engine when the file is missing,
//! broken or unreadable is always a usable answer - so nothing here is fatal and no secret lives
//! here either: a password belongs in the system keychain, a private key stays in its own file, and
//! what this file holds is an address, a method, a user name and a path.
//!
//! The file names its backend the way serde writes an enum: `last_used: Local`, or `last_used:`
//! followed by a YAML tag and the target's fields under it - `last_used: !Remote` with `url`,
//! `login`, `user` and `key_path` beneath. Anyone editing it by hand copies what a save wrote.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::config::{ConfigError, replace_file, suffixed};
use crate::paths::Paths;

/// A `ui.yaml` that does not parse becomes its own file name plus this, the convention
/// `config.yaml` follows: never overwrite what could not be read.
const BAD_FILE_SUFFIX: &str = ".bad";

/// What the last run used, and so what the next one starts on.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// The backend to start on.
    pub last_used: LastUsed,
}

/// Which backend a run starts on.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LastUsed {
    /// The engine in this process: the library on this machine, driven directly.
    #[default]
    Local,
    /// A server over the network, at the address given here.
    Remote(RemoteTarget),
}

/// Where to reach a server and how to sign in there.
///
/// Nothing here is a secret: a password lives in the system keychain under its user name and this
/// URL, and a device key is used from its file or through ssh-agent, never copied in here.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteTarget {
    /// The server's address with its scheme: `https://bit.example` or `http://127.0.0.1:8090`.
    pub url: String,
    /// How to sign in there.
    pub login: LoginMethod,
    /// The user name a password login uses, when the method needs one.
    pub user: Option<String>,
    /// The device key to sign with, or to look for in ssh-agent, when the method needs one.
    pub key_path: Option<PathBuf>,
}

/// How a target signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LoginMethod {
    /// Nothing: this server answers without a login (`server.auth.enabled: false`).
    #[default]
    None,
    /// A user name and password, from the keychain or typed in at the connect screen.
    Password,
    /// An OpenSSH private key read from `key_path`, decrypted if it is encrypted.
    KeyFile,
    /// The same key signed by ssh-agent, so its bytes are never read.
    Agent,
}

impl UiState {
    /// Reads this library's `ui.yaml`.
    ///
    /// Everything that is not the file going well answers [`UiState::default`]: the local engine.
    /// Which backend a run started on is a convenience, and a person's torrents are worth opening
    /// even when it was written by something this run cannot make sense of. Bytes that do not parse
    /// are kept aside as `ui.yaml.bad` first, because the next save writes a file under this name
    /// and must not erase what could not be read; the same goes for bytes that parse into something
    /// no run could act on, a server target with no address to reach. Bytes this run cannot even
    /// open are left exactly where they are, since they cannot be moved either.
    pub fn load(paths: &Paths) -> UiState {
        let path = paths.ui_file();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            // No file yet - a library nobody has connected anywhere from - or one this run cannot
            // read at all: the local engine either way, and nothing is written over it.
            Err(_) => return UiState::default(),
        };
        match serde_norway::from_slice::<UiState>(&bytes) {
            Ok(state) if state.usable() => state,
            // Not YAML, or a target this run could not connect to: the local engine, and the bytes
            // kept under another name for whoever can make something of them.
            _ => {
                let aside = suffixed(&path, BAD_FILE_SUFFIX);
                let _ = std::fs::rename(&path, aside);
                UiState::default()
            }
        }
    }

    /// Whether what was read says something a run could act on. A `Remote` target with no URL is not
    /// a connection - the half of a dialog someone left before typing the address - and starting on
    /// it would leave the run looking connected to nowhere.
    fn usable(&self) -> bool {
        !matches!(&self.last_used, LastUsed::Remote(target) if target.url.trim().is_empty())
    }

    /// Writes this state to `ui.yaml`, making its directory first if it is not there yet.
    ///
    /// The bytes go into a temp file beside the target, are synced, and only then renamed over it -
    /// the way every featherbittorrent file is written - so a crash leaves either the last
    /// connection this run started on or the one it ended on, never half of each.
    pub fn save(&self, paths: &Paths) -> Result<(), ConfigError> {
        let path = paths.ui_file();
        let cannot_write = |source| ConfigError::Write {
            path: path.clone(),
            source,
        };
        let text = serde_norway::to_string(self).map_err(|error| {
            std::io::Error::other(format!(
                "the last connection cannot be written as YAML: {error}"
            ))
        });
        let text = match text {
            Ok(text) => text,
            Err(error) => return Err(cannot_write(error)),
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(cannot_write)?;
        }
        replace_file(&path, text.as_bytes()).map_err(cannot_write)
    }
}

#[cfg(test)]
mod tests {
    use super::{LastUsed, LoginMethod, RemoteTarget, UiState};
    use crate::paths::Paths;
    use crate::scratch::ScratchDir;
    use std::fs;
    use std::path::PathBuf;

    /// The config directory is the only half of a library `ui.yaml` cares about.
    fn library(dir: &ScratchDir) -> Paths {
        Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        }
    }

    /// What one run ended on is what the next run starts on, and a library nobody has connected
    /// anywhere from starts on its own engine.
    #[test]
    fn ui_state_round_trip() {
        let dir = ScratchDir::new("ui_state_round_trip");
        let paths = library(&dir);
        assert_eq!(
            UiState::load(&paths),
            UiState::default(),
            "no file yet means the local engine"
        );

        let remote = UiState {
            last_used: LastUsed::Remote(RemoteTarget {
                url: "https://bit.example".to_string(),
                login: LoginMethod::KeyFile,
                user: Some("alice".to_string()),
                key_path: Some(PathBuf::from("/home/alice/.ssh/id_ed25519")),
            }),
        };
        remote.save(&paths).expect("save the last connection");
        assert_eq!(
            UiState::load(&paths),
            remote,
            "the file says what the run ended on"
        );

        // Switching back to the local engine replaces it, and nothing of the save is left behind.
        UiState::default()
            .save(&paths)
            .expect("save the local choice");
        assert_eq!(UiState::load(&paths), UiState::default());
        let leftovers: Vec<_> = fs::read_dir(paths.config_dir.parent().expect("a scratch root"))
            .expect("read the directory")
            .flatten()
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a temp file survived the save: {leftovers:?}"
        );

        // A target that names no user and no key file is a login with nothing in it.
        let open_server = UiState {
            last_used: LastUsed::Remote(RemoteTarget {
                url: "http://127.0.0.1:8090".to_string(),
                login: LoginMethod::None,
                user: None,
                key_path: None,
            }),
        };
        open_server.save(&paths).expect("save the open server");
        assert_eq!(UiState::load(&paths), open_server);
    }

    /// A `ui.yaml` this run cannot read as YAML does not stop anything: the last connection is a
    /// convenience, so the answer is the local engine and the bytes are kept aside rather than
    /// written over by the next save.
    #[test]
    fn bad_ui_state_is_local() {
        let dir = ScratchDir::new("ui_state_bad");
        let paths = library(&dir);
        fs::create_dir_all(&paths.config_dir).expect("the config directory");
        fs::write(paths.ui_file(), "last_used: [unterminated\n")
            .expect("write a ui file that is not YAML");

        assert_eq!(
            UiState::load(&paths),
            UiState::default(),
            "an unreadable last connection is the local engine"
        );
        assert!(
            !paths.ui_file().exists(),
            "what could not be read moves out of the way instead of being overwritten"
        );
        assert!(
            paths.config_dir.join("ui.yaml.bad").exists(),
            "and it stays readable under another name"
        );

        // Something that is not a file at all where `ui.yaml` belongs is the same answer.
        fs::create_dir(paths.ui_file()).expect("a directory where the ui file belongs");
        assert_eq!(UiState::load(&paths), UiState::default());
    }

    /// A `Remote` target with no address is not a connection anyone could make, so it reads like a
    /// file this run cannot use: the local engine, and the bytes kept aside.
    #[test]
    fn remote_without_url_is_local() {
        let dir = ScratchDir::new("ui_state_no_url");
        let paths = library(&dir);
        fs::create_dir_all(&paths.config_dir).expect("the config directory");
        fs::write(
            paths.ui_file(),
            "last_used: !Remote\n  url: \"\"\n  login: Password\n",
        )
        .expect("write a remote target with no address");

        assert_eq!(
            UiState::load(&paths),
            UiState::default(),
            "a target with no address is no server"
        );
        assert!(
            paths.config_dir.join("ui.yaml.bad").exists(),
            "and what could not be used stays readable under another name"
        );
    }
}
