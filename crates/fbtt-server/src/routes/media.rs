//! Playing a torrent's file from the library: what it holds, one track of it as fragmented MP4, when its
//! subtitles say what, and the subtitle files that sit beside it.
//!
//! All four routes answer about one file of one torrent, named the way the rest of this API names it - the
//! 40 or 64 hex digits [`stream`] takes, and the index of the file in the torrent's own list. Three things
//! decide the shape of these handlers:
//!
//! - **The container work runs on the blocking pool.** fbtt-media reads through `std::io` and seeks the way
//!   a file does, and the reader behind it is an engine object that answers on the runtime. So each handler
//!   does its probing, demuxing and parsing inside `tokio::task::spawn_blocking`, over a
//!   [`BlockingReader`], and never on the connection's task: a worker that took a slow peer's word for a
//!   piece would take every other connection sharing it down for the wait.
//! - **The work is bounded three ways.** One read or seek gets the timeout in [`Shared::media_read_timeout`]
//!   ([`READ_TIMEOUT`] until somebody shortens it), so a piece that never arrives is an answer rather than a
//!   thread that never comes back; a stream nobody is draining any more is dropped after [`IDLE_SEND`] of
//!   nothing being taken from it; and [`MEDIA_PERMITS`] of these jobs run at once in the whole server,
//!   because every one of them is a thread, a demuxer and a read-ahead held open on the engine.
//! - **What the client sees is decided before the first byte.** A route that cannot play the file says so
//!   with a status code; once bytes are on their way the status is spent, so an error that arrives later ends
//!   the body and goes to the log. The stream route therefore lets its blocking job answer a `oneshot` with
//!   its verdict on the file - the content type and the time it really started at, or the error - and only
//!   then hands the fragments over as the body.
//!
//! The codes are the ones the plan set: `unsupported_media` for bytes this crate cannot play, `not_found` for
//! a torrent, file or pad that is not there, `no_metadata` for a torrent whose metadata has not arrived (the
//! same answer [`stream`] gives for the same torrent), `invalid_request` for an index or query that does not
//! parse, `read_timeout` for pieces that did not arrive, `too_large` for a subtitle file over the cap, and
//! three `503`s for the ways this server is temporarily full.
//!
//! Cue text is remote text like any other and goes through [`clean`] - one line at a time, so a cue keeps the
//! line breaks it had: see [`clean_cue_lines`].

use std::io::{self, BufReader, Read as _};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use fbt::{FileStatus, InfoHashes, SessionError, TorrentHandle};
use fbtt_core::engine::prefer_for_playback;
use fbtt_media::{self as media, Cue, MediaError, SubtitleFormat, TrackInfo};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot, watch};
use tokio::task::JoinError;

use super::clean;
use super::stream::{no_metadata, parse_id};
use super::{RouteError, Shared};
use crate::media_io::BlockingReader;

/// How many media routes one server runs at once, across all four of them.
///
/// Every one of them is a thread on the blocking pool, a container demuxer carrying its own read budget, and
/// an engine file reader with the read-ahead that goes with it. The blocking pool starts several hundred
/// threads if nothing tells it not to, and the engine's own bound of open readers applies to one torrent
/// ([`SessionError::TooManyReaders`]) - which is no bound at all across a library. Eight is what the plan
/// sets: room for a household's players and a probe or two, and a ceiling on what a page that reloads
/// itself can make this machine do.
pub(super) const MEDIA_PERMITS: usize = 8;

/// How long one read or one seek of a media route may take before it becomes the client's `504`.
///
/// Sixty seconds is longer than any piece of a healthy torrent takes to arrive and much shorter than the
/// indefinite wait an unbounded bridge would settle into. An operator with a sick tracker, or a test that
/// cannot wait a minute, shortens it through [`Server::set_media_read_timeout`](crate::Server).
pub(super) const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a media stream may go with nothing taken from it before its job stops making more.
///
/// A player that stops reading - a tab backgrounded mid-seek, a phone locked over a buffer - leaves the job
/// holding a thread, a demuxer, a file reader and a permit for work nobody is collecting. Ten minutes of a
/// full queue is longer than any ordinary player goes without wanting the next part, and short enough that a
/// tab nobody came back to does not hold a permit for the rest of the run. The read timeout is the shorter
/// leash of the two, and the one that fires when it is the *engine* that is stuck rather than the viewer.
pub(super) const IDLE_SEND: Duration = Duration::from_secs(600);

/// How much of a file the probe and the cue routes read at a time.
///
/// Both read a container header - and a Matroska header is a tree of elements the demuxer walks a little at
/// a time - out of a torrent file, where a read is a request to a peer rather than a copy. Without a buffer
/// that walk is thousands of reads. 64 KiB is a piece's worth of slack: enough to make a header cost a
/// handful of reads, small enough that eight of these jobs are not holding half a megabyte of bytes nobody
/// has looked at yet.
const HEADER_READ: usize = 64 * 1024;

/// How many finished fragments the stream route buffers between the job and the connection.
///
/// The job decodes ahead so a slow connection never makes it wait, and the connection plays at its own pace.
/// Four fragments - a few tens of seconds of video - is enough slack for that, and it is the bound that
/// keeps a stalled player from making the job put the rest of the film into a queue nobody drains.
const FRAGMENTS_QUEUED: usize = 4;

/// `X-Fbtt-Start-Ms`: the time the first sample of a stream actually started at.
///
/// A seek lands where the container can land, which is not where the viewer asked to go - the rules are in
/// [`media::open_track`]'s own words - so a player that asked for 2000 ms can be handed a keyframe from
/// 1801 ms. This header is the answer to "where did it really start", which a page needs before it draws a
/// subtitle or a seek bar against the same clock.
const X_FBTT_START_MS: HeaderName = HeaderName::from_static("x-fbtt-start-ms");

/// The two path segments of the routes that name one file: `/{torrent}/{file}`.
#[derive(Debug, Deserialize)]
pub(super) struct TorrentFile {
    /// The torrent's hash: the 40-hex wire hash or the 64-hex v2 hash, in either case.
    torrent: String,
    /// Its file to play, by index in the metadata's file list (pad files included).
    file: String,
}

/// The three path segments of the two routes that name one track: `/{torrent}/{file}/tracks/{track}`.
#[derive(Debug, Deserialize)]
pub(super) struct TrackPath {
    torrent: String,
    file: String,
    track: String,
}

