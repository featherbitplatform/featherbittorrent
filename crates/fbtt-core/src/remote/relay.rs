//! The loopback relay a gateway target streams through.
//!
//! A media player cannot send an `Authorization` header, and a gateway standing in front
//! of a server checks one before the request reaches it, so a stream URL on the server
//! is not something a player can open behind such a gateway. This relay is the thing in
//! between: a server on `127.0.0.1` that a player *can* reach, which forwards the
//! request to the server's own stream address with the gateway's credential added.
//!
//! What it keeps, in one place, is the bound on what a hostile upstream may cost this
//! process: a token that opens exactly one server path, at most eight requests at once,
//! one `Range` header of at most 256 visible ASCII bytes, an upstream that must answer
//! its head within [`RELAY_HEAD_TIMEOUT`] or not at all, and a response that copies back
//! only the status and five headers. A body that never ends is the player's to close,
//! not the relay's to keep.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use axum::Router;
use axum::extract::{Path, Request, State as RouteState};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures_util::StreamExt;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;

use crate::client::StreamUrl;
use crate::remote::http::Api;
use crate::stream::tokens::StreamTokens;

/// How many requests the relay forwards at once: a player re-requests its URL on every
/// seek, and an upstream that answers none of them must not hold every one of them open
/// at the same time. One more than the eighth gets `503` with a second to wait.
const MAX_REQUESTS: usize = 8;
/// How many live relay tokens the relay holds: the same cap a local endpoint keeps.
const TOKEN_CAP: usize = 16;
/// The longest a forwarded `Range` header may be: a single range, at most. Longer than
/// this is not a range a player would send, and what a server answers with is whole
/// either way.
const MAX_RANGE_BYTES: usize = 256;

/// The loopback relay: the server on `127.0.0.1:0` that a player opens instead of the
/// server's own stream address, for a target that signs to a gateway.
///
/// It is started on the first `Stream` a gateway target is asked for and kept for the
/// run; [`shutdown`](Self::shutdown) is called on the backend's `Shutdown`, the same way
/// the local endpoint ends (see [`crate::stream::local::LocalEndpoint`]).
pub struct Relay {
    /// The listener's bound address, the port the OS picked.
    addr: SocketAddr,
    /// The tokens that open the relay's paths: one per server address, the same store
    /// the route checks, kept here to issue against.
    tokens: Arc<StreamTokens>,
    /// The one bit that ends the relay's server task with the backend.
    stop: Arc<crate::remote::stopping::Stopping>,
    /// The server task, joined on shutdown so the listener is closed before this ends.
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay")
            .field("addr", &self.addr)
            .field("tokens", &self.tokens)
            .finish_non_exhaustive()
    }
}

impl Relay {
    /// Starts the relay on `127.0.0.1:0`, forwarding through `api`. The error is the
    /// reason the bind failed: there is nothing else to say about a listener that could
    /// not be opened.
    pub async fn start(api: Arc<Api>) -> Result<Relay, String> {
        let tokens = Arc::new(StreamTokens::new(TOKEN_CAP));
        let slots = Arc::new(Semaphore::new(MAX_REQUESTS));
        let stop = api.stopping();
        let port = Arc::new(OnceLock::new());

        let state = State {
            tokens: Arc::clone(&tokens),
            api: Arc::clone(&api),
            slots,
            addr: Arc::clone(&port),
        };
        let app = Router::new()
            .route("/relay/{token}", any(relay_request))
            .with_state(state)
            .into_make_service_with_connect_info::<SocketAddr>();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|problem| problem.to_string())?;
        let addr = listener
            .local_addr()
            .expect("a bound listener has an address");
        port.set(addr)
            .expect("the address is set once, here, before a request can arrive");

        // The server ends with the backend: the same stop signal every call on this
        // connection races, so a relay whose owner is going is not one a request keeps
        // waiting on.
        let stop_wait = Arc::clone(&stop);
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { stop_wait.waited().await })
                .await
                .ok();
        });
        Ok(Relay {
            addr,
            tokens,
            stop,
            task,
        })
    }

    /// The URL a player opens to stream the server address `server_path_and_query`: a
    /// token issued for that address, on the relay's own path. The scope and the URL are
    /// one string, as on the server: the check is byte for byte, and re-spelling the
    /// address here would hand out an address its own token could not open.
    pub fn url_for(&self, server_path_and_query: &str) -> StreamUrl {
        let token = self.tokens.issue(server_path_and_query.to_string());
        StreamUrl::new(format!("http://{}/relay/{}", self.addr, token))
    }

    /// Ends the relay: the stop signal closes the listener and ends every stream it
    /// forwards, and this waits for the server task. Called on the backend's `Shutdown`,
    /// after the backend's own calls are done.
    pub async fn shutdown(self) {
        self.stop.stop();
        let _ = self.task.await;
        // A request still in flight holds its permit in its body stream; the permit is
        // released when the body ends or the player hangs up, and the task above is
        // gone by then, whatever that order was.
    }
}

/// The route's state: everything a request needs to decide whether it is one this relay
/// serves, and what it forwards to.
#[derive(Clone)]
struct State {
    tokens: Arc<StreamTokens>,
    api: Arc<Api>,
    slots: Arc<Semaphore>,
    /// The bound address, set once at start (see [`Relay::start`]): the `Host` check
    /// needs the port, and the port is only known after the bind. A request in the
    /// moment between the bind and the set is refused, which is closed.
    addr: Arc<OnceLock<SocketAddr>>,
}

