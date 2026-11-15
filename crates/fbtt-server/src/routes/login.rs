//! Getting a session token: the challenge a device key signs, the password that skips it, and the
//! answer either one gets.
//!
//! Two credentials, one outcome. A person types a user name and a password (`Authorization: Basic`,
//! the way the design says); a device answers a nonce with an `sshsig` over it. Both end with the same
//! session token every later request carries, so nothing after login has to know which kind it came
//! from.
//!
//! Two things shape the order these handlers do things in:
//!
//! - **The answer about a user name must not come faster than the hashing.** An Argon2 verify is slow
//!   on purpose, and a route that answered "no such account" immediately would let someone with no
//!   credential at all walk the list of names by timing requests. So an unknown name is checked against
//!   a fixed dummy hash - computed once when the server starts, in [`crate::Server`] - and the answer
//!   arrives after the same work either way. The verify runs on the blocking pool: fbt-api calls the
//!   authenticator synchronously on the connection's own task, and a hash held there would stall every
//!   request that shares it.
//! - **Nothing may be trusted from the client's key but its fingerprint.** The signature is checked
//!   against the public key this server has on file for that fingerprint ([`Shared::device`]), never
//!   against the text the client sent, so a client cannot present a key of its own invention and ask
//!   us to verify against it.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, JsonRejection};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use base64::Engine as _;
use base64::prelude::{BASE64_STANDARD, BASE64_URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use super::{RouteError, Shared, clean};
use crate::auth::sessions::{CHALLENGE_TTL, random_bytes};
use crate::auth::{bearer, keys, password};

/// The largest login body fbt-api's 32 MiB would allow. A key login is three fields - a public key
/// line, a nonce and an armored signature - and this route answers strangers before it knows who they
/// are, so the room a request may make the server hold is capped well below what the API reserves for
/// uploading a `.torrent`.
pub(crate) const MAX_LOGIN_BODY: usize = 64 * 1024;

/// The nonce's and token's spelling: base64url without padding, the same one session tokens use, so
/// one decoder handles both.
fn encoded(bytes: &[u8]) -> String {
    BASE64_URL_SAFE_NO_PAD.encode(bytes)
}

/// `POST /api/v1/auth/challenge`: the bytes a device key is asked to sign, good for a minute.
///
/// Every well-formed Ed25519 key gets an answer, configured here or not: an empty answer would tell a
/// passer-by which keys this server has on file. What differs is what gets *stored*. A configured
/// device's nonce goes into the challenge store; anyone else's is random and never stored, so it can
/// never be spent and one address cannot fill the store by asking for it. Asking again adds another
/// nonce rather than cancelling an outstanding one, and this route is capped per address at thirty a
/// minute - separately from the login failure budget, because asking has guessed nothing yet.
pub(super) async fn challenge(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    given: Result<Json<ChallengeRequest>, JsonRejection>,
) -> Result<Json<ChallengeResponse>, RouteError> {
    let peer = peer.ip();
    // Before any work on the key: a rate limit that ran after the parsing would still let an address
    // spend the server's time while it was being refused.
    if let Err(retry_after) = shared.auth.challenge_limiter().check(peer) {
        return Err(RouteError::too_many_requests(retry_after));
    }
    // A body that is not JSON of this shape, or bigger than this route takes, comes back as this API's
    // error rather than in axum's plain-text one.
    let Json(body) = given?;
    let key = keys::parse_device_key(&body.public_key)
        .map_err(|problem| RouteError::invalid_request(problem.to_string()))?;
    let fingerprint = keys::fingerprint(&key);
    let configured = shared.has_device(&fingerprint);
    let nonce = if configured {
        shared.auth.challenges().issue(fingerprint)
    } else {
        random_bytes::<32>().to_vec()
    };
    // Only an address that was answered is counted, and it is counted for the answer it got.
    shared.auth.challenge_limiter().record(peer);
    tracing::debug!(peer = %peer, configured, "challenge issued");
    Ok(Json(ChallengeResponse {
        nonce: encoded(&nonce),
        expires_in: CHALLENGE_TTL.as_secs(),
    }))
}

/// `POST /api/v1/auth/session`: a session token, from a password or from an answered challenge.
///
/// With `Authorization: Basic <user:password>` the body is not read as anything; without it the body is
/// the key login's `{public_key, nonce, signature}`. A limiter check comes first on both kinds - a
/// blocked address is refused before its credential has been looked at - and every refusal counts one
/// failure for the address: ten of them in a five-minute window then closes the door to guessing. The
/// answer is the same shape either way, `{token, expires_in, subject}`, where `subject` is this
/// server's own name for whoever got in - an account name or a device name, never something the client
/// typed.
///
/// With auth off this route still answers a matching account: the accounts are in `config.yaml` whether or
/// not this server asks anyone to use them, and a client that was set up with a password should not break
/// because its address happens to be trusted. What is off here is the *need* to log in, not the logins.
pub(super) async fn session(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<SessionResponse>, RouteError> {
    let peer = peer.ip();
    if let Err(retry_after) = shared.auth.limiter().check(peer) {
        return Err(RouteError::too_many_requests(retry_after));
    }
    // A body bigger than this route allows is refused here, in this API's error shape, rather than in
    // axum's plain-text one. Nothing was hashed to find that out, so it costs the address no failure.
    let body = body?;
    match credential(&headers, &body) {
        Credential::Password(name, password) => {
            password_login(&shared, peer, &name, &password).await
        }
        Credential::Key(body) => key_login(&shared, peer, body),
    }
}

/// `DELETE /api/v1/auth/session`: end the caller's own session.
///
/// The token is the one this request authenticated with - fbt-api's guard has already accepted it, or
/// this handler would not be running. With auth off a caller may have no token at all: there is nothing
/// to revoke, and logging out of nothing still means they are logged out.
pub(super) async fn logout(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> StatusCode {
    if let Some(token) = bearer(&headers) {
        // Who it was, before it stops being anyone's. The subject is this server's own name for the
        // caller, cleaned on its way into a log line like everything else that came from a file.
        let subject = shared.auth.sessions().check(token);
        shared.auth.sessions().revoke(token);
        tracing::info!(
            subject = %subject.map(|who| clean(&who)).unwrap_or_default(),
            "signed out",
        );
    }
    StatusCode::NO_CONTENT
}

/// What the session route was handed. A `Basic` header is a password attempt; anything else - no
/// header, another scheme, or a basic credential this server cannot read - is an attempt at the key
/// login, whose body comes back unchanged for the handler to parse.
enum Credential<'a> {
    /// The user name and password of a `Basic` credential. Both are empty when the header was there but
    /// could not be read: that is a credential that does not match, so it costs the server the same
    /// verify an unknown name costs rather than being a faster answer.
    Password(String, String),
    /// The request body, to be read as a `{public_key, nonce, signature}` login.
    Key(&'a [u8]),
}

/// Which credential this request brought. See [`Credential`].
fn credential<'a>(headers: &HeaderMap, body: &'a [u8]) -> Credential<'a> {
    let encoded = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(after_basic_scheme);
    // No header, another scheme, or a basic credential this server cannot read: all of them are an
    // attempt at the key login, whose body comes back unchanged for that handler to parse.
    let Some(encoded) = encoded else {
        return Credential::Key(body);
    };
    match decode_credentials(encoded) {
        Some((name, password)) => Credential::Password(name, password),
        None => Credential::Password(String::new(), String::new()),
    }
}

/// The part of an `Authorization` value that comes after `Basic `, or `None` for anything that is not that.
/// The scheme is matched without regard to case and needs exactly one space after it, the form fbt-api's
/// own bearer parsing accepts, so a password credential and a session token cannot be confused by
/// spelling. Splitting at a checked boundary is what answers `None` for a value that merely starts
/// mid-character, rather than panicking on a slice.
fn after_basic_scheme(value: &str) -> Option<&str> {
    let (scheme, rest) = value.split_at_checked(BASIC_SCHEME.len())?;
    scheme.eq_ignore_ascii_case(BASIC_SCHEME).then_some(())?;
    rest.strip_prefix(' ')
}

/// The `Authorization: Basic` scheme name, matched without regard to case.
const BASIC_SCHEME: &str = "Basic";

/// The user and password behind `Authorization: Basic`: standard base64 of `user:password`, split at
/// the first colon so a password may contain one. `None` for anything that is not that - not base64, no
/// colon, bytes that are not text - which the caller treats as a credential that does not match.
fn decode_credentials(encoded: &str) -> Option<(String, String)> {
    let decoded = BASE64_STANDARD.decode(encoded.trim()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (name, password) = text.split_once(':')?;
    Some((name.to_string(), password.to_string()))
}

/// One login with a password: one verify against the hash on file (or the dummy), then either a token
/// or the same answer every wrong credential gets.
async fn password_login(
    shared: &Arc<Shared>,
    peer: IpAddr,
    name: &str,
    password: &str,
) -> Result<Json<SessionResponse>, RouteError> {
    // Chosen before any hashing starts, so an unknown name is asked the same question a known one is.
    let Some(phc) = shared.password_hash_for(name) else {
        return Err(refused(
            shared,
            peer,
            "password",
            "no account on this server",
        ));
    };
    // Off the connection's task: this is the slow part of logging in, and it must not hold up anything
    // else that shares the connection or the worker. The hash and the password move in with it.
    let counting = Arc::clone(shared);
    let password = password.to_string();
    let matched = tokio::task::spawn_blocking(move || {
        counting.count_verify();
        password::verify(&phc, &password)
    })
    .await
    .map_err(|problem| {
        RouteError::internal(format!("the password check did not finish ({problem})"))
    })?;
    if !matched {
        return Err(refused(shared, peer, "password", "wrong password"));
    }
    let subject = name.to_string();
    let (token, ttl) = shared.auth.sessions().issue(subject.clone());
    shared.auth.limiter().success(peer);
    tracing::info!(peer = %peer, method = "password", subject = %clean(&subject), "signed in");
    Ok(Json(SessionResponse {
        token,
        expires_in: ttl.as_secs(),
        subject,
    }))
}

/// One login with a device key: the fingerprint of the key the client names finds this server's own
/// copy of it, the nonce has to be one we handed that key and not spent, and the signature has to be
/// made by the private half of *our* copy.
fn key_login(
    shared: &Arc<Shared>,
    peer: IpAddr,
    body: &[u8],
) -> Result<Json<SessionResponse>, RouteError> {
    let given: KeyLogin = serde_json::from_slice(body).map_err(|problem| {
        RouteError::invalid_request(format!(
            "a key login takes a JSON object with public_key, nonce and signature ({})",
            clean(&problem.to_string()),
        ))
    })?;
    let Ok(key) = keys::parse_device_key(&given.public_key) else {
        // Not a key this server can read at all. It gets the answer an unknown device gets, so this
        // route does not become somewhere to test whether a key line parses - the challenge route
        // already says that out loud, and only to someone who asked for a nonce.
        return Err(refused(shared, peer, "key", "unreadable public key"));
    };
    let fingerprint = keys::fingerprint(&key);
    // The verification key is ours: found by the fingerprint of what the client named, never taken from
    // what it sent.
    let Some((subject, configured)) = shared.device(&fingerprint) else {
        return Err(refused(shared, peer, "key", "not a device here"));
    };
    let Ok(nonce) = BASE64_URL_SAFE_NO_PAD
        .decode(&given.nonce)
        .map_err(|_| ())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).map_err(|_| ()))
    else {
        // Not the shape of a nonce we hand out, so it cannot be one either.
        return Err(refused(shared, peer, "key", "nonce is not ours"));
    };
    // Answering once is what a challenge is for: presenting the nonce spends it whether or not the
    // signature then holds, so one challenge buys exactly one attempt at a signature.
    if !shared.auth.challenges().take(&fingerprint, &nonce) {
        return Err(refused(shared, peer, "key", "no such open challenge"));
    }
    if !keys::verify_login(&configured, &nonce, &given.signature) {
        return Err(refused(shared, peer, "key", "wrong signature"));
    }
    let (token, ttl) = shared.auth.sessions().issue(subject.clone());
    shared.auth.limiter().success(peer);
    tracing::info!(peer = %peer, method = "key", subject = %clean(&subject), "signed in");
    Ok(Json(SessionResponse {
        token,
        expires_in: ttl.as_secs(),
        subject,
    }))
}

