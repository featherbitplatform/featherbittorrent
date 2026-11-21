//! Browser sessions end to end: the cookie a sign-in hands out, and what a request carrying it may do.
//!
//! These are the parts of a session that only show up over http. The CSRF value's derivation, the cookie's
//! exact-name parsing and the guard's order of credentials are checked in `src/auth/mod.rs`; what is checked
//! here is what a browser actually sees: the flags on the `Set-Cookie`, whether a page that has the cookie
//! and nothing else can change something, whether a page signed in another way gets dragged along by it, and
//! what happens when the session behind it ends.
//!
//! The engine settings are loopback-only, as in `tests/server.rs`: nothing here can leave the machine, so
//! what these tests exercise is the cookie rather than the BitTorrent parts, and every wait has a deadline so
//! a server that stops answering is a failure with a message instead of a hung test job.

use std::path::Path;
use std::time::Duration;

use fbtt_core::scratch::ScratchDir;
use fbtt_core::{AuthConfig, Config, CookieSecure, LibraryLock, Paths, ServerConfig, UserEntry};
use fbtt_server::Server;
use fbtt_server::auth::password;
use reqwest::{Response, StatusCode};
use serde_json::Value;

/// How long a test waits for anything from the server. Starting an engine takes a moment and an Argon2
/// verify is meant to be slow; nothing here should take longer than that many times over.
const DEADLINE: Duration = Duration::from_secs(60);
/// A session token's life, from this crate's session store: what `expires_in` may never exceed, and the
/// shorter of the two lifetimes a signed-in browser is told about.
const SESSION_TTL_SECS: u64 = 3600;
/// The cookie's own life, which is longer: a browser that comes back within a week may find its session
/// gone, and the page signs in again.
const COOKIE_MAX_AGE: u64 = 7 * 24 * 60 * 60;
/// How many failed logins one address gets in a window. The web sign-in shares this budget with the bearer
/// login rather than having its own, which is what the limiter test fills up.
const LOGIN_BUDGET: usize = 10;
/// The account every test here signs in as, the password it signs in with, and the one it does not. Neither
/// name is a secret: this is a test log, and the point of the pair is that one is on the config's list.
const USER: &str = "alice";
const SECRET: &str = "hunter2";

/// Runs `what` under the deadline, so a server that never answers is a failure with a message rather than
/// a hung test job.
async fn within<T>(what: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
}

/// A server with a library of its own, on a scratch directory kept alive beside it.
struct Served {
    /// The scratch library, removed when this goes down. Held for no other reason than that everything the
    /// server writes is inside it, and its `Drop` is what takes the directory away.
    dir: ScratchDir,
    server: Server,
}

impl Served {
    /// A server on a library in a scratch directory named after `tag`, configured by `base`. The directory
    /// is made here and handed to `base`, so the config can name its download folder as soon as it knows
    /// where it is; what comes back is resolved the way a run resolves it.
    async fn start(tag: &str, base: impl FnOnce(&Path) -> Config) -> Served {
        let dir = ScratchDir::new(tag);
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        let config = base(dir.path()).resolve(&paths);
        let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
        let server = within(Server::start_with_settings(
            loopback(&config),
            config,
            &paths,
            lock,
        ))
        .await
        .expect("start the server");
        Served { dir, server }
    }

    /// The address to send requests to.
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.server.local_addr())
    }

    /// Where this server's own origin is: the one `Origin` it accepts. Every request here goes to an address
    /// rather than a name, and a browser sends the scheme, host and port it was talking to, so this is what a
    /// real same-origin request would send.
    fn origin(&self) -> String {
        format!("http://{}", self.server.local_addr())
    }

    /// Stops serving, saves and releases the library, so the scratch directory goes after the last write.
    async fn stop(self) {
        let Served { dir, server } = self;
        within(server.shutdown()).await.expect("the last save ran");
        // The save is done and the lock is released, so the directory - and the lock file inside it - go
        // here, by `ScratchDir`'s `Drop`, rather than possibly in the middle of a write.
        drop(dir);
    }
}

/// The settings every engine here runs on: this config's, with the listening moved to loopback on a port
/// the OS picks and with DHT, LSD, UPnP and NAT-PMP turned off.
fn loopback(config: &Config) -> fbt::Settings {
    let mut settings = config.session_settings();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    settings
}

/// A config that serves on a port the OS picks, with [`USER`] as its only account, logins in state `auth`,
/// cookies flagged as `cookie_secure` says, and downloads in `<scratch>/downloads`.
fn base_config(scratch: &Path, auth: Option<bool>, cookie_secure: CookieSecure) -> Config {
    Config {
        download_dir: scratch.join("downloads"),
        server: Some(ServerConfig {
            bind: "127.0.0.1:0".parse().expect("a loopback bind address"),
            auth: AuthConfig {
                enabled: auth,
                users: vec![UserEntry {
                    name: USER.to_string(),
                    password_hash: password::hash(SECRET),
                }],
                devices: Vec::new(),
            },
            cookie_secure,
            ..ServerConfig::default()
        }),
        ..Config::default()
    }
}

