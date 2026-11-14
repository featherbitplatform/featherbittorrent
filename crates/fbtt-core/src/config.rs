//! The config file: what the user sets once and every run reads back.
//!
//! `config.yaml` is plain YAML in [`Paths::config_file`](crate::Paths::config_file),
//! small enough to edit by hand. Rates are whole KiB/s and the cache whole MiB,
//! because that is how people state them: 0 means unlimited, and a queue count
//! left out stays whatever the engine's own default is. A file that cannot be
//! parsed does not stop the app. It runs on the defaults, and the file it could
//! not read is kept aside as `config.yaml.bad` so the next save cannot overwrite
//! what we never understood.

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use directories::UserDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths::Paths;

/// The port to listen on when the user does not pick one: BitTorrent's own.
const DEFAULT_LISTEN_PORT: u16 = 6881;
/// The engine's write cache, in MiB.
const DEFAULT_CACHE_SIZE_MIB: u64 = 32;
/// Where downloads go on a host with no Downloads directory of its own and no
/// library to put them under.
const DOWNLOADS_DIR_NAME: &str = "downloads";
/// A config that cannot be parsed becomes its own file name plus this.
const BAD_FILE_SUFFIX: &str = ".bad";
/// A save writes this file next to the target, then renames it onto the target.
const TEMP_FILE_SUFFIX: &str = ".tmp";
/// One KiB and one MiB, in the bytes per second and bytes the engine counts in.
const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

/// What can go wrong finding, reading or writing the config.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The host has no home directory and no debug directory was set, so there
    /// is nowhere to put the library.
    #[error("no home directory: set FBTT_CONFIG_DIR and FBTT_DATA_DIR")]
    NoHome,
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file we tried to read.
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file could not be written or moved into place. It names the file the
    /// run wanted in place, not the temp file behind it.
    #[error("cannot write {path}: {source}")]
    Write {
        /// The file we tried to put in place.
        path: PathBuf,
        source: std::io::Error,
    },
}

/// What reading the config file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigLoad {
    /// The file parsed cleanly.
    Loaded(Config),
    /// There is no file yet: a library nobody has configured.
    Missing,
    /// The file is there but cannot be read as YAML.
    Bad {
        /// What to run on instead: every default.
        config: Config,
        /// One line naming the file and saying what is wrong, fit to show the
        /// user and to log.
        problem: String,
    },
}

/// The running limits, in the units a person thinks in.
///
/// A rate of 0 KiB/s is unlimited; a queue count of `None` is unlimited too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Download rate in KiB/s (0: unlimited).
    pub download_kib: u64,
    /// Upload rate in KiB/s (0: unlimited).
    pub upload_kib: u64,
    /// Torrents downloading at once (the engine's own count when unset).
    pub active_downloads: Option<usize>,
    /// Torrents seeding at once (the engine's own count when unset).
    pub active_seeds: Option<usize>,
    /// Torrents running at once, downloading or seeding (the engine's own
    /// count when unset).
    pub active_total: Option<usize>,
}

impl Default for Limits {
    /// No rate limits and the engine's own queue counts. The counts come from
    /// `fbt::QueueSettings::default()` rather than numbers we picked, so what a
    /// first run does is what the engine says it does.
    fn default() -> Self {
        let queue = fbt::QueueSettings::default();
        Limits {
            download_kib: 0,
            upload_kib: 0,
            active_downloads: queue.active_downloads,
            active_seeds: queue.active_seeds,
            active_total: queue.active_limit,
        }
    }
}

/// The settings one library runs on.
///
/// Every field has a default, so a config that names one setting keeps the rest
/// as they are; see [`Config::load`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where torrent contents go.
    pub download_dir: PathBuf,
    /// The port both address families listen on.
    pub listen_port: u16,
    /// Rates and queue counts.
    pub limits: Limits,
    /// The engine's write cache, in MiB.
    pub cache_size_mib: u64,
}

impl Default for Config {
    /// The settings a library starts from: BitTorrent's port, no rate limits,
    /// the engine's own queue counts, a 32 MiB cache and downloads in the host's
    /// Downloads directory. A host with none gets `downloads` relative to the
    /// working directory; [`Config::default_for`] roots them in the library
    /// instead.
    fn default() -> Self {
        Config::with_download_dir(
            user_download_dir().unwrap_or_else(|| PathBuf::from(DOWNLOADS_DIR_NAME)),
        )
    }
}

impl Config {
    /// The defaults for a library in `paths`: [`Config::default`], except that a
    /// host with no Downloads directory downloads into `<data_dir>/downloads`
    /// rather than somewhere under the working directory.
    pub fn default_for(paths: &Paths) -> Config {
        Config::with_download_dir(
            user_download_dir().unwrap_or_else(|| paths.data_dir.join(DOWNLOADS_DIR_NAME)),
        )
    }

    /// The settings a first run starts from, with downloads in `download_dir`
    /// and everything else at its default.
    fn with_download_dir(download_dir: PathBuf) -> Config {
        Config {
            download_dir,
            listen_port: DEFAULT_LISTEN_PORT,
            limits: Limits::default(),
            cache_size_mib: DEFAULT_CACHE_SIZE_MIB,
        }
    }

