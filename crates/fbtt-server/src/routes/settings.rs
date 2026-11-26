//! The server's own settings, read and written over the API: the limits the engine is running under and
//! the downloads section, saved to the server's `config.yaml` so a change survives a restart.
//!
//! These are product routes, so they arrive behind [`FbttAuth`](crate::auth::FbttAuth) and the browser
//! session's CSRF check exactly like the device and folder routes: who may change a server's settings is
//! the same question as who may add a device. What is decided here is the shape of the answers and what
//! lands in the file.
//!
//! Two things are worth being explicit about.
//!
//! The limits a screen sees are the ones the session is *running* under, read back off it rather than out
//! of the last thing a client asked for: something else - fbt-api's own `/api/v1/session/limits`, a run's
//! own startup - can change them, and a view built from the form would drift the moment it did. So `speed`
//! comes off the session (the same reading `fbtt_core::engine::map::limits` makes for the TUI, which is not
//! reachable from here) and only the *save* writes them down.
//!
//! Every write is ordered the same way - validate, write `config.yaml`, apply - and a write that fails
//! applies nothing. The file is the promise; a limit the file does not have must not take hold in a run that
//! could not record it. A file the server cannot read is answered `500 config_unreadable` and left where it
//! is, with nothing written and the running session untouched; a file that cannot be written is
//! `500 config_unwritable`, saying what the operating system said - neither answer names the config's path
//! or file name, which is where the server's log comes in. The apply is the last step of
//! the same write rather than something that follows it: it runs with the config lock still held, so two
//! changes written in one order cannot take effect in the other.
//!
//! A folder in a submitted section is checked twice over: lexically against the folders this run serves, in
//! both the spelling `config.yaml` gives them and the one the file system resolves them to, and then for real
//! by resolving the deepest part of the path that exists and requiring it inside a root. A link inside the
//! roots that leads out is refused by the second check, and a folder this server does not serve by either.
//!
//! The program a finished torrent runs is the one setting a client may not name on a server: it lives in the
//! server's `config.yaml`, and a request that carries the desktop app's launch secret - and only as a bearer
//! header - is the one thing that may set it. A password or key session, a browser's cookie session, and the
//! secret arriving in a URL all get `403 set_in_config_file`, which is the same line a read-only screen
//! shows.
//!
//! The proxy is written the same way and then applied to the running engine inside the same save, so the
//! engine ends on the last proxy written. Its password never leaves this process: the view says only
//! whether one is stored, a change keeps, clears or replaces it without the form ever holding it, and no
//! answer or log line here carries it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderMap, StatusCode};
use fbt::{RateLimit, Session};
use fbtt_core::fs_list::inside_roots;
use fbtt_core::{
    Config, DownloadsChange, DownloadsView, LiveLimits, OnCompleteView, ProxyChange, ProxyProblem,
    ProxySetting, ProxyType, ProxyView, SettingsProblem, SettingsView, validate_downloads,
};

use super::{RouteError, Shared, clean};
use crate::ProxyPasswordStore;
use crate::auth::locked;

/// How large a settings body may be, in bytes. The two bodies are a handful of numbers and three paths, so
/// this is far more room than they need and far less than fbt-api's 32 MiB for a `.torrent`.
pub(super) const MAX_SETTINGS_BODY: usize = 8 * 1024;

/// One KiB, the unit the limits are stated in. The engine counts bytes per second.
const KIB: u64 = 1024;

/// `GET /api/v1/settings`: the limits the engine is running under and this run's downloads settings, in the
/// one shape every surface reads ([`SettingsView`]).
///
/// `downloads.on_complete.editable` answers about *this* request: whether it carries the launch secret. The
/// configured program is shown either way - the difference a screen draws is between "you cannot change
/// this" and "there is nothing here" - and the two problems the automation last filed are shown beside the
/// fields they are about.
pub(super) async fn get(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
) -> Result<Json<SettingsView>, RouteError> {
    let config = shared.current_config();
    Ok(Json(view(&shared, &config, &headers).await?))
}