/// The same, with logins on and cookies flagged the default way, which is what most of these tests want: no
/// `Secure` over plain http to loopback.
fn with_alice(scratch: &Path) -> Config {
    base_config(scratch, Some(true), CookieSecure::Auto)
}

/// The same, with `server.allowed_hosts` filled with `names`.
fn with_allowed(scratch: &Path, names: &[&str]) -> Config {
    let mut config = with_alice(scratch);
    config
        .server
        .as_mut()
        .expect("a server section of our own config")
        .allowed_hosts = names.iter().map(|name| (*name).to_string()).collect();
    config
}

/// The client every request goes through: no redirects followed, no per-request timeout ([`within`] bounds a
/// test), and no proxy (every URL here is `127.0.0.1`). It also has no cookie store, which is what these
/// tests need rather than something they configure: reqwest is built here without its `cookies` feature, so
/// a cookie is only ever on a request a test put it on. A store would send the cookie on every request by
/// itself and hide which request the test was actually about.
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("an http client")
}

/// A request of `method` carrying `cookie` and nothing else that says who sent it.
fn carrying(
    client: &reqwest::Client,
    method: &str,
    url: &str,
    cookie: &str,
) -> reqwest::RequestBuilder {
    let request = match method {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "DELETE" => client.delete(url),
        other => panic!("no request builder for {other}"),
    };
    request.header(reqwest::header::COOKIE, format!("fbtt_session={cookie}"))
}

/// A `GET` carrying the session cookie and nothing else.
async fn get_with_cookie(client: &reqwest::Client, url: &str, cookie: &str) -> Response {
    within(carrying(client, "GET", url, cookie).send())
        .await
        .expect("the server answered")
}

/// A `DELETE` carrying the session cookie, and its CSRF token when it carries one.
async fn delete_with_cookie(
    client: &reqwest::Client,
    url: &str,
    cookie: &str,
    csrf: Option<&str>,
) -> Response {
    let mut request = carrying(client, "DELETE", url, cookie);
    if let Some(csrf) = csrf {
        request = request.header(CSRF, csrf);
    }
    within(request.send()).await.expect("the server answered")
}

/// The header a change carries its session's CSRF token in. Named here rather than imported: the server's
/// own copy of the name is a detail of its guard, and a test that spelled it differently would be testing
/// the test.
const CSRF: &str = "x-csrf-token";

/// `POST /api/v1/auth/web-session` with `body`, plus the extra headers a test names. The content type goes on
/// last, and only when `extra` did not name one, because reqwest's `header` appends: a test that names
/// `content-type` is saying what the body is, and sending the helper's JSON type as well would let the server
/// read the request as the JSON one it already was.
async fn post_web(
    client: &reqwest::Client,
    url: &str,
    body: Value,
    extra: &[(&str, &str)],
) -> Response {
    let mut request = client.post(url);
    if !extra
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
    }
    request = request.body(serde_json::to_vec(&body).expect("a JSON body"));
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    within(request.send()).await.expect("the server answered")
}

/// A `POST` of nothing at all with `Authorization: Basic`: the bearer login's password credential, which the
/// limiter test spends the budget with.
async fn post_basic(client: &reqwest::Client, url: &str, name: &str, secret: &str) -> Response {
    within(client.post(url).basic_auth(name, Some(secret)).send())
        .await
        .expect("the server answered")
}

/// The body of a sign-in, in the shape the route takes.
fn credentials(user: &str, secret: &str) -> Value {
    serde_json::json!({ "user": user, "password": secret })
}

/// A browser sign-in, from the password to what the browser ends up with: the cookie's value and the CSRF
/// value the page has to put on every change it makes. Checked while they are still news, because a session
/// with no life behind it, or no subject in front of it, is neither.
async fn sign_in(served: &Served) -> (String, String) {
    let response = post_web(
        &http(),
        &served.url("/api/v1/auth/web-session"),
        credentials(USER, SECRET),
        &[],
    )
    .await;
    let status = response.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "a sign-in that did not succeed: {status}"
    );
    let cookie = cookie_of(&response);
    let body = body(response).await;
    let subject = body["subject"].as_str().expect("a subject");
    let csrf = body["csrf"].as_str().expect("a CSRF value");
    let expires_in = body["expires_in"].as_u64().expect("a lifetime");
    assert_eq!(
        subject, USER,
        "the subject is this server's own name for the account",
    );
    assert!(
        expires_in > 0 && expires_in <= SESSION_TTL_SECS,
        "an implausible session lifetime: {expires_in}",
    );
    assert!(
        !csrf.is_empty(),
        "a page with no CSRF value cannot change anything"
    );
    (cookie, csrf.to_string())
}

