//! Text subtitles read from sidecar files: SRT, ASS/SSA and WebVTT.
//!
//! A subtitle file comes from a torrent and is hostile input: nothing here may
//! panic on one, hang on one, or allocate more than a multiple of the file's
//! own size. Malformed cue blocks inside an otherwise valid file are skipped,
//! never an error: a caption file with one bad cue still has the rest of the
//! conversation to show. The only whole-file error is the cue count, which
//! keeps one file from holding a million cues behind a small byte budget.
//!
//! Embedded subtitles, the cues a Matroska or MP4 track carries for itself,
//! are not read here. They need a track's frames, and those arrive with the
//! Matroska and MP4 readers.

use crate::{MAX_CUES, MediaError, SUBTITLE_FILE_CAP};

/// One line of a subtitle, in source time.
///
/// `start_ms` and `end_ms` are milliseconds of the video's own timeline, the
/// same timeline the probe reports and the player seeks on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    /// When the line appears, in milliseconds.
    pub start_ms: u64,
    /// When it goes away, in milliseconds. Always greater than `start_ms`.
    pub end_ms: u64,
    /// What it says, already cleaned: override blocks and tags removed, and
    /// the format's line breaks turned into newlines.
    pub text: String,
}

/// The text format a sidecar subtitle file is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleFormat {
    /// SubRip: a cue number, a timing line, one or more text lines.
    Srt,
    /// SubStation Alpha: an `[Events]` table of `Dialogue:` lines. SSA is
    /// read the same way, with its own column order.
    Ass,
    /// WebVTT: a `WEBVTT` header, then cue timing lines and text.
    Vtt,
}

/// What format a subtitle file is, from its name's extension.
///
/// `.srt`, `.ass`, `.ssa` and `.vtt`, case-insensitive. Anything else, a file
/// with no name, or a name with no extension is no subtitle file here.
pub fn subtitle_format(file_name: &str) -> Option<SubtitleFormat> {
    let ext = file_name.rsplit('.').next()?;
    match ext.to_ascii_lowercase().as_str() {
        "srt" => Some(SubtitleFormat::Srt),
        "ass" | "ssa" => Some(SubtitleFormat::Ass),
        "vtt" => Some(SubtitleFormat::Vtt),
        _ => None,
    }
}

/// The cues a sidecar subtitle file holds, sorted by `start_ms`.
///
/// The file is at most [`SUBTITLE_FILE_CAP`] bytes, and no file may hold more
/// than [`MAX_CUES`] cues: past either bound it is [`MediaError::Unsupported`]
/// and [`MediaError::Malformed`] respectively, before or during the parse.
/// A cue whose block does not parse is skipped, so a hostile file gives its
/// good cues back and drops the rest rather than failing the file.
///
/// A cue with `end_ms <= start_ms` is dropped too: it would never be on
/// screen, and a file of them would only grow the list.
pub fn file_cues(bytes: &[u8], format: SubtitleFormat) -> Result<Vec<Cue>, MediaError> {
    if (bytes.len() as u64) > SUBTITLE_FILE_CAP {
        return Err(MediaError::Unsupported(format!(
            "subtitle file is {} bytes, over the {} byte cap",
            bytes.len(),
            SUBTITLE_FILE_CAP
        )));
    }
    let text = decode(bytes);
    let mut cues: Vec<Cue> = match format {
        SubtitleFormat::Srt => srt_cues(&text),
        SubtitleFormat::Ass => ass_cues(&text),
        SubtitleFormat::Vtt => vtt_cues(&text),
    };
    if cues.len() > MAX_CUES {
        return Err(MediaError::Malformed(format!(
            "subtitle file holds more than {MAX_CUES} cues"
        )));
    }
    cues.sort_by_key(|cue| cue.start_ms);
    cues.retain(|cue| cue.end_ms > cue.start_ms);
    Ok(cues)
}

