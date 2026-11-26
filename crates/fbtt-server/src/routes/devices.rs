//! The device list, shown and changed over the API: what a key login recognises.
//!
//! A device is a name plus an Ed25519 public key in the one-line OpenSSH format - the same text a
//! person pastes into `authorized_keys` - and this is where it is added from a connected client rather
//! than from a command line. Adding one does two things at once: it goes into `config.yaml`, so the next
//! run knows it, and into the list this run logs devices in against, so nobody has to restart for a new
//! machine to be able to sign in. Removing one takes it out of both.
//!
//! The change is a read-modify-write of a file, so the routes do it under one lock: two requests that
//! each read the file, add their own device and write it back would leave one of them with a device
//! nobody asked for and the other with none at all. What they cannot do while that lock is held is lose
//! the work - `Config::save` replaces the file in one step, so a crash leaves either the old list or the
//! new one. And what they do not do, when the file cannot be read, is anything at all: no rename, no
//! defaults written over somebody's hand-edited bytes, only an answer that sends whoever asked to the
//! server log, where the file and what is wrong with it are written out for the person who can fix them.
//!
//! With nobody signing in these three routes answer `403`: a device is a way of proving who you are, and a
//! server that asks nobody to prove anything has nothing for one to be proof against. It also closes the
//! worst thing an unauthenticated server could do to itself - let a request it cannot authenticate add a key
//! that every later run would log in, and keep it in `config.yaml` after the page that added it was closed.
//!
//! A name is free text within limits (see [`MAX_NAME_CHARS`] and [`name_problem`]), which means it can
//! contain characters that need percent-encoding: what `GET /api/v1/auth/devices` reports is what a
//! client has to send back, encoded, to reach that device again.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use fbtt_core::{DeviceEntry, ServerConfig};
use serde::Deserialize;

use super::{DeviceRow, DeviceTable, RouteError, Shared, clean, locked};
use crate::auth::keys;

/// How long a device name may be, in characters. Long enough for "work laptop of alice", short enough
/// that it is one line wherever it is drawn.
const MAX_NAME_CHARS: usize = 64;

/// Whether these routes are open at all: a device is one server's answer to "who are you", and with nobody
/// signing in there is no question for one to answer. The message says what to do about it rather than
/// pretending the list is empty - and adding a key anonymously to a server that authenticates nobody would
/// be adding it to whoever next reached this address.
fn open_for_devices(shared: &Shared) -> Result<(), RouteError> {
    if shared.auth.auth_on() {
        Ok(())
    } else {
        Err(RouteError::forbidden(
            "devices need authentication on; add device keys to config.yaml by hand",
        ))
    }
}

/// `GET /api/v1/auth/devices`: the devices a key login recognises, as `[{name, fingerprint}]`, in the
/// order `config.yaml` has them. Entries whose key this server cannot use are not here - they have no
/// fingerprint to show - and say so in the log at startup. Names come back cleaned for display, like
/// everything else here that repeats text from a file (see [`Shared::device_rows`]).
pub(super) async fn list(
    State(shared): State<Arc<Shared>>,
) -> Result<Json<Vec<DeviceRow>>, RouteError> {
    open_for_devices(&shared)?;
    Ok(Json(shared.device_rows()))
}

