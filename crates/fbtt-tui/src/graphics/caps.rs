//! What the terminal proved it can draw, worked out from the bytes it answered the probe with.
//!
//! The probe (Task 8) writes [`PROBE_QUERY`] once, before the event loop exists, and reads what
//! comes back. This file turns those bytes into [`Replies`] and then into one [`Graphics`]
//! verdict: which image protocol to draw the logo with, how big this terminal's cells are, and
//! whether iTerm2's pixels-per-point scale has to be multiplied in. Nothing here touches a
//! terminal or a clock, so the whole decision table is testable from a byte string.
//!
//! Parsing is defensive in one direction: an answer counts only when it is complete and its
//! numbers are usable, and anything else is skipped. A truncated answer is no answer (the probe
//! may have stopped reading mid-sequence), a number past what a `u16` holds is not a value anyone
//! could act on, and no input panics or allocates without bound, because a terminal's answer is
//! remote input like any other.

/// The image protocol to draw the logo with, chosen by [`choose`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Nothing this terminal proved, so the text title stays where it is.
    None,
    /// The kitty graphics protocol, drawn through Unicode placeholder cells so that ratatui's
    /// diff treats the image as ordinary text. Only chosen for a terminal whose kitty support has
    /// placeholders: kitty 0.28 or later, or Ghostty.
    Kitty,
    /// OSC 1337 inline images: iTerm2's own protocol, which WezTerm also implements in full even
    /// though its kitty support has no placeholders.
    Iterm2,
    /// Sixel through a DCS string: what foot, fbterm and a sixel-enabled xterm answer with.
    Sixel,
}

/// The size of one character cell, in the device pixels the terminal draws in.
///
/// Every protocol reports something different and [`choose`] turns it into this: kitty, Ghostty and the
/// sixel terminals answer `CSI 16 t` or a window size divided by a grid, which are already device
/// pixels; iTerm2 answers `ReportCellSize` in *points* with its own pixels-per-point factor, and that
/// factor is multiplied in before anything is rounded, so what lands here is the pixel size of the cell
/// and not a point size waiting to be scaled. That is why [`Graphics::scale`] is not something to
/// multiply `cell` by again: the prepared image has to be exactly `cols * cell.w` by `rows * cell.h`
/// pixels, or the terminal rescales it and the mark goes soft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellPx {
    /// The width of one cell in device pixels.
    pub w: u16,
    /// The height of one cell in device pixels.
    pub h: u16,
}

/// The graphics verdict for one terminal: what to draw with, how big its cells are, and the factor
/// that turns those cells into device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Graphics {
    /// The protocol to send image bytes with.
    pub protocol: Protocol,
    /// The cell size, or `None` when the terminal never answered any of the cell size queries.
    /// That is also when `protocol` is [`Protocol::None`]: no cell pixel size means no logo.
    pub cell: Option<CellPx>,
    /// What this terminal multiplies a point by to get a device pixel, rounded and clamped to 1..=4: 1
    /// for every protocol except iTerm2, which reports the factor in its `ReportCellSize` answer.
    ///
    /// `choose` has already multiplied that factor into [`Graphics::cell`] when it needed it, so
    /// this is a fact about the terminal rather than something to apply to `cell`: sizing a logo
    /// from `cell` alone is right for every protocol, and multiplying by this as well would build a
    /// bitmap twice the size of the cells it is placed over.
    pub scale: u16,
}

impl Graphics {
    /// The verdict that draws no image: keep the text title.
    pub fn none() -> Self {
        Graphics {
            protocol: Protocol::None,
            cell: None,
            scale: 1,
        }
    }
}

/// The six queries the probe writes in one go: the kitty graphics query (with id 4242, so its
/// answer can be told from anything else on the line), XTVERSION, iTerm2's `ReportCellSize`,
/// `CSI 16 t` for the cell size, `CSI 14 t` for the text area and DA1.
///
/// DA1 goes last because every terminal answers it, so reading stops as soon as its final `c`
/// arrives and a keystroke typed after the answers is never eaten (decision 9).
pub const PROBE_QUERY: &[u8] = b"\x1b_Gi=4242,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[>0q\x1b]1337;ReportCellSize\x07\x1b[16t\x1b[14t\x1b[c";

/// What the terminal answered, one field per query. A field is `None` when that query went
/// unanswered, which is the common case: no terminal answers all six.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Replies {
    /// Whether the kitty graphics query was answered `OK`.
    pub kitty_ok: bool,
    /// XTVERSION's text, cut to 64 printable ASCII characters: the terminal's own name and version.
    pub xtversion: Option<String>,
    /// iTerm2's cell size answer as `(height, width, scale)`: cell points and pixels per point. The
    /// scale is 1.0 when the answer leaves it out.
    pub iterm_cell: Option<(f32, f32, f32)>,
    /// `CSI 16 t`'s answer: `(height, width)` in pixels.
    pub cell_16t: Option<(u16, u16)>,
    /// `CSI 14 t`'s answer: the text area as `(height, width)` in pixels.
    pub area_14t: Option<(u16, u16)>,
    /// DA1's parameters, in order. Sixel support is the `4` in this list.
    pub da1: Option<Vec<u16>>,
}

/// `ESC`, the first byte of every answer this file looks for.
const ESC: u8 = 0x1b;
/// `BEL`, which ends an OSC as `ST` does.
const BEL: u8 = 0x07;
/// `ST`: the two bytes that end a DCS, an APC string and an OSC that does not use `BEL`.
const ST: &[u8] = b"\x1b\\";
/// XTVERSION's text is cut to this many printable ASCII characters.
const XTVERSION_MAX: usize = 64;