/// `start_ms` on the stream route: where to start the track, in milliseconds of the file's own timeline.
///
/// A string rather than a number so that an unparsable value gets this API's `400 invalid_request` with the
/// words this route uses, rather than a rejection in axum's own shape.
#[derive(Debug, Deserialize)]
pub(super) struct StreamQuery {
    start_ms: Option<String>,
}

/// `from_ms` and `to_ms` on the cue route: the window, in milliseconds of the file's own timeline.
///
/// Both are strings so this route can say what is wrong with them in this API's words rather than axum's
/// own rejection, and both are `Option` only because the checking happens below in [`window_bound`]: an
/// absent bound is refused here, not defaulted.
#[derive(Debug, Deserialize)]
pub(super) struct Window {
    from_ms: Option<String>,
    to_ms: Option<String>,
}

/// The path of [`subtitle_file`]: `{sub}` is the index of a subtitle file in the same torrent.
#[derive(Debug, Deserialize)]
pub(super) struct SubtitlePath {
    torrent: String,
    file: String,
    sub: String,
}

/// What `GET /api/v1/media/{torrent}/{file}` answers: what the file is, and what could be played with it.
#[derive(Debug, Serialize)]
pub(super) struct ProbeAnswer {
    /// `matroska`, `webm` or `mp4`, spelled as the container spells its own name.
    container: String,
    /// How long it is, when the container states a length; `null` when it does not.
    duration_ms: Option<u64>,
    /// How far the times this file's tracks carry are shifted to reach the player's clock, which a page
    /// needs to line a cue or a seek bar up with what it is playing. See [`media::TIME_OFFSET_MS`].
    time_offset_ms: u64,
    /// The tracks, in the order the container lists them, subtitle tracks included.
    tracks: Vec<TrackRow>,
    /// The subtitle files in the same torrent that could be handed to the player beside this one.
    external_subtitles: Vec<SubtitleRow>,
}

/// One track, as a player needs it in order to offer the track.
#[derive(Debug, Serialize)]
pub(super) struct TrackRow {
    /// The container's own track number: the number the two track routes take back as `{track}`.
    id: u32,
    /// `video`, `audio` or `subtitle`.
    kind: &'static str,
    /// The codec, named as `ffprobe` and the web's codec vocabulary name it.
    codec: String,
    /// The RFC 6381 strings that name this track's codec to a manifest or a `canPlayType` call. Empty for a
    /// codec this crate cannot put into a fragmented MP4 sample entry, which is the track's "not playable
    /// from here" signal.
    mime_codecs: Vec<String>,
    /// The track's language, when the file states one; `null` for a file that only says "undetermined",
    /// which is the containers' default and not a fact about the track.
    language: Option<String>,
    /// The track's title, when the container carries one.
    name: Option<String>,
    /// Whether a player should pick this track when the viewer expressed no preference.
    default: bool,
    /// Whether this subtitle track plays even for a viewer who did not ask for subtitles.
    forced: bool,
    /// Whether this subtitle track's payload is text this crate can rewrite as WebVTT.
    text: bool,
}

impl TrackRow {
    /// A track row from what the container said about the track.
    fn of(track: &TrackInfo) -> Self {
        TrackRow {
            id: track.id,
            kind: kind_name(track.kind),
            codec: track.codec.name().to_string(),
            mime_codecs: track.mime_codecs.clone(),
            language: track.language.as_deref().map(clean),
            name: track.name.as_deref().map(clean),
            default: track.default,
            forced: track.forced,
            text: track.codec.text_subtitle(),
        }
    }
}

/// What kind of media a track is, in the one spelling this API uses for it.
fn kind_name(kind: media::TrackKind) -> &'static str {
    match kind {
        media::TrackKind::Video => "video",
        media::TrackKind::Audio => "audio",
        media::TrackKind::Subtitle => "subtitle",
    }
}

/// One subtitle file that could be played beside the file this request named.
#[derive(Debug, Serialize)]
pub(super) struct SubtitleRow {
    /// Its index in the same torrent's file list: what `{sub}` takes.
    file: usize,
    /// Its name, cleaned like every other name this server repeats.
    name: String,
    /// `srt`, `ass` or `vtt`, from the extension the format was recognised from.
    format: &'static str,
}

/// What the two cue routes answer.
#[derive(Debug, Serialize)]
pub(super) struct CueAnswer {
    cues: Vec<CueRow>,
}

/// One line of a subtitle, with its text already cleaned for display.
#[derive(Debug, Serialize)]
pub(super) struct CueRow {
    start_ms: u64,
    end_ms: u64,
    text: String,
}

impl CueRow {
    /// A cue as this API answers it: the times as the file gave them, the text one line at a time through
    /// [`clean_cue_lines`].
    fn of(cue: Cue) -> Self {
        CueRow {
            start_ms: cue.start_ms,
            end_ms: cue.end_ms,
            text: clean_cue_lines(&cue.text),
        }
    }
}

