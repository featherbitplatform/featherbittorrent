//! The commands the page calls through Tauri's `invoke`.
//!
//! Each command is a thin wrapper over a plain function that takes what the command reads from the app's
//! state, so the decisions are tested without a webview. Torrents, files and settings never pass through
//! here: the page calls the engine's HTTP API directly with the endpoint [`connect`] hands it, which is the
//! one client code path the browser build uses too.

use std::time::Duration;

use fbtt_desktop_core::LOCAL_ID;
use serde::Serialize;

use crate::instances::Instances;
use crate::startup::{START_WAIT, Startup};

/// What the shell keeps for the commands: This computer's start and, once it finished, the engine or why
/// there is none ([`Startup`]).
#[derive(Debug)]
pub struct DesktopState {
    startup: Startup,
}

impl DesktopState {
    /// The state while This computer starts (off the main thread, `startup::spawn`).
    pub fn starting() -> Self {
        DesktopState {
            startup: Startup::starting(),
        }
    }

    /// The state of a run whose engine already runs: what a test starts from.
    #[cfg(test)]
    pub fn running(this_computer: crate::local::ThisComputer) -> Self {
        let state = DesktopState::starting();
        state.startup.finish(Ok(this_computer));
        state
    }

    /// This computer's start, which connect, quit and a serving restart wait on.
    pub fn startup(&self) -> &Startup {
        &self.startup
    }

    /// The address This computer listens on, while it runs (served to others or not). The tests' view.
    #[cfg(test)]
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.startup
            .with_running(crate::local::ThisComputer::local_addr)
    }

    /// The address This computer serves other machines on: only while it runs and was started with Serve
    /// the web UI on. A window-only engine (loopback, launch secret only) is never an address to give out.
    pub fn serving_addr(&self) -> Option<std::net::SocketAddr> {
        self.startup
            .with_running(|this| this.exposed().then(|| this.local_addr()))
            .flatten()
    }
}

/// Which system the app runs on, spelled the way the page spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Windows,
    Linux,
}

impl Platform {
    /// The system this build is for. Anything that is neither macOS nor Windows is built with the Linux
    /// stack (GTK and WebKitGTK), so it gets the Linux answer.
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::Macos
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

/// The app's version and the system it runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DesktopInfo {
    pub version: String,
    pub platform: Platform,
}

/// An instance the page can talk to: an absolute base address with no trailing slash, and the bearer
/// token to present there, or none when the page has to sign in first (a remote instance with no saved
/// password). Serialized as `{ kind: "bearer", base, token }`, the page's `Endpoint`.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct EndpointDto {
    pub kind: &'static str,
    pub base: String,
    pub token: Option<String>,
}

impl std::fmt::Debug for EndpointDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the token.
        f.debug_struct("EndpointDto")
            .field("kind", &self.kind)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// What [`connect`] answers for `local`: This computer's endpoint, once its start finished (waiting at
/// most `limit`), or the line saying why there is none. Any other id is refused here; the command hands
/// remote ids to [`Instances::connect`].
pub async fn connect_to(
    state: &DesktopState,
    id: &str,
    limit: Duration,
) -> Result<EndpointDto, String> {
    if id != LOCAL_ID {
        return Err(format!(
            "There is no instance called {}.",
            fbt::display_safe(id, fbt::MAX_DISPLAY_CHARS)
        ));
    }
    let (base, token) = state.startup.endpoint(limit).await?;
    Ok(EndpointDto {
        kind: "bearer",
        base,
        token: Some(token),
    })
}

/// The app's version and platform, for the page's title bar and About text.
#[tauri::command]
pub fn desktop_info() -> DesktopInfo {
    DesktopInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        platform: Platform::current(),
    }
}

/// The endpoint for instance `id`, which the page builds its connection on.
#[tauri::command]
pub async fn connect(
    id: String,
    state: tauri::State<'_, DesktopState>,
    instances: tauri::State<'_, Instances>,
) -> Result<EndpointDto, String> {
    if id != LOCAL_ID {
        return instances.connect(&id).await;
    }
    connect_to(&state, &id, START_WAIT).await
}

#[cfg(test)]
mod tests {
    use fbtt_core::scratch::ScratchDir;

    use std::time::Duration;

    use super::{DesktopState, Platform, connect_to, desktop_info};
    use crate::local::tests::{scratch_paths, start_offline};

    /// `local` answers a bearer endpoint carrying This computer's own address and launch secret.
    #[tokio::test(flavor = "multi_thread")]
    async fn connect_local_returns_the_secret() {
        let dir = ScratchDir::new("desktop-connect-local");
        let paths = scratch_paths(&dir);
        let this = start_offline(&paths).await.expect("This computer starts");
        let (base, secret) = this.endpoint();
        let state = DesktopState::starting();
        state.startup().finish(Ok(this));
        let wait = Duration::from_secs(30);

        let answer = connect_to(&state, "local", wait)
            .await
            .expect("local connects");
        assert_eq!(answer.kind, "bearer");
        assert_eq!(answer.base, base);
        assert_eq!(answer.token.as_deref(), Some(secret.as_str()));
        assert!(
            !format!("{answer:?}").contains(&secret),
            "Debug hides the token"
        );

        assert!(connect_to(&state, "nowhere", wait).await.is_err());

        state
            .startup()
            .take_when_settled(wait)
            .await
            .expect("still running")
            .stop()
            .await;
        assert!(
            connect_to(&state, "local", wait).await.is_err(),
            "a stopped engine has no endpoint"
        );
    }

    #[test]
    fn desktop_info_names_this_build() {
        let info = desktop_info();
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.platform, Platform::current());
        let json = serde_json::to_value(&info).expect("serializes");
        assert!(matches!(
            json["platform"].as_str(),
            Some("macos" | "windows" | "linux")
        ));
    }

    /// `tauri.conf.json` names no version of its own, so the window, the bundles and `desktop_info` all
    /// carry the workspace's: Tauri falls back to the crate's version, which is the workspace's.
    #[test]
    fn tauri_conf_takes_the_workspace_version() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json parses");
        assert!(
            conf.get("version").is_none(),
            "tauri.conf.json must not pin a version: the workspace's is the app's"
        );
        assert_eq!(
            conf["identifier"],
            "io.featherbit.featherbittorrent.desktop"
        );
        assert_eq!(conf["productName"], "featherbittorrent");
    }
}
