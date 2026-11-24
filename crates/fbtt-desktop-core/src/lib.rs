//! fbtt-desktop-core: everything the desktop app does that is testable without a webview.
//!
//! The Tauri shell (the `featherbittorrent-desktop` crate) is thin on purpose: the window, the tray and
//! the plugins are its own, and every decision that can be made in plain Rust lives here - where the
//! app's files go ([`DesktopPaths`]), what the settings file says ([`DesktopSettings`], including the
//! saved remote [`Instance`]s), the secret this run's private server accepts ([`launch_secret`]), and
//! what a `magnet:` link or a `.torrent` file handed to the process actually is ([`opened_from_args`]).
//! Those are also the parts that must keep working when the file a person edited by hand names a key
//! this version does not know, or a default instance that no longer exists: nothing here rewrites a file
//! it could not read, and nothing here loses a key it does not understand.
//!
//! What the page shows and what the engine does stay where they already are: the web UI in `web/`, the
//! library, config and lock in `fbtt-core`. This crate hands the shell the paths to point them at.

#![forbid(unsafe_code)]

pub mod handoff;
pub mod instances;
pub mod paths;
pub mod secret;
pub mod settings;

pub use handoff::{Opened, opened_from_args};
pub use instances::{Instance, InstanceError, LOCAL_ID};
pub use paths::DesktopPaths;
pub use secret::launch_secret;
pub use settings::{CloseAction, DesktopSettings, ServeSettings, SettingsError, ShortcutOverride};
