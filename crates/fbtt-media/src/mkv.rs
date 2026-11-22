//! One Matroska track, remuxed into fragmented MP4 from any start time.
//!
//! [`open_track`] takes a reader, a track number and a start time, and hands
//! back an [`MkvSource`]: a [`TrackSource`] that yields exactly that track's
//! samples, in the order the container stores them, starting at the first sync
//! point at or after the requested time. The samples are the container's own
//! bytes - this crate copies, it does not decode - plus the timestamps, keyframe
//! flags and durations the container states, in the timescale
//! [`FragmentStream`](crate::FragmentStream) puts on the wire.
//!
//! Everything here treats the file as hostile. The pinned featherbit fork of
//! `matroska-demuxer` overflow-checks the cue and offset arithmetic whose
//! additions upstream 0.8.1 could panic on with junk input, but the indexing
//! additions upstream panicked on (a doubled `SeekHead` reference, a seek
//! position overflowing the position arithmetic) are panics in the fork too,
//! and it reads every frame of every track through one stream - so the walk
//! to a track's samples, its seek points and its first frame all run under a
//! [`catch_unwind`], with [`CappedLen`] and [`ReadBudget`] holding the
//! demuxer's trust in declared sizes down before any of it is reached. A hostile
//! file produces a [`SourceError`], never a panic and never
//! a hang.
//!
//! Content encodings are the demuxer's blind spot: it returns block bytes
//! exactly as the container stores them, so header stripping is undone here,
//! byte for byte, before samples leave for a decoder. Every other compaction
//! and real encryption are `Unsupported`: a signed-only track
//! (`ContentEncAlgo::NotEncrypted`) passes through because nothing about its
//! bytes is encrypted.
use crate::capped::{CappedLen, ReadBudget};
use crate::codec::{Codec, TrackKind};
use crate::probe::{TrackInfo, matroska_audio_rate, matroska_error, matroska_kind, open_matroska};
use crate::source::{
    FragmentStream, Sample, SourceError, SourceResult, StoredError, TrackConfig, TrackSource,
};
use crate::{ELEMENT_CAP, MAX_SKIPPED_FRAMES, MediaError};
use matroska_demuxer::{
    ContentCompAlgo, ContentEncAlgo, ContentEncoding, ContentEncodingValue, Frame, MatroskaFile,
    TrackEntry,
};
use std::io::{Read, Seek};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The video track timescale: 90 kHz, which is Matroska's own default rate
/// (`TimestampScale` 1 ms expressed per second), so a video PTS transfers from
/// the container to the MP4 timeline without rounding, and the rate ISO BMFF
/// video timelines use.
const VIDEO_TIMESCALE: u32 = 90_000;

/// One Matroska track, opened for reading.
///
/// The demuxer interleaves every track of the file into one frame stream; this
/// hands over only the track it was opened on, and a frame it does not want is
/// passed over after the demuxer has already read it - the seeking that skips
/// whole clusters happens in the seek, not in the walk.
///
/// The reader type stays private on purpose: [`open_track`] boxes this into a
/// [`FragmentStream`] and hands that out, because the point of a source is its
/// samples, not its type.
///
/// `R` stays the caller's own reader inside [`CappedLen`]: a `Box<dyn Read +
/// Seek>` would erase a torrent reader's timeouts into the reader trait's
/// own `io::Error`, and those have to survive to the API as retryable.
pub(crate) struct MkvSource<R: Read + Seek> {
    /// The demuxer, past its header and (when the start time asked for it)
    /// sought to its first cluster.
    file: MatroskaFile<CappedLen<R>>,
    /// What [`TrackSource::config`] answers.
    config: TrackConfig,
    /// `Content-Type` for a fragmented MP4 stream of this track: the container
    /// for the track's kind plus its RFC 6381 codec string, see [`mp4_mime`].
    mime: String,
    /// The container's own track number, compared against each frame's.
    track: u64,
    /// Nanoseconds of source time per container timestamp unit.
    scale: u64,
    /// Ticks per second of the output timeline.
    timescale: u32,
    /// Nanoseconds of source time below which audio frames are dropped; video
    /// ignores it (a keyframe before the start time is reference material for
    /// the frames that follow it, so video starts at its sync point, full
    /// stop).
    start_ns: u128,
    /// Bytes stripped off every block by the container, to be prepended back.
    strip_prefix: Vec<u8>,
    /// The first frame, held back for a sample entry that needs one. Served
    /// out through [`next_sample`](TrackSource::next_sample) like any other.
    pending: Option<PendingFrame>,
    /// Frames read without one being emitted since the last emitted frame.
    skipped: usize,
    /// Where the samples actually begin, in milliseconds of source time: the
    /// requested start until the first frame arrives, then that frame's PTS.
    reported_start_ms: u64,
    /// Whether a video keyframe has been emitted yet: everything before the
    /// first one is decode-only material and gets dropped.
    anchored: bool,
    /// Whether any sample has been emitted yet.
    emitted: bool,
    /// The first failure, repeated to every later request with its kind kept:
    /// a read that timed out is still timed out the second time it is asked.
    failed: Option<StoredError>,
    /// The track's `DefaultDuration` in nanoseconds, the duration a frame gets
    /// when its block states none - and the spacing between the frames of a
    /// same-timestamp run, see [`MkvSource::sample_with_timestamp`].
    default_duration: Option<u64>,
    /// Same-timestamp run state: the container timestamp the run started at,
    /// and how many of this track's frames have carried it. Every frame of a
    /// laced block carries the block's own timestamp, and the run length is
    /// what turns that into one distinct PTS per frame.
    run_timestamp: u64,
    run_len: u64,
    /// A frame slot the demuxer fills in place, reused every read so each
    /// frame costs the buffer it needs, not one per call.
    frame: Frame,
}

