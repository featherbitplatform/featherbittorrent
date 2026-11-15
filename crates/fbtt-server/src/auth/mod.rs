//! Who may use this server, and what a login gives them.
//!
//! Three ways in, matching the config's three ([`fbtt_core::AuthConfig`]): nobody has to sign in at
//! all (auth off, which is what a loopback bind defaults to), a password, or a device key that
//! signs a challenge nonce. All three end the same way, with a session token: from then on every
//! request carries it as `Authorization: Bearer <token>`, and [`FbttAuth`] is what fbt-api asks
//! before it serves one.
//!
//! Everything here is in memory, behind a [`Mutex`] with nothing slow inside it, because of two
//! facts about how it is used. fbt-api calls [`Authenticator::authorize`] synchronously on the
//! connection's own task, so an Argon2 hash running there would hold up that connection: the hash
//! is verified in the login route instead, never here. And a restart empties these stores, so a
//! client has to log in again - which is what a token that expires anyway is for.

pub mod keys;
pub mod limiter;
pub mod password;
pub mod sessions;

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use axum::http::{HeaderMap, Method, header};
use fbt_api::{AuthRejection, AuthRequest, Authenticator};
use fbtt_core::ServerConfig;

pub use fbtt_core::auth::LOGIN_NAMESPACE;

/// The clock a login store keeps time by: the process's own in a run, whatever a test wants.
///
/// A session lives an hour and a challenge one minute. A test that waited for either would spend
/// the waiting, and a suite made of those is a suite nobody runs twice. So every store here is
/// handed its clock instead of reading one itself, and a test hands in a counter it moves forward
/// by hand.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The clock a run uses: the monotonic clock, which no change to the wall clock moves.
fn wall_clock() -> Clock {
    Arc::new(Instant::now)
}

/// Takes a store's lock, poison and all.
///
/// A panic inside a critical section poisons a [`Mutex`], and the next lock would panic too: one
/// bad request would leave every later one failing, in a way fbt-api can only catch by being handed
/// a panic again. None of these stores hold an invariant a panic could break beyond the login that
/// panicked, so the poison is unwrapped rather than repeated.
pub(crate) fn locked<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The routes a token is how you get one, so requiring one there would be circular. `POST` only:
/// `DELETE /api/v1/auth/session` logs a session out and has to know whose it is.
const LOGIN_ROUTES: [(Method, &str); 2] = [
    (Method::POST, "/api/v1/auth/challenge"),
    (Method::POST, "/api/v1/auth/session"),
];

/// Whether `(method, matched_path)` names one of the login routes, by exact equality with the paths
/// these are mounted at. A suffix match - the way fbt-api matches its own exempt list - fails open:
/// a route that merely ends in `/api/v1/auth/session` (an admin's session list under a prefix) would
/// be handed the exemption too. A request that matched no route at all is not a login route either.
fn login_route(method: &Method, matched_path: Option<&str>) -> bool {
    LOGIN_ROUTES
        .iter()
        .any(|(route, path)| route == method && matched_path == Some(path))
}

/// The authorization scheme a session token is presented in, matched without regard to case.
const BEARER_SCHEME: &str = "Bearer";

/// The token of `Authorization: Bearer <token>`: the scheme in any case, then exactly one space, as
/// fbt-api's own bearer parsing spells it. `None` when the header is absent, cannot be read as text,
/// or is in another form - which is why [`FbttAuth`] asks whether a header is there before it decides
/// that no credential came in at all, and why logging out has to treat "no token" as an answer rather
/// than an error.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_at_checked(BEARER_SCHEME.len())?;
    let token = token.strip_prefix(' ')?;
    scheme.eq_ignore_ascii_case(BEARER_SCHEME).then_some(token)
}

/// Who may use this server: fbt-api's authenticator over the session, challenge and limiter stores.
///
/// With auth off everything is allowed, which is what a trusted LAN or a loopback-only server asks
/// for (fbt-api's own `Auth::Disabled` guard - loopback peer, local `Host`, matching `Origin` - is
/// not used then because we hand it [`fbt_api::Auth::Custom`] instead; the bind address is what
/// decides). With auth on, only the two login routes are open and everything else needs a token these
/// sessions issued.
pub struct FbttAuth {
    /// Whether anyone has to sign in: what [`ServerConfig::auth_on`] answered.
    on: bool,
    sessions: sessions::Sessions,
    challenges: sessions::Challenges,
    limiter: limiter::LoginLimiter,
    /// The separate cap on how often one address may ask for a nonce: asking is not an attempt at a
    /// login, so it does not spend the failure budget above.
    challenge_limiter: limiter::ChallengeLimiter,
}

impl FbttAuth {
    /// The login stores for one server config, on or off exactly as [`ServerConfig::auth_on`] says.
    pub fn new(server: &ServerConfig) -> Self {
        Self::with_clock(server, wall_clock())
    }