/// Strip a UTF-8 BOM and decode lossily.
///
/// A sidecar file may be encoded in anything, and the lossy decode is what
/// keeps a hostile byte sequence out of a panic. The BOM, when one is there,
/// would otherwise come back as a zero width character at the start of the
/// first cue's text.
fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// The file's lines, accepting `\n`, `\r\n` and a lone `\r`.
///
/// Subtitle files are written by hand, by converters and by players on every
/// platform, so all three line endings occur in the wild. A `\r\n` is one
/// line ending, not two, so a cue's number line and its timing line stay
/// adjacent. A trailing newline leaves one empty line, and the parsers treat
/// empty lines as the separators they are.
fn lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                out.push(std::mem::take(&mut current));
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// SRT cue blocks: an optional number line, a timing line, one or more text
/// lines, then an empty line. A block whose timing line does not parse is
/// skipped to its empty line, and the rest of the file still parses.
fn srt_cues(text: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    let lines = lines(text);
    let mut i = 0;
    while i < lines.len() {
        // Skip the blanks between blocks.
        while i < lines.len() && lines[i].is_empty() {
            i += 1;
        }
        if i >= lines.len() {
            break;
        }
        // The cue number, when there is one: it is the line before a timing
        // line, never a text line.
        let number_here =
            lines[i].trim().chars().all(|c| c.is_ascii_digit()) && !lines[i].trim().is_empty();
        if number_here {
            i += 1;
            if i >= lines.len() || srt_timing(&lines[i]).is_none() {
                // A number with no timing line after it: a block out of place,
                // to the next empty line.
                skip_block(&lines, &mut i);
                continue;
            }
        } else if srt_timing(&lines[i]).is_none() {
            // Not a number and not a timing: a text line or garbage, and it
            // runs to the next empty line.
            skip_block(&lines, &mut i);
            continue;
        }
        let (start_ms, end_ms) = match srt_timing(&lines[i]) {
            Some(timing) => timing,
            None => unreachable!("checked above"),
        };
        i += 1;
        let mut text_lines = Vec::new();
        while i < lines.len() && !lines[i].is_empty() {
            text_lines.push(lines[i].clone());
            i += 1;
        }
        let text = clean_html_tags(&text_lines.join("\n"));
        push_cue(&mut cues, start_ms, end_ms, text);
    }
    cues
}

/// Walk past a line that is not a cue block's start.
fn skip_block(lines: &[String], i: &mut usize) {
    while *i < lines.len() && !lines[*i].is_empty() {
        *i += 1;
    }
}

/// `HH:MM:SS,mmm --> HH:MM:SS,mmm`.
fn srt_timing(line: &str) -> Option<(u64, u64)> {
    let line = line.trim();
    let (start, rest) = line.split_once("-->")?;
    let start = srt_time(start.trim())?;
    let end = srt_time(rest.trim())?;
    Some((start, end))
}

/// One SRT time, `HH:MM:SS,mmm`, in milliseconds.
///
/// The fields are parsed as decimal strings and combined with checked
/// arithmetic, so an hours value that overflows `u64` milliseconds is a cue
/// that does not parse, not a panic. A cue whose timestamp does not fit the
/// millisecond arithmetic of the file is skipped, and the rest of the file
/// still comes back.
fn srt_time(field: &str) -> Option<u64> {
    let (hms, millis) = field.split_once(',')?;
    let (h, ms) = hms.split_once(':')?;
    let (m, s) = ms.split_once(':')?;
    let millis: u64 = millis.trim().parse().ok()?;
    if millis > 999 {
        return None;
    }
    let h = parse_hms(h.trim(), 3_600_000)?;
    let m = parse_hms(m.trim(), 60_000)?;
    let s = parse_hms(s.trim(), 1_000)?;
    h.checked_add(m)?.checked_add(s)?.checked_add(millis)
}

/// One ASS time, `H:MM:SS.cc`, in milliseconds.
fn ass_time(field: &str) -> Option<u64> {
    let field = field.trim();
    let (hms, cents) = field.split_once('.')?;
    let cents: u64 = cents.trim().parse().ok()?;
    if cents > 99 {
        return None;
    }
    let (h, ms) = hms.split_once(':')?;
    let (m, s) = ms.split_once(':')?;
    let h = parse_hms(h.trim(), 3_600_000)?;
    let m = parse_hms(m.trim(), 60_000)?;
    let s = parse_hms(s.trim(), 1_000)?;
    h.checked_add(m)?
        .checked_add(s)?
        .checked_add(cents.checked_mul(10)?)
}

/// One WebVTT time, `[HH:]MM:SS.mmm`, in milliseconds.
fn vtt_time(field: &str) -> Option<u64> {
    let (hms, millis) = field.split_once('.')?;
    let millis: u64 = millis.trim().parse().ok()?;
    if millis > 999 {
        return None;
    }
    let parts: Vec<&str> = hms.split(':').collect();
    let (h, m, s) = match parts.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => ("0", *m, *s),
        _ => return None,
    };
    let h = parse_hms(h.trim(), 3_600_000)?;
    let m = parse_hms(m.trim(), 60_000)?;
    let s = parse_hms(s.trim(), 1_000)?;
    h.checked_add(m)?.checked_add(s)?.checked_add(millis)
}

