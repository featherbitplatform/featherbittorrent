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
//! - **An attempt is spent before this server spends any work on it.** The limiter takes the attempt as
//!   soon as the body is accepted - before a hash is chosen, before a signature is checked - so ten
//!   guesses arriving in parallel cost ten verifies and not one per connection: a check-first design would
//!   answer that question fifty times over before any of them had been counted. The slow work has its own
//!   bound too: the Argon2 verifies share a fixed set of permits, because each wants about 19 MiB on the
//!   blocking pool and nothing else stops an address-per-address guess from having hundreds running at
//!   once. Being refused is not itself an attempt, so knocking on a closed door costs nothing.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

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
/// minute - separately from the login failure budget, because asking has guessed nothing yet. The cap
/// counts asks rather than answers: parsing a key line and getting random bytes is what an ask costs
/// whoever received it, whether or not the answer was a nonce.
pub(super) async fn challenge(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    given: Result<Json<ChallengeRequest>, JsonRejection>,
) -> Result<Json<ChallengeResponse>, RouteError> {
    let peer = peer.ip();
    // A body that is not JSON of this shape, or bigger than this route takes, comes back as this API's
    // error rather than in axum's plain-text one - and costs the address nothing, because there was no ask
    // in it to count.
    let Json(body) = given?;
    // Taken before any work on the key: a limit that ran after the parsing would still let an address spend
    // the server's time while it was being refused, and one that counted afterwards would let every request
    // in a burst through the same question at once.
    if let Err(retry_after) = shared.auth.challenge_limiter().reserve(peer) {
        return Err(RouteError::too_many_requests(retry_after));
    }
    let key = keys::parse_device_key(&body.public_key)
        .map_err(|problem| RouteError::invalid_request(problem.to_string()))?;
    let fingerprint = keys::fingerprint(&key);
    let configured = shared.has_device(&fingerprint);
    let nonce = if configured {
        shared.auth.challenges().issue(fingerprint)
    } else {
        random_bytes::<32>().to_vec()
    };
    tracing::debug!(peer = %peer, configured, "challenge issued");
    Ok(Json(ChallengeResponse {
        nonce: encoded(&nonce),
        expires_in: CHALLENGE_TTL.as_secs(),
    }))
}

/// `POST /api/v1/auth/session`: a session token, from a password or from an answered challenge.
///
/// With `Authorization: Basic <user:password>` the body is not read as anything; without it the body is
/// the key login's `{public_key, nonce, signature}`. Once a body this route accepts has arrived, the
/// address's attempt is taken before any credential is looked at: ten of them in a five-minute window close
/// the door to guessing from that address, and one refused after the budget went costs this server no work.
/// The answer is the same shape either way, `{token, expires_in, subject}`, where `subject` is this
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
    // A body bigger than this route allows is refused here, in this API's error shape, rather than in
    // axum's plain-text one. Nothing was read out of it to find that out, so it costs the address nothing.
    let body = body?;
    // From here on the request carries a credential this server has not looked at, and everything that
    // follows is work spent on somebody else's guess: choosing a hash, running an Argon2 verify, spending a
    // challenge. Taking the attempt now is what makes "ten per window" mean ten however they arrive - a
    // burst cannot all pass a question asked before anybody had failed anything - and a refused address
    // pays nothing for being refused, so knocking on a closed door does not push it further away.
    let _attempt = shared
        .auth
        .limiter()
        .reserve(peer)
        .map_err(RouteError::too_many_requests)?;
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

/// How long a password login waits its turn at the Argon2 check before it is told to come back: long
/// enough that a person logging in behind four others does not notice, short enough that a brute-force run
/// cannot keep connections and tasks open on this server while it queues.
const MAX_VERIFY_WAIT: Duration = Duration::from_secs(10);

