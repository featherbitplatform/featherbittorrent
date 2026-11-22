//! One MP4 track, remuxed into fragmented MP4 from any start time.
//!
//! [`open_source`] takes a reader, a track number and a start time, and hands back an
//! [`Mp4Source`]: a [`TrackSource`](crate::source::TrackSource) that yields exactly that track's
//! samples, in the order the container stores them, starting where the track's own tables say a
//! seek lands - the last sync sample at or before the requested time for video, the first sample
//! at or after it for audio. The samples are the file's own bytes (this crate copies, it does not
//! decode) plus the timestamps, sync flags and durations the tables state, in the timescale the
//! track's own `mdhd` gives it.
//!
//! `moov` may sit at either end of the file. The box walk that finds it (`read_moov`, shared with
//! [`probe`](crate::probe::probe)) skips `ftyp`, `free`, `mdat` and anything else by seeking, so a
//! file with its `moov` behind 40 GB of media data pays the same few hundred bytes as one with the
//! table first, and that walk plus [`sized_moov_header`](crate::probe::sized_moov_header) refusing
//! a `moov` longer than [`MOOV_CAP`](crate::MOOV_CAP) is the crate's only moov cap.
//!
//! # What is bounded
//!
//! Every sample table is walked as the runs the container states them in. An `stts` entry is
//! `(sample_count, sample_delta)`, an `stsc` entry is `(first_chunk, samples_per_chunk)`, and the
//! walk keeps a [`Cursor`] - a handful of integers - into each of them, so a 189 sample audio track
//! with one run costs one entry and a 20 million sample track costs the arrays `mp4-atom` already
//! allocated for the container's own entries and nothing this module adds. Nothing here is expanded
//! per sample: no `Vec` of samples, no `Vec` of times, no second copy of the sizes.
//!
//! [`MAX_MP4_SAMPLES`] is what a count is trusted up to, and [`ELEMENT_CAP`] is what one sample's
//! declared size is trusted up to, because the sample's bytes are read into a buffer that size.
//!
//! # What is not read
//!
//! `edts`/`elst` edit lists are ignored. An edit list maps a track's media times onto timeline
//! times, and this remuxer puts samples on the wire at their media times, so an edited track
//! streams from its own timeline rather than the presentation's. On
//! `tests/fixtures/h264-aac2-movtext.mp4` that is visible in the numbers: ffmpeg gives every trak
//! an `elst` with `media_time` 2048 (video) or 1024 (audio) ticks, and the video track's own tables
//! put its first sync sample at presentation time 2048 ticks - 200 ms, where ffprobe, which applies
//! the edit list, and `tests/fixtures/expected.json` with it, report 0 ms. The MP4 tests therefore
//! ask the fixture's tables what the answer should be rather than reading `expected.json`.
//!
//! `mvex` (and a top level `moof` on the way to the `moov`) is `Unsupported`: a fragmented source
//! file keeps its sample tables in its fragments, so reading this file's `moov` would describe a
//! track with no samples, and following fragments would be a second reader. `sidx`, `mfra`, `udta`
//! and everything else outside the track's `stbl` are simply not looked at.
//!
//! # Hostile input
//!
//! The tables are cross-checked against each other at the open - `stts`, `stsz`, `stsc`, `ctts`,
//! `stss` and the chunk offsets all count the same samples a different way, and a file whose tables
//! disagree about that is a file whose samples cannot be placed on a timeline - and then walked
//! once, which is where an offset that overflows a byte address, or a time no i64 timeline holds,
//! turns up. A sample past what the stream holds is [`SourceError`](crate::source::SourceError)'s
//! `Io` and not a verdict about the file: a torrent that has the header and not the tail is a
//! stream that has not finished arriving, so a `moov` at byte 40 and an `mdat` still on its way is
//! exactly what this module expects to be handed.

use crate::codec::TrackKind;
use crate::probe;
use crate::source::{
    Sample, SourceError, SourceResult, StoredError, TrackConfig, TrackSource, mp4_mime,
};
use crate::{ELEMENT_CAP, MAX_MP4_SAMPLES, MediaError};
use mp4_atom::{CttsEntry, Stbl, StscEntry, StszSamples, SttsEntry};
use std::io::{Read, Seek, SeekFrom};

/// One track's sample tables, as the run-per-entry arrays the container states them in.
///
/// These are moved out of the track's `stbl` rather than borrowed from it: the `Trak` they came
/// from is dropped at the open, and with it every other track's tables and the boxes around these,
/// so what stays alive for the length of a stream is one track's tables and nothing else.
struct Tables {
    /// The track's own `track_ID`: every verdict these tables give is stated about it, and the walk
    /// has nothing else to name a track by.
    track: u32,
    /// Ticks per second, from this track's `mdhd`: the timeline the fragments carry.
    timescale: u32,
    /// `stts`: how many samples each `sample_delta` covers. Never empty, and no entry covers none.
    stts: Vec<SttsEntry>,
    /// `ctts`: how many samples share each composition offset. Empty for a track with no `ctts`.
    ctts: Vec<CttsEntry>,
    /// `stss`: the sync samples' 1-based numbers, ascending, no repeats. `None` when the track has
    /// no `stss`, which the specification reads as every sample being a sync sample.
    stss: Option<Vec<u32>>,
    /// `stsc`: which chunks carry how many samples. Never empty, and no entry carries none.
    stsc: Vec<StscEntry>,
    /// `stsz`: the sample sizes.
    sizes: Sizes,
    /// `stco` or `co64`: where each chunk starts. Never empty.
    offsets: Offsets,
    /// How many samples the track holds, from `stsz`. Every other table's total is checked against
    /// it at the open, so a walk of the tables and a walk of this number cannot disagree.
    count: u64,
}