    /// Reads the config file at `path`.
    ///
    /// A file that is not there is [`ConfigLoad::Missing`], not an error. A file
    /// that is not valid YAML becomes [`ConfigLoad::Bad`] with the defaults, and
    /// it is renamed to `<file>.bad`: a run must not overwrite what it could not
    /// read. A file that cannot be opened at all (no permission, a directory in
    /// the way) is an error, because there is no way to tell "empty" from
    /// "unavailable".
    pub fn load(path: &Path) -> Result<ConfigLoad, ConfigError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ConfigLoad::Missing);
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        match serde_norway::from_slice::<Config>(&bytes) {
            Ok(config) => Ok(ConfigLoad::Loaded(config)),
            Err(error) => {
                let aside = suffixed(path, BAD_FILE_SUFFIX);
                std::fs::rename(path, &aside).map_err(|source| ConfigError::Write {
                    path: path.to_path_buf(),
                    source,
                })?;
                Ok(ConfigLoad::Bad {
                    config: Config::default(),
                    problem: problem_line(path, &error),
                })
            }
        }
    }

    /// Writes the config to `path`, making its directory first if it is not
    /// there yet.
    ///
    /// The bytes go into `<file>.tmp` in the same directory (the rename cannot
    /// cross a filesystem), are synced, and only then are renamed over the
    /// target: a crash leaves either the old config or the new one, never half
    /// of each. A failure before the rename removes the temp file; a failed
    /// rename leaves it holding what we tried to save.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let cannot_write = |source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        };
        let text = serde_norway::to_string(self)
            .map_err(|error| {
                std::io::Error::other(format!("the config cannot be written as YAML: {error}"))
            })
            .map_err(cannot_write)?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(cannot_write)?;
        }
        write_then_rename(&suffixed(path, TEMP_FILE_SUFFIX), path, text.as_bytes())
            .map_err(cannot_write)
    }

    /// The engine settings these values describe.
    ///
    /// Rates convert from KiB/s to bytes per second and the cache from MiB to
    /// bytes; a value too large to convert saturates instead of wrapping. 0 and
    /// `None` mean unlimited in both vocabularies, so they pass straight
    /// through. Both address families listen on the one port, and everything
    /// the config does not name keeps the engine's own default: a new fbt-lib
    /// release changes it here only by choice.
    pub fn session_settings(&self) -> fbt::Settings {
        let mut settings = fbt::Settings::default();
        settings.listen_addrs = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), self.listen_port),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), self.listen_port),
        ];
        settings.rate_limit = fbt::RateLimit::new(
            self.limits.download_kib.saturating_mul(KIB),
            self.limits.upload_kib.saturating_mul(KIB),
        );
        let mut queue = fbt::QueueSettings::default();
        queue.active_downloads = self.limits.active_downloads;
        queue.active_seeds = self.limits.active_seeds;
        queue.active_limit = self.limits.active_total;
        settings.queue = queue;
        settings.cache_size = self.cache_size_mib.saturating_mul(MIB);
        settings
    }
}

/// The host's own Downloads directory, when it has one.
fn user_download_dir() -> Option<PathBuf> {
    UserDirs::new().and_then(|dirs| dirs.download_dir().map(PathBuf::from))
}

/// `path` with `suffix` added to its file name: `config.yaml` becomes
/// `config.yaml.bad`.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or(path.as_os_str()).to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// One line about a parse failure: the whole message with its newlines folded
/// into spaces, led by the file's name so the line makes sense on its own.
fn problem_line(path: &Path, error: &serde_norway::Error) -> String {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let message = error
        .to_string()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");
    if message.is_empty() {
        format!("{name}: malformed YAML")
    } else {
        format!("{name}: {message}")
    }
}