    /// The same stores on a clock the caller moves, so a test can expire a session without waiting
    /// for an hour.
    pub fn with_clock(server: &ServerConfig, clock: Clock) -> Self {
        FbttAuth {
            on: server.auth_on(),
            sessions: sessions::Sessions::with_clock(clock.clone()),
            challenges: sessions::Challenges::with_clock(clock.clone()),
            limiter: limiter::LoginLimiter::with_clock(clock.clone()),
            challenge_limiter: limiter::ChallengeLimiter::with_clock(clock),
        }
    }

    /// Whether anyone has to sign in here at all.
    pub fn auth_on(&self) -> bool {
        self.on
    }

    /// The sessions: what a login issues and what every accepted request renews. The login routes
    /// reach the stores through these accessors rather than each holding its own copy, so exactly
    /// one thing knows who is signed in.
    pub fn sessions(&self) -> &sessions::Sessions {
        &self.sessions
    }

    /// The challenges a key login has to answer.
    pub fn challenges(&self) -> &sessions::Challenges {
        &self.challenges
    }

    /// The per-address login limiter.
    pub fn limiter(&self) -> &limiter::LoginLimiter {
        &self.limiter
    }

    /// The per-address cap on asking for a challenge, which the challenge route spends and the login
    /// route does not.
    pub fn challenge_limiter(&self) -> &limiter::ChallengeLimiter {
        &self.challenge_limiter
    }
}

impl fmt::Debug for FbttAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Counts, not contents: who is signed in belongs in a log line written on purpose, not in a
        // `Debug` printing that ends up in a panic message.
        f.debug_struct("FbttAuth")
            .field("auth", &self.on)
            .field("sessions", &self.sessions)
            .field("challenges", &self.challenges)
            .field("limiter", &self.limiter)
            .field("challenge_limiter", &self.challenge_limiter)
            .finish()
    }
}

impl Authenticator for FbttAuth {
    fn authorize(&self, request: &AuthRequest<'_>) -> Result<(), AuthRejection> {
        if !self.on {
            return Ok(());
        }
        // The two login routes are how you get a token at all, so they cannot ask for one. Everything
        // else - including `DELETE /api/v1/auth/session`, which has to know whose session it is
        // ending - needs one.
        if login_route(request.method, request.matched_path) {
            return Ok(());
        }
        // The header, or the query parameter on the routes fbt-api says take one (the file streams and
        // the event stream, which a browser cannot send headers on): everywhere else it hands us
        // `None`, so this does not have to know which routes those are. Only when there is no header
        // at all: a header that is here but is not a bearer token is a wrong credential, not a missing
        // one, and letting the query token answer it would let a request pick which credential loses.
        let token = match request.headers.get(header::AUTHORIZATION) {
            Some(_) => bearer(request.headers),
            None => request.query_token,
        };
        if token.is_some_and(|token| self.sessions.check(token).is_some()) {
            Ok(())
        } else {
            Err(AuthRejection::Unauthorized)
        }
    }
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct TestClock(Arc<Mutex<Instant>>);

#[cfg(test)]
impl TestClock {
    /// A clock standing at `start`, and the handle to hand a store.
    pub(crate) fn starting_at(start: Instant) -> (Self, Clock) {
        let clock = Self(Arc::new(Mutex::new(start)));
        let handle = clock.clone();
        let shared: Clock = Arc::new(move || *handle.0.lock().expect("the test clock"));
        (clock, shared)
    }

    /// Moves the clock forward.
    pub(crate) fn advance(&self, by: std::time::Duration) {
        let mut now = self.0.lock().expect("the test clock");
        *now += by;
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthRejection, Authenticator, FbttAuth, LOGIN_NAMESPACE, TestClock, bearer};
    use axum::http::{HeaderMap, Method, header};
    use fbtt_core::{AuthConfig, ServerConfig};
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::{Duration, Instant};

