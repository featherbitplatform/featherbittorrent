//! fbtt-media: what a media file holds, and the fragmented MP4 the web player streams from it.
//!
//! It is pure, synchronous Rust over `Read + Seek`: no tokio, no network, no engine. A caller hands
//! it any reader it likes - a file, a piece of a torrent behind a timeout, a `Cursor` over bytes in a
//! test - and this crate probes it ([`probe`()]), opens one of its video or audio tracks for
//! streaming ([`open_track`], whichever of Matroska, WebM or MP4 holds it), turns a track's samples
//! into fragmented MP4 ([`init_segment`], [`FragmentStream`]), and reads text subtitles into cues
//! (`subtitle`, from Task 5). It copies bytes: nothing here decodes or transcodes, which is what the
//! plan's "no transcoding, no native libraries" rule asks of it.
//!
//! Everything a file claims is treated as untrusted: sizes are bounded by [`CappedLen`] and
//! [`ELEMENT_CAP`] - with [`MOOV_CAP`] tighter for an MP4 `moov`, which is read whole - the calls a
//! header can cost by [`ReadBudget`] and [`MAX_HEADER_OPS`], one sample or frame by
//! [`MAX_SAMPLE_BYTES`], and a file that lies about itself yields a [`MediaError`], never a panic.

#![forbid(unsafe_code)]

pub mod capped;
pub mod codec;
pub mod fmp4;
mod mkv;
mod mp4in;
mod open;
pub mod probe;
mod source;
pub mod subtitle;

pub use capped::{CappedLen, MAX_HEADER_OPS, ReadBudget};
pub use codec::{Codec, TrackKind, sample_entry};
pub use fmp4::{FragmentWriter, init_segment};
pub use open::open_track;
pub use probe::{MediaInfo, TrackInfo, probe};
pub use source::{FragmentStream, Sample, TrackConfig};
pub use subtitle::{Cue, SubtitleFormat, embedded_cues, file_cues, subtitle_format};

/// Which container a file is.
///
/// WebM is a Matroska file whose EBML `DocType` says `webm`: the same reader
/// handles both, and the probe tells them apart because the answer says where
/// the bytes came from - a track of either streams as fragmented MP4 with
/// [`open_track`], whichever container held it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Container {
    /// Matroska, `DocType` `matroska`.
    Matroska,
    /// Matroska with `DocType` `webm`.
    WebM,
    /// ISO BMFF: MP4 and its QuickTime relatives.
    Mp4,
}

impl Container {
    /// The name a player or an HLS manifest calls this container.
    pub fn name(&self) -> &'static str {
        match self {
            Container::Matroska => "matroska",
            Container::WebM => "webm",
            Container::Mp4 => "mp4",
        }
    }
}

/// How far every remuxed track is shifted forward, so synthesized decode times are never negative.
///
/// The page maps between source time and media time with this constant, and the probe hands it out
/// with the rest of the file's description.
pub const TIME_OFFSET_MS: u64 = 1000;

/// The most room a Matroska element may be given, and so the most a demuxer may allocate for one.
///
/// A file states an element's size before it states its contents, so a 4 GiB `CodecPrivate` in a
/// 200 KiB file is one `Vec::with_capacity` away from a stalled process. [`CappedLen`] is what keeps
/// that honest.
///
/// This is a bound on one element. The demuxer measures the room for each element from that
/// element's own position, so a header that states many of them needs a total as well, and
/// [`ReadBudget`] is that: [`probe`](crate::probe::probe) reads a header with a budget of exactly
/// this many bytes.
pub const ELEMENT_CAP: u64 = 256 * 1024 * 1024;

/// The most contents a `moov` may declare itself to hold, which is what one probe or one open pays
/// for a header.
///
/// A `moov` is read whole: `mp4_atom` sizes its buffer from the declaration and decodes every table
/// box inside it, so a `moov` costs several times its own size at the peak of a single open, and
/// every player seek is an open. That is why this cap is tighter than [`ELEMENT_CAP`]: 256 MiB of
/// `moov` is 64 million 4 byte `stsz` entries, already a few hundred MB before this crate looks at
/// any of them. It is the bound on the sample tables a decoded header holds (see
/// [`MAX_MP4_SAMPLES`], which bounds the walk over them), and 64 MiB of entries is 16 million
/// samples, dozens of times past what a file worth streaming declares.
pub const MOOV_CAP: u64 = 64 * 1024 * 1024;

