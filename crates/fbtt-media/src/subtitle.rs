//! Text subtitles read from sidecar files: SRT, ASS/SSA and WebVTT.
//!
//! A subtitle file comes from a torrent and is hostile input: nothing here may
//! panic on one, hang on one, or allocate more than a multiple of the file's
//! own size. The parsers borrow their lines from the decoded text rather than
//! copy them, so a 4 MiB file holds one decoded `String` and a few small
//! scratch buffers, not a `Vec<String>` of every line.
//!
//! Malformed cue blocks inside an otherwise valid file are skipped, never an
//! error: a caption file with one bad cue still has the rest of the
//! conversation to show. The only whole-file error is the cue count, which is
//! enforced the moment a kept cue would be number [`MAX_CUES`] + 1.
//!
//! Subtitles embedded in a Matroska or WebM file are read the same way:
//! [`embedded_cues`] opens the file through the same guarded, budgeted
//! Matroska open [`open_track`](crate::open_track) uses, walks the
//! frames of one subtitle track, and keeps the cues that overlap the asked
//! window. A frame is at most the file cap and a track is at most the cue
//! cap, so a hostile file holds one decoded string and a bounded cue list.

use crate::capped::ReadBudget;
use crate::codec::Codec;
use crate::mkv::{guarded_next, guarded_seek};
use crate::probe::{matroska_kind, open_matroska};
use crate::{
    CUE_WINDOW_MAX_MS, ELEMENT_CAP, MAX_CUE_SCAN_FRAMES, MAX_CUES, MAX_SKIPPED_FRAMES, MediaError,
    SUBTITLE_FILE_CAP,
};
use matroska_demuxer::Frame;
use std::io::{Read, Seek};

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
/// `.srt`, `.ass`, `.ssa` and `.vtt`, case-insensitive. A name without a
/// dot, or with an empty stem (`"srt"`, `".srt"`), is no subtitle file here.
pub fn subtitle_format(file_name: &str) -> Option<SubtitleFormat> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
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
/// and [`MediaError::Malformed`] respectively. The cue cap is enforced while
/// parsing, the moment a kept cue would be number `MAX_CUES + 1`, so a file
/// of a million cues does not grow a list of a million cues first.
///
/// A cue whose block does not parse is skipped, so a hostile file gives its
/// good cues back and drops the rest rather than failing the file. A cue with
/// `end_ms <= start_ms` is dropped before it counts: it would never be on
/// screen, and a file of them is not a file with too many cues.
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
        SubtitleFormat::Srt => srt_cues(&text)?,
        SubtitleFormat::Ass => ass_cues(&text)?,
        SubtitleFormat::Vtt => vtt_cues(&text)?,
    };
    cues.sort_by_key(|cue| cue.start_ms);
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

/// The file's lines, as borrowed slices of `text`, accepting `\n`, `\r\n`
/// and a lone `\r` as one line ending.
///
/// Subtitle files are written by hand, by converters and by players on every
/// platform, so all three line endings occur in the wild. A `\r\n` is one
/// line ending, not two, so a cue's number line and its timing line stay
/// adjacent. The lines borrow from `text`, so a 4 MiB file holds one decoded
/// `String` and a slice of its line starts, not a copy of every line.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        match rest.find(['\n', '\r']) {
            None => Some(std::mem::take(&mut rest)),
            Some(i) => {
                let line = &rest[..i];
                let crlf =
                    rest.as_bytes()[i] == b'\r' && rest.as_bytes().get(i + 1) == Some(&b'\n');
                rest = &rest[i + if crlf { 2 } else { 1 }..];
                Some(line)
            }
        }
    })
}

/// Add a cue to `cues`, dropping it when it has no text or `end_ms` does not
/// exceed `start_ms`, and refusing the file when a kept cue would be number
/// [`MAX_CUES`] + 1.
fn push_cue(
    cues: &mut Vec<Cue>,
    start_ms: u64,
    end_ms: u64,
    text: String,
) -> Result<(), MediaError> {
    if text.is_empty() || end_ms <= start_ms {
        return Ok(());
    }
    if cues.len() >= MAX_CUES {
        return Err(MediaError::Malformed(format!(
            "subtitle file holds more than {MAX_CUES} cues"
        )));
    }
    cues.push(Cue {
        start_ms,
        end_ms,
        text,
    });
    Ok(())
}

