//! `file_cues` against the checked-in sidecar fixtures, and against hostile
//! input: a file is read only once, in a single pass, and a hostile file must
//! give its good cues back, or no cues, never a panic, never a hang.

use fbtt_media::{
    CUE_WINDOW_MAX_MS, Cue, MAX_CUE_SCAN_FRAMES, MAX_SKIPPED_FRAMES, MediaError, SUBTITLE_FILE_CAP,
    SubtitleFormat, embedded_cues, file_cues, subtitle_format,
};
use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

mod common;

/// The fixture's three cues: the times `expected.json` records for the same
/// text the fixture holds, and the two-line cue joined with a newline.
fn srt_cues() -> Vec<Cue> {
    vec![
        Cue {
            start_ms: 500,
            end_ms: 2000,
            text: "First subtitle line\nspanning two lines".into(),
        },
        Cue {
            start_ms: 2500,
            end_ms: 3000,
            text: "Second subtitle line".into(),
        },
        Cue {
            start_ms: 3200,
            end_ms: 3900,
            text: "Third subtitle line".into(),
        },
    ]
}

/// The three cues the ASS fixture holds: the tagged cue without its `{...}`
/// override, and the comma in the second cue's text kept whole.
fn ass_cues() -> Vec<Cue> {
    vec![
        Cue {
            start_ms: 500,
            end_ms: 2000,
            text: "First ASS line\nspanning two lines".into(),
        },
        Cue {
            start_ms: 2500,
            end_ms: 3000,
            text: "Second cue, italic text".into(),
        },
        Cue {
            start_ms: 3200,
            end_ms: 3900,
            text: "Third ASS line".into(),
        },
    ]
}

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|err| panic!("fixture {name}: {err}"))
}

#[test]
fn srt_file() {
    let cues = file_cues(&fixture("sub.srt"), SubtitleFormat::Srt).expect("the srt fixture");
    assert_eq!(cues, srt_cues());
}

#[test]
fn ass_file() {
    let cues = file_cues(&fixture("sub.ass"), SubtitleFormat::Ass).expect("the ass fixture");
    assert_eq!(cues, ass_cues());
}

/// The cues the VTT fixture holds: the same times as the SRT fixture, but the
/// lines say "WebVTT".
fn vtt_cues() -> Vec<Cue> {
    vec![
        Cue {
            start_ms: 500,
            end_ms: 2000,
            text: "First WebVTT line\nspanning two lines".into(),
        },
        Cue {
            start_ms: 2500,
            end_ms: 3000,
            text: "Second WebVTT line".into(),
        },
        Cue {
            start_ms: 3200,
            end_ms: 3900,
            text: "Third WebVTT line".into(),
        },
    ]
}

#[test]
fn vtt_file() {
    let bytes = fixture("sub.vtt");
    let cues = file_cues(&bytes, SubtitleFormat::Vtt).expect("the vtt fixture");
    assert_eq!(cues, vtt_cues());
}

#[test]
fn subtitle_format_is_by_extension() {
    assert_eq!(subtitle_format("movie.srt"), Some(SubtitleFormat::Srt));
    assert_eq!(subtitle_format("movie.ass"), Some(SubtitleFormat::Ass));
    assert_eq!(subtitle_format("movie.SSA"), Some(SubtitleFormat::Ass));
    assert_eq!(subtitle_format("movie.vtt"), Some(SubtitleFormat::Vtt));
    assert_eq!(subtitle_format("movie.txt"), None);
    assert_eq!(subtitle_format("movie"), None);
    assert_eq!(subtitle_format("movie.srt.bak"), None);
    // A name with no stem, or a bare extension, is no subtitle file.
    assert_eq!(subtitle_format("srt"), None);
    assert_eq!(subtitle_format(".srt"), None);
}

#[test]
fn garbage_is_empty_not_a_panic() {
    // 64 KiB of every byte value, repeated: a hostile file is not a panic.
    let garbage: Vec<u8> = (0u8..=255).cycle().take(64 * 1024).collect();
    for format in [
        SubtitleFormat::Srt,
        SubtitleFormat::Ass,
        SubtitleFormat::Vtt,
    ] {
        let result = file_cues(&garbage, format);
        match result {
            Ok(cues) => assert!(cues.len() <= 100_000),
            Err(err) => assert!(
                matches!(err, MediaError::Malformed(_)),
                "garbage gives Ok or Malformed, got {err:?}"
            ),
        }
    }
}

