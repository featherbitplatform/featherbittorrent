//! The device key this client signs with: where its private half lives, and both spellings of its public
//! half.
//!
//! Two spellings because two things ask for them: a server's login challenge wants the OpenSSH line, and
//! ssh-agent wants the wire blob. The agent path never reads the private bytes at all - it asks the agent
//! to sign, and matches identities by the blob its own key would produce.
//!
//! Signing has two spellings too, for the same reason: an `sshsig` wants OpenSSH's signature blob, and a
//! gateway's JWT assertion wants what RFC 7518 wants, which for an ECDSA key is not the same bytes
//! ([`Device::sign`] and [`Device::sign_jws`]).

use std::path::Path;
use std::time::Duration;

use signature::Signer as _;
use ssh_agent_lib::proto::{Identity, PublicCredential, SignRequest};
use ssh_key::encoding::Decode as _;
use ssh_key::{Algorithm, EcdsaCurve, Mpint, PrivateKey, Signature};

use crate::remote::Secret;
use crate::ui_state::LoginMethod;

/// How long ssh-agent may take to answer. It is a local socket and every call here is one request, so ten
/// seconds means the agent is wedged - and a login should say that rather than sit on it, because a wedged
/// agent otherwise looks exactly like a slow server from the screen's side.
const AGENT_TIMEOUT: Duration = Duration::from_secs(10);

/// The line for an agent that did not answer in time, naming which call it was asked to make.
fn wedged(what: &str) -> String {
    format!(
        "ssh-agent did not {what} within {} seconds",
        AGENT_TIMEOUT.as_secs()
    )
}

/// The width of one P-256 scalar in the fixed-width spelling a JWS wants, in bytes.
const P256_FIELD: usize = 32;

/// What a gateway's assertion is signed with, spelled the way JWS names it, or `None` for a key this client
/// signs no assertion with.
///
/// Two algorithms because two kinds of key are worth accepting: Ed25519 (`EdDSA`) is what everything else
/// here is signed with, and ECDSA P-256 (`ES256`) is what the providers that document a `private_key_jwt`
/// grant hand out - several of them take RSA or ECDSA client keys and no Ed25519 at all, so an
/// Ed25519-only client could not use them. The rest are left out on purpose: P-384 and P-521 buy bytes this
/// trade does not need, and RSA buys a crate with an open advisory (see the workspace comment on ssh-key).
fn gateway_alg(public: &ssh_key::public::KeyData) -> Option<&'static str> {
    match public.algorithm() {
        Algorithm::Ed25519 => Some("EdDSA"),
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        } => Some("ES256"),
        _ => None,
    }
}

/// The key types `login` signs with, spelled for the line somebody gets when their key is not one of them.
fn accepted(login: LoginMethod) -> &'static str {
    match login {
        LoginMethod::GatewayKey => "Ed25519 or P-256",
        _ => "Ed25519",
    }
}

/// Says so, before anything is asked of a server, when a key is not one this client signs `login` with.
///
/// Worth settling here rather than at the far end: a challenge, an assertion and the round trip to either
/// all cost the same for a key that could never have answered them, and "this client signs with Ed25519 or
/// P-256" is a better line on the screen than a server that refused without explaining itself. `where_key`
/// names the key in that line: a path for a file, ssh-agent for an identity the agent holds.
fn check_type(
    login: LoginMethod,
    public: &ssh_key::public::KeyData,
    where_key: &str,
) -> Result<(), String> {
    if matches!(login, LoginMethod::GatewayKey) {
        return gateway_alg(public).map(|_| ()).ok_or_else(|| {
            format!(
                "{where_key} is a {} key; a gateway assertion is signed with {}",
                public.algorithm(),
                accepted(login)
            )
        });
    }
    if public.is_ed25519() {
        Ok(())
    } else {
        Err(format!(
            "{where_key} is a {} key; this client signs logins with {}",
            public.algorithm(),
            accepted(login)
        ))
    }
}

/// The device key this client signs with: its public half, spelled both ways a server wants it, and
/// where the private half is.
pub(crate) struct Device {
    /// The one-line OpenSSH public key: what a challenge is asked for, and what a login names.
    pub(crate) line: String,
    /// ...as the type an `sshsig` is built from, and the fingerprint of.
    pub(crate) public: ssh_key::public::KeyData,
    source: Source,
}

