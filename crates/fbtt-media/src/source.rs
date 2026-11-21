//! One track's worth of samples on their way into fragmented MP4, and what the
//! writer needs to know about the track they came from.
//!
//! Task 3 builds both: a `TrackSource` reads frames from a container and hands
//! them over as [`Sample`]s, and a [`TrackConfig`] carries what
//! [`init_segment`](crate::init_segment) and [`FragmentWriter`](crate::FragmentWriter)
//! need before the first sample arrives. This module is only the data those two
//! exchange; the reader that produces it is the next task.
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
