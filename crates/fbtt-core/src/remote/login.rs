//! Who we are to a server: the credential a request carries, and how it is earned.
//!
//! A featherbittorrent server takes one of three things as proof of who is asking - nothing, a user name
//! and password, or a signature from a device key - and answers with a session token that every later
//! request carries. A gateway standing in front of a server that runs with authentication off takes the
//! password on every request instead, or a signed OAuth2 assertion it trades for a bearer token. This
//! module decides which of those a [`RemoteTarget`] describes, gets the credential, and holds it until it
//! needs replacing; the transport that carries it is [`super::http`].
//!
//! The login calls go out on this module's own client rather than through [`super::Api`], because `Api`
//! holds this: a login that asked `Api` to send its own request would wait forever on the lock that
//! already belongs to whoever is waiting for the login to finish.
//!
//! Nothing here fails in a way a caller has to handle carefully. A bad combination of settings is an error
//! at construction, which the backend turns into a notice; everything after that - a refused password, an
//! agent that is not running, a gateway that will not issue a token - is one line for the screen and a
//! link that says it is down.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use ssh_key::{Algorithm, HashAlg, LineEnding, Signature, SshSig};
use zeroize::Zeroizing;

use crate::auth::LOGIN_NAMESPACE;
use crate::engine::map::clean;
use crate::remote::Secret;
use crate::remote::bodies::{from_json, read_capped};
use crate::remote::device::Device;
use crate::remote::gateway::{Gateway, Live};
use crate::remote::http::{Credential, GatewayKey, basic_header, gateway_headers};
use crate::ui_state::{LoginMethod, RemoteTarget};

/// How much of a long credential's life is spent before it is replaced: renewal starts this far ahead of the
/// expiry the server named, so a token that would expire between here and the next request is not sent. A
/// credential shorter than twice this renews at half its life instead (see [`Live::bought`]).
pub(crate) const RENEW_BEFORE: Duration = Duration::from_secs(30);
/// How long a login or a token request may take overall. It is one round trip plus a hash, not a download.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the revoke that ends our session on the way out may take. A person pressing stop is not waiting
/// for an answer they will never use, and a server that has gone quiet would make this the longest part of
/// closing the connection ([`super::http::Api::log_out`]).
const REVOKE_TIMEOUT: Duration = Duration::from_secs(2);
/// The life of a gateway assertion this client signs for itself: long enough to survive a slow gateway,
/// short enough that a leaked one stops being useful on its own.
pub(crate) const ASSERTION_LIFE: u64 = 60;
/// The OAuth2 grant type and assertion type of a `private_key_jwt` client (RFC 7523).
pub(crate) const JWT_BEARER: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// What this client proves to the server, and the credential that proof bought.
pub(crate) struct Auth {
    /// The client every login and token request goes out on. Kept here rather than borrowed from
    /// [`super::Api`] so a login cannot deadlock on the lock around this `Auth` (see the module docs).
    client: Client,
    /// The server's address with no trailing slash: `{base}/api/v1/auth/session` is where a session is.
    base: String,
    /// The header a gateway standing in front of this server checks before the request reaches it, when the
    /// target named one. These are the calls that earn the credential, and a gateway that turns them away
    /// means there is no credential to earn - so they carry it too (see [`super::http::gateway_headers`]).
    /// The exception is the gateway's own token endpoint, which is another host's business and gets nothing
    /// from this client but the assertion it signed (see [`Gateway::fetch`]).
    gateway: Option<Arc<GatewayKey>>,
    kind: Kind,
}

/// Which of the four things this client does to get in.
enum Kind {
    /// Nothing: this server asks nobody to prove anything.
    Open,
    /// A featherbittorrent server: log in, then carry the token it answers.
    Server(Server),
    /// A gateway in front of one: trade a signed assertion for a bearer token.
    Gateway(Gateway),
    /// A gateway that takes the user name and password on every request instead.
    Basic {
        user: String,
        password: Zeroizing<String>,
    },
}

/// A login to a featherbittorrent server, and the session it bought.
#[derive(Debug)]
struct Server {
    /// Which of this server's logins is being used.
    login: LoginMethod,
    /// The account a password login names.
    user: Option<String>,
    /// The private key a key login signs with, or the one to look for in ssh-agent.
    key_path: Option<PathBuf>,
    /// The passphrase or password, kept until it has been spent.
    secret: Secret,
    /// The token we hold, and when it stops being good. `None` once it has been given up on.
    session: Option<Live>,
}

