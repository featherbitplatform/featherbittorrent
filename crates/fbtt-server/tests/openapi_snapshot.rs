//! The API document the web UI is typed from, kept identical to the one this server serves.
//!
//! The front end builds its API types from a copy of the document under `web/src/api/`, and a copy goes
//! stale the moment the routes move. What is checked here is that it has not: a server is started, the
//! document is fetched from `GET /api/v1/openapi.json` - the one route fbt-api answers without a credential,
//! which is what a browser that has not signed in yet fetches - and the response, pretty-printed with a
//! trailing newline, is compared with the file byte for byte, after the one thing a checkout can change
//! without anybody editing the file (a `\r\n` where the file holds `\n`; see the comparison below). A difference in formatting counts as a
//! difference in content for the same reason a difference in content does: the generated types are made from
//! the file, so the file has to say exactly what the server says.
//!
//! Run it with `UPDATE_OPENAPI=1` and it writes the file with what the server serves instead of comparing,
//! which is the fix the failure message gives. That variable is the only thing read from the process
//! environment in this file, and it only picks between writing and comparing: it changes nothing about the
//! server or about what is compared. The repository root is baked in from `CARGO_MANIFEST_DIR` at compile
//! time for the same reason - a test here should not be reading its environment to find the source tree.
//!
//! The engine settings are loopback-only, as in `tests/web_session.rs`: this starts an engine to have
//! something to serve, and nothing it does can leave the machine.

use std::path::Path;
use std::time::Duration;

use fbtt_core::scratch::ScratchDir;
use fbtt_core::{Config, LibraryLock, Paths, ServerConfig};
use fbtt_server::Server;

/// How long a test waits for anything from the server. Starting an engine takes a moment; nothing here
/// should take longer than that many times over.
const DEADLINE: Duration = Duration::from_secs(60);
/// Where the web UI keeps its copy of the document, relative to the repository root.
const DOC_PATH: &str = "web/src/api/openapi.json";
/// What a mismatch tells the person who saw it, since the fix is one command and not a decision.
const OUT_OF_DATE: &str = concat!(
    "web/src/api/openapi.json is out of date: run UPDATE_OPENAPI=1 cargo test -p fbtt-server ",
    "--test openapi_snapshot"
);

/// Runs `what` under the deadline, so a server that never answers is a failure with a message rather than
/// a hung test job.
async fn within<T>(what: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, what)
        .await
        .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
}

/// The repository root: two levels up from `crates/fbtt-server`, from the manifest directory cargo compiles
/// in. The crate lives in `crates/`, so its parent's parent is the root the path above is relative to.
fn repo_root() -> &'static Path {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("fbtt-server lives in a crates directory");
    crates
        .parent()
        .expect("crates lives in the repository root")
}

/// The settings every engine here runs on: this config's, with the listening moved to loopback on a port
/// the OS picks and with DHT, LSD, UPnP and NAT-PMP turned off.
fn loopback(config: &Config) -> fbt::Settings {
    let mut settings = config.session_settings();
    settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
    settings.dht_enabled = false;
    settings.enable_lsd = false;
    settings.enable_upnp = false;
    settings.enable_natpmp = false;
    settings
}

/// A config that serves on a port the OS picks, with everything else left at its default: auth left unset,
/// which on a loopback bind means nobody has to sign in. The document is the one route fbt-api serves
/// without a credential whatever that section says, so which way this falls is beside the point; it is left
/// alone because this test is about the document and not about who may read the rest.
fn base_config(scratch: &Path) -> Config {
    Config {
        download_dir: scratch.join("downloads"),
        server: Some(ServerConfig {
            bind: "127.0.0.1:0".parse().expect("a loopback bind address"),
            ..ServerConfig::default()
        }),
        ..Config::default()
    }
}

/// The client every request goes through: no redirects followed, no per-request timeout ([`within`] bounds
/// the wait), and no proxy (every URL here is `127.0.0.1`). The same shape the other server tests build.
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("an http client")
}

