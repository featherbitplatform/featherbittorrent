//! Container sniffing and the two header readers behind [`probe`].
use crate::capped::{CappedLen, ReadBudget};
use crate::codec::{self, Codec, TrackKind};
use crate::{Container, ELEMENT_CAP, MediaError};
use matroska_demuxer::{DemuxError, MatroskaFile, TrackEntry, TrackType};
use mp4_atom::{FourCC, Header, Moov, ReadAtom, ReadFrom};
use std::io::{Read, Seek, SeekFrom};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::result::Result;

/// A track: what kind it is, what codec it holds, and how a player should treat it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    /// The container's own track number: `TrackNumber` in Matroska, `track_ID` in MP4.
    pub id: u32,
    /// Video, audio or subtitle.
    pub kind: TrackKind,
    /// Which codec the samples are.
    pub codec: Codec,
    /// The RFC 6381 and ISO 14496-15 codec strings for this track: what an HLS
    /// manifest's `CODECS` attribute names, and what the fMP4 sample entry is
    /// called. Empty for a codec fbtt does not remux, which is the same set
    /// [`Codec::remuxable`] reports.
    pub mime_codecs: Vec<String>,
    /// The track's BCP 47 or three letter language.
    ///
    /// A container's "undetermined" marker (`und`, which both containers use as
    /// their default) is left out, so a language is present only when the file
    /// actually states one.
    pub language: Option<String>,
    /// The container's track title, if it carries one.
    pub name: Option<String>,
    /// Whether a player should pick this track when the viewer expresses no preference.
    pub default: bool,
    /// Whether this track should play even for a viewer who did not ask for subtitles.
    pub forced: bool,
}

/// What [`probe`] found: the container, how long it is, and what is in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaInfo {
    /// Matroska, WebM or MP4.
    pub container: Container,
    /// Duration in milliseconds, when the container states one.
    pub duration_ms: Option<u64>,
    /// The tracks, in container order.
    pub tracks: Vec<TrackInfo>,
}

/// Reads a container's header and reports its tracks.
///
/// The container is sniffed from the first eight bytes: the EBML magic
/// `1A 45 DF A3` is Matroska or WebM, and an `ftyp` or `moov` box header at
/// offset 4 is MP4.
///
/// `reader` may be any seekable source: a `BufReader<File>` in a test, a
/// `BufReader<Upload>` behind a torrent reader in the API. The container is read
/// from the start of the stream, so the reader may sit anywhere when it is handed
/// over. On return it sits wherever the header parse ended, which is media data
/// for Matroska and the byte after `moov` for MP4; a caller that still holds it
/// (a `&mut` reader, or one sharing a file position) has to seek before using it
/// again.
///
/// Only the header is read: the elements `matroska-demuxer` needs for its own
/// `open`, and for MP4 the boxes up to and including `moov`.
///
/// A file that is not Matroska, WebM or MP4, one too short to hold a container
/// header, and an MP4 that never reaches a `moov` are [`MediaError::Unsupported`]:
/// in each case there is no header to have got wrong. A file that starts like one
/// of the three and whose header does not parse is [`MediaError::Malformed`].
pub fn probe<R: Read + Seek>(mut reader: R) -> Result<MediaInfo, MediaError> {
    match sniff(&mut reader)? {
        Container::Matroska | Container::WebM => probe_matroska(&mut reader),
        Container::Mp4 => probe_mp4(&mut reader),
    }
}

/// Which container the eight bytes a caller read are, by the magic rule [`probe`] uses.
///
/// `Matroska` here means "EBML", which is Matroska *or* WebM: the two differ only
/// in the EBML header's `DocType`, which is deeper than these eight bytes, so
/// [`probe`] refines the answer when it reads that header and [`open_track`](crate::open_track)
/// does not care at all - a track of either streams the same way.
///
/// [`MediaError::Unsupported`] for bytes that are neither magic, with the same
/// message [`probe`] gives.
pub(crate) fn sniff_container(magic: &[u8; 8]) -> Result<Container, MediaError> {
    if magic[..4] == [0x1a, 0x45, 0xdf, 0xa3] {
        return Ok(Container::Matroska);
    }
    if magic[4..] == *b"ftyp" || magic[4..] == *b"moov" {
        return Ok(Container::Mp4);
    }
    Err(MediaError::Unsupported(format!(
        "not a Matroska, WebM or MP4 file: starts with {magic:02X?}"
    )))
}