impl Auth {
    /// The credential holder for `target`, signed with `secret`, behind a front door that checks `gateway`
    /// if it names one.
    ///
    /// Errs only about a target and secret that do not go together - a password login with no user name, a
    /// gateway with no token endpoint - which is a thing to say once rather than on every request.
    pub(crate) fn new(
        target: &RemoteTarget,
        secret: Secret,
        gateway: Option<Arc<GatewayKey>>,
    ) -> Result<Auth, String> {
        let client = Client::builder()
            .connect_timeout(LOGIN_TIMEOUT)
            .timeout(LOGIN_TIMEOUT)
            // The same rule the transport follows: a redirect would leave these paths behind.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|problem| problem.to_string())?;
        let base = target.url.trim_end_matches('/').to_string();
        let kind = match target.login {
            LoginMethod::None => {
                if matches!(secret, Secret::None) {
                    Kind::Open
                } else {
                    return Err(
                        "this server needs no credential, so none was asked for".to_string()
                    );
                }
            }
            LoginMethod::Password | LoginMethod::GatewayBasic => {
                let user = named(&target.user, "a user name")?;
                let Secret::Password(password) = secret else {
                    return Err("a password login needs a password".to_string());
                };
                match target.login {
                    LoginMethod::Password => Kind::Server(Server {
                        login: LoginMethod::Password,
                        user: Some(user),
                        key_path: target.key_path.clone(),
                        secret: Secret::Password(password),
                        session: None,
                    }),
                    _ => Kind::Basic { user, password },
                }
            }
            LoginMethod::KeyFile | LoginMethod::Agent => Kind::Server(Server {
                login: target.login,
                user: target.user.clone(),
                key_path: target.key_path.clone(),
                secret,
                session: None,
            }),
            LoginMethod::GatewayKey => Kind::Gateway(Gateway {
                token_url: named(&target.token_url, "a gateway token endpoint")?,
                client_id: named(&target.client_id, "a gateway client id")?,
                key_path: target.key_path.clone(),
                secret,
                live: None,
            }),
        };
        Ok(Auth {
            client,
            base,
            gateway,
            kind,
        })
    }

    /// The credential to put on the next request, signing in or fetching a token when there is none, or
    /// the one there is has run down.
    pub(crate) async fn credential(&mut self) -> Result<Credential, String> {
        // Field by field, so the immutable halves and the mutable half do not fight over `self`. The gateway
        // header is only ever read, and it goes out on these calls beside whatever credential they earn from
        // this server - the token endpoint excepted, which is another host's (see [`Gateway::fetch`]).
        let Auth {
            client,
            base,
            gateway: key,
            kind,
        } = self;
        match kind {
            Kind::Open => Ok(Credential::Open),
            Kind::Basic { user, password } => Ok(Credential::Basic(user.clone(), password.clone())),
            Kind::Server(server) => {
                if let Some(session) = &server.session
                    && session.still_good()
                {
                    return Ok(Credential::Bearer(session.token.clone()));
                }
                let session = server.sign_in(client, base, key.as_deref()).await?;
                server.session = Some(session.clone());
                Ok(Credential::Bearer(session.token))
            }
            Kind::Gateway(gateway) => {
                if let Some(live) = &gateway.live
                    && live.still_good()
                {
                    return Ok(Credential::Bearer(live.token.clone()));
                }
                let live = gateway.fetch(client).await?;
                gateway.live = Some(live.clone());
                Ok(Credential::Bearer(live.token))
            }
        }
    }