/// `PUT /api/v1/settings/speed`: the running rates and queue counts, saved to `config.yaml` so the next run
/// starts under the same limits, answering the view the save left behind.
///
/// The order is the point: the file is written first, and only then is the engine put under the limits. A
/// save that could not be recorded - a config this server cannot read - is answered and applies nothing, so
/// a run never comes up under limits the file does not hold.
pub(super) async fn put_speed(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    given: Result<Json<LiveLimits>, JsonRejection>,
) -> Result<Json<SettingsView>, RouteError> {
    // A body that is not JSON of this shape comes back in this API's error shape rather than axum's
    // plain-text one.
    let Json(limits) = given?;
    // The engine is put under the limits inside the same lock the file was written under, so a speed save
    // racing another one cannot land on the older limits after the newer save has already been written.
    // `LiveLimits` is `Copy`, so the same value goes to both halves.
    let session = shared.session().clone();
    let config = shared
        .write_config(
            move |config| {
                config.limits = limits.into();
                Ok(())
            },
            move |_config| apply_speed(&session, limits),
        )
        .await
        .map_err(|error| error.named(CONFIG_UNREADABLE))?;
    Ok(Json(view(&shared, &config, &headers).await?))
}

/// `PUT /api/v1/settings/downloads`: the folders, the switch and, for the one client allowed to name it, the
/// program to run - saved to `config.yaml`, answering the view the save left behind.
///
/// The checks come before the write, in this order: the program may only be named by a request carrying the
/// launch secret (`403 set_in_config_file`); the section has to be one this server could run, every folder
/// inside the folders it serves (`403 outside_roots` for one that is not, `400 invalid_setting` with the
/// plan's own line for the rest); and every folder has to be somewhere this server may really write, checked
/// against the roots once the deepest part of the path that exists is resolved, so a link inside the roots
/// that leads out is refused the same way. Nothing is written for a refusal. Once it is written, the
/// downloads automation is put under the new settings, inside the same lock the write was made under.
pub(super) async fn put_downloads(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    given: Result<Json<DownloadsChange>, JsonRejection>,
) -> Result<Json<SettingsView>, RouteError> {
    let Json(change) = given?;
    // The program is the one setting a client may not name on a server, whatever account it signed in with:
    // it lives in the server's own `config.yaml`, and only the desktop app that started this engine may set
    // it, through its launch secret. Answered before anything else, so the file is exactly as it was.
    if change.on_complete.is_some() && !shared.program_is_editable(&headers) {
        return Err(RouteError::new(
            StatusCode::FORBIDDEN,
            "set_in_config_file",
            SettingsProblem::NotEditable.message(),
        ));
    }
    // What this server may name: the folders it serves, in both the spelling `config.yaml` gives them and the
    // one the file system resolves them to. The section has to be one this run could work in, and the three
    // folders have to be different places.
    let roots = shared.served_roots_for_validation();
    if let Err(problem) = validate_downloads(&change, Some(&roots)) {
        return Err(problem_error(problem));
    }
    // And every folder has to be really inside one of them, not only spelled that way: below a root as
    // spelled, and below a root again once the deepest part of it that exists is resolved, so a link inside
    // the roots that leads out - or a folder named past one - is refused rather than written through. This
    // asks the file system, so it runs on the blocking pool like every other disk call here.
    folders_are_inside(&shared, &change).await?;
    // The roots this run serves, as spelled: pinned into the file before a change moves the download folder,
    // so the allowed folders do not move with it.
    let roots = shared.served_roots();
    // The automation is put under the settings the file now holds - inside the same lock the write was made
    // under - so the watch folder and the completion program follow a save without a restart, and two saves
    // racing cannot leave it on the older settings. This runs after the write: a write that failed changed
    // nothing and applies nothing.
    let applying = Arc::clone(&shared);
    let config = shared
        .write_config(
            move |config| {
                pin_roots_before_moving(config, &change, &roots);
                config.download_dir = change.default_dir.clone();
                config.downloads.apply(&change);
                Ok(())
            },
            move |config| {
                applying
                    .automation()
                    .update(config.download_dir.clone(), config.downloads.clone());
            },
        )
        .await
        .map_err(|error| error.named(CONFIG_UNREADABLE))?;
    Ok(Json(view(&shared, &config, &headers).await?))
}

