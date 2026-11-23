//! This computer: the engine the desktop app runs for its own window.
//!
//! It is a `featherbittorrent serve` on the app's own library ([`DesktopPaths::library`]) with one
//! difference that matters: unless the person turned on Serve the web UI, it listens on `127.0.0.1` only,
//! on a port the OS picks, and the one credential it accepts is a secret made at this launch
//! ([`launch_secret`]). The secret never leaves process memory except as the answer to the page's
//! `connect` command, so a program elsewhere on the machine that finds the port has nothing to present.
//!
//! The library lock is taken first, before the config is read or written, exactly as `serve` does it: a
//! second app (or a `serve` pointed at the same directories) is refused before it changes anything.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use fbtt_core::config::ConfigError;
use fbtt_core::{Config, ConfigLoad, LibraryLock, LockError, Paths};
use fbtt_desktop_core::{DesktopPaths, launch_secret};
use fbtt_server::{DesktopAccess, Server};

/// What the app says, and the whole of what it says, when its library is already held by another run.
pub const ALREADY_RUNNING: &str = "featherbittorrent is already running.";

/// The exit code for [`ALREADY_RUNNING`]: the terminal client's, so a script tells "in use" apart from a
/// failure the same way for both programs.
pub const EXIT_ALREADY_RUNNING: i32 = 3;

/// The origin the app's webview sends with every request, which This computer adds to its CORS list.
///
/// Tauri serves the page from `tauri://localhost` on macOS and Linux, and from `http://tauri.localhost`
/// on Windows, where WebView2 cannot register a scheme of its own.
pub fn desktop_origin() -> &'static str {
    if cfg!(windows) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    }
}

/// The line shown when there is no home directory to put the app's files in.
///
/// [`ConfigError::NoHome`]'s own text names the terminal client's variables, which this app does not
/// read, so this names the two it does.
pub fn paths_problem(problem: &ConfigError) -> String {
    match problem {
        ConfigError::NoHome => {
            "featherbittorrent cannot find a home directory to keep its files in. \
             Set FBTT_DESKTOP_CONFIG_DIR and FBTT_DESKTOP_DATA_DIR to the folders it should use."
                .to_string()
        }
        other => other.to_string(),
    }
}

/// This computer while it runs: the server, and the secret the page presents to it.
pub struct ThisComputer {
    server: Server,
    secret: String,
    /// Whether this engine was started serving other machines (Serve the web UI), not the window alone.
    exposed: bool,
}

impl std::fmt::Debug for ThisComputer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the secret: a `Debug` printing ends up beside a panic message.
        f.debug_struct("ThisComputer")
            .field("addr", &self.server.local_addr())
            .finish_non_exhaustive()
    }
}

/// Starts This computer on the app's library: the lock, the config (written with this library's defaults
/// when there is none), then the server with a fresh launch secret and the webview's origin. `serve` is
/// the Serve the web UI switch: on, the library's `server:` section applies as it does for `serve`; off,
/// the engine is the window's alone.
///
/// The error is the line to show. A library another run holds is exactly [`ALREADY_RUNNING`].
pub async fn start(paths: &DesktopPaths, serve: bool) -> Result<ThisComputer, String> {
    start_with(paths, serve, Config::session_settings).await
}

/// [`start`] with the engine settings made from the config by `settings`, which is how a test keeps the
/// engine on loopback with no DHT and no multicast while everything else runs as it does for real.
pub(crate) async fn start_with(
    paths: &DesktopPaths,
    serve: bool,
    settings: impl FnOnce(&Config) -> fbt::Settings,
) -> Result<ThisComputer, String> {
    let library = paths.library();
    let lock = match LibraryLock::acquire(&library.lock_file()) {
        Ok(lock) => lock,
        Err(LockError::Held) => return Err(ALREADY_RUNNING.to_string()),
        Err(LockError::Io(problem)) => {
            return Err(format!(
                "Could not lock the library ({}): {problem}",
                library.data_dir.display()
            ));
        }
    };
    let config = library_config(&library)?;
    let secret = launch_secret();
    let access = DesktopAccess {
        launch_secret: secret.clone(),
        origins: vec![desktop_origin().to_string()],
        expose: serve,
    };
    let server = Server::start_desktop(settings(&config), config, &library, lock, access)
        .await
        .map_err(|problem| format!("This computer could not start: {problem}"))?;
    Ok(ThisComputer {
        server,
        secret,
        exposed: serve,
    })
}

/// The config This computer starts on: the library's file as read, this library's defaults written out
/// when there is none yet, and the defaults (with the file left where it is) when it cannot be read. A
/// problem worth knowing about that does not stop the start goes to stderr.
fn library_config(paths: &Paths) -> Result<Config, String> {
    match Config::load(paths) {
        Ok(ConfigLoad::Loaded(config)) => Ok(config.resolve(paths)),
        Ok(ConfigLoad::Missing) => {
            let config = Config::default_for(paths).resolve(paths);
            if let Err(problem) = config.save(&paths.config_file()) {
                eprintln!("{problem}");
            }
            Ok(config)
        }
        Ok(ConfigLoad::PlayerProblem { config, problem })
        | Ok(ConfigLoad::Bad { config, problem }) => {
            eprintln!("{problem}");
            Ok(config)
        }
        Err(problem) => Err(problem.to_string()),
    }
}

