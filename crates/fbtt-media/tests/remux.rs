//! Task 3: opening one Matroska track and streaming it as fragmented MP4.
//!
//! The tests below check the two things Task 3 promises. First, that the
//! samples the source hands over are the container's own: same bytes, same
//! timestamps, same keyframe decisions, at the sync point a seek should land
//! on - so `matroska-demuxer` reading the same file through its own `seek` and
//! `next_frame` serves as the independent oracle for every PTS claim. Second,
//! that the fragments are real fragmented MP4: a box walk with `mp4_atom`
//! reading the headers, and manual reads of the fields a player depends on -
//! `tfdt`'s base media decode time and `trun`'s sample count and sync flags.
//!
//! The hostile-file tests build their inputs in memory: a cue pointing past
//! the end, an element size that overruns the stream, a cluster table with 60k
//! entries. The demuxer panics on two of those shapes in a debug build, and a
//! panic in a library reading remote input is a crashed API worker, so the
//! crate wraps it: those tests assert the panic arrives as a `Malformed`
//! verdict (or, for the cue, as a stream that ends cleanly), never as a panic
//! and never as a hang.
use fbtt_media::{
    FragmentStream, MediaError, Sample, SourceError, TIME_OFFSET_MS, TrackKind, TrackSource,
    open_track,
};
use matroska_demuxer::{Frame, MatroskaFile};
use std::fs::File;
use std::io::{Cursor, Error as IoError, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

const MKV: &str = "h264-aac-opus-srt-ass.mkv";
const AV1_AC3: &str = "av1-ac3-eac3.mkv";
const HEVC_FLAC: &str = "hevc-flac.mkv";
const MP3_FILE: &str = "h264-mp3.mkv";
const LIVE: &str = "h264-live-nocues.mkv";
const WEBM: &str = "vp9-opus.webm";

/// The frame counts `expected.json` reports for the fixture tracks, restated
/// here as the counts a drain of one track must reproduce. Draining through the
/// source is a track filter test: a source that leaked other tracks' frames
/// through, or lost its own, would change these numbers.
const MKV_VIDEO_FRAMES: usize = 40;
// AAC frames are 1024 samples = 21.33 ms at 48 kHz, and the clip's last frame
// starts at 4010 ms: frames 0 through 4010, which is 189 of them, one more
// than the 188 the packet count suggested.
const MKV_AAC_FRAMES: usize = 189;
const MKV_OPUS_FRAMES: usize = 201;
const FLAC_FRAMES: usize = 175;
const MP3_FRAMES: usize = 168;

fn fixture_path(name: &str) -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures");
    path.push(name);
    path
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(fixture_path(name)).expect("fixture bytes")
}

/// The MKV video track's timescale: 90 kHz, which is the container's own rate.
const VIDEO_TIMESCALE: u32 = 90_000;

/// Milliseconds of source time from a PTS on the 90 kHz timeline.
fn video_ms(ticks: i64) -> i64 {
    ticks / (VIDEO_TIMESCALE as i64 / 1000)
}

// ---------------------------------------------------------------------------
// EBML assembly, copied in shape from tests/probe.rs: the fixture files are
// mkvmerge's, these inputs are hand-built, because the shapes under test (a cue
// past the end, an element that declares more than it holds) are not things a
// real encoder writes.
// ---------------------------------------------------------------------------

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
    ebml_declaring(
        id,
        u64::try_from(body.len()).expect("an element under 4 GiB"),
        body,
    )
}

fn ebml_declaring(id: &[u8], declared: u64, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(id.len() + 5 + body.len());
    out.extend_from_slice(id);
    out.extend_from_slice(&vint(declared));
    out.extend_from_slice(body);
    out
}

const EBML_HEADER: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];
const SEGMENT: [u8; 4] = [0x18, 0x53, 0x80, 0x67];
const SEEK_HEAD: [u8; 4] = [0x11, 0x4D, 0x9B, 0x74];
const SEEK: [u8; 2] = [0x4D, 0xBB];
const SEEK_ID: [u8; 2] = [0x53, 0xAB];
const SEEK_POSITION: [u8; 2] = [0x53, 0xAC];
const DOC_TYPE: [u8; 2] = [0x42, 0x82];
const DOC_TYPE_VERSION: [u8; 2] = [0x42, 0x87];
const DOC_TYPE_READ_VERSION: [u8; 2] = [0x42, 0x85];
const TRACKS: [u8; 4] = [0x16, 0x54, 0xAE, 0x6B];
const TRACK_ENTRY: [u8; 1] = [0xAE];
const TRACK_NUMBER: [u8; 1] = [0xD7];
const TRACK_UID: [u8; 2] = [0x73, 0xC5];
const TRACK_TYPE: [u8; 1] = [0x83];
const FLAG_DEFAULT: [u8; 1] = [0x88];
const CODEC_ID: [u8; 1] = [0x86];
const CODEC_PRIVATE: [u8; 2] = [0x63, 0xA2];
const DEFAULT_DURATION: [u8; 3] = [0x23, 0xE3, 0x83];
const AUDIO: [u8; 1] = [0xE1];
const SAMPLING_FREQUENCY: [u8; 1] = [0xB5];
const CHANNELS: [u8; 1] = [0x9F];
const VIDEO: [u8; 1] = [0xE0];
const PIXEL_WIDTH: [u8; 1] = [0xB0];
const PIXEL_HEIGHT: [u8; 1] = [0xBA];
const CONTENT_ENCODINGS: [u8; 2] = [0x6D, 0x80];
const CONTENT_ENCODING: [u8; 2] = [0x62, 0x40];
const CONTENT_ENCODING_ORDER: [u8; 2] = [0x50, 0x31];
const CONTENT_ENCODING_SCOPE: [u8; 2] = [0x50, 0x32];
const CONTENT_ENCODING_TYPE: [u8; 2] = [0x50, 0x33];
const CONTENT_COMPRESSION: [u8; 2] = [0x50, 0x34];
const CONTENT_COMP_ALGO: [u8; 2] = [0x42, 0x54];
const CONTENT_COMP_SETTINGS: [u8; 2] = [0x42, 0x55];
const CONTENT_ENCRYPTION: [u8; 2] = [0x50, 0x35];
const CONTENT_ENC_ALGO: [u8; 2] = [0x47, 0xE1];
const INFO: [u8; 4] = [0x15, 0x49, 0xA9, 0x66];
const MULTIPLEXING_APP: [u8; 2] = [0x4D, 0x80];
const WRITE_APP: [u8; 2] = [0x57, 0x41];
const TIMECODE_SCALE: [u8; 3] = [0x2A, 0xD7, 0xB1];
const CLUSTER: [u8; 4] = [0x1F, 0x43, 0xB6, 0x75];
const TIMECODE: [u8; 1] = [0xE7];
const SIMPLE_BLOCK: [u8; 1] = [0xA3];
const BLOCK: [u8; 1] = [0xA1];
const BLOCK_GROUP: [u8; 1] = [0xA0];
const BLOCK_DURATION: [u8; 1] = [0x9B];
const CUES: [u8; 4] = [0x1C, 0x53, 0xBB, 0x6B];
const CUE_POINT: [u8; 1] = [0xBB];
const CUE_TIME: [u8; 1] = [0xB3];
const CUE_TRACK_POSITIONS: [u8; 1] = [0xB7];
const CUE_TRACK: [u8; 1] = [0xF7];
const CUE_CLUSTER_POSITION: [u8; 1] = [0xF1];
const CUE_RELATIVE_POSITION: [u8; 1] = [0xF0];

const H264_ID: &[u8] = b"V_MPEG4/ISO/AVC";
const MP3_ID: &[u8] = b"A_MPEG/L3";
const AC3_ID: &[u8] = b"A_AC3";
const VP9_ID: &[u8] = b"V_VP9";
const SUBRIP_ID: &[u8] = b"S_TEXT/UTF8";

/// An `AVCDecoderConfigurationRecord` small enough to type out and valid enough
/// for `mp4_atom`'s parser: one 5-byte SPS, one 2-byte PPS. H.264 without a
/// configuration record is `Unsupported`, so every video fixture carries one.
fn avcc() -> Vec<u8> {
    vec![
        0x01, 0x64, 0x0C, 0x15, 0xFF, // version, profile, compat, level, NALU lengths
        0xE1, 0x00, 0x05, 0x67, 0x64, 0x0C, 0x15, 0xFF, // one SPS, 5 bytes
        0x01, 0x00, 0x02, 0x68, 0xEB, // one PPS, 2 bytes
    ]
}

fn ebml_header() -> Vec<u8> {
    ebml(
        &EBML_HEADER,
        &[
            ebml(&DOC_TYPE, b"matroska"),
            ebml(&DOC_TYPE_VERSION, &[1]),
            ebml(&DOC_TYPE_READ_VERSION, &[1]),
        ]
        .concat(),
    )
}

