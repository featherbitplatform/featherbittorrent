//! The browser UI as a browser sees it: the page without a session, the icon it names, the headers on
//! it and on its assets, and the switch that removes it again.
//!
//! Only what shows up over http is checked here. Which templates are exempt from a credential, and
//! with which methods, is checked in `src/auth/mod.rs`; what is checked here is that a page reaches a
//! person who has not signed in yet, that it reaches them with the headers that decide what the
//! browser is allowed to run and load, that the files it names are really there and really typed, and
//! that a build told not to serve it serves nothing where the page used to be.
//!
//! The asset the header checks are run against is taken from the served page rather than named here:
//! Vite gives every built file a hash, and a test that spelled one out would fail on the next build
//! for a reason that has nothing to do with what it is testing.
//!
//! The engine settings are loopback-only, as in `tests/server.rs` and `tests/web_session.rs`, and
//! every wait has a deadline, so a server that stops answering is a failure with a message instead of
//! a hung test job. Nothing here reads or sets the process environment.

#![cfg(feature = "web-ui")]

use std::path::Path;
use std::time::Duration;

use fbtt_core::scratch::ScratchDir;
use fbtt_core::{AuthConfig, Config, LibraryLock, Paths, ServerConfig, UserEntry};
use fbtt_server::Server;
use fbtt_server::auth::password;
use reqwest::{Response, StatusCode, header};

/// How long a test waits for anything from the server. Starting an engine takes a moment; nothing
/// here asks for more than a page and a couple of files.
const DEADLINE: Duration = Duration::from_secs(60);
/// The account the servers with logins on have, and the password nobody here types. Neither is a
/// secret: this is a test log, and the account is only there so "with logins on" means something.
const USER: &str = "alice";
const SECRET: &str = "hunter2";

/// The page's `Content-Security-Policy`, verbatim, as `src/web.rs` spells it.
///
/// Asserted as one string rather than one directive at a time because the whole string is the product:
/// `script-src 'self'` is only worth having next to `object-src 'none'` and `base-uri 'none'`, and a
/// directive quietly dropped from the middle of the list is exactly the mistake this catches.
/// `style-src-attr 'unsafe-inline'` is in there for the inline `style=` a progress bar and a
/// virtualised row set, and for nothing else.
const CSP: &str = concat!(
    "default-src 'self'; script-src 'self'; style-src 'self'; style-src-attr 'unsafe-inline'; ",
    "img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; font-src 'self'; ",
    "object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
);
/// What a page that is rebuilt the moment the server is, and a sign-in that must not be cached, get.
const NO_STORE: &str = "no-store";
/// What a built file whose name is its content hash gets: nobody ever asks for it again, so it may be
/// cached for as long as a cache can hold it.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Runs `what` under the deadline, so a server that never answers is a failure with a message rather
/// than a hung test job.
async fn within<T>(what: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
}

/// A server with a library of its own, on a scratch directory kept alive beside it.
struct Served {
    /// The scratch library, removed when this goes down: everything the server writes is inside it,
    /// and its `Drop` is what takes the directory away.
    dir: ScratchDir,
    server: Server,
}

impl Served {
    /// A server on a library in a scratch directory named after `tag`, configured by `base`.
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

    /// The name a request here arrives with when it does not name one: the address it was sent to,
    /// which is what a browser sends for a URL like that, and one of the names this server answers.
    fn host(&self) -> String {
        self.server.local_addr().to_string()
    }

    /// Stops serving, saves and releases the library, so the scratch directory goes after the last write.
    async fn stop(self) {
        let Served { dir, server } = self;
        within(server.shutdown()).await.expect("the last save ran");
        drop(dir);
    }
}

/// The settings every engine here runs on: this config's, with the listening moved to loopback on a
/// port the OS picks and with DHT, LSD, UPnP and NAT-PMP turned off.
fn loopback(config: &Config) -> fbt::Settings {
    let mut settings = config.session_settings();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    settings
}

/// A config that serves on a port the OS picks, with logins in state `auth` and the web UI switch set
/// to `web_ui` (`None` is the default: on), downloads in `<scratch>/downloads`, and with [`USER`] as its
/// only account whatever `auth` says.
fn base_config(scratch: &Path, auth: Option<bool>, web_ui: Option<bool>) -> Config {
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
            web_ui,
            ..ServerConfig::default()
        }),
        ..Config::default()
    }
}

/// Logins on, the web UI left as it is by default: what a browser that has not signed in yet finds.
fn with_alice(scratch: &Path) -> Config {
    base_config(scratch, Some(true), None)
}

/// Nobody has to sign in, which is what a loopback bind defaults to and is said here rather than left
/// to the default because the host guard is what the two tests that use it are about.
fn open(scratch: &Path) -> Config {
    base_config(scratch, Some(false), None)
}

