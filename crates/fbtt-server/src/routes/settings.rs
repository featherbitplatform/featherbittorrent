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
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderMap, StatusCode};
use fbt::{RateLimit, Session};
use fbtt_core::fs_list::inside_roots;
use fbtt_core::{
    Config, DownloadsChange, DownloadsView, LiveLimits, OnCompleteView, PasswordStorage,
    ProxyChange, ProxyView, SettingsProblem, SettingsView, validate_downloads,
};

use super::{RouteError, Shared};

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
) -> Json<SettingsView> {
    let config = shared.current_config();
    Json(view(&shared, &config, &headers))
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
    Ok(Json(view(&shared, &config, &headers)))
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
    Ok(Json(view(&shared, &config, &headers)))
}

/// `PUT /api/v1/settings/proxy`: the proxy every connection goes through, saved to `config.yaml` and
/// applied to the running engine without a restart, answering the view the save left behind.
///
/// The change is resolved against the password the file holds as it is read for the write (`keep` keeps
/// it, `clear` removes it, `{"set": ..}` replaces it) and the setting that leaves is checked with the same
/// rules the engine starts under; one it refuses is `400 invalid_setting` with the line to show, and
/// neither the file nor the engine changes. `type: none` removes the `proxy:` key and puts back the
/// switches this run started on. The engine is switched inside the same save, so two saves in quick
/// succession leave it on the second; a switch that fails after the write (a session shutting down) is
/// `500 proxy_not_applied` with the engine's reason, never a quiet direct connection.
pub(super) async fn put_proxy(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    given: Result<Json<ProxyChange>, JsonRejection>,
) -> Result<Json<SettingsView>, RouteError> {
    let Json(change) = given?;
    let applying = Arc::clone(&shared);
    let config = shared
        .write_config_then(
            move |config| {
                let resolved = change.resolve(config.proxy.as_ref());
                if let Some(setting) = &resolved
                    && let Err(problem) = setting.validate()
                {
                    return Err(RouteError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_setting",
                        problem.message(),
                    ));
                }
                config.proxy = resolved;
                Ok(())
            },
            move |config| {
                // The setting the file now holds, applied over the settings this run started on: the
                // config handed here is the one just written, under the lock, so it is always the latest.
                let proxy = config.proxy.clone();
                async move {
                    fbtt_core::proxy::apply_now(
                        applying.session(),
                        proxy.as_ref(),
                        applying.base_settings(),
                    )
                    .await
                    .map_err(|problem| {
                        tracing::warn!(problem = %problem, "the saved proxy could not be applied");
                        RouteError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "proxy_not_applied",
                            format!(
                                "The proxy is saved, but the engine could not switch to it: {problem}"
                            ),
                        )
                    })
                }
            },
        )
        .await
        .map_err(|error| error.named(CONFIG_UNREADABLE))?;
    Ok(Json(view(&shared, &config, &headers)))
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
/// this request's own credential.
fn view(shared: &Shared, config: &Config, headers: &HeaderMap) -> SettingsView {
    let (watch_problem, on_complete_problem) = shared.automation().problems();
    SettingsView {
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
        // A server keeps the proxy password in `config.yaml`; the view says only whether there is one.
        proxy: ProxyView::of(config.proxy.as_ref(), PasswordStorage::ConfigFile),
    }
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
