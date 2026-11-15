//! The one thing both ends of a key login have to agree on: the namespace it signs in.
//!
//! A device logs in by signing a nonce the server sent, as an `sshsig` signature. The namespace is
//! what makes that signature mean "this person signed in here" and nothing else: the same Ed25519
//! key may have signed a file, a git tag or a message on some other service, and none of those
//! signatures should be spendable as a featherbittorrent login. Only the holder of the private key
//! can produce one, so the server checks it instead of asking for a password.
//!
//! It lives here rather than in the server crate because both ends need the same spelling: the
//! server verifies a login and the remote backend signs one, and the client side does not depend on
//! the server crate.

/// The `sshsig` namespace a featherbittorrent login signature is made in.
///
/// A signature over the challenge nonce in any other namespace is not a login, even from an
/// authorised key: it was made to mean something else. Both ends read this constant, so neither can
/// drift into a spelling the other refuses without noticing.
pub const LOGIN_NAMESPACE: &str = "featherbittorrent-login";