    /// Says the credential `sent` - the one the refused request actually carried - is no good, and gives
    /// up only what it is: the session or token still held when another call in flight has already earned a
    /// different one stays held.
    ///
    /// This is the same compare-and-swap the relay uses for its gateway token (see [`Self::gateway_token_is`]):
    /// two calls can be refused at once, and the refusal of the older credential must not throw away the
    /// session the newer one just bought - an Argon2 verify or a device signature round, or a gateway token
    /// fetch, spent again for no reason. A `Basic` credential is resent on every call, so there is nothing
    /// cached to clear for it; `Open` holds nothing at all. The compare is on the token string: a `Bearer`
    /// holds its secret in a type that carries a password elsewhere, and deriving equality on that type to
    /// compare one field is what this check is not for.
    pub(crate) fn invalidate_if(&mut self, sent: &Credential) {
        let Credential::Bearer(token) = sent else {
            return;
        };
        match &mut self.kind {
            Kind::Server(server)
                if server
                    .session
                    .as_ref()
                    .is_some_and(|session| session.token == *token) =>
            {
                server.session = None;
            }
            Kind::Gateway(gateway)
                if gateway
                    .live
                    .as_ref()
                    .is_some_and(|live| live.token == *token) =>
            {
                gateway.live = None;
            }
            _ => {}
        }
    }

    /// Says only the gateway's token is no good, so the next gateway call renews it. This is what the
    /// relay's `401` path calls (see [`super::http::Api::invalidate_gateway`]): a `Kind::Server` session
    /// is the TUI's, and the relay never sent it, so a `401` on the relay's forward must not throw it
    /// away.
    pub(crate) fn invalidate_gateway(&mut self) {
        if let Kind::Gateway(gateway) = &mut self.kind {
            gateway.live = None;
        }
    }

    /// Whether the gateway token held right now is exactly `token`. The relay's `401` path uses it as the
    /// compare of a compare-and-swap: only the request that sent the token still held invalidates it.
    pub(crate) fn gateway_token_is(&self, token: &str) -> bool {
        match &self.kind {
            Kind::Gateway(gateway) => gateway
                .live
                .as_ref()
                .is_some_and(|live| live.token == token),
            _ => false,
        }
    }

    /// Whether this target signs to a gateway rather than to the server itself, which changes what a `403`
    /// means (see [`super::http::Api::refused`]) and what a notice about a refusal should say.
    pub(crate) fn signs_for_a_gateway(&self) -> bool {
        matches!(self.kind, Kind::Gateway(_) | Kind::Basic { .. })
    }

    /// Whether this login's relay credential can be renewed after a `401`: only `Kind::Gateway`
    /// holds a bearer the gateway can re-issue. A `Kind::Server` session is the TUI's, and the relay
    /// never sent it, so a `401` on the relay's forward cannot be about the session.
    pub(crate) fn gateway_token_kind(&self) -> bool {
        matches!(self.kind, Kind::Gateway(_))
    }

    /// The credential the relay may send on a stream forward: only the gateway's proof, never the
    /// server's session bearer. The stream token is already in the query the relay forwards, so a
    /// password or device login behind a gateway that checks only its key header sends that header
    /// alone, and a `GatewayBasic` login sends its `Authorization`. [`Credential::Open`] when the
    /// target has no gateway credential of its own.
    ///
    /// This is what keeps the relay from carrying the TUI's session bearer onto every player seek:
    /// a `Kind::Server` credential is the session, and the relay is not where a session goes. A
    /// `Kind::Gateway` login's bearer is a gateway token earned from the gateway's own endpoint, and
    /// that one the relay may send: earning it is what the gateway checks.
    pub(crate) async fn relay_credential(&mut self) -> Result<Credential, String> {
        match &mut self.kind {
            // A server login's credential is the session bearer: the relay never sends it. The
            // gateway header still goes on the request beside it (see [`super::http::gateway_headers`]).
            Kind::Server(_) | Kind::Open => Ok(Credential::Open),
            Kind::Basic { user, password } => Ok(Credential::Basic(user.clone(), password.clone())),
            // The gateway's own bearer token, earning it when the one held is missing or stale.
            // `Gateway::fetch` signs an assertion the gateway checks against this client's public
            // key, so it needs nothing from the server - and no gateway key (see its own docs).
            Kind::Gateway(gateway) => {
                if let Some(live) = &gateway.live
                    && live.still_good()
                {
                    return Ok(Credential::Bearer(live.token.clone()));
                }
                let live = gateway.fetch(&self.client).await?;
                gateway.live = Some(live.clone());
                Ok(Credential::Bearer(live.token))
            }
        }
    }

