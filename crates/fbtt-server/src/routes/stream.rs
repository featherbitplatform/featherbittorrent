//! An address to stream one file from, and the short-lived token that opens it.
//!
//! A player cannot send an `Authorization` header, so this route hands out the credential in the URL: a path
//! under fbt-api's own file-stream route with a token on it. The range handling, the permits, the stall guard
//! and the content types are fbt-api's - this route decides only *which file* the address names and whether
//! a token goes with it at all.
//!
//! Two things decide the shape of the answer:
//!
//! - **The scope and the address are one string.** The token is issued for [`stream_path`] of the torrent's
//!   wire hash and the file's index, and the path handed back is that same string with `?token=` added. The
//!   guard that lets a request through ([`crate::auth::FbttAuth`]) compares the received path against the
//!   scope byte for byte, so re-spelling the address here - an upper-cased hash, a leading zero in the index
//!   - would hand out an address its own token could not open.
//! - **With logins off there is no credential to hand out.** The guard that stands in for one (the name the
//!   request carries) has already answered this request, and the stream route accepts it from loopback on
//!   the same terms, so the address comes back with nothing on it and nothing is stored.

use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::{FromRequest, Request, State};
use axum::http::{HeaderMap, StatusCode};
use fbt::{InfoHashV1, InfoHashV2, InfoHashes, SessionError};
use fbtt_core::client::TorrentId;
use fbtt_core::stream::stream_path;
use fbtt_core::stream::tokens::FIRST_USE;
use serde::{Deserialize, Serialize};

use super::{RouteError, Shared, clean};
use crate::auth::bearer;

/// The largest body this route takes. It is two short fields - a hash and a number - and fbt-api's 32 MiB is
/// room for uploading a `.torrent`, which is not what happens here. With logins off this route answers
/// whoever reaches the address, so the bytes one request may make this server hold are capped to about what
/// it could be: a megabyte of JSON naming a file is refused before any of it is parsed.
pub(super) const MAX_TOKEN_BODY: usize = 8 * 1024;

/// How much of a body over [`MAX_TOKEN_BODY`] is still read, and thrown away, before the `413` goes out.
///
/// A server that answers without reading the rest of a request closes a connection with unread bytes on
/// it, and the operating system turns that into a reset: on Windows the client's next write fails with
/// `WSAECONNABORTED`, on macOS and Linux with `ECONNRESET`, and the client reports a failed send instead of
/// the `413` that was already on its way. Reading a refused body to its end is what lets an ordinary client
/// see the answer. This bound is what keeps that from being an invitation: past it the rest is not read, the
/// connection closes, and whoever sent a body that size gets the reset it asked for. Nothing past
/// [`MAX_TOKEN_BODY`] is kept either way, so the room one request may make this server hold does not change.
pub(super) const MAX_REFUSED_BODY: usize = 64 * 1024;