/// A cue's text, cleaned one line at a time.
///
/// [`clean`] over the whole string would turn every newline in it into a space, and a two-line cue would
/// arrive as one line with its second line's words glued to its first. So each line goes through on its own
/// and the breaks are put back: what arrives has no control characters in it - the escape a terminal's
/// colour codes leave inside a `.srt` someone copied from one, the bidi override that makes a name read the
/// other way - and exactly the lines the file had, with a carriage return that some `.srt` files carry
/// dropped rather than kept as an invisible character. A line that is nothing once cleaned is dropped
/// rather than spending a line of the viewer's screen on it.
fn clean_cue_lines(text: &str) -> String {
    text.lines()
        .map(clean)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `GET /api/v1/media/{torrent}/{file}`: what this file is, what tracks it has, and what could be played
/// beside it.
///
/// The answer is the file's own description, and this is the route that decides whether the other three have
/// anything to do: a file that is not Matroska, WebM or MP4, one too short to hold a container header, and
/// one whose header does not hold together are `415 unsupported_media`, with what was found in the message,
/// so a player looks for another file instead of guessing. A torrent this server does not have, an index
/// past the end of its list, and a pad file are `404`; a torrent whose metadata has not arrived is
/// `409 no_metadata`, the answer [`stream`] gives it.
pub(super) async fn probe(
    State(shared): State<Arc<Shared>>,
    Path(params): Path<TorrentFile>,
) -> Result<Json<ProbeAnswer>, RouteError> {
    let hashes = parse_id(&params.torrent)?;
    let file = file_index(&params.file)?;
    let permit = media_permit(&shared)?;
    let (torrent, files) = listed(&shared, &hashes, file).await?;
    // The sidecar list comes off the file list this route already holds, so it is worked out here rather than
    // carried down to the blocking pool: it costs no reads, and the job has enough to carry.
    let beside = external_subtitles(&files, file);
    let (reader, _stop) = open_reader(&shared, &torrent, file).await?;
    let answer = match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let header = BufReader::with_capacity(HEADER_READ, reader);
        let info = media::probe(header).map_err(media_problem)?;
        let answer = ProbeAnswer {
            container: info.container.name().to_string(),
            duration_ms: info.duration_ms,
            time_offset_ms: media::TIME_OFFSET_MS,
            tracks: info.tracks.iter().map(TrackRow::of).collect(),
            external_subtitles: beside,
        };
        Ok::<ProbeAnswer, RouteError>(answer)
    })
    .await
    {
        Ok(Ok(answer)) => answer,
        Ok(Err(problem)) => return Err(problem),
        Err(problem) => return Err(job_failed(problem)),
    };
    Ok(Json(answer))
}

/// `GET /api/v1/media/{torrent}/{file}/tracks/{track}/stream?start_ms=N`: one track of one file as
/// fragmented MP4, starting at or after `start_ms`.
///
/// The content type comes from the stream - `video/mp4` or `audio/mp4` with the track's RFC 6381 codec
/// string - because the bytes are fragmented MP4 whatever the file was. [`X_FBTT_START_MS`] says where the
/// first sample really started, which for a video track is the keyframe the container could land on rather
/// than the time that was asked for. `Cache-Control: no-store`, because this body is the answer to a seek: a
/// cache that replayed it would be playing the wrong part of the film.
///
/// The bytes are the init segment and then fragments, out of a queue [`FRAGMENTS_QUEUED`] deep. What the
/// job does is [`play`]; what decides the status code is the `oneshot` it answers before it has produced
/// anything the client can see.
pub(super) async fn track_stream(
    State(shared): State<Arc<Shared>>,
    Path(params): Path<TrackPath>,
    Query(query): Query<StreamQuery>,
) -> Result<Response, RouteError> {
    let hashes = parse_id(&params.torrent)?;
    let file = file_index(&params.file)?;
    let track = track_number(&params.track)?;
    let start_ms = match query.start_ms.as_deref() {
        // No `start_ms` at all is the start of the file, which is what `open_track` wants spelled as zero.
        None => 0,
        Some(wanted) => wanted.parse::<u64>().map_err(|_| {
            RouteError::invalid_request("start_ms is a time in milliseconds, or absent")
        })?,
    };
    let permit = media_permit(&shared)?;
    let (torrent, _) = listed(&shared, &hashes, file).await?;
    // The viewer pressed Play on this file, so it goes to High priority if it is below it and its pieces
    // download in order, and the torrent's other files keep their priorities. Any track of it counts as
    // playing it - a subtitle track and an unplayable container are still this file being asked for - and
    // the routes that only read a header are not plays and do not do it (see `probe`).
    prefer_for_playback(&torrent, file).await;
    let (reader, stop) = open_reader(&shared, &torrent, file).await?;
    let (heads, head) = oneshot::channel();
    let (fragments, queue) = mpsc::channel::<Vec<u8>>(FRAGMENTS_QUEUED);
    let id = params.torrent.clone();
    // Not awaited: this job *is* the rest of the request, and it ends by itself when the body below is
    // dropped. The permit travels inside it, because the permit is about this work and not about the handler
    // that started it - the same reason [`login::verify_password`] carries its permit down, and see [`play`]
    // for the three things that end this one.
    tokio::task::spawn_blocking(move || {
        let out = Playing {
            id,
            file,
            heads,
            fragments,
            _permit: permit,
        };
        play(reader, track, start_ms, out)
    });
    // The verdict on the file, before a byte of it has been promised to anybody.
    let head = match head.await {
        Ok(Ok(head)) => head,
        Ok(Err(problem)) => return Err(problem),
        // The job stopped without answering at all: it panicked, or this handler was already going away.
        Err(_) => return Err(RouteError::internal("the media stream did not answer")),
    };
    let mut response = Response::new(Body::from_stream(Fragments {
        next: queue,
        _stop: stop,
    }));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&head.mime)
            .map_err(|_| RouteError::internal("the track's content type is not usable"))?,
    );
    headers.insert(X_FBTT_START_MS, head.start_ms.into());
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// What the stream route has to know before it answers: what the bytes are, and where they started.
struct Head {
    mime: String,
    start_ms: u64,
}

/// Everything [`play`] owns on the request's side of the boundary between the handler and the job: where
/// the parts and the verdict go, who to name in a log line, and the pool place this work is holding.
///
/// Its own type because the four destinations and the identity of a stream belong to the job and not to any
/// of the handler's arguments: the handler builds all of it, hands it over, and keeps nothing.
struct Playing {
    /// The torrent as the client spelled it, for the log line about a stream that ended. A hex id, which is
    /// already the display form [`tracing`] sees in the torrent routes.
    id: String,
    /// The file index, for the same line: the name is the viewer's business and never logged.
    file: usize,
    /// Where the mime type and the landed start time go, or the verdict on the file if there is no body.
    heads: oneshot::Sender<Result<Head, RouteError>>,
    /// Where the fragments go, one part at a time, to the body the handler is already holding.
    fragments: mpsc::Sender<Vec<u8>>,
    /// This route's place in the media pool. Never read: dropped with the job.
    _permit: OwnedSemaphorePermit,
}

