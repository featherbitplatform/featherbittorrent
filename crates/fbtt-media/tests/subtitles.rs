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
use common::mp4;

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
    // 31 000 is reached by a seek to 19 000: the demuxer lands in the 30 s
    // cluster and reads from the 30 s block, so the 30 s cue comes back and
    // the earlier ones do not.
    //
    // The SeekHead holds one `Cluster` entry - the first cluster, as a real
    // multiplexer writes it. The demuxer keeps its SeekHead entries in a map,
    // so naming every cluster makes the last one win and `open` starts reading
    // at the 30 s cluster: a reader that never seeks then never sees the 0 s
    // or 15 s cues, and this test would pass without the seek. One entry keeps
    // `open` at the first cluster, so a reader that never seeks reads from the
    // start and returns the 15 s cue too - which the seek must drop.
    //
    // The 15 s cue ends at 30 000 (a `BlockDuration` of 15 000), so it overlaps
    // the window 29 000..31 000. Read from the start, it comes back; the seek
    // to 19 000 drops it, because the demuxer yields only frames at or after
    // the seek point. That is the accepted contract: the 10 s pre-roll can lose
    // a cue that started before the seek point, so a cue that starts before
    // 19 000 and ends after 29 000 is not guaranteed by the seek. The 30 s cue
    // is a `SimpleBlock` - the demuxer's `next_frame` steps cleanly out of a
    // `SimpleBlock` after a seek, and that is the shape this test's seek must
    // land in. Without a `BlockDuration` its end is the fallback, start plus
    // 5000.
    let tracks = common::tracks(&[common::track_entry(1, 0x11, b"S_TEXT/UTF8")]);
    let info = common::info_element(1_000_000);
    let c0 = common::cluster(0, &[common::simple_block(1, 0, false, b"zero")]);
    let c1 = common::cluster(15_000, &[common::block_group(1, 0, b"fifteen", 15_000)]);
    let c2 = common::cluster(30_000, &[common::simple_block(1, 0, false, b"thirty")]);
    let clusters = [c0, c1, c2];

    // One cue per cluster, pointing at its first block's offset within the
    // cluster. Each cluster opens with the same `Timecode` element, so the
    // relative position is the same for all three.
    let relative = cluster_block_offset(&clusters[0]);
    // The segment layout is SeekHead, tracks, info, clusters, cues. The cues'
    // own position depends on the SeekHead's length, and the SeekHead's length
    // depends on the number of entries (not the offset values, which are fixed
    // width), so the length is learned from a placeholder with the same entry
    // count, and the cues' position is computed from it.
    // The real SeekHead holds one entry each for tracks, info, the first
    // cluster and the cues. The placeholder carries the same entry ids at zero
    // offsets, so the two heads are the same length.
    let placeholder = common::seek_head_named(&[
        (&common::TRACKS, 0u64),
        (&common::INFO, 0),
        (&common::CLUSTER, 0),
        (&common::CUES, 0),
    ]);
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
    // The real SeekHead: one entry each for tracks, info, the first cluster and
    // the cues. The cues sit after the clusters, at a position the SeekHead's
    // own length makes known.
    let head = common::seek_head_named(&[
        (&common::TRACKS, tracks_at),
        (&common::INFO, info_at),
        (&common::CLUSTER, c0_at),
        (
            &common::CUES,
            c2_at + u64::try_from(clusters[2].len()).expect("a cluster under 4 GiB"),
        ),
    ]);
    assert_eq!(
        head.len(),
        placeholder.len(),
        "the SeekHead length is entry-count driven"
    );
    let mut segment = head;
    segment.extend_from_slice(&tracks);
    segment.extend_from_slice(&info);
    for cluster in &clusters {
        segment.extend_from_slice(cluster);
    }
    segment.extend_from_slice(&cues);
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
    // MAX_CUE_SCAN_FRAMES + 1 video frames, 1000 per cluster, all at 0 s
    // (inside the window), none of them the subtitle track: the read must give
    // up after MAX_CUE_SCAN_FRAMES frames, not read on. Many blocks per cluster
    // rather than one cluster per frame: a million tiny clusters is slow to
    // assemble and to walk in a debug build, and the frame count is what the
    // bound is about, not the cluster count.
    let per_cluster = 1_000usize;
    let cluster_count = (MAX_CUE_SCAN_FRAMES as usize + 1).div_ceil(per_cluster);
    let clusters: Vec<Vec<u8>> = (0..cluster_count)
        .map(|_| {
            common::cluster(
                0,
                &(0..per_cluster)
                    .map(|_| common::simple_block(2, 0, true, &[0x41, 0x01]))
                    .collect::<Vec<_>>(),
            )
        })
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