fn matroska_file(segment_body: &[u8]) -> Vec<u8> {
    [ebml_header(), ebml(&SEGMENT, segment_body)].concat()
}

/// The `Info` with the scale the synthetic files are cut on.
fn info_element(scale: u64) -> Vec<u8> {
    ebml(
        &INFO,
        &[
            ebml(&MULTIPLEXING_APP, b"fbtt"),
            ebml(&WRITE_APP, b"fbtt"),
            ebml(&TIMECODE_SCALE, &scale.to_be_bytes()),
        ]
        .concat(),
    )
}

/// A `TrackEntry`: the demuxer requires a number, a UID and a type, and the
/// codec configuration elements are what `sample_entry` reads.
#[allow(clippy::too_many_arguments)]
fn track_element(
    number: u64,
    track_type: u8,
    codec_id: &[u8],
    private: Option<&[u8]>,
    audio: Option<(f32, u64)>,
    video: Option<(u64, u64)>,
    default_duration: Option<u64>,
    encodings: Option<&[u8]>,
) -> Vec<u8> {
    let mut body = vec![
        ebml(
            &TRACK_NUMBER,
            &[u8::try_from(number).expect("a small track number")],
        ),
        ebml(
            &TRACK_UID,
            &[u8::try_from(number + 10).expect("a small track uid")],
        ),
        ebml(&TRACK_TYPE, &[track_type]),
        ebml(&FLAG_DEFAULT, &[1]),
        ebml(&CODEC_ID, codec_id),
    ];
    if let Some(private) = private {
        body.push(ebml(&CODEC_PRIVATE, private));
    }
    if let Some(default_duration) = default_duration {
        body.push(ebml(&DEFAULT_DURATION, &default_duration.to_be_bytes()));
    }
    if let Some((rate, channels)) = audio {
        body.push(ebml(
            &AUDIO,
            &[
                ebml(&SAMPLING_FREQUENCY, &rate.to_be_bytes()),
                ebml(&CHANNELS, &[u8::try_from(channels).expect("few channels")]),
            ]
            .concat(),
        ));
    }
    if let Some((width, height)) = video {
        body.push(ebml(
            &VIDEO,
            &[
                ebml(&PIXEL_WIDTH, &width.to_be_bytes()),
                ebml(&PIXEL_HEIGHT, &height.to_be_bytes()),
            ]
            .concat(),
        ));
    }
    if let Some(encodings) = encodings {
        body.push(ebml(&CONTENT_ENCODINGS, encodings));
    }
    ebml(&TRACK_ENTRY, &body.concat())
}

/// One `ContentEncoding`: compression with an algorithm and settings, or
/// encryption with an algorithm.
fn compression_encoding(order: u64, scope: u64, algo: u64, settings: &[u8]) -> Vec<u8> {
    ebml(
        &CONTENT_ENCODING,
        &[
            ebml(
                &CONTENT_ENCODING_ORDER,
                &[u8::try_from(order).expect("few encodings")],
            ),
            ebml(
                &CONTENT_ENCODING_SCOPE,
                &[u8::try_from(scope).expect("a small scope")],
            ),
            ebml(&CONTENT_ENCODING_TYPE, &[0]), // 0: compression
            ebml(
                &CONTENT_COMPRESSION,
                &[
                    ebml(
                        &CONTENT_COMP_ALGO,
                        &[u8::try_from(algo).expect("a small algorithm")],
                    ),
                    ebml(&CONTENT_COMP_SETTINGS, settings),
                ]
                .concat(),
            ),
        ]
        .concat(),
    )
}

fn encryption_encoding(algo: u64) -> Vec<u8> {
    ebml(
        &CONTENT_ENCODING,
        &[
            ebml(&CONTENT_ENCODING_ORDER, &[0]),
            ebml(&CONTENT_ENCODING_SCOPE, &[1]),
            ebml(&CONTENT_ENCODING_TYPE, &[1]), // 1: encryption
            ebml(
                &CONTENT_ENCRYPTION,
                &[ebml(
                    &CONTENT_ENC_ALGO,
                    &[u8::try_from(algo).expect("a small algorithm")],
                )]
                .concat(),
            ),
        ]
        .concat(),
    )
}

/// A `SimpleBlock` for track `track`, `offset` scale-units after its cluster's
/// timecode. Track 1..9 are one-byte VINTs; the keyframe flag is the flag
/// byte's high bit, and lacing bits stay 0 (a plain single-frame block).
fn simple_block(track: u64, offset: i16, keyframe: bool, payload: &[u8]) -> Vec<u8> {
    let flags = if keyframe { 0x80 } else { 0x00 };
    ebml(
        &SIMPLE_BLOCK,
        &[
            vec![0x80 | u8::try_from(track).expect("a small track number")],
            offset.to_be_bytes().to_vec(),
            vec![flags],
            payload.to_vec(),
        ]
        .concat(),
    )
}

/// A `BlockGroup` - the shape that carries a `BlockDuration`, and whose `Block`
/// the demuxer reports with no keyframe flag at all.
fn block_group(track: u64, offset: i16, duration: u64, payload: &[u8]) -> Vec<u8> {
    ebml(
        &BLOCK_GROUP,
        &[
            ebml(
                &BLOCK,
                &[
                    vec![0x80 | u8::try_from(track).expect("a small track number")],
                    offset.to_be_bytes().to_vec(),
                    vec![0x00],
                    payload.to_vec(),
                ]
                .concat(),
            ),
            ebml(&BLOCK_DURATION, &duration.to_be_bytes()),
        ]
        .concat(),
    )
}

fn cluster(timecode_ms: u64, blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut body = ebml(&TIMECODE, &timecode_ms.to_be_bytes());
    for block in blocks {
        body.extend_from_slice(block);
    }
    ebml(&CLUSTER, &body)
}

fn seek_entry(id: &[u8], at: u64) -> Vec<u8> {
    ebml(
        &SEEK,
        &[ebml(&SEEK_ID, id), ebml(&SEEK_POSITION, &at.to_be_bytes())].concat(),
    )
}

fn seek_head_named(entries: &[(&[u8], u64)]) -> Vec<u8> {
    let body: Vec<u8> = entries
        .iter()
        .flat_map(|(id, at)| seek_entry(id, *at))
        .collect();
    ebml(&SEEK_HEAD, &body)
}

/// A `Segment` body laid out as head, tracks, info, then the clusters: the
/// `SeekHead` names the first cluster (and the cues, if given) so `open`
/// indexes like a real file instead of walking the whole stream.
///
/// The head is built once with placeholder offsets to learn its length, then
/// again with the real ones; all elements after it sit at known offsets because
/// everything ahead of them is already assembled.
fn indexed_segment(tracks: &[u8], scale: u64, clusters: &[Vec<u8>]) -> Vec<u8> {
    let info = info_element(scale);
    let names: Vec<&[u8]> = vec![&TRACKS, &INFO, &CLUSTER];
    let placeholder = seek_head_named(&names.iter().map(|id| (*id, 0u64)).collect::<Vec<_>>());
    let tracks_at = u64::try_from(placeholder.len()).expect("a SeekHead under 4 GiB");
    let info_at = tracks_at + u64::try_from(tracks.len()).expect("Tracks under 4 GiB");
    let cluster_at = info_at + u64::try_from(info.len()).expect("Info under 4 GiB");

    let head = seek_head_named(
        &names
            .iter()
            .enumerate()
            .map(|(index, id)| match index {
                0 => (*id, tracks_at),
                1 => (*id, info_at),
                _ => (*id, cluster_at),
            })
            .collect::<Vec<_>>(),
    );
    assert_eq!(head.len(), placeholder.len());
    let mut body = head;
    body.extend_from_slice(tracks);
    body.extend_from_slice(&info);
    for cluster in clusters {
        body.extend_from_slice(cluster);
    }
    body
}

/// The GOP file every fragment-level test reads: a 90 kHz video track and four
/// clusters of 25 frames at 40 ms, each cluster opening with a keyframe - so
/// keyframes at 0, 1000, 2000 and 3000 ms, matching the real fixture's pattern
/// at a scale the test can type out. 100 frames, 4 seconds.
fn gop_file() -> Vec<u8> {
    let tracks = ebml(
        &TRACKS,
        &[track_element(
            1,
            1,
            H264_ID,
            Some(avcc().as_slice()),
            None,
            Some((640, 360)),
            None,
            None,
        )]
        .concat(),
    );
    let mut clusters = Vec::new();
    for gop in 0..4u64 {
        let mut blocks = Vec::new();
        for frame in 0..25u16 {
            blocks.push(simple_block(
                1,
                (frame * 40) as i16,
                frame == 0,
                &[0x41, 0x01, 0x02, 0x03],
            ));
        }
        clusters.push(cluster(gop * 1000, &blocks));
    }
    matroska_file(&indexed_segment(&tracks, 1_000_000, &clusters))
}