impl<R: Read + Seek> std::fmt::Debug for MkvSource<R> {
    /// Field by field, except the demuxer and the frame slot: the reader behind
    /// a source is the caller's (a file, a torrent connection), and the frame
    /// slot is a megabyte-sized `Vec` a log line has no business printing.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MkvSource")
            .field("mime", &self.mime)
            .field("track", &self.track)
            .field("timescale", &self.timescale)
            .field("start_ns", &self.start_ns)
            .field("pending", &self.pending.is_some())
            .field("skipped", &self.skipped)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

/// The frame held back for a sample entry, with just enough of the demuxer's
/// report to pass it on later.
struct PendingFrame {
    timestamp: u64,
    keyframe: Option<bool>,
    duration: Option<u64>,
    data: Vec<u8>,
}

/// Ask the demuxer for one frame under a panic guard, `Ok(false)` being the
/// end of the stream. The frame lands in `slot`, which is why callers read it
/// back through the slot rather than getting it by value.
///
/// `slot.track` is zeroed before every call because `Ok(true)` does not
/// guarantee a filled slot: the demuxer answers `Ok(true)` for a `BlockGroup`
/// that holds no `Block` without touching the slot at all, so the stale track
/// number of the previous frame would pass such a read off as "a frame of the
/// right track, empty". Zero is the one value no real `TrackNumber` has, so
/// "the slot still says track 0" is how callers tell a filled frame from a
/// stale one.
pub(crate) fn guarded_next<R: Read + Seek>(
    file: &mut MatroskaFile<CappedLen<R>>,
    slot: &mut Frame,
    what: &str,
) -> Result<bool, MediaError> {
    slot.track = 0;
    match catch_unwind(AssertUnwindSafe(|| file.next_frame(slot))) {
        Ok(Ok(got)) => Ok(got),
        Ok(Err(error)) => Err(matroska_error(error)),
        Err(_) => Err(MediaError::Malformed(format!(
            "matroska demuxer panicked reading {what}"
        ))),
    }
}

/// Seek the demuxer under a panic guard.
pub(crate) fn guarded_seek<R: Read + Seek>(
    file: &mut MatroskaFile<CappedLen<R>>,
    target: u64,
) -> Result<(), MediaError> {
    match catch_unwind(AssertUnwindSafe(|| file.seek(target))) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(matroska_error(error)),
        Err(_) => Err(MediaError::Malformed(
            "matroska demuxer panicked seeking to the start time".to_string(),
        )),
    }
}

/// Container timestamp units for a millisecond request, and the start time in
/// nanoseconds, or `Malformed` when the file's `TimestampScale` cannot hold
/// the request.
fn start_in_scale_units(start_ms: u64, scale: u64) -> Result<(u64, u128), MediaError> {
    let start_ns = u128::from(start_ms) * 1_000_000;
    // Truncating down: the target handed the demuxer is the whole scale unit
    // at or below `start_ms`, and the demuxer's own walk then lands on the
    // first frame at or after that unit - never earlier, which is what makes
    // the emitted stream begin at or after the time that was asked for.
    let units = start_ns / u128::from(scale);
    let units = u64::try_from(units).map_err(|_| {
        MediaError::Malformed(format!(
            "start time {start_ms} ms is past what TimestampScale {scale} can count"
        ))
    })?;
    Ok((units, start_ns))
}

impl<R: Read + Seek> MkvSource<R> {
    /// The first error, if there is one, as the verdict every later call gives,
    /// its kind intact: an `Io` verdict replays as `Io`, not as a `Malformed`
    /// retelling of it.
    fn poisoned(&self) -> Option<SourceError> {
        self.failed.as_ref().map(StoredError::to_source_error)
    }

    fn poison(&mut self, error: &SourceError) {
        self.failed
            .get_or_insert_with(|| StoredError::from_source(error));
    }

    /// Count one frame walked without emitting, and say whether the walk has
    /// gone on long enough to be a stream that never delivers.
    fn walk_beyond_patience(&mut self) -> Option<MediaError> {
        self.skipped += 1;
        if self.skipped > MAX_SKIPPED_FRAMES {
            Some(MediaError::Malformed("no keyframe found".to_string()))
        } else {
            None
        }
    }