/// The `fbtt_session` value a `Set-Cookie` handed out, taken out of the attribute list the way a browser
/// takes it: the part before the first `;`.
fn cookie_of(response: &Response) -> String {
    let name = "fbtt_session=";
    let given = set_cookie_of(response);
    let (_, value) = given
        .split(';')
        .next()
        .expect("a cookie line")
        .split_once(name)
        .unwrap_or_else(|| panic!("a Set-Cookie that is not the session's: {given}"));
    value.to_string()
}

/// The whole `Set-Cookie` line, for a test that is reading the attributes off it.
fn set_cookie_of(response: &Response) -> String {
    response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("an answer that sets a cookie")
        .to_string()
}

/// The JSON a response carries, which every route here answers in - errors included.
async fn body(response: Response) -> Value {
    within(response.json::<Value>()).await.expect("a JSON body")
}

/// The `{error: {code, message}}` every error here carries, so that one parse handles the whole API.
async fn error_of(response: Response) -> (StatusCode, String) {
    let status = response.status();
    let body = body(response).await;
    let code = body["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("an error without a code in it: {body}"))
        .to_string();
    (status, code)
}

/// Whether one answer's `Set-Cookie` carries `Secure`.
fn is_secure(set_cookie: &str) -> bool {
    set_cookie
        .split(';')
        .any(|part| part.trim().eq_ignore_ascii_case("secure"))
}

// ---------------------------------------------------------------------------
// Signing in, and what the cookie says about itself
// ---------------------------------------------------------------------------

/// A password typed into a page buys the same session a password typed at a terminal buys, and the browser is
/// told about it in the only way a browser can be told: a cookie it cannot read, and a value it can.
#[tokio::test]
async fn a_browser_signs_in_with_a_cookie() {
    let served = Served::start("web-sign-in", with_alice).await;
    let client = http();

    let (cookie, csrf) = sign_in(&served).await;
    let set_cookie = set_cookie_of(
        &post_web(
            &client,
            &served.url("/api/v1/auth/web-session"),
            credentials(USER, SECRET),
            &[],
        )
        .await,
    );
    // The attributes the design fixes, in the order it fixes them, with nothing added and nothing missing. No
    // `Secure` here: this request came over plain http with nothing forwarded, and `cookie_secure` was left
    // on `auto`.
    let (_, attributes) = set_cookie
        .split_once("; ")
        .expect("a cookie with attributes on it");
    assert_eq!(
        attributes,
        format!("HttpOnly; SameSite=Strict; Path=/; Max-Age={COOKIE_MAX_AGE}"),
        "the cookie's attributes",
    );
    assert!(
        !is_secure(&set_cookie),
        "auto with nothing forwarded is not https"
    );

    // The cookie alone opens a read: it is this server's own session, and a GET changes nothing.
    let torrents = get_with_cookie(&client, &served.url("/api/v1/torrents"), &cookie).await;
    assert_eq!(
        torrents.status(),
        StatusCode::OK,
        "the cookie is the session, and a read needs nothing else",
    );

    // What the page asks before it draws: that logins are on, who this is, and the value it must send back.
    // `no-store`, because all three of those fields are about whoever asked.
    let status =
        body(get_with_cookie(&client, &served.url("/api/v1/auth/web-session"), &cookie).await)
            .await;
    assert_eq!(status["auth"].as_bool(), Some(true));
    assert_eq!(status["subject"].as_str(), Some(USER));
    assert_eq!(
        status["csrf"].as_str(),
        Some(csrf.as_str()),
        "the same session, the same CSRF value: deriving it issues nothing new",
    );

    served.stop().await;
}

/// A cookie a browser sends by itself is a credential the page cannot refuse to send, so a call that changes
/// something has to prove it knows the session as well. Two sessions are needed to say that: one session's
/// own CSRF value and another's are the same shape of header, and only one of them is this session's.
#[tokio::test]
async fn a_cookie_request_that_changes_something_needs_the_csrf_token() {
    let served = Served::start("web-csrf", with_alice).await;
    let client = http();
    let (cookie, _) = sign_in(&served).await;
    let (other_cookie, other_csrf) = sign_in(&served).await;
    let torrents = served.url("/api/v1/torrents");

    // The change, with the cookie and nothing else: refused, and refused as "no credentials", because that is
    // what a page that cannot produce the token is.
    let response = carrying(&client, "POST", &torrents, &cookie)
        .send()
        .await
        .expect("the server answered");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a cookie call without its CSRF token was let through",
    );
    let (_, code) = error_of(response).await;
    assert_eq!(code, "unauthorized");

    // Another session's token is a token that is not this session's, which is the same refusal: the header has
    // to be cut from *this* cookie, and a page holding two sessions cannot spend one on the other.
    let response = carrying(&client, "POST", &torrents, &cookie)
        .header(CSRF, &other_csrf)
        .send()
        .await
        .expect("the server answered");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "another session's CSRF token opened this session's change",
    );

    // This session's own value opens it: `204` from the route behind the guard, which is the answer that says
    // a request was let through rather than merely not refused.
    let response = delete_with_cookie(
        &client,
        &served.url("/api/v1/auth/web-session"),
        &other_cookie,
        Some(&other_csrf),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "a cookie call with its own CSRF token was not let through",
    );

    // A read needs no token, and a header needs no CSRF either: what the guard is guarding is a credential the
    // page never chose to send, and a header is one it did choose.
    assert_eq!(
        get_with_cookie(&client, &torrents, &cookie).await.status(),
        StatusCode::OK,
        "a read changes nothing, so it asks for nothing more",
    );

    served.stop().await;
}