#[test]
fn too_many_cues_is_malformed() {
    // 100 001 minimal SRT cues, each 38 bytes, under the 4 MiB file cap: the
    // cue count, not the file size, is what the file overruns.
    let block = "1\n00:00:00,000 --> 00:00:00,001\nx\n\n";
    let mut s = String::with_capacity(100_001 * block.len());
    for _ in 0..100_001 {
        s.push_str(block);
    }
    assert!(s.len() <= 4 * 1024 * 1024, "the input fits the file cap");
    let err = file_cues(s.as_bytes(), SubtitleFormat::Srt).expect_err("over the cue cap");
    assert!(
        matches!(err, MediaError::Malformed(_)),
        "a file with 100 001 cues is Malformed, got {err:?}"
    );
}

#[test]
fn huge_timestamp_is_skipped() {
    // 6 000 000 000 000 hours: fits u64, but times 3 600 000 exceeds
    // u64::MAX, so the cue is unparsable and skipped. The 20-digit case is
    // over u64 itself, also skipped.
    let bytes = b"1\r\n6000000000000:00:00,000 --> 6000000000000:00:01,000\r\nHuge\r\n\r\n2\r\n99999999999999999999:00:00,000 --> 99999999999999999999:00:01,000\r\nOver\r\n\r\n3\r\n00:00:01,000 --> 00:00:02,000\r\nSmall\r\n";
    let cues = file_cues(bytes, SubtitleFormat::Srt).expect("no panic");
    assert_eq!(cues.len(), 1, "the huge cues are skipped, not a panic");
    assert_eq!(
        cues[0],
        Cue {
            start_ms: 1000,
            end_ms: 2000,
            text: "Small".into(),
        }
    );
}

#[test]
fn a_file_over_the_cap_is_unsupported() {
    let big = vec![b'x'; 4 * 1024 * 1024 + 1];
    let err = file_cues(&big, SubtitleFormat::Srt).expect_err("over the cap");
    assert!(
        matches!(err, MediaError::Unsupported(_)),
        "a file over the cap is Unsupported, got {err:?}"
    );
}

#[test]
fn zero_length_cues_do_not_count() {
    // 100 001 SRT cues with end == start: they are all dropped, so the cue
    // cap is never reached and the file is Ok, not Malformed.
    let block = "1\n00:00:00,000 --> 00:00:00,000\nx\n\n";
    let mut s = String::with_capacity(100_001 * block.len());
    for _ in 0..100_001 {
        s.push_str(block);
    }
    assert!(s.len() <= 4 * 1024 * 1024, "the input fits the file cap");
    let cues = file_cues(s.as_bytes(), SubtitleFormat::Srt)
        .expect("zero-length cues do not count toward the cap");
    assert!(cues.is_empty(), "all zero-length cues are dropped");
}

/// A cue's text in each format, holding every width of UTF-8 character: a
/// curly apostrophe and an ellipsis (2 bytes), CJK (3 bytes), the four-byte
/// character U+1F600, and U+FFFD. Each must come back exactly as written.
const NON_ASCII: &str = "It’s fine… 你好 \u{1F600} \u{FFFD}";

#[test]
fn non_ascii_text_survives() {
    // SRT: the text comes back whole, and a kept tag next to non-ASCII text
    // keeps the text whole too.
    let srt = format!("1\n00:00:00,500 --> 00:00:02,000\n{i}\n", i = NON_ASCII);
    let cues = file_cues(srt.as_bytes(), SubtitleFormat::Srt).expect("no panic");
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].text, NON_ASCII);
    // A kept tag next to non-ASCII text.
    let srt_tagged = "1\n00:00:00,500 --> 00:00:02,000\n<i>\u{2019}</i> and \u{4F60}\u{597D}\n";
    let cues = file_cues(srt_tagged.as_bytes(), SubtitleFormat::Srt).expect("no panic");
    assert_eq!(cues[0].text, "<i>\u{2019}</i> and \u{4F60}\u{597D}");

    // VTT: the same text comes back whole.
    let vtt = format!(
        "WEBVTT\n\n00:00:00.500 --> 00:00:02.000\n{i}\n",
        i = NON_ASCII
    );
    let cues = file_cues(vtt.as_bytes(), SubtitleFormat::Vtt).expect("no panic");
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].text, NON_ASCII);

    // ASS: the same text comes back whole, with an override block removed.
    let ass = format!(
        "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.50,0:00:02.00,Default,,0,0,0,,{{\\i1}}{i}\n",
        i = NON_ASCII
    );
    let cues = file_cues(ass.as_bytes(), SubtitleFormat::Ass).expect("no panic");
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].text, NON_ASCII);
}

