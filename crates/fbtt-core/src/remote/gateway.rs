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
use crate::remote::bodies::{from_json, read_capped};
use crate::remote::device::Device;
use crate::remote::login::{ASSERTION_LIFE, JWT_BEARER, RENEW_BEFORE, reached, reason};
use crate::ui_state::LoginMethod;

/// The life to assume for a credential whose issuer named no expiry. Not "forever": something this client
/// cannot see the end of is something it should plan to replace.
const DEFAULT_LIFE: u64 = 3_600;

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

/// A credential with a life: what to send, until when it is good, and when to start replacing it.
#[derive(Clone)]
pub(crate) struct Live {
    pub(crate) token: String,
    expires_at: Instant,
    /// When sending this stops being worth it, which is before it expires: a token that dies in the middle of
    /// a request costs the same as one that was never sent ([`Live::bought`] says how far ahead).
    renew_at: Instant,
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
        // Which algorithm the header names follows which key was loaded: Ed25519 signs `EdDSA`, P-256 signs
        // `ES256`. The gateway is told before it checks anything, because a provider that only takes ECDSA
        // client keys has to be able to see that this one sent something it can check.
        let Some(alg) = device.jws_alg() else {
            return Err(
                "the device key is not one this client signs a gateway assertion with".to_string(),
            );
        };
        let unsigned = assertion(&device, alg, &self.client_id, &self.token_url);
        let signature = BASE64_URL_SAFE_NO_PAD.encode(device.sign_jws(unsigned.as_bytes()).await?);
        let response = client
            .post(&self.token_url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_assertion_type", JWT_BEARER),
                ("client_assertion", &format!("{unsigned}.{signature}")),
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
        Ok(Live::bought(token.access_token, token.expires_in))
    }
}

/// The JWT assertion a `private_key_jwt` grant is built from: the two base64url parts, unsigned. What
/// gets signed is exactly these bytes, which is what makes the signature checkable by a gateway that has
/// only this client's public key and no shared secret. `alg` is the header's promise about the signature
/// that will follow it ([`Device::jws_alg`] answers that for whichever key was loaded).
fn assertion(device: &Device, alg: &str, client_id: &str, token_url: &str) -> String {
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
        "alg": alg,
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

impl Live {
    /// A credential the issuer says lives `seconds` from now, and when this client starts replacing it.
    ///
    /// The renewal point sits [`RENEW_BEFORE`] ahead of the expiry - except for a credential shorter than
    /// twice that window, which would already be "nearly expired" the moment it was issued and get fetched
    /// again on every single call. For those, half its life is the honest rule: a token that lives eight
    /// seconds is replaced at four, not once a second until it dies.
    pub(crate) fn bought(token: String, seconds: u64) -> Live {
        // A server that says nothing lives an hour; one that says zero is not promising forever, so it is
        // treated as the shortest thing it could mean.
        let life = if seconds == 0 { DEFAULT_LIFE } else { seconds };
        let before = if life <= RENEW_BEFORE.as_secs() * 2 {
            Duration::from_secs((life / 2).max(1))
        } else {
            RENEW_BEFORE
        };
        let now = Instant::now();
        // A life so long it does not fit in an `Instant` is treated as "not soon", which is what the fallback
        // says: two renewal windows ahead.
        let expires_at = now
            .checked_add(Duration::from_secs(life))
            .unwrap_or_else(|| now + before * 2);
        let renew_at = expires_at.checked_sub(before).unwrap_or(now);
        Live {
            token,
            expires_at,
            renew_at,
        }
    }

    /// Whether this credential is still worth sending.
    pub(crate) fn still_good(&self) -> bool {
        Instant::now() < self.renew_at
    }
}

/// What a gateway answers to an assertion.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TokenDto {
    access_token: String,
    expires_in: u64,
}

#[cfg(test)]
mod tests {
    use super::Live;
    use std::time::{Duration, Instant};

    /// A short-lived credential is replaced at half its life rather than on every call. With the 30-second
    /// renewal window applied to an eight-second token, it would have been "nearly expired" from the moment
    /// it was issued - which means a new assertion and a new token for every request.
    #[test]
    fn a_short_lived_credential_is_renewed_at_half_its_life() {
        // The clock is read immediately before each buying, so the bounds below allow for the microseconds a
        // buying takes and nothing else. Lives are whole seconds, so an odd one splits with the renewal window
        // rounded down and the renewal point rounded up: half of thirty-one is fifteen, replaced at sixteen.
        let now = Instant::now();
        let short = Live::bought("t".to_string(), 8);
        assert!(short.still_good(), "a token just bought is worth sending");
        assert!(
            short.renew_at >= now + Duration::from_secs(4)
                && short.renew_at <= now + Duration::from_secs(5),
            "an eight-second credential is replaced at four, not at the end of its life"
        );

        // Thirty-one seconds is shorter than twice the window, so it gets the same rule rather than being
        // stale from birth.
        let now = Instant::now();
        let edge = Live::bought("t".to_string(), 31);
        assert!(
            edge.renew_at >= now + Duration::from_secs(15)
                && edge.renew_at <= now + Duration::from_secs(17),
            "half of thirty-one, rounded"
        );

        // A long life keeps the window it had: nothing about this changes an hour-long session token.
        let now = Instant::now();
        let hour = Live::bought("t".to_string(), 3_600);
        assert!(
            hour.renew_at >= now + Duration::from_secs(3_570)
                && hour.renew_at <= now + Duration::from_secs(3_571),
            "an hour-long credential is replaced half a minute before it expires"
        );

        // No expiry named is not forever: it gets the default life, and a renewal point inside it.
        let now = Instant::now();
        let unstated = Live::bought("t".to_string(), 0);
        assert!(unstated.still_good());
        assert!(
            unstated.renew_at >= now + Duration::from_secs(3_570)
                && unstated.renew_at <= now + Duration::from_secs(3_571),
            "an unstated life is an hour, replaced at fifty-nine minutes"
        );
    }
}
