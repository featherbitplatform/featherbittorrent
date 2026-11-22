//! One Matroska track, remuxed into fragmented MP4 from any start time.
//!
//! [`open_track`] takes a reader, a track number and a start time, and hands
//! back an [`MkvSource`]: a [`TrackSource`] that yields exactly that track's
//! samples, in the order the container stores them, starting at the first sync
//! point at or before the requested time. The samples are the container's own
//! bytes - this crate copies, it does not decode - plus the timestamps, keyframe
//! flags and durations the container states, in the timescale
//! [`FragmentStream`](crate::FragmentStream) puts on the wire.
//!
//! Everything here treats the file as hostile. `matroska-demuxer` 0.8.1 panics
//! on some of its own arithmetic when the input is junk - a cue's relative
//! position added to a cluster offset, a block timestamp added to a cluster
//! timestamp - and it reads every frame of every track through one stream, so
//! the walk to a track's samples, its seek points and its first frame all run
//! under a [`catch_unwind`], with [`CappedLen`] and [`ReadBudget`] holding the
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
use crate::source::{Sample, SourceError, SourceResult, TrackConfig, TrackSource};
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
/// `R` stays the caller's own reader inside [`CappedLen`]: a `Box<dyn Read +
/// Seek>` would erase a torrent reader's timeouts into the reader trait's
/// own `io::Error`, and those have to survive to the API as retryable.
pub struct MkvSource<R: Read + Seek> {
    /// The demuxer, past its header and (when the start time asked for it)
    /// sought to its first cluster.
    file: MatroskaFile<CappedLen<R>>,
    /// What [`TrackSource::config`] answers.
    config: TrackConfig,
    /// `Content-Type` for a stream of this source's fragments.
    mime: &'static str,
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
    /// The message of the first failure, repeated to every later request.
    failed: Option<String>,
    /// The track's `DefaultDuration` in nanoseconds, the duration a frame gets
    /// when its block states none.
    default_duration: Option<u64>,
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
fn guarded_next<R: Read + Seek>(
    file: &mut MatroskaFile<CappedLen<R>>,
    slot: &mut Frame,
    what: &str,
) -> Result<bool, MediaError> {
    match catch_unwind(AssertUnwindSafe(|| file.next_frame(slot))) {
        Ok(Ok(got)) => Ok(got),
        Ok(Err(error)) => Err(matroska_error(error)),
        Err(_) => Err(MediaError::Malformed(format!(
            "matroska demuxer panicked reading {what}"
        ))),
    }
}

/// Seek the demuxer under a panic guard.
fn guarded_seek<R: Read + Seek>(
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
    // Truncating: the seek lands at or before the requested time, and the
    // sync-point clamp moves it further back, never forward.
    let units = start_ns / u128::from(scale);
    let units = u64::try_from(units).map_err(|_| {
        MediaError::Malformed(format!(
            "start time {start_ms} ms is past what TimestampScale {scale} can count"
        ))
    })?;
    Ok((units, start_ns))
}

impl<R: Read + Seek> MkvSource<R> {
    /// The first error, if there is one, as the verdict every later call gives.
    fn poisoned(&self) -> Option<SourceError> {
        self.failed
            .as_deref()
            .map(|reason| SourceError::Malformed(reason.to_string()))
    }

