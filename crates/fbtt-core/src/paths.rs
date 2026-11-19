//! Where one featherbittorrent library lives: `config.yaml` and `ui.yaml` in the platform's
//! config directory, the session state and the library lock in its data
//! directory.
//!
//! The debug variables `FBTT_CONFIG_DIR` and `FBTT_DATA_DIR` each replace one
//! of the two directories, which is how a scratch library runs beside the real
//! one (see `CLAUDE.md`). Nothing here creates a directory: [`Config::save`]
//! and the engine make room in whatever they write into.
//!
//! [`Config::save`]: crate::Config::save

use std::ffi::OsString;
use std::path::PathBuf;

use directories::ProjectDirs;

use crate::config::ConfigError;

/// The identity the platform's conventions key on: the reverse domain
/// `io.featherbit` plus the application name, which becomes
/// `~/.config/io.featherbit.featherbittorrent` on Linux and the matching
/// bundle or AppData folder elsewhere.
const PROJECT: (&str, &str, &str) = ("io", "featherbit", "featherbittorrent");

/// The config file, in [`Paths::config_dir`].
const CONFIG_FILE_NAME: &str = "config.yaml";
/// Which backend the last run used, in [`Paths::config_dir`] (see [`crate::ui_state`]).
const UI_FILE_NAME: &str = "ui.yaml";
/// The engine's resume state, in [`Paths::data_dir`].
const STATE_FILE_NAME: &str = "session.state";
/// The lock that keeps a second featherbittorrent off the same library, in
/// [`Paths::data_dir`].
const LOCK_FILE_NAME: &str = "library.lock";
/// Where a playlist for a media player is written, in [`Paths::data_dir`] (see
/// [`crate::player`]).
const PLAY_DIR_NAME: &str = "play";

/// The two directories one library uses.
///
/// Make one with [`Paths::resolve`]. On a fresh host neither directory exists
/// yet, and that is fine: the paths are where files *will* be, and whoever
/// writes creates the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Where `config.yaml` is read and written.
    pub config_dir: PathBuf,
    /// Where `session.state` and `library.lock` live.
    pub data_dir: PathBuf,
}

impl Paths {
    /// Reads the two directories from the environment and the platform.
    ///
    /// `FBTT_CONFIG_DIR` and `FBTT_DATA_DIR` win; what they leave out comes
    /// from [`ProjectDirs::from("io", "featherbit", "featherbittorrent")`](ProjectDirs::from).
    /// A host with no home directory and no debug variable is
    /// [`ConfigError::NoHome`]; setting both variables works even there.
    pub fn resolve() -> Result<Paths, ConfigError> {
        Self::resolve_with(|key| std::env::var_os(key))
    }

    /// [`resolve`](Self::resolve) with the environment read through `look_up`
    /// rather than [`std::env::var_os`], so a test can hand in values instead
    /// of setting process-global variables that would race with other tests.
    ///
    /// An empty value counts as unset: an `FBTT_DATA_DIR=` left empty by a
    /// script is a typo, and it should not send the library into whatever
    /// directory the app happens to run in.
    pub fn resolve_with<F>(look_up: F) -> Result<Paths, ConfigError>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        let from_env = |key| {
            look_up(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        let config_dir = from_env("FBTT_CONFIG_DIR");
        let data_dir = from_env("FBTT_DATA_DIR");
        if let (Some(config_dir), Some(data_dir)) = (&config_dir, &data_dir) {
            // Both directories are given, so no home directory is needed.
            return Ok(Paths {
                config_dir: config_dir.clone(),
                data_dir: data_dir.clone(),
            });
        }
        let project =
            ProjectDirs::from(PROJECT.0, PROJECT.1, PROJECT.2).ok_or(ConfigError::NoHome)?;
        Ok(Paths {
            config_dir: config_dir.unwrap_or_else(|| project.config_dir().to_path_buf()),
            data_dir: data_dir.unwrap_or_else(|| project.data_dir().to_path_buf()),
        })
    }

    /// The config file: `<config_dir>/config.yaml`.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE_NAME)
    }

