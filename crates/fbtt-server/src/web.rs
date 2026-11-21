//! The browser UI, out of the binary and onto the address the API is already on.
//!
//! Three routes, and all of them are files: the page, the icon the page names, and whatever Vite put
//! in `assets/`. They are merged into the same router as the API routes (see [`crate::Server`]) so
//! the page and `/api/v1/torrents` are one origin, which is what makes the sign-in call a same-origin
//! one and the cookie's `SameSite=Strict` rule enough.
//!
//! There is no fallback route here, on purpose. The front end routes with the URL fragment, so every
//! address that is not a file this build really carries is a missing file, and it gets the `404` in
//! the same `{error: {code, message}}` shape the API uses for a missing torrent. A catch-all returning
//! the page instead would make a typo'd address look like a broken app.
//!
//! The page is open to somebody who has not signed in, which is the one thing about this module that
//! is a security decision rather than plumbing: the sign-in form is *on* the page, so a page that
//! asked for a session before it showed a sign-in form could never be signed in to. The exemption is
//! `crate::auth`'s, and it is exactly these three templates on `GET` and `HEAD`; everything a session
//! does still goes through the API routes and their credential.
//!
//! What is served is bytes from `web/dist`, embedded at compile time by `rust-embed`. A debug build
//! reads that folder from disk on each request instead, which is what makes editing the front end
//! without recompiling work, and is also why a debug run whose `web/dist` went away answers `500`
//! where the shipped binary could not: the release binary has the bytes in it and cannot lose them.
//! A headless build has none of this module at all, so `web/dist` is not in its build and not in its
//! binary.

use axum::Router;
use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Request};
use axum::http::{HeaderValue, header};
use axum::middleware::{Next, from_fn};
use axum::response::Response;
use axum::routing::get;
use rust_embed::Embed;
use rust_embed::EmbeddedFile;

use crate::routes::{RouteError, clean};

/// What this server will let a browser run and load on its own page, and nothing else.
///
/// Spelled as one string, asserted as one string, because the directives are worth having together:
/// `'self'` everywhere is what makes an injected `<script>` load only from this origin, and the three
/// `none`s close the doors a `'self'` leaves open (`object-src` for an `<embed>` that would run
/// somebody else's plugin, `frame-ancestors` for somebody else framing this page, `base-uri` for a
/// `<base>` that would silently reparent every relative URL on it, which is how a `<base
/// href="http://evil.example/">` in injected markup turns a `'self'` script-src into a lie).
///
/// `style-src-attr 'unsafe-inline'` is the one permission that is not `'self'`, and it is there for
/// inline `style=` attributes and nothing else: a progress bar's width and a virtualised row's offset
/// are numbers computed per render, and they belong on the element they describe. Stylesheets
/// themselves come from `'self'` only, and a `<style>` element would need `style-src-elem`, which is
/// deliberately not relaxed here.
const CSP: &str = concat!(
    "default-src 'self'; script-src 'self'; style-src 'self'; style-src-attr 'unsafe-inline'; ",
    "img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; font-src 'self'; ",
    "object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
);

/// What the page gets, and anything else whose name does not change when its contents do.
///
/// The page is the one file that has to be re-read every time: it is short, it is what a person lands
/// on, and a cached copy of it keeps an old front end talking to a new server, which is the pairing
/// that produces a page that appears to work and shows nothing.
const NO_STORE: &str = "no-store";

/// What a built asset gets: nobody is ever going to ask for `index-YqC5im7E.js` again once the front
/// end changes, because the next build names a different file. A year is what a cache can hold this
/// for; `immutable` is what tells it not to even come and ask.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// The built front end, as it was when this binary was compiled.
///
/// The path is relative to this crate's manifest, which is where `rust-embed` resolves it, and it is
/// `web/dist` two levels up rather than anything inside this crate: the front end is built by npm and
/// lives at the workspace root, and the embed is the only thing that reaches across from a crate.
#[derive(Embed)]
#[folder = "../../web/dist/"]
struct Assets;

/// The front end's own routes, ready to merge into the router that goes to fbt-api.
///
/// Merge order does not matter here - these paths are not ones fbt-api or the API routes serve, and axum
/// panics loudly if they ever become one - but the `Host` and `Origin` guard and the session check do:
/// fbt-api puts its auth layer over everything it is merged with, so these routes are asked about like
/// any other, and `crate::auth` is where their exemption is stated.
pub(crate) fn router() -> Router {
    Router::new()
        .route("/", get(page))
        .route("/favicon.png", get(favicon))
        .route("/assets/{*path}", get(asset))
        // Added last, so it is on the way out of every route above: in axum a layer goes onto the
        // routes this router already has, and `Router::merge` leaves each route's own stack alone. That
        // is what keeps these headers on the front end and off the API's responses, where the TUI and
        // every other client is reading them for the first time.
        .layer(from_fn(shield))
}

