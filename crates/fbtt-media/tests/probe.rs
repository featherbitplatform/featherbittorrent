//! `probe` against the checked-in fixtures, and against ffprobe's recorded view
//! of them in `expected.json`.
//!
//! Every fixture is synthetic and `dev/make-media-fixtures.sh` writes
//! `expected.json` by asking ffprobe, so the tables these tests compare against
//! are ffprobe's own answers rather than a transcription of what fbtt-media
//! returns: the two agree because both read the same bytes, and a mapping that
//! drifts from the container's own reading fails here. Where a codec string is
//! asserted in full (`avc1.64000C`), that literal is the RFC 6381 id an HLS
//! manifest has to carry, which no container tag spells out.

use std::fs::{self, File};
use std::io::{BufReader, Cursor, Error as IoError, ErrorKind, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use fbtt_media::{
    CappedLen, Codec, Container, ELEMENT_CAP, MediaError, MediaInfo, TrackKind, probe,
};
use serde_json::Value;

const MKV: &str = "h264-aac-opus-srt-ass.mkv";
const HEVC: &str = "hevc-flac.mkv";
const AV1: &str = "av1-ac3-eac3.mkv";
const MP3: &str = "h264-mp3.mkv";
const LIVE: &str = "h264-live-nocues.mkv";
const WEBM: &str = "vp9-opus.webm";
const FASTSTART: &str = "h264-aac2-movtext.mp4";
const MOOV_LAST: &str = "h264-aac-moovlast.mp4";

/// Every fixture, for the invariants that are meant to hold of all of them.
const EVERY_FIXTURE: [&str; 8] = [MKV, HEVC, AV1, MP3, LIVE, WEBM, FASTSTART, MOOV_LAST];

/// What the synthesis asks for, and how far a container's own answer may sit
/// from it: a `Duration` counts the last sample's end, so ffmpeg is a few
/// milliseconds off the 4 seconds that were drawn.
const FOUR_SECONDS_MS: u64 = 4000;
const TOLERANCE_MS: u64 = 100;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
}

fn fixture(name: &str) -> BufReader<File> {
    BufReader::new(File::open(fixture_path(name)).expect("fixture"))
}

/// ffprobe's recorded view of one fixture, as `dev/make-media-fixtures.sh` wrote it.
fn expected(name: &str) -> &'static Value {
    static ALL: OnceLock<Value> = OnceLock::new();
    let all = ALL.get_or_init(|| {
        let text = fs::read_to_string(fixture_path("expected.json")).expect("expected.json");
        serde_json::from_str(&text).expect("expected.json is JSON")
    });
    &all["fixtures"][name]
}

fn expected_tracks(name: &str) -> &'static [Value] {
    expected(name)["tracks"]
        .as_array()
        .expect("a track table")
        .as_slice()
}

fn want_str(table: &'static Value, key: &str) -> &'static str {
    table[key]
        .as_str()
        .unwrap_or_else(|| panic!("expected.json has no string for {key}"))
}

fn want_opt_str(table: &'static Value, key: &str) -> Option<&'static str> {
    match &table[key] {
        Value::Null => None,
        Value::String(text) => Some(text.as_str()),
        other => panic!("expected.json {key} is {other}, not a string or null"),
    }
}

fn want_bool(table: &Value, key: &str) -> bool {
    table[key]
        .as_bool()
        .unwrap_or_else(|| panic!("expected.json has no bool for {key}"))
}

fn want_u64(table: &Value, key: &str) -> u64 {
    table[key]
        .as_u64()
        .unwrap_or_else(|| panic!("expected.json has no number for {key}"))
}

/// ffprobe names `codec_type` the same way a player names a track, and this is
/// the matching name for `TrackKind`.
fn kind_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "video",
        TrackKind::Audio => "audio",
        TrackKind::Subtitle => "subtitle",
    }
}

