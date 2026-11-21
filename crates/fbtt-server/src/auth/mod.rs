//! Who may use this server, and what a login gives them.
//!
//! Three ways in, matching the config's three ([`fbtt_core::AuthConfig`]): nobody has to sign in at
//! all (auth off, which is what a loopback bind defaults to), a password, or a device key that
//! signs a challenge nonce. All three end the same way, with a session token: from then on every
//! request carries it as `Authorization: Bearer <token>`, and [`FbttAuth`] is what fbt-api asks
//! before it serves one.
//!
//! One credential does not come from a login: the short-lived token that opens one file's stream, which a
//! media player cannot send as a header and so carries in its URL. It is checked here too, on the file-stream
//! route alone, against the exact path it was issued for - never as a bearer token and never on the event
//! stream (`stream_route`). It opens one file; a session is the whole API.
//!
//! With nobody signing in there is still one question to answer - which name the request says it came to,
//! and where it says it came from - because that is all that separates this server's own user from a page
//! somebody else opened in a browser on the same machine (see [`FbttAuth`]). With logins on every
//! request carries a credential instead, and no web page picks up a bearer token by accident.
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
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use axum::http::{HeaderMap, Method, header};
use fbt_api::{AuthRejection, AuthRequest, Authenticator};
use fbtt_core::ServerConfig;
use fbtt_core::stream::tokens::StreamTokens;

/// The clock a store with a life keeps time by: the process's own in a run, whatever a test
/// wants. The login stores and the core's stream tokens keep time by the same shape, so the
/// alias lives in the core and is re-exported here, where the login stores were the first to
/// need it.
pub use fbtt_core::Clock;
pub use fbtt_core::auth::LOGIN_NAMESPACE;

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

/// How many live stream tokens one server hands out at most, oldest evicted: the files several players
/// could plausibly have open at once, rather than one slot per client, because a player re-requests its URL
/// on every seek and each of those addresses stays live while it is being used
/// (see [`fbtt_core::stream::tokens`]).
const STREAM_TOKEN_CAP: usize = 64;

/// The matched-path template of fbt-api's file-stream route, as that crate sets it.
const STREAM_ROUTE: &str = "/api/v1/torrents/{id}/files/{index}/stream";

/// Whether `(method, matched_path)` names the file-stream route: its two methods, since a player fetches a
/// file with `HEAD` before it starts asking for ranges. Matched by suffix, the way fbt-api recognises its own
/// routes, so a router mounted under a prefix keeps the same rules; a token issued here names an unprefixed
/// path, so under a mount point one simply stops matching, which is the direction that refuses rather than
/// the one that lets through.
fn stream_route(method: &Method, matched_path: Option<&str>) -> bool {
    (*method == Method::GET || *method == Method::HEAD)
        && matched_path.is_some_and(|path| path.ends_with(STREAM_ROUTE))
}

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

/// The one name every server answers without a login, whatever its config says: the loopback convention,
/// which no amount of DNS can be made to mean something else.
const LOCAL_HOST: &str = "localhost";

/// The host part of a `Host` header or an `Origin`'s authority: the name with its port taken off, and with
/// its brackets taken off too when it is an IPv6 literal - the only way a Host may spell an address
/// (`[::1]:8090`). `None` for anything not shaped like a host at all: nothing there, a bracket that never
/// closes, a port that is not digits. Splitting on the *first* colon is what makes `[::1]` (no port),
/// `[::1]:8090` and `evil.example:8090` each come out with a name while an unbracketed IPv6 address - which
/// is not a legal Host in the first place - gets none.
fn host_part(authority: &str) -> Option<(&str, bool)> {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (name, after) = bracketed.split_once(']')?;
        let port_ok = match after.strip_prefix(':') {
            Some(port) => port.bytes().all(|byte| byte.is_ascii_digit()),
            None => after.is_empty(),
        };
        return port_ok.then_some((name, true));
    }
    // An unbracketed authority has at most one colon, and what follows it is the port - so `2001:db8::1`
    // arriving this way has no name anyone could mean by it.
    let (name, port) = match authority.split_once(':') {
        Some((name, port)) => (name, port),
        None => (authority, ""),
    };
    if !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    (!name.is_empty()).then_some((name, false))
}