/// The same, with `server.web_ui: false` in the file.
fn switched_off(scratch: &Path) -> Config {
    base_config(scratch, Some(false), Some(false))
}

/// The client every request goes through: no redirects followed, no per-request timeout ([`within`]
/// bounds a test), and no proxy (every URL here is `127.0.0.1`). Built without the cookie feature, as
/// the other server tests are: no request here is meant to be signed in, and a cookie store would send
/// one on every request by itself.
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("an http client")
}

/// A `GET` that names the `Host`, and the `Origin`, it arrived with, when the test says it does.
/// reqwest takes the `Host` from the URL, an address since every test here connects to one, so
/// overriding it is how a test sends the name a browser would send, which is what the guard reads
/// when nobody has to sign in.
async fn get(
    client: &reqwest::Client,
    url: &str,
    host: Option<&str>,
    origin: Option<&str>,
) -> Response {
    let mut request = client.get(url);
    if let Some(host) = host {
        request = request.header(reqwest::header::HOST, host);
    }
    if let Some(origin) = origin {
        request = request.header(reqwest::header::ORIGIN, origin);
    }
    within(request.send()).await.expect("the server answered")
}

/// A `HEAD` of the same address.
async fn head(client: &reqwest::Client, url: &str, host: Option<&str>) -> Response {
    let mut request = client.head(url);
    if let Some(host) = host {
        request = request.header(reqwest::header::HOST, host);
    }
    within(request.send()).await.expect("the server answered")
}

/// One header of a response, as text, or `None` when it is not there at all.
fn header_of(response: &Response, name: header::HeaderName) -> Option<String> {
    response.headers().get(name).map(|value| {
        value
            .to_str()
            .expect("a header we can read as text")
            .to_string()
    })
}

/// The four headers every answer of the web UI carries, checked on the response in hand.
///
/// Each is a separate assertion so a failure says which one went missing rather than that the set was
/// not the set.
fn assert_shielded(response: &Response, what: &str) {
    let headers = response.headers();
    assert_eq!(
        header_of(response, header::CONTENT_SECURITY_POLICY).as_deref(),
        Some(CSP),
        "{what} does not carry the page's Content-Security-Policy: {headers:?}"
    );
    assert_eq!(
        header_of(response, header::X_FRAME_OPTIONS).as_deref(),
        Some("DENY"),
        "{what} may be framed by somebody else's page: {headers:?}"
    );
    assert_eq!(
        header_of(response, header::X_CONTENT_TYPE_OPTIONS).as_deref(),
        Some("nosniff"),
        "{what} lets a browser decide what its own bytes are: {headers:?}"
    );
    assert_eq!(
        header_of(response, header::REFERRER_POLICY).as_deref(),
        Some("no-referrer"),
        "{what} tells somewhere else which torrent this page was about: {headers:?}"
    );
}

/// The body of a response as text.
async fn text(response: Response) -> String {
    within(response.text()).await.expect("a body")
}