/// Where the private half of a device key lives.
enum Source {
    /// A file this process reads (and decrypts once, with the passphrase it was given). Boxed: an ECDSA
    /// private key is several times the size of an Ed25519 one, and this enum is held for one login at a
    /// time, so the extra pointer costs nothing worth keeping.
    File(Box<PrivateKey>),
    /// ssh-agent, which is asked to sign and never hands its key over.
    Agent(PublicCredential),
}

impl Device {
    /// Reads or finds the key `login` says to use.
    ///
    /// Nothing about the key is cached between logins: a login happens once an hour, and re-reading the
    /// file (or asking the agent what it holds) is what lets a key be swapped out in ssh-agent, or a
    /// `authorized_keys` line be fixed, without restarting this client.
    pub(crate) async fn load(
        login: LoginMethod,
        key_path: Option<&Path>,
        secret: &Secret,
    ) -> Result<Device, String> {
        let passphrase = match secret {
            Secret::KeyPassphrase(passphrase) => passphrase.as_deref().map(|text| text.as_str()),
            _ => None,
        };
        // Which half signs is the method's answer, not the secret's: `Agent`, and a gateway that names no
        // key file, both mean ssh-agent; `KeyFile` (or a gateway that does name one) means the file.
        let from_file = match login {
            LoginMethod::KeyFile => true,
            LoginMethod::GatewayKey => key_path.is_some(),
            _ => false,
        };
        if from_file {
            let path = key_path.ok_or_else(|| "this login needs a device key file".to_string())?;
            let key = read_key(path, passphrase)?;
            let asked = key.public_key();
            let public = asked.key_data().clone();
            // The key type is settled before anything is asked of a server: only Ed25519 answers a
            // featherbittorrent challenge, and only Ed25519 or P-256 signs a gateway assertion - so a key
            // file of any other kind is worth saying so about now, rather than after a round trip that
            // could not have worked.
            check_type(login, &public, &path.display().to_string())?;
            return Ok(Device {
                line: asked.to_openssh().map_err(|problem| problem.to_string())?,
                public,
                source: Source::File(Box::new(key)),
            });
        }
        agent_device(login, key_path).await
    }

    /// Signs `data` with the private half and hands back OpenSSH's spelling of the signature: for Ed25519
    /// that is the 64-byte signature itself, for an ECDSA key it is `r` and `s` as two SSH mpints. An
    /// `sshsig` is built from exactly this; a JWS wants [`Device::sign_jws`] instead.
    pub(crate) async fn sign(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        match &self.source {
            // In-process signing: `Signer::try_sign`, which is what `SshSig` is built on top of, signs the
            // bytes it is given and nothing else - a JWT needs exactly that.
            Source::File(key) => {
                let signature: Signature = key.try_sign(data).map_err(signing)?;
                Ok(signature.as_bytes().to_vec())
            }
            Source::Agent(credential) => {
                let mut client = connect_agent().await?;
                let request = SignRequest {
                    credential: credential.clone(),
                    data: data.to_vec(),
                    flags: 0,
                };
                use ssh_agent_lib::agent::Session as _;
                let signature = tokio::time::timeout(AGENT_TIMEOUT, client.sign(request))
                    .await
                    .map_err(|_| wedged("sign"))?
                    .map_err(signing)?;
                // The agent's own `Signature` type is a different crate's version of ours, and what it hands
                // back is the same blob OpenSSH would put in an authentication request - which is what our
                // own signing produces too, for both key types.
                Ok(signature.as_bytes().to_vec())
            }
        }
    }

    /// The `alg` a gateway assertion this key signs carries, or `None` for a key that signs none.
    ///
    /// A header has to name the algorithm before the claims are signed, so this is asked first; the answer
    /// comes from the same rule [`check_type`] applied when the key was loaded, which is why the gateway's
    /// own call site only has to handle it defensively.
    pub(crate) fn jws_alg(&self) -> Option<&'static str> {
        gateway_alg(&self.public)
    }

    /// Signs `data` the way a JWS wants a signature spelled: Ed25519 as it comes, ECDSA P-256 as the two
    /// scalars at their fixed width.
    pub(crate) async fn sign_jws(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        let signature = self.sign(data).await?;
        match self.public.algorithm() {
            // OpenSSH spells an ECDSA signature as two mpints - length-prefixed, each possibly carrying a
            // leading zero byte or coming up short. RFC 7518 wants neither: `r` then `s`, both the width of
            // the field, so this is where the two worlds are reconciled.
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            } => fixed_width_r_s(&signature),
            // An Ed25519 signature is one fixed-size blob in both worlds, so it goes across unchanged.
            _ => Ok(signature),
        }
    }
}

