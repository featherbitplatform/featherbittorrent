//! Device keys: the public half of an SSH key, and the signature that spends a challenge.
//!
//! A device entry in the config is a name plus a key in the one-line OpenSSH format - the same text
//! that sits in `~/.ssh/authorized_keys`, which is how people already keep a list of machines that
//! may act as them. Ed25519 is the only kind accepted: it is what `ssh-keygen` makes by default now,
//! and every other kind pulls another crypto crate into the tree for a key type nobody logs in with.
//!
//! A login signature is an `sshsig` over the challenge nonce in [`LOGIN_NAMESPACE`] with SHA-512,
//! armored as PEM (what `ssh-keygen -Y sign` produces and what ssh-agent signs for us).

use ssh_key::{Algorithm, HashAlg, PublicKey, SshSig};
use thiserror::Error;

use crate::auth::LOGIN_NAMESPACE;

/// What can go wrong reading a device key or checking one's signature.
#[derive(Debug, Error)]
pub enum KeyError {
    /// The text is not an OpenSSH public key line at all: a private key block, a truncated base64
    /// body, garbage.
    #[error("not an OpenSSH public key: {source}")]
    Invalid {
        /// What was wrong with it, in the parser's words.
        source: ssh_key::Error,
    },
    /// A well-formed key of a kind this server does not accept. Only Ed25519 device keys log in.
    #[error("{algorithm} keys are not supported: use an ed25519 key")]
    Unsupported {
        /// The kind the text named, for the message a person sees.
        algorithm: String,
    },
}

/// Reads a device key from the one-line OpenSSH format (`ssh-ed25519 <base64> [comment]`).
///
/// Surrounding whitespace is ignored: a key arrives by pasting it out of a terminal, and the newline
/// that comes with it is not part of the key. The comment is kept in what is returned, because it is
/// part of the format, but nothing here reads it - the config's own `name` field is what names a
/// device.
pub fn parse_device_key(text: &str) -> Result<PublicKey, KeyError> {
    let text = text.trim();
    let key = PublicKey::from_openssh(text).map_err(|source| match source {
        // An algorithm whose decoder this build does not contain - an ecdsa line, with the ecdsa
        // feature off - is a well-formed key of a kind we would refuse anyway.
        ssh_key::Error::AlgorithmUnknown | ssh_key::Error::AlgorithmUnsupported { .. } => {
            unsupported(text, source)
        }
        source => KeyError::Invalid { source },
    })?;
    if key.algorithm() != Algorithm::Ed25519 {
        return Err(KeyError::Unsupported {
            algorithm: key.algorithm().to_string(),
        });
    }
    Ok(key)
}

/// The answer for a key we cannot read because of what kind it is: the kind named by the line's first
/// word, which is what a person can compare against the `ssh-ed25519` we ask for. A line that names
/// that very kind and still could not be read as one is not a key of any use here, so it gets
/// [`KeyError::Invalid`] instead of a message telling someone to convert a key they already have.
fn unsupported(text: &str, source: ssh_key::Error) -> KeyError {
    match text.split_whitespace().next() {
        Some(kind) if kind != Algorithm::Ed25519.as_str() => KeyError::Unsupported {
            algorithm: kind.to_string(),
        },
        _ => KeyError::Invalid { source },
    }
}

/// The key's fingerprint in the OpenSSH spelling (`SHA256:<base64>`), the short thing a person
/// checks before trusting a device list.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Whether `sig_pem` is an Ed25519 login signature over `nonce`, made by the private half of `key`.
///
/// All of it has to hold: the armour parses as an `sshsig`, the signature carries this exact key, it
/// was made in [`LOGIN_NAMESPACE`] - a signature for some other purpose is not a login - it was
/// hashed with SHA-512, as this protocol requires, and the nonce it signs is `nonce` and not some
/// other challenge. Anything else answers `false`: one bad login, no detail about which part failed,
/// because the answer goes to someone trying to guess.
pub fn verify_login(key: &PublicKey, nonce: &[u8], sig_pem: &str) -> bool {
    SshSig::from_pem(sig_pem.trim())
        .ok()
        // `verify` checks that the signature carries this exact key, that it was made in this
        // namespace, and that it signs this nonce. The hash algorithm is the one part it takes from
        // the signature rather than being told, so the protocol's own rule - SHA-512 - is checked
        // here, and a signature made with a weaker hash is refused rather than accepted.
        .is_some_and(|signature| {
            signature.hash_alg() == HashAlg::Sha512
                && key.verify(LOGIN_NAMESPACE, nonce, &signature).is_ok()
        })
}

