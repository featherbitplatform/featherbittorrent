//! The config file: what the user sets once and every run reads back.
//!
//! `config.yaml` is plain YAML in [`Paths::config_file`](crate::Paths::config_file),
//! small enough to edit by hand. Rates are whole KiB/s and the cache whole MiB,
//! because that is how people state them: 0 means unlimited, and a queue count
//! left out stays whatever the engine's own default is. A download folder may be
//! named relative to the library (`download_dir: dl`), which is where it lands;
//! see [`Config::resolve`]. The `server` section says where `featherbittorrent serve`
//! listens, which folders its API may name, and whether anyone has to sign in; it is
//! written only by a run that chose to serve (`serve`, the setup wizard, `user` or
//! `device`), so a config the TUI saves for itself carries none - see [`Config::server`].
//! A full build's browser UI follows this section too: [`ServerConfig::web_ui_on`] is the
//! question the server asks it, and [`web_ui_env_override`] says how `FBTT_WEB_UI` can
//! overrule the file before that question is asked.
//! A file that cannot be parsed does not stop the app. It runs on this library's defaults, and
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
/// The most extra arguments a media player may be handed before the file is refused:
/// a playlist's arguments, and the player's own, are one line a hostile file must not fill.
const MAX_PLAYER_ARGS: usize = 32;
/// The most bytes a single extra argument may be, for the same reason.
const MAX_PLAYER_ARG_BYTES: usize = 1024;

/// The name of the one environment variable `serve` reads about the browser UI.
const WEB_UI_ENV: &str = "FBTT_WEB_UI";

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
    /// The file read as this config, but the player's setting is not something a run may act on:
    /// an empty program, more than `MAX_PLAYER_ARGS` arguments, or an argument longer than
    /// `MAX_PLAYER_ARG_BYTES` bytes. The file is left exactly where it is - it was read, and a
    /// person fixing the line needs to see what was in it - and every setting but the player's is
    /// kept as read. Only the player's comes back at its default, and the run works on that.
    ///
    /// The reset is in memory, not in the file: the next save of this config - for example the
    /// first limits save the local engine makes while it runs - writes the default player over
    /// the bad value, so the file is corrected on the next write, not before.
    PlayerProblem {
        /// The config as the file has it, with only the player's setting at its default.
        config: Config,
        /// One line saying what is wrong with the player's setting, fit to show the user and to log.
        problem: String,
    },
}

/// The media player a library tells a file to open in, and the extra arguments it passes
/// before the media.
///
/// `program` is `auto` (the run finds mpv, then VLC), `mpv`, `vlc`, or a path or command
/// name; `args` are put before the media. Both are bounded, because they end up on a
/// process's argument line, which a hostile or a merely careless file must not be able to
/// fill: at most 32 arguments, each at most 1024 bytes, and a program that is not empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerConfig {
    /// What to start. `auto` finds mpv or VLC; otherwise that program by name or path.
    pub program: String,
    /// Extra arguments put before the media.
    pub args: Vec<String>,
}

impl Default for PlayerConfig {
    /// Find a player by itself, with nothing to put before the media.
    fn default() -> Self {
        PlayerConfig {
            program: "auto".to_string(),
            args: Vec::new(),
        }
    }
}

/// Whether the header and help screen draw the image logo.
///
/// `Auto` (the default) shows it in terminals that speak one of the image protocols and
/// leaves the text title everywhere else; `Off` is the way out for a terminal that
/// misbehaves on the probe's queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogoSetting {
    /// The terminal decides: an image where it can show one, the text title everywhere else.
    #[default]
    Auto,
    /// The text title, wherever.
    Off,
}

/// Whether the browser session cookie carries the `Secure` flag.
///
/// `Auto` (the default) takes the flag from the request itself: `Secure` when it says it
/// arrived over https, through `X-Forwarded-Proto` or `Forwarded`. A person behind a proxy
/// that forwards neither header sets `Always` and says so, rather than having the cookie
/// quietly drop the flag the browser needs to send it over plain http at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CookieSecure {
    /// `Secure` when the request says it arrived over https, and not otherwise.
    #[default]
    Auto,
    /// `Secure` whatever the request says.
    Always,
}

impl CookieSecure {
    /// Whether this is the default. `skip_serializing_if` uses it to keep a config that
    /// never chose a cookie rule silent about one.
    fn is_auto(&self) -> bool {
        matches!(self, Self::Auto)
    }
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

/// Where `featherbittorrent serve` listens, which folders its API may name, which host names it answers
/// when nobody signs in, who has to sign in to use it, and whether the browser UI rides along on the
/// API's own origin.
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
    /// The names this server answers when nobody signs in, beside `localhost` and an address.
    ///
    /// With logins off the server has no credential to say who a request came from, so all that stands
    /// between a hostile web page and this library is the name the browser thinks it is talking to: a
    /// request whose `Host` is some other domain is refused, because that is how a DNS-rebinding attack
    /// arrives. An address (an IPv4 one, or an IPv6 one in brackets) is always accepted - someone using
    /// this on their own network types an address, and a rebound page cannot name one. This list is for
    /// the person who runs the server behind a name of their own (`bit.local`) and turned logins off on
    /// purpose: it says which names count as that address. Empty means `localhost` and addresses only,
    /// which is what a loopback-only server needs and the safe answer for any other bind. Logins being on
    /// makes this list irrelevant: every request then has to carry a token, and no web page gets one by
    /// accident. Spell an entry as a bare name - `bit.local`, not `https://bit.local:8090` - because the
    /// port is taken off the request before names are compared, and a server says out loud when it starts
    /// which of the entries it was given could never be matched by anything.
    pub allowed_hosts: Vec<String>,
    /// Whether login is on, and who may use it.
    pub auth: AuthConfig,
    /// Whether `serve` also serves the browser UI, as far as the config gets a say.
    ///
    /// Left out of the file and `Some(true)` are both on; only `Some(false)` turns the UI off,
    /// and [`ServerConfig::web_ui_on`] is the one question the server asks this value.
    /// `FBTT_WEB_UI` can replace the file's answer before the server ever asks (see
    /// [`web_ui_env_override`]). Which build has a UI to serve at all is the cargo feature
    /// `web-ui` on `fbtt-server`, not something a config file can reach: a headless build
    /// accepts this key, ignores what it says, and logs one line when it is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_ui: Option<bool>,
    /// When the browser session cookie carries the `Secure` flag. See [`CookieSecure`].
    #[serde(default, skip_serializing_if = "CookieSecure::is_auto")]
    pub cookie_secure: CookieSecure,
    /// The origins of clients served from somewhere else that may call this API from a browser engine:
    /// the desktop app's webview (`tauri://localhost` on macOS and Linux, `http://tauri.localhost` on
    /// Windows) is the one this is for. Each entry is a whole origin, scheme included, and is handed to
    /// fbt-api's CORS layer as it is. Being on this list changes nothing about credentials: a listed
    /// origin still has to sign in, and the browser session cookie keeps its own origin rule. Empty, the
    /// default, means no cross-origin caller at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cors_origins: Vec<String>,
}