/// The JWS spelling of an SSH ECDSA signature: `r || s`, each scalar left-padded to [`P256_FIELD`] bytes.
///
/// `blob` is what a signature's `as_bytes` gives for an `ecdsa-sha2-nistp256` key - two SSH mpints, the same
/// thing OpenSSH puts in its own signatures and what ssh-agent answers with. Each one loses its length
/// prefix and any leading zero byte, and is put back right-aligned in a field-width integer; anything that
/// does not fit those two rules is not a P-256 signature this client may put in an assertion.
fn fixed_width_r_s(blob: &[u8]) -> Result<Vec<u8>, String> {
    let problem =
        || "the device key's ECDSA signature is not one this client can put in a JWT".to_string();
    let mut reader = blob;
    let r = Mpint::decode(&mut reader).map_err(|_| problem())?;
    let s = Mpint::decode(&mut reader).map_err(|_| problem())?;
    // Nothing may trail the two scalars: a longer blob is somebody else's signature format, and guessing
    // which part to ignore would mean signing an assertion nobody can check.
    if !reader.is_empty() {
        return Err(problem());
    }
    let mut out = Vec::with_capacity(P256_FIELD * 2);
    for scalar in [r, s] {
        // This is the strip-the-leading-zero part of the rule; it says `None` for a value SSH spelled as a
        // negative one, which neither half of a signature ever is.
        let bytes = scalar.as_positive_bytes().ok_or_else(problem)?;
        // A value wider than the field cannot be a P-256 scalar at all.
        if bytes.len() > P256_FIELD {
            return Err(problem());
        }
        out.extend(core::iter::repeat_n(0u8, P256_FIELD - bytes.len()));
        out.extend_from_slice(bytes);
    }
    Ok(out)
}

/// Reads and, if needed, decrypts a private key file.
fn read_key(path: &Path, passphrase: Option<&str>) -> Result<PrivateKey, String> {
    let key = PrivateKey::read_openssh_file(path)
        .map_err(|problem| format!("cannot read the key file {}: {problem}", path.display()))?;
    if !key.is_encrypted() {
        return Ok(key);
    }
    let passphrase = passphrase.ok_or_else(|| {
        format!(
            "{} is passphrase-protected: give this connection a passphrase",
            path.display()
        )
    })?;
    key.decrypt(passphrase)
        .map_err(|_| format!("the passphrase does not open {}", path.display()))
}

/// Finds the key ssh-agent is holding that this client should log in as.
async fn agent_device(login: LoginMethod, key_path: Option<&Path>) -> Result<Device, String> {
    let identities = identities().await?;
    // A `.pub` beside the private key says which of the agent's keys this connection means; without one,
    // the first key this client could sign this login with is the only answer - Ed25519 for a
    // featherbittorrent server, Ed25519 or P-256 for a gateway.
    let wanted = key_path
        .map(|path| format!("{}.pub", path.display()))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| ssh_key::PublicKey::from_openssh(text.trim()).ok());
    let identity = identities
        .iter()
        .find(|identity| match (&wanted, identity) {
            (Some(wanted), identity) => same_key(wanted.key_data(), identity.credential.key_data()),
            (None, identity) => {
                matches!(identity.credential, PublicCredential::Key(_))
                    && usable_by_agent(login, identity.credential.key_data())
            }
        })
        .ok_or_else(|| {
            format!(
                "ssh-agent holds no {} key this client can log in with",
                accepted(login)
            )
        })?;
    let credential = match &identity.credential {
        PublicCredential::Key(key) => key,
        // A certificate is not what a featherbittorrent server lists as a device.
        PublicCredential::Cert(_) => return Err("ssh-agent's key is a certificate".to_string()),
    };
    let public = ours(credential)?;
    // The same rule as for a key file, checked before a challenge or an assertion is asked for: an agent
    // holding nothing but RSA keys is not this client's device, and finding that out over the network costs
    // a round trip and a worse line on the screen.
    check_type(login, &public, "ssh-agent's key")?;
    // The line the challenge and the login name: the one from the `.pub` file when there was one, and
    // otherwise spelled back out of the agent's own key.
    let line = wanted
        .map(|key| key.to_openssh())
        .unwrap_or_else(|| ssh_key::PublicKey::new(public.clone(), "").to_openssh())
        .map_err(|problem| problem.to_string())?;
    Ok(Device {
        line,
        public,
        source: Source::Agent(identity.credential.clone()),
    })
}