/// SRT cue blocks: an optional number line, a timing line, one or more text
/// lines, then an empty line. A block whose timing line does not parse is
/// skipped to its empty line, and the rest of the file still parses.
fn srt_cues(text: &str) -> Result<Vec<Cue>, MediaError> {
    let mut cues = Vec::new();
    let lines: Vec<&str> = lines(text).collect();
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
            !lines[i].trim().is_empty() && lines[i].trim().chars().all(|c| c.is_ascii_digit());
        if number_here {
            i += 1;
            if i >= lines.len() {
                break;
            }
        }
        // The timing line, parsed once. A number with no timing line after
        // it, or a line that is neither a number nor a timing, is a block out
        // of place: skip it to the next empty line.
        let Some((start_ms, end_ms)) = srt_timing(lines[i]) else {
            skip_block(&lines, &mut i);
            continue;
        };
        i += 1;
        let text_start = i;
        while i < lines.len() && !lines[i].is_empty() {
            i += 1;
        }
        let mut text = String::new();
        for (idx, line) in lines[text_start..i].iter().enumerate() {
            if idx > 0 {
                text.push('\n');
            }
            text.push_str(line);
        }
        push_cue(&mut cues, start_ms, end_ms, clean_html_tags(&text))?;
    }
    Ok(cues)
}

/// Walk past a line that is not a cue block's start.
fn skip_block(lines: &[&str], i: &mut usize) {
    while *i < lines.len() && !lines[*i].is_empty() {
        *i += 1;
    }
}

/// `HH:MM:SS,mmm --> HH:MM:SS.mmm`, the milliseconds separated by `,` or `.`,
/// and only the first whitespace-separated token after `-->` is the end time:
/// SRT `X1:` coordinates and anything else are ignored.
fn srt_timing(line: &str) -> Option<(u64, u64)> {
    let line = line.trim();
    let (start, rest) = line.split_once("-->")?;
    let start = srt_time(start.trim())?;
    let end = srt_time(rest.split_whitespace().next()?)?;
    Some((start, end))
}

/// One SRT time, `HH:MM:SS,mmm` or `HH:MM:SS.mmm`, in milliseconds.
///
/// The fields are parsed as ASCII decimal strings and combined with checked
/// arithmetic, so an hours value that overflows `u64` milliseconds is a cue
/// that does not parse, not a panic. A cue whose timestamp does not fit the
/// millisecond arithmetic of the file is skipped, and the rest of the file
/// still comes back.
fn srt_time(field: &str) -> Option<u64> {
    let field = field.trim();
    let (hms, millis) = field.rsplit_once(',').or_else(|| field.rsplit_once('.'))?;
    let (h, ms) = hms.split_once(':')?;
    let (m, s) = ms.split_once(':')?;
    let millis = ascii_u64(millis.trim())?;
    if millis > 999 {
        return None;
    }
    let h = parse_hms(h.trim(), 3_600_000, u64::MAX)?;
    let m = parse_hms(m.trim(), 60_000, 59)?;
    let s = parse_hms(s.trim(), 1_000, 59)?;
    h.checked_add(m)?.checked_add(s)?.checked_add(millis)
}

/// One ASS time, `H:MM:SS.cc`, in milliseconds.
fn ass_time(field: &str) -> Option<u64> {
    let field = field.trim();
    let (hms, cents) = field.rsplit_once('.')?;
    let cents = ascii_u64(cents.trim())?;
    if cents > 99 {
        return None;
    }
    let (h, ms) = hms.split_once(':')?;
    let (m, s) = ms.split_once(':')?;
    let h = parse_hms(h.trim(), 3_600_000, u64::MAX)?;
    let m = parse_hms(m.trim(), 60_000, 59)?;
    let s = parse_hms(s.trim(), 1_000, 59)?;
    h.checked_add(m)?
        .checked_add(s)?
        .checked_add(cents.checked_mul(10)?)
}

/// One WebVTT time, `[HH:]MM:SS.mmm`, in milliseconds.
fn vtt_time(field: &str) -> Option<u64> {
    let field = field.trim();
    let (hms, millis) = field.rsplit_once('.')?;
    let millis = ascii_u64(millis.trim())?;
    if millis > 999 {
        return None;
    }
    let (h, m, s) = match hms.split(':').collect::<Vec<_>>().as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => ("", *m, *s),
        _ => return None,
    };
    let h = parse_hms(h.trim(), 3_600_000, u64::MAX)?;
    let m = parse_hms(m.trim(), 60_000, 59)?;
    let s = parse_hms(s.trim(), 1_000, 59)?;
    h.checked_add(m)?.checked_add(s)?.checked_add(millis)
}

/// One ASCII decimal field, as a multiple of `per`, not exceeding `max`.
///
/// A leading `+` or `-` makes the field unparsable: the formats use unsigned
/// fields, and a signed one is a cue out of place. The value is multiplied
/// with checked arithmetic, so a value that fits the field's own range but
/// overflows the milliseconds of the whole timestamp is a no-cue, not a
/// panic.
fn parse_hms(field: &str, per: u64, max: u64) -> Option<u64> {
    let v = ascii_u64(field.trim())?;
    if v > max {
        return None;
    }
    v.checked_mul(per)
}

