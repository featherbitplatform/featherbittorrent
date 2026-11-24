//! Where the desktop app's own files live: `desktop.yaml` in the app's config directory, and the
//! library This computer runs its engine on in its data directory.
//!
//! The desktop app keeps its own library, in directories of its own, beside but never shared with the
//! terminal client's ([`fbtt_core::Paths`]): two programs writing one library is how one clobbers the
//! other's download folder, and the design spec's "This computer is the app's own engine" needs the
//! same separation the library lock enforces inside one directory. `FBTT_DESKTOP_CONFIG_DIR` and
//! `FBTT_DESKTOP_DATA_DIR` are the overrides, and the CLI's `FBTT_CONFIG_DIR` and `FBTT_DATA_DIR` are
//! deliberately not read here: a service or container that redirects the CLI's library must not move
//! this one by accident, and a run that wants both redirected sets four variables.
//!
//! Nothing here creates a directory, exactly as in [`fbtt_core::Paths`]: the paths are where files
//! *will* be, and whoever writes makes room in whatever they write into.

use std::ffi::OsString;
use std::path::PathBuf;

use directories::ProjectDirs;
use fbtt_core::Paths;
use fbtt_core::config::ConfigError;

/// The identity the platform's conventions key on for this app: the reverse domain `io.featherbit` plus
/// the desktop application name, which becomes `~/.config/featherbittorrent-desktop` on Linux and the
/// matching bundle or AppData folder elsewhere. The terminal client's name is one word shorter, so the
/// two land in different directories on every system.
const PROJECT: (&str, &str, &str) = ("io", "featherbit", "featherbittorrent-desktop");

/// The override for the settings side, the desktop app's own answer to the CLI's `FBTT_CONFIG_DIR`.
const CONFIG_DIR_ENV: &str = "FBTT_DESKTOP_CONFIG_DIR";
/// The override for the library side, the desktop app's own answer to the CLI's `FBTT_DATA_DIR`.
const DATA_DIR_ENV: &str = "FBTT_DESKTOP_DATA_DIR";
/// The settings file, in [`DesktopPaths::config_dir`] (see [`crate::settings::DesktopSettings`]).
const SETTINGS_FILE_NAME: &str = "desktop.yaml";

/// The two directories one desktop app uses: its settings on one side, the library This computer runs its
/// engine on the other.
///
/// Make one with [`DesktopPaths::resolve`]. On a fresh host neither directory exists yet and that is fine,
/// exactly as in [`fbtt_core::Paths`]: these are where files *will* be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPaths {
    /// Where `desktop.yaml` is read and written, and where this library's own `config.yaml` sits.
    pub config_dir: PathBuf,
    /// Where This computer's engine keeps its state, its lock and the files it downloads.
    pub data_dir: PathBuf,
}

impl DesktopPaths {
    /// Reads the two directories from the environment and the platform.
    ///
    /// `FBTT_DESKTOP_CONFIG_DIR` and `FBTT_DESKTOP_DATA_DIR` win; what they leave out comes from
    /// [`ProjectDirs::from("io", "featherbit", "featherbittorrent-desktop")`](ProjectDirs::from). A host
    /// with no home directory and neither override set is [`ConfigError::NoHome`]; setting both works even
    /// there, which is what a container or a service that keeps its files outside any home has to do.
    ///
    /// That error's text names the terminal client's variables, because it is the same error the CLI's
    /// paths return: whoever shows it names the two variables this app actually reads.
    pub fn resolve() -> Result<DesktopPaths, ConfigError> {
        Self::resolve_with(|key| std::env::var_os(key))
    }