/// One refused login: the failure counted against the address, and one line for the log saying which
/// of the reasons it was. The client gets the same answer for all of them - an attacker comparing
/// answers learns nothing it did not already have to guess.
///
/// Refusals are logged at `info` because the limiter bounds them: ten per address per window, so a
/// brute-force run cannot use this route to fill the log either. The name or key that was refused is
/// remote text and unverified besides, so it goes in the line only cleaned, and never with a password
/// beside it.
fn refused(shared: &Arc<Shared>, peer: IpAddr, method: &str, reason: &str) -> RouteError {
    shared.auth.limiter().failure(peer);
    tracing::info!(peer = %peer, method, reason, "login refused");
    RouteError::unauthorized()
}

/// The body of `POST /api/v1/auth/challenge`: the public key to be given a nonce, in the one-line
/// OpenSSH format.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChallengeRequest {
    public_key: String,
}

/// What the challenge route answers: the bytes to sign, base64url without padding, and how long they
/// stay answerable. The same fields a client needs to make an `sshsig`, and nothing about whether this
/// server has heard of the key it was asked for.
#[derive(Debug, Serialize)]
pub(super) struct ChallengeResponse {
    nonce: String,
    expires_in: u64,
}

/// The body of a key login: the key the client is logging in as, the nonce it was given, and its
/// `sshsig` over that nonce in `featherbittorrent-login`, armored as PEM.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyLogin {
    public_key: String,
    nonce: String,
    signature: String,
}

/// What either login answers: the token every later request carries, how long it lives from now, and
/// who this server thinks it belongs to.
#[derive(Debug, Serialize)]
pub(super) struct SessionResponse {
    token: String,
    expires_in: u64,
    subject: String,
}
