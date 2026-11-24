//! Serve the web UI: This computer's library, served to other machines as `featherbittorrent serve`
//! serves it.
//!
//! The switch is `serve.enabled` in `desktop.yaml`; everything else about serving (the address to listen
//! on, the logins) is the library's own `config.yaml` `server:` section, which is what `serve` reads, so
//! one file says how this machine serves whichever program does it. Turning serving on asks for a user
//! name and a password: the password is hashed with the server's own function and written to
//! `server.auth.users` (the same entry `featherbittorrent user set` writes), and logins are switched on.
//! Applying restarts This computer through [`Lifecycle::restart_this_computer`], so a quit during the
//! restart still saves; the page then connects to `local` again, because a new launch has a new secret.
//!
//! The app's first start of This computer goes through here too ([`Serving::start_this_computer`]), so a
//! saved switch that is on starts the engine serving, the same as an apply does; and an engine that will
//! not start serving falls back to the window alone the same way, with the line kept for the Serving
//! section ([`ServingView::problem`]) rather than shown as a start that could not run at all.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};

use fbtt_core::{Config, LibraryLock, LockError, ServerConfig, UserEntry};
use fbtt_desktop_core::DesktopPaths;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::commands::DesktopState;
use crate::lifecycle::Lifecycle;
use crate::local::{self, ThisComputer};
use crate::settings_store::SettingsStore;

/// Where serving listens when the library's config has never chosen: every interface, port 8090.
pub const DEFAULT_BIND: &str = "0.0.0.0:8090";

/// The longest user name the server's own list takes, as `featherbittorrent user set` checks it.
const MAX_NAME_CHARS: usize = 64;

/// What the page shows when turning serving on has no password to go with it.
pub const PASSWORD_NEEDED: &str = "Serving the web UI needs a password.";

/// What the page shows when turning serving on has no user name.
pub const USER_NEEDED: &str = "Serving the web UI needs a user name.";

/// What the page asks for: the switch, where to listen, and the login to create or replace.
#[derive(Clone, Deserialize)]
pub struct ServingRequest {
    pub enabled: bool,
    #[serde(default)]
    pub bind: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub password: String,
}

impl std::fmt::Debug for ServingRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the password.
        f.debug_struct("ServingRequest")
            .field("enabled", &self.enabled)
            .field("bind", &self.bind)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// What the Serving section shows: the switch as saved, the address and login the config names, and
/// where the web UI is reachable now (none while serving is off or This computer is not running).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServingView {
    pub enabled: bool,
    pub bind: String,
    pub user: String,
    pub address: Option<String>,
    /// Why serving is off although it was asked for at this launch: the saved switch was on and the
    /// engine would not start serving (the port is taken, say), so it started for the window alone.
    pub problem: Option<String>,
}

/// What the serving commands keep: where the library is and the shell's one settings store.
#[derive(Debug, Clone)]
pub struct Serving {
    paths: DesktopPaths,
    settings: SettingsStore,
    /// The line from a first start that could not serve, until an apply succeeds.
    problem: Arc<Mutex<Option<String>>>,
}

/// A request that passed its checks, with the password already hashed.
struct Change {
    enabled: bool,
    bind: SocketAddr,
    user: String,
    /// `None` keeps the hash the config already has for `user`.
    password_hash: Option<String>,
}

impl Serving {
    /// The serving state for the app's library in `paths`, saving its switch through `settings`.
    pub fn new(paths: DesktopPaths, settings: SettingsStore) -> Self {
        Serving {
            paths,
            settings,
            problem: Arc::new(Mutex::new(None)),
        }
    }

