//! The loopback endpoint that streams a local file through fbt-api's own route.
//!
//! A torrent that is not seeding yet (or whose wanted file is not fully on disk) is
//! played through this endpoint: fbt-api's `GET /api/v1/torrents/{id}/files/{index}/stream`
//! route, bound to `127.0.0.1` on a port the OS picks. The authenticator lets through
//! exactly one kind of request: a `GET` or `HEAD` on the file-stream route, from a
//! loopback `Host`, carrying the token that was scoped to that one file's path.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::OnceLock;

use axum::http::{Method, header};
use fbt::Session;
use fbt_api::{ApiConfig, ApiServer, Auth, AuthRejection, AuthRequest, Authenticator};

use crate::client::{StreamUrl, TorrentId};
use crate::stream::stream_path;
use crate::stream::tokens::StreamTokens;

/// The matched-path template of the file-stream route, as fbt-api sets it.
const STREAM_ROUTE: &str = "/api/v1/torrents/{id}/files/{index}/stream";
/// How many live stream tokens the endpoint's store may hold. Each play issues a
/// fresh token, and a player may still be using a token from an earlier play when
/// a newer play issues one, so the cap is the number of plays that could be open at
/// once: a player that seeks re-requests the URL it was given, and a full store
/// evicts the least recently used token rather than one a player is still on.
const TOKEN_CAP: usize = 64;
/// How many concurrent streams fbt-api serves on this endpoint.
const MAX_STREAMS: usize = 8;

/// Which source a local file plays from, and the decision behind it (decision 4 in
/// the plan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Choice {
    /// The file is complete on disk: hand the player its path.
    Path,
    /// The file is not (or the state is not `Seeding`): stream it through the endpoint.
    Stream,
}

/// The file a `Command::Stream` plays: the wanted index if it names a non-pad file,
/// else the largest non-pad file (lowest index on a tie), `None` when there is none.
///
/// It is generic over the one thing it reads of a file, so a test can hand it
/// values the engine's type would not be built from outside the crate.
pub(crate) fn choose_file<F>(files: &[F], wanted: Option<usize>) -> Option<usize>
where
    F: Indexable,
{
    if let Some(index) = wanted
        && let Some(file) = files.get(index)
        && !file.pad()
    {
        return Some(index);
    }
    files
        .iter()
        .filter(|file| !file.pad())
        .max_by_key(|file| (file.len(), std::cmp::Reverse(file.index())))
        .map(|file| file.index())
}

/// What [`choose_file`] reads of one file: its position in the list, whether it is
/// a pad, and its length.
pub(crate) trait Indexable {
    /// Its index in the list.
    fn index(&self) -> usize;
    /// Whether it is a pad file.
    fn pad(&self) -> bool;
    /// Its length in bytes.
    fn len(&self) -> u64;
}

impl Indexable for fbt::FileStatus {
    fn index(&self) -> usize {
        self.index
    }
    fn pad(&self) -> bool {
        self.pad
    }
    fn len(&self) -> u64 {
        self.len
    }
}

/// The decision of which source a local file plays from (decision 4 in the plan):
/// a path only when the torrent is seeding, the file is not skipped, every byte of
/// it is verified, and it exists on disk at exactly that length.
pub(crate) fn local_source(
    state: fbt::TorrentState,
    file: &fbt::FileStatus,
    on_disk_len: Option<u64>,
) -> Choice {
    if state == fbt::TorrentState::Seeding
        && file.priority != fbt::Priority::Skip
        && file.downloaded == file.len
        && on_disk_len == Some(file.len)
    {
        Choice::Path
    } else {
        Choice::Stream
    }
}

/// The fbt-api authenticator of the local endpoint: the file-stream route alone,
/// a loopback `Host`, and the token scoped to that one file's path.
///
/// `host` is the endpoint's bound address as `127.0.0.1:<port>`; it is only known
/// after binding, so it is set behind the lock once `ApiServer::start` returns.
#[derive(Debug)]
pub struct LocalStreamAuth {
    /// The tokens that open this endpoint's files.
    pub tokens: Arc<StreamTokens>,
    /// `127.0.0.1:<port>`: set after the bind, `None` while the endpoint is starting
    /// (a request in that moment is refused, which is closed).
    host: OnceLock<SocketAddr>,
}

