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
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use fbtt_media::{
    CappedLen, Codec, Container, ELEMENT_CAP, MAX_HEADER_OPS, MediaError, MediaInfo, ReadBudget,
    TrackKind, probe,
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
    let mut capped = CappedLen::new(
        Cursor::new(vec![0_u8; 1000]),
        100,
        ReadBudget::new(ELEMENT_CAP),
    );
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
    let stream = LazyZeros::new(lie_about_one_element(), THREE_GIB, Arc::clone(&read_so_far));
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
    // The room the refusal quotes is the one the capped reader reported: a hair under
    // `ELEMENT_CAP`, because the budget has paid for the headers read on the way here, and not
    // the 3 GiB the stream really is. The declared 2 GiB is larger than a header may be, so the
    // only number in the message that fits under the cap is the measured room.
    let quoted = message
        .split(|byte: char| !byte.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok())
        .filter(|value| *value <= ELEMENT_CAP)
        .max()
        .unwrap_or_else(|| panic!("the refusal should quote the room it measured: {message}"));
    assert!(
        quoted > ELEMENT_CAP - (1 << 20),
        "the refusal quotes {quoted} bytes of room, which is not the capped room near {ELEMENT_CAP}: {message}"
    );
    assert!(
        !message.contains(THREE_GIB.to_string().as_str()),
        "the refusal should report the capped room, not the {THREE_GIB} bytes the stream really is: {message}"
    );

    // A little over a hundred bytes were read, so nothing remotely 2 GiB was
    // allocated either: the refusal happened on the declaration.
    let read = read_so_far.load(Ordering::Relaxed);
    assert!(
        read < 1 << 20,
        "probe read {read} bytes of a stream whose element claimed {TWO_GIB}"
    );
}

/// The cap bounds one element. A header is a list of them.
///
/// Eight `TrackEntry` elements each declare a 200 MiB `CodecPrivate`, which is under
/// [`ELEMENT_CAP`] and so a claim the cap alone waves through. `matroska-demuxer` measures the
/// room for an element from that element's own position and never from the end of the element
/// that contains it, so all eight claims are measured against the same part of the stream, and a
/// reader that bounds one element at a time is asked for 1.6 GiB of it. [`ReadBudget`] is the
/// number that is not per element, and `probe` hands a header one element's worth of it, which
/// is what makes the refusal land on the second track instead of after the eighth.
#[test]
fn capped_len_bounds_total_header_reads() {
    let read_so_far = Arc::new(AtomicU64::new(0));
    let stream = LazyZeros::new(
        matroska_file(&indexed_segment(tracks_claiming(8, TWO_HUNDRED_MIB))),
        FOUR_GIB,
        Arc::clone(&read_so_far),
    );

    let outcome = probe(stream);
    let read = read_so_far.load(Ordering::Relaxed);

    let error = match outcome {
        Ok(info) => panic!(
            "eight claims of {TWO_HUNDRED_MIB} bytes are more than a {ELEMENT_CAP} byte header \
             budget: probe accepted {} tracks",
            info.tracks.len()
        ),
        Err(error) => error,
    };
    assert!(
        read <= ELEMENT_CAP + (1 << 20),
        "eight claims of {TWO_HUNDRED_MIB} bytes are more than a {ELEMENT_CAP} byte header \
         budget: {read} bytes were read"
    );
    assert!(
        matches!(error, MediaError::Malformed(_)),
        "a header that outclaims the budget it is read under is Malformed, got {error:?}"
    );
}