/// `PUT /api/v1/settings/proxy`: the proxy every connection goes through, saved to `config.yaml` and
/// applied to the running engine without a restart, answering the view the save left behind.
///
/// The change is resolved against the password the file holds as it is read for the write (`keep` keeps
/// it, `clear` removes it, `{"set": ..}` replaces it) and the setting that leaves is checked with the same
/// rules the engine starts under; one it refuses is `400 invalid_setting` with the line to show, and
/// neither the file nor the engine changes. Keeping a stored password while naming a new host or port is
/// refused the same way: the password is typed again for a new address. `type: none` removes the
/// `proxy:` key and puts back the switches this run started on. The engine is switched inside the same
/// save, so two saves in quick succession leave it on the second. A switch the engine refuses after the
/// write (its new network stack would not build, or the session is shutting down) leaves the engine on
/// the previous setting, so the previous setting is written back to the file and the view, and the
/// answer is `500 proxy_not_applied` with the engine's reason (and the write-back's, if that failed too):
/// never a quiet direct connection behind a file that names a proxy.
pub(super) async fn put_proxy(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    given: Result<Json<ProxyChange>, JsonRejection>,
) -> Result<Json<SettingsView>, RouteError> {
    let Json(change) = given?;
    // What the change step hands the apply step, both inside the same serialized save: the proxy the file
    // held and the password the keychain held before this change, so a failure can put both back, what
    // the keychain holds now, and the setting the engine is to switch to, password included (the file may
    // not hold it).
    let previous: Arc<Mutex<Previous>> = Arc::default();
    let before = Arc::clone(&previous);
    let keychain = shared.proxy_keychain().cloned();
    let applying = Arc::clone(&shared);
    let config = shared
        .write_config_then(
            move |mut config| async move {
                let invalid = |problem: ProxyProblem| {
                    RouteError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_setting",
                        problem.message(),
                    )
                };
                // With a keychain, the stored password is the keychain's (or one the file still holds,
                // which the app's start could not move): `keep` keeps that one, under B3's same rules.
                let held = match &keychain {
                    Some(store) => Shared::keychain_call(store, |store| store.get())
                        .await
                        .map_err(|problem| keychain_error(&problem))?,
                    None => None,
                };
                let mut stored = config.proxy.clone();
                if let Some(setting) = stored.as_mut()
                    && setting.password.as_deref().is_none_or(str::is_empty)
                {
                    setting.password.clone_from(&held);
                }
                let resolved = change.resolve(stored.as_ref()).map_err(invalid)?;
                if let Some(setting) = &resolved {
                    setting.validate().map_err(invalid)?;
                }
                // A password the keychain holds belongs to the address it was given for. When the address
                // changes, it leaves the keychain before the file names the new address, so a crash
                // between the two writes can leave no password but never the old one beside a new host.
                let moving = held.is_some()
                    && match (stored.as_ref(), resolved.as_ref()) {
                        (Some(old), Some(new)) => !old.same_address(new),
                        _ => true,
                    };
                let mut now = held.clone();
                if moving && let Some(store) = &keychain {
                    let cleared = Shared::keychain_call(store, |store| store.clear())
                        .await
                        .and_then(|cleared| cleared);
                    if let Err(problem) = cleared {
                        let problem = clean(&problem);
                        tracing::warn!(problem = %problem, "the old proxy password could not be removed");
                        // Nothing is written yet: the file, the keychain and the engine are as they were.
                        return Err(not_saved(password_not_saved(&problem)));
                    }
                    now = None;
                }
                let mut file = resolved.clone();
                if keychain.is_some()
                    && let Some(setting) = file.as_mut()
                {
                    // The keychain holds it; the file never does.
                    setting.password = None;
                }
                *locked(&before) = Previous {
                    file: config.proxy.clone(),
                    keychain: held,
                    now,
                    engine: resolved,
                };
                config.proxy = file;
                Ok(config)
            },
            move |_config| async move {
                let (engine, keychain_before, keychain_now) = {
                    let previous = locked(&previous);
                    (
                        previous.engine.clone(),
                        previous.keychain.clone(),
                        previous.now.clone(),
                    )
                };
                let wanted = engine.as_ref().and_then(|setting| setting.password.clone());
                let keychain = applying.proxy_keychain().cloned();
                // The keychain is written after the file and before the engine: a keychain that will not
                // take the password leaves the engine on the setting it has, and the file goes back.
                if let Some(store) = &keychain
                    && wanted != keychain_now
                {
                    let writing = wanted.clone();
                    let written = Shared::keychain_call(store, move |store| match &writing {
                        Some(password) => store.set(password),
                        None => store.clear(),
                    })
                    .await
                    .and_then(|written| written);
                    if let Err(problem) = written {
                        let problem = clean(&problem);
                        tracing::warn!(problem = %problem, "the proxy password could not be saved");
                        let keychain_back = restore_keychain(
                            store,
                            keychain_now.as_ref() != keychain_before.as_ref(),
                            keychain_before,
                        )
                        .await;
                        let file = locked(&previous).file.take();
                        let file_back = applying
                            .rewrite_from_apply(move |config| config.proxy = file)
                            .await;
                        return Err(not_saved(keychain_refused(
                            &problem,
                            file_back.err(),
                            keychain_back.err(),
                        )));
                    }
                }
                // The setting just resolved, applied over the settings this run started on: the
                // change step ran under the same lock, so it is always the latest.
                let Err(problem) = fbtt_core::proxy::apply_now(
                    applying.session(),
                    engine.as_ref(),
                    applying.base_settings(),
                )
                .await
                else {
                    return Ok(());
                };
                // The engine refused the new stack (a listen address that will not bind again, a session
                // shutting down) and stays on the previous setting. The file, the keychain and the view go
                // back to it too, still under the same lock, so nothing claims a proxy the engine is not
                // using and the engine is never on a password the keychain does not hold.
                tracing::warn!(problem = %problem, "the saved proxy could not be applied");
                let keychain_back = match &keychain {
                    Some(store) => {
                        restore_keychain(store, wanted != keychain_before, keychain_before).await
                    }
                    None => Ok(()),
                };
                let file = locked(&previous).file.take();
                let file_back = applying
                    .rewrite_from_apply(move |config| config.proxy = file)
                    .await;
                let message = engine_refused(&problem, file_back.err(), keychain_back.err());
                Err(RouteError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "proxy_not_applied",
                    message,
                ))
            },
        )
        .await
        .map_err(|error| error.named(CONFIG_UNREADABLE))?;
    Ok(Json(view(&shared, &config, &headers).await?))
}