impl LocalStreamAuth {
    /// A fresh authenticator whose host is not set yet: set it with
    /// [`set_host`](Self::set_host) once the endpoint has bound its port.
    pub fn new(tokens: Arc<StreamTokens>) -> Self {
        LocalStreamAuth {
            tokens,
            host: OnceLock::new(),
        }
    }

    /// Records the endpoint's bound address, the first time only.
    pub fn set_host(&self, host: SocketAddr) {
        let _ = self.host.set(host);
    }

    /// Whether `request` is one this endpoint serves: a `GET` or `HEAD` on the file-stream
    /// route, from this endpoint's own `127.0.0.1:<port>` or `localhost:<port>`, nothing
    /// else. `Host` is checked before the route, so a request for another host is refused
    /// even on the right path with the right token.
    fn serves(&self, request: &AuthRequest<'_>) -> bool {
        let (method, matched) = (request.method, request.matched_path);
        if *method != Method::GET && *method != Method::HEAD {
            return false;
        }
        if matched != Some(STREAM_ROUTE) {
            return false;
        }
        let Some(host) = self.host.get() else {
            // The port is not set yet: the bind is in flight. Fail closed.
            return false;
        };
        let Some(header) = request.headers.get(header::HOST) else {
            return false;
        };
        let Ok(text) = header.to_str() else {
            return false;
        };
        let port = host.port().to_string();
        format!("127.0.0.1:{port}") == text || format!("localhost:{port}") == text
    }
}

impl Authenticator for LocalStreamAuth {
    fn authorize(&self, request: &AuthRequest<'_>) -> Result<(), AuthRejection> {
        // Anything that is not the file-stream route, the right method, and this endpoint's
        // own host is `403`: there is nothing on this server it could open.
        if !self.serves(request) {
            return Err(AuthRejection::Forbidden);
        }
        // The token, scoped to this one file's path: the compare is byte for byte, so
        // another spelling of the same file is refused.
        let Some(token) = request.query_token else {
            return Err(AuthRejection::Unauthorized);
        };
        if self.tokens.check(token, request.path) {
            Ok(())
        } else {
            Err(AuthRejection::Unauthorized)
        }
    }
}

/// The loopback endpoint: fbt-api's file-stream route on `127.0.0.1:0`, with the
/// tokens that open its files.
pub struct LocalEndpoint {
    server: Option<ApiServer>,
    addr: SocketAddr,
    /// The tokens the endpoint's authenticator checks on: a URL this endpoint
    /// hands out must pass its own server, so it is the store the authenticator
    /// holds, kept here to issue against.
    tokens: Arc<StreamTokens>,
}

impl std::fmt::Debug for LocalEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalEndpoint")
            .field("addr", &self.addr)
            .finish()
    }
}

impl LocalEndpoint {
    /// Starts the endpoint on the session: binds `127.0.0.1:0` (the OS picks the port),
    /// serves fbt-api's routes behind [`LocalStreamAuth`], and returns the endpoint with
    /// its bound address. The error is the reason the bind or the config failed.
    pub async fn start(session: Session) -> Result<LocalEndpoint, String> {
        let tokens = Arc::new(StreamTokens::new(TOKEN_CAP));
        let auth = Arc::new(LocalStreamAuth::new(Arc::clone(&tokens)));
        let mut config = ApiConfig::new("unused");
        config.bind = "127.0.0.1:0".parse().expect("a loopback bind address");
        config.auth = Auth::Custom(auth.clone());
        config.max_streams = MAX_STREAMS;
        let server = ApiServer::start(session, config)
            .await
            .map_err(|error| error.to_string())?;
        let addr = server.local_addr();
        // The port is known only now, after the bind: the authenticator checks `Host`
        // against it, so it is set here, before any request can arrive.
        auth.set_host(addr);
        // The endpoint's token store is the one the authenticator holds: a URL it
        // hands out must pass its own server, so it is issued from that store.
        Ok(LocalEndpoint {
            server: Some(server),
            addr,
            tokens,
        })
    }

