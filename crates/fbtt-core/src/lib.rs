//! fbtt-core: config, platform paths, the library lock and the client
//! interface: everything featherbittorrent needs that is not a screen.
//!
//! The UI talks to one client interface: commands go in, immutable snapshots
//! come out, so a screen never awaits anything. This crate implements it over
//! the embedded engine; the remote implementation over HTTP arrives with
//! milestone 2.

#![forbid(unsafe_code)]
