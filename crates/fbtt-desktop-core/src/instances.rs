//! The instances the app can connect to, and the rules for adding and removing one.
//!
//! "This computer" is not in the list: it is the engine the app runs itself, always available, always the
//! fallback ([`LOCAL_ID`]). What `DesktopSettings::instances` holds is the remote ones - another
//! featherbittorrent `serve` on the network, or one running on this same machine, which is a separate
//! instance by the design spec's rule and is added by address like any other.
//!
//! The rules here are the ones a person editing the file by hand and a person pressing Add in the
//! window have to agree on: an address this client can actually dial, a name that is not empty, not
//! longer than the row that shows it, not already used by another instance reading the same way, and not
//! the app's own instance's name. An id is random so two people adding the same server on two machines
//! never end up with ids that mean different things.
//!
//! [`LOCAL_ID`]: LOCAL_ID

use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::settings::DesktopSettings;

/// The id that means This computer, the instance the app runs itself.
///
/// It is in no list, because the app's own engine needs no entry to be an instance, and no saved instance
/// is ever given it: another featherbittorrent on this machine is another instance, added by address, and
/// calling it `local` would make the switcher and the settings file disagree about which one the app is.
pub const LOCAL_ID: &str = "local";

/// The name of the app's own instance, the one the switcher always shows: never a name to save, so an
/// instance cannot be added that the switcher would then show twice.
const LOCAL_NAME: &str = "This computer";

/// The most characters an instance name may have: the row in the switcher that shows it is about this
/// wide, and a name that does not fit is not a name a person can tell two others apart from.
const MAX_NAME_CHARS: usize = 64;

/// The bytes an instance id is made of: twelve url-safe characters, no padding, and enough difference
/// between two ids drawn in the same second that adding the same server twice is one id and not a
/// collision.
const ID_BYTES: usize = 9;

/// One instance the app can connect to: this app's own engine, or another featherbittorrent reached over
/// the network by address.
///
/// No credential is in here. A saved password lives in the operating system's keychain and a remote
/// session's bearer token lives in the page's memory (plan Global Constraints), so `save_password` is a
/// flag saying whether to look in the keychain and `user` is the name to look it up under, neither one
/// anything a file or a log line would be sorry to hold.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Instance {
    /// The id this settings file and every command name it by: random, so two people adding the same
    /// server on two machines never end up with ids that mean different things.
    pub id: String,
    /// What the person called it, as shown in the switcher. Trimmed, and unique among the saved ones read
    /// case-insensitively.
    pub name: String,
    /// The address to dial, in the spelling the URL parser read it in with the trailing slash it adds
    /// taken back off: the page puts `/api/v1/...` straight onto it.
    pub url: String,
    /// Whether this instance's password is kept in the keychain for the next run. The person's choice,
    /// and off until they make it.
    pub save_password: bool,
    /// The user name the shell signs in with when `save_password` is on. `None` is "ask on connect".
    pub user: Option<String>,
}

/// What stopped an instance from being added, or a default from pointing at something real.
#[derive(Debug, Error)]
pub enum InstanceError {
    /// The address is not one this client can dial: not http or https, no host to reach, or something
    /// after the host that a connection would have to guess about.
    #[error(
        "{given} is not an address this app can dial: http or https to a host, with no user or \
         password, no path and no query"
    )]
    NotAnAddress {
        /// What was typed, as it was typed.
        given: String,
    },
    /// Nothing was typed, or only spaces were.
    #[error("an instance needs a name")]
    NameEmpty,
    /// The name is longer than the row that shows it.
    #[error("the name is longer than {limit} characters (it is {length})")]
    NameTooLong {
        /// How long what was typed is.
        length: usize,
        /// What the limit is.
        limit: usize,
    },
    /// Another saved instance already answers to that name, spelled some other way or the same.
    #[error("an instance named {name} is already saved")]
    NameTaken {
        /// The name, trimmed.
        name: String,
    },
    /// `This computer` is the app's own instance. It is in the switcher already and cannot be added.
    #[error("This computer is the app's own instance, so it is not one to add")]
    ReservedName,
    /// The id names no saved instance, so it cannot be the one to connect to.
    #[error("no instance with id {id} is saved")]
    UnknownInstance {
        /// The id that was asked for.
        id: String,
    },
}

