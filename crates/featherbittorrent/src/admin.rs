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
use fbtt_core::{Config, DeviceEntry, Paths, UserEntry};
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
        match config
            .server
            .auth
            .users
            .iter_mut()
            .find(|user| user.name == name)
        {
            Some(existing) => *existing = entry,
            None => config.server.auth.users.push(entry),
        }
    })?;
    writeln!(out, "Saved user {name}.")?;
    Ok(())
}

/// Prints the users in `config.yaml`: name, then password hash. With none, the
/// one line that says so: an empty list is a state of the file, not silence.
pub fn list_users(paths: &Paths, out: &mut dyn Write) -> Result<(), Failure> {
    let users = read(paths)?.server.auth.users;
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
        match config
            .server
            .auth
            .devices
            .iter_mut()
            .find(|device| device.name == name)
        {
            Some(existing) => *existing = entry,
            None => config.server.auth.devices.push(entry),
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
        let before = config.server.auth.devices.len();
        config
            .server
            .auth
            .devices
            .retain(|device| device.name != name);
        removed = config.server.auth.devices.len() != before;
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
        let before = config.server.auth.users.len();
        config.server.auth.users.retain(|user| user.name != name);
        removed = config.server.auth.users.len() != before;
    })?;
    match removed {
        true => writeln!(out, "Removed user {name}.")?,
        false => return Err(Failure::problem(format!("No user named {name}."))),
    }
    Ok(())
}

/// Prints registered devices: name, then fingerprint. With none, the one line
/// that says so. A key the parser cannot read has no fingerprint, so it is named
/// by what the file calls it instead.
pub fn list_devices(paths: &Paths, out: &mut dyn Write) -> Result<(), Failure> {
    let devices = read(paths)?.server.auth.devices;
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
/// could not read, and nothing here wants the `.bad` rename either.
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