    /// The credential in hand right now, without signing in: what the event stream is allowed to send, and
    /// `None` when a request would first have to log in or fetch a token. The stream takes this rather than
    /// [`Self::credential`] so that only the refresh loop ever spends a login (see [`crate::remote::sse`]).
    pub(crate) fn held(&self) -> Option<Credential> {
        match &self.kind {
            Kind::Open => Some(Credential::Open),
            Kind::Basic { user, password } => {
                Some(Credential::Basic(user.clone(), password.clone()))
            }
            // A credential in its renewal window counts as not held: the stream would be sending a token the
            // refresh loop is about to replace, and waiting costs it nothing.
            Kind::Server(server) => server
                .session
                .as_ref()
                .filter(|session| session.still_good())
                .map(|session| Credential::Bearer(session.token.clone())),
            Kind::Gateway(gateway) => gateway
                .live
                .as_ref()
                .filter(|live| live.still_good())
                .map(|live| Credential::Bearer(live.token.clone())),
        }
    }

    /// Ends this client's own session with the server, when it has one to end. A gateway token is not ours
    /// to revoke - it expires on its own - and a server that asks nobody for anything has nothing to end.
    /// This is the one thing here that does not need an answer: whatever happens, this client is leaving, so
    /// the revoke gets [`REVOKE_TIMEOUT`] in total rather than the fifteen a login is allowed.
    pub(crate) async fn log_out(&self) {
        let Kind::Server(server) = &self.kind else {
            return;
        };
        let Some(session) = &server.session else {
            return;
        };
        let revoke = self
            .client
            .delete(format!("{}/api/v1/auth/session", self.base))
            .timeout(REVOKE_TIMEOUT)
            .headers(gateway_headers(self.gateway.as_deref()))
            .bearer_auth(&session.token)
            .send();
        // The request's own ceiling plus a wall-clock one: a client on its way out should not have to trust
        // that the timeout machinery is what ends it.
        let _ = tokio::time::timeout(REVOKE_TIMEOUT, revoke).await;
    }
}

/// `Debug` for the credential holder says which login is in use and nothing else: a token, a password and
/// a decrypted key are the three things a log line must not repeat.
impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let method = match &self.kind {
            Kind::Open => "no login",
            Kind::Server(server) => match server.login {
                LoginMethod::Password => "password",
                LoginMethod::KeyFile => "device key file",
                LoginMethod::Agent => "ssh-agent",
                // A gateway method gets its own holder, so this one is always one of the three above.
                _ => "device key",
            },
            Kind::Gateway(_) => "gateway token",
            Kind::Basic { .. } => "gateway password",
        };
        f.debug_struct("Auth")
            .field("base", &self.base)
            .field("login", &method)
            .finish_non_exhaustive()
    }
}

impl Server {
    /// A session the login tests hold in place of a real one: long enough to be good, renewal window the
    /// default. See [`Auth::holding`].
    #[cfg(test)]
    fn holding_session(token: &str) -> Option<Live> {
        Some(Live::bought_now(token.to_string()))
    }

    /// One login to this server, answered with the token it hands out.
    ///
    /// Every request here goes past the same front door as the requests that carry the token afterwards: a
    /// gateway that checks `gateway` never sees a session token, so what it looks for instead is this header.
    async fn sign_in(
        &self,
        client: &Client,
        base: &str,
        gateway: Option<&GatewayKey>,
    ) -> Result<Live, String> {
        match self.login {
            LoginMethod::Password => {
                let user = named(&self.user, "a user name")?;
                let Secret::Password(password) = &self.secret else {
                    return Err("a password login needs a password".to_string());
                };
                let response = client
                    .post(format!("{base}/api/v1/auth/session"))
                    .headers(basic_header(&user, password))
                    .headers(gateway_headers(gateway))
                    .send()
                    .await
                    .map_err(reached)?;
                session(response).await
            }
            LoginMethod::KeyFile | LoginMethod::Agent => {
                let device =
                    Device::load(self.login, self.key_path.as_deref(), &self.secret).await?;
                let challenge: ChallengeDto = post(
                    client,
                    &format!("{base}/api/v1/auth/challenge"),
                    &ChallengeBody {
                        public_key: &device.line,
                    },
                    gateway,
                )
                .await?;
                let nonce = BASE64_URL_SAFE_NO_PAD
                    .decode(&challenge.nonce)
                    .map_err(|_| "the server's challenge is not base64url".to_string())?;
                // The same framing `ssh-keygen -Y sign` uses, so the server's own verifier accepts it.
                let framed = SshSig::signed_data(LOGIN_NAMESPACE, HashAlg::Sha512, &nonce)
                    .map_err(|problem| problem.to_string())?;
                let signature = device.sign(&framed).await?;
                let sshsig = SshSig::new(
                    device.public.clone(),
                    LOGIN_NAMESPACE,
                    HashAlg::Sha512,
                    Signature::new(Algorithm::Ed25519, signature)
                        .map_err(|problem| problem.to_string())?,
                )
                .map_err(|problem| problem.to_string())?;
                let body = KeyLoginBody {
                    public_key: &device.line,
                    nonce: &challenge.nonce,
                    signature: &sshsig
                        .to_pem(LineEnding::LF)
                        .map_err(|problem| problem.to_string())?,
                };
                session(
                    client
                        .post(format!("{base}/api/v1/auth/session"))
                        .json(&body)
                        .headers(gateway_headers(gateway))
                        .send()
                        .await
                        .map_err(reached)?,
                )
                .await
            }
            // A gateway method never gets here: it has its own holder.
            _ => Err("this login method is not a server login".to_string()),
        }
    }
}