/// A `SeekHead` carrying two `Seek` entries that name a `SeekHead`.
///
/// `parse_seek_head` re-reads the head an entry names and queues the head it finds for a second
/// pass, and it keeps room for one such queue: getting a second one is an `expect_err`
/// (`lib.rs:1916`), which is a panic rather than an error, and a file reaches it with nothing
/// else wrong with it. A torrent's file is attacker controlled, so every Matroska open in fbtt
/// runs inside `catch_unwind` and the panic comes back as the refusal the file deserves.
#[test]
fn probe_seekhead_twice_is_malformed() {
    let entry = seek_entry(&SEEK_HEAD, 0);
    let head = ebml(&SEEK_HEAD, &[entry.clone(), entry].concat());

    match probe(Cursor::new(matroska_file(&head))) {
        Ok(info) => panic!("a SeekHead naming two SeekHeads was accepted: {info:?}"),
        Err(MediaError::Malformed(what)) => assert_eq!(
            what, "matroska header",
            "a panic inside the header parse is reported as a malformed header"
        ),
        Err(error) => panic!(
            "a SeekHead naming two SeekHeads is a panic the reader turns into Malformed, got {error:?}"
        ),
    }
}

/// A `SeekPosition` so large that the Segment's own offset no longer fits beside it.
///
/// `matroska-demuxer` adds the file's number to the segment offset where it stands
/// (`lib.rs:1903`, and `lib.rs:1499` does the same to cue positions), so in the debug build
/// `cargo test` runs an overflowing add is a panic. This value is sixteen bytes short of
/// `u64::MAX`: enough to overflow the addition, not enough for the position to mean anything.
#[test]
fn probe_seek_position_overflow_is_malformed() {
    let far = seek_entry(&SEEK_HEAD, 0xFFFF_FFFF_FFFF_FFF0);
    let head = ebml(&SEEK_HEAD, &[far].concat());

    match probe(Cursor::new(matroska_file(&head))) {
        Ok(info) => panic!("a SeekPosition of 0xFFFFFFFFFFFFFFF0 was accepted: {info:?}"),
        // What the file gets is a refusal either way: the debug build catches the overflowing
        // add and reports "matroska header", and a release build wraps it and refuses whatever
        // position the wrapped sum lands on. Accept any Malformed; a panic escaping the open or
        // an acceptance of the file is what this test is about, and both still fail here.
        Err(MediaError::Malformed(_)) => {}
        Err(error) => panic!(
            "a SeekPosition that overflows the offset arithmetic is Malformed, got {error:?}"
        ),
    }
}

/// A `SeekHead` whose only entry names the `SeekHead` it sits in, at its own position.
///
/// The loop at `lib.rs:1893-1925` re-reads the head an entry names and then the entries of the
/// head it queued, so an entry naming its own head has no exit and nothing about it is illegal.
/// It is also cheap per pass: the whole file is about fifty bytes, a pass reads a few dozen of
/// them, and a byte budget of [`ELEMENT_CAP`] alone would allow around 520 million passes before
/// it said no, which is hours on a blocking reader. The call budget is what ends this: a
/// [`ReadBudget`] counts [`MAX_HEADER_OPS`] read and seek calls beside its bytes.
///
/// Run in a thread with a five second channel timeout: the failure mode is a loop that does
/// return, just far too slowly, and only a wall clock can tell that from one that does not.
#[test]
fn probe_self_referencing_seekhead_ends() {
    let head = ebml(&SEEK_HEAD, &seek_entry(&SEEK_HEAD, 0));
    let calls = Arc::new(AtomicU64::new(0));
    let stream = CountCalls::new(Cursor::new(matroska_file(&head)), Arc::clone(&calls));

    let (tx, rx) = channel();
    thread::spawn(move || {
        let outcome = probe(stream);
        let _ = tx.send(outcome);
    });

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(info)) => panic!("a SeekHead pointing at itself was accepted: {info:?}"),
        Ok(Err(MediaError::Malformed(what))) => {
            assert!(
                !what.is_empty(),
                "the refusal should say what it refused: {what}"
            );
            let calls = calls.load(Ordering::Relaxed);
            // What the budget allows is MAX_HEADER_OPS CappedLen calls; what the reader under
            // it counts is more, because one CappedLen::seek(End(0)) is about three calls there
            // and a sniff or two comes free with the probe. Four times is the ceiling the
            // shapes leave room for.
            assert!(
                calls <= MAX_HEADER_OPS * 4,
                "the call budget is what ends this loop, and {calls} inner calls is more than \
                 {MAX_HEADER_OPS} CappedLen calls can make"
            );
        }
        Ok(Err(error)) => panic!("a budget spent inside a header read is Malformed, got {error:?}"),
        Err(RecvTimeoutError::Timeout) => panic!(
            "a SeekHead pointing at itself did not end within 5 seconds, which is the hang the \
             call budget exists to end"
        ),
        Err(RecvTimeoutError::Disconnected) => {
            panic!("the thread reading the self referencing SeekHead died instead of answering")
        }
    }
}

