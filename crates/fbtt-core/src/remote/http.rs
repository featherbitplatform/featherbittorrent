//! The HTTP side of a remote library: one client, bounded bodies, and a credential that renews itself.
//!
//! Everything the backend asks of a server goes through [`Api`], so the three rules that keep a
//! hostile or half-broken server from hurting this process are written once, here:
//!
//! - **No response is trusted to be short.** A body is read chunk by chunk and refused past
//!   [`MAX_BODY`]; a server that answers `GET /api/v1/torrents` with 100 MiB costs this client 16 MiB
//!   and a notice, not its memory.
//! - **A credential is renewed before it is given up on.** A call that comes back refused logs in (or
//!   fetches a new gateway token) once and tries again; only the second refusal is an answer. That is
//!   what makes an hour-old session token and a server that restarted mid-download both ordinary.
//! - **Nothing waits forever.** The client has a connect timeout and a per-request timeout; the event
//!   stream is the one call without the latter, because it is *meant* to stay open, and it is bounded
//!   by its own line cap instead ([`crate::remote::sse`]).

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

// Named rather than imported anonymously: the stream reader below bounds a body with `poll_next`, and a
// generic bound has to name the trait it is bounding.
use futures_core::Stream;
use reqwest::{Client, ClientBuilder, RequestBuilder, Response, StatusCode, header};
use serde::de::DeserializeOwned;

use crate::engine::map::clean;
use crate::remote::login::Auth;
use crate::ui_state::RemoteTarget;

/// The largest response body this client reads. A torrent list, a file list and a peer list are all far
/// below it; what reaches it is a server answering a question about a million-file torrent, or one that
/// is not answering in JSON at all.
pub(crate) const MAX_BODY: usize = 16 * 1024 * 1024;
/// How long a connection may take to open. Longer than a dial across a wide area network needs, and
/// short enough that a server whose address routes into a black hole says so in one tick rather than
/// after two minutes of a frozen download list.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long any one call may take, the event stream excepted. A `.torrent` upload to a busy server is
/// the slowest thing here that is working properly.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A client with this backend's rules, short of the request timeout. A connection needs the same client
/// twice - once with the ceiling and once without - and `ClientBuilder` is not clonable, so the settings
/// are written once here and named at both places instead of copied.
fn client_builder() -> ClientBuilder {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        // A redirect would leave this API's paths behind for somewhere else, and a server that answers
        // with one is telling us something we should see rather than follow.
        .redirect(reqwest::redirect::Policy::none())
}

/// What can go wrong with one call to a server, sorted by what this client should do about it.
#[derive(Debug)]
pub(crate) enum ApiError {
    /// The server could not be reached, or dropped the connection partway. The link is gone: the last
    /// snapshot stays on the screen and the backend dials again with a growing pause.
    Transport(String),
    /// The credential was refused and signing in again did not change that. Without a credential there
    /// is no library to list, so this counts as a lost link too. What it carries is the reason - the
    /// server's own line when it gave one, or what this client found wrong with the settings.
    Refused(String),
    /// An ordinary answer to a request this client should not have made: no such torrent, a folder
    /// outside the server's roots, a body the server could not parse. The link is fine; one line tells
    /// the person what the server said.
    Answer(String),
}

impl ApiError {
    /// The one line this error becomes on the screen, fit to draw.
    pub(crate) fn notice(&self) -> String {
        match self {
            ApiError::Transport(problem) => format!("the server could not be reached: {problem}"),
            ApiError::Refused(reason) => format!("sign-in was refused: {reason}"),
            ApiError::Answer(message) => message.clone(),
        }
    }

    /// Whether this error means the link to the server is gone, rather than this one request having
    /// asked for something the server does not have.
    pub(crate) fn breaks_the_link(&self) -> bool {
        matches!(self, ApiError::Transport(_) | ApiError::Refused(_))
    }
}

