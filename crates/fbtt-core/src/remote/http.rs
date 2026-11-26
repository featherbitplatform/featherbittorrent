//! The HTTP side of a remote library: one client, bounded bodies, and a credential that renews itself.
//!
//! Everything the backend asks of a server goes through [`Api`], so the rules that keep a hostile or
//! half-broken server from hurting this process are written once, here:
//!
//! - **No response is trusted to be short.** A body is read chunk by chunk and refused past
//!   [`MAX_BODY`](bodies::MAX_BODY); a server that answers `GET /api/v1/torrents` with 100 MiB costs this
//!   client 16 MiB and a notice, not its memory.
//! - **A credential is renewed before it is given up on.** A call that comes back refused logs in (or
//!   fetches a new gateway token) once and tries again; only the second refusal is an answer. That is what
//!   makes an hour-old session token and a server that restarted mid-download both ordinary. Only this
//!   module's `Api` logs in: the event stream sends whatever credential is already held
//!   ([`Api::held`]), which is why one wrong password is not two lots of hammering.
//! - **Nothing waits forever, and nothing waits out a stop.** The client has a connect timeout and a
//!   per-request timeout; the event stream is the one call without the latter, because it is *meant* to stay
//!   open, and it is bounded by its own line cap instead ([`crate::remote::sse`]). Every wait here also
//!   ends when [`Stopping`] says this client is on its way out.
//! - **One place decides what goes on the wire.** [`Api::headers`] puts the credential on every call, and
//!   beside it the gateway's key header when the target named one; the login calls that cannot come through
//!   `Api` add the same header from [`gateway_headers`]. So "no request reaches this server without the
//!   header" is a property of this module rather than something each call site has to remember. The other
//!   half of the same rule says where that header may *not* go: only to the server's own origin. Every
//!   address here is [`Api::url`] joining one of this API's paths onto the address the target named, and
//!   redirects are refused rather than followed ([`client_builder`]), so nothing this client dials has left
//!   that origin. The one call that goes somewhere else on purpose - a gateway's token endpoint, in
//!   [`crate::remote::gateway`] - is the one call that carries no gateway key: a provider that has never
//!   seen this client's key has nothing to check it against, and a key spent on somebody else's host is a
//!   key handed to whatever runs it.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client, ClientBuilder, RequestBuilder, Response, StatusCode, header};
use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

use crate::engine::map::clean;
use crate::remote::bodies::{ApiError, from_json, next, push, status_line, too_big};
use crate::remote::login::Auth;
use crate::remote::stopping::{STOPPED, Stopping};
use crate::ui_state::RemoteTarget;

/// How long a connection may take to open. Longer than a dial across a wide area network needs, and
/// short enough that a server whose address routes into a black hole says so in one tick rather than
/// after two minutes of a frozen download list.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long any one call may take, the event stream excepted. A `.torrent` upload to a busy server is
/// the slowest thing here that is working properly.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the relay waits for an upstream's response head before it gives the player a `504`: the relay's
/// own, kept beside this one because the relay needs the same clock for the one thing it wraps. A test
/// build shortens it to one second, so the integration tests that prove the `504` do not wait out a
/// thirty-second clock; the rule is the same, only the clock is faster.
#[cfg(any(test, feature = "test-support"))]
pub(crate) const RELAY_HEAD_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(not(any(test, feature = "test-support")))]
pub(crate) const RELAY_HEAD_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// The header a gateway standing in front of this server checks before the request reaches it, when the
    /// target named one. Sent beside the credential on every call here, and shared with [`Auth`] so the
    /// calls that earn the credential carry it too (see [`GatewayKey`]).
    gateway: Option<Arc<GatewayKey>>,
    /// Set when this client is stopping: every wait below ends early because of it.
    stopping: Arc<Stopping>,
}

