//! `server.cors_origins` on an ordinary server: an origin on the list is enough for a client in another
//! origin (the desktop app's webview) to sign in with a password and use the session it gets.
//!
//! The engine settings are loopback-only, as in `tests/server.rs`, so nothing here leaves the machine.

use std::path::Path;
use std::time::Duration;

use fbtt_core::scratch::ScratchDir;
use fbtt_core::{AuthConfig, Config, LibraryLock, Paths, ServerConfig, UserEntry};
use fbtt_server::Server;
use fbtt_server::auth::password;
use reqwest::StatusCode;
use serde_json::Value;

const DEADLINE: Duration = Duration::from_secs(60);
/// The origin the desktop app's webview sends on macOS and Linux.
const DESKTOP_ORIGIN: &str = "tauri://localhost";
const USER: &str = "alice";
const SECRET: &str = "hunter2";

async fn within<T>(what: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
}

/// The settings every engine here runs on: loopback on a port the OS picks, no DHT, LSD, UPnP or NAT-PMP.
fn loopback(config: &Config) -> fbt::Settings {
    let mut settings = config.session_settings();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    settings
}

fn config(scratch: &Path) -> Config {
    Config {
        download_dir: scratch.join("downloads"),
        server: Some(ServerConfig {
            bind: "127.0.0.1:0".parse().expect("a loopback bind address"),
            cors_origins: vec![DESKTOP_ORIGIN.to_string()],
            auth: AuthConfig {
                enabled: Some(true),
                users: vec![UserEntry {
                    name: USER.to_string(),
                    password_hash: password::hash(SECRET),
                }],
                devices: Vec::new(),
            },
            ..ServerConfig::default()
        }),
        ..Config::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_cors_origin_can_sign_in_with_a_password() {
    let dir = ScratchDir::new("cors-sign-in");
    let paths = Paths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
    };
    let config = config(dir.path()).resolve(&paths);
    let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
    let server = within(Server::start_with_settings(
        loopback(&config),
        config,
        &paths,
        lock,
    ))
    .await
    .expect("start the server");
    let base = format!("http://{}", server.local_addr());
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("an http client");

    // The preflight a webview sends before a sign-in carrying `Authorization`.
    let preflight = within(
        client
            .request(
                reqwest::Method::OPTIONS,
                format!("{base}/api/v1/auth/session"),
            )
            .header(reqwest::header::ORIGIN, DESKTOP_ORIGIN)
            .header(reqwest::header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(
                reqwest::header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization",
            )
            .send(),
    )
    .await
    .expect("the server answered");
    assert!(preflight.status().is_success(), "{}", preflight.status());
    assert_eq!(
        preflight
            .headers()
            .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok()),
        Some(DESKTOP_ORIGIN),
    );

    let signed_in = within(
        client
            .post(format!("{base}/api/v1/auth/session"))
            .header(reqwest::header::ORIGIN, DESKTOP_ORIGIN)
            .basic_auth(USER, Some(SECRET))
            .send(),
    )
    .await
    .expect("the server answered");
    assert_eq!(signed_in.status(), StatusCode::OK);
    assert_eq!(
        signed_in
            .headers()
            .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok()),
        Some(DESKTOP_ORIGIN),
    );
    let body: Value = within(signed_in.json()).await.expect("a JSON body");
    let token = body["token"].as_str().expect("a session token").to_string();

    let listed = within(
        client
            .get(format!("{base}/api/v1/torrents"))
            .header(reqwest::header::ORIGIN, DESKTOP_ORIGIN)
            .bearer_auth(&token)
            .send(),
    )
    .await
    .expect("the server answered");
    assert_eq!(listed.status(), StatusCode::OK);

    within(server.shutdown()).await.expect("the last save ran");
    drop(dir);
}