/// 20000 `CuePoint`s, which is more than a 2 hour file with one a second carries.
///
/// `MatroskaFile::open` parses the whole `Cues` table the moment a `SeekHead` entry points at
/// one (`lib.rs:1489`), and every cue point costs a handful of read and seek calls, so the call
/// budget has to fit a cue heavy file with room to spare rather than merely refuse a looping
/// one. The lower bound on the call count is the guard that this fixture really put a cue
/// table in front of the parser: cues the file never shows it would pass this test on 50 calls.
#[test]
fn probe_many_cue_points_is_ok() {
    let calls = Arc::new(AtomicU64::new(0));
    let file = matroska_file(&indexed_segment_with_cues(
        &ebml(&TRACKS, &plain_track()),
        20_000,
    ));
    let stream = CountCalls::new(Cursor::new(file), Arc::clone(&calls));

    let outcome = probe(stream);
    let calls = calls.load(Ordering::Relaxed);

    match outcome {
        Ok(info) => {
            assert_eq!(
                info.tracks.len(),
                1,
                "the one video track is still reported"
            );
            assert!(
                calls >= 20_000,
                "the cue table is parsed at open (lib.rs:1489), so {calls} calls says it was read"
            );
        }
        Err(error) => panic!(
            "20000 cue points are a fraction of the {MAX_HEADER_OPS} call budget, and the probe \
             refused with {error} after {calls} calls"
        ),
    }
}

/// A `moov` with no declared size in a stream longer than a header may be.
///
/// A box size of 0 is how ISO BMFF says the contents run to the end of the file, and `mp4_atom`
/// reports it as `None` (`header.rs:92`); `Header::read_body` then copies the stream with
/// `io::copy` and no bound whatsoever (`header.rs:126`), so a `moov` claiming the rest of a big
/// file is read as the rest of a big file. The run to the end is measured here instead and
/// refused when it is longer than a header.
#[test]
fn probe_mp4_moov_to_eof_is_bounded() {
    let read_so_far = Arc::new(AtomicU64::new(0));
    let stream = LazyZeros::new(moov_running_to_the_end(), ONE_GIB, Arc::clone(&read_so_far));
    let outcome = probe(stream);
    let read = read_so_far.load(Ordering::Relaxed);

    let error = match outcome {
        Ok(info) => panic!(
            "a moov running to the end of a {ONE_GIB} byte stream must be refused on its length: \
             probe accepted {} after {read} bytes",
            info.container.name()
        ),
        Err(error) => error,
    };
    assert!(
        read < 1 << 20,
        "a moov running to the end of a {ONE_GIB} byte stream must be refused on its length: \
         probe read {read} bytes"
    );
    assert!(
        matches!(error, MediaError::Malformed(_)),
        "a moov whose run to the end of the file is longer than a header is Malformed, got {error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains(ELEMENT_CAP.to_string().as_str()),
        "the refusal should name the cap the run broke: {message}"
    );
}