/// The prefix of the answer to the probe's graphics query: our id, then the message.
const KITTY_ANSWER: &[u8] = b"\x1b_Gi=4242;";
/// The prefix of XTVERSION's answer: a DCS whose text is the terminal's own name and version.
const XTVERSION_ANSWER: &[u8] = b"\x1bP>|";
/// The prefix of iTerm2's cell size answer.
const CELL_SIZE_ANSWER: &[u8] = b"\x1b]1337;ReportCellSize=";
/// The prefix of DA1's answer.
const DA1_ANSWER: &[u8] = b"\x1b[?";

/// The lowest kitty version whose graphics support has placeholder cells (decision 10). A kitty
/// older than this answers the query but cannot draw an image that way, and nothing else about it
/// promises a logo.
const KITTY_PLACEHOLDERS: (u32, u32) = (0, 28);

/// A cell this far outside these device pixel sizes says nothing useful about the terminal, so
/// decision 11 ignores that answer and the logo with it: no font produces a 1-pixel or a 200-pixel
/// cell, and an image sized from one is either unreadable or larger than the screen.
const CELL_MIN_W: u32 = 2;
const CELL_MAX_W: u32 = 64;
const CELL_MIN_H: u32 = 4;
const CELL_MAX_H: u32 = 128;

/// The largest scale worth multiplying in. iTerm2 reports 1 or 2 today; clamping means a terminal
/// that answered something wilder gets a logo sized for the biggest cell this can place rather than
/// one that overflows the header.
const SCALE_MAX: u16 = 4;

/// Read every answer in a buffer of terminal output.
///
/// Anything that is not one of these answers is skipped a byte at a time, so garbage between
/// answers costs nothing and an answer that has not finished yet matches nothing. The first
/// complete answer of each kind wins: a terminal that answers twice is answering the same query.
#[must_use]
pub fn parse_replies(bytes: &[u8]) -> Replies {
    let mut replies = Replies::default();
    let mut at = 0usize;
    while at < bytes.len() {
        // Every answer starts with `ESC`, so the scan only tries to match on those bytes.
        if bytes[at] != ESC {
            at += 1;
            continue;
        }
        // A match consumes the whole answer and the scan resumes after it; anything that is not an
        // answer, or is one that has not finished yet, gives up one byte and tries again from the
        // next. The first complete answer of each kind is the one kept.
        if !replies.kitty_ok
            && let Some(end) = match_kitty_ok(bytes, at)
        {
            replies.kitty_ok = true;
            at = end;
            continue;
        }
        if replies.xtversion.is_none()
            && let Some((text, end)) = match_xtversion(bytes, at)
        {
            replies.xtversion = Some(text);
            at = end;
            continue;
        }
        if replies.iterm_cell.is_none()
            && let Some((cell, end)) = match_iterm_cell(bytes, at)
        {
            replies.iterm_cell = Some(cell);
            at = end;
            continue;
        }
        if replies.cell_16t.is_none()
            && let Some((cell, end)) = match_csi_pair(bytes, at, b'6')
        {
            replies.cell_16t = Some(cell);
            at = end;
            continue;
        }
        if replies.area_14t.is_none()
            && let Some((area, end)) = match_csi_pair(bytes, at, b'4')
        {
            replies.area_14t = Some(area);
            at = end;
            continue;
        }
        if replies.da1.is_none()
            && let Some((da1, end)) = match_da1(bytes, at)
        {
            replies.da1 = Some(da1);
            at = end;
            continue;
        }
        at += 1;
    }
    replies
}

/// The length past a complete `OK` answer to the graphics query, or `None`.
///
/// The message after the id has to be exactly `OK`: kitty answers `ERROR`, `ENOLICENSE` and others
/// through the same shape, and each of them is this terminal saying no.
fn match_kitty_ok(bytes: &[u8], at: usize) -> Option<usize> {
    let message = bytes.get(at..)?.strip_prefix(KITTY_ANSWER)?;
    if !message.starts_with(b"OK") || !message[2..].starts_with(ST) {
        return None;
    }
    Some(at + KITTY_ANSWER.len() + 2 + ST.len())
}

/// The XTVERSION text and the length past its answer, or `None`.
fn match_xtversion(bytes: &[u8], at: usize) -> Option<(String, usize)> {
    let body = bytes.get(at..)?.strip_prefix(XTVERSION_ANSWER)?;
    let end = find(ST, body)?;
    Some((
        xtversion_text(&body[..end]),
        at + XTVERSION_ANSWER.len() + end + ST.len(),
    ))
}

/// The answer's text: printable ASCII only, cut to [`XTVERSION_MAX`] characters.
///
/// A terminal's name and version are plain text, so anything else in the answer is either noise or
/// something hostile, and neither is worth holding on to. The length cap is what keeps a terminal
/// that answers with a paragraph from making the probe carry an unbounded string.
fn xtversion_text(body: &[u8]) -> String {
    body.iter()
        .filter(|b| b.is_ascii_graphic() || **b == b' ')
        .take(XTVERSION_MAX)
        .map(|b| char::from(*b))
        .collect()
}

/// iTerm2's cell size `(height, width, scale)` and the length past its answer, or `None`.
///
/// The scale is 1.0 when the answer leaves it out, which is what the older versions of iTerm2 do.
fn match_iterm_cell(bytes: &[u8], at: usize) -> Option<((f32, f32, f32), usize)> {
    let body = bytes.get(at..)?.strip_prefix(CELL_SIZE_ANSWER)?;
    let (end, term_len) = find_osc_end(body)?;
    let mut fields = body[..end].split(|b| *b == b';');
    let height = parse_point(fields.next()?)?;
    let width = parse_point(fields.next()?)?;
    let scale = match fields.next() {
        Some(field) => parse_point(field)?,
        None => 1.0,
    };
    // A fourth field is not part of this answer, so the whole thing is something else.
    if fields.next().is_some() {
        return None;
    }
    Some((
        (height, width, scale),
        at + CELL_SIZE_ANSWER.len() + end + term_len,
    ))
}