/// The blocking half of [`track_stream`]: open the track, answer the head, then feed the queue.
///
/// This job is the request for as long as it runs, and three things end it: the track ran out, nobody is
/// collecting fragments any more (the body was dropped, which is the viewer hanging up), or the file or the
/// engine refused something. The first two are ordinary and are not worth a log line beyond a `debug` of who
/// gave up; the third is logged with the torrent, the file and the verdict's *kind* through
/// [`media_verdict`] - variant name and io kind, never the message, which for an engine or OS failure
/// names a full path on this machine - and ends the stream where it stands, because the status code was
/// spent hours of buffering ago.
///
/// The place in the pool travels inside `out`, so that it goes back when *this* work is over and not when
/// the handler that started it happened to return.
fn play(reader: BlockingReader, track: u32, start_ms: u64, out: Playing) {
    let Playing {
        id,
        file,
        heads,
        fragments,
        _permit,
    } = out;
    let runtime = tokio::runtime::Handle::current();
    let mut stream = match media::open_track(reader, track, start_ms) {
        Ok(stream) => stream,
        Err(problem) => {
            // Nothing has been written, so the client can still be given a code for this file.
            let _ = heads.send(Err(media_problem(problem)));
            return;
        }
    };
    let mime = stream.mime();
    // The first part before the head is answered, because producing it is where a header's worth of waiting
    // on the engine happens: an error here is still an error a status code can describe.
    let first = match stream.next_fragment() {
        Ok(Some(bytes)) => bytes,
        // A track with nothing in it: the head is still true, and the body is empty.
        Ok(None) => {
            let _ = heads.send(Ok(Head {
                mime,
                start_ms: stream.start_ms(),
            }));
            return;
        }
        Err(problem) => {
            let _ = heads.send(Err(media_problem(problem)));
            return;
        }
    };
    let head = Head {
        mime,
        start_ms: stream.start_ms(),
    };
    if heads.send(Ok(head)).is_err() {
        // The answer is not going to be sent at all: the request is gone.
        return;
    }
    let mut part = Some(first);
    loop {
        let Some(bytes) = part.take() else { return };
        // A queue nobody is emptying is a viewer who has stopped wanting this: stop decoding rather than
        // spend the rest of the track on it. Checked before the work rather than after it, so a hang-up
        // costs one fragment's worth of demuxing and not the whole file.
        if fragments.is_closed() {
            tracing::debug!(torrent = %id, file, "the viewer stopped playing this stream");
            return;
        }
        match runtime.block_on(tokio::time::timeout(
            IDLE_SEND,
            fragments.clone().send(bytes),
        )) {
            Ok(Ok(())) => {}
            // The body went away in the middle of a send: the same hang-up as the check above, one part
            // later, with the queue's receiver already gone.
            Ok(Err(_)) => return,
            Err(_) => {
                tracing::debug!(
                    torrent = %id,
                    file,
                    seconds = IDLE_SEND.as_secs(),
                    "nothing was taken from this stream for that long",
                );
                return;
            }
        }
        part = match stream.next_fragment() {
            Ok(next) => next,
            // Past the first part the status code is spent: the body ends where it stands, which is what a
            // player reads as the end of what is fetchable, and the reason for it goes here rather than to
            // the viewer - as its kind only, because an `Io` verdict from the engine carries a path inside
            // its message and a viewer's file name is not for the log.
            Err(problem) => {
                let (verdict, kind) = media_verdict(&problem);
                tracing::debug!(
                    torrent = %id,
                    file,
                    verdict,
                    ?kind,
                    "the media stream ended early",
                );
                return;
            }
        };
    }
}

/// The body of a media stream: the queue the job feeds, and the handle that tells the job to stop.
///
/// Ending this is what hangs up: dropping it drops the watch sender [`BlockingReader`] waits on, which is
/// what wakes a read that is waiting for a piece that is never going to arrive. The channel ending is what
/// ends the body: the job is finished, successfully or not, and said so where the viewer cannot see it.
struct Fragments {
    next: mpsc::Receiver<Vec<u8>>,
    /// Held and never sent on: see [`BlockingReader`].
    _stop: watch::Sender<()>,
}

impl futures_util::Stream for Fragments {
    type Item = Result<Vec<u8>, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // A part is either more of the film or the end of it. The job's own failures never reach this body:
        // the only thing that can tell a viewer about them is where the bytes stop.
        match self.get_mut().next.poll_recv(cx) {
            Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(bytes))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// `GET /api/v1/media/{torrent}/{file}/tracks/{track}/cues?from_ms=A&to_ms=B`: when this track's subtitles
/// say what, within the window the viewer is about to play.
///
/// The window is checked before anything is read: both bounds have to be given and parse as numbers,
/// `to_ms` has to come after `from_ms`, and the pair has to fit inside [`media::CUE_WINDOW_MAX_MS`], so a
/// query naming a year of a film is refused by this route rather than by the demuxer's own bound. Neither
/// bound is defaulted - see [`window_bound`] for why guessing "the rest of the file" is exactly the wrong
/// answer here. A track that is not a text subtitle track - a video track, a bitmap subtitle track, a
/// number the file does not carry - is `415 unsupported_media`, because there are no cues of any format to
/// give for it.
pub(super) async fn track_cues(
    State(shared): State<Arc<Shared>>,
    Path(params): Path<TrackPath>,
    Query(query): Query<Window>,
) -> Result<Json<CueAnswer>, RouteError> {
    let hashes = parse_id(&params.torrent)?;
    let file = file_index(&params.file)?;
    let track = track_number(&params.track)?;
    let from_ms = window_bound(query.from_ms.as_deref(), "from_ms")?;
    let to_ms = window_bound(query.to_ms.as_deref(), "to_ms")?;
    if to_ms <= from_ms {
        return Err(RouteError::invalid_request("to_ms has to be after from_ms"));
    }
    if to_ms - from_ms > media::CUE_WINDOW_MAX_MS {
        return Err(RouteError::invalid_request(format!(
            "a cue window is at most {} ms",
            media::CUE_WINDOW_MAX_MS
        )));
    }
    let permit = media_permit(&shared)?;
    let (torrent, _) = listed(&shared, &hashes, file).await?;
    let (reader, _stop) = open_reader(&shared, &torrent, file).await?;
    let cues = match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // A cue scan is a walk of frames, and the demuxer asks where it is and where the end of the
        // file is on the way to every one of them. A `std::io::BufReader` answers each of those by
        // giving its buffer up and filling it again, which makes the scan cost a 64 KiB read per
        // frame; this buffer answers them itself and keeps the bytes it read.
        let file = reader.buffered_across_seeks(HEADER_READ);
        let found = media::embedded_cues(file, track, from_ms, to_ms).map_err(media_problem)?;
        Ok::<Vec<Cue>, RouteError>(found)
    })
    .await
    {
        Ok(Ok(cues)) => cues,
        Ok(Err(problem)) => return Err(problem),
        Err(problem) => return Err(job_failed(problem)),
    };
    Ok(Json(CueAnswer {
        cues: cues.into_iter().map(CueRow::of).collect(),
    }))
}

