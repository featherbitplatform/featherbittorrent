//! The config file: what the user sets once and every run reads back.
//!
//! `config.yaml` is plain YAML in [`Paths::config_file`](crate::Paths::config_file),
//! small enough to edit by hand. Rates are whole KiB/s and the cache whole MiB,
//! because that is how people state them: 0 means unlimited, and a queue count
//! left out stays whatever the engine's own default is. A download folder may be
//! named relative to the library (`download_dir: dl`), which is where it lands;
//! see [`Config::resolve`]. The `server` section says where `featherbittorrent serve`
//! listens, which folders its API may name, and whether anyone has to sign in. A file
//! that cannot be parsed does not stop the app. It runs on this library's defaults, and
//! the file it could not read is kept aside as `config.yaml.bad` so the next save cannot
//! overwrite what we never understood.

use std::fmt;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use directories::UserDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths::Paths;

/// The port to listen on when the user does not pick one: BitTorrent's own.
const DEFAULT_LISTEN_PORT: u16 = 6881;
/// The port the API listens on when the config does not name an address at all.
const DEFAULT_SERVER_PORT: u16 = 8090;
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
        /// What to run on instead: this library's defaults, its download folder
        /// rooted in the data directory (see [`Config::resolve`]).
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

/// Where `featherbittorrent serve` listens, which folders its API may name, and who has to sign
/// in to use it.
///
/// The address decides the login rule ([`auth_on`](Self::auth_on)): a port on loopback is already
/// reachable only by someone sitting at this machine, who owns the library anyway; a port on any
/// other address is reachable by everyone on the network, and without logins they would all read
/// and control the same torrents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// The address and port to serve on.
    pub bind: SocketAddr,
    /// The directories the API may name in a request: save paths, move targets, folders to
    /// browse. An empty list means the download folder alone. [`Config::resolve`] fills that in
    /// and makes every entry absolute, so what the server enforces is what the run works on - and
    /// [`Config::save`] writes it back unnamed when it is only that folder, so the default keeps
    /// following `download_dir` wherever the person moves it.
    pub allowed_roots: Vec<PathBuf>,
    /// Whether login is on, and who may use it.
    pub auth: AuthConfig,
}

impl Default for ServerConfig {
    /// Loopback on port 8090, the download folder as the only root (filled in by
    /// [`Config::resolve`]), and login left to [`ServerConfig::auth_on`] to decide.
    fn default() -> Self {
        ServerConfig {
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_SERVER_PORT)),
            allowed_roots: Vec::new(),
            auth: AuthConfig::default(),
        }
    }
}

impl ServerConfig {
    /// Whether this server signs people in.
    ///
    /// `auth.enabled` spells the answer when the config sets it; left out, authentication is on
    /// for anything but a loopback address. Turning it off on a public bind is therefore an
    /// explicit `enabled: false`, not something a missing field does by accident.
    pub fn auth_on(&self) -> bool {
        self.auth.enabled.unwrap_or(!self.bind.ip().is_loopback())
    }
}

/// Login for the server: whether it is on, and who is let in.
///
/// Neither list holds a secret beyond `UserEntry`'s hash, which its `Debug` hides. A device key
/// is a public key and may be printed freely.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// `None`: on unless [`ServerConfig::bind`] is loopback, which is what
    /// [`ServerConfig::auth_on`] answers with. `Some(true)` and `Some(false)` mean exactly that.
    pub enabled: Option<bool>,
    /// The accounts a password logs in to. Empty means no password logins at all.
    pub users: Vec<UserEntry>,
    /// The device keys that may log in without a password. Empty means none.
    pub devices: Vec<DeviceEntry>,
}

/// One account a password logs in to.
///
/// The hash is a secret, so this is the one type in the config whose `Debug` hides a field: a
/// config printed in a log line or a panic message must not carry a password hash out of it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserEntry {
    /// The name typed at the login prompt.
    pub name: String,
    /// The Argon2id hash (a PHC string) of the password, never the password itself.
    pub password_hash: String,
}