/// What `probe` answers for `name`, track by track, against ffprobe's own table.
///
/// The ids, kinds, codec names, languages, titles and flags all come from the
/// file, and both readers are reading the same bytes: this is the check that
/// `probe` is reading the container and not inventing a view of it.
fn assert_tracks(name: &str, info: &MediaInfo) {
    let want = expected_tracks(name);
    assert_eq!(
        info.tracks.len(),
        want.len(),
        "{name}: probe found {} tracks, ffprobe found {}",
        info.tracks.len(),
        want.len()
    );
    assert_eq!(
        info.container.name(),
        want_str(expected(name), "container"),
        "{name}: container"
    );
    for (got, want) in info.tracks.iter().zip(want) {
        let id = want_u64(want, "expect_id");
        assert_eq!(
            u64::from(got.id),
            id,
            "{name}: track ids follow container order"
        );
        assert_eq!(
            kind_name(got.kind),
            want_str(want, "kind"),
            "{name}: track {id} kind"
        );
        assert_eq!(
            got.codec.name(),
            want_str(want, "codec_name"),
            "{name}: track {id} codec"
        );
        assert_eq!(
            got.language.as_deref(),
            want_opt_str(want, "language"),
            "{name}: track {id} language, where `und` means no language"
        );
        assert_eq!(
            got.name.as_deref(),
            want_opt_str(want, "title"),
            "{name}: track {id} title"
        );
        assert_eq!(
            got.default,
            want_bool(want, "default"),
            "{name}: track {id} default"
        );
        assert_eq!(
            got.forced,
            want_bool(want, "forced"),
            "{name}: track {id} forced"
        );
    }
    assert_eq!(
        info.duration_ms,
        expected(name)["duration_ms"].as_u64(),
        "{name}: duration_ms is ffprobe's own number"
    );
}

/// Whether box `a` comes before box `b` in the top level box order `expected.json`
/// recorded from the fixture's own bytes.
fn box_comes_before(name: &str, a: &str, b: &str) -> bool {
    let boxes = expected(name)["boxes"]
        .as_array()
        .expect("a box table for an MP4 fixture");
    let offset_of = |kind: &str| {
        boxes
            .iter()
            .position(|entry| entry["kind"].as_str() == Some(kind))
            .unwrap_or_else(|| panic!("{name}: expected.json records no {kind} box"))
    };
    offset_of(a) < offset_of(b)
}