/// The calls one backend makes: its client, the server's address, and the credential it carries.
///
/// Cloning this is cheap - the client is a handle on one connection pool and the credential is behind
/// one lock - which is what lets the event stream hold its own copy: a stream that stays open for hours
/// cannot be waiting on the same task that answers keystrokes, and a 401 on either half renews the same
/// session for both.
#[derive(Debug)]
pub(crate) struct Api {
    /// The client every call but the event stream goes through: it carries the request timeout.
    client: Client,
    /// ...and the one for the event stream, which has no request timeout because its body never ends.
    events_client: Client,
    /// The server's address with its scheme and no trailing slash: `http://127.0.0.1:8090`.
    base: String,
    /// Who we sign in as, and the token that proves it. Shared with the event stream so a re-login on
    /// one half is a re-login on the other.
    auth: Arc<tokio::sync::Mutex<Auth>>,
}

impl Api {
    /// The API for `target`, signed in with `secret`.
    ///
    /// Errs only about what cannot be asked of a server at all: an address that is not one, a method
    /// with no credential to go with it, or a client that this build could not put together.
    pub(crate) fn try_new(
        target: &RemoteTarget,
        secret: crate::remote::Secret,
    ) -> Result<Api, String> {
        let url = reqwest::Url::parse(&target.url)
            .map_err(|_| format!("{} is not a server address", clean(&target.url)))?;
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(format!(
                "{} names no http or https server",
                clean(&target.url)
            ));
        }
        let base = target.url.trim_end_matches('/').to_string();
        let auth = Auth::new(target, secret)?;
        let client = client_builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|problem| clean(&problem.to_string()))?;
        // The event stream needs the same client with the ceiling taken back off: its body never ends, so
        // a per-request timeout would end it on schedule however healthy the server is.
        let events_client = client_builder()
            .build()
            .map_err(|problem| clean(&problem.to_string()))?;
        Ok(Api {
            client,
            events_client,
            base,
            auth: Arc::new(tokio::sync::Mutex::new(auth)),
        })
    }

    /// A `GET` of `path`. The paths are this API's, written out at the call sites rather than built
    /// from parts, so a reader of either crate can match a call to the route it reaches.
    pub(crate) fn get(&self, path: &str) -> RequestBuilder {
        self.client.get(self.url(path))
    }

    /// A `POST` of nothing at all: the control routes take their whole answer in the status.
    pub(crate) fn post(&self, path: &str) -> RequestBuilder {
        self.client.post(self.url(path))
    }

    /// A `PUT` whose body is JSON.
    pub(crate) fn put_json<T: serde::Serialize>(&self, path: &str, body: &T) -> RequestBuilder {
        self.client.put(self.url(path)).json(body)
    }

    /// A `POST` whose body is JSON: adding a device is the one call here that sends a body and needs
    /// nothing from the answer but its status.
    pub(crate) fn post_json<T: serde::Serialize>(&self, path: &str, body: &T) -> RequestBuilder {
        self.client.post(self.url(path)).json(body)
    }

    /// A `DELETE`.
    pub(crate) fn delete(&self, path: &str) -> RequestBuilder {
        self.client.delete(self.url(path))
    }

    /// The full address of one of this API's paths.
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// `GET /api/v1/events`, answered as a body to read for as long as it lasts.
    ///
    /// No `categories` parameter: every category wakes the same refresh, and asking for a subset is one
    /// more thing to keep in step with the server's list of names. This is the one call on
    /// [`Self::events_client`], the client without a request timeout, because its body never ends.
    pub(crate) async fn events(&self) -> Result<Response, ApiError> {
        self.call(self.events_client.get(self.url("/api/v1/events")))
            .await
    }

    /// The JSON a call answers, read within [`MAX_BODY`].
    pub(crate) async fn json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> Result<T, ApiError> {
        let response = self.call(request).await?;
        from_json(read_capped(response).await?)
    }

    /// A call whose answer is only its status: `204` for a pause, `201` for an added torrent. Any body
    /// it does send - an error this client will not use - is read and dropped within the cap.
    pub(crate) async fn empty(&self, request: RequestBuilder) -> Result<(), ApiError> {
        let response = self.call(request).await?;
        read_capped(response).await.map(|_| ())
    }

    /// Sends `request` with our credential, renewing that credential once if it comes back refused.
    ///
    /// A `RequestBuilder` carries the client that made it, so the event stream reaches its own
    /// timeout-free client through the builder rather than through anything here. The retry sends a clone
    /// taken before the first attempt went out, so a `.torrent` upload is signed twice from the same bytes
    /// rather than read twice from wherever they came from; a body that cannot be repeated gets one attempt
    /// rather than a wrong second one.
    async fn call(&self, request: RequestBuilder) -> Result<Response, ApiError> {
        let mut again = request.try_clone();
        let mut first = Some(request);
        for attempt in 0..2 {
            let credential = self.authorise().await?;
            let request = match attempt {
                0 => first.take(),
                _ => again.take(),
            };
            let Some(request) = request else {
                return Err(ApiError::Transport(
                    "this request cannot be sent a second time".to_string(),
                ));
            };
            let response = request
                .headers(credential.header())
                .send()
                .await
                .map_err(|problem| ApiError::Transport(clean(&problem.to_string())))?;
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            // The server's own reason comes first, because it is the answer to the question we asked;
            // only a refusal with no business answer behind it is about the credential.
            let answer = read_problem(response).await?;
            if !self.refused(status, answer.as_deref()).await {
                return Err(ApiError::Answer(
                    answer.unwrap_or_else(|| status_line(status)),
                ));
            }
            if attempt == 1 {
                // Refused twice, with a new credential in between: this connection's proof is not good.
                let gateway = self.auth.lock().await.signs_for_a_gateway();
                return Err(ApiError::Refused(match answer {
                    Some(message) if !gateway => message,
                    _ if gateway => {
                        "a gateway stands in front of this server and will not let us through"
                            .to_string()
                    }
                    _ => "the credential this connection presents is not accepted".to_string(),
                }));
            }
            // Drop the held credential so the next attempt earns a new one.
            self.invalidate().await;
        }
        unreachable!("the second attempt always returns")
    }

    /// The credential to send, signing in or fetching a fresh token when the one we hold is missing,
    /// stale, or has just been refused. A login that cannot be completed is a refusal, not a transport
    /// failure: what went wrong is about the credential, and only the reaching part looks like a link.
    async fn authorise(&self) -> Result<Credential, ApiError> {
        let mut auth = self.auth.lock().await;
        auth.credential().await.map_err(ApiError::Refused)
    }

    /// Whether a refused call is about the credential rather than about what was asked for.
    ///
    /// `401` is always about that: this API answers it nowhere else. A `403` depends on who answered -
    /// a featherbittorrent server uses it for "that folder is not one of mine" and says so in its error
    /// body, which is what `answer` carries; a gateway that does not understand our error bodies answers
    /// with nothing behind the status, and there `403` means the same thing to us that `401` does.
    async fn refused(&self, status: StatusCode, answer: Option<&str>) -> bool {
        if status == StatusCode::UNAUTHORIZED {
            return true;
        }
        if status != StatusCode::FORBIDDEN {
            return false;
        }
        let gateway = self.auth.lock().await.signs_for_a_gateway();
        gateway && answer.is_none()
    }

    /// Says the credential this client holds is no good, so that the next call renews it.
    async fn invalidate(&self) {
        self.auth.lock().await.invalidate();
    }

    /// Ends this client's own session with the server, so the device stops showing up as connected. A
    /// gateway token is not ours to revoke and a server with authentication off has nothing to end, which
    /// is what [`crate::remote::login`] says and does nothing about.
    pub(crate) async fn log_out(&self) {
        self.auth.lock().await.log_out().await;
    }
}

