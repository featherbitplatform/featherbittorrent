//! Signing a browser in and out: the cookie that carries the session, and the value that proves a change
//! came from whoever has the cookie.
//!
//! The bearer login gives a client a token to put in a header, and a header is something a program chooses
//! to send. A browser has no such choice about its cookie: it sends `fbtt_session` on every request it makes
//! to this address, whether the page that made it wanted that or not. That is what makes a sign-in route for
//! browsers worth having, and what makes three things here necessary rather than merely good practice:
//!
//! - **The cookie is not readable by script.** `HttpOnly` is what keeps a cross-site script from copying
//!   the session out of `document.cookie` and phoning it home; `SameSite=Strict` is what keeps a browser
//!   from sending it at all on a request that came from somewhere else.
//! - **Where a change came from is asked for twice.** `SameSite` is a browser's promise, so the cookie's
//!   changes also need `x-csrf-token` (see [`csrf_for`], and [`crate::auth`] for the guard that checks it)
//!   and, when a browser volunteered an `Origin`, that the origin is one this server answers. These routes
//!   are merged ahead of fbt-api's CORS layer, so a cross-origin `POST` this route answers is one CORS
//!   already let through: the origin check here is about where the request came from, not about whether the
//!   caller is allowed to read the answer.
//! - **`Secure`, so the token does not travel in the clear.** When to add it is a question about this
//!   server's deployment rather than about the request, which is why it is configured
//!   (`server.cookie_secure`) with `auto` reading the forwarding headers a proxy writes.
//!
//! The password check itself lives in [`login::verify_password`]: the same attempt budget, the same Argon2
//! work and the same dummy hash behind an unknown name, whether the credential arrived in a header, in a
//! JSON body, or later through this cookie.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fbtt_core::CookieSecure;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{RouteError, Shared, clean, login, read_capped};
use crate::auth::{bearer, csrf_for, session_cookie};

/// The cookie's life, in seconds: seven days. The session behind it is the server's own, and it expires on
/// its own schedule, which is the shorter one - a cookie that outlives its session simply buys a `401` and a
/// page that asks for the password again. Longer than that would be a promise the server does not keep.
const COOKIE_MAX_AGE: u64 = 7 * 24 * 60 * 60;

/// The cookie with nothing in it, which is how a cookie is taken away rather than left to age out: the same
/// name and the same attributes, with a life of nothing.
const CLEAR_COOKIE: &str = "fbtt_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0";

/// The media type this sign-in reads, and the only one: a page that posts something else is a page that did
/// not come from this server's own front end.
const JSON_MEDIA_TYPE: &str = "application/json";

/// `X-Forwarded-Proto`, the header a proxy writes to say what scheme it spoke to. `http` has no constant for
/// it, and the spelling is not a standard name the way `Forwarded` is; every proxy that has ever forwarded a
/// request sends this one.
const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");

