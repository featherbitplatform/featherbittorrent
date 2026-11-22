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
    // 20 digits of hours: the milliseconds overflow a u64, so the cue is
    // unparsable and skipped, and the valid cue still comes back.
    let bytes = b"1\r\n99999999999999999999:00:00,000 --> 99999999999999999999:00:01,000\r\nHuge\r\n\r\n2\r\n00:00:01,000 --> 00:00:02,000\r\nSmall\r\n";
    let cues = file_cues(bytes, SubtitleFormat::Srt).expect("no panic");
    assert_eq!(cues.len(), 1, "the huge cue is skipped, not a panic");
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