// ---------------------------------------------------------------------------
// The cookie never answers behind a credential the page chose
// ---------------------------------------------------------------------------

/// A wrong bearer header stays wrong when the browser happens to carry a cookie: letting the cookie answer
/// behind it would make a page's own credential a suggestion the server may overrule, and would turn every
/// mistyped header into a request that silently belongs to somebody else - in a browser, to whoever signed in
/// first.
#[tokio::test]
async fn a_bearer_header_never_falls_back_to_the_cookie() {
    let served = Served::start("web-no-fallback", with_alice).await;
    let client = http();
    let (cookie, _) = sign_in(&served).await;
    let url = served.url("/api/v1/torrents");

    // The cookie on its own is enough for this read, which is what makes the next answer the interesting one.
    assert_eq!(
        get_with_cookie(&client, &url, &cookie).await.status(),
        StatusCode::OK,
        "the session this test is about",
    );

    let response = within(
        client
            .get(&url)
            .bearer_auth("not-a-token-this-server-issued")
            .header(reqwest::header::COOKIE, format!("fbtt_session={cookie}"))
            .send(),
    )
    .await
    .expect("the server answered");
    let (status, code) = error_of(response).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a bearer header fell back to the cookie"
    );
    assert_eq!(code, "unauthorized");

    // The header is not a suggestion even when it holds a token this server did issue: what stands here is that
    // a header was offered, and a header is this server's own credential, not a browser's.
    let with_our_own = within(
        client
            .get(&url)
            .bearer_auth(&cookie)
            .header(reqwest::header::COOKIE, format!("fbtt_session={cookie}"))
            .send(),
    )
    .await
    .expect("the server answered");
    assert_eq!(
        with_our_own.status(),
        StatusCode::OK,
        "the cookie contradicted a header this server agreed with",
    );

    served.stop().await;
}

/// The query token is the credential for the routes a browser cannot send headers on - the file streams, and
/// `GET /api/v1/events` - so it is offered in the same sense a header is, and the cookie is no more an answer
/// to it. A stream URL somebody pasted into the wrong browser is a wrong credential, not a missing one.
#[tokio::test]
async fn a_query_token_never_falls_back_to_the_cookie() {
    let served = Served::start("web-query-no-fallback", with_alice).await;
    let client = http();
    let (cookie, _) = sign_in(&served).await;

    // The cookie is good on a route that takes a query token, so the answer below is about the token.
    assert_eq!(
        get_with_cookie(&client, &served.url("/api/v1/torrents"), &cookie)
            .await
            .status(),
        StatusCode::OK,
        "the session this test is about",
    );

    let response = within(
        client
            .get(served.url("/api/v1/events"))
            .query(&[("token", "not-a-token-this-server-issued")])
            .header(reqwest::header::COOKIE, format!("fbtt_session={cookie}"))
            .send(),
    )
    .await
    .expect("the server answered");
    let (status, code) = error_of(response).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a query token fell back to the cookie"
    );
    assert_eq!(code, "unauthorized");

    served.stop().await;
}

// ---------------------------------------------------------------------------
// Signing out, and what a session that is not there any more says
// ---------------------------------------------------------------------------

/// Signing out ends the session on the server and tells the browser to drop its copy. Both halves matter:
/// clearing the cookie without revoking leaves the token valid for anybody holding it, and revoking without
/// clearing leaves a browser carrying a cookie that can only ever get `401`.
#[tokio::test]
async fn sign_out_ends_the_cookie_session() {
    let served = Served::start("web-sign-out", with_alice).await;
    let client = http();
    let (cookie, csrf) = sign_in(&served).await;

    let response = delete_with_cookie(
        &client,
        &served.url("/api/v1/auth/web-session"),
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        set_cookie_of(&response),
        "fbtt_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",
        "the same cookie with nothing in it, which is how a browser is told to drop it",
    );

    // The token is gone from the server as well: a browser that kept its copy - or anybody who had read the
    // header - gets the answer for a session that is not here, not a second chance.
    let (status, code) =
        error_of(get_with_cookie(&client, &served.url("/api/v1/torrents"), &cookie).await).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the session outlived its own sign-out",
    );
    assert_eq!(code, "unauthorized");

    served.stop().await;
}