impl fmt::Debug for UserEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserEntry")
            .field("name", &self.name)
            .field("password_hash", &"<hash>")
            .finish()
    }
}

/// One device key that may log in by signing a challenge, with no password involved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEntry {
    /// What the person named this device, for the devices list.
    pub name: String,
    /// The public key in the one-line OpenSSH format (`ssh-ed25519 AAAA... comment`).
    pub public_key: String,
}

/// The settings one library runs on.
///
/// Every field has a default, so a config that names one setting keeps the rest
/// as they are; see [`Config::load`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where torrent contents go. A relative one says "below the library" and is
    /// made absolute by [`Config::resolve`] before a run uses it.
    pub download_dir: PathBuf,
    /// The port both address families listen on.
    pub listen_port: u16,
    /// Rates and queue counts.
    pub limits: Limits,
    /// The engine's write cache, in MiB.
    pub cache_size_mib: u64,
    /// The headless server: where it listens, what its API may name, and who signs in. A run of
    /// the TUI reads none of this; `featherbittorrent serve` is what acts on it.
    pub server: ServerConfig,
}

impl Default for Config {
    /// The settings a library starts from: BitTorrent's port, no rate limits,
    /// the engine's own queue counts, a 32 MiB cache and downloads in the host's
    /// Downloads directory. A host with none gets `downloads`, which is relative:
    /// [`Config::default_for`] roots it in the library, and
    /// [`Config::resolve`] roots whatever came out of a config file.
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

    /// Makes this config's `download_dir` a directory the run can actually use:
    /// relative to `paths.data_dir`, and an empty one becomes
    /// `<data_dir>/downloads`.
    ///
    /// A config file is hand-edited, so it may name its folder as `dl` or leave the
    /// field out entirely, and a bare `Default::default()` names `downloads`. None
    /// of those may mean "wherever this process happened to be started": downloads
    /// would land there and move whenever the app was launched from somewhere else.
    /// An absolute folder is the person's own choice and stays exactly as written.
    /// Every config a run works on goes through here (see the binary's setup), so
    /// what the backend holds, what a snapshot offers the add dialog, and what a
    /// save writes back are all rooted in the library.
    ///
    /// `server.allowed_roots` is put through the same rule, and an empty list becomes
    /// the download folder alone: the folders the API may name have to be absolute
    /// directories for the server to confine requests to them, and a config that names
    /// no root means "downloads only", not "everything on this machine".
    pub fn resolve(mut self, paths: &Paths) -> Config {
        self.download_dir = absolute_dir(&self.download_dir, paths);
        self.server.allowed_roots =
            resolved_roots(&self.server.allowed_roots, &self.download_dir, paths);
        self
    }

    /// The settings a first run starts from, with downloads in `download_dir`
    /// and everything else at its default.
    fn with_download_dir(download_dir: PathBuf) -> Config {
        Config {
            download_dir,
            listen_port: DEFAULT_LISTEN_PORT,
            limits: Limits::default(),
            cache_size_mib: DEFAULT_CACHE_SIZE_MIB,
            server: ServerConfig::default(),
        }
    }