/// `stsz`, both of its shapes.
enum Sizes {
    /// One size for all `count` samples: the field `stsz` puts where the sizes would go.
    Identical { size: u32 },
    /// One size per sample, in order. `mp4-atom` reads exactly as many entries as the box declares
    /// and fails the whole `moov` if the bytes run out, so this `Vec`'s length *is* the count.
    Different(Vec<u32>),
}

/// The chunk offset table, either of the two that can hold it.
enum Offsets {
    /// `stco`: 32 bit chunk offsets.
    Stco(Vec<u32>),
    /// `co64`: 64 bit chunk offsets, for a file whose media data runs past 4 GB.
    Co64(Vec<u64>),
}

impl Offsets {
    /// Where chunk `chunk` starts, in bytes.
    fn at(&self, chunk: u64) -> u64 {
        let index = usize::try_from(chunk).expect("a chunk index below the sample count");
        match self {
            Offsets::Stco(entries) => u64::from(entries[index]),
            Offsets::Co64(entries) => entries[index],
        }
    }

    /// How many chunks the table describes, which is how many the last `stsc` group can run to.
    fn chunks(&self) -> u64 {
        match self {
            Offsets::Stco(entries) => entries.len() as u64,
            Offsets::Co64(entries) => entries.len() as u64,
        }
    }
}

/// One sample's coordinates, read out of the tables without touching the file.
struct Meta {
    /// Where the sample's bytes are, and how many they are.
    offset: u64,
    size: u32,
    /// Decode time and presentation time (decode time plus this sample's `ctts` offset) in ticks,
    /// and the duration `stts` states for it.
    dts: i128,
    pts: i128,
    duration: u32,
    /// Whether a decoder can start here, which is `stss`'s whole purpose.
    sync: bool,
}

/// Where a walk of [`Tables`] has got to: the next sample's index and decode time, plus one cursor
/// per table run.
///
/// Every field is an integer, so `Clone` is a handful of copies - which is what lets the seek walk
/// remember a landing candidate without keeping a second set of tables, and lets a table walk be
/// reversed by copying a cursor instead of re-reading the file.
#[derive(Clone)]
struct Cursor {
    /// Samples consumed so far, which is the index of the next one.
    index: u64,
    /// The next sample's decode time, in ticks: the sum of the deltas of everything consumed.
    dts: i128,
    /// `stts`: the run being consumed, and how many of its samples are left.
    stts: usize,
    stts_left: u32,
    /// `ctts`: the run being consumed, how many of its samples are left, and the offset they share.
    /// A track with no `ctts` leaves this at zero offset and zero left, which is what the track
    /// means: its samples are presented when they are decoded.
    ctts: usize,
    ctts_left: u32,
    ctts_value: i64,
    /// `stsc` and the chunk table together: the group being consumed, how many chunks of it are
    /// left, how many samples of the current chunk are left to read, the current chunk's number and
    /// how many of its bytes have been consumed.
    stsc: usize,
    group_chunks_left: u64,
    in_chunk_left: u32,
    chunk: u64,
    chunk_bytes: u64,
    /// `stss`: the position of the next sync sample number, past every entry that names an already
    /// consumed sample.
    stss: usize,
}

impl Cursor {
    /// A walk that has consumed nothing, or the verdict its first `stsc` group makes impossible.
    fn start(tables: &Tables) -> Result<Cursor, MediaError> {
        let stts = tables
            .stts
            .first()
            .expect("a validated track has at least one stts entry");
        let ctts = tables.ctts.first();
        let stsc = tables
            .stsc
            .first()
            .expect("a validated track has at least one stsc entry");
        Ok(Cursor {
            index: 0,
            dts: 0,
            stts: 0,
            stts_left: stts.sample_count,
            ctts: 0,
            ctts_left: ctts.map_or(0, |entry| entry.sample_count),
            ctts_value: ctts.map_or(0, |entry| entry.sample_offset),
            stsc: 0,
            group_chunks_left: chunks_in_group(tables, 0)?,
            in_chunk_left: stsc.samples_per_chunk,
            chunk: 0,
            chunk_bytes: 0,
            stss: 0,
        })
    }

    /// A walk that has consumed everything, which is the answer to a seek past the track's end.
    fn end(tables: &Tables) -> Result<Cursor, MediaError> {
        let mut cursor = Cursor::start(tables)?;
        cursor.index = tables.count;
        Ok(cursor)
    }
}