#[test]
fn embedded_preroll_text_is_capped() {
    // Five blocks of 1 MiB of text each, all in the pre-roll: after the seek
    // point (19 000) but before the window (29 000). The total pushed text
    // passes SUBTITLE_FILE_CAP and the call is Malformed at once, not only
    // after the walk finishes. The cues do not overlap the window, so a check
    // that ran only over the kept cues would never see them and would return
    // Ok(empty).
    let tracks = common::tracks(&[
        common::track_entry(1, 0x11, b"S_TEXT/UTF8"),
        common::track_entry(2, 1, b"V_MPEG4/ISO/AVC"),
    ]);
    // The file has no Cues, so the seek to 19 000 is the demuxer's linear
    // search: it returns the last cluster that starts at or before the seek
    // point. A one-block video cluster at 10 s gives it a real cluster to land
    // on (without it the first cluster, at 20 s, is already past 19 000 and the
    // seek falls back to offset 0, so the blocks would be read only by the
    // re-open from the start). The walk then starts at 10 s and reads the five
    // blocks after the seek, which is the pre-roll path.
    // Five blocks, each 1 MiB of 'x', spaced 1 s apart from 20 s to 24 s. The
    // window is 29 000 to 31 000, so none overlaps it.
    let payload = vec![b'x'; 1_024 * 1_024];
    let mut clusters: Vec<Vec<u8>> = vec![common::cluster(
        10_000,
        &[common::simple_block(2, 0, true, b"v")],
    )];
    clusters.extend((0..5).map(|i| {
        common::cluster(
            20_000 + i * 1_000,
            &[common::simple_block(1, 0, false, &payload)],
        )
    }));

    let err = embedded_cues(
        Cursor::new(common::matroska_file(&common::indexed_segment(
            &tracks, 1_000_000, &clusters, None,
        ))),
        1,
        29_000,
        31_000,
    )
    .expect_err("the total pushed text is over the cap");
    assert!(
        matches!(err, MediaError::Malformed(_)),
        "five 1 MiB pre-roll blocks is Malformed, got {err:?}"
    );
}

#[test]
fn embedded_cue_after_late_video_frame_is_kept() {
    // A video frame at 4040 muxed before a subtitle cue at 3980, window 0 to
    // 4000: the cue comes back. The video frame's start (4040) is at or past
    // the window's end (4000), but not at or past the end plus the interleave
    // margin (2 000), so the walk does not stop on it; it reads on and finds
    // the cue at 3980. A stop on the video frame alone would lose the cue.
    let tracks = common::tracks(&[
        common::track_entry(1, 0x11, b"S_TEXT/UTF8"),
        common::track_entry(2, 1, b"V_MPEG4/ISO/AVC"),
    ]);
    // One cluster: the video block at 4040 first (the late, interleaved
    // frame), the subtitle block at 3980 second. Mux order, not time order.
    let cluster = common::cluster(
        0,
        &[
            common::simple_block(2, 4_040, true, &[0x41, 0x01]),
            common::simple_block(1, 3_980, false, b"kept"),
        ],
    );
    let clusters = [cluster];

    let cues = embedded_cues(
        Cursor::new(common::matroska_file(&common::indexed_segment(
            &tracks, 1_000_000, &clusters, None,
        ))),
        1,
        0,
        4_000,
    )
    .expect("the cue is read after the late video frame");
    assert_eq!(
        cues,
        vec![Cue {
            start_ms: 3_980,
            // No BlockDuration: the end is the fallback, start plus 5000.
            end_ms: 8_980,
            text: "kept".into(),
        }],
        "the cue at 3980 survives the video frame at 4040"
    );
}