impl DesktopSettings {
    /// Saves a new remote instance and returns it, adding it to `instances`.
    ///
    /// The name is checked first because it is the first field in the form, and the address second: the
    /// address is read by `url`, the same WHATWG parser the HTTP client dials with, so the spellings
    /// refused here are the ones a connection would fail on later, read the same way a connection reads
    /// them. See the module docs and [`InstanceError`] for the rules, and [`Instance`] for what ends up in
    /// the file. Nothing is changed when something is refused.
    pub fn add(&mut self, name: &str, url: &str) -> Result<Instance, InstanceError> {
        let name = checked_name(name, &self.instances)?;
        let url = dialable_base(url)?;
        let instance = Instance {
            id: fresh_id(&self.instances),
            name,
            url,
            save_password: false,
            user: None,
        };
        self.instances.push(instance.clone());
        Ok(instance)
    }

    /// Removes the instance with this id, if there is one, and puts the default back on This computer when
    /// it pointed at the one that just went.
    ///
    /// An app that opens on an id nobody has anymore has nothing to connect to and a switcher that does
    /// not offer it, and the design's answer to a default that is gone is the instance that is never gone.
    /// Removing an id nobody has is nothing to undo, which is what makes removing something from a stale
    /// list safe.
    pub fn remove(&mut self, id: &str) {
        self.instances.retain(|entry| entry.id != id);
        if self.default_instance == id {
            self.default_instance = LOCAL_ID.to_string();
        }
    }

    /// Makes this id the instance to connect to when the app opens.
    ///
    /// `local` is always a valid choice, because This computer is never absent; a remote id has to be one
    /// of the [`usable`](Self::usable) instances, which is what stops a hand-edited file or a stale picker from setting a default the
    /// app cannot honor. `default_or_local` is what actually reads the choice, so a default made invalid
    /// some other way degrades to This computer rather than to a failure.
    pub fn set_default(&mut self, id: &str) -> Result<(), InstanceError> {
        if id != LOCAL_ID && !self.usable().iter().any(|entry| entry.id == id) {
            return Err(InstanceError::UnknownInstance { id: id.to_string() });
        }
        self.default_instance = id.to_string();
        Ok(())
    }

    /// The instance to connect to: the one `default_instance` names when it names a
    /// [`usable`](Self::usable) one, and This computer otherwise.
    ///
    /// Everything that decides where to connect reads this rather than `default_instance` directly, so a
    /// file edited by hand, an instance deleted on the machine it lives on, an entry with an empty id or an
    /// address nobody can dial, and a file from before this key existed all land on the app's own engine
    /// instead of on an empty address.
    pub fn default_or_local(&self) -> &str {
        self.usable()
            .into_iter()
            .find(|entry| entry.id == self.default_instance)
            .map_or(LOCAL_ID, |entry| entry.id.as_str())
    }

    /// The saved instances the shell may list and dial: the ones that pass the rules [`add`](Self::add)
    /// applies (an address this client can dial, a name that is not empty, not too long and not This
    /// computer's) and have an id that is neither empty nor `local`. When two entries share an id only the
    /// first is usable, because a command naming that id has to mean one of them.
    ///
    /// The file can hold anything a person typed into it, and [`load`](Self::load) keeps all of it:
    /// entries that are not usable stay in `instances` and are written back unchanged on the next save,
    /// because the app rewrites nothing it cannot read. They are only never offered.
    pub fn usable(&self) -> Vec<&Instance> {
        let mut seen: Vec<&str> = Vec::new();
        let mut usable = Vec::new();
        for entry in &self.instances {
            if entry.id.is_empty() || entry.id == LOCAL_ID || seen.contains(&entry.id.as_str()) {
                continue;
            }
            seen.push(&entry.id);
            if shaped_name(&entry.name).is_ok() && dialable_base(&entry.url).is_ok() {
                usable.push(entry);
            }
        }
        usable
    }
}

