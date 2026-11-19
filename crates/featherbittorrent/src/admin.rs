//! The `user` and `device` commands: the two things a person does before the
//! server can serve anybody else, creating the first login and registering a
//! device key.
//!
//! These edit `config.yaml`, which the running server also reads, so anything
//! that writes takes the same library lock the server holds and refuses with a
//! clear message instead of racing it for the file. Listing is the read-only
//! exception: a list never touches the file, so it answers while a server runs.

use std::io::{BufRead, Write};
use std::path::Path;

use crate::exit::Failure;
use crate::wizard::{ask_new_password, ask_user_name, name_problem};
use fbtt_core::{Config, DeviceEntry, Paths, ServerConfig, UserEntry};
use fbtt_server::auth::keys::{fingerprint, parse_device_key};
use fbtt_server::auth::password::hash;

/// What a write command says when it finds the library already running. The
/// alternative it offers is real: the API has a device-add route, so a TUI that
/// is connected to this server can add the key without stopping anything.
pub const IN_USE: &str = "The library is in use (a server or TUI is running). \
    Stop it first, or add the device from a connected TUI.";

/// Creates or replaces one user. The password is asked for twice and nothing is
/// written when the two answers disagree.
pub fn set_user(
    paths: &Paths,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    secret_input: bool,
    name: Option<&str>,
) -> Result<(), Failure> {
    let name = match name {
        Some(given) => match name_problem(given.trim()) {
            None => given.trim().to_string(),
            Some(problem) => return Err(Failure::problem(problem)),
        },
        // The same question, the same rules and the same number of goes-wrong as the
        // wizard's, so one product asks for a name in one way.
        None => ask_user_name(input, out)?,
    };

    let password = ask_new_password(input, out, secret_input)?;

    edit_config(paths, |config| {
        let entry = UserEntry {
            name: name.clone(),
            password_hash: hash(&password),
        };
        // A config that never chose to serve gets the section this command is about: registering a login
        // *is* choosing to serve, and the defaults here are the ones `serve` would have asked about.
        let server = config.server.get_or_insert_with(ServerConfig::default);
        match server.auth.users.iter_mut().find(|user| user.name == name) {
            Some(existing) => *existing = entry,
            None => server.auth.users.push(entry),
        }
    })?;
    writeln!(out, "Saved user {name}.")?;
    Ok(())
}

/// Prints the users in `config.yaml`: name, then password hash. With none, the
/// one line that says so: an empty list is a state of the file, not silence. A
/// config with no server section at all has no logins either, which is the same
/// answer and gets the same line.
pub fn list_users(paths: &Paths, out: &mut dyn Write) -> Result<(), Failure> {
    let users = read(paths)?.server.unwrap_or_default().auth.users;
    if users.is_empty() {
        writeln!(out, "No users.")?;
    }
    for user in users {
        writeln!(out, "{}  {}", user.name, user.password_hash)?;
    }
    Ok(())
}

/// Registers a device key under the name the operator chose for it. `key` is an
/// OpenSSH public key line or the path of a file holding one; with nothing given,
/// the line is read from stdin.
pub fn add_device(
    paths: &Paths,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    name: &str,
    key: Option<&str>,
) -> Result<(), Failure> {
    match name_problem(name.trim()) {
        None => {}
        Some(problem) => return Err(Failure::problem(problem)),
    }
    let name = name.trim().to_string();
    let text = match key {
        Some(value) => match std::fs::read_to_string(Path::new(value)) {
            // A file wins when the argument names one: a key line is not a path
            // that exists, so the two forms cannot be confused for each other.
            Ok(contents) => contents,
            Err(_) => value.to_string(),
        },
        None => {
            let mut read = String::new();
            input.read_line(&mut read)?;
            read
        }
    };
    // The first non-blank line is the key: a file may carry a header or trailing
    // newlines, and only one line can be a public key anyway.
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string();
    let key = parse_device_key(&line)?;
    let fingerprint = fingerprint(&key);

    edit_config(paths, |config| {
        let entry = DeviceEntry {
            name: name.clone(),
            public_key: line.clone(),
        };
        // Matched on the name, like the API's device list and like `user set`:
        // typing the same command again means "this is the key for that name now",
        // which is what a laptop that got a new keypair needs. The API refuses a
        // second `device add` with a name it already has instead, because a request
        // cannot be assumed to mean the same thing; here the person can see both
        // answers agree that a name belongs to one device.
        let server = config.server.get_or_insert_with(ServerConfig::default);
        match server
            .auth
            .devices
            .iter_mut()
            .find(|device| device.name == name)
        {
            Some(existing) => *existing = entry,
            None => server.auth.devices.push(entry),
        }
    })?;
    writeln!(out, "Added device {name} ({fingerprint}).")?;
    Ok(())
}

/// Takes a device out by name. The name is what `device list` printed.
pub fn remove_device(paths: &Paths, out: &mut dyn Write, name: &str) -> Result<(), Failure> {
    let name = name.trim().to_string();
    let mut removed = false;
    edit_config(paths, |config| {
        // A config with no server section has no devices registered, which is the same answer as a name
        // that is not in the list: nothing was taken out.
        let Some(server) = config.server.as_mut() else {
            return;
        };
        let before = server.auth.devices.len();
        server.auth.devices.retain(|device| device.name != name);
        removed = server.auth.devices.len() != before;
    })?;
    match removed {
        true => writeln!(out, "Removed device {name}.")?,
        false => return Err(Failure::problem(format!("No device named {name}."))),
    }
    Ok(())
}

