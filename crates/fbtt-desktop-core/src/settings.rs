//! The desktop app's own settings file: `desktop.yaml` in [`DesktopPaths::config_dir`].
//!
//! What is in it is what a person chooses in the desktop Settings screen and nothing else: what closing
//! the window does, whether the app starts at login, whether the window takes the system's title bar,
//! which instance the app connects to when it opens, the remote instances, the shortcut overrides, and
//! whether This computer serves the web UI to other machines. Bind addresses, logins and allowed folders
//! for serving are *not* here: those live in the library's own `config.yaml` `server:` section
//! ([`fbtt_core::ServerConfig`]), so serving from This computer follows exactly the rules `serve` follows
//! and a person moving a download folder changes it in one place.
//!
//! Two rules about the file, because a file this short is a file people edit by hand:
//!
//! - A missing file is [`DesktopSettings::default`], and an empty one is the same thing. There is no
//!   first-run wizard to fill in: every setting has a default the app is happy to run on.
//! - A file that does not parse is an [`SettingsError`] the shell shows. Nothing is written and nothing
//!   is moved aside: unlike the CLI's config, which has to run on *something* and so keeps the bad file
//!   under another name, this load has no basis for guessing what the bytes meant, and its own defaults
//!   written over them would quietly drop everything the person had set.
//!
//! A key this version does not know is kept: it comes back on the next save exactly as it was read, so
//! an app from an older release does not erase a newer one's settings, and a typo costs a line in a log
//! rather than the setting itself.
//!
//! [`DesktopPaths::config_dir`]: crate::paths::DesktopPaths::config_dir

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fbtt_core::config::{create_private_dir, replace_file};
use serde::{Deserialize, Serialize};
use serde_norway::Value;
use thiserror::Error;

use crate::instances::{Instance, LOCAL_ID};

/// What can go wrong reading or writing the settings file.
///
/// A file that is not there is none of these: it is the defaults (see [`DesktopSettings::load`]).
#[derive(Debug, Error)]
pub enum SettingsError {
    /// The file could not be opened at all: no permission, or a directory where the file should be.
    /// There is no way to tell that from an empty file, so it is an error rather than a default.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file we tried to read.
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file is there and is not this settings file. One line naming the file and saying what is
    /// wrong, fit to show the person who wrote it; the bytes themselves are left where they are.
    #[error("{path}: {problem}")]
    Unparsable {
        /// The file we could not read as settings.
        path: PathBuf,
        /// What is wrong with it, folded onto one line.
        problem: String,
    },
    /// The file could not be written or moved into place. It names the file the run wanted in place, not
    /// the temp file behind it.
    #[error("cannot write {path}: {source}")]
    Write {
        /// The file we tried to put in place.
        path: PathBuf,
        source: std::io::Error,
    },
}

/// What closing the window does.
///
/// `Ask` is the first run's answer and the only one that asks anything: the app asks once, saves the
/// answer here, and never asks again (design spec 2.5). `Tray` is the one a system without a tray does
/// not offer, and the shell is what knows whether this one has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloseAction {
    /// Ask what closing should do, the first time, and remember the answer.
    #[default]
    Ask,
    /// Quit the app.
    Quit,
    /// Hide the window and keep running in the tray.
    Tray,
}

/// Whether This computer serves the web UI and API to other machines.
///
/// Only the switch is here. The bind address, the logins, the allowed hosts and the folders the API may
/// name are the library's own `server:` section ([`fbtt_core::ServerConfig`]), which is what `serve`
/// follows: turning this on turns on those rules rather than a second, smaller set of them, and a person
/// who serves from this machine edits one file and sees the same behavior from `serve` and from the app.
/// Off by default, because nothing is exposed by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServeSettings {
    /// Whether the web UI and API are served beyond `127.0.0.1`.
    pub enabled: bool,
}