/// An ASCII decimal unsigned integer. A leading `+` or `-`, or any non-digit
/// byte, is `None`.
fn ascii_u64(field: &str) -> Option<u64> {
    let bytes = field.as_bytes();
    let mut v: u128 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as u128)?;
        if v > u64::MAX as u128 {
            return None;
        }
    }
    Some(v as u64)
}

/// A line whose trimmed text is exactly `[Events]`, compared ASCII
/// case-insensitively, so `[events]` and `  [Events]  ` are headers too.
/// Comparing by byte keeps the check from allocating.
fn is_events_header(line: &str) -> bool {
    let b = line.trim().as_bytes();
    b.len() == 8
        && b[0] == b'['
        && b[7] == b']'
        && b[1..7]
            .iter()
            .zip(b"events".iter())
            .all(|(a, c)| a.to_ascii_lowercase() == *c)
}

/// A section header: a line whose trimmed text starts with `[` and ends with
/// `]`. Scanning line by line keeps a `[` inside a `Dialogue:` text from being
/// mistaken for one.
fn is_section_header(line: &str) -> bool {
    let b = line.trim().as_bytes();
    b.len() >= 2 && b[0] == b'[' && b[b.len() - 1] == b']'
}

/// The `[Events]` section of an ASS file: from just after its header line to
/// the next `[...]` section header, or to the end of the file when there is
/// none. `None` when the file has no `[Events]` header.
///
/// The header is a whole line, compared ASCII case-insensitively: the text
/// `[Events]` inside a `Title:` line is not a section. A line start's offset
/// is the distance between its pointer and the text's, so the scan is one
/// forward pass over the bytes.
fn events_section(text: &str) -> Option<&str> {
    let base = text.as_ptr() as usize;
    let header_line = lines(text).find(|line| is_events_header(line))?;
    // The body begins just past the header line's ending.
    let after = (header_line.as_ptr() as usize - base) + header_line.len();
    let nl = text[after..].find(['\n', '\r'])?;
    let advance = if text.as_bytes().get(after + nl) == Some(&b'\r')
        && text.as_bytes().get(after + nl + 1) == Some(&b'\n')
    {
        nl + 2
    } else {
        nl + 1
    };
    let body = &text[after + advance..];
    // The next section header ends the section.
    let mut offset = 0usize;
    for line in lines(body) {
        if is_section_header(line) {
            return Some(&body[..offset]);
        }
        offset += line.len();
        offset += match body.get(offset..offset + 2) {
            Some("\r\n") => 2,
            _ => 1,
        };
    }
    Some(body)
}

/// The standard ASS/SSA column order, used when there is no `[Events]`
/// section or its `Format:` line names none of Start, End or Text.
fn standard_ass_columns() -> Vec<&'static str> {
    vec![
        "layer", "start", "end", "style", "name", "marginl", "marginr", "marginv", "effect", "text",
    ]
}

/// The `Format:` column names from inside the `[Events]` section only: a
/// `[V4+ Styles]` `Format:` line never applies to `Dialogue:` lines. `None`
/// when the section has no `Format:` line.
fn events_format_fields(section: &str) -> Option<Vec<String>> {
    lines(section)
        .find_map(|line| {
            let fields = line.trim().strip_prefix("Format:")?;
            Some(
                fields
                    .split(',')
                    .map(|f| f.trim().to_ascii_lowercase())
                    .filter(|f| !f.is_empty())
                    .collect::<Vec<_>>(),
            )
        })
        .filter(|fields| !fields.is_empty())
}

