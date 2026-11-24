//! The player's Download, in the app: a save dialog, and the file streamed to the place chosen.
//!
//! In a browser the page clicks an `<a download>` on its own origin and the browser saves the file. In
//! the app the page's origin (`tauri://localhost`, or `http://tauri.localhost` on Windows) is not the
//! engine's, and a webview ignores `download` on a link to another origin: the click becomes a navigation
//! of the main window to the media URL, which shows a bare video in place of the app. So the shell does
//! it: [`choose_download_place`] asks where to put the file and answers an opaque handle, and only then
//! does the page fetch a stream address (its token lives 60 seconds, which a person reading a dialog can
//! outlast) and hand it, with the handle, to [`save_download`], which streams the file there chunk by
//! chunk. A movie of several gigabytes never sits in memory, which fetching it into a
//! `Blob` on the page would have done. The page watches the file being written (each
//! [`Progress`] goes to it over a channel) and can stop it by the same handle it started it with;
//! quitting the app stops the running downloads and takes their part files away.
//!
//! The shell fetches only what the page could have opened itself: an address on the instance the page
//! is connected to, at that instance's file-stream route ([`stream_target`]). Anything else is refused
//! before a request goes out, so the command cannot be used to make the shell fetch some other address.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use fbtt_desktop_core::{LOCAL_ID, launch_secret};
use tauri::{AppHandle, Manager, Runtime};

use crate::commands::DesktopState;
use crate::instances::{Instances, normalise_base};
use crate::lifecycle::MAIN_WINDOW;
use crate::startup::START_WAIT;

/// How long the shell waits to reach the instance.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the answer may go quiet before the download is given up. There is no limit on the whole
/// download: a large file from a remote instance takes as long as it takes.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// The file-stream route the token route hands out addresses on, before the torrent's hash.
const STREAM_PREFIX: &str = "/api/v1/torrents/";

/// What the page is told when the address it handed over is not one this command fetches.
pub const NOT_A_STREAM: &str = "That is not a file of this instance.";

/// What the page is told when the file could not be saved.
const SAVE_FAILED: &str = "The file could not be saved.";

/// What the page is told, and shows, when the person stops a running download.
pub const CANCELLED: &str = "Download cancelled.";

/// How often at most a running download reports what it has written, besides the reports at
/// the start (nothing written yet) and at the end (the whole file).
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// What a running download reports: `received` bytes written so far, and `total`, the answer's
/// Content-Length when it brings one, which the person's file may not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Progress {
    pub received: u64,
    pub total: Option<u64>,
}

/// The address to fetch, when `url` is a file-stream address on the instance at `base`: the same
/// scheme, host and port, `base`'s own path (if it has one) followed by
/// `/api/v1/torrents/<hash>/files/<index>/stream`, and at most a `token` in the query. `None` for
/// anything else.
pub fn stream_target(base: &str, url: &str) -> Option<url::Url> {
    let base = normalise_base(base);
    if base.is_empty() {
        return None;
    }
    // The prefix check is on the text, as the page's `isOwnUrl` does it, and then the whole address is
    // parsed again and compared part by part, so a spelling that parses to another host is caught too.
    let rest = url.strip_prefix(base.as_str())?;
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    if path.contains('#') || query.is_some_and(|query| query.contains('#')) {
        return None;
    }
    if !is_stream_path(path) {
        return None;
    }
    if let Some(query) = query
        && !is_token_query(query)
    {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    let own = url::Url::parse(&base).ok()?;
    let same_origin = parsed.scheme() == own.scheme()
        && parsed.host_str() == own.host_str()
        && parsed.port_or_known_default() == own.port_or_known_default()
        && parsed.username().is_empty()
        && parsed.password().is_none();
    let expected_path = format!("{}{path}", own.path().trim_end_matches('/'));
    (matches!(parsed.scheme(), "http" | "https") && same_origin && parsed.path() == expected_path)
        .then_some(parsed)
}

/// `/api/v1/torrents/<40 lowercase hex>/files/<decimal>/stream`, spelled the way the token route spells it.
fn is_stream_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix(STREAM_PREFIX) else {
        return false;
    };
    let mut parts = rest.split('/');
    let (Some(hash), Some("files"), Some(index), Some("stream"), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };
    hash.len() == 40
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && !index.is_empty()
        && index.bytes().all(|b| b.is_ascii_digit())
}