/// The multi track MKV is the fixture the player's track list is drawn from:
/// every track, in container order, with its language, its flags and its codec
/// string, over the 4 seconds the synthesis drew.
#[test]
fn probe_mkv_lists_every_track() {
    let info = probe(fixture(MKV)).expect("probe");
    assert_tracks(MKV, &info);

    let kinds: Vec<TrackKind> = info.tracks.iter().map(|track| track.kind).collect();
    assert_eq!(
        kinds,
        vec![
            TrackKind::Video,
            TrackKind::Audio,
            TrackKind::Audio,
            TrackKind::Subtitle,
            TrackKind::Subtitle,
        ]
    );
    let codecs: Vec<Codec> = info
        .tracks
        .iter()
        .map(|track| track.codec.clone())
        .collect();
    assert_eq!(
        codecs,
        vec![
            Codec::H264,
            Codec::Aac,
            Codec::Opus,
            Codec::SubRip,
            Codec::Ass
        ]
    );

    // eng and ita on the audio, eng and ita on the subtitles, and no language on
    // a video that states none. ffprobe sees the same five, which is what
    // assert_tracks checked.
    assert_eq!(info.tracks[0].language, None);
    assert_eq!(info.tracks[1].language.as_deref(), Some("eng"));
    assert_eq!(info.tracks[2].language.as_deref(), Some("ita"));
    assert_eq!(info.tracks[3].language.as_deref(), Some("eng"));
    assert_eq!(info.tracks[4].language.as_deref(), Some("ita"));

    // The AAC track is the default audio and the Opus one is not, so a player
    // with no preference picks AAC. The file's default subtitle track is default
    // too, which is what the container says and assert_tracks checked.
    assert!(info.tracks[1].default, "the AAC track is the default");
    assert!(!info.tracks[2].default, "the Opus track is not");

    // 4 seconds, three ways: what the Segment states, what ffprobe measured, and
    // the 40 frames at 10 fps with a keyframe every 10 frames that `-g 10` drew.
    let duration = info.duration_ms.expect("a Duration element");
    assert!(
        duration.abs_diff(FOUR_SECONDS_MS) <= TOLERANCE_MS,
        "duration {duration} ms is not within {TOLERANCE_MS} ms of {FOUR_SECONDS_MS}"
    );
    let video = &expected_tracks(MKV)[0];
    assert_eq!(want_u64(video, "frame_count"), 40, "10 fps for 4 s");
    let keyframes: Vec<u64> = video["keyframe_pts_ms"]
        .as_array()
        .expect("a keyframe table")
        .iter()
        .map(|frame| frame.as_u64().expect("a keyframe PTS"))
        .collect();
    assert_eq!(
        keyframes,
        vec![0, 1000, 2000, 3000],
        "one keyframe a second"
    );

    // The codec strings the avcC and AudioSpecificConfig in this file describe,
    // and nothing for the two text subtitle tracks.
    assert_eq!(info.tracks[0].mime_codecs, ["avc1.64000C"]);
    assert!(info.tracks[0].mime_codecs[0].starts_with("avc1."));
    assert_eq!(info.tracks[1].mime_codecs, ["mp4a.40.2"]);
    assert_eq!(info.tracks[2].mime_codecs, ["opus"]);
    assert!(
        info.tracks[3].mime_codecs.is_empty() && info.tracks[4].mime_codecs.is_empty(),
        "a codec fbtt does not remux carries no codec string"
    );

    // Track titles come from the container and are what a track list shows.
    assert_eq!(
        info.tracks[1].name.as_deref(),
        Some("English commentary"),
        "the AAC track is named"
    );
}

/// The codec strings, read off the codec private data each fixture carries.
#[test]
fn probe_mkv_codec_strings() {
    // hvc1.1.6.L30.90 is this file's hvcC read out per ISO 14496-15: profile 1
    // (Main), compatibility indicators 0x60 reversed to 6, level 30 as L30, and
    // constraint octets 90 00 00 00 00 00 with the trailing zero octets dropped.
    let hevc = probe(fixture(HEVC)).expect("probe");
    assert_eq!(hevc.tracks[0].codec, Codec::Hevc);
    assert!(hevc.tracks[0].mime_codecs[0].starts_with("hvc1."));
    assert_eq!(hevc.tracks[0].mime_codecs, ["hvc1.1.6.L30.90"]);
    // FLAC names itself twice, the ISO name and the fourcc, because that is what
    // a browser and what an MP4 sample entry each ask for.
    assert_eq!(hevc.tracks[1].codec, Codec::Flac);
    assert_eq!(hevc.tracks[1].mime_codecs, ["flac", "fLaC"]);

    // av01.0.00M.08: seq_profile 0 (Main), level index 0 as two digits with the
    // tier letter M, 8 bit. Then the two Dolby tracks, which the browser knows
    // as ac-3 and ec-3.
    let av1 = probe(fixture(AV1)).expect("probe");
    assert_eq!(av1.tracks[0].codec, Codec::Av1);
    assert!(av1.tracks[0].mime_codecs[0].starts_with("av01.0."));
    assert_eq!(av1.tracks[0].mime_codecs, ["av01.0.00M.08"]);
    assert_eq!(av1.tracks[1].codec, Codec::Ac3);
    assert_eq!(av1.tracks[1].mime_codecs, ["ac-3"]);
    assert_eq!(av1.tracks[2].codec, Codec::Eac3);
    assert_eq!(av1.tracks[2].mime_codecs, ["ec-3"]);

    // MP3 in a Matroska file is the MPEG-2 object type, not the MPEG-4 one.
    let mp3 = probe(fixture(MP3)).expect("probe");
    assert_eq!(mp3.tracks[1].codec, Codec::Mp3);
    assert_eq!(mp3.tracks[1].mime_codecs, ["mp4a.6B"]);

    // The codec names are the names ffprobe gives the same tracks.
    for (name, info) in [(HEVC, &hevc), (AV1, &av1), (MP3, &mp3)] {
        assert_tracks(name, info);
    }

    // Of every fixture: a codec string exists exactly when fbtt can remux the
    // track, which is the same decision as "this track can be streamed".
    for name in EVERY_FIXTURE {
        let info = probe(fixture(name)).expect("probe");
        for track in &info.tracks {
            assert_eq!(
                track.mime_codecs.is_empty(),
                !track.codec.remuxable(),
                "{name}: {} carries {}/{}, remuxable says {}",
                track.codec.name(),
                track.mime_codecs.len(),
                track.mime_codecs.join(","),
                track.codec.remuxable(),
            );
        }
    }
}

