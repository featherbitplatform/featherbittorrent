//! One track's worth of samples on their way into fragmented MP4, and what the
//! writer needs to know about the track they came from.
//!
//! A [`TrackSource`] hands over [`Sample`]s from one track of one container, a
//! [`FragmentStream`] turns a source's samples into fragmented MP4 bytes, and a
//! [`TrackConfig`] carries what [`init_segment`](crate::init_segment) and
//! [`FragmentWriter`](crate::FragmentWriter) need before the first sample
//! arrives. The sources themselves live in the container modules:
//! [`mkv`](crate::mkv) opens a Matroska or WebM track, `mp4in` an MP4 one, and
//! [`open_track`](crate::open_track) picks between them.
use crate::codec::{Codec, TrackKind};

/// One sample of one track: a packet out of a container, with its timestamps.
///
/// Times are in the track's own timescale and are the container's, before
/// [`TIME_OFFSET_MS`](crate::TIME_OFFSET_MS) is applied; the writer offsets every
/// decode time it puts on the wire. Samples arrive in decode order, which is the
/// order containers store them in.
///
/// `dts` is `None` when the source has only presentation times, which is
/// Matroska: the writer then synthesizes decode times so a B-frame's composition
/// offset is still signed the way `trun` version 1 says (see [`fmp4`](crate::fmp4)).
/// When the source does know a decode time - MP4 in, later - it is used as given,
/// subject to the same strictly-increasing clamp. A fragment cannot mix the two:
/// some samples with and some without is `Malformed`.
///
/// `duration` is `None` when the container does not state one, which is most
/// samples in both containers: the writer then takes it from the next sample's
/// decode time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// Presentation time, in the track's timescale.
    pub pts: i64,
    /// Decode time, when the source states one.
    pub dts: Option<i64>,
    /// Duration, when the source states one. Only the last sample of a stream
    /// gets its stated value written (it is what `finish` would otherwise have
    /// to guess); every other duration on the wire comes from the decode
    /// timeline, because a stated value that differed from the gap between
    /// decode times would shift everything after it.
    pub duration: Option<u32>,
    /// Whether a decoder can start from this sample: a keyframe, or the first
    /// sample after a seek.
    pub keyframe: bool,
    /// The sample's bytes, exactly as the container holds them.
    pub data: Vec<u8>,
}

/// What the fragmented MP4 writer needs about one track, decided before any
/// sample arrives.
///
/// Task 3 builds this from a probed track plus the sample entry
/// [`sample_entry`](crate::sample_entry) worked out for it. The sample entry is
/// kept because the init segment writes it into `stsd`, and because fragments of
/// the same track all refer to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackConfig {
    /// The track's timescale: every timestamp in the output, `tfdt` included.
    pub timescale: u32,
    /// Video or audio: which handler the track gets, and how its samples are
    /// flagged.
    pub kind: TrackKind,
    /// Which codec the samples are, for messages and for the flags a codec asks for.
    pub codec: Codec,
    /// The sample entry `stsd` holds and every fragment's `trun` refers to.
    pub sample_entry: mp4_atom::Codec,
    /// Encoded pixel dimensions, for `tkhd` and the sample entry; 0 when the
    /// container did not state them.
    pub width: u16,
    /// Encoded pixel dimensions, for `tkhd` and the sample entry; 0 when the
    /// container did not state them.
    pub height: u16,
}

/// Why a source could not deliver the next sample.
///
/// [`MediaError`](crate::MediaError) is the container's own verdict; this adds
/// the source's side of the boundary: a reader that went away between two
/// requests is `Disconnected`, which a file on disk cannot do but a piece of a
/// torrent can, and the container verdicts are carried through unchanged so a
/// caller can still tell a codec it does not speak from a file that does not
/// hold together.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SourceError {
    /// The reader failed. A [`std::io::ErrorKind::TimedOut`] stays here: a piece
    /// of a torrent that has not arrived yet is not a verdict about the file.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The stream stopped making sense: an element that contradicts its own
    /// container, a timestamp that cannot be placed on the timeline.
    #[error("malformed: {0}")]
    Malformed(String),
    /// A codec or container feature this source does not read.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The source went away between requests: its reader was dropped, or the
    /// task feeding it ended. A retry from a fresh open is the caller's move,
    /// not a verdict about the file.
    #[error("source disconnected")]
    Disconnected,
}