/// A `moov` declaring 2 GiB of contents in a stream that is 8 MiB long.
///
/// The stream is the short one, on purpose: what is being refused is the declaration. On a stream
/// that could hold 2 GiB, a version of the check that only noticed the bytes on hand would pass
/// this test while still reading 2 GiB of them.
#[test]
fn probe_mp4_moov_over_cap_is_refused() {
    let read_so_far = Arc::new(AtomicU64::new(0));
    let stream = LazyZeros::new(
        moov_declaring_two_gib(),
        EIGHT_MIB,
        Arc::clone(&read_so_far),
    );
    let outcome = probe(stream);
    let read = read_so_far.load(Ordering::Relaxed);

    let error = match outcome {
        Ok(info) => panic!(
            "a moov declaring {TWO_GIB} bytes must be refused on its size: probe accepted {} after \
             {read} bytes",
            info.container.name()
        ),
        Err(error) => error,
    };
    assert!(
        read < 1 << 20,
        "a moov declaring {TWO_GIB} bytes must be refused on its size: probe read {read} bytes"
    );
    assert!(
        matches!(error, MediaError::Malformed(_)),
        "a moov declaring more than an element may be given is Malformed, got {error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains(ELEMENT_CAP.to_string().as_str()),
        "the refusal should name the cap the declaration broke: {message}"
    );
}

/// 5000 eight byte `free` boxes, and no `moov` anywhere in the file.
///
/// Skipping a box is a seek, so the walk is cheap per box, but how many there are is the file's
/// choice, not ours: 4 GiB of 8 byte `free` boxes is 512 million headers read before the stream
/// runs out, and a file with no `moov` in it walks every one of them.
#[test]
fn mp4_walk_stops_after_box_limit() {
    let file = five_thousand_free_boxes();
    let read_so_far = Arc::new(AtomicU64::new(0));
    let len = u64::try_from(file.len()).expect("the fixture is under 4 GiB");
    let outcome = probe(LazyZeros::new(file, len, Arc::clone(&read_so_far)));

    match outcome {
        Ok(info) => panic!("a file of 5000 boxes with no moov was accepted: {info:?}"),
        Err(MediaError::Malformed(what)) => assert!(
            what.contains("4096"),
            "the refusal should name the box limit the walk stopped at: {what}"
        ),
        Err(error) => panic!(
            "a walk of 5000 boxes with no moov must stop at the 4096 box limit as Malformed, got \
             {error:?}"
        ),
    }

    // The walk stopped, so it read a header for the 4097 boxes it gave up on rather than for all
    // 5001 the file holds.
    let read = read_so_far.load(Ordering::Relaxed);
    assert!(
        read < 4500 * 8,
        "the walk gave up after 4096 boxes, and read {read} bytes of box headers doing it"
    );
}

/// 2 GiB, which is more than [`ELEMENT_CAP`] and so more than a Matroska element
/// may be given.
const TWO_GIB: u64 = 2 * 1024 * 1024 * 1024;

/// 3 GiB, the stream these fixtures are laid on: long enough that a claim of 2 GiB is
/// something a demuxer measuring the stream would believe.
const THREE_GIB: u64 = 3 * 1024 * 1024 * 1024;

/// 4 GiB, long enough that eight claims of 200 MiB are all plausible for the stream.
const FOUR_GIB: u64 = 4 * 1024 * 1024 * 1024;

/// 1 GiB: more than a header may be, and little enough that a reader which ignores that reads it.
const ONE_GIB: u64 = 1024 * 1024 * 1024;

/// 8 MiB, the stream a declared 2 GiB sits in: too short to hold the claim, long enough that
/// reading it instead of refusing it is visible in a byte count.
const EIGHT_MIB: u64 = 8 * 1024 * 1024;

/// 200 MiB: under [`ELEMENT_CAP`], so a claim this big is something the cap alone accepts,
/// which is what makes it the size to amplify with.
const TWO_HUNDRED_MIB: u64 = 200 * 1024 * 1024;

/// A stream of `len` bytes that are lazily zero, whose first bytes are `header`.
///
/// The length is the point: long enough that the claims the fixture makes are plausible
/// for the stream, and the bytes are never materialised, so what a demuxer asks for is
/// visible in the counter it shares with the test.
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
        // Past the bytes the fixture wrote the stream is zeros, and a header that claims a
        // few hundred megabytes of them asks for them in one read: filling that region is
        // what makes handing it out cheap instead of a copy loop per megabyte.
        let written = (self.header.len() as u64).saturating_sub(from);
        let written = usize::try_from(wanted.min(left).min(written))
            .expect("a read is no longer than its buffer");
        for (offset, byte) in buf[..written].iter_mut().enumerate() {
            let at = usize::try_from(from + offset as u64).expect("an offset inside the stream");
            *byte = self.header[at];
        }
        buf[written..count].fill(0);
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