#[test]
fn embedded_window_after_end_is_empty() {
    // A window starting after the Duration and after the last cue gives
    // Ok(empty) without the seek, and without the fallback's full re-read.
    // The Duration is 42 000, the window is 45 000 to 47 000, and the last
    // cue is the 30 s cue. The seek target is 35 000, which is past the 30 s
    // cue, so the demuxer yields no frame and the fallback re-read runs. The
    // file is built long enough in frames that the re-read trips
    // MAX_CUE_SCAN_FRAMES (Malformed), so only the duration check stands
    // between the window and the re-read.
    let tracks = common::tracks(&[
        common::track_entry(1, 0x11, b"S_TEXT/UTF8"),
        common::track_entry(2, 1, b"V_MPEG4/ISO/AVC"),
    ]);
    let info = common::info_with_duration(1_000_000, 42_000);
    let relative = cluster_block_offset(&common::cluster(
        30_000,
        &[common::simple_block(1, 0, false, b"thirty")],
    ));
    // The cues: 0 s, 15 s and 30 s. The seek to 35 000 lands past the 30 s
    // cue and finds no frame, so without the duration check the fallback
    // re-reads the whole file from the start.
    let c0 = common::cluster(0, &[common::simple_block(1, 0, false, b"zero")]);
    let c1 = common::cluster(15_000, &[common::simple_block(1, 0, false, b"fifteen")]);
    let c2 = common::cluster(30_000, &[common::simple_block(1, 0, false, b"thirty")]);

    // MAX_CUE_SCAN_FRAMES + 1 video frames, 1000 per cluster, all at 0 s: the
    // fallback's full re-read walks them all and gives up with Malformed. The
    // seek to 35 000 finds no frame (it is past the 30 s cue), so the re-read
    // is what the duration check stands between the window and.
    let per_cluster = 1_000usize;
    let video_count = (MAX_CUE_SCAN_FRAMES + 1) as usize;
    let video_cluster_count = video_count.div_ceil(per_cluster);
    let video_clusters: Vec<Vec<u8>> = (0..video_cluster_count)
        .map(|_| {
            common::cluster(
                0,
                &(0..per_cluster)
                    .map(|_| common::simple_block(2, 0, true, &[0x41, 0x01]))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let mut clusters = vec![c0.clone(), c1.clone(), c2.clone()];
    clusters.extend(video_clusters);

    // The cues point at the three subtitle clusters. Their positions depend on
    // the SeekHead's length, which is learned from a placeholder with the same
    // entry count; the cues sit after all the clusters.
    // The real SeekHead holds one entry each for tracks, info, the first
    // cluster and the cues. The placeholder carries the same entry ids at zero
    // offsets, so the two heads are the same length.
    let placeholder = common::seek_head_named(&[
        (&common::TRACKS, 0u64),
        (&common::INFO, 0),
        (&common::CLUSTER, 0),
        (&common::CUES, 0),
    ]);
    let tracks_at = u64::try_from(placeholder.len()).expect("a SeekHead under 4 GiB");
    let info_at = tracks_at + u64::try_from(tracks.len()).expect("Tracks under 4 GiB");
    let c0_at = info_at + u64::try_from(info.len()).expect("Info under 4 GiB");
    let c1_at = c0_at + u64::try_from(c0.len()).expect("a cluster under 4 GiB");
    let c2_at = c1_at + u64::try_from(c1.len()).expect("a cluster under 4 GiB");
    let cues = common::cues_element(&[
        (0, c0_at, relative),
        (15_000, c1_at, relative),
        (30_000, c2_at, relative),
    ]);
    let clusters_end = c2_at
        + u64::try_from(c2.len()).expect("a cluster under 4 GiB")
        + clusters[3..]
            .iter()
            .map(|c| u64::try_from(c.len()).expect("a cluster under 4 GiB"))
            .sum::<u64>();
    let head = common::seek_head_named(&[
        (&common::TRACKS, tracks_at),
        (&common::INFO, info_at),
        (&common::CLUSTER, c0_at),
        (&common::CUES, clusters_end),
    ]);
    assert_eq!(
        head.len(),
        placeholder.len(),
        "the SeekHead length is entry-count driven"
    );
    let mut segment = head;
    segment.extend_from_slice(&tracks);
    segment.extend_from_slice(&info);
    for cluster in &clusters {
        segment.extend_from_slice(cluster);
    }
    segment.extend_from_slice(&cues);
    let cues = embedded_cues(
        Cursor::new(common::matroska_file(&segment)),
        1,
        45_000,
        47_000,
    )
    .expect("a window past the end is an empty answer, not a re-read");
    assert!(
        cues.is_empty(),
        "a window starting after the duration is empty, got {cues:?}"
    );
}

#[test]
fn embedded_zero_duration_is_unknown() {
    // The late-window file with Duration 0. A recording cut short can carry
    // Duration 0, and treating it as a known end would make every window
    // return empty. The duration is unknown, so the window 29 000 to 31 000
    // still seeks to 19 000 and returns the 30 s cue.
    let tracks = common::tracks(&[common::track_entry(1, 0x11, b"S_TEXT/UTF8")]);
    let info = common::info_with_duration(1_000_000, 0);
    let c0 = common::cluster(0, &[common::simple_block(1, 0, false, b"zero")]);
    let c1 = common::cluster(15_000, &[common::simple_block(1, 0, false, b"fifteen")]);
    let c2 = common::cluster(30_000, &[common::simple_block(1, 0, false, b"thirty")]);
    let clusters = [c0.clone(), c1.clone(), c2.clone()];
    let relative = cluster_block_offset(&c2);
    let placeholder = common::seek_head_named(&[
        (&common::TRACKS, 0u64),
        (&common::INFO, 0),
        (&common::CLUSTER, 0),
        (&common::CUES, 0),
    ]);
    let tracks_at = u64::try_from(placeholder.len()).expect("a SeekHead under 4 GiB");
    let info_at = tracks_at + u64::try_from(tracks.len()).expect("Tracks under 4 GiB");
    let c0_at = info_at + u64::try_from(info.len()).expect("Info under 4 GiB");
    let c1_at = c0_at + u64::try_from(c0.len()).expect("a cluster under 4 GiB");
    let c2_at = c1_at + u64::try_from(c1.len()).expect("a cluster under 4 GiB");
    let cues = common::cues_element(&[
        (0, c0_at, relative),
        (15_000, c1_at, relative),
        (30_000, c2_at, relative),
    ]);
    let head = common::seek_head_named(&[
        (&common::TRACKS, tracks_at),
        (&common::INFO, info_at),
        (&common::CLUSTER, c0_at),
        (
            &common::CUES,
            c2_at + u64::try_from(c2.len()).expect("a cluster under 4 GiB"),
        ),
    ]);
    assert_eq!(
        head.len(),
        placeholder.len(),
        "the SeekHead length is entry-count driven"
    );
    let mut segment = head;
    segment.extend_from_slice(&tracks);
    segment.extend_from_slice(&info);
    for cluster in &clusters {
        segment.extend_from_slice(cluster);
    }
    segment.extend_from_slice(&cues);
    let cues = embedded_cues(
        Cursor::new(common::matroska_file(&segment)),
        1,
        29_000,
        31_000,
    )
    .expect("Duration 0 is unknown, the seek runs");
    assert_eq!(
        cues,
        vec![Cue {
            start_ms: 30_000,
            // No BlockDuration: the end is the fallback, start plus 5000.
            end_ms: 35_000,
            text: "thirty".into(),
        }],
        "Duration 0 does not make the window empty"
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

// ---------------------------------------------------------------------------
// The bounds the `tx3g` walk puts on one window: where it stops reading, what
// it refuses before it allocates, and what a reader that has not given the
// bytes over gets for an answer. The inputs are the fixture's own tables
// rewritten in memory, because the shapes under test - a track of a million
// samples, samples that together are over the text cap - are not things the
// checked-in fixture holds. `common::mp4` locates the boxes, reads the tables
// with the library the reader reads them with, and checks the two against each
// other, so a patch here is a patch at a number the reader looks at.
// ---------------------------------------------------------------------------

/// The text a sample's bytes hold: the 2-byte length prefix and the text it
/// counts.
fn record_text<'a>(bytes: &'a [u8], sample: &mp4::Sample) -> &'a [u8] {
    let at = usize::try_from(sample.offset).expect("a sample offset inside the file");
    let len = u16::from_be_bytes(
        bytes[at..at + 2]
            .try_into()
            .expect("a 2-byte length prefix"),
    ) as usize;
    &bytes[at + 2..at + 2 + len]
}

/// A reader that fails the test the moment a read starts at `at`.
///
/// `read_sample_bytes` seeks to exactly one sample's offset before it reads it, so a read that
/// starts here is that sample being read, and the bytes it asks for are the size its `stsz` entry
/// states. The cue walk's stop at the window's end is the only thing that keeps a walk from
/// reading a sample that presents at or past the window, and this makes "it did not" a fact about
/// the run rather than a guess from the cues that came back.
struct NoReadAt {
    inner: Cursor<Vec<u8>>,
    at: u64,
}

impl Read for NoReadAt {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let here = self
            .inner
            .stream_position()
            .expect("the position a read starts at");
        assert_ne!(
            here,
            self.at,
            "the walk read the sample at byte {here}, asking for {} bytes: the window's end is \
             where it was to stop, not where the sample list ends",
            buf.len()
        );
        self.inner.read(buf)
    }
}

impl Seek for NoReadAt {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn embedded_movtext_stops_at_the_window_end() {
    // The window 2600..2900 lands on the second cue's sample, which opened before the window and
    // is on screen when it opens, and stops at the gap sample the muxer wrote at 3000 ms. That
    // sample's `stsz` entry is patched to 2 MiB: still under the cap one sample may claim, so the
    // per-sample bound is not what keeps its bytes unread, and more than the bytes the file holds
    // after that offset, so a read of it is a read the stream cannot answer. NoReadAt ends the test
    // if a read ever starts at that sample's offset.
    const PATCHED: u32 = 2 * 1024 * 1024;
    const FROM_MS: u64 = 2_600;
    const TO_MS: u64 = 2_900;
    let bytes = fixture(MP4);
    let track = mp4::locate(&bytes, 4);
    assert_eq!(
        mp4::cues(&bytes, &track),
        embedded_expected(),
        "the model reads this track's own cues out of its boxes, so the sample it names is the \
         sample the reader walks to"
    );
    let samples = mp4::samples(&bytes, &track);
    let (from_ticks, to_ticks) = mp4::window_ticks(&track, FROM_MS, TO_MS);
    // The landing the walk starts on, decided from the tables alone: the last sample at or before
    // the window's start.
    let landing = samples
        .iter()
        .rfind(|sample| sample.pts_ticks <= from_ticks)
        .expect("a sample at or before the window's start");
    assert_eq!(
        record_text(&bytes, landing),
        b"Second subtitle line",
        "the window opens inside the second cue, so its own sample is where the read starts"
    );
    let stopped = samples
        .iter()
        .find(|sample| sample.pts_ticks >= to_ticks)
        .expect("a sample at or past the window's end");
    assert!(
        stopped.index > landing.index,
        "the walk steps forward from the landing to reach the window's end"
    );
    assert_eq!(
        stopped.size, 2,
        "the sample it stops on is the gap between cues"
    );
    assert!(
        u64::from(PATCHED) < SUBTITLE_FILE_CAP,
        "the patched size is one a sample is allowed to claim: only the stop keeps these bytes \
         from being read"
    );
    assert!(
        u64::from(PATCHED) > bytes.len() as u64 - stopped.offset,
        "the patched size is more than the bytes behind it in the file, so a read of it is a read \
         the stream cannot answer"
    );
    let entry_at = mp4::stsz_entry_at(&track, stopped.index);
    let mut patched = bytes.clone();
    patched[entry_at..entry_at + 4].copy_from_slice(&PATCHED.to_be_bytes());
    let cues = embedded_cues(
        NoReadAt {
            inner: Cursor::new(patched),
            at: stopped.offset,
        },
        4,
        FROM_MS,
        TO_MS,
    )
    .expect("the window stops before the sample at its end, which is the sample that was patched");
    assert_eq!(
        cues,
        window_cues(FROM_MS, TO_MS),
        "the cue the window is inside, and nothing past the window"
    );
    assert_eq!(cues.len(), 1, "the window is inside exactly one cue");
    let plain = embedded_cues(Cursor::new(bytes.clone()), 4, FROM_MS, TO_MS)
        .expect("the same window on the untouched track");
    assert_eq!(
        cues, plain,
        "the patch changes nothing the window reads: it only makes the read that must not happen \
         one that would run off the end of the file"
    );
}

#[test]
fn embedded_movtext_frame_cap_is_malformed() {
    // `MAX_CUE_SCAN_FRAMES` + 448 samples, every one of them inside one window. The count lives
    // in one `stsz` field: a nonzero `sample_size` says every sample is that long and no entry
    // array follows. One `stts` run of one tick each covers them, and 1954 chunks of 512 samples
    // each point at the same 1 KiB of media data - samples within a chunk are contiguous, so 512
    // samples of 2 bytes ask for 1024 bytes of file, and the chunk table repeats that region
    // instead of stating a place for every sample.
    //
    // That is the smallest in-memory table that reaches the bound out of this 57 KiB fixture. The
    // alternatives are not: the per-sample `stsz` form this fixture itself uses would need 4 MiB
    // of entry bytes to count a million samples, and one chunk holding them all would need 2 MiB
    // of media bytes the file does not hold.
    const PER_CHUNK: u32 = 512;
    const CHUNKS: u32 = 1_954;
    const SAMPLES: u32 = PER_CHUNK * CHUNKS;
    const GAP: u32 = 2;
    const TO_MS: u64 = 4_000;
    assert!(
        u64::from(SAMPLES) > MAX_CUE_SCAN_FRAMES,
        "the rewritten track puts more samples in the window than the bound"
    );
    assert!(
        u64::from(SAMPLES) * u64::from(GAP) < SUBTITLE_FILE_CAP,
        "the track's samples together stay under the text cap, so the count is the bound it \
         reaches first"
    );
    let bytes = fixture(MP4);
    let track = mp4::locate(&bytes, 4);
    let stts = mp4::stts_box(&[(SAMPLES, 1)]);
    let stsc = mp4::stsc_box(&[(1, PER_CHUNK)]);
    let stsz = mp4::stsz_box(SAMPLES, GAP);
    // The chunk offsets point at media bytes, and the media bytes slide forward when the header
    // grows, so they are written once the growth is known. An offset costs 4 bytes whatever offset
    // it states, which is what lets the placeholder pass measure the growth first.
    let stub = mp4::stbl_body(
        &bytes,
        &track,
        &[
            &stts,
            &stsc,
            &stsz,
            &mp4::stco_box(&[0u32; CHUNKS as usize]),
        ],
    );
    let growth = stub.len() - track.stbl.body_len;
    let at = u32::try_from(mp4::mdat_payload(&bytes) + growth)
        .expect("a chunk offset that fits a 32 bit stco entry");
    let body = mp4::stbl_body(
        &bytes,
        &track,
        &[&stts, &stsc, &stsz, &mp4::stco_box(&[at; CHUNKS as usize])],
    );
    assert_eq!(
        body.len(),
        stub.len(),
        "a chunk offset costs the same bytes whatever offset it states"
    );
    let mut rewritten = mp4::rewrite_stbl(&bytes, &track, &body);
    assert_eq!(
        mp4::mdat_payload(&rewritten),
        at as usize,
        "the media data moved by exactly the growth the header gained, so the chunk offsets point \
         at it"
    );
    // Every sample of the rewritten track is a 2-byte length prefix stating a text of nothing, so
    // a gap sample. Zeroing the 1024 bytes every chunk reads keeps a sample's own length from
    // being the verdict instead of the count.
    let region = GAP as usize * PER_CHUNK as usize;
    rewritten[at as usize..at as usize + region].fill(0);
    let after = mp4::locate(&rewritten, 4);
    let samples = mp4::samples(&rewritten, &after);
    assert_eq!(
        samples.len(),
        SAMPLES as usize,
        "the rewritten tables describe as many samples as they claim"
    );
    let (from_ticks, to_ticks) = mp4::window_ticks(&after, 0, TO_MS);
    assert_eq!(
        samples[0].pts_ticks, from_ticks,
        "the window starts on the track's first sample, so every sample it refuses is in the walk"
    );
    assert!(
        samples
            .last()
            .expect("a track with samples has a last one")
            .pts_ticks
            < to_ticks,
        "the last sample still presents before the window closes, so the sample count is what \
         stops this walk and not the window"
    );
    let start = Instant::now();
    let err = embedded_cues(Cursor::new(rewritten), 4, 0, TO_MS)
        .expect_err("a window holding more samples than the scan bound is Malformed");
    let MediaError::Malformed(message) = &err else {
        panic!("samples past the frame bound are Malformed, got {err:?}");
    };
    assert!(
        message.contains(&MAX_CUE_SCAN_FRAMES.to_string()),
        "the verdict is the frame bound, got {message}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the bound must be reached quickly, took {:?}",
        start.elapsed()
    );
}

/// A reader that counts the reads of one sample's worth of bytes at one offset - the cue walk's
/// reads of this test's samples, which all point at the same bytes - and fails the test when one
/// more is asked for than `limit`. A passing test is a run in which sample `limit + 1` was never
/// read, which is what "capped before the over-cap sample is read" has to mean.
struct CapReads {
    inner: Cursor<Vec<u8>>,
    /// Where every chunk of the rewritten track points.
    at: u64,
    /// The bytes one sample's `stsz` entry states: the only reads of this length in the file.
    size: usize,
    /// The reads the walk may still make before the cap must have stopped it.
    limit: usize,
    made: usize,
}

impl Read for CapReads {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.len() == self.size {
            let here = self
                .inner
                .stream_position()
                .expect("the position a read starts at");
            if here == self.at {
                assert!(
                    self.made < self.limit,
                    "the walk read sample {} of {} bytes from byte {}, past the {} a total over \
                     the cap stops it at",
                    self.made + 1,
                    self.size,
                    self.at,
                    self.limit
                );
                self.made += 1;
            }
        }
        self.inner.read(buf)
    }
}

impl Seek for CapReads {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn embedded_movtext_total_text_is_capped() {
    // 120 samples of 40 KiB each, every chunk pointing at the same 40 KiB at the front of the
    // media data. Each sample is well under the cap one sample may claim and the reads are sought
    // to one at a time, so overlapping samples are 120 reads of bytes a 57 KiB file really holds.
    // Together they are over the cap, and the sample that puts the running total over it must be
    // refused before its bytes are asked for: CapReads fails the test on that read.
    const SAMPLE_BYTES: u32 = 40 * 1024;
    const SAMPLES: u32 = 120;
    const TO_MS: u64 = 4_000;
    assert!(
        u64::from(SAMPLE_BYTES) < SUBTITLE_FILE_CAP,
        "one sample of this size is under the cap a single sample may claim, so the per-sample \
         bound is not what stops the walk"
    );
    assert!(
        u64::from(SAMPLES) * u64::from(SAMPLE_BYTES) > SUBTITLE_FILE_CAP,
        "the track's samples together are over it"
    );
    // How many of them fit under the cap before the next one puts the total over it.
    let reads_before_the_cap =
        usize::try_from(SUBTITLE_FILE_CAP / u64::from(SAMPLE_BYTES)).expect("a small count");
    assert!(
        u64::from(SAMPLES) > reads_before_the_cap as u64 + 1,
        "the track has more samples than the cap can stop, so the walk reaches the one over it"
    );
    let bytes = fixture(MP4);
    let track = mp4::locate(&bytes, 4);
    // One sample every millisecond of the track's 1 000 000 tick timescale: 120 samples fill 120
    // ms of the window, so the total is what stops the walk and not the window.
    let stts = mp4::stts_box(&[(SAMPLES, 1_000)]);
    let stsc = mp4::stsc_box(&[(1, 1)]);
    let stsz = mp4::stsz_box(SAMPLES, SAMPLE_BYTES);
    let stub = mp4::stbl_body(
        &bytes,
        &track,
        &[
            &stts,
            &stsc,
            &stsz,
            &mp4::stco_box(&[0u32; SAMPLES as usize]),
        ],
    );
    let growth = stub.len() - track.stbl.body_len;
    let at = u32::try_from(mp4::mdat_payload(&bytes) + growth)
        .expect("a chunk offset that fits a 32 bit stco entry");
    let body = mp4::stbl_body(
        &bytes,
        &track,
        &[&stts, &stsc, &stsz, &mp4::stco_box(&[at; SAMPLES as usize])],
    );
    assert_eq!(
        body.len(),
        stub.len(),
        "a chunk offset costs the same bytes whatever offset it states"
    );
    let mut rewritten = mp4::rewrite_stbl(&bytes, &track, &body);
    assert_eq!(
        mp4::mdat_payload(&rewritten),
        at as usize,
        "the media data moved by exactly the growth the header gained, so the chunk offsets point \
         at it"
    );
    // The samples state lengths the bytes they point at cannot hold a text in, which is the point
    // of this test, but each one's own 2-byte prefix has to say "no text" or a lying length would
    // be the verdict instead of the total.
    rewritten[at as usize..at as usize + 2].fill(0);
    let after = mp4::locate(&rewritten, 4);
    let samples = mp4::samples(&rewritten, &after);
    assert_eq!(
        samples.len(),
        SAMPLES as usize,
        "the rewritten tables describe as many samples as they claim"
    );
    let (from_ticks, to_ticks) = mp4::window_ticks(&after, 0, TO_MS);
    assert_eq!(
        samples[0].pts_ticks, from_ticks,
        "the window starts on the first sample"
    );
    assert!(
        samples
            .last()
            .expect("a track with samples has a last one")
            .pts_ticks
            < to_ticks,
        "the last sample presents before the window closes, so the running total is what stops \
         this walk and not the window"
    );
    let err = embedded_cues(
        CapReads {
            inner: Cursor::new(rewritten),
            at: at as u64,
            size: SAMPLE_BYTES as usize,
            limit: reads_before_the_cap,
            made: 0,
        },
        4,
        0,
        TO_MS,
    )
    .expect_err("more subtitle bytes in one window than the cap is Malformed");
    let MediaError::Malformed(message) = &err else {
        panic!("bytes past the total cap are Malformed, got {err:?}");
    };
    assert!(
        message.contains(&SUBTITLE_FILE_CAP.to_string()),
        "the verdict is the total-bytes bound, got {message}"
    );
}

/// A reader that answers one particular read - the read of `size` bytes starting at `at` - with a
/// timeout, and every other read with the file's own bytes. `read_sample_bytes` seeks to exactly
/// a sample's offset and asks for exactly the bytes its `stsz` entry states, so the read this
/// wrapper fails is that sample's read, and an `Io` verdict can only have come from it.
struct TimeoutAt {
    inner: Cursor<Vec<u8>>,
    at: u64,
    size: usize,
    timed_out: bool,
}

impl Read for TimeoutAt {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let here = self
            .inner
            .stream_position()
            .expect("the position a read starts at");
        if !self.timed_out && here == self.at && buf.len() == self.size {
            self.timed_out = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the bytes behind this sample have not arrived yet",
            ));
        }
        self.inner.read(buf)
    }
}