    /// One frame, in container terms: this track's, already filtered against
    /// the start time and the keyframe rule, with the container's timestamps
    /// translated into output ticks - same-timestamp runs spaced by
    /// `DefaultDuration` on the way. `Ok(None)` is the end of the stream, and
    /// `skipped` accounts for every frame passed over on the way.
    fn next_emittable(&mut self) -> Result<Option<Sample>, MediaError> {
        loop {
            let (timestamp, keyframe, data) = match self.pending.take() {
                Some(pending) => {
                    // The slot's duration is stale between frames, so hand the
                    // held frame's stated duration to it: `frame_duration`
                    // reads the duration through the slot.
                    self.frame.duration = pending.duration;
                    (pending.timestamp, pending.keyframe, pending.data)
                }
                None => {
                    if !guarded_next(&mut self.file, &mut self.frame, "frames")? {
                        return Ok(None);
                    }
                    // A slot that still claims track 0 was filled by nothing:
                    // `guarded_next` zeroes it before the call, and an empty
                    // `BlockGroup` leaves it that way. No frame, so it counts
                    // against patience exactly like the frames of other tracks
                    // this branch already passes over.
                    if self.frame.track != self.track {
                        if let Some(gave_up) = self.walk_beyond_patience() {
                            return Err(gave_up);
                        }
                        continue;
                    }
                    let timestamp = self.frame.timestamp;
                    let keyframe = self.frame.is_keyframe;
                    let data = std::mem::take(&mut self.frame.data);
                    (timestamp, keyframe, data)
                }
            };
            // Every frame of this track advances the same-timestamp run,
            // emitted or dropped: a frame dropped below still occupied its
            // slot in its block's timeline, and the next one must not slide
            // back into it.
            let run_index = self.run_index_for(timestamp);
            let keyframe = match self.filter_frame(keyframe, timestamp) {
                FrameAction::Drop => {
                    if let Some(gave_up) = self.walk_beyond_patience() {
                        return Err(gave_up);
                    }
                    continue;
                }
                FrameAction::Emit(keyframe) => keyframe,
            };
            if !self.strip_prefix.is_empty() {
                let mut with_header = Vec::with_capacity(self.strip_prefix.len() + data.len());
                with_header.extend_from_slice(&self.strip_prefix);
                with_header.extend_from_slice(&data);
                self.frame.data = data;
                return Ok(Some(self.sample_with_timestamp(
                    timestamp,
                    run_index,
                    keyframe,
                    with_header,
                )?));
            }
            return Ok(Some(
                self.sample_with_timestamp(timestamp, run_index, keyframe, data)?,
            ));
        }
    }

    /// What to do with a frame of this track: drop it, or emit it with the
    /// keyframe flag the fragment writer will write.
    ///
    /// Video drops everything before its first sync point: frames the block
    /// flags `Some(false)` are decode-only material, and an unflagged frame
    /// (a `BlockGroup` frame: the demuxer reports no flag at all, because the
    /// flag lives in `ReferenceBlock` elements it does not read) counts as a
    /// sync point only while the track has not found one yet, which is the
    /// only honest reading of a container that does not say. After the anchor,
    /// an unflagged frame is emitted as non-sync: `Some(false)` is what the
    /// writer understands, and inventing sync flags the file never claimed
    /// would let the player cut a fragment where no keyframe is.
    ///
    /// Video ignores `start_ns` on purpose: what a seek leaves is the stream
    /// from its landing frame onward, and the frames after that keyframe are
    /// the references and reorder partners that make it decodable - including
    /// ones that present before the start time, which a start-time filter
    /// would drop precisely to the decoder's loss. The keyframe rule above is
    /// what decides where video starts. Audio filters by start time, because
    /// every audio frame is independently decodable and one before the start
    /// time would play a sound the viewer asked to skip.
    fn filter_frame(&mut self, keyframe: Option<bool>, timestamp: u64) -> FrameAction {
        if !self.is_video() {
            if (u128::from(timestamp) * u128::from(self.scale)) < self.start_ns {
                return FrameAction::Drop;
            }
            return FrameAction::Emit(true);
        }
        match keyframe {
            Some(true) => FrameAction::Emit(true),
            Some(false) if !self.anchored => FrameAction::Drop,
            Some(false) => FrameAction::Emit(false),
            None if self.anchored => FrameAction::Emit(false),
            None => FrameAction::Emit(true),
        }
    }

    fn is_video(&self) -> bool {
        self.config.kind == TrackKind::Video
    }

    /// Which frame of a same-timestamp run this frame is: 0 for the first
    /// frame to carry a timestamp, and one more for every later frame of this
    /// track that carries it. Every frame of a laced block carries the block's
    /// own timestamp; this index is the multiplier behind
    /// [`sample_with_timestamp`](Self::sample_with_timestamp)'s spacing.
    fn run_index_for(&mut self, timestamp: u64) -> u64 {
        let index = if timestamp == self.run_timestamp {
            self.run_len
        } else {
            self.run_timestamp = timestamp;
            self.run_len = 0;
            0
        };
        self.run_len = self.run_len.saturating_add(1);
        index
    }