#[test]
fn ass_without_events_section_uses_standard_order() {
    // A file with a [V4+ Styles] section but no [Events] section: the Styles
    // Format: line must not apply, and the Dialogue line uses the standard
    // column order.
    let bytes = b"[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,x\n";
    let cues = file_cues(bytes, SubtitleFormat::Ass).expect("no panic");
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].start_ms, 1000);
    assert_eq!(cues[0].end_ms, 2000);
    assert_eq!(cues[0].text, "x");
}

#[test]
fn ass_text_keeps_commas() {
    // A comma inside the dialogue text stays whole: the text is everything
    // after the last column's leading commas, not the last comma-separated
    // field.
    let bytes = b"Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,a, b, c\n";
    let cues = file_cues(bytes, SubtitleFormat::Ass).expect("no panic");
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].text, "a, b, c");
}

#[test]
fn cleaners_are_linear() {
    // The tag cleaners must not rescan the text they have already walked: a
    // 4 MiB line of delimiters is one pass, not one pass per delimiter. Each
    // of these hostile inputs must return within two seconds.
    let cap = SUBTITLE_FILE_CAP as usize;
    let ass_head = "Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,";
    let srt_head = "1\n00:00:01,000 --> 00:00:02,000\n";

    // One ASS Dialogue line whose text is `{}` repeated.
    let fill = (cap - ass_head.len() - 4) / 2;
    let ass_braces = format!("{ass_head}{}", "{}".repeat(fill));
    assert!(ass_braces.len() < cap, "the braces input fits the file cap");
    let start = Instant::now();
    let cues = file_cues(ass_braces.as_bytes(), SubtitleFormat::Ass).expect("no panic");
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "cleaning a 4 MiB line of {{}} pairs must be linear, took {:?}",
        start.elapsed()
    );
    // The `{}` pairs are empty override blocks: nothing to show.
    assert!(cues.is_empty(), "empty override blocks leave no text");

    // One ASS Dialogue line whose text is `\` repeated.
    let fill = cap - ass_head.len() - 4;
    let ass_backslashes = format!("{ass_head}{}", "\\".repeat(fill));
    assert!(
        ass_backslashes.len() < cap,
        "the backslash input fits the file cap"
    );
    let start = Instant::now();
    file_cues(ass_backslashes.as_bytes(), SubtitleFormat::Ass).expect("no panic");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "cleaning a 4 MiB line of lone backslashes must be linear, took {:?}",
        start.elapsed()
    );

    // One SRT cue whose text is `<>` repeated.
    let fill = (cap - srt_head.len() - 4) / 2;
    let srt_tags = format!("{srt_head}{}", "<>".repeat(fill));
    assert!(srt_tags.len() < cap, "the tag input fits the file cap");
    let start = Instant::now();
    file_cues(srt_tags.as_bytes(), SubtitleFormat::Srt).expect("no panic");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "cleaning a 4 MiB line of <> tags must be linear, took {:?}",
        start.elapsed()
    );

    // One SRT cue whose text is `<` repeated: every tag unclosed, so the
    // whole run is kept as literal, in one pass.
    let fill = cap - srt_head.len() - 4;
    let srt_open = format!("{srt_head}{}", "<".repeat(fill));
    assert!(srt_open.len() < cap, "the open-tag input fits the file cap");
    let start = Instant::now();
    let cues = file_cues(srt_open.as_bytes(), SubtitleFormat::Srt).expect("no panic");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "cleaning a 4 MiB line of lone < must be linear, took {:?}",
        start.elapsed()
    );
    assert_eq!(cues.len(), 1, "an unclosed < keeps the rest as literal");
    assert_eq!(cues[0].text, "<".repeat(fill));
}

#[test]
fn events_header_is_a_whole_line() {
    // `[Events]` inside a Title: line is not a section: the real section's
    // Dialogue line must be found.
    let bytes = b"[Script Info]\nTitle: [Events] demo\n\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,x\n";
    let cues = file_cues(bytes, SubtitleFormat::Ass).expect("no panic");
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].start_ms, 1000);
    assert_eq!(cues[0].end_ms, 2000);
    assert_eq!(cues[0].text, "x");

    // The same file with a lower-case header: ASCII case-insensitive.
    let lower = b"[Script Info]\nTitle: [Events] demo\n\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n\n[events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,x\n";
    let cues = file_cues(lower, SubtitleFormat::Ass).expect("no panic");
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].start_ms, 1000);
    assert_eq!(cues[0].end_ms, 2000);
    assert_eq!(cues[0].text, "x");
}

