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
//!   header" is a property of this module rather than something each call site has to remember.

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

    /// Says the credential this client holds is no good, so that the next call renews it.
    async fn invalidate(&self) {
        self.auth.lock().await.invalidate();
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
