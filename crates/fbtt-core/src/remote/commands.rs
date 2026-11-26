//! What a remote backend does with one command: which route to call, and what to remember from the answer.
//!
//! Every one of these is an answer to a [`Command`] a screen sent, and every one of them publishes a
//! snapshot rather than replying directly - success needs no line, because the table changing is the
//! answer. What a command cost when it failed is reported by [`Backend::report`], not here.

use std::path::PathBuf;

use crate::client::{
    AddSource, Command, DeviceCommand, DirListing, FilePriority, LiveLimits, Notice,
    SettingsChange, SettingsCommand, SettingsState, TorrentId,
};
use crate::engine::map::{clean, rate_bytes};
use crate::remote::bodies::ApiError;
use crate::remote::routes::{encode, torrent_route};
use crate::remote::{Backend, map};
use crate::settings::SettingsView;

impl Backend {
    /// One command, answered. `Shutdown` never reaches here: the loop stops on it before doing any work.
    pub(crate) async fn handle(&mut self, command: Command) {
        match command {
            Command::Add { source, dir } => self.add(source, dir).await,
            Command::Pause(id) => {
                let path = torrent_route(&id.0, "/pause");
                self.report(self.api.empty(self.api.post(&path)).await);
            }
            Command::Resume(id) => {
                let path = torrent_route(&id.0, "/resume");
                self.report(self.api.empty(self.api.post(&path)).await);
            }
            Command::Remove { id, delete_files } => {
                let path = format!("{}?delete_files={}", torrent_route(&id.0, ""), delete_files);
                self.report(self.api.empty(self.api.delete(&path)).await);
            }
            Command::SetFilePriority { id, file, priority } => {
                self.set_file_priority(&id, file, priority).await;
            }
            Command::SetLimits(limits) => self.set_limits(limits).await,
            // The pane the screen just chose is answered by the read that follows it, so it appears now
            // rather than on the next tick.
            Command::Watch(target) => {
                self.watching = target;
                self.refresh().await;
            }
            Command::ListDirs(path) => self.dirs = Some(self.list_dirs(path).await),
            Command::Devices(command) => self.devices(command).await,
            Command::Settings(command) => self.settings(command).await,
            // Playing a file: a stream address from the server's token route, checked
            // before it reaches a player, through the relay for a gateway target.
            Command::Stream { id, file } => self.stream(&id, file).await,
            Command::Shutdown => {}
        }
    }

    /// Adds a torrent: a magnet as JSON, the bytes of a `.torrent` as themselves. The server names the
    /// torrent it took in its answer, which is the name to repeat back - for a magnet its `dn`, until
    /// peers confirm what the torrent actually is.
    pub(crate) async fn add(&mut self, source: AddSource, dir: Option<PathBuf>) {
        let save_path = dir.unwrap_or_else(|| self.default_dir.clone());
        let request = match source {
            AddSource::Magnet(uri) => self.api.post("/api/v1/torrents").json(&map::MagnetBody {
                magnet: &uri,
                save_path: &save_path.to_string_lossy(),
            }),
            // The body *is* the torrent file, with its own content type; where it goes is a query
            // parameter because there is no JSON body left to put it in.
            AddSource::File(bytes) => self
                .api
                .post(&format!(
                    "/api/v1/torrents?save_path={}",
                    encode(save_path.to_string_lossy().as_ref())
                ))
                .header(reqwest::header::CONTENT_TYPE, "application/x-bittorrent")
                .body(bytes),
        };
        match self.api.json::<map::TorrentDto>(request).await {
            Ok(dto) => {
                let name = clean(&dto.name);
                // A magnet whose metadata has not arrived is named by its hash, which is server text too and
                // gets the same cut as everything else this client repeats back.
                let shown = if name.is_empty() {
                    clean(&dto.id)
                } else {
                    name
                };
                self.notify(Notice::Info(format!("Added {shown}")));
            }
            Err(problem) => self.notify(problem.notice()),
        }
    }