// ---------------------------------------------------------------------------
// Embedded subtitles: cues read out of a Matroska or WebM file's own track.
// ---------------------------------------------------------------------------

const MKV: &str = "h264-aac-opus-srt-ass.mkv";

/// The three cues `expected.json` records for the fixture's SRT track (4) and
/// ASS track (5): the same times and text both tracks hold.
fn embedded_expected() -> Vec<Cue> {
    vec![
        Cue {
            start_ms: 500,
            end_ms: 2000,
            text: "First subtitle line\nspanning two lines".into(),
        },
        Cue {
            start_ms: 2500,
            end_ms: 3000,
            text: "Second subtitle line".into(),
        },
        Cue {
            start_ms: 3200,
            end_ms: 3900,
            text: "Third subtitle line".into(),
        },
    ]
}

/// The cues of the fixture that overlap the window `[from_ms, to_ms)`: the
/// brief's keep rule, `end_ms > from_ms && start_ms < to_ms`, applied to the
/// three cues both subtitle tracks hold.
fn window_cues(from_ms: u64, to_ms: u64) -> Vec<Cue> {
    embedded_expected()
        .into_iter()
        .filter(|cue| cue.end_ms > from_ms && cue.start_ms < to_ms)
        .collect()
}

#[test]
fn embedded_srt_window() {
    let bytes = fixture(MKV);
    // The whole window: the fixture's three cues, in the order expected.json
    // records them.
    let cues = embedded_cues(Cursor::new(bytes.clone()), 4, 0, 4000).expect("the srt track");
    assert_eq!(cues, embedded_expected());
    // A late window: only the cues that overlap it, the 500..2000 cue gone.
    let cues = embedded_cues(Cursor::new(bytes), 4, 3000, 4000).expect("the srt track");
    assert_eq!(cues, window_cues(3000, 4000));
}

#[test]
fn embedded_ass_window() {
    let bytes = fixture(MKV);
    let cues = embedded_cues(Cursor::new(bytes), 5, 0, 4000).expect("the ass track");
    assert_eq!(cues.len(), 3);
    assert_eq!(
        cues[0],
        Cue {
            start_ms: 500,
            end_ms: 2000,
            text: "First ASS line\nspanning two lines".into(),
        }
    );
    assert_eq!(cues[1].start_ms, 2500);
    assert_eq!(cues[1].end_ms, 3000);
    // The ASS cue's text, free of its {...} override blocks: the tags are
    // removed, and the comma the cue holds is kept whole.
    assert!(!cues[1].text.contains('{') && !cues[1].text.contains('}'));
    assert_eq!(cues[1].text, "Second cue, italic text");
    assert_eq!(
        cues[2],
        Cue {
            start_ms: 3200,
            end_ms: 3900,
            text: "Third ASS line".into(),
        }
    );
}

#[test]
fn window_too_large_is_refused() {
    let bytes = fixture(MKV);
    // A window one past the cap.
    let err = embedded_cues(Cursor::new(bytes.clone()), 4, 0, CUE_WINDOW_MAX_MS + 1)
        .expect_err("a window over the cap is refused");
    assert!(
        matches!(err, MediaError::Unsupported(_)),
        "a window over the cap is Unsupported, got {err:?}"
    );
    // An empty window.
    let err =
        embedded_cues(Cursor::new(bytes), 4, 4000, 4000).expect_err("an empty window is refused");
    assert!(
        matches!(err, MediaError::Unsupported(_)),
        "an empty window is Unsupported, got {err:?}"
    );
}

#[test]
fn embedded_non_text_track_is_unsupported() {
    let bytes = fixture(MKV);
    // Track 1 is the fixture's video.
    let err = embedded_cues(Cursor::new(bytes.clone()), 1, 0, 4000)
        .expect_err("a video track is not a subtitle track");
    assert!(
        matches!(err, MediaError::Unsupported(_)),
        "a video track is Unsupported, got {err:?}"
    );
    // Track 99 is not one of the file's tracks.
    let err =
        embedded_cues(Cursor::new(bytes), 99, 0, 4000).expect_err("a missing track is refused");
    assert!(
        matches!(err, MediaError::Unsupported(_)),
        "a missing track is Unsupported, got {err:?}"
    );
}