/// Both MP4 layouts, because the probe has to find the track table wherever it
/// is: `moov` before `mdat` is what faststart writes for a browser, and `moov`
/// after `mdat` is what the muxer writes by default.
#[test]
fn probe_mp4_faststart_and_moov_last() {
    let fast = probe(fixture(FASTSTART)).expect("faststart probe");
    let last = probe(fixture(MOOV_LAST)).expect("moov last probe");

    assert_eq!(fast.container, Container::Mp4);
    assert_eq!(last.container, Container::Mp4);
    // Four tracks in the faststart file, two in the other, both as ffprobe
    // lists them: the same table either way, because seeking finds `moov` in
    // both positions.
    assert_tracks(FASTSTART, &fast);
    assert_tracks(MOOV_LAST, &last);

    assert!(
        box_comes_before(FASTSTART, "moov", "mdat"),
        "{FASTSTART}: faststart puts moov before mdat"
    );
    assert!(
        !box_comes_before(MOOV_LAST, "moov", "mdat"),
        "{MOOV_LAST}: with no faststart, moov follows mdat"
    );

    for info in [&fast, &last] {
        let duration = info.duration_ms.expect("an mvhd duration");
        assert!(
            duration.abs_diff(FOUR_SECONDS_MS) <= TOLERANCE_MS,
            "duration {duration} ms is not within {TOLERANCE_MS} ms of {FOUR_SECONDS_MS}"
        );
        assert_eq!(info.tracks[0].mime_codecs, ["avc1.64000C"]);
        assert_eq!(info.tracks[1].mime_codecs, ["mp4a.40.2"]);
    }

    // The mov_text track is a text subtitle, so the cue endpoint can serve it
    // from the sample table. It carries no codec string: fbtt copies subtitle
    // tracks as cues, not as samples.
    let text = fast
        .tracks
        .iter()
        .find(|track| track.kind == TrackKind::Subtitle)
        .expect("the mov_text track");
    assert_eq!(text.codec, Codec::MovText);
    assert!(text.codec.text_subtitle());
    assert!(text.mime_codecs.is_empty());
    assert_eq!(text.language.as_deref(), Some("eng"));

    // The untagged audio track states no language, the one tagged `ita` does,
    // and the muxer left the Italian alternative selectable but not the one a
    // player picks on its own: `tkhd.enabled` is what says so, and ffprobe
    // reports the same default track from the same box.
    assert_eq!(fast.tracks[1].language, None);
    assert_eq!(fast.tracks[2].language.as_deref(), Some("ita"));
    assert!(fast.tracks[1].default, "the English audio is the default");
    assert!(
        !fast.tracks[2].default,
        "the Italian audio is in the movie and selectable, not the default"
    );
}