/// How many chunks `stsc` group `index` covers: up to the next group's first chunk, and to the end
/// of the offset table for the last one.
///
/// The subtraction is checked even though [`validate_chunks`] has already put the two numbers in
/// order: this function is what the walk's chunk cursor is built from, and a group whose own first
/// chunk sat past the limit it is measured against would wrap to a chunk count of 2^64 and send the
/// walk off the end of the offset table looking for chunks that were never there.
fn chunks_in_group(tables: &Tables, index: usize) -> Result<u64, MediaError> {
    let first = u64::from(tables.stsc[index].first_chunk);
    let limit = match tables.stsc.get(index + 1) {
        Some(next) => u64::from(next.first_chunk),
        None => tables.offsets.chunks() + 1,
    };
    limit.checked_sub(first).ok_or_else(|| {
        MediaError::Malformed(format!(
            "track {}'s stsc group {} starts at chunk {first}, which is not before the chunk \
             {limit} it would have to end on",
            tables.track,
            index + 1
        ))
    })
}

/// The next sample's coordinates, or `None` when the walk is past the track's last one.
///
/// Cheap and pure: everything here comes out of the two integers a sample is described by (its
/// index, and the run positions the cursor keeps) plus the chunk's running byte count. A walk that
/// is deciding whether to take a sample can look at it, keep the cursor, and hand the same sample
/// to the reader later.
///
/// One answer here can be impossible, and it is the arithmetic's fault rather than the file's
/// shape: a chunk's samples are contiguous, so this sample's offset is its chunk's offset plus the
/// bytes of it already consumed, and a file stating an offset at the top of a 64 bit address space
/// has samples whose bytes are past any address.
fn peek(tables: &Tables, at: &Cursor) -> Result<Option<Meta>, MediaError> {
    if at.index >= tables.count {
        return Ok(None);
    }
    let base = tables.offsets.at(at.chunk);
    let offset = base.checked_add(at.chunk_bytes).ok_or_else(|| {
        MediaError::Malformed(format!(
            "track {}'s sample {} sits {} bytes into chunk {}, whose offset of {base} leaves no \
             room for it in a 64 bit address space",
            tables.track,
            at.index + 1,
            at.chunk_bytes,
            at.chunk + 1
        ))
    })?;
    let size = match &tables.sizes {
        Sizes::Identical { size } => *size,
        Sizes::Different(sizes) => {
            sizes[usize::try_from(at.index).expect("a sample index below the count")]
        }
    };
    let stts = tables
        .stts
        .get(at.stts)
        .expect("stts covers exactly the samples stsz counts");
    let sync = match &tables.stss {
        // No `stss` is not "no sync samples": ISO 14496-12 says every sample of such a track is a
        // sync sample, which is what ffmpeg relies on for an all-intra track.
        None => true,
        Some(entries) => entries
            .get(at.stss)
            .is_some_and(|number| u64::from(*number) == at.index + 1),
    };
    Ok(Some(Meta {
        offset,
        size,
        dts: at.dts,
        pts: at.dts + i128::from(at.ctts_value),
        duration: stts.sample_delta,
        sync,
    }))
}

/// Consume the sample [`peek`] just described, moving every cursor on to the next one, or giving
/// the verdict [`peek`] or the next group's chunk count made impossible.
fn step(tables: &Tables, at: &mut Cursor) -> Result<(), MediaError> {
    let Some(meta) = peek(tables, at)? else {
        return Ok(());
    };
    at.index += 1;
    at.dts += i128::from(meta.duration);
    at.chunk_bytes += u64::from(meta.size);

    // The sync cursor moves with the sample cursor, on every sample rather than only where a chunk
    // or group boundary happens to fall: most tracks put one sample in each chunk, so the sync table
    // of a normal video track would freeze on its second entry forever if this advance lived below
    // the chunk advance's early return. The entries name samples in ascending order, so past the
    // sample just consumed the cursor is only ever one pass forward: it stops on the first entry
    // naming a sample still ahead, which is the sample number `peek` compares against.
    if let Some(entries) = &tables.stss {
        while at.stss < entries.len() && u64::from(entries[at.stss]) <= at.index {
            at.stss += 1;
        }
    }

    // Every run cursor steps into its next entry when its run is spent. Validation rules out the
    // empty entries that would otherwise need a loop to get past, so one step is the whole advance.
    at.stts_left -= 1;
    if at.stts_left == 0
        && let Some(next) = tables.stts.get(at.stts + 1)
    {
        at.stts += 1;
        at.stts_left = next.sample_count;
    }
    if at.ctts_left > 0 {
        at.ctts_left -= 1;
        if at.ctts_left == 0
            && let Some(next) = tables.ctts.get(at.ctts + 1)
        {
            at.ctts += 1;
            at.ctts_left = next.sample_count;
            at.ctts_value = next.sample_offset;
        }
    }

    // Samples within a chunk are contiguous, which is the whole reason a chunk table exists; the
    // byte count is what turns that into the next sample's offset. When a chunk's samples are spent
    // the walk moves to the next chunk, and when a group's chunks are spent it moves to the next
    // `stsc` group, whose chunks hold a different number of samples.
    at.in_chunk_left -= 1;
    if at.in_chunk_left > 0 {
        return Ok(());
    }
    at.chunk += 1;
    at.chunk_bytes = 0;
    at.group_chunks_left -= 1;
    if at.group_chunks_left > 0 {
        at.in_chunk_left = tables.stsc[at.stsc].samples_per_chunk;
        return Ok(());
    }
    if let Some(next) = tables.stsc.get(at.stsc + 1) {
        at.stsc += 1;
        at.in_chunk_left = next.samples_per_chunk;
        at.group_chunks_left = chunks_in_group(tables, at.stsc)?;
    }
    Ok(())
}