/// ASS: the `[Events]` section, a `Format:` line that names its columns, and
/// one cue per `Dialogue:` line.
///
/// The `Format:` line is read only from inside a found `[Events]` section: a
/// `[V4+ Styles]` `Format:` line never applies to `Dialogue:` lines. With no
/// `[Events]` section, `Dialogue:` lines anywhere use the standard column
/// order. When the section has no `Format:` line, the standard order is used
/// too. The standard order is the one the prompt spells out: Layer, Start,
/// End, Style, Name, MarginL, MarginR, MarginV, Effect, Text.
fn ass_cues(text: &str) -> Result<Vec<Cue>, MediaError> {
    // With a [Events] section, the cues are its Dialogue lines and the
    // Format: line its own. Without one, the cues are the Dialogue lines
    // anywhere in the file, and the column order is the standard one.
    let (scope, format_fields) = match events_section(text) {
        Some(section) => {
            // Fall back to the standard order when the section's Format: line
            // is missing or names none of Start, End or Text.
            let fields = match events_format_fields(section) {
                Some(f)
                    if f.iter().any(|c| c == "start")
                        && f.iter().any(|c| c == "end")
                        && f.iter().any(|c| c == "text") =>
                {
                    f
                }
                _ => standard_ass_columns()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            };
            (section, fields)
        }
        None => (
            text,
            standard_ass_columns()
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
    };
    let (start_col, end_col, text_col) = match (
        format_fields.iter().position(|f| f == "start"),
        format_fields.iter().position(|f| f == "end"),
        format_fields.iter().position(|f| f == "text"),
    ) {
        (Some(s), Some(e), Some(t)) => (s, e, t),
        _ => return Ok(Vec::new()),
    };
    let mut cues = Vec::new();
    for line in lines(scope) {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("Dialogue:") else {
            continue;
        };
        // Split at most text_col + 1 fields: the text is the last one, whole,
        // so a comma inside the text stays whole.
        let fields: Vec<&str> = rest.splitn(text_col + 1, ',').collect();
        if fields.len() <= text_col {
            continue;
        }
        let start = ass_time(fields.get(start_col).copied().unwrap_or_default());
        let end = ass_time(fields.get(end_col).copied().unwrap_or_default());
        let (start, end) = match (start, end) {
            (Some(s), Some(e)) => (s, e),
            _ => continue,
        };
        let text = clean_ass(fields[text_col]);
        push_cue(&mut cues, start, end, text)?;
    }
    Ok(cues)
}

/// WebVTT: the `WEBVTT` header, `NOTE`/`STYLE`/`REGION` blocks and cue timing
/// lines. A cue may open with an identifier line before its timing line, and
/// the timing line may carry cue settings after the end time, which are
/// ignored.
fn vtt_cues(text: &str) -> Result<Vec<Cue>, MediaError> {
    let mut cues = Vec::new();
    let lines: Vec<&str> = lines(text).collect();
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
        if is_vtt_block(lines[i]) {
            i += 1;
            while i < lines.len() && !lines[i].is_empty() {
                i += 1;
            }
            continue;
        }
        let timing_line = if vtt_timing(lines[i]).is_some() {
            i
        } else if i + 1 < lines.len() && vtt_timing(lines[i + 1]).is_some() {
            // An identifier line, when there is one.
            i + 1
        } else {
            // Neither this line nor the next is a timing line: skip to the
            // blank.
            skip_block_vtt(&lines, &mut i);
            continue;
        };
        let (start_ms, end_ms) = match vtt_timing(lines[timing_line]) {
            Some(timing) => timing,
            None => continue,
        };
        i = timing_line + 1;
        let text_start = i;
        while i < lines.len() && !lines[i].is_empty() {
            i += 1;
        }
        let mut text = String::new();
        for (idx, line) in lines[text_start..i].iter().enumerate() {
            if idx > 0 {
                text.push('\n');
            }
            text.push_str(line);
        }
        push_cue(&mut cues, start_ms, end_ms, clean_html_tags(&text))?;
    }
    Ok(cues)
}

/// A `NOTE`, `STYLE` or `REGION` block's first line.
fn is_vtt_block(line: &str) -> bool {
    let trimmed = line.trim_start();
    ["NOTE", "STYLE", "REGION"].iter().any(|tag| {
        trimmed == *tag
            || trimmed.starts_with(tag)
                && trimmed.len() > tag.len()
                && trimmed.as_bytes()[tag.len()] == b' '
    })
}

/// `00:00:00.500 --> 00:00:02.000`, the hours optional, only the first
/// whitespace-separated token after `-->` is the end time: cue settings are
/// ignored.
fn vtt_timing(line: &str) -> Option<(u64, u64)> {
    let line = line.trim();
    let (start, rest) = line.split_once("-->")?;
    let start = vtt_time(start.trim())?;
    let end = vtt_time(rest.split_whitespace().next()?)?;
    Some((start, end))
}

/// Skip a VTT block that is not a cue: a line that is neither a block header
/// nor a timing line, running to the next blank.
fn skip_block_vtt(lines: &[&str], i: &mut usize) {
    while *i < lines.len() && !lines[*i].is_empty() {
        *i += 1;
    }
}