/// Whether a key ssh-agent is holding is one this client signs `login` with.
///
/// The rule is [`check_type`]'s, but asked of the agent crate's own copy of the key: it is a different
/// version of the same library, so its curve enum is a different type and the comparison has to be made in
/// its terms rather than ours. Converting every identity to our types to ask the question would work too,
/// and would report nothing useful about the ones that do not convert - which is worth more when the answer
/// is "none of these".
fn usable_by_agent(login: LoginMethod, key: &ssh_agent_lib::ssh_key::public::KeyData) -> bool {
    if !matches!(login, LoginMethod::GatewayKey) {
        return key.is_ed25519();
    }
    key.is_ed25519()
        || key
            .ecdsa()
            .is_some_and(|public| public.curve() == ssh_agent_lib::ssh_key::EcdsaCurve::NistP256)
}

/// Whether an agent's key and a key file are the same key: their wire blobs, which every implementation
/// spells identically, compared rather than the two crates' different `KeyData` types.
fn same_key(
    ours: &ssh_key::public::KeyData,
    theirs: &ssh_agent_lib::ssh_key::public::KeyData,
) -> bool {
    let ours = encode_ours(ours);
    let theirs = encode_theirs(theirs);
    match (ours, theirs) {
        (Some(ours), Some(theirs)) => ours == theirs,
        _ => false,
    }
}

/// A key as the OpenSSH wire blob, with this crate's ssh-key.
fn encode_ours(key: &ssh_key::public::KeyData) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    ssh_key::encoding::Encode::encode(key, &mut bytes)
        .ok()
        .map(|()| bytes)
}

/// ...and with the agent crate's, which is a different version of the same library spelling the same key.
fn encode_theirs(key: &ssh_agent_lib::ssh_key::public::KeyData) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    ssh_agent_lib::ssh_encoding::Encode::encode(key, &mut bytes)
        .ok()
        .map(|()| bytes)
}

/// The agent's key as this crate's type: the blob it was sent in, read back with our version of ssh-key,
/// which is the version an `sshsig` and a fingerprint are built from.
fn ours(key: &ssh_agent_lib::ssh_key::public::KeyData) -> Result<ssh_key::public::KeyData, String> {
    let bytes =
        encode_theirs(key).ok_or_else(|| "the agent's key cannot be spelled out".to_string())?;
    let mut reader = bytes.as_slice();
    ssh_key::public::KeyData::decode(&mut reader)
        .map_err(|problem| format!("the agent's key is not one this client understands: {problem}"))
}

/// What ssh-agent is holding.
async fn identities() -> Result<Vec<Identity>, String> {
    use ssh_agent_lib::agent::Session as _;
    let mut client = connect_agent().await?;
    tokio::time::timeout(AGENT_TIMEOUT, client.request_identities())
        .await
        .map_err(|_| wedged("list its keys"))?
        .map_err(|problem| format!("ssh-agent would not list its keys: {problem}"))
}

/// A connection to the agent this machine is running.
async fn connect_agent() -> Result<ssh_agent_lib::client::Client<AgentStream>, String> {
    #[cfg(unix)]
    {
        let path = std::env::var("SSH_AUTH_SOCK")
            .map_err(|_| "SSH_AUTH_SOCK is not set: ssh-agent is not running".to_string())?;
        let stream = tokio::time::timeout(AGENT_TIMEOUT, tokio::net::UnixStream::connect(&path))
            .await
            .map_err(|_| wedged("answer the connection"))?
            .map_err(|problem| format!("cannot reach ssh-agent at {path}: {problem}"))?;
        Ok(ssh_agent_lib::client::Client::new(stream))
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        let path = r"\\.\pipe\openssh-ssh-agent";
        let stream = ClientOptions::new()
            .open(path)
            .map_err(|problem| format!("cannot reach ssh-agent at {path}: {problem}"))?;
        Ok(ssh_agent_lib::client::Client::new(stream))
    }
}