    /// This computer's first start of the run, serving when `serve` (the saved switch) is on. An engine
    /// that will not start serving is started for the window alone, the switch is saved off and the line
    /// kept for the Serving section, exactly as [`apply`](Self::apply) does it; only a start that cannot
    /// run at all (the library held by another run, a second failure) is the answer, which the page then
    /// shows as the startup problem. A saved switch over a config with logins off, or with no user, is
    /// started for the window alone the same way ([`local::LOGINS_OFF`]). `settings` makes the engine's
    /// settings from the config.
    pub async fn start_this_computer<S>(
        &self,
        serve: bool,
        settings: S,
    ) -> Result<ThisComputer, String>
    where
        S: Fn(&Config) -> fbt::Settings + Clone,
    {
        // Serving to other machines needs logins: the switch asks for a user name and a password, and a
        // config edited since to turn logins off (or to drop every user) would serve the page to the
        // network with only the Host guard. Such a start serves nothing: the window gets its engine
        // alone, and the Serving section the line.
        let problem = if serve
            && !self
                .config()
                .is_some_and(|config| local::logins_on(&config))
        {
            local::LOGINS_OFF.to_string()
        } else {
            match local::start_with(&self.paths, serve, settings.clone()).await {
                Ok(running) => return Ok(running),
                Err(problem) if !serve || problem == local::ALREADY_RUNNING => return Err(problem),
                Err(problem) => problem,
            }
        };
        eprintln!("{problem}");
        let running = local::start_with(&self.paths, false, settings).await?;
        if let Err(saving) = self.save_switch(false) {
            eprintln!("Serving is off, but the switch could not be saved: {saving}");
        }
        self.set_problem(Some(problem));
        Ok(running)
    }

    fn set_problem(&self, problem: Option<String>) {
        *self
            .problem
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = problem;
    }