/// What a proxy save's change step hands its apply step. Its `Debug` is never asked for: it holds
/// passwords.
#[derive(Default)]
struct Previous {
    /// The `proxy:` section `config.yaml` held before the change, to write back on a failure.
    file: Option<ProxySetting>,
    /// The password the keychain held before the change, to put back on a failure. Always `None` with no
    /// keychain.
    keychain: Option<String>,
    /// The password the keychain holds once the change step is done: the old one, or none when the
    /// address changed and the old one was taken out before the save.
    now: Option<String>,
    /// The setting the engine is to switch to, with its password.
    engine: Option<ProxySetting>,
}

/// Puts `password` back in the keychain (or clears it, for none) when `changed` says the keychain no
/// longer holds it. The error is the display-safe line saying the keychain would not.
async fn restore_keychain(
    store: &Arc<dyn ProxyPasswordStore>,
    changed: bool,
    password: Option<String>,
) -> Result<(), String> {
    if !changed {
        return Ok(());
    }
    Shared::keychain_call(store, move |store| match &password {
        Some(password) => store.set(password),
        None => store.clear(),
    })
    .await
    .and_then(|restored| restored)
    .map_err(|problem| {
        let problem = clean(&problem);
        tracing::warn!(problem = %problem, "the previous proxy password could not be put back");
        problem
    })
}

/// A proxy save the keychain refused: `500 proxy_password_not_saved` with `message`.
fn not_saved(message: String) -> RouteError {
    RouteError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "proxy_password_not_saved",
        message,
    )
}

/// What a proxy save answers when the keychain would not take the new password (or let the old one
/// go) and nothing changed: `problem` is the keychain's display-safe reason.
fn password_not_saved(problem: &str) -> String {
    format!(
        "The keychain did not save the proxy password, so the previous setting is kept: {problem}"
    )
}