/// Whether `authority` - a `Host` value, or the host part of an `Origin` - names something this server
/// answers when nobody signs in: `localhost`, an address (IPv4, or IPv6 in its bracketed form), or a name
/// from `server.allowed_hosts`. The port is not part of the answer: someone reaching this server on an odd
/// port is still reaching this server. Names are compared without regard to case because DNS does, and
/// nothing else about them is interpreted, so a suffix trick like `bit.local.evil.example` is simply a name
/// that was never listed.
fn host_is_local_or_allowed(authority: &str, allowed: &[String]) -> bool {
    let Some((name, bracketed)) = host_part(authority) else {
        return false;
    };
    if bracketed {
        // Only an address arrives bracketed, so a bracketed word is not a name here at all.
        return name.parse::<Ipv6Addr>().is_ok();
    }
    name.eq_ignore_ascii_case(LOCAL_HOST)
        || name.parse::<Ipv4Addr>().is_ok()
        || allowed.iter().any(|host| host.eq_ignore_ascii_case(name))
}

/// Whether an `Origin` header names a place this server answers when nobody signs in: `http://` or
/// `https://` followed by a host that passes the same rule as `Host`, with any port. A browser sends
/// `Origin` precisely on the cross-site requests it is allowed to make at all - which is what a rebinding
/// page uses - so an origin this server would not answer as a Host is refused as an origin too. Anything
/// that is not one of those two schemes with a host after it (`null`, `ftp://localhost`, a bare path) is
/// refused rather than guessed at.
fn origin_is_allowed(origin: &str, allowed: &[String]) -> bool {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    // An origin is `scheme://host[:port]` and nothing else, but a value that carries more than that is read
    // up to where an origin stops rather than refused for it. A userinfo prefix is not part of an origin;
    // dropping what precedes the last `@` costs nothing and cannot make a hostile one look friendly.
    let end = match rest.find(['/', '?', '#']) {
        Some(end) => end,
        None => rest.len(),
    };
    let authority = &rest[..end];
    let (_, authority) = authority.rsplit_once('@').unwrap_or(("", authority));
    host_is_local_or_allowed(authority, allowed)
}

/// Why an entry in `server.allowed_hosts` could never be the name a request carries, or `None` when it could
/// be. What gets compared is the bare name: [`host_part`] takes the port off the request's side and
/// [`host_is_local_or_allowed`] matches whole names without regard to case, so the entries people type - a
/// name with the port they mean on it, the URL they copied, a path, `*.local`, the dot DNS appends, a name
/// with a space in it - are ones no `Host` can ever equal. Saying so at startup ([`crate::Server`]) is the
/// difference between a person finding out while writing the file and finding out from a `403` about the name
/// they are sure they listed. An entry that is an address gets said too: those are answered whether they are
/// listed or not, so one of those in the list only ever hides the name it was written beside.
pub(crate) fn allowed_host_problem(entry: &str) -> Option<&'static str> {
    if entry.is_empty() {
        return Some("it is nothing");
    }
    // A `Host` carries no whitespace, and neither does a name in a URL's authority: what has some in it was
    // typed with something else around it.
    if entry.chars().any(char::is_whitespace) {
        return Some("it has whitespace in it, which a host name does not");
    }
    if entry.contains("://") {
        return Some("it spells a whole URL, and only the host part of one is compared");
    }
    if entry.contains(['/', '\\']) {
        return Some("it has a path in it, which a host never carries");
    }
    if entry.contains('*') {
        return Some("it uses `*`, and names here are matched whole rather than by prefix");
    }
    // `bit.local.` is the same host to DNS as `bit.local` and a different name to this server, which is the
    // safe direction for a rule whose job is refusing: what it refuses is somebody else's guess.
    if entry.ends_with('.') {
        return Some("it ends in a dot, which is what DNS appends rather than part of the name");
    }
    if entry.contains(':') {
        let bare = entry.strip_prefix('[').unwrap_or(entry);
        let bare = bare.strip_suffix(']').unwrap_or(bare);
        // A colon means either an address - which this server answers without being told - or a port, which
        // is taken off the request before anything is compared. Either way the entry as written matches
        // nothing.
        if bare.parse::<Ipv6Addr>().is_ok() {
            return Some("it is an address, and those are answered whether they are listed or not");
        }
        return Some("it carries a port, which is taken off before names are compared");
    }
    None
}
/// Who may use this server: fbt-api's authenticator over the session, challenge and limiter stores.
///
/// With auth off no token is asked for, but every request still has to name this server - `localhost`, an
/// address, or a host from `server.allowed_hosts` - because that is what "nobody has to sign in" means on a
/// machine other people can reach, and the only thing keeping a hostile web page out. With auth on, only the
/// two login routes are open and everything else needs a token these sessions issued, whatever name the
/// request carries: a credential no web page picks up by accident is the guard there, not the `Host`.
pub struct FbttAuth {
    /// Whether anyone has to sign in: what [`ServerConfig::auth_on`] answered.
    on: bool,
    /// The host names answered without a login besides `localhost` and an address - `server.allowed_hosts`,
    /// read once here because nothing changes it while the server runs, and consulted only with auth off.
    hosts: Vec<String>,
    sessions: sessions::Sessions,
    challenges: sessions::Challenges,
    limiter: limiter::LoginLimiter,
    /// The separate cap on how often one address may ask for a nonce: asking is not an attempt at a
    /// login, so it does not spend the failure budget above.
    challenge_limiter: limiter::ChallengeLimiter,
    /// The short-lived tokens that open one file's stream each ([`StreamTokens`]). Kept here rather than in
    /// the routes because the guard that lets a stream request through and the route that hands its token out
    /// have to agree about which tokens this server issued - the same reason they share the session store.
    stream_tokens: Arc<StreamTokens>,
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
            hosts: server.allowed_hosts.clone(),
            sessions: sessions::Sessions::with_clock(clock.clone()),
            challenges: sessions::Challenges::with_clock(clock.clone()),
            limiter: limiter::LoginLimiter::with_clock(clock.clone()),
            challenge_limiter: limiter::ChallengeLimiter::with_clock(clock.clone()),
            stream_tokens: Arc::new(StreamTokens::with_clock(STREAM_TOKEN_CAP, clock.clone())),
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