    /// The bound address, with the port the OS picked.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL that plays file `index` of torrent `id`: a token issued for
    /// `stream_path(id, index)`, on this endpoint's address. The URL and its
    /// token are the one string the token was scoped to: never re-spelled.
    pub fn url_for(&self, id: &TorrentId, index: usize) -> StreamUrl {
        let path = stream_path(id, index);
        let token = self.tokens.issue(path.clone());
        let url = format!("http://{}{}?token={}", self.addr, path, token);
        StreamUrl::new(url)
    }

    /// Stops the endpoint: closes the listener and ends every stream it serves.
    pub async fn shutdown(mut self) {
        if let Some(server) = self.server.take() {
            server.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    /// How long a test in this module waits for anything.
    const DEADLINE: Duration = Duration::from_secs(60);

    /// Runs `what` under the deadline.
    async fn within<T>(what: impl Future<Output = T>) -> T {
        tokio::time::timeout(DEADLINE, what)
            .await
            .unwrap_or_else(|_| panic!("nothing happened within {} seconds", DEADLINE.as_secs()))
    }

    use axum::http::{HeaderMap, HeaderValue, Method};
    use fbt_api::{AuthRejection, AuthRequest, Authenticator};

    use super::{Choice, Indexable, LocalStreamAuth, choose_file, local_source};
    use crate::client::TorrentId;
    use crate::stream::stream_path;
    use crate::stream::tokens::StreamTokens;

    fn id() -> TorrentId {
        TorrentId("0123456789abcdef0123456789abcdef01234567".to_string())
    }

    /// A file as [`choose_file`] sees it: the engine's `FileStatus` is not buildable
    /// outside the crate, so the function is tested through this stand-in.
    struct F {
        index: usize,
        len: u64,
        pad: bool,
    }

    impl Indexable for F {
        fn index(&self) -> usize {
            self.index
        }
        fn pad(&self) -> bool {
            self.pad
        }
        fn len(&self) -> u64 {
            self.len
        }
    }

    fn f(index: usize, len: u64, pad: bool) -> F {
        F { index, len, pad }
    }

    /// The wanted index wins when it names a real file; otherwise the largest
    /// non-pad file, the lowest index on a tie.
    #[test]
    fn choose_file_prefers_wanted_then_largest() {
        let files = vec![
            f(0, 10, false),
            f(1, 30, false),
            f(2, 20, false),
            f(3, 40, false),
        ];
        // The wanted index, when it names a real file.
        assert_eq!(choose_file(&files, Some(2)), Some(2));
        // The largest non-pad file when nothing is wanted.
        assert_eq!(choose_file(&files, None), Some(3));
        // A pad file is not chosen, even when it is the largest.
        let with_pad = vec![f(0, 10, false), f(4, 99, true)];
        assert_eq!(choose_file(&with_pad, None), Some(0));
        // A pad file is not named, even when wanted: the wanted index is refused
        // and the largest non-pad file is played instead.
        assert_eq!(choose_file(&with_pad, Some(4)), Some(0));
        // All files are pads: nothing to play.
        let all_pad = vec![f(0, 10, true), f(1, 99, true)];
        assert_eq!(choose_file(&all_pad, None), None);
        assert_eq!(choose_file(&all_pad, Some(1)), None);
        // A tie in size: the lowest index.
        let tie = vec![f(0, 50, false), f(1, 50, false)];
        assert_eq!(choose_file(&tie, None), Some(0));
        // No file at all.
        assert_eq!(choose_file::<F>(&[], None), None);
        // An out-of-range wanted index falls back to the largest.
        assert_eq!(choose_file(&files, Some(99)), Some(3));
    }

    /// Each condition of decision 4, one case per: `Path` only when all four hold.
    ///
    /// The file comes from a real torrent the engine builds, because `FileStatus`
    /// is not buildable outside the crate; the state and the on-disk length are
    /// what the decision reads beside it, so they are the test's to vary. The
    /// torrent is seeded (data written, check complete) so `downloaded == len`,
    /// which is the condition `Path` needs beside the state and the on-disk
    /// length.
    #[tokio::test]
    async fn local_source_rules() {
        let dir = crate::scratch::ScratchDir::new("local_source_rules");
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, vec![7u8; 100]).expect("write a test file");
        let bytes = tokio::task::spawn_blocking(move || {
            fbt::CreateTorrent::new(path)
                .build(&mut |_done, _total| std::ops::ControlFlow::Continue(()))
                .expect("hash the test torrent")
        })
        .await
        .expect("the hashing task ran");
        let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
        let mut settings = fbt::Settings::default();
        settings.listen_addrs = vec!["127.0.0.1:0".parse().expect("a loopback listen address")];
        settings.dht_enabled = false;
        settings.enable_lsd = false;
        settings.enable_upnp = false;
        settings.enable_natpmp = false;
        let session = fbt::Session::new(settings)
            .await
            .expect("a session to read the files of");
        let torrent = session
            .add_torrent(fbt::AddTorrent::new(metainfo, dir.path().to_path_buf()))
            .await
            .expect("add the torrent");
        // The torrent must finish its check: only then is `downloaded == len`.
        within(async {
            loop {
                if torrent.status().state == fbt::TorrentState::Seeding {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await;
        let files = torrent.files().await.expect("the file list");
        let full = files
            .iter()
            .find(|file| file.len == 100)
            .expect("the 100-byte file");
        assert_eq!(full.len, 100, "the file is 100 bytes");
        assert_eq!(
            full.downloaded, full.len,
            "the file is fully downloaded after the check"
        );

        // All four hold: seeding, not skipped, fully downloaded, on disk at that length.
        assert_eq!(
            local_source(fbt::TorrentState::Seeding, full, Some(100)),
            Choice::Path
        );
        // Not seeding: `Downloading` at 100 % still streams.
        assert_eq!(
            local_source(fbt::TorrentState::Downloading, full, Some(100)),
            Choice::Stream
        );
        // Not on disk at all.
        assert_eq!(
            local_source(fbt::TorrentState::Seeding, full, None),
            Choice::Stream,
            "a file that is not on disk at its length streams"
        );
        // On disk but at the wrong length.
        assert_eq!(
            local_source(fbt::TorrentState::Seeding, full, Some(99)),
            Choice::Stream
        );
        // A skipped file streams even when it is complete.
        let mut priorities = vec![fbt::Priority::Normal; files.len()];
        priorities[full.index] = fbt::Priority::Skip;
        torrent
            .set_file_priorities(priorities)
            .await
            .expect("skip the file");
        let files = torrent.files().await.expect("the file list again");
        let skipped = files
            .iter()
            .find(|file| file.index == full.index)
            .expect("the skipped file");
        assert_eq!(skipped.priority, fbt::Priority::Skip);
        assert_eq!(
            local_source(fbt::TorrentState::Seeding, skipped, Some(100)),
            Choice::Stream
        );
        session.shutdown().await;
    }

    /// A host other than `127.0.0.1:<port>` or `localhost:<port>` is refused, the
    /// file-stream route and method are required, and the token must match the path.
    ///
    /// `AuthRequest` borrows its parts, so the request and the check are one
    /// closure each: the test names the case, the closure builds the request and
    /// answers it.
    #[test]
    fn local_auth_routes() {
        let addr: SocketAddr = "127.0.0.1:8090".parse().expect("an address");
        let tokens = Arc::new(StreamTokens::new(16));
        let auth = LocalStreamAuth::new(Arc::clone(&tokens));
        auth.set_host(addr);

        let path = stream_path(&id(), 0);
        let other = stream_path(&id(), 1);
        let token = tokens.issue(path.clone());
        let route = "/api/v1/torrents/{id}/files/{index}/stream";
        let get = Method::GET;
        let head = Method::HEAD;
        let post = Method::POST;

        fn check(
            auth: &LocalStreamAuth,
            method: &Method,
            matched: Option<&str>,
            path: &str,
            token: Option<&str>,
            host: Option<&str>,
        ) -> Result<(), AuthRejection> {
            let mut headers = HeaderMap::new();
            if let Some(host) = host {
                headers.insert(
                    axum::http::header::HOST,
                    HeaderValue::from_str(host).expect("a host header"),
                );
            }
            let request = AuthRequest::new(method, matched, &headers, token, None).with_path(path);
            auth.authorize(&request)
        }

        // The right token, the right path, the right host: allowed.
        assert!(
            check(
                &auth,
                &get,
                Some(route),
                &path,
                Some(&token),
                Some("127.0.0.1:8090")
            )
            .is_ok(),
            "the right token and path are allowed"
        );
        // `localhost:<port>` is allowed too.
        assert!(
            check(
                &auth,
                &get,
                Some(route),
                &path,
                Some(&token),
                Some("localhost:8090")
            )
            .is_ok(),
            "localhost is allowed"
        );
        // `HEAD` is allowed too.
        assert!(
            check(
                &auth,
                &head,
                Some(route),
                &path,
                Some(&token),
                Some("127.0.0.1:8090")
            )
            .is_ok(),
            "HEAD is allowed"
        );

        // The right token, the wrong file: `401`.
        assert!(
            matches!(
                check(
                    &auth,
                    &get,
                    Some(route),
                    &other,
                    Some(&token),
                    Some("127.0.0.1:8090")
                ),
                Err(AuthRejection::Unauthorized)
            ),
            "the right token for another file is refused"
        );

        // A route that is not the file-stream route: `403`.
        assert!(
            matches!(
                check(
                    &auth,
                    &get,
                    Some("/api/v1/torrents"),
                    "/api/v1/torrents",
                    Some(&token),
                    Some("127.0.0.1:8090")
                ),
                Err(AuthRejection::Forbidden)
            ),
            "another route is refused"
        );
        // The event stream: `403`, even with the token.
        assert!(
            matches!(
                check(
                    &auth,
                    &get,
                    Some("/api/v1/events"),
                    "/api/v1/events",
                    Some(&token),
                    Some("127.0.0.1:8090")
                ),
                Err(AuthRejection::Forbidden)
            ),
            "the event stream is refused"
        );

        // The wrong method on the stream route: `403`.
        assert!(
            matches!(
                check(
                    &auth,
                    &post,
                    Some(route),
                    &path,
                    Some(&token),
                    Some("127.0.0.1:8090")
                ),
                Err(AuthRejection::Forbidden)
            ),
            "POST on the stream route is refused"
        );

        // The wrong host: `403`, even with the right token and path.
        assert!(
            matches!(
                check(
                    &auth,
                    &get,
                    Some(route),
                    &path,
                    Some(&token),
                    Some("evil.example:8090")
                ),
                Err(AuthRejection::Forbidden)
            ),
            "another host is refused"
        );
        // A host without the port: `403`.
        assert!(
            matches!(
                check(
                    &auth,
                    &get,
                    Some(route),
                    &path,
                    Some(&token),
                    Some("127.0.0.1")
                ),
                Err(AuthRejection::Forbidden)
            ),
            "a host without the port is refused"
        );
        // No host at all: `403`.
        assert!(
            matches!(
                check(&auth, &get, Some(route), &path, Some(&token), None),
                Err(AuthRejection::Forbidden)
            ),
            "a missing host is refused"
        );

        // No token: `401`, not `403`.
        assert!(
            matches!(
                check(
                    &auth,
                    &get,
                    Some(route),
                    &path,
                    None,
                    Some("127.0.0.1:8090")
                ),
                Err(AuthRejection::Unauthorized)
            ),
            "a missing token is refused"
        );
    }
}