/// One keyboard shortcut a person has changed.
///
/// The field is `group` in Rust and `where` in the file and across the bridge, because the page's binding
/// list is keyed by the screen an action lives on and one spelling at both ends is the point: the file a
/// person edits, the JSON the shell hands the page and the page's own type are the same shape.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutOverride {
    /// The screen the action belongs to. The file and the page call this `where`.
    #[serde(rename = "where")]
    pub group: String,
    /// The action whose keys this replaces, named the way the page's binding list names it. An action a
    /// newer version renamed or dropped is ignored by the page rather than refused here, so an older app
    /// can read a newer file.
    pub action: String,
    /// The keys that mean it now, spelled the way the page spells keys.
    pub keys: Vec<String>,
}

/// What the desktop app runs on: `desktop.yaml` as one file.
///
/// Every field has a default, so a file that names one setting keeps the rest as they are, and a file that
/// is not there at all is this struct (see [`load`](Self::load)). `default_instance` is the id of the
/// instance to connect to when the app opens, and `local` (This computer) is the default: the app's own
/// engine needs no entry in `instances` to be an instance, which is why `instances` holds only the remote
/// ones and why [`default_or_local`](crate::DesktopSettings::default_or_local) is what everything reads
/// that decides where to connect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopSettings {
    /// What closing the window does. See [`CloseAction`].
    pub close: CloseAction,
    /// Whether the app puts itself in the system's login items. Off unless the person turns it on, and
    /// the entry itself is written by the app at runtime, never by a package.
    pub start_at_login: bool,
    /// Whether the window takes the system's title bar instead of drawing its own. The fallback the Linux
    /// design asks for, so it is off by default everywhere and offered where a drawn bar can work.
    pub system_title_bar: bool,
    /// The instance to connect to when the app opens: `local`, or the id of one of `instances`.
    pub default_instance: String,
    /// The remote instances, added by address. This computer is not in this list; it is `local`.
    pub instances: Vec<Instance>,
    /// The shortcuts a person changed, applied by the page over its own binding list.
    pub shortcuts: Vec<ShortcutOverride>,
    /// Whether this machine's torrents are served to other machines. See [`ServeSettings`].
    pub serve: ServeSettings,
    /// The keys this version does not know, kept as they were read and written back on the next save.
    ///
    /// A file written by a newer app, or by a person who left a note in it, keeps every part of itself:
    /// a key nobody recognised is not evidence about what it meant, and dropping it would be this version
    /// erasing a setting it has never seen. Flattened, so these keys sit at the top level of the file and
    /// of the JSON the page gets, exactly where they were.
    #[serde(flatten)]
    pub unknown_keys: BTreeMap<String, Value>,
}

impl Default for DesktopSettings {
    /// The settings an app with no settings file runs on: ask about closing, no login item, the app's own
    /// title bar, nothing served, no remote instances, and This computer as the instance to open on.
    fn default() -> Self {
        DesktopSettings {
            close: CloseAction::Ask,
            start_at_login: false,
            system_title_bar: false,
            default_instance: LOCAL_ID.to_string(),
            instances: Vec::new(),
            shortcuts: Vec::new(),
            serve: ServeSettings::default(),
            unknown_keys: BTreeMap::new(),
        }
    }
}