    /// The tokens that open this server's files: issued by the `crate::routes` stream-token route, checked
    /// here on the way in. One store for both, so a token this server handed out is accepted by this server
    /// and nowhere else.
    pub fn stream_tokens(&self) -> &Arc<StreamTokens> {
        &self.stream_tokens
    }

    /// The guard that stands in for a credential when nobody has to sign in: which name the request says it
    /// came to, and - if a browser sent one - where it says it came from.
    ///
    /// With logins off this server answers whoever reaches it, so all that tells a person at a terminal
    /// apart from a page someone else wrote is the name the client believes it is talking to. A DNS
    /// rebinding attack wins exactly that argument: the victim's browser is made to resolve the attacker's
    /// domain to 127.0.0.1, connects to it, and still sends `Host: evil.example` on every request. Refusing
    /// any name this server was not told to answer is what stops such a page adding a device key of its own
    /// (which outlives the page - it is written into `config.yaml`), browsing the library, and queueing a
    /// download. An address is allowed because that is how someone reaches this server from another machine
    /// with logins off, and a browser cannot be tricked into naming an address: the name in a Host header
    /// comes from the URL it was given, and only a domain can be rebound.
    ///
    /// fbt-api has the same rule for its own [`fbt_api::Auth::Disabled`] - loopback peer, local `Host`,
    /// listed `Origin` - but it does not run here: this server hands fbt-api [`fbt_api::Auth::Custom`] even
    /// with logins off, because which logins are allowed and which tokens are this server's is decided in
    /// this crate (see [`crate::Server`]). So the rule lives here too, next to the decision it stands in for.
    fn guard_local(&self, request: &AuthRequest<'_>) -> Result<(), AuthRejection> {
        // A request that names no host at all is not named by anything this server was told to answer. HTTP
        // 1.1 requires `Host`, and hyper refuses a request without it before one gets here; falling through
        // to the URI's authority covers a router mounted somewhere that hands absolute URIs instead.
        let given = request
            .headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok());
        let Some(host) = given else {
            return Err(AuthRejection::Forbidden);
        };
        if !host_is_local_or_allowed(host, &self.hosts) {
            tracing::debug!(host = %crate::routes::clean(host), "refused a host this server does not answer");
            return Err(AuthRejection::Forbidden);
        }
        // A browser that sends an Origin is telling us where it came from; believe it only for a place this
        // server would answer as a Host. Absent is ordinary: a command line, or a same-origin GET.
        if let Some(origin) = request.headers.get(header::ORIGIN) {
            let Ok(origin) = origin.to_str() else {
                return Err(AuthRejection::Forbidden);
            };
            if !origin_is_allowed(origin, &self.hosts) {
                tracing::debug!(
                    origin = %crate::routes::clean(origin),
                    "refused an origin this server does not answer",
                );
                return Err(AuthRejection::Forbidden);
            }
        }
        Ok(())
    }
}