/// One MP4 track, opened for reading.
///
/// The reader type stays private on purpose: [`open_track`](crate::open_track) boxes this into a
/// [`FragmentStream`](crate::FragmentStream) and hands that out, because the point of a source is
/// its samples, not its type.
///
/// No buffering wraps the reader: samples are sought to, one offset each, in an order the tables
/// set (and, on a B-frame track, not the order the bytes are in). A buffer ahead of those seeks
/// would read bytes the walk then jumps over, which is the double-read the plan's accounting rule
/// is about.
pub(crate) struct Mp4Source<R: Read + Seek> {
    /// The caller's own reader, which stays the caller's type: a `Box<dyn Read + Seek>` would erase
    /// a torrent reader's timeouts into the reader trait's own `io::Error`, and those have to
    /// survive to the API as retryable.
    reader: R,
    /// What [`TrackSource::config`] answers.
    config: TrackConfig,
    /// `Content-Type` for a fragmented MP4 stream of this track: the container for the track's kind
    /// plus its RFC 6381 codec string, see [`mp4_mime`]. `pub(crate)` because
    /// [`open_track`](crate::open_track) reads it out before the box takes the source's name away.
    pub(crate) mime: String,
    /// The tables the samples are described by, and the only thing in the file this source reads
    /// besides the samples themselves.
    tables: Tables,
    /// Where the next sample comes from: the seek's landing, then one `step` further each time.
    cursor: Cursor,
    /// Where the samples actually begin, in milliseconds of source time: the requested start until
    /// the first sample arrives, then that sample's presentation time.
    reported_start_ms: u64,
    /// Whether any sample has been emitted yet: the first one is what `reported_start_ms` reports.
    emitted: bool,
    /// The first failure, repeated to every later request with its kind kept: a read that timed out
    /// is still timed out the second time it is asked.
    failed: Option<StoredError>,
}

impl<R: Read + Seek> std::fmt::Debug for Mp4Source<R> {
    /// Field by field, except the reader: the reader behind a source is the caller's (a file, a
    /// torrent connection), and the tables behind it are arrays a log line has no business
    /// printing in full.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mp4Source")
            .field("mime", &self.mime)
            .field("timescale", &self.config.timescale)
            .field("samples", &self.tables.count)
            .field("next", &self.cursor.index)
            .field("reported_start_ms", &self.reported_start_ms)
            .field("emitted", &self.emitted)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl<R: Read + Seek> Mp4Source<R> {
    /// The first error, if there is one, as the verdict every later call gives, its kind intact: an
    /// `Io` verdict replays as `Io`, not as a `Malformed` retelling of it.
    fn poisoned(&self) -> Option<SourceError> {
        self.failed.as_ref().map(StoredError::to_source_error)
    }

    fn poison(&mut self, error: &SourceError) {
        self.failed
            .get_or_insert_with(|| StoredError::from_source(error));
    }

    /// The sample at the cursor, its bytes included, or `None` at the end of the track.
    ///
    /// The two range checks are the ones the open's walk already made for every sample in the
    /// track, done again here rather than trusted: a source's verdicts come from what it is about
    /// to hand the writer, and a sample the writer cannot put on a timeline should be an error from
    /// this function rather than a `Malformed` from three modules away.
    fn next_sample_of_track(&mut self) -> Result<Option<Sample>, MediaError> {
        let Some(meta) = peek(&self.tables, &self.cursor)? else {
            return Ok(None);
        };
        let data = self.read_sample(&meta)?;
        let pts =
            i64::try_from(meta.pts).map_err(|_| self.time_overflow("presentation", meta.pts))?;
        let dts = i64::try_from(meta.dts).map_err(|_| self.time_overflow("decode", meta.dts))?;
        let duration = meta.duration;
        let sync = meta.sync;
        step(&self.tables, &mut self.cursor)?;
        if !self.emitted {
            self.reported_start_ms = ms_from_ticks(meta.pts, self.tables.timescale);
            self.emitted = true;
        }
        Ok(Some(Sample {
            pts,
            // A composition offset is what an MP4 source has and a Matroska one does not, so this
            // is the one source that can hand the writer both times instead of making it invent one.
            dts: Some(dts),
            // `stts` states a duration for every sample, so unlike a Matroska block this one never
            // needs the next sample's decode time to be measured.
            duration: Some(duration),
            keyframe: sync,
            data,
        }))
    }

    /// The bytes at one sample's offset: seek to it, and read exactly the size `stsz` gave it.
    ///
    /// A stream that ends short of that is an `Io` verdict, keeping the reader's own error kind,
    /// because a file whose tail has not arrived yet is not a file that lied about this sample.
    fn read_sample(&mut self, meta: &Meta) -> Result<Vec<u8>, MediaError> {
        let size = usize::try_from(meta.size).map_err(|_| {
            MediaError::Malformed(format!(
                "a sample of {} bytes does not fit this platform's address space",
                meta.size
            ))
        })?;
        self.reader.seek(SeekFrom::Start(meta.offset))?;
        let mut data = vec![0u8; size];
        self.reader.read_exact(&mut data)?;
        Ok(data)
    }

    fn time_overflow(&self, which: &str, ticks: i128) -> MediaError {
        MediaError::Malformed(format!(
            "a {} time of {ticks} ticks is past what a timeline holds",
            which
        ))
    }
}

impl<R: Read + Seek + Send> TrackSource for Mp4Source<R> {
    fn config(&self) -> &TrackConfig {
        &self.config
    }