/// Where an OSC ends: whichever of `ST` and `BEL` comes first, and how long that terminator is.
/// Either one alone ends it, so a terminal that uses one is not treated as having answered nothing.
fn find_osc_end(body: &[u8]) -> Option<(usize, usize)> {
    // A tie is impossible: the byte at the start of `ST` is `ESC`, not `BEL`.
    let bel = body.iter().position(|b| *b == BEL).map(|i| (i, 1usize));
    let st = find(ST, body).map(|i| (i, ST.len()));
    [bel, st].into_iter().flatten().min()
}

/// One number from an OSC 1337 answer: a value in points or a scale factor. Anything that does not
/// parse, is negative or is not finite makes the answer unusable, because half a cell size is not
/// enough to place an image on.
fn parse_point(field: &[u8]) -> Option<f32> {
    let value = std::str::from_utf8(field)
        .ok()?
        .trim()
        .parse::<f32>()
        .ok()?;
    value.is_finite().then_some(value).filter(|v| *v >= 0.0)
}

/// The two parameters of `ESC [ <intro> ; <n> ; <n> t` and the length past it, or `None`.
///
/// This is the shape both `CSI 16 t` (a cell's pixel size) and `CSI 14 t` (the text area) answer
/// with, height first in both cases; which one was answered is decided by `intro`.
fn match_csi_pair(bytes: &[u8], at: usize, intro: u8) -> Option<((u16, u16), usize)> {
    let body = bytes.get(at..)?.strip_prefix(&[ESC, b'[', intro, b';'])?;
    let end = find(b"t", body)?;
    let mut fields = body[..end].split(|b| *b == b';');
    let first = parse_u16(fields.next()?)?;
    let second = parse_u16(fields.next()?)?;
    if fields.next().is_some() {
        return None;
    }
    Some(((first, second), at + 4 + end + 1))
}

/// DA1's parameters and the length past its answer, or `None`.
///
/// One parameter that does not fit a `u16` makes the whole answer mean nothing: it is either a
/// terminal that is not answering seriously or a buffer that has run past the end of the answer,
/// and in both cases acting on part of the list would be guessing.
fn match_da1(bytes: &[u8], at: usize) -> Option<(Vec<u16>, usize)> {
    let body = bytes.get(at..)?.strip_prefix(DA1_ANSWER)?;
    let end = find(b"c", body)?;
    let mut params = Vec::new();
    for field in body[..end].split(|b| *b == b';') {
        params.push(parse_u16(field)?);
    }
    Some((params, at + DA1_ANSWER.len() + end + 1))
}