impl fmt::Debug for FbttAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Counts, not contents: who is signed in belongs in a log line written on purpose, not in a
        // `Debug` printing that ends up in a panic message.
        f.debug_struct("FbttAuth")
            .field("auth", &self.on)
            .field("allowed_hosts", &self.hosts.len())
            .field("sessions", &self.sessions)
            .field("challenges", &self.challenges)
            .field("limiter", &self.limiter)
            .field("challenge_limiter", &self.challenge_limiter)
            .field("stream_tokens", &self.stream_tokens)
            .finish()
    }
}

impl Authenticator for FbttAuth {
    fn authorize(&self, request: &AuthRequest<'_>) -> Result<(), AuthRejection> {
        if !self.on {
            // Nobody needs a token here, so the name the request carries is the only thing left to check.
            return self.guard_local(request);
        }
        // The two login routes are how you get a token at all, so they cannot ask for one. Everything
        // else - including `DELETE /api/v1/auth/session`, which has to know whose session it is
        // ending - needs one.
        if login_route(request.method, request.matched_path) {
            return Ok(());
        }
        // A stream token is a player's credential for one file, so it is tried first exactly where a player
        // could have been sent one: the two methods of the file-stream route, against the path it was issued
        // for, and only when there is no header for it to answer behind. Nowhere else - not
        // `GET /api/v1/events`, which takes a query token but is a session's stream of news, and never as a
        // bearer header, which would make it a second way to be anybody rather than a way to fetch one file.
        // Behind it stays the session check below, so a web page that puts its own session token in the query
        // still works.
        if !request.headers.contains_key(header::AUTHORIZATION)
            && let Some(token) = request.query_token
            && stream_route(request.method, request.matched_path)
            && self.stream_tokens.check(token, request.path)
        {
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
    use super::{
        AuthRejection, Authenticator, FbttAuth, LOCAL_HOST, LOGIN_NAMESPACE, TestClock,
        allowed_host_problem, bearer, host_is_local_or_allowed, host_part, origin_is_allowed,
    };
    use axum::http::{HeaderMap, Method, header};
    use fbtt_core::{AuthConfig, ServerConfig};
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::{Duration, Instant};

    /// The server config a test needs: the bind only matters for what auth defaults to.
    fn server(on: bool) -> ServerConfig {
        server_with(on, &[])
    }

    /// The same with `server.allowed_hosts` holding `hosts`.
    fn server_with(on: bool, hosts: &[&str]) -> ServerConfig {
        ServerConfig {
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 8090)),
            allowed_roots: Vec::new(),
            allowed_hosts: hosts.iter().map(|host| (*host).to_string()).collect(),
            auth: AuthConfig {
                enabled: Some(on),
                users: Vec::new(),
                devices: Vec::new(),
            },
            ..ServerConfig::default()
        }
    }

    /// The headers a request that reaches this server carries: the `Host` it arrived with, and an `Origin`
    /// as well when a browser sent one.
    fn headers(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, host.parse().expect("a host header value"));
        if let Some(origin) = origin {
            headers.insert(
                header::ORIGIN,
                origin.parse().expect("an origin header value"),
            );
        }
        headers
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
        // Auth off answers a request that names this server without asking for a token - which names it may
        // answer is what `auth_off_answers_only_names_it_was_given` below is about.
        let open = FbttAuth::new(&server(false));
        assert!(
            open.authorize(&request(
                &Method::GET,
                Some("/api/v1/torrents"),
                &headers("127.0.0.1:8090", None),
            ))
            .is_ok(),
            "a server on loopback with logins off serves the machine it is on",
        );

        let none = HeaderMap::new();
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

