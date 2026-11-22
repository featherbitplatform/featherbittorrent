//! `file_cues` against the checked-in sidecar fixtures, and against hostile
//! input: a file is read only once, in a single pass, and a hostile file must
//! give its good cues back, or no cues, never a panic, never a hang.

use fbtt_media::{Cue, MediaError, SubtitleFormat, file_cues, subtitle_format};
use std::fs;

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