/// A query of one `token=<value>` and nothing else.
fn is_token_query(query: &str) -> bool {
    query
        .strip_prefix("token=")
        .is_some_and(|value| !value.is_empty() && !value.contains('&'))
}

/// The name the save dialog suggests: the last part of the file's name, with nothing in it that names a
/// folder. The person can change it, and the dialog decides where it goes.
pub fn suggested_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    let clean: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let clean = clean.trim_matches('.').trim();
    if clean.is_empty() {
        "download".to_string()
    } else {
        clean.to_string()
    }
}

/// The client the download is fetched with: no redirects (a redirect would take the request off the
/// instance [`stream_target`] checked), a connect deadline and a stall deadline, and no deadline on the
/// whole answer.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(STALL_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("the download client builds with its timeouts and no redirects")
}

/// Fetches `url` with `client` into `destination`, chunk by chunk, telling `on_progress` what has
/// been written and watching `cancel` for the person stopping it. The bytes go to a `.part` file
/// next to the destination first, which takes the final name only once the whole answer arrived,
/// so a broken or cancelled download never leaves a short file under the name the person chose.
/// Progress goes out at most [`PROGRESS_EVERY`] apart, plus once at the start and once at the end.
/// A cancel stops reading, drops the answer (which closes the connection), and takes the part
/// file away. Answers how many bytes were saved.
pub async fn save_to(
    client: &reqwest::Client,
    url: url::Url,
    destination: &Path,
    mut on_progress: impl FnMut(Progress),
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<u64, String> {
    // A cancel that arrived before the fetch even started leaves nothing to take away.
    if *cancel.borrow() {
        return Err(CANCELLED.to_string());
    }
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "Cannot reach the server.".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "The server did not send the file (HTTP {}).",
            response.status().as_u16()
        ));
    }
    let total = response.content_length();
    let (partial, mut file) = create_partial(destination).await?;
    let result = async {
        let mut written = 0u64;
        on_progress(Progress { received: 0, total });
        let mut since = tokio::time::Instant::now();
        let mut watching = true;
        loop {
            let chunk = tokio::select! {
                result = response.chunk() => result.map_err(|_| "The download broke off.".to_string())?,
                changed = cancel.changed(), if watching => {
                    match changed {
                        Ok(()) if *cancel.borrow_and_update() => return Err(CANCELLED.to_string()),
                        Ok(_) => {}
                        // The sender is gone: nothing can cancel this any more, and asking
                        // again would only spin. Stop watching.
                        Err(_) => watching = false,
                    }
                    continue;
                }
            };
            let Some(chunk) = chunk else { break };
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(|_| SAVE_FAILED.to_string())?;
            written += chunk.len() as u64;
            if since.elapsed() >= PROGRESS_EVERY {
                since = tokio::time::Instant::now();
                on_progress(Progress { received: written, total });
            }
        }
        // A cancel that arrived after the last byte still cancels: the part file comes down
        // rather than being renamed into the name the person chose.
        if *cancel.borrow_and_update() {
            return Err(CANCELLED.to_string());
        }
        on_progress(Progress { received: written, total });
        tokio::io::AsyncWriteExt::flush(&mut file)
            .await
            .map_err(|_| SAVE_FAILED.to_string())?;
        file.sync_all().await.map_err(|_| SAVE_FAILED.to_string())?;
        Ok(written)
    }
    .await;
    // The answer goes with a cancelled or broken read: dropping it closes the connection.
    drop(response);
    drop(file);
    let result = match result {
        Ok(written) => {
            // A cancel that landed while the file was flushed and synced still cancels: this is
            // the last point where it can, right where the rename happens.
            if *cancel.borrow() {
                Err(CANCELLED.to_string())
            } else {
                tokio::fs::rename(&partial, destination)
                    .await
                    .map(|()| written)
                    .map_err(|_| SAVE_FAILED.to_string())
            }
        }
        Err(problem) => Err(problem),
    };
    if result.is_err() {
        // Only the part file this download made itself: it was created new, so it is nobody else's.
        let _ = tokio::fs::remove_file(&partial).await;
    }
    result
}