/// An audio file whose only track is a Layer III track at 44.1 kHz with frames
/// one millisecond apart from 0 to just under 3 seconds: `A_MPEG/L3` needs no
/// CodecPrivate at all, and every block is independently decodable, so this is
/// the shape for testing what the start time filters (and what it must not).
/// The frames are split into one cluster per second: the cue-less seek walks
/// cluster timecodes, and a target past the last cluster's timecode is past
/// the end as the demuxer sees it, however many blocks trail after it.
fn before_start_audio_file() -> Vec<u8> {
    let tracks = ebml(
        &TRACKS,
        &[track_element(
            2,
            2,
            MP3_ID,
            None,
            Some((44100.0, 2)),
            None,
            None,
            None,
        )]
        .concat(),
    );
    let chunks: Vec<Vec<u8>> = (0..3u64)
        .map(|second| {
            let blocks: Vec<Vec<u8>> = (0..1000i16)
                .map(|millisecond| simple_block(2, millisecond, false, &[0xFF, 0xFB, 0x90, 0x64]))
                .collect();
            cluster(second * 1_000, &blocks)
        })
        .collect();
    matroska_file(&indexed_segment(&tracks, 1_000_000, &chunks))
}

// ---------------------------------------------------------------------------
// A minimal fragmented-MP4 reader, for reading back what the writer wrote.
// `mp4_atom` models the boxes it parses, but a fragment arrives as bytes from
// the stream, and the fields these tests check (tfdt's base decode time, trun's
// count and flags) are read field by field here rather than through a model
// that could be as wrong as the writer.
// ---------------------------------------------------------------------------

struct Mp4Box<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

fn boxes(mut bytes: &[u8]) -> Vec<Mp4Box<'_>> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        assert!(bytes.len() >= 8, "a box is at least its header");
        let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        assert!(size >= 8 && size <= bytes.len(), "a box fits its bytes");
        out.push(Mp4Box {
            kind: [bytes[4], bytes[5], bytes[6], bytes[7]],
            body: &bytes[8..size],
        });
        bytes = &bytes[size..];
    }
    out
}

fn find_box<'a>(boxes: &[Mp4Box<'a>], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes
        .iter()
        .find(|entry| &entry.kind == kind)
        .map(|entry| entry.body)
}

/// The `traf` inside the first `moof` of a fragment, and its `tfdt` base media
/// decode time plus its `trun`'s sample count, durations and flags.
struct Traf {
    tfdt_base: u64,
    trun_count: u32,
    trun_durations: Vec<u32>,
    trun_flags: Vec<u32>,
}

fn read_first_traf(fragment: &[u8]) -> Traf {
    let fragment_boxes = boxes(fragment);
    let moof = find_box(&fragment_boxes, b"moof").expect("a moof in a fragment");
    let moof_boxes = boxes(moof);
    let traf = find_box(&moof_boxes, b"traf").expect("a traf in a moof");
    let traf_boxes = boxes(traf);
    let tfdt = find_box(&traf_boxes, b"tfdt").expect("a tfdt in a traf");
    let version = tfdt[0];
    let tfdt_base = match version {
        0 => u64::from(u32::from_be_bytes([tfdt[4], tfdt[5], tfdt[6], tfdt[7]])),
        1 => u64::from_be_bytes([
            tfdt[4], tfdt[5], tfdt[6], tfdt[7], tfdt[8], tfdt[9], tfdt[10], tfdt[11],
        ]),
        other => panic!("tfdt version {other}"),
    };
    let trun = find_box(&traf_boxes, b"trun").expect("a trun in a traf");
    let version = trun[0];
    assert_eq!(version, 1, "a fragmented stream writes trun version 1");
    let flags = u32::from_be_bytes([0, trun[1], trun[2], trun[3]]);
    let count = u32::from_be_bytes([trun[4], trun[5], trun[6], trun[7]]);
    let mut rest = &trun[8..];
    if flags & 0x1 != 0 {
        rest = &rest[4..]; // data_offset
    }
    if flags & 0x4 != 0 {
        rest = &rest[4..]; // first_sample_flags
    }
    let mut durations = Vec::new();
    let mut sample_flags = Vec::new();
    for _ in 0..count {
        if flags & 0x100 != 0 {
            durations.push(u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]));
            rest = &rest[4..];
        } else {
            durations.push(0);
        }
        if flags & 0x200 != 0 {
            rest = &rest[4..]; // sample_size
        }
        if flags & 0x400 != 0 {
            sample_flags.push(u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]));
            rest = &rest[4..];
        }
        if flags & 0x800 != 0 {
            rest = &rest[4..]; // composition offset (signed, unread here)
        }
    }
    assert!(rest.is_empty(), "trun entries account for every byte");
    Traf {
        tfdt_base,
        trun_count: count,
        trun_durations: durations,
        trun_flags: sample_flags,
    }
}

/// Pull every fragment a stream will answer with, in order, plus the source's
/// samples, so a test can check both views.
fn drain_fragments(stream: &mut FragmentStream) -> Vec<Vec<u8>> {
    let mut fragments = Vec::new();
    loop {
        match stream
            .next_fragment()
            .expect("no error from a healthy stream")
        {
            Some(fragment) => fragments.push(fragment),
            None => return fragments,
        }
    }
}

fn drain_samples(source: &mut fbtt_media::MkvSource<impl Read + Seek + Send>) -> Vec<Sample> {
    let mut samples = Vec::new();
    loop {
        match source
            .next_sample()
            .expect("no error from a healthy source")
        {
            Some(sample) => samples.push(sample),
            None => return samples,
        }
    }
}

/// The same file read by `matroska-demuxer` directly: the seek and frame stream
/// our source sits on, asked the same question, answered by the library rather
/// than by restating its arithmetic in the test.
fn oracle_first_keyframe(bytes: &[u8], track: u64, start_units: Option<u64>) -> Option<Frame> {
    let cursor = Cursor::new(bytes.to_vec());
    let mut file = MatroskaFile::open(cursor).expect("the demuxer opens its own fixture");
    if let Some(start) = start_units {
        file.seek(start).expect("the demuxer seeks");
    }
    let mut frame = Frame::default();
    loop {
        if !file
            .next_frame(&mut frame)
            .expect("no error from a healthy file")
        {
            return None;
        }
        if frame.track == track && frame.is_keyframe == Some(true) {
            return Some(frame);
        }
    }
}

/// The library's frame stream for `bytes`, seeked to `start_units` when given
/// and filtered to one track: the stream our source sits on, asked the same
/// question, answered by the demuxer rather than restated in the test.
fn oracle_stream(bytes: &[u8], track: u64, start_units: Option<u64>) -> Vec<Frame> {
    let cursor = Cursor::new(bytes.to_vec());
    let mut file = MatroskaFile::open(cursor).expect("the demuxer opens its own fixture");
    if let Some(start) = start_units {
        // A seek to zero is not a no-op in this library, and our source does
        // not seek at zero either: ask the demuxer the same question the
        // source was asked, which at zero means "read from the beginning".
        if start > 0 {
            file.seek(start).expect("the demuxer seeks");
        }
    }
    let mut out = Vec::new();
    let mut frame = Frame::default();
    while file
        .next_frame(&mut frame)
        .expect("no error from a healthy file")
    {
        if frame.track == track {
            out.push(frame.clone());
        }
    }
    out
}

/// The frames a video source keeps from a frame stream, with the sync flag
/// each one carries: everything from the first frame that is not known to be
/// a reference frame, with unknown flags counting as sync until a real
/// keyframe anchors the timeline, after which only flagged-key frames are
/// sync. The crate's documented reading, restated in test code so the
/// comparison is against a second implementation and not itself.
fn source_kept_video(frames: &[Frame]) -> Vec<(bool, &Frame)> {
    let mut anchored = false;
    let mut out: Vec<(bool, &Frame)> = Vec::new();
    for frame in frames {
        let sync = match (frame.is_keyframe, anchored) {
            (Some(true), _) => true,
            (Some(false), false) => continue,
            (Some(false), true) => false,
            (None, false) => true,
            (None, true) => false,
        };
        anchored |= sync;
        out.push((sync, frame));
    }
    out
}

// ---------------------------------------------------------------------------
// A reader that counts bytes, for the "the file is read about once" test.
// ---------------------------------------------------------------------------

struct CountingReader {
    inner: File,
    bytes: Arc<AtomicU64>,
}