impl SourceError {
    /// A container verdict, carried across the source boundary unchanged.
    pub(crate) fn from_media(error: crate::MediaError) -> Self {
        match error {
            crate::MediaError::Io(err) => SourceError::Io(err),
            crate::MediaError::Malformed(message) => SourceError::Malformed(message),
            crate::MediaError::Unsupported(message) => SourceError::Unsupported(message),
        }
    }
}

impl From<SourceError> for crate::MediaError {
    /// The other direction, for a caller that treats a source request as one
    /// fallible media operation. `Disconnected` is the only verdict with no
    /// `MediaError` equivalent: it is a claim about the reader, not the file,
    /// and a media-side caller that must answer with a `MediaError` gets the
    /// closest one, a read failure.
    fn from(error: SourceError) -> Self {
        match error {
            SourceError::Io(err) => crate::MediaError::Io(err),
            SourceError::Malformed(message) => crate::MediaError::Malformed(message),
            SourceError::Unsupported(message) => crate::MediaError::Unsupported(message),
            SourceError::Disconnected => crate::MediaError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "media source disconnected",
            )),
        }
    }
}

/// A source's result: a sample, an end, or a verdict about why neither arrived.
pub(crate) type SourceResult<T> = Result<T, SourceError>;

/// The first failure of a stream, kept so every later request gets the same
/// verdict.
///
/// The verdicts themselves are not `Clone` - an `io::Error` carries OS state -
/// so the kind is kept apart from the message and the error is rebuilt for
/// every call. That is what makes a reader that timed out keep answering
/// `Io` after its first timeout instead of inventing a `Malformed` verdict
/// about a file it never got to finish reading.
#[derive(Debug, Clone)]
pub(crate) enum StoredError {
    Io(std::io::ErrorKind, String),
    Malformed(String),
    Unsupported(String),
    /// Kept as itself so a source that reported one is replayed verbatim;
    /// the closest `MediaError` is the read failure
    /// [`MediaError::from`] gives it.
    Disconnected,
}

impl StoredError {
    pub(crate) fn from_source(error: &SourceError) -> Self {
        match error {
            SourceError::Io(err) => Self::Io(err.kind(), err.to_string()),
            SourceError::Malformed(message) => Self::Malformed(message.clone()),
            SourceError::Unsupported(message) => Self::Unsupported(message.clone()),
            SourceError::Disconnected => Self::Disconnected,
        }
    }

    fn from_media(error: &crate::MediaError) -> Self {
        match error {
            crate::MediaError::Io(err) => Self::Io(err.kind(), err.to_string()),
            crate::MediaError::Malformed(message) => Self::Malformed(message.clone()),
            crate::MediaError::Unsupported(message) => Self::Unsupported(message.clone()),
        }
    }

    pub(crate) fn to_source_error(&self) -> SourceError {
        match self {
            Self::Io(kind, message) => SourceError::Io(std::io::Error::new(*kind, message.clone())),
            Self::Malformed(message) => SourceError::Malformed(message.clone()),
            Self::Unsupported(message) => SourceError::Unsupported(message.clone()),
            Self::Disconnected => SourceError::Disconnected,
        }
    }

    fn to_media_error(&self) -> crate::MediaError {
        crate::MediaError::from(self.to_source_error())
    }
}

/// One track's samples, from whatever container they live in.
///
/// A source hands over [`Sample`]s in decode order, the order containers store
/// them, and never panics: a hostile file - one whose elements contradict their
/// own sizes, whose timestamps overflow, whose declared durations are junk -
/// produces an `Err`, not a panic, and not a hang. After the first error a
/// source reports the same verdict to every later request rather than trying to
/// keep half-parsed state alive; the plan's rule is that remote input changes
/// the answer, never the control flow.
///
/// `Send` is a supertrait because a stream of samples crosses threads on its way
/// to an HTTP response: the source is boxed into a
/// [`FragmentStream`] and moved to whichever task is writing the response.
pub(crate) trait TrackSource: Send {
    /// What the writer needs before the first sample: timescale, kind, codec,
    /// sample entry. Infallible because a source that could not decide these
    /// was never opened: `open_track` reports every verdict this could need to
    /// make before it hands a source over.
    fn config(&self) -> &TrackConfig;