/// `GET /api/v1/media/{torrent}/{file}/subtitles/{sub}`: the cues of a subtitle file of the same torrent, in
/// the same shape the embedded route gives.
///
/// `{file}` is the video the subtitle belongs to and `{sub}` the file to read; both have to be files of this
/// torrent and not pads. This route then reads `{sub}` whole - it is a text file, and a cue needs its timing
/// line and its text lines together - under [`media::SUBTITLE_FILE_CAP`], which the torrent's own metadata
/// answers before a byte is fetched: a sidecar of a hundred megabytes is refused with no reads at all, and
/// one whose metadata lied about its length gets the same answer the moment the byte past the cap turns up.
pub(super) async fn subtitle_file(
    State(shared): State<Arc<Shared>>,
    Path(params): Path<SubtitlePath>,
) -> Result<Json<CueAnswer>, RouteError> {
    let hashes = parse_id(&params.torrent)?;
    let file = file_index(&params.file)?;
    let sub = file_index(&params.sub)?;
    let permit = media_permit(&shared)?;
    let (torrent, files) = listed(&shared, &hashes, file).await?;
    let Some(row) = files.iter().find(|row| row.index == sub) else {
        return Err(RouteError::not_found("no such subtitle file"));
    };
    if row.pad {
        return Err(RouteError::not_found("no such subtitle file"));
    }
    // The format comes from the name as the torrent carries it, and the answer quotes the cleaned one:
    // `subtitle_format` reads the extension at a name's end, and cleaning keeps its front, so a subtitle
    // file with a name over the display limit has its extension in the part that was cut (the same split
    // [`subtitle_row`] makes for the list).
    let name = display_name(row);
    let Some(format) = media::subtitle_format(&file_name_of(row)) else {
        return Err(unsupported_media(format!(
            "{name} is not a subtitle file: .srt, .ass, .ssa and .vtt are"
        )));
    };
    // The cap, from the torrent's metadata: a file this route could not possibly send is refused before it
    // spends a permit's worth of engine reads finding that out.
    if row.len > media::SUBTITLE_FILE_CAP {
        return Err(too_large(row.len));
    }
    let (reader, _stop) = open_reader(&shared, &torrent, sub).await?;
    let cues = match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // One byte past the cap is enough to know the file is over it, and it is what makes the cap a bound
        // on the bytes this job holds rather than on a promise the metadata made.
        let mut bytes = Vec::new();
        // This read is the route's own work rather than the crate's, so its failure is put into the same
        // verdict a reader failure inside fbtt-media gets by hand: a timeout is still a timeout, and a file
        // that has gone is still a file that has gone.
        reader
            .take(media::SUBTITLE_FILE_CAP + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| media_problem(MediaError::Io(err)))?;
        if bytes.len() as u64 > media::SUBTITLE_FILE_CAP {
            return Err(too_large(bytes.len() as u64));
        }
        let found = media::file_cues(&bytes, format).map_err(media_problem)?;
        Ok::<Vec<Cue>, RouteError>(found)
    })
    .await
    {
        Ok(Ok(cues)) => cues,
        Ok(Err(problem)) => return Err(problem),
        Err(problem) => return Err(job_failed(problem)),
    };
    Ok(Json(CueAnswer {
        cues: cues.into_iter().map(CueRow::of).collect(),
    }))
}

/// The file list, with the file this request named found in it: the lookup all four routes do, spelled once
/// so that they answer "no such torrent", "no metadata" and "no such file" alike.
///
/// A pad file is `404` rather than a playable file with nothing in it: a pad has no bytes to demux, and
/// fbt-api's own stream route refuses one too.
async fn listed(
    shared: &Arc<Shared>,
    hashes: &InfoHashes,
    file: usize,
) -> Result<(TorrentHandle, Vec<FileStatus>), RouteError> {
    let Some(torrent) = shared.session().torrent(hashes) else {
        return Err(RouteError::not_found("no such torrent"));
    };
    let files = match torrent.files().await {
        Ok(files) => files,
        Err(SessionError::NoMetadata) => return Err(no_metadata()),
        Err(SessionError::Shutdown) => return Err(shutting_down()),
        // Anything else is this server's own trouble. The engine's words for it may name a path on this
        // machine, so the log gets the verdict's kind from [`engine_verdict`] and the client gets what it
        // can act on; neither gets the file's name.
        Err(problem) => {
            let (verdict, kind) = engine_verdict(&problem);
            tracing::warn!(
                verdict,
                ?kind,
                "the file list of a torrent could not be read",
            );
            return Err(RouteError::internal("the file list could not be read"));
        }
    };
    let Some(row) = files.iter().find(|row| row.index == file) else {
        return Err(RouteError::not_found("no such file"));
    };
    if row.pad {
        return Err(RouteError::not_found("no such file"));
    }
    Ok((torrent, files))
}

/// A reader for one file of one torrent, ready to be read from a blocking thread.
///
/// The engine's reader is opened here, while the handler still runs on the runtime that owns the session, and
/// given the timeout and the hang-up watch this run is serving under. The `watch::Sender` that comes back is
/// the other half of that: whoever holds it is what keeps a pending read awake, and dropping it is what
/// wakes one. Hold it for as long as the reading matters - in the handler for the three routes that answer
/// JSON, in the body of the stream route.
async fn open_reader(
    shared: &Arc<Shared>,
    torrent: &TorrentHandle,
    file: usize,
) -> Result<(BlockingReader, watch::Sender<()>), RouteError> {
    let reader = match torrent.open_file(file).await {
        Ok(reader) => reader,
        Err(problem) => return Err(engine_problem(&problem)),
    };
    let (stop, waiting) = watch::channel(());
    let reader = BlockingReader::new(
        reader,
        tokio::runtime::Handle::current(),
        shared.media_read_timeout(),
        waiting,
    );
    Ok((reader, stop))
}