/// The first position of `needle` in `hay`, or `None`.
fn find(needle: &[u8], hay: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A decimal parameter that fits a `u16`: digits only, no sign, and nothing past 65535. An empty
/// field is not a number either.
fn parse_u16(field: &[u8]) -> Option<u16> {
    if field.is_empty() || !field.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut value: u32 = 0;
    for byte in field {
        // `value` is at most 65535 before each step, so this cannot overflow a `u32`.
        value = value * 10 + u32::from(byte - b'0');
        if value > u32::from(u16::MAX) {
            return None;
        }
    }
    Some(value as u16)
}

/// Whether the buffer *ends* with a complete DA1 answer, which is the probe's stop condition: DA1 is
/// the last query in [`PROBE_QUERY`] and every terminal answers it.
///
/// The probe calls this after every byte it reads, so it looks at one candidate instead of at every
/// `ESC` in the buffer. An answer that ends here has its `c` as the buffer's last byte, and a DA1 answer
/// carries no `ESC` of its own, so the `ESC [ ?` that opened it is the last `ESC` there is to look at.
/// A stream that answers nothing - `\x1b[?` repeated to the probe's 4096-byte cap - is turned away by
/// the first check, which is what stops a hostile terminal costing one scan per escape byte. Bytes past
/// a complete answer are not this function's question: the probe reads a byte at a time, stops on the
/// answer's own last byte, and hands anything else to whoever reads next.
///
/// An answer whose numbers are unusable counts as no answer here too, which costs the probe at most
/// the deadline it was going to pay anyway.
#[must_use]
pub fn da1_complete(bytes: &[u8]) -> bool {
    if bytes.last() != Some(&b'c') {
        return false;
    }
    let Some(at) = bytes.iter().rposition(|b| *b == ESC) else {
        return false;
    };
    match_da1(bytes, at).is_some()
}

/// The protocol and cell size to draw with, from what the terminal answered (decisions 10 and 11).
///
/// `grid` is the terminal's `(columns, rows)` and `ioctl` its window pixel size `(width, height)`
/// from `crossterm::terminal::window_size()`; both are only consulted for a cell size when the
/// queries that give one directly went unanswered. [`Graphics::none()`] comes back whenever no
/// protocol was proved or no cell size was found, because a logo the terminal has to scale is
/// worse than the text title.
#[must_use]
pub fn choose(replies: &Replies, grid: (u16, u16), ioctl: Option<(u16, u16)>) -> Graphics {
    let protocol = pick_protocol(replies);
    if protocol == Protocol::None {
        return Graphics::none();
    }
    let Some((cell, scale)) = cell_size(replies, protocol, grid, ioctl) else {
        return Graphics::none();
    };
    Graphics {
        protocol,
        cell: Some(cell),
        scale,
    }
}

/// Which protocol to draw with, in decision 10's order.
///
/// XTVERSION's text is the terminal's own sentence and nothing standardises it beyond "some
/// identifying text", so this matches substrings against what each terminal is documented to report
/// (`kitty(0.35.2)`, `ghostty 1.1.3`, `WezTerm 20240203-...`, `iTerm2 3.6.9`), case-insensitively
/// because only the letters of a name are guaranteed. A terminal that never answered XTVERSION is
/// not taken for anything it did not prove some other way, which is why fbterm - it answers the
/// graphics query but not XTVERSION - gets sixel rather than kitty.
fn pick_protocol(replies: &Replies) -> Protocol {
    let name = replies.xtversion.as_deref().unwrap_or("");
    let lower = name.to_ascii_lowercase();

    // Placeholder cells need a terminal whose kitty support has them: the query answered `OK` and
    // either kitty at 0.28 or later, or Ghostty, which has had them since its first release.
    if replies.kitty_ok && (lower.contains("ghostty") || kitty_has_placeholders(&lower)) {
        return Protocol::Kitty;
    }
    // `ReportCellSize` is an iTerm2 extension and answers only there, and WezTerm implements OSC
    // 1337 in full while its kitty support has no placeholders: both draw with inline images. This
    // comes before sixel on purpose - a WezTerm DA1 also claims `4`.
    if replies.iterm_cell.is_some() || lower.starts_with("iterm2") || lower.starts_with("wezterm") {
        return Protocol::Iterm2;
    }
    // Sixel is what DA1's `4` claims, and DA1 is the one answer every terminal gives.
    if replies
        .da1
        .as_ref()
        .is_some_and(|params| params.contains(&4))
    {
        return Protocol::Sixel;
    }
    Protocol::None
}

/// Whether a kitty answer names a version with placeholder cells: `kitty(0.35.2)` is 0.35, which is
/// at least [`KITTY_PLACEHOLDERS`]. A name with no numbers in it says nothing about which version,
/// and versions are compared as pairs so 1.0 outranks 0.99.
fn kitty_has_placeholders(lower_name: &str) -> bool {
    let bytes = lower_name.as_bytes();
    let Some(named) = find(b"kitty", bytes) else {
        return false;
    };
    // The version follows the name, in parentheses or after a space; only the first `digits.digits`
    // past the name is taken, so a stray number elsewhere cannot make an old kitty look new.
    let mut at = named + b"kitty".len();
    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() {
            at += 1;
            continue;
        }
        let major_start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        let Some(major) = parse_number(&bytes[major_start..at]) else {
            return false;
        };
        if at >= bytes.len() || bytes[at] != b'.' {
            return false;
        }
        at += 1;
        let minor_start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        if minor_start == at {
            return false;
        }
        let Some(minor) = parse_number(&bytes[minor_start..at]) else {
            return false;
        };
        return (major, minor) >= KITTY_PLACEHOLDERS;
    }
    false
}

/// A version number that fits a `u32`; a longer one is not a version anyone can compare.
fn parse_number(digits: &[u8]) -> Option<u32> {
    let mut value: u32 = 0;
    for byte in digits {
        value = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
    }
    Some(value)
}

/// The cell size in device pixels and the terminal's own scale, in decision 11's order: `ReportCellSize`
/// (iTerm2 only: its points multiplied by its pixels-per-point factor), else `CSI 16 t`, else the window
/// size divided by the grid, else `CSI 14 t` divided by the grid. An answer whose device pixel size is
/// outside [`CELL_MIN_W`]..=[`CELL_MAX_W`] wide or [`CELL_MIN_H`]..=[`CELL_MAX_H`] tall is ignored and
/// the next source gets its turn.
///
/// The scale that comes back with it is iTerm2's factor rounded to a whole number, and 1 for every other
/// protocol: those answers are already device pixels, so there is nothing to multiply in. For iTerm2 the
/// multiplying happens here, once, before the rounding, so what comes back is the cell this terminal
/// really draws rather than a point size a caller still has to convert.
fn cell_size(
    replies: &Replies,
    protocol: Protocol,
    grid: (u16, u16),
    ioctl: Option<(u16, u16)>,
) -> Option<(CellPx, u16)> {
    if protocol == Protocol::Iterm2
        && let Some((height_pts, width_pts, raw_scale)) = replies.iterm_cell
    {
        // `ReportCellSize` answers in points and its third number is the device pixels per point, so the
        // multiplication comes before the rounding: an 8.25-point column at scale 2 is a 16.5-pixel column,
        // which is the 17 pixels this terminal draws, where rounding the points first would call it 8
        // points and then return 16 or 17 on nothing but which way the half fell. A bitmap sized against
        // the wrong one of those is rescaled by the terminal, which is what this module exists to avoid.
        if let Some(cell) = device_cell(width_pts, height_pts, raw_scale) {
            return Some((cell, iterm_scale(raw_scale)));
        }
    }
    if let Some((height, width)) = replies.cell_16t
        && let Some(cell) = sane_cell(u32::from(width), u32::from(height))
    {
        return Some((cell, 1));
    }
    if let Some(cell) = divided_cell(ioctl, grid) {
        return Some((cell, 1));
    }
    let area = replies.area_14t.map(|(height, width)| (width, height));
    if let Some(cell) = divided_cell(area, grid) {
        return Some((cell, 1));
    }
    None
}

