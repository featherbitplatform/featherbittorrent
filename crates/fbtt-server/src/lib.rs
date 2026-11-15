//! fbtt-server: the headless server - who may use an engine over the network, and what they get.
//!
//! The API itself is fbt-api's: it serves the engine's torrents, events and file streams. This
//! crate owns everything featherbittorrent adds in front of it. [`auth`] holds the answers to "who
//! is this": Argon2id password hashes, Ed25519 device keys that answer a challenge, the session
//! tokens a login yields, and the limiter that stops a brute-force run. The server that carries an
//! engine, serves those routes and shuts both down cleanly comes next, in `server.rs`.

#![forbid(unsafe_code)]

pub mod auth;

pub use auth::{Clock, FbttAuth, LOGIN_NAMESPACE};