/// `POST /api/v1/auth/web-session`: sign a browser in with a user name and a password.
///
/// A browser cannot put a header in a form, so the credential is in the body here rather than in
/// `Authorization: Basic` like the bearer login. Everything else is that login's: the same check
/// ([`login::verify_password`]), the same per-address attempt budget, `401` for a credential that does not
/// match and `429` once an address has spent its window, and the same `subject` in the answer. What differs
/// is what the answer carries: the session token goes into an HttpOnly cookie that script cannot read, and
/// what the page gets back is the derived CSRF value it must put on every change it makes.
///
/// Two gates come before the password is looked at, because both cost nothing and both are about who sent
/// the request rather than what they sent: the body has to say it is JSON, and an `Origin` that is present
/// has to be a place this server answers. A sign-in from elsewhere is refused for the same reason a
/// rebinding attack is refused anywhere else - this server's answers are about the library on this person's
/// disk, and a page from another site has no business getting one issued a cookie for it.
pub(super) async fn sign_in(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Result<Response, RouteError> {
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    // The body is read here, under the login routes' own cap, before anything is decided about it (see
    // [`login::session`]): a route that answers while bytes are still owed closes the connection the client
    // is still writing on, and the client reports a failed send instead of the answer it was given.
    let body = read_capped(
        &headers,
        body,
        login::MAX_LOGIN_BODY,
        login::MAX_LOGIN_REFUSED_BODY,
        "a login request",
    )
    .await?;
    if !content_type_is_json(&headers) {
        tracing::debug!(peer = %peer.ip(), content_type = %clean(&media_type(&headers)), "refused a sign-in body this route does not read");
        return Err(RouteError::unsupported_media_type(format!(
            "the body must be {JSON_MEDIA_TYPE}"
        )));
    }
    if !shared.auth.origin_is_accepted(&headers) {
        tracing::debug!(
            peer = %peer.ip(),
            origin = %clean(&header_text(&headers, &header::ORIGIN)),
            "refused a sign-in from an origin this server does not answer",
        );
        return Err(RouteError::forbidden(
            "the request came from an origin this server does not answer",
        ));
    }
    // Parsed here rather than by the `Json` extractor, so a body of the wrong shape gets this API's error
    // shape in the words this route uses, exactly as the key login's body does.
    let given: SignInRequest = serde_json::from_slice(&body).map_err(|problem| {
        RouteError::invalid_request(format!(
            "a web sign-in takes a JSON object with user and password ({})",
            clean(&problem.to_string()),
        ))
    })?;
    // The bearer login's check, so the bearer login's budget is this route's budget too: an address that
    // has guessed ten passwords at one route has spent its window at the other.
    let subject = login::verify_password(&shared, peer, &given.user, &given.password).await?;
    let (token, ttl) = shared.auth.sessions().issue(subject.clone());
    let secure = match shared.auth.cookie_secure() {
        CookieSecure::Always => true,
        CookieSecure::Auto => came_over_https(&headers),
    };
    let mut response = Json(SignInResponse {
        subject,
        csrf: csrf_for(&token),
        expires_in: ttl.as_secs(),
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, issue_cookie(&token, secure));
    Ok(response)
}

/// `GET /api/v1/auth/web-session`: what the page needs to know before it draws anything.
///
/// Whether anyone has to sign in at all (`auth`), who this request is signed in as, and the CSRF value for
/// the cookie it is already carrying. `no-store` on every answer, because the three fields describe whoever
/// sent this request: a cached copy of it would tell the next person who reaches this URL that they are the
/// one signed in.
///
/// A cookie that is not here, or not this server's, gets the `401` every other route gives for that. With
/// logins off there is nothing to be signed in to, and the answer says so rather than pretending the
/// address is somebody.
pub(super) async fn status(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let answer = match session_cookie(&headers).and_then(|token| {
        shared
            .auth
            .sessions()
            .check(token)
            .map(|subject| (token, subject))
    }) {
        Some((token, subject)) => json!({
            "auth": true,
            "subject": subject,
            "csrf": csrf_for(token),
        }),
        None if !shared.auth.auth_on() => json!({ "auth": false }),
        None => return no_store(RouteError::unauthorized().into_response()),
    };
    no_store(Json(answer).into_response())
}

/// `DELETE /api/v1/auth/web-session`: end this browser's session and take its cookie away.
///
/// The token is the one this request authenticated with. [`crate::auth`] accepts the bearer header whenever
/// it is present, so that is what this revokes when it is here and only the cookie's session when it is not.
/// A `DELETE` carrying a valid `Bearer` and somebody else's cookie would otherwise end that other session
/// while its caller stayed signed in, having skipped the CSRF check as a bearer call. A `DELETE` carries no
/// query token anywhere on this API, so there is no third credential to look at.
///
/// The cookie is cleared whether or not a session was revoked: with logins off a caller may reach this
/// route with nothing to end, and logging out of nothing still has to take the cookie away from the browser
/// that asked. Revoking a session whose cookie had already been dropped, or whose browser is not this one,
/// is the same ordinary answer.
pub(super) async fn sign_out(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if let Some(token) = bearer(&headers).or_else(|| session_cookie(&headers)) {
        // Who it was, before it stops being anyone's: the subject is this server's own name for the caller,
        // cleaned on its way into a log line like everything else that came from a file.
        let subject = shared.auth.sessions().check(token);
        shared.auth.sessions().revoke(token);
        tracing::info!(
            subject = %subject.map(|who| clean(&who)).unwrap_or_default(),
            "signed out",
        );
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_static(CLEAR_COOKIE));
    response
}

/// The cookie for a session this server just issued: the token, then the attributes that say who may see it
/// and for how long. `HttpOnly` against a script copying the token out of the page, `SameSite=Strict`
/// against a browser being made to carry it somewhere else, `Path=/` because the whole API is this
/// server's, and [`COOKIE_MAX_AGE`] as the browser's copy of a lifetime the server really holds.
fn issue_cookie(token: &str, secure: bool) -> HeaderValue {
    let value = format!(
        "fbtt_session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={COOKIE_MAX_AGE}{}",
        if secure { "; Secure" } else { "" },
    );
    // Both halves of it are this server's own doing: the attribute list is fixed text, and the token is
    // base64url of bytes this server generated, so neither can hold anything a header cannot carry.
    HeaderValue::from_str(&value).expect("a cookie of this server's own token")
}

/// The header every answer from [`status`] carries, so a proxy or a back button cannot hand one person's
/// signed-in answer to somebody who is not.
fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Whether the body this request brought says it is [`JSON_MEDIA_TYPE`], with or without parameters and in
/// any case. A `+json` spelling is not it: this route reads one shape, and a page posting it knows that.
fn content_type_is_json(headers: &HeaderMap) -> bool {
    media_type(headers) == JSON_MEDIA_TYPE
}

/// The media type a request's `Content-Type` names, without its parameters, lowercased, or empty when there
/// is no readable one. What [`content_type_is_json`] decides on, and what goes in the log line about a body
/// that was refused, which is why it is cleaned by the caller rather than trusted here.
fn media_type(headers: &HeaderMap) -> String {
    let given = header_text(headers, &header::CONTENT_TYPE);
    // The media type is everything before the first `;`. Its parameters say how the bytes are spelled, not
    // which route should read them.
    given
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// The text of a header a client wrote, or empty when it is not there or is not text. For log lines and
/// error messages, so the caller cleans it like any other remote text.
fn header_text(headers: &HeaderMap, name: &HeaderName) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// Whether the forwarding headers say this request reached this server over `https`, which is what decides
/// `Secure` when `server.cookie_secure` was left on `auto`.
///
/// Either header may describe a chain of hops, one entry per proxy, and the entry that matters is the one
/// written by the proxy closest to this server: the last. A proxy that rewrites the header instead of
/// appending to it, which is nginx's usual spelling, leaves one entry, and that is the same answer.
/// `Forwarded` is read for its `proto=` parameter only; its other parameters name somebody else's address
/// and host, and this route has no use for either. A value that is not readable as text is not `https`.
fn came_over_https(headers: &HeaderMap) -> bool {
    // The header a proxy actually sends is asked first, and an entry of it that says nothing at all - empty,
    // or only commas - is no answer, so `Forwarded` still gets its turn.
    let proto = last_entry(headers, &X_FORWARDED_PROTO)
        .or_else(|| last_entry(headers, &header::FORWARDED).map(forwarded_proto));
    proto.is_some_and(|proto| proto.eq_ignore_ascii_case("https"))
}

/// The last entry of a header that may hold a comma-separated list, or may simply have been sent twice.
/// `None` when the header is not there, or is not text, or is only commas and spaces.
fn last_entry<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    // A repeated header is a list too, and the one written last is the one nearest this server, so the
    // iteration goes backwards from the end rather than through everything before it.
    let given = headers.get_all(name).iter().next_back()?.to_str().ok()?;
    // The part after the last comma, which is all of it when there is no comma at all.
    let entry = given
        .rsplit_once(',')
        .map_or(given, |(_, last)| last)
        .trim();
    (!entry.is_empty()).then_some(entry)
}

/// The `proto=` parameter of one `Forwarded` entry, without its quotes, or `None` when the entry does not
/// name a protocol. The other parameters are left alone: they say who forwarded the request, not how.
fn forwarded_proto(entry: &str) -> &str {
    entry
        .split(';')
        .find_map(|param| {
            let (name, value) = param.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("proto")
                .then_some(value.trim().trim_matches('"'))
        })
        // An entry without `proto=` names no protocol, and that is not `https` either.
        .unwrap_or_default()
}

/// The body of `POST /api/v1/auth/web-session`: the account and its password. `user` is the name as
/// `config.yaml` spells it, the same name `Authorization: Basic` carries.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignInRequest {
    user: String,
    password: String,
}

/// What the sign-in answers: who got in, the value every change this session makes has to carry, and how
/// long the session itself lives, which is the shorter life the browser's copy cannot outlast.
#[derive(Debug, Serialize)]
struct SignInResponse {
    subject: String,
    csrf: String,
    expires_in: u64,
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use super::{
        CLEAR_COOKIE, COOKIE_MAX_AGE, JSON_MEDIA_TYPE, X_FORWARDED_PROTO, came_over_https,
        content_type_is_json, forwarded_proto, issue_cookie, last_entry, media_type,
    };

    /// A `Content-Type` header.
    fn typed(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(value).expect("a content type"),
        );
        headers
    }

    /// The media type gate is on the media type alone: a parameter is how the bytes are spelled, and case
    /// is how somebody spells the same type.
    #[test]
    fn only_json_bodies_are_read() {
        assert!(content_type_is_json(&typed("application/json")));
        assert!(content_type_is_json(&typed(
            "Application/JSON; charset=UTF-8"
        )));
        assert!(
            !content_type_is_json(&typed("text/plain")),
            "the brief's 415"
        );
        assert!(!content_type_is_json(&typed(
            "application/x-www-form-urlencoded"
        )));
        // A `+json` spelling is somebody else's JSON dialect, and this route reads one shape.
        assert!(!content_type_is_json(&typed(
            "application/merge-patch+json"
        )));
        // No header at all is not a header saying json.
        assert!(!content_type_is_json(&HeaderMap::new()));
        assert_eq!(
            media_type(&typed("application/json;q=1")),
            "application/json"
        );
        assert_eq!(media_type(&HeaderMap::new()), "", "nothing to log");
    }

    /// `Secure` under `auto` follows the forwarding headers a proxy writes, and nothing else about the
    /// request: not the peer address, not whether this server itself thinks it is behind one.
    #[test]
    fn only_a_forwarded_https_says_https() {
        let mut forwarded = HeaderMap::new();
        forwarded.insert(X_FORWARDED_PROTO, HeaderValue::from_static("https"));
        assert!(came_over_https(&forwarded));

        // `Forwarded` is the standard spelling of the same fact, hidden among parameters this route does
        // not care about, and quoted the way RFC 7239 lets it be quoted.
        let mut standard = HeaderMap::new();
        standard.insert(
            header::FORWARDED,
            HeaderValue::from_static("for=1.2.3.4;proto=https;by=203.0.113.7"),
        );
        assert!(came_over_https(&standard));
        let mut quoted = HeaderMap::new();
        quoted.insert(
            header::FORWARDED,
            HeaderValue::from_static(r#"for=1.2.3.4;proto="https""#),
        );
        assert!(came_over_https(&quoted));

        // Nothing forwarded, and http forwarded, are both "no `Secure` here": one has no answer, the other
        // says the opposite.
        assert!(!came_over_https(&HeaderMap::new()));
        let mut http = HeaderMap::new();
        http.insert(X_FORWARDED_PROTO, HeaderValue::from_static("http"));
        assert!(!came_over_https(&http));
        let mut no_proto = HeaderMap::new();
        no_proto.insert(header::FORWARDED, HeaderValue::from_static("for=1.2.3.4"));
        assert!(!came_over_https(&no_proto), "an entry naming no protocol");
    }

    /// A chain of hops says what the hop nearest this server saw, which is the last entry rather than the
    /// first somebody else could have forged.
    #[test]
    fn the_last_hop_is_the_one_that_counted() {
        let mut chain = HeaderMap::new();
        chain.insert(X_FORWARDED_PROTO, HeaderValue::from_static("http, https"));
        assert!(came_over_https(&chain), "the hop nearest us spoke https");

        let mut back_to_http = HeaderMap::new();
        back_to_http.insert(X_FORWARDED_PROTO, HeaderValue::from_static("https, http"));
        assert!(
            !came_over_https(&back_to_http),
            "and the last one is the one we trust"
        );

        // Sent twice is the same list, spread over two header lines.
        let mut twice = HeaderMap::new();
        twice.append(X_FORWARDED_PROTO, HeaderValue::from_static("http"));
        twice.append(X_FORWARDED_PROTO, HeaderValue::from_static(" https"));
        assert!(came_over_https(&twice));

        assert_eq!(last_entry(&chain, &X_FORWARDED_PROTO), Some("https"));
        assert_eq!(last_entry(&HeaderMap::new(), &X_FORWARDED_PROTO), None);
        let mut blank = HeaderMap::new();
        blank.insert(X_FORWARDED_PROTO, HeaderValue::from_static(" "));
        assert_eq!(last_entry(&blank, &X_FORWARDED_PROTO), None, "no entry");

        assert_eq!(forwarded_proto("for=1.2.3.4;proto=https"), "https");
        assert_eq!(
            forwarded_proto("for=1.2.3.4"),
            "",
            "an entry naming no protocol"
        );
    }

    /// The cookie the sign-in hands out carries the attributes the design says it does, in the order the
    /// design says them, with `Secure` only when the caller said this request came over https.
    #[test]
    fn the_cookie_is_http_only_and_scoped_to_this_server() {
        let plain = issue_cookie("abc-123", false)
            .to_str()
            .expect("a cookie of our own token")
            .to_string();
        assert_eq!(
            plain,
            format!(
                "fbtt_session=abc-123; HttpOnly; SameSite=Strict; Path=/; Max-Age={COOKIE_MAX_AGE}"
            ),
            "seven days, and no `Secure` where the request came over http",
        );
        assert_eq!(
            issue_cookie("abc-123", true)
                .to_str()
                .expect("a cookie of our own token"),
            format!("{plain}; Secure"),
            "`Secure` is added at the end, not written over something else",
        );
        // The two numbers the design fixes: seven days for a session a browser carries, and nothing at all
        // for the cookie that takes it away.
        assert_eq!(COOKIE_MAX_AGE, 7 * 24 * 60 * 60);
        assert_eq!(
            CLEAR_COOKIE,
            "fbtt_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"
        );
        assert_eq!(JSON_MEDIA_TYPE, "application/json");
    }
}
