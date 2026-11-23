//! fbtt-server: the headless server - who may use an engine over the network, and what they get.
//!
//! The API itself is fbt-api's: it serves the engine's torrents, events and file streams. This crate
//! owns everything featherbittorrent adds in front of it. [`auth`] holds the answers to "who is this":
//! Argon2id password hashes, Ed25519 device keys that answer a challenge, the session tokens a login
//! yields, and the limiter that stops a brute-force run. [`Server`] carries an engine: it takes the
//! library lock, starts the save task, serves fbt-api's routes plus these logins, devices and folder
//! browsing, and shuts both down in the order that leaves the library whole.

#![forbid(unsafe_code)]

pub mod auth;
mod media_io;
mod routes;
mod server;
// The front end this build carries, if it carries one: the page, its files, and the headers on them.
// Without the `web-ui` feature there is nothing to serve and no `web/dist` to read, so none of this is
// compiled at all (see `build.rs`).
#[cfg(feature = "web-ui")]
mod web;

pub use auth::{Clock, FbttAuth, LOGIN_NAMESPACE};
pub use server::{DesktopAccess, Server, ServerError};