/// The `{error: {code, message}}` every error of this server answers with, as the API's other
/// responses give it.
async fn problem_of(response: Response) -> (StatusCode, String) {
    let status = response.status();
    let body: serde_json::Value = within(response.json()).await.expect("a JSON body");
    let code = body["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("an error without a code in it: {body}"))
        .to_string();
    (status, code)
}

/// Every `/assets/...` path the served page names, in the order it names them, with the repeats
/// taken out.
///
/// Read out of the page rather than spelled out in the test, because Vite names built files after what
/// is in them and the names change with the next change to the front end.
fn asset_paths(html: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("/assets/") {
        let tail = &rest[at..];
        let end = tail.find(['"', '\'', ' ', '>', ',']).unwrap_or(tail.len());
        let path = &tail[..end];
        if !found.iter().any(|seen: &String| seen == path) {
            found.push(path.to_string());
        }
        rest = &tail[end..];
    }
    found
}

/// A browser that has not signed in gets the page: the page is how a person gets to the sign-in form,
/// so it cannot be the thing that asks for a session first.
#[tokio::test]
async fn the_page_is_served_without_a_session() {
    let served = Served::start("web-page", with_alice).await;
    let response = get(&http(), &served.url("/"), None, None).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a server with logins on still has to hand the page to somebody who has not signed in"
    );
    assert_eq!(
        header_of(&response, header::CONTENT_TYPE).as_deref(),
        Some("text/html; charset=utf-8"),
        "the page is not typed as the document it is"
    );
    let html = text(response).await;
    assert!(
        html.contains(r#"<div id="root">"#),
        "the page does not carry the element the app mounts in: {html}"
    );
    served.stop().await;
}

/// The page, and the files it names, carry the headers this server promises a browser.
///
/// The assets are fetched by the names the served page actually uses, so what is checked is the pair
/// of files a browser really loads: the script and the stylesheet, both named by a hash of their own
/// contents, which is what makes a year of caching the right answer for them and the wrong one for the
/// page.
#[tokio::test]
async fn the_page_carries_the_security_headers() {
    let served = Served::start("web-headers", with_alice).await;
    let client = http();

    let page = get(&client, &served.url("/"), None, None).await;
    assert_eq!(page.status(), StatusCode::OK, "the page was not served");
    assert_shielded(&page, "the page");
    assert_eq!(
        header_of(&page, header::CACHE_CONTROL).as_deref(),
        Some(NO_STORE),
        "a cached page would keep an old front end alive next to a new server"
    );
    let html = text(page).await;

    let assets = asset_paths(&html);
    assert!(
        assets.iter().any(|path| path.ends_with(".js")),
        "the page names no script of its own: {html}"
    );
    assert!(
        assets.iter().any(|path| path.ends_with(".css")),
        "the page names no stylesheet of its own: {html}"
    );
    for path in &assets {
        let asset = get(&client, &served.url(path), None, None).await;
        assert_eq!(
            asset.status(),
            StatusCode::OK,
            "the page names {path}, which this server does not have"
        );
        assert_shielded(&asset, &format!("the asset {path}"));
        assert_eq!(
            header_of(&asset, header::CACHE_CONTROL).as_deref(),
            Some(IMMUTABLE),
            "{path} is named by a hash of its contents and may be cached for as long as a cache holds"
        );
    }

    // The typed answer for the two kinds of file a page is made of. A stylesheet served as anything but
    // `text/css` is dropped by the browser rather than drawn wrong, and the CSP here lets no other
    // origin load it either.
    for path in &assets {
        let asset = get(&client, &served.url(path), None, None).await;
        let expected = if path.ends_with(".js") {
            "text/javascript"
        } else if path.ends_with(".css") {
            "text/css; charset=utf-8"
        } else {
            continue;
        };
        assert_eq!(
            header_of(&asset, header::CONTENT_TYPE).as_deref(),
            Some(expected),
            "{path} is not typed as what it is"
        );
    }

    served.stop().await;
}

/// The icon the page names is served, typed and shielded like everything else here, and it asks for
/// no session either: a browser that has not signed in is the one looking at the tab it is drawn in.
///
/// It gets a test of its own rather than being one more row of the header test because it is the one
/// file of the build whose name never changes. Vite renames everything under `assets/` after what is
/// in it, which is what makes a year of caching the right answer for those files; this one keeps the
/// name it was given, which is what makes a year the wrong answer for it. The body check is the
/// cheapest way to say it is the same picture the tab icon and the page's `<img>` both ask for.
#[tokio::test]
async fn the_icon_is_served_without_a_session() {
    let served = Served::start("web-icon", with_alice).await;
    let response = get(&http(), &served.url("/favicon.png"), None, None).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the icon is not something a person who has not signed in may look at"
    );
    assert_eq!(
        header_of(&response, header::CONTENT_TYPE).as_deref(),
        Some("image/png"),
        "the icon is not typed as the image it is"
    );
    assert_shielded(&response, "the icon");
    assert_eq!(
        header_of(&response, header::CACHE_CONTROL).as_deref(),
        Some(NO_STORE),
        "the icon has the same name in every build, so caching it for a year caches the wrong picture"
    );
    let body = within(response.bytes()).await.expect("a body");
    assert!(
        body.starts_with(b"\x89PNG\r\n\x1a\n"),
        "the icon the server answered with is not a PNG: {} bytes",
        body.len()
    );
    served.stop().await;
}

/// An asset the page does not name is a `404` and not the page: this server has no fallback route,
/// because the front end routes with the hash and every address that is not a file it has really is
/// missing.
#[tokio::test]
async fn an_unknown_asset_is_a_404() {
    let served = Served::start("web-asset-404", with_alice).await;
    let response = get(&http(), &served.url("/assets/nope.js"), None, None).await;
    let (status, code) = problem_of(response).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a missing asset is not missing"
    );
    assert_eq!(
        code, "not_found",
        "a missing asset is answered in the API's shape, so one parse handles every error here"
    );
    served.stop().await;
}

