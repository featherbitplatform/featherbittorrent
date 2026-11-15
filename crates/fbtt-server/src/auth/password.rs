//! Password hashes: what the config keeps instead of a password.
//!
//! Argon2id, written as a PHC string (`$argon2id$v=19$m=...,t=...,p=...$salt$hash`), which is what
//! [`fbtt_core::UserEntry`] stores and what `featherbittorrent user set` writes. The parameters come
//! from the crate's own default, so they move when the library's advice moves instead of being
//! frozen in a config file: every hash says in itself how it was made, so an old hash still verifies
//! after this crate changes its mind.

use argon2::{Argon2, PasswordHash, PasswordHasher as _, PasswordVerifier as _};

/// The hash of `password`, with a fresh random salt and [`Argon2`]'s default parameters.
///
/// Two calls on the same password give different strings: the salt is what makes two accounts with
/// the same password hash differently.
///
/// # Panics
/// If the operating system has no working random source, which on every platform this project runs
/// on it always does.
pub fn hash(password: &str) -> String {
    Argon2::default()
        .hash_password(password.as_bytes())
        .expect("a random salt, and a password argon2 can hash")
        .to_string()
}

/// Whether `password` is the password behind `phc`, a hash as [`hash`] writes it.
///
/// The comparison is constant time, and a string that is not a hash at all - truncated, hand-edited,
/// from another algorithm this crate does not know - answers `false` rather than an error: every
/// caller here is answering "is this the password?", and one that cannot be parsed is not.
pub fn verify(phc: &str, password: &str) -> bool {
    match PasswordHash::new(phc) {
        // The parameters come out of the hash itself, so a hash made with older or stricter settings
        // still verifies, and `Argon2::default()` here only says which algorithm to run.
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{hash, verify};

    /// The right password verifies against its hash; a wrong one and a string that is not a hash do
    /// not.
    #[test]
    fn hash_and_verify() {
        let phc = hash("hunter42");
        assert!(phc.starts_with("$argon2id$"), "not argon2id: {phc}");
        assert!(verify(&phc, "hunter42"));
        assert!(
            !verify(&phc, "hunter43"),
            "one character off is not the password"
        );
        assert!(!verify("not a phc string", "hunter42"));
        // A hash with no salt and no digest in it cannot match anything.
        assert!(!verify("$argon2id$v=19$m=65536,t=2,p=1", "hunter42"));
        // The salt is random, so the same password hashes differently every time.
        assert_ne!(hash("hunter42"), hash("hunter42"));
    }
}
