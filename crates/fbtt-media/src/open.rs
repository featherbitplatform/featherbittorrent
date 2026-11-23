//! The one door into a file's tracks: sniff the container, then let that container read it.
//!
//! [`open_track`] is the public entry point of the crate's remuxing half. It decides *which*
//! reader a file gets from the same eight bytes [`probe`](crate::probe::probe) uses, hands the
//! reader to that container's module, and boxes what comes back into a
//! [`FragmentStream`](crate::FragmentStream). Both containers end up as the same fragmented MP4
//! stream; what differs is how a start time is turned into a landing, because the containers say
//! different things about where a decoder can start.

use crate::probe;
use crate::source::FragmentStream;
use crate::{Container, MediaError};
use std::io::{Read, Seek};

/// Open one track of a Matroska, WebM or MP4 file as a fragmented MP4 stream.
///
/// `track` is the container's own track number - Matroska's `TrackNumber`, MP4's `track_ID`, the
/// number [`probe`](crate::probe::probe) reports as a
/// [`TrackInfo`](crate::probe::TrackInfo)'s `id`. `start_ms` is source time, and zero is a request
/// for the start of the timeline rather than the absence of a request: the container's own rule,
/// below, is what decides where that lands.
///
/// The container is sniffed from the reader's first eight bytes, exactly as the probe sniffs it,
/// and the reader then belongs to that container: Matroska and WebM go to the `mkv` module, MP4 to
/// the `mp4in` module. Neither reads the whole file. A Matroska open reads the elements the
/// demuxer needs for its header and then frames; an MP4 open reads `moov`, wherever it is, cross
/// checks the track's sample tables, and then seeks to each sample it emits. The reader may sit
/// anywhere when it is handed over - the sniff rewinds it - and on return it sits wherever the
/// container left it, which is media data in both cases. A `BufReader` in front of the reader is
/// the wrong wrapper for either: both seek, and a buffer would re-read bytes they jumped over.
///
/// A seek lands differently per container, because the containers describe different starting
/// points. A Matroska video stream starts at the first keyframe at or after `start_ms`, which is
/// where the demuxer's own forward walk arrives; an MP4 video stream starts at the last sync sample
/// at or before it, the landing `stss` exists to describe - which for a file whose opening frame is
/// not a keyframe is its first keyframe, at `start_ms` 0 too. Audio needs no rule in either, every
/// audio frame being independently decodable, and starts at or after `start_ms`: an MP4 audio track
/// counts samples in its own ticks, so its target rounds up to the next whole one and a request for
/// 209 ms of a 44100 Hz track does not land on the sample at 9216 ticks, 0.9 ms short. The stream's
/// [`start_ms`](crate::FragmentStream::start_ms) is where the samples really began, so an MP4 video
/// stream that landed on a keyframe earlier than the time that was asked for reports that earlier
/// time rather than the one it was handed.
///
/// The stream's [`mime`](crate::FragmentStream::mime) is the fragmented MP4 `Content-Type` for the
/// track - `video/mp4` or `audio/mp4` plus the track's RFC 6381 codec string - because the bytes it
/// produces are fragmented MP4 whatever the source container was.
///
/// # Errors
///
/// [`MediaError::Unsupported`] for bytes that are neither Matroska, WebM nor MP4 and for a stream
/// too short to hold a container header; for a track number the file does not carry, a track type
/// or handler that holds no media a player would offer, a codec or subtitle track no fragmented MP4
/// sample entry can name, a fragmented MP4 input (a `moov` carrying `mvex`, or a top level `moof`
/// on the way to the `moov`), an AC-3 or E-AC-3 Matroska track whose first frame the walk cannot
/// find, and Matroska content encodings this crate does not undo (see the `mkv` module's own docs).
///
/// [`MediaError::Malformed`] for a header that does not parse, a file that ends inside a header
/// element, a demuxer panic, a `TimestampScale` or start time no timeline can hold, a frame
/// timestamp or duration too wide for the track's timeline, one frame or sample over
/// [`MAX_SAMPLE_BYTES`](crate::MAX_SAMPLE_BYTES), and a Matroska walk of
/// [`MAX_SKIPPED_FRAMES`](crate::MAX_SKIPPED_FRAMES) frames that produced nothing; for an MP4
/// `moov` declaring more than [`MOOV_CAP`](crate::MOOV_CAP) bytes, an `mdhd` timescale of 0,
/// sample tables that contradict each other, a track with no samples or with more than
/// [`MAX_MP4_SAMPLES`](crate::MAX_MP4_SAMPLES) of them, a sample declaring more than
/// [`MAX_SAMPLE_BYTES`](crate::MAX_SAMPLE_BYTES) bytes, and a sample whose byte offset or timeline
/// does not fit.
///
/// A reader that fails stays [`MediaError::Io`] - a torrent piece that has not arrived is not a
/// verdict about the file, and neither is a sample whose bytes are past what the stream holds. The
/// same verdict is given to every later request on the same stream, kind intact.
///
/// # Panics
///
/// None. The demuxer's panics are caught, unwound and reported; hostile input changes the answer,
/// never the control flow.
pub fn open_track<R: Read + Seek + Send + 'static>(
    mut reader: R,
    track: u32,
    start_ms: u64,
) -> Result<FragmentStream, MediaError> {
    // Both branches do the same two things: open the container's own source, then read the mime
    // out of it before the box takes the name with it.
    match probe::sniff(&mut reader)? {
        Container::Matroska | Container::WebM => {
            let source = crate::mkv::open_source(reader, u64::from(track), start_ms)?;
            let mime = source.mime.clone();
            FragmentStream::new(Box::new(source), mime)
        }
        Container::Mp4 => {
            let source = crate::mp4in::open_source(reader, track, start_ms)?;
            let mime = source.mime.clone();
            FragmentStream::new(Box::new(source), mime)
        }
    }
}
