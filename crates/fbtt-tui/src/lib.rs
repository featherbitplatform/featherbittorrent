//! fbtt-tui: the terminal UI: a pure model (keys in, actions out) plus its
//! ratatui rendering. The model speaks the core crate's client interface, so
//! every screen works the same over the embedded engine and over a server.

#![forbid(unsafe_code)]