/// This server's place in the media pool, or the `503` that says none is free.
///
/// `try_acquire_owned` rather than an await: a media route is not somewhere to sit and wait for somebody
/// else's demuxer to finish, and a client told to come back in a second is better off than one whose
/// request is held open while it waits. The permit is owned because the stream route hands its permit to the
/// job that outlives this handler.
fn media_permit(shared: &Arc<Shared>) -> Result<OwnedSemaphorePermit, RouteError> {
    shared
        .media_permits()
        .try_acquire_owned()
        .map_err(|_| too_many_media_streams())
}

/// The file index a path segment names.
///
/// Parsed here rather than by the path extractor, so that a segment that is not a number gets this API's
/// `400 invalid_request` in this API's words.
fn file_index(given: &str) -> Result<usize, RouteError> {
    given.parse::<usize>().map_err(|_| {
        RouteError::invalid_request("a file index is a number, in the torrent's own file list")
    })
}

/// `{track}`: the container's own track number.
fn track_number(given: &str) -> Result<u32, RouteError> {
    given.parse::<u32>().map_err(|_| {
        RouteError::invalid_request("a track number is a number this file's probe reported")
    })
}

/// One bound of a cue window: the client has to give it, and it has to be a number.
///
/// Nothing about this window is guessed. `start_ms` on the stream route may be absent because "no start"
/// has exactly one meaning, the start of the file, while a missing `to_ms` could mean the end of the file,
/// the end of the widest window, or a client that simply forgot the parameter - and defaulting it to a
/// ten-minute window makes "give me everything from here on" quietly answer with only the first ten
/// minutes of a film. A page that never asked for a limit and never got one is a page whose subtitles
/// stop, with no answer that says why. So an absent bound is this API's `400 invalid_request`, the same
/// verdict an unparsable one gets; `?from_ms=` with nothing after the `=` is the unparsable case, not a
/// zero.
fn window_bound(given: Option<&str>, what: &str) -> Result<u64, RouteError> {
    let Some(given) = given else {
        return Err(RouteError::invalid_request(format!(
            "{what} is a time in milliseconds, and this route will not guess it"
        )));
    };
    ms(given, what)
}

/// One millisecond bound of a cue window, as the client gave it.
fn ms(given: &str, what: &str) -> Result<u64, RouteError> {
    given
        .parse::<u64>()
        .map_err(|_| RouteError::invalid_request(format!("{what} is a time in milliseconds")))
}

/// The name a file is judged by: the last component of the path the torrent carries, exactly as it
/// came.
///
/// The name is also structure this route reads: its extension says which format to parse a subtitle
/// as, and its stem says whether it belongs to the video beside it. Both sit at the end of a name, and
/// cleaning counts from the front, so the two answers are taken from the name as it arrived and not
/// from the one [`display_name`] shows.
fn file_name_of(row: &FileStatus) -> String {
    match row.path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => row.path.to_string_lossy().into_owned(),
    }
}

/// The name to show for a file: the last component of the path the torrent carries, which is what a player
/// has to name a track after, cleaned like every other name this server repeats.
fn display_name(row: &FileStatus) -> String {
    clean(&file_name_of(row))
}

/// One subtitle file, as the list wants it: whether its name says it belongs to the video named
/// `video`, and the row that lists it.
///
/// `name` and `video` are names as the torrent carries them, because both questions this answers are
/// questions about the end of a name: [`media::subtitle_format`] reads the extension and [`sidecar_of`]
/// compares stems, while [`clean`] keeps a name's first characters, so a name long enough to be cut is
/// cut where its extension was. A 300 character `film.en.srt` cleaned first is no format at all and no
/// sidecar of anything, and a file that cannot be recognised cannot be listed or played. The row carries
/// the cleaned name, because that is the half of it a viewer reads.
fn subtitle_row(index: usize, name: &str, video: Option<&str>) -> Option<(bool, SubtitleRow)> {
    let format = media::subtitle_format(name)?;
    let beside = video.is_some_and(|video| sidecar_of(video, name));
    Some((
        beside,
        SubtitleRow {
            file: index,
            name: clean(name),
            format: format_name(format),
        },
    ))
}

/// The subtitle files of one torrent, spelled the way a viewer wants a list of them: the ones named after
/// the video first, then the rest in the order the torrent lists them.
///
/// One video and four subtitle files is the ordinary shape of a download, and someone picking from that list
/// wants `movie.en.srt` before `captions.zh.ass`: sharing the video's name is the best sign available that a
/// sidecar belongs to this file, and index order after that is at least stable. "Sharing the name" means the
/// stem and nothing after it but a dot and a suffix - see [`sidecar_of`] - so `about.srt` is not `a.mkv`'s
/// own sidecar. The file the request named is left out - it is the one being played, and it is not a
/// subtitle for itself.
fn external_subtitles(files: &[FileStatus], video: usize) -> Vec<SubtitleRow> {
    let base = files
        .iter()
        .find(|row| row.index == video)
        .map(file_name_of);
    let mut rows: Vec<(bool, usize, SubtitleRow)> = files
        .iter()
        .filter(|file| !file.pad && file.index != video)
        .filter_map(|file| {
            let index = file.index;
            let (beside, row) = subtitle_row(index, &file_name_of(file), base.as_deref())?;
            // A row that did not match the video's name sorts after one that did.
            Some((!beside, index, row))
        })
        .collect();
    rows.sort_by_key(|(later, index, _)| (*later, *index));
    rows.into_iter().map(|(_, _, row)| row).collect()
}

/// Whether `name` is the sidecar of the video named `video`: that video's stem, exactly, or the stem
/// with a dot and something after it.
///
/// Neither name is trusted to have arrived in the other one's case: `ÄRGER.srt` beside `ärger.mkv` is
/// one film's captions, and the two only meet once both are put in lower case. That folding is the
/// whole Unicode one rather than the ASCII one, because the ASCII form leaves an `Ä` standing as it
/// was and then holds it against the `ä` the other name carries, which misses the sidecar of every
/// film whose name is not plain ASCII. `movie.srt`, `movie.en.srt` and `movie.ita.srt` belong to the
/// video named `movie.mkv`; `about.srt` does not belong to a video named `a.mkv` however much its
/// first letters agree, and neither does `movies.vtt` to a video named `movie`. A bare prefix test
/// says all of those are sidecars, and then a folder's stray `about.srt` ranks as `a.mkv`'s own. The
/// stem, and after it either nothing or a dot, is the whole rule.
fn sidecar_of(video: &str, name: &str) -> bool {
    let video = video.to_lowercase();
    let name = name.to_lowercase();
    let stem = match video.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => video.as_str(),
    };
    name == stem
        || name
            .strip_prefix(stem)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// A subtitle format, in the one spelling this API uses for it.