/// The file's bytes with a checkout's line-ending translation undone: every `\r\n` pair becomes
/// `\n`, and a `\r` that is not part of a pair is left alone. Used on the file's side of the
/// comparison only, for checkouts made before the root `.gitattributes` pinned this file to LF;
/// see [`the_web_ui_has_the_current_api_document`].
fn with_lf_endings(bytes: &[u8]) -> Vec<u8> {
    let mut folded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' && index + 1 < bytes.len() && bytes[index + 1] == b'\n' {
            index += 1;
        }
        folded.push(bytes[index]);
        index += 1;
    }
    folded
}

/// The document as this server serves it, pretty-printed with a trailing newline: the exact bytes the file
/// holds, so a reformat and a change of content fail the same way.
///
/// The response is parsed and written again rather than compared as the bytes it arrived in. The route
/// answers the document in one line; the file is kept pretty-printed because a diff of an API document that
/// moved one field is something a person has to read, in a review and in this test's failure. Re-serialising
/// the parsed response is how the two are asked about their content rather than about their spelling, and it
/// puts the keys in `serde_json`'s own order - alphabetical here, where nothing in the tree asks it for
/// `preserve_order` - so a server that writes the same fields in another order serves the same file.
async fn served_document(server: &Server) -> String {
    let url = format!("http://{}{}", server.local_addr(), "/api/v1/openapi.json");
    let response = within(http().get(&url).send())
        .await
        .expect("the server answered");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "the document route did not answer with the document",
    );
    let document = within(response.json::<serde_json::Value>())
        .await
        .expect("a JSON body");
    let mut pretty =
        serde_json::to_string_pretty(&document).expect("the served document serialises again");
    pretty.push('\n');
    pretty
}

/// The web UI's copy of the API document is the one this server serves.
#[tokio::test]
async fn the_web_ui_has_the_current_api_document() {
    let dir = ScratchDir::new("openapi-snapshot");
    let paths = Paths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
    };
    let config = base_config(dir.path()).resolve(&paths);
    let lock = LibraryLock::acquire(&paths.lock_file()).expect("lock the library");
    let server = within(Server::start_with_settings(
        loopback(&config),
        config,
        &paths,
        lock,
    ))
    .await
    .expect("start the server");

    let served = served_document(&server).await;
    let file = repo_root().join(DOC_PATH);
    // Any value switches it on; `1` is what the message above says to run. Rewriting rather than
    // comparing is the whole difference this makes: the server, the fetch and the bytes are the same.
    if std::env::var("UPDATE_OPENAPI").is_ok() {
        // The folder is created here because it is not in the tree until the first run of this test puts
        // it there, and a write that failed for a missing folder would read as a broken server.
        std::fs::create_dir_all(file.parent().expect("the document has a parent directory"))
            .expect("create the folder the document goes in");
        std::fs::write(&file, served.as_bytes()).expect("write the document");
    } else {
        // Read as bytes, not as text: the comparison is byte-exact, and the file is bytes the server
        // wrote, so anything the two disagree about - a lone CR, a re-encoding, one space of
        // indentation - has to fail here rather than be smoothed over on the way in. The one thing
        // that is folded is `\r\n` into `\n`, and on the file's side only: a checkout made before the
        // root `.gitattributes` pinned this file to LF holds the CRLF line endings Git checks text
        // files out with on Windows, and that checkout is older than the fix, so the test has to
        // tolerate exactly those endings and nothing else. With the attribute in place every fresh
        // checkout carries the LF the file is committed with, and the fold changes nothing.
        let on_disk = std::fs::read(&file)
            .unwrap_or_else(|e| panic!("{DOC_PATH} could not be read: {e}. {OUT_OF_DATE}"));
        let on_disk = with_lf_endings(&on_disk);
        let served = served.as_bytes();
        // Both sides are hundreds of kilobytes, so the failure says where they part company instead of
        // printing two documents at the person.
        if on_disk != served {
            let at = on_disk
                .iter()
                .zip(served)
                .position(|(from_file, from_server)| from_file != from_server)
                .unwrap_or(on_disk.len().min(served.len()));
            panic!(
                "{OUT_OF_DATE}\nThe two first differ at byte offset {at}: the file is {} bytes, what \
                 this server serves is {}.",
                on_disk.len(),
                served.len(),
            );
        }
    }

    // The server goes down before the scratch directory does, as in the other tests here: everything it
    // writes is inside that directory, and its `Drop` is what takes the directory away.
    within(server.shutdown()).await.expect("the last save ran");
    drop(dir);
}