impl Read for CountingReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        let read = self.inner.read(buf)?;
        self.bytes.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

impl Seek for CountingReader {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, IoError> {
        self.inner.seek(pos)
    }
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

#[test]
fn remux_h264_fixture_tracks_all_start_at_zero() {
    // Every remuxable track of the main fixture, opened at zero: the stream
    // must reproduce exactly the track's frame count from expected.json - a
    // source that leaked another track's frames through, or lost its own,
    // changes these numbers - and must end cleanly.
    // The other two remuxable codec families get their own fixtures: FLAC (a
    // sample entry rebuilt from the fLaC header) and Layer III (no
    // configuration record at all), opened at zero and counted.
    for (name, track, frames) in [(HEVC_FLAC, 2u64, FLAC_FRAMES), (MP3_FILE, 2, MP3_FRAMES)] {
        let mut source =
            open_track(Cursor::new(fixture_bytes(name)), track, 0).expect("track opens");
        let samples = drain_samples(&mut source);
        assert_eq!(samples.len(), frames, "{name} track {track} frame count");
    }
    let file = fixture_bytes(MKV);
    for (track, frames) in [
        (1u64, MKV_VIDEO_FRAMES),
        (2, MKV_AAC_FRAMES),
        (3, MKV_OPUS_FRAMES),
    ] {
        let mut source = open_track(Cursor::new(file.clone()), track, 0).expect("track opens");
        let config = source.config().clone();
        assert_eq!(
            config.kind == TrackKind::Video,
            track == 1,
            "track {track} kind"
        );
        // Decode order, PTS and bytes, straight against the demuxer's own
        // frame stream for the same track: the fixture's video track carries
        // B-frames, so PTS climbs and falls in the decode order the container
        // stored, and "monotonic" would be the wrong promise.
        let stream_frames = oracle_stream(&file, track, None);
        let samples = drain_samples(&mut source);
        assert_eq!(samples.len(), frames, "track {track} frame count");
        assert_eq!(
            stream_frames.len(),
            frames,
            "the demuxer counts the same frames on track {track}"
        );
        assert_eq!(
            samples.first().expect("a sample").pts,
            0,
            "track {track} starts at 0"
        );
        for (frame, sample) in stream_frames.iter().zip(&samples) {
            let ticks = u64::from(config.timescale);
            assert_eq!(
                sample.pts,
                i64::try_from(frame.timestamp * ticks / 1_000).expect("fixture ticks"),
                "track {track} PTS matches the demuxer's own frame stream"
            );
            assert_eq!(
                &sample.data, &frame.data,
                "track {track} bytes pass through unchanged"
            );
        }
    }
    // The subtitle track: a sample entry a browser can demux does not exist for
    // it, and that is an open-time verdict, not a mid-stream surprise.
    let error = open_track(Cursor::new(file), 4, 0).expect_err("a SubRip track is not remuxable");
    assert!(
        matches!(error, MediaError::Unsupported(_)),
        "a subtitle track is Unsupported, got {error:?}"
    );
    assert!(
        error.to_string().contains("track 4"),
        "the refusal names the track: {error}"
    );
}

#[test]
fn remux_seek_pts_oracle() {
    // For a list of start times, the first sample our source emits has the PTS
    // of the first keyframe at or after where matroska-demuxer's own seek
    // lands - computed by the library, not restated here. Where the file has
    // no keyframe left to land on, both answers are "nothing".
    let file = fixture_bytes(MKV);
    for start_ms in [0u64, 500, 1200, 2000, 3500, 4000] {
        // This fixture's cue table is junk the demuxer trusts: its cue
        // positions point past the end of the file, so any nonzero seek lands
        // the reader - the library's own and ours alike - beyond the last
        // cluster. Where the library finds no frames, so must we; where it
        // finds them (the zero case, no seek at all), the streams must match
        // frame for frame once the crate's keep rule is applied.
        let oracle_frames = oracle_stream(&file, 1, Some(start_ms));
        let kept = source_kept_video(&oracle_frames);
        let mut source = open_track(Cursor::new(file.clone()), 1, start_ms)
            .expect("video track opens at every start time");
        let first = source.next_sample().expect("no error");
        let mut samples: Vec<Sample> = first.into_iter().collect();
        samples.extend(drain_samples(&mut source));
        assert_eq!(
            kept.len(),
            samples.len(),
            "track 1 sample count at start {start_ms} ms"
        );
        for ((sync, frame), sample) in kept.iter().zip(&samples) {
            assert_eq!(
                sample.pts,
                i64::try_from(frame.timestamp * u64::from(VIDEO_TIMESCALE) / 1_000)
                    .expect("a fixture PTS"),
                "track 1 PTS at start {start_ms} ms"
            );
            assert_eq!(&sample.data, &frame.data, "bytes pass through unchanged");
            assert_eq!(
                sample.keyframe, *sync,
                "the sync flag follows the frame's flags through the crate's reading"
            );
        }
    }
}

#[test]
fn remux_seek_beyond_end_yields_no_samples() {
    // A seek past the last frame is not an error: the source reports the end,
    // the stream carries the init segment alone, and everything stays clean.
    let file = gop_file();
    let mut source = open_track(Cursor::new(file.clone()), 1, 100_000)
        .expect("seeking past the end is not an error");
    assert!(
        source.next_sample().expect("no error").is_none(),
        "no samples after a seek past the last frame"
    );
    let stream =
        FragmentStream::new(open_track(Cursor::new(file), 1, 100_000).expect("opens again"))
            .expect("a stream");
    let mut stream = stream;
    let first = drain_fragments(&mut stream);
    // One Vec at most (the init segment alone), then nothing.
    assert!(
        first.len() <= 1,
        "a stream with no samples emits at most the init segment"
    );
}

#[test]
fn remux_fragment_starts_at_keyframe() {
    // Seek to 1200 ms - not a keyframe - and the stream starts at the keyframe
    // at or after it, 2000 ms. The library's own seek lands on the first
    // block at or after the target and never rewinds, so the keyframe at 1000
    // is already behind the reader when the source opens; both what the stream
    // reports as its start and the tfdt the fragments carry say 2000 ms, not
    // 1200, and that is what the demuxer's own seek reports as well.
    let file = gop_file();
    let mut stream =
        FragmentStream::new(open_track(Cursor::new(file.clone()), 1, 1200).expect("opens at 1200"))
            .expect("a stream");
    let fragments = drain_fragments(&mut stream);
    assert!(
        !fragments.is_empty(),
        "a stream with samples emits fragments"
    );
    let expected =
        oracle_first_keyframe(&file, 1, Some(1200)).expect("a keyframe at or after 1200");
    assert_eq!(
        expected.timestamp,
        stream.start_ms(),
        "the stream starts where the demuxer's own seek lands"
    );
    assert_eq!(
        stream.start_ms(),
        2000,
        "the sync point reachable from 1200 ms is 2000 ms"
    );
    // tfdt = source PTS + the plan's 1000 ms, in track ticks.
    let traf = read_first_traf(&fragments[fragments.len() - 1]);
    let first_pts_ticks = expected.timestamp * u64::from(VIDEO_TIMESCALE) / 1_000;
    let offset = TIME_OFFSET_MS * u64::from(VIDEO_TIMESCALE) / 1_000;
    let earliest = fragments
        .iter()
        .filter(|fragment| boxes(fragment).iter().any(|entry| &entry.kind == b"moof"))
        .map(|fragment| read_first_traf(fragment).tfdt_base)
        .min()
        .expect("a moof was emitted");
    assert_eq!(
        earliest,
        first_pts_ticks + offset,
        "the first fragment's tfdt carries the sync point plus the plan's offset"
    );
    assert_eq!(
        traf.trun_flags[0] & 0x0200_0000,
        0x0200_0000,
        "the first sample is sync"
    );
}

#[test]
fn remux_one_fragment_per_gop() {
    // Four GOPs, four fragments, every fragment a moof plus an mdat, and the
    // durations the fragments add up to span the timeline: the last sample of
    // the last fragment carries whatever the timeline leaves it, and nothing is
    // double-counted across the seams between fragments.
    let file = gop_file();
    let stream = FragmentStream::new(open_track(Cursor::new(file), 1, 0).expect("opens at 0"))
        .expect("a stream");
    let mut stream = stream;
    let fragments = drain_fragments(&mut stream);
    let moof_count = fragments
        .iter()
        .flat_map(|fragment| boxes(fragment))
        .filter(|entry| &entry.kind == b"moof")
        .count();
    let mdat_count = fragments
        .iter()
        .flat_map(|fragment| boxes(fragment))
        .filter(|entry| &entry.kind == b"mdat")
        .count();
    assert_eq!(moof_count, 4, "one moof per GOP");
    assert_eq!(mdat_count, 4, "one mdat per moof");
    let counts: Vec<u32> = fragments
        .iter()
        .flat_map(|fragment| {
            boxes(fragment)
                .into_iter()
                .filter(|entry| &entry.kind == b"moof")
                .map(|entry| read_first_traf(&entry_body(fragment, &entry)).trun_count)
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        counts,
        vec![25; 4],
        "each fragment carries exactly its GOP's samples"
    );
    assert_eq!(
        fragments.len(),
        3,
        "the writer holds one fragment back, so 3 Vecs for 4 GOPs"
    );
    // The first fragment is prefixed with the init segment.
    let first = boxes(&fragments[0]);
    assert_eq!(
        &first[0].kind, b"ftyp",
        "the first Vec starts with the init segment"
    );
    assert!(find_box(&first, b"moov").is_some(), "and carries its moov");
    // The timeline total: sum every trun duration across every fragment.
    let total: u64 = fragments
        .iter()
        .flat_map(|fragment| {
            boxes(fragment)
                .into_iter()
                .filter(|entry| &entry.kind == b"moof")
                .map(|entry| read_first_traf(&entry_body(fragment, &entry)))
                .collect::<Vec<_>>()
        })
        .map(|traf| traf.trun_durations.iter().sum::<u32>() as u64)
        .sum();
    // Four GOPs of 1000 ms each end at 4000 ms, which in 90 kHz ticks is
    // 360_000_000; durations are derived from decode times, so the pieces add
    // up exactly and the last sample's borrowed duration closes the final gap.
    assert_eq!(
        total,
        4_000 * u64::from(VIDEO_TIMESCALE) / 1_000,
        "the fragments cover the timeline"
    );
}

/// The body bytes of one moof box, re-framed so `read_first_traf` (which wants
/// a whole fragment starting at the moof) can read it.
fn entry_body(_fragment: &[u8], entry: &Mp4Box<'_>) -> Vec<u8> {
    // Rebuild the moof as a standalone fragment: the box bytes exactly as they
    // appear, with no mdat, which read_first_traf never looks at.
    let size = entry.body.len() + 8;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&(size as u32).to_be_bytes());
    out.extend_from_slice(&entry.kind);
    out.extend_from_slice(entry.body);
    out
}

#[test]
fn remux_audio_track_from_midsecond_start() {
    // Audio has no keyframes, so nothing is ever dropped for being unsynced:
    // the stream must carry exactly the frames the library's own seek leaves
    // it. On the real fixture the cue table is junk the demuxer trusts - its
    // positions point past the end - so the library's seek to 1500 ms lands
    // beyond the last cluster, the stream is empty, and the start the source
    // reports is the time it was asked for, unchanged, because no sample ever
    // contradicts it. On a synthetic file with a sane cue table the same seek
    // lands exactly on 1500 ms.
    let file = fixture_bytes(MKV);
    let frames = oracle_stream(&file, 3, Some(1500));
    let mut source = open_track(Cursor::new(file.clone()), 3, 1500).expect("opus opens at 1500");
    let first = source.next_sample().expect("no error");
    let mut samples: Vec<Sample> = first.into_iter().collect();
    samples.extend(drain_samples(&mut source));
    assert_eq!(
        samples.len(),
        frames.len(),
        "the Opus stream is the demuxer's own frame stream after the same seek"
    );
    for (frame, sample) in frames.iter().zip(&samples) {
        assert_eq!(
            sample.pts,
            i64::try_from(frame.timestamp * 48_000 / 1_000).expect("opus ticks")
        );
        assert_eq!(&sample.data, &frame.data, "bytes pass through unchanged");
        assert!(sample.keyframe, "every audio sample is a sync point");
    }
    assert_eq!(
        source.start_ms(),
        1500,
        "a stream that emitted nothing reports the start time it was asked for"
    );
    // The synthetic file makes the drop side visible: 1500 of the 3000 frames
    // are gone, and the first kept frame is exactly 1500 ms in.
    let before = before_start_audio_file();
    let mut source = open_track(Cursor::new(before), 2, 1500).expect("the mp3 track opens at 1500");
    let samples = drain_samples(&mut source);
    assert_eq!(
        samples.len(),
        1500,
        "frames before the start time are dropped"
    );
    assert_eq!(
        samples[0].pts,
        1500 * 44_100 / 1_000,
        "the first kept frame is the start time"
    );
    // A file with no cue table at all (the live-recording fixture) still seeks:
    // the linear cluster walk is the fallback, not an error.
    let live = fixture_bytes(LIVE);
    let stream = FragmentStream::new(
        open_track(Cursor::new(live), 1, 2000)
            .expect("the nocues fixture seeks by walking clusters"),
    )
    .expect("a stream");
    let mut stream = stream;
    assert!(
        stream.start_ms() <= 2000,
        "the stream reports where its samples begin"
    );
    assert!(
        !drain_fragments(&mut stream).is_empty(),
        "and it carries the samples from there"
    );
}

#[test]
fn remux_truncated_mkvs_stream_to_an_end() {
    // Two truncated files: a real fixture cut at 60% (mid-cluster, so the cut
    // lands inside block data the declared size promised), and a synthetic file
    // whose first block declares 4096 bytes and holds 40. The demuxer's size
    // check catches the synthetic one, and reading past the end catches the
    // real one; either way the verdict is an error or a clean end - never a
    // panic, never a hang.
    let whole = fixture_bytes(MKV);
    let cut = whole.len() * 3 / 5;
    let mut errored = false;
    match open_track(Cursor::new(whole[..cut].to_vec()), 1, 0) {
        Err(error) => {
            assert!(
                matches!(
                    error,
                    MediaError::Malformed(_) | MediaError::Unsupported(_) | MediaError::Io(_)
                ),
                "a truncated file refuses to open with a verdict: {error:?}"
            );
            errored = true;
        }
        Ok(mut source) => {
            // It opened: then the frames either stream and stop with an error,
            // or stream to a clean end, but they stream (the cut is well past
            // the first cluster).
            let mut streamed = 0;
            loop {
                match source.next_sample() {
                    Ok(Some(_)) => streamed += 1,
                    Ok(None) => break,
                    Err(_) => {
                        errored = true;
                        break;
                    }
                }
            }
            assert!(
                streamed > 100,
                "60% of the fixture is media data, and it streamed: {streamed}"
            );
        }
    }
    // The synthetic overrun must produce an error: its declared block size is a
    // promise the stream does not keep, and that is the definition of a
    // malformed stream.
    let tracks = ebml(
        &TRACKS,
        &[track_element(
            1,
            1,
            H264_ID,
            Some(avcc().as_slice()),
            None,
            Some((640, 360)),
            None,
            None,
        )]
        .concat(),
    );
    let short_block = ebml_declaring(
        &SIMPLE_BLOCK,
        4096,
        &[0x81u8, 0x00, 0x00, 0x80, 0x41, 0x01, 0x02, 0x03],
    );
    let clusters = vec![ebml(
        &CLUSTER,
        &[ebml(&TIMECODE, &0u64.to_be_bytes()), short_block].concat(),
    )];
    let truncated = matroska_file(&indexed_segment(&tracks, 1_000_000, &clusters));
    let mut source = open_track(Cursor::new(truncated), 1, 0)
        .expect("the header parses; the overrun is downstream");
    match source.next_sample() {
        Ok(maybe) => panic!("a block that overruns the stream ended as {maybe:?}, not an error"),
        Err(error) => assert!(
            matches!(error, SourceError::Malformed(_) | SourceError::Io(_)),
            "the overrun is a verdict, not a panic: {error:?}"
        ),
    }
    assert!(
        errored,
        "the truncated fixture produced an error at open or drain"
    );
}

/// The cue table this fixture carries: 60000 `CuePoint`s, one per second for
/// 60000 seconds, all naming the one cluster the file has. `MatroskaFile::open`
/// parses the whole table (its SeekHead names `Cues`), and the walk costs a
/// dozen-plus reader calls an entry - measured, not estimated - so the file
/// needs a header budget an order of magnitude above what a thousand-entry
/// table costs. That is the `MAX_HEADER_OPS` raise this fixture exists to prove.
#[test]
fn remux_open_track_with_60000_cue_points() {
    let tracks = ebml(
        &TRACKS,
        &[track_element(
            1,
            1,
            H264_ID,
            Some(avcc().as_slice()),
            None,
            Some((640, 360)),
            None,
            None,
        )]
        .concat(),
    );
    let info = info_element(1_000_000);
    let flat_clusters: Vec<Vec<u8>> = (0..40u64)
        .map(|second| {
            cluster(
                second * 1_000,
                &[simple_block(1, 0, true, &[0x41, 0x01, 0x02, 0x03])],
            )
        })
        .collect();

    // Layout: head, tracks, info, clusters..., cues - the head names all four
    // element kinds, with the clusters named at their first.
    let names: Vec<&[u8]> = vec![&TRACKS, &INFO, &CLUSTER, &CUES];
    let placeholder = seek_head_named(&names.iter().map(|id| (*id, 0u64)).collect::<Vec<_>>());
    let tracks_at = u64::try_from(placeholder.len()).expect("a SeekHead under 4 GiB");
    let info_at = tracks_at + u64::try_from(tracks.len()).expect("Tracks under 4 GiB");
    let cluster_at = info_at + u64::try_from(info.len()).expect("Info under 4 GiB");
    let clusters_len: usize = flat_clusters.iter().map(Vec::len).sum();
    let cues_at = cluster_at + u64::try_from(clusters_len).expect("clusters under 4 GiB");

    let head = seek_head_named(
        &names
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, [tracks_at, info_at, cluster_at, cues_at][index]))
            .collect::<Vec<_>>(),
    );
    assert_eq!(head.len(), placeholder.len());

    let mut cue_body = Vec::new();
    for second in 0..60_000u64 {
        let positions = ebml(
            &CUE_TRACK_POSITIONS,
            &[
                ebml(&CUE_TRACK, &[1]),
                ebml(&CUE_CLUSTER_POSITION, &cluster_at.to_be_bytes()),
            ]
            .concat(),
        );
        cue_body.extend_from_slice(&ebml(
            &CUE_POINT,
            &[ebml(&CUE_TIME, &(second * 1_000).to_be_bytes()), positions].concat(),
        ));
    }
    let cues = ebml(&CUES, &cue_body);

    let mut segment = head;
    segment.extend_from_slice(&tracks);
    segment.extend_from_slice(&info);
    for cluster in &flat_clusters {
        segment.extend_from_slice(cluster);
    }
    segment.extend_from_slice(&cues);
    let file = matroska_file(&segment);

    let calls = Arc::new(AtomicU64::new(0));
    let counting = CountCalls {
        inner: Cursor::new(file),
        calls: Arc::clone(&calls),
    };
    // The seek to 30000 seconds: broad phase is a search through the 60000
    // cue entries, and narrow phase walks the cluster headers between the
    // landing cluster and the target to find the first frame at or after it.
    // That is real cost on reader calls, which is what the budget assertions
    // measure right after the open.
    let mut source = match open_track(counting, 1, 30_000) {
        Ok(source) => source,
        Err(error) => panic!("open_track refused a 60000-entry cue table: {error}"),
    };
    let opened_with = calls.load(Ordering::Relaxed);
    assert!(
        opened_with > 1 << 20,
        "the fixture must actually be long: {opened_with} reader calls, which is not over the old budget"
    );
    assert!(
        opened_with < 1 << 22,
        "the fixture must fit the raised budget: {opened_with} reader calls"
    );
    let samples = drain_samples(&mut source);
    // The seek landed on 30 seconds: the source kept the keyframes at or after
    // where its own seek clamped (all 40 frames are keyframes, from 30 s on
    // that is 10 of them).
    assert_eq!(
        samples.len(),
        10,
        "the 30000 s seek lands inside the last 10 seconds"
    );
    assert_eq!(
        samples[0].pts,
        30_000 * 90,
        "first sample PTS at 30 s on the 90 kHz timeline"
    );
}