impl Seek for TimeoutAt {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn embedded_movtext_read_timeout_keeps_its_kind() {
    // The window [0, 4000) starts on the track's leading gap sample: the last one its tables place
    // at or before the window's start. The walk seeks to that sample's offset and asks for its
    // bytes, and TimeoutAt answers exactly that read the way a torrent whose tail has not arrived
    // answers a read, with `TimedOut`. The `moov` sits ahead of the `mdat` in this fixture and is
    // read whole before any sample is touched, so the read this wrapper fails is a read of media
    // data and not of a table.
    const FROM_MS: u64 = 0;
    const TO_MS: u64 = 4_000;
    let bytes = fixture(MP4);
    let track = mp4::locate(&bytes, 4);
    assert_eq!(
        mp4::cues(&bytes, &track),
        embedded_expected(),
        "the sample this test stalls is placed by the same tables that gave the fixture's cues"
    );
    let samples = mp4::samples(&bytes, &track);
    let (from_ticks, _) = mp4::window_ticks(&track, FROM_MS, TO_MS);
    let landing = samples
        .iter()
        .rfind(|sample| sample.pts_ticks <= from_ticks)
        .expect("a sample at or before the window's start");
    let payload = u64::try_from(mp4::mdat_payload(&bytes)).expect("a file under 4 GiB");
    assert!(
        landing.offset >= payload,
        "the first sample this window reads is inside the media data, past the moov the walk read \
         first"
    );
    let err = embedded_cues(
        TimeoutAt {
            inner: Cursor::new(bytes),
            at: landing.offset,
            size: usize::try_from(landing.size).expect("a sample size that fits this platform"),
            timed_out: false,
        },
        4,
        FROM_MS,
        TO_MS,
    )
    .expect_err("a sample read that timed out is a reader that failed, not a window with no cues");
    match err {
        MediaError::Io(io) => assert_eq!(
            io.kind(),
            std::io::ErrorKind::TimedOut,
            "a timed-out sample read keeps the reader's kind: bytes that have not arrived yet say \
             nothing about the file, and a retryable read must not turn into a dead track"
        ),
        other => panic!("a timed-out sample read is MediaError::Io, got {other:?}"),
    }
}
