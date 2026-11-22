//! fbtt-media: what a media file holds, and the fragmented MP4 the web player streams from it.
//!
//! It is pure, synchronous Rust over `Read + Seek`: no tokio, no network, no engine. A caller hands
//! it any reader it likes - a file, a piece of a torrent behind a timeout, a `Cursor` over bytes in a
//! test - and this crate probes it ([`probe`()]), opens one of its Matroska tracks for streaming
//! ([`open_track`]), turns a track's samples into fragmented MP4 ([`init_segment`], [`FragmentStream`]),
//! and reads text subtitles into cues (`subtitle`, from Task 5). It copies
//! bytes: nothing here decodes or transcodes, which is what the plan's "no transcoding, no native
//! libraries" rule asks of it.
//!
//! Everything a file claims is treated as untrusted: sizes are bounded by [`CappedLen`] and
//! [`ELEMENT_CAP`], the calls a header can cost by [`ReadBudget`] and [`MAX_HEADER_OPS`], and a
//! file that lies about itself yields a [`MediaError`], never a panic.

#![forbid(unsafe_code)]

pub mod capped;
pub mod codec;
pub mod fmp4;
pub mod mkv;
pub mod probe;
pub mod source;

pub use capped::{CappedLen, MAX_HEADER_OPS, ReadBudget};
pub use codec::{Codec, TrackKind, sample_entry};
pub use fmp4::{FragmentWriter, init_segment};
pub use mkv::{MAX_SKIPPED_FRAMES, MkvSource, open_track};
pub use probe::{MediaInfo, TrackInfo, probe};
pub use source::{FragmentStream, Sample, SourceError, SourceResult, TrackConfig, TrackSource};

/// Which container a file is.
///
/// WebM is a Matroska file whose EBML `DocType` says `webm`: the same reader
/// handles both, and the probe tells them apart because the player asks the
/// browser for `video/webm` or `video/x-matroska` depending on it.
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

/// The most of a sidecar subtitle file is read: a caption track nobody can scroll is not worth
/// unbounded memory.
pub const SUBTITLE_FILE_CAP: u64 = 4 * 1024 * 1024;

/// The widest window of source time a cue request may ask for.
pub const CUE_WINDOW_MAX_MS: u64 = 600_000;

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