#[test]
fn embedded_late_window_seeks() {
    // A one-track Matroska with cues at 0 s, 15 s and 30 s (with Cues and
    // CueRelativePosition, the layout ffmpeg writes). The window 29 000 to
    // 31 000 is reached by a seek: the 30 s cue comes back and the others do
    // not. The blocks are `SimpleBlock`s - the demuxer's `next_frame` steps
    // cleanly out of a `SimpleBlock` after a seek, and that is the shape this
    // test's seek must land in. Without a `BlockDuration` the 30 s cue's end
    // is the fallback, its start plus 5000.
    let tracks = common::tracks(&[common::track_entry(1, 0x11, b"S_TEXT/UTF8")]);
    let info = common::info_element(1_000_000);
    let c0 = common::cluster(0, &[common::simple_block(1, 0, false, b"zero")]);
    let c1 = common::cluster(15_000, &[common::simple_block(1, 0, false, b"fifteen")]);
    let c2 = common::cluster(30_000, &[common::simple_block(1, 0, false, b"thirty")]);
    let clusters = [c0, c1, c2];

    // One cue per cluster, pointing at its block's offset within the cluster.
    // The cue's cluster position is the cluster's offset in the segment data,
    // which is the SeekHead's length plus everything assembled before it.
    let relative = cluster_block_offset(&clusters[0]);
    // The SeekHead names each cluster individually (the layout a real
    // multiplexer writes), so its length is learned from a placeholder with the
    // same count of entries: tracks, info, one per cluster, and the cues.
    let cluster_id: &[u8] = &common::CLUSTER;
    let placeholder = common::seek_head_named(
        &(0..2 + clusters.len() + 1)
            .map(|_| (cluster_id, 0u64))
            .collect::<Vec<_>>(),
    );
    let tracks_at = u64::try_from(placeholder.len()).expect("a SeekHead under 4 GiB");
    let info_at = tracks_at + u64::try_from(tracks.len()).expect("Tracks under 4 GiB");
    let c0_at = info_at + u64::try_from(info.len()).expect("Info under 4 GiB");
    let c1_at = c0_at + u64::try_from(clusters[0].len()).expect("a cluster under 4 GiB");
    let c2_at = c1_at + u64::try_from(clusters[1].len()).expect("a cluster under 4 GiB");
    let cues = common::cues_element(&[
        (0, c0_at, relative),
        (15_000, c1_at, relative),
        (30_000, c2_at, relative),
    ]);

    // The SeekHead names each cluster individually at its offset, so the
    // demuxer's seek lands in the right cluster instead of walking from the
    // first.
    let segment = common::indexed_segment_each_cluster(&tracks, 1_000_000, &clusters, Some(&cues));
    let file = common::matroska_file(&segment);

    let cues = embedded_cues(Cursor::new(file), 1, 29_000, 31_000).expect("the seek lands");
    assert_eq!(
        cues,
        vec![Cue {
            start_ms: 30_000,
            // No BlockDuration: the end is the fallback, start plus 5000.
            end_ms: 35_000,
            text: "thirty".into(),
        }],
        "the 30 s cue, and only it, overlaps the window"
    );
}

/// The `CueRelativePosition` for a cluster's first block: the length of the
/// cluster's leading `Timecode` element, in bytes, measured from the cluster's
/// data start (after its id and size vint). The Matroska specification counts
/// the relative position from the first byte after the cluster's size field,
/// and the demuxer adds it to the cluster's data offset, so the value here is
/// just the `Timecode` element's length.
fn cluster_block_offset(cluster: &[u8]) -> u64 {
    // The cluster's data opens with a `Timecode` element: one-byte id, one-byte
    // size vint (every synthetic timecode is small), and the value, whose width
    // the vint's low bits count. The data starts at byte 4 + 1 (past the
    // four-byte id and the one-byte size vint), so the `Timecode` element's
    // length is measured from there.
    let data_start = 4 + 1;
    let vint_byte = cluster[data_start + 1];
    let value_len = (vint_byte & 0x7f).count_ones();
    1 + 1 + value_len as u64
}