/// A browser cannot be made to sign out by a page it never visited: dropping its cookie is a change like any
/// other, so the `DELETE` needs the session's own CSRF token, and being refused for want of one must not spend
/// the session it was refused for.
#[tokio::test]
async fn sign_out_needs_the_csrf_token() {
    let served = Served::start("web-sign-out-csrf", with_alice).await;
    let client = http();
    let (cookie, csrf) = sign_in(&served).await;
    let url = served.url("/api/v1/auth/web-session");
    let torrents = served.url("/api/v1/torrents");

    let (status, code) = error_of(delete_with_cookie(&client, &url, &cookie, None).await).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a sign-out without its CSRF token was let through",
    );
    assert_eq!(code, "unauthorized");

    // Refused before the route ran, so the session is still here: still readable, and still able to sign out
    // the way it was meant to.
    assert_eq!(
        get_with_cookie(&client, &torrents, &cookie).await.status(),
        StatusCode::OK,
        "the refused sign-out took the session with it",
    );
    let signed_out = delete_with_cookie(&client, &url, &cookie, Some(&csrf)).await;
    assert_eq!(
        signed_out.status(),
        StatusCode::NO_CONTENT,
        "a session refused once will not sign out",
    );
    assert_eq!(
        set_cookie_of(&signed_out),
        "fbtt_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",
        "signing out takes the browser's copy away",
    );

    served.stop().await;
}

/// Sign-out ends the session the caller authenticated with, not whichever credential the route happens to look
/// at first. A `DELETE` carrying a bearer header is a bearer call all the way through: the guard answers it
/// from the header and never asks the cookie to prove itself, so a route that then revoked the cookie's session
/// would end somebody else's browser while its own caller stayed signed in.
#[tokio::test]
async fn sign_out_revokes_the_credential_that_authorised_it() {
    let served = Served::start("web-sign-out-which-credential", with_alice).await;
    let client = http();
    // Two sessions from two sign-ins. The first is the one that signs out - as a header, which is also what
    // its own browser is holding in its cookie - and the second is the one this request's cookie happens to
    // carry, belonging to a browser that is not in this test at all.
    let (bearer_a, _csrf_a) = sign_in(&served).await;
    let (cookie_b, csrf_b) = sign_in(&served).await;
    let url = served.url("/api/v1/auth/web-session");
    let torrents = served.url("/api/v1/torrents");

    let response = within(
        client
            .delete(&url)
            .bearer_auth(&bearer_a)
            .header(reqwest::header::COOKIE, format!("fbtt_session={cookie_b}"))
            .send(),
    )
    .await
    .expect("the server answered");
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "a sign-out carrying its bearer token was refused",
    );

    // The cookie was never asked to authorise this call, so its session is still here.
    assert_eq!(
        get_with_cookie(&client, &torrents, &cookie_b)
            .await
            .status(),
        StatusCode::OK,
        "sign-out revoked a session other than the caller's",
    );

    // And the session that did authorise it is gone. A session token is a bearer token as well, so the same
    // value comes back now as the answer for a token this server has never heard of.
    let (status, code) = error_of(get_with_cookie(&client, &torrents, &bearer_a).await).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the session that signed out is still live",
    );
    assert_eq!(code, "unauthorized");

    // The other browser is untouched by any of it, and signs out with its own token.
    let signed_out = delete_with_cookie(&client, &url, &cookie_b, Some(&csrf_b)).await;
    assert_eq!(
        signed_out.status(),
        StatusCode::NO_CONTENT,
        "the cookie's own sign-out was refused",
    );

    served.stop().await;
}

/// When the cookie carries `Secure` is a question about how this server is reached rather than about the
/// request, which is why it is configured. Under `auto` the answer comes from what a proxy says it saw; under
/// `always` nothing is asked, which is what the person behind a proxy that forwards no proto sets.
#[tokio::test]
async fn the_secure_flag() {
    let served = Served::start("web-secure-auto", |scratch| {
        base_config(scratch, Some(true), CookieSecure::Auto)
    })
    .await;
    let client = http();
    let url = served.url("/api/v1/auth/web-session");

    let plain = set_cookie_of(&post_web(&client, &url, credentials(USER, SECRET), &[]).await);
    assert!(
        !is_secure(&plain),
        "auto with nothing forwarded: this request came over http",
    );

    let forwarded = set_cookie_of(
        &post_web(
            &client,
            &url,
            credentials(USER, SECRET),
            &[("x-forwarded-proto", "https")],
        )
        .await,
    );
    assert!(is_secure(&forwarded), "a proxy said it spoke https");

    // The standard spelling of the same fact, among parameters this route has no use for.
    let standard = set_cookie_of(
        &post_web(
            &client,
            &url,
            credentials(USER, SECRET),
            &[("forwarded", "for=1.2.3.4;proto=https")],
        )
        .await,
    );
    assert!(
        is_secure(&standard),
        "the same answer from `Forwarded`, which is the header the RFC spells out",
    );

    // Under `always`, the request is not asked: a proxy that says nothing about the scheme is the case this
    // setting exists for.
    let served_always = Served::start("web-secure-always", |scratch| {
        base_config(scratch, Some(true), CookieSecure::Always)
    })
    .await;
    let always = set_cookie_of(
        &post_web(
            &http(),
            &served_always.url("/api/v1/auth/web-session"),
            credentials(USER, SECRET),
            &[],
        )
        .await,
    );
    assert!(is_secure(&always), "`always` does not ask the request");

    served.stop().await;
    served_always.stop().await;
}