    /// A stream token opens one file and nothing else: the two methods that fetch a file, on the exact path
    /// it was issued for. Another file of the same torrent, another spelling of the right one, the event
    /// stream (which takes a `?token=` too), and any `Authorization` header are all refused - while a session
    /// token still opens what it always did. This is Review Focus item 1 of the milestone plan, checked where
    /// the rule lives.
    #[test]
    fn stream_tokens_open_one_file_each() {
        let (_clock, handle) = TestClock::starting_at(Instant::now());
        let auth = FbttAuth::with_clock(&server(true), handle);
        let none = HeaderMap::new();

        let route = "/api/v1/torrents/{id}/files/{index}/stream";
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let path = format!("/api/v1/torrents/{hash}/files/0/stream");
        let token = auth.stream_tokens().issue(path.clone());

        // The request, the way fbt-api's layer builds it: the matched template, and the path as received.
        fn ask(
            auth: &FbttAuth,
            method: &Method,
            matched: Option<&str>,
            path: &str,
            query: Option<&str>,
            headers: &HeaderMap,
        ) -> Result<(), AuthRejection> {
            let request =
                fbt_api::AuthRequest::new(method, matched, headers, query, None).with_path(path);
            auth.authorize(&request)
        }

        // Its own file, on both methods a player fetches with.
        for method in [&Method::GET, &Method::HEAD] {
            assert!(
                ask(&auth, method, Some(route), &path, Some(&token), &none).is_ok(),
                "{method} on the file this token was issued for",
            );
        }

        // Another file of the same torrent, and another spelling of the right one: the scope is compared byte
        // for byte, so both are refused rather than normalised.
        for other in [
            format!("/api/v1/torrents/{hash}/files/1/stream"),
            format!("/api/v1/torrents/{hash}/files/00/stream"),
            format!("/api/v1/torrents/{}/files/0/stream", hash.to_uppercase()),
        ] {
            assert!(
                matches!(
                    ask(
                        &auth,
                        &Method::GET,
                        Some(route),
                        &other,
                        Some(&token),
                        &none
                    ),
                    Err(AuthRejection::Unauthorized)
                ),
                "{other} is not the address this token opens",
            );
        }

        // The event stream takes a query token as well, and this one is not a session token.
        assert!(
            matches!(
                ask(
                    &auth,
                    &Method::GET,
                    Some("/api/v1/events"),
                    "/api/v1/events",
                    Some(&token),
                    &none,
                ),
                Err(AuthRejection::Unauthorized)
            ),
            "a stream token is not a way to watch the library",
        );

        // A bearer header is a session's place: this token never gets to be one, not even on the URL it was
        // issued for, and it does not get to answer behind a header this server cannot read either.
        let mut bearer_headers = HeaderMap::new();
        bearer_headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().expect("a bearer header"),
        );
        assert!(
            matches!(
                ask(
                    &auth,
                    &Method::GET,
                    Some(route),
                    &path,
                    None,
                    &bearer_headers
                ),
                Err(AuthRejection::Unauthorized)
            ),
            "a stream token is not a bearer credential",
        );
        let mut other_scheme = HeaderMap::new();
        other_scheme.insert(
            header::AUTHORIZATION,
            "Basic YWxpY2U6aHVudGVyNDI=".parse().expect("a header"),
        );
        assert!(
            matches!(
                ask(
                    &auth,
                    &Method::GET,
                    Some(route),
                    &path,
                    Some(&token),
                    &other_scheme
                ),
                Err(AuthRejection::Unauthorized)
            ),
            "a header the request brought decides, and this token is not it",
        );

        // A session token still streams: the query parameter keeps working on these routes for a web page
        // that has one, which is what the stream-token check sits in front of.
        let (session, _) = auth.sessions().issue("alice".to_string());
        assert!(
            ask(
                &auth,
                &Method::GET,
                Some(route),
                &path,
                Some(&session),
                &none
            )
            .is_ok(),
            "a session token still opens the stream route",
        );

