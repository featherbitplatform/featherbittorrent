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
use std::sync::Arc;

use fbtt_core::config::ConfigError;
use fbtt_core::{Config, ConfigLoad, LibraryLock, LockError, Paths, ServerConfig};
use fbtt_desktop_core::{DesktopPaths, launch_secret};
use fbtt_server::{DesktopAccess, ProxyPasswordStore, Server};

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

/// Why Serve the web UI did not start: the library's config has logins off, or no user to sign in as.
pub const LOGINS_OFF: &str = "Serve the web UI did not start: logins are off in the library's config.yaml, \
     or it has no user. Turn it on again with a user name and a password.";

/// Whether `config` has logins on and at least one user to sign in as: what serving to other machines
/// needs.
pub fn logins_on(config: &Config) -> bool {
    config
        .server
        .as_ref()
        .is_some_and(|server| server.auth_on() && !server.auth.users.is_empty())
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
/// The engine's settings are made from the config by `settings`: [`Config::settings_before_proxy`] for real,
/// and a test keeps the engine on loopback with no DHT and no multicast while everything else runs as it
/// does for real. The config `settings` is handed carries the proxy password the engine logs in with,
/// wherever it is kept. The app's own start goes through [`crate::serving::Serving::start_this_computer`].
///
/// The proxy password lives in `proxy_password`, the system keychain, when the system has one: a password
/// `config.yaml` still holds (written by hand, or by a run on a system with no keychain) is moved there
/// before the engine starts ([`move_password_to_keychain`]).
///
/// The error is the line to show. A library another run holds is exactly [`ALREADY_RUNNING`].
pub(crate) async fn start_with(
    paths: &DesktopPaths,
    serve: bool,
    proxy_password: Arc<dyn ProxyPasswordStore>,
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
    let mut config = library_config(&library, proxy_password.available())?;
    move_password_to_keychain(&mut config, &library, proxy_password.as_ref());
    let config = with_allowed_roots(config, &library);
    // The engine logs in with the keychain's password; `config` keeps saying what the file holds, which
    // is what the server reports from and writes back.
    let mut engine = config.clone();
    if let Some(proxy) = engine.proxy.as_mut()
        && proxy.password.as_deref().is_none_or(str::is_empty)
        && proxy_password.available()
    {
        proxy.password = proxy_password.get();
    }
    let secret = launch_secret();
    let access = DesktopAccess {
        launch_secret: secret.clone(),
        origins: vec![desktop_origin().to_string()],
        expose: serve,
        proxy_password: Some(proxy_password),
    };
    let server = Server::start_desktop(settings(&engine), config, &library, lock, access)
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
///
/// `keychained`: the system has a keychain, where the proxy password is kept, so the file's proxy login
/// is read without one ([`Config::load_keychained`]).
fn library_config(paths: &Paths, keychained: bool) -> Result<Config, String> {
    let loaded = if keychained {
        Config::load_keychained(paths)
    } else {
        Config::load(paths)
    };
    match loaded {
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

/// Moves a proxy password `config.yaml` holds into `store`, when the system has a keychain: the keychain
/// first, then the file without it, in one save. The config This computer starts on (`config`, as read)
/// stops holding the password only once the file does.
///
/// A keychain that will not take it leaves the file as it is, and the engine starts on the file's
/// password; a file that will not save after the keychain took it is left too, and the engine starts on
/// the keychain's (the same password), and the next start tries again. Either is one line on stderr,
/// which never carries the password.
fn move_password_to_keychain(config: &mut Config, paths: &Paths, store: &dyn ProxyPasswordStore) {
    let Some(password) = config
        .proxy
        .as_ref()
        .and_then(|proxy| proxy.password.clone())
        .filter(|password| !password.is_empty())
    else {
        return;
    };
    if !store.available() {
        return;
    }
    if let Err(problem) = store.set(&password) {
        eprintln!("The proxy password stays in config.yaml: {problem}");
        return;
    }
    let file = paths.config_file();
    let saved = match Config::load_for_edit_keychained(&file) {
        Ok(Some(mut on_disk)) => {
            if let Some(proxy) = on_disk.proxy.as_mut() {
                proxy.password = None;
            }
            on_disk.save(&file).map_err(|problem| problem.to_string())
        }
        Ok(None) => Ok(()),
        Err(problem) => Err(problem.to_string()),
    };
    match saved {
        Ok(()) => {
            if let Some(proxy) = config.proxy.as_mut() {
                proxy.password = None;
            }
        }
        Err(problem) => eprintln!(
            "The proxy password is in the keychain, but config.yaml still holds it: {problem}"
        ),
    }
}

/// `config` as the engine runs it: a library whose file names no `server:` section (the desktop's default)
/// gets one in memory, resolved the way `featherbittorrent serve` resolves an absent one, so the folders
/// the engine may name are the download folder rather than none at all (every add would be refused as
/// outside the allowed folders). Nothing is written: the file stays the person's own, with no section.
fn with_allowed_roots(mut config: Config, paths: &Paths) -> Config {
    if config.server.is_none() {
        config.server = Some(ServerConfig::default());
    }
    config.resolve(paths)
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

    /// The network settings the engine runs under right now. Only a test reads this.
    #[cfg(test)]
    pub(crate) fn network_settings(&self) -> fbt::NetworkSettings {
        self.server.network_settings()
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
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use fbtt_core::scratch::ScratchDir;
    use fbtt_core::{Config, ProxySetting, ProxyType};
    use fbtt_desktop_core::DesktopPaths;
    use fbtt_server::ProxyPasswordStore;
    use reqwest::StatusCode;

    use super::{ALREADY_RUNNING, ThisComputer, paths_problem, start_with};
    use crate::keychain::{Keychain, MemoryKeychain, PROXY_ACCOUNT, ProxyKeychain};

    /// How long a test waits for anything from the engine.
    const DEADLINE: Duration = Duration::from_secs(60);

    /// The engine settings every test runs on: nothing leaves loopback, and nothing asks a router.
    pub(crate) fn offline(config: &Config) -> fbt::Settings {
        // Before the proxy, as the app hands them: a proxy removed at runtime goes back to these.
        let mut settings = config.settings_before_proxy();
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
        start_keychained(paths, memory_store()).await
    }

    /// This computer on `paths`, kept off the network, with its proxy password in `store`.
    async fn start_keychained(
        paths: &DesktopPaths,
        store: Arc<dyn ProxyPasswordStore>,
    ) -> Result<ThisComputer, String> {
        tokio::time::timeout(DEADLINE, start_with(paths, false, store, offline))
            .await
            .expect("This computer started within the deadline")
    }

    /// A proxy password store over an empty keychain in memory.
    pub(crate) fn memory_store() -> Arc<dyn ProxyPasswordStore> {
        Arc::new(ProxyKeychain::new(Arc::new(MemoryKeychain::new())))
    }

    /// The proxy password the tests below store. A test value, not anybody's credential.
    const PROXY_PASSWORD: &str = "proxy-pass-hunter2";

    /// A port nothing listens on: the proxy the tests below name, which the engine never reaches.
    fn closed_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        drop(listener);
        port
    }

    /// Writes a `kind` proxy at `127.0.0.1:<closed port>` into the scratch library's config, logged in to
    /// as `bob` with `password` when there is one, keeping the download folder the scratch config named.
    fn write_proxy(paths: &DesktopPaths, kind: ProxyType, password: Option<&str>) {
        let file = paths.library().config_file();
        let mut config = Config::load_for_edit_keychained(&file)
            .expect("the scratch config reads")
            .expect("it is there");
        config.proxy = Some(ProxySetting {
            kind,
            host: "127.0.0.1".to_string(),
            port: closed_port(),
            username: password.map(|_| "bob".to_string()),
            password: password.map(str::to_string),
        });
        config.save(&file).expect("write the proxy");
    }

    fn file_text(paths: &DesktopPaths) -> String {
        std::fs::read_to_string(paths.library().config_file()).expect("the config file")
    }

    /// A password `config.yaml` holds moves into the keychain at the start, and out of the file in the
    /// same start; the engine logs in with it.
    #[tokio::test(flavor = "multi_thread")]
    async fn desktop_start_moves_a_file_password_into_the_keychain() {
        let dir = ScratchDir::new("desktop-local-move-password");
        let paths = scratch_paths(&dir);
        write_proxy(&paths, ProxyType::Socks5, Some(PROXY_PASSWORD));
        let keychain = Arc::new(MemoryKeychain::new());
        let store: Arc<dyn ProxyPasswordStore> = Arc::new(ProxyKeychain::new(keychain.clone()));

        let this = start_keychained(&paths, store)
            .await
            .expect("This computer starts");
        assert_eq!(
            keychain.get(PROXY_ACCOUNT).as_deref(),
            Some(PROXY_PASSWORD),
            "the keychain has it"
        );
        let written = file_text(&paths);
        assert!(!written.contains(PROXY_PASSWORD), "{written}");
        assert!(!written.contains("password:"), "{written}");
        assert!(written.contains("username: bob"), "{written}");
        assert_eq!(
            this.network_settings().proxy.password.as_deref(),
            Some(PROXY_PASSWORD)
        );
        this.stop().await;
    }

    /// The engine logs in with the keychain's password when the file holds none: the config the settings
    /// are made from carries it, and so does the engine.
    #[tokio::test(flavor = "multi_thread")]
    async fn desktop_start_uses_the_keychain_password() {
        let dir = ScratchDir::new("desktop-local-keychain-password");
        let paths = scratch_paths(&dir);
        write_proxy(&paths, ProxyType::Socks5, Some(PROXY_PASSWORD));
        let file = paths.library().config_file();
        let mut config = Config::load_for_edit(&file).expect("reads").expect("there");
        if let Some(proxy) = config.proxy.as_mut() {
            proxy.password = None;
        }
        config.save(&file).expect("the file without the password");
        let keychain = Arc::new(MemoryKeychain::new());
        keychain
            .set(PROXY_ACCOUNT, PROXY_PASSWORD)
            .expect("the keychain holds it");
        let store: Arc<dyn ProxyPasswordStore> = Arc::new(ProxyKeychain::new(keychain));

        let seen: Arc<Mutex<Option<String>>> = Arc::default();
        let seeing = Arc::clone(&seen);
        let this = tokio::time::timeout(
            DEADLINE,
            start_with(&paths, false, store, move |config: &Config| {
                *seeing.lock().expect("the lock") = config
                    .proxy
                    .as_ref()
                    .and_then(|proxy| proxy.password.clone());
                offline(config)
            }),
        )
        .await
        .expect("within the deadline")
        .expect("This computer starts");
        assert_eq!(
            seen.lock().expect("the lock").as_deref(),
            Some(PROXY_PASSWORD),
            "the settings are made from the keychain's password"
        );
        assert_eq!(
            this.network_settings().proxy.password.as_deref(),
            Some(PROXY_PASSWORD)
        );
        assert!(!file_text(&paths).contains(PROXY_PASSWORD));
        this.stop().await;
    }

    /// A keychain that will not take the password leaves the file as it is, and the engine starts on the
    /// file's password.
    #[tokio::test(flavor = "multi_thread")]
    async fn desktop_start_keeps_the_file_password_when_the_keychain_refuses() {
        let dir = ScratchDir::new("desktop-local-keychain-refuses");
        let paths = scratch_paths(&dir);
        write_proxy(&paths, ProxyType::Socks5, Some(PROXY_PASSWORD));
        let before = file_text(&paths);
        let keychain = Arc::new(MemoryKeychain::new());
        keychain.fail_writes(true);
        let store: Arc<dyn ProxyPasswordStore> = Arc::new(ProxyKeychain::new(keychain.clone()));

        let this = start_keychained(&paths, store)
            .await
            .expect("This computer starts");
        assert_eq!(file_text(&paths), before, "the file is as it was");
        assert_eq!(keychain.get(PROXY_ACCOUNT), None);
        assert_eq!(
            this.network_settings().proxy.password.as_deref(),
            Some(PROXY_PASSWORD)
        );
        this.stop().await;
    }

    /// A file that will not save after the keychain took the password is left as it is (the next start
    /// tries again), and the engine starts on the keychain's password.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn desktop_start_keeps_the_keychain_password_when_the_file_will_not_save() {
        use std::os::unix::fs::PermissionsExt;

        let dir = ScratchDir::new("desktop-local-file-refuses");
        let paths = scratch_paths(&dir);
        write_proxy(&paths, ProxyType::Socks5, Some(PROXY_PASSWORD));
        let before = file_text(&paths);
        let folder = paths.library().config_dir.clone();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500))
            .expect("make the config folder read-only");
        let keychain = Arc::new(MemoryKeychain::new());
        let store: Arc<dyn ProxyPasswordStore> = Arc::new(ProxyKeychain::new(keychain.clone()));

        let started = start_keychained(&paths, store).await;
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))
            .expect("make it writable again");
        let this = started.expect("This computer starts");
        assert_eq!(file_text(&paths), before, "the file is left");
        assert_eq!(keychain.get(PROXY_ACCOUNT).as_deref(), Some(PROXY_PASSWORD));
        assert_eq!(
            this.network_settings().proxy.password.as_deref(),
            Some(PROXY_PASSWORD)
        );
        this.stop().await;
    }

    /// On a system with no keychain the password stays in `config.yaml`, and the engine logs in with it.
    #[tokio::test(flavor = "multi_thread")]
    async fn desktop_start_without_keychain_keeps_the_file_password() {
        let dir = ScratchDir::new("desktop-local-no-keychain");
        let paths = scratch_paths(&dir);
        write_proxy(&paths, ProxyType::Socks5, Some(PROXY_PASSWORD));
        let before = file_text(&paths);
        let store: Arc<dyn ProxyPasswordStore> =
            Arc::new(ProxyKeychain::new(Arc::new(MemoryKeychain::unavailable())));

        let this = start_keychained(&paths, store)
            .await
            .expect("This computer starts");
        assert_eq!(file_text(&paths), before, "the file keeps it");
        assert_eq!(
            this.network_settings().proxy.password.as_deref(),
            Some(PROXY_PASSWORD)
        );
        this.stop().await;
    }

    /// Removing the proxy at runtime over This computer's own route brings back the switches it turned
    /// off: the engine is handed its settings before the proxy, as the app's own start hands them.
    #[tokio::test(flavor = "multi_thread")]
    async fn removing_the_proxy_at_runtime_restores_the_switches() {
        let dir = ScratchDir::new("desktop-local-proxy-removed");
        let paths = scratch_paths(&dir);
        write_proxy(&paths, ProxyType::Socks5, Some(PROXY_PASSWORD));
        let this = start_offline(&paths).await.expect("This computer starts");
        let proxied = this.network_settings();
        assert_eq!(proxied.proxy.kind, fbt::ProxyKind::Socks5);
        assert!(
            !proxied.enable_incoming_tcp,
            "the proxy turned incoming off"
        );

        let (base, secret) = this.endpoint();
        let answer = tokio::time::timeout(
            DEADLINE,
            reqwest::Client::new()
                .put(format!("{base}/api/v1/settings/proxy"))
                .bearer_auth(&secret)
                .json(&serde_json::json!({
                    "type": "none", "host": "", "port": 0, "username": null, "password": "keep"
                }))
                .send(),
        )
        .await
        .expect("an answer within the deadline")
        .expect("the request completed");
        let status = answer.status();
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            answer.text().await.unwrap_or_default()
        );

        let running = this.network_settings();
        let config = Config::load_for_edit(&paths.library().config_file())
            .expect("reads")
            .expect("there");
        assert!(config.proxy.is_none(), "the file names no proxy");
        assert_eq!(running.proxy.kind, fbt::ProxyKind::None);
        assert!(running.enable_incoming_tcp, "incoming connections are back");
        assert_eq!(
            running,
            offline(&config.resolve(&paths.library())).network(),
            "the switches are what This computer had before its proxy"
        );
        this.stop().await;
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

    /// Desktop paths in `dir` with nothing written: a library the app has never run on.
    fn fresh_paths(dir: &ScratchDir) -> DesktopPaths {
        DesktopPaths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        }
    }

    /// The first run on a library with no config.yaml: the config it writes names no server, and the
    /// engine's copy has the download folder as its one allowed root, as `serve` would have it. Nothing
    /// starts and nothing is created but the scratch config file (the download folder may be the host's,
    /// so it is only named, never made).
    #[test]
    fn a_fresh_library_gets_the_download_folder_as_its_root() {
        let dir = ScratchDir::new("desktop-local-fresh-roots-unit");
        let paths = fresh_paths(&dir);
        let library = paths.library();
        assert!(!library.config_file().exists(), "a fresh library");

        let config = super::library_config(&library, false).expect("the defaults");
        let written = Config::load_for_edit(&library.config_file())
            .expect("the config reads")
            .expect("the first run wrote one");
        assert!(written.server.is_none(), "no server section was written");

        let download_dir = config.download_dir.clone();
        let engine = super::with_allowed_roots(config, &library);
        assert_eq!(
            engine.server.expect("a section in memory").allowed_roots,
            vec![download_dir]
        );
        let after = Config::load_for_edit(&library.config_file())
            .expect("the config reads")
            .expect("still there");
        assert!(after.server.is_none(), "still no server section on disk");
    }

    /// A config.yaml whose `proxy:` cannot be used is the startup problem: This computer does not start an
    /// engine that would connect directly, and the line the app shows is the proxy refusal.
    #[test]
    fn an_unusable_proxy_is_the_startup_problem() {
        let dir = ScratchDir::new("desktop-local-bad-proxy");
        let paths = scratch_paths(&dir);
        let library = paths.library();
        let file = library.config_file();
        std::fs::write(
            &file,
            "proxy:\n  type: socks4\n  host: proxy.example\n  port: 1080\n",
        )
        .expect("write a config that names a proxy");

        assert_eq!(
            super::library_config(&library, false).err().as_deref(),
            Some("Unknown proxy type \"socks4\"; use none, http or socks5.")
        );
        assert!(file.exists(), "the file is left in place");
        assert!(
            !library.config_file().with_extension("yaml.bad").exists(),
            "nothing was set aside"
        );
    }

    /// A library whose config.yaml names no `server:` section (the desktop's default) can add a magnet
    /// into its download folder: the engine's allowed folders are that folder.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_magnet_add_on_a_fresh_library_succeeds() {
        let dir = ScratchDir::new("desktop-local-fresh-add");
        let paths = scratch_paths(&dir);
        let library = paths.library();
        let written = Config::load_for_edit(&library.config_file())
            .expect("the config reads")
            .expect("written by the scratch library");
        assert!(written.server.is_none(), "no server section");
        let downloads = written.resolve(&library).download_dir;
        assert!(
            downloads.starts_with(dir.path()),
            "a scratch download folder"
        );
        let this = start_offline(&paths).await.expect("This computer starts");
        let (base, secret) = this.endpoint();

        let body = serde_json::json!({
            "magnet": format!("magnet:?xt=urn:btih:{}", "cd".repeat(20)),
            "save_path": downloads.to_string_lossy(),
        });
        let added = tokio::time::timeout(
            DEADLINE,
            reqwest::Client::new()
                .post(format!("{base}/api/v1/torrents"))
                .bearer_auth(&secret)
                .header("content-type", "application/json")
                .body(body.to_string())
                .send(),
        )
        .await
        .expect("an answer within the deadline")
        .expect("the request completed");
        let status = added.status();
        assert!(
            status.is_success(),
            "{status}: {}",
            added.text().await.unwrap_or_default()
        );
        this.stop().await;
    }

    /// The folder picker on a library with no `server:` section has somewhere to start: its roots list
    /// is not empty.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_folder_roots_are_not_empty_on_a_fresh_library() {
        let dir = ScratchDir::new("desktop-local-fresh-roots");
        let paths = scratch_paths(&dir);
        let no_section = Config::load_for_edit(&paths.library().config_file())
            .expect("the config reads")
            .expect("written by the scratch library");
        assert!(no_section.server.is_none(), "no server section");
        let this = start_offline(&paths).await.expect("This computer starts");
        let (base, secret) = this.endpoint();
        let answer = tokio::time::timeout(
            DEADLINE,
            reqwest::Client::new()
                .get(format!("{base}/api/v1/fs/roots"))
                .bearer_auth(&secret)
                .send(),
        )
        .await
        .expect("an answer within the deadline")
        .expect("the request completed");
        assert_eq!(answer.status(), StatusCode::OK);
        let roots: Vec<String> =
            serde_json::from_str(&answer.text().await.expect("a body")).expect("a list of roots");
        assert!(!roots.is_empty(), "the download folder is a root");
        this.stop().await;
    }

    /// The desktop's own variables are what the no-home line tells a person to set.
    #[test]
    fn no_home_names_the_desktop_variables() {
        let line = paths_problem(&fbtt_core::config::ConfigError::NoHome);
        assert!(line.contains("FBTT_DESKTOP_CONFIG_DIR"), "{line}");
        assert!(line.contains("FBTT_DESKTOP_DATA_DIR"), "{line}");
    }
}
