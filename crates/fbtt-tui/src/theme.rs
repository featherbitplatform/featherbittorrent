//! The Graphite palette: every colour a screen may use, named after the design system's
//! tokens rather than re-derived at the call site.
//!
//! Ink is the only ground and violet is the only accent; green, amber, cyan and red are
//! state signals, not decoration. A view that wants a colour looks it up here, so the
//! palette can be changed in one file and every screen follows it.

use fbtt_core::RowState;
use ratatui::style::Color;

/// The canvas everything is drawn on: `#0b0b0e`.
pub const INK_950: Color = Color::Rgb(0x0b, 0x0b, 0x0e);
/// A surface lifted off the canvas, and the row that is selected: `#151518`.
pub const INK_850: Color = Color::Rgb(0x15, 0x15, 0x18);
/// Rules, borders, and the empty part of a progress bar: `#313135`.
pub const INK_600: Color = Color::Rgb(0x31, 0x31, 0x35);
/// Text that is there to be read second: labels, units, counts. `#919199`.
pub const INK_400: Color = Color::Rgb(0x91, 0x91, 0x99);
/// Body text: names, paths, addresses. `#b6b6bf`.
pub const INK_300: Color = Color::Rgb(0xb6, 0xb6, 0xbf);
/// Text that matters most, and the title: `#f5f5f7`.
pub const INK_50: Color = Color::Rgb(0xf5, 0xf5, 0xf7);
/// The accent: what is downloading, the selected-row marker, what a key does. `#863bff`.
pub const VIOLET_500: Color = Color::Rgb(0x86, 0x3b, 0xff);
/// The accent lifted for text on ink: an active tab, a key in the footer. `#a584ff`.
pub const VIOLET_400: Color = Color::Rgb(0xa5, 0x84, 0xff);
/// Seeding, and anything finished: `#2bbb71`.
pub const GREEN_500: Color = Color::Rgb(0x2b, 0xbb, 0x71);
/// Running with nothing arriving: `#f2a635`.
pub const AMBER_500: Color = Color::Rgb(0xf2, 0xa6, 0x35);
/// The connection to the engine or a server: `#59c5f5`.
pub const CYAN_400: Color = Color::Rgb(0x59, 0xc5, 0xf5);
/// Something failed: `#ed4a49`.
pub const RED_500: Color = Color::Rgb(0xed, 0x4a, 0x49);

/// The colour a torrent's state label and its bars wear.
///
/// Running work is the accent, finished is green, running-but-nothing-arriving is amber
/// and a failure is red. Everything stopped or waiting is muted, so the coloured rows in
/// the table are exactly the ones doing something.
pub fn state_colour(state: RowState) -> Color {
    match state {
        RowState::Downloading => VIOLET_500,
        RowState::Seeding => GREEN_500,
        RowState::Stalled => AMBER_500,
        RowState::Error => RED_500,
        RowState::Paused | RowState::Queued | RowState::Metadata | RowState::Checking(_) => INK_400,
    }
}