    fn problem(&self) -> Option<String> {
        self.problem
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The library's config as it is on disk now; `None` when there is none yet or it cannot be read.
    fn config(&self) -> Option<Config> {
        Config::load_for_edit(&self.paths.library().config_file())
            .ok()
            .flatten()
    }

    /// What the Serving section shows now. `serving` is the address This computer serves other machines
    /// on ([`DesktopState::serving_addr`]: none for a window-only engine, whatever the switch says), and
    /// `user` the login just applied (the config's first login when none was).
    pub fn view(&self, serving: Option<SocketAddr>, user: Option<&str>) -> ServingView {
        let enabled = self
            .settings
            .read()
            .map(|settings| settings.serve.enabled)
            .unwrap_or(false);
        let server = self.config().and_then(|config| config.server);
        let bind = server.as_ref().map_or_else(
            || DEFAULT_BIND.to_string(),
            |server| server.bind.to_string(),
        );
        let user = user.map(str::to_string).unwrap_or_else(|| {
            server
                .as_ref()
                .and_then(|server| server.auth.users.first())
                .map(|user| user.name.clone())
                .unwrap_or_default()
        });
        let address = serving.filter(|_| enabled).map(reachable_at);
        ServingView {
            enabled,
            bind,
            user,
            address,
            problem: self.problem(),
        }
    }

    /// Checks `request` before anything is stopped: a bind that parses, a user name, and a password
    /// unless the config already has one for that user. The password is hashed here, on the blocking
    /// pool, so the engine is down for no longer than the restart itself.
    async fn check(&self, mut request: ServingRequest) -> Result<Change, String> {
        if !request.enabled {
            return Ok(Change {
                enabled: false,
                bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
                user: String::new(),
                password_hash: None,
            });
        }
        let bind_text = request.bind.trim();
        let bind_text = if bind_text.is_empty() {
            DEFAULT_BIND
        } else {
            bind_text
        };
        let bind: SocketAddr = match bind_text.parse() {
            Ok(bind) => bind,
            Err(_) => {
                let line = format!(
                    "{} is not an address and port, like {DEFAULT_BIND}.",
                    fbt::display_safe(bind_text, fbt::MAX_DISPLAY_CHARS)
                );
                request.password.zeroize();
                return Err(line);
            }
        };
        let user = request.user.trim().to_string();
        if let Some(problem) = name_problem(&user) {
            request.password.zeroize();
            return Err(problem);
        }
        let password_hash = if request.password.is_empty() {
            let known = self
                .config()
                .and_then(|config| config.server)
                .is_some_and(|server| server.auth.users.iter().any(|entry| entry.name == user));
            if !known {
                return Err(PASSWORD_NEEDED.to_string());
            }
            None
        } else {
            // Overwritten when dropped, once it is hashed (or the hashing failed).
            let password = Zeroizing::new(request.password);
            let hashed =
                tokio::task::spawn_blocking(move || fbtt_server::auth::password::hash(&password))
                    .await
                    .map_err(|problem| format!("The password could not be hashed: {problem}"))?;
            Some(hashed)
        };
        Ok(Change {
            enabled: true,
            bind,
            user,
            password_hash,
        })
    }

    /// Writes `change` into the library's config: the bind, logins on, and the user. Holds the library
    /// lock while it does, as `featherbittorrent user set` does, so no other program edits it at once.
    fn write_config(&self, change: &Change) -> Result<(), String> {
        let library = self.paths.library();
        let lock = match LibraryLock::acquire(&library.lock_file()) {
            Ok(lock) => lock,
            Err(LockError::Held) => return Err(local::ALREADY_RUNNING.to_string()),
            Err(LockError::Io(problem)) => {
                return Err(format!(
                    "Could not lock the library ({}): {problem}",
                    library.data_dir.display()
                ));
            }
        };
        let mut config = Config::load_for_edit(&library.config_file())?
            .unwrap_or_else(|| Config::default_for(&library));
        let server = config.server.get_or_insert_with(ServerConfig::default);
        server.bind = change.bind;
        // A password was asked for, so it is what lets people in: logins on whatever the bind.
        server.auth.enabled = Some(true);
        if let Some(hash) = &change.password_hash {
            let entry = UserEntry {
                name: change.user.clone(),
                password_hash: hash.clone(),
            };
            match server
                .auth
                .users
                .iter_mut()
                .find(|user| user.name == change.user)
            {
                Some(existing) => *existing = entry,
                None => server.auth.users.push(entry),
            }
        }
        config
            .save(&library.config_file())
            .map_err(|problem| problem.to_string())?;
        drop(lock);
        Ok(())
    }

    /// Applies `request`: checks it, restarts This computer with serving on or off (the config written
    /// in between), and saves the switch once the engine started the way it asked. An engine that will
    /// not start serving (the port is taken, say) is started again for the window alone, the switch is
    /// saved off, and the answer is the line saying why. `settings` makes the engine's settings from the
    /// config, as [`local::start_with`] takes them.
    pub async fn apply<S>(
        &self,
        lifecycle: &Lifecycle,
        state: &DesktopState,
        request: ServingRequest,
        settings: S,
    ) -> Result<ServingView, String>
    where
        S: Fn(&Config) -> fbt::Settings + Clone,
    {
        let change = self.check(request).await?;
        let user = change.enabled.then(|| change.user.clone());
        if self.unchanged(&change, state.serving_addr().is_some()) {
            return Ok(self.view(state.serving_addr(), user.as_deref()));
        }
        let restarted = lifecycle
            .restart_this_computer(state, || self.restart(change, settings))
            .await;
        // Whatever the first start said is old news once serving was applied again.
        self.set_problem(None);
        restarted?;
        Ok(self.view(state.serving_addr(), user.as_deref()))
    }

    /// Whether applying `change` would leave everything as it is: off while the switch is off and the
    /// engine serves nobody, or on with the bind the config has, logins on, the same user and no new
    /// password while the engine already serves. Nothing is restarted for that.
    fn unchanged(&self, change: &Change, engine_serves: bool) -> bool {
        let saved = self
            .settings
            .read()
            .map(|settings| settings.serve.enabled)
            .unwrap_or(false);
        if !change.enabled {
            return !saved && !engine_serves;
        }
        if !saved || !engine_serves || change.password_hash.is_some() {
            return false;
        }
        self.config()
            .and_then(|config| config.server)
            .is_some_and(|server| {
                server.bind == change.bind
                    && server.auth.enabled == Some(true)
                    && server
                        .auth
                        .users
                        .iter()
                        .any(|user| user.name == change.user)
            })
    }

    /// The part of [`apply`](Self::apply) that runs with the old engine stopped.
    async fn restart<S>(
        &self,
        change: Change,
        settings: S,
    ) -> (Option<ThisComputer>, Result<(), String>)
    where
        S: Fn(&Config) -> fbt::Settings + Clone,
    {
        if change.enabled
            && let Err(problem) = self.write_config(&change)
        {
            // Back to the window's own engine, and the switch saved off to match it.
            let back = local::start_with(&self.paths, false, settings).await.ok();
            if let Err(saving) = self.save_switch(false) {
                eprintln!("Serving is off, but the switch could not be saved: {saving}");
            }
            return (back, Err(problem));
        }
        match local::start_with(&self.paths, change.enabled, settings.clone()).await {
            Ok(running) => {
                let saved = self.save_switch(change.enabled);
                (Some(running), saved)
            }
            Err(problem) if change.enabled => {
                // Back to the window's own engine, and the switch saved off: the line says why.
                let back = local::start_with(&self.paths, false, settings).await.ok();
                if let Err(saving) = self.save_switch(false) {
                    eprintln!("Serving is off, but the switch could not be saved: {saving}");
                }
                (back, Err(problem))
            }
            Err(problem) => (None, Err(problem)),
        }
    }

    /// Saves `serve.enabled` through the settings store.
    fn save_switch(&self, enabled: bool) -> Result<(), String> {
        self.settings.change(|settings| {
            settings.serve.enabled = enabled;
            Ok(())
        })
    }
}

/// What is wrong with `name` as a login name, the rules `featherbittorrent user set` checks: not empty,
/// not too long, nothing invisible.
fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some(USER_NEEDED.to_string());
    }
    let chars = name.chars().count();
    if chars > MAX_NAME_CHARS {
        return Some(format!(
            "A name is at most {MAX_NAME_CHARS} characters, this one has {chars}."
        ));
    }
    if name.chars().any(char::is_control) {
        return Some("A name may not contain a control character.".to_string());
    }
    None
}

