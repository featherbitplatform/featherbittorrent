//! fbtt-tui: the terminal UI: a pure model (keys in, actions out) plus its
//! ratatui rendering. The model speaks the core crate's client interface, so
//! every screen works the same over the embedded engine and over a server.
//!
//! What a binary needs is all at the crate root: a [`Model`] that answers key presses with
//! commands, [`draw`] to put a model and a snapshot on the screen, and the palette in
//! [`theme`] for anything drawn on top of them.

#![forbid(unsafe_code)]

pub mod format;
mod keys;
pub mod model;
pub mod theme;
pub mod view;

pub use model::{Descending, Modal, Model, SortKey, View, visible_rows};
pub use view::draw;