/// iTerm2's cell in points as the device pixels it is worth: each axis multiplied by the terminal's own
/// pixels-per-point factor and rounded once, at the end, then checked against the sizes decision 11
/// accepts. `None` for an answer whose numbers are not finite or whose cells no font produces.
fn device_cell(width_pts: f32, height_pts: f32, raw_scale: f32) -> Option<CellPx> {
    let w = round_to_u16(width_pts * raw_scale)?;
    let h = round_to_u16(height_pts * raw_scale)?;
    sane_cell(u32::from(w), u32::from(h))
}

/// iTerm2's pixels-per-point factor as [`Graphics::scale`] reports it: rounded to a whole number because
/// a bitmap has whole pixels, and inside 1..=[`SCALE_MAX`] because nothing bigger is worth placing. The
/// cell size above uses the factor before it was rounded or clamped; this is the answer to "what does
/// this terminal multiply a point by", which is what a caller needs when it wants the terminal's own
/// arithmetic rather than this module's.
fn iterm_scale(scale: f32) -> u16 {
    match round_to_u16(scale) {
        Some(0) | None => 1,
        Some(value) => value.min(SCALE_MAX),
    }
}

/// A cell size from an answer that gives a whole area in pixels: the area divided by the grid.
fn divided_cell(
    area: Option<(u16, u16)>, /* (px width, px height) */
    grid: (u16, u16),
) -> Option<CellPx> {
    let (px_w, px_h) = area?;
    let (cols, rows) = grid;
    if cols == 0 || rows == 0 {
        return None;
    }
    sane_cell(
        div_rounded(u32::from(px_w), u32::from(cols)),
        div_rounded(u32::from(px_h), u32::from(rows)),
    )
}

/// `n / d` rounded to the nearest whole number, so a window that is 1024 pixels wide over 80 columns
/// reports the 13-pixel cell it really has rather than a 12-pixel one.
fn div_rounded(n: u32, d: u32) -> u32 {
    (n + d / 2) / d
}

/// The cell size decision 11 accepts, or `None` for a size no terminal font produces.
fn sane_cell(w_px: u32, h_px: u32) -> Option<CellPx> {
    if !(CELL_MIN_W..=CELL_MAX_W).contains(&w_px) || !(CELL_MIN_H..=CELL_MAX_H).contains(&h_px) {
        return None;
    }
    Some(CellPx {
        w: u16::try_from(w_px).ok()?,
        h: u16::try_from(h_px).ok()?,
    })
}