    fn next_sample(&mut self) -> SourceResult<Option<Sample>> {
        if let Some(error) = self.poisoned() {
            return Err(error);
        }
        match self.next_sample_of_track() {
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

/// Milliseconds of source time for a tick count in the track's timescale, rounded toward zero.
///
/// A track whose composition offsets put its first sample before its decode zero has a negative
/// presentation time; there is no negative place on a stream that starts at zero, so it reads as
/// zero, which is also what a player that starts the stream there would report.
fn ms_from_ticks(ticks: i128, timescale: u32) -> u64 {
    let ms = ticks * 1_000 / i128::from(timescale);
    match u64::try_from(ms) {
        Ok(ms) => ms,
        Err(_) if ticks < 0 => 0,
        Err(_) => u64::MAX,
    }
}

/// The tables of one track's `stbl`, taken out of it and checked against each other.
///
/// `stbl` comes in by value and is destructured, so every box the walk has no use for - the
/// sample group boxes, the composition timing box a player may reorder from, `stsd` beyond the one
/// entry the caller kept - is dropped here rather than staying alive as part of a `Trak` nobody
/// will look at again.
fn tables_from(stbl: Stbl, track: u32, timescale: u32) -> Result<Tables, MediaError> {
    let Stbl {
        stts,
        ctts,
        stss,
        stsc,
        stsz,
        stco,
        co64,
        ..
    } = stbl;
    let (count, sizes) = match stsz.samples {
        StszSamples::Identical { count, size } => (u64::from(count), Sizes::Identical { size }),
        // The entries `mp4-atom` read are the count: it reads exactly as many as the box declares
        // and fails the whole `moov` when the bytes run out before that.
        StszSamples::Different { sizes } => {
            let count = u64::try_from(sizes.len()).unwrap_or(u64::MAX);
            (count, Sizes::Different(sizes))
        }
    };
    let offsets = match (stco, co64) {
        (Some(stco), None) => Offsets::Stco(stco.entries),
        (None, Some(co64)) => Offsets::Co64(co64.entries),
        (Some(_), Some(_)) => {
            return Err(MediaError::Malformed(format!(
                "track {track} has both a stco and a co64 chunk offset table"
            )));
        }
        (None, None) => {
            return Err(MediaError::Malformed(format!(
                "track {track} has no chunk offset table"
            )));
        }
    };
    let tables = Tables {
        track,
        timescale,
        stts: stts.entries,
        ctts: ctts.map(|ctts| ctts.entries).unwrap_or_default(),
        stss: stss.map(|stss| stss.entries),
        stsc: stsc.entries,
        sizes,
        offsets,
        count,
    };
    validate(&tables)?;
    Ok(tables)
}

/// Everything about the tables that has to agree before a byte of media data is read.
///
/// Each table counts the same samples its own way, so the totals are the check that matters: a
/// track whose `stts` covers 40 samples and whose `stsz` sizes 39 cannot be walked without a
/// sample that has a duration and no bytes, or one with bytes and no duration. A track that fails
/// here has no answer about where its samples are, and it fails before the file's media data is
/// asked for, which is what makes the verdict about the file rather than about a reader that
/// stopped answering.
fn validate(tables: &Tables) -> Result<(), MediaError> {
    let track = tables.track;
    if tables.timescale == 0 {
        return Err(MediaError::Malformed(format!(
            "track {track} has an mdhd timescale of 0"
        )));
    }
    // A count first, before anything that walks it: `stsz` states how many samples there are before
    // it states any of them, and the tables that grow with it are the container's own.
    if tables.count > MAX_MP4_SAMPLES {
        return Err(MediaError::Malformed(format!(
            "track {track} declares {} samples, more than the {MAX_MP4_SAMPLES} this remuxes",
            tables.count
        )));
    }
    if tables.count == 0 {
        return Err(MediaError::Malformed(format!(
            "track {track} has no samples"
        )));
    }
    // Sizes, and so the size of the buffer one sample's bytes go into.
    let over_cap = |size: u32| u64::from(size) > ELEMENT_CAP;
    match &tables.sizes {
        Sizes::Identical { size } if over_cap(*size) => {
            return Err(MediaError::Malformed(format!(
                "track {track}'s samples claim {size} bytes each, more than {ELEMENT_CAP}"
            )));
        }
        Sizes::Different(sizes) if sizes.iter().any(|size| over_cap(*size)) => {
            return Err(MediaError::Malformed(format!(
                "a sample of track {track} claims more than {ELEMENT_CAP} bytes"
            )));
        }
        _ => {}
    }
    // `stts`: the timeline.
    if tables.stts.is_empty() {
        return Err(MediaError::Malformed(format!(
            "track {track} has an empty stts table"
        )));
    }
    let mut timed = 0_u128;
    for entry in &tables.stts {
        if entry.sample_count == 0 {
            return Err(MediaError::Malformed(format!(
                "an stts entry of track {track} covers no samples"
            )));
        }
        timed += u128::from(entry.sample_count);
    }
    if timed != u128::from(tables.count) {
        return Err(MediaError::Malformed(format!(
            "track {track}'s stts covers {timed} samples, its stsz sizes {}",
            tables.count
        )));
    }
    // `ctts`: the presentation times on top of it.
    let mut offset_rows = 0_u128;
    for entry in &tables.ctts {
        if entry.sample_count == 0 {
            return Err(MediaError::Malformed(format!(
                "a ctts entry of track {track} covers no samples"
            )));
        }
        offset_rows += u128::from(entry.sample_count);
    }
    if !tables.ctts.is_empty() && offset_rows != u128::from(tables.count) {
        return Err(MediaError::Malformed(format!(
            "track {track}'s ctts offsets {offset_rows} samples, its stsz sizes {}",
            tables.count
        )));
    }
    // `stss`: which of them a decoder can start from. Ascending because the walk reads them as a
    // sorted list, which is what a writer of the box is supposed to produce. A box that is there and
    // names nothing is a different claim from no box at all: no box says every sample is a sync
    // sample, an empty one says none is, and a walk of such a track has no sample it may start on.
    if let Some(entries) = &tables.stss {
        if entries.is_empty() {
            return Err(MediaError::Malformed(format!(
                "track {track}'s stss box is there and names no sync sample, so a decoder has \
                 nowhere to start"
            )));
        }
        for pair in entries.windows(2) {
            if pair[0] >= pair[1] {
                return Err(MediaError::Malformed(format!(
                    "track {track}'s stss names sample {} after sample {}, and a walk cannot \
                     follow a list that does not increase",
                    pair[0], pair[1]
                )));
            }
        }
        for number in entries {
            if *number == 0 || u64::from(*number) > tables.count {
                return Err(MediaError::Malformed(format!(
                    "track {track}'s stss names sample {number}, which its stsz does not size"
                )));
            }
        }
    }
    // The chunk tables last: an empty offset table needs no check of its own, because `stsc`
    // covering any samples at all across zero chunks fails the count check inside it.
    validate_chunks(tables)
}

/// `stsc` and the chunk offset table, together: the chunks they describe, and the samples in them.
///
/// A group covers the chunks up to the next group's first chunk (the last group, to the end of the
/// offset table), so the two tables together say how many samples the track holds a second time -
/// and a track whose chunk table reaches past its offset table has samples whose bytes have no
/// stated place in the file.
fn validate_chunks(tables: &Tables) -> Result<(), MediaError> {
    let track = tables.track;
    if tables.stsc.is_empty() {
        return Err(MediaError::Malformed(format!(
            "track {track} has an empty stsc table"
        )));
    }
    // Every group's own claims first, because the second pass measures the distance between two
    // groups' first chunks and can only subtract numbers this pass has put in order.
    for (index, entry) in tables.stsc.iter().enumerate() {
        if index == 0 && entry.first_chunk != 1 {
            return Err(MediaError::Malformed(format!(
                "track {track}'s stsc starts at chunk {}, not at chunk 1",
                entry.first_chunk
            )));
        }
        if index > 0 && entry.first_chunk <= tables.stsc[index - 1].first_chunk {
            return Err(MediaError::Malformed(format!(
                "track {track}'s stsc goes from chunk {} to chunk {}, and a walk cannot follow a \
                 group list that does not increase",
                tables.stsc[index - 1].first_chunk,
                entry.first_chunk
            )));
        }
        if entry.samples_per_chunk == 0 {
            return Err(MediaError::Malformed(format!(
                "an stsc entry of track {track} puts no samples in its chunks"
            )));
        }
        if u64::from(entry.first_chunk) > tables.offsets.chunks() {
            return Err(MediaError::Malformed(format!(
                "track {track}'s stsc describes chunk {}, which has no offset in its {} chunk \
                 offset table",
                entry.first_chunk,
                tables.offsets.chunks()
            )));
        }
        // One `stsd` entry is the whole `stsd` this remuxer writes and every fragment's `trun`
        // refers to it by index 1, so a track that samples from a second entry - a second AVC
        // configuration record arriving at chunk 200 - would be decoded with a configuration the
        // file never put in front of it.
        if entry.sample_description_index != 1 {
            return Err(MediaError::Unsupported(format!(
                "track {track} samples from stsd entry {}, which this remuxer does not follow",
                entry.sample_description_index
            )));
        }
    }
    // Then what the two tables say about the samples between them.
    let mut chunked = 0_u128;
    for index in 0..tables.stsc.len() {
        let in_group = u128::from(chunks_in_group(tables, index)?);
        chunked += in_group * u128::from(tables.stsc[index].samples_per_chunk);
    }
    if chunked != u128::from(tables.count) {
        return Err(MediaError::Malformed(format!(
            "track {track}'s stsc puts {chunked} samples in its chunks, its stsz sizes {}",
            tables.count
        )));
    }
    Ok(())
}

/// The tables walked once: the arithmetic every sample is checked against, and the sample a seek
/// asked for lands on.
///
/// `target` is the time asked for in the track's own ticks, and 0 is a time like any other: a video
/// track whose first sync sample is its third one starts on that sample when it is opened at 0 ms
/// too, because the two frames in front of it are not decodable on their own.
///
/// For video the landing is the last sync sample whose presentation time is at or before the
/// target: the samples ahead of it are reference material for what follows, `stss` exists to name
/// those sync samples, and this is the landing a player asks a seek to give. A track with no `stss`
/// has every sample sync, so the answer for one is the last sample at or before the target. When
/// no sync sample reaches back that far - a seek to the first 200 ms of a track whose opening frame
/// is not a keyframe, or a track whose first keyframe is not its first sample opened at 0 - the
/// first sync sample is as close as the file gets, and a stream from there is still one a decoder
/// can start from.
///
/// For audio it is the first sample at or after the target, because every audio frame is
/// independently decodable and nothing before it is needed to decode it. A target past the track's
/// last sample lands past its end, which is a stream with no samples rather than an error, exactly
/// as a Matroska seek past its cues is.
///
/// The walk also decides nothing about the file that the tables do not say: it checks that a
/// sample's offset and size still fit a byte address, and that its times fit a timeline. It walks
/// the whole track even when the target is the first tick of it, so a track whose four hundred
/// thousandth sample has an impossible offset is refused at the open rather than four hundred
/// thousand samples into a stream, and it stops when the sample count says the samples are done.
fn scan(tables: &Tables, kind: TrackKind, target: i128) -> Result<Cursor, MediaError> {
    let track = tables.track;
    let mut cursor = Cursor::start(tables)?;
    let mut landing: Option<Cursor> = None;
    let mut first_sync: Option<Cursor> = None;
    while let Some(meta) = peek(tables, &cursor)? {
        let number = cursor.index + 1;
        if meta.offset.checked_add(u64::from(meta.size)).is_none() {
            return Err(MediaError::Malformed(format!(
                "track {track}'s sample {number} runs past a 64 bit byte offset: {} bytes from {}",
                meta.size, meta.offset
            )));
        }
        for (which, ticks) in [("decode", meta.dts), ("presentation", meta.pts)] {
            if i64::try_from(ticks).is_err() {
                return Err(MediaError::Malformed(format!(
                    "track {track}'s sample {number} has a {which} time of {ticks} ticks, past \
                     what a timeline holds"
                )));
            }
        }
        match kind {
            TrackKind::Video => {
                if meta.sync {
                    first_sync.get_or_insert_with(|| cursor.clone());
                    if meta.pts <= target {
                        landing = Some(cursor.clone());
                    }
                }
            }
            TrackKind::Audio => {
                if landing.is_none() && meta.pts >= target {
                    landing = Some(cursor.clone());
                }
            }
            TrackKind::Subtitle => {
                unreachable!("subtitle tracks are refused before this")
            }
        }
        step(tables, &mut cursor)?;
    }
    // Nothing at or before the video target, and nothing at or after the audio one: the closest the
    // file gets. For video that is `stss`'s first sync sample, the one `first_sync` stopped on; for
    // audio, whose walk never sets `first_sync` at all, it is past the track's end.
    match landing.or(first_sync) {
        Some(at) => Ok(at),
        None => Cursor::end(tables),
    }
}

/// Open one MP4 track for fragmented MP4: the MP4 half of [`open_track`](crate::open_track), as
/// the concrete source it boxes.
///
/// `track` is the container's own `track_ID`, which the public wrapper narrows from a `u32`;
/// `start_ms` is source time, and what it means here is what [`scan`] does with it: video lands on
/// the last sync sample at or before it, audio on the first sample at or after it. Zero is a target
/// like any other, so a video track whose first keyframe is its third sample starts on that
/// keyframe rather than on the two frames in front of it.
///
/// The header is read by the same box walk and under the same bounds as [`probe`](crate::probe::probe)
/// reads it, then the track's own tables are cross-checked and walked, and only after all of that
/// does anything ask this reader for a sample's bytes. The reader is not wrapped: it is asked for
/// exactly the bytes the tables point at, seeked to one at a time, and a buffer in front of those
/// seeks would read bytes the walk then jumps over.
///
/// The `mime` field becomes the stream's [`mime`](crate::FragmentStream::mime): the fragmented MP4
/// `Content-Type` for the track - `video/mp4` or `audio/mp4` plus its RFC 6381 codec string -
/// because the bytes it produces are fragmented MP4 whatever held the samples.
///
/// # Errors
///
/// [`MediaError::Unsupported`] for a track number the file does not carry, a handler that carries
/// no media a player would offer (timecode, metadata and helper tracks), a subtitle track or a
/// codec no fragmented MP4 sample entry can name, a fragmented input (`moov` with `mvex`, or a top
/// level `moof` on the way to the `moov`), and a track whose `stsc` samples from a second `stsd`
/// entry. [`MediaError::Malformed`] for a `moov` that does not parse, one that declares more than
/// [`MOOV_CAP`](crate::MOOV_CAP) bytes or that only this walk can reach, an `mdhd` timescale of 0,
/// tables that contradict each other (see [`validate`]), more samples than [`MAX_MP4_SAMPLES`], a
/// track with no samples, and a sample whose byte offset or timeline does not fit (see [`scan`]).
/// [`MediaError::Io`] for a reader that failed, which includes a `moov` this reader has not been
/// given yet.
///
/// # Panics
///
/// None. What a hostile file can do is a verdict, and the indexing that assumes the validated
/// tables is sound is written as `expect` with its reason.
pub(crate) fn open_source<R: Read + Seek>(
    reader: R,
    track: u32,
    start_ms: u64,
) -> Result<Mp4Source<R>, MediaError> {
    let mut reader = reader;
    let (moov, saw_moof) = probe::read_moov(&mut reader)?;
    // A fragmented file's `moov` parses the same way - its track simply has empty tables, the real
    // ones being in the fragments - so the answer has to come from `mvex` and `moof`, not from
    // whether the parse worked.
    if saw_moof || moov.mvex.is_some() {
        return Err(MediaError::Unsupported("fragmented MP4 input".to_string()));
    }
    let index = moov
        .trak
        .iter()
        .position(|trak| trak.tkhd.track_id == track)
        .ok_or_else(|| {
            MediaError::Unsupported(format!("track {track} is not one of this file's tracks"))
        })?;
    // Take the track out of the moov rather than borrowing it: every other trak's tables are the
    // same size as these and nothing else in this stream will ever ask for them, so the choice
    // between this and a borrow is the difference between one track's tables and the file's whole
    // moov being alive for the length of the stream.
    let trak = moov
        .trak
        .into_iter()
        .nth(index)
        .expect("the index came from this very vec");
    let info = probe::mp4_track(&trak)?.ok_or_else(|| {
        MediaError::Unsupported(format!(
            "track {track} has a {} handler, which this crate offers no stream for",
            trak.mdia.hdlr.handler
        ))
    })?;
    if info.kind == TrackKind::Subtitle || !info.codec.remuxable() {
        return Err(MediaError::Unsupported(format!(
            "track {track} ({}) has no fragmented MP4 sample entry fbtt writes",
            info.codec.name()
        )));
    }
    // The sample entry the track's samples are described by, and the one every chunk group is
    // checked below to actually point at. `mp4_track` read the same first entry to name the codec,
    // so the codec the probe reported and the configuration a decoder is handed cannot drift.
    let Some(entry) = trak.mdia.minf.stbl.stsd.codecs.first() else {
        return Err(MediaError::Unsupported(format!(
            "track {track} has an empty stsd table"
        )));
    };
    let entry = entry.clone();
    let timescale = trak.mdia.mdhd.timescale;
    // The encoded size comes from the sample entry rather than `tkhd`: the entry states the picture
    // a decoder produces, and `tkhd` states the size it is displayed at, which anamorphic video
    // does not keep the same number.
    let (width, height) = entry
        .visual()
        .map_or((0, 0), |visual| (visual.width, visual.height));
    let tables = tables_from(trak.mdia.minf.stbl, track, timescale)?;
    // The target in the track's own ticks, since the tables count in those and a landing has to be
    // compared to them rather than to milliseconds. Which way the division rounds is the two kinds'
    // difference: a video landing may sit before the target, because a decoder needs the sync sample
    // and the frames behind it, so video takes the whole ticks the division reaches; audio can use
    // nothing from before the target, so audio rounds up - 209 ms at 44100 Hz is 9216.9 ticks, and
    // the sample sitting on 9216 is 0.9 ms short of the time that was asked for.
    let product = u128::from(start_ms) * u128::from(timescale);
    let ticks = if info.kind == TrackKind::Audio {
        product.div_ceil(1_000)
    } else {
        product / 1_000
    };
    let target = i128::try_from(ticks).map_err(|_| {
        MediaError::Malformed(format!(
            "start time {start_ms} ms is past what timescale {timescale} can count"
        ))
    })?;
    let cursor = scan(&tables, info.kind, target)?;
    Ok(Mp4Source {
        reader,
        config: TrackConfig {
            // The source's own rate, not 90 kHz: a 48 kHz audio track's sample durations and an
            // edited video track's composition offsets are stated in it, and converting them would
            // round values the container did not round.
            timescale,
            kind: info.kind,
            codec: info.codec.clone(),
            // The source's own sample entry, copied through: an `avc1` entry's AVC decoder
            // configuration records and an `mp4a` entry's `esds` are exactly what a decoder needs
            // and were parsed once already.
            sample_entry: entry,
            width,
            height,
        },
        mime: mp4_mime(info.kind, &info.mime_codecs),
        tables,
        cursor,
        reported_start_ms: start_ms,
        emitted: false,
        failed: None,
    })
}