/// One login with a password: one verify against the hash on file (or the dummy), then either a token or
/// the same answer every wrong credential gets.
///
/// The address's attempt was already taken by [`session`]. What is left to bound here is the work itself: a
/// verify wants about 19 MiB and a core's worth of time, and the blocking pool will run as many of them as
/// arrive if nothing tells it otherwise. The permit is for the whole server, so it bounds what every
/// address together can do - which is the case an address-per-address guesser is built for, each one inside
/// its own budget and none of them limited by anybody else's. It travels down to the verify rather than
/// staying here, because a handler is not what makes the memory: whoever connected may hang up halfway,
/// and this server is still going to finish the hash it started.
async fn password_login(
    shared: &Arc<Shared>,
    peer: IpAddr,
    name: &str,
    password: &str,
) -> Result<Json<SessionResponse>, RouteError> {
    // Chosen before any hashing starts, so an unknown name is asked the same question a known one is.
    let Some(phc) = shared.password_hash_for(name) else {
        return Err(refusal(peer, "password", "no account on this server"));
    };
    // A turn at the check, or a `503` saying to come back in a second: waiting for room is bounded, so a
    // queued login neither hangs nor holds this connection open while somebody else's list gets worked
    // through. The permit owns its reference to the semaphore - that is what lets it outlive this handler -
    // so the `Arc` is cloned rather than borrowed, and the clone goes down to the verify with the closure.
    let permits = Arc::clone(&shared.verify_permits);
    let permit = tokio::time::timeout(MAX_VERIFY_WAIT, permits.acquire_owned())
        .await
        .map_err(|_| {
            RouteError::busy("too many password checks are running, try again in a second")
        })?
        .map_err(|_| RouteError::internal("the password check queue is closed"))?;
    // Off the connection's task: this is the slow part of logging in, and it must not hold up anything
    // else that shares the connection or the worker. The hash, the password and the permit all move in
    // with it, and the permit coming back is what finishing the verify means: a client that hangs up
    // halfway makes hyper drop this handler, and a permit still held out here would be handed back while
    // its 19 MiB of hashing was still running - which is the very hole the permit exists to close. A
    // blocking job cannot be cancelled, so the only thing that can end it is the verify itself.
    let counting = Arc::clone(shared);
    let password = password.to_string();
    let matched = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        counting.verify_started();
        // A test that armed a hold stops here, with the permit in hand, so it can count what is holding
        // this server's permits and hang up on them without anything else getting a turn.
        counting.park_inside_verify();
        let matched = password::verify(&phc, &password);
        counting.verify_finished();
        matched
    })
    .await
    .map_err(|problem| {
        RouteError::internal(format!("the password check did not finish ({problem})"))
    })?;
    if !matched {
        return Err(refusal(peer, "password", "wrong password"));
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
        return Err(refusal(peer, "key", "unreadable public key"));
    };
    let fingerprint = keys::fingerprint(&key);
    // The verification key is ours: found by the fingerprint of what the client named, never taken from
    // what it sent.
    let Some((subject, configured)) = shared.device(&fingerprint) else {
        return Err(refusal(peer, "key", "not a device here"));
    };
    let Ok(nonce) = BASE64_URL_SAFE_NO_PAD
        .decode(&given.nonce)
        .map_err(|_| ())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).map_err(|_| ()))
    else {
        // Not the shape of a nonce we hand out, so it cannot be one either.
        return Err(refusal(peer, "key", "nonce is not ours"));
    };
    // Answering once is what a challenge is for: presenting the nonce spends it whether or not the
    // signature then holds, so one challenge buys exactly one attempt at a signature.
    if !shared.auth.challenges().take(&fingerprint, &nonce) {
        return Err(refusal(peer, "key", "no such open challenge"));
    }
    if !keys::verify_login(&configured, &nonce, &given.signature) {
        return Err(refusal(peer, "key", "wrong signature"));
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

/// One refused login: one line for the log saying which of the reasons it was, and the same answer every
/// wrong credential gets - someone comparing answers learns nothing they did not already have to guess.
///
/// Nothing is counted here: [`session`] took the address's attempt before any of this work started, which is
/// what makes a refusal that arrived after half a second of hashing cost exactly what one arriving at once
/// would have cost. Refusals are logged at `info` because the limiter bounds them: ten per address per
/// window, so a brute-force run cannot use this route to fill the log either. The name or key that was
/// refused is remote text and unverified besides, so it goes in the line only cleaned, and never with a
/// password beside it.
fn refusal(peer: IpAddr, method: &str, reason: &str) -> RouteError {
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

#[cfg(test)]
mod tests {
    use super::{after_basic_scheme, decode_credentials};
    use base64::Engine as _;
    use base64::prelude::BASE64_STANDARD;

    /// The credential form the session route reads a password out of: standard base64 of `user:password`,
    /// split at the *first* colon so that a password may contain one. Anything else answers `None`, which the
    /// caller treats as a credential that does not match rather than as a bad request - the same answer a
    /// wrong password gets, so this route is nowhere to probe for how a header should be spelled.
    #[test]
    fn basic_credentials() {
        let encoded = |text: &str| BASE64_STANDARD.encode(text);

        assert_eq!(
            decode_credentials(&encoded("alice:hunter2")),
            Some(("alice".to_string(), "hunter2".to_string())),
        );
        // A password may contain colons; the name is what comes before the first one.
        assert_eq!(
            decode_credentials(&encoded("alice:hunter:42:more")),
            Some(("alice".to_string(), "hunter:42:more".to_string())),
        );
        // Either half being empty is still a credential - an empty password simply will not match the
        // accounts, which hash non-empty passwords.
        assert_eq!(
            decode_credentials(&encoded("alice:")),
            Some(("alice".to_string(), String::new())),
        );
        assert_eq!(
            decode_credentials(&encoded(":hunter2")),
            Some((String::new(), "hunter2".to_string())),
        );
        // Padding and surrounding spaces are ordinary spellings of the same value.
        assert_eq!(
            decode_credentials(&format!("  {}  ", encoded("alice:hunter2"))),
            Some(("alice".to_string(), "hunter2".to_string())),
        );

        // Not base64 at all; base64 whose bytes are not text; and text with no colon in it, which has no
        // password to split off. All three are a credential this server cannot read.
        let not_text = BASE64_STANDARD.encode([0xff_u8, 0xfe, 0xfd]);
        for unusable in ["!!!", not_text.as_str(), encoded("alice").as_str()] {
            assert_eq!(decode_credentials(unusable), None, "{unusable}");
        }
    }

    /// Which `Authorization` values are a basic credential at all: the scheme in any case, then exactly one
    /// space - the form fbt-api's own bearer parsing accepts, so a password and a session token cannot be
    /// confused for one another by spelling. A value whose first five bytes cut a character in half is
    /// answered too, which is what splitting at a checked boundary is for.
    #[test]
    fn basic_scheme_spelling() {
        for (value, expected) in [
            ("Basic YWxpY2U6aHVudGVyMg==", Some("YWxpY2U6aHVudGVyMg==")),
            ("basic YWxpY2U6aHVudGVyMg==", Some("YWxpY2U6aHVudGVyMg==")),
            ("BASIC abc", Some("abc")),
            // One space is the scheme's separator; a second one belongs to the credential.
            ("Basic  abc", Some(" abc")),
            ("Basic\tabc", None),
            ("Basic", None),
            ("Basicabc", None),
            ("Bearer abc", None),
            ("", None),
            // `Basi` plus a two-byte character: the fifth byte is inside it, and slicing there used to panic.
            ("Basi\u{e9}c abc", None),
        ] {
            assert_eq!(after_basic_scheme(value), expected, "{value:?}");
        }
    }
}