    /// The server config a test needs: the bind only matters for what auth defaults to.
    fn server(on: bool) -> ServerConfig {
        ServerConfig {
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 8090)),
            allowed_roots: Vec::new(),
            auth: AuthConfig {
                enabled: Some(on),
                users: Vec::new(),
                devices: Vec::new(),
            },
        }
    }

    /// An `AuthRequest` the way fbt-api's auth layer would build one.
    fn request<'a>(
        method: &'a Method,
        matched_path: Option<&'a str>,
        headers: &'a HeaderMap,
    ) -> fbt_api::AuthRequest<'a> {
        fbt_api::AuthRequest::new(method, matched_path, headers, None, None)
    }

    /// Auth off lets every request through; auth on opens only the two login routes and otherwise
    /// wants a token this server issued.
    #[test]
    fn authorizer_routes() {
        let open = FbttAuth::new(&server(false));
        let none = HeaderMap::new();
        assert!(
            open.authorize(&request(&Method::GET, Some("/api/v1/torrents"), &none))
                .is_ok()
        );

        let (clock, handle) = TestClock::starting_at(Instant::now());
        let auth = FbttAuth::with_clock(&server(true), handle);
        assert!(
            auth.authorize(&request(
                &Method::POST,
                Some("/api/v1/auth/challenge"),
                &none
            ))
            .is_ok(),
            "a challenge is how you get a token"
        );
        assert!(
            auth.authorize(&request(&Method::POST, Some("/api/v1/auth/session"), &none))
                .is_ok(),
            "the session route is where the password goes"
        );
        assert!(matches!(
            auth.authorize(&request(&Method::GET, Some("/api/v1/torrents"), &none)),
            Err(AuthRejection::Unauthorized),
        ));
        assert!(
            matches!(
                auth.authorize(&request(
                    &Method::DELETE,
                    Some("/api/v1/auth/session"),
                    &none
                )),
                Err(AuthRejection::Unauthorized),
            ),
            "logging out has to know whose session it is"
        );
        assert!(
            matches!(
                auth.authorize(&request(&Method::GET, None, &none)),
                Err(AuthRejection::Unauthorized),
            ),
            "a route that matched nothing is not a login route"
        );

        let (token, ttl) = auth.sessions().issue("alice".to_string());
        assert_eq!(ttl, Duration::from_secs(60 * 60));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().expect("a bearer header"),
        );
        assert!(
            auth.authorize(&request(&Method::GET, Some("/api/v1/torrents"), &headers))
                .is_ok()
        );

        let mut bogus = HeaderMap::new();
        bogus.insert(
            header::AUTHORIZATION,
            "Bearer not-a-token".parse().expect("a header"),
        );
        assert!(matches!(
            auth.authorize(&request(&Method::GET, Some("/api/v1/torrents"), &bogus)),
            Err(AuthRejection::Unauthorized),
        ));

        // The query token counts on the routes fbt-api offers it on (the streams and the event
        // stream), which is exactly where it hands one to us.
        let streamed = fbt_api::AuthRequest::new(
            &Method::GET,
            Some("/api/v1/events"),
            &none,
            Some(&token),
            None,
        );
        assert!(auth.authorize(&streamed).is_ok());

        // A header that is here but is not a bearer token is a wrong credential, not a missing one:
        // the query token does not get to answer behind it.
        let mut other_scheme = HeaderMap::new();
        other_scheme.insert(
            header::AUTHORIZATION,
            "Basic YWxpY2U6aHVudGVyNDI=".parse().expect("a header"),
        );
        let overshadowed = fbt_api::AuthRequest::new(
            &Method::GET,
            Some("/api/v1/events"),
            &other_scheme,
            Some(&token),
            None,
        );
        assert!(
            matches!(
                auth.authorize(&overshadowed),
                Err(AuthRejection::Unauthorized)
            ),
            "a good query token behind a header we do not read is still refused",
        );

        // An hour after the last request that used it, the token stops working on its own: a stolen
        // one dies eventually without anyone revoking it.
        clock.advance(Duration::from_secs(60 * 60 + 1));
        assert!(
            matches!(
                auth.authorize(&request(&Method::GET, Some("/api/v1/torrents"), &headers)),
                Err(AuthRejection::Unauthorized),
            ),
            "the hour slides with each use, and then it ends",
        );

        // `Debug` counts the sessions; it does not say who is in them.
        let printed = format!("{auth:?}");
        assert!(!printed.contains("alice"), "{printed}");
        assert!(printed.contains("sessions"), "{printed}");
    }

    /// The login exemption is exact, not a suffix match: our routes are mounted at the root, so a
    /// path that merely ends in one of them - an admin's session list under a prefix - is an ordinary
    /// request and needs a token.
    #[test]
    fn prefixed_login_path_needs_token() {
        let (_clock, handle) = TestClock::starting_at(Instant::now());
        let auth = FbttAuth::with_clock(&server(true), handle);
        let none = HeaderMap::new();

        for path in [
            "/admin/api/v1/auth/session",
            "/v2/api/v1/auth/challenge",
            "/api/v1/auth/session/",
        ] {
            assert!(
                matches!(
                    auth.authorize(&request(&Method::POST, Some(path), &none)),
                    Err(AuthRejection::Unauthorized),
                ),
                "{path} only looks like a login route",
            );
        }

        // The two paths themselves stay open.
        for path in ["/api/v1/auth/challenge", "/api/v1/auth/session"] {
            assert!(
                auth.authorize(&request(&Method::POST, Some(path), &none))
                    .is_ok(),
                "{path} is how you get a token",
            );
        }
    }

    /// The header form a bearer token arrives in, matched the way fbt-api matches it.
    #[test]
    fn bearer_header_forms() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer(&headers), None);
        for (value, expected) in [
            ("Bearer abc", Some("abc")),
            ("bearer abc", Some("abc")),
            ("BeArEr abc", Some("abc")),
            ("Bearer  abc", Some(" abc")),
            ("Bearer", None),
            ("Bearerabc", None),
            ("Basic YWxpY2U6aHVudGVyNDI=", None),
        ] {
            headers.insert(
                header::AUTHORIZATION,
                value.parse().expect("a header value"),
            );
            assert_eq!(bearer(&headers), expected, "{value}");
        }
    }

    /// The namespace is one spelling both ends read.
    #[test]
    fn login_namespace_is_the_shared_spelling() {
        assert_eq!(LOGIN_NAMESPACE, fbtt_core::auth::LOGIN_NAMESPACE);
    }
}
