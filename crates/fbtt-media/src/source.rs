//! One track's worth of samples on their way into fragmented MP4, and what the
//! writer needs to know about the track they came from.
//!
//! A [`TrackSource`] hands over [`Sample`]s from one track of one container, a
//! [`FragmentStream`] turns a source's samples into fragmented MP4 bytes, and a
//! [`TrackConfig`] carries what [`init_segment`](crate::init_segment) and
//! [`FragmentWriter`](crate::FragmentWriter) need before the first sample
//! arrives. The sources themselves live in the container modules:
//! [`mkv`](crate::mkv) opens a Matroska track; MP4 in is the plan's Task 4.
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
pub enum SourceError {
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
pub type SourceResult<T> = Result<T, SourceError>;

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
pub trait TrackSource: Send {
    /// What the writer needs before the first sample: timescale, kind, codec,
    /// sample entry. Infallible because a source that could not decide these
    /// was never opened: [`open_track`](crate::mkv::open_track) reports every
    /// verdict this could need to make before it hands a source over.
    fn config(&self) -> &TrackConfig;

    /// The next sample, or `None` at the end of the track. Errors are verdicts
    /// about the stream, and a source that errored once keeps reporting it: a
    /// caller may poll a source again (a torrent piece that was not there at
    /// request *n* may be there at *n + 1*), and a stream that went malformed
    /// mid-track stays malformed.
    fn next_sample(&mut self) -> SourceResult<Option<Sample>>;

    /// Re-position the source at or just before `start_ms` of source time, at a
    /// sync point. A source overrides this only when it can actually seek; the
    /// default reports `Unsupported` rather than silently doing nothing,
    /// because a caller that asked to seek and got silence would serve samples
    /// from the wrong place.
    fn seek(&mut self, start_ms: u64) -> SourceResult<()> {
        let _ = start_ms;
        Err(SourceError::Unsupported(
            "this source cannot seek".to_string(),
        ))
    }

    /// Where this source's samples actually begin, in milliseconds of source
    /// time, after any seeking it did. A source that does not seek can leave
    /// this at zero and mean it.
    fn start_ms(&self) -> u64 {
        0
    }

    /// The MIME type of the byte stream a [`FragmentStream`] of this source
    /// produces, so a response handler can set `Content-Type` without knowing
    /// the container: Matroska sources say `video/x-matroska` or `video/webm`,
    /// and the default says fragmented MP4, which is what a source that carries
    /// MP4 samples remuxes to.
    fn mime(&self) -> &'static str {
        "video/mp4"
    }
}

/// A [`TrackSource`]'s samples as fragmented MP4 bytes: one init segment, then
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
    /// The init segment, until the first fragment has taken it. `Vec::new()`
    /// afterwards frees nothing by itself, but it is dropped when the first
    /// fragment is built, so a long stream holds no init segment.
    init: Vec<u8>,
    mime: &'static str,
    /// The message of the first failure, which every later call repeats: a
    /// failed write leaves the fragment writer half-moved, and a second attempt
    /// would invent bytes.
    failed: Option<String>,
}

impl std::fmt::Debug for FragmentStream {
    /// Field-by-field, except the source (whose own `Debug` a boxed source may
    /// not answer for every reader behind it) and the sample table, which are
    /// megabytes of data a log line should not carry.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FragmentStream")
            .field("mime", &self.mime)
            .field("init_len", &self.init.len())
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl FragmentStream {
    /// The fragmented MP4 stream for one track source.
    ///
    /// The init segment is built from the source's [`TrackConfig`] here, before
    /// any sample arrives, because a caller needs it for `Content-Type` and a
    /// first request whether or not the track has samples: a track that turns
    /// out to be empty is an empty stream, not a failed open.
    ///
    /// A zero timescale in the config is not rejected here (it is rejected by
    /// [`init_segment`](crate::init_segment), which this calls, and by the
    /// fragment writer at its first use).
    pub fn new<S: TrackSource + 'static>(source: S) -> Result<Self, SourceError> {
        // Read everything the source has to say before boxing it: the config
        // borrow and the mime answer need the source by name, and the box eats
        // the name.
        let config = source.config().clone();
        let mime = source.mime();
        let init = crate::init_segment(&config).map_err(SourceError::from_media)?;
        Ok(Self {
            source: Box::new(source),
            writer: crate::FragmentWriter::new(&config),
            init,
            mime,
            failed: None,
        })
    }

    /// The MIME type to serve this stream under: what [`TrackSource::mime`]
    /// answered when the stream was built - `video/webm` or `video/x-matroska`
    /// for a Matroska source, per the plan's ruling that the player asks the
    /// browser for the container the samples came from. A response handler sets
    /// `Content-Type` from this.
    pub fn mime(&self) -> &'static str {
        self.mime
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
    pub fn next_fragment(&mut self) -> SourceResult<Option<Vec<u8>>> {
        if let Some(reason) = &self.failed {
            return Err(SourceError::Malformed(reason.clone()));
        }
        let result = self.next_fragment_inner();
        if let Err(error) = &result {
            // Keep the verdict's message, and repeat it: a source that failed
            // once has already moved half a fragment's state.
            let reason = match error {
                SourceError::Io(_) | SourceError::Disconnected | SourceError::Unsupported(_) => {
                    error.to_string()
                }
                SourceError::Malformed(message) => message.clone(),
            };
            self.failed.get_or_insert(reason);
        }
        result
    }

    fn next_fragment_inner(&mut self) -> SourceResult<Option<Vec<u8>>> {
        loop {
            match self.source.next_sample()? {
                Some(sample) => {
                    if let Some(fragment) =
                        self.writer.push(sample).map_err(SourceError::from_media)?
                    {
                        return Ok(Some(self.take_prefix(fragment)));
                    }
                }
                None => {
                    // The source is over: seal whatever the writer still holds.
                    // `finish` answers `None` when no sample was ever pushed or
                    // held, and then this stream is done.
                    let last = self.writer.finish().map_err(SourceError::from_media)?;
                    return Ok(match last {
                        Some(fragment) => Some(self.take_prefix(fragment)),
                        None if !self.init.is_empty() => Some(std::mem::take(&mut self.init)),
                        None => None,
                    });
                }
            }
        }
    }

    /// The init segment in front of `fragment` if it has not been handed out
    /// yet, and `fragment` alone afterwards.
    fn take_prefix(&mut self, fragment: Vec<u8>) -> Vec<u8> {
        if self.init.is_empty() {
            return fragment;
        }
        let mut out = std::mem::take(&mut self.init);
        out.extend_from_slice(&fragment);
        out
    }
}