struct CountCalls<R> {
    inner: R,
    calls: Arc<AtomicU64>,
}

impl<R: Read> Read for CountCalls<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for CountCalls<R> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, IoError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.seek(pos)
    }
}

#[test]
fn remux_cue_past_end_is_an_error_or_timeout() {
    // A cue whose cluster is real but whose CueRelativePosition is 0xFFFF...FF0:
    // `seek` adds it to the cluster's data offset and overflows a `u64`, and in
    // a debug build `matroska-demuxer` 0.8.1 panics right there (`lib.rs`, the
    // broad phase's `cluster_data_offset + relative_position`). A library that
    // panics on hostile remote input crashes the API worker, so the crate wraps
    // the whole seek in a catch-unwind; this runs the open in a thread with a
    // five second bound so neither a panic escaping nor a hang can hide.
    let tracks = ebml(
        &TRACKS,
        &[track_element(
            1,
            1,
            H264_ID,
            Some(avcc().as_slice()),
            None,
            Some((640, 360)),
            None,
            None,
        )]
        .concat(),
    );
    let info = info_element(1_000_000);
    let cluster = cluster(0, &[simple_block(1, 0, true, &[0x41, 0x01])]);

    let names: Vec<&[u8]> = vec![&TRACKS, &INFO, &CLUSTER, &CUES];
    let placeholder = seek_head_named(&names.iter().map(|id| (*id, 0u64)).collect::<Vec<_>>());
    let tracks_at = u64::try_from(placeholder.len()).expect("a SeekHead under 4 GiB");
    let info_at = tracks_at + u64::try_from(tracks.len()).expect("Tracks under 4 GiB");
    let cluster_at = info_at + u64::try_from(info.len()).expect("Info under 4 GiB");
    let cues_at = cluster_at + u64::try_from(cluster.len()).expect("Cluster under 4 GiB");
    let head = seek_head_named(
        &names
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, [tracks_at, info_at, cluster_at, cues_at][index]))
            .collect::<Vec<_>>(),
    );

    let positions = ebml(
        &CUE_TRACK_POSITIONS,
        &[
            ebml(&CUE_TRACK, &[1]),
            ebml(&CUE_CLUSTER_POSITION, &cluster_at.to_be_bytes()),
            ebml(
                &CUE_RELATIVE_POSITION,
                &0xFFFF_FFFF_FFFF_FFF0u64.to_be_bytes(),
            ),
        ]
        .concat(),
    );
    let point = ebml(
        &CUE_POINT,
        &[ebml(&CUE_TIME, &500_000u64.to_be_bytes()), positions].concat(),
    );
    let cues = ebml(&CUES, &[point].concat());

    let mut segment = head;
    segment.extend_from_slice(&tracks);
    segment.extend_from_slice(&info);
    segment.extend_from_slice(&cluster);
    segment.extend_from_slice(&cues);
    let file = matroska_file(&segment);

    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let outcome = match open_track(Cursor::new(file), 1, 500_000) {
            Ok(mut source) => {
                // Open survived: the stream then either errors or ends. A
                // wrapped panic surfaces here as an error; a clean end (the
                // seek landing past everything and the walk finding nothing)
                // is also an acceptable answer.
                let mut frames = 0;
                let mut verdict = "clean end of stream".to_string();
                loop {
                    match source.next_sample() {
                        Ok(Some(_)) => frames += 1,
                        Ok(None) => break,
                        Err(error) => {
                            verdict = error.to_string();
                            break;
                        }
                    }
                }
                if frames > 0 {
                    verdict = format!(
                        "{frames} samples streamed, which is not a stream from past the end"
                    );
                }
                verdict
            }
            Err(error) => error.to_string(),
        };
        let _ = sender.send(outcome);
    });
    let outcome = match receiver.recv_timeout(Duration::from_secs(5)) {
        Ok(outcome) => outcome,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("seeking to a cue past the end hung instead of answering")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the seek panicked out of its guard instead of reporting a verdict")
        }
    };
    // The acceptable answers are all verdicts: a refusal at open (Malformed:
    // the demuxer tripped on its own arithmetic, and the guard caught it) or a
    // stream that says the same, or a clean empty end. "Panicked" or "hung"
    // would have reached us as a disconnect or a timeout above.
    assert!(
        outcome.contains("malformed") || outcome.contains("clean end of stream"),
        "a cue past the end answered {outcome:?}, which is neither a Malformed verdict nor a clean end"
    );
}