/// `POST /api/v1/auth/devices`: add a device, answering 201 with the row it created.
///
/// The name has to be printable and unique in the list; the key has to be an Ed25519 public key line,
/// and it is parsed before anything is written so a key this server cannot use never reaches the file.
/// The device can log in as soon as this answers - the list logins read is replaced with what was just
/// saved - which is the point of having the route at all.
pub(super) async fn add(
    State(shared): State<Arc<Shared>>,
    given: Result<Json<NewDevice>, JsonRejection>,
) -> Result<(StatusCode, Json<DeviceRow>), RouteError> {
    open_for_devices(&shared)?;
    // A body that is not JSON of this shape, or bigger than the API allows, comes back in this API's error
    // shape rather than axum's plain-text one.
    let Json(body) = given?;
    let name = body.name.trim();
    if let Some(problem) = name_problem(name) {
        return Err(RouteError::invalid_request(problem));
    }
    let public_key = body.public_key.trim();
    let key = keys::parse_device_key(public_key)
        .map_err(|problem| RouteError::invalid_request(problem.to_string()))?;
    let row = DeviceRow {
        name: name.to_string(),
        fingerprint: keys::fingerprint(&key),
    };
    // What goes in the file is the trimmed line, so the next run reads exactly what this one checked.
    let entry = DeviceEntry {
        name: row.name.clone(),
        public_key: public_key.to_string(),
    };
    change(&shared, move |devices| {
        if devices.iter().any(|device| device.name == entry.name) {
            return Err(RouteError::already_exists(format!(
                "a device named {} is already here",
                clean(&entry.name),
            )));
        }
        devices.push(entry);
        Ok(())
    })
    .await?;
    tracing::info!(device = %clean(&row.name), fingerprint = %row.fingerprint, "device added");
    Ok((StatusCode::CREATED, Json(row)))
}

/// `DELETE /api/v1/auth/devices/{name}`: take a device out of the file and out of the list logins check
/// against, answering 204, or 404 when no device has that name.
///
/// What it does not do is end sessions that device already opened: a token lives until it is logged out
/// or its hour runs down, whether or not the key that earned it is still on the list - which is what
/// removing a line from `authorized_keys` does not do either.
pub(super) async fn remove(
    State(shared): State<Arc<Shared>>,
    given: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, RouteError> {
    open_for_devices(&shared)?;
    // A name in the path that cannot be read - a stray percent sign - gets this API's error shape too.
    let Path(name) = given?;
    // The closure runs inside the change's task now, so it has to own the name; `name` itself stays for the
    // log line below.
    let to_remove = name.clone();
    change(&shared, move |devices| {
        let before = devices.len();
        devices.retain(|device| device.name != to_remove);
        if devices.len() == before {
            return Err(RouteError::not_found(format!(
                "no device named {}",
                clean(&to_remove),
            )));
        }
        Ok(())
    })
    .await?;
    tracing::info!(device = %clean(&name), "device removed");
    Ok(StatusCode::NO_CONTENT)
}

/// What is wrong with `name` as a device name, in the words the client should see: not empty, at most
/// [`MAX_NAME_CHARS`] characters, and text a display can show - which is fbt's own rule for remote text
/// ([`fbt::display_safe`]) applied to this name, so a name that would need cleaning on the way out cannot be
/// put in at all. That rule drops Unicode control characters (Cc) and format characters (Cf): a bidi
/// override (`phone\u{202E}` draws as `enohp`), a zero-width space hiding what follows it, a soft hyphen
/// that a copy out of a browser turns into something else. It also collapses repeated spaces and trims, so
/// the trimmed name the caller passes in is exactly what survives to the file and back out again.
fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("a device name needs at least one character".to_string());
    }
    let chars = name.chars().count();
    if chars > MAX_NAME_CHARS {
        return Some(format!(
            "a device name is at most {MAX_NAME_CHARS} characters, this one has {chars}",
        ));
    }
    if fbt::display_safe(name, MAX_NAME_CHARS) != name {
        return Some(
            "a device name is text a display can show: no tabs, no zero-width or bidi marks, one space between words"
                .to_string(),
        );
    }
    None
}

/// The body of `POST /api/v1/auth/devices`: what to call the device, and its public key in the one-line
/// OpenSSH format.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewDevice {
    name: String,
    public_key: String,
}