    /// Reads this library's config file, [`Paths::config_file`].
    ///
    /// A file that is not there is [`ConfigLoad::Missing`], not an error. A file
    /// that is not valid YAML becomes [`ConfigLoad::Bad`] with this library's own
    /// defaults ([`Config::default_for`], with its download folder resolved), and
    /// it is renamed to `<file>.bad`: a run must not overwrite what it could not
    /// read. A file that cannot be opened at all (no permission, a directory in
    /// the way) is an error, because there is no way to tell "empty" from
    /// "unavailable". The library comes in rather than the file for exactly this
    /// reason: the defaults have to be rooted in it.
    pub fn load(paths: &Paths) -> Result<ConfigLoad, ConfigError> {
        let path = paths.config_file();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ConfigLoad::Missing);
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        match serde_norway::from_slice::<Config>(&bytes) {
            Ok(config) => Ok(ConfigLoad::Loaded(config)),
            Err(error) => {
                let aside = suffixed(&path, BAD_FILE_SUFFIX);
                std::fs::rename(&path, &aside).map_err(|source| ConfigError::Write {
                    path: path.clone(),
                    source,
                })?;
                Ok(ConfigLoad::Bad {
                    config: Config::default_for(paths).resolve(paths),
                    problem: problem_line(&path, &error),
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
    ///
    /// What goes in is this config with `server.allowed_roots` unnamed when it is only the download
    /// folder [`resolve`] filled in: a run saves the config it resolved, and pinning the roots to
    /// that directory would leave the server's root behind the day the person moves `download_dir`.
    /// An empty list means "the download folder" on disk, which is what keeps following it.
    ///
    /// [`resolve`]: Self::resolve
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let cannot_write = |source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        };
        let text = serde_norway::to_string(&self.for_the_file())
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

    /// This config as it belongs in a file: the roots unnamed when they are the one directory
    /// [`resolve`](Self::resolve) would fill in anyway. See [`save`](Self::save).
    fn for_the_file(&self) -> Config {
        let mut out = self.clone();
        let pins_only_downloads =
            out.server.allowed_roots.len() == 1 && out.server.allowed_roots[0] == out.download_dir;
        if pins_only_downloads {
            out.server.allowed_roots.clear();
        }
        out
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

/// A directory the config named, as a path this run can use: absolute stays, relative hangs
/// below the library's data directory, and nothing at all means the library's own `downloads`.
/// See [`Config::resolve`], which puts `download_dir` and each `server.allowed_roots` entry
/// through here.
fn absolute_dir(dir: &Path, paths: &Paths) -> PathBuf {
    if dir.as_os_str().is_empty() {
        return paths.data_dir.join(DOWNLOADS_DIR_NAME);
    }
    if dir.is_absolute() {
        return dir.to_path_buf();
    }
    paths.data_dir.join(dir)
}

/// The directories the API may name, as absolute paths: what the config named, rooted in the
/// library like `download_dir` is, or the download folder alone when it named none. An empty
/// list cannot mean "no folders" to a server that has to answer requests about somewhere. See
/// [`Config::resolve`].
fn resolved_roots(roots: &[PathBuf], download_dir: &Path, paths: &Paths) -> Vec<PathBuf> {
    if roots.is_empty() {
        return vec![download_dir.to_path_buf()];
    }
    roots.iter().map(|root| absolute_dir(root, paths)).collect()
}

/// `path` with `suffix` added to its file name: `config.yaml` becomes
/// `config.yaml.bad`. The session state save uses the same convention for
/// `session.state` ([`crate::LocalBackend`]).
pub(crate) fn suffixed(path: &Path, suffix: &str) -> PathBuf {
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

/// Writes `bytes` into `path` the way every featherbittorrent file is written: a temp
/// file beside the target, synced, then renamed over it. The session state save shares
/// this so both files are replaced by the same rules ([`crate::LocalBackend`]).
pub(crate) fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_then_rename(&suffixed(path, TEMP_FILE_SUFFIX), path, bytes)
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
    use super::{
        AuthConfig, Config, ConfigLoad, DOWNLOADS_DIR_NAME, Limits, ServerConfig, UserEntry,
        user_download_dir,
    };
    use crate::paths::Paths;
    use crate::scratch::ScratchDir;
    use std::fs;
    use std::path::PathBuf;

    /// The scratch directory stands in for a whole library: config side and data
    /// side in the same place, which is all `Config::load` needs to know.
    fn library(dir: &ScratchDir) -> Paths {
        Paths {
            config_dir: dir.path().to_path_buf(),
            data_dir: dir.path().to_path_buf(),
        }
    }

    /// What a first run saves is what the next run reads back.
    #[test]
    fn defaults_round_trip() {
        let dir = ScratchDir::new("config_round_trip");
        let paths = library(&dir);
        let path = paths.config_file();
        let config = Config::default();

        config.save(&path).expect("save the defaults");
        match Config::load(&paths).expect("read them back") {
            ConfigLoad::Loaded(loaded) => assert_eq!(loaded, config),
            other => panic!("expected the saved config back, got {other:?}"),
        }

        // An unlimited queue count is written as null and read back as `None`.
        let mut unlimited = config.clone();
        unlimited.limits.active_downloads = None;
        unlimited.limits.active_seeds = None;
        unlimited.limits.active_total = None;
        unlimited.save(&path).expect("save the unlimited counts");
        match Config::load(&paths).expect("read them back") {
            ConfigLoad::Loaded(loaded) => assert_eq!(loaded, unlimited),
            other => panic!("expected the saved config back, got {other:?}"),
        }
    }

    /// A library nobody has configured yet is not an error.
    #[test]
    fn missing_file_is_missing() {
        let dir = ScratchDir::new("config_missing");
        assert!(matches!(
            Config::load(&library(&dir)),
            Ok(ConfigLoad::Missing)
        ));
    }

    /// A config that names one setting keeps every other one at its default.
    #[test]
    fn partial_yaml_keeps_defaults() {
        let dir = ScratchDir::new("config_partial");
        let paths = library(&dir);
        fs::write(paths.config_file(), "listen_port: 51413\n").expect("write a partial config");

        let ConfigLoad::Loaded(config) = Config::load(&paths).expect("read a partial config")
        else {
            panic!("a partial config is still a config");
        };
        let defaults = Config::default();
        assert_eq!(config.listen_port, 51413);
        assert_eq!(config.download_dir, defaults.download_dir);
        assert_eq!(config.limits, defaults.limits);
        assert_eq!(config.cache_size_mib, defaults.cache_size_mib);
    }

    /// A config that names only a port still has somewhere to download to once the
    /// run resolves it: an absolute directory, and the library's own when the host
    /// has no Downloads directory of its own. That is [`Config::default_for`]'s
    /// answer, so a half-written file and no file at all go to the same place.
    #[test]
    fn partial_yaml_download_dir_is_absolute() {
        let dir = ScratchDir::new("config_partial_download_dir");
        let paths = library(&dir);
        fs::write(paths.config_file(), "listen_port: 51413\n").expect("write a partial config");

        let ConfigLoad::Loaded(config) = Config::load(&paths).expect("read a partial config")
        else {
            panic!("a partial config is still a config");
        };
        let resolved = config.resolve(&paths);
        assert_eq!(resolved.listen_port, 51413);
        assert_eq!(
            resolved.download_dir,
            Config::default_for(&paths).download_dir,
            "a partial config downloads somewhere other than a missing one"
        );
        assert!(
            resolved.download_dir.is_absolute(),
            "the download folder depends on where the process was started: {}",
            resolved.download_dir.display()
        );
        if user_download_dir().is_none() {
            assert_eq!(
                resolved.download_dir,
                paths.data_dir.join(DOWNLOADS_DIR_NAME),
                "a host with no Downloads directory should download under its library"
            );
        }
    }

    /// A relative `download_dir` is a folder below the library, not below whatever
    /// directory the app happened to be started in. An absolute one is the person's
    /// own choice and stays as written.
    #[test]
    fn relative_download_dir_resolves_against_data_dir() {
        let dir = ScratchDir::new("config_relative_download_dir");
        let paths = library(&dir);
        fs::write(paths.config_file(), "download_dir: dl\n").expect("write a relative folder");

        let ConfigLoad::Loaded(config) = Config::load(&paths).expect("read the file as written")
        else {
            panic!("a relative folder is still a config");
        };
        assert_eq!(
            config.download_dir,
            PathBuf::from("dl"),
            "the file should be read as it was written and resolved afterwards"
        );
        assert_eq!(
            config.resolve(&paths).download_dir,
            paths.data_dir.join("dl")
        );

        // Nothing named means the library's own downloads folder.
        let empty = Config {
            download_dir: PathBuf::new(),
            ..Config::default()
        };
        assert_eq!(
            empty.resolve(&paths).download_dir,
            paths.data_dir.join(DOWNLOADS_DIR_NAME)
        );

        // An absolute folder is left alone, and resolving twice changes nothing.
        let chosen = paths.data_dir.join("elsewhere");
        let once = Config {
            download_dir: chosen.clone(),
            ..Config::default()
        }
        .resolve(&paths);
        assert_eq!(once.download_dir, chosen);
        assert_eq!(once.resolve(&paths).download_dir, chosen);
    }

    /// A malformed config does not stop the app: it runs on this library's defaults
    /// and the unreadable file is kept aside instead of being overwritten.
    #[test]
    fn bad_config_is_reported() {
        let dir = ScratchDir::new("config_bad");
        let paths = library(&dir);
        fs::write(paths.config_file(), "listen_port: [nope").expect("write a malformed config");

        let ConfigLoad::Bad { config, problem } =
            Config::load(&paths).expect("a bad file is still read")
        else {
            panic!("a malformed config is reported, not swallowed");
        };
        assert!(!problem.is_empty());
        assert_eq!(
            problem.lines().count(),
            1,
            "the problem is one line: {problem}"
        );
        assert_eq!(config, Config::default_for(&paths).resolve(&paths));
        assert!(
            config.download_dir.is_absolute(),
            "defaults a run starts on should not depend on the working directory"
        );
        assert!(
            !paths.config_file().exists(),
            "the bad file moves out of the way"
        );
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
        let paths = library(&dir);
        let path = paths.config_file();

        Config::default().save(&path).expect("save once");
        let changed = Config {
            listen_port: 51000,
            ..Config::default()
        };
        changed
            .save(&path)
            .expect("save twice, over a shorter file");

        let ConfigLoad::Loaded(config) = Config::load(&paths).expect("read it back") else {
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

    /// The server section's defaults, and the one rule that decides whether anyone has to sign
    /// in: a server on loopback does not, a server anyone can reach does, and only an explicit
    /// `enabled: false` turns that off. A config file that names part of the section keeps the
    /// rest at these defaults, like every other part of it.
    #[test]
    fn server_config_defaults() {
        let dir = ScratchDir::new("config_server");
        let paths = library(&dir);

        let defaults = Config::default();
        assert_eq!(
            defaults.server.bind,
            "127.0.0.1:8090".parse().expect("the default bind"),
            "the API listens on loopback unless the config says otherwise"
        );
        assert!(
            defaults.server.allowed_roots.is_empty(),
            "no roots are named until the run resolves the config: {:?}",
            defaults.server.allowed_roots
        );
        assert_eq!(defaults.server.auth.enabled, None);
        assert!(defaults.server.auth.users.is_empty());
        assert!(defaults.server.auth.devices.is_empty());

        assert!(
            !defaults.server.auth_on(),
            "a server on loopback does not need a login"
        );
        let mut localhost = Config::default();
        localhost.server.bind = "[::1]:8090".parse().expect("the IPv6 loopback");
        assert!(!localhost.server.auth_on(), "::1 is loopback too");

        let mut public = Config::default();
        public.server.bind = "0.0.0.0:8090".parse().expect("a bind on every address");
        assert!(
            public.server.auth_on(),
            "a server anyone can reach signs people in"
        );

        let mut turned_off = public.clone();
        turned_off.server.auth.enabled = Some(false);
        assert!(
            !turned_off.server.auth_on(),
            "only an explicit `enabled: false` turns authentication off"
        );

        let let_in = Config {
            server: ServerConfig {
                auth: AuthConfig {
                    enabled: Some(true),
                    ..AuthConfig::default()
                },
                ..ServerConfig::default()
            },
            ..Config::default()
        };
        assert!(
            let_in.server.auth_on(),
            "and `enabled: true` keeps it on even on loopback"
        );

        // A file naming one server setting keeps the rest at the defaults above.
        fs::write(
            paths.config_file(),
            "server:\n  bind: 0.0.0.0:8090\n  auth:\n    enabled: false\n",
        )
        .expect("write a config naming the server");
        let ConfigLoad::Loaded(config) = Config::load(&paths).expect("read it back") else {
            panic!("a config naming the server is still a config");
        };
        assert_eq!(config.server.bind, public.server.bind);
        assert!(!config.server.auth_on());
        assert_eq!(
            config.server.allowed_roots, defaults.server.allowed_roots,
            "the part the file leaves out is default too"
        );

        // And what a save writes is what the next run reads.
        let with_users = Config {
            server: ServerConfig {
                bind: public.server.bind,
                allowed_roots: vec![paths.data_dir.join("share")],
                auth: AuthConfig {
                    enabled: Some(true),
                    users: vec![UserEntry {
                        name: "alice".to_string(),
                        password_hash: "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA".to_string(),
                    }],
                    devices: Vec::new(),
                },
            },
            ..Config::default()
        };
        with_users
            .save(&paths.config_file())
            .expect("save the server section");
        match Config::load(&paths).expect("read it back") {
            ConfigLoad::Loaded(loaded) => assert_eq!(loaded, with_users),
            other => panic!("expected the saved server back, got {other:?}"),
        }
    }

    /// The folders the API may touch: none named means the download folder alone, and a named
    /// one follows `download_dir`'s rule - relative to the library, absolute left alone.
    #[test]
    fn allowed_roots_default_to_the_download_dir() {
        let dir = ScratchDir::new("config_allowed_roots");
        let paths = library(&dir);

        let nothing_named = Config::default().resolve(&paths);
        let downloads = nothing_named.download_dir.clone();
        assert_eq!(
            nothing_named.server.allowed_roots,
            [downloads],
            "with no roots in the config the download folder is the one root"
        );

        let named = Config {
            server: ServerConfig {
                allowed_roots: vec![PathBuf::from("share"), paths.data_dir.join("elsewhere")],
                ..ServerConfig::default()
            },
            ..Config::default()
        }
        .resolve(&paths);
        assert_eq!(
            named.server.allowed_roots,
            [
                paths.data_dir.join("share"),
                paths.data_dir.join("elsewhere")
            ],
            "a relative root hangs below the library and an absolute one stays as written"
        );

        // Resolving twice changes nothing, as it does for `download_dir`.
        let roots = named.server.allowed_roots.clone();
        assert_eq!(named.resolve(&paths).server.allowed_roots, roots);
    }

    /// A password hash is a secret: nothing that prints a config may show it. `Debug` names the
    /// field and hides its value, so a log line or a panic message carries the name only.
    #[test]
    fn user_debug_hides_hash() {
        let user = UserEntry {
            name: "alice".to_string(),
            password_hash: "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaGFnb3JpdGht".to_string(),
        };
        let shown = format!("{user:?}");
        assert!(
            shown.contains("<hash>"),
            "the field is still named, with its value replaced: {shown}"
        );
        assert!(
            !shown.contains("argon2") && !shown.contains("c2FsdA"),
            "neither the hash nor its salt is printed: {shown}"
        );
        assert!(shown.contains("alice"), "the name is shown: {shown}");

        // The same through a config, and through a list of users.
        let config = Config {
            server: ServerConfig {
                auth: AuthConfig {
                    enabled: Some(true),
                    users: vec![user.clone()],
                    ..AuthConfig::default()
                },
                ..ServerConfig::default()
            },
            ..Config::default()
        };
        let shown = format!("{config:?}");
        assert!(
            !shown.contains("argon2"),
            "a config's Debug hides the hash too: {shown}"
        );
        assert!(shown.contains("<hash>"), "and says so: {shown}");
        let shown = format!("{:?}", vec![user]);
        assert!(!shown.contains("argon2"), "so does a list of them: {shown}");
    }
}