impl ThisComputer {
    /// Where the page reaches this engine, and the token it presents: `http://127.0.0.1:<port>` (no
    /// trailing slash) and the launch secret.
    ///
    /// An engine that serves other machines may listen on every address; the page still dials loopback,
    /// because an unspecified address is not one a client can connect to.
    pub fn endpoint(&self) -> (String, String) {
        let addr = self.server.local_addr();
        let ip = match addr.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
            ip => ip,
        };
        let base = format!("http://{}", SocketAddr::new(ip, addr.port()));
        (base, self.secret.clone())
    }

    /// The address the engine listens on, as it was bound: an unspecified address when it serves every
    /// interface. [`crate::serving`] turns that into the address other machines type.
    pub fn local_addr(&self) -> SocketAddr {
        self.server.local_addr()
    }

    /// Whether this engine serves other machines: started with Serve the web UI on.
    pub fn exposed(&self) -> bool {
        self.exposed
    }

    /// Stops the engine: the API closes, the last save runs, the library is released. A save that did not
    /// complete is said on stderr; there is nobody left to show it to.
    pub async fn stop(self) {
        if let Err(problem) = self.server.shutdown().await {
            eprintln!("The last save did not complete: {problem}");
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use fbtt_core::Config;
    use fbtt_core::scratch::ScratchDir;
    use fbtt_desktop_core::DesktopPaths;
    use reqwest::StatusCode;

    use super::{ALREADY_RUNNING, ThisComputer, paths_problem, start_with};

    /// How long a test waits for anything from the engine.
    const DEADLINE: Duration = Duration::from_secs(60);

    /// The engine settings every test runs on: nothing leaves loopback, and nothing asks a router.
    pub(crate) fn offline(config: &Config) -> fbt::Settings {
        let mut settings = config.session_settings();
        settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
        settings.dht_enabled = false;
        settings.enable_lsd = false;
        settings.enable_upnp = false;
        settings.enable_natpmp = false;
        settings
    }

    /// Desktop paths in `dir`, with a config already written that downloads into `dir` too, so no test
    /// reaches for the host's Downloads folder.
    pub(crate) fn scratch_paths(dir: &ScratchDir) -> DesktopPaths {
        let paths = DesktopPaths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        let library = paths.library();
        let config = Config {
            download_dir: dir.path().join("downloads"),
            ..Config::default()
        };
        config
            .save(&library.config_file())
            .expect("write the test config");
        paths
    }

    /// This computer on `paths`, kept off the network.
    pub(crate) async fn start_offline(paths: &DesktopPaths) -> Result<ThisComputer, String> {
        tokio::time::timeout(DEADLINE, start_with(paths, false, offline))
            .await
            .expect("This computer started within the deadline")
    }

    async fn status(request: reqwest::RequestBuilder) -> StatusCode {
        tokio::time::timeout(DEADLINE, request.send())
            .await
            .expect("an answer within the deadline")
            .expect("the request completed")
            .status()
    }

    /// The engine answers on loopback, opens for the launch secret and nothing else, and holds the library
    /// so a second start on the same directories is refused with the one line the app shows.
    #[tokio::test(flavor = "multi_thread")]
    async fn start_serves_loopback_with_a_fresh_secret() {
        let dir = ScratchDir::new("desktop-local-start");
        let paths = scratch_paths(&dir);
        let this = start_offline(&paths).await.expect("This computer starts");
        let (base, secret) = this.endpoint();
        assert!(
            base.starts_with("http://127.0.0.1:"),
            "a loopback base: {base}"
        );
        assert!(!base.ends_with('/'), "no trailing slash: {base}");
        assert!(secret.len() >= fbtt_server::MIN_LAUNCH_SECRET_CHARS);

        let client = reqwest::Client::new();
        let torrents = format!("{base}/api/v1/torrents");
        assert_eq!(
            status(client.get(&torrents).bearer_auth(&secret)).await,
            StatusCode::OK
        );
        assert_eq!(
            status(client.get(&torrents)).await,
            StatusCode::UNAUTHORIZED
        );

        let second = start_offline(&paths).await;
        assert_eq!(second.err().as_deref(), Some(ALREADY_RUNNING));

        // A new launch makes a new secret.
        this.stop().await;
        let again = start_offline(&paths)
            .await
            .expect("This computer starts again");
        assert_ne!(again.endpoint().1, secret);
        again.stop().await;
    }

    /// The desktop's own variables are what the no-home line tells a person to set.
    #[test]
    fn no_home_names_the_desktop_variables() {
        let line = paths_problem(&fbtt_core::config::ConfigError::NoHome);
        assert!(line.contains("FBTT_DESKTOP_CONFIG_DIR"), "{line}");
        assert!(line.contains("FBTT_DESKTOP_DATA_DIR"), "{line}");
    }
}