/// One decimal field, as a multiple of `per`, with the field's own range
/// capped so the multiplication is checked against a known bound.
///
/// The cap is `1_000_000_000` per field. A field that fits the format's own
/// range but overflows the milliseconds of the whole timestamp is then
/// caught by the checked multiplication, which is the same shape as a field
/// that overflows the format's range.
fn parse_hms(field: &str, per: u64) -> Option<u64> {
    let v: u128 = field.trim().parse().ok()?;
    if v >= 1_000_000_000 {
        return None;
    }
    let ms = v * per as u128;
    u64::try_from(ms).ok()
}

/// ASS: the `[Events]` section, a `Format:` line that names its columns, and
/// one cue per `Dialogue:` line.
///
/// When there is no `[Events]` section, the whole file is the events, and the
/// standard column order is used. When the `[Events]` section has no
/// `Format:` line, the standard order is used too. The standard order is the
/// one the prompt spells out: Layer, Start, End, Style, Name, MarginL,
/// MarginR, MarginV, Effect, Text.
fn ass_cues(text: &str) -> Vec<Cue> {
    // The [Events] section, or the whole file when there is no section header.
    let events = match text.find("[Events]") {
        Some(pos) => text[pos..].trim_start_matches("[Events]"),
        None => text,
    };
    let format_fields: Vec<String> = events
        .lines()
        .find_map(|line| {
            let fields = line.strip_prefix("Format:")?;
            Some(
                fields
                    .split(',')
                    .map(|f| f.trim().to_ascii_lowercase())
                    .filter(|f| !f.is_empty())
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_else(|| {
            [
                "layer", "start", "end", "style", "name", "marginl", "marginr", "marginv",
                "effect", "text",
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        });
    let (start_col, end_col, text_col) = match (
        format_fields.iter().position(|f| f == "start"),
        format_fields.iter().position(|f| f == "end"),
        format_fields.iter().position(|f| f == "text"),
    ) {
        (Some(s), Some(e), Some(t)) => (s, e, t),
        _ => return Vec::new(),
    };
    let mut cues = Vec::new();
    for line in events.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("Dialogue:") else {
            continue;
        };
        let fields: Vec<&str> = rest.split(',').collect();
        if fields.len() <= text_col {
            continue;
        }
        let start = ass_time(fields.get(start_col).copied().unwrap_or_default());
        let end = ass_time(fields.get(end_col).copied().unwrap_or_default());
        let (start, end) = match (start, end) {
            (Some(s), Some(e)) => (s, e),
            _ => continue,
        };
        // The text is everything after the text column's leading commas, so a
        // comma inside the text stays whole.
        let mut tail = rest;
        for _ in 0..text_col {
            let Some(pos) = tail.find(',') else {
                break;
            };
            tail = &tail[pos + 1..];
        }
        let text = clean_ass(tail);
        push_cue(&mut cues, start, end, text);
    }
    cues
}

/// WebVTT: the `WEBVTT` header, `NOTE`/`STYLE`/`REGION` blocks and cue timing
/// lines. A cue may open with an identifier line before its timing line, and
/// the timing line may carry cue settings after the end time, which are
/// ignored.
fn vtt_cues(text: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    let lines = lines(text);
    let mut i = 0;
    // The header block: WEBVTT and its metadata lines, to the first blank.
    while i < lines.len() {
        let is_header = lines[i].trim_start().starts_with("WEBVTT");
        if is_header {
            i += 1;
            while i < lines.len() && !lines[i].is_empty() {
                i += 1;
            }
            break;
        }
        if lines[i].is_empty() {
            i += 1;
        } else {
            break;
        }
    }
    while i < lines.len() {
        while i < lines.len() && lines[i].is_empty() {
            i += 1;
        }
        if i >= lines.len() {
            break;
        }
        // NOTE, STYLE and REGION blocks run to the next blank line.
        if is_vtt_block(&lines[i]) {
            i += 1;
            while i < lines.len() && !lines[i].is_empty() {
                i += 1;
            }
            continue;
        }
        let timing_line = if vtt_timing(&lines[i]).is_some() {
            i
        } else if i + 1 < lines.len() && vtt_timing(&lines[i + 1]).is_some() {
            // An identifier line, when there is one.
            i + 1
        } else {
            // Neither this line nor the next is a timing line: skip to the
            // blank.
            skip_block_vtt(&lines, &mut i);
            continue;
        };
        let (start_ms, end_ms) = match vtt_timing(&lines[timing_line]) {
            Some(timing) => timing,
            None => continue,
        };
        i = timing_line + 1;
        let mut text_lines = Vec::new();
        while i < lines.len() && !lines[i].is_empty() {
            text_lines.push(lines[i].clone());
            i += 1;
        }
        let text = clean_html_tags(&text_lines.join("\n"));
        push_cue(&mut cues, start_ms, end_ms, text);
    }
    cues
}

/// A `NOTE`, `STYLE` or `REGION` block's first line.
fn is_vtt_block(line: &str) -> bool {
    let trimmed = line.trim_start();
    ["NOTE", "STYLE", "REGION"]
        .iter()
        .any(|tag| trimmed == *tag || trimmed.starts_with(&format!("{tag} ")))
}

/// `00:00:00.500 --> 00:00:02.000`, the hours optional, cue settings after
/// the end time ignored.
fn vtt_timing(line: &str) -> Option<(u64, u64)> {
    let line = line.trim();
    let (start, rest) = line.split_once("-->")?;
    let start = vtt_time(start.trim())?;
    let end_field = rest.split_whitespace().next()?;
    let end = vtt_time(end_field)?;
    Some((start, end))
}

/// Skip a VTT block that is not a cue: a line that is neither a block header
/// nor a timing line, running to the next blank.
fn skip_block_vtt(lines: &[String], i: &mut usize) {
    while *i < lines.len() && !lines[*i].is_empty() {
        *i += 1;
    }
}

/// A cleaned cue, dropped when it has no time or nothing to say.
fn push_cue(cues: &mut Vec<Cue>, start_ms: u64, end_ms: u64, text: String) {
    if text.is_empty() {
        return;
    }
    cues.push(Cue {
        start_ms,
        end_ms,
        text,
    });
}

/// ASS text: `{...}` override blocks gone, `\N` and `\n` newlines, `\h` a
/// space. The rest of the text is kept, so a cue that does not use
/// overrides is unchanged.
fn clean_ass(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' => {
                // Drop to the closing brace, or the end when there is none.
                match chars[i + 1..].iter().position(|&c| c == '}') {
                    Some(close) => i = i + 2 + close,
                    None => break,
                }
            }
            '\\' => match chars.get(i + 1) {
                Some('N') | Some('n') => {
                    i += 2;
                    out.push('\n');
                }
                Some('h') => {
                    i += 2;
                    out.push(' ');
                }
                _ => {
                    out.push(chars[i]);
                    i += 1;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// SRT and VTT text: `<...>` tags gone, except `<i>`, `<b>` and `<u>` and
/// their closing forms, which the WebVTT engine renders.
fn clean_html_tags(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            let Some(close) = chars[i + 1..].iter().position(|&c| c == '>') else {
                // An unclosed tag: drop to the end.
                break;
            };
            let tag: String = chars[i + 1..i + 1 + close].iter().collect();
            let tag = tag.trim();
            let kept = matches!(tag, "i" | "b" | "u" | "/i" | "/b" | "/u");
            if kept {
                out.push('<');
                out.push_str(tag);
                out.push('>');
            }
            i = i + 2 + close;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MediaError;

    /// The file's cues, with the fixture's times: two-line cues joined with
    /// newlines, and the file's own blank lines no cues.
    fn srt_fixture() -> Vec<Cue> {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sub.srt"
        ))
        .expect("sub.srt");
        file_cues(&bytes, SubtitleFormat::Srt).expect("srt")
    }

    #[test]
    fn file_cues_refuses_an_oversized_file() {
        let mut big = vec![0u8; (SUBTITLE_FILE_CAP as usize) + 1];
        big[0] = b'1';
        let err = file_cues(&big, SubtitleFormat::Srt).expect_err("over the cap");
        assert!(
            matches!(err, MediaError::Unsupported(_)),
            "an oversized file is Unsupported, got {err:?}"
        );
    }

    #[test]
    fn a_file_at_the_cap_is_accepted() {
        // Exactly the cap's size is allowed: the rule is over the cap.
        let at_cap = vec![b'1'; SUBTITLE_FILE_CAP as usize];
        let result = file_cues(&at_cap, SubtitleFormat::Srt);
        assert!(result.is_ok(), "exactly the cap is allowed");
    }

    #[test]
    fn srt_cues_match_the_fixture() {
        let cues = srt_fixture();
        assert_eq!(cues.len(), 3, "the fixture has three cues");
        assert_eq!(cues[0].start_ms, 500);
        assert_eq!(cues[0].end_ms, 2000);
        assert_eq!(cues[0].text, "First subtitle line\nspanning two lines");
        assert_eq!(cues[1].start_ms, 2500);
        assert_eq!(cues[1].end_ms, 3000);
        assert_eq!(cues[1].text, "Second subtitle line");
        assert_eq!(cues[2].start_ms, 3200);
        assert_eq!(cues[2].end_ms, 3900);
        assert_eq!(cues[2].text, "Third subtitle line");
    }

    #[test]
    fn huge_hours_are_skipped_not_a_panic() {
        let bytes = b"1\r\n99999999999999999999:00:00,000 --> 99999999999999999999:00:01,000\r\nHuge\r\n\r\n2\r\n00:00:01,000 --> 00:00:02,000\r\nSmall\r\n";
        let cues = file_cues(bytes, SubtitleFormat::Srt).expect("no panic");
        assert_eq!(cues.len(), 1, "the huge cue is skipped");
        assert_eq!(cues[0].start_ms, 1000);
        assert_eq!(cues[0].text, "Small");
    }

    #[test]
    fn ass_overrides_and_commas_are_whole() {
        let bytes = b"[Events]\nFormat: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: Marked=0,0:00:00.50,0:00:02.00,Default,,0,0,0,,Tagged, {\\i1}italic{\\i0} text, still whole\n";
        let cues = file_cues(bytes, SubtitleFormat::Ass).expect("no panic");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Tagged, italic text, still whole");
    }

    #[test]
    fn vtt_header_note_and_identifier_are_skipped() {
        let bytes = b"WEBVTT\n\nNOTE a comment\nmore note\n\n1\n00:00:01.000 --> 00:00:02.000 align:start\nIdentified\n\n00:00:03.000 --> 00:00:04.000\nPlain\n";
        let cues = file_cues(bytes, SubtitleFormat::Vtt).expect("no panic");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 1000);
        assert_eq!(cues[0].text, "Identified");
        assert_eq!(cues[1].start_ms, 3000);
        assert_eq!(cues[1].text, "Plain");
    }

    #[test]
    fn cues_are_sorted_and_nonpositive_are_dropped() {
        let bytes = b"3\r\n00:00:03,000 --> 00:00:04,000\r\nThree\r\n\r\n1\r\n00:00:01,000 --> 00:00:02,000\r\nOne\r\n\r\n2\r\n00:00:01,000 --> 00:00:01,000\r\nZero length, dropped\r\n";
        let cues = file_cues(bytes, SubtitleFormat::Srt).expect("no panic");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 1000);
        assert_eq!(cues[0].text, "One");
        assert_eq!(cues[1].start_ms, 3000);
        assert_eq!(cues[1].text, "Three");
    }

    #[test]
    fn a_bom_is_stripped() {
        let bytes = b"\xef\xbb\xbf1\r\n00:00:00,500 --> 00:00:01,000\r\nBom\r\n";
        let cues = file_cues(bytes, SubtitleFormat::Srt).expect("no panic");
        assert_eq!(cues[0].text, "Bom");
    }

    #[test]
    fn garbage_is_never_a_panic() {
        let garbage: Vec<u8> = (0..64 * 1024).map(|i| (i * 7 + 3) as u8).collect();
        for format in [
            SubtitleFormat::Srt,
            SubtitleFormat::Ass,
            SubtitleFormat::Vtt,
        ] {
            let result = file_cues(&garbage, format);
            assert!(
                result.is_ok() || matches!(result, Err(MediaError::Malformed(_))),
                "garbage gives Ok or Malformed, got {result:?}"
            );
        }
    }

    #[test]
    fn srt_tags_keep_the_ones_webvtt_renders() {
        let bytes = b"1\r\n00:00:00,000 --> 00:00:01,000\r\n<b>bold</b> and <i>italic</i> and <strike>gone</strike>\r\n";
        let cues = file_cues(bytes, SubtitleFormat::Srt).expect("no panic");
        assert_eq!(cues[0].text, "<b>bold</b> and <i>italic</i> and gone");
    }

    #[test]
    fn ass_default_columns_with_no_format_line() {
        let bytes = b"Dialogue: 0,0:00:00.50,0:00:02.00,Default,,0,0,0,,Default order\n";
        let cues = file_cues(bytes, SubtitleFormat::Ass).expect("no panic");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_ms, 500);
        assert_eq!(cues[0].end_ms, 2000);
        assert_eq!(cues[0].text, "Default order");
    }
}
