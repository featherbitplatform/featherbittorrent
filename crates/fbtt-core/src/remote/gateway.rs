//! The gateway logins: a bearer token bought with an assertion the device key signed.
//!
//! A featherbittorrent server behind a gateway runs with its own auth off, so nothing here talks to the
//! server - only to the token endpoint. What makes the trade safe is the signature: the gateway has this
//! client's public key on file and needs no shared secret to check what it signed.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use reqwest::Client;
use serde::Deserialize;
use ssh_key::HashAlg;

use crate::remote::Secret;
use crate::remote::device::Device;
use crate::remote::http::{from_json, read_capped};
use crate::remote::login::{ASSERTION_LIFE, JWT_BEARER, RENEW_BEFORE, reached, reason};
use crate::ui_state::LoginMethod;

/// A gateway login that trades a signed assertion for a bearer token, and the token it bought.
#[derive(Debug)]
pub(crate) struct Gateway {
    /// Where to post the assertion, which is also the audience it names.
    pub(crate) token_url: String,
    /// The client id this installation is known to the gateway by.
    pub(crate) client_id: String,
    /// The device key that signs the assertion.
    pub(crate) key_path: Option<PathBuf>,
    pub(crate) secret: Secret,
    pub(crate) live: Option<Live>,
}

/// A credential with a life: what to send, and until when it is worth sending.
#[derive(Clone)]
pub(crate) struct Live {
    pub(crate) token: String,
    pub(crate) expires_at: Instant,
}

/// `Debug` says how long it lives and not what it is: a session token, a gateway token and a password are
/// all the same shape here, and none of them belongs in a log line.
impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl Gateway {
    /// One assertion, and the token the gateway trades it for.
    pub(crate) async fn fetch(&self, client: &Client) -> Result<Live, String> {
        let device = Device::load(
            LoginMethod::GatewayKey,
            self.key_path.as_deref(),
            &self.secret,
        )
        .await?;
        let unsigned = assertion(&device, &self.client_id, &self.token_url);
        let signature = device.sign(unsigned.as_bytes()).await?;
        let encoded = BASE64_URL_SAFE_NO_PAD.encode(signature);
        let response = client
            .post(&self.token_url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_assertion_type", JWT_BEARER),
                ("client_assertion", &format!("{unsigned}.{encoded}")),
            ])
            .send()
            .await
            .map_err(reached)?;
        let status = response.status();
        let body = read_capped(response)
            .await
            .map_err(|problem| problem.notice())?;
        if !status.is_success() {
            return Err(format!(
                "the gateway refused this client: {}",
                reason(&body, status)
            ));
        }
        let token: TokenDto = from_json(body).map_err(|problem| problem.notice())?;
        if token.access_token.is_empty() {
            return Err("the gateway answered with no access token".to_string());
        }
        Ok(Live {
            token: token.access_token,
            expires_at: lives(token.expires_in),
        })
    }
}

/// The JWT assertion a `private_key_jwt` grant is built from: the two base64url parts, unsigned. What
/// gets signed is exactly these bytes, which is what makes the signature checkable by a gateway that has
/// only this client's public key and no shared secret.
fn assertion(device: &Device, client_id: &str, token_url: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let mut random = [0u8; 16];
    // A fresh id per attempt is what stops a gateway replaying one assertion twice.
    if getrandom::fill(&mut random).is_err() {
        random = [0u8; 16];
    }
    let mut jti = String::with_capacity(random.len() * 2);
    for byte in random {
        // Writing into a String does not fail.
        let _ = std::fmt::Write::write_fmt(&mut jti, format_args!("{byte:02x}"));
    }
    let header = serde_json::json!({
        "alg": "EdDSA",
        "typ": "JWT",
        // The key a gateway should check the signature against, spelled the way it saw it on the list.
        "kid": device.public.fingerprint(HashAlg::Sha256).to_string(),
    });
    let claims = serde_json::json!({
        "iss": client_id,
        "sub": client_id,
        "aud": token_url,
        "jti": jti,
        "iat": now,
        "exp": now.saturating_add(ASSERTION_LIFE),
    });
    // A `serde_json::Value` always writes itself.
    let header = serde_json::to_string(&header).unwrap_or_default();
    let claims = serde_json::to_string(&claims).unwrap_or_default();
    format!(
        "{}.{}",
        BASE64_URL_SAFE_NO_PAD.encode(header),
        BASE64_URL_SAFE_NO_PAD.encode(claims)
    )
}

/// When a credential that lives `seconds` from now stops being good. A server that says nothing lives an
/// hour; one that says zero is not promising forever, so it is treated as the shortest thing it could mean.
pub(crate) fn lives(seconds: u64) -> Instant {
    let seconds = if seconds == 0 { 3_600 } else { seconds };
    Instant::now()
        .checked_add(Duration::from_secs(seconds))
        .unwrap_or_else(|| Instant::now() + RENEW_BEFORE * 2)
}

impl Live {
    /// Whether this credential is still worth sending, which it is not once it is within [`RENEW_BEFORE`]
    /// of expiring: a token that dies during a request costs the same as one that was never sent.
    pub(crate) fn still_good(&self) -> bool {
        self.expires_at > Instant::now() + RENEW_BEFORE
    }
}

/// What a gateway answers to an assertion.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TokenDto {
    access_token: String,
    expires_in: u64,
}