#[test]
fn remux_open_missing_track_is_unsupported() {
    let file = gop_file();
    let error = open_track(Cursor::new(file), 9, 0).expect_err("track 9 does not exist");
    match error {
        MediaError::Unsupported(message) => assert!(
            message.contains("track 9"),
            "the refusal names what is missing: {message}"
        ),
        other => panic!("a missing track is Unsupported, got {other:?}"),
    }
}

#[test]
fn remux_unsupported_codec_is_unsupported() {
    // VP9: a browser demuxes VP9, but only from WebM - there is no sample entry
    // fbtt writes for it in an fMP4, so it is refused at open, per plan, not
    // streamed and trusted to work. And a subtitle track has no fragmented-MP4
    // samples at all. Both refusals arrive naming the track, at open.
    // The real VP9 fixture, refused exactly like the synthetic one: a browser
    // demuxes VP9 from WebM, and there is no fMP4 sample entry fbtt writes.
    let error = open_track(Cursor::new(fixture_bytes(WEBM)), 1, 0)
        .expect_err("VP9 has no fragmented-MP4 sample entry fbtt writes");
    assert!(
        matches!(error, MediaError::Unsupported(_)),
        "VP9 is Unsupported: {error:?}"
    );
    let audio_body = compression_encoding(0, 1, 0, &[0xAB, 0xCD]); // zlib: not undoable
    let tracks = ebml(
        &TRACKS,
        &[
            track_element(1, 1, VP9_ID, None, None, Some((1280, 720)), None, None),
            track_element(
                2,
                3,
                SUBRIP_ID,
                None,
                None,
                None,
                None,
                Some(audio_body.as_slice()),
            ),
            // A track type that holds no media (a logo track) is the third shape
            // of "not for the player".
            ebml(
                &TRACK_ENTRY,
                &[
                    ebml(&TRACK_NUMBER, &[3]),
                    ebml(&TRACK_UID, &[13]),
                    ebml(&TRACK_TYPE, &[7]), // logo
                    ebml(&CODEC_ID, b"V_MPEG4/ISO/AVC"),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let clusters = vec![cluster(0, &[simple_block(1, 0, true, &[0x01])])];
    let file = matroska_file(&indexed_segment(&tracks, 1_000_000, &clusters));
    for track in [1u64, 2, 3] {
        let error = open_track(Cursor::new(file.clone()), track, 0)
            .expect_err("none of these tracks has fragmented-MP4 samples fbtt can write");
        match error {
            MediaError::Unsupported(message) => assert!(
                message.contains(&format!("track {track}")),
                "the refusal names track {track}: {message}"
            ),
            other => panic!("track {track} is Unsupported, got {other:?}"),
        }
    }
}

#[test]
fn remux_malformed_stream_is_an_error_not_a_panic() {
    // A BlockGroup whose BlockDuration is `u64::MAX` and a TimestampScale of 1:
    // the duration, counted in scale units and then in nanoseconds, overflows
    // every timeline width, and the first sample after the start time has to
    // say so rather than serve a wrapped number.
    let tracks = ebml(
        &TRACKS,
        &[track_element(
            1,
            1,
            H264_ID,
            Some(avcc().as_slice()),
            None,
            Some((640, 360)),
            None,
            None,
        )]
        .concat(),
    );
    let clusters = vec![cluster(0, &[block_group(1, 0, u64::MAX, &[0x41, 0x01])])];
    let file = matroska_file(&indexed_segment(&tracks, 1_000_000, &clusters));
    let mut source =
        open_track(Cursor::new(file), 1, 0).expect("the header is fine; the duration is not");
    match source.next_sample() {
        Ok(maybe) => panic!("a duration no timeline can hold ended as {maybe:?}, not an error"),
        Err(error) => assert!(
            matches!(error, SourceError::Malformed(_)),
            "the overflow is Malformed, got {error:?}"
        ),
    }
    // A start time that cannot be expressed in the file's timestamp units, at
    // a TimestampScale of 1: 2e13 ms is 2e19 units, past `u64`, and the open
    // refuses it before the demuxer ever sees the request.
    let coarse = matroska_file(&indexed_segment(
        &ebml(
            &TRACKS,
            &[track_element(
                1,
                1,
                H264_ID,
                Some(avcc().as_slice()),
                None,
                Some((640, 360)),
                None,
                None,
            )]
            .concat(),
        ),
        1,
        &[cluster(0, &[simple_block(1, 0, true, &[0x41])])],
    ));
    match open_track(Cursor::new(coarse), 1, 20_000_000_000_000) {
        Err(MediaError::Malformed(message)) => assert!(
            message.contains("TimestampScale"),
            "the refusal says why: {message}"
        ),
        other => panic!("a start time past the scale's reach is Malformed, got {other:?}"),
    }
    // A PTS jump backward, past what the fragment writer can sign into: the
    // writer's verdict arrives through the stream, not as a panic.
    let file = gop_file();
    let stream =
        FragmentStream::new(open_track(Cursor::new(file), 1, 0).expect("opens")).expect("a stream");
    let mut stream = stream;
    assert!(
        drain_fragments(&mut stream).len() >= 3,
        "the GOP file streams"
    );
}

#[test]
fn remux_reads_each_byte_about_once() {
    // The plan's byte-accounting rule, measured against a raw `File` (no
    // buffering in front of it, so every byte the demuxer asks for is counted
    // as it crosses): seeking the cluster table costs *seeks*, and block bodies
    // are read whole, so a whole-file stream costs about one file pass plus the
    // header elements it re-reads - never the quadratic re-reading a
    // buffering mistake would cost. Two passes is the bound; a BufReader with a
    // big buffer in front of the seeking demuxer re-reads kilobytes per seek
    // and would blow it.
    let path = fixture_path(MKV);
    let bytes = Arc::new(AtomicU64::new(0));
    let counting = CountingReader {
        inner: File::open(&path).expect("fixture"),
        bytes: Arc::clone(&bytes),
    };
    let file_size = counting.inner.metadata().expect("metadata").len();
    let mut source = open_track(counting, 1, 0).expect("the video track opens");
    let samples = drain_samples(&mut source);
    assert_eq!(
        samples.len(),
        MKV_VIDEO_FRAMES,
        "the whole video track streamed"
    );
    let total = bytes.load(Ordering::Relaxed);
    assert!(
        total <= file_size * 2,
        "streamed a {file_size} byte file while reading {total} bytes"
    );
}

#[test]
fn remux_blockgroup_first_frame_counts_as_sync() {
    // A BlockGroup carries no keyframe flag (the demuxer reads no
    // ReferenceBlock, so `is_keyframe` is None): the crate's documented reading
    // is that the first frame of a video track with no flags is the sync point
    // a stream starts from, and every later unflagged frame is non-sync. And
    // the BlockGroup's BlockDuration - counted in the file's TimestampScale
    // units - becomes the sample's stated duration in track ticks.
    let tracks = ebml(
        &TRACKS,
        &[track_element(
            1,
            1,
            H264_ID,
            Some(avcc().as_slice()),
            None,
            Some((640, 360)),
            None,
            None,
        )]
        .concat(),
    );
    let clusters = vec![cluster(
        5000,
        &[
            block_group(1, 0, 45, &[0x41, 0x01, 0x02]),
            block_group(1, 20, 45, &[0x41, 0x01, 0x03]),
        ],
    )];
    let file = matroska_file(&indexed_segment(&tracks, 1_000_000, &clusters));
    let mut source = open_track(Cursor::new(file), 1, 0).expect("opens");
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
    assert_eq!(
        video_ms(second.pts),
        5020,
        "the second frame follows the first"
    );
}

#[test]
fn remux_content_encodings_gate() {
    // Header stripping is undone byte for byte; zlib, bzlib and lzo1x are
    // refused (fbtt does not inflate, and promising a stream with a header
    // where its deflate stream is would fool a decoder); a scope other than
    // whole-frames is refused (the removal cannot be put back frame-wise);
    // real encryption is refused; and NotEncrypted - signed, not encrypted -
    // passes, because nothing about the bytes is encrypted.
    let stripping = compression_encoding(0, 1, 3, &[0xAB, 0xCD]); // Stripping, frames
    let zlib = compression_encoding(0, 1, 0, &[0xAB]); // Zlib
    let scoped = compression_encoding(0, 2, 3, &[0xAB]); // Stripping, scope 2
    let aes = encryption_encoding(5); // AES
    let signed = encryption_encoding(0); // NotEncrypted
    let private = avcc();
    let tracks = ebml(
        &TRACKS,
        &[
            track_element(
                1,
                1,
                H264_ID,
                Some(private.as_slice()),
                None,
                Some((640, 360)),
                None,
                Some(stripping.as_slice()),
            ),
            track_element(
                2,
                1,
                H264_ID,
                Some(private.as_slice()),
                None,
                Some((640, 360)),
                None,
                Some(zlib.as_slice()),
            ),
            track_element(
                3,
                1,
                H264_ID,
                Some(private.as_slice()),
                None,
                Some((640, 360)),
                None,
                Some(scoped.as_slice()),
            ),
            track_element(
                4,
                1,
                H264_ID,
                Some(private.as_slice()),
                None,
                Some((640, 360)),
                None,
                Some(aes.as_slice()),
            ),
            track_element(
                5,
                1,
                H264_ID,
                Some(private.as_slice()),
                None,
                Some((640, 360)),
                None,
                Some(signed.as_slice()),
            ),
        ]
        .concat(),
    );
    // One cluster with one keyframe per track: track 1's blocks have the
    // stripped header missing from their stored bytes.
    let mut blocks = Vec::new();
    blocks.push(simple_block(1, 0, true, &[0x41, 0x01, 0x02]));
    for track in 2..=5u64 {
        blocks.push(simple_block(track, 0, true, &[0x41, 0x01, 0x02]));
    }
    let clusters = vec![cluster(0, &blocks)];
    let file = matroska_file(&indexed_segment(&tracks, 1_000_000, &clusters));

    // Track 1: opens, and the emitted sample carries the stripped header back.
    let mut source = open_track(Cursor::new(file.clone()), 1, 0).expect("stripping is undoable");
    let sample = source.next_sample().expect("no error").expect("a sample");
    assert_eq!(
        sample.data,
        vec![0xAB, 0xCD, 0x41, 0x01, 0x02],
        "the settings bytes go back where the container took them off"
    );
    for (track, expectation) in [(2u64, "zlib"), (3, "scope"), (4, "encrypted")] {
        let error = open_track(Cursor::new(file.clone()), track, 0)
            .expect_err(&format!("{expectation} on track {track} is refused"));
        assert!(
            matches!(error, MediaError::Unsupported(_)),
            "track {track} ({expectation}) is Unsupported, got {error:?}"
        );
    }
    let source = open_track(Cursor::new(file), 5, 0).expect("signed is not encrypted");
    assert_eq!(
        source.config().kind,
        TrackKind::Video,
        "the signed track plays"
    );
}

#[test]
fn remux_dolby_tracks_read_their_first_frame() {
    // AC-3 and E-AC-3 sample entries are a copy of a syncframe header, and
    // Matroska has no CodecPrivate for them: the first frame has to be read
    // before the sample entry can be written - at zero, and after a seek. On
    // the real fixture both Dolby tracks open at both start times.
    let file = fixture_bytes(AV1_AC3);
    for track in [2u64, 3] {
        for start_ms in [0u64, 500] {
            // The Dolby sample entry is a copy of the first syncframe the hunt
            // finds past the seek, and the stream then carries exactly the
            // frames the library's own seek left: not one more (the hunt did
            // not invent frames) and not one fewer (the first frame is emitted
            // as a sample, not eaten to write the header).
            let frames = oracle_stream(&file, track, Some(start_ms));
            let mut source = open_track(Cursor::new(file.clone()), track, start_ms)
                .unwrap_or_else(|_| panic!("track {track} opens at {start_ms} ms"));
            let first = source.next_sample().expect("no error");
            let mut samples: Vec<Sample> = first.into_iter().collect();
            samples.extend(drain_samples(&mut source));
            assert_eq!(
                samples.len(),
                frames.len(),
                "track {track} streams exactly the frames its own seek leaves at {start_ms} ms"
            );
            for (frame, sample) in frames.iter().zip(&samples) {
                assert_eq!(
                    sample.pts,
                    i64::try_from(frame.timestamp * 48_000 / 1_000).expect("dolby ticks"),
                    "track {track} PTS at {start_ms} ms"
                );
                assert_eq!(
                    &sample.data, &frame.data,
                    "the syncframe bytes pass through unchanged"
                );
                assert!(sample.keyframe, "every audio sample is a sync point");
            }
        }
    }
    // An AC-3 track with no frame anywhere: the hunt reaches the end of the
    // stream and the sample entry says, in codec.rs's own words, that it has no
    // frame to read its header from.
    let tracks = ebml(
        &TRACKS,
        &[
            track_element(1, 2, AC3_ID, None, Some((48000.0, 2)), None, None, None),
            track_element(
                2,
                1,
                H264_ID,
                Some(avcc().as_slice()),
                None,
                Some((640, 360)),
                None,
                None,
            ),
        ]
        .concat(),
    );
    let clusters = vec![cluster(0, &[simple_block(2, 0, true, &[0x41])])];
    let file = matroska_file(&indexed_segment(&tracks, 1_000_000, &clusters));
    let error = open_track(Cursor::new(file), 1, 0).expect_err("an AC-3 track with no frames");
    match error {
        MediaError::Unsupported(message) => assert!(
            message.contains("AC-3") || message.contains("frame"),
            "the refusal says what is missing: {message}"
        ),
        other => panic!("an AC-3 track with no frame is Unsupported, got {other:?}"),
    }
}

#[test]
fn remux_too_many_walked_frames_give_up() {
    // The bound on walking without emitting, from both directions: a video
    // track whose first 100k frames are all flagged non-sync (so nothing is
    // emittable and the keyframe never arrives), and an audio track whose
    // frames all sit before a start time past them. Either would otherwise
    // walk a huge file sample by sample forever.
    let video_tracks = ebml(
        &TRACKS,
        &[track_element(
            1,
            1,
            H264_ID,
            Some(avcc().as_slice()),
            None,
            Some((640, 360)),
            None,
            None,
        )]
        .concat(),
    );
    let mut clusters = Vec::new();
    let mut total = 0;
    while total < 100_002 {
        let chunk = 25_000u16;
        let blocks: Vec<Vec<u8>> = (0..chunk)
            .map(|frame| {
                simple_block(
                    1,
                    i16::try_from(frame).expect("a chunk under i16"),
                    false,
                    &[0x41],
                )
            })
            .collect();
        clusters.push(cluster(total, &blocks));
        total += u64::from(chunk);
    }
    let video = matroska_file(&indexed_segment(&video_tracks, 1_000_000, &clusters));
    let mut source = open_track(Cursor::new(video), 1, 0).expect("the header is fine");
    match source.next_sample() {
        Err(SourceError::Malformed(message)) => assert!(
            message.contains("100000"),
            "the walk gave up with the bound it hit: {message}"
        ),
        other => panic!("100k non-sync frames before any keyframe is a verdict, got {other:?}"),
    }
    // The audio direction: the seek's narrow phase walks whole blocks ahead of
    // us, so frames only reach the source's own drop rule when the file's
    // order puts them after the landing point and before the start time in
    // time. The cluster at 500000 ms opens with a frame at or after the start
    // and then counts its remaining frames backwards below it; the clusters
    // behind it (unsorted in time, and with no cue table for the seek to get
    // lost in) are all below the start time as well. The first sample arrives,
    // and then the walk drops more than the bound of frames before it gives up
    // mid-tail. The blocks are chunked into 25k-frame clusters because a
    // block's relative timecode is an i16.
    let audio_tracks = ebml(
        &TRACKS,
        &[track_element(
            2,
            2,
            MP3_ID,
            None,
            Some((44100.0, 2)),
            None,
            None,
            None,
        )]
        .concat(),
    );
    let mut audio_clusters = vec![
        cluster(0, &[simple_block(2, 0, false, &[0xFF, 0xFB, 0x90, 0x64])]),
        cluster(
            350_000,
            &[simple_block(2, 0, false, &[0xFF, 0xFB, 0x90, 0x64])],
        ),
    ];
    let counted: Vec<Vec<u8>> = (0..25_001u16)
        .map(|frame| {
            let offset =
                i16::try_from(10_000i32 - i32::from(frame)).expect("an i16 relative timecode");
            simple_block(2, offset, false, &[0xFF, 0xFB, 0x90, 0x64])
        })
        .collect();
    audio_clusters.push(cluster(500_000, &counted));
    for tail in 0..5u64 {
        let blocks: Vec<Vec<u8>> = (0..25_001i16)
            .map(|frame| simple_block(2, frame, false, &[0xFF, 0xFB, 0x90, 0x64]))
            .collect();
        audio_clusters.push(cluster(100_000 + tail * 50_000, &blocks));
    }
    // No SeekHead and no Cues: the seek falls back to the cluster walk, and
    // either landing - the cluster at the target or the one below it - puts
    // the backwards cluster's tail and the unsorted clusters behind it in the
    // source's way, which is the point.
    let mut segment = audio_tracks;
    segment.extend_from_slice(&info_element(1_000_000));
    for cluster in &audio_clusters {
        segment.extend_from_slice(cluster);
    }
    let before = matroska_file(&segment);
    let mut source =
        open_track(Cursor::new(before), 2, 500_000).expect("the start time is expressible");
    let first = source
        .next_sample()
        .expect("no error")
        .expect("a frame exists at or after the start time");
    assert_eq!(
        first.pts,
        510_000 * 44_100 / 1_000,
        "the first frame at or after the start time is the one the seek landed on"
    );
    // The bound then trips mid-walk, on a later call: the frames between the
    // landing point and the end of the stream all sit before the start time,
    // and there are more than the bound of them.
    let verdict = loop {
        match source.next_sample() {
            Ok(Some(_)) => {}
            Ok(None) => break "clean end of stream".to_string(),
            Err(error) => break error.to_string(),
        }
    };
    assert!(
        verdict.contains("100000"),
        "the walk gave up with the bound it hit: {verdict}"
    );
}
