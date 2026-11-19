//! The logo's graphics core: what a terminal proved it can draw, how the mark is prepared for
//! it, and the three ways an image is sent to a terminal.
//!
//! Everything here is pure. Bytes go in, bytes come out; no terminal, no clock, no thread. That
//! is deliberate: the probe (Task 8) owns the only read from the tty, the placer (Task 9) owns
//! the only write to it, and what sits between them - deciding a protocol, sizing a bitmap,
//! encoding it - is a table of functions that can be tested without a terminal at all.
//!
//! Two rules from the plan hold the whole module together. A protocol is chosen only on an answer
//! the terminal actually gave ([`caps::choose`], decision 10), and no cell pixel size means no
//! logo: an image the terminal has to scale itself is the blurry mark the owner ruled out
//! (decision 11). The image is therefore built at exactly the device pixel size of the cells it
//! will be placed over.

pub mod caps;
pub mod image;
pub mod iterm2;
pub mod kitty;
pub mod sixel;

pub use caps::{
    CellPx, Graphics, PROBE_QUERY, Protocol, Replies, choose, da1_complete, parse_replies,
};
pub use image::{LogoDims, Rgba, logo_dims, mark, render};