        // `Debug` counts the tokens; it does not print one.
        let printed = format!("{auth:?}");
        assert!(!printed.contains(token.as_str()), "{printed}");
        assert!(printed.contains("stream_tokens"), "{printed}");
    }

    /// With logins off, the name a request says it came to is the only credential there is, so it has to be
    /// this server's own: `localhost`, an address, or a name the config listed. A DNS-rebinding attack
    /// arrives with the attacker's domain in `Host` while connecting to 127.0.0.1, and that is what gets
    /// refused here - before the request reaches any route, this server's own or fbt-api's.
    #[test]
    fn auth_off_answers_only_names_it_was_given() {
        let open = FbttAuth::new(&server(false));
        let torrents = Method::GET;
        let path = Some("/api/v1/torrents");

        // What a person at this machine, or on this network, types into a browser.
        for host in [
            "localhost",
            "LocalHost:8090",
            "127.0.0.1",
            "127.0.0.1:8090",
            "[::1]",
            "[::1]:8090",
            "192.168.1.5:8090",
        ] {
            assert!(
                open.authorize(&request(&torrents, path, &headers(host, None)))
                    .is_ok(),
                "{host} is this server's own name",
            );
        }

        // What a rebound domain looks like: the attacker's name, or somebody else's name made to look like
        // this one. An unbracketed IPv6 address is not a host a browser sends, and a port that is not digits
        // means the value is not a host either.
        for host in [
            "evil.example",
            "localhost.evil.example",
            "127.0.0.1.nip.io",
            "bit.local",
            "[::1]x",
            "::1",
            "evil.example:notaport",
            "",
        ] {
            assert!(
                matches!(
                    open.authorize(&request(&torrents, path, &headers(host, None))),
                    Err(AuthRejection::Forbidden),
                ),
                "{host} is a name this server was not given",
            );
        }

        // A request that names nothing at all is not named by anything either.
        assert!(
            matches!(
                open.authorize(&request(&torrents, path, &HeaderMap::new())),
                Err(AuthRejection::Forbidden),
            ),
            "no Host is no name",
        );

        // A listed name answers - any case, any port - and a name that merely contains it does not.
        let listed = FbttAuth::new(&server_with(false, &["bit.local"]));
        for host in ["bit.local", "BIT.LOCAL:8090"] {
            assert!(
                listed
                    .authorize(&request(&torrents, path, &headers(host, None)))
                    .is_ok(),
                "{host} is what the config named",
            );
        }
        for host in ["bit.local.evil.example", "mybit.local", "bit.local."] {
            assert!(
                matches!(
                    listed.authorize(&request(&torrents, path, &headers(host, None))),
                    Err(AuthRejection::Forbidden),
                ),
                "{host} only looks like the name the config named",
            );
        }

        // A browser sends `Origin` on exactly the cross-site requests a rebinding page makes, so it is
        // believed only for a place this server would answer as a Host.
        assert!(
            open.authorize(&request(
                &torrents,
                path,
                &headers("127.0.0.1:8090", Some("http://127.0.0.1:8090")),
            ))
            .is_ok(),
            "a same-origin request from a place this server answers",
        );
        assert!(
            open.authorize(&request(
                &torrents,
                path,
                &headers("localhost:8090", Some("https://LOCALHOST")),
            ))
            .is_ok(),
            "https and case do not change the answer",
        );
        assert!(
            open.authorize(&request(
                &torrents,
                path,
                &headers("127.0.0.1:8090", Some("http://[::1]:8090/some/path")),
            ))
            .is_ok(),
            "what comes after the host is not part of an origin",
        );
        for origin in [
            "http://evil.example",
            "http://localhost.evil.example",
            "null",
            "ftp://localhost",
            "http://",
            "//evil.example",
        ] {
            assert!(
                matches!(
                    open.authorize(&request(
                        &torrents,
                        path,
                        &headers("127.0.0.1:8090", Some(origin))
                    )),
                    Err(AuthRejection::Forbidden),
                ),
                "{origin} is not a place this server answers",
            );
        }

        // With logins on the name decides nothing: the two login routes are open to whoever asks, and
        // everything else wants a token - from this machine or any other.
        let closed = FbttAuth::new(&server(true));
        assert!(
            closed
                .authorize(&request(
                    &Method::POST,
                    Some("/api/v1/auth/session"),
                    &headers("evil.example", None),
                ))
                .is_ok(),
            "a login attempt is not turned away for the name it used",
        );
        assert!(
            matches!(
                closed.authorize(&request(&torrents, path, &headers("127.0.0.1:8090", None),)),
                Err(AuthRejection::Unauthorized),
            ),
            "logins on asks for a token, not for a name",
        );
    }

    /// The two rules the guard is built out of, spelled out: what counts as a host in a `Host` header or an
    /// origin, and which hosts and origins are answered.
    #[test]
    fn host_and_origin_rules() {
        let listed = ["bit.local".to_string()];

        // The name, with its port taken off and an IPv6 literal's brackets taken off too - `None` when the
        // value is not a host at all. The second half of each pair says whether it arrived bracketed, which
        // is what tells an address from a name.
        for (authority, expected) in [
            ("evil.example", Some(("evil.example", false))),
            ("evil.example:8090", Some(("evil.example", false))),
            ("evil.example:", Some(("evil.example", false))),
            ("127.0.0.1:1", Some(("127.0.0.1", false))),
            ("[::1]", Some(("::1", true))),
            ("[fe80::1]:80", Some(("fe80::1", true))),
            // A bracket that does not close, a port that is not digits, and an IPv6 address that was never
            // bracketed - none of which a browser sends as a Host.
            ("[::1]8090", None),
            ("[::1]:80x", None),
            ("2001:db8::1", None),
            (":8090", None),
            ("", None),
        ] {
            assert_eq!(host_part(authority), expected, "{authority:?}");
        }

        // Which of those names this server answers: its own (`localhost` or an address, in either family)
        // and the ones the config listed - any case, any port. A name that merely looks like one of them is
        // not.
        for (authority, allowed) in [
            ("localhost", true),
            ("LocalHost:8443", true),
            ("127.0.0.1:8090", true),
            ("[::1]", true),
            ("[fe80::1]:80", true),
            ("192.168.1.5", true),
            ("bit.local", true),
            ("BIT.LOCAL:8090", true),
            ("evil.example", false),
            ("localhost.evil.example", false),
            ("bit.local.evil.example", false),
            ("[::1]x", false),
            ("2001:db8::1", false),
            (":8090", false),
            ("", false),
        ] {
            assert_eq!(
                host_is_local_or_allowed(authority, &listed),
                allowed,
                "{authority:?}",
            );
        }

        // Only `http` and `https`, and only with a host after them; the port and anything past it are not
        // part of the answer.
        for (origin, allowed) in [
            ("http://localhost", true),
            ("https://127.0.0.1:8443", true),
            ("HTTP://[::1]", true),
            ("http://bit.local:8090/app", true),
            ("http://evil.example", false),
            ("ftp://localhost", false),
            ("null", false),
            ("http://", false),
            ("http:///path", false),
        ] {
            assert_eq!(origin_is_allowed(origin, &listed), allowed, "{origin:?}");
        }
    }

    /// What `server.allowed_hosts` is compared against is a bare name with its port already taken off, so most
    /// of the ways people spell a host are entries no request could ever satisfy. Each one gets said out loud
    /// when the server starts ([`crate::Server`]) rather than left as a `403` about a name the config appears
    /// to have been told to answer - and the reason has to be the one thing the person reading it can act on.
    #[test]
    fn allowed_host_entries_that_cannot_match_are_named() {
        for usable in [
            "bit.local",
            "BIT.LOCAL",
            "server-1.lan",
            "xn--bcher-kva.local",
            LOCAL_HOST,
        ] {
            assert_eq!(allowed_host_problem(usable), None, "{usable:?} is a name");
        }

        // The second half of each pair is the word the startup line would have to contain, because "it cannot
        // match" on its own tells nobody what to type instead.
        for (entry, expected) in [
            ("bit.local:8090", "port"),
            ("http://bit.local", "URL"),
            ("https://bit.local:8443/app", "URL"),
            ("/srv/bit.local", "path"),
            ("bit.local\\share", "path"),
            ("*.local", "`*`"),
            ("*", "`*`"),
            ("bit.local.", "dot"),
            (" bit.local", "whitespace"),
            ("bit local", "whitespace"),
            ("\tbit.local", "whitespace"),
            ("", "nothing"),
            // An address in this list is not wrong so much as pointless: one is answered whether it is listed
            // or not, so the entry only hides the name written beside it.
            ("[::1]", "address"),
            ("fe80::1", "address"),
        ] {
            let problem = allowed_host_problem(entry).unwrap_or_else(|| {
                panic!("{entry:?} looks like a name to answer, so nothing should warn about it")
            });
            assert!(problem.contains(expected), "{entry:?}: {problem}");
        }
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