    /// The next sample, or `None` at the end of the track. Errors are verdicts
    /// about the stream, and a source that errored once keeps reporting it: a
    /// caller may poll a source again (a torrent piece that was not there at
    /// request *n* may be there at *n + 1*), and a stream that went malformed
    /// mid-track stays malformed.
    fn next_sample(&mut self) -> SourceResult<Option<Sample>>;

    /// Where this source's samples actually begin, in milliseconds of source
    /// time, after any seeking it did. A source that does not seek can leave
    /// this at zero and mean it.
    fn start_ms(&self) -> u64 {
        0
    }
}

/// The `Content-Type` a fragmented MP4 stream of this track is served under:
/// the MP4 container for the track's kind, plus the track's RFC 6381 codec
/// string - `video/mp4; codecs="avc1.64000A"`, `audio/mp4; codecs="opus"`.
/// The codec string is not worked out twice: the probe already computed it when
/// the track was identified (see [`crate::codec`]), and this pastes that exact
/// entry in, whichever container the track came from. A codec the probe gave no
/// RFC 6381 string gets the bare container type, which a `SourceBuffer` still
/// accepts for the init segment it is about to parse.
pub(crate) fn mp4_mime(kind: TrackKind, mime_codecs: &[String]) -> String {
    let container = match kind {
        TrackKind::Video => "video/mp4",
        TrackKind::Audio => "audio/mp4",
        TrackKind::Subtitle => unreachable!("subtitle tracks are refused before this"),
    };
    match mime_codecs.first() {
        Some(codec) => format!("{container}; codecs=\"{codec}\""),
        None => container.to_string(),
    }
}

/// One track source's samples as fragmented MP4 bytes: one init segment, then
/// one `moof` + `mdat` fragment at a time, in `Vec`s sized exactly to their
/// contents.
///
/// This is the boundary every later task hands a player: the API's byte range
/// endpoint pulls fragments until the response ends or the client disconnects,
/// and the HLS manifest writer (Task 4) cuts the same fragments to list them.
/// The first fragment is prefixed with the init segment, so a client that asks
/// for fragment 1 of a fresh stream needs only one request to start decoding;
/// later fragments are plain `moof` + `mdat`. At the end of the source the
/// writer is finished exactly once, so the last fragment carries the tail the
/// writer could not seal earlier, and further requests answer `None`.
///
/// Fragments are pulled, never queued: exactly one source request can produce
/// at most one returned fragment (and one fragment is exactly one source
/// request plus a seal, never more), so a caller that stops pulling - a browser
/// tab that closes mid-stream - leaves at most one fragment in memory, which is
/// the plan's bounded-memory rule for a stream nobody is watching.
pub struct FragmentStream {
    source: Box<dyn TrackSource>,
    writer: crate::FragmentWriter,
    /// The init segment. Kept for the whole life of the stream so
    /// [`init_segment`](Self::init_segment) answers whenever a caller asks -
    /// a few kilobytes, and the first fragment copies it rather than moving it
    /// out.
    init: Vec<u8>,
    /// Whether the init segment has already been put in front of a fragment.
    /// `init` stays readable for [`init_segment`](Self::init_segment) for the
    /// stream's whole life, so "the prefix has been served" needs its own
    /// memory; without it every fragment would arrive as init-plus-fragment.
    prefixed: bool,
    mime: String,
    /// Whether the source has run out and the writer has been finished: after
    /// this, every call answers `None` without touching either again.
    ended: bool,
    /// The first failure, which every later call repeats with the same kind: a
    /// failed write leaves the fragment writer half-moved, and a second attempt
    /// would invent bytes.
    failed: Option<StoredError>,
}