/// The container, read off the first eight bytes of a stream and rewound after.
///
/// A stream too short to hold those bytes is [`MediaError::Unsupported`] - there
/// is no header in it to have got wrong - and any other read failure stays
/// [`MediaError::Io`], which is how a torrent piece that has not arrived yet
/// reaches a caller.
pub(crate) fn sniff<R: Read + Seek>(reader: &mut R) -> Result<Container, MediaError> {
    let mut magic = [0u8; 8];
    match reader.read_exact(&mut magic) {
        Ok(()) => {
            reader.rewind()?;
            sniff_container(&magic)
        }
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Err(
            MediaError::Unsupported("too short to hold a container header".to_string()),
        ),
        Err(err) => Err(err.into()),
    }
}

/// A `MatroskaFile` opened over a capped and budgeted reader, with the demuxer's panics turned
/// back into errors.
///
/// Two things a hostile file can do are still not errors in the pinned
/// `matroska-demuxer` fork. A `SeekHead` that carries two `Seek` entries
/// pointing at a `SeekHead` hits an `expect_err`, and a `SeekPosition` big
/// enough to overflow the position arithmetic panics on the addition. The
/// upstream's third panic shape - the same unchecked addition done to cue
/// cluster positions - is what the fork's `checked_add` replaced: a cue point
/// whose sum overflows is dropped instead. A torrent's file is attacker
/// controlled, so every Matroska open in fbtt goes through here and a panic
/// comes back as `MediaError::Malformed`, which is what the crate already
/// says about a header that does not hold together.
///
/// The reader is wrapped in [`CappedLen`]: no element may be given more than [`ELEMENT_CAP`]
/// bytes, and the whole open may not read more than `budget`. The budget is the caller's, because
/// Task 3 plays through this same wrapper and raises the budget once the header is read
/// ([`ReadBudget::lift`]), while a probe of an untrusted file keeps it at [`ELEMENT_CAP`].
///
/// `open` reads the EBML header, `Segment`, `SeekHead`, `Info`, `Tracks`, `Cues`, `Chapters` and
/// `Tags`, and stops at the first `Cluster` without touching a frame's bytes. One case costs more
/// than a header: a file with no `SeekHead` makes `open` build its own index by walking element
/// headers from the segment to the end of the stream, so the walk is as long as the file. Every
/// file ffmpeg or mkvtoolnix writes carries a `SeekHead`, so that is the shape of a live
/// recording, and the walk reads headers and seeks over bodies rather than reading media data.
pub(crate) fn open_matroska<R: Read + Seek>(
    reader: R,
    budget: ReadBudget,
) -> Result<MatroskaFile<CappedLen<R>>, MediaError> {
    let wrapped = CappedLen::new(reader, ELEMENT_CAP, budget);
    // `open` takes the reader with it, so a panic in the middle of the parse drops the reader and
    // everything it had allocated, which is why unwinding here is safe to ignore.
    // Relies on panics unwinding: a profile that sets panic = "abort" would undo this.
    match catch_unwind(AssertUnwindSafe(|| MatroskaFile::open(wrapped))) {
        Ok(opened) => opened.map_err(matroska_error),
        Err(_) => Err(MediaError::Malformed("matroska header".to_string())),
    }
}

/// The Matroska or WebM header, read by [`open_matroska`] under a budget of one element's bytes
/// and [`MAX_HEADER_OPS`](crate::MAX_HEADER_OPS) calls.
fn probe_matroska<R: Read + Seek>(reader: &mut R) -> Result<MediaInfo, MediaError> {
    // One element's worth is the whole header's budget: the cap alone is per element, so a file
    // with eight tracks each claiming 200 MiB of `CodecPrivate` would read 1.6 GiB of the same
    // bytes, and a `SeekHead` that points at itself would walk forever.
    let file = open_matroska(reader, ReadBudget::new(ELEMENT_CAP))?;
    let container = match file.ebml_header().doc_type() {
        "webm" => Container::WebM,
        _ => Container::Matroska,
    };
    let info = file.info();
    let scale = info.timestamp_scale().get();
    Ok(MediaInfo {
        container,
        duration_ms: info
            .duration()
            .and_then(|duration| ms_from_timecode(duration, scale)),
        tracks: matroska_tracks(&file)?,
    })
}