/// A name for a new instance, as it will be stored: trimmed, not empty, not too long for the row that
/// shows it, not the app's own instance's name, and not one another saved instance already answers to.
/// Names are compared case-insensitively because the switcher shows them in a list where `Pi` and `pi` are
/// the same thing to look at, and because a person adding the Pi they already added would otherwise get
/// two rows they cannot tell apart.
fn checked_name(name: &str, taken: &[Instance]) -> Result<String, InstanceError> {
    let name = shaped_name(name)?;
    let folded = name.to_lowercase();
    if taken
        .iter()
        .any(|entry| entry.name.to_lowercase() == folded)
    {
        return Err(InstanceError::NameTaken {
            name: name.to_string(),
        });
    }
    Ok(name.to_string())
}

/// The rules a name has to pass on its own, whatever else is saved: trimmed, not empty, not too long for
/// the row that shows it, and not the app's own instance's name. Returns the trimmed name.
fn shaped_name(name: &str) -> Result<&str, InstanceError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(InstanceError::NameEmpty);
    }
    let length = name.chars().count();
    if length > MAX_NAME_CHARS {
        return Err(InstanceError::NameTooLong {
            length,
            limit: MAX_NAME_CHARS,
        });
    }
    if name.to_lowercase() == LOCAL_NAME.to_lowercase() {
        return Err(InstanceError::ReservedName);
    }
    Ok(name)
}

/// An address this client can dial, in the spelling the page puts an API path onto.
///
/// `Url` is the parser `reqwest` dials with, so a spelling that reads as a host here is a host the
/// connection reaches and one that does not is refused now, where a person can fix it, rather than later
/// as a connection error that names none of what was wrong about the address. What is kept is the
/// parser's own spelling, which lowercases the scheme and the host, with the one trailing slash it always
/// writes taken back off.
fn dialable_base(url: &str) -> Result<String, InstanceError> {
    let given = url.trim();
    let not_an_address = || InstanceError::NotAnAddress {
        given: given.to_string(),
    };
    let parsed = Url::parse(given).map_err(|_| not_an_address())?;
    // Only the two schemes this client can dial. `ftp://bit.example` parses into something with a host and
    // means nothing here, the same way the TUI's connect screen reads it.
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(not_an_address());
    }
    // A scheme with nothing after it (`https://`) has no host to dial. `https:///x` does have one: the
    // parser skips the extra slash and reads `x` as the host, and whether that host answers is the
    // connection's thing to report.
    parsed.host().ok_or_else(not_an_address)?;
    // Login text in a saved address is somebody's paste (`http://u:p@h`): the password belongs in the
    // keychain, where it is overwritten when this run is done with it, not in a file and not in a URL.
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(not_an_address());
    }
    // The page puts `/api/v1/torrents` onto this, so the address ends at the host. An empty path, or the
    // bare `/` the parser adds, is the same address; `http://h/api` or `http://h/?debug=1` is somebody's
    // link to a page, and dialing it would ask for `http://h/api/api/v1/torrents`.
    if !matches!(parsed.path(), "" | "/") {
        return Err(not_an_address());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(not_an_address());
    }
    let spelling = parsed.as_str();
    Ok(spelling.strip_suffix('/').unwrap_or(spelling).to_string())
}