    /// A [`Sample`] from one frame of the track: PTS in output ticks, the
    /// container's stated duration (its own `BlockDuration`, or the track's
    /// `DefaultDuration`) as a stated duration in output ticks, and the bytes.
    ///
    /// `run_index` spaces the frames that share a timestamp: every frame of a
    /// laced block carries its block's timestamp, and the i-th frame of such
    /// a run plays `i * DefaultDuration` later. Without that spacing four
    /// fixed-laced MP3 frames become four samples at one instant, which the
    /// fragment writer can only endow with one-tick durations. A run of one
    /// frame, or a track with no `DefaultDuration` to space by, keeps the
    /// container's timestamp as it is - those frames share the instant, and
    /// the writer then derives every duration from the gaps between decode
    /// times, which is its documented fallback.
    ///
    /// A PTS or duration too wide for the timeline is `Malformed`: a frame at
    /// 2^63 ticks on a 90 kHz timeline is a claim about the file no MP4
    /// timeline can carry.
    fn sample_with_timestamp(
        &mut self,
        timestamp: u64,
        run_index: u64,
        keyframe: bool,
        data: Vec<u8>,
    ) -> Result<Sample, MediaError> {
        let base_ns = u128::from(timestamp)
            .checked_mul(u128::from(self.scale))
            .ok_or_else(|| self.time_overflow())?;
        let nanoseconds = match (run_index, self.default_duration) {
            (0, _) | (_, None) => base_ns,
            (index, Some(default_ns)) => {
                let spacing = u128::from(index)
                    .checked_mul(u128::from(default_ns))
                    .ok_or_else(|| self.time_overflow())?;
                base_ns
                    .checked_add(spacing)
                    .ok_or_else(|| self.time_overflow())?
            }
        };
        let pts = self.to_ticks_from_ns(nanoseconds)?;
        let duration = self.frame_duration()?;
        if !self.emitted {
            self.emitted = true;
            self.reported_start_ms = source_milliseconds(nanoseconds);
        }
        self.skipped = 0;
        self.anchored |= keyframe;
        Ok(Sample {
            pts,
            dts: None,
            duration,
            keyframe,
            data,
        })
    }

    /// Nanoseconds of source time to output ticks, all in `u128` so a junk
    /// timestamp or spacing is refused rather than wrapped.
    fn to_ticks_from_ns(&self, nanoseconds: u128) -> Result<i64, MediaError> {
        let ticks = nanoseconds
            .checked_mul(u128::from(self.timescale))
            .ok_or_else(|| self.time_overflow())?
            / 1_000_000_000;
        i64::try_from(ticks).map_err(|_| self.time_overflow())
    }

    fn time_overflow(&self) -> MediaError {
        MediaError::Malformed(format!(
            "track {} has a timestamp no timeline can hold",
            self.track
        ))
    }

    /// The frame's stated duration in output ticks: `BlockDuration` when the
    /// block states one (that element counts the segment's `TimestampScale`),
    /// the track's `DefaultDuration` otherwise (that one counts nanoseconds,
    /// full stop, whatever the scale says - the specification is explicit).
    /// `None` when the track states neither: the writer then takes durations
    /// from the gaps between decode times.
    fn frame_duration(&self) -> Result<Option<u32>, MediaError> {
        let nanoseconds: u128 = match (self.frame.duration, self.default_duration) {
            (Some(block), _) => u128::from(block)
                .checked_mul(u128::from(self.scale))
                .ok_or_else(|| self.duration_overflow())?,
            (None, Some(default)) => u128::from(default),
            (None, None) => return Ok(None),
        };
        let ticks = nanoseconds
            .checked_mul(u128::from(self.timescale))
            .ok_or_else(|| self.duration_overflow())?
            / 1_000_000_000;
        let ticks = u64::try_from(ticks).map_err(|_| self.duration_overflow())?;
        // A duration that fits a `u32` of output ticks is stated; a wider one
        // is refused the same way the fragment writer refuses it (its sample
        // table counts durations as `u32`), so the verdict arrives as
        // `Malformed` rather than as a truncated number the writer would then
        // have to guess past.
        u32::try_from(ticks)
            .map(Some)
            .map_err(|_| self.duration_overflow())
    }

    fn duration_overflow(&self) -> MediaError {
        MediaError::Malformed(format!(
            "track {} states a frame duration no timeline can hold",
            self.track
        ))
    }
}

/// Milliseconds of source time from nanoseconds, saturating: only ever
/// reported to a caller as "where this stream begins", where a start time
/// beyond `u64` milliseconds of anything is a start time beyond the file.
fn source_milliseconds(nanoseconds: u128) -> u64 {
    let ms = nanoseconds / 1_000_000;
    u64::try_from(ms).unwrap_or(u64::MAX)
}

/// What [`MkvSource::filter_frame`] decided about one frame.
enum FrameAction {
    /// Pass it over, counting it against [`MAX_SKIPPED_FRAMES`].
    Drop,
    /// Emit it, with the sync flag the fragments will carry.
    Emit(bool),
}

