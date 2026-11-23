//! EBML assembly for the in-memory Matroska files the integration tests
//! build. Copied in shape from `tests/remux.rs`: the fixture files there are
//! mkvmerge's, these inputs are hand-built, because the shapes under test
//! (a subtitle track with cues, a track that never makes a cue) are not
//! things the fixture set carries.

/// Where a track's boxes are and what its sample tables say, for the in-memory MP4 files the
/// mov_text tests build out of the checked-in fixture.
pub mod mp4;

pub const EBML_HEADER: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];
pub const SEGMENT: [u8; 4] = [0x18, 0x53, 0x80, 0x67];
pub const SEEK_HEAD: [u8; 4] = [0x11, 0x4D, 0x9B, 0x74];
pub const SEEK: [u8; 2] = [0x4D, 0xBB];
pub const SEEK_ID: [u8; 2] = [0x53, 0xAB];
pub const SEEK_POSITION: [u8; 2] = [0x53, 0xAC];
pub const DOC_TYPE: [u8; 2] = [0x42, 0x82];
pub const DOC_TYPE_VERSION: [u8; 2] = [0x42, 0x87];
pub const DOC_TYPE_READ_VERSION: [u8; 2] = [0x42, 0x85];
pub const TRACKS: [u8; 4] = [0x16, 0x54, 0xAE, 0x6B];
pub const TRACK_ENTRY: [u8; 1] = [0xAE];
pub const TRACK_NUMBER: [u8; 1] = [0xD7];
pub const TRACK_UID: [u8; 2] = [0x73, 0xC5];
pub const TRACK_TYPE: [u8; 1] = [0x83];
pub const FLAG_DEFAULT: [u8; 1] = [0x88];
pub const CODEC_ID: [u8; 1] = [0x86];
pub const INFO: [u8; 4] = [0x15, 0x49, 0xA9, 0x66];
pub const MULTIPLEXING_APP: [u8; 2] = [0x4D, 0x80];
pub const WRITE_APP: [u8; 2] = [0x57, 0x41];
pub const TIMECODE_SCALE: [u8; 3] = [0x2A, 0xD7, 0xB1];
pub const CLUSTER: [u8; 4] = [0x1F, 0x43, 0xB6, 0x75];
pub const TIMECODE: [u8; 1] = [0xE7];
pub const SIMPLE_BLOCK: [u8; 1] = [0xA3];
pub const BLOCK_GROUP: [u8; 1] = [0xA0];
pub const BLOCK: [u8; 1] = [0xA1];
pub const BLOCK_DURATION: [u8; 1] = [0x9B];
pub const DURATION: [u8; 2] = [0x44, 0x89];
pub const CUES: [u8; 4] = [0x1C, 0x53, 0xBB, 0x6B];
pub const CUE_POINT: [u8; 1] = [0xBB];
pub const CUE_TIME: [u8; 1] = [0xB3];
pub const CUE_TRACK_POSITIONS: [u8; 1] = [0xB7];
pub const CUE_TRACK: [u8; 1] = [0xF7];
pub const CUE_CLUSTER_POSITION: [u8; 1] = [0xF1];
pub const CUE_RELATIVE_POSITION: [u8; 1] = [0xF0];