// ---------------------------------------------------------------------------
// The other two answers this route gives
// ---------------------------------------------------------------------------

/// With logins off there is nothing to sign in to, and the page has to be told that whatever it sends: the
/// sign-in route checks the password and not whether logins are required, so a cookie can exist on a server
/// like this one, and reading it back must not make the page believe somebody is here. No `subject`, no CSRF
/// value, and the `no-store` the answer needs because it is about whoever asked.
#[tokio::test]
async fn web_session_says_when_logins_are_off() {
    let served = Served::start("web-logins-off", |scratch| {
        base_config(scratch, Some(false), CookieSecure::Auto)
    })
    .await;
    let client = http();
    let url = served.url("/api/v1/auth/web-session");

    let response = within(client.get(&url).send())
        .await
        .expect("the server answered");
    assert_eq!(
        response.headers()["cache-control"]
            .to_str()
            .expect("a cache-control header"),
        "no-store",
        "this answer is about whoever sent it",
    );
    let status = body(response).await;
    assert_eq!(status["auth"].as_bool(), Some(false));
    assert!(
        status["subject"].is_null() && status["csrf"].is_null(),
        "nobody is signed in, so there is no name and no session's value to give: {status}",
    );

    // A cookie that is not this server's changes that answer into nothing: with logins off a stranger's token
    // is not somebody, and the page should not be told it is.
    let foreign = body(
        carrying(&client, "GET", &url, "not-a-token-this-server-issued")
            .send()
            .await
            .expect("the server answered"),
    )
    .await;
    assert_eq!(foreign["auth"].as_bool(), Some(false));

    // Signing out with nobody signed in still takes the cookie away, because that is the thing a page needs
    // this call to do whatever else it says about who is here.
    let logged_out =
        delete_with_cookie(&client, &url, "not-a-token-this-server-issued", None).await;
    assert_eq!(
        logged_out.status(),
        StatusCode::NO_CONTENT,
        "logging out of nothing still means you are logged out",
    );
    assert_eq!(
        set_cookie_of(&logged_out),
        "fbtt_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",
    );

    // A cookie does not change that answer either. The sign-in route checks the password and not whether logins
    // are required, so a browser really can sign in on a server like this one; what it is told when it asks is
    // still that nobody has to be signed in, with no name and no session's value to fill a page in with.
    let (cookie, _csrf) = sign_in(&served).await;
    let signed_in = body(get_with_cookie(&client, &url, &cookie).await).await;
    assert_eq!(
        signed_in["auth"].as_bool(),
        Some(false),
        "a cookie was read as a session on a server with nothing to sign in to",
    );
    assert!(
        signed_in["subject"].is_null() && signed_in["csrf"].is_null(),
        "with logins off there is still no name and no session's value to give: {signed_in}",
    );

    served.stop().await;
}