/// Open one Matroska track for fragmented MP4.
///
/// `track` is the container's own `TrackNumber`; a track number above what a
/// `u32` holds names a track this crate cannot open (`probe` refuses such a
/// track the same way). `start_ms` is source time, and a seek lands forward,
/// never back: the demuxer's walk gives the first frame at or after the
/// target, and video's keyframe rule starts the stream at the first sync
/// point at or after `start_ms` - the latest-at-or-before landing players
/// sometimes describe is not what this does, and `start_ms()` reports where
/// the samples really began. Audio needs no sync clamp: every audio frame is
/// independently decodable, and the frames before `start_ms` are dropped one
/// by one, so its samples start at or after it too. Zero opens from the
/// track's first sample with no seek at all.
///
/// The stream's [`mime`](FragmentStream::mime) is the fragmented MP4
/// `Content-Type` for this track - `video/mp4` or `audio/mp4` plus the
/// track's RFC 6381 codec string - because the bytes it produces are
/// fragmented MP4 whatever the source container was.
///
/// The reader is wrapped in [`CappedLen`] with [`ELEMENT_CAP`], so the header
/// parse and every frame read see a stream that refuses to claim more room
/// than a file is worth; the [`ReadBudget`] starts at exactly what a header is
/// worth ([`ELEMENT_CAP`] bytes, [`MAX_HEADER_OPS`](crate::MAX_HEADER_OPS)
/// calls) and is raised once the header has been accepted - media data is
/// bounded by the fragment caps and by [`MAX_SKIPPED_FRAMES`], not by header
/// paranoia, but a single element's declared size stays capped the whole time.
///
/// The header parse itself is `open_matroska`, the same guarded, budgeted
/// walk [`probe`](crate::probe::probe) uses; every frame read and the seek are
/// guarded the same way, because the demuxer's own arithmetic can panic on
/// junk input (see `MkvSource`'s module docs, this file's header) and the
/// plan's rule is that hostile input changes the answer, never the control
/// flow.
///
/// # Errors
///
/// [`MediaError::Unsupported`] for a track number the file does not carry, a
/// track type that holds no media (a logo track has no samples to remux), a
/// codec or subtitle track [`sample_entry`](crate::sample_entry) cannot write,
/// an AC-3 or E-AC-3 track whose first frame the walk cannot find, and content
/// encodings this crate does not undo (see `MkvSource`'s module docs).
/// [`MediaError::Malformed`] for a header that does not parse, a file that
/// ends inside a header element, a demuxer panic, a `TimestampScale` or start
/// time no timeline can hold, a frame timestamp or duration too wide for the
/// track's timeline, and a walk of [`MAX_SKIPPED_FRAMES`] frames that produced
/// nothing. A reader that fails stays [`MediaError::Io`] - a torrent piece
/// that has not arrived is not a verdict about the file.
///
/// # Panics
///
/// None. The demuxer's panics are caught, unwound, and reported.
pub fn open_track<R: Read + Seek + Send + 'static>(
    reader: R,
    track: u32,
    start_ms: u64,
) -> Result<FragmentStream, MediaError> {
    let source = open_source(reader, u64::from(track), start_ms)?;
    // Read the mime out of the source before the box eats the name.
    let mime = source.mime.clone();
    FragmentStream::new(Box::new(source), mime)
}

/// The same open as [`open_track`], unbuilt into a stream: the concrete
/// source, for this module's tests and for the box `open_track` puts it in.
/// `track` is the container's full `TrackNumber` here; the public wrapper
/// narrows a `u32` to it.
pub(crate) fn open_source<R: Read + Seek>(
    reader: R,
    track: u64,
    start_ms: u64,
) -> Result<MkvSource<R>, MediaError> {
    // One element's bytes and MAX_HEADER_OPS calls is exactly what a header is
    // worth, the same budget `probe` pays; it is raised below, once the header
    // parse that spends it has succeeded.
    let budget = ReadBudget::new(ELEMENT_CAP);
    let mut file = open_matroska(reader, budget.clone())?;
    let scale = file.info().timestamp_scale().get();
    let (start_units, start_ns) = start_in_scale_units(start_ms, scale)?;
    let entry = file
        .tracks()
        .iter()
        .find(|entry| entry.track_number().get() == track)
        .ok_or_else(|| {
            MediaError::Unsupported(format!("track {track} is not one of this file's tracks"))
        })?;
    let kind = matroska_kind(entry).ok_or_else(|| {
        MediaError::Unsupported(format!(
            "track {track} is a {:?} track, which carries no media",
            entry.track_type()
        ))
    })?;
    let (codec, mime_codecs) =
        crate::codec::matroska_codec(entry.codec_id(), entry.codec_private());
    if kind == TrackKind::Subtitle || !codec.remuxable() {
        return Err(MediaError::Unsupported(format!(
            "track {track} ({}) has no fragmented MP4 sample entry fbtt writes",
            codec.name()
        )));
    }
    let strip_prefix = header_stripping_prefix(entry)?;
    let timescale = match kind {
        TrackKind::Video => VIDEO_TIMESCALE,
        TrackKind::Audio => matroska_audio_rate(entry),
        TrackKind::Subtitle => unreachable!("subtitle tracks were refused above"),
    };
    // Past the header now: everything below this line reads media data, which
    // is bounded by MAX_SKIPPED_FRAMES and the fragment caps rather than by
    // the header budget - and `CappedLen` still caps every element it meets.
    budget.lift();
    let hunt_first_frame = matches!(codec, Codec::Ac3 | Codec::Eac3);
    let mut slot = Frame::default();
    if start_ms > 0 {
        guarded_seek(&mut file, start_units)?;
    }
    let pending = if hunt_first_frame {
        hunt_first_frame_of_track(&mut file, &mut slot, track)?
    } else {
        None
    };
    // Re-find the track entry: the seek and the hunt borrowed the file
    // mutably, and the entry borrowed from it earlier died with them. Looking
    // it up again costs a scan of a Vec that is already in memory.
    let entry = file
        .tracks()
        .iter()
        .find(|entry| entry.track_number().get() == track)
        .expect("the track was found before the seek, and seeking changes frames, not tracks");
    let audio_opts: Option<(u32, u16)> = match kind {
        TrackKind::Audio => Some((matroska_audio_rate(entry), channels_of(entry))),
        _ => None,
    };
    let (width, height) = match kind {
        TrackKind::Video => video_dims(entry),
        _ => (0, 0),
    };
    let video_opts: Option<(u16, u16)> = match kind {
        TrackKind::Video => Some((width, height)),
        _ => None,
    };
    let info = track_identity(entry, track, kind, codec.clone(), &mime_codecs);
    let sample_entry = crate::sample_entry(
        &info,
        entry.codec_private(),
        pending.as_ref().map(|frame| frame.data.as_slice()),
        audio_opts,
        video_opts,
    )?;
    let default_duration = entry.default_duration().map(|ns| ns.get());
    Ok(MkvSource {
        file,
        config: TrackConfig {
            timescale,
            kind,
            codec,
            sample_entry,
            width,
            height,
        },
        mime: mp4_mime(kind, &mime_codecs),
        track,
        scale,
        timescale,
        start_ns,
        strip_prefix,
        pending,
        skipped: 0,
        reported_start_ms: start_ms,
        anchored: false,
        emitted: false,
        failed: None,
        default_duration,
        run_timestamp: 0,
        run_len: 0,
        frame: slot,
    })
}

