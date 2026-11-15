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
//! new one.
//!
//! A name is free text within limits (see [`MAX_NAME_CHARS`] and [`name_problem`]), which means it can
//! contain characters that need percent-encoding: what `GET /api/v1/auth/devices` reports is what a
//! client has to send back, encoded, to reach that device again.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use fbtt_core::{Config, ConfigLoad, DeviceEntry};
use serde::Deserialize;

use super::{DeviceRow, DeviceTable, RouteError, Shared, clean, locked};
use crate::auth::keys;

/// How long a device name may be, in characters. Long enough for "work laptop of alice", short enough
/// that it is one line wherever it is drawn.
const MAX_NAME_CHARS: usize = 64;

/// `GET /api/v1/auth/devices`: the devices a key login recognises, as `[{name, fingerprint}]`, in the
/// order `config.yaml` has them. Entries whose key this server cannot use are not here - they have no
/// fingerprint to show - and say so in the log at startup.
pub(super) async fn list(State(shared): State<Arc<Shared>>) -> Json<Vec<DeviceRow>> {
    Json(shared.device_rows())
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
    // A name in the path that cannot be read - a stray percent sign - gets this API's error shape too.
    let Path(name) = given?;
    change(&shared, |devices| {
        let before = devices.len();
        devices.retain(|device| device.name != name);
        if devices.len() == before {
            return Err(RouteError::not_found(format!(
                "no device named {}",
                clean(&name),
            )));
        }
        Ok(())
    })
    .await?;
    tracing::info!(device = %clean(&name), "device removed");
    Ok(StatusCode::NO_CONTENT)
}

/// What is wrong with `name` as a device name, in the words the client should see: not empty, at most
/// [`MAX_NAME_CHARS`] characters, and nothing that would let a name forge a log line or fail to survive
/// a round trip through a config file - control characters, including tabs and newlines. Surrounding
/// spaces are trimmed by the caller rather than refused.
fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("a device name needs at least one character".to_string());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Some(format!(
            "a device name is at most {MAX_NAME_CHARS} characters, this one has {}",
            name.chars().count(),
        ));
    }
    if name.chars().any(char::is_control) {
        return Some("a device name cannot contain tabs or other control characters".to_string());
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
/// The config written back is the one that was on disk with only `server.auth.devices` touched: the
/// accounts, the folders and the limits in it belong to whoever wrote the file, not to this route. With
/// no file yet - or with one this run could not read as YAML, which [`Config::load`] has already moved
/// aside under another name - what gets written is the config this run started on, so adding a device
/// cannot quietly change which library is being served.
async fn change(
    shared: &Arc<Shared>,
    change: impl FnOnce(&mut Vec<DeviceEntry>) -> Result<(), RouteError>,
) -> Result<(), RouteError> {
    let _guard = shared.changing_devices.lock().await;
    let mut config = match Config::load(&shared.paths) {
        Ok(ConfigLoad::Loaded(config)) => config,
        Ok(_) => shared.base_config.clone(),
        Err(problem) => {
            return Err(RouteError::internal(format!(
                "cannot read the config ({problem})",
            )));
        }
    };
    change(&mut config.server.auth.devices)?;
    let devices = config.server.auth.devices.clone();
    let file = shared.paths.config_file();
    // The bytes go into a temp file beside it, are synced and renamed over it (`Config::save`), so a
    // crash leaves either the old list or the new one and never half of each.
    tokio::task::spawn_blocking(move || config.save(&file))
        .await
        .map_err(|problem| {
            RouteError::internal(format!("the config could not be written ({problem})"))
        })?
        .map_err(|problem| RouteError::internal(format!("cannot write the config ({problem})")))?;
    // What logins check against becomes what was just written, so from here the file and this run agree.
    // A key in it that cannot be used says so here too, where a person who has just edited the list by
    // hand would want to hear it.
    *locked(&shared.devices) = DeviceTable::build(&devices);
    Ok(())
}
