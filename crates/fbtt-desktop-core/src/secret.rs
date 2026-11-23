//! The secret one run of the desktop app accepts, and nothing else.
//!
//! This computer's server binds to `127.0.0.1` on a port anybody on the machine can reach, so the thing
//! between a stranger's program and this library is this string: the shell hands it to the page when the
//! window is created, the page sends it as a bearer token, and every other credential on the port is
//! refused. It is made once per launch and lives in process memory only - never in the settings file,
//! never in browser storage (design spec section 2.3 and its anti-goals), so a secret that leaked from a
//! running app stops being one the moment the app is restarted.

use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;

/// The bytes one launch's secret is made of. Thirty-two makes 43 url-safe characters with no padding:
/// long enough that guessing one is not a plan, short enough to sit in a header, a query string and an
/// initialization script without escaping. The session tokens the server issues are the same size, spelled
/// the same way, for the same reason.
const SECRET_BYTES: usize = 32;

/// The bearer token this run's private server accepts, made once at launch.
///
/// The shell hands it to the page when the window is created and the page sends it as
/// `Authorization: Bearer` on every request to This computer; the server compares it in constant time, as
/// it does its other credentials. Call it once per run and keep the result in memory: a secret written
/// into the settings file would outlive the run it protects, which is what the design's anti-goals are
/// about.
pub fn launch_secret() -> String {
    let mut bytes = [0u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).expect("the platform has a random source");
    BASE64_URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::launch_secret;

    /// 32 bytes from the operating system's random source, spelled the way the other credentials in this
    /// product are spelled: base64url with no padding, so the value fits in a header, a query string and a
    /// URL without escaping, and long enough that guessing one is not an option.
    #[test]
    fn secret_is_43_chars_and_differs_each_call() {
        let first = launch_secret();
        let second = launch_secret();
        assert_eq!(first.len(), 43, "32 bytes in base64url with no padding");
        assert_ne!(first, second, "two launches do not share a secret");
        assert!(
            !first.contains('+') && !first.contains('/') && !first.contains('='),
            "url-safe and unpadded: {first}"
        );
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{first}"
        );
    }
}