/// A fresh id for an instance that no saved instance already has, and is not This computer's.
///
/// Random rather than the next number, because a settings file moves between machines and a numbered id
/// that means the Pi on one machine would mean the NAS on another. The counter suffix, which is only ever
/// reached if the random id is somehow already taken, can run out only after every entry in `taken` has
/// been named, so it cannot loop without bound.
fn fresh_id(taken: &[Instance]) -> String {
    let mut bytes = [0u8; ID_BYTES];
    getrandom::fill(&mut bytes).expect("the platform has a random source");
    let base = BASE64_URL_SAFE_NO_PAD.encode(bytes);
    // Twelve url-safe characters cannot spell `local`, but the id still has to be unlike it: two things
    // with the same id in one file is a file nothing can connect to.
    let claimed = |id: &str| id == LOCAL_ID || taken.iter().any(|entry| entry.id == id);
    if !claimed(&base) {
        return base;
    }
    let mut suffix = 1;
    loop {
        let id = format!("{base}-{suffix}");
        if !claimed(&id) {
            return id;
        }
        suffix += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{InstanceError, MAX_NAME_CHARS};
    use crate::paths::DesktopPaths;
    use crate::{DesktopSettings, Instance, LOCAL_ID};
    use fbtt_core::scratch::ScratchDir;

    /// The addresses this client cannot dial, and therefore has no business saving: they are read by the
    /// same parser `reqwest` dials with, so refusing them here is exactly the set of spellings a
    /// connection would fail on later.
    #[test]
    fn add_refuses_bad_urls() {
        let mut settings = DesktopSettings::default();
        for given in [
            "ftp://x",
            "http://",
            "http://u:p@h",
            "http://h/path",
            "http://h/?q",
            "",
        ] {
            let problem = settings
                .add("Some name", given)
                .expect_err("an address this client cannot dial is refused");
            assert!(
                matches!(problem, InstanceError::NotAnAddress { .. }),
                "{given:?}: {problem}"
            );
            let line = problem.to_string();
            assert_eq!(
                line.lines().count(),
                1,
                "the Add page shows one line: {line}"
            );
            assert!(line.contains(given), "{given:?} is named in: {line}");
        }
        assert!(
            settings.instances.is_empty(),
            "a refused add adds nothing to save"
        );
    }

    /// A name is how the switcher and every message about an instance refer to it, so it has to be
    /// present, short enough to fit the row that shows it, and different from every other instance's
    /// reading the same way. "This computer" belongs to the app's own instance and is never saved.
    #[test]
    fn add_refuses_duplicate_and_reserved_names() {
        let mut settings = DesktopSettings::default();
        settings
            .add("Pi at home", "http://pi.local:8090")
            .expect("the first add");

        let problem = settings
            .add("pi AT HOME", "https://elsewhere.example")
            .expect_err("names are unique read case-insensitively");
        assert!(
            matches!(problem, InstanceError::NameTaken { .. }),
            "a second spelling of a saved name is the same name: {problem}"
        );

        let problem = settings
            .add("  This computer  ", "http://another.example")
            .expect_err("the app's own instance is not one you can add");
        assert!(matches!(problem, InstanceError::ReservedName), "{problem}");

        let problem = settings
            .add("   \u{a0} ", "http://another.example")
            .expect_err("a name that is only whitespace is no name");
        assert!(matches!(problem, InstanceError::NameEmpty), "{problem}");

        let too_long = "n".repeat(MAX_NAME_CHARS + 1);
        let problem = settings
            .add(&too_long, "http://another.example")
            .expect_err("a name longer than the row that shows it is refused");
        assert!(
            matches!(problem, InstanceError::NameTooLong { .. }),
            "{problem}"
        );

        let exact = "m".repeat(MAX_NAME_CHARS);
        settings
            .add(&exact, "http://another.example")
            .expect("exactly the limit is still a name");
        assert_eq!(
            settings.instances.len(),
            2,
            "only the two accepted adds are saved"
        );
    }

    /// A default that names nothing - an id a person deleted from the file, an instance removed on the
    /// other machine, an older app that never wrote the key - is answered with This computer rather than
    /// with a screen that cannot connect to `""`.
    #[test]
    fn missing_default_falls_back_to_this_computer() {
        let mut settings = DesktopSettings::default();
        assert_eq!(settings.default_or_local(), LOCAL_ID, "no file, no choice");

        let added = settings.add("Pi", "http://pi.local:8090").expect("add one");
        assert_eq!(
            settings.default_or_local(),
            LOCAL_ID,
            "adding an instance does not make it the default"
        );

        settings.default_instance = "gone".to_string();
        assert_eq!(
            settings.default_or_local(),
            LOCAL_ID,
            "the id names nothing saved"
        );

        settings.default_instance = String::new();
        assert_eq!(
            settings.default_or_local(),
            LOCAL_ID,
            "an empty default is no default"
        );

        settings.default_instance = added.id.clone();
        assert_eq!(
            settings.default_or_local(),
            added.id.as_str(),
            "and a default that names a saved instance is honoured"
        );
    }

    /// Removing the default instance leaves no default behind: the app opens on This computer, which is
    /// always there, instead of trying to connect to an id that is not in the file anymore.
    #[test]
    fn remove_resets_the_default() {
        let mut settings = DesktopSettings::default();
        let pi = settings.add("Pi", "http://pi.local:8090").expect("add one");
        settings.set_default(&pi.id).expect("make it the default");
        assert_eq!(settings.default_or_local(), pi.id.as_str());

        settings.remove(&pi.id);
        assert!(
            settings.instances.iter().all(|entry| entry.id != pi.id),
            "it is gone from the list"
        );
        assert_eq!(
            settings.default_instance, LOCAL_ID,
            "and the default came back to This computer"
        );
        assert_eq!(settings.default_or_local(), LOCAL_ID);

        // Removing something else leaves a chosen default alone.
        let other = settings
            .add("Other", "https://other.example")
            .expect("add another");
        settings
            .set_default(&other.id)
            .expect("make it the default");
        settings.remove("not-there");
        assert_eq!(
            settings.default_instance, other.id,
            "an id nobody removed does not reset it"
        );

        // This computer is always a valid default; an id that is not saved is not.
        settings
            .set_default(LOCAL_ID)
            .expect("This computer is always a choice");
        assert_eq!(settings.default_or_local(), LOCAL_ID);
        assert!(
            matches!(
                settings.set_default("not-there"),
                Err(InstanceError::UnknownInstance { .. })
            ),
            "a default has to name something"
        );
        assert_eq!(
            settings.default_instance, LOCAL_ID,
            "the refusal changed nothing"
        );
    }

    /// What a saved instance holds is what the page and the shell need from it: the name trimmed, the
    /// address as the parser read it with the trailing slash taken back off (the page puts an API path
    /// straight onto it), no credential, and an id that is url-safe and never This computer's.
    #[test]
    fn add_stores_a_trimmed_name_a_dialable_base_and_a_random_id() {
        let mut settings = DesktopSettings::default();
        let pi = settings
            .add("  Pi at home  ", "HTTPS://Pi.Local:8090/")
            .expect("add one with the case and the trailing slash a person types");
        assert_eq!(pi.name, "Pi at home", "the name is stored trimmed");
        assert_eq!(
            pi.url, "https://pi.local:8090",
            "the base is the one the page can put /api/v1/torrents onto"
        );
        assert!(
            !pi.save_password,
            "saving the password is the person's choice, made later"
        );
        assert!(pi.user.is_none(), "and so is the user name");

        let nas = settings
            .add("NAS", "http://nas.local")
            .expect("add another");
        assert_eq!(nas.url, "http://nas.local");
        assert!(!nas.id.is_empty());
        assert_ne!(nas.id, pi.id, "two adds do not get the same id");
        assert_ne!(
            nas.id, LOCAL_ID,
            "This computer's id is not handed to an instance"
        );
        assert!(
            nas.id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "url-safe: it goes in a keychain account name and a file: {}",
            nas.id
        );

        // The adds are what the settings file holds, and they survive the file.
        assert_eq!(settings.instances, vec![pi.clone(), nas.clone()]);
        let dir = ScratchDir::new("desktop_instances_round_trip");
        let paths = DesktopPaths::resolve_with(|key| {
            if key == "FBTT_DESKTOP_CONFIG_DIR" || key == "FBTT_DESKTOP_DATA_DIR" {
                Some(dir.path().into())
            } else {
                None
            }
        })
        .expect("both directories are given");
        settings
            .save(&paths.settings_file())
            .expect("save the list");
        let back = DesktopSettings::load(&paths.settings_file()).expect("read it back");
        assert_eq!(
            back, settings,
            "an instance written to the file reads back the same"
        );
    }

    /// An entry as a hand-edited file may hold it: every field given, nothing checked.
    fn entry(id: &str, name: &str, url: &str) -> Instance {
        Instance {
            id: id.to_string(),
            name: name.to_string(),
            url: url.to_string(),
            save_password: false,
            user: None,
        }
    }

    /// The list a person broke by hand: an entry with no id, a second entry reusing an id, an address
    /// `add` would refuse, and an entry with no name. Only the valid one, and the first of the duplicated
    /// id, are offered to the shell to list and dial.
    #[test]
    fn hand_edited_bad_entries_are_not_usable() {
        let settings = DesktopSettings {
            instances: vec![
                entry("good", "Pi", "http://pi.local:8090"),
                entry("", "No id", "http://noid.example"),
                entry("good", "Second good", "http://second.example"),
                entry("ftp", "Ftp", "ftp://files.example"),
                entry("noname", "   ", "http://noname.example"),
                entry(LOCAL_ID, "Fake local", "http://fake.example"),
                entry("reserved", "This computer", "http://reserved.example"),
            ],
            ..DesktopSettings::default()
        };
        let usable: Vec<&str> = settings
            .usable()
            .iter()
            .map(|instance| instance.name.as_str())
            .collect();
        assert_eq!(usable, vec!["Pi"], "only the valid entry is usable");

        for id in ["", "ftp", "noname", LOCAL_ID, "reserved"] {
            let mut pointed = settings.clone();
            pointed.default_instance = id.to_string();
            assert_eq!(
                pointed.default_or_local(),
                LOCAL_ID,
                "a default naming an unusable entry ({id:?}) is This computer"
            );
        }
        let mut pointed = settings.clone();
        pointed.default_instance = "good".to_string();
        assert_eq!(pointed.default_or_local(), "good");
    }

    /// An empty default and an entry with an empty id match each other as strings, and must still not
    /// make the app open on an instance called `""`.
    #[test]
    fn empty_default_and_empty_id_fall_back_to_local() {
        let settings = DesktopSettings {
            default_instance: String::new(),
            instances: vec![entry("", "Pi", "http://pi.local:8090")],
            ..DesktopSettings::default()
        };
        assert_eq!(settings.default_or_local(), LOCAL_ID);
        assert!(settings.usable().is_empty());
    }

    /// Entries the app cannot use are still the person's: a load and a save write them back as they
    /// were read, because the app rewrites nothing it cannot read.
    #[test]
    fn invalid_entries_survive_a_save() {
        let dir = ScratchDir::new("desktop_invalid_instances_survive");
        let paths = DesktopPaths::resolve_with(|key| {
            if key == "FBTT_DESKTOP_CONFIG_DIR" || key == "FBTT_DESKTOP_DATA_DIR" {
                Some(dir.path().into())
            } else {
                None
            }
        })
        .expect("both directories are given");
        let file = paths.settings_file();
        std::fs::write(
            &file,
            "default_instance: ''\n\
             instances:\n\
             - id: ''\n  name: No id\n  url: http://noid.example\n\
             - id: dup\n  name: First\n  url: http://first.example\n\
             - id: dup\n  name: Second\n  url: http://second.example\n\
             - id: ftp\n  name: Ftp\n  url: ftp://files.example\n\
             - id: noname\n  url: http://noname.example\n",
        )
        .expect("write a hand-edited file");

        let loaded = DesktopSettings::load(&file).expect("a file with bad entries still loads");
        assert_eq!(loaded.instances.len(), 5, "nothing is dropped on load");
        assert_eq!(loaded.default_or_local(), LOCAL_ID);
        assert_eq!(
            loaded
                .usable()
                .iter()
                .map(|instance| instance.name.as_str())
                .collect::<Vec<_>>(),
            vec!["First"]
        );

        loaded.save(&file).expect("save");
        let back = DesktopSettings::load(&file).expect("read it back");
        assert_eq!(back, loaded, "every entry is written back as it was read");
        assert_eq!(back.instances[0].id, "");
        assert_eq!(back.instances[3].url, "ftp://files.example");
        assert_eq!(back.instances[4].name, "");
        assert_eq!(back.default_instance, "");
    }
}