/// One budget for guessing, whatever route the guessing arrives at: a page posting passwords and a terminal
/// posting them are the same person spending the same allowance, and the door that closes on one closes on the
/// other. That is the point of sharing the check, and the point of the shared limiter.
#[tokio::test]
async fn web_sign_in_shares_the_login_limiter() {
    let served = Served::start("web-limiter", with_alice).await;
    let client = http();

    for attempt in 1..=LOGIN_BUDGET {
        let response =
            post_basic(&client, &served.url("/api/v1/auth/session"), USER, "wrong").await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "attempt {attempt} of the budget, at the bearer login",
        );
    }

    // The budget is spent, and the web sign-in is where it shows: the right password arrives at the other
    // route and is refused before it is looked at.
    let (status, code) = error_of(
        post_web(
            &client,
            &served.url("/api/v1/auth/web-session"),
            credentials(USER, SECRET),
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "the web sign-in has its own budget",
    );
    assert_eq!(code, "too_many_requests");

    // And the other way round, on a server of its own: guessing at the browser route closes the browser route.
    let fresh = Served::start("web-limiter-other-way", with_alice).await;
    for attempt in 1..=LOGIN_BUDGET {
        let response = post_web(
            &http(),
            &fresh.url("/api/v1/auth/web-session"),
            credentials(USER, "wrong"),
            &[],
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "attempt {attempt} of the budget, at the web sign-in",
        );
    }
    let (status, _) = error_of(
        post_web(
            &http(),
            &fresh.url("/api/v1/auth/web-session"),
            credentials(USER, SECRET),
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "guessing at the web sign-in spends nothing",
    );

    served.stop().await;
    fresh.stop().await;
}

/// The two gates a sign-in puts up before it looks at a password are both about where the request came from
/// and what it said it was, and both are worth having: this server's answers are about the library on this
/// person's disk, and a page from another site has no business getting one issued a cookie for it.
#[tokio::test]
async fn web_sign_in_refuses_other_origins_and_bodies() {
    let served = Served::start("web-origins", with_alice).await;
    let client = http();
    let url = served.url("/api/v1/auth/web-session");

    let response = post_web(
        &client,
        &url,
        credentials(USER, SECRET),
        &[("origin", "http://evil.example")],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a sign-in from another origin was accepted"
    );
    let (_, code) = error_of(response).await;
    assert_eq!(code, "forbidden");

    // The same body, the same address, this server's own origin: the gate is about where, not whether.
    let same_origin = post_web(
        &client,
        &url,
        credentials(USER, SECRET),
        &[("origin", &served.origin())],
    )
    .await;
    assert_eq!(
        same_origin.status(),
        StatusCode::OK,
        "the same origin this server answers is refused",
    );

    // A body that does not say it is JSON is not this route's body: `415`, rather than being parsed anyway or
    // refused as a malformed one. A form that posted the wrong content type is a form that is not this page's.
    let response = post_web(
        &http(),
        &url,
        credentials(USER, SECRET),
        &[("content-type", "text/plain")],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "a body of another media type was read as JSON anyway",
    );
    let (_, code) = error_of(response).await;
    assert_eq!(code, "unsupported_media_type");

    // The origin gate comes before the password is looked at, which is what keeps a hostile page from spending
    // a person's attempt budget or this server's hashing on a credential it will never get: the wrong password
    // from the same foreign origin gets the same `403`.
    let response = post_web(
        &client,
        &url,
        credentials(USER, "wrong"),
        &[("origin", "http://evil.example")],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a sign-in from another origin was accepted"
    );

    served.stop().await;
}

// ---------------------------------------------------------------------------
// Reaching the browser half through a host name
// ---------------------------------------------------------------------------

/// With logins on, a sign-in reached at a host name answers whatever that name is in
/// `server.allowed_hosts`: an `Origin` that is exactly the request's own `Host` is the page this server
/// served, and the thing that turns it into a session is the password, which no other page is holding.
#[tokio::test]
async fn sign_in_through_a_host_name_is_accepted() {
    let served = Served::start("web-host-name", with_alice).await;
    let client = http();
    let url = served.url("/api/v1/auth/web-session");

    // The public name, at its scheme's default port on both sides: `Host` written without one and
    // `Origin` written without one, which is exactly what a browser sends over https.
    let through_name = post_web(
        &client,
        &url,
        credentials(USER, SECRET),
        &[
            ("host", "torrent.example.test"),
            ("origin", "https://torrent.example.test"),
        ],
    )
    .await;
    assert_eq!(
        through_name.status(),
        StatusCode::OK,
        "a sign-in reached through a public host name was refused for its origin"
    );

    // The same name with its port spelled on both headers, which is what a browser sends for a
    // non-default port. Nothing in this server's config named `nas.local`, and nothing has to.
    let port = served.server.local_addr().port();
    let local = format!("nas.local:{port}");
    let through_local = post_web(
        &client,
        &url,
        credentials(USER, SECRET),
        &[
            ("host", local.as_str()),
            ("origin", format!("http://{local}").as_str()),
        ],
    )
    .await;
    assert_eq!(
        through_local.status(),
        StatusCode::OK,
        "a sign-in reached through a local host name with its port was refused for its origin"
    );

    served.stop().await;
}

/// The same-name rule runs the whole session, not just its first call: a change made with the cookie
/// and its CSRF token reaches its route when it arrives at a host name too. The torrent is one this
/// library has never had, so what proves the guard let the request through is the route's own `404`.
#[tokio::test]
async fn a_cookie_change_through_a_host_name_is_accepted() {
    let served = Served::start("web-host-name-change", with_alice).await;
    let client = http();
    let (cookie, csrf) = sign_in(&served).await;
    let port = served.server.local_addr().port();
    let local = format!("nas.local:{port}");
    let pause = served.url("/api/v1/torrents/0000000000000000000000000000000000000000/pause");

    let response = carrying(&client, "POST", &pause, &cookie)
        .header(CSRF, &csrf)
        .header(reqwest::header::HOST, local.as_str())
        .header(reqwest::header::ORIGIN, format!("http://{local}").as_str())
        .send()
        .await
        .expect("the server answered");
    let (status, code) = error_of(response).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a cookie change reached through a host name was refused for its origin instead of reaching its route"
    );
    assert_eq!(
        code, "not_found",
        "the pause answered a host-named change with {code}, not with its own unknown torrent"
    );

    served.stop().await;
}

