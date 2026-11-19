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
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Request, State as RouteState};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures_util::StreamExt;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio::sync::watch;

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
    /// The signal every relayed body stream watches: when it flips, every stream ends
    /// at once, which is what lets `shutdown` bound its wait instead of waiting for
    /// players to hang up.
    body_stop: watch::Sender<bool>,
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
        let (body_stop, _) = watch::channel(false);

        let state = State {
            tokens: Arc::clone(&tokens),
            api: Arc::clone(&api),
            slots,
            addr: Arc::clone(&port),
            body_stop: body_stop.clone(),
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
            body_stop,
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

    /// Ends the relay: every relayed body stream is told to end, the stop signal closes
    /// the listener, and this waits for the server task at most two seconds, then aborts
    /// it. Called on the backend's `Shutdown`, after the backend's own calls are done.
    ///
    /// A held-open body that the player never closes would otherwise keep the server
    /// task alive indefinitely: axum's graceful shutdown waits for open connections, and
    /// a relayed body is one. Flipping the body-stop signal ends every stream at once,
    /// which is what makes the two-second bound enough.
    pub async fn shutdown(mut self) {
        // Tell every relayed body to end. The body streams watch this signal; when it
        // flips they finish on the next poll, releasing their permits and closing the
        // upstream connections.
        let _ = self.body_stop.send(true);
        // Close the listener so no new connection is accepted.
        self.stop.stop();
        // Wait for the server task to finish, but not for more than two seconds: a body
        // that is already ending (the signal just flipped) should end in well under a
        // second, and anything slower is not going to end on its own.
        match tokio::time::timeout(Duration::from_secs(2), &mut self.task).await {
            Ok(_) => {}
            Err(_) => {
                self.task.abort();
            }
        }
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
    /// The signal every relayed body stream watches: when it flips, every stream ends
    /// at once (see [`Relay::shutdown`]).
    body_stop: watch::Sender<bool>,
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
    let body_stop = state.body_stop.subscribe();
    forward(api, scope, range, head, permit, body_stop).await
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
    body_stop: watch::Receiver<bool>,
) -> Response {
    // The call itself. `relay_get` carries the gateway's credential and header, renews it once
    // on a `401`, and wraps only the wait for the head in [`RELAY_HEAD_TIMEOUT`]: a server that
    // opens a connection and answers none of them costs the relay that much, and not the whole
    // request timeout.
    //
    // The body of every error the relay answers is a fixed sentence, never the error's own text:
    // a reqwest error holds the URL it failed on, and that URL holds the stream token. What goes
    // in the body is what the player can act on, not what the client saw.
    let response = match api.relay_get(&scope, range.as_ref(), head).await {
        Ok(response) => response,
        Err(problem) => {
            // A head timeout is `504`: the upstream opened the connection and did not answer its
            // head in time. Everything else that is not a credential refusal is a `502`.
            let (status, line) = if problem.is_head_timeout() {
                (
                    StatusCode::GATEWAY_TIMEOUT,
                    "the server did not answer the stream request".to_string(),
                )
            } else {
                (
                    StatusCode::BAD_GATEWAY,
                    "the server could not be reached".to_string(),
                )
            };
            return (status, line).into_response();
        }
    };
    // The response copies only the status and five headers: enough for a player to
    // seek and to know what it is playing. A `Set-Cookie`, a forwarding header and the
    // rest of the upstream's headers are the upstream's business, not the relay's.
    let upstream = response.headers().clone();
    let status = response.status();
    // The body is the upstream's own, one chunk at a time, ending when the body ends,
    // the player closes the connection, or the relay's stop signal flips (see
    // [`Relay::shutdown`]). The permit goes with it: it is held for every item the
    // stream yields and released when the body ends or the player closes the connection.
    let body = crate::remote::bodies::stream(response.bytes_stream()).map(move |chunk| {
        let _ = &permit;
        chunk
    });
    let body_stop_wait = async move {
        let mut rx = body_stop;
        // End the stream when the stop signal flips, or when the sender is dropped
        // (the relay is gone). `wait_for` returns `Ok(true)` when the value changes
        // to `true` and `Err` when the sender is dropped; both end the stream.
        while rx.wait_for(|stopped| *stopped).await.is_ok() {}
    };
    let body = body.take_until(body_stop_wait);
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

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::http::{HeaderMap, HeaderValue};

    use super::{range_of, serves_host};

    fn addr(port: u16) -> SocketAddr {
        format!("127.0.0.1:{port}").parse().unwrap()
    }

    #[test]
    fn serves_host_accepts_the_two_forms() {
        let a = addr(8443);
        let mut headers = HeaderMap::new();
        headers.insert(axum::http::header::HOST, "127.0.0.1:8443".parse().unwrap());
        assert!(serves_host(&headers, a));
        headers.insert(axum::http::header::HOST, "localhost:8443".parse().unwrap());
        assert!(serves_host(&headers, a));
    }

    #[test]
    fn serves_host_refuses_without_port() {
        let a = addr(8443);
        let mut headers = HeaderMap::new();
        headers.insert(axum::http::header::HOST, "127.0.0.1".parse().unwrap());
        assert!(!serves_host(&headers, a));
        headers.insert(axum::http::header::HOST, "localhost".parse().unwrap());
        assert!(!serves_host(&headers, a));
    }

    #[test]
    fn serves_host_refuses_wrong_port() {
        let a = addr(8443);
        let mut headers = HeaderMap::new();
        headers.insert(axum::http::header::HOST, "127.0.0.1:9999".parse().unwrap());
        assert!(!serves_host(&headers, a));
        headers.insert(axum::http::header::HOST, "localhost:9999".parse().unwrap());
        assert!(!serves_host(&headers, a));
    }

    #[test]
    fn serves_host_refuses_another_host() {
        let a = addr(8443);
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::HOST,
            "192.168.1.1:8443".parse().unwrap(),
        );
        assert!(!serves_host(&headers, a));
        headers.insert(axum::http::header::HOST, "evil.com:8443".parse().unwrap());
        assert!(!serves_host(&headers, a));
    }

    #[test]
    fn serves_host_refuses_no_host() {
        let a = addr(8443);
        let headers = HeaderMap::new();
        assert!(!serves_host(&headers, a));
    }

    #[test]
    fn range_of_accepts_a_single_visible_ascii_value() {
        let mut headers = HeaderMap::new();
        headers.append(axum::http::header::RANGE, "bytes=0-15".parse().unwrap());
        assert_eq!(
            range_of(&headers),
            Some("bytes=0-15".parse::<HeaderValue>().unwrap())
        );
    }

    #[test]
    fn range_of_accepts_a_256_byte_value() {
        let value = format!("bytes=0-{}", "x".repeat(248));
        assert_eq!(value.len(), 256);
        let mut headers = HeaderMap::new();
        headers.append(axum::http::header::RANGE, value.parse().unwrap());
        assert_eq!(
            range_of(&headers).map(|v| v.to_str().unwrap().to_string()),
            Some(value)
        );
    }

    #[test]
    fn range_of_drops_two_values() {
        let mut headers = HeaderMap::new();
        headers.append(axum::http::header::RANGE, "bytes=0-15".parse().unwrap());
        headers.append(axum::http::header::RANGE, "bytes=16-31".parse().unwrap());
        assert_eq!(range_of(&headers), None);
    }

    #[test]
    fn range_of_drops_257_bytes() {
        let value = format!("bytes=0-{}", "x".repeat(249));
        assert_eq!(value.len(), 257);
        let mut headers = HeaderMap::new();
        headers.append(axum::http::header::RANGE, value.parse().unwrap());
        assert_eq!(range_of(&headers), None);
    }

    #[test]
    fn range_of_drops_a_byte_above_the_printable_range() {
        // The HTTP spec allows bytes 0x20-0x7e and 0x80-0xff in header values, but
        // `range_of` only passes 0x20-0x7e (visible ASCII). A byte above 0x7e is a
        // visible-ASCII violation even when the header parser accepts it.
        // 0x7f (DEL) is rejected by the header parser, so we use 0x80, which is
        // accepted by the parser but outside `range_of`'s allowed range.
        let mut headers = HeaderMap::new();
        headers.append(
            axum::http::header::RANGE,
            HeaderValue::from_bytes(b"bytes=0-\x80").expect("a valid header value"),
        );
        assert_eq!(range_of(&headers), None);
    }

    #[test]
    fn range_of_drops_no_range_header() {
        let headers = HeaderMap::new();
        assert_eq!(range_of(&headers), None);
    }
}