/// WebM is the EBML DocType and not the file name: the reader that answers `webm`
/// for a DocType of `webm` answers `matroska` for a DocType of `matroska`.
#[test]
fn probe_webm_is_webm() {
    let info = probe(fixture(WEBM)).expect("probe");
    assert_eq!(info.container, Container::WebM);
    assert_eq!(
        info.container.name(),
        want_str(expected(WEBM), "container"),
        "the name a player puts in a Content-Type"
    );
    assert_tracks(WEBM, &info);

    let codecs: Vec<Codec> = info
        .tracks
        .iter()
        .map(|track| track.codec.clone())
        .collect();
    assert_eq!(codecs, vec![Codec::Vp9, Codec::Opus]);

    let duration = info.duration_ms.expect("a Duration element");
    assert!(
        duration.abs_diff(FOUR_SECONDS_MS) <= TOLERANCE_MS,
        "duration {duration} ms is not within {TOLERANCE_MS} ms of {FOUR_SECONDS_MS}"
    );

    // VP9 is not remuxed, so the browser never gets a codec string for it, while
    // Opus is the one WebM is played with.
    assert_eq!(info.tracks[0].codec, Codec::Vp9);
    assert!(info.tracks[0].mime_codecs.is_empty());
    assert_eq!(info.tracks[1].mime_codecs, ["opus"]);

    // The same code path over a matroska DocType stays matroska, and the live
    // fixture, which has no Cues at all, is matroska too.
    let mkv = probe(fixture(MKV)).expect("probe");
    assert_eq!(mkv.container, Container::Matroska);
    assert_eq!(mkv.container.name(), "matroska");
    let live = probe(fixture(LIVE)).expect("probe");
    assert_eq!(live.container, Container::Matroska);
}

/// Bytes that are not one of the three containers are `Unsupported`, and so is a
/// file that starts like an MP4 and never reaches its track table: nothing was
/// wrong with a header, there was no header to be wrong.
#[test]
fn probe_refuses_unknown_bytes() {
    // A kilobyte of zeros: no EBML magic, no ftyp at offset 4.
    match probe(Cursor::new(vec![0u8; 1024])) {
        Err(MediaError::Unsupported(what)) => assert!(
            what.starts_with("not a Matroska"),
            "the message should say what was refused: {what}"
        ),
        other => panic!("a kilobyte of zeros must be Unsupported, got {other:?}"),
    }

    // Three bytes cannot even hold the sniff.
    match probe(Cursor::new(vec![0u8; 3])) {
        Err(MediaError::Unsupported(what)) => {
            assert_eq!(what, "too short to hold a container header")
        }
        other => panic!("three bytes must be Unsupported, got {other:?}"),
    }

    // A real MP4 header shape, an ftyp box and a free box, and then the file
    // ends before any moov.
    let mut no_moov: Vec<u8> = Vec::new();
    no_moov.extend_from_slice(&(24_u32).to_be_bytes());
    no_moov.extend_from_slice(b"ftypisom");
    no_moov.extend_from_slice(&[0_u8; 4]);
    no_moov.extend_from_slice(b"isomiso2");
    no_moov.extend_from_slice(&(8_u32).to_be_bytes());
    no_moov.extend_from_slice(b"free");
    match probe(Cursor::new(no_moov)) {
        Err(MediaError::Unsupported(what)) => assert!(
            what.contains("moov"),
            "the message should name what is missing: {what}"
        ),
        other => panic!("an MP4 with no moov box must be Unsupported, got {other:?}"),
    }
}

/// A container cut short in its header is an error and never a panic: either a
/// Matroska parse that does not hold together, or the reader's own
/// `UnexpectedEof`. A head too short to hold the sniff is `Unsupported`, because
/// nothing in it has claimed to be Matroska yet.
#[test]
fn probe_truncated_mkv_is_an_error() {
    let all = fs::read(fixture_path(MKV)).expect("fixture");
    let head = |len: usize| Cursor::new(all[..len].to_vec());

    // One byte is not a container, and four bytes is the EBML magic alone, which
    // is not enough to read a header or a DocType from.
    for len in [1_usize, 4] {
        match probe(head(len)) {
            Err(MediaError::Unsupported(what)) => assert!(!what.is_empty()),
            other => panic!("a {len} byte head must be Unsupported, got {other:?}"),
        }
    }

    // 12 bytes stops inside the EBML header and 300 bytes ends before the first
    // cluster: both claim to be Matroska and stop short of being one.
    for len in [12_usize, 300] {
        match probe(head(len)) {
            Err(MediaError::Malformed(what)) => assert!(!what.is_empty()),
            Err(MediaError::Io(err)) => assert_eq!(
                err.kind(),
                ErrorKind::UnexpectedEof,
                "a read failure at {len} bytes is the stream ending: {err}"
            ),
            other => panic!("a {len} byte head must be Malformed or UnexpectedEof, got {other:?}"),
        }
    }
}