/// `GET /`: the page, which is where a person who has not signed in starts.
async fn page() -> Result<Response, RouteError> {
    Assets::get("index.html")
        .map(|found| serve("index.html", found, false))
        // A missing page is not a wrong request: this build was told to carry a front end and has no
        // page in it. `web/dist` was empty when this binary was compiled, which is worth saying in as
        // many words as a `500` message holds.
        .ok_or_else(|| {
            RouteError::internal(
                "this build carries no page: web/dist was empty when it was compiled",
            )
        })
}

/// The icon the page names, which is a file like any other and gets the same headers.
///
/// It is a route of its own because that is where the build puts it: a file in `web/public` is copied
/// to the top of `web/dist` under the name it was given, not hashed into `assets/`. So its name never
/// changes, which is also why it does not get a year of caching.
async fn favicon() -> Result<Response, RouteError> {
    file("favicon.png")
}

/// One built file: `/assets/index-YqC5im7E.js`, `/assets/geist-latin-wght-normal-BgDaEnEv.woff2`.
///
/// The name arrives as a `Result` rather than extracted outright because of the one case extraction
/// fails on: a path whose percent-encoding does not decode, `/assets/%FF` say. Axum's own answer to
/// that is a `400` with a plain-text message, and this is the half of the server a browser's `fetch`
/// talks to, where every other error is `{error: {code, message}}` - so the rejection is put in that
/// shape by the same `From` the API routes use, and one parse handles every error this router answers.
async fn asset(given: Result<Path<String>, PathRejection>) -> Result<Response, RouteError> {
    let Path(name) = given?;
    file(&format!("assets/{name}"))
}

/// One file from the build, or the API's `404` for a name it does not carry.
fn file(name: &str) -> Result<Response, RouteError> {
    Assets::get(name)
        .map(|found| serve(name, found, name.starts_with("assets/")))
        .ok_or_else(|| {
            // The name came from the request, so it is remote text and gets cleaned on its way back
            // out: this is a path a client typed, and a path can carry anything at all.
            let name = clean(name);
            RouteError::not_found(format!("{name} is not a file this build carries"))
        })
}

/// A file this build carries, typed, and cached for as long as its name can be trusted.
///
/// `immutable` goes only on the files whose name is a hash of their own contents, which is every file
/// under `assets/` and no others: the page and the icon keep the same names release after release, so
/// a cache that held one for a year would hold the wrong one. Everything that is left without a
/// `Cache-Control` here gets [`NO_STORE`] from [`shield`].
fn serve(name: &str, found: EmbeddedFile, immutable: bool) -> Response {
    let mut response = Response::new(Body::from(found.data));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, content_type(name));
    if immutable {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE));
    }
    response
}

/// What type a built file is, from the extension its name still carries.
///
/// The extension is all that is left to ask: Vite renames everything under `assets/` after what is in
/// it, so a name says nothing about its kind and a saved guess about "this build has a `.js` in it"
/// would be a second answer to keep in step. `mime_guess` reads the same registration list the
/// browsers do, so what it says and what a browser expects from the same name are the same answer.
///
/// Two of the types are read as a document rather than as bytes, and those have to name the encoding
/// they are in: `text/html` and `text/css` without a charset make a browser guess one, and a guess is
/// how a stylesheet with a non-ASCII torrent name in it goes wrong. A script is not guessed at - it is
/// read as UTF-8 whatever the header says, and `charset=` on it is the spelling that asks for an extra
/// fetch - so the charset goes with the two that need it and nowhere else.
fn content_type(name: &str) -> HeaderValue {
    let guessed = mime_guess::from_path(name).first_or_octet_stream();
    let value = if matches!(guessed.essence_str(), "text/html" | "text/css") {
        format!("{guessed}; charset=utf-8")
    } else {
        guessed.to_string()
    };
    // A MIME type is ASCII by construction, so this cannot fail; octet-stream is what it would fall
    // back to if the guess ever produced something a header could not hold.
    HeaderValue::from_str(&value)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

/// The headers this server puts on every answer of the front end, and on nothing else.
///
/// Four of them say what a browser may do with what it gets here ([`CSP`], and the three that are not
/// content policy but cover the gaps in it: `X-Content-Type-Options: nosniff` so a file typed one way
/// is not re-read as another, `X-Frame-Options: DENY` so no other page can put this one inside a frame
/// and click on its behalf, `Referrer-Policy: no-referrer` so the address a person is looking at -
/// which for a torrent detail page *is* the torrent - is not sent off to somewhere else). All four are
/// set with `insert`, not `append`: this router makes its own responses, so a value already here came
/// from this server and replacing it is correcting it rather than arguing with it.
///
/// `Cache-Control` is the one header left alone if it is already there, which is how a hashed asset
/// keeps [`IMMUTABLE`] while everything else, including the `404` of an asset that is not in the build,
/// falls back to [`NO_STORE`]. A missing file is not cacheable for a year: the build that adds it is
/// the answer the same name should get.
///
/// It runs on the way out rather than the way in, so what it looks at is the response a route actually
/// made, and it is on this router only: the API's answers are the ones the TUI, the players and the
/// gateway have been parsing all along, and they do not change because a browser exists.
async fn shield(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static(NO_STORE));
    response
}