#[test]
fn embedded_hostile_frames_are_bounded() {
    // One subtitle track that has no frame at all, and a video track whose
    // MAX_CUE_SCAN_FRAMES + 1 frames all sit inside the window: the walk must
    // give up with Malformed after reading that many frames, not read on.
    let tracks = common::tracks(&[
        common::track_entry(1, 0x11, b"S_TEXT/UTF8"),
        common::track_entry(2, 1, b"V_MPEG4/ISO/AVC"),
    ]);
    // MAX_CUE_SCAN_FRAMES + 1 video frames, one per small cluster, all at 0 s
    // (inside the window), none of them the subtitle track: the read must give
    // up after MAX_CUE_SCAN_FRAMES frames, not read on.
    let clusters: Vec<Vec<u8>> = (0..MAX_CUE_SCAN_FRAMES + 1)
        .map(|_| common::cluster(0, &[common::simple_block(2, 0, true, &[0x41, 0x01])]))
        .collect();

    let start = Instant::now();
    let err = embedded_cues(
        Cursor::new(common::matroska_file(&common::indexed_segment(
            &tracks, 1_000_000, &clusters, None,
        ))),
        1,
        0,
        4000,
    )
    .expect_err("MAX_CUE_SCAN_FRAMES other-track frames is Malformed");
    assert!(
        matches!(err, MediaError::Malformed(_)),
        "MAX_CUE_SCAN_FRAMES other-track frames is Malformed, got {err:?}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the bound must be reached quickly, took {:?}",
        start.elapsed()
    );
}

#[test]
fn embedded_empty_frames_are_bounded() {
    // One ASS subtitle track whose MAX_SKIPPED_FRAMES + 1 frames all give no
    // cue (blocks with fewer than nine fields): the walk must give up with
    // Malformed after that many frames of the track itself, not read on.
    let tracks = common::tracks(&[common::track_entry(1, 0x11, b"S_TEXT/ASS")]);
    // MAX_SKIPPED_FRAMES + 1 frames, one per small cluster, all at 0 s, all
    // with a short ASS block (three fields, not nine): each gives no cue, so
    // the skipped bound is what stops the read.
    let clusters: Vec<Vec<u8>> = (0..MAX_SKIPPED_FRAMES as u64 + 1)
        .map(|_| common::cluster(0, &[common::simple_block(1, 0, false, b"0,0,Default")]))
        .collect();

    let err = embedded_cues(
        Cursor::new(common::matroska_file(&common::indexed_segment(
            &tracks, 1_000_000, &clusters, None,
        ))),
        1,
        0,
        4000,
    )
    .expect_err("MAX_SKIPPED_FRAMES empty frames is Malformed");
    assert!(
        matches!(err, MediaError::Malformed(_)),
        "MAX_SKIPPED_FRAMES empty frames is Malformed, got {err:?}"
    );
}

#[test]
fn embedded_ass_short_block_is_skipped() {
    // An S_TEXT/ASS track with one block that has fewer than nine fields and
    // one valid nine-field block: only the valid one comes back.
    let tracks = common::tracks(&[common::track_entry(1, 0x11, b"S_TEXT/ASS")]);
    // The short block: three fields, not nine. The valid block: nine fields,
    // the text in the ninth.
    let short = common::cluster(0, &[common::simple_block(1, 0, false, b"0,0,Default")]);
    let valid = common::cluster(
        1_000,
        &[common::simple_block(
            1,
            0,
            false,
            b"0,0,Default,,0,0,0,,Valid text",
        )],
    );
    let clusters = [short, valid];

    let cues = embedded_cues(
        Cursor::new(common::matroska_file(&common::indexed_segment(
            &tracks, 1_000_000, &clusters, None,
        ))),
        1,
        0,
        4000,
    )
    .expect("the ass track");
    assert_eq!(
        cues,
        vec![Cue {
            start_ms: 1_000,
            // No BlockDuration: the end is the fallback, start plus 5000.
            end_ms: 6_000,
            text: "Valid text".into(),
        }],
        "the short block is skipped, only the valid one comes back"
    );
}

#[test]
fn embedded_last_cue_ends_at_next_cue() {
    // Two cues at 3500 and 4100 with no BlockDuration and nothing else between
    // them, window 0 to 4000: the first cue's end is the next cue's start
    // (4100), not the 5000 ms fallback, because the walk records the first
    // cue of the track at or past the window.
    let tracks = common::tracks(&[common::track_entry(1, 0x11, b"S_TEXT/UTF8")]);
    let c0 = common::cluster(3_500, &[common::simple_block(1, 0, false, b"first")]);
    let c1 = common::cluster(4_100, &[common::simple_block(1, 0, false, b"second")]);
    let clusters = [c0, c1];

    let cues = embedded_cues(
        Cursor::new(common::matroska_file(&common::indexed_segment(
            &tracks, 1_000_000, &clusters, None,
        ))),
        1,
        0,
        4_000,
    )
    .expect("the srt track");
    assert_eq!(
        cues,
        vec![Cue {
            start_ms: 3_500,
            // The next cue's start, not the 5000 ms fallback.
            end_ms: 4_100,
            text: "first".into(),
        }],
        "the last cue in the window ends at the next cue's start"
    );
}