    /// One file's priority. The server wants one entry per file including pads, so its own list is read
    /// back, the named entry changed, and the whole thing sent again - as the local backend does.
    pub(crate) async fn set_file_priority(
        &mut self,
        id: &TorrentId,
        file: usize,
        priority: FilePriority,
    ) {
        let path = torrent_route(&id.0, "/files");
        let files = match self
            .api
            .json::<Vec<map::FileDto>>(self.api.get(&path))
            .await
        {
            Ok(files) => files,
            Err(problem) => return self.report(Err(problem)),
        };
        let body = match map::priorities_with(&files, file, priority) {
            Some(body) => body,
            // Ids and indices both come from the screen, and a pane that has not caught up with a removal
            // names an index that is no longer there.
            None => {
                return self.notify(Notice::Error(format!("no file with index {file}")));
            }
        };
        let path = torrent_route(&id.0, "/file-priorities");
        self.report(self.api.empty(self.api.put_json(&path, &body)).await);
    }

    /// Changes the running rates and queue counts. The rates have their own route; the counts are read
    /// back first because a `PUT` replaces all eight of them together.
    pub(crate) async fn set_limits(&mut self, limits: LiveLimits) {
        let path = "/api/v1/session/limits";
        let body = map::LimitsBody {
            // The server counts bytes per second; the screen states KiB/s.
            download: rate_bytes(limits.download_kib),
            upload: rate_bytes(limits.upload_kib),
        };
        if let Err(problem) = self.api.empty(self.api.put_json(path, &body)).await {
            return self.report(Err(problem));
        }
        let mut queue = match self.queue.clone() {
            Some(queue) => queue,
            None => {
                match self
                    .api
                    .json::<map::QueueDto>(self.api.get("/api/v1/session/queue"))
                    .await
                {
                    Ok(queue) => queue,
                    Err(problem) => return self.report(Err(problem)),
                }
            }
        };
        queue.active_downloads = limits.active_downloads;
        queue.active_seeds = limits.active_seeds;
        queue.active_limit = limits.active_total;
        let path = "/api/v1/session/queue";
        match self.api.empty(self.api.put_json(path, &queue)).await {
            Ok(()) => {
                // Take the new counts back in so the next read of the limits says what we just set, and a
                // later change starts from the same numbers rather than a stale copy.
                self.queue = Some(queue);
                self.limits = LiveLimits {
                    download_kib: limits.download_kib,
                    upload_kib: limits.upload_kib,
                    active_downloads: limits.active_downloads,
                    active_seeds: limits.active_seeds,
                    active_total: limits.active_total,
                };
            }
            Err(problem) => self.report(Err(problem)),
        }
    }

    /// Which folders may be named as a destination: the roots when none was asked about, and otherwise
    /// what is directly inside the one that was. A folder this server will not list says so inside the
    /// listing rather than as a notice, because a picker shows that line beside the folder it failed on.
    pub(crate) async fn list_dirs(&mut self, path: Option<PathBuf>) -> DirListing {
        let roots = self.roots.clone();
        let Some(path) = path else {
            return DirListing {
                path: None,
                roots,
                dirs: Vec::new(),
                error: None,
            };
        };
        let url = format!(
            "/api/v1/fs/list?path={}",
            encode(path.to_string_lossy().as_ref())
        );
        match self.api.json::<map::ListingDto>(self.api.get(&url)).await {
            Ok(listing) => DirListing {
                path: Some(path),
                roots,
                // Folder names are the server's text about somebody's disk, so they get the same cut and
                // clean as every other string a screen draws.
                dirs: listing.dirs.into_iter().map(|dir| clean(&dir)).collect(),
                error: None,
            },
            Err(problem) => DirListing {
                path: Some(path),
                roots,
                dirs: Vec::new(),
                error: Some(clean(&problem.line())),
            },
        }
    }