impl Api {
    /// The API for `target`, signed in with `secret` and let through the front door by `gateway_key`.
    ///
    /// Errs only about what cannot be asked of a server at all: an address that is not one, a method
    /// with no credential to go with it, a gateway header that is not a header or a key that is not there
    /// to send under it, or a client that this build could not put together.
    pub(crate) fn try_new(
        target: &RemoteTarget,
        secret: crate::remote::Secret,
        gateway_key: Option<Zeroizing<String>>,
        stopping: Arc<Stopping>,
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
        let gateway = gateway_header(target, gateway_key)?;
        let auth = Auth::new(target, secret, gateway.clone())?;
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
            gateway,
            stopping,
        })
    }

    /// The handle that says this client is stopping, for the one task that gets to set it (see
    /// [`crate::remote::relay`]).
    pub(crate) fn stopping(&self) -> Arc<Stopping> {
        Arc::clone(&self.stopping)
    }

    /// Whether somebody has asked this client to stop. A round of reads that gave up because of that has
    /// nothing new to show and its failure is not trouble worth reporting, which is how the loop behind
    /// this goes straight to ending the session.
    pub(crate) fn asked_to_stop(&self) -> bool {
        self.stopping.stopping()
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

    /// The address this API talks to, with its scheme and no trailing slash.
    pub(crate) fn base(&self) -> &str {
        &self.base
    }

    /// Whether this target signs to a gateway: the methods that need a credential the
    /// server itself does not know about, and the header that goes with them. A stream
    /// for such a target goes through the relay, because a player cannot send that
    /// credential itself (see [`crate::remote::relay`]).
    ///
    /// The gateway's key header is the one that says so: a target that names it is
    /// behind a gateway, and a player cannot send it. A basic login with no gateway
    /// header is a server that asks for a user name and password on every request,
    /// and its stream URL is one a player can open.
    pub(crate) async fn has_gateway_credential(&self) -> bool {
        if self.gateway.is_some() {
            return true;
        }
        // A basic login with no gateway header: the credential is sent on every
        // request, and a player cannot send it. The relay is the one that does.
        self.auth.lock().await.signs_for_a_gateway()
    }

    /// The full address of one of this API's paths.
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The headers a request from this client carries: the credential's, with the gateway's header beside it
    /// when the target named one.
    ///
    /// One map rather than two calls because a `RequestBuilder` takes its headers as one. Both belong on the
    /// same request: the gateway decides who is asking before it passes the request on, and the server behind
    /// it reads our own token out of the very request the gateway let through.
    ///
    /// Nothing here asks where the request is going, because nothing this client builds goes anywhere but
    /// this server's origin: the addresses come from [`Self::url`] and redirects are refused (see
    /// [`client_builder`], and the module docs for the whole rule).
    fn headers(&self, credential: &Credential) -> header::HeaderMap {
        let mut headers = credential.header();
        headers.extend(gateway_headers(self.gateway.as_deref()));
        headers
    }

    /// The JSON a call answers, read within the cap.
    pub(crate) async fn json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> Result<T, ApiError> {
        let response = self.call(request).await?;
        from_json(self.read(response).await?)
    }

    /// A call whose answer is only its status: `204` for a pause, `201` for an added torrent. Any body
    /// it does send - an error this client will not use - is read and dropped within the cap.
    pub(crate) async fn empty(&self, request: RequestBuilder) -> Result<(), ApiError> {
        let response = self.call(request).await?;
        self.read(response).await.map(|_| ())
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
            let response = self
                .send(request.headers(self.headers(&credential)))
                .await?;
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            // The server's own reason comes first, because it is the answer to the question we asked;
            // only a refusal with no business answer behind it is about the credential.
            let answer = self.problem(response).await?;
            if !self.refused(status, answer.as_deref()).await {
                let line = answer.unwrap_or_else(|| status_line(status));
                return Err(if status == reqwest::StatusCode::NOT_FOUND {
                    ApiError::NotFound(line)
                } else {
                    ApiError::Answer(line)
                });
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
            // Drop the credential this attempt sent - only if it is still the one held, never one a call in
            // flight has already replaced it with (see [`Self::invalidate_if_held`]) - so the next attempt
            // earns a new one.
            self.invalidate_if_held(&credential).await;
        }
        unreachable!("the second attempt always returns")
    }

    /// `GET /api/v1/events` with a credential somebody else earned, and no renewing of it: one attempt, so
    /// that a stream which is refused waits for the refresh loop rather than logging in to ask again (see
    /// [`crate::remote::sse`]). It clears nothing either: the credential it was refused with may already
    /// have been replaced by the loop's own call, and the loop will find the same dead one on its next round
    /// and renew it there.
    ///
    /// No `categories` parameter: every category wakes the same refresh, and asking for a subset is one more
    /// thing to keep in step with the server's list of names. This is the one call on
    /// [`Self::events_client`], the client without a request timeout, because its body never ends.
    pub(crate) async fn events_with(&self, credential: &Credential) -> Result<Response, ApiError> {
        let request = self
            .events_client
            .get(self.url("/api/v1/events"))
            .headers(self.headers(credential));
        let response = self.send(request).await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let answer = self.problem(response).await?;
        if self.refused(status, answer.as_deref()).await {
            return Err(ApiError::Refused(answer.unwrap_or_else(|| {
                "the credential this connection presents is not accepted".to_string()
            })));
        }
        Err(ApiError::Answer(
            answer.unwrap_or_else(|| status_line(status)),
        ))
    }

    /// Sends a request, giving up the moment somebody asks to stop. Waiting for headers is the long part -
    /// a server that has gone quiet opens the connection and then says nothing for its whole timeout - so
    /// that is where a stop has to land.
    async fn send(&self, request: RequestBuilder) -> Result<Response, ApiError> {
        tokio::select! {
            result = request.send() => result.map_err(|problem| ApiError::Transport(clean(&problem.to_string()))),
            _ = self.stopping.waited() => Err(ApiError::Transport(STOPPED.to_string())),
        }
    }

    /// The relay's half of a call: a `GET` or `HEAD` of a stream address the relay was told to open,
    /// through the client without a request timeout, with the gateway's credential and header beside it.
    ///
    /// The credential here is the gateway's proof only, never the server's session bearer (see
    /// [`super::login::Auth::relay_credential`]): the stream token is already in the address, so a
    /// password or device login behind a gateway that checks only its key header sends that header
    /// alone. A `401` from the upstream renews the gateway credential once and tries again; a `401`
    /// that is the server's own answer (a bad stream token) does not trigger a session login.
    ///
    /// The only wait wrapped is the one for the response head: a body may run for as long as the player
    /// keeps asking for it, and the relay's own cap (see [`RELAY_HEAD_TIMEOUT`]) is the bound on the head.
    ///
    /// Statuses the relay copies back as-is: `200`, `206`, `404`, `416`. Every other status, including
    /// every `3xx`, is an upstream the relay does not understand: it answers `502` with a fixed body.
    pub(crate) async fn relay_get(
        &self,
        path_and_query: &str,
        range: Option<&header::HeaderValue>,
        head: bool,
    ) -> Result<Response, ApiError> {
        let url = self.url(path_and_query);
        // The relay's credential is the gateway's proof only, never the server's session bearer
        // (see [`super::login::Auth::relay_credential`]). It is earned under the same stop race the
        // refresh loop's [`Self::authorise`] uses: a `Kind::Gateway` token fetch can take a network
        // round, and a stop pressed while it is running should not be waited out behind the auth lock.
        // The lock is held only for the fetch, never across an upstream request (see
        // [`Self::current_relay_credential`]).
        let credential = self.current_relay_credential().await?;
        // Kept for the `401` path: a renewal only invalidates the token this request actually sent.
        let sent = credential.clone();
        // Only a `Kind::Gateway` login can renew its relay credential, so only it retries a `401`.
        // A `Kind::Server` session was never sent by the relay, and a `Kind::Basic` re-send is the
        // same credential: for both, a `401` maps straight to a `502` with no retry and no
        // invalidate.
        let renew_on_401 = self.gateway_token_kind().await;
        let attempt = self
            .relay_get_with_credential(&url, range, head, credential)
            .await;
        // A `401` on a gateway login is the only answer that means "renew the credential and try
        // again": the gateway token is the credential the relay sent, and it can be re-issued. Every
        // other status, including every `3xx` and a `401` on any other kind, is an upstream the relay
        // does not forward: a fixed `502`.
        if matches!(attempt, Err(ApiError::Refused(_))) && renew_on_401 {
            // The gateway token is no good: invalidate it (never the server session), earn a new one,
            // and make the one retry. The lock is held only for the invalidate and the fetch, never
            // across the retry's upstream request.
            let renewed = self.renew_gateway_credential(&sent).await?;
            self.relay_get_with_credential(&url, range, head, renewed)
                .await
        } else {
            attempt
        }
    }

    /// The one upstream call the relay makes on its own behalf: the request built for `url` with the
    /// credential the relay holds, the head waited on within [`RELAY_HEAD_TIMEOUT`], and the status
    /// mapped - a status the relay forwards is the response, a `401` is a refused credential (the one
    /// that earns a retry), and every other status is a fixed `502` the relay answers with its own
    /// body (see [`crate::remote::relay::forward`]).
    ///
    /// Both the first attempt and the `401` retry go through this, so the head timeout and the
    /// status allowlist are written once.
    async fn relay_get_with_credential(
        &self,
        url: &str,
        range: Option<&header::HeaderValue>,
        head: bool,
        credential: Credential,
    ) -> Result<Response, ApiError> {
        let mut request = if head {
            self.events_client.head(url.to_string())
        } else {
            self.events_client.get(url.to_string())
        };
        if let Some(range) = range {
            request = request.header(header::RANGE, range.clone());
        }
        request = request.headers(self.headers(&credential));
        // Only the wait for the head is timed: the body after it may run as long as the player keeps
        // asking for it, and it is bounded by the relay's permit, not by a clock.
        let response = match tokio::time::timeout(RELAY_HEAD_TIMEOUT, self.send(request)).await {
            Ok(result) => result?,
            // The head wait ran out: its own error, so the relay can answer `504` rather
            // than the `502` a transport failure or a bad upstream answer gets.
            Err(_) => return Err(ApiError::HeadTimeout),
        };
        let status = response.status();
        // Copy back the statuses a media player understands.
        if status == StatusCode::OK
            || status == StatusCode::PARTIAL_CONTENT
            || status == StatusCode::NOT_FOUND
            || status == StatusCode::RANGE_NOT_SATISFIABLE
        {
            return Ok(response);
        }
        // A `401` is a refused credential: the relay's caller decides whether it retries. It is its
        // own [`ApiError::Refused`] (with the fixed line) rather than an `Answer`, so the retry path
        // can tell a `401` from every other status.
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Refused(
                "the upstream refused the relay's credential".to_string(),
            ));
        }
        // Every other status, including every `3xx`: a fixed 502. The body is a constant the relay
        // sends, never this response's text, which might hold a URL with the stream token in it.
        Err(ApiError::Answer(
            "the server could not be reached: the stream answer was not a stream".to_string(),
        ))
    }

    /// The credential the relay sends on a stream forward, earned under the stop race [`Self::authorise`]
    /// uses. The auth lock is held only for the fetch itself: a `Kind::Gateway` token fetch can take a
    /// network round, and a stop pressed while it is running is not waited out behind the lock, nor is the
    /// lock held across any upstream request that follows.
    async fn current_relay_credential(&self) -> Result<Credential, ApiError> {
        let mut auth = self.auth.lock().await;
        tokio::select! {
            result = auth.relay_credential() => result.map_err(ApiError::Refused),
            _ = self.stopping.waited() => Err(ApiError::Transport(STOPPED.to_string())),
        }
    }

    /// The gateway token is no good: say so, and earn a new one. The lock is held only for the invalidate
    /// and the fetch - never across the retry's upstream request - and the fetch runs under the same stop
    /// race as the first attempt.
    ///
    /// This is a compare-and-swap on the token: `refused` is the credential the failed request sent. Under
    /// the lock, the cached token is invalidated only if it is still that one. If another request already
    /// refreshed it, the cached new token is used as it is, so one expiry costs one token fetch however
    /// many requests were in flight.
    async fn renew_gateway_credential(&self, refused: &Credential) -> Result<Credential, ApiError> {
        let mut auth = self.auth.lock().await;
        if let Credential::Bearer(token) = refused
            && auth.gateway_token_is(token)
        {
            auth.invalidate_gateway();
        }
        tokio::select! {
            result = auth.relay_credential() => result.map_err(ApiError::Refused),
            _ = self.stopping.waited() => Err(ApiError::Transport(STOPPED.to_string())),
        }
    }

    /// A body this client is still willing to wait for: the same cap as [`read_capped`](bodies::read_capped),
    /// except that a server dribbling bytes out stops being a reason to stay the moment somebody asks to
    /// stop.
    async fn read(&self, response: Response) -> Result<Vec<u8>, ApiError> {
        let mut stream = Box::pin(response.bytes_stream());
        let mut body = Vec::new();
        loop {
            tokio::select! {
                chunk = next(&mut stream) => match chunk {
                    Some(Ok(chunk)) => {
                        if !push(&mut body, chunk.as_ref()) {
                            return Err(too_big());
                        }
                    }
                    Some(Err(problem)) => {
                        return Err(ApiError::Transport(clean(&problem.to_string())));
                    }
                    None => return Ok(body),
                },
                _ = self.stopping.waited() => return Err(ApiError::Transport(STOPPED.to_string())),
            }
        }
    }

    /// Reads a failed response's body within the cap and pulls out the reason this API puts in every error:
    /// `{"error": {"code": ..., "message": ...}}`, ours and fbt-api's alike. `None` for a body that says
    /// nothing in that shape, which is what a gateway's error page and an empty answer both look like.
    async fn problem(&self, response: Response) -> Result<Option<String>, ApiError> {
        let body = self.read(response).await?;
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

    /// The credential to send, signing in or fetching a fresh token when the one we hold is missing,
    /// stale, or has just been refused. A login that cannot be completed is a refusal, not a transport
    /// failure: what went wrong is about the credential, and only the reaching part looks like a link.
    async fn authorise(&self) -> Result<Credential, ApiError> {
        let mut auth = self.auth.lock().await;
        // A login already under way when somebody asks to stop should not be waited out: fifteen seconds of
        // connect timeout plus an Argon2 verify is the longest thing this connection can be waiting on.
        tokio::select! {
            result = auth.credential() => result.map_err(ApiError::Refused),
            _ = self.stopping.waited() => Err(ApiError::Transport(STOPPED.to_string())),
        }
    }

    /// The credential this connection holds right now, without signing in: `None` when a request would have
    /// to log in or fetch a token first. The event stream sends this instead of calling [`Self::call`], so
    /// that exactly one task ever logs in (see [`crate::remote::sse`]).
    pub(crate) fn held(&self) -> Option<Credential> {
        // `try_lock`, not `lock`: a login in flight is exactly the case where there is nothing yet worth
        // sending, and the stream has its own short pause to sit through instead of queueing behind it.
        let Ok(auth) = self.auth.try_lock() else {
            return None;
        };
        auth.held()
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

    /// Says the credential `sent` is no good, so that the next call renews it - if it is still the one held.
    ///
    /// This is a compare-and-swap the relay already uses for its gateway token (see
    /// [`Self::renew_gateway_credential`]): two calls in flight can both be refused, and the one that
    /// refused the *older* credential must not throw away the session the newer one just earned. Unconditional
    /// invalidation would pay for that with a fresh login - an Argon2 verify or a device signature round, or
    /// a gateway token fetch - for a refusal that had already been overtaken.
    async fn invalidate_if_held(&self, sent: &Credential) {
        self.auth.lock().await.invalidate_if(sent);
    }

    /// Whether this client's login is a `Kind::Gateway` token, the only kind whose relay credential
    /// the relay may renew on a `401` (see [`Self::relay_get`]). A `Kind::Server` session and a
    /// `Kind::Basic` credential are never renewed by the relay: for the former the relay never sent
    /// the session, and for the latter a re-send is the same credential.
    async fn gateway_token_kind(&self) -> bool {
        self.auth.lock().await.gateway_token_kind()
    }

    /// Ends this client's own session with the server, so the device stops showing up as connected. A
    /// gateway token is not ours to revoke and a server with authentication off has nothing to end, which
    /// is what [`crate::remote::login`] says and does nothing about.
    ///
    /// Two things keep a stop button quick. The credential is taken only if nobody else is holding it: a
    /// login still in flight would otherwise be waited out for its own timeout, and a session this client
    /// cannot read is one it cannot revoke either. And the revoke gets two seconds rather than the fifteen
    /// a login is allowed - whatever it has not answered by then it is not going to answer usefully, and
    /// this client is leaving regardless of what it says.
    pub(crate) async fn log_out(&self) {
        let Ok(auth) = self.auth.try_lock() else {
            return;
        };
        auth.log_out().await;
    }
}

/// What one request carries as its proof of who is asking.
#[derive(Clone)]
pub(crate) enum Credential {
    /// Nothing: this server answers without a login, or a gateway has already let us through.
    Open,
    /// A session token from the server, or a bearer token from a gateway.
    Bearer(String),
    /// A user name and password, given to a gateway rather than to the server. The password keeps its
    /// `Zeroizing` wrapper all the way to the header, so no plain copy of it is left behind here.
    Basic(String, Zeroizing<String>),
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
    /// The `Authorization` header this credential is sent as. Both spellings are marked sensitive, so a
    /// header map that reaches a log line or a panic message says `<sensitive>` rather than the secret.
    fn header(&self) -> header::HeaderMap {
        let mut headers = header::HeaderMap::new();
        match self {
            Credential::Open => {}
            Credential::Bearer(token) => {
                if let Ok(mut value) = header::HeaderValue::from_str(&format!("Bearer {token}")) {
                    value.set_sensitive(true);
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
/// which is what this server's login route and any gateway in front of it read. The pair and its base64 are
/// wrapped as they are built, so the password never gets a plain-`String` copy that outlives the header; the
/// value itself is marked sensitive, because a `HeaderValue`'s bytes are where a secret would be printed. A
/// password that is not header-safe (a byte above 0x7f) gets no header at all rather than a wrong one, and
/// what comes back - an ordinary unauthenticated answer - is then the honest one.
pub(crate) fn basic_header(user: &str, password: &str) -> header::HeaderMap {
    let encoded = Zeroizing::new({
        use base64::Engine as _;
        // The colon-joined pair is the secret in its plainest spelling, so it gets wrapped too rather
        // than being a temporary that zeroize has no chance at.
        let pair = Zeroizing::new(format!("{user}:{password}"));
        format!(
            "Basic {}",
            base64::prelude::BASE64_STANDARD.encode(pair.as_str())
        )
    });
    let mut headers = header::HeaderMap::new();
    if let Ok(mut value) = header::HeaderValue::from_str(encoded.as_str()) {
        value.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, value);
    }
    headers
}

/// The header names a gateway key must not be sent under, spelled lowercase because `HeaderName` lowercases
/// everything it parses and a person types `Content-Length` as readily as `content-length`.
///
/// A gateway header is added to a request with `HeaderMap::insert`, which *replaces* whatever was already
/// under that name rather than adding a second value beside it. So a key typed into one of these names does
/// not fail loudly: it deletes something the request needs. `authorization` and `proxy-authorization` are
/// where this client's own credential goes, so the call would carry the gateway's key as its proof and no
/// session at all; `content-type` and `content-length` say what the body is and how long it is, which a
/// server that cannot read the body answers with something unhelpful about the credential; `host`,
/// `connection`, `transfer-encoding`, `te` and `upgrade` are what HTTP itself writes - `te` and `upgrade`
/// are the pair a gateway uses to ask for a protocol switch, which is how an events stream that wants none
/// ends up in somebody's websocket handler; `cookie` is a server's own way of remembering somebody, and this
/// client keeps nothing in one.
const OWNED_HEADER_NAMES: [&str; 10] = [
    "authorization",
    "proxy-authorization",
    "host",
    "content-length",
    "content-type",
    "cookie",
    "connection",
    "transfer-encoding",
    "te",
    "upgrade",
];

/// Whether `name` is one of those: see [`OWNED_HEADER_NAMES`].
fn is_our_own_header(name: &header::HeaderName) -> bool {
    OWNED_HEADER_NAMES.contains(&name.as_str())
}

/// What is wrong with a name somebody typed for the header a gateway reads its key out of, when something is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatewayHeaderNameProblem {
    /// Not a field name in the sense HTTP uses: RFC 9110's `field-name` grammar, which is letters, digits
    /// and the punctuation a header name may contain - dashes, underscores and dots among them, spaces and
    /// colons not among them. A space, a colon, an accented letter or a newline all make one of these.
    NotAFieldName,
    /// A header this client puts on its own requests: `authorization`, `content-type` and the rest of the
    /// list in this module. Sending a gateway key under one replaces what belongs there rather than adding
    /// a second credential beside it.
    ThisClientsOwnHeader,
}

/// What is wrong with `name` as a gateway key header, if anything: `None` says a gateway key can go under it.
///
/// Whitespace at either end is trimmed away, because that is what a person types into a field whose end they
/// cannot see (`GatewayKey::check` trims the same way, so the two agree about what ` X-API-Key ` means).
/// Inside the name there is no licence: every byte has to be one a field name may carry. The connect screen
/// asks this before it lets an answer go out (`fbtt_tui`'s `modal::connect`) and says what to type instead in
/// its own words, so the same spelling is refused in both places rather than only at the connection.
pub fn gateway_header_name_problem(name: &str) -> Option<GatewayHeaderNameProblem> {
    let Ok(parsed) = header::HeaderName::from_str(name.trim()) else {
        return Some(GatewayHeaderNameProblem::NotAFieldName);
    };
    is_our_own_header(&parsed).then_some(GatewayHeaderNameProblem::ThisClientsOwnHeader)
}

/// The front door in front of this server: the header a gateway checks before the request reaches it, and
/// the key that opens it.
///
/// The two halves are paired at construction rather than looked up per request, for one reason: a name with
/// a space in it or a key with a newline in it is something somebody typed into `ui.yaml` or the connect
/// screen, and it is worth one line said before anything is dialled rather than a transport error on each
/// call. The value keeps its `Zeroizing` wrapper here and is marked sensitive on its way out, so neither
/// this struct nor a header map that reaches a log line repeats it.
pub(crate) struct GatewayKey {
    name: header::HeaderName,
    value: Zeroizing<String>,
}

impl std::fmt::Debug for GatewayKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The name is a setting anybody could read off `ui.yaml`; the key is the secret.
        write!(f, "GatewayKey({}, <redacted>)", self.name)
    }
}

impl GatewayKey {
    /// Checks these are a header this client can actually send, and pairs them.
    ///
    /// Both lines name the header and never the key: an error here is shown on a screen somebody else may
    /// be looking at, and a gateway key repeated in one is a key spent.
    pub(crate) fn check(name: &str, value: Zeroizing<String>) -> Result<GatewayKey, String> {
        let name = header::HeaderName::from_str(name.trim())
            .map_err(|_| format!("{} is not a header name", clean(name)))?;
        // The same names the connect screen refuses (see [`gateway_header_name_problem`]), asked here too
        // because `ui.yaml` is edited by hand and this is the only place every target passes through.
        if is_our_own_header(&name) {
            return Err(format!(
                "{name} is a header this client sends itself: a gateway key needs a name of its own"
            ));
        }
        if value.is_empty() {
            return Err(format!("the gateway key for {name} is empty"));
        }
        // Checked here and built again per request (see [`Self::apply`]) so the secret's only long-lived
        // home is the `Zeroizing` above: a `HeaderValue` is plain bytes, and its copy of the key would not
        // be overwritten when this went.
        header::HeaderValue::from_str(&value).map_err(|_| {
            format!("the gateway key for {name} is not a value an HTTP header can carry")
        })?;
        Ok(GatewayKey { name, value })
    }
}

/// The gateway's own header on its own: the target's header when this connection has a key to send under it,
/// and an empty map when it does not.
///
/// A call site hands this to `RequestBuilder::headers` either way and says nothing about the difference -
/// which is what the login calls need (they carry no session token of ours, so they have no other headers to
/// fold it into), and it is what [`Api::headers`] extends the credential's map with. The value is built here
/// rather than kept in a `HeaderValue`, so the key's only long-lived home is its `Zeroizing`; it is marked
/// sensitive, because a `HeaderValue`'s bytes are where a secret would be printed.
pub(crate) fn gateway_headers(gateway: Option<&GatewayKey>) -> header::HeaderMap {
    let mut headers = header::HeaderMap::new();
    if let Some(key) = gateway {
        let mut value =
            header::HeaderValue::from_str(&key.value).expect("checked when this key was paired");
        value.set_sensitive(true);
        headers.insert(key.name.clone(), value);
    }
    headers
}

/// The front door for `target`: the header its name names, with the key this run got hold of under it.
///
/// Both halves have to be there or neither is. A target that names a header whose key is nowhere describes a
/// server behind a gateway that will refuse everything before it ever sees our credential - so it gets one
/// line saying the key is missing, which the person can answer at the connect screen, rather than a refusal
/// per call. A key with no name to send it under is the same mistake the other way round, and this client
/// does not guess which header a gateway was configured to read.
fn gateway_header(
    target: &RemoteTarget,
    key: Option<Zeroizing<String>>,
) -> Result<Option<Arc<GatewayKey>>, String> {
    match (target.gateway_key_header.as_deref(), key) {
        (Some(name), Some(key)) => Ok(Some(Arc::new(GatewayKey::check(name, key)?))),
        (Some(name), None) => Err(format!(
            "this server is behind a gateway that checks {name}, and no key for it was given"
        )),
        (None, Some(_)) => {
            Err("a gateway key was given with no header to send it under".to_string())
        }
        (None, None) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GatewayHeaderNameProblem, GatewayKey, gateway_header_name_problem, gateway_headers,
    };
    use zeroize::Zeroizing;

    /// The fixture every refusal below is asked about: distinctive enough that its appearing in a line means
    /// the line repeated the secret.
    const SECRET: &str = "correct-horse-battery";

    /// A gateway key typed into one of this client's own header names is refused before anything is dialled.
    /// `HeaderMap::insert` replaces, so accepting `Authorization` would have meant every request carrying the
    /// gateway's key as its only proof: the sign-in call would carry it instead of the password, and the
    /// server behind the gateway would refuse a login it was never shown. Case-insensitive because
    /// `HeaderName` lowercases what it parses: `Content-Length` and `content-length` are the same header.
    #[test]
    fn a_gateway_key_refuses_a_header_this_client_sends_itself() {
        for name in [
            "authorization",
            "Authorization",
            " AUTHORIZATION ",
            "Proxy-Authorization",
            "host",
            "Content-Length",
            "content-type",
            "Cookie",
            "connection",
            "Transfer-Encoding",
            "TE",
            "Upgrade",
        ] {
            assert_eq!(
                gateway_header_name_problem(name),
                Some(GatewayHeaderNameProblem::ThisClientsOwnHeader),
                "{name:?} is a header this client puts on its own requests"
            );
            let problem = GatewayKey::check(name, Zeroizing::new(SECRET.to_string()))
                .expect_err("a name this client uses itself should not pair with a key");
            assert!(
                problem.contains(name.trim().to_lowercase().as_str()),
                "the refusal did not say which header: {problem}"
            );
            assert!(
                !problem.contains(SECRET),
                "the refusal repeated the gateway key: {problem}"
            );
        }
    }

    /// The other half of the same name rule: something that is not a field name at all is refused too, rather
    /// than becoming a transport error on every call. A space and a colon are the two spellings a person
    /// arrives with most often - `X API Key` read out loud from a gateway's documentation, `X:API-Key` copied
    /// from one that spells its headers the HTTP/2 way.
    #[test]
    fn a_gateway_header_name_has_to_be_one_http_allows() {
        for name in [
            "",
            "   ",
            "X API Key",
            "x-api key",
            "X:API-Key",
            ":authority",
            "X-API\tKey",
            "X-ÄPI-Key",
            "X-API@Key",
        ] {
            assert_eq!(
                gateway_header_name_problem(name),
                Some(GatewayHeaderNameProblem::NotAFieldName),
                "{name:?} is not a header name"
            );
            let problem = GatewayKey::check(name, Zeroizing::new(SECRET.to_string()))
                .expect_err("a name HTTP does not allow should not pair with a key");
            assert!(
                problem.ends_with("is not a header name"),
                "the refusal did not say what was wrong with {name:?}: {problem}"
            );
        }
    }

    /// The names people actually type, accepted: `HeaderName` lowercases them on the way in, so what a target
    /// keeps and what a request carries is one spelling however the person typed it - which matters because
    /// a gateway matches header names case-insensitively and this client must not send two different ones.
    #[test]
    fn an_ordinary_gateway_header_name_pairs_with_its_key() {
        for name in [
            "X-API-Key",
            "x-api-key",
            "  X-Api-Key  ",
            "X_Api_Key",
            "cf-access-secret",
        ] {
            assert_eq!(
                gateway_header_name_problem(name),
                None,
                "{name:?} is usable"
            );
            let key = GatewayKey::check(name, Zeroizing::new(SECRET.to_string()))
                .expect("a gateway's own header name should pair with its key");
            // The value is redacted where a log line can reach it, and the name travels lowercase: a gateway
            // matches header names case-insensitively, so one spelling is all this connection should send.
            let want = name.trim().to_lowercase();
            assert_eq!(
                format!("{key:?}"),
                format!("GatewayKey({want}, <redacted>)")
            );
            let headers = gateway_headers(Some(&key));
            let sent = headers
                .get(want.as_str())
                .expect("the key goes out under the name it was paired with");
            assert_eq!(sent.to_str().expect("an ASCII key"), SECRET);
            assert!(
                sent.is_sensitive(),
                "a secret header says so to anything that prints it"
            );
        }
    }
}