/// A Matroska element states its size before it carries its contents, so the one
/// defence against a 2 GiB `CodecPrivate` in a 100 byte file is the answer the
/// demuxer gets when it asks how much room is left. `CappedLen` answers that
/// question, and `probe` hands every Matroska reader through it.
#[test]
fn capped_len_bounds_huge_elements() {
    // A 1000 byte reader and a 100 byte cap. From position 0, End(0) reports the
    // cap rather than the 1000 bytes that are really there.
    let mut capped = CappedLen::new(Cursor::new(vec![0_u8; 1000]), 100);
    assert_eq!(capped.seek(SeekFrom::Start(0)).expect("seek"), 0);
    assert_eq!(capped.seek(SeekFrom::End(0)).expect("End(0)"), 100);
    // The reader is left where it said it was, so its position and the length it
    // reported agree, which is what `remaining_len` measures.
    assert_eq!(capped.stream_position().expect("position"), 100);
    // Near the real end, the real end is the smaller of the two answers.
    assert_eq!(capped.seek(SeekFrom::Start(950)).expect("seek"), 950);
    assert_eq!(capped.seek(SeekFrom::End(0)).expect("End(0)"), 1000);
    // End(n) with n != 0 is a position in the real file, past the cap and all.
    assert_eq!(capped.seek(SeekFrom::End(-10)).expect("End(-10)"), 990);
    // Start passes straight through, even past the cap: that is how a demuxer
    // skips an element it has decided not to read.
    assert_eq!(capped.seek(SeekFrom::Start(1500)).expect("seek"), 1500);
    // Reads pass straight through too, and stop at the real end of the reader.
    let mut buf = [0_u8; 16];
    assert_eq!(capped.read(&mut buf).expect("read"), 0);

    // Now a file that lies, inside a stream 3 GiB long. The length is what makes
    // a 2 GiB element plausible, and the bytes are never materialised, so a
    // demuxer that allocates on the strength of a declaration shows up in the
    // count of what was read.
    let read_so_far = Arc::new(AtomicU64::new(0));
    let stream = LazyZeros::new(
        lie_about_one_element(),
        3 * 1024 * 1024 * 1024,
        Arc::clone(&read_so_far),
    );
    let error = match probe(stream) {
        Ok(info) => panic!("a 2 GiB element in a 3 GiB stream must be refused, got {info:?}"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        matches!(error, MediaError::Malformed(_)),
        "a size that contradicts the room available is Malformed, got {error:?}"
    );
    assert!(
        message.contains(TWO_GIB.to_string().as_str()),
        "the refusal should name the size the file declared: {message}"
    );
    assert!(
        message.contains(ELEMENT_CAP.to_string().as_str()),
        "the refusal should report the capped room, not the 3 GiB the stream really is: {message}"
    );

    // A little over a hundred bytes were read, so nothing remotely 2 GiB was
    // allocated either: the refusal happened on the declaration.
    let read = read_so_far.load(Ordering::Relaxed);
    assert!(
        read < 1 << 20,
        "probe read {read} bytes of a stream whose element claimed {TWO_GIB}"
    );
}

/// 2 GiB, which is more than [`ELEMENT_CAP`] and so more than a Matroska element
/// may be given.
const TWO_GIB: u64 = 2 * 1024 * 1024 * 1024;

/// A stream of `len` bytes that are lazily zero, whose first bytes are `header`.
///
/// The 3 GiB length is the point: long enough that a 2 GiB element is plausible,
/// and the bytes are never materialised, so what a demuxer asks for is visible in
/// the counter it shares with the test.
struct LazyZeros {
    header: Vec<u8>,
    len: u64,
    pos: u64,
    read: Arc<AtomicU64>,
}

impl LazyZeros {
    fn new(header: Vec<u8>, len: u64, read: Arc<AtomicU64>) -> Self {
        Self {
            header,
            len,
            pos: 0,
            read,
        }
    }
}

impl Read for LazyZeros {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        let from = self.pos.min(self.len);
        let left = self.len.saturating_sub(self.pos);
        // A read asks for at most its buffer, and it never runs past the end of
        // the stream, which is what makes the tail of a long stream an EOF.
        let wanted = u64::try_from(buf.len()).expect("a buffer fits in a u64");
        let count = usize::try_from(wanted.min(left)).expect("a read is no longer than its buffer");
        for (offset, byte) in buf[..count].iter_mut().enumerate() {
            let at = usize::try_from(from + offset as u64).expect("an offset inside the stream");
            *byte = self.header.get(at).copied().unwrap_or_default();
        }
        self.pos += count as u64;
        self.read.fetch_add(count as u64, Ordering::Relaxed);
        Ok(count)
    }
}