/// A number the terminal reported as a whole number of pixels or points, or `None` when it is not finite,
/// is negative or is past what a `u16` holds. Half rounds away from zero, which is what makes an exact
/// half pixel land on the larger cell rather than the smaller one.
fn round_to_u16(x: f32) -> Option<u16> {
    if !x.is_finite() {
        return None;
    }
    let rounded = x.round();
    // The comparison bounds the cast below: `rounded` is neither negative nor past `u16::MAX`, and a
    // whole number that small is exact in an `f32`.
    if !(0.0..=65535.0).contains(&rounded) {
        return None;
    }
    Some(rounded as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The answer to the probe's kitty graphics query, from a terminal that speaks it.
    const KITTY_OK: &[u8] = b"\x1b_Gi=4242;OK\x1b\\";
    /// XTVERSION's answer from a kitty new enough to have placeholder cells.
    const KITTY_VERSION: &[u8] = b"\x1bP>|kitty(0.35.2)\x1b\\";
    /// `CSI 16 t`'s answer: a cell 8 pixels wide and 16 tall.
    const CELL_16T: &[u8] = b"\x1b[6;16;8t";
    /// `CSI 14 t`'s answer: a text area 1024 by 768 pixels.
    const AREA_14T: &[u8] = b"\x1b[4;768;1024t";
    /// DA1's answer, listing `4`: sixel.
    const DA1_SIXEL: &[u8] = b"\x1b[?62;4;22c";
    /// A whole reply set, the way a real terminal writes it back.
    fn full_replies() -> Vec<u8> {
        [
            KITTY_OK,
            KITTY_VERSION,
            b"\x1b]1337;ReportCellSize=17.50;8.00;2.0\x07",
            CELL_16T,
            AREA_14T,
            DA1_SIXEL,
        ]
        .concat()
    }

    /// The XTVERSION text a buffer reports, or an empty string when it reports none.
    fn xtversion_of(bytes: &[u8]) -> String {
        parse_replies(bytes).xtversion.unwrap_or_default()
    }

    /// A deterministic run of bytes (SplitMix64), so the garbage this feeds the parser is the same
    /// garbage on every machine and no test needs a random number dependency.
    fn garbage(len: usize) -> Vec<u8> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            out.push((z ^ (z >> 31)) as u8);
        }
        out
    }

    #[test]
    fn parse_kitty_ok_and_error() {
        assert!(parse_replies(KITTY_OK).kitty_ok);
        // Bytes around the answer do not change it.
        assert!(parse_replies(b"x\x1b_Gi=4242;OK\x1b\\y").kitty_ok);
        // Any message other than `OK` after the id is not a yes.
        assert!(!parse_replies(b"\x1b_Gi=4242;ERROR;unsupported\x1b\\").kitty_ok);
        assert!(!parse_replies(b"\x1b_Gi=4242;INVALID\x1b\\").kitty_ok);
        // Another id is not an answer to this probe's query.
        assert!(!parse_replies(b"\x1b_Gi=99;OK\x1b\\").kitty_ok);
        // An answer that has not finished yet is no answer: the probe may have stopped mid-sequence.
        assert!(!parse_replies(b"\x1b_Gi=4242;OK").kitty_ok);
        assert!(!parse_replies(b"").kitty_ok);
    }

    #[test]
    fn parse_xtversion_kitty_ghostty_wezterm_iterm2() {
        // What each of these terminals is documented to report.
        assert_eq!(xtversion_of(KITTY_VERSION), "kitty(0.35.2)");
        assert_eq!(xtversion_of(b"\x1bP>|ghostty 1.1.3\x1b\\"), "ghostty 1.1.3");
        assert_eq!(
            xtversion_of(b"\x1bP>|WezTerm 20240203-110809-5046fc22\x1b\\"),
            "WezTerm 20240203-110809-5046fc22"
        );
        assert_eq!(xtversion_of(b"\x1bP>|iTerm2 3.6.9\x1b\\"), "iTerm2 3.6.9");
        // No answer at all is `None`, not an empty string: the two mean different things to
        // `choose`, since a terminal that never answers XTVERSION may still answer everything else.
        assert!(parse_replies(b"").xtversion.is_none());
        assert!(parse_replies(b"\x1bP>|kitty(0.35.2)").xtversion.is_none());
        // The text is cut to 64 characters, so a terminal that answers with a paragraph cannot make
        // the probe hold an unbounded string.
        let mut long = b"\x1bP>|".to_vec();
        long.extend(std::iter::repeat_n(b'a', 70));
        long.extend(ST);
        assert_eq!(xtversion_of(&long).len(), XTVERSION_MAX);
        // Non-printable bytes are dropped rather than landing in a string the UI would draw.
        assert_eq!(xtversion_of(b"\x1bP>|kitty\x7f(0.35)\x1b\\"), "kitty(0.35)");
    }

    #[test]
    fn parse_iterm_cell_size() {
        // Height first, then width, then the pixels-per-point scale: iTerm2's own order.
        assert_eq!(
            parse_replies(b"\x1b]1337;ReportCellSize=17.00;8.00;2.00\x1b\\").iterm_cell,
            Some((17.0, 8.0, 2.0))
        );
        // `BEL` ends an OSC as well as `ST`.
        assert_eq!(
            parse_replies(b"\x1b]1337;ReportCellSize=17.50;8.00;2.0\x07").iterm_cell,
            Some((17.5, 8.0, 2.0))
        );
        // The scale is optional and means 1.0 when it is left out.
        assert_eq!(
            parse_replies(b"\x1b]1337;ReportCellSize=16.00;8.00\x1b\\").iterm_cell,
            Some((16.0, 8.0, 1.0))
        );
        // A number that is not a number, a negative one and one that is not finite each make the
        // whole answer unusable: half a cell size is not enough to place an image on.
        assert_eq!(
            parse_replies(b"\x1b]1337;ReportCellSize=nan;8.00;2.0\x1b\\").iterm_cell,
            None
        );
        assert_eq!(
            parse_replies(b"\x1b]1337;ReportCellSize=-16.00;8.00;2.0\x1b\\").iterm_cell,
            None
        );
        assert_eq!(
            parse_replies(b"\x1b]1337;ReportCellSize=1e400;8.00;2.0\x1b\\").iterm_cell,
            None
        );
        // A missing width, a fourth field and an answer that has not ended are all no answer.
        assert_eq!(
            parse_replies(b"\x1b]1337;ReportCellSize=17.00\x1b\\").iterm_cell,
            None
        );
        assert_eq!(
            parse_replies(b"\x1b]1337;ReportCellSize=17.00;8.00;2.00;9\x1b\\").iterm_cell,
            None
        );
        assert_eq!(
            parse_replies(b"\x1b]1337;ReportCellSize=17.00;8.00").iterm_cell,
            None
        );
    }

    #[test]
    fn parse_csi_t_and_da1() {
        // `CSI 6 t` answers the cell size and `CSI 14 t` the text area, height first both times.
        assert_eq!(parse_replies(CELL_16T).cell_16t, Some((16, 8)));
        assert_eq!(parse_replies(AREA_14T).area_14t, Some((768, 1024)));
        // DA1, which every terminal answers.
        assert_eq!(parse_replies(DA1_SIXEL).da1, Some(vec![62, 4, 22]));
        assert_eq!(parse_replies(b"\x1b[?62c").da1, Some(vec![62]));
        // A parameter past `u16` is not a value anyone could act on, so the answer means nothing.
        assert_eq!(parse_replies(b"\x1b[6;99999;8t").cell_16t, None);
        assert_eq!(parse_replies(b"\x1b[?62;4;99999c").da1, None);
        // Answers that have not ended yet are no answer.
        assert_eq!(parse_replies(b"\x1b[4;768;1024").area_14t, None);
        assert_eq!(parse_replies(b"\x1b[?62;4;22").da1, None);
        // One buffer carrying every answer answers all six queries at once.
        let all = parse_replies(&full_replies());
        assert!(all.kitty_ok);
        assert_eq!(all.xtversion.as_deref(), Some("kitty(0.35.2)"));
        assert_eq!(all.iterm_cell, Some((17.5, 8.0, 2.0)));
        assert_eq!(all.cell_16t, Some((16, 8)));
        assert_eq!(all.area_14t, Some((768, 1024)));
        assert_eq!(all.da1, Some(vec![62, 4, 22]));
        // The probe's stop condition: the buffer up to DA1's final `c`, and not before it.
        let full = full_replies();
        assert!(da1_complete(&full));
        // The stop condition is about the end of the buffer: the probe reads a byte at a time and stops on
        // the answer's own final `c`, so bytes after it are never in the buffer while it is deciding.
        assert!(da1_complete(b"junk\x1b[?62;4;22c"));
        assert!(!da1_complete(b"junk\x1b[?62;4;22cmore"));
        // `parse_replies` still finds an answer anywhere in a buffer, so nothing is lost by the narrower
        // stop condition: these same bytes answer DA1 whether or not something follows them.
        assert_eq!(
            parse_replies(b"junk\x1b[?62;4;22cmore").da1,
            Some(vec![62, 4, 22])
        );
        assert!(!da1_complete(&full[..full.len() - 1]));
        assert!(!da1_complete(b"\x1b[>0q\x1b[16t"));
    }

    /// The probe calls [`da1_complete`] after every byte it reads, so a terminal that answers nothing must
    /// not cost it a fresh scan from every `ESC` in the buffer: 4 KiB read a byte at a time is 4096 checks
    /// of a buffer that grows to 4096 bytes.
    #[test]
    fn da1_complete_is_linear_on_hostile_input() {
        // `\x1b[?` repeated is the worst such stream: an escape opener every three bytes, and no final `c`
        // anywhere for a scan to stop at.
        let repeated = b"\x1b[?".repeat(1366);
        let hostile = &repeated[..4096];
        assert_eq!(hostile.len(), 4096);
        assert!(!hostile.contains(&b'c'), "nothing here ends a DA1 answer");

        let started = Instant::now();
        for cut in 1..=hostile.len() {
            assert!(
                !da1_complete(&hostile[..cut]),
                "a prefix of a probe nobody answered is not an answer"
            );
        }
        // Generous on purpose: this measures that the check looks at the tail once rather than at every
        // escape byte, not that it is quick. Checking DA1 from every `ESC` took seconds here.
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "the stop check took {:?} over {} prefixes of 4 KiB",
            elapsed,
            hostile.len()
        );
    }

    #[test]
    fn parse_survives_garbage() {
        let full = full_replies();
        let complete = parse_replies(&full);
        // Every prefix of a full reply set: a truncated answer is no answer, and no prefix invents
        // one that the whole buffer does not also report.
        for cut in 0..=full.len() {
            let replies = parse_replies(&full[..cut]);
            assert!(!replies.kitty_ok || complete.kitty_ok);
            assert!(replies.xtversion.is_none() || replies.xtversion == complete.xtversion);
            assert!(replies.iterm_cell.is_none() || replies.iterm_cell == complete.iterm_cell);
            assert!(replies.cell_16t.is_none() || replies.cell_16t == complete.cell_16t);
            assert!(replies.area_14t.is_none() || replies.area_14t == complete.area_14t);
            assert!(replies.da1.is_none() || replies.da1 == complete.da1);
        }
        // 4 KiB of bytes from a fixed seed, every prefix of it: nothing panics and nothing that
        // comes back is out of range for the type it came back in.
        let junk = garbage(4096);
        for cut in 0..=junk.len() {
            let replies = parse_replies(&junk[..cut]);
            if let Some(text) = &replies.xtversion {
                assert!(text.len() <= XTVERSION_MAX);
                assert!(text.bytes().all(|b| b == b' ' || b.is_ascii_graphic()));
            }
            if let Some((h, w, scale)) = replies.iterm_cell {
                assert!(h.is_finite() && w.is_finite() && scale.is_finite());
                assert!(h >= 0.0 && w >= 0.0 && scale >= 0.0);
            }
        }
        // A DA1 whose number does not fit a `u16`: no answer, and it is not the stop condition
        // either, which costs the probe the deadline it was already going to pay.
        assert_eq!(parse_replies(b"\x1b[?99999999999c").da1, None);
        assert!(!da1_complete(b"\x1b[?99999999999c"));
    }

    /// A kitty or Ghostty that answered the graphics query and named itself, with its cell size
    /// from `CSI 16 t`.
    fn named(xtversion: &str) -> Replies {
        Replies {
            kitty_ok: true,
            xtversion: Some(xtversion.to_string()),
            cell_16t: Some((16, 8)),
            ..Replies::default()
        }
    }

    /// The same terminal with no answer to any of the cell size queries.
    fn name_only(xtversion: &str) -> Replies {
        Replies {
            cell_16t: None,
            ..named(xtversion)
        }
    }

    #[test]
    fn choose_protocol_table() {
        let grid = (80, 24);
        let cells = Some(CellPx { w: 8, h: 16 });

        // Kitty that answers the graphics query and names a version with placeholder cells.
        assert_eq!(
            choose(&named("kitty(0.35.2)"), grid, None),
            Graphics {
                protocol: Protocol::Kitty,
                cell: cells,
                scale: 1
            }
        );
        // 0.28 is the first version with them, so the boundary answers for itself.
        assert_eq!(
            choose(&named("kitty(0.28.0)"), grid, None).protocol,
            Protocol::Kitty
        );
        // 0.27 answers `OK` but has no placeholders, and nothing else here claims an image protocol.
        assert_eq!(
            choose(&named("kitty(0.27.0)"), grid, None),
            Graphics::none()
        );
        // Ghostty needs no version check: its kitty support has placeholders from the start.
        assert_eq!(
            choose(&named("ghostty 1.1.3"), grid, None).protocol,
            Protocol::Kitty
        );
        // WezTerm answers the kitty query but has no placeholders while its OSC 1337 is complete, so
        // it gets inline images even though its DA1 also claims sixel.
        let wezterm = Replies {
            kitty_ok: true,
            xtversion: Some("WezTerm 20240203-110809-5046fc22".to_string()),
            cell_16t: Some((16, 8)),
            da1: Some(vec![62, 4]),
            ..Replies::default()
        };
        assert_eq!(choose(&wezterm, grid, None).protocol, Protocol::Iterm2);
        // iTerm2's own cell size answer is enough to pick it, and its scale comes with the answer. The
        // cell is what those points are worth in device pixels: 8 x 17 points at scale 2 draws 16 x 34.
        let iterm = Replies {
            xtversion: Some("iTerm2 3.6.9".to_string()),
            iterm_cell: Some((17.0, 8.0, 2.0)),
            ..Replies::default()
        };
        assert_eq!(
            choose(&iterm, grid, None),
            Graphics {
                protocol: Protocol::Iterm2,
                cell: Some(CellPx { w: 16, h: 34 }),
                scale: 2
            }
        );
        // A half pixel is settled on the multiplied number and not on the points: 8.25 x 16.5 points at
        // scale 2 is a 17-pixel column and a 33-pixel row. Rounding the points first would have called
        // this a cell of 8 x 17 points and then handed back 16 x 34 pixels, a size this terminal never
        // draws and a bitmap it would have to rescale.
        let iterm_half = Replies {
            iterm_cell: Some((16.50, 8.25, 2.00)),
            ..iterm
        };
        assert_eq!(
            choose(&iterm_half, grid, None),
            Graphics {
                protocol: Protocol::Iterm2,
                cell: Some(CellPx { w: 17, h: 33 }),
                scale: 2
            }
        );
        // foot: DA1 lists `4`, so sixel at the cell size `CSI 16 t` gave.
        let foot = Replies {
            da1: Some(vec![62, 4]),
            cell_16t: Some((16, 8)),
            ..Replies::default()
        };
        assert_eq!(
            choose(&foot, grid, None),
            Graphics {
                protocol: Protocol::Sixel,
                cell: cells,
                scale: 1
            }
        );
        // fbterm answers the kitty query `OK` but never answers XTVERSION, so it is not taken for a
        // placeholder-capable kitty and gets sixel from its DA1 (controller ruling of 2026-11-19).
        let fbterm = Replies {
            kitty_ok: true,
            da1: Some(vec![62, 4, 22]),
            cell_16t: Some((16, 8)),
            ..Replies::default()
        };
        assert_eq!(
            choose(&fbterm, grid, None),
            Graphics {
                protocol: Protocol::Sixel,
                cell: cells,
                scale: 1
            }
        );
        // A DA1 that does not list `4` claims no sixel, and nothing else here claims anything either.
        assert_eq!(
            choose(
                &Replies {
                    da1: Some(vec![62, 22]),
                    ..Replies::default()
                },
                grid,
                None
            ),
            Graphics::none()
        );
        // No cell size from anywhere means no logo, whatever the terminal can draw (decision 11).
        assert_eq!(
            choose(&name_only("kitty(0.35.2)"), grid, None),
            Graphics::none()
        );
        assert_eq!(
            choose(&name_only("ghostty 1.1.3"), grid, None),
            Graphics::none()
        );
        assert_eq!(
            choose(
                &Replies {
                    da1: Some(vec![62, 4]),
                    ..Replies::default()
                },
                grid,
                None
            ),
            Graphics::none()
        );
        // A grid of zero cells divides nothing, so the window size cannot give a cell size either.
        assert_eq!(
            choose(
                &Replies {
                    da1: Some(vec![62, 4]),
                    ..Replies::default()
                },
                (0, 0),
                Some((1024, 768))
            ),
            Graphics::none()
        );
        // The window size divided by the grid is the third source, and `CSI 14 t` divided by the
        // grid is the fourth: 1024 x 768 over 80 x 24 cells is a 13 x 32 cell.
        let from_ioctl = Replies {
            da1: Some(vec![62, 4]),
            ..Replies::default()
        };
        assert_eq!(
            choose(&from_ioctl, grid, Some((1024, 768))),
            Graphics {
                protocol: Protocol::Sixel,
                cell: Some(CellPx { w: 13, h: 32 }),
                scale: 1
            }
        );
        let from_14t = Replies {
            da1: Some(vec![62, 4]),
            area_14t: Some((768, 1024)),
            ..Replies::default()
        };
        assert_eq!(
            choose(&from_14t, grid, None),
            Graphics {
                protocol: Protocol::Sixel,
                cell: Some(CellPx { w: 13, h: 32 }),
                scale: 1
            }
        );
        // Earlier sources win when they answer: `CSI 16 t` over the window size and `CSI 14 t`.
        let all_sources = Replies {
            da1: Some(vec![62, 4]),
            cell_16t: Some((16, 8)),
            area_14t: Some((768, 1024)),
            ..Replies::default()
        };
        assert_eq!(choose(&all_sources, grid, Some((2048, 1536))).cell, cells);
        // A cell size outside 2..=64 wide or 4..=128 tall is ignored, and the next source gets its
        // turn: this `CSI 16 t` answer says a 300 x 300 pixel cell, which no font produces.
        let absurd_16t = Replies {
            da1: Some(vec![62, 4]),
            cell_16t: Some((300, 300)),
            area_14t: Some((768, 1024)),
            ..Replies::default()
        };
        assert_eq!(
            choose(&absurd_16t, grid, None).cell,
            Some(CellPx { w: 13, h: 32 })
        );
    }

    /// The bytes a run in fbterm 0.11 on Windows reads back from the console: the kitty answer
    /// dropped by the console layer apart from a stray `\`, then the `CSI 16 t` answer (a 10 x 20
    /// pixel cell) and DA1 claiming sixel. The decision table has to get the logo from what survives.
    #[test]
    fn fbterm_on_windows_picks_sixel_from_its_console_replies() {
        let replies = b"\x1b_Gi=31;OK\\_\x1b[6;20;10t\x1b[?62;4;22c";
        let verdict = choose(&parse_replies(replies), (80, 24), None);
        assert_eq!(
            verdict,
            Graphics {
                protocol: Protocol::Sixel,
                cell: Some(CellPx { w: 10, h: 20 }),
                scale: 1
            },
            "fbterm's Windows replies did not pick sixel"
        );
    }
}