    fn poison(&mut self, error: &SourceError) {
        let reason = error.to_string();
        self.failed.get_or_insert(reason);
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
    /// translated into output ticks. `Ok(None)` is the end of the stream, and
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
                    keyframe,
                    with_header,
                )?));
            }
            return Ok(Some(self.sample_with_timestamp(timestamp, keyframe, data)?));
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
    /// Video ignores `start_ns` on purpose: the seek landed on a sync point at
    /// or before the start time because the frames between it and the start
    /// time are reference material the decoder needs, so the stream starts
    /// where a decoder can, and the page trims the difference. Audio filters
    /// by start time, because every audio frame is independently decodable and
    /// one before the start time would play a sound the viewer asked to skip.
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

    /// A [`Sample`] from one container frame: PTS in output ticks, the
    /// container's stated duration (its own `BlockDuration`, or the track's
    /// `DefaultDuration`) as a stated duration in output ticks, and the bytes.
    ///
    /// A PTS or duration too wide for the timeline is `Malformed`: a frame at
    /// 2^63 ticks on a 90 kHz timeline is a claim about the file no MP4
    /// timeline can carry.
    fn sample_with_timestamp(
        &mut self,
        timestamp: u64,
        keyframe: bool,
        data: Vec<u8>,
    ) -> Result<Sample, MediaError> {
        let pts = self.to_ticks(timestamp)?;
        let duration = self.frame_duration()?;
        if !self.emitted {
            self.emitted = true;
            self.reported_start_ms = source_milliseconds(timestamp, self.scale);
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

    /// Container timestamp units to output ticks: scale units to nanoseconds
    /// to seconds to ticks, all in `u128` so a junk timestamp is refused
    /// rather than wrapped.
    fn to_ticks(&self, units: u64) -> Result<i64, MediaError> {
        let nanoseconds = u128::from(units)
            .checked_mul(u128::from(self.scale))
            .ok_or_else(|| self.time_overflow())?;
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

/// Milliseconds of source time from container units, saturating: only ever
/// reported to a caller as "where this stream begins", where a start time
/// beyond `u64` milliseconds of anything is a start time beyond the file.
fn source_milliseconds(units: u64, scale: u64) -> u64 {
    let ns = u128::from(units) * u128::from(scale);
    let ms = ns / 1_000_000;
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
/// `track` is the container's own `TrackNumber`. `start_ms` is source time:
/// the source seeks to the latest sync point at or before it - video seeks to
/// a keyframe, and the samples start there even when that is before `start_ms`,
/// because frames between the keyframe and `start_ms` are the decoder's
/// reference material; audio states no keyframes, so every audio frame passes,
/// and its samples start at or after `start_ms`. Zero opens from the track's
/// first sample with no seek at all.
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
/// junk input (see [`MkvSource`]'s field docs) and the plan's rule is that
/// hostile input changes the answer, never the control flow.
///
/// # Errors
///
/// [`MediaError::Unsupported`] for a track number the file does not carry, a
/// track type that holds no media (a logo track has no samples to remux), a
/// codec or subtitle track [`sample_entry`](crate::sample_entry) cannot write,
/// an AC-3 or E-AC-3 track whose first frame the walk cannot find, and content
/// encodings this crate does not undo (see [`MkvSource`]'s module docs]).
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
    let is_webm = matches!(file.ebml_header().doc_type(), "webm");
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
        mime: if is_webm {
            "video/webm"
        } else {
            "video/x-matroska"
        },
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
        frame: slot,
    })
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

    /// Re-seek the source. `start_ms` means what it means in
    /// [`open_track`], and a Dolby track's held first frame (its sample entry
    /// needed it) is dropped and not hunted again: its sample entry is built,
    /// and re-hunting it would walk frames the new seek just skipped all over
    /// again.
    ///
    /// Seeking is the demuxer's cue-table walk (or a linear cluster scan),
    /// under the same panic guard as the initial seek: a cue table built from
    /// junk positions is hostile input too, and it gets the same answer.
    fn seek(&mut self, start_ms: u64) -> SourceResult<()> {
        if let Some(error) = self.poisoned() {
            return Err(error);
        }
        let target = match start_in_scale_units(start_ms, self.scale) {
            Ok((units, start_ns)) => {
                self.start_ns = start_ns;
                self.reported_start_ms = start_ms;
                units
            }
            Err(error) => {
                let error = SourceError::from_media(error);
                self.poison(&error);
                return Err(error);
            }
        };
        match guarded_seek(&mut self.file, target) {
            Ok(()) => {
                self.skipped = 0;
                self.anchored = false;
                self.pending = None;
                Ok(())
            }
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

    fn mime(&self) -> &'static str {
        self.mime
    }
}