    /// The devices this server lets sign in, and what the screen asked to do with one. After an add or a
    /// removal the list is read again rather than patched by hand: the server keeps names unique and trims
    /// them, so its answer is the one to show.
    pub(crate) async fn devices(&mut self, command: DeviceCommand) {
        match command {
            DeviceCommand::List => self.refresh_devices().await,
            DeviceCommand::Add { name, public_key } => {
                let body = map::NewDeviceBody {
                    name: &name,
                    public_key: &public_key,
                };
                match self
                    .api
                    .empty(self.api.post_json("/api/v1/auth/devices", &body))
                    .await
                {
                    Ok(()) => self.refresh_devices().await,
                    Err(problem) => self.notify(problem.notice()),
                }
            }
            DeviceCommand::Remove(name) => {
                let path = format!("/api/v1/auth/devices/{}", encode(&name));
                match self.api.empty(self.api.delete(&path)).await {
                    Ok(()) => self.refresh_devices().await,
                    Err(problem) => self.notify(problem.notice()),
                }
            }
        }
    }

    /// `GET /api/v1/auth/devices`, taken in as the answer the next snapshot carries.
    pub(crate) async fn refresh_devices(&mut self) {
        match self
            .api
            .json::<Vec<map::DeviceDto>>(self.api.get("/api/v1/auth/devices"))
            .await
        {
            Ok(devices) => self.devices = Some(map::device_rows(devices)),
            Err(problem) => {
                // A server with authentication off has no device list to give at all, which is worth
                // saying; a server that is down is already being reported by the refresh loop.
                self.notify(problem.notice());
            }
        }
    }

    /// One [`Command::Settings`]: `GET /api/v1/settings` to load, and the matching `PUT` to save. The
    /// server's answer to either is the whole view, which is what the next snapshot carries. A server
    /// older than these routes answers `404`, which is [`SettingsState::Unsupported`] and nothing worse:
    /// its limits still save through [`Command::SetLimits`].
    pub(crate) async fn settings(&mut self, command: SettingsCommand) {
        self.settings_round(command, false).await;
    }

    /// The same, and with `quiet` an answer that is an error says nothing: the read made on connecting
    /// is for learning the default folder, not something a person asked for.
    ///
    /// The view's paths are the server's own text and are kept raw, because they are values that go back
    /// to the server; a surface must pass them through [`fbt::display_safe`] when it draws them.
    pub(crate) async fn settings_round(&mut self, command: SettingsCommand, quiet: bool) {
        let saving = !matches!(command, SettingsCommand::Load);
        let request = match command {
            SettingsCommand::Load => self.api.get("/api/v1/settings"),
            SettingsCommand::Save(SettingsChange::Speed(limits)) => {
                self.api.put_json("/api/v1/settings/speed", &limits)
            }
            SettingsCommand::Save(SettingsChange::Downloads(mut change)) => {
                // The program is the server's own config file's to hold: a remote client never names it,
                // whatever the screen handed in, and an absent key keeps what the file has.
                change.on_complete = None;
                self.api.put_json("/api/v1/settings/downloads", &change)
            }
        };
        match self.api.json::<SettingsView>(request).await {
            Ok(mut view) => {
                // Both lines are the server's text about its own disk.
                view.downloads.watch_problem =
                    view.downloads.watch_problem.map(|line| clean(&line));
                view.downloads.on_complete_problem =
                    view.downloads.on_complete_problem.map(|line| clean(&line));
                if saving {
                    // What the server says it is running under is the truth until the next read.
                    self.limits = view.speed;
                    self.notify(Notice::Info("Saved.".to_owned()));
                }
                // Where a torrent goes when the add names no folder, and what the snapshot shows as the
                // default: the server's own answer, since its roots stay where they were.
                self.default_dir = view.downloads.default_dir.clone();
                self.settings = SettingsState::Loaded(view);
            }
            Err(ApiError::NotFound(_)) => {
                self.settings = SettingsState::Unsupported;
                if saving && !quiet {
                    self.notify(Notice::Error(TOO_OLD.to_owned()));
                }
            }
            Err(_) if quiet => {}
            Err(problem) => self.notify(problem.notice()),
        }
    }
}

/// What a save answers when the server has no settings routes: said in the plan's own words.
const TOO_OLD: &str =
    "This server is too old to change its settings here. Update it to change them.";