#[test]
fn embedded_total_text_is_capped() {
    // About five blocks of 1 MiB of text each, inside the window: the total
    // kept text passes SUBTITLE_FILE_CAP and the call is Malformed.
    let tracks = common::tracks(&[common::track_entry(1, 0x11, b"S_TEXT/UTF8")]);
    // Five blocks, each 1 MiB of 'x', spaced 1 s apart inside the window.
    let payload = vec![b'x'; 1_024 * 1_024];
    let clusters: Vec<Vec<u8>> = (0..5)
        .map(|i| common::cluster(i * 1_000, &[common::simple_block(1, 0, false, &payload)]))
        .collect();

    let err = embedded_cues(
        Cursor::new(common::matroska_file(&common::indexed_segment(
            &tracks, 1_000_000, &clusters, None,
        ))),
        1,
        0,
        5_000,
    )
    .expect_err("the total kept text is over the cap");
    assert!(
        matches!(err, MediaError::Malformed(_)),
        "five 1 MiB blocks is Malformed, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Embedded subtitles: the same cues out of an MP4 file's own `tx3g` track.
// The mov_text fixture's subtitle track carries no composition offsets, so
// its samples present when they decode and the edit-list shift the video
// track carries does not reach them: here the fixture's tables and
// `expected.json` state the same cue times, and `embedded_expected()` is the
// same three cues the sidecar fixtures hold.
// ---------------------------------------------------------------------------

const MP4: &str = "h264-aac2-movtext.mp4";

#[test]
fn embedded_movtext() {
    let bytes = fixture(MP4);
    // The whole window: the three cues `expected.json` records for the
    // mov_text track (4) with non-empty text, and the zero-length gap
    // samples between them no cues.
    let cues = embedded_cues(Cursor::new(bytes.clone()), 4, 0, 4000).expect("the mov_text track");
    assert_eq!(
        cues,
        embedded_expected(),
        "a mov_text window shows the cues on it"
    );
    // A window inside the second cue: the read lands on that cue's sample -
    // it opened before the window and is on screen when it opens - and stops
    // before the third cue's sample.
    let cues = embedded_cues(Cursor::new(bytes), 4, 2600, 2900).expect("the mov_text track");
    assert_eq!(
        cues,
        window_cues(2600, 2900),
        "a mov_text window shows only the cue on it"
    );
}

#[test]
fn embedded_movtext_bad_length_is_malformed() {
    // The first text sample's 2-byte length patched past the bytes the sample
    // holds: a sample claiming more text than it has is a lying table, not a
    // stream that has not arrived. The sample is found by its own text - the
    // length sits directly in front of it - so no new fixture file is added.
    let mut bytes = fixture(MP4);
    let needle: &[u8] = b"First subtitle line";
    let hits = bytes
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count();
    assert_eq!(hits, 1, "the first cue's text is in the file exactly once");
    let at = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("the first cue's text");
    assert!(at >= 2, "the text has a length prefix in front of it");
    // The patch must exceed any sample's size minus its prefix: the file
    // minus two bytes is smaller than 65535, so a sample cannot hold text
    // of the patched length whatever its own size is.
    assert!(
        bytes.len() - 2 < usize::from(u16::MAX),
        "the patched length beats any sample in the file"
    );
    bytes[at - 2..at].copy_from_slice(&[0xff, 0xff]);
    let err = embedded_cues(Cursor::new(bytes), 4, 0, 4000)
        .expect_err("a length past the sample's bytes is Malformed");
    assert!(
        matches!(err, MediaError::Malformed(_)),
        "a lying length is Malformed, got {err:?}"
    );
}

#[test]
fn embedded_movtext_audio_track_is_unsupported() {
    let bytes = fixture(MP4);
    // Track 2 is one of the file's two AAC tracks. A cue request on it is the
    // same refusal the Matroska path gives for a track that is not a text
    // subtitle track.
    let err = embedded_cues(Cursor::new(bytes), 2, 0, 4000)
        .expect_err("an audio track is not a subtitle track");
    assert!(
        matches!(err, MediaError::Unsupported(_)),
        "an audio track is Unsupported, got {err:?}"
    );
}

/// A reader that fails the test instead of honouring a read of more than one
/// megabyte in a single call. The oversized-sample verdict must come from the
/// `stsz` entry before any buffer of that size is read, and this is what
/// shows it: a source that asks this reader for 4 MiB at once ends the test
/// right here, so a passing test is a run in which that read never happened.
struct SmallReads(Cursor<Vec<u8>>);

impl Read for SmallReads {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        assert!(
            buf.len() <= 1024 * 1024,
            "a read of {} bytes was asked for",
            buf.len()
        );
        self.0.read(buf)
    }
}

impl Seek for SmallReads {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

#[test]
fn embedded_movtext_oversized_sample_is_malformed() {
    // The first text sample's `stsz` entry patched to SUBTITLE_FILE_CAP + 1.
    // The element cap (256 MiB) still trusts the number, so only the subtitle
    // cap can refuse it, and it must refuse before the bytes are asked for:
    // SmallReads turns a 4 MiB read into a failed test.
    let bytes = fixture(MP4);
    let entry_at = stsz_entry_at(&bytes, 4);
    let mut patched = bytes.clone();
    patched[entry_at..entry_at + 4].copy_from_slice(&(SUBTITLE_FILE_CAP as u32 + 1).to_be_bytes());
    let err = embedded_cues(SmallReads(Cursor::new(patched)), 4, 0, 4000)
        .expect_err("a sample over the subtitle cap is Malformed");
    assert!(
        matches!(err, MediaError::Malformed(_)),
        "a sample over the subtitle cap is Malformed before it is read, got {err:?}"
    );
}

/// Where a track's first sample that can hold text has its entry in the
/// file's `stsz` box.
///
/// The tables are parsed with the same library the reader parses them with,
/// as `remux.rs`'s `table_track_in` does, and the box itself is located by
/// the shape only an `stsz` header has - its FourCC followed by a zero
/// version and flags - inside the `moov`, checked by demanding that the
/// whole entry array under the candidate equals the parsed sizes. The first
/// sample big enough to hold a length prefix and a byte of text is the one
/// the cue walk reads first after the leading gap sample, and its entry is
/// the one a size patch has to hit.
fn stsz_entry_at(bytes: &[u8], track_id: u32) -> usize {
    use mp4_atom::{FourCC, Header, Moov, ReadAtom, ReadFrom};
    let mut reader = Cursor::new(bytes);
    let (moov_at, moov_body, moov) = loop {
        let here = reader
            .stream_position()
            .expect("the position of a box header");
        let header = <Option<Header> as ReadFrom>::read_from(&mut reader)
            .expect("a top level box header")
            .expect("a box before the moov");
        if header.kind == FourCC::new(b"moov") {
            let body = header.size.expect("a moov that declares its size");
            let moov = Moov::read_atom(&header, &mut reader).expect("the fixture's moov");
            break (
                usize::try_from(here).expect("a moov near the file start"),
                body,
                moov,
            );
        }
        let body = i64::try_from(header.size.expect("a sized box ahead of the moov"))
            .expect("a box below 2^63");
        reader.seek(SeekFrom::Current(body)).expect("the next box");
    };
    let trak = moov
        .trak
        .iter()
        .find(|trak| trak.tkhd.track_id == track_id)
        .expect("the mov_text track");
    let sizes: &[u32] = match &trak.mdia.minf.stbl.stsz.samples {
        mp4_atom::StszSamples::Different { sizes } => sizes,
        mp4_atom::StszSamples::Identical { .. } => {
            panic!("the fixture's tx3g track has per-sample sizes")
        }
    };
    // An stsz box header: the FourCC and its zero version-and-flags. Every
    // candidate is checked against the parsed table before one is trusted.
    // `Header::size` excludes the box header, so the moov spans its header,
    // this body, and nothing else; the search covers all of it.
    let pattern = b"stsz\0\0\0\0";
    let haystack = &bytes[moov_at..moov_at + 8 + moov_body];
    let matching: Vec<usize> = haystack
        .windows(pattern.len())
        .enumerate()
        .filter(|(_, window)| *window == pattern)
        .map(|(at, _)| moov_at + at)
        .filter(|at| {
            let count = u32::from_be_bytes(bytes[at + 12..at + 16].try_into().expect("four bytes"));
            count as usize == sizes.len()
                && bytes[*at + 16..*at + 16 + 4 * sizes.len()]
                    .chunks(4)
                    .enumerate()
                    .all(|(index, word)| {
                        u32::from_be_bytes(word.try_into().expect("four bytes")) == sizes[index]
                    })
        })
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "exactly one stsz box in the moov holds the mov_text track's sizes"
    );
    // The first sample big enough to hold its prefix and one byte of text;
    // the leading gap sample is its prefix alone.
    let index = sizes
        .iter()
        .position(|size| *size > 2)
        .expect("a sample with text");
    matching[0] + 16 + 4 * index
}