/// The socket type `connect_agent` returns, named so the two cfg branches above can share a return type.
#[cfg(unix)]
type AgentStream = tokio::net::UnixStream;
#[cfg(windows)]
type AgentStream = tokio::net::windows::named_pipe::NamedPipeClient;

/// A key that would not sign, whether it was in a file or held by the agent.
fn signing<E: std::fmt::Display>(problem: E) -> String {
    format!("the device key would not sign: {problem}")
}

#[cfg(test)]
mod tests {
    use super::{Device, P256_FIELD, Secret, fixed_width_r_s};
    use crate::scratch::ScratchDir;
    use crate::ui_state::LoginMethod;

    /// The bytes a hex string stands for, so the fixtures below read the way OpenSSH spells a signature.
    /// Whitespace between the pairs is only there to keep the lines short enough to read.
    fn blob(hex: &str) -> Vec<u8> {
        let packed: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        packed
            .as_bytes()
            .chunks(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).expect("a hex fixture");
                u8::from_str_radix(text, 16).expect("a hex fixture")
            })
            .collect()
    }

    /// An `ecdsa-sha2-nistp384` private key, unencrypted, as `ssh-keygen -t ecdsa -b 384` writes it. A
    /// fixture made for this test, not anybody's key: what it is here to show is that a curve this client
    /// does not sign with is named before anything is asked of a server.
    const P384_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAiAAAABNlY2RzYS
