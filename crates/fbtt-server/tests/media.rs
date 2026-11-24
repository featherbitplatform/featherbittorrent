//! The media routes end to end: a real video file in a real library, requests over http, and the answers a
//! player has to be able to rely on.
//!
//! The file these tests play is the media crate's own multi-track fixture - Matroska, four seconds, one
//! h264 video track, AAC and Opus audio, SubRip and ASS subtitles - seeded into a torrent beside a
//! `.srt` sidecar and a text file that is not a subtitle. That is the least interesting set of files that
//! still exercises every branch these routes have: track kinds, seek landing, an embedded cue track, an
//! external subtitle file, a file to refuse, and a name beside the video for the sidecar list to sort by.
//! Where an assertion is about what a file *holds*, it is compared against `expected.json` next to the
//! fixture - the ffprobe ground truth the media crate's own tests check themselves against - rather than
//! against numbers typed into this file.
//!
//! Two tests need a torrent whose bytes are not there: a file hashed into a `.torrent` and then taken off
//! the disk again, with no peers to ask for a single piece of it. fbt's file reader waits forever for a
//! piece that has not arrived, so those torrents are how these tests watch a media route decide to stop
//! waiting: once on its read timeout, and once when the viewer hangs up mid-read and the timeout has
//! another twenty-nine seconds to go. A third needs a torrent whose bytes are only partly there - a
//! middle piece of its video blanked on disk, between the last byte its first streamed fragment
//! needs and the cues element at the file's end - because that is the only stream that serves
//! fragments and holds a permit while still waiting inside a read.
//!
//! Logins are off except in the one test about credentials, because most of these tests are about
//! containers and budgets, not about who is allowed to read them. The last group of tests is about what a
//! play does to the file it plays, which is a question about the file list rather than the bytes: the
//! address a player is handed is the address it always got, and what changed is which file this server is
//! now asked for first.

use std::fs;
use std::io::{Cursor, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fbtt_core::scratch::ScratchDir;
use fbtt_core::{AuthConfig, Config, LibraryLock, Paths, ServerConfig, UserEntry};
use fbtt_server::Server;
use fbtt_server::auth::password;
use reqwest::{Response, StatusCode};
use serde_json::Value;
use tokio::task::JoinSet;

/// How long a test waits for anything from the server. Starting an engine takes a moment; a media route
/// whose pieces never arrive gives up after its read timeout, which these tests set far below its default.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long a stream of the partly-seeded torrent has to say nothing - no bytes, and no end of stream
/// either - before a test may call it blocked on the missing piece. It is far shorter than anything the
/// engine takes to give up on a piece - the tests that watch one set this server's read timeout to
/// at least thirty seconds, and the dropped-stream test raises its own to two minutes - and far
/// longer than a fragment needs to cross a loopback connection, so quiet for this long means waiting,
/// not slow.
const WAIT_SILENT: Duration = Duration::from_secs(2);

/// The password of the one account these tests have, and the name it is filed under.
const USER: &str = "alice";
const SECRET: &str = "hunter2";

/// The media crate's multi-track fixture, and the ffprobe answers beside it that these tests check against.
const FIXTURE: &str = "h264-aac-opus-srt-ass.mkv";

/// The path a media test's folder is seeded under, and the three files in it: the video first, then its
/// sidecar, then a text file that is not a subtitle. The names sort in that order, so the file indices are
/// 0, 1 and 2.
const FOLDER: &str = "the-show";

/// The sidecar these tests seed beside the video: three cues, the first across two lines, the second with
/// the escape codes a terminal's colours leave behind when somebody copies text out of one.
const MOVIE_SRT: &str = concat!(
    "1\n00:00:01,000 --> 00:00:03,500\nLine one\nLine two\n\n",
    "2\n00:00:04,000 --> 00:00:06,000\nColour \u{1b}[31mred\u{1b}[0m here\n\n",
    "3\n00:00:07,000 --> 00:00:09,000\nLast line\n",
);

/// The file in the same folder that is not a subtitle: real text, no subtitle extension, and the answer to
/// "what does this route do with a file it cannot read cues out of".
const NOTES: &str = "notes for the screening, not a subtitle file\n";

/// The length of the sidecar `seed_partial` leads its torrent with, and the number of cues in it.
/// The length is the whole point: a torrent lays its files end to end in name order, so leading the
/// 46134-byte video with this many bytes puts the torrent's fourth 16 KiB piece - the only one a
/// stream of this fixture can block on and still have streamed bytes behind it - at 29152..45536
/// inside the video. `seed_partial` spells out why the hole has to start and end exactly there.
const SIDECAR_BYTES: usize = 20_000;
const SIDECAR_CUES: usize = 400;

/// [`SIDECAR_CUES`] SubRip cues over a rising clock - two seconds apart, half a second long, nothing a
/// parser could object to - with the last cue's text grown so the file lands exactly on
/// [`SIDECAR_BYTES`]. The cue probe reads this file whole and asserts its cue count back.
fn cues_srt() -> Vec<u8> {
    let mut text = String::new();
    for cue in 1..=SIDECAR_CUES {
        let seconds = cue * 2;
        text.push_str(&format!(
            "{cue}\n00:{:02}:{:02},000 --> 00:{:02}:{:02},500\ncue {cue}\n\n",
            seconds / 60,
            seconds % 60,
            seconds / 60,
            seconds % 60
        ));
    }
    let tail = format!("cue {SIDECAR_CUES}\n\n");
    assert!(
        text.ends_with(&tail),
        "the pad wants to grow the last cue's text"
    );
    // The last block loses its closing blank line and its text grows by the padding instead, so
    // the file ends the way `MOVIE_SRT` ends - mid-block, one newline after the last text line -
    // and lands exactly on `SIDECAR_BYTES`.
    let body = &text[..text.len() - tail.len()];
    let name = format!("cue {SIDECAR_CUES}");
    assert!(
        body.len() + name.len() < SIDECAR_BYTES,
        "SIDECAR_BYTES has to leave room for the pad"
    );
    let pad = SIDECAR_BYTES - body.len() - name.len() - 1;
    format!("{body}{name}{}\n", "x".repeat(pad)).into_bytes()
}

/// Runs `what` under the deadline, so a server that never answers is a failure with a message rather than a
/// hung test job.
async fn within<T>(what: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
}

/// A server running on a library of its own, with the scratch directory kept alive beside it.
struct Served {
    /// The scratch library, removed when this goes down. Held for no other reason than that everything
    /// the test works on is inside it - which [`Served::root`] is what checks.
    dir: ScratchDir,
    config: Config,
    server: Server,
}

impl Served {
    /// A server on a library in a scratch directory named after `tag`, with `base` naming its config once
    /// it knows where that directory is. The library lock is taken here and given to the server, which
    /// holds it until its last save.
    async fn start(tag: &str, base: impl FnOnce(&Path) -> Config) -> Served {
        let dir = ScratchDir::new(tag);
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        let config = base(dir.path()).resolve(&paths);
        let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
        let server = within(Server::start_with_settings(
            loopback(&config),
            config.clone(),
            &paths,
            lock,
        ))
        .await
        .expect("start the server");
        Served {
            dir,
            config,
            server,
        }
    }

    /// The address to send requests to.
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.server.local_addr())
    }

    /// The one folder this server may name: the download folder, which is what `Config::resolve` makes
    /// the only allowed root when the config named none. Checked against the scratch directory, because
    /// every file these tests seed goes inside it and nothing outside it is ever written.
    fn root(&self) -> &Path {
        let root = &self.config.download_dir;
        assert!(
            root.starts_with(self.dir.path()),
            "the download folder {root:?} is outside the scratch library {:?}",
            self.dir.path(),
        );
        root
    }

    /// Stops serving, saves and releases the library, so the scratch directory goes after the last write.
    async fn stop(self) {
        let Served {
            dir: _,
            config: _,
            server,
        } = self;
        within(server.shutdown()).await.expect("the last save ran");
    }
}

/// The settings every engine here runs on: loopback on a port the OS picks, with DHT, LSD, UPnP and NAT-PMP
/// off. A torrent whose peers are absent has to stay absent, which nothing that could leave the machine
/// may be trusted to do.
fn loopback(config: &Config) -> fbt::Settings {
    let mut settings = config.session_settings();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    settings
}