/// A `POST` of JSON that answers JSON, with the failures this client cares about turned into lines.
///
/// `gateway` is the header a front door checks before this request reaches the server (see
/// [`super::http::gateway_headers`]).
async fn post<T: serde::de::DeserializeOwned>(
    client: &Client,
    url: &str,
    body: &impl Serialize,
    gateway: Option<&GatewayKey>,
) -> Result<T, String> {
    let response = client
        .post(url)
        .json(body)
        .headers(gateway_headers(gateway))
        .send()
        .await
        .map_err(reached)?;
    json(response).await
}

/// One answer to a login: the token and how long it lives.
async fn session(response: reqwest::Response) -> Result<Live, String> {
    #[derive(Debug, Default, Deserialize)]
    #[serde(default)]
    struct SessionDto {
        token: String,
        expires_in: u64,
    }
    let live = json::<SessionDto>(response).await?;
    if live.token.is_empty() {
        return Err("the server answered with no session token".to_string());
    }
    Ok(Live::bought(live.token, live.expires_in))
}

/// A successful response's JSON, or the reason a failed one gave.
async fn json<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T, String> {
    let status = response.status();
    let body = read_capped(response)
        .await
        .map_err(|problem| problem.line())?;
    if !status.is_success() {
        return Err(reason(&body, status));
    }
    from_json(body).map_err(|problem| problem.line())
}

/// The reason a server or gateway gave for refusing something, or the status when it gave none. The message
/// is somebody else's text and this client repeats it on the screen, so it goes through the same cut and
/// clean every other remote string gets (see [`crate::engine::map::clean`]).
pub(crate) fn reason(body: &[u8], status: reqwest::StatusCode) -> String {
    let message = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(serde_json::Value::as_str)
                .map(clean)
        })
        .filter(|message| !message.is_empty());
    message.unwrap_or_else(|| format!("the server answered {status}"))
}

/// The body of `POST /api/v1/auth/challenge`.
#[derive(Serialize)]
struct ChallengeBody<'a> {
    public_key: &'a str,
}

/// The body of a key login: the three fields the server's route names, and no others.
#[derive(Serialize)]
struct KeyLoginBody<'a> {
    public_key: &'a str,
    nonce: &'a str,
    signature: &'a str,
}

/// The challenge a server answers with: the bytes to sign. How long they stay answerable is not read -
/// this client answers the challenge it was just given, in the same round.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ChallengeDto {
    nonce: String,
}

/// A target field one of these logins cannot do without.
fn named(value: &Option<String>, what: &str) -> Result<String, String> {
    value
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("this connection needs {what}"))
}

/// A login request that never got an answer.
pub(crate) fn reached(problem: reqwest::Error) -> String {
    format!("the sign-in request did not reach the server: {problem}")
}

/// A holder with exactly one thing held: a `Kind::Server` session, a `Kind::Gateway` token or a
/// `Basic` credential, and nothing else. Built only to test the compare-and-swap in
/// [`Auth::invalidate_if`], where signing in over a real login would cost an Argon2 verify or a
/// gateway round the test does not want to wait for.
#[cfg(test)]
fn holding(kind: Kind) -> Auth {
    Auth {
        client: reqwest::Client::new(),
        base: String::new(),
        gateway: None,
        kind,
    }
}

