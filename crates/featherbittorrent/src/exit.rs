//! A problem to show the user, plus the exit code it deserves.
//!
//! The codes are part of the contract: 0 success, 1 a problem, clap's own 2 for
//! a usage error, and 3 when the library is already running somewhere else.

/// A message for stderr and the status to exit with.
pub struct Failure {
    /// The process exit code.
    pub code: i32,
    /// What to print on stderr.
    pub message: String,
}

impl Failure {
    /// A problem worth exit 1.
    pub fn problem(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }

    /// The library is already running somewhere else: worth exit 3.
    pub fn in_use(message: impl Into<String>) -> Self {
        Self {
            code: 3,
            message: message.into(),
        }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::problem(message)
    }
}

impl From<std::io::Error> for Failure {
    fn from(problem: std::io::Error) -> Self {
        Self::problem(problem.to_string())
    }
}

impl From<fbtt_core::ConfigError> for Failure {
    fn from(problem: fbtt_core::ConfigError) -> Self {
        Self::problem(problem.to_string())
    }
}

impl From<fbtt_server::auth::keys::KeyError> for Failure {
    fn from(problem: fbtt_server::auth::keys::KeyError) -> Self {
        Self::problem(problem.to_string())
    }
}