/// How many names a part file is tried under before the download gives up.
const PART_ATTEMPTS: usize = 8;

/// A part file of this download's own next to `destination`: `<name>.<random>.part`, created new, so two
/// downloads to the same name and a file of the person's own that happens to end `.part` are never
/// written over. A name that is taken is tried again with another random tag.
async fn create_partial(destination: &Path) -> Result<(PathBuf, tokio::fs::File), String> {
    for _ in 0..PART_ATTEMPTS {
        let partial = partial_path(destination, &random_tag());
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .await
        {
            Ok(file) => return Ok((partial, file)),
            Err(problem) if problem.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(SAVE_FAILED.to_string()),
        }
    }
    Err(SAVE_FAILED.to_string())
}

/// A short random tag for a part file's name: file-name safe (URL-safe base64).
fn random_tag() -> String {
    launch_secret().chars().take(12).collect()
}

/// `destination` with `.<tag>.part` added to its name.
fn partial_path(destination: &Path, tag: &str) -> PathBuf {
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{tag}.part"));
    destination.with_file_name(name)
}

/// How many chosen places are kept waiting for their download.
const MAX_PLACES: usize = 16;

/// The places the person chose in the save dialog, each under an opaque handle the page holds until it
/// has a fresh stream address to fetch. The page never sees a path: it gets the handle, and hands it back
/// with the address. At most [`MAX_PLACES`] are kept; an older one the page never came back for is
/// forgotten first. Beside the places, the running downloads: each registered handle carries the
/// switch [`cancel`](Self::cancel) throws, so the page can stop a download by the same handle it
/// started it with, and quitting can stop them all.
#[derive(Debug, Default)]
pub struct Downloads {
    places: Mutex<VecDeque<(String, PathBuf)>>,
    running: Mutex<std::collections::HashMap<String, tokio::sync::watch::Sender<bool>>>,
}

impl Downloads {
    /// No place chosen yet, no download running.
    pub fn new() -> Self {
        Downloads::default()
    }

    fn places(&self) -> std::sync::MutexGuard<'_, VecDeque<(String, PathBuf)>> {
        self.places
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn running(
        &self,
    ) -> std::sync::MutexGuard<
        '_,
        std::collections::HashMap<String, tokio::sync::watch::Sender<bool>>,
    > {
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Keeps `path` and answers the handle it is kept under.
    pub fn keep(&self, path: PathBuf) -> String {
        let handle = launch_secret();
        let mut places = self.places();
        if places.len() == MAX_PLACES {
            places.pop_front();
        }
        places.push_back((handle.clone(), path));
        handle
    }

    /// The place kept under `handle`, taken out: a handle opens one download.
    pub fn take(&self, handle: &str) -> Option<PathBuf> {
        let mut places = self.places();
        let index = places.iter().position(|(kept, _)| kept == handle)?;
        places.remove(index).map(|(_, path)| path)
    }

    /// Starts watching for the cancel of the download the page opened under `handle`: the answer
    /// is the switch [`save_to`] reads, and [`cancel`](Self::cancel) throws, until
    /// [`finish`](Self::finish) takes the handle back out.
    pub fn register(&self, handle: &str) -> tokio::sync::watch::Receiver<bool> {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        self.running().insert(handle.to_string(), sender);
        receiver
    }

    /// The download under `handle` is over (saved, failed, or cancelled): the handle stops
    /// meaning "running".
    pub fn finish(&self, handle: &str) {
        self.running().remove(handle);
    }

    /// Stops the download running under `handle`. False when none is running under it: a finished
    /// download is not cancellable.
    pub fn cancel(&self, handle: &str) -> bool {
        match self.running().get(handle) {
            Some(sender) => {
                let _ = sender.send(true);
                true
            }
            None => false,
        }
    }

    /// Throws the switch of every running download: the way out of the app stops them all.
    pub fn cancel_all(&self) {
        for sender in self.running().values() {
            let _ = sender.send(true);
        }
    }

    /// Cancels every running download and then waits, for at most `limit`, for them to finish
    /// taking their part files away. A save that has not stopped by then is left to itself:
    /// quitting cannot wait on it forever.
    pub async fn stop_all(&self, limit: Duration) {
        self.cancel_all();
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            // Never hold the lock across the wait: the running downloads finish themselves.
            let quiet = self.running().is_empty();
            if quiet {
                break;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            tokio::time::sleep((deadline - now).min(Duration::from_millis(50))).await;
        }
    }
}

/// Takes `handle` out of [`Downloads::finish`] on the way out of the save that registered it, on
/// any way out: success, refusal, failure, or cancel. A running download is never left in the map
/// because a command returned early.
struct Running<'a> {
    downloads: &'a Downloads,
    handle: String,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.downloads.finish(&self.handle);
    }
}

