//! The device key this client signs with: where its private half lives, and both spellings of its public
//! half.
//!
//! Two spellings because two things ask for them: a server's login challenge wants the OpenSSH line, and
//! ssh-agent wants the wire blob. The agent path never reads the private bytes at all - it asks the agent
//! to sign, and matches identities by the blob its own key would produce.

use std::path::Path;

use signature::Signer as _;
use ssh_agent_lib::proto::{Identity, PublicCredential, SignRequest};
use ssh_key::encoding::Decode as _;
use ssh_key::{PrivateKey, Signature};

use crate::remote::Secret;
use crate::ui_state::LoginMethod;

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
    /// A file this process reads (and decrypts once, with the passphrase it was given).
    File(PrivateKey),
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
            let public = key.public_key();
            return Ok(Device {
                line: public.to_openssh().map_err(|problem| problem.to_string())?,
                public: public.key_data().clone(),
                source: Source::File(key),
            });
        }
        agent_device(key_path).await
    }

    /// Signs `data` with the private half, and hands back the raw signature bytes.
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
                let signature = client.sign(request).await.map_err(signing)?;
                // The agent's own `Signature` type is a different crate's version of ours; the bytes are
                // the same Ed25519 signature, and those are all this client needs.
                Ok(signature.as_bytes().to_vec())
            }
        }
    }
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
async fn agent_device(key_path: Option<&Path>) -> Result<Device, String> {
    let identities = identities().await?;
    // A `.pub` beside the private key says which of the agent's keys this connection means; without one,
    // the first Ed25519 key is the only answer that could log in anywhere here.
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
                    && identity.credential.key_data().is_ed25519()
            }
        })
        .ok_or_else(|| "ssh-agent holds no Ed25519 key this client can log in with".to_string())?;
    let credential = match &identity.credential {
        PublicCredential::Key(key) => key,
        // A certificate is not what a featherbittorrent server lists as a device.
        PublicCredential::Cert(_) => return Err("ssh-agent's key is a certificate".to_string()),
    };
    let public = ours(credential)?;
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
    client
        .request_identities()
        .await
        .map_err(|problem| format!("ssh-agent would not list its keys: {problem}"))
}

/// A connection to the agent this machine is running.
async fn connect_agent() -> Result<ssh_agent_lib::client::Client<AgentStream>, String> {
    #[cfg(unix)]
    {
        let path = std::env::var("SSH_AUTH_SOCK")
            .map_err(|_| "SSH_AUTH_SOCK is not set: ssh-agent is not running".to_string())?;
        let stream = tokio::net::UnixStream::connect(&path)
            .await
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