/// The most room one sample or frame's bytes may be given, whatever the container element holding it
/// declares.
///
/// [`ELEMENT_CAP`] is the right bound on a header element: one open reads it once, a
/// `Vec::with_capacity` and then a parse. Media is paid for more than once. The buffer the demuxer
/// fills for a frame, the copy that prepends the stripped header the container took off it, the
/// `mdat` a fragment is written into, and the fragment that queue is held in are each that frame's
/// size again, and the server queues four fragments for every stream it allows eight of: a file
/// claiming one 256 MiB frame is inside the element cap and costs about 1.5 GiB on one stream of it.
/// A frame therefore gets its own bound, one far above any real one (raw 4K YUV 4:2:0 is just under
/// 12 MiB, and a codec's keyframe of that picture is a fraction of it), where a bigger claim is a
/// file lying about its media rather than a header this crate needs room to read.
pub const MAX_SAMPLE_BYTES: u64 = 16 * 1024 * 1024;

/// The most of a sidecar subtitle file is read: a caption track nobody can scroll is not worth
/// unbounded memory.
pub const SUBTITLE_FILE_CAP: u64 = 4 * 1024 * 1024;

/// The most cues one subtitle file may hold: a cue table is one flat list, so a file with a
/// million cues behind a small byte budget would grow it without limit. Past this, the file is
/// [`MediaError::Malformed`] rather than a memory spike.
pub const MAX_CUES: usize = 100_000;

/// The widest window of source time a cue request may ask for.
pub const CUE_WINDOW_MAX_MS: u64 = 600_000;

/// The most frames an embedded subtitle read may walk through in one call, any track, before it gives up.
pub const MAX_CUE_SCAN_FRAMES: u64 = 1_000_000;

/// The longest a video fragment runs, however few keyframes the file has.
pub const MAX_FRAGMENT_MS: u64 = 5000;

/// The most bytes a fragment holds, however big the samples are.
pub const MAX_FRAGMENT_BYTES: usize = 16 * 1024 * 1024;

/// The most samples a video fragment may hold, however few keyframes the file has.
///
/// A fragment is one `moof` and one `mdat`, so its sample table is one flat array: a file with no
/// keyframes at all would otherwise grow a fragment's table for as long as its longest
/// non-keyframe run, and the cap is what bounds that array.
pub const MAX_FRAGMENT_SAMPLES: usize = 10_000;

/// The most frames a source walks through - other tracks' frames, frames before
/// the start time, frames ahead of a keyframe that never arrives - without
/// emitting one, before it gives up.
///
/// Every legitimate need is far below this: a one second keyframe distance
/// means a video seek skips a GOP's worth of frames at most, and interleaving
/// between tracks multiplies that by the track count, not by the file size. A
/// stream that never produces an emittable frame - a track with no frames at
/// all, a start time past every frame - would otherwise walk a 100 GiB file
/// sample by sample while its caller waits, which is the hang the plan's rules
/// forbid. The bound is deliberately far above any real stream; hitting it is
/// the `Malformed("too many frames without a sample")` verdict [`open_track`]
/// reports for a Matroska track that never delivers. An MP4 track needs no
/// bound like it: its samples are counted in the tables this walks before it
/// reads a single sample's bytes (see [`MAX_MP4_SAMPLES`]).
pub const MAX_SKIPPED_FRAMES: usize = 100_000;

/// The most samples one MP4 track's sample tables may describe, which is what bounds one walk of
/// them.
///
/// This does not bound the tables themselves: `mp4_atom` decodes a `moov`'s tables while it reads
/// the box, so a count is looked at only after the arrays it grew have been paid for, and it is
/// [`MOOV_CAP`] on the box that keeps that cost finite. What this bounds is the pass after the
/// decode: the one walk per open that checks every sample's byte offset and timeline and finds the
/// sample a seek lands on, an addition per sample and never a copy. A `u32` count allows four
/// billion samples, and a file that got one past the caps would be walked for minutes before it was
/// refused; 20 million is dozens of times what a four hour video track holds at 30 frames a second.
pub const MAX_MP4_SAMPLES: u64 = 20_000_000;

/// Why a file could not be used.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// A container or codec this crate does not read. The message names what it saw.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// A file that does not hold together: a truncated element, a size that contradicts the stream,
    /// a track with no samples.
    #[error("malformed: {0}")]
    Malformed(String),
    /// The reader failed. An [`std::io::ErrorKind::TimedOut`] stays here rather than becoming a
    /// `Malformed`: a piece of a torrent that has not arrived yet says nothing about the file.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