/// ASS text: `{...}` override blocks gone, `\N` and `\n` newlines, `\h` a
/// space. The rest of the text is kept, so a cue that does not use overrides
/// is unchanged. An unclosed `{` keeps the rest of the text as literal.
///
/// The delimiters `{`, `}` and `\` are ASCII, so every offset `find` returns
/// is a char boundary, and the runs between them are copied as slices. That
/// is what keeps a 3- or 4-byte character whole: the offset arithmetic never
/// looks at the bytes of a character, only at the ASCII delimiters.
///
/// One `find` per step, for both delimiters at once, so the text is scanned
/// in a single forward pass: a byte is looked at a bounded number of times,
/// and a 4 MiB line of `{}` pairs or lone `\` is cleaned in one go.
fn clean_ass(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(hit) = rest.find(['{', '\\']) {
        // Copy the run before the hit, whole.
        out.push_str(&rest[..hit]);
        if rest.as_bytes()[hit] == b'{' {
            match rest[hit + 1..].find('}') {
                Some(close) => {
                    // Drop the override block.
                    rest = &rest[hit + 2 + close..];
                }
                None => {
                    // An unclosed brace: keep the rest as literal.
                    out.push_str(&rest[hit..]);
                    return out;
                }
            }
        } else {
            match rest.as_bytes().get(hit + 1) {
                Some(b'N') | Some(b'n') => {
                    out.push('\n');
                    rest = &rest[hit + 2..];
                }
                Some(b'h') => {
                    out.push(' ');
                    rest = &rest[hit + 2..];
                }
                _ => {
                    // A lone backslash: keep it, and advance one char.
                    let char_len = rest[hit..].chars().next().map_or(1, char::len_utf8);
                    out.push_str(&rest[hit..hit + char_len]);
                    rest = &rest[hit + char_len..];
                }
            }
        }
    }
    // No more delimiters: the run to the end of the text stays whole.
    out.push_str(rest);
    out
}

