//! The player's Download, in the app: a save dialog, and the file streamed to the place chosen.
//!
//! In a browser the page clicks an `<a download>` on its own origin and the browser saves the file. In
//! the app the page's origin (`tauri://localhost`, or `http://tauri.localhost` on Windows) is not the
//! engine's, and a webview ignores `download` on a link to another origin: the click becomes a navigation
//! of the main window to the media URL, which shows a bare video in place of the app. So the page hands
//! the stream address to [`save_download`] instead, and the shell asks where to put the file and streams
//! it there chunk by chunk. A movie of several gigabytes never sits in memory, which fetching it into a
//! `Blob` on the page would have done.
//!
//! The shell fetches only what the page could have opened itself: an address on the instance the page
//! is connected to, at that instance's file-stream route ([`stream_target`]). Anything else is refused
//! before a request goes out, so the command cannot be used to make the shell fetch some other address.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fbtt_desktop_core::LOCAL_ID;
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

/// Fetches `url` with `client` into `destination`, chunk by chunk. The bytes go to a `.part` file next to
/// it first, which takes the final name only once the whole answer arrived, so a broken download never
/// leaves a short file under the name the person chose. Answers how many bytes were saved.
pub async fn save_to(
    client: &reqwest::Client,
    url: url::Url,
    destination: &Path,
) -> Result<u64, String> {
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
    let partial = partial_path(destination);
    let result = async {
        let mut file = tokio::fs::File::create(&partial)
            .await
            .map_err(|_| SAVE_FAILED.to_string())?;
        let mut written = 0u64;
        loop {
            let chunk = response
                .chunk()
                .await
                .map_err(|_| "The download broke off.".to_string())?;
            let Some(chunk) = chunk else { break };
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(|_| SAVE_FAILED.to_string())?;
            written += chunk.len() as u64;
        }
        tokio::io::AsyncWriteExt::flush(&mut file)
            .await
            .map_err(|_| SAVE_FAILED.to_string())?;
        file.sync_all().await.map_err(|_| SAVE_FAILED.to_string())?;
        drop(file);
        tokio::fs::rename(&partial, destination)
            .await
            .map_err(|_| SAVE_FAILED.to_string())?;
        Ok(written)
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
    }
    result
}

/// `destination` with `.part` added to its name.
fn partial_path(destination: &Path) -> PathBuf {
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    destination.with_file_name(name)
}

/// The base address of instance `id`: This computer's while it runs, a saved instance's otherwise.
async fn instance_base(
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

/// Saves the file at `url` on instance `instance` where the person chooses. Answers `true` once it is
/// saved and `false` when the person cancelled the dialog; a refusal or a failure rejects with the line
/// to show.
#[tauri::command]
pub async fn save_download<R: Runtime>(
    app: AppHandle<R>,
    instance: String,
    url: String,
    name: String,
    state: tauri::State<'_, DesktopState>,
    instances: tauri::State<'_, Instances>,
) -> Result<bool, String> {
    let base = instance_base(&state, &instances, &instance).await?;
    let target = stream_target(&base, &url).ok_or_else(|| NOT_A_STREAM.to_string())?;
    let Some(destination) = ask_where(&app, suggested_name(&name)).await? else {
        return Ok(false);
    };
    save_to(&client(), target, &destination).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fbtt_core::scratch::ScratchDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{client, save_to, stream_target, suggested_name};

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

        let saved = tokio::time::timeout(
            Duration::from_secs(30),
            save_to(&client(), target, &destination),
        )
        .await
        .expect("within the deadline")
        .expect("the file is saved");

        assert_eq!(saved, body.len() as u64);
        assert_eq!(std::fs::read(&destination).expect("the saved file"), body);
        assert!(
            !dir.path().join("movie.mkv.part").exists(),
            "no part file is left"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_answer_saves_nothing() {
        let dir = ScratchDir::new("desktop-download-refused");
        let base = answering("403 Forbidden", b"no".to_vec()).await;
        let target = stream_target(&base, &stream(&base)).expect("a target");
        let destination = dir.path().join("movie.mkv");

        let problem = save_to(&client(), target, &destination)
            .await
            .expect_err("a 403 is not a file");

        assert!(problem.contains("403"), "{problem}");
        assert!(!destination.exists());
        assert!(!dir.path().join("movie.mkv.part").exists());
    }
}