/// Reads `config.yaml`, gives `change` its device list to alter, writes the file and puts the new list
/// where logins read it - one request at a time, which is what makes this a change rather than a race.
///
/// This is [`Shared::write_config`]'s read-modify-write under the one lock every product write shares: the
/// speed and downloads saves go through it too, so a device add and a settings save started together both
/// survive. What this adds on top is putting the list logins check against where the write left it: a key
/// can log in as soon as this answers, without a restart. The change itself brings the device list out of
/// the config, changes it, and only its own edit is this route's - the accounts, the folders and the limits
/// in the file belong to whoever wrote it.
///
/// The list logins check against is replaced inside the lock, with the config the write just saved, so a
/// remove racing an add cannot leave the two of them swapped: two swaps made after the lock was released
/// could land in the reverse order and leave a key the file no longer has still accepted. A refusal from
/// `change` - a name that is already there, a name to remove that is not - writes nothing and swaps
/// nothing, and a file this server cannot read is answered `500 internal` and left exactly where it is
/// (see [`Shared::write_config`]).
async fn change(
    shared: &Arc<Shared>,
    change: impl FnOnce(&mut Vec<DeviceEntry>) -> Result<(), RouteError> + Send + 'static,
) -> Result<(), RouteError> {
    // A second handle on the same state, for the step that runs while the config lock is held: the closure
    // below cannot borrow `shared` again while `write_config` is borrowing it.
    let applying = Arc::clone(shared);
    shared
        .write_config(
            move |config| {
                // The device list is part of the server section. A config that never named one gets it here:
                // this route only answers to a server that is already running, so registering a key with it is
                // the same choice `featherbittorrent device add` makes from a command line.
                let server = config.server.get_or_insert_with(ServerConfig::default);
                change(&mut server.auth.devices)
            },
            move |config| {
                // What logins check against becomes what was just written, so from here the file and this run
                // agree. A key in it that cannot be used says so here too, where a person who has just edited
                // the list by hand would want to hear it.
                let devices = config
                    .server
                    .as_ref()
                    .map(|server| server.auth.devices.clone())
                    .unwrap_or_default();
                *locked(&applying.devices) = DeviceTable::build(&devices);
            },
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MAX_NAME_CHARS, name_problem};

    /// A device name is text a display can show, in the same sense fbt uses for text that arrived from
    /// somewhere else: at most [`MAX_NAME_CHARS`] characters of it, nothing invisible inside it, and one
    /// space between words. That is what keeps `phone\u{202e}` - which draws as `enohp`, so a list lies
    /// about which device is in it - out of the list and out of the log line that names whoever signed in,
    /// and it is the same rule the response applies on the way out, so what a client reads back is what it
    /// can send back.
    #[test]
    fn device_names_are_text_a_display_can_show() {
        for good in [
            "phone",
            "work laptop of alice",
            "LAN-Server_2",
            "l\u{e9}n-serv\u{e9}",
            "phone \u{1f4f1}",
        ] {
            assert_eq!(name_problem(good), None, "{good} is a name");
        }

        // One character over the limit, and the answer says by how much.
        let long = "n".repeat(MAX_NAME_CHARS + 1);
        let problem = name_problem(&long).expect("one character too many is a problem");
        assert!(problem.contains("at most"), "{problem}");
        assert!(problem.contains("65"), "{problem}");

        // The invisible ones: characters that move the display around, hide what follows them, or mean
        // something else again once copied out of a browser. All of them are Unicode format characters (Cf)
        // or control characters (Cc), which is exactly the set fbt drops from remote text.
        for invisible in [
            "phone\u{202e}",
            "a\u{200b}b",
            "\u{feff}phone",
            "soft\u{ad}hyphen",
            "tab\tname",
            "new\nline",
        ] {
            assert!(
                name_problem(invisible).is_some(),
                "{invisible:?} would not survive being displayed",
            );
        }

        // Whitespace is one space, in the middle and at the ends: a name that needs cleaning on its way out
        // cannot be put in at all, because then the list could not be used to delete what it lists. (The
        // caller trims the ends; `name_problem` sees what remains.)
        assert!(name_problem("work  laptop").is_some());
        assert!(
            name_problem("").is_some(),
            "trimmed to nothing by the caller"
        );

        // A combining mark with nothing to sit on lands on whatever came before it.
        assert!(name_problem("\u{301}x").is_some());
    }
}