impl Seek for LazyZeros {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, IoError> {
        let from = |base: u64, offset: i64| {
            i64::try_from(base)
                .ok()
                .and_then(|base| base.checked_add(offset))
                .filter(|moved| *moved >= 0)
                .map(|moved| u64::try_from(moved).expect("a position in the stream"))
                .ok_or_else(|| IoError::new(ErrorKind::InvalidInput, "seek outside the stream"))
        };
        self.pos = match pos {
            SeekFrom::Start(at) => at,
            SeekFrom::Current(by) => from(self.pos, by)?,
            SeekFrom::End(by) => from(self.len, by)?,
        };
        Ok(self.pos)
    }
}

/// An EBML size: the plain number behind a length marker byte, which is how
/// libebml writes them and how `matroska-demuxer` reads them. 126 and under take
/// one byte, `0x80 | n`; anything larger takes the five byte form, which is all
/// this test needs and is enough for any size up to 4 GiB.
fn vint(size: u64) -> Vec<u8> {
    if size <= 126 {
        return vec![0x80 | u8::try_from(size).expect("a size of 126 or less")];
    }
    let size = u32::try_from(size).expect("a size of 4 GiB or less");
    let mut out = vec![0x08];
    out.extend_from_slice(&size.to_be_bytes());
    out
}

/// An EBML element whose children or contents are `body`, with the size that
/// `body` actually has.
fn ebml(id: &[u8], body: &[u8]) -> Vec<u8> {
    ebml_declaring(
        id,
        u64::try_from(body.len()).expect("an element under 4 GiB"),
        body,
    )
}

/// An EBML element that declares `declared` bytes of contents and then carries
/// `body`. The two differ in the fixture this builds, which is the whole point:
/// a file may claim more than it holds.
fn ebml_declaring(id: &[u8], declared: u64, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(id.len() + 5 + body.len());
    out.extend_from_slice(id);
    out.extend_from_slice(&vint(declared));
    out.extend_from_slice(body);
    out
}