/// `server.web_ui: false` takes the page away and leaves the API alone.
///
/// The server has no logins here, which is what makes the answer the interesting one: with nobody
/// signing in, a path nothing answers gets the `404` the router gives it, so the failure is about the
/// web UI and not about a credential. With logins on, `GET /` on a headless server is `401`, as every
/// path with no route on it is, and that is the existing rule rather than this switch.
#[tokio::test]
async fn the_off_switch_removes_the_page() {
    let served = Served::start("web-off", switched_off).await;
    let client = http();

    let response = get(&client, &served.url("/"), None, None).await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a config that turns the web UI off still served a page"
    );
    let asset = get(&client, &served.url("/assets/nope.js"), None, None).await;
    assert_eq!(
        asset.status(),
        StatusCode::NOT_FOUND,
        "a config that turns the web UI off still served an asset"
    );

    // The API is untouched by the switch: the same server answers its routes exactly as it did before
    // the front end existed, which is the promise the headless build keeps.
    let api = get(&client, &served.url("/api/v1/torrents"), None, None).await;
    assert_eq!(
        api.status(),
        StatusCode::OK,
        "the API stopped answering because the web UI was turned off"
    );
    let torrents: serde_json::Value = within(api.json()).await.expect("a JSON body");
    assert_eq!(
        torrents,
        serde_json::json!([]),
        "a fresh library answers with no torrents"
    );

    served.stop().await;
}

/// `HEAD` gets the same answer as `GET` apart from the body, and a `404` is still shielded: the headers
/// are on everything this router answers, not only on the requests that found a file.
#[tokio::test]
async fn the_headers_hold_on_head_and_404_too() {
    let served = Served::start("web-head-404", with_alice).await;
    let client = http();

    let page = get(&client, &served.url("/"), None, None).await;
    let served_bytes = header_of(&page, header::CONTENT_LENGTH);
    text(page).await;

    let same = head(&client, &served.url("/"), None).await;
    assert_eq!(
        same.status(),
        StatusCode::OK,
        "the page does not answer HEAD as it answers GET"
    );
    assert_shielded(&same, "the page, on HEAD");
    assert_eq!(
        header_of(&same, header::CACHE_CONTROL).as_deref(),
        Some(NO_STORE),
        "a HEAD of the page would cache it"
    );
    assert_eq!(
        header_of(&same, header::CONTENT_LENGTH).as_deref(),
        served_bytes.as_deref(),
        "a HEAD of the page does not say how big the page is, which is the whole point of asking"
    );
    assert_eq!(
        text(same).await,
        "",
        "a HEAD of the page sent a body, which the request asked for none of"
    );

    let missing = get(&client, &served.url("/assets/nope.js"), None, None).await;
    assert_eq!(
        missing.status(),
        StatusCode::NOT_FOUND,
        "a missing asset is not missing"
    );
    assert_shielded(&missing, "the 404 of a missing asset");
    assert_eq!(
        header_of(&missing, header::CACHE_CONTROL).as_deref(),
        Some(NO_STORE),
        "a year spent caching a file that was not there would outlive the build that added it"
    );

    served.stop().await;
}

/// With logins off the name a request says it came to is the only credential there is, and the page is
/// served to whoever brings it, exactly as the API is: a rebinding page gets the same `403` from `/`
/// that it gets from `/api/v1/torrents`.
#[tokio::test]
async fn logins_off_still_check_the_host() {
    let served = Served::start("web-host", open).await;
    let client = http();
    let origin = served.host();

    // The names this server answers, on the page as on the API.
    for host in [origin.as_str(), "localhost", "LocalHost:8090"] {
        let page = get(&client, &served.url("/"), Some(host), None).await;
        assert_eq!(
            page.status(),
            StatusCode::OK,
            "{host} is a name this server answers, with logins off, on the page"
        );
    }

    // The name a rebound domain arrives with: refused, the same way on both halves of this server.
    //
    // The refusal comes from the guard that stands in front of every route, so it is an API answer in
    // the API's shape, and it carries none of the front end's headers: the guard has already decided
    // nobody gets an answer of this server, and the headers are what a browser attaches to an answer
    // it got. Asserting the type rather than only the parsed body is what puts that limit in writing.
    for path in ["/", "/api/v1/torrents"] {
        let refused = get(&client, &served.url(path), Some("evil.example"), None).await;
        assert_eq!(
            header_of(&refused, header::CONTENT_TYPE).as_deref(),
            Some("application/json"),
            "{path} refused the host with something that is not the API's JSON answer"
        );
        assert_eq!(
            header_of(&refused, header::CONTENT_SECURITY_POLICY),
            None,
            "{path} refused the host with a page's headers on an answer that is not a page"
        );
        let (status, code) = problem_of(refused).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{path} took a request that named a host this server was not given"
        );
        assert_eq!(
            code, "forbidden",
            "{path} refused the host with another answer"
        );
    }

    // And the origin a browser adds, which is refused for a place this server would not answer as a
    // Host even when the Host itself is fine.
    let (status, _) = problem_of(
        get(
            &client,
            &served.url("/"),
            Some(origin.as_str()),
            Some("http://evil.example"),
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a page from another origin got this server's own page anyway"
    );

    served.stop().await;
}