impl DesktopSettings {
    /// Reads the settings file at `path`.
    ///
    /// No file is [`DesktopSettings::default`], and a file holding nothing but blank lines is the same:
    /// an empty file names no settings, and making it a parse failure would have a person explain a
    /// truncation to themselves. A file that does not parse is [`SettingsError::Unparsable`] and nothing
    /// else happens - nothing is written and nothing is renamed, which is the difference from the CLI's
    /// [`fbtt_core::Config::load`]: a run of the CLI has to start on *something*, so it moves the file it
    /// could not read aside and goes on with defaults rooted in that library. This load has no such
    /// defaults on hand and no basis for guessing what the bytes meant, and writing its own defaults over
    /// them would drop every setting the person had actually set. The shell shows the line this returns
    /// and works on whatever it decides to work on.
    ///
    /// A file that cannot be opened at all (no permission, a directory in the way) is
    /// [`SettingsError::Io`], because there is no way to tell "empty" from "unavailable".
    pub fn load(path: &Path) -> Result<DesktopSettings, SettingsError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            // A library nobody has opened the app in yet.
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DesktopSettings::default());
            }
            Err(source) => {
                return Err(SettingsError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        // The check a YAML parser would make for us, made here so the answer for "no bytes" is the same
        // as for "no file" instead of a parse error.
        if bytes.iter().all(|byte| byte.is_ascii_whitespace()) {
            return Ok(DesktopSettings::default());
        }
        serde_norway::from_slice::<DesktopSettings>(&bytes).map_err(|error| {
            SettingsError::Unparsable {
                path: path.to_path_buf(),
                problem: problem_line(&error),
            }
        })
    }

    /// Writes the settings to `path`, making its directory first if it is not there yet.
    ///
    /// The write follows the product's one rule for files, [`fbtt_core::config::replace_file`] and
    /// [`fbtt_core::config::create_private_dir`]: a synced temp file beside the target renamed over it,
    /// owner-only on Unix along with any directory this save makes, because the same directory holds this
    /// library's `config.yaml`, whose password hashes are somebody else's business to keep out of.
    ///
    /// What goes in is every key that came out of the file, including the ones this version did not
    /// understand, so saving cannot erase a newer app's settings.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let cannot_write = |source| SettingsError::Write {
            path: path.to_path_buf(),
            source,
        };
        let text = serde_norway::to_string(self)
            .map_err(|error| {
                std::io::Error::other(format!("the settings cannot be written as YAML: {error}"))
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
}

/// One line about a parse failure: the whole message with its newlines folded into spaces, so the shell
/// has one line to show. The file's name comes from [`SettingsError::Unparsable`], which carries the path.
fn problem_line(error: &serde_norway::Error) -> String {
    let message = error
        .to_string()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");
    if message.is_empty() {
        "malformed YAML".to_string()
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::{CloseAction, DesktopSettings, ShortcutOverride};
    use crate::paths::DesktopPaths;
    use crate::{Instance, LOCAL_ID};
    use fbtt_core::scratch::ScratchDir;
    use serde_norway::Value;
    use std::fs;
    use std::path::PathBuf;

    /// The file the shell would be pointed at: `DesktopPaths::settings_file` for a library in a scratch
    /// directory, so these tests read and write exactly the path a run reads and writes.
    fn settings_file(dir: &ScratchDir) -> PathBuf {
        let paths = DesktopPaths::resolve_with(|key| {
            if key == "FBTT_DESKTOP_CONFIG_DIR" || key == "FBTT_DESKTOP_DATA_DIR" {
                Some(dir.path().into())
            } else {
                None
            }
        })
        .expect("both directories are given");
        paths.settings_file()
    }

    /// The value `key` names at the top level of `text`, when a line of it names that key. This asks
    /// about the file's own keys, which a plain `contains` on the whole file cannot: the same words turn
    /// up inside values. `None` is what "no line names this key" looks like.
    fn yaml_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
        text.lines().find_map(|line| {
            line.trim_start()
                .strip_prefix(key)
                .and_then(|named| named.strip_prefix(':'))
                .map(|value| value.trim())
        })
    }

    /// A first run has no settings file, and that is not an error: every setting has a default the app is
    /// happy to run on, including the default instance, which is This computer until the person changes
    /// it.
    #[test]
    fn missing_file_is_default_settings() {
        let dir = ScratchDir::new("desktop_settings_missing");
        let path = settings_file(&dir);

        let settings =
            DesktopSettings::load(&path).expect("a file nobody has written yet is not an error");
        assert_eq!(settings, DesktopSettings::default());
        assert_eq!(settings.close, CloseAction::Ask);
        assert!(!settings.start_at_login);
        assert!(!settings.system_title_bar);
        assert_eq!(settings.default_instance, LOCAL_ID);
        assert!(
            settings.instances.is_empty(),
            "This computer is not in the list"
        );
        assert!(settings.shortcuts.is_empty());
        assert!(!settings.serve.enabled, "nothing is exposed by default");
        assert!(settings.unknown_keys.is_empty(), "nothing was read");
        assert!(!path.exists(), "reading does not create the file");
    }

    /// A file that was truncated, or that a person emptied, holds no settings: the same answer as no file
    /// at all, not a parse failure a person has to explain to themselves.
    #[test]
    fn an_empty_file_is_the_same_as_no_file() {
        let dir = ScratchDir::new("desktop_settings_empty");
        let path = settings_file(&dir);
        fs::write(&path, "\n").expect("write an empty file");

        let settings = DesktopSettings::load(&path).expect("an empty file is not a broken one");
        assert_eq!(settings, DesktopSettings::default());
    }

    /// Bytes nobody could read are an error the shell shows, and they stay exactly where they are. This
    /// load has no defaults on hand to overwrite them with and no business guessing what they meant, so
    /// unlike the CLI's config nothing is renamed and nothing is written.
    #[test]
    fn unparsable_file_is_an_error_and_is_not_overwritten() {
        let dir = ScratchDir::new("desktop_settings_unparsable");
        let path = settings_file(&dir);
        fs::write(&path, "close: [nope\n").expect("write a malformed file");

        let problem = DesktopSettings::load(&path)
            .expect_err("bytes this version could not read are not something to guess at");
        let line = problem.to_string();
        assert_eq!(
            line.lines().count(),
            1,
            "the shell shows this as one line: {line}"
        );
        assert!(
            line.contains("desktop.yaml"),
            "the line names the file: {line}"
        );

        // The bytes are still there under the name they had, and nothing appeared beside them.
        assert_eq!(
            fs::read_to_string(&path).expect("the file is where it was"),
            "close: [nope\n",
        );
        let names: Vec<String> = fs::read_dir(dir.path())
            .expect("read the directory")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["desktop.yaml"],
            "nothing was moved aside and nothing was written: {names:?}"
        );

        // A well-formed file naming a value this version does not offer is refused the same way rather
        // than run on a guess: `snooze` is not something closing the window can do.
        fs::write(&path, "close: snooze\n").expect("write a word nobody offers");
        assert!(
            DesktopSettings::load(&path).is_err(),
            "an unknown close action is a question, not a default"
        );
    }

    /// A key this version does not know is kept and written back as it was read, so an app from an older
    /// release does not erase a newer one's settings, and a known key is still written exactly once.
    #[test]
    fn unknown_fields_are_kept_on_save() {
        let dir = ScratchDir::new("desktop_settings_unknown_fields");
        let path = settings_file(&dir);
        fs::write(
            &path,
            "close: tray\ntheme: midnight\nwindow:\n  width: 900\n  titles: [a, b]\n",
        )
        .expect("write a file from a newer app");

        let settings = DesktopSettings::load(&path)
            .expect("a key this version does not know is kept, not refused");
        assert_eq!(settings.close, CloseAction::Tray);
        assert!(
            matches!(settings.unknown_keys.get("theme"), Some(Value::String(text)) if text == "midnight"),
            "the scalar is kept as it was read"
        );
        assert!(
            matches!(settings.unknown_keys.get("window"), Some(Value::Mapping(_))),
            "a whole section this version does not know is kept too"
        );

        settings.save(&path).expect("save what a newer app wrote");
        let text = fs::read_to_string(&path).expect("read back what was saved");
        assert_eq!(yaml_value(&text, "close"), Some("tray"), "{text}");
        assert_eq!(yaml_value(&text, "theme"), Some("midnight"), "{text}");
        assert!(text.contains("width: 900"), "{text}");
        assert!(text.contains("titles:"), "{text}");
        assert_eq!(
            text.matches("close: ").count(),
            1,
            "a known key is written once: {text}"
        );

        // And the round trip is stable: reading the saved file gives the same settings, unknown keys and
        // all, so a second save by this version writes the same bytes over them.
        let again = DesktopSettings::load(&path).expect("read the file this app wrote");
        assert_eq!(again, settings);
    }

    /// The file spells the setting the setting spells: `ask`, `quit` and `tray`, and nothing else, and a
    /// file that names one setting keeps every other one at its default.
    #[test]
    fn close_action_round_trips_as_ask_quit_tray() {
        let dir = ScratchDir::new("desktop_settings_close_action");
        let path = settings_file(&dir);
        for (action, word) in [
            (CloseAction::Ask, "ask"),
            (CloseAction::Quit, "quit"),
            (CloseAction::Tray, "tray"),
        ] {
            let settings = DesktopSettings {
                close: action,
                ..DesktopSettings::default()
            };
            settings.save(&path).expect("save the setting");
            let text = fs::read_to_string(&path).expect("read back what was saved");
            assert_eq!(
                yaml_value(&text, "close"),
                Some(word),
                "{word} is spelled {word} in the file"
            );
            let loaded = DesktopSettings::load(&path).expect("read it back");
            assert_eq!(loaded, settings, "{word}");
        }

        fs::write(&path, "close: quit\n").expect("write a hand-edited file");
        let loaded = DesktopSettings::load(&path).expect("read a file that names one setting");
        assert_eq!(
            loaded,
            DesktopSettings {
                close: CloseAction::Quit,
                ..DesktopSettings::default()
            },
        );
    }

    /// What the app saves is what the next run reads back: the instances, the overrides under the page's
    /// own `where` key, the serving switch, and everything else.
    #[test]
    fn everything_round_trips() {
        let dir = ScratchDir::new("desktop_settings_round_trip");
        let path = settings_file(&dir);
        let settings = DesktopSettings {
            close: CloseAction::Tray,
            start_at_login: true,
            system_title_bar: true,
            default_instance: "pi-1".to_string(),
            instances: vec![
                Instance {
                    id: "pi-1".to_string(),
                    name: "Pi at home".to_string(),
                    url: "https://pi.local:8090".to_string(),
                    save_password: true,
                    user: Some("alice".to_string()),
                },
                Instance {
                    id: "lan-2".to_string(),
                    name: "Studio".to_string(),
                    url: "http://studio.local:8090".to_string(),
                    save_password: false,
                    user: None,
                },
            ],
            shortcuts: vec![
                ShortcutOverride {
                    group: "torrents".to_string(),
                    action: "pause".to_string(),
                    keys: vec!["space".to_string()],
                },
                ShortcutOverride {
                    group: "global".to_string(),
                    action: "open_settings".to_string(),
                    keys: vec!["ctrl+comma".to_string(), "ctrl+shift+comma".to_string()],
                },
            ],
            serve: crate::ServeSettings { enabled: true },
            unknown_keys: Default::default(),
        };

        settings.save(&path).expect("save everything");
        let text = fs::read_to_string(&path).expect("read back what was saved");
        assert!(
            text.contains("where: torrents"),
            "the file key is the page's key: {text}"
        );
        assert!(!text.contains("group:"), "there is no group key: {text}");
        assert_eq!(yaml_value(&text, "start_at_login"), Some("true"), "{text}");
        assert_eq!(
            yaml_value(&text, "default_instance"),
            Some("pi-1"),
            "{text}"
        );
        assert!(text.contains("enabled: true"), "{text}");
        assert!(text.contains("ctrl+shift+comma"), "{text}");

        let loaded = DesktopSettings::load(&path).expect("read it back");
        assert_eq!(loaded, settings);
    }
}