fn format_name(format: SubtitleFormat) -> &'static str {
    match format {
        SubtitleFormat::Srt => "srt",
        SubtitleFormat::Ass => "ass",
        SubtitleFormat::Vtt => "vtt",
    }
}

/// A media verdict's kind, for the log line that must not repeat its message.
///
/// `Unsupported` and `Malformed` quote what the demuxer found, and `Io` carries an `io::Error` whose words
/// are the engine's or the OS's about a file on this machine - a full path with a viewer's file name in
/// it. The message belongs in the client's answer (where [`media_problem`] puts the fbtt-media ones) or
/// nowhere; a log line gets the variant's name and, for an `io`, its [`io::ErrorKind`]: enough to tell an
/// operator which kind of "this stream stopped" happened, with no viewer's path in it.
fn media_verdict(problem: &MediaError) -> (&'static str, Option<io::ErrorKind>) {
    match problem {
        MediaError::Unsupported(_) => ("unsupported", None),
        MediaError::Malformed(_) => ("malformed", None),
        MediaError::Io(err) => ("io", Some(err.kind())),
    }
}

/// A container, codec or reader verdict, in the shape this API answers with.
///
/// Both halves of fbtt-media's "this cannot be played here" - a codec or container it does not read, and a
/// header that does not hold together - are the same thing to a player: nothing will make these bytes play,
/// so look for another file, which is what `415` says, with the crate's own words about what it found in the
/// message. The reader failures are the other half of the story, and they are sorted by what the client can
/// do about them: pieces that have not arrived are worth asking for again (`504`), a file that has left this
/// machine is not (`404`), and a request nobody is still reading is nobody's error (`503`).
fn media_problem(problem: MediaError) -> RouteError {
    match problem {
        MediaError::Unsupported(message) | MediaError::Malformed(message) => {
            unsupported_media(clean(&message))
        }
        MediaError::Io(err) => match err.kind() {
            io::ErrorKind::TimedOut => read_timeout(),
            io::ErrorKind::NotFound => RouteError::not_found("the file is gone"),
            // The viewer stopped reading, so there is no answer left to give. What a request nobody is
            // waiting for any more answers hardly matters; `503` is the truth of it.
            io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted => shutting_down(),
            // A stream that ends in the middle of a header is the crate's own `Malformed` verdict arriving
            // as a reader failure instead: the file is not playable from here either way.
            io::ErrorKind::UnexpectedEof => {
                unsupported_media("the file ends before its container header does")
            }
            kind => {
                // The engine's or the OS's own words about a file on this machine, which the viewer's log
                // must not carry: this line keeps the verdict's kind and nothing else.
                tracing::warn!(
                    verdict = "io",
                    ?kind,
                    "reading a torrent file for the media routes failed",
                );
                RouteError::internal("the torrent file could not be read")
            }
        },
    }
}

/// An engine verdict's kind, for the same reason [`media_verdict`] exists.
///
/// The names are this API's snake_case spelling of the variant names, so an operator can read the log and
/// the engine's source with the same word. The two variants that wrap an `io::Error` come with its kind.
/// No message is kept: `Storage` and `Bind` carry an OS error naming a path or an address, and
/// `InvalidPath`, `InvalidUrl`, `InvalidSettings`, `Tracker` and `Proxy` carry a viewer's path, URL or
/// setting as their entire message. The deprecated `V2OnlyUnsupported` is deliberately not named in the
/// match - naming a deprecated variant would make its deprecation this crate's warning to carry - so it
/// lands in the catch-all as `session`.
fn engine_verdict(problem: &SessionError) -> (&'static str, Option<io::ErrorKind>) {
    match problem {
        SessionError::Storage(err) => ("storage", Some(err.kind())),
        SessionError::Bind(err) => ("bind", Some(err.kind())),
        SessionError::DuplicateTorrent => ("duplicate_torrent", None),
        SessionError::TorrentNotFound => ("torrent_not_found", None),
        SessionError::InvalidPriorities { .. } => ("invalid_priorities", None),
        SessionError::Shutdown => ("shutdown", None),
        SessionError::Tracker(_) => ("tracker", None),
        SessionError::InvalidSettings(_) => ("invalid_settings", None),
        SessionError::NoMetadata => ("no_metadata", None),
        SessionError::InvalidFileIndex { .. } => ("invalid_file_index", None),
        SessionError::TooManyReaders => ("too_many_readers", None),
        SessionError::Proxy(_) => ("proxy", None),
        SessionError::InvalidPath(_) => ("invalid_path", None),
        SessionError::Unsupported => ("unsupported", None),
        SessionError::Busy => ("busy", None),
        SessionError::InvalidUrl(_) => ("invalid_url", None),
        SessionError::TooManyWebSeeds => ("too_many_web_seeds", None),
        SessionError::Resume(_) => ("resume", None),
        SessionError::DhtDisabled => ("dht_disabled", None),
        SessionError::Dht(_) => ("dht", None),
        _ => ("session", None),
    }
}

/// A verdict from the engine itself, before any of the file's bytes were read.
///
/// The two the client can act on are the two the plan names: a torrent whose metadata has not arrived gets
/// the same `409` [`stream`] gives it, and a torrent with sixteen readers on it is at the engine's own
/// limit, so it gets that limit's answer with the second fbt-api's `busy` asks a client to wait. The rest
/// are this server failing at its job, and they reach the log through [`engine_verdict`] as a kind only:
/// the engine's words for them may name a path on this machine.
fn engine_problem(problem: &SessionError) -> RouteError {
    match problem {
        SessionError::NoMetadata => no_metadata(),
        SessionError::InvalidFileIndex { .. } => RouteError::not_found("no such file"),
        SessionError::TooManyReaders => too_many_readers(),
        SessionError::Shutdown => shutting_down(),
        // The engine's own disk trouble: a file moved out from under this run is not a file anybody can
        // name any more.
        SessionError::Storage(err) if err.kind() == io::ErrorKind::NotFound => {
            RouteError::not_found("the file is not on this machine any more")
        }
        other => {
            let (verdict, kind) = engine_verdict(other);
            tracing::warn!(
                verdict,
                ?kind,
                "the engine could not open this file for the media routes",
            );
            RouteError::internal("the torrent file could not be opened")
        }
    }
}