#[cfg(test)]
mod tests {
    use super::{KeyError, fingerprint, parse_device_key, verify_login};
    use crate::auth::LOGIN_NAMESPACE;
    use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey, rand_core::UnwrapErr};

    /// A 4096-bit RSA public key line - public data, the same one the `ssh-key` crate tests its
    /// reader with - of the kind this server refuses: RSA drags in the `rsa` crate, which carries an
    /// open advisory.
    const RSA_KEY: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAACAQC0WRHtxuxefSJhpIxGq4ibGFgwYnESPm8C3JFM88A1JJLoprenklrd7VJ+VH3Ov/bQwZwLyRU5dRmfR/SWTtIPWs7tToJVayKKDB+/qoXmM5ui/0CU2U4rCdQ6PdaCJdC7yFgpPL8WexjWN06+eSIKYz1AAXbx9rRv1iasslK/KUqtsqzVliagI6jl7FPO2GhRZMcso6LsZGgSxuYf/Lp0D/FcBU8GkeOo1Sx5xEt8H8bJcErtCe4Blb8JxcW6EXO3sReb4z+zcR07gumPgFITZ6hDA8sSNuvo/AlWg0IKTeZSwHHVknWdQqDJ0uczE837caBxyTZllDNIGkBjCIIOFzuTT76HfYc/7CTTGk07uaNkUFXKN79xDiFOX8JQ1ZZMZvGOTwWjuT9CqgdTvQRORbRWwOYv3MH8re9ykw3Ip6lrPifY7s6hOaAKry/nkGPMt40m1TdiW98MTIpooE7W+WXu96ax2l2OJvxX8QR7l+LFlKnkIEEJd/ItF1G22UmOjkVwNASTwza/hlY+8DoVvEmwum/nMgH2TwQT3bTQzF9s9DOJkH4d8p4Mw4gEDjNx0EgUFA91ysCAeUMQQyIvuR8HXXa+VcvhOOO5mmBcVhxJ3qUOJTyDBsT0932Zb4mNtkxdigoVxu+iiwk0vwtvKwGVDYdyMP5EAQeEIP1t0w== alice@example";

    /// A fresh Ed25519 key pair, the kind `ssh-keygen -t ed25519` makes. `PrivateKey::random` wants a
    /// source that cannot fail, so the system's random bytes go through `UnwrapErr`.
    fn keypair() -> PrivateKey {
        let mut rng = UnwrapErr(getrandom::SysRng);
        PrivateKey::random(&mut rng, Algorithm::Ed25519).expect("an ed25519 key")
    }

    /// An RSA key is a well-formed key of a kind we do not accept; text that is not a public key at
    /// all is something else again.
    #[test]
    fn rsa_device_key_is_unsupported() {
        let error = parse_device_key(RSA_KEY).expect_err("an rsa key is refused");
        assert!(matches!(error, KeyError::Unsupported { .. }), "{error}");
        assert!(
            error.to_string().contains("ed25519"),
            "the message says what to use instead: {error}"
        );
        // A private key block pasted where a public key belongs is not a kind of public key.
        assert!(matches!(
            parse_device_key("-----BEGIN OPENSSH PRIVATE KEY-----"),
            Err(KeyError::Invalid { .. }),
        ));
    }

    /// A signature the private key made verifies against the public one, and everything that is not
    /// exactly that does not.
    #[test]
    fn ed25519_round_trip_signature() {
        let private = keypair();
        let line = private.public_key().to_openssh().expect("the public line");
        let public = parse_device_key(&line).expect("an ed25519 device key");
        assert_eq!(public, *private.public_key(), "the line round-trips");
        let print = fingerprint(&public);
        assert!(print.starts_with("SHA256:"), "{print}");

        let nonce = [3u8; 32];
        let pem = private
            .sign(LOGIN_NAMESPACE, HashAlg::Sha512, &nonce)
            .expect("sign the nonce")
            .to_pem(LineEnding::LF)
            .expect("armour the signature");
        assert!(verify_login(&public, &nonce, &pem));
        assert!(
            !verify_login(&public, &[4u8; 32], &pem),
            "another challenge"
        );

        let other = keypair();
        assert!(
            !verify_login(other.public_key(), &nonce, &pem),
            "another device's key"
        );

        let elsewhere = private
            .sign("file-checksums", HashAlg::Sha512, &nonce)
            .expect("sign for something else")
            .to_pem(LineEnding::LF)
            .expect("armour it");
        assert!(
            !verify_login(&public, &nonce, &elsewhere),
            "a signature made for another purpose"
        );

        let weaker = private
            .sign(LOGIN_NAMESPACE, HashAlg::Sha256, &nonce)
            .expect("sign with sha256")
            .to_pem(LineEnding::LF)
            .expect("armour it");
        assert!(
            !verify_login(&public, &nonce, &weaker),
            "not the agreed hash"
        );

        assert!(!verify_login(&public, &nonce, "not armour at all"));
    }
}