/// The address another machine types to reach an engine listening on `addr`: `addr` itself, or, for an
/// engine on every interface, this machine's address on the network the default route leaves by.
fn reachable_at(addr: SocketAddr) -> String {
    let ip = if addr.ip().is_unspecified() {
        network_address().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
    } else {
        addr.ip()
    };
    format!("http://{}", SocketAddr::new(ip, addr.port()))
}

/// This machine's address on its network: the source address the system would pick to reach a
/// documentation-only address (TEST-NET-1). A UDP `connect` only asks the routing table; nothing is sent.
fn network_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// What the Serving section shows now.
#[tauri::command]
pub fn serving_status(
    serving: tauri::State<'_, Serving>,
    state: tauri::State<'_, DesktopState>,
) -> ServingView {
    serving.view(state.serving_addr(), None)
}

/// Turns Serve the web UI on or off (or changes its address and login), restarting This computer. The
/// page connects to `local` again afterwards: the restart made a new launch secret.
#[tauri::command]
pub async fn set_serving(
    request: ServingRequest,
    serving: tauri::State<'_, Serving>,
    lifecycle: tauri::State<'_, Lifecycle>,
    state: tauri::State<'_, DesktopState>,
) -> Result<ServingView, String> {
    serving
        .apply(&lifecycle, &state, request, Config::session_settings)
        .await
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr, TcpListener};
    use std::time::Duration;

    use fbtt_core::Config;
    use fbtt_core::scratch::ScratchDir;
    use reqwest::StatusCode;

    use super::{PASSWORD_NEEDED, Serving, ServingRequest};
    use crate::commands::DesktopState;
    use crate::lifecycle::Lifecycle;
    use crate::local::tests::{offline, scratch_paths, start_offline};
    use crate::settings_store::SettingsStore;

    const DEADLINE: Duration = Duration::from_secs(60);

    struct Running {
        _dir: ScratchDir,
        serving: Serving,
        downloads: std::path::PathBuf,
        lifecycle: Lifecycle,
        state: DesktopState,
    }

    async fn running(name: &str) -> Running {
        let dir = ScratchDir::new(name);
        let paths = scratch_paths(&dir);
        // No `server:` section: the window's own engine still has the download folder as its root, so a
        // torrent can be added while serving is off (the fresh-library fix).
        let library = paths.library();
        let config = Config::load_for_edit(&library.config_file())
            .expect("reads")
            .expect("there");
        assert!(config.server.is_none(), "a desktop library names no server");
        let store = SettingsStore::new(paths.settings_file());
        let this = start_offline(&paths).await.expect("This computer starts");
        Running {
            downloads: config.download_dir.clone(),
            serving: Serving::new(paths, store.clone()),
            lifecycle: Lifecycle::new(store),
            state: DesktopState::running(this),
            _dir: dir,
        }
    }

    impl Running {
        async fn apply(&self, request: ServingRequest) -> Result<super::ServingView, String> {
            tokio::time::timeout(
                DEADLINE,
                self.serving
                    .apply(&self.lifecycle, &self.state, request, offline),
            )
            .await
            .expect("applied within the deadline")
        }

        async fn endpoint(&self) -> (String, String) {
            crate::commands::connect_to(&self.state, "local", DEADLINE)
                .await
                .map(|endpoint| (endpoint.base, endpoint.token.expect("a launch secret")))
                .expect("local connects")
        }

        async fn stop(self) {
            if let Some(this) = self.state.startup().take_when_settled(DEADLINE).await {
                this.stop().await;
            }
        }
    }

    /// A loopback port nobody listens on now, to ask serving for by number.
    fn free_loopback_port() -> u16 {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback port")
            .local_addr()
            .expect("its address")
            .port()
    }

    fn on(bind: &str, user: &str, password: &str) -> ServingRequest {
        ServingRequest {
            enabled: true,
            bind: bind.to_string(),
            user: user.to_string(),
            password: password.to_string(),
        }
    }

    fn off() -> ServingRequest {
        ServingRequest {
            enabled: false,
            bind: String::new(),
            user: String::new(),
            password: String::new(),
        }
    }

    async fn send(request: reqwest::RequestBuilder) -> reqwest::Response {
        tokio::time::timeout(DEADLINE, request.send())
            .await
            .expect("an answer within the deadline")
            .expect("the request completed")
    }

    /// Turning serving on with no password (and no login already in the config for that name) is refused
    /// before anything stops: the engine keeps its secret, and nothing is saved.
    #[tokio::test(flavor = "multi_thread")]
    async fn serving_requires_a_password() {
        let app = running("desktop-serving-password").await;
        let (_, secret) = app.endpoint().await;
        let config_file = app.serving.paths.library().config_file();
        let before = std::fs::read_to_string(&config_file).expect("the config reads");

        let refused = app.apply(on("127.0.0.1:0", "alice", "")).await;
        assert_eq!(refused.err().as_deref(), Some(PASSWORD_NEEDED));
        assert!(app.apply(on("127.0.0.1:0", "  ", "hunter2")).await.is_err());
        assert!(
            app.apply(on("not an address", "alice", "hunter2"))
                .await
                .is_err()
        );

        assert_eq!(app.endpoint().await.1, secret, "nothing was restarted");
        assert!(!app.serving.view(None, None).enabled, "nothing was saved");
        assert_eq!(
            std::fs::read_to_string(&config_file).expect("the config reads"),
            before,
            "the config was not touched"
        );
        app.stop().await;
    }

    /// A torrent added before serving was switched on is still there after the restart, and after the
    /// restart that switches it off again; each restart makes a new launch secret.
    #[tokio::test(flavor = "multi_thread")]
    async fn serving_restart_keeps_torrents() {
        let app = running("desktop-serving-restart").await;
        let (base, secret) = app.endpoint().await;
        let client = reqwest::Client::new();
        let id = "ab".repeat(20);
        let body = serde_json::json!({
            "magnet": format!("magnet:?xt=urn:btih:{id}"),
            "save_path": app.downloads.to_string_lossy(),
        });
        let added = send(
            client
                .post(format!("{base}/api/v1/torrents"))
                .bearer_auth(&secret)
                .header("content-type", "application/json")
                .body(body.to_string()),
        )
        .await;
        let status = added.status();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            added.text().await.unwrap_or_default()
        );

        let listed = |base: String, secret: String| {
            let client = client.clone();
            async move {
                let answer = send(
                    client
                        .get(format!("{base}/api/v1/torrents"))
                        .bearer_auth(&secret),
                )
                .await;
                assert_eq!(answer.status(), StatusCode::OK);
                answer.text().await.expect("a body")
            }
        };

        let view = app
            .apply(on("127.0.0.1:0", "alice", "hunter2"))
            .await
            .expect("serving turns on");
        assert!(view.enabled);
        let (base, after_on) = app.endpoint().await;
        assert_ne!(after_on, secret, "a restart makes a new secret");
        assert!(listed(base, after_on.clone()).await.contains(&id));

        let view = app.apply(off()).await.expect("serving turns off");
        assert!(!view.enabled);
        assert_eq!(view.address, None);
        let (base, after_off) = app.endpoint().await;
        assert_ne!(after_off, after_on);
        assert!(listed(base, after_off).await.contains(&id));
        app.stop().await;
    }

    /// With serving on, the web UI answers on the address asked for, logins are on (the API refuses a
    /// request with no credential), the user is in the config with a hash rather than the password, and
    /// the section shows where it is reachable. Off again, that port is closed.
    #[tokio::test(flavor = "multi_thread")]
    async fn serving_on_answers_the_web_ui_on_the_chosen_bind() {
        let app = running("desktop-serving-web-ui").await;
        let port = free_loopback_port();
        let bind = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

        let view = app
            .apply(on(&bind.to_string(), "alice", "hunter2"))
            .await
            .expect("serving turns on");
        assert_eq!(view.bind, bind.to_string());
        assert_eq!(view.user, "alice");
        assert_eq!(
            view.address.as_deref(),
            Some(format!("http://{bind}").as_str())
        );
        assert_eq!(app.state.local_addr(), Some(bind));

        let client = reqwest::Client::new();
        let page = send(client.get(format!("http://{bind}/"))).await;
        assert_eq!(page.status(), StatusCode::OK);
        let html = page.text().await.expect("a body");
        assert!(
            html.contains("<div id=\"root\">"),
            "the web UI's page: {html}"
        );
        let api = send(client.get(format!("http://{bind}/api/v1/torrents"))).await;
        assert_eq!(api.status(), StatusCode::UNAUTHORIZED, "logins are on");

        let config = Config::load_for_edit(&app.serving.paths.library().config_file())
            .expect("reads")
            .expect("there");
        let server = config.server.expect("a server section");
        assert_eq!(server.bind, bind);
        assert_eq!(server.auth.enabled, Some(true));
        let alice = &server.auth.users[0];
        assert_eq!(alice.name, "alice");
        assert!(fbtt_server::auth::password::verify(
            &alice.password_hash,
            "hunter2"
        ));
        assert!(!alice.password_hash.contains("hunter2"));

        // The same bind and user with no password changes nothing: no restart, the same secret.
        let (_, secret) = app.endpoint().await;
        app.apply(on(&bind.to_string(), "alice", ""))
            .await
            .expect("a known user needs no new password");
        assert_eq!(app.endpoint().await.1, secret, "nothing was restarted");

        // A second login applied is the user the section shows, not the config's first.
        let view = app
            .apply(on(&bind.to_string(), "bob", "correct horse"))
            .await
            .expect("a second login");
        assert_eq!(view.user, "bob");

        app.apply(off()).await.expect("serving turns off");
        assert!(
            tokio::net::TcpStream::connect(bind).await.is_err(),
            "the port is closed once serving is off"
        );
        app.stop().await;
    }

    /// A restart asked for once a quit began starts nothing, and the quit's own stop still runs.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_restart_after_a_quit_began_starts_nothing() {
        let app = running("desktop-serving-quit").await;
        assert!(app.lifecycle.begin_quit());
        let refused = app.apply(on("127.0.0.1:0", "alice", "hunter2")).await;
        assert_eq!(refused.err().as_deref(), Some(crate::lifecycle::QUITTING));
        app.lifecycle.stop_for_quit(&app.state).await;
        assert!(app.state.local_addr().is_none(), "the quit stopped it");
    }

    /// A serving restart asked for while This computer's first start is still under way waits for that
    /// start, then restarts the engine it started; a connect afterwards answers the new secret, and a quit
    /// after it stops (and saves) the restarted engine.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_serving_restart_waits_for_the_initial_start() {
        let dir = ScratchDir::new("desktop-serving-waits-for-start");
        let paths = scratch_paths(&dir);
        let store = SettingsStore::new(paths.settings_file());
        let serving = Serving::new(paths.clone(), store.clone());
        let lifecycle = Lifecycle::new(store);
        let state = DesktopState::starting();

        let restart = serving.apply(
            &lifecycle,
            &state,
            on("127.0.0.1:0", "alice", "hunter2"),
            offline,
        );
        let first = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            // The restart has not touched the library yet: the first start still gets its lock.
            let this = start_offline(&paths).await.expect("the first start runs");
            let secret = this.endpoint().1;
            assert!(state.startup().finish(Ok(this)).is_none());
            secret
        };
        let (applied, first_secret) =
            tokio::time::timeout(DEADLINE, async { tokio::join!(restart, first) })
                .await
                .expect("both finish in time");
        let view = applied.expect("serving turns on after the first start");
        assert!(view.enabled);
        assert!(view.address.is_some(), "the restarted engine serves");

        let endpoint = crate::commands::connect_to(&state, "local", DEADLINE)
            .await
            .expect("local connects");
        let secret = endpoint.token.expect("a launch secret");
        assert_ne!(secret, first_secret, "the connect answers the new secret");

        assert!(lifecycle.begin_quit());
        lifecycle.stop_for_quit(&state).await;
        assert!(state.local_addr().is_none(), "the quit stopped it");
        let again = start_offline(&paths)
            .await
            .expect("the library was released");
        again.stop().await;
    }

    /// With the switch saved on, the app's first start serves at once (the same start a restart uses);
    /// an engine that cannot serve (its port is taken) starts for the window alone, the switch is saved
    /// off and the Serving section has the line, while the start itself counts as fine.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_first_start_serves_or_falls_back_to_the_window() {
        let app = running("desktop-serving-first-start").await;
        let port = free_loopback_port();
        let bind = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        app.apply(on(&bind.to_string(), "alice", "hunter2"))
            .await
            .expect("serving turns on");
        let paths = app.serving.paths.clone();
        let store = app.serving.settings.clone();
        // The app quits; its scratch directory stays until the end of the test.
        if let Some(this) = app.state.startup().take_when_settled(DEADLINE).await {
            this.stop().await;
        }

        // Next launch: the saved switch is on, the first start serves.
        let serving = Serving::new(paths.clone(), store.clone());
        let this = tokio::time::timeout(DEADLINE, serving.start_this_computer(true, offline))
            .await
            .expect("in time")
            .expect("starts serving");
        assert!(this.exposed());
        assert_eq!(this.local_addr(), bind);
        this.stop().await;

        // The port is taken by something else: the window still gets its engine.
        let squatter = TcpListener::bind(bind).expect("the port is free again");
        let serving = Serving::new(paths, store);
        let this = tokio::time::timeout(DEADLINE, serving.start_this_computer(true, offline))
            .await
            .expect("in time")
            .expect("falls back to the window");
        assert!(!this.exposed(), "the window's engine alone");
        let state = DesktopState::running(this);
        let view = serving.view(state.serving_addr(), None);
        assert!(!view.enabled, "the switch was saved off");
        assert!(view.address.is_none());
        assert!(view.problem.is_some(), "the line is kept for Settings");
        assert_eq!(
            state.startup().status(DEADLINE).await,
            crate::startup::StartupStatus::Fine,
            "not a startup problem"
        );
        drop(squatter);
        if let Some(this) = state.startup().take_when_settled(DEADLINE).await {
            this.stop().await;
        }
        drop(app);
    }

    /// With the switch saved on but logins turned off in `config.yaml` since (or every user removed), the
    /// first start does not serve: the window gets its engine alone, and the Serving section has the line.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_first_start_with_logins_off_serves_nothing() {
        let app = running("desktop-serving-logins-off").await;
        let bind = SocketAddr::from((Ipv4Addr::LOCALHOST, free_loopback_port()));
        app.apply(on(&bind.to_string(), "alice", "hunter2"))
            .await
            .expect("serving turns on");
        let paths = app.serving.paths.clone();
        let store = app.serving.settings.clone();
        if let Some(this) = app.state.startup().take_when_settled(DEADLINE).await {
            this.stop().await;
        }
        let config_file = paths.library().config_file();
        let edit = |change: &dyn Fn(&mut Config)| {
            let mut config = Config::load_for_edit(&config_file)
                .expect("reads")
                .expect("there");
            change(&mut config);
            config.save(&config_file).expect("saves");
        };

        for (what, change) in [
            (
                "logins off",
                &(|config: &mut Config| {
                    config
                        .server
                        .as_mut()
                        .expect("a server section")
                        .auth
                        .enabled = Some(false);
                }) as &dyn Fn(&mut Config),
            ),
            (
                "no user",
                &(|config: &mut Config| {
                    let server = config.server.as_mut().expect("a server section");
                    server.auth.enabled = Some(true);
                    server.auth.users.clear();
                }) as &dyn Fn(&mut Config),
            ),
        ] {
            edit(change);
            store
                .change(|settings| {
                    settings.serve.enabled = true;
                    Ok(())
                })
                .expect("the switch is saved on");
            let serving = Serving::new(paths.clone(), store.clone());
            let this = tokio::time::timeout(DEADLINE, serving.start_this_computer(true, offline))
                .await
                .expect("in time")
                .expect("starts for the window");
            assert!(!this.exposed(), "{what}: served with logins off");
            let state = DesktopState::running(this);
            let view = serving.view(state.serving_addr(), None);
            assert!(view.address.is_none(), "{what}");
            assert_eq!(
                view.problem.as_deref(),
                Some(crate::local::LOGINS_OFF),
                "{what}: the line is kept for the Serving section"
            );
            if let Some(this) = state.startup().take_when_settled(DEADLINE).await {
                this.stop().await;
            }
        }
        drop(app);
    }

    /// Off while already off restarts nothing: the launch secret is the same afterwards.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_apply_that_changes_nothing_does_not_restart() {
        let app = running("desktop-serving-unchanged").await;
        let (_, secret) = app.endpoint().await;
        let view = app.apply(off()).await.expect("off while off");
        assert!(!view.enabled);
        assert_eq!(app.endpoint().await.1, secret, "nothing was restarted");
        app.stop().await;
    }

    /// The config cannot be written (here: it no longer parses): the engine comes back for the window
    /// alone, the switch is saved off, and no address is shown, though serving was on before.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_config_write_saves_serving_off_and_shows_no_address() {
        let app = running("desktop-serving-write-fails").await;
        app.apply(on("127.0.0.1:0", "alice", "hunter2"))
            .await
            .expect("serving turns on");
        assert!(app.state.serving_addr().is_some());

        let config_file = app.serving.paths.library().config_file();
        std::fs::write(&config_file, "server: [not, a, section").expect("the config is broken");
        let refused = app
            .apply(on("127.0.0.1:0", "alice", "other password"))
            .await;
        assert!(refused.is_err(), "the write was refused");

        assert!(
            app.state.local_addr().is_some(),
            "the window still has its engine"
        );
        assert_eq!(app.state.serving_addr(), None, "it serves nobody");
        let view = app.serving.view(app.state.serving_addr(), None);
        assert!(!view.enabled, "the switch was saved off");
        assert_eq!(
            view.address, None,
            "no loopback address is shown as reachable"
        );
        app.stop().await;
    }

    /// A quit that arrives while a serving restart holds the engine lock waits for it, then stops the
    /// engine the restart started: nothing is left running and the library is free again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_quit_during_a_serving_restart_stops_the_engine() {
        let app = running("desktop-serving-quit-during").await;
        let paths = app.serving.paths.clone();
        let inside = tokio::sync::Notify::new();
        let release = tokio::sync::Notify::new();

        let restart = app.lifecycle.restart_this_computer(&app.state, || async {
            inside.notify_one();
            release.notified().await;
            match crate::local::start_with(&paths, true, offline).await {
                Ok(running) => (Some(running), Ok(())),
                Err(problem) => (None, Err(problem)),
            }
        });
        let quit = async {
            inside.notified().await;
            assert!(
                app.lifecycle.begin_quit(),
                "the quit starts during the restart"
            );
            let mut stopping = std::pin::pin!(app.lifecycle.stop_for_quit(&app.state));
            assert!(
                tokio::time::timeout(Duration::from_millis(200), &mut stopping)
                    .await
                    .is_err(),
                "the quit's stop waits for the restart"
            );
            release.notify_one();
            stopping.await;
        };
        let (restarted, ()) = tokio::time::timeout(DEADLINE, async { tokio::join!(restart, quit) })
            .await
            .expect("both finish in time");
        restarted.expect("the restart started its engine");
        assert!(app.state.local_addr().is_none(), "nothing is left running");
        let again = start_offline(&paths)
            .await
            .expect("the library was released");
        again.stop().await;
    }
}