/// What one request carries as its proof of who is asking.
#[derive(Clone)]
pub(crate) enum Credential {
    /// Nothing: this server answers without a login, or a gateway has already let us through.
    Open,
    /// A session token from the server, or a bearer token from a gateway.
    Bearer(String),
    /// A user name and password, given to a gateway rather than to the server.
    Basic(String, String),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token and the password are the secret; which kind it is says everything worth printing.
        f.write_str(match self {
            Credential::Open => "Credential::Open",
            Credential::Bearer(_) => "Credential::Bearer(<token>)",
            Credential::Basic(_, _) => "Credential::Basic(<user>, <password>)",
        })
    }
}

impl Credential {
    /// The `Authorization` header this credential is sent as.
    fn header(&self) -> header::HeaderMap {
        let mut headers = header::HeaderMap::new();
        match self {
            Credential::Open => {}
            Credential::Bearer(token) => {
                if let Ok(value) = header::HeaderValue::from_str(&format!("Bearer {token}")) {
                    headers.insert(header::AUTHORIZATION, value);
                }
            }
            // Built by hand rather than left to reqwest, because a login sends the same header before any
            // request builder exists (see [`crate::remote::login`]).
            Credential::Basic(user, password) => return basic_header(user, password),
        }
        headers
    }
}

