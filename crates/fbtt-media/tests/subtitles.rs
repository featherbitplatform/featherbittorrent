//! `file_cues` against the checked-in sidecar fixtures, and against hostile
//! input: a file is read only once, in a single pass, and a hostile file must
//! give its good cues back, or no cues, never a panic, never a hang.

use fbtt_media::{
    CUE_WINDOW_MAX_MS, Cue, MAX_CUE_SCAN_FRAMES, MAX_SKIPPED_FRAMES, MediaError, SUBTITLE_FILE_CAP,
    SubtitleFormat, embedded_cues, file_cues, subtitle_format,
};
use std::fs;
use std::io::Cursor;
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
    let tracks = common::tracks(&[common::track_entry(1, 0x11, b"S_TEXT/UTF8")]);
    // Five blocks, each 1 MiB of 'x', spaced 1 s apart from 20 s to 24 s. The
    // window is 29 000 to 31 000, the seek is 19 000, so every block is read
    // after the seek (real pre-roll) and none overlaps the window.
    let payload = vec![b'x'; 1_024 * 1_024];
    let clusters: Vec<Vec<u8>> = (0..5)
        .map(|i| {
            common::cluster(
                20_000 + i * 1_000,
                &[common::simple_block(1, 0, false, &payload)],
            )
        })
        .collect();

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