/// The relaxed rule is only "you are the page this request was addressed as". At a host name, an
/// origin naming some other site is still refused, before the password at the sign-in and after the
/// CSRF token at a change.
#[tokio::test]
async fn an_origin_that_is_not_the_host_is_still_refused() {
    let served = Served::start("web-host-name-foreign-origin", with_alice).await;
    let client = http();
    let port = served.server.local_addr().port();
    let local = format!("nas.local:{port}");

    let response = post_web(
        &client,
        &served.url("/api/v1/auth/web-session"),
        credentials(USER, SECRET),
        &[("host", local.as_str()), ("origin", "http://evil.example")],
    )
    .await;
    let status = response.status();
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a sign-in at a host name carrying somebody else's origin was accepted"
    );
    let (_, code) = error_of(response).await;
    assert_eq!(
        code, "forbidden",
        "the sign-in refusal did not carry the forbidden code"
    );

    let (cookie, csrf) = sign_in(&served).await;
    let pause = served.url("/api/v1/torrents/0000000000000000000000000000000000000000/pause");
    let response = carrying(&client, "POST", &pause, &cookie)
        .header(CSRF, &csrf)
        .header(reqwest::header::HOST, local.as_str())
        .header(reqwest::header::ORIGIN, "http://evil.example")
        .send()
        .await
        .expect("the server answered");
    let status = response.status();
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a cookie change at a host name carrying somebody else's origin was let through"
    );
    let (_, code) = error_of(response).await;
    assert_eq!(
        code, "forbidden",
        "the cookie-change refusal did not carry the forbidden code"
    );

    served.stop().await;
}

/// Behind a proxy that rewrites `Host`, which is what the featherbit gateway's `upstream` node does,
/// the browser's origin is not the host the server sees, and the list rule is what carries it: `403`
/// while the public name is unlisted, `200` once `server.allowed_hosts` names it.
#[tokio::test]
async fn a_rewritten_host_needs_the_listed_name() {
    let served = Served::start("web-rewritten-host", with_alice).await;
    let port = served.server.local_addr().port();
    let address = format!("127.0.0.1:{port}");

    let response = post_web(
        &http(),
        &served.url("/api/v1/auth/web-session"),
        credentials(USER, SECRET),
        &[
            ("host", address.as_str()),
            ("origin", "https://public.example"),
        ],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a sign-in through a rewritten Host was accepted for an origin server.allowed_hosts does not list"
    );

    // The same request answers once the gateway's public name is listed: it is the list, and not any
    // agreement between origin and Host, that carries a rewritten request.
    let listed = Served::start("web-rewritten-host-listed", |scratch| {
        with_allowed(scratch, &["public.example"])
    })
    .await;
    let response = post_web(
        &http(),
        &listed.url("/api/v1/auth/web-session"),
        credentials(USER, SECRET),
        &[
            ("host", address.as_str()),
            ("origin", "https://public.example"),
        ],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a sign-in through a rewritten Host was refused for an origin server.allowed_hosts lists"
    );

    served.stop().await;
    listed.stop().await;
}

/// With logins off the host name is the whole defence, so none of this softens there: an origin that
/// repeats the Host is refused when the name was not listed, a Host that is answered anyway does not
/// make an unlisted origin acceptable, and only the list turns either one around.
#[tokio::test]
async fn logins_off_keep_the_host_rule() {
    let served = Served::start("web-logins-off-host", |scratch| {
        base_config(scratch, Some(false), CookieSecure::Auto)
    })
    .await;
    let client = http();
    let port = served.server.local_addr().port();
    let local = format!("nas.local:{port}");
    let url = served.url("/api/v1/auth/web-session");

    let response = post_web(
        &client,
        &url,
        credentials(USER, SECRET),
        &[
            ("host", local.as_str()),
            ("origin", format!("http://{local}").as_str()),
        ],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "logins off answered a host name this server was not told to answer"
    );

    // A Host the address rule answers anyway, and an origin the list does not know: with logins off
    // the origin still has to earn its place on the list, not merely agree with the Host.
    let address = format!("127.0.0.1:{port}");
    let response = post_web(
        &client,
        &url,
        credentials(USER, SECRET),
        &[
            ("host", address.as_str()),
            ("origin", format!("http://{local}").as_str()),
        ],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "logins off accepted an unlisted origin because the Host guard let the request in"
    );

    // Listed, the same name and origin answer exactly as the host rule has always answered them.
    let listed = Served::start("web-logins-off-host-listed", |scratch| {
        let mut config = base_config(scratch, Some(false), CookieSecure::Auto);
        config
            .server
            .as_mut()
            .expect("a server section of our own config")
            .allowed_hosts = vec!["nas.local".to_string()];
        config
    })
    .await;
    let listed_port = listed.server.local_addr().port();
    let listed_name = format!("nas.local:{listed_port}");
    let response = post_web(
        &http(),
        &listed.url("/api/v1/auth/web-session"),
        credentials(USER, SECRET),
        &[
            ("host", listed_name.as_str()),
            ("origin", format!("http://{listed_name}").as_str()),
        ],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "logins off refused a host name it was told to answer"
    );

    served.stop().await;
    listed.stop().await;
}