/// The base address of instance `id`: This computer's while it runs, a saved instance's otherwise.
pub(crate) async fn instance_base(
    state: &DesktopState,
    instances: &Instances,
    id: &str,
) -> Result<String, String> {
    if id == LOCAL_ID {
        state
            .startup()
            .endpoint(START_WAIT)
            .await
            .map(|(base, _)| base)
    } else {
        instances.base(id)
    }
}

/// Asks where to save, on the main thread as the system's dialogs need, and answers the path chosen, or
/// `None` when the person cancelled. The dialog is a sheet on the main window where there is one.
async fn ask_where<R: Runtime>(
    app: &AppHandle<R>,
    name: String,
) -> Result<Option<PathBuf>, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    let window = app.get_webview_window(MAIN_WINDOW);
    app.run_on_main_thread(move || {
        let mut dialog = rfd::AsyncFileDialog::new()
            .set_title("Save the file")
            .set_file_name(name)
            .set_can_create_directories(true);
        if let Some(window) = window.as_ref() {
            dialog = dialog.set_parent(window);
        }
        // Made here, on the main thread; waited for by the command, off it.
        let _ = send.send(dialog.save_file());
    })
    .map_err(|_| SAVE_FAILED.to_string())?;
    let chosen = receive.await.map_err(|_| SAVE_FAILED.to_string())?.await;
    Ok(chosen.map(|handle| handle.path().to_path_buf()))
}

/// Asks where to save a file called `name`, before anything is fetched. Answers an opaque handle for the
/// place chosen, or `None` when the person cancelled; the path itself stays in the shell. The page asks
/// for a stream address only once it has the handle, so the address's short-lived token is not spent
/// while the dialog is open.
#[tauri::command]
pub async fn choose_download_place<R: Runtime>(
    app: AppHandle<R>,
    name: String,
    downloads: tauri::State<'_, Downloads>,
) -> Result<Option<String>, String> {
    Ok(ask_where(&app, suggested_name(&name))
        .await?
        .map(|path| downloads.keep(path)))
}

/// Saves the file at `url` on instance `instance` to the place [`choose_download_place`] answered
/// `handle` for, reporting each [`Progress`] to the page through `on_progress` (a page that has gone
/// away simply stops receiving them: the download goes on). The handle is registered as running
/// before the fetch, so [`cancel_download`] can stop it by that same handle until the save is over.
/// A refusal or a failure rejects with the line to show, including [`CANCELLED`] for a cancel.
#[tauri::command]
pub async fn save_download(
    instance: String,
    handle: String,
    url: String,
    on_progress: tauri::ipc::Channel<Progress>,
    state: tauri::State<'_, DesktopState>,
    instances: tauri::State<'_, Instances>,
    downloads: tauri::State<'_, Downloads>,
) -> Result<(), String> {
    let destination = downloads
        .take(&handle)
        .ok_or_else(|| "Choose where to save the file again.".to_string())?;
    let cancel = downloads.register(&handle);
    let _running = Running {
        downloads: &downloads,
        handle: handle.clone(),
    };
    let base = instance_base(&state, &instances, &instance).await?;
    let target = stream_target(&base, &url).ok_or_else(|| NOT_A_STREAM.to_string())?;
    save_to(
        &client(),
        target,
        &destination,
        |progress| {
            // A failed send means the page is gone; the download is the person's file and goes on.
            let _ = on_progress.send(progress);
        },
        cancel,
    )
    .await?;
    Ok(())
}