impl Default for ServerConfig {
    /// Loopback on port 8090, the download folder as the only root (filled in by
    /// [`Config::resolve`]), no host names beside `localhost` and an address (logins are on off-loopback
    /// anyway), login left to [`ServerConfig::auth_on`] to decide, the browser UI left on by silence and
    /// the cookie rule left to `auto` - so a fresh section writes neither of those keys.
    fn default() -> Self {
        ServerConfig {
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_SERVER_PORT)),
            allowed_roots: Vec::new(),
            allowed_hosts: Vec::new(),
            auth: AuthConfig::default(),
            web_ui: None,
            cookie_secure: CookieSecure::default(),
            cors_origins: Vec::new(),
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

    /// Whether the browser UI is served, as far as the config gets a say.
    ///
    /// Yes unless the config says `web_ui: false` - and `FBTT_WEB_UI`, when it names an answer,
    /// has replaced the file's word before the server ever asks (see
    /// [`web_ui_env_override`]). This is the only question a server asks these values; whether
    /// *this build* has a UI to serve at all is the cargo feature `web-ui` on `fbtt-server`,
    /// which no config value can turn on or off.
    pub fn web_ui_on(&self) -> bool {
        self.web_ui != Some(false)
    }
}

/// The browser UI's environment switch: what `FBTT_WEB_UI` holds, or `None` when it holds
/// nothing this understands. The words `0`, `false`, `off` and `no` mean off and `1`, `true`,
/// `on` and `yes` mean on, both lists read case-insensitively; anything else - a stranger
/// word, or no variable at all - is no answer rather than a guess, which leaves the config
/// in charge. It takes the lookup as an argument so the decision can be tested with an
/// environment handed in: the process's own is read at exactly one call site (`serve`'s
/// `open` in the binary), so no test ever has to set anything.
pub fn web_ui_env_override(env: &dyn Fn(&str) -> Option<String>) -> Option<bool> {
    let value = env(WEB_UI_ENV)?;
    match value.to_ascii_lowercase().as_str() {
        "0" | "false" | "off" | "no" => Some(false),
        "1" | "true" | "on" | "yes" => Some(true),
        _ => None,
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
    ///
    /// `None` means this config never chose to serve, which is not the same as choosing the defaults:
    /// it is why a second machine that finds this library locked cannot invent an address for the
    /// server holding it ([`crate::ui_state`] and `named_server` in the binary read this field for
    /// exactly that). A config written by `serve`, the setup wizard, or `featherbittorrent user` and
    /// `device` names one; a config the TUI saves for itself does not. See the module docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerConfig>,
    /// The media player a file opens in, and its extra arguments.
    pub player: PlayerConfig,
    /// Whether the image logo is drawn.
    pub logo: LogoSetting,
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
    /// no root means "downloads only", not "everything on this machine". A config with no
    /// `server` section at all gets none added here: resolving is not deciding to serve.
    pub fn resolve(mut self, paths: &Paths) -> Config {
        self.download_dir = absolute_dir(&self.download_dir, paths);
        if let Some(server) = self.server.as_mut() {
            server.allowed_roots = resolved_roots(&server.allowed_roots, &self.download_dir, paths);
        }
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
            // No server section: this config has not chosen to serve anything.
            server: None,
            player: PlayerConfig::default(),
            logo: LogoSetting::default(),
        }
    }

    /// Reads this library's config file, [`Paths::config_file`].
    ///
    /// A file that is not there is [`ConfigLoad::Missing`], not an error. A file
    /// that is not valid YAML becomes [`ConfigLoad::Bad`] with this library's own
    /// defaults ([`Config::default_for`], with its download folder resolved), and
    /// it is renamed to `<file>.bad`: a run must not overwrite what it could not
    /// read. A file that reads as this config but overruns the player's bounds
    /// becomes [`ConfigLoad::PlayerProblem`]: the file is left where it is, and
    /// the config comes back with the player's setting at its default and a line
    /// saying what is wrong. A file that cannot be opened at all (no permission,
    /// a directory in the way) is an error, because there is no way to tell
    /// "empty" from "unavailable". The library comes in rather than the file for
    /// exactly this reason: the defaults have to be rooted in it.
    pub fn load(paths: &Paths) -> Result<ConfigLoad, ConfigError> {
        let path = paths.config_file();
        match read_and_parse(&path)? {
            Reading::Absent => Ok(ConfigLoad::Missing),
            Reading::Parsed(config) => Ok(ConfigLoad::Loaded(*config)),
            Reading::PlayerProblem(config, problem) => {
                // The file was read and is left exactly where it is: a person fixing the line needs to
                // see what was in it, and the run keeps everything the file named but the player's.
                Ok(ConfigLoad::PlayerProblem {
                    config: (*config).resolve(paths),
                    problem,
                })
            }
            Reading::Unparsable(error) => {
                // The file moves out of the way before anything can take its place.
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

    /// Reads one config file for a caller that is about to change it, leaving the file alone.
    ///
    /// `Ok(None)` when there is no file yet; `Ok(Some(config))` when the bytes read as a config; `Err`
    /// with one line saying what is wrong when the file cannot be opened or cannot be read as YAML.
    /// Nothing is renamed and nothing is written, which is the whole difference from
    /// [`load`](Self::load): a run has to start on *something*, so `load` keeps an unreadable file aside
    /// under another name and goes on with defaults. Something that edits the file has no basis for
    /// deciding what the file should have said, and writing its own defaults over bytes it never read
    /// would quietly drop everything else that was in them. `path` rather than [`Paths`] for the same
    /// reason - no library defaults are wanted here, and having them on hand is how a caller gets tempted
    /// to use them.
    pub fn load_for_edit(path: &Path) -> Result<Option<Config>, String> {
        match read_and_parse(path) {
            Ok(Reading::Absent) => Ok(None),
            Ok(Reading::Parsed(config)) => Ok(Some(*config)),
            // The file read as this config; the player's setting is not what a run may act on, but a
            // caller editing the file wants the bytes as written - including the bad line - to see and
            // to fix. The one line names the file and says the player's setting is what to fix, so the
            // person who got it knows where and what, and the edit is refused rather than rewritten.
            Ok(Reading::PlayerProblem(_, problem)) => Err(player_problem_line(path, &problem)),
            Ok(Reading::Unparsable(error)) => Err(problem_line(path, &error)),
            // A file that cannot be opened at all: the same reason `load` calls it an error, put in one
            // line instead of carrying an enum a caller only formats anyway.
            Err(problem) => Err(problem.to_string()),
        }
    }

    /// Writes the config to `path`, making its directory first if it is not
    /// there yet.
    ///
    /// The bytes go into `<file>.tmp` in the same directory (the rename cannot
    /// cross a filesystem), are synced, and only then are renamed over the
    /// target: a crash leaves either the old config or the new one, never half
    /// of each. A failure before the rename removes the temp file; a failed
    /// rename leaves it holding what we tried to save. A temp file left behind by
    /// such a failure is removed before the next save writes, so its mode is never
    /// the mode the config ends up with.
    ///
    /// On Unix the file and any directory this save makes are owner-only, because what goes in them
    /// is Argon2 password hashes and device public keys: the temp file is created 0600 rather than
    /// loosened after the write, and a new directory is 0700. A directory that was already there
    /// keeps the mode its owner gave it. Windows permissions do not work this way, so nothing
    /// changes there.
    ///
    /// What goes in is this config with `server.allowed_roots` unnamed when it is only the download
    /// folder [`resolve`] filled in: a run saves the config it resolved, and pinning the roots to
    /// that directory would leave the server's root behind the day the person moves `download_dir`.
    /// An empty list means "the download folder" on disk, which is what keeps following it. A config with
    /// no `server` section writes no `[server]` either: a settings save from the TUI does not put this
    /// library into serving, and a file written without one stays written without one.
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
            create_private_dir(parent).map_err(cannot_write)?;
        }
        replace_file(path, text.as_bytes()).map_err(cannot_write)
    }

    /// This config as it belongs in a file: the roots unnamed when they are the one directory
    /// [`resolve`](Self::resolve) would fill in anyway, and no `server` section added when this
    /// config has none. See [`save`](Self::save).
    fn for_the_file(&self) -> Config {
        let mut out = self.clone();
        let pins_only_downloads = out.server.as_ref().is_some_and(|server| {
            server.allowed_roots.len() == 1 && server.allowed_roots[0] == out.download_dir
        });
        if pins_only_downloads {
            // Writing the roots back when they are only what `resolve` fills in would pin the
            // server to today's download folder.
            out.server
                .as_mut()
                .expect("a section that pinned the roots")
                .allowed_roots
                .clear();
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

/// What reading one config file gave, before anybody decides what to do about a bad one. A config is a few
/// hundred bytes of everything the run works on, and the box keeps the "there are bytes" cases the same
/// size instead of making the empty one pay for all of them.
enum Reading {
    /// Nothing at that path.
    Absent,
    /// Bytes that read as this config.
    Parsed(Box<Config>),
    /// Bytes that read as this config, but the player's setting is not something a run may act on,
    /// with the one line that says what is wrong.
    PlayerProblem(Box<Config>, String),
    /// Bytes that are there and are not this config, with the parse failure that says so.
    Unparsable(serde_norway::Error),
}

/// Reads `path` and parses it: [`Reading::Absent`] for no file, [`Reading::Unparsable`] for bytes that
/// are not this config, and [`ConfigError`] only for a file that cannot be opened at all (no permission,
/// a directory in the way), where there is no way to tell "empty" from "unavailable". A file that reads
/// as this config but overruns the player's bounds is a [`Reading::PlayerProblem`], not an
/// [`Reading::Unparsable`]: the bytes were understood, so the file stays where it is and only the
/// player's setting is the part the run does not act on. This is the one place a config file is read;
/// [`Config::load`] and [`Config::load_for_edit`] differ in what they do with an unparsable file, not
/// in how they find out about it.
fn read_and_parse(path: &Path) -> Result<Reading, ConfigError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Reading::Absent),
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let config = match serde_norway::from_slice::<Config>(&bytes) {
        Ok(config) => config,
        Err(error) => return Ok(Reading::Unparsable(error)),
    };
    // The player's program and arguments end up on a process's argument line, so a file that overruns
    // them is not acted on: the config comes back with the player's setting at its default, and the
    // line saying why is the one the run shows. The file itself is read, and it stays where it is.
    if let Some(problem) = player_problem(&config) {
        let mut kept = config;
        kept.player = PlayerConfig::default();
        return Ok(Reading::PlayerProblem(Box::new(kept), problem));
    }
    Ok(Reading::Parsed(Box::new(config)))
}

/// What is wrong with a config's player setting, or `None` when the program and its arguments
/// may be put on a player's line: an empty program names nothing to start, and more than
/// [`MAX_PLAYER_ARGS`] arguments, or one longer than [`MAX_PLAYER_ARG_BYTES`] bytes, would fill
/// the argument line a playlist or a file is handed in.
fn player_problem(config: &Config) -> Option<String> {
    let player = &config.player;
    if player.program.is_empty() {
        return Some(
            "player.program is empty; it must be auto, mpv, vlc, or a program name or path"
                .to_string(),
        );
    }
    if player.args.len() > MAX_PLAYER_ARGS {
        return Some(format!(
            "player.args has {} entries; at most {MAX_PLAYER_ARGS} are allowed",
            player.args.len()
        ));
    }
    if player
        .args
        .iter()
        .any(|arg| arg.len() > MAX_PLAYER_ARG_BYTES)
    {
        return Some(format!(
            "an entry in player.args is longer than {MAX_PLAYER_ARG_BYTES} bytes"
        ));
    }
    None
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

/// One line about a config whose player's setting is not something a run may act on: the file's
/// path, and the player's value is what to fix. Unlike [`problem_line`] (a file that cannot be
/// read at all), this file *was* read and is left where it is, so the line says the file and the
/// setting to fix rather than a parse failure.
fn player_problem_line(path: &Path, problem: &str) -> String {
    format!(
        "{}: the player setting is not usable ({}); fix the player section and keep the file",
        path.display(),
        problem
    )
}

/// Writes `bytes` into `path` the way every featherbittorrent file is written: a temp
/// file beside the target (`<file>.tmp`), readable only by the owner on Unix, synced, then
/// renamed over it, so a reader sees either the old file or the new one and never half of
/// each. This is the product's one rule for writing a file atomically with private
/// permissions: the config save, the session state save ([`crate::LocalBackend`]) and the
/// desktop app's settings save all go through it. A leftover temp file from a failed write is
/// taken out of the way first, so its mode is never the mode the file ends up with. The
/// directory must already exist; [`create_private_dir`] makes it the same way.
pub fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_then_rename(&suffixed(path, TEMP_FILE_SUFFIX), path, bytes)
}

/// Writes `bytes` to `temp`, synced, then renames it over `path`. The temp file
/// is removed when the write fails; when only the rename failed it stays behind
/// holding what we tried to save.
fn write_then_rename(temp: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let written = (|| -> std::io::Result<()> {
        let mut file = create_private_file(temp)?;
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

/// Opens `temp` for writing so that the bytes are unreadable by anybody but the owner from the
/// first byte: mode 0600 at creation rather than chmod'd after the write, because a hash that sat
/// in a world-readable file was readable while it sat there. A leftover temp file is taken out of
/// the way first - opening over an existing file keeps the mode that file already had - and
/// `create_new` after the removal means the bytes land in a file this call made.
#[cfg(unix)]
fn create_private_file(temp: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    match std::fs::remove_file(temp) {
        Ok(()) => {}
        // No leftover is the normal case, not a failure.
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(source),
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        // A umask only clears bits, and 0600 has no group or other bit left to clear.
        .mode(0o600)
        .open(temp)
}

/// Opens `temp` for writing as it has always been opened on a platform whose permissions are not
/// per-file modes: nothing here tightens, and nothing fails that used to succeed.
#[cfg(not(unix))]
fn create_private_file(temp: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::create(temp)
}

/// Makes `dir` and every missing directory above it, owner-only on Unix for the same reason the
/// file inside it is: 0700 keeps another local user out of the folder holding the hashes. Only what
/// this call creates gets that mode; a directory that was already there keeps whatever its owner
/// chose for it. The directory half of the rule [`replace_file`] is the file half of: every
/// featherbittorrent save that may have to make its directory makes it here.
#[cfg(unix)]
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(dir)
}

/// Makes `dir` and every missing parent, as it always has on a platform where the mode a new
/// directory gets is not something this call decides.
#[cfg(not(unix))]
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(test)]
mod tests {
    use super::{
        AuthConfig, BAD_FILE_SUFFIX, Config, ConfigLoad, CookieSecure, DOWNLOADS_DIR_NAME, Limits,
        LogoSetting, PlayerConfig, ServerConfig, UserEntry, suffixed, user_download_dir,
        web_ui_env_override,
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

    /// The value `key` names in `text`, when a line of it names that key: the line, trimmed of its
    /// indentation, has to start `key:`. This asks about the YAML's own keys, which a plain
    /// `contains` on the whole file cannot: the same words turn up inside values, and the scratch
    /// test directory named after a key reaches a saved config as part of the `download_dir` value
    /// on any host with no Downloads folder for the default to name instead. `None` is what "no
    /// line names this key" looks like.
    fn yaml_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
        text.lines().find_map(|line| {
            line.trim_start()
                .strip_prefix(key)
                .and_then(|named| named.strip_prefix(':'))
                .map(|value| value.trim())
        })
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

    /// A caller that is about to *edit* the file gets the unreadable one as an error with the bytes left
    /// where they are: nothing is renamed and nothing is written, so a failed request cannot move the
    /// person's own config out from under them, and it has no defaults to overwrite it with either.
    #[test]
    fn load_for_edit_does_not_touch_the_file() {
        let dir = ScratchDir::new("config_load_for_edit");
        let path = dir.path().join("config.yaml");

        // No file yet: there is nothing to read, and `None` says that rather than inventing a config.
        assert_eq!(
            Config::load_for_edit(&path).expect("no file is not an error"),
            None,
        );

        fs::write(&path, "listen_port: 51413\n").expect("write a config");
        let loaded = Config::load_for_edit(&path)
            .expect("read a whole config")
            .expect("the file is there");
        assert_eq!(loaded.listen_port, 51413);

        fs::write(&path, "listen_port: [nope\n").expect("write a malformed config");
        let problem = Config::load_for_edit(&path).expect_err(
            "something that cannot guess a file's contents must not pretend to have read it",
        );
        assert_eq!(
            problem.lines().count(),
            1,
            "the problem is one line: {problem}"
        );
        assert!(problem.contains("config.yaml"), "{problem}");

        // The bytes are still there under the name they had, and nothing appeared beside them.
        assert_eq!(
            fs::read_to_string(&path).expect("the file is where it was"),
            "listen_port: [nope\n",
        );
        let names: Vec<String> = fs::read_dir(dir.path())
            .expect("read the directory")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["config.yaml"],
            "nothing was moved aside and nothing was written: {names:?}"
        );
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
    /// rest at these defaults, like every other part of it - but having a section at all is a
    /// choice, which [`a_save_without_a_server_section_writes_none`] is about.
    #[test]
    fn server_config_defaults() {
        let dir = ScratchDir::new("config_server");
        let paths = library(&dir);

        let defaults = ServerConfig::default();
        assert_eq!(
            defaults.bind,
            "127.0.0.1:8090".parse().expect("the default bind"),
            "the API listens on loopback unless the config says otherwise"
        );
        assert!(
            defaults.allowed_roots.is_empty(),
            "no roots are named until the run resolves the config: {:?}",
            defaults.allowed_roots
        );
        assert!(
            defaults.allowed_hosts.is_empty(),
            "nothing but localhost and an address is answered until the config names a host: {:?}",
            defaults.allowed_hosts
        );
        assert_eq!(defaults.auth.enabled, None);
        assert!(defaults.auth.users.is_empty());
        assert!(defaults.auth.devices.is_empty());

        assert!(
            !defaults.auth_on(),
            "a server on loopback does not need a login"
        );
        let localhost = ServerConfig {
            bind: "[::1]:8090".parse().expect("the IPv6 loopback"),
            ..ServerConfig::default()
        };
        assert!(!localhost.auth_on(), "::1 is loopback too");

        let public = ServerConfig {
            bind: "0.0.0.0:8090".parse().expect("a bind on every address"),
            ..ServerConfig::default()
        };
        assert!(
            public.auth_on(),
            "a server anyone can reach signs people in"
        );

        let mut turned_off = public.clone();
        turned_off.auth.enabled = Some(false);
        assert!(
            !turned_off.auth_on(),
            "only an explicit `enabled: false` turns authentication off"
        );

        let let_in = ServerConfig {
            auth: AuthConfig {
                enabled: Some(true),
                ..AuthConfig::default()
            },
            ..ServerConfig::default()
        };
        assert!(
            let_in.auth_on(),
            "and `enabled: true` keeps it on even on loopback"
        );

        // A file naming one server setting keeps the rest at the defaults above. Naming any part of it is
        // also what makes the section exist: there is no implicit one to overwrite.
        fs::write(
            paths.config_file(),
            "server:\n  bind: 0.0.0.0:8090\n  auth:\n    enabled: false\n",
        )
        .expect("write a config naming the server");
        let ConfigLoad::Loaded(config) = Config::load(&paths).expect("read it back") else {
            panic!("a config naming the server is still a config");
        };
        let named = config
            .server
            .expect("naming part of the server gives a section");
        assert_eq!(named.bind, public.bind);
        assert!(!named.auth_on());
        assert_eq!(
            named.allowed_roots, defaults.allowed_roots,
            "the part the file leaves out is default too"
        );

        // Host names come back exactly as the file wrote them, and resolving - the step that makes every
        // root absolute - leaves them alone: they are names a server answers, not folders to root in the
        // library. Nothing about them depends on where the library is.
        fs::write(
            paths.config_file(),
            "server:\n  allowed_hosts:\n    - bit.local\n    - LAN-Server\n",
        )
        .expect("write a config naming hosts");
        let ConfigLoad::Loaded(config) = Config::load(&paths).expect("read the host names back")
        else {
            panic!("a config naming hosts is still a config");
        };
        // Resolving - the step that makes every root absolute - keeps the section it was given.
        let resolved = config
            .clone()
            .resolve(&paths)
            .server
            .expect("resolving does not drop a server");
        let named = config.server.expect("hosts are part of the server section");
        assert_eq!(
            named.allowed_hosts,
            ["bit.local", "LAN-Server"],
            "spelling is kept: the comparison that uses these is the case-insensitive one"
        );
        assert_eq!(
            named.bind, defaults.bind,
            "a file naming only hosts leaves the rest alone"
        );
        assert_eq!(
            resolved.allowed_hosts, named.allowed_hosts,
            "resolving roots says nothing about names"
        );
        assert!(
            !resolved.allowed_hosts.is_empty(),
            "and it does not clear them either"
        );

        // And what a save writes is what the next run reads.
        let with_users = Config {
            server: Some(ServerConfig {
                bind: public.bind,
                allowed_roots: vec![paths.data_dir.join("share")],
                allowed_hosts: vec!["bit.local".to_string()],
                auth: AuthConfig {
                    enabled: Some(true),
                    users: vec![UserEntry {
                        name: "alice".to_string(),
                        password_hash: "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA".to_string(),
                    }],
                    devices: Vec::new(),
                },
                ..ServerConfig::default()
            }),
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

    /// The browser UI is on unless somebody turns it off. A config that says nothing about it
    /// leaves [`ServerConfig::web_ui_on`] answering yes, which is what a full build's `serve`
    /// does with it, and the file stays silent about a choice nobody made.
    #[test]
    fn web_ui_is_on_by_default() {
        let defaults = ServerConfig::default();
        assert_eq!(
            defaults.web_ui, None,
            "the file names the switch only when somebody chooses: {:?}",
            defaults.web_ui
        );
        assert!(
            defaults.web_ui_on(),
            "a server serves the browser UI unless the config turns it off"
        );
    }

    /// `web_ui: false` is the only word that turns the browser UI off, and it has to be said:
    /// an absence is yes, and `Some(true)` only spells out what silence already meant.
    #[test]
    fn the_config_turns_the_web_ui_off() {
        let off = ServerConfig {
            web_ui: Some(false),
            ..ServerConfig::default()
        };
        assert!(!off.web_ui_on(), "`web_ui: false` turns the browser UI off");
        let on = ServerConfig {
            web_ui: Some(true),
            ..ServerConfig::default()
        };
        assert!(
            on.web_ui_on(),
            "`web_ui: true` is the default spelled out, not a change"
        );
    }

    /// The words `FBTT_WEB_UI` takes, and only these: the off words mean off, the on words mean
    /// on, case-insensitively, and anything else - a stranger word, or no variable at all - is
    /// no answer, which leaves the config in charge. See [`web_ui_env_override`].
    #[test]
    fn the_environment_override_parses_its_words() {
        let env = |value: &'static str| {
            move |name: &str| (name == "FBTT_WEB_UI").then_some(value.to_string())
        };
        for word in ["0", "false", "off", "no"] {
            assert_eq!(
                web_ui_env_override(&env(word)),
                Some(false),
                "`{word}` means off"
            );
        }
        for word in ["1", "true", "on", "yes"] {
            assert_eq!(
                web_ui_env_override(&env(word)),
                Some(true),
                "`{word}` means on"
            );
        }
        // Case-insensitive: the same words spelled other ways answer the same.
        for (word, want) in [
            ("OFF", Some(false)),
            ("False", Some(false)),
            ("nO", Some(false)),
            ("ON", Some(true)),
            ("tRuE", Some(true)),
            ("YeS", Some(true)),
        ] {
            assert_eq!(
                web_ui_env_override(&env(word)),
                want,
                "`{word}` parses wrong"
            );
        }
        // A word this does not take is no answer at all rather than a guess.
        assert_eq!(
            web_ui_env_override(&env("maybe")),
            None,
            "`maybe` is neither on nor off, so it says nothing"
        );
        // No variable at all is the same no-answer.
        assert_eq!(
            web_ui_env_override(&|_name| None),
            None,
            "an unset FBTT_WEB_UI says nothing"
        );
    }

    /// The two new server keys round-trip: a file that names them reads as the config that
    /// writes them back, with the same lines the file had; and a config that names neither
    /// writes neither key, so a save never adds lines nobody chose.
    #[test]
    fn web_ui_and_cookie_secure_round_trip_in_yaml() {
        let dir = ScratchDir::new("config_web_ui_round_trip");
        let paths = library(&dir);
        let path = paths.config_file();

        fs::write(&path, "server:\n  web_ui: false\n  cookie_secure: always\n")
            .expect("write a config naming the two new keys");
        let ConfigLoad::Loaded(chosen) = Config::load(&paths).expect("read the new keys") else {
            panic!("a config naming the new keys is still a config");
        };
        let server = chosen.server.as_ref().expect("naming them gives a section");
        assert_eq!(server.web_ui, Some(false), "`web_ui: false` reads as off");
        assert_eq!(
            server.cookie_secure,
            CookieSecure::Always,
            "`cookie_secure: always` reads as always"
        );

        // What that config saves, the next run reads back unchanged, and both keys are in the
        // file the save wrote.
        chosen.save(&path).expect("save the chosen config");
        let written = fs::read_to_string(&path).expect("read the save back");
        // Asked as YAML keys, not as words anywhere in the file: this file also names the scratch
        // folder it was saved from, and that folder is named after this test.
        assert_eq!(
            yaml_value(&written, "web_ui"),
            Some("false"),
            "the switch is written back as the config has it: {written}"
        );
        assert_eq!(
            yaml_value(&written, "cookie_secure"),
            Some("always"),
            "the cookie rule is spelled in lowercase: {written}"
        );
        let ConfigLoad::Loaded(again) = Config::load(&paths).expect("read the save back") else {
            panic!("the save wrote a config");
        };
        assert_eq!(again, chosen, "the round trip is the identity");

        // And a config that chose neither key writes neither line: the default switch is on
        // without saying so, and the default cookie rule is auto without saying so.
        let quiet = Config {
            server: Some(ServerConfig::default()),
            ..Config::default()
        }
        .resolve(&paths);
        quiet
            .save(&path)
            .expect("save a config that chose neither key");
        let written = fs::read_to_string(&path).expect("read the silent save back");
        assert_eq!(
            yaml_value(&written, "web_ui"),
            None,
            "no switch line for a config that never chose one: {written}"
        );
        assert_eq!(
            yaml_value(&written, "cookie_secure"),
            None,
            "no cookie line for the rule nobody changed: {written}"
        );
    }

    /// A config that never chose to serve - which is what the TUI saves when somebody changes a rate or a
    /// download folder - writes no `[server]` at all and reads back with none. That is the difference
    /// between "this library has a server on it, at this address" and "nobody has said so": the machine
    /// that finds the library locked may only name an address once the first is true (see `named_server`
    /// in the binary, and `second_tui_on_locked_library_exits_3` beside the `serve` tests).
    #[test]
    fn a_save_without_a_server_section_writes_none() {
        let dir = ScratchDir::new("config_no_server");
        let paths = library(&dir);
        let path = paths.config_file();

        // What a settings change saves: another port, and the same absence of a server.
        let config = Config {
            listen_port: 51_999,
            limits: Limits {
                download_kib: 512,
                ..Limits::default()
            },
            ..Config::default()
        };
        config.save(&path).expect("save a settings-only config");

        let written = fs::read_to_string(&path).expect("read the file back");
        // A `server:` line, not the word anywhere in the file: this scratch folder is named after
        // a server that is absent, and on a host without a Downloads folder the file carries that
        // name in its `download_dir` value.
        assert_eq!(
            yaml_value(&written, "server"),
            None,
            "no server section appears in a config that never chose one: {written}"
        );
        let ConfigLoad::Loaded(loaded) = Config::load(&paths).expect("read it back") else {
            panic!("a settings-only config is still a config");
        };
        assert_eq!(loaded, config);
        assert_eq!(loaded.listen_port, 51_999);
        assert_eq!(
            loaded.server, None,
            "and the run that reads it has no address to offer anybody"
        );

        // Resolving such a config invents no section either: roots belong to a server that is going to
        // run, and this library is not running one.
        assert_eq!(loaded.resolve(&paths).server, None);

        // A file that did name a server keeps it through a save made by somebody changing only a rate:
        // the section is what the person chose, not something a settings save drops.
        fs::write(&path, "server:\n  bind: 127.0.0.1:8099\n")
            .expect("write a config naming a server");
        let mut edited = Config::load_for_edit(&path)
            .expect("read it for editing")
            .expect("the file is there");
        edited.listen_port = 6882;
        edited.save(&path).expect("save the edit");
        let ConfigLoad::Loaded(loaded) = Config::load(&paths).expect("read the edit back") else {
            panic!("a config naming a server is still a config");
        };
        assert_eq!(
            loaded.server.expect("the section survives").bind,
            "127.0.0.1:8099"
                .parse()
                .expect("the address the file named"),
            "a settings save keeps the serving somebody else chose"
        );
    }

    /// The folders the API may touch: none named means the download folder alone, and a named
    /// one follows `download_dir`'s rule - relative to the library, absolute left alone. Only a
    /// config that has a server section gets roots at all (see
    /// [`a_save_without_a_server_section_writes_none`]).
    #[test]
    fn allowed_roots_default_to_the_download_dir() {
        let dir = ScratchDir::new("config_allowed_roots");
        let paths = library(&dir);

        let nothing_named = Config {
            server: Some(ServerConfig::default()),
            ..Config::default()
        }
        .resolve(&paths);
        let downloads = nothing_named.download_dir.clone();
        assert_eq!(
            nothing_named
                .server
                .expect("the section was given")
                .allowed_roots,
            [downloads],
            "with no roots in the config the download folder is the one root"
        );

        let named = Config {
            server: Some(ServerConfig {
                allowed_roots: vec![PathBuf::from("share"), paths.data_dir.join("elsewhere")],
                ..ServerConfig::default()
            }),
            ..Config::default()
        }
        .resolve(&paths);
        let roots = named
            .server
            .as_ref()
            .expect("the section was given")
            .allowed_roots
            .clone();
        assert_eq!(
            roots,
            [
                paths.data_dir.join("share"),
                paths.data_dir.join("elsewhere")
            ],
            "a relative root hangs below the library and an absolute one stays as written"
        );

        // Resolving twice changes nothing, as it does for `download_dir`.
        assert_eq!(
            named
                .resolve(&paths)
                .server
                .expect("still there")
                .allowed_roots,
            roots
        );
    }

    /// What the media player is told to be: `auto` lets the run find mpv or VLC itself,
    /// no extra arguments, and the logo is on. A config file that names none of these
    /// gets these.
    #[test]
    fn player_config_defaults() {
        let dir = ScratchDir::new("config_player_defaults");
        let paths = library(&dir);

        let loaded = match Config::load(&paths).expect("a missing file is still read") {
            ConfigLoad::Missing => Config::default(),
            ConfigLoad::Loaded(config) => config,
            ConfigLoad::PlayerProblem { config, .. } | ConfigLoad::Bad { config, .. } => config,
        };
        assert_eq!(
            loaded.player.program, "auto",
            "a library nobody set up finds a player itself"
        );
        assert!(
            loaded.player.args.is_empty(),
            "no extra arguments until the config names some"
        );
        assert_eq!(
            loaded.logo,
            LogoSetting::Auto,
            "the logo is drawn when the terminal can show it"
        );

        // A file that names only the program keeps the rest at the defaults above.
        fs::write(paths.config_file(), "player:\n  program: vlc\nlogo: off\n")
            .expect("write a player config");
        let ConfigLoad::Loaded(config) = Config::load(&paths).expect("read the player config")
        else {
            panic!("a player config is still a config");
        };
        assert_eq!(config.player.program, "vlc");
        assert!(config.player.args.is_empty());
        assert_eq!(config.logo, LogoSetting::Off);
    }

    /// What the player is allowed to be told: at most 32 extra arguments, each at most
    /// 1024 bytes, and a program that is not empty. Anything past that is reported in one
    /// line; the file itself is left exactly where it is, with everything else kept, and
    /// only the player's setting comes back at its default.
    #[test]
    fn player_config_rejects_oversized_args() {
        let dir = ScratchDir::new("config_player_oversized");
        let paths = library(&dir);

        // One argument that is 1025 bytes is one byte too many to put on a player's line.
        let long = "x".repeat(1025);
        fs::write(
            paths.config_file(),
            format!("player:\n  program: mpv\n  args:\n    - {long}\n"),
        )
        .expect("write an oversized argument");
        let ConfigLoad::PlayerProblem { problem, .. } =
            Config::load(&paths).expect("an oversized argument is reported")
        else {
            panic!("a 1025-byte argument is refused, not swallowed");
        };
        assert_eq!(
            problem.lines().count(),
            1,
            "the problem is one line: {problem}"
        );
        assert!(
            problem.contains("1024"),
            "the limit is named in the line: {problem}"
        );

        // Thirty-three arguments are one too many to hold in a playlist's arguments.
        let many = (0..33)
            .map(|slot| format!("  - a{slot}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            paths.config_file(),
            format!("player:\n  program: mpv\n  args:\n{many}\n"),
        )
        .expect("write too many arguments");
        let ConfigLoad::PlayerProblem { problem, .. } =
            Config::load(&paths).expect("too many arguments are reported")
        else {
            panic!("33 arguments are refused, not swallowed");
        };
        assert_eq!(
            problem.lines().count(),
            1,
            "the problem is one line: {problem}"
        );
        assert!(
            problem.contains("32"),
            "the limit is named in the line: {problem}"
        );

        // An empty program names no player at all, which is nothing to start.
        fs::write(paths.config_file(), "player:\n  program: \"\"\n")
            .expect("write an empty program");
        let ConfigLoad::PlayerProblem { problem, .. } =
            Config::load(&paths).expect("an empty program is reported")
        else {
            panic!("an empty program is refused, not swallowed");
        };
        assert_eq!(
            problem.lines().count(),
            1,
            "the problem is one line: {problem}"
        );
    }

    /// A bad `player` setting is a problem with one value, not with the whole file: the server
    /// section and every other setting stay as the file has them, the player's comes back at
    /// its default, and the file itself is not renamed aside. A genuinely unparsable file is
    /// still set aside, which is the point this test guards against.
    #[test]
    fn a_player_problem_keeps_the_config() {
        let dir = ScratchDir::new("config_player_problem_keeps");
        let paths = library(&dir);

        // A server that names a bind, and a player whose program is empty.
        fs::write(
            paths.config_file(),
            "server:\n  bind: 127.0.0.1:8099\nplayer:\n  program: \"\"\n",
        )
        .expect("write a config with a bad player");

        let ConfigLoad::PlayerProblem { config, problem } =
            Config::load(&paths).expect("the file is read, not refused")
        else {
            panic!("a player problem keeps the config, it does not set it aside");
        };

        // The server section is exactly as the file has it.
        let server = config
            .server
            .as_ref()
            .expect("the server section survives a bad player");
        assert_eq!(
            server.bind,
            "127.0.0.1:8099"
                .parse()
                .expect("the address the file named"),
            "the server's bind is kept as the file named it"
        );

        // The player's setting is at its default: the default program, no extra arguments.
        assert_eq!(
            config.player,
            PlayerConfig::default(),
            "only the player's setting is reset"
        );

        // One line saying what is wrong with the player's setting.
        assert_eq!(problem.lines().count(), 1, "one line: {problem}");

        // The file itself is where it started: nothing was renamed.
        assert!(
            paths.config_file().exists(),
            "the config file is still there"
        );
        assert!(
            !suffixed(&paths.config_file(), BAD_FILE_SUFFIX).exists(),
            "no .bad file was created"
        );
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
            server: Some(ServerConfig {
                auth: AuthConfig {
                    enabled: Some(true),
                    users: vec![user.clone()],
                    ..AuthConfig::default()
                },
                ..ServerConfig::default()
            }),
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

    /// The config file holds Argon2 password hashes and device public keys, so another local user
    /// must not be able to read it. Owner-only from the first byte: 0600 is whatever the umask is,
    /// because a umask only clears bits and 0600 has no group or other bits left to clear.
    #[cfg(unix)]
    #[test]
    fn saved_config_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = ScratchDir::new("config_owner_only");
        let path = dir.path().join("config.yaml");

        Config::default().save(&path).expect("save the defaults");

        let mode = fs::metadata(&path)
            .expect("the config is there")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the file holds the hashes: {:o}",
            mode & 0o777
        );
    }

    /// A `<file>.tmp` left by a save that failed halfway may carry looser permissions, and opening
    /// over it keeps the mode the leftover already had. The save takes the leftover out of the way
    /// first, so the file the hashes land in is one this save created.
    #[cfg(unix)]
    #[test]
    fn save_tightens_a_loose_leftover_tmp() {
        use super::TEMP_FILE_SUFFIX;
        use std::os::unix::fs::PermissionsExt;

        let dir = ScratchDir::new("config_loose_tmp");
        let path = dir.path().join("config.yaml");
        let temp = suffixed(&path, TEMP_FILE_SUFFIX);
        fs::write(&temp, "leftover from a save that failed\n").expect("leave a temp file behind");
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o644))
            .expect("loosen it, as an older run's umask would have");

        Config::default()
            .save(&path)
            .expect("save over the leftover temp file");

        let mode = fs::metadata(&path)
            .expect("the config is there")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "a loose leftover must not set the mode of the config: {:o}",
            mode & 0o777
        );
        assert!(
            !temp.exists(),
            "the leftover is gone rather than renamed onto the target: {}",
            temp.display()
        );
    }

    /// The directory a first save makes holds that file, so it is made owner-only too: 0700, which
    /// also keeps another local user from listing the name. A directory that was already there
    /// keeps the mode its owner gave it; this only decides the mode of what it creates.
    #[cfg(unix)]
    #[test]
    fn new_config_dir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = ScratchDir::new("config_new_dir_mode");
        let nested = dir.path().join("nested");
        let path = nested.join("config.yaml");

        Config::default()
            .save(&path)
            .expect("save into a directory that does not exist yet");

        let mode = fs::metadata(&nested)
            .expect("the directory was made")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "the config directory holds the hashes: {:o}",
            mode & 0o777
        );

        // A directory that was already there is left as its owner set it, even when that is looser.
        let chosen = dir.path().join("chosen");
        fs::create_dir(&chosen).expect("make the directory first");
        fs::set_permissions(&chosen, fs::Permissions::from_mode(0o755)).expect("loosen it");
        Config::default()
            .save(&chosen.join("config.yaml"))
            .expect("save into a directory that exists already");
        let mode = fs::metadata(&chosen)
            .expect("the directory is still there")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o755,
            "an existing directory's own mode stands: {:o}",
            mode & 0o777
        );
    }
}
