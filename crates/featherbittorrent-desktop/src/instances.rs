//! The instances the window can switch between: This computer, and the saved remote servers.
//!
//! The list itself, and its rules, are `fbtt-desktop-core`'s ([`DesktopSettings::add`],
//! [`DesktopSettings::usable`] and the rest). What lives here is what needs the shell: the settings file
//! read and written around each change, the keychain for saved passwords ([`Keychain`]), and the HTTP the
//! shell speaks for the page in two places. One is signing in with a saved password, so the password goes
//! from the keychain to the server without ever reaching the page. The other is [`Instances::probe`],
//! which tells a server that is down from one that is up but does not list this app's origin in its
//! `server.cors_origins`: the webview sees both as the same network error.
//!
//! The settings file is read again before every change and written straight after, so a change made
//! elsewhere in the app (the close choice, the title bar switch) is never overwritten with an older copy,
//! and a file that cannot be read is never written over.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fbtt_desktop_core::{DesktopSettings, Instance, LOCAL_ID};
use serde::Serialize;
use zeroize::Zeroizing;

use crate::commands::EndpointDto;
use crate::keychain::{Keychain, NO_KEYCHAIN};

/// The name the switcher shows for the app's own engine.
pub const LOCAL_NAME: &str = "This computer";

/// What connecting says when the server gives no answer at all: the page's own unreachable line.
pub const UNREACHABLE: &str = "Cannot reach the server.";

/// The route a password sign-in posts to, and the one the CORS preflight asks about.
const SIGN_IN: &str = "/api/v1/auth/session";

/// The route the reachability probe asks: any answer, whatever its status, means a server is there.
const PROBE: &str = "/api/v1/session";

/// How long the shell waits to reach a server, and for its whole answer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);

/// One row of the switcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstanceDto {
    pub id: String,
    pub name: String,
    /// The address, without a trailing slash. Empty for This computer, whose address changes every launch.
    pub url: String,
    pub saved_password: bool,
}

/// The switcher's list: This computer first, then every usable saved instance; the one to open on; and
/// whether this system can save passwords at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstancesDto {
    pub default: String,
    pub items: Vec<InstanceDto>,
    pub keychain: bool,
}

/// What [`Instances::probe`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProbeDto {
    /// Something answered at the address, with any HTTP status.
    pub reachable: bool,
    /// The server's CORS answer names this app's origin.
    pub cors_ok: bool,
}

/// The saved instances, their passwords, and the HTTP client the shell speaks to them with.
pub struct Instances {
    settings_file: PathBuf,
    keychain: Arc<dyn Keychain>,
    client: reqwest::Client,
    /// The origin this app's webview sends, which a remote server has to list.
    origin: String,
    /// One change to the settings file at a time.
    writing: Mutex<()>,
}

impl std::fmt::Debug for Instances {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instances")
            .field("settings_file", &self.settings_file)
            .field("keychain", &self.keychain.available())
            .finish_non_exhaustive()
    }
}

impl Instances {
    /// The instances in `settings_file`, with passwords in `keychain`, for a webview whose origin is
    /// `origin`.
    pub fn new(settings_file: PathBuf, keychain: Arc<dyn Keychain>, origin: &str) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(ANSWER_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Instances::with_client(settings_file, keychain, origin, client)
    }

    fn with_client(
        settings_file: PathBuf,
        keychain: Arc<dyn Keychain>,
        origin: &str,
        client: reqwest::Client,
    ) -> Self {
        Instances {
            settings_file,
            keychain,
            client,
            origin: origin.to_string(),
            writing: Mutex::new(()),
        }
    }

    /// The settings as the file says now. A file that cannot be read lists only This computer.
    fn read(&self) -> Result<DesktopSettings, String> {
        DesktopSettings::load(&self.settings_file).map_err(|problem| problem.to_string())
    }

    /// Reads the file, applies `change`, and writes it back; nothing is written when either fails.
    fn change<T>(
        &self,
        change: impl FnOnce(&mut DesktopSettings) -> Result<T, String>,
    ) -> Result<T, String> {
        let _one = self
            .writing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut settings = self.read()?;
        let answer = change(&mut settings)?;
        settings
            .save(&self.settings_file)
            .map_err(|problem| problem.to_string())?;
        Ok(answer)
    }