/// A reader that counts the `read` and `seek` calls made on it.
///
/// A byte counter cannot see a loop that reads thirty bytes at a time forever: the call budget
/// is about how many times a file gets to ask, so the tests that bound calls count asks.
struct CountCalls<R> {
    inner: R,
    calls: Arc<AtomicU64>,
}

impl<R> CountCalls<R> {
    fn new(inner: R, calls: Arc<AtomicU64>) -> Self {
        Self { inner, calls }
    }
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

/// The element ids these fixtures are assembled from, spelled as EBML spells them.
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
const CODEC_ID: [u8; 1] = [0x86];
const CODEC_PRIVATE: [u8; 2] = [0x63, 0xA2];
const INFO: [u8; 4] = [0x15, 0x49, 0xA9, 0x66];
const MULTIPLEXING_APP: [u8; 2] = [0x4D, 0x80];
const WRITE_APP: [u8; 2] = [0x57, 0x41];
const TIMECODE_SCALE: [u8; 3] = [0x2A, 0xD7, 0xB1];
const CLUSTER: [u8; 4] = [0x1F, 0x43, 0xB6, 0x75];
const CUES: [u8; 4] = [0x1C, 0x53, 0xBB, 0x6B];
const CUE_POINT: [u8; 1] = [0xBB];
const CUE_TIME: [u8; 1] = [0xB3];
const CUE_TRACK_POSITIONS: [u8; 1] = [0xB7];
const CUE_TRACK: [u8; 1] = [0xF7];
const CUE_CLUSTER_POSITION: [u8; 1] = [0xF1];

/// The `EBMLHeader` every fixture in this file starts with.
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

/// A Matroska file whose `Segment` states `segment_body` as its contents.
fn matroska_file(segment_body: &[u8]) -> Vec<u8> {
    [ebml_header(), ebml(&SEGMENT, segment_body)].concat()
}

/// The `Info` these fixtures carry: enough for `open` to want one, and nothing that
/// bears on what is being refused.
fn info_element() -> Vec<u8> {
    ebml(
        &INFO,
        &[
            ebml(&MULTIPLEXING_APP, b"fbtt"),
            ebml(&WRITE_APP, b"fbtt"),
            // TimestampScale: 1 ms, the scale every fixture here is cut on.
            ebml(&TIMECODE_SCALE, &[0x00, 0x0F, 0x42, 0x40]),
        ]
        .concat(),
    )
}

/// An empty `Cluster`, so that the search for a first cluster finds one instead of
/// walking a stream of zeros looking for it.
fn cluster_element() -> Vec<u8> {
    ebml(&CLUSTER, &[])
}

/// One `Seek` entry naming element id `id` at position `at`.
///
/// The position is eight bytes wide whatever the value. EBML would allow a shorter one for a
/// small offset and `matroska-demuxer` reads either, and a fixed width is what makes a `SeekHead`
/// the same length no matter what its entries point at, which is how the fixtures below know their
/// own offsets.
fn seek_entry(id: &[u8], at: u64) -> Vec<u8> {
    ebml(
        &SEEK,
        &[ebml(&SEEK_ID, id), ebml(&SEEK_POSITION, &at.to_be_bytes())].concat(),
    )
}

/// A `SeekHead` naming `entries`, each an element id and the position it sits at.
fn seek_head_named(entries: &[(&[u8], u64)]) -> Vec<u8> {
    let body: Vec<u8> = entries
        .iter()
        .flat_map(|(id, at)| seek_entry(id, *at))
        .collect();
    ebml(&SEEK_HEAD, &body)
}

/// A `SeekHead` pointing at the three top level elements `open` indexes by name.
fn seek_head(tracks_at: u64, info_at: u64, cluster_at: u64) -> Vec<u8> {
    seek_head_named(&[
        (&TRACKS, tracks_at),
        (&INFO, info_at),
        (&CLUSTER, cluster_at),
    ])
}

/// A `Segment` body: a `SeekHead` naming the three elements `open` looks up, then those elements.
///
/// The head is first because that is where the specification puts one and what `matroska-demuxer`
/// looks for first, and its length has to be known before the offsets it carries can be written:
/// it is built once with placeholder offsets to learn that length, and then again with the real
/// ones, which are that length plus the lengths of whatever comes before each of them. The
/// positions count from the start of the Segment's children, which is what EBML says and what the
/// demuxer adds the Segment's own offset back to, so the width of the Segment's size marker never
/// enters them.
///
/// Carrying a `SeekHead` is load bearing. Without one the demuxer indexes the segment itself by
/// walking top level elements to the end of the stream, which on a multi gigabyte stream of zeros
/// reads a header per zero byte and says nothing about the element under test.
fn indexed_segment(tracks: Vec<u8>) -> Vec<u8> {
    let info = info_element();
    let cluster = cluster_element();

    let placeholder = seek_head(0, 0, 0);
    let tracks_at = u64::try_from(placeholder.len()).expect("a SeekHead under 4 GiB");
    let info_at = tracks_at + u64::try_from(tracks.len()).expect("Tracks under 4 GiB");
    let cluster_at = info_at + u64::try_from(info.len()).expect("Info under 4 GiB");

    let head = seek_head(tracks_at, info_at, cluster_at);
    assert_eq!(
        head.len(),
        placeholder.len(),
        "a SeekHead is its own length whatever its entries point at"
    );

    [head, tracks, info, cluster].concat()
}

/// A `Segment` body whose `SeekHead` also names a `Cues` element: head, tracks, info, cluster,
/// cues.
///
/// Same placeholder trick as [`indexed_segment`], with a fourth entry. It is load bearing that
/// the head names `Cues`: `MatroskaFile::open` only parses the cue table when the head points at
/// one (`lib.rs:1489`), and an unindexed cue table is a table the probe never reads. Every cue
/// point carries fixed-width payloads, so the cue count changes the file's size but never the
/// head's, which is what keeps the offsets knowable before the head is written.
fn indexed_segment_with_cues(tracks: &[u8], cue_count: usize) -> Vec<u8> {
    let info = info_element();
    let cluster = cluster_element();

    let placeholder = seek_head_named(&[(&TRACKS, 0), (&INFO, 0), (&CLUSTER, 0), (&CUES, 0)]);
    let tracks_at = u64::try_from(placeholder.len()).expect("a SeekHead under 4 GiB");
    let info_at = tracks_at + u64::try_from(tracks.len()).expect("Tracks under 4 GiB");
    let cluster_at = info_at + u64::try_from(info.len()).expect("Info under 4 GiB");
    let cues_at = cluster_at + u64::try_from(cluster.len()).expect("Cluster under 4 GiB");

    let head = seek_head_named(&[
        (&TRACKS, tracks_at),
        (&INFO, info_at),
        (&CLUSTER, cluster_at),
        (&CUES, cues_at),
    ]);
    assert_eq!(
        head.len(),
        placeholder.len(),
        "a four entry SeekHead is its own length whatever its entries point at"
    );

    let cues = cues(cue_count, cluster_at);
    [head, tracks.to_vec(), info, cluster, cues].concat()
}

/// A `Cues` element holding `count` `CuePoint`s at one second apart on track 1, each pointing at
/// the cluster sitting at `cluster_at`.
fn cues(count: usize, cluster_at: u64) -> Vec<u8> {
    let mut points = Vec::new();
    for index in 0..count {
        let time = u64::try_from(index)
            .expect("a cue count fits a u64")
            .saturating_mul(1000);
        let positions = ebml(
            &CUE_TRACK_POSITIONS,
            &[
                ebml(&CUE_TRACK, &[1]),
                ebml(&CUE_CLUSTER_POSITION, &cluster_at.to_be_bytes()),
            ]
            .concat(),
        );
        let point = ebml(
            &CUE_POINT,
            &[ebml(&CUE_TIME, &time.to_be_bytes()), positions].concat(),
        );
        points.extend_from_slice(&point);
    }
    ebml(&CUES, &points)
}

/// A `TrackEntry` carrying only what a video track needs to be a track.
fn plain_track() -> Vec<u8> {
    ebml(
        &TRACK_ENTRY,
        &[
            ebml(&TRACK_NUMBER, &[1]),
            ebml(&TRACK_UID, &[1]),
            ebml(&TRACK_TYPE, &[1]),
            ebml(&CODEC_ID, b"V_MPEG4/ISO/AVC"),
        ]
        .concat(),
    )
}

/// A `TrackEntry` on a video track whose `CodecPrivate` claims `claim` bytes of data without
/// carrying them. The entry declares only the bytes it writes, so nothing inside it contradicts
/// the entry around it: only the reader spending claim after claim does.
///
/// `CodecPrivate` is the element to claim with because it is binary and the demuxer reads it
/// eagerly, which is the one place in a header where a declared size becomes an allocation.
fn track_claiming(claim: u64) -> Vec<u8> {
    ebml(
        &TRACK_ENTRY,
        &[
            ebml(&TRACK_NUMBER, &[1]),
            ebml(&TRACK_UID, &[1]),
            ebml(&TRACK_TYPE, &[1]),
            ebml(&CODEC_ID, b"V_MPEG4/ISO/AVC"),
            ebml_declaring(&CODEC_PRIVATE, claim, &[]),
        ]
        .concat(),
    )
}

/// A `Tracks` element holding `count` entries that each claim `claim` bytes of `CodecPrivate`.
fn tracks_claiming(count: usize, claim: u64) -> Vec<u8> {
    let entries: Vec<u8> = (0..count).flat_map(|_| track_claiming(claim)).collect();
    ebml(&TRACKS, &entries)
}

/// A Matroska file that claims a 2 GiB `CodecPrivate` on its only track.
///
/// The declared bytes are never written: past the header the stream is whatever `LazyZeros`
/// serves, which is what lets a claim this big sit in a fixture this small.
fn lie_about_one_element() -> Vec<u8> {
    matroska_file(&indexed_segment(tracks_claiming(1, TWO_GIB)))
}

/// An MP4 `ftyp` box of eight body bytes, which is what the sniff needs to call a file MP4:
/// bytes 4 through 8 of the head say `ftyp`, and the walk then skips the box by seeking.
fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let size = u32::try_from(body.len() + 8).expect("a test box under 4 GiB");
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

/// An `ftyp` and then a `moov` header that declares no size, which ISO BMFF spells as "my
/// contents are the rest of the file": a size of 0, which `mp4_atom` reads as `None`.
fn moov_running_to_the_end() -> Vec<u8> {
    let mut out = mp4_box(b"ftyp", b"isom");
    out.extend_from_slice(&0_u32.to_be_bytes());
    out.extend_from_slice(b"moov");
    out
}

/// An `ftyp` and then a `moov` header declaring 2 GiB of contents.
fn moov_declaring_two_gib() -> Vec<u8> {
    let declared = u32::try_from(TWO_GIB).expect("2 GiB fits a 32 bit box size");
    let mut out = mp4_box(b"ftyp", b"isom");
    out.extend_from_slice(&declared.to_be_bytes());
    out.extend_from_slice(b"moov");
    out
}

/// An `ftyp` and then 5000 `free` boxes of eight bytes each, the smallest a box can be, with no
/// `moov` among them.
fn five_thousand_free_boxes() -> Vec<u8> {
    let mut out = mp4_box(b"ftyp", b"isom");
    for _ in 0..5000 {
        out.extend_from_slice(&mp4_box(b"free", &[]));
    }
    out
}