/// Takes a user out by name. The name is matched exactly, like everywhere else
/// that reads this list.
pub fn remove_user(paths: &Paths, out: &mut dyn Write, name: &str) -> Result<(), Failure> {
    let name = name.trim().to_string();
    let mut removed = false;
    edit_config(paths, |config| {
        // The same as for a device: no server section means no logins to take out.
        let Some(server) = config.server.as_mut() else {
            return;
        };
        let before = server.auth.users.len();
        server.auth.users.retain(|user| user.name != name);
        removed = server.auth.users.len() != before;
    })?;
    match removed {
        true => writeln!(out, "Removed user {name}.")?,
        false => return Err(Failure::problem(format!("No user named {name}."))),
    }
    Ok(())
}

/// Prints registered devices: name, then fingerprint. With none, the one line
/// that says so. A key the parser cannot read has no fingerprint, so it is named
/// by what the file calls it instead. No server section is the same answer as an
/// empty list, and gets the same line.
pub fn list_devices(paths: &Paths, out: &mut dyn Write) -> Result<(), Failure> {
    let devices = read(paths)?.server.unwrap_or_default().auth.devices;
    if devices.is_empty() {
        writeln!(out, "No devices.")?;
    }
    for device in devices {
        match parse_device_key(&device.public_key) {
            Ok(key) => writeln!(out, "{}  {}", device.name, fingerprint(&key))?,
            Err(_) => writeln!(out, "{}  unreadable key", device.name)?,
        }
    }
    Ok(())
}

/// The config file as it is now, or this library's defaults when there is no file
/// yet. `load_for_edit` rather than `load`: an editor must not overwrite bytes it
/// could not read, and nothing here wants the `.bad` rename either. A file that reads
/// as a config but whose player's setting is not usable is refused with one line naming
/// the file and the player's setting to fix: a user or device command must never rewrite
/// a file holding a bad value it would silently reset.
fn read(paths: &Paths) -> Result<Config, Failure> {
    match Config::load_for_edit(&paths.config_file())? {
        Some(config) => Ok(config),
        None => Ok(Config::default_for(paths)),
    }
}

/// Applies one change to `config.yaml` while holding the library lock, so an
/// editing command can never race the server for the file.
fn edit_config(paths: &Paths, change: impl FnOnce(&mut Config)) -> Result<(), Failure> {
    let lock = match fbtt_core::LibraryLock::acquire(&paths.lock_file()) {
        Ok(lock) => lock,
        Err(fbtt_core::LockError::Held) => return Err(Failure::in_use(IN_USE)),
        Err(fbtt_core::LockError::Io(problem)) => {
            return Err(Failure::problem(format!(
                "Could not lock the library ({}): {problem}",
                paths.data_dir.display()
            )));
        }
    };
    let mut config = read(paths)?;
    change(&mut config);
    config.save(&paths.config_file())?;
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{list_users, remove_user};
    use fbtt_core::Paths;
    use fbtt_core::scratch::ScratchDir;
    use std::fs;

    /// A user command that finds a config whose only problem is the player's setting is refused, and
    /// the line it prints names the config file's path and says the player's setting is what to fix.
    /// The refusal must not rewrite the file: the bad value stays where the person can see and fix it,
    /// and nothing is renamed aside. The write command is the one that would otherwise rewrite the file
    /// (removing a user, and in doing so silently resetting the player to its default), so it is the
    /// clearest case the refusal has to hold.
    #[test]
    fn a_player_problem_refuses_with_the_path() {
        let dir = ScratchDir::new("admin_player_problem");
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        fs::create_dir_all(&paths.config_dir).expect("the config directory");

        // A valid server section and a player whose program is empty: the one thing a run does not act
        // on, and nothing else wrong. The command must refuse rather than rewrite this file.
        let written = "server:\n  bind: \"127.0.0.1:8090\"\nplayer:\n  program: \"\"\n";
        fs::write(paths.config_file(), written).expect("write a server config with a bad player");

        // The write command: it would remove a user and, in rewriting the file, reset the player's
        // value to its default. It must refuse instead, with the line naming the file and the player.
        let mut out = Vec::new();
        let problem = remove_user(&paths, &mut out, "alice")
            .expect_err("a player-only problem is refused, not rewritten");
        assert_eq!(
            problem.code,
            1,
            "a refusal is a problem, not a lock: {message}",
            message = problem.message
        );
        // The line names the config file's path, so the person knows which file to fix.
        let path = paths.config_file().display().to_string();
        assert!(
            problem.message.contains(&path),
            "the line names the config file's path: {path}, {message}",
            message = problem.message
        );
        // ...and says the player's setting is what to fix.
        assert!(
            problem.message.to_lowercase().contains("player"),
            "the line names the player's setting: {message}",
            message = problem.message
        );

        // The read command refuses with the same line, for the same reason.
        let mut out = Vec::new();
        let problem = list_users(&paths, &mut out)
            .expect_err("a player-only problem is refused on a read too");
        assert!(
            problem.message.contains(&path),
            "the read command's line names the path too: {message}",
            message = problem.message
        );

        // The refusal left the file exactly where it was: the bad value is still there, and nothing
        // was renamed aside under `.bad`.
        assert_eq!(
            fs::read_to_string(paths.config_file()).expect("the file is still where it was"),
            written,
            "the bad value stays for the person to fix"
        );
        // The `.bad` name `Config::load` would use, `config.yaml.bad`: nothing was renamed to it.
        let config_name = paths.config_file().file_name().unwrap().to_os_string();
        let aside = paths
            .config_dir
            .join(config_name)
            .with_extension("yaml.bad");
        assert!(!aside.exists(), "nothing was renamed aside: {aside:?}");
    }
}