    /// The saved instance `id`, when it is one the app may dial.
    fn usable(&self, id: &str) -> Result<Instance, String> {
        let settings = self.read()?;
        settings
            .usable()
            .into_iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "There is no instance called {}.",
                    fbt::display_safe(id, fbt::MAX_DISPLAY_CHARS)
                )
            })
    }

    /// The switcher's list.
    pub fn list(&self) -> InstancesDto {
        let keychain = self.keychain.available();
        let settings = self.read().unwrap_or_else(|problem| {
            eprintln!("{problem}");
            DesktopSettings::default()
        });
        let mut items = vec![InstanceDto {
            id: LOCAL_ID.to_string(),
            name: LOCAL_NAME.to_string(),
            url: String::new(),
            saved_password: false,
        }];
        items.extend(settings.usable().into_iter().map(|entry| InstanceDto {
            id: entry.id.clone(),
            name: entry.name.clone(),
            url: normalise_base(&entry.url),
            saved_password: keychain && entry.save_password,
        }));
        InstancesDto {
            default: settings.default_or_local().to_string(),
            items,
            keychain,
        }
    }

    /// Saves a new remote instance.
    pub fn add(&self, name: &str, url: &str) -> Result<InstanceDto, String> {
        let added = self.change(|settings| {
            settings
                .add(name, url)
                .map_err(|problem| sentence(&problem.to_string()))
        })?;
        Ok(InstanceDto {
            url: normalise_base(&added.url),
            id: added.id,
            name: added.name,
            saved_password: false,
        })
    }

    /// Removes a saved instance and its saved password. The default falls back to This computer when it
    /// was this one.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        if id == LOCAL_ID {
            return Err("This computer cannot be removed.".to_string());
        }
        self.change(|settings| {
            settings.remove(id);
            Ok(())
        })?;
        self.keychain.delete(id);
        Ok(())
    }

    /// Makes `id` the instance the app opens on.
    pub fn set_default(&self, id: &str) -> Result<(), String> {
        self.change(|settings| {
            settings
                .set_default(id)
                .map_err(|problem| sentence(&problem.to_string()))
        })
    }

    /// Saves `password` for `user` on instance `id`, for the shell to sign in with next time.
    pub fn save_password(&self, id: &str, user: &str, password: &str) -> Result<(), String> {
        if !self.keychain.available() {
            return Err(NO_KEYCHAIN.to_string());
        }
        let user = user.trim();
        if user.is_empty() {
            return Err("A saved password needs a user name.".to_string());
        }
        self.usable(id)?;
        self.keychain.set(id, password)?;
        let marked = self.change(|settings| {
            let entry = settings
                .instances
                .iter_mut()
                .find(|entry| entry.id == id)
                .ok_or_else(|| "That instance was removed.".to_string())?;
            entry.save_password = true;
            entry.user = Some(user.to_string());
            Ok(())
        });
        if marked.is_err() {
            // A password the settings file does not point at would never be found or forgotten.
            self.keychain.delete(id);
        }
        marked
    }

    /// Forgets the saved password for `id`: out of the keychain, and the file no longer asks for it.
    pub fn forget_password(&self, id: &str) -> Result<(), String> {
        self.keychain.delete(id);
        self.change(|settings| {
            if let Some(entry) = settings.instances.iter_mut().find(|entry| entry.id == id) {
                entry.save_password = false;
                entry.user = None;
            }
            Ok(())
        })
    }

    /// The endpoint for remote instance `id`: its address, and a session token when a password is saved
    /// and the server takes it. No token means the page shows its sign-in screen. A server that gives no
    /// answer at all is [`UNREACHABLE`].
    pub async fn connect(&self, id: &str) -> Result<EndpointDto, String> {
        let instance = self.usable(id)?;
        let base = normalise_base(&instance.url);
        let user = instance.user.as_deref().filter(|_| instance.save_password);
        let password = user.and_then(|_| self.keychain.get(id)).map(Zeroizing::new);
        let token = match (user, password) {
            (Some(user), Some(password)) => self.sign_in(&base, user, &password).await?,
            _ => None,
        };
        Ok(EndpointDto {
            kind: "bearer",
            base,
            token,
        })
    }

    /// `POST /api/v1/auth/session` with Basic credentials. A refused password is `None`, so the page asks
    /// for one; no answer at all is [`UNREACHABLE`].
    async fn sign_in(
        &self,
        base: &str,
        user: &str,
        password: &str,
    ) -> Result<Option<String>, String> {
        let response = self
            .client
            .post(format!("{base}{SIGN_IN}"))
            .basic_auth(user, Some(password))
            .send()
            .await
            .map_err(|_| UNREACHABLE.to_string())?;
        if !response.status().is_success() {
            return Ok(None);
        }
        #[derive(serde::Deserialize)]
        struct Session {
            token: String,
        }
        let Ok(bytes) = response.bytes().await else {
            return Ok(None);
        };
        Ok(serde_json::from_slice::<Session>(&bytes)
            .ok()
            .map(|session| session.token))
    }

    /// Whether instance `id` answers at all, and whether it lets this app's origin in.
    pub async fn probe(&self, id: &str) -> Result<ProbeDto, String> {
        let base = normalise_base(&self.usable(id)?.url);
        let reachable = self
            .client
            .get(format!("{base}{PROBE}"))
            .send()
            .await
            .is_ok();
        if !reachable {
            return Ok(ProbeDto {
                reachable,
                cors_ok: false,
            });
        }
        let preflight = self
            .client
            .request(reqwest::Method::OPTIONS, format!("{base}{SIGN_IN}"))
            .header(reqwest::header::ORIGIN, &self.origin)
            .header(reqwest::header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(
                reqwest::header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization",
            )
            .send()
            .await;
        let cors_ok = preflight.is_ok_and(|answer| {
            answer
                .headers()
                .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|value| value.to_str().ok())
                == Some(self.origin.as_str())
        });
        Ok(ProbeDto { reachable, cors_ok })
    }
}