/// Stops the download running under `handle` (the one [`choose_download_place`] answered to the
/// page). False when none is running under it: a finished download is not cancellable.
#[tauri::command]
pub fn cancel_download(handle: String, downloads: tauri::State<'_, Downloads>) -> bool {
    downloads.cancel(&handle)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fbtt_core::scratch::ScratchDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::watch;

    use super::{Downloads, MAX_PLACES, Progress, client, save_to, stream_target, suggested_name};

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    fn stream(base: &str) -> String {
        format!("{base}/api/v1/torrents/{HASH}/files/3/stream")
    }

    #[test]
    fn a_stream_address_on_the_instance_is_fetched() {
        let base = "http://127.0.0.1:53211";
        assert!(stream_target(base, &stream(base)).is_some());
        assert!(stream_target(base, &format!("{}?token=abc", stream(base))).is_some());
        assert!(stream_target("http://127.0.0.1:53211/", &stream(base)).is_some());
        let prefixed = "https://pi.local:8443/fbt";
        assert!(stream_target(prefixed, &format!("{}?token=t", stream(prefixed))).is_some());
    }

    #[test]
    fn anything_else_is_refused() {
        let base = "http://127.0.0.1:53211";
        let refused = [
            // Another instance, another port, a host that only starts the same way.
            stream("http://127.0.0.2:53211"),
            stream("http://127.0.0.1:5321"),
            stream("http://127.0.0.1:532111"),
            format!("http://127.0.0.1:53211@evil.example/api/v1/torrents/{HASH}/files/3/stream"),
            // Another route of the same instance.
            format!("{base}/api/v1/torrents"),
            format!("{base}/api/v1/torrents/{HASH}/files/3"),
            format!("{base}/api/v1/torrents/{HASH}/files/3/stream/x"),
            format!("{base}/api/v1/torrents/{HASH}/files/x/stream"),
            format!(
                "{base}/api/v1/torrents/{}/files/3/stream",
                HASH.to_uppercase()
            ),
            format!("{base}/api/v1/torrents/../files/3/stream"),
            format!("{base}/api/v1/torrents/{HASH}/files/%2e%2e/stream"),
            // More in the query than the token, or a fragment.
            format!("{}?token=a&x=1", stream(base)),
            format!("{}?x=1", stream(base)),
            format!("{}?token=", stream(base)),
            format!("{}#x", stream(base)),
            // Not http.
            stream("file://127.0.0.1:53211"),
            String::new(),
        ];
        for url in refused {
            assert!(stream_target(base, &url).is_none(), "{url} was accepted");
        }
        assert!(stream_target("", &stream("")).is_none());
    }

    #[test]
    fn the_suggested_name_names_no_folder() {
        assert_eq!(suggested_name("movie.mkv"), "movie.mkv");
        assert_eq!(suggested_name("Season 1/movie.mkv"), "movie.mkv");
        assert_eq!(suggested_name("..\\..\\x.mkv"), "x.mkv");
        assert_eq!(suggested_name("a:b?.mkv"), "ab.mkv");
        assert_eq!(suggested_name(".."), "download");
        assert_eq!(suggested_name(""), "download");
    }

    /// A stand-in instance that answers every request with `status` and `body`, then closes.
    async fn answering(status: &'static str, body: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: video/x-matroska\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        base
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_file_is_streamed_to_the_chosen_place() {
        let dir = ScratchDir::new("desktop-download-save");
        let body: Vec<u8> = (0..300_000u32).map(|n| (n % 251) as u8).collect();
        let base = answering("200 OK", body.clone()).await;
        let target = stream_target(&base, &format!("{}?token=t", stream(&base))).expect("a target");
        let destination = dir.path().join("movie.mkv");
        let (_keep, cancel) = watch::channel(false);

        let saved = tokio::time::timeout(
            Duration::from_secs(30),
            save_to(&client(), target, &destination, |_| {}, cancel),
        )
        .await
        .expect("within the deadline")
        .expect("the file is saved");

        assert_eq!(saved, body.len() as u64);
        assert_eq!(std::fs::read(&destination).expect("the saved file"), body);
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("the folder").count(),
            1,
            "no part file is left"
        );
    }

    /// A file of the person's own called `<name>.part`, and another download's part file, are never
    /// written over or removed: each download makes a part file of its own, new.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_download_writes_only_its_own_part_file() {
        let dir = ScratchDir::new("desktop-download-own-part");
        let theirs = dir.path().join("movie.mkv.part");
        std::fs::write(&theirs, b"the person's own").expect("their file");
        let destination = dir.path().join("movie.mkv");
        let refusing = answering("403 Forbidden", b"no".to_vec()).await;
        let (_keep, refused_cancel) = watch::channel(false);
        save_to(
            &client(),
            stream_target(&refusing, &stream(&refusing)).expect("a target"),
            &destination,
            |_| {},
            refused_cancel,
        )
        .await
        .expect_err("a 403 is not a file");

        let body: Vec<u8> = (0..70_000u32).map(|n| (n % 7) as u8).collect();
        let base = answering("200 OK", body.clone()).await;
        let target = || stream_target(&base, &stream(&base)).expect("a target");
        let (one, two) = (client(), client());
        let (_one_keep, cancel_one) = watch::channel(false);
        let (_two_keep, cancel_two) = watch::channel(false);
        let (first, second) = tokio::join!(
            save_to(&one, target(), &destination, |_| {}, cancel_one),
            save_to(&two, target(), &destination, |_| {}, cancel_two),
        );
        first.expect("the first download");
        second.expect("the second download");

        assert_eq!(std::fs::read(&destination).expect("the saved file"), body);
        assert_eq!(
            std::fs::read(&theirs).expect("their file"),
            b"the person's own"
        );
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("the folder")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        assert_eq!(left.len(), 2, "only the file and theirs are left: {left:?}");
    }

    #[test]
    fn a_chosen_place_is_handed_out_once_by_its_handle() {
        let downloads = Downloads::new();
        let handle = downloads.keep("/somewhere/movie.mkv".into());
        assert!(!handle.contains("movie"), "the handle names no path");
        assert_eq!(downloads.take("nope"), None);
        assert_eq!(
            downloads.take(&handle),
            Some(std::path::PathBuf::from("/somewhere/movie.mkv"))
        );
        assert_eq!(downloads.take(&handle), None, "a handle opens one download");

        let first = downloads.keep("/a".into());
        for _ in 0..MAX_PLACES {
            downloads.keep("/b".into());
        }
        assert_eq!(
            downloads.take(&first),
            None,
            "the oldest is forgotten past the cap"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_answer_saves_nothing() {
        let dir = ScratchDir::new("desktop-download-refused");
        let base = answering("403 Forbidden", b"no".to_vec()).await;
        let target = stream_target(&base, &stream(&base)).expect("a target");
        let destination = dir.path().join("movie.mkv");
        let (_keep, cancel) = watch::channel(false);

        let problem = save_to(&client(), target, &destination, |_| {}, cancel)
            .await
            .expect_err("a 403 is not a file");

        assert!(problem.contains("403"), "{problem}");
        assert!(!destination.exists());
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("the folder").count(),
            0,
            "no part file is left"
        );
    }

    /// A stand-in instance that answers `body` with no Content-Length: the answer ends when the
    /// connection does, so the client has no size to report against.
    async fn answering_without_length(body: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: video/x-matroska\r\nconnection: close\r\n\r\n";
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        base
    }

    /// A stand-in instance that answers with `bytes`-sized chunked frames `every` apart, forever:
    /// the answer has no end, so only a cancel (or the stall timeout) stops a download from it.
    async fn streaming_chunks(bytes: usize, every: Duration) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: video/x-matroska\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let body = vec![7u8; bytes];
                    let frame_head = format!("{:x}\r\n", bytes);
                    loop {
                        tokio::time::sleep(every).await;
                        if socket.write_all(frame_head.as_bytes()).await.is_err() {
                            return;
                        }
                        if socket.write_all(&body).await.is_err() {
                            return;
                        }
                        if socket.write_all(b"\r\n").await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        base
    }

    /// A 1 MiB answer with a Content-Length: progress starts at 0, never decreases, and its last
    /// line is the whole file.
    #[tokio::test(flavor = "multi_thread")]
    async fn progress_is_reported_and_ends_at_the_size() {
        let dir = ScratchDir::new("desktop-download-progress");
        let body: Vec<u8> = (0..1_048_576u32).map(|n| (n % 251) as u8).collect();
        let base = answering("200 OK", body.clone()).await;
        let target = stream_target(&base, &stream(&base)).expect("a target");
        let destination = dir.path().join("movie.mkv");
        let (_keep, cancel) = watch::channel(false);
        let reports = std::sync::Mutex::new(Vec::new());

        let saved = tokio::time::timeout(
            Duration::from_secs(30),
            save_to(
                &client(),
                target,
                &destination,
                |progress| reports.lock().expect("the reports").push(progress),
                cancel,
            ),
        )
        .await
        .expect("within the deadline")
        .expect("the file is saved");

        let reports = reports.into_inner().expect("the reports");
        let size = body.len() as u64;
        assert_eq!(saved, size);
        assert_eq!(
            reports.first(),
            Some(&Progress {
                received: 0,
                total: Some(size),
            }),
            "progress starts at 0, with the answer's size: {reports:?}"
        );
        assert!(
            reports
                .windows(2)
                .all(|pair| pair[0].received <= pair[1].received),
            "progress never decreases: {reports:?}"
        );
        assert_eq!(
            reports.last(),
            Some(&Progress {
                received: size,
                total: Some(size),
            }),
            "the last progress is the whole file: {reports:?}"
        );
    }

    /// An answer with no Content-Length: every progress line says bytes only, with no total.
    #[tokio::test(flavor = "multi_thread")]
    async fn progress_without_a_length_has_no_total() {
        let dir = ScratchDir::new("desktop-download-no-length");
        let body: Vec<u8> = (0..70_000u32).map(|n| (n % 7) as u8).collect();
        let base = answering_without_length(body.clone()).await;
        let target = stream_target(&base, &stream(&base)).expect("a target");
        let destination = dir.path().join("movie.mkv");
        let (_keep, cancel) = watch::channel(false);
        let reports = std::sync::Mutex::new(Vec::new());

        tokio::time::timeout(
            Duration::from_secs(30),
            save_to(
                &client(),
                target,
                &destination,
                |progress| reports.lock().expect("the reports").push(progress),
                cancel,
            ),
        )
        .await
        .expect("within the deadline")
        .expect("the file is saved");

        let reports = reports.into_inner().expect("the reports");
        assert!(
            reports.iter().all(|progress| progress.total.is_none()),
            "an answer with no Content-Length made a total: {reports:?}"
        );
        assert_eq!(
            reports.last(),
            Some(&Progress {
                received: body.len() as u64,
                total: None,
            }),
            "the last progress is the whole file, with no total: {reports:?}"
        );
    }

    /// A server that sends chunked frames forever: the cancel that follows the first progress
    /// with bytes in it ends the download well before the stall timeout, and the part file it
    /// made is taken away.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancel_stops_the_download_and_leaves_no_file() {
        let dir = ScratchDir::new("desktop-download-cancel");
        let base = streaming_chunks(4096, Duration::from_millis(40)).await;
        let target = stream_target(&base, &stream(&base)).expect("a target");
        let destination = dir.path().join("movie.mkv");
        let (sender, cancel) = watch::channel(false);
        let reports = std::sync::Mutex::new(Vec::new());

        let mut asked = false;
        let problem = tokio::time::timeout(
            // Ten seconds, where the stall timeout is sixty: the cancel must end this on its own.
            Duration::from_secs(10),
            save_to(
                &client(),
                target,
                &destination,
                |progress| {
                    reports.lock().expect("the reports").push(progress);
                    if progress.received > 0 && !asked {
                        asked = true;
                        let _ = sender.send(true);
                    }
                },
                cancel,
            ),
        )
        .await
        .expect("the download was cancelled, not stalled out")
        .expect_err("a cancelled download is not a file");

        assert_eq!(problem, "Download cancelled.");
        let seen = reports.into_inner().expect("the reports");
        assert!(
            seen.iter().any(|progress| progress.received > 0),
            "the download was cancelled before any byte arrived: {seen:?}"
        );
        assert!(!destination.exists(), "a cancelled download left the file");
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("the folder")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        assert!(left.is_empty(), "a cancelled download left {left:?}");
    }

    #[test]
    fn cancel_finds_the_running_download_by_its_handle() {
        let downloads = Downloads::new();
        let one = downloads.register("handle-one");
        let two = downloads.register("handle-two");
        assert!(
            !downloads.cancel("handle-three"),
            "a handle with no download running under it cancels nothing"
        );
        assert!(
            !*one.borrow() && !*two.borrow(),
            "nothing was cancelled yet"
        );
        assert!(
            downloads.cancel("handle-one"),
            "the running download is found by its handle"
        );
        assert!(*one.borrow(), "the flag the download watches is up");
        assert!(!*two.borrow(), "the other download goes on");
        downloads.cancel_all();
        assert!(*two.borrow(), "cancel_all stops every running download");
    }

    #[test]
    fn a_finished_download_is_no_longer_cancellable() {
        let downloads = Downloads::new();
        let receiver = downloads.register("place-1");
        assert!(
            downloads.cancel("place-1"),
            "while it runs, the handle finds it"
        );
        downloads.finish("place-1");
        assert!(
            !downloads.cancel("place-1"),
            "a finished download is forgotten and cannot be cancelled again"
        );
        assert!(
            *receiver.borrow(),
            "the cancel that came first still stands"
        );
        let untouched = downloads.register("place-2");
        downloads.finish("place-2");
        assert!(
            !*untouched.borrow(),
            "finishing on its own does not cancel what it watched"
        );
        let fresh = downloads.register("place-1");
        assert!(!*fresh.borrow(), "a download started again is uncancelled");
    }

    /// The cancel that arrives on the tail of the last progress report, while the file is being
    /// flushed and synced, still cancels: the rename is checked again right before it happens.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancel_after_the_last_report_still_cancels_the_rename() {
        let dir = ScratchDir::new("desktop-download-cancel-rename");
        let body: Vec<u8> = (0..70_000u32).map(|n| (n % 7) as u8).collect();
        let base = answering("200 OK", body.clone()).await;
        let target = stream_target(&base, &stream(&base)).expect("a target");
        let destination = dir.path().join("movie.mkv");
        let (sender, cancel) = watch::channel(false);
        let mut asked = false;

        let problem = tokio::time::timeout(
            Duration::from_secs(30),
            save_to(
                &client(),
                target,
                &destination,
                |progress| {
                    // The tail of the reports: the whole file against the answer's length. The
                    // cancel lands after the last check before the report, in the flush-and-sync
                    // stretch that precedes the rename itself.
                    if progress.received > 0 && progress.total == Some(progress.received) && !asked
                    {
                        asked = true;
                        let _ = sender.send(true);
                    }
                },
                cancel,
            ),
        )
        .await
        .expect("within the deadline")
        .expect_err("a download cancelled before its rename is not a file");

        assert_eq!(problem, "Download cancelled.");
        assert!(
            !destination.exists(),
            "the cancelled download renamed its part file"
        );
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("the folder")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        assert!(left.is_empty(), "the cancelled download left {left:?}");
    }
}