/// A config serving on a port the OS picks, downloading into `<scratch>/downloads`, with `users` as its
/// accounts and logins on, off or unset as `auth` says.
fn base_config(scratch: &Path, users: Vec<UserEntry>, auth: Option<bool>) -> Config {
    Config {
        download_dir: scratch.join("downloads"),
        server: Some(ServerConfig {
            bind: "127.0.0.1:0".parse().expect("a loopback bind address"),
            auth: AuthConfig {
                enabled: auth,
                users,
                devices: vec![],
            },
            ..ServerConfig::default()
        }),
        ..Config::default()
    }
}

/// The account the credential test signs in with.
fn alice() -> UserEntry {
    UserEntry {
        name: USER.to_string(),
        password_hash: password::hash(SECRET),
    }
}

/// The one-line fixture path: the media crate's fixture, which these tests seed rather than copy, so that
/// what is streamed is a file the media crate's own tests have already agreed is playable.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fbtt-media/tests/fixtures")
        .join(name)
}

/// The fixture's bytes.
fn movie() -> Vec<u8> {
    fs::read(fixture(FIXTURE)).expect("read the media fixture")
}

/// The ffprobe ground truth for one fixture, as the media crate's own tests read it: its `expected.json`
/// entry, with the track list in container order.
fn ground_truth(name: &str) -> Value {
    let raw = fs::read_to_string(fixture("expected.json")).expect("read the fixture expectations");
    let all: Value = serde_json::from_str(&raw).expect("the fixture expectations are JSON");
    let entry = &all["fixtures"][name];
    assert!(
        entry.is_object(),
        "expected.json has no entry for {name}: {entry}"
    );
    entry.clone()
}

/// The client every request goes through: redirects un-followed so a test sees the answer the server gave,
/// no per-request timeout ([`within`] is what bounds a test), and no proxy.
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("a test client")
}

/// A `GET` with the bearer token, when there is one.
async fn get(client: &reqwest::Client, url: &str, token: Option<&str>) -> Response {
    let mut request = client.get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    within(request.send()).await.expect("the server answered")
}

/// The JSON a response carries, which every route here answers in - errors included.
async fn body(response: Response) -> Value {
    within(response.json::<Value>()).await.expect("a JSON body")
}

