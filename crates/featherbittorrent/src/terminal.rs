//! The terminal's setup and teardown, as one guard.
//!
//! `ratatui::init` enables raw mode, enters the alternate screen and installs a panic
//! hook that restores the terminal before running the hook it replaced; this module adds
//! only the guarantee that the same restore happens on an ordinary return from the run.

use ratatui::{DefaultTerminal, restore};

/// The terminal, restored when this guard goes out of scope: on a clean quit, on an
/// error return, and on a panic through the hook `ratatui::init` installed.
#[derive(Debug)]
pub struct Terminal(DefaultTerminal);

impl Terminal {
    /// Enters raw mode and the alternate screen, with ratatui's restoring panic hook.
    ///
    /// A terminal that cannot be entered - no tty, a closed stdout - is an error here
    /// rather than a panic, because every caller prints its own one-line answer.
    pub fn setup() -> Result<Terminal, std::io::Error> {
        // A failure partway in - raw mode in, alternate screen refused - leaves the tty
        // half changed: put back what did go in before handing the error out, because
        // every caller answers with a line printed to that tty.
        match ratatui::try_init() {
            Ok(terminal) => Ok(Terminal(terminal)),
            Err(error) => {
                ratatui::restore();
                Err(error)
            }
        }
    }

    /// The terminal to draw into.
    pub fn get_mut(&mut self) -> &mut DefaultTerminal {
        &mut self.0
    }
}

impl Drop for Terminal {
    /// Leaves raw mode and the alternate screen. ratatui's own restore prints what it
    /// cannot fix itself; there is nothing left for us to do about that.
    fn drop(&mut self) {
        restore();
    }
}