/// An address in the spelling every request is built on: no surrounding space and no trailing slash, as
/// the page's `normaliseBase` spells it.
pub fn normalise_base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// A core error line as the page shows it: a capital first letter and a full stop.
fn sentence(line: &str) -> String {
    let mut chars = line.chars();
    let mut out: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    };
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

/// The instance list.
#[tauri::command]
pub async fn list_instances(
    instances: tauri::State<'_, Instances>,
) -> Result<InstancesDto, String> {
    Ok(instances.list())
}

/// Saves a new remote instance.
#[tauri::command]
pub async fn add_instance(
    name: String,
    url: String,
    instances: tauri::State<'_, Instances>,
) -> Result<InstanceDto, String> {
    instances.add(&name, &url)
}

/// Removes a saved instance and its saved password.
#[tauri::command]
pub async fn remove_instance(
    id: String,
    instances: tauri::State<'_, Instances>,
) -> Result<(), String> {
    instances.remove(&id)
}

/// Makes an instance the one the app opens on.
#[tauri::command]
pub async fn set_default_instance(
    id: String,
    instances: tauri::State<'_, Instances>,
) -> Result<(), String> {
    instances.set_default(&id)
}

/// Saves a password in the keychain for an instance.
#[tauri::command]
pub async fn save_password(
    id: String,
    user: String,
    password: String,
    instances: tauri::State<'_, Instances>,
) -> Result<(), String> {
    let password = Zeroizing::new(password);
    instances.save_password(&id, &user, &password)
}

/// Forgets an instance's saved password.
#[tauri::command]
pub async fn forget_password(
    id: String,
    instances: tauri::State<'_, Instances>,
) -> Result<(), String> {
    instances.forget_password(&id)
}