#[cfg(test)]
mod tests {
    use super::{Credential, Gateway, Kind, Live, Server, holding};
    use crate::ui_state::LoginMethod;
    use zeroize::Zeroizing;

    /// What a held credential is: the token, or nothing. A `Credential` cannot be compared whole (it
    /// carries a password elsewhere in its variants), so the tests compare the string that matters.
    fn bearer_of(held: Option<Credential>) -> Option<String> {
        match held {
            Some(Credential::Bearer(token)) => Some(token),
            _ => None,
        }
    }

    /// A `Kind::Server` login holding the session `token`, nothing else to look at.
    fn server_session(token: &str) -> Kind {
        Kind::Server(Server {
            login: LoginMethod::Password,
            user: Some("alice".to_string()),
            key_path: None,
            secret: crate::remote::Secret::Password(Zeroizing::new(String::new())),
            session: Server::holding_session(token),
        })
    }

    /// A `Kind::Gateway` login holding the token `token`, nothing else to look at.
    fn gateway_token(token: &str) -> Kind {
        Kind::Gateway(Gateway {
            token_url: "http://127.0.0.1:1/token".to_string(),
            client_id: "test".to_string(),
            key_path: None,
            secret: crate::remote::Secret::KeyPassphrase(None),
            live: Some(Live::bought_now(token.to_string())),
        })
    }

    // A `401` clears only the credential the refused call sent. Two calls in flight can both be refused:
    // the one that sent the older credential must not throw away the session the newer one just earned,
    // which would cost a fresh login - an Argon2 verify or a device signature round, or a gateway token
    // fetch - for a refusal that had already been overtaken.

    /// The refusal of the session still held clears it.
    #[test]
    fn a_refusal_of_the_held_session_clears_it() {
        let mut auth = holding(server_session("a"));
        auth.invalidate_if(&Credential::Bearer("a".to_string()));
        assert!(auth.held().is_none(), "the sent session is cleared");
    }

    /// The refusal of an older session leaves a newer one, earned in the meantime, still held.
    #[test]
    fn a_refusal_of_an_older_session_keeps_a_newer_one() {
        let mut auth = holding(server_session("b"));
        auth.invalidate_if(&Credential::Bearer("a".to_string()));
        assert_eq!(
            bearer_of(auth.held()),
            Some("b".to_string()),
            "the session the newer call earned is kept"
        );
    }

    /// The same, for the gateway's token: a refusal of the token still held clears it.
    #[test]
    fn a_refusal_of_the_held_gateway_token_clears_it() {
        let mut auth = holding(gateway_token("a"));
        auth.invalidate_if(&Credential::Bearer("a".to_string()));
        assert_eq!(bearer_of(auth.held()), None, "the sent token is cleared");
    }

    /// The refusal of an older gateway token leaves a newer one, fetched in the meantime, still held.
    #[test]
    fn a_refusal_of_an_older_gateway_token_keeps_a_newer_one() {
        let mut auth = holding(gateway_token("b"));
        auth.invalidate_if(&Credential::Bearer("a".to_string()));
        assert_eq!(
            bearer_of(auth.held()),
            Some("b".to_string()),
            "the token the newer call fetched is kept"
        );
    }

    /// A credential with nothing cached behind it clears nothing: a `Basic` is resent on every call, and
    /// an `Open` target holds nothing at all, so a refusal of either one may not touch what is held.
    #[test]
    fn a_refusal_with_nothing_cached_clears_nothing() {
        let mut auth = holding(server_session("a"));
        auth.invalidate_if(&Credential::Basic(
            "alice".to_string(),
            Zeroizing::new(String::new()),
        ));
        assert_eq!(
            bearer_of(auth.held()),
            Some("a".to_string()),
            "a basic refusal does not touch the held session"
        );

        let mut auth = holding(server_session("a"));
        auth.invalidate_if(&Credential::Open);
        assert_eq!(
            bearer_of(auth.held()),
            Some("a".to_string()),
            "an open refusal does not touch the held session"
        );
    }
}