/// SRT and VTT text: `<...>` tags gone, except `<i>`, `<b>` and `<u>` and
/// their closing forms, which the WebVTT engine renders. The kept tags match
/// case-insensitively and are kept as written. An unclosed `<` keeps the rest
/// of the text as literal.
///
/// `<` and `>` are ASCII, so every offset `find` returns is a char boundary,
/// and the runs between them are copied as slices. That is what keeps a
/// 3- or 4-byte character whole.
fn clean_html_tags(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        // Copy the run before the tag, whole.
        out.push_str(&rest[..open]);
        let tag_body = &rest[open + 1..];
        match tag_body.find('>') {
            Some(close) => {
                let tag = tag_body[..close].trim();
                let kept = matches!(
                    tag.to_ascii_lowercase().as_str(),
                    "i" | "b" | "u" | "/i" | "/b" | "/u"
                );
                if kept {
                    out.push('<');
                    out.push_str(tag);
                    out.push('>');
                }
                rest = &tag_body[close + 1..];
            }
            None => {
                // An unclosed tag: keep the rest as literal text, tag and all.
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Subtitles embedded in a Matroska or WebM file: the cues of one track that
/// overlap the window `[from_ms, to_ms)`, sorted by `start_ms`.
///
/// `reader` is any seekable source, the same reader the probe and the
/// remuxer take. The window is checked before anything is read: `to_ms <=
/// from_ms`, or a window wider than [`CUE_WINDOW_MAX_MS`], is
/// [`MediaError::Unsupported`], because neither names a moment of the video.
/// A file that is not Matroska or WebM is [`MediaError::Unsupported`] too; an
/// MP4 (`ftyp`/`moov` at offset 4) is `Unsupported("embedded MP4 subtitles")`
/// until the MP4 path lands.
///
/// The file is opened exactly the way
/// [`open_track`](crate::open_track) opens it: through the crate's
/// budgeted, capped Matroska open, and every frame read and seek wrapped in
/// the same `catch_unwind` guard, so a hostile file answers with a
/// [`MediaError`], never a panic.
///
/// `track` is the container's own `TrackNumber`. It must be a subtitle track
/// with a text codec fbtt reads (`S_TEXT/UTF8`, `S_TEXT/ASS`, `S_TEXT/SSA`);
/// any other track or codec - video, audio, a track the file does not carry,
/// and bitmap subtitles like PGS or VobSub - is [`MediaError::Unsupported`].
///
/// When `from_ms > 10 000` the demuxer seeks to `from_ms - 10 000`, because a
/// subtitle can start before the seek point; below that it reads from the
/// start with no seek. In a file with cues the seek lands correctly, but in
/// a file without them a seek into the last cluster ends the stream: if the
/// seek gives no frame at all, the file is re-opened from the start and read
/// forward instead.
pub fn embedded_cues<R: Read + Seek>(
    mut reader: R,
    track: u32,
    from_ms: u64,
    to_ms: u64,
) -> Result<Vec<Cue>, MediaError> {
    // The window, checked before anything is read.
    if to_ms <= from_ms || to_ms - from_ms > CUE_WINDOW_MAX_MS {
        return Err(MediaError::Unsupported(format!(
            "embedded subtitle window {from_ms}..{to_ms} is empty or wider than {CUE_WINDOW_MAX_MS} ms"
        )));
    }
    // The container. Eight bytes decide: the EBML magic is Matroska or WebM,
    // an `ftyp` or `moov` box is the MP4 path (Unsupported for now), anything
    // else is not a container this function reads.
    let mut magic = [0u8; 8];
    match reader.read_exact(&mut magic) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(MediaError::Unsupported(
                "too short to hold a container header".to_string(),
            ));
        }
        Err(err) => return Err(MediaError::Io(err)),
    }
    reader.rewind()?;
    if magic[4..] == *b"ftyp" || magic[4..] == *b"moov" {
        return Err(MediaError::Unsupported(
            "embedded MP4 subtitles".to_string(),
        ));
    }
    if magic[..4] != [0x1a, 0x45, 0xdf, 0xa3] {
        return Err(MediaError::Unsupported(format!(
            "not a Matroska or WebM file: starts with {magic:02X?}"
        )));
    }
    let cues = open_and_read(reader, u64::from(track), from_ms, to_ms)?;
    Ok(cues)
}

/// The `embedded_cues` walk, over a reader already past its container check.
///
/// Opens the file the way the remuxer does, verifies the track, then reads
/// frames from the seek point to `to_ms` and keeps the track's cues that
/// overlap the window. The re-open for a seek that gave no frame at all
/// happens here, on the same reader.
fn open_and_read<R: Read + Seek>(
    mut reader: R,
    track: u64,
    from_ms: u64,
    to_ms: u64,
) -> Result<Vec<Cue>, MediaError> {
    // The seek target, when there is one: 10 000 ms before the window, so a
    // subtitle that starts before the seek point is not cut. The guard means
    // the subtraction cannot underflow.
    #[allow(clippy::implicit_saturating_sub)]
    let seek_ms = if from_ms > 10_000 {
        from_ms - 10_000
    } else {
        0
    };
    let (mut cues, saw_frame) = read_track(&mut reader, track, from_ms, to_ms, seek_ms)?;
    // A seek that gave no frame at all: a file without cues, whose last
    // cluster the seek ran into and ended the stream on. Re-open from the
    // start and read forward - the reader sits after the failed open, so
    // rewind it first. An empty window is a normal answer, not a signal to
    // re-read the file: only a seek that read nothing at all re-opens.
    if seek_ms > 0 && !saw_frame {
        reader.rewind()?;
        let (rewound, _) = read_track(&mut reader, track, from_ms, to_ms, 0)?;
        cues = rewound;
    }
    cues.sort_by_key(|cue| cue.start_ms);
    Ok(cues)
}

/// Open one Matroska file, confirm `track` is a readable text subtitle track,
/// and read frames from `seek_ms` to the first frame at or past `to_ms` into
/// cues of that track overlapping `[from_ms, to_ms)`.
///
/// The second return value says whether the read after the seek returned at
/// least one frame, of any track: the caller re-opens from the start only
/// when a seek was made and no frame at all came back, and an empty window
/// with frames around it is a normal answer.
fn read_track<R: Read + Seek>(
    reader: R,
    track: u64,
    from_ms: u64,
    to_ms: u64,
    seek_ms: u64,
) -> Result<(Vec<Cue>, bool), MediaError> {
    // The same budgeted, capped open the remuxer and the probe use. The budget
    // is cloned before `open_matroska` moves it in, and lifted once the header
    // parse has succeeded: the frame reads are bounded by
    // MAX_CUE_SCAN_FRAMES, MAX_SKIPPED_FRAMES and the caps, not by the header
    // budget.
    let budget = ReadBudget::new(ELEMENT_CAP);
    let mut file = open_matroska(reader, budget.clone())?;
    let scale = file.info().timestamp_scale().get();
    // The track, and what kind of text it holds.
    let codec = file
        .tracks()
        .iter()
        .find(|entry| entry.track_number().get() == track)
        .ok_or_else(|| {
            MediaError::Unsupported(format!("track {track} is not one of this file's tracks"))
        })
        .and_then(|entry| {
            if matroska_kind(entry) != Some(crate::codec::TrackKind::Subtitle) {
                return Err(MediaError::Unsupported(format!(
                    "track {track} is not a subtitle track"
                )));
            }
            let id = entry.codec_id().to_string();
            let codec = match id.as_str() {
                "S_TEXT/UTF8" => Codec::SubRip,
                "S_TEXT/ASS" | "S_TEXT/SSA" => Codec::Ass,
                other => {
                    return Err(MediaError::Unsupported(format!(
                        "track {track} has subtitle codec {other}, which fbtt does not read"
                    )));
                }
            };
            Ok(codec)
        })?;
    budget.lift();
    let mut slot = Frame::default();
    if seek_ms > 0 {
        let units = scale_units(seek_ms, scale)?;
        guarded_seek(&mut file, units)?;
    }
    // The kept cues, with the start the next cue of the track takes when the
    // block states no duration: the last kept cue's end is the start of the
    // first cue of the track at or past the window, recorded when the walk
    // stops on it.
    let mut cues: Vec<(u64, Option<u64>, String)> = Vec::new();
    let mut next_start_after_window: Option<u64> = None;
    // A frame of the requested track that gave no cue, counted against
    // MAX_SKIPPED_FRAMES: the bound on a track whose frames never make a cue.
    // Frames of other tracks are bounded by the total, `scanned`.
    let mut skipped = 0usize;
    let mut scanned = 0u64;
    let mut saw_frame = false;
    loop {
        if !guarded_next(&mut file, &mut slot, "subtitle frames")? {
            break;
        }
        saw_frame = true;
        scanned += 1;
        if scanned > MAX_CUE_SCAN_FRAMES {
            return Err(MediaError::Malformed(format!(
                "no subtitle within {MAX_CUE_SCAN_FRAMES} frames"
            )));
        }
        // A slot still claiming track 0 was filled by nothing (an empty
        // BlockGroup): no frame, so it counts against the total.
        //
        // The start time of every frame is computed, any track, with the same
        // checked maths: the walk stops at the first frame, of any track,
        // whose start is at or past the window, so a window after the last
        // subtitle does not read to the end of the file.
        let start_ms = frame_start_ms(slot.timestamp, scale)?;
        if start_ms >= to_ms {
            // The stopping frame's start is the next cue's start when it
            // belongs to the requested track and gives text; a frame of
            // another track stops the read first, and then the next cue is
            // unknown, so the 5000 ms rule applies to the last kept cue.
            if slot.track == track
                && next_start_after_window.is_none()
                && frame_text(&slot.data, &codec).is_some_and(|text| !text.is_empty())
            {
                next_start_after_window = Some(start_ms);
            }
            break;
        }
        // A frame of another track: it counts against the total only.
        if slot.track != track {
            continue;
        }
        // The block's duration, in milliseconds of source time, when the
        // frame states one.
        let duration_ms = slot
            .duration
            .and_then(|duration| frame_duration_ms(duration, scale));
        // The text, cleaned by the format's rules, or `None` for a frame with
        // no cue (an ASS block with fewer than nine fields, or a frame over
        // the per-frame cap).
        let text = frame_text(&slot.data, &codec);
        match text {
            None => {
                skipped += 1;
                if skipped > MAX_SKIPPED_FRAMES {
                    return Err(MediaError::Malformed(format!(
                        "no cue within {MAX_SKIPPED_FRAMES} frames"
                    )));
                }
                continue;
            }
            Some(text) => {
                skipped = 0;
                if cues.len() >= MAX_CUES {
                    return Err(MediaError::Malformed(format!(
                        "track {track} holds more than {MAX_CUES} cues"
                    )));
                }
                cues.push((start_ms, duration_ms, text));
            }
        }
    }
    // The cues, with ends worked out: the block's duration when it states one,
    // the next cue's start otherwise - the start of the first cue of the track
    // at or past the window when the walk recorded it, or start + 5000 when
    // the read ended or another track's frame stopped it first.
    let mut out = Vec::with_capacity(cues.len());
    let mut text_bytes = 0u64;
    for (index, (start_ms, duration_ms, text)) in cues.iter().enumerate() {
        let end_ms = duration_ms
            .and_then(|duration| (*start_ms).checked_add(duration))
            .or_else(|| cues.get(index + 1).map(|(next, _, _)| *next))
            .or_else(|| match (index, next_start_after_window) {
                (last, Some(next)) if last == cues.len() - 1 => Some(next),
                _ => None,
            })
            .unwrap_or_else(|| start_ms.saturating_add(5000));
        // Keep the cues that overlap the window; a cue with no room on screen
        // (end_ms <= start_ms) is dropped.
        if end_ms > from_ms && *start_ms < to_ms && end_ms > *start_ms {
            if text.is_empty() {
                continue;
            }
            // The total kept text is bounded: a hostile file with a million
            // small cues would otherwise grow a list of megabytes.
            text_bytes = text_bytes.saturating_add(text.len() as u64);
            if text_bytes > SUBTITLE_FILE_CAP {
                return Err(MediaError::Malformed(format!(
                    "track {track} holds more than {SUBTITLE_FILE_CAP} bytes of subtitle text"
                )));
            }
            out.push(Cue {
                start_ms: *start_ms,
                end_ms,
                text: text.clone(),
            });
        }
    }
    Ok((out, saw_frame))
}

/// Container timestamp units for a millisecond request, or `Malformed` when
/// the file's `TimestampScale` cannot count it.
fn scale_units(ms: u64, scale: u64) -> Result<u64, MediaError> {
    let ns = u128::from(ms) * 1_000_000;
    let units = ns / u128::from(scale);
    u64::try_from(units).map_err(|_| {
        MediaError::Malformed(format!(
            "time {ms} ms is past what TimestampScale {scale} can count"
        ))
    })
}

/// A frame's start, in milliseconds: nanoseconds over a million, all checked
/// in `u128`, `Malformed` on overflow.
fn frame_start_ms(timestamp: u64, scale: u64) -> Result<u64, MediaError> {
    let overflow =
        || MediaError::Malformed("a subtitle timestamp overflows the timeline".to_string());
    let ns = u128::from(timestamp)
        .checked_mul(u128::from(scale))
        .ok_or_else(overflow)?;
    let ms = ns / 1_000_000;
    u64::try_from(ms).map_err(|_| overflow())
}

/// A frame's stated duration, in milliseconds, when it fits the timeline.
fn frame_duration_ms(duration: u64, scale: u64) -> Option<u64> {
    let ns = u128::from(duration).checked_mul(u128::from(scale))?;
    let ms = ns / 1_000_000;
    u64::try_from(ms).ok()
}

/// A subtitle frame's text, cleaned by the format's rules.
///
/// `S_TEXT/UTF8` is plain text: the same tag rule as SRT. `S_TEXT/ASS` and
/// `S_TEXT/SSA` are `Dialogue:` fields: the text is the ninth comma field
/// onward (ReadOrder, Layer, Style, Name, MarginL, MarginR, MarginV, Effect,
/// Text), and the ASS cleaner runs over it. A frame with fewer than nine
/// fields is no cue, and a frame whose text is over the file cap is skipped.
fn frame_text(data: &[u8], codec: &Codec) -> Option<String> {
    if data.len() > SUBTITLE_FILE_CAP as usize {
        return None;
    }
    let text = decode(data);
    let cleaned = match codec {
        Codec::Ass => {
            // The ninth field (index 8) is the text; a block with fewer than
            // nine fields is no cue. `.last()` would never be `None`, so a
            // short block's final field would come back as its text.
            let rest = text.splitn(9, ',').nth(8)?.trim_start();
            clean_ass(rest)
        }
        _ => clean_html_tags(&text),
    };
    Some(cleaned)
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
        // 20 digits of hours: overflows u64, no cue. 13 digits: fits u64 but
        // overflows u64 milliseconds, also no cue.
        let bytes = b"1\r\n99999999999999999999:00:00,000 --> 99999999999999999999:00:01,000\r\nHuge\r\n\r\n2\r\n6000000000000:00:00,000 --> 6000000000000:00:01,000\r\nBig\r\n\r\n3\r\n00:00:01,000 --> 00:00:02,000\r\nSmall\r\n";
        let cues = file_cues(bytes, SubtitleFormat::Srt).expect("no panic");
        assert_eq!(cues.len(), 1, "the huge cues are skipped");
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

    #[test]
    fn unclosed_tags_stay_text() {
        // SRT: an unclosed < keeps the rest as literal.
        let bytes = b"1\r\n00:00:00,000 --> 00:00:01,000\r\n1 < 2 and more\r\n";
        let cues = file_cues(bytes, SubtitleFormat::Srt).expect("no panic");
        assert_eq!(cues[0].text, "1 < 2 and more");
        // ASS: an unclosed { keeps the rest as literal.
        let bytes = b"Dialogue: 0,0:00:00.50,0:00:02.00,Default,,0,0,0,,a { b\r\n";
        let cues = file_cues(bytes, SubtitleFormat::Ass).expect("no panic");
        assert_eq!(cues[0].text, "a { b");
    }

    #[test]
    fn srt_dot_milliseconds_and_coordinates() {
        // SRT accepts . before the milliseconds, and X1: coordinates after
        // the end time are ignored.
        let bytes = b"1\r\n00:00:00.500 --> 00:00:02.000 X1:123\r\nDot\r\n";
        let cues = file_cues(bytes, SubtitleFormat::Srt).expect("no panic");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_ms, 500);
        assert_eq!(cues[0].end_ms, 2000);
        assert_eq!(cues[0].text, "Dot");
    }

    #[test]
    fn minutes_over_59_are_skipped() {
        // A minutes field over 59 makes the cue unparsable, not a panic.
        let bytes = b"1\r\n00:75:00,000 --> 00:75:01,000\r\nBad\r\n\r\n2\r\n00:00:01,000 --> 00:00:02,000\r\nGood\r\n";
        let cues = file_cues(bytes, SubtitleFormat::Srt).expect("no panic");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_ms, 1000);
        assert_eq!(cues[0].text, "Good");
    }

    #[test]
    fn ass_lone_cr_line_endings() {
        // The fixture sub.ass with every \n replaced by \r gives the same
        // cues as ass_file.
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sub.ass"
        ))
        .expect("sub.ass");
        let original: Vec<u8> = bytes
            .iter()
            .map(|b| if *b == b'\n' { b'\r' } else { *b })
            .collect();
        let cues = file_cues(&original, SubtitleFormat::Ass).expect("no panic");
        assert_eq!(cues.len(), 3, "the ass fixture with lone CR has three cues");
        assert_eq!(cues[0].start_ms, 500);
        assert_eq!(cues[0].end_ms, 2000);
        assert_eq!(cues[0].text, "First ASS line\nspanning two lines");
    }
}