/// A Matroska file that claims a 2 GiB `CodecPrivate` on its only track.
///
/// The EBML header, the `SeekHead` and the `Info` are what
/// `MatroskaFile::open` walks before it looks at tracks, and the empty `Cluster`
/// is what stops the search for a first cluster from failing first, so the
/// biggest number the reader is asked to believe is the `CodecPrivate`
/// declaration. The declared body is never written: past the header the stream
/// is whatever `LazyZeros` serves.
///
/// A `CodecPrivate` is used because it is a binary element the demuxer knows and
/// reads eagerly, which makes it the one place in a header where a declared size
/// turns into an allocation. The `SeekHead` is here because without one the
/// demuxer builds its own index by walking top level elements to the end of the
/// stream, which on a 3 GiB stream of zeros would read 3 GiB of them and say
/// nothing about the element being refused.
fn lie_about_one_element() -> Vec<u8> {
    let ebml_header = ebml(
        &[0x1A, 0x45, 0xDF, 0xA3],
        &[
            ebml(&[0x42, 0x82], b"matroska"), // DocType
            ebml(&[0x42, 0x87], &[1]),        // DocTypeVersion
            ebml(&[0x42, 0x85], &[1]),        // DocTypeReadVersion
        ]
        .concat(),
    );

    let track_entry = [
        ebml(&[0xD7], &[1]),                         // TrackNumber
        ebml(&[0x73, 0xC5], &[1]),                   // TrackUID
        ebml(&[0x83], &[1]),                         // TrackType: video
        ebml(&[0x86], b"V_MPEG4/ISO/AVC"),           // CodecID
        ebml_declaring(&[0x63, 0xA2], TWO_GIB, &[]), // CodecPrivate: 2 GiB claimed
    ]
    .concat();
    let tracks = ebml(&[0x16, 0x54, 0xAE, 0x6B], &ebml(&[0xAE], &track_entry));
    let info = ebml(
        &[0x15, 0x49, 0xA9, 0x66],
        &[
            ebml(&[0x4D, 0x80], b"fbtt"),                         // MuxingApp
            ebml(&[0x57, 0x41], b"fbtt"),                         // WritingApp
            ebml(&[0x2A, 0xD7, 0xB1], &[0x00, 0x0F, 0x42, 0x40]), // TimestampScale: 1 ms
        ]
        .concat(),
    );
    let cluster = ebml(&[0x1F, 0x43, 0xB6, 0x75], &[]);

    // A SeekHead entry is the same length whatever offset it carries, because
    // every offset in this file fits a single byte, so the head can be built
    // once with placeholders to learn its own length, and the offsets of the
    // three elements it points at then follow from that.
    // The three positions are counted from the start of the Segment's children,
    // which is where the SeekHead sits, so the length of the EBML header and of
    // the Segment's own header never enters them.
    let placeholder = seek_head(0, 0, 0);
    let tracks_at = u64::try_from(placeholder.len()).expect("a SeekHead under 4 GiB");
    let info_at = tracks_at + u64::try_from(tracks.len()).expect("Tracks under 4 GiB");
    let cluster_at = info_at + u64::try_from(info.len()).expect("Info under 4 GiB");

    let head = seek_head(tracks_at, info_at, cluster_at);
    assert_eq!(
        head.len(),
        placeholder.len(),
        "a SeekHead of three one byte positions is its own length whatever they point at"
    );

    let body = [head, tracks, info, cluster].concat();
    assert!(
        body.len() <= 126,
        "the Segment needs a one byte size for the offsets above to hold: {} bytes",
        body.len()
    );

    [ebml_header, ebml(&[0x18, 0x53, 0x80, 0x67], &body)].concat()
}

/// A `SeekHead` pointing at the three top level elements `open` asks for.
///
/// Positions are counted from the start of the Segment's children, which is what
/// EBML says and what `matroska-demuxer` adds the Segment offset back to.
fn seek_head(tracks_at: u64, info_at: u64, cluster_at: u64) -> Vec<u8> {
    let entry = |id: &[u8], at: u64| {
        ebml(
            &[0x4D, 0xBB], // Seek
            &[
                ebml(&[0x53, 0xAB], id), // SeekId, the element id as a number
                ebml(
                    &[0x53, 0xAC],
                    &[u8::try_from(at).expect("a SeekPosition under 256")],
                ),
            ]
            .concat(),
        )
    };
    ebml(
        &[0x11, 0x4D, 0x9B, 0x74],
        &[
            entry(&[0x16, 0x54, 0xAE, 0x6B], tracks_at),
            entry(&[0x15, 0x49, 0xA9, 0x66], info_at),
            entry(&[0x1F, 0x43, 0xB6, 0x75], cluster_at),
        ]
        .concat(),
    )
}