/// Whether an instance answers, and whether it lets this app's origin in.
#[tauri::command]
pub async fn probe_instance(
    id: String,
    instances: tauri::State<'_, Instances>,
) -> Result<ProbeDto, String> {
    instances.probe(&id).await
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use fbtt_core::scratch::ScratchDir;
    use fbtt_core::{AuthConfig, Config, LibraryLock, Paths, ServerConfig, UserEntry};
    use fbtt_desktop_core::DesktopSettings;
    use fbtt_server::Server;
    use fbtt_server::auth::password;

    use super::{Instances, LOCAL_NAME, UNREACHABLE};
    use crate::keychain::{Keychain, MemoryKeychain, NO_KEYCHAIN};

    const DEADLINE: Duration = Duration::from_secs(60);
    const ORIGIN: &str = "tauri://localhost";
    const USER: &str = "alice";
    const SECRET: &str = "hunter2";

    async fn within<T>(what: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(DEADLINE, what)
            .await
            .expect("an answer within the deadline")
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("an http client")
    }

    fn store(dir: &ScratchDir, keychain: Arc<dyn Keychain>) -> Instances {
        Instances::with_client(dir.path().join("desktop.yaml"), keychain, ORIGIN, client())
    }

    /// A featherbittorrent server on loopback with logins on, one user, and `cors` as its origins.
    async fn server(scratch: &Path, cors: Vec<String>) -> Server {
        let paths = Paths {
            config_dir: scratch.join("server-config"),
            data_dir: scratch.join("server-data"),
        };
        let config = Config {
            download_dir: scratch.join("downloads"),
            server: Some(ServerConfig {
                bind: "127.0.0.1:0".parse().expect("a loopback bind address"),
                cors_origins: cors,
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
        .resolve(&paths);
        let mut settings = config.session_settings();
        settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
        settings.dht_enabled = false;
        settings.enable_lsd = false;
        settings.enable_upnp = false;
        settings.enable_natpmp = false;
        let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
        within(Server::start_with_settings(settings, config, &paths, lock))
            .await
            .expect("the server starts")
    }

    #[test]
    fn remove_deletes_the_keychain_entry() {
        let dir = ScratchDir::new("desktop-instances-remove");
        let keychain = Arc::new(MemoryKeychain::new());
        let instances = store(&dir, keychain.clone());
        let pi = instances
            .add("Pi", "http://pi.local:8090/")
            .expect("the add");
        assert_eq!(pi.url, "http://pi.local:8090");
        instances
            .save_password(&pi.id, USER, SECRET)
            .expect("the password is saved");
        instances.set_default(&pi.id).expect("the default");
        let listed = instances.list();
        assert_eq!(listed.default, pi.id);
        assert!(listed.items[1].saved_password);
        assert_eq!(keychain.get(&pi.id).as_deref(), Some(SECRET));

        instances.remove(&pi.id).expect("the remove");
        assert_eq!(keychain.get(&pi.id), None, "the password went with it");
        let listed = instances.list();
        assert_eq!(listed.items.len(), 1);
        assert_eq!(listed.items[0].name, LOCAL_NAME);
        assert_eq!(listed.default, "local", "the default falls back");
        assert!(instances.remove("local").is_err());
    }

    #[test]
    fn no_keychain_means_no_save() {
        let dir = ScratchDir::new("desktop-instances-no-keychain");
        let instances = store(&dir, Arc::new(MemoryKeychain::unavailable()));
        let pi = instances
            .add("Pi", "http://pi.local:8090")
            .expect("the add");
        assert!(!instances.list().keychain);
        assert_eq!(
            instances
                .save_password(&pi.id, USER, SECRET)
                .err()
                .as_deref(),
            Some(NO_KEYCHAIN)
        );
        let settings = DesktopSettings::load(&dir.path().join("desktop.yaml")).expect("reads");
        assert!(!settings.instances[0].save_password);
        assert_eq!(settings.instances[0].user, None);
        assert!(!instances.list().items[1].saved_password);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_remote_with_saved_password_returns_a_token() {
        let dir = ScratchDir::new("desktop-instances-connect-saved");
        let server = server(dir.path(), vec![ORIGIN.to_string()]).await;
        let base = format!("http://{}", server.local_addr());
        let instances = store(&dir, Arc::new(MemoryKeychain::new()));
        let remote = instances.add("Box", &format!("{base}/")).expect("the add");
        instances
            .save_password(&remote.id, USER, SECRET)
            .expect("saved");

        let endpoint = within(instances.connect(&remote.id))
            .await
            .expect("connects");
        assert_eq!(endpoint.kind, "bearer");
        assert_eq!(endpoint.base, base);
        let token = endpoint.token.expect("a session token");
        let listed = within(
            client()
                .get(format!("{base}/api/v1/torrents"))
                .bearer_auth(&token)
                .send(),
        )
        .await
        .expect("an answer");
        assert_eq!(listed.status(), reqwest::StatusCode::OK);

        let probe = within(instances.probe(&remote.id)).await.expect("probes");
        assert!(probe.reachable && probe.cors_ok, "{probe:?}");

        // A saved password the server no longer takes is the sign-in screen, not an error.
        instances
            .save_password(&remote.id, USER, "wrong")
            .expect("saved");
        let endpoint = within(instances.connect(&remote.id))
            .await
            .expect("connects");
        assert_eq!(endpoint.token, None);

        within(server.shutdown()).await.expect("the last save ran");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_remote_without_password_needs_sign_in() {
        let dir = ScratchDir::new("desktop-instances-connect-bare");
        let server = server(dir.path(), Vec::new()).await;
        let base = format!("http://{}", server.local_addr());
        let instances = store(&dir, Arc::new(MemoryKeychain::new()));
        let remote = instances.add("Box", &base).expect("the add");

        let endpoint = within(instances.connect(&remote.id))
            .await
            .expect("connects");
        assert_eq!(endpoint.base, base);
        assert_eq!(
            endpoint.token, None,
            "no saved password, so the page signs in"
        );

        // Up, but this app's origin is not on its list: the page names the origin to add.
        let probe = within(instances.probe(&remote.id)).await.expect("probes");
        assert!(probe.reachable && !probe.cors_ok, "{probe:?}");

        within(server.shutdown()).await.expect("the last save ran");

        // Down: nothing answers, and a saved password cannot change that.
        let probe = within(instances.probe(&remote.id)).await.expect("probes");
        assert!(!probe.reachable && !probe.cors_ok, "{probe:?}");
        instances
            .save_password(&remote.id, USER, SECRET)
            .expect("saved");
        assert_eq!(
            within(instances.connect(&remote.id)).await.err().as_deref(),
            Some(UNREACHABLE)
        );
        assert!(within(instances.connect("nowhere")).await.is_err());
    }
}