/// A `SimpleBlock` for track `track`, `offset` scale-units after its cluster's
/// timecode. Track 1..9 are one-byte VINTs; the keyframe flag is the flag
/// byte's high bit, and lacing bits stay 0 (a plain single-frame block).
pub fn simple_block(track: u64, offset: i16, keyframe: bool, payload: &[u8]) -> Vec<u8> {
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

/// A `BlockGroup` holding one `Block` (a `SimpleBlock`'s body, track and flags
/// included) and a `BlockDuration`. A `Block` - unlike a `SimpleBlock` - takes
/// no keyframe flag and has no offset of its own; the group's `BlockDuration`
/// is the one the demuxer reports as the frame's duration.
pub fn block_group(track: u64, offset: i16, payload: &[u8], duration_ms: u64) -> Vec<u8> {
    let block = ebml(
        &BLOCK,
        &[
            vec![0x80 | u8::try_from(track).expect("a small track number")],
            offset.to_be_bytes().to_vec(),
            vec![0x00],
            payload.to_vec(),
        ]
        .concat(),
    );
    let body = [block, ebml(&BLOCK_DURATION, &duration_ms.to_be_bytes())].concat();
    ebml(&BLOCK_GROUP, &body)
}

/// A `Cluster`: a timecode and its blocks, in the order given.
pub fn cluster(timecode_ms: u64, blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut body = ebml(&TIMECODE, &timecode_ms.to_be_bytes());
    for block in blocks {
        body.extend_from_slice(block);
    }
    ebml(&CLUSTER, &body)
}

/// A `TrackEntry`: the demuxer requires a number, a UID and a type, and the
/// codec id is what the subtitle reader dispatches on.
pub fn track_entry(number: u64, track_type: u8, codec_id: &[u8]) -> Vec<u8> {
    let body = [
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
    ]
    .concat();
    ebml(&TRACK_ENTRY, &body)
}

/// A `Tracks` element holding the given `TrackEntry` elements.
pub fn tracks(entries: &[Vec<u8>]) -> Vec<u8> {
    ebml(&TRACKS, &entries.concat())
}

/// The `Info` with the scale the synthetic files are cut on.
pub fn info_element(scale: u64) -> Vec<u8> {
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

/// The `Info` with the scale and a `Duration`, in milliseconds of the file's
/// own timeline. The `Duration` counts `TimestampScale` units as a float, so
/// at scale 1 000 000 (one unit per millisecond) the value is the milliseconds.
pub fn info_with_duration(scale: u64, duration_ms: u64) -> Vec<u8> {
    let body = [
        ebml(&MULTIPLEXING_APP, b"fbtt"),
        ebml(&WRITE_APP, b"fbtt"),
        ebml(&TIMECODE_SCALE, &scale.to_be_bytes()),
        ebml(&DURATION, &float64(duration_ms as f64)),
    ]
    .concat();
    ebml(&INFO, &body)
}

/// The IEEE-754 `f64` encoding of `value`, big-endian.
pub fn float64(value: f64) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

pub fn seek_entry(id: &[u8], at: u64) -> Vec<u8> {
    ebml(
        &SEEK,
        &[ebml(&SEEK_ID, id), ebml(&SEEK_POSITION, &at.to_be_bytes())].concat(),
    )
}

pub fn seek_head_named(entries: &[(&[u8], u64)]) -> Vec<u8> {
    let body: Vec<u8> = entries
        .iter()
        .flat_map(|(id, at)| seek_entry(id, *at))
        .collect();
    ebml(&SEEK_HEAD, &body)
}

/// A `Segment` body laid out as `SeekHead`, tracks, info, then the clusters,
/// and (when `cues` is given) the cues after them. The `SeekHead` names the
/// first cluster (and the cues) so `open` indexes like a real file instead of
/// walking the whole stream.
///
/// The head is built once with placeholder offsets to learn its length, then
/// again with the real ones; all elements after it sit at known offsets
/// because everything ahead of them is already assembled.
pub fn indexed_segment(
    tracks: &[u8],
    scale: u64,
    clusters: &[Vec<u8>],
    cues: Option<&[u8]>,
) -> Vec<u8> {
    let info = info_element(scale);
    let cues_len = cues.map_or(0, |cues: &[u8]| cues.len());
    let names: Vec<&[u8]> = if cues.is_some() {
        vec![&TRACKS, &INFO, &CLUSTER, &CUES]
    } else {
        vec![&TRACKS, &INFO, &CLUSTER]
    };
    let placeholder = seek_head_named(&names.iter().map(|id| (*id, 0u64)).collect::<Vec<_>>());
    let tracks_at = u64::try_from(placeholder.len()).expect("a SeekHead under 4 GiB");
    let info_at = tracks_at + u64::try_from(tracks.len()).expect("Tracks under 4 GiB");
    let cluster_at = info_at + u64::try_from(info.len()).expect("Info under 4 GiB");
    let cues_at = cluster_at + u64::try_from(cues_len).expect("clusters under 4 GiB");

    let offsets: Vec<u64> = if cues.is_some() {
        vec![tracks_at, info_at, cluster_at, cues_at]
    } else {
        vec![tracks_at, info_at, cluster_at]
    };
    let head = seek_head_named(
        &names
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, offsets[index]))
            .collect::<Vec<_>>(),
    );
    assert_eq!(head.len(), placeholder.len());

    let mut body = head;
    body.extend_from_slice(tracks);
    body.extend_from_slice(&info);
    for cluster in clusters {
        body.extend_from_slice(cluster);
    }
    if let Some(cues) = cues {
        body.extend_from_slice(cues);
    }
    body
}

/// An EBML file: the magic header and one `Segment`.
pub fn matroska_file(segment_body: &[u8]) -> Vec<u8> {
    [ebml_header(), ebml(&SEGMENT, segment_body)].concat()
}

/// The EBML magic and its three version elements.
pub fn ebml_header() -> Vec<u8> {
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

/// One `Cues` element: a point per cue, each naming its cluster's absolute
/// position and the block's offset within that cluster, the layout ffmpeg
/// writes.
pub fn cues_element(points: &[(u64, u64, u64)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (time, cluster_position, relative) in points {
        let positions = ebml(
            &CUE_TRACK_POSITIONS,
            &[
                ebml(&CUE_TRACK, &[1]),
                ebml(&CUE_CLUSTER_POSITION, &cluster_position.to_be_bytes()),
                ebml(&CUE_RELATIVE_POSITION, &relative.to_be_bytes()),
            ]
            .concat(),
        );
        body.extend_from_slice(&ebml(
            &CUE_POINT,
            &[ebml(&CUE_TIME, &time.to_be_bytes()), positions].concat(),
        ));
    }
    ebml(&CUES, &body)
}

fn ebml(id: &[u8], body: &[u8]) -> Vec<u8> {
    let declared = u64::try_from(body.len()).expect("an element under 4 GiB");
    let mut out = Vec::with_capacity(id.len() + 5 + body.len());
    out.extend_from_slice(id);
    out.extend_from_slice(&vint(declared));
    out.extend_from_slice(body);
    out
}

/// The size prefix for an element of `size` bytes: one byte for sizes of 126
/// or less, the 5-byte `0x08` form above that.
fn vint(size: u64) -> Vec<u8> {
    if size <= 126 {
        return vec![0x80 | u8::try_from(size).expect("a size of 126 or less")];
    }
    let size = u32::try_from(size).expect("an element under 4 GiB");
    let mut out = vec![0x08];
    out.extend_from_slice(&size.to_be_bytes());
    out
}