/// The status and the `{"error": {"code": ...}}` of a response, for the answers whose point is the code.
async fn error_of(response: Response) -> (StatusCode, String) {
    let status = response.status();
    let body = body(response).await;
    let code = body["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("an error without a code in it: {body}"))
        .to_string();
    (status, code)
}

/// A folder `name` under `dir` holding `files` - `(name, bytes)` for each - and its path.
fn write_folder(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let folder = dir.join(name);
    fs::create_dir_all(&folder).expect("make the test folder");
    for (file, bytes) in files {
        fs::write(folder.join(file), bytes).expect("write a test file");
    }
    folder
}

/// The `.torrent` for the file or folder at `path`, hashed off this thread: a v1 torrent, so its file list
/// is the files themselves in name order with no BEP 47 pad between them to move the indices around.
async fn torrent_for(path: &Path) -> Vec<u8> {
    let path = path.to_path_buf();
    within(tokio::task::spawn_blocking(move || {
        fbt::CreateTorrent::new(path)
            .version(fbt::TorrentVersion::V1)
            .build(&mut |_done, _total| std::ops::ControlFlow::Continue(()))
            .expect("hash the test torrent")
    }))
    .await
    .expect("the hashing task ran")
}

/// The id of a torrent added from `bytes`, without waiting for anything about it: the add through fbt-api's
/// own route, exactly as any client's arrives, into this server's download folder.
async fn add_bytes(served: &Served, token: Option<&str>, bytes: &[u8]) -> String {
    let mut request = http()
        .post(served.url("/api/v1/torrents"))
        .query(&[("save_path", served.root().to_string_lossy().as_ref())])
        .header(reqwest::header::CONTENT_TYPE, "application/x-bittorrent")
        .body(bytes.to_vec());
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = within(request.send())
        .await
        .expect("the add reached the server");
    if !response.status().is_success() {
        let (status, code) = error_of(response).await;
        panic!("the test torrent was not added: {status} {code}");
    }
    let info = body(response).await;
    info["id"]
        .as_str()
        .expect("the answer names the torrent by its 40-hex wire hash")
        .to_string()
}

/// Waits until this server answers about torrent `id` at all, which for a torrent whose data is absent is
/// as far as it will ever get: the metadata is in the `.torrent`, the pieces are nowhere.
async fn wait_until_known(served: &Served, token: Option<&str>, id: &str) {
    let client = http();
    let until = Instant::now() + DEADLINE;
    loop {
        let response = get(
            &client,
            &served.url(&format!("/api/v1/torrents/{id}")),
            token,
        )
        .await;
        if response.status().is_success() {
            return;
        }
        if Instant::now() >= until {
            panic!(
                "torrent {id} was never known to the server within {} seconds",
                DEADLINE.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Waits until this server has finished checking the bytes a partly-seeded torrent already has on
/// disk. While the check pass runs the engine trusts no piece, so a read that starts then waits for
/// a piece to arrive rather than answering from bytes the pass has not reached yet; once the pass is
/// over, every piece that hashed right is verified and reads of it answer at once. With a hole no
/// peer holds, the pass ends with the torrent at `downloading` - it will never reach `seeding` -
/// and that settled state is exactly the moment pieces zero to two can be played from.
///
/// The state on its own does not prove the pass is over, so the answer is asked twice over. The
/// engine reports `checking_progress` in one state only, the checking one, and nothing else about a
/// torrent says how far its pass has got; a torrent that has been added and has not started its pass
/// says `queued_for_checking`, and one whose metadata has not arrived has nothing checked at all. So
/// the wait is over once the report carries the metadata, carries no check progress, and names a
/// state that is neither a check nor a wait for one.
async fn wait_until_checked(served: &Served, token: Option<&str>, id: &str) {
    let client = http();
    let until = Instant::now() + DEADLINE;
    loop {
        let response = get(
            &client,
            &served.url(&format!("/api/v1/torrents/{id}")),
            token,
        )
        .await;
        let info = if response.status().is_success() {
            body(response).await
        } else {
            Value::Null
        };
        let checked = info["has_metadata"].as_bool() == Some(true)
            && info["checking_progress"].is_null()
            && matches!(
                info["state"].as_str(),
                Some("downloading") | Some("seeding")
            );
        if checked {
            return;
        }
        if Instant::now() >= until {
            panic!(
                "torrent {id} never reported its check pass over within {} seconds: {info}",
                DEADLINE.as_secs(),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Waits until this server says torrent `id` is seeding - every piece of what was already on disk checked -
/// so that a media request after it reads bytes rather than waiting for pieces nobody has.
async fn wait_until_seeding(served: &Served, token: Option<&str>, id: &str) {
    let client = http();
    let until = Instant::now() + DEADLINE;
    loop {
        let info = body(
            get(
                &client,
                &served.url(&format!("/api/v1/torrents/{id}")),
                token,
            )
            .await,
        )
        .await;
        if info["state"].as_str() == Some("seeding") {
            return;
        }
        if Instant::now() >= until {
            panic!(
                "torrent {id} was not seeding within {} seconds: {info}",
                DEADLINE.as_secs(),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The three-file folder this server's media tests play from, seeded: the media fixture, `movie.srt` and
/// `notes.txt`, in that order of file indices. Seeded rather than downloaded, because the routes under
/// test answer about a file that is there.
async fn seed_show(served: &Served, token: Option<&str>) -> String {
    let bytes = movie();
    let folder = write_folder(
        served.root(),
        FOLDER,
        &[
            (FIXTURE, &bytes),
            ("movie.srt", MOVIE_SRT.as_bytes()),
            ("notes.txt", NOTES.as_bytes()),
        ],
    );
    let torrent = torrent_for(&folder).await;
    let id = add_bytes(served, token, &torrent).await;
    wait_until_seeding(served, token, &id).await;
    id
}

/// A torrent whose data is not on this machine: a video and its sidecar hashed into a `.torrent` while
/// they sat in the download folder, and then taken away again before the add. The file list and the
/// lengths are in the `.torrent`, so the metadata is there; no peer has a single piece, so every read of
/// either file waits. The sidecar keeps the extension it has, so the subtitle route - which answers
/// 415 about a name before it reads anything - is reached with a name it will accept and bytes it cannot
/// get.
async fn seed_absent(served: &Served, token: Option<&str>) -> String {
    let bytes = movie();
    let folder = write_folder(
        served.root(),
        "pending",
        &[("pending.mkv", &bytes), ("pending.srt", &bytes)],
    );
    let torrent = torrent_for(&folder).await;
    fs::remove_dir_all(&folder).expect("take the data away again");
    let id = add_bytes(served, token, &torrent).await;
    wait_until_known(served, token, &id).await;
    id
}

/// A torrent that is only partly here, built so a stream of its video serves fragments and *then*
/// waits on a piece: the fixture and a padded `.srt` hashed whole, then with the torrent's fourth
/// 16 KiB piece - a window of the video from 29152 to 45536 - blanked to zeros on disk. A torrent
/// lays its files end to end in name order, so the 20000-byte sidecar leads and the automatic
/// piece length - 16 KiB, the floor `auto_piece_len` holds for a torrent this small - puts the
/// video's share of that piece at 49152 - 20000 = 29152. The file keeps its declared length, so
/// the engine's check pass reads every piece and trusts only what hashes right: the four pieces
/// that are not this one verify - including the piece holding the file's last 598 bytes - and no
/// peer is there to bring the missing one.
///
/// The fixture's measured numbers decide the window, and both ends of it are hard. The hard end
/// at the file's tail: this file's `Cues` element is its *last* element, bytes 45931..46134, and
/// its `SeekHead` points at it - so every probe and every track open seeks there and reads it
/// (`matroska-demuxer`'s `open` fetches the cues through that index), which means a hole anywhere
/// in the file's tail stops those routes before they answer: this is exactly what the round that
/// blanked the tail measured, a `read_timeout` with nothing streamed even though the header's
/// first 1820 bytes and three verified pieces were all in place. The hard end toward the start:
/// the fragmented-MP4 writer hands over its first wire fragment one keyframe behind, so [0, 1000)
/// flushes only once the 2000 ms keyframe block - cluster 3's first, bytes 22707..25304 - is
/// fully read; a hole starting before 25304 is met by the sample walk before anything is
/// streamed. So the hole must start after 25304 and end before 45931, and among the piece
/// boundaries a leading sidecar can produce, 29152..45536 is the only 16 KiB window that fits.
///
/// Inside that window the stream is the state the permit test needs: opening, the header, the
/// cues at the file's end, and every byte the first fragment needs all sit in verified pieces,
/// and then the sample walk crosses 29152 into the zeros the hash pass refused - waiting inside a
/// read, with a fragment already on the wire and the file's remaining bytes, cues included,
/// intact in a verified piece past the hole.
async fn seed_partial(served: &Served, token: Option<&str>) -> String {
    let video = movie();
    let sidecar = cues_srt();
    assert_eq!(
        sidecar.len(),
        SIDECAR_BYTES,
        "cues_srt promises the length the piece geometry needs"
    );
    // The torrent lays its files end to end in name order, so the video starts at global
    // `SIDECAR_BYTES` and its piece 3 - the fourth 16 KiB boundary, 49152, minus the sidecar's
    // bytes - runs 29152..45536. The window below is the whole plan as an assertion: after 25304,
    // the last byte the first fragment needs; ending before 45931, where the cues element waits.
    let hole = 49152 - sidecar.len();
    assert_eq!(
        video.len(),
        46134,
        "the piece arithmetic here is fixed to the fixture's length"
    );
    assert!(
        hole > 25304 && hole + 16384 <= 45931,
        "the blanked window has to end after the first fragment's last needed byte and before the cues"
    );
    let folder = write_folder(
        served.root(),
        "partial",
        &[("a-movie.srt", &sidecar), ("partial.mkv", &video)],
    );
    let torrent = torrent_for(&folder).await;
    // Blank the window after the hashing, so the `.torrent` describes the whole fixture and only
    // the bytes on this machine have gone bad. The blanked run is all video: the sidecar leads, so
    // global 49152..65536 is video-local 29152..45536, 16384 bytes that do not reach the file's
    // end - the cues behind it keep their real bytes.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(folder.join("partial.mkv"))
        .expect("open the seeded file to blank its middle piece");
    file.seek(SeekFrom::Start(hole as u64))
        .expect("seek to where the missing piece starts in the video");
    file.write_all(&vec![0u8; 16384])
        .expect("blank the missing piece's bytes in the video");
    drop(file);
    let id = add_bytes(served, token, &torrent).await;
    wait_until_checked(served, token, &id).await;
    id
}

/// The `fbtt_session` cookie value a sign-in handed out, without its attributes: what a browser sends back
/// on every request it makes to this address.
fn cookie_of(response: &Response) -> String {
    let headers = response.headers();
    let cookie = headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("fbtt_session="))
        .unwrap_or_else(|| panic!("a sign-in with no session cookie in {headers:?}"));
    cookie.split(';').next().expect("a cookie").to_string()
}

/// The token out of the stream address `POST /api/v1/stream-tokens` handed back: the address is the path
/// with `?token=` on it, and the token is what a player puts in its URL.
fn token_of(address: &str) -> String {
    address
        .split_once("?token=")
        .unwrap_or_else(|| panic!("the stream address has no token on it: {address}"))
        .1
        .to_string()
}

/// `POST /api/v1/auth/session` with `alice`'s password: the bearer token every protected request here uses.
async fn bearer(served: &Served) -> String {
    let response = within(
        http()
            .post(served.url("/api/v1/auth/session"))
            .basic_auth(USER, Some(SECRET))
            .send(),
    )
    .await
    .expect("the login reached the server");
    let body = body(response).await;
    body["token"]
        .as_str()
        .expect("a login that answers with a token")
        .to_string()
}

/// The priorities this server reports for `id`'s files, in index order, spelled the way the file pane
/// spells them: `skip`, `low`, `normal`, `high`.
async fn priorities_of(served: &Served, id: &str) -> Vec<String> {
    let response = get(
        &http(),
        &served.url(&format!("/api/v1/torrents/{id}/files")),
        None,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the file list of the test torrent did not answer"
    );
    let files = body(response).await;
    let rows = files
        .as_array()
        .unwrap_or_else(|| panic!("the file list is not an array: {files}"));
    rows.iter()
        .map(|row| {
            row["priority"]
                .as_str()
                .unwrap_or_else(|| panic!("a file row with no priority in it: {row}"))
                .to_string()
        })
        .collect()
}

/// `PUT /api/v1/torrents/{id}/file-priorities`, one priority per file the way the web UI's file pane
/// sends them: the starting point a play is then measured against.
async fn put_priorities(served: &Served, id: &str, priorities: &[&str]) {
    let response = within(
        http()
            .put(served.url(&format!("/api/v1/torrents/{id}/file-priorities")))
            .json(&serde_json::json!({ "priorities": priorities }))
            .send(),
    )
    .await
    .expect("the priorities reached the server");
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "the server refused the priorities this test starts from"
    );
}

/// `POST /api/v1/stream-tokens` for file `file` of torrent `id`, with the 201 checked: the address a
/// player would be handed, token in it wherever one was issued.
async fn token_for(served: &Served, id: &str, file: usize) -> String {
    let response = within(
        http()
            .post(served.url("/api/v1/stream-tokens"))
            .json(&serde_json::json!({ "torrent": id, "file": file }))
            .send(),
    )
    .await
    .expect("the token request reached the server");
    let status = response.status();
    let answer = body(response).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "no stream address was issued for file {file}: {answer}"
    );
    answer["path"]
        .as_str()
        .unwrap_or_else(|| panic!("the answer names no path: {answer}"))
        .to_string()
}

/// The bytes of a media stream response, walked box by box the way a demuxer walks them: `mp4-atom` reads
/// each top level box header, and the walk jumps the body of every box it is not interested in. Returns the
/// four letters of each box, in the order they arrived.
fn boxes(bytes: &[u8]) -> Vec<String> {
    let mut reader = Cursor::new(bytes);
    let mut kinds = Vec::new();
    loop {
        let Some(header) = within_io(<Option<mp4_atom::Header> as mp4_atom::ReadFrom>::read_from(
            &mut reader,
        )) else {
            return kinds;
        };
        // `mp4-atom` prints a box's four letters the way the spec spells them, which is what a test
        // failure should read like.
        kinds.push(header.kind.to_string());
        let body = header
            .size
            .expect("a box with a length (one that ran to the end of the stream has none)");
        reader
            .seek(SeekFrom::Current(
                i64::try_from(body).expect("a box below 2^63"),
            ))
            .expect("the next box");
    }
}

/// A `std::io` walk that failed is a test failure with the bytes' first words in it, not a panic with no
/// context: a body that is not fragmented MP4 at all is the interesting failure here.
fn within_io<T, E: std::fmt::Debug>(result: Result<T, E>) -> T {
    result.unwrap_or_else(|problem| panic!("the stream body is not parseable: {problem:?}"))
}

/// The `x-fbtt-start-ms` of a stream response, as the number the player has to treat it as.
fn start_ms_of(response: &Response) -> u64 {
    let value = response
        .headers()
        .get("x-fbtt-start-ms")
        .expect("a fragmented MP4 stream answers where it started")
        .to_str()
        .expect("a start time in digits")
        .to_string();
    value.parse().expect("a start time in milliseconds")
}

#[tokio::test(flavor = "multi_thread")]
async fn probe_route_lists_tracks_and_subtitle_files() {
    let served = Served::start("media-probe", |dir| base_config(dir, vec![], Some(false))).await;
    let id = seed_show(&served, None).await;

    let response = get(&http(), &served.url(&format!("/api/v1/media/{id}/0")), None).await;
    assert_eq!(response.status(), StatusCode::OK, "the probe failed");
    let answer = body(response).await;
    let want = ground_truth(FIXTURE);

    assert_eq!(
        answer["container"].as_str(),
        want["container"].as_str(),
        "the probe did not name the container the file is in"
    );
    assert_eq!(
        answer["duration_ms"].as_i64(),
        want["duration_ms"].as_i64(),
        "the probe did not report the length the file states"
    );
    // The constant every track's times are shifted by: without it the page cannot line a cue up with the
    // frame it is playing.
    assert_eq!(
        answer["time_offset_ms"].as_i64(),
        Some(1000),
        "the probe did not say how far its times are shifted"
    );

    let tracks = answer["tracks"].as_array().expect("a track list");
    let want_tracks = want["tracks"].as_array().expect("ground truth tracks");
    assert_eq!(
        tracks.len(),
        5,
        "the fixture has five media tracks, and the probe listed {tracks:?}"
    );
    for (got, want) in tracks.iter().zip(want_tracks) {
        let id = got["id"].as_i64();
        assert_eq!(
            id,
            want["expect_id"].as_i64(),
            "a track with the wrong number"
        );
        assert_eq!(
            got["kind"].as_str(),
            want["kind"].as_str(),
            "track {id:?} is the wrong kind of media"
        );
        assert_eq!(
            got["codec"].as_str(),
            want["codec_name"].as_str(),
            "track {id:?} has the wrong codec"
        );
        assert_eq!(
            got["language"].as_str(),
            want["language"].as_str(),
            "track {id:?} has the wrong language (null means the file states none)"
        );
        assert_eq!(
            got["name"].as_str(),
            want["title"].as_str(),
            "track {id:?} has the wrong title"
        );
        assert_eq!(
            got["default"].as_bool(),
            want["default"].as_bool(),
            "track {id:?} has the wrong default flag"
        );
        assert_eq!(
            got["forced"].as_bool(),
            want["forced"].as_bool(),
            "track {id:?} has the wrong forced flag"
        );
    }

    // What a browser can actually demux from a fragmented MP4, in the vocabulary a manifest uses: the
    // video and both audio tracks have it, and the subtitle tracks have none of it, which is how the page
    // knows to render those itself.
    let video = tracks[0]["mime_codecs"]
        .as_array()
        .expect("a video track's codecs");
    assert!(
        video
            .first()
            .and_then(|codec| codec.as_str())
            .is_some_and(|codec| codec.starts_with("avc1.")),
        "the h264 track names no avc1 codec: {video:?}"
    );
    for track in &tracks[1..3] {
        assert!(
            !track["mime_codecs"]
                .as_array()
                .expect("an audio track's codecs")
                .is_empty(),
            "an audio track names no browser codec: {track}"
        );
    }
    for track in &tracks[3..5] {
        assert_eq!(
            track["mime_codecs"]
                .as_array()
                .expect("a subtitle track's codecs")
                .len(),
            0,
            "a subtitle track has no fragmented MP4 sample entry: {track}"
        );
        assert_eq!(
            track["text"].as_bool(),
            Some(true),
            "a subtitle track is not text"
        );
    }
    assert_eq!(
        tracks[0]["text"].as_bool(),
        Some(false),
        "a video track is not a text subtitle"
    );

    // The sidecar list: only the `.srt` beside it, because `notes.txt` is not a subtitle file and the video
    // being probed is not a subtitle for itself.
    let beside = answer["external_subtitles"]
        .as_array()
        .expect("an external subtitle list");
    assert_eq!(
        beside.len(),
        1,
        "the folder holds one subtitle file and the list says {beside:?}"
    );
    assert_eq!(
        beside[0]["file"].as_i64(),
        Some(1),
        "the sidecar list names the wrong file index"
    );
    assert_eq!(
        beside[0]["name"].as_str(),
        Some("movie.srt"),
        "the sidecar list names the wrong file"
    );
    assert_eq!(
        beside[0]["format"].as_str(),
        Some("srt"),
        "the sidecar list names the wrong format"
    );

    // A file index that is not in the torrent, and a file that is not a container, are both answers about
    // the file rather than failures of the route.
    let (status, code) =
        error_of(get(&http(), &served.url(&format!("/api/v1/media/{id}/9")), None).await).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no file has index 9");
    assert_eq!(code, "not_found");
    let (status, code) =
        error_of(get(&http(), &served.url(&format!("/api/v1/media/{id}/2")), None).await).await;
    assert_eq!(
        status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "notes.txt is not a playable file"
    );
    assert_eq!(code, "unsupported_media");

    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_route_returns_init_then_fragments() {
    let served = Served::start("media-stream", |dir| base_config(dir, vec![], Some(false))).await;
    let id = seed_show(&served, None).await;

    let response = get(
        &http(),
        &served.url(&format!("/api/v1/media/{id}/0/tracks/1/stream")),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "the stream failed");
    let headers = response.headers();
    let mime = headers
        .get(reqwest::header::CONTENT_TYPE)
        .expect("a fragmented MP4 stream says what it is")
        .to_str()
        .expect("a content type in ascii")
        .to_string();
    assert!(
        mime.starts_with("video/mp4; codecs=\"avc1."),
        "the h264 track was served as {mime:?}"
    );
    assert_eq!(
        start_ms_of(&response),
        0,
        "a stream that was not asked to seek started somewhere else"
    );
    assert_eq!(
        headers
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store"),
        "a body that is the answer to a seek must not be cached"
    );

    let bytes = within(response.bytes()).await.expect("the whole body");
    let kinds = boxes(&bytes);
    // The init segment first (an `ftyp` and a `moov`), then at least one fragment: the stream's first
    // part carries the init segment inside it, so seeing `moov` twice here would mean the route wrote the
    // init segment and then the first fragment, which is what a player would not survive.
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("ftyp"),
        "the body does not start with the brand box: {kinds:?}"
    );
    assert_eq!(
        kinds.get(1).map(String::as_str),
        Some("moov"),
        "the body does not carry a track table after its brand: {kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|kind| kind.as_str() == "ftyp").count(),
        1,
        "the init segment was written twice: {kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|kind| kind.as_str() == "moov").count(),
        1,
        "the track table was written twice: {kinds:?}"
    );
    assert!(
        kinds.iter().skip(2).any(|kind| kind == "moof"),
        "a fragmented MP4 with no `moof` has no samples in it: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "mdat"),
        "a fragmented MP4 with no `mdat` has no bytes in it: {kinds:?}"
    );
    assert!(
        bytes.len() > 1024,
        "the whole four second video stream came back as {} bytes",
        bytes.len()
    );

    // A track the file does not carry is an answer about the file, in the code that says "no part of this
    // is playable", not a broken stream.
    let (status, code) = error_of(
        get(
            &http(),
            &served.url(&format!("/api/v1/media/{id}/0/tracks/99/stream")),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "track 99 is not in this file"
    );
    assert_eq!(code, "unsupported_media");

    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_route_audio_and_seek() {
    let served = Served::start("media-seek", |dir| base_config(dir, vec![], Some(false))).await;
    let id = seed_show(&served, None).await;

    // Track 3 is the Opus audio track. Audio needs no keyframe, so a seek asks for the sample at or after
    // the time, and the answer is the codec string a browser needs for an `audio/mp4` source.
    let response = get(
        &http(),
        &served.url(&format!(
            "/api/v1/media/{id}/0/tracks/3/stream?start_ms=2000"
        )),
        None,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a seek into the audio track failed"
    );
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .expect("a fragmented MP4 stream says what it is")
        .to_str()
        .expect("a content type in ascii")
        .to_string();
    assert_eq!(
        mime, "audio/mp4; codecs=\"opus\"",
        "the opus track was served as {mime:?}"
    );
    let started = start_ms_of(&response);
    // The fixture's clusters start at 0, 801, 1801 and 2816 ms, so a request for 2000 ms lands on a sample
    // inside the cluster that began at 1801: at or after what was asked for, and not past the next
    // cluster. Reporting the requested number instead of the landed one would make the page draw its
    // subtitle a second too early.
    assert!(
        (2000..2816).contains(&started),
        "a seek to 2000 ms landed at {started} ms, which is not a sample inside the cluster that holds 2000"
    );
    let bytes = within(response.bytes()).await.expect("the whole body");
    let kinds = boxes(&bytes);
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("ftyp"),
        "a seeked stream is still a complete fragmented MP4: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "moof"),
        "a seeked audio stream has no fragments in it: {kinds:?}"
    );

    // A `start_ms` that is not a number is the client's mistake, said before anything is fetched.
    let (status, code) = error_of(
        get(
            &http(),
            &served.url(&format!(
                "/api/v1/media/{id}/0/tracks/3/stream?start_ms=soon"
            )),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "start_ms=soon is not a time"
    );
    assert_eq!(code, "invalid_request");

    // A file that is not a container has no tracks to stream at all.
    let (status, code) = error_of(
        get(
            &http(),
            &served.url(&format!("/api/v1/media/{id}/2/tracks/1/stream")),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "notes.txt has no tracks"
    );
    assert_eq!(code, "unsupported_media");

    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cues_and_subtitle_file_routes() {
    let served = Served::start("media-cues", |dir| base_config(dir, vec![], Some(false))).await;
    let id = seed_show(&served, None).await;

    // The embedded SubRip track (4), which is the fixture's own subtitle track: the same three cues the
    // media crate's tests read out of the same bytes.
    let response = get(
        &http(),
        &served.url(&format!(
            "/api/v1/media/{id}/0/tracks/4/cues?from_ms=0&to_ms=10000"
        )),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "the cues failed");
    let answer = body(response).await;
    let cues = answer["cues"].as_array().expect("a cue list");
    let want: &[(&str, &str, &str)] = &[
        ("500", "2000", "First subtitle line\nspanning two lines"),
        ("2500", "3000", "Second subtitle line"),
        ("3200", "3900", "Third subtitle line"),
    ];
    assert_eq!(cues.len(), want.len(), "the wrong number of cues: {cues:?}");
    for (got, (start_ms, end_ms, text)) in cues.iter().zip(want) {
        assert_eq!(
            got["start_ms"].as_i64().expect("a start time").to_string(),
            *start_ms,
            "a cue starts at the wrong time"
        );
        assert_eq!(
            got["end_ms"].as_i64().expect("an end time").to_string(),
            *end_ms,
            "a cue ends at the wrong time"
        );
        assert_eq!(
            got["text"].as_str().expect("cue text"),
            *text,
            "a cue says the wrong thing"
        );
    }

    // A window that cuts the first cue out: the keep rule is overlap, so 2000-4000 keeps the cue that ends
    // at 2000 out and the one that starts at 2500 in.
    let answer = body(
        get(
            &http(),
            &served.url(&format!(
                "/api/v1/media/{id}/0/tracks/4/cues?from_ms=2000&to_ms=4000"
            )),
            None,
        )
        .await,
    )
    .await;
    let cues = answer["cues"].as_array().expect("a cue list");
    assert_eq!(
        cues.iter()
            .map(|cue| cue["start_ms"].as_i64())
            .collect::<Vec<_>>(),
        vec![Some(2500), Some(3200)],
        "the window kept the wrong cues"
    );

    // The two bounds that are this route's job to catch before it reads anything, rather than the
    // demuxer's to catch after.
    let (status, code) = error_of(
        get(
            &http(),
            &served.url(&format!(
                "/api/v1/media/{id}/0/tracks/4/cues?from_ms=1000&to_ms=1000"
            )),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an empty window is not a window"
    );
    assert_eq!(code, "invalid_request");
    let (status, code) = error_of(
        get(
            &http(),
            &served.url(&format!(
                "/api/v1/media/{id}/0/tracks/4/cues?from_ms=0&to_ms=600001"
            )),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a window ten seconds wider than the cap is refused"
    );
    assert_eq!(code, "invalid_request");
    let (status, code) = error_of(
        get(
            &http(),
            &served.url(&format!(
                "/api/v1/media/{id}/0/tracks/4/cues?from_ms=x&to_ms=10000"
            )),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "from_ms=x is not a time");
    assert_eq!(code, "invalid_request");

    // A video track has no cues to give, and a bitmap subtitle track would be the same answer: there is
    // no text of any format here to hand over.
    let (status, code) = error_of(
        get(
            &http(),
            &served.url(&format!(
                "/api/v1/media/{id}/0/tracks/1/cues?from_ms=0&to_ms=10000"
            )),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "a video track has no cues"
    );
    assert_eq!(code, "unsupported_media");

    // The sidecar, by its index in the same torrent: three cues, the same shape the embedded route answers
    // in, so a player has one parse for both.
    let answer = body(
        get(
            &http(),
            &served.url(&format!("/api/v1/media/{id}/0/subtitles/1")),
            None,
        )
        .await,
    )
    .await;
    let cues = answer["cues"].as_array().expect("a cue list");
    assert_eq!(cues.len(), 3, "the sidecar holds three cues: {cues:?}");
    assert_eq!(
        cues[0]["start_ms"].as_i64(),
        Some(1000),
        "the file's own times"
    );
    assert_eq!(
        cues[2]["text"].as_str(),
        Some("Last line"),
        "the sidecar's last cue says something else"
    );

    // A file of the same torrent that is not a subtitle file, and a subtitle-looking name on a file the
    // torrent does not have, are both answers about the file. The status gets its own assertion before
    // the code is read out of the body: when this route stops knowing what a subtitle file is, the answer
    // is a cue list rather than an error, and the failure worth having is "notes.txt is not a subtitle
    // file" rather than a missing error code.
    let response = get(
        &http(),
        &served.url(&format!("/api/v1/media/{id}/0/subtitles/2")),
        None,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "notes.txt is not a subtitle file"
    );
    let (_, code) = error_of(response).await;
    assert_eq!(code, "unsupported_media");
    let (status, code) = error_of(
        get(
            &http(),
            &served.url(&format!("/api/v1/media/{id}/0/subtitles/9")),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "this torrent has no file 9");
    assert_eq!(code, "not_found");

    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cues_route_needs_both_bounds() {
    let served = Served::start("media-cue-bounds", |dir| {
        base_config(dir, vec![], Some(false))
    })
    .await;
    let id = seed_show(&served, None).await;

    // Both bounds are the client's to give. `start_ms` may be absent on the stream route because "no
    // start" means exactly one thing there; "no `to_ms`" could mean the end of the file, the end of the
    // widest window, or a client that forgot the parameter - and guessing either of the first two
    // answers "everything from here on" with ten quiet minutes. Absent is this route's `400`, same as a
    // value that does not parse.
    let cues = |query: &str| served.url(&format!("/api/v1/media/{id}/0/tracks/4/cues{query}"));
    for (query, why) in [
        ("", "a cue query with no window at all"),
        ("?from_ms=0", "a cue query missing to_ms"),
        ("?to_ms=10000", "a cue query missing from_ms"),
        (
            "?from_ms=&to_ms=10000",
            "an empty from_ms, which is not a time",
        ),
    ] {
        let response = get(&http(), &cues(query), None).await;
        let status = response.status();
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query} is {why}");
        let (_, code) = error_of(response).await;
        assert_eq!(
            code, "invalid_request",
            "{query} is {why} and got the wrong code"
        );
    }

    // The same three cues with the window spelled out, which is what every caller of this route now has
    // to do, the tests among them.
    let response = get(&http(), &cues("?from_ms=0&to_ms=10000"), None).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "an explicit window was refused"
    );
    assert_eq!(
        body(response).await["cues"]
            .as_array()
            .expect("a cue list")
            .len(),
        3,
        "an explicit window covering the whole file changed the answer"
    );

    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cue_text_keeps_its_lines() {
    let served = Served::start("media-cue-text", |dir| {
        base_config(dir, vec![], Some(false))
    })
    .await;
    let id = seed_show(&served, None).await;

    let answer = body(
        get(
            &http(),
            &served.url(&format!("/api/v1/media/{id}/0/subtitles/1")),
            None,
        )
        .await,
    )
    .await;
    let cues = answer["cues"].as_array().expect("a cue list");

    // The two-line cue arrives as two lines: one newline between them, and not a space where the break
    // was. Cleaning cue text with the display-safe function applied to the whole string would arrive here
    // instead as "Line one Line two", which is a cue nobody can read.
    let lines = cues[0]["text"].as_str().expect("a two-line cue");
    assert_eq!(lines, "Line one\nLine two", "the cue's lines were joined");
    assert_eq!(
        lines.matches('\n').count(),
        1,
        "a two-line cue arrived with {} newlines in it",
        lines.matches('\n').count()
    );

    // The cue that carries a terminal's colour escape loses the escape character - it is a control
    // character, and a control character in a JSON string is a thing a terminal cannot show and a bidi
    // override cannot be seen in - and keeps the printable text around it.
    let coloured = cues[1]["text"].as_str().expect("the coloured cue");
    assert_eq!(
        coloured, "Colour [31mred[0m here",
        "the escape was not removed: {coloured:?}"
    );
    for cue in cues {
        let text = cue["text"].as_str().expect("cue text");
        assert!(
            !text.contains('\u{1b}'),
            "a cue arrived with an escape character in it: {text:?}"
        );
    }

    // The same rule for a cue that comes out of a container rather than a file: the fixture's own SubRip
    // track holds a two-line cue, and it arrives with its break in it too.
    let answer = body(
        get(
            &http(),
            &served.url(&format!(
                "/api/v1/media/{id}/0/tracks/4/cues?from_ms=0&to_ms=10000"
            )),
            None,
        )
        .await,
    )
    .await;
    let cues = answer["cues"].as_array().expect("a cue list");
    assert_eq!(
        cues[0]["text"].as_str(),
        Some("First subtitle line\nspanning two lines"),
        "a container's two-line cue was joined into one line"
    );

    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn media_routes_refuse_a_stream_token() {
    let served = Served::start("media-auth", |dir| {
        base_config(dir, vec![alice()], Some(true))
    })
    .await;
    let token = bearer(&served).await;
    let id = seed_show(&served, Some(&token)).await;

    // A stream token is the credential a *player* uses, on the one address a player can reach with a query
    // string. On a route a browser can put a header on, the query spelling must not be accepted: it is the
    // one form of credential that leaks into a referer header, a browser history and a proxy log.
    let address = {
        let response = within(
            http()
                .post(served.url("/api/v1/stream-tokens"))
                .json(&serde_json::json!({ "torrent": id, "file": 0 }))
                .bearer_auth(&token)
                .send(),
        )
        .await
        .expect("the token request reached the server");
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "no token was issued"
        );
        let answer = body(response).await;
        answer["path"]
            .as_str()
            .expect("the answer names a path")
            .to_string()
    };
    let stream_token = token_of(&address);
    let probe = served.url(&format!("/api/v1/media/{id}/0"));

    let (status, code) =
        error_of(get(&http(), &format!("{probe}?token={stream_token}"), None).await).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a stream token opened a media route"
    );
    assert_eq!(code, "unauthorized");

    // The same torrent, the same file, the same route, with the header the browser was asked to send.
    let response = get(&http(), &probe, Some(&token)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a bearer token did not open the media route"
    );
    assert_eq!(
        body(response).await["tracks"]
            .as_array()
            .expect("tracks")
            .len(),
        5
    );

    // And with the cookie a sign-in handed out, which is what the page this API's own front end runs on
    // carries: a media request is a GET with no body, so the cookie and its CSRF value need no header.
    let signed_in = within(
        http()
            .post(served.url("/api/v1/auth/web-session"))
            .json(&serde_json::json!({ "user": USER, "password": SECRET }))
            .send(),
    )
    .await
    .expect("the sign-in reached the server");
    assert_eq!(
        signed_in.status(),
        StatusCode::OK,
        "the browser sign-in failed"
    );
    let cookie = cookie_of(&signed_in);
    let response = within(
        http()
            .get(served.url(&format!(
                "/api/v1/media/{id}/0/tracks/4/cues?from_ms=0&to_ms=10000"
            )))
            .header(reqwest::header::COOKIE, &cookie)
            .send(),
    )
    .await
    .expect("the server answered");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a cookie session did not open the cue route"
    );
    assert_eq!(
        body(response).await["cues"]
            .as_array()
            .expect("a cue list")
            .len(),
        3,
        "the cookie session got a different answer"
    );

    // Nothing at all, on a server where somebody has to sign in.
    let (status, code) = error_of(get(&http(), &probe, None).await).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an anonymous probe answered"
    );
    assert_eq!(code, "unauthorized");

    served.stop().await;
}

/// The same stream token, sent the other way: in the `Authorization: Bearer` header a sign-in hands its
/// own token in, on a route that accepts that header.
///
/// The query form is the form a player can use and the form that leaks into histories and proxy logs;
/// the header form is the one this API's own page uses, and it is reserved for the credential a sign-in
/// issued. A stream token names one file's stream address and nothing else, so treating it as a bearer
/// credential would make it a login for whatever the header is sent to, with a shorter lease on the
/// same machine. The last two requests are the control that this is a verdict about the credential and
/// not about the header: the same token on the address it was issued for streams, and the login token
/// in the same header on the same probe route answers as it always did.
#[tokio::test(flavor = "multi_thread")]
async fn media_routes_refuse_a_stream_token_as_bearer() {
    let served = Served::start("media-bearer-token", |dir| {
        base_config(dir, vec![alice()], Some(true))
    })
    .await;
    let token = bearer(&served).await;
    let id = seed_show(&served, Some(&token)).await;
    let address = {
        let response = within(
            http()
                .post(served.url("/api/v1/stream-tokens"))
                .json(&serde_json::json!({ "torrent": id, "file": 0 }))
                .bearer_auth(&token)
                .send(),
        )
        .await
        .expect("the token request reached the server");
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "no token was issued"
        );
        let answer = body(response).await;
        answer["path"]
            .as_str()
            .expect("the answer names a path")
            .to_string()
    };
    let stream_token = token_of(&address);
    let probe = served.url(&format!("/api/v1/media/{id}/0"));

    let response = get(&http(), &probe, Some(&stream_token)).await;
    // The status is asserted before anything is read out of the body: if the token opens the route, the
    // body is a probe answer rather than an error, and the failure worth having is the one that says a
    // stream token signed in.
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a stream token opened a media route as a bearer header"
    );
    let (_, code) = error_of(response).await;
    assert_eq!(code, "unauthorized");

    // The stream address the token was issued for, which is the one place it is a credential.
    let response = get(&http(), &served.url(&address), None).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the stream address its token was issued for did not stream"
    );
    let bytes = within(response.bytes()).await.expect("a body");
    assert!(!bytes.is_empty(), "and it carried bytes");

    // And the same header carrying the credential a sign-in issued.
    let response = get(&http(), &probe, Some(&token)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the bearer header itself was refused, which is not what this test is about"
    );
    assert_eq!(
        body(response).await["tracks"]
            .as_array()
            .expect("tracks")
            .len(),
        5
    );

    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn permits_are_released_when_the_client_goes_away() {
    let served = Served::start("media-permits", |dir| base_config(dir, vec![], Some(false))).await;
    // Two torrents, because the one thing these eight streams have to do is still be running when the
    // ninth asks: the video is here, so a stream of it finishes as fast as it can be written, while a
    // stream of a file no peer has is a job sitting on a read.
    let id = seed_show(&served, None).await;
    let absent = seed_absent(&served, None).await;
    // Far longer than the five seconds this test waits on, so that a place coming back inside five
    // seconds can only be the hang-up waking the read and not the timeout reaching it.
    served
        .server
        .set_media_read_timeout(Duration::from_secs(30));

    let stream = |torrent: &str| served.url(&format!("/api/v1/media/{torrent}/0/tracks/1/stream"));
    // The eight requests nobody is going to finish reading, and the client they are made with. The request
    // that asks about the pool uses a client of its own: one shared pool would let the asking request hand a
    // closing connection back to the very pool the eight are reusing, which is how a viewer's request would
    // come apart for the client's own reasons rather than because the server cancelled it.
    let viewers = http();
    let asking = http();
    // Spawned rather than awaited, because a request on a torrent with no data has no answer to await: each
    // future is the connection, and dropping it is the viewer hanging up.
    let hold = |client: &reqwest::Client, url: String| {
        let client = client.clone();
        tokio::spawn(async move { client.get(&url).send().await })
    };
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(hold(&viewers, stream(&absent)));
    }

    // The pool fills as fast as the server accepts these connections, and a connection can come apart for
    // the client's own reasons, so every round puts back whatever a viewer lost and asks again. The `503`
    // is what proves the pool full: the routes turn a request away before they touch the engine when no
    // place is free, so this answer does not depend on what the eight are doing or how far they got.
    let until = Instant::now() + Duration::from_secs(20);
    let response = loop {
        let mut live = 0;
        while live < held.len() {
            if held[live].is_finished() {
                held.push(hold(&viewers, stream(&absent)));
            }
            live += 1;
        }
        let response = get(&asking, &stream(&id), None).await;
        if response.status() == StatusCode::SERVICE_UNAVAILABLE {
            break response;
        }
        // A stream that got a place of its own is not evidence about the pool. Its whole body is the four
        // seconds of video on disk, which is short, and reading it to the end is what hands the connection
        // back for the next attempt.
        within(response.bytes())
            .await
            .expect("the stream that got through");
        if Instant::now() >= until {
            panic!("the ninth stream was never refused: the eight did not fill the pool");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (status, code) = error_of(response).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a full pool is not a `503`"
    );
    assert_eq!(
        code, "too_many_media_streams",
        "a full pool answered with the wrong code"
    );
    assert_eq!(
        retry_after.as_deref(),
        Some("1"),
        "a full pool has to say when to come back"
    );

    // The viewers hang up. Nothing else should be needed to get their places back: the reads are what hold
    // them, and a read whose response has gone is a read with nothing left to wait for.
    let hung_up_at = Instant::now();
    for task in held {
        task.abort();
    }
    loop {
        let response = get(&asking, &stream(&id), None).await;
        let status = response.status();
        let bytes = within(response.bytes()).await.expect("an answer");
        if status == StatusCode::OK {
            break;
        }
        let answer: Value =
            serde_json::from_slice(&bytes).expect("a stream that was turned away answers in JSON");
        assert_eq!(
            answer["error"]["code"].as_str(),
            Some("too_many_media_streams"),
            "after the viewers hung up a stream answered {status}: {answer}"
        );
        if hung_up_at.elapsed() >= Duration::from_secs(5) {
            panic!(
                "five seconds after the viewers hung up the pool was still full, \
                 which means a hung-up read is still sitting on the blocking pool"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let seconds = hung_up_at.elapsed().as_secs_f32();
    assert!(
        seconds < 5.0,
        "the pool was free after {seconds} s, not within five seconds"
    );

    served.stop().await;
}

/// The place in the media pool has to come back when a viewer hangs up on a stream that is still
/// waiting for bytes. That is the state [`permits_are_released_when_the_client_goes_away`] cannot
/// reach - its viewers hang up while their jobs are still waiting on a first piece, with nothing
/// across the wire - and this torrent is built for it: a middle piece of its video is blanked on
/// disk, starting just past the last byte the stream's first fragment needs (`seed_partial` says
/// why the hole has to sit exactly there), so every stream here serves its head and its first
/// fragment and then sits inside a read of the blanked window.
///
/// Each of the eight viewers gets its first chunk - the stream route answers 200 only after it has
/// produced the stream's first part, so a chunk is a running job holding its place - and then every
/// stream is confirmed silent mid-body: its buffered bytes drain, and after that it says nothing for
/// [`WAIT_SILENT`], with no end of stream either, which is waiting rather than finishing. The eight
/// checks run together, so a blocked read is seconds old by the hang-up, not a stream per viewer
/// old. Only then do all eight hang up at once, and a cue probe on the same torrent's sidecar - a
/// file whose bytes are all here - has to answer with its 200 within five seconds of the hang-up.
/// A job waiting on the missing piece has only two ways to stop waiting: the hang-up that wakes its
/// read, and the read's own timeout, which this test raises to two minutes - past everything the
/// checks and the probe together can take, so a place that comes back inside the window came back
/// *because* the body went away. Move the wake sender out of `Fragments` so a hang-up no longer
/// wakes a blocked read, and all eight jobs sit through the whole window - their reads have more
/// than a minute to run - the probe answers `503 too_many_media_streams` to the last retry, and
/// this test says so.
#[tokio::test(flavor = "multi_thread")]
async fn permit_returns_after_a_dropped_stream() {
    let served = Served::start("media-permit-return", |dir| {
        base_config(dir, vec![], Some(false))
    })
    .await;
    let id = seed_partial(&served, None).await;
    // This is the timeout a blocked read gives up on, and it is what separates "the hang-up woke
    // this job" from "the engine finally gave up on this piece". The eight viewers below run their
    // checks together, so by the hang-up a blocked read is one first fragment plus about one
    // WAIT_SILENT old - seconds - and the probe then has five more to answer in. Two minutes is
    // fifteen times that span: no read's deadline can be what frees the pool inside the window,
    // so a place that comes back inside it came back *because* its body went away - and a run
    // whose hang-ups wake nothing keeps the pool full to the last retry, because those jobs hand
    // their places back only at the timeout, a minute past the window. The job's other budget,
    // IDLE_SEND, is ten minutes and out of the question entirely.
    served
        .server
        .set_media_read_timeout(Duration::from_secs(120));

    // The video is file 1 here - the sidecar leads, which is what moves the piece boundaries - and
    // the cue probe below reads that sidecar as file 0.
    let stream = served.url(&format!("/api/v1/media/{id}/1/tracks/1/stream"));
    // The viewers hold their responses; the client probing after the hang-up has a client of its
    // own, for the reason the last test spells out: on a shared pool the probe could come apart on
    // a closing viewer connection rather than on the server's answer.
    let viewers = http();
    let asking = http();

    // Eight viewers, one for every place this server has for demuxing, each taken to its first
    // chunk and then watched for the stream to go quiet - all eight checks run together, so the
    // silence phase costs one WAIT_SILENT and not eight, and no blocked read gets old before the
    // hang-up. A viewer that was refused gets its own failure name, because it would leave the
    // pool looking free for a reason this test is not about. What each stream delivered before
    // the hole drains out of the connection first - those bytes are already on their way - and
    // then the stream has to go quiet without ending: a stream whose file ended says so with an
    // end of stream, and a stream that was still waiting says nothing. Every way this can fail
    // is named, because it is the point of the test: an early end means the stream stopped
    // waiting, and a broken connection means the viewer came apart for the client's own reasons.
    // Each check hands back the response it proved silent, and dropping all eight of those at
    // once is the hang-up.
    let mut checking = JoinSet::new();
    for _ in 0..8 {
        let viewer = viewers.clone();
        let stream = stream.clone();
        checking.spawn(async move {
            let mut response = match within(viewer.get(&stream).send()).await {
                Ok(response) => response,
                Err(problem) => {
                    return Err(format!("a viewer did not reach the server: {problem}"));
                }
            };
            if response.status() != StatusCode::OK {
                let (status, code) = error_of(response).await;
                return Err(format!(
                    "a video with a middle piece missing was not streamed: {status} {code}"
                ));
            }
            match within(response.chunk()).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err("a stream that answered with headers has a body".to_string());
                }
                Err(problem) => {
                    return Err(format!(
                        "a viewer's stream came apart before its first bytes: {problem}"
                    ));
                }
            }
            loop {
                match tokio::time::timeout(WAIT_SILENT, response.chunk()).await {
                    Ok(Ok(Some(_))) => continue,
                    Ok(Ok(None)) => {
                        return Err(
                            "a viewer's stream ended instead of waiting on the missing piece"
                                .to_string(),
                        );
                    }
                    Ok(Err(problem)) => {
                        return Err(format!("a viewer's stream came apart: {problem}"));
                    }
                    Err(_) => return Ok(response),
                }
            }
        });
    }
    let mut held: Vec<Response> = Vec::new();
    while let Some(check) = checking.join_next().await {
        match check.expect("a viewer's silence check ran") {
            Ok(response) => held.push(response),
            Err(problem) => panic!("a viewer's check failed: {problem}"),
        }
    }

    // Everyone hangs up at once. Eight jobs that have served bytes and are waiting for more hold
    // every place in the pool, and within five seconds only the bodies going away can free them.
    let dropped_at = Instant::now();
    drop(held);
    let probe = served.url(&format!("/api/v1/media/{id}/1/subtitles/0"));
    loop {
        let response = get(&asking, &probe, None).await;
        let status = response.status();
        if status == StatusCode::OK {
            let answer = body(response).await;
            assert_eq!(
                answer["cues"].as_array().expect("a cue list").len(),
                SIDECAR_CUES,
                "the freed place answered the sidecar's cues wrong"
            );
            break;
        }
        let (_, code) = error_of(response).await;
        assert_eq!(
            code, "too_many_media_streams",
            "after the hang-ups a stream answered {status} with the wrong code"
        );
        if dropped_at.elapsed() >= Duration::from_secs(5) {
            panic!(
                "five seconds after eight viewers hung up mid-stream the pool was still full: \
                 a blocked job did not wake when its body went away"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    served.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn read_timeout_ends_the_stream() {
    let served = Served::start("media-timeout", |dir| base_config(dir, vec![], Some(false))).await;
    let absent = seed_absent(&served, None).await;
    // The window a route waits on the engine for one read, shortened from the sixty-second default.
    served.server.set_media_read_timeout(Duration::from_secs(1));

    let client = http();
    let until = Instant::now() + DEADLINE;
    loop {
        let attempt = Instant::now();
        let response = get(
            &client,
            &served.url(&format!("/api/v1/media/{absent}/0")),
            None,
        )
        .await;
        let status = response.status();
        // A torrent whose metadata has not arrived is a different answer, and the route is not what is
        // being tested: try again until the file list is there and the read is what fails.
        if status == StatusCode::CONFLICT {
            if attempt >= until {
                panic!("the torrent's metadata never arrived");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        let (status, code) = error_of(response).await;
        assert_eq!(
            status,
            StatusCode::GATEWAY_TIMEOUT,
            "a file no peer has was not answered as a timeout"
        );
        assert_eq!(code, "read_timeout", "the wrong code for a timeout");
        assert!(
            attempt.elapsed() < Duration::from_secs(3),
            "the probe waited {} s on a one second read timeout",
            attempt.elapsed().as_secs_f32()
        );
        break;
    }

    // The same file, on the route that seeks instead of probing: the read timeout is the reader's rule, so
    // every route that reads gets the same answer, and none of them waits the default sixty seconds.
    let attempt = Instant::now();
    let response = get(
        &client,
        &served.url(&format!(
            "/api/v1/media/{absent}/0/tracks/1/stream?start_ms=1000"
        )),
        None,
    )
    .await;
    let (status, code) = error_of(response).await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "a seek into a file nobody has was not a timeout"
    );
    assert_eq!(code, "read_timeout");
    assert!(
        attempt.elapsed() < Duration::from_secs(3),
        "the stream route waited {} s on a one second read timeout",
        attempt.elapsed().as_secs_f32()
    );

    // A cue window on the same absent file, and the sidecar route on its `.srt` too: the timeout is the
    // reader's rule, so every route that reads gets the same answer and none of them waits out the
    // default sixty seconds.
    for path in [
        format!("/api/v1/media/{absent}/0/tracks/4/cues?from_ms=0&to_ms=10000"),
        format!("/api/v1/media/{absent}/0/subtitles/1"),
    ] {
        let response = get(&client, &served.url(&path), None).await;
        let (status, code) = error_of(response).await;
        assert_eq!(
            status,
            StatusCode::GATEWAY_TIMEOUT,
            "{path} did not time out like the rest"
        );
        assert_eq!(code, "read_timeout", "{path} gave the wrong code");
    }

    served.stop().await;
}

// What a play does to the file it plays: the file goes to High priority and every other file keeps what
// it had.
//
// The five tests here are about the file list rather than the bytes. The stream address a player is
// handed is the address it always got; what changed is which file this server is now asked for first,
// and the file list read back right after the play is where that shows. The three-file torrent is the
// one every media test here plays from (the video, its `.srt` sidecar, then a text file), and one of
// its files is always put on Skip first: a play that rebuilt the list from scratch rather than raising
// one row of it would look correct until a file with something to keep is in it.

/// `POST /api/v1/stream-tokens` is what the web player's Play button, the desktop app's Open in player
/// and `s` against a server all ask for an address with: the file it names goes to High, and the video
/// beside it and the skipped file below it are left alone.
#[tokio::test(flavor = "multi_thread")]
async fn stream_token_raises_the_file_to_high() {
    let served = Served::start("media-token-raise", |dir| {
        base_config(dir, vec![], Some(false))
    })
    .await;
    let id = seed_show(&served, None).await;
    put_priorities(&served, &id, &["normal", "normal", "skip"]).await;

    token_for(&served, &id, 1).await;

    assert_eq!(
        priorities_of(&served, &id).await,
        vec!["normal", "high", "skip"],
        "an address for file 1 did not leave the other two files as they were"
    );

    served.stop().await;
}

/// A file the person already pushed to the top is not moved by playing it: the play asks for the same
/// priority it already had, so the engine is not asked to lower it back down.
#[tokio::test(flavor = "multi_thread")]
async fn stream_token_keeps_a_high_file_high() {
    let served = Served::start("media-token-keep-high", |dir| {
        base_config(dir, vec![], Some(false))
    })
    .await;
    let id = seed_show(&served, None).await;
    put_priorities(&served, &id, &["normal", "high", "skip"]).await;

    token_for(&served, &id, 1).await;

    assert_eq!(
        priorities_of(&served, &id).await,
        vec!["normal", "high", "skip"],
        "playing a file that was already at the top moved it off the top"
    );

    served.stop().await;
}

/// A file the person had set to Skip, played anyway, is asked for: pressing Play on it is them changing
/// their mind about that file, and the priority list they set for the others stays as they set it.
#[tokio::test(flavor = "multi_thread")]
async fn a_skipped_file_played_is_raised() {
    let served = Served::start("media-token-raise-skip", |dir| {
        base_config(dir, vec![], Some(false))
    })
    .await;
    let id = seed_show(&served, None).await;
    put_priorities(&served, &id, &["normal", "normal", "skip"]).await;

    token_for(&served, &id, 2).await;

    assert_eq!(
        priorities_of(&served, &id).await,
        vec!["normal", "normal", "high"],
        "playing the skipped file left it skipped, or moved one of the other two"
    );

    served.stop().await;
}

/// The web player's own route, the one that streams the track: it raises the file the same way, from a
/// lower priority than the token tests start at, and for a track of the video rather than of the sidecar.
#[tokio::test(flavor = "multi_thread")]
async fn track_stream_raises_the_file() {
    let served = Served::start("media-stream-raise", |dir| {
        base_config(dir, vec![], Some(false))
    })
    .await;
    let id = seed_show(&served, None).await;
    put_priorities(&served, &id, &["low", "normal", "skip"]).await;

    let response = get(
        &http(),
        &served.url(&format!("/api/v1/media/{id}/0/tracks/1/stream")),
        None,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the video track did not stream, which is not what this test is about"
    );
    drop(response);

    assert_eq!(
        priorities_of(&served, &id).await,
        vec!["high", "normal", "skip"],
        "streaming the video did not put it at the head of the queue, or moved a file nobody played"
    );

    served.stop().await;
}

/// The routes that only read a file's header, or read a different file altogether, are not plays: a probe
/// and a cue read must not change what this server is asked for first.
#[tokio::test(flavor = "multi_thread")]
async fn a_probe_and_a_cue_read_change_no_priority() {
    let served = Served::start("media-read-keeps", |dir| {
        base_config(dir, vec![], Some(false))
    })
    .await;
    let id = seed_show(&served, None).await;
    put_priorities(&served, &id, &["low", "normal", "skip"]).await;
    let before = priorities_of(&served, &id).await;

    for path in [
        format!("/api/v1/media/{id}/0"),
        format!("/api/v1/media/{id}/0/tracks/4/cues?from_ms=0&to_ms=10000"),
        format!("/api/v1/media/{id}/0/subtitles/1"),
    ] {
        let response = get(&http(), &served.url(&path), None).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{path} did not answer as it used to, so its control here proves nothing"
        );
    }

    assert_eq!(
        priorities_of(&served, &id).await,
        before,
        "a route that only reads changed the file list"
    );

    served.stop().await;
}