/// The answer to a keychain that refused the new password after the file was written: what failed, and
/// what could not be put back. The engine was never asked. Every reason is display-safe.
fn keychain_refused(problem: &str, file: Option<String>, keychain: Option<String>) -> String {
    match (file, keychain) {
        (None, None) => password_not_saved(problem),
        (Some(file), None) => format!(
            "The keychain did not save the proxy password, and config.yaml could not be put back and \
             names the new proxy: {file}. The engine is still on the previous setting. The keychain's \
             reason: {problem}"
        ),
        (None, Some(keychain)) => format!(
            "The keychain did not save the proxy password, and it could not get the previous password \
             back: {keychain}. config.yaml names the previous proxy again and the engine is still on it. \
             The keychain's reason: {problem}"
        ),
        (Some(file), Some(keychain)) => format!(
            "The keychain did not save the proxy password, config.yaml could not be put back and names \
             the new proxy ({file}), and the keychain could not get the previous password back \
             ({keychain}). The engine is still on the previous setting. The keychain's reason: {problem}"
        ),
    }
}

/// The answer to an engine that refused to switch: what was put back, and what could not be. Every reason
/// is display-safe.
fn engine_refused(problem: &str, file: Option<String>, keychain: Option<String>) -> String {
    match (file, keychain) {
        (None, None) => fbtt_core::proxy::not_applied(problem),
        (Some(file), None) => fbtt_core::proxy::not_rolled_back(problem, &file),
        (None, Some(keychain)) => format!(
            "The engine could not switch to this proxy and is still on the previous setting, and \
             config.yaml names it again, but the keychain could not get the previous password back: \
             {keychain}. The engine's reason: {problem}"
        ),
        (Some(file), Some(keychain)) => fbtt_core::proxy::not_rolled_back(
            problem,
            &format!(
                "{file}; the keychain could not get the previous password back either: {keychain}"
            ),
        ),
    }
}

/// Whether every folder a submitted section names is somewhere this server may really write: below a root as
/// spelled, and below a root again once the deepest part of the path that exists is resolved
/// ([`inside_roots`]). A folder outside every root is `403 outside_roots`.
///
/// The check asks the file system for each folder, so it runs on the blocking pool: a request waits on it,
/// and a config directory on a slow disk must not hold the connection's task while it does.
async fn folders_are_inside(
    shared: &Arc<Shared>,
    change: &DownloadsChange,
) -> Result<(), RouteError> {
    let roots = Arc::clone(&shared.roots);
    let folders: Vec<PathBuf> = submitted_folders(change).map(Path::to_path_buf).collect();
    let inside = tokio::task::spawn_blocking(move || {
        folders.iter().all(|folder| inside_roots(&roots, folder))
    })
    .await
    .map_err(|problem| {
        tracing::warn!(problem = %problem, "the downloads folders could not be checked");
        RouteError::internal("the downloads folder could not be checked")
    })?;
    if inside {
        Ok(())
    } else {
        Err(problem_error(SettingsProblem::OutsideRoots))
    }
}

/// The error code a settings write answers when `config.yaml` cannot be read for edit: the same `500` a
/// device add gives, under the name a client of these routes reads. A config that cannot be *written* keeps
/// its own `config_unwritable` ([`Shared::write_config`]).
const CONFIG_UNREADABLE: &str = "config_unreadable";

/// The view one request reads: the live limits and the downloads section, with `editable` answering about
/// this request's own credential. A keychain that could not be asked whether it holds the proxy password
/// is a `500`.
async fn view(
    shared: &Shared,
    config: &Config,
    headers: &HeaderMap,
) -> Result<SettingsView, RouteError> {
    let (watch_problem, on_complete_problem) = shared.automation().problems();
    Ok(SettingsView {
        speed: live_limits(shared.session()),
        downloads: DownloadsView {
            default_dir: config.download_dir.clone(),
            unfinished_dir: config.downloads.unfinished_dir.clone(),
            start_paused: config.downloads.start_paused,
            watch_dir: config.downloads.watch_dir.clone(),
            on_complete: OnCompleteView {
                program: config.downloads.on_complete.clone(),
                editable: shared.program_is_editable(headers),
            },
            watch_problem,
            on_complete_problem,
        },
        proxy: proxy_view(shared, config).await?,
    })
}