/// Writes `bytes` to `temp`, synced, then renames it over `path`. The temp file
/// is removed when the write fails; when only the rename failed it stays behind
/// holding what we tried to save.
fn write_then_rename(temp: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(temp)?;
        file.write_all(bytes)?;
        // The bytes are on disk before the rename makes them the config.
        file.sync_all()
    })();
    match written {
        Ok(()) => std::fs::rename(temp, path),
        Err(source) => {
            let _ = std::fs::remove_file(temp);
            Err(source)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigLoad, Limits};
    use crate::scratch::ScratchDir;
    use std::fs;

    /// What a first run saves is what the next run reads back.
    #[test]
    fn defaults_round_trip() {
        let dir = ScratchDir::new("config_round_trip");
        let path = dir.path().join("config.yaml");
        let config = Config::default();

        config.save(&path).expect("save the defaults");
        match Config::load(&path).expect("read them back") {
            ConfigLoad::Loaded(loaded) => assert_eq!(loaded, config),
            other => panic!("expected the saved config back, got {other:?}"),
        }

        // An unlimited queue count is written as null and read back as `None`.
        let mut unlimited = config.clone();
        unlimited.limits.active_downloads = None;
        unlimited.limits.active_seeds = None;
        unlimited.limits.active_total = None;
        unlimited.save(&path).expect("save the unlimited counts");
        match Config::load(&path).expect("read them back") {
            ConfigLoad::Loaded(loaded) => assert_eq!(loaded, unlimited),
            other => panic!("expected the saved config back, got {other:?}"),
        }
    }

    /// A library nobody has configured yet is not an error.
    #[test]
    fn missing_file_is_missing() {
        let dir = ScratchDir::new("config_missing");
        let path = dir.path().join("config.yaml");
        assert!(matches!(Config::load(&path), Ok(ConfigLoad::Missing)));
    }

    /// A config that names one setting keeps every other one at its default.
    #[test]
    fn partial_yaml_keeps_defaults() {
        let dir = ScratchDir::new("config_partial");
        let path = dir.path().join("config.yaml");
        fs::write(&path, "listen_port: 51413\n").expect("write a partial config");

        let ConfigLoad::Loaded(config) = Config::load(&path).expect("read a partial config") else {
            panic!("a partial config is still a config");
        };
        let defaults = Config::default();
        assert_eq!(config.listen_port, 51413);
        assert_eq!(config.download_dir, defaults.download_dir);
        assert_eq!(config.limits, defaults.limits);
        assert_eq!(config.cache_size_mib, defaults.cache_size_mib);
    }

    /// A malformed config does not stop the app: it runs on the defaults and
    /// the unreadable file is kept aside instead of being overwritten.
    #[test]
    fn bad_config_is_reported() {
        let dir = ScratchDir::new("config_bad");
        let path = dir.path().join("config.yaml");
        fs::write(&path, "listen_port: [nope").expect("write a malformed config");

        let ConfigLoad::Bad { config, problem } =
            Config::load(&path).expect("a bad file is still read")
        else {
            panic!("a malformed config is reported, not swallowed");
        };
        assert!(!problem.is_empty());
        assert_eq!(
            problem.lines().count(),
            1,
            "the problem is one line: {problem}"
        );
        assert_eq!(config, Config::default());
        assert!(!path.exists(), "the bad file moves out of the way");
        assert!(dir.path().join("config.yaml.bad").exists());
    }

    /// KiB/s and MiB in the file become bytes per second and bytes for the
    /// engine, and `None` counts stay unlimited. The fields the config does not
    /// name keep the engine's own values.
    #[test]
    fn limits_map_to_settings() {
        let config = Config {
            listen_port: 7946,
            cache_size_mib: 8,
            limits: Limits {
                download_kib: 100,
                upload_kib: 25,
                active_downloads: Some(2),
                active_seeds: None,
                active_total: Some(4),
            },
            ..Config::default()
        };

        let settings = config.session_settings();
        let engine = fbt::Settings::default();
        assert_eq!(settings.rate_limit.download, 102_400);
        assert_eq!(settings.rate_limit.upload, 25_600);
        assert_eq!(settings.queue.active_downloads, Some(2));
        assert_eq!(settings.queue.active_seeds, None);
        assert_eq!(settings.queue.active_limit, Some(4));
        assert_eq!(settings.cache_size, 8 * 1024 * 1024);
        let ports: Vec<u16> = settings
            .listen_addrs
            .iter()
            .map(|addr| addr.port())
            .collect();
        assert_eq!(ports, [7946, 7946]);
        assert!(settings.listen_addrs[0].is_ipv4());
        assert!(settings.listen_addrs[1].is_ipv6());
        assert_eq!(
            settings.queue.dont_count_slow_torrents,
            engine.queue.dont_count_slow_torrents
        );
        assert_eq!(settings.max_peers_per_torrent, engine.max_peers_per_torrent);
        assert_eq!(settings.cache_expiry, engine.cache_expiry);
    }

    /// The queue limits are the engine's, not numbers we picked here.
    #[test]
    fn defaults_come_from_the_engine() {
        let queue = fbt::QueueSettings::default();
        let limits = Limits::default();
        assert_eq!(limits.active_downloads, queue.active_downloads);
        assert_eq!(limits.active_seeds, queue.active_seeds);
        assert_eq!(limits.active_total, queue.active_limit);
        assert_eq!(limits.download_kib, 0);
        assert_eq!(limits.upload_kib, 0);

        let config = Config::default();
        assert_eq!(config.listen_port, 6881);
        assert_eq!(config.cache_size_mib, 32);
        assert!(!config.download_dir.as_os_str().is_empty());
    }

    /// A save replaces the file in one step and leaves nothing behind, so a
    /// crash cannot leave a half-written config.
    #[test]
    fn save_replaces_the_file_without_leftovers() {
        let dir = ScratchDir::new("config_save");
        let path = dir.path().join("config.yaml");

        Config::default().save(&path).expect("save once");
        let changed = Config {
            listen_port: 51000,
            ..Config::default()
        };
        changed
            .save(&path)
            .expect("save twice, over a shorter file");

        let ConfigLoad::Loaded(config) = Config::load(&path).expect("read it back") else {
            panic!("the second save replaced the first");
        };
        assert_eq!(config.listen_port, 51000);
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .expect("read the directory")
            .flatten()
            .filter(|entry| entry.path() != path)
            .map(|entry| entry.file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a temp file survived the save: {leftovers:?}"
        );
    }
}