impl std::fmt::Debug for FragmentStream {
    /// Field-by-field, except the source (whose own `Debug` a boxed source may
    /// not answer for every reader behind it) and the sample table, which are
    /// megabytes of data a log line should not carry.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FragmentStream")
            .field("mime", &self.mime)
            .field("init_len", &self.init.len())
            .field("ended", &self.ended)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl FragmentStream {
    /// The fragmented MP4 stream for one boxed track source.
    ///
    /// Crate-internal: the public way in is [`open_track`](crate::open_track),
    /// which opens the container and knows the codec string `mime` reports.
    ///
    /// The init segment is built from the source's [`TrackConfig`] here, before
    /// any sample arrives, because a caller needs it for `Content-Type` and a
    /// first request whether or not the track has samples: a track that turns
    /// out to be empty is an empty stream, not a failed open.
    ///
    /// A zero timescale in the config is not rejected here (it is rejected by
    /// [`init_segment`](crate::init_segment), which this calls, and by the
    /// fragment writer at its first use).
    pub(crate) fn new(
        source: Box<dyn TrackSource>,
        mime: String,
    ) -> Result<Self, crate::MediaError> {
        // Read the config out of the box before the fields take it: the borrow
        // needs the source by name, and `writer` moves the box away.
        let config = source.config().clone();
        let init = crate::init_segment(&config)?;
        Ok(Self {
            source,
            writer: crate::FragmentWriter::new(&config),
            init,
            prefixed: false,
            mime,
            ended: false,
            failed: None,
        })
    }

    /// The MIME type to serve this stream under: fragmented MP4 for the
    /// track's kind, carrying the track's RFC 6381 codec string - exactly what
    /// a browser constructs a `SourceBuffer` with, e.g.
    /// `video/mp4; codecs="avc1.64000A"` or `audio/mp4; codecs="opus"`. A
    /// response handler sets `Content-Type` from this; the bytes it describes
    /// are the init segment plus the fragments, never the source container.
    pub fn mime(&self) -> String {
        self.mime.clone()
    }

    /// The init segment: the bytes a player needs before any fragment, which
    /// the first returned fragment also carries in front of itself. Kept for
    /// the stream's whole life, so this answers the same bytes after the first
    /// fragment has been handed out as before it.
    pub fn init_segment(&self) -> &[u8] {
        &self.init
    }

    /// Where the stream's samples begin, in milliseconds of source time, after
    /// the source's own seeking: what a `tfdt`-based segment map and the page's
    /// offset arithmetic have to agree with.
    pub fn start_ms(&self) -> u64 {
        self.source.start_ms()
    }

    /// The next fragment, or `None` when the stream is over.
    ///
    /// The first call returns the init segment concatenated with the first
    /// fragment; later calls return plain `moof` + `mdat`. A source with no
    /// samples at all gets no fragment, so its first call is the init segment
    /// alone - a complete if silent stream - and the call after that is
    /// `None`.
    ///
    /// A source that fails ends the stream with that failure, repeated
    /// verbatim to every later call; the writer is not retried past its first
    /// error.
    pub fn next_fragment(&mut self) -> Result<Option<Vec<u8>>, crate::MediaError> {
        if let Some(failure) = &self.failed {
            return Err(failure.to_media_error());
        }
        if self.ended {
            return Ok(None);
        }
        let result = self.next_fragment_inner();
        if let Err(error) = &result {
            // Keep the verdict's kind and its message, and repeat both: a
            // source that failed once has already moved half a fragment's
            // state, and a reader that timed out is still a reader that timed
            // out on the next call, not a file that stopped making sense.
            self.failed
                .get_or_insert_with(|| StoredError::from_media(error));
        }
        result
    }

    fn next_fragment_inner(&mut self) -> Result<Option<Vec<u8>>, crate::MediaError> {
        loop {
            match self.source.next_sample().map_err(crate::MediaError::from)? {
                Some(sample) => {
                    if let Some(fragment) = self.writer.push(sample)? {
                        return Ok(Some(self.take_prefix(fragment)));
                    }
                }
                None => {
                    // The source is over: seal whatever the writer still holds,
                    // once. `finish` answers `None` when no sample was ever
                    // pushed or held, and then the stream's answer is the init
                    // segment alone - a complete if silent stream.
                    let last = self.writer.finish()?;
                    self.ended = true;
                    return Ok(match last {
                        Some(fragment) => Some(self.take_prefix(fragment)),
                        None if !self.init.is_empty() => Some(self.init.clone()),
                        None => None,
                    });
                }
            }
        }
    }

    /// The init segment in front of `fragment` the first time this is called,
    /// and `fragment` alone every time after: a `SourceBuffer` wants the init
    /// segment exactly once, ahead of the first media bytes.
    fn take_prefix(&mut self, fragment: Vec<u8>) -> Vec<u8> {
        if self.prefixed {
            return fragment;
        }
        self.prefixed = true;
        let mut out = Vec::with_capacity(self.init.len() + fragment.len());
        out.extend_from_slice(&self.init);
        out.extend_from_slice(&fragment);
        out
    }
}