    /// [`resolve`](Self::resolve) with the environment read through `look_up` rather than
    /// [`std::env::var_os`], so a test hands in values instead of setting process-global variables that
    /// would race with other tests.
    ///
    /// An empty value counts as unset, exactly as in [`fbtt_core::Paths::resolve_with`]: an
    /// `FBTT_DESKTOP_DATA_DIR=` left empty by a script is a typo, and it should not send the library into
    /// whatever directory the app happened to be started in.
    pub fn resolve_with<F>(look_up: F) -> Result<DesktopPaths, ConfigError>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        let from_env = |key| {
            look_up(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        // Only this app's own variables are read: the CLI's are how a service redirects the terminal
        // client's library, and that is not a reason to move this one.
        let config_dir = from_env(CONFIG_DIR_ENV);
        let data_dir = from_env(DATA_DIR_ENV);
        if let (Some(config_dir), Some(data_dir)) = (&config_dir, &data_dir) {
            // Both directories are given, so no home directory is needed.
            return Ok(DesktopPaths {
                config_dir: config_dir.clone(),
                data_dir: data_dir.clone(),
            });
        }
        let project =
            ProjectDirs::from(PROJECT.0, PROJECT.1, PROJECT.2).ok_or(ConfigError::NoHome)?;
        Ok(DesktopPaths {
            config_dir: config_dir.unwrap_or_else(|| project.config_dir().to_path_buf()),
            data_dir: data_dir.unwrap_or_else(|| project.data_dir().to_path_buf()),
        })
    }

    /// The library This computer runs its engine on: these two directories, spelled in the type
    /// `fbtt-core` takes.
    ///
    /// The engine, its config file and its lock are all written by code that wants a [`Paths`], and the
    /// desktop library being a separate one is the whole point of this type: the terminal client that
    /// finds this library locked is another instance to add by address, not this app's own engine.
    pub fn library(&self) -> Paths {
        Paths {
            config_dir: self.config_dir.clone(),
            data_dir: self.data_dir.clone(),
        }
    }

    /// The settings file: `<config_dir>/desktop.yaml` (see [`crate::settings::DesktopSettings`]).
    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join(SETTINGS_FILE_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::{DesktopPaths, PROJECT, SETTINGS_FILE_NAME};
    use directories::ProjectDirs;
    use fbtt_core::Paths as CliPaths;
    use fbtt_core::config::ConfigError;
    use fbtt_core::scratch::ScratchDir;
    use std::ffi::OsString;

    /// The terminal client's project, spelled out here because the point of the test below is that the
    /// two projects are not the same directory.
    const CLI_PROJECT: (&str, &str, &str) = ("io", "featherbit", "featherbittorrent");

    /// The desktop overrides win over the platform's directories, and an empty value counts as no value
    /// at all: a script that left `FBTT_DESKTOP_DATA_DIR=` behind sends the app back to the platform's
    /// own directory rather than into whatever directory the app happened to be started in.
    #[test]
    fn env_overrides_win_and_empty_counts_as_unset() {
        let dir = ScratchDir::new("desktop_paths_env");
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let look_up = |key: &str| -> Option<OsString> {
            match key {
                "FBTT_DESKTOP_CONFIG_DIR" => Some(config_dir.clone().into()),
                "FBTT_DESKTOP_DATA_DIR" => Some(data_dir.clone().into()),
                _ => None,
            }
        };

        let paths =
            DesktopPaths::resolve_with(look_up).expect("the environment gives both directories");
        assert_eq!(paths.config_dir, config_dir);
        assert_eq!(paths.data_dir, data_dir);
        assert_eq!(paths.settings_file(), config_dir.join(SETTINGS_FILE_NAME));
        assert_eq!(
            paths.library(),
            CliPaths {
                config_dir: config_dir.clone(),
                data_dir: data_dir.clone(),
            },
            "This computer's library is the desktop directories, spelled in the type the engine reads"
        );

        // A host with no home directory and an empty override has no answer at all, and says so instead
        // of inventing a path; every host with a home gets the platform's own directory back.
        let Some(project) = ProjectDirs::from(PROJECT.0, PROJECT.1, PROJECT.2) else {
            assert!(matches!(
                DesktopPaths::resolve_with(|key| {
                    if key == "FBTT_DESKTOP_CONFIG_DIR" {
                        Some(OsString::new())
                    } else {
                        None
                    }
                }),
                Err(ConfigError::NoHome)
            ));
            return;
        };
        let paths = DesktopPaths::resolve_with(|key| {
            if key == "FBTT_DESKTOP_DATA_DIR" {
                Some(OsString::new())
            } else {
                None
            }
        })
        .expect("the platform fills in what the empty value left out");
        assert_eq!(paths.config_dir, project.config_dir());
        assert_eq!(paths.data_dir, project.data_dir());
    }

    /// The desktop app's directories are its own, in both directions: the terminal client's variables do
    /// not move them, and they do not move the terminal client's library. A container or a service that
    /// redirects one library must not silently redirect the other, and the two programs must never end up
    /// sharing one library, which is what the library lock is there to make impossible inside a directory.
    #[test]
    fn desktop_dirs_differ_from_the_cli_dirs() {
        let Some(desktop) = ProjectDirs::from(PROJECT.0, PROJECT.1, PROJECT.2) else {
            assert!(
                matches!(
                    DesktopPaths::resolve_with(|_| None),
                    Err(ConfigError::NoHome)
                ),
                "a host with no home directory says so"
            );
            return;
        };
        let Some(cli) = ProjectDirs::from(CLI_PROJECT.0, CLI_PROJECT.1, CLI_PROJECT.2) else {
            return; // No home directory; the branch above covers that host.
        };

        let paths = DesktopPaths::resolve_with(|_| None)
            .expect("the platform gives this app its own directories");
        assert_eq!(paths.config_dir, desktop.config_dir());
        assert_eq!(paths.data_dir, desktop.data_dir());
        assert_ne!(
            paths.config_dir,
            cli.config_dir(),
            "the desktop app must not write its settings into the terminal client's config directory"
        );
        assert_ne!(
            paths.data_dir,
            cli.data_dir(),
            "the desktop app must not run its engine in the terminal client's library"
        );

        let dir = ScratchDir::new("desktop_paths_cli_env");
        let cli_env = |key: &str| -> Option<OsString> {
            match key {
                "FBTT_CONFIG_DIR" => Some(dir.path().join("cli-config").into()),
                "FBTT_DATA_DIR" => Some(dir.path().join("cli-data").into()),
                _ => None,
            }
        };
        let unchanged =
            DesktopPaths::resolve_with(cli_env).expect("the CLI's variables are not this app's");
        assert_eq!(unchanged.config_dir, desktop.config_dir());
        assert_eq!(unchanged.data_dir, desktop.data_dir());

        // The same rule read from the other side.
        let desktop_env = |key: &str| -> Option<OsString> {
            match key {
                "FBTT_DESKTOP_CONFIG_DIR" => Some(dir.path().join("desktop-config").into()),
                "FBTT_DESKTOP_DATA_DIR" => Some(dir.path().join("desktop-data").into()),
                _ => None,
            }
        };
        let cli_paths =
            CliPaths::resolve_with(desktop_env).expect("this app's variables are not the CLI's");
        assert_eq!(cli_paths.config_dir, cli.config_dir());
        assert_eq!(cli_paths.data_dir, cli.data_dir());
    }
}