1zaGEyLW5pc3RwMzg0AAAACG5pc3RwMzg0AAAAYQSHXOPT3iF2nw8mAEAX1ZXyiIWnQnGC
Xy7kuYHf8K2QLXWliwWqrQyDP9HEwCidRxWmu8eHBb7cKqBA7aHC4p/vslaM6GkdNqtdEP
pgScUb48PpX8tYGWK5q8umcXSoQG4AAADgnY4xk52OMZMAAAATZWNkc2Etc2hhMi1uaXN0
cDM4NAAAAAhuaXN0cDM4NAAAAGEEh1zj094hdp8PJgBAF9WV8oiFp0Jxgl8u5LmB3/CtkC
11pYsFqq0Mgz/RxMAonUcVprvHhwW+3CqgQO2hwuKf77JWjOhpHTarXRD6YEnFG+PD6V/L
WBliuavLpnF0qEBuAAAAMQDn/57qbtnrMacXCgFuncBuzNdkBIoyg3VG5C33nn7W93Hg0L
EMefhS9IgLMVqEgZ0AAAARcDM4NC10ZXN0LWZpeHR1cmUBAgMEBQY=
-----END OPENSSH PRIVATE KEY-----
";

    /// An SSH `ecdsa-sha2-nistp256` signature becomes the JWS's 64-byte `r || s`.
    ///
    /// The first fixture is a real one - the signature OpenSSH put in an `SSHSIG`, with its `r` carrying the
    /// leading zero byte a signed mpint needs and its `s` not. The second is the other half of the rule: an
    /// `r` that came up short, which a JWS still wants at the full width of the field.
    #[test]
    fn es256_signature_is_raw_r_s() {
        // Two mpints: 33 bytes with a leading zero, then 32 without one.
        let ssh = blob(
            "00000021 00a552e35cd6ab3701561c5331111b5d316870c13464f63cd5dced04a4619b481a
             00000020 55dff5c6faf0150923dba3395fd9fb70e050d77fd484894f29535ec869bd0e21",
        );
        let raw = fixed_width_r_s(&ssh).expect("a P-256 signature");
        assert_eq!(
            raw,
            blob(
                "a552e35cd6ab3701561c5331111b5d316870c13464f63cd5dced04a4619b481a
                 55dff5c6faf0150923dba3395fd9fb70e050d77fd484894f29535ec869bd0e21"
            ),
            "the leading zero goes and the two scalars stay in order"
        );
        assert_eq!(raw.len(), P256_FIELD * 2);

        // A short `r`: five bytes of value, which becomes twenty-seven of padding plus those five.
        let ssh = blob(
            "00000005 0102030405
             00000020 55dff5c6faf0150923dba3395fd9fb70e050d77fd484894f29535ec869bd0e21",
        );
        let raw = fixed_width_r_s(&ssh).expect("a P-256 signature with a short r");
        let mut want = vec![0u8; P256_FIELD - 5];
        want.extend_from_slice(&blob("0102030405"));
        want.extend_from_slice(&blob(
            "55dff5c6faf0150923dba3395fd9fb70e050d77fd484894f29535ec869bd0e21",
        ));
        assert_eq!(raw, want, "the value sits right-aligned in its field");
    }

    /// What is not two P-256 scalars is not put in an assertion.
    ///
    /// Every one of these is a blob that could come back from something calling itself an ECDSA signature,
    /// and every one of them would otherwise be turned into 64 bytes of nonsense a gateway would reject -
    /// after this client had already spent an assertion and a round trip on it.
    #[test]
    fn a_signature_that_is_not_two_p256_scalars_is_refused() {
        let good = blob(
            "00000021 00a552e35cd6ab3701561c5331111b5d316870c13464f63cd5dced04a4619b481a
             00000020 55dff5c6faf0150923dba3395fd9fb70e050d77fd484894f29535ec869bd0e21",
        );
        assert!(
            fixed_width_r_s(&good).is_ok(),
            "the fixture is the good case"
        );

        // Nothing at all, one scalar only, and something trailing the two: none of them is a signature.
        assert!(fixed_width_r_s(&blob("")).is_err());
        assert!(fixed_width_r_s(&blob("00000021 00a552e35cd6ab3701561c5331111b5d")).is_err());
        let mut longer = good.clone();
        longer.push(0);
        assert!(fixed_width_r_s(&longer).is_err(), "somebody else's format");

        // A scalar wider than the field cannot be a P-256 one, and a negative mpint is not a scalar.
        let wide = blob(
            "00000021 7f00a552e35cd6ab3701561c5331111b5d316870c13464f63cd5dced04a4619b
             00000020 55dff5c6faf0150923dba3395fd9fb70e050d77fd484894f29535ec869bd0e21",
        );
        assert!(
            fixed_width_r_s(&wide).is_err(),
            "33 bytes of value is not a P-256 scalar"
        );
        let negative = blob(
            "00000001 80
             00000020 55dff5c6faf0150923dba3395fd9fb70e050d77fd484894f29535ec869bd0e21",
        );
        assert!(
            fixed_width_r_s(&negative).is_err(),
            "a signature does not sign with a negative r"
        );
    }

    /// A P-384 key file is refused by both login methods that read one, each with its own rule in the line.
    ///
    /// The curve parses - ssh-key reads all three NIST curves whatever features are turned on - so this is
    /// the client's own answer and not a decoder's complaint about a format it does not know.
    #[tokio::test]
    async fn a_p384_key_file_says_what_a_gateway_assertion_is_signed_with() {
        let dir = ScratchDir::new("p384_notice");
        let path = dir.path().join("id_ecdsa384");
        std::fs::write(&path, P384_KEY).expect("write the key file");

        let Err(problem) = Device::load(
            LoginMethod::GatewayKey,
            Some(&path),
            &Secret::KeyPassphrase(None),
        )
        .await
        else {
            panic!("a gateway assertion is not signed with P-384");
        };
        assert!(
            problem.contains("ecdsa-sha2-nistp384") && problem.contains("Ed25519 or P-256"),
            "the line names the key and says what would work: {problem}"
        );

        // The server's own device login keeps its narrower rule, and does not mention a curve it cannot use.
        let Err(problem) = Device::load(
            LoginMethod::KeyFile,
            Some(&path),
            &Secret::KeyPassphrase(None),
        )
        .await
        else {
            panic!("a featherbittorrent challenge is answered with Ed25519");
        };
        assert!(
            problem.contains("Ed25519") && !problem.contains("P-256"),
            "{problem}"
        );
    }
}