    /// Which backend the last run used: `<config_dir>/ui.yaml` (see [`crate::ui_state`]).
    pub fn ui_file(&self) -> PathBuf {
        self.config_dir.join(UI_FILE_NAME)
    }

    /// The engine's resume state: `<data_dir>/session.state`.
    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join(STATE_FILE_NAME)
    }

    /// The lock that keeps a second featherbittorrent off the same library:
    /// `<data_dir>/library.lock`.
    pub fn lock_file(&self) -> PathBuf {
        self.data_dir.join(LOCK_FILE_NAME)
    }

    /// Where a playlist handed to a media player is written: `<data_dir>/play` (see
    /// [`crate::player`]).
    ///
    /// Nothing here creates it, like nothing else here: the one that writes a playlist makes room
    /// for it, private to this user.
    pub fn play_dir(&self) -> PathBuf {
        self.data_dir.join(PLAY_DIR_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::{CONFIG_FILE_NAME, LOCK_FILE_NAME, PLAY_DIR_NAME, PROJECT, Paths, STATE_FILE_NAME};
    use crate::config::ConfigError;
    use crate::scratch::ScratchDir;
    use directories::ProjectDirs;
    use std::ffi::OsString;

    /// The debug variables win over the platform's directories, so a scratch
    /// library can run anywhere without touching the real one.
    #[test]
    fn env_overrides_paths() {
        let dir = ScratchDir::new("paths_env");
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let look_up = |key: &str| -> Option<OsString> {
            match key {
                "FBTT_CONFIG_DIR" => Some(config_dir.clone().into()),
                "FBTT_DATA_DIR" => Some(data_dir.clone().into()),
                _ => None,
            }
        };

        let paths = Paths::resolve_with(look_up).expect("the env gives both directories");
        assert_eq!(paths.config_dir, config_dir);
        assert_eq!(paths.data_dir, data_dir);
        assert_eq!(paths.config_file(), config_dir.join(CONFIG_FILE_NAME));
        assert_eq!(paths.state_file(), data_dir.join(STATE_FILE_NAME));
        assert_eq!(paths.lock_file(), data_dir.join(LOCK_FILE_NAME));
        assert_eq!(paths.play_dir(), data_dir.join(PLAY_DIR_NAME));
    }

    /// Without the debug variables the platform's own directories are used, so
    /// a normal run lands in `~/.config` and `~/.local/share` on Linux, or in
    /// their macOS and Windows equivalents.
    #[test]
    fn without_env_the_project_dirs_are_used() {
        let Some(project) = ProjectDirs::from(PROJECT.0, PROJECT.1, PROJECT.2) else {
            // A host with no home directory has nothing to fall back to, and
            // says so instead of inventing a path.
            assert!(matches!(
                Paths::resolve_with(|_| None),
                Err(ConfigError::NoHome)
            ));
            return;
        };
        let paths = Paths::resolve_with(|_| None).expect("the platform gives both directories");
        assert_eq!(paths.config_dir, project.config_dir());
        assert_eq!(paths.data_dir, project.data_dir());
    }

    /// An empty debug variable is a mistake, not a directory: it does not turn
    /// the library into files in whatever directory the app was started in.
    #[test]
    fn an_empty_env_value_is_no_value() {
        let Some(project) = ProjectDirs::from(PROJECT.0, PROJECT.1, PROJECT.2) else {
            return; // No home to fall back to; the test above covers that host.
        };
        let paths = Paths::resolve_with(|key| {
            if key == "FBTT_CONFIG_DIR" {
                Some(OsString::new())
            } else {
                None
            }
        })
        .expect("the platform fills in what the empty value left out");
        assert_eq!(paths.config_dir, project.config_dir());
        assert_eq!(paths.data_dir, project.data_dir());
    }
}