/// The media tracks of a Matroska file, in container order.
fn matroska_tracks<R: Read + Seek>(file: &MatroskaFile<R>) -> Result<Vec<TrackInfo>, MediaError> {
    let mut tracks = Vec::new();
    for track in file.tracks() {
        let Some(kind) = matroska_kind(track) else {
            continue;
        };
        let number = track.track_number().get();
        let id = u32::try_from(number)
            .map_err(|_| MediaError::Malformed(format!("TrackNumber {number} is over a u32")))?;
        let (codec, mime_codecs) = codec::matroska_codec(track.codec_id(), track.codec_private());
        let language = track
            .language_bcp47()
            .or_else(|| track.language())
            .filter(|language| *language != "und")
            .map(str::to_string);
        tracks.push(TrackInfo {
            id,
            kind,
            codec,
            mime_codecs,
            language,
            name: track.name().map(str::to_string),
            default: track.flag_default(),
            forced: track.flag_forced(),
        });
    }
    Ok(tracks)
}

/// The audio properties `sample_entry` and `open_track` ask a Matroska track for,
/// shared by the two so a rate Matroska states in its own units is converted in
/// one place.
///
/// Matroska's `SamplingFrequency` is in hertz and fractional; the sample entry
/// and the fragment timescale want a whole number, so this rounds, and the
/// element's own default (8 kHz, which the demuxer reports when the track left
/// the element out) passes through unchanged.
pub(crate) fn matroska_audio_rate(track: &TrackEntry) -> u32 {
    let hz = track.audio().map(|audio| audio.sampling_frequency());
    match hz {
        Some(hz) if hz.is_finite() && hz >= 1.0 => hz.round().min(u32::MAX as f64) as u32,
        // A rate below 1 Hz or a NaN is junk; the element's default is what a
        // player assumes for a track that states no rate at all.
        _ => 8000,
    }
}

/// What kind of media a Matroska track holds, or `None` for the track types
/// that hold none: complex, logo, button, control and metadata tracks.
///
/// [`open_track`](crate::open_track) maps the same track types the same way, which is
/// why this is `pub(crate)` rather than private to the probe.
pub(crate) fn matroska_kind(track: &TrackEntry) -> Option<TrackKind> {
    match track.track_type() {
        TrackType::Video => Some(TrackKind::Video),
        TrackType::Audio => Some(TrackKind::Audio),
        TrackType::Subtitle => Some(TrackKind::Subtitle),
        _ => None,
    }
}

/// Milliseconds from a Matroska `Duration`, which counts the segment's
/// `TimestampScale` (one millisecond, the default, in everything ffmpeg writes).
fn ms_from_timecode(duration: f64, scale: u64) -> Option<u64> {
    let ms = duration * (scale as f64 / 1_000_000.0);
    if ms.is_finite() && ms >= 0.0 {
        Some(ms.round() as u64)
    } else {
        None
    }
}

/// The most top level boxes [`read_moov`] looks through before it gives up on
/// finding a `moov`.
///
/// Skipping a box is a seek, so the walk is cheap per box, but its length is the
/// file's choice: 4 GiB of 8 byte `free` boxes is 512 million headers, and a file
/// with no `moov` anywhere walks every one of them. A track table does not hide
/// behind 4096 boxes.
const MAX_MOOV_SEARCH_BOXES: usize = 4096;

/// The MP4 header: the `moov` box, and whether a `moof` turned up on the way to it.
///
/// `mp4-atom`'s `moov` parser gives the same typed view it gives an MP4 muxer, so this reads track
/// metadata through the models the writer uses. The `moov`'s own size goes through
/// [`sized_moov_header`] before its bytes are read, because `mp4_atom` sizes its buffer from the
/// declaration. Boxes before it (`ftyp`, `free`, `moof`, and `mdat` in a file with `moov` last) are
/// skipped by seeking, so a large media data box costs nothing, and the walk gives up after
/// [`MAX_MOOV_SEARCH_BOXES`] of them: how many boxes there are is the file's choice, not ours.
///
/// The second half of the answer is how a reader tells a fragmented file: `mp4-atom` parses the
/// `moov` of one happily - its sample tables are simply empty, the real ones being in the
/// fragments - so `open_track` needs to know a `moof` was there, and `probe` does not care which
/// kind of MP4 a file is.
///
/// # Errors
///
/// [`MediaError::Unsupported`] for a stream that ends, or runs to its end inside a box, without a
/// `moov`; [`MediaError::Malformed`] for a `moov` that does not parse, or one only reachable past
/// the box limit; [`MediaError::Io`] for a reader that failed.
pub(crate) fn read_moov<R: Read + Seek>(reader: &mut R) -> Result<(Moov, bool), MediaError> {
    let mut skipped = 0_usize;
    let mut saw_moof = false;
    loop {
        let Some(header) = <Option<Header> as ReadFrom>::read_from(reader).map_err(atom_error)?
        else {
            return Err(MediaError::Unsupported(
                "MP4 file has no moov box".to_string(),
            ));
        };
        if header.kind == FourCC::new(b"moov") {
            let moov = sized_moov_header(reader, &header)?;
            let moov = Moov::read_atom(&moov, reader).map_err(atom_error)?;
            return Ok((moov, saw_moof));
        }
        saw_moof |= header.kind == FourCC::new(b"moof");
        skipped += 1;
        if skipped > MAX_MOOV_SEARCH_BOXES {
            return Err(MediaError::Malformed(format!(
                "no moov box within {MAX_MOOV_SEARCH_BOXES} top level boxes"
            )));
        }
        // A box that runs to the end of the file has nothing after it, so a
        // `moov` can only still be found in a box with a declared size.
        let Some(body) = header.size else {
            return Err(MediaError::Unsupported(
                "MP4 file has no moov box".to_string(),
            ));
        };
        let body = i64::try_from(body).map_err(|_| {
            MediaError::Malformed("a box declares more than 2^63 bytes".to_string())
        })?;
        // `Header::size` excludes the header, so this lands on the next box.
        reader.seek(SeekFrom::Current(body))?;
    }
}