/// The `Content-Type` a fragmented MP4 stream of this track is served under:
/// the MP4 container for the track's kind, plus the track's RFC 6381 codec
/// string - `video/mp4; codecs="avc1.64000A"`, `audio/mp4; codecs="opus"`.
/// The codec string is not worked out twice: Task 1's `matroska_codec`
/// already computed it when the track was identified, and this pastes that
/// exact entry in. A codec the probe gave no RFC 6381 string gets the bare
/// container type, which a `SourceBuffer` still accepts for the init segment
/// it is about to parse.
fn mp4_mime(kind: TrackKind, mime_codecs: &[String]) -> String {
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

/// The bytes the container stripped off every block, in prepend order, or
/// `Unsupported` for encodings this crate cannot undo.
///
/// `matroska-demuxer` hands block bytes back exactly as stored. Per the
/// Matroska specification, `Stripping` removes bytes from the front of each
/// frame - a track whose blocks are missing a header the decoder still needs -
/// and `ContentCompSettings` holds the bytes that were removed, so prepending
/// them restores the decoder's stream, byte for byte, with no decompression
/// anywhere. Encodings apply in `ContentEncodingOrder` sequence, and the last
/// one applied removed bytes from a stream that already had the earlier ones'
/// bytes removed from it, so the prepend runs in ascending order of that field.
///
/// A scope other than 1 is refused: scope 1 is "whole frames", which is the
/// only scope whose removal this function can put back, and the specification
/// marks the frame-scoped use as the one that applies to the codecs fbtt
/// remuxes. A zlib, bzlib or lzo1x compaction means the blocks are compressed
/// and this crate does not inflate them (a decoder expecting its own header
/// would get a deflate stream instead), and a real encryption algorithm means
/// the blocks are ciphertext. `ContentEncAlgo::NotEncrypted` is signed-only:
/// nothing is encrypted about the bytes, so it passes through.
fn header_stripping_prefix(entry: &TrackEntry) -> Result<Vec<u8>, MediaError> {
    let Some(encodings) = entry.content_encodings() else {
        return Ok(Vec::new());
    };
    let track = entry.track_number().get();
    let mut ordered: Vec<&ContentEncoding> = encodings.iter().collect();
    ordered.sort_by_key(|encoding| encoding.order());
    let mut prefix = Vec::new();
    for encoding in ordered {
        match encoding.encoding() {
            ContentEncodingValue::Compression(compression) => {
                if compression.algo() != ContentCompAlgo::Stripping {
                    return Err(MediaError::Unsupported(format!(
                        "track {track} compacts blocks with {:?}, which fbtt does not undo",
                        compression.algo()
                    )));
                }
                if encoding.scope() != 1 {
                    return Err(MediaError::Unsupported(format!(
                        "track {track} strips with scope {}, which fbtt cannot put back frame by frame",
                        encoding.scope()
                    )));
                }
                prefix.extend_from_slice(compression.settings().unwrap_or(&[]));
            }
            ContentEncodingValue::Encryption(encryption) => {
                if encryption.algo() != ContentEncAlgo::NotEncrypted {
                    return Err(MediaError::Unsupported(format!(
                        "track {track} is encrypted with {:?}, which fbtt does not undo",
                        encryption.algo()
                    )));
                }
            }
            ContentEncodingValue::Unknown => {
                return Err(MediaError::Unsupported(format!(
                    "track {track} carries a content encoding type fbtt cannot read"
                )));
            }
        }
    }
    Ok(prefix)
}

/// Walk the frame stream to this track's first frame, for a sample entry that
/// is built from an actual frame (the Dolby codecs read sample rate and
/// channel layout out of a syncframe header). Other tracks' frames are passed
/// over against [`MAX_SKIPPED_FRAMES`]; at the end of the stream the track
/// simply has no first frame, and `sample_entry` reports that as `Unsupported`
/// with the codec's own words.
fn hunt_first_frame_of_track<R: Read + Seek>(
    file: &mut MatroskaFile<CappedLen<R>>,
    slot: &mut Frame,
    track: u64,
) -> Result<Option<PendingFrame>, MediaError> {
    let mut skipped = 0usize;
    loop {
        if !guarded_next(file, slot, "frames while looking for the first frame")? {
            return Ok(None);
        }
        // A slot still claiming track 0 was filled by nothing - the demuxer
        // reports an empty `BlockGroup` without touching it, see
        // [`guarded_next`] - and no frame means this walk keeps walking.
        if slot.track != track {
            skipped += 1;
            if skipped > MAX_SKIPPED_FRAMES {
                return Err(MediaError::Unsupported(format!(
                    "track {track} has no frame within {MAX_SKIPPED_FRAMES} frames"
                )));
            }
            continue;
        }
        return Ok(Some(PendingFrame {
            timestamp: slot.timestamp,
            keyframe: slot.is_keyframe,
            duration: slot.duration,
            data: slot.data.clone(),
        }));
    }
}

/// The video sample entry's dimensions: the encoded pixel size.
///
/// Display size is the player's business (the page maps it per the plan), and
/// `tkhd` and the sample entry want the encoded size, so
/// `PixelWidth`/`PixelHeight` it is - 0 for 0 when the track states neither,
/// which is what [`sample_entry`](crate::sample_entry) documents.
fn video_dims(entry: &TrackEntry) -> (u16, u16) {
    let Some(video) = entry.video() else {
        return (0, 0);
    };
    let width = u16::try_from(video.pixel_width().get()).unwrap_or(0);
    let height = u16::try_from(video.pixel_height().get()).unwrap_or(0);
    (width, height)
}

/// The `Audio` box's channel count; a track that states none has the
/// specification's default of one, which is also what the demuxer reports.
fn channels_of(entry: &TrackEntry) -> u16 {
    let channels = entry.audio().map(|audio| audio.channels().get());
    u16::try_from(channels.unwrap_or(1)).unwrap_or(1)
}

/// The [`TrackInfo`] [`sample_entry`](crate::sample_entry) takes: the track's
/// identity. The player-facing fields (language, name, flags) ride along from
/// the same entry the probe reads, because the sample entry ignores them and
/// the cost is three `Option<String>` clones once per open.
fn track_identity(
    entry: &TrackEntry,
    track: u64,
    kind: TrackKind,
    codec: Codec,
    mime_codecs: &[String],
) -> TrackInfo {
    TrackInfo {
        id: u32::try_from(track).unwrap_or(0),
        kind,
        codec,
        mime_codecs: mime_codecs.to_vec(),
        language: entry
            .language_bcp47()
            .or_else(|| entry.language())
            .filter(|language| *language != "und")
            .map(str::to_string),
        name: entry.name().map(str::to_string),
        default: entry.flag_default(),
        forced: entry.flag_forced(),
    }
}

impl<R: Read + Seek + Send> TrackSource for MkvSource<R> {
    fn config(&self) -> &TrackConfig {
        &self.config
    }

    fn next_sample(&mut self) -> SourceResult<Option<Sample>> {
        if let Some(error) = self.poisoned() {
            return Err(error);
        }
        let result = self.next_emittable();
        match result {
            Ok(sample) => Ok(sample),
            Err(error) => {
                let error = SourceError::from_media(error);
                self.poison(&error);
                Err(error)
            }
        }
    }

    fn start_ms(&self) -> u64 {
        self.reported_start_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Minimal EBML assembly: the same rules as `tests/remux.rs`'s helpers
    /// (a 1-byte or 5-byte size prefix ahead of the body, the body after the
    /// ID), this file's share of its elements. The full files live in
    /// `tests/remux.rs`; this one input is assembled here because this test is
    /// a unit test only in order to see a `BlockDuration`.
    fn vint(size: u64) -> Vec<u8> {
        if size <= 126 {
            return vec![0x80 | u8::try_from(size).expect("a size of 126 or less")];
        }
        let size = u32::try_from(size).expect("an element under 4 GiB");
        let mut out = vec![0x08];
        out.extend_from_slice(&size.to_be_bytes());
        out
    }

    fn ebml(id: &[u8], body: &[u8]) -> Vec<u8> {
        let declared = u64::try_from(body.len()).expect("an element under 4 GiB");
        let mut out = Vec::with_capacity(id.len() + 5 + body.len());
        out.extend_from_slice(id);
        out.extend_from_slice(&vint(declared));
        out.extend_from_slice(body);
        out
    }

    /// An unsigned-integer element in its shortest valid width: EBML allows
    /// one to eight value bytes and the demuxer reads any of them.
    fn uint(id: &[u8], value: u64) -> Vec<u8> {
        let bytes = value.to_be_bytes();
        let first = bytes
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or(bytes.len() - 1);
        ebml(id, &bytes[first..])
    }

    fn ebml_header() -> Vec<u8> {
        ebml(
            &[0x1A, 0x45, 0xDF, 0xA3],
            &[
                ebml(&[0x42, 0x82], b"matroska"),
                ebml(&[0x42, 0x87], &[1]),
                ebml(&[0x42, 0x85], &[1]),
            ]
            .concat(),
        )
    }

    /// An `AVCDecoderConfigurationRecord`: H.264 without a CodecPrivate is
    /// `Unsupported`, so the track needs one.
    fn avcc() -> Vec<u8> {
        vec![
            0x01, 0x64, 0x0C, 0x15, 0xFF, // version, profile, compat, level
            0xE1, 0x00, 0x05, 0x67, 0x64, 0x0C, 0x15, 0xFF, // one 5-byte SPS
            0x01, 0x00, 0x02, 0x68, 0xEB, // one 2-byte PPS
        ]
    }

    #[test]
    fn remux_blockgroup_first_frame_counts_as_sync() {
        // A `BlockGroup` carries no keyframe flag (the demuxer reads no
        // `ReferenceBlock`, so `is_keyframe` is None): the crate's documented
        // reading is that the first frame of a video track with no flags is
        // the sync point a stream starts from, and every later unflagged frame
        // is non-sync. And the `BlockGroup`'s `BlockDuration` - counted in the
        // file's `TimestampScale` units - becomes the sample's stated duration
        // in track ticks.
        //
        // This is a unit test rather than a `tests/remux.rs` one because
        // `BlockDuration` reaches a `Sample` and nothing public: on the wire,
        // only a stream's last sample carries a stated duration (earlier
        // samples' durations are the gaps to the next decode time), so a test
        // through `open_track` could never ask a stated duration about all
        // but the tail.
        let tracks = ebml(
            &[0x16, 0x54, 0xAE, 0x6B],
            &[ebml(
                &[0xAE],
                &[
                    uint(&[0xD7], 1),                  // TrackNumber
                    uint(&[0x73, 0xC5], 11),           // TrackUID
                    uint(&[0x83], 1),                  // TrackType: video
                    uint(&[0x88], 1),                  // FlagDefault
                    ebml(&[0x86], b"V_MPEG4/ISO/AVC"), // CodecID
                    ebml(&[0x63, 0xA2], &avcc()),      // CodecPrivate
                    ebml(
                        &[0xE0], // Video
                        &[uint(&[0xB0], 640), uint(&[0xBA], 360)].concat(),
                    ),
                ]
                .concat(),
            )]
            .concat(),
        );
        let info = ebml(
            &[0x15, 0x49, 0xA9, 0x66],
            &[
                ebml(&[0x4D, 0x80], b"fbtt"),         // MuxingApp
                ebml(&[0x57, 0x41], b"fbtt"),         // WritingApp
                uint(&[0x2A, 0xD7, 0xB1], 1_000_000), // TimestampScale: 1 ms
            ]
            .concat(),
        );
        let block_group = |offset: i16, duration: u64, payload: &[u8]| -> Vec<u8> {
            let block = ebml(
                &[0xA1], // Block: track 1 (one-byte VINT), timecode, no flags
                &[
                    vec![0x81],
                    offset.to_be_bytes().to_vec(),
                    vec![0x00],
                    payload.to_vec(),
                ]
                .concat(),
            );
            ebml(&[0xA0], &[block, uint(&[0x9B], duration)].concat())
        };
        let cluster = ebml(
            &[0x1F, 0x43, 0xB6, 0x75],
            &[
                uint(&[0xE7], 5_000),                     // 5 s timecode
                block_group(0, 45, &[0x41, 0x01, 0x02]),  // 45 ms duration
                block_group(20, 45, &[0x41, 0x01, 0x03]), // 20 ms later
            ]
            .concat(),
        );
        let file = [
            ebml_header(),
            ebml(
                &[0x18, 0x53, 0x80, 0x67], // Segment: no SeekHead, no Cues
                &[tracks, info, cluster].concat(),
            ),
        ]
        .concat();

        let mut source = open_source(Cursor::new(file), 1, 0).expect("opens");
        let first = source.next_sample().expect("no error").expect("a sample");
        assert!(
            first.keyframe,
            "the unflagged first frame is the sync point"
        );
        assert_eq!(
            first.pts,
            5_000 * 90,
            "cluster timecode 5000 ms on the 90 kHz timeline"
        );
        assert_eq!(
            first.duration,
            Some(4_050),
            "BlockDuration 45 ms of TimestampScale units is 4050 ticks at 90 kHz"
        );
        let second = source.next_sample().expect("no error").expect("a sample");
        assert!(!second.keyframe, "a later unflagged frame is non-sync");
        assert_eq!(second.pts, 5_020 * 90, "the second frame follows the first");
    }
}