/// The proxy section of the view: whether a password is stored, never the password, and where it is kept.
/// A server keeps it in `config.yaml`; the desktop app's engine in the keychain when the system has one.
async fn proxy_view(shared: &Shared, config: &Config) -> Result<ProxyView, RouteError> {
    let mut view = ProxyView::of(config.proxy.as_ref(), shared.proxy_password_storage());
    if view.kind != ProxyType::None
        && !view.password_saved
        && let Some(store) = shared.proxy_keychain()
    {
        view.password_saved = Shared::keychain_call(store, |store| {
            store.get().is_some_and(|password| !password.is_empty())
        })
        .await
        .map_err(|problem| keychain_error(&problem))?;
    }
    Ok(view)
}

/// A keychain the blocking pool could not ask: `500` with the line to show.
fn keychain_error(problem: &str) -> RouteError {
    RouteError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "keychain_unavailable",
        clean(problem),
    )
}

/// The limits the session is running under, in KiB/s. The engine counts bytes per second, so a limit that is
/// not a whole KiB shows as the KiB below it. This is the same reading
/// [`fbtt_core::engine::map::limits`] makes for the TUI; that one is `pub(crate)` to its own crate, so a
/// server that needs it reads the session itself rather than reaching across.
fn live_limits(session: &Session) -> LiveLimits {
    let rate = session.rate_limit();
    let queue = session.queue_settings();
    LiveLimits {
        download_kib: rate.download / KIB,
        upload_kib: rate.upload / KIB,
        active_downloads: queue.active_downloads,
        active_seeds: queue.active_seeds,
        active_total: queue.active_limit,
    }
}

/// Puts the engine under `limits`, the step that follows a successful write.
///
/// The rates go straight in; the queue counts the engine validates itself and answers about, and nothing in
/// a request can wait on that answer, so a refusal is written where whoever runs this server reads such
/// things. `fbt::SessionError` is non-exhaustive, so it is reported rather than matched.
fn apply_speed(session: &Session, limits: LiveLimits) {
    session.set_rate_limit(RateLimit::new(
        limits.download_kib.saturating_mul(KIB),
        limits.upload_kib.saturating_mul(KIB),
    ));
    let mut queue = session.queue_settings();
    queue.active_downloads = limits.active_downloads;
    queue.active_seeds = limits.active_seeds;
    queue.active_limit = limits.active_total;
    if let Err(problem) = session.set_queue_settings(queue) {
        tracing::warn!(problem = %problem, "the queue settings could not be applied");
    }
}

/// The folders a submitted section names, so the canonical check can see each one.
fn submitted_folders(change: &DownloadsChange) -> impl Iterator<Item = &Path> {
    [
        Some(change.default_dir.as_path()),
        change.unfinished_dir.as_deref(),
        change.watch_dir.as_deref(),
    ]
    .into_iter()
    .flatten()
}

/// The folders this run serves, pinned into the file before a change moves the download folder, so the
/// allowed folders do not move with it.
///
/// An empty `server.allowed_roots` in the file means "the download folder", which [`Config::resolve`] fills
/// in at startup with the folders this run serves. Writing a new download folder without first naming the
/// roots would leave the next run with "the *new* download folder" as its allowed folders, so a change that
/// moves it pins the roots this run serves into the file first. This is done only when the file already has
/// a `server` section: adding one where there was none would choose the defaults for an address this server
/// is not serving on, and a file that named its roots already keeps them.
fn pin_roots_before_moving(config: &mut Config, change: &DownloadsChange, roots: &[PathBuf]) {
    if config.download_dir == change.default_dir {
        return;
    }
    if let Some(server) = config.server.as_mut()
        && server.allowed_roots.is_empty()
    {
        server.allowed_roots = roots.to_vec();
    }
}

/// A refused downloads section in this API's error shape: `403 outside_roots` for a folder the server does
/// not serve, `400 invalid_setting` for anything else, each with the plan's own line for the problem.
fn problem_error(problem: SettingsProblem) -> RouteError {
    match problem {
        SettingsProblem::OutsideRoots => {
            RouteError::new(StatusCode::FORBIDDEN, "outside_roots", problem.message())
        }
        _ => RouteError::new(
            StatusCode::BAD_REQUEST,
            "invalid_setting",
            problem.message(),
        ),
    }
}
