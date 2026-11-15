//! fbtt-core: config, platform paths, the library lock, the engine and the client interface:
//! everything featherbittorrent needs that is not a screen.
//!
//! The UI talks to one client interface: commands go in, immutable snapshots come
//! out, so a screen never awaits anything. [`LocalBackend`] implements it over the
//! embedded engine; the remote implementation over HTTP arrives with milestone 2.
//!
//! The engine itself ([`Engine`]) is separate from either role on purpose: the terminal and the
//! headless server drive the same session by the same rules about the same files, so those rules -
//! and the library lock - are written once, here. Which role a run starts on is what
//! [`ui_state`] remembers. The one question both roles answer the same way - which folders may a
//! torrent go into ([`fs_list`]) - is written once for the same reason.

#![forbid(unsafe_code)]

pub mod auth;
pub mod client;
pub mod config;
pub mod engine;
pub mod fs_list;
pub mod local;
pub mod lock;
pub mod paths;
pub mod ui_state;

pub use auth::LOGIN_NAMESPACE;
pub use client::{
    AddSource, Client, ClientParts, Command, Commands, Connection, Detail, DetailTab,
    DeviceCommand, DeviceRow, DirListing, FilePriority, FileRow, LinkState, LiveLimits, Notice,
    PeerRow, RowState, Snapshot, TorrentId, TorrentRow, Totals, TrackerRow,
};
pub use config::{
    AuthConfig, Config, ConfigError, ConfigLoad, DeviceEntry, ServerConfig, UserEntry,
};
pub use engine::Engine;
pub use local::LocalBackend;
pub use lock::{LibraryLock, LockError};
pub use paths::Paths;
pub use ui_state::{LastUsed, LoginMethod, RemoteTarget, UiState};

/// A directory under the system temp dir, removed when the test that made it is done.
/// The workspace takes no `tempfile` dependency, so tests make their own scratch space.
///
/// Unit tests see this always; integration tests see it with the `test-support`
/// feature, which is what the backend's test target requires.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod scratch {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A scratch library: unique on the machine, and gone when the test ends.
    #[derive(Debug)]
    pub struct ScratchDir {
        path: PathBuf,
    }

    impl ScratchDir {
        /// A new empty directory named after `tag`, unique on the machine.
        pub fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let since_epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0);
            let name = format!(
                "featherbittorrent-test-{tag}-{since_epoch}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed),
            );
            let path = std::env::temp_dir().join(name);
            fs::create_dir_all(&path).expect("a scratch directory");
            ScratchDir { path }
        }

        /// The directory itself.
        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