/// The MP4 header, through [`read_moov`], plus the tracks it describes.
fn probe_mp4<R: Read + Seek>(reader: &mut R) -> Result<MediaInfo, MediaError> {
    // A fragmented file's tracks are still tracks: the probe reports them, and
    // `open_track` is the one that refuses to stream them (see `read_moov`).
    let (moov, _saw_moof) = read_moov(reader)?;

    let mvhd = &moov.mvhd;
    let mut tracks = Vec::new();
    for track in &moov.trak {
        if let Some(info) = mp4_track(track)? {
            tracks.push(info);
        }
    }
    Ok(MediaInfo {
        container: Container::Mp4,
        duration_ms: ms_from_timescale(mvhd.timescale, mvhd.duration),
        tracks,
    })
}

/// A declared box size, widened from `usize` so it can be compared to the cap.
fn capped_size(size: usize) -> u64 {
    u64::try_from(size).unwrap_or(u64::MAX)
}

/// A `moov` header whose declared size `mp4_atom` can be trusted to read.
///
/// A declared size is checked against [`ELEMENT_CAP`] because `read_body` sizes its buffer from
/// the declaration. A size of 0 means "runs to the end of the file", which `mp4_atom` reports as
/// `None`, and `read_body` then copies the stream with `io::copy` and no bound at all (its
/// `header.rs:126`), so a `moov` that claims to run to the end of a 4 GiB file is read as 4 GiB
/// of `Vec`. The run to the end is measured here instead, refused when it is longer than a header
/// may be, and otherwise handed back as the declared size the bounded reader wants.
///
/// Task 4 reads a source file's `moov` the same way when it writes fragmented MP4 headers out of
/// an existing file (the `mp4in` module behind [`open_track`](crate::open_track)), which is why this
/// is `pub(crate)`. The box walk that finds the header, and this bound on its contents, are the
/// crate's only moov cap: an MP4 open pays for one `moov` worth of bytes, whichever of its tracks
/// is opened.
pub(crate) fn sized_moov_header<R: Read + Seek>(
    reader: &mut R,
    header: &Header,
) -> Result<Header, MediaError> {
    let size = match header.size {
        Some(size) if capped_size(size) > ELEMENT_CAP => {
            return Err(MediaError::Malformed(format!(
                "moov declares more than {ELEMENT_CAP} bytes"
            )));
        }
        Some(size) => Some(size),
        None => {
            let here = reader.stream_position()?;
            let end = reader.seek(SeekFrom::End(0))?;
            reader.seek(SeekFrom::Start(here))?;
            let remaining = end.saturating_sub(here);
            if remaining > ELEMENT_CAP {
                return Err(MediaError::Malformed(format!(
                    "moov runs to the end of the file, which is more than {ELEMENT_CAP} bytes"
                )));
            }
            Some(usize::try_from(remaining).unwrap_or(usize::MAX))
        }
    };
    Ok(Header {
        kind: header.kind,
        size,
    })
}

/// Milliseconds from an `mvhd` duration counted in its own timescale.
fn ms_from_timescale(timescale: u32, duration: u64) -> Option<u64> {
    if timescale == 0 {
        return None;
    }
    u64::try_from(u128::from(duration) * 1000 / u128::from(timescale)).ok()
}