/// An `Authorization: Basic` header for `user:password`: standard base64 of the two joined with a colon,
/// which is what this server's login route and any gateway in front of it read. A password that is not
/// header-safe (a byte above 0x7f) gets no header at all rather than a wrong one, and what comes back - an
/// ordinary unauthenticated answer - is then the honest one.
pub(crate) fn basic_header(user: &str, password: &str) -> header::HeaderMap {
    let encoded = {
        use base64::Engine as _;
        base64::prelude::BASE64_STANDARD.encode(format!("{user}:{password}"))
    };
    let mut headers = header::HeaderMap::new();
    if let Ok(value) = header::HeaderValue::from_str(&format!("Basic {encoded}")) {
        headers.insert(header::AUTHORIZATION, value);
    }
    headers
}

/// Reads a response body, refusing one larger than [`MAX_BODY`]. Public to this module's neighbours
/// because a login answer needs the same bound as a torrent list: a hostile server answers everything.
pub(crate) async fn read_capped(response: Response) -> Result<Vec<u8>, ApiError> {
    let mut stream = Box::pin(response.bytes_stream());
    let mut body = Vec::new();
    loop {
        match next(&mut stream).await {
            Some(Ok(chunk)) => {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(ApiError::Answer(format!(
                        "the server sent more than {} MiB of response",
                        MAX_BODY / (1024 * 1024)
                    )));
                }
                body.extend_from_slice(chunk.as_ref());
            }
            Some(Err(problem)) => {
                return Err(ApiError::Transport(clean(&problem.to_string())));
            }
            None => return Ok(body),
        }
    }
}

/// The next item of a boxed stream.
///
/// `bytes_stream` answers with an `impl Stream` this crate cannot name, so both readers box it and come
/// through here; the item type stays generic so neither has to name `reqwest`'s chunk type either.
pub(crate) async fn next<S>(stream: &mut Pin<Box<S>>) -> Option<S::Item>
where
    S: Stream,
{
    std::future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

/// Reads a failed response's body within the cap and pulls out the reason this API puts in every error:
/// `{"error": {"code": ..., "message": ...}}`, ours and fbt-api's alike. `None` for a body that says
/// nothing in that shape, which is what a gateway's error page and an empty answer both look like.
async fn read_problem(response: Response) -> Result<Option<String>, ApiError> {
    let body = read_capped(response).await?;
    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return Ok(None);
    };
    let message = parsed
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(clean);
    // A body that is JSON but not this shape (a gateway's own error, an empty object) says nothing this
    // client should repeat: the status is what it has.
    Ok(message.filter(|message| !message.is_empty()))
}

/// The line for a failed response with no reason in it: the status, spelled as a person would say it.
fn status_line(status: StatusCode) -> String {
    let reason = status.canonical_reason().unwrap_or("an error");
    format!("the server answered {status} {reason}")
}

/// Parses a body this client has already bounded. A server that answers JSON-shaped nonsense, or not
/// JSON at all, is a broken server rather than a fatal one: the last good snapshot stays on the screen.
pub(crate) fn from_json<T: DeserializeOwned>(body: Vec<u8>) -> Result<T, ApiError> {
    serde_json::from_slice(&body).map_err(|problem| {
        ApiError::Answer(format!(
            "the server sent a response this client could not read: {}",
            clean(&problem.to_string())
        ))
    })
}