/// A blocking media job that stopped without answering: it panicked, and a panic in a job is this server's
/// failure rather than a verdict about somebody's file.
fn job_failed(problem: JoinError) -> RouteError {
    tracing::error!(
        problem = %clean(&problem.to_string()),
        "a media job did not finish",
    );
    RouteError::internal("the media job did not finish")
}

/// `415 unsupported_media`: these bytes will not play from this route, in fbtt-media's own words about what
/// it found.
fn unsupported_media(message: impl Into<String>) -> RouteError {
    RouteError::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media",
        message,
    )
}

/// `504 read_timeout`: the file is probably fine, and the pieces this request needed did not come.
fn read_timeout() -> RouteError {
    RouteError::new(
        StatusCode::GATEWAY_TIMEOUT,
        "read_timeout",
        "the pieces this file needs have not arrived",
    )
}

/// `413 too_large`: a subtitle file bigger than this route reads, in the size it turns out to be.
fn too_large(bytes: u64) -> RouteError {
    RouteError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "too_large",
        format!(
            "a subtitle file is at most {} bytes, this one is {bytes}",
            media::SUBTITLE_FILE_CAP
        ),
    )
}

/// `503 too_many_readers` with `Retry-After: 1`: the engine's own limit of open readers on one torrent, the
/// answer for a client with sixteen streams on one file.
fn too_many_readers() -> RouteError {
    let mut error = RouteError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "too_many_readers",
        "this torrent already has as many open readers as it allows",
    );
    error.retry_after_secs = Some(1);
    error
}

/// `503 too_many_media_streams` with `Retry-After: 1`: every place this server has for demuxing a file is
/// busy. This is this crate's pool, not one torrent's readers, so it gets its own code and a client that
/// backs off does not need to know the difference.
fn too_many_media_streams() -> RouteError {
    let mut error = RouteError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "too_many_media_streams",
        "this server is playing as many files as it allows",
    );
    error.retry_after_secs = Some(1);
    error
}

/// `503 shutting_down`: the engine has stopped taking work, which is what [`stream`] says in the same case.
fn shutting_down() -> RouteError {
    RouteError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "shutting_down",
        "the server is stopping",
    )
}

#[cfg(test)]
mod tests {
    use super::{clean, sidecar_of, subtitle_row};

    /// A subtitle name too long for the display limit still says what it is and whose it is.
    ///
    /// `clean` keeps a name's first 256 characters, so a name cleaned before it is read has lost the
    /// extension `subtitle_format` answers from and the stem `sidecar_of` compares: this 300 character
    /// `x.srt` is SubRip text, and it is the sidecar of the 300 character video named beside it. What
    /// the row shows stays the cleaned name, because that is the half a viewer reads.
    #[test]
    fn a_long_subtitle_name_keeps_its_format() {
        let stem = "x".repeat(296);
        let name = format!("{stem}.srt");
        let video = format!("{stem}.mkv");
        assert_eq!(
            name.chars().count(),
            300,
            "the point of the name is that it is past the display limit"
        );
        let (beside, row) =
            subtitle_row(7, &name, Some(&video)).expect("a long name is still a subtitle file");
        assert_eq!(
            row.format, "srt",
            "the extension past 256 characters still named the format"
        );
        assert_eq!(row.file, 7, "and the row is the file it was built from");
        assert!(
            beside,
            "a name built on the video's own stem is its sidecar at any length"
        );
        assert_eq!(row.name, clean(&name), "the name shown is the cleaned one");
        assert_eq!(
            row.name.chars().count(),
            256,
            "cleaned to the display limit, which is where the extension went"
        );
    }

    /// What ranks as a video's own sidecar, and what only ranks after it.
    ///
    /// The dot is the rule: `a.srt` and `a.en.srt` are the sidecars of a video named `a`, and
    /// `about.srt`, `ab.srt`, `movies.vtt` and `en.srt` are not sidecars of the videos they are tested
    /// against here, however much they agree with them at the front.
    #[test]
    fn sidecar_match_needs_a_dot() {
        for (stem, name) in [
            ("a", "a.srt"),
            ("a", "a.en.srt"),
            ("the show", "the show.ass"),
            ("movie", "movie.ita.srt"),
        ] {
            assert!(
                sidecar_of(stem, name),
                "{name} should rank as {stem}'s own sidecar"
            );
        }
        for (stem, name) in [
            ("a", "about.srt"),
            ("a", "ab.srt"),
            ("movie", "movies.vtt"),
            ("movie", "en.srt"),
        ] {
            assert!(
                !sidecar_of(stem, name),
                "{name} wrongly ranks as {stem}'s own sidecar"
            );
        }
    }

    /// The dot rule carries over to names that are not plain ASCII, and neither side of the
    /// comparison is trusted to have arrived in the other one's case.
    ///
    /// `ÄRGER.srt` and `Ärger.en.SRT` are the sidecars of the film named `ärger.mkv`, and
    /// `Ärgerlich.srt` is not, however much it agrees with them at the front. Both spellings of the
    /// pair are run through: an ASCII-only folding lower-cases neither `Ä` nor `ä`, so the side that
    /// is left with it standing misses the side that does not have it, whichever one that is. An
    /// `a` with no umlaut is a different name and stays a different name: this folds case, not the
    /// letters themselves.
    #[test]
    fn sidecar_names_match_in_any_case() {
        for (video, name) in [
            ("ärger.mkv", "ÄRGER.srt"),
            ("ärger.mkv", "Ärger.en.SRT"),
            ("Ärger.mkv", "ärger.srt"),
            ("Ärger.mkv", "ÄRGER.EN.SRT"),
        ] {
            assert!(
                sidecar_of(video, name),
                "{name} should rank as {video}'s own sidecar"
            );
        }
        for (video, name) in [
            ("ärger.mkv", "Ärgerlich.srt"),
            ("Ärger.mkv", "arger.srt"),
            ("ärger.mkv", "arger.en.srt"),
        ] {
            assert!(
                !sidecar_of(video, name),
                "{name} wrongly ranks as {video}'s own sidecar"
            );
        }
    }
}
