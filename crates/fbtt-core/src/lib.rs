//! fbtt-core: config, platform paths, the library lock and the client
//! interface: everything featherbittorrent needs that is not a screen.
//!
//! The UI talks to one client interface: commands go in, immutable snapshots
//! come out, so a screen never awaits anything. This crate implements it over
//! the embedded engine; the remote implementation over HTTP arrives with
//! milestone 2.

#![forbid(unsafe_code)]

pub mod config;
pub mod paths;

pub use config::{Config, ConfigError, ConfigLoad};
pub use paths::Paths;

/// A directory under the system temp dir, removed when the test that made it
/// is done. The workspace takes no `tempfile` dependency, so tests make their
/// own scratch space.
#[cfg(test)]
mod scratch {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    pub(crate) struct ScratchDir {
        path: PathBuf,
    }

    impl ScratchDir {
        /// A new empty directory named after `tag`, unique on the machine.
        pub(crate) fn new(tag: &str) -> Self {
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

        pub(crate) fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