/// `GET` and `HEAD` on `/relay/{token}`: the one route, answered in order - the method,
/// the `Host` the request came in on, the token it names, a free slot, and the upstream
/// call.
///
/// The order is the rule: a host this relay does not serve is refused before the token
/// is looked up, so a request for another machine's host learns nothing about the
/// tokens; a token the store does not know is refused before a slot is taken, so a
/// stranger's guesses do not count against the bound; and a slot that is not free ends
/// the request before it costs the upstream anything.
async fn relay_request(
    RouteState(state): RouteState<State>,
    Path(token): Path<String>,
    request: Request,
) -> Response {
    // Nothing but the stream route's methods: a player re-requests its URL with a `GET`
    // or a `HEAD`, and nothing else is a player.
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let headers = request.headers();
    // The host this relay serves, checked before anything else: a request for another
    // host is refused even on the right token, the same rule the local endpoint applies.
    let Some(&host) = state.addr.get() else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if !serves_host(headers, host) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // The token: which server path this request may open. `resolve` applies the whole
    // life of the token - the first-use window, the idle limit, the cap - and hands
    // back the scope, which is the server address to relay to.
    let Some(scope) = state.tokens.resolve(&token) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    // A free slot, or a second to wait. The permit is moved into the body stream, so it
    // is held for the whole response - the player may seek, and the upstream may hold
    // the body open - and released when the body ends or the player hangs up.
    let Some(permit) = state.slots.try_acquire_owned().ok() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "1")],
        )
            .into_response();
    };
    // The `Range` the player sent, or nothing: one value of at most [`MAX_RANGE_BYTES`]
    // visible ASCII bytes, and only that. The player's other headers go nowhere.
    let range = range_of(headers);
    let api = state.api.clone();
    let head = request.method() == Method::HEAD;
    forward(api, scope, range, head, permit).await
}

/// Whether `headers` name one of the two hosts this relay serves: its own
/// `127.0.0.1:<port>` and `localhost:<port>`. Anything else - another machine, a host
/// without the port, no host at all - is a request this relay does not answer.
fn serves_host(headers: &HeaderMap, addr: SocketAddr) -> bool {
    let Some(value) = headers.get(header::HOST) else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return false;
    };
    let port = addr.port().to_string();
    format!("127.0.0.1:{port}") == text || format!("localhost:{port}") == text
}

/// The one header a relay request may carry: a single `Range` value of at most
/// [`MAX_RANGE_BYTES`] bytes, every one of them visible ASCII or a space - the range a
/// player spells is `bytes=0-15`. Anything else - a second value, a byte outside the
/// printable, a length past the limit - comes out as no `Range` at all, and the
/// upstream serves the file whole.
fn range_of(headers: &HeaderMap) -> Option<HeaderValue> {
    let values = headers.get_all(header::RANGE);
    let mut values = values.iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let bytes = value.as_bytes();
    if bytes.len() > MAX_RANGE_BYTES {
        return None;
    }
    if !bytes.iter().all(|byte| matches!(byte, 0x20..=0x7e)) {
        return None;
    }
    Some(value.clone())
}

/// The upstream call: the server's own stream address with the credential the relay
/// holds, the head waited on within [`RELAY_HEAD_TIMEOUT`], and a body that streams
/// chunk by chunk until it ends or the player stops asking.
async fn forward(
    api: Arc<Api>,
    scope: String,
    range: Option<HeaderValue>,
    head: bool,
    permit: OwnedSemaphorePermit,
) -> Response {
    // The call itself. `relay_get` carries the credential and the gateway's header,
    // renews them once on a `401`, and wraps only the wait for the head in
    // [`RELAY_HEAD_TIMEOUT`]: a server that opens a connection and answers none of
    // them costs the relay that much, and not the whole request timeout.
    let response = match api.relay_get(&scope, range.as_ref(), head).await {
        Ok(response) => response,
        Err(problem) => {
            // A refusal of the credential is the one this relay cannot fix: it is the same
            // credential every other call on the connection carries, and the refresh loop
            // is the one that renews it. What the player gets is what the server said.
            let (status, line) = match &problem {
                crate::remote::bodies::ApiError::Refused(_) => {
                    (StatusCode::UNAUTHORIZED, problem.line())
                }
                crate::remote::bodies::ApiError::Answer(_) => {
                    (StatusCode::BAD_GATEWAY, problem.line())
                }
                crate::remote::bodies::ApiError::Transport(_) => {
                    (StatusCode::BAD_GATEWAY, problem.line())
                }
            };
            return (status, line).into_response();
        }
    };
    // The response copies only the status and five headers: enough for a player to
    // seek and to know what it is playing. A `Set-Cookie`, a forwarding header and the
    // rest of the upstream's headers are the upstream's business, not the relay's.
    let upstream = response.headers().clone();
    let status = response.status();
    // The body is the upstream's own, one chunk at a time. The permit goes with it: the
    // map holds it for every item the stream produces, and it is released when the body
    // ends or the player closes the connection - a slow body holds its slot for as long
    // as the player plays it, and no longer.
    let body = crate::remote::bodies::stream(response.bytes_stream()).map(move |chunk| {
        // The permit goes with the body: it is held for every item the stream yields
        // and released when the body ends or the player closes the connection.
        let _ = &permit;
        chunk
    });
    let mut reply = Response::new(axum::body::Body::from_stream(body));
    *reply.status_mut() = status;
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_LENGTH,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
        header::CONTENT_DISPOSITION,
    ] {
        if let Some(value) = upstream.get(&name) {
            reply.headers_mut().insert(name, value.clone());
        }
    }
    reply
}