/// An MP4 track, or `None` for a track whose handler carries no media.
///
/// [`open_track`](crate::open_track) reads the same fields the probe reports - kind, codec and its
/// RFC 6381 strings - out of the track it is about to stream, so the mapping from a `hdlr` handler
/// and a sample entry to a [`TrackInfo`] lives here once, and this is `pub(crate)` for that reason.
pub(crate) fn mp4_track(track: &mp4_atom::Trak) -> Result<Option<TrackInfo>, MediaError> {
    let handler = track.mdia.hdlr.handler;
    let kind = if handler == FourCC::new(b"vide") {
        TrackKind::Video
    } else if handler == FourCC::new(b"soun") {
        TrackKind::Audio
    } else if [
        FourCC::new(b"sbtl"),
        FourCC::new(b"subt"),
        FourCC::new(b"text"),
    ]
    .contains(&handler)
    {
        TrackKind::Subtitle
    } else {
        // Timecode, metadata, closed caption and helper tracks are not tracks a
        // player offers a viewer.
        return Ok(None);
    };
    let Some(entry) = track.mdia.minf.stbl.stsd.codecs.first() else {
        return Ok(None);
    };
    let (codec, mime_codecs) = codec::mp4_codec(entry);
    let language = &track.mdia.mdhd.language;
    Ok(Some(TrackInfo {
        id: track.tkhd.track_id,
        kind,
        codec,
        mime_codecs,
        language: (language != "und" && !language.is_empty()).then(|| language.clone()),
        // ISO BMFF has no track title box that `mp4-atom` models, so an MP4
        // track is named by its codec, language and track number alone.
        name: None,
        // ISO BMFF has no forced flag either: an MP4 subtitle track is forced
        // by a `cla3` handler or an edit list, neither of which is our files.
        forced: false,
        // There is no default track flag in ISO BMFF. The `tkhd` enabled flag is
        // what a muxer leaves set on the track it means a player to pick and
        // clears on the alternatives it offers for selection, which is what
        // ffmpeg writes and what its reader reports as the default disposition.
        // `in_movie` says only that the track belongs to the presentation, so
        // every track ffmpeg muxes has it, and reading it would call every track
        // the default.
        default: track.tkhd.enabled,
    }))
}

/// `matroska-demuxer`'s errors, as `MediaError`s.
///
/// An element declaring more data than the stream holds is what a truncated
/// file, or a file whose sizes lie, looks like: `Malformed`. So is anything
/// else the parse can trip over, because the file said it was Matroska and was
/// not. So is an `InvalidData` read, which is how a spent [`ReadBudget`]
/// arrives: the header asked for more bytes than the file is worth, which says
/// something about the file. A document type, or a `DocTypeReadVersion`, fbtt
/// cannot read is `Unsupported`, and so is an EBML header that is not Matroska
/// at all. A read failure stays `Io`, so a torrent piece that has not arrived
/// stays distinct from a file that is broken.
///
/// Task 3 maps the demuxer's mid-stream failures with it too, which is why it is
/// `pub(crate)`: a frame read that fails is the same verdict as a header read that
/// fails, and the two paths must not drift apart on what counts as `Io`.
pub(crate) fn matroska_error(error: DemuxError) -> MediaError {
    match error {
        DemuxError::IoError(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
            MediaError::Malformed("file ends inside an element".to_string())
        }
        DemuxError::IoError(err) if err.kind() == std::io::ErrorKind::InvalidData => {
            MediaError::Malformed(err.to_string())
        }
        DemuxError::IoError(err) => MediaError::Io(err),
        DemuxError::UnsupportedDocType(doc_type) => {
            MediaError::Unsupported(format!("{doc_type} is not matroska or webm"))
        }
        DemuxError::UnsupportedDocTypeReadVersion(version) => {
            MediaError::Unsupported(format!("DocTypeReadVersion {version} is too new"))
        }
        DemuxError::InvalidEbmlHeader(reason) => {
            MediaError::Unsupported(format!("not a Matroska file: {reason}"))
        }
        other => MediaError::Malformed(other.to_string()),
    }
}

/// `mp4-atom`'s errors, as `MediaError`s.
///
/// A file that starts with an `ftyp` or `moov` box and then fails to parse is
/// not a valid MP4, so everything but a read failure is `Malformed`.
pub(crate) fn atom_error(error: mp4_atom::Error) -> MediaError {
    match error {
        mp4_atom::Error::Io(err) => MediaError::Io(err),
        other => MediaError::Malformed(other.to_string()),
    }
}