/// `POST /api/v1/stream-tokens`: an address to stream one file from, answering 201 with
/// `{path, expires_in}`.
///
/// The torrent is named by either of its hashes in either case (the way fbt-api's own routes take an id) and
/// the answer names it by its wire hash in 40 lowercase hex digits, because that is the spelling a token's
/// scope is built from. A torrent this library does not have, or a file index outside its list or naming a
/// pad, gets `404`; a torrent whose metadata has not arrived gets fbt-api's own `409 no_metadata`, which is
/// what the stream route itself answers for the same torrent.
pub(super) async fn issue(
    State(shared): State<Arc<Shared>>,
    request: Request,
) -> Result<(StatusCode, Json<StreamTokenAnswer>), RouteError> {
    // The body is read here rather than by the `Json` extractor, because the extractor stops at the cap and
    // leaves the rest unread (see [`MAX_REFUSED_BODY`]). What is kept is then handed to the same extractor, so
    // a body that is not JSON of this shape - a wrong content type included - gets the answer it always got,
    // in this API's error shape rather than axum's plain-text one.
    let (parts, body) = request.into_parts();
    let headers = parts.headers.clone();
    let kept = super::read_capped(
        &headers,
        body,
        MAX_TOKEN_BODY,
        MAX_REFUSED_BODY,
        "a stream-token request",
    )
    .await?;
    let Json(body) =
        Json::<StreamTokenRequest>::from_request(Request::from_parts(parts, Body::from(kept)), &())
            .await?;
    let hashes = parse_id(&body.torrent)?;
    let Some(torrent) = shared.session().torrent(&hashes) else {
        return Err(RouteError::not_found("no such torrent"));
    };
    // The file list is the index space the answer names, so it is read from the same session the stream
    // route will read the bytes from. A torrent whose metadata has not arrived has no list to name a row of.
    let files = match torrent.files().await {
        Ok(files) => files,
        Err(SessionError::NoMetadata) => return Err(no_metadata()),
        Err(SessionError::Shutdown) => {
            return Err(RouteError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "shutting_down",
                "the server is stopping",
            ));
        }
        // Anything else is this server's own failure, and the engine's reason for it can name a file on this
        // machine. That goes where whoever runs this server reads such things; what the client gets is what
        // it can act on.
        Err(problem) => {
            tracing::warn!(
                problem = %clean(&problem.to_string()),
                "the file list of a torrent could not be read",
            );
            return Err(RouteError::internal("the file list could not be read"));
        }
    };
    // The file has to be one a player could open: the stream route refuses a pad file and an index past the
    // end of the list, so this route refuses them too rather than handing out an address that answers `404`.
    let Some(file) = files.iter().find(|file| file.index == body.file) else {
        return Err(RouteError::not_found("no such file"));
    };
    if file.pad {
        return Err(RouteError::not_found("no such file"));
    }

    // The address, spelled once: the id is the wire hash this torrent actually has - lower-cased by
    // [`TorrentId::from_wire_hash`], whatever spelling named it - and the index as a plain decimal. A token
    // issued for that string opens exactly that string.
    let id = TorrentId::from_wire_hash(torrent.info_hashes().wire_hash());
    let path = stream_path(&id, file.index);
    let answer = match shared.auth.auth_on() {
        true => {
            let token = shared.auth.stream_tokens().issue(path.clone());
            format!("{path}?token={token}")
        }
        // Nothing on this server will look for a token on the way back in, so none is issued.
        false => path,
    };
    // The subject, the torrent and the file - never the token, which is a credential from here on.
    tracing::info!(
        subject = %subject_of(&shared, &headers),
        torrent = %id.0,
        file = file.index,
        "stream address issued",
    );
    Ok((
        StatusCode::CREATED,
        Json(StreamTokenAnswer {
            path: answer,
            // The window a token has before it must be used, which is the only part of its life a client
            // can act on: once it is used, keeping it alive is the player's seeking and this server's cap.
            expires_in: FIRST_USE.as_secs(),
        }),
    ))
}

/// Who asked, for the log line: this server's own name for whoever holds the session token that
/// authenticated this request, or nothing when nobody had to sign in at all. The guard has already accepted
/// that token before this handler ran; looking it up again here asks only *who*, on the same store, and a
/// name that came out of `config.yaml` is cleaned like every other name this crate logs.
fn subject_of(shared: &Shared, headers: &HeaderMap) -> String {
    bearer(headers)
        .and_then(|token| shared.auth.sessions().check(token))
        .map(|who| clean(&who))
        .unwrap_or_default()
}

/// A torrent id as fbt-api's own routes read one: 40 hex digits are a wire hash, 64 a v2 hash, in either
/// case. Anything else gets the code and words fbt-api uses for it, so one client parse handles both sets of
/// routes - which is why [`media`](super::media) takes its ids through here too rather than spelling the
/// same rule a second time.
pub(super) fn parse_id(id: &str) -> Result<InfoHashes, RouteError> {
    let bad = || {
        RouteError::new(
            StatusCode::BAD_REQUEST,
            "invalid_id",
            "a torrent id is 40 or 64 hex digits",
        )
    };
    match id.len() {
        40 => id
            .parse::<InfoHashV1>()
            .map(InfoHashes::from_v1)
            .map_err(|_| bad()),
        64 => id
            .parse::<InfoHashV2>()
            .map(InfoHashes::from_v2)
            .map_err(|_| bad()),
        _ => Err(bad()),
    }
}

/// `409 no_metadata`: the answer fbt-api's own routes give a torrent whose metadata has not arrived, spelled
/// with its code so that one client handles "add it", "ask for an address" and "stream it".
pub(super) fn no_metadata() -> RouteError {
    RouteError::new(
        StatusCode::CONFLICT,
        "no_metadata",
        "the torrent has no metadata yet",
    )
}

/// The body of `POST /api/v1/stream-tokens`: which torrent, and which file of it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StreamTokenRequest {
    /// The torrent's hash: the 40-hex wire hash or the 64-hex v2 hash, in either case.
    torrent: String,
    /// Its file to stream, by index in the metadata's file list (pad files included).
    file: usize,
}

/// What a file costs to play from here: the path to fetch, with `?token=` on it wherever a token came with
/// it, and how long that credential may go before somebody uses it.
#[derive(Debug, Serialize)]
pub(super) struct StreamTokenAnswer {
    path: String,
    expires_in: u64,
}
