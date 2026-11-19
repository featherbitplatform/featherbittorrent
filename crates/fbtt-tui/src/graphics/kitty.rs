//! The kitty graphics protocol, as bytes to write down the pipe.
//!
//! Three things happen here: an image is transmitted once and kept under an id, a placement says
//! where that id goes, and - the part that makes this protocol worth using - a virtual placement is
//! drawn by printing ordinary text. A cell of a virtual placement carries `PLACEHOLDER_CHAR` with
//! two combining marks that encode its row and column inside the image and a foreground colour that
//! encodes the image id, so an image becomes cells in the grid ratatui already diffs. No cursor
//! positioning around the picture, no flicker when the table next to it changes (decision 10).
//!
//! Transmission is chunked because a terminal's write buffer is not unbounded: raw RGBA goes out as
//! base64 in pieces of at most 4096 characters, and `m=1` on every piece but the last tells the
//! terminal to keep receiving.

use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use ratatui::style::Color;

use super::image::Rgba;

/// The id of the small logo (the header's three rows), so a redraw can place, move or delete it
/// without sending its pixels again.
pub const SMALL_ID: u32 = 0x00FB_7001;
/// The id of the large logo (the help screen's ten rows).
pub const LARGE_ID: u32 = 0x00FB_7002;

/// The private-use character a placeholder cell carries in place of an image.
const PLACEHOLDER_CHAR: char = '\u{10EEEE}';

/// `ST`: the two bytes that end every kitty graphics sequence this file writes.
const ST: &[u8] = b"\x1b\\";

/// The combining marks that encode a row or column number, mark `n` meaning value `n`: the first 24
/// entries of the kitty protocol's `rowcolumn-diacritics.txt`, copied from the same table in
/// feathercritty rather than from memory.
///
/// Twenty-four covers more cells than [`crate::graphics::image::logo_dims`] will ever ask for, since
/// it refuses a logo past 24 rows or columns.
pub const DIACRITICS: [char; 24] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
];

/// The most base64 characters one transmission piece carries. Raw RGBA is chunked so that no piece
/// is bigger than this once it is encoded.
const CHUNK_B64_BYTES: usize = 4096;

/// Transmit `img` under `id` as raw RGBA (`f=32`), in base64 pieces.
///
/// The first piece carries the whole header - id, size and the mode bits - and every later one only
/// its `m` flag. A single piece carries `m=0`, which is also what the last of several carries: it is
/// the terminal's cue that the image is complete and can be drawn. An image with no pixels sends
/// nothing at all.
#[must_use]
pub fn transmit(id: u32, img: &Rgba) -> Vec<u8> {
    let encoded = BASE64_STANDARD.encode(&img.px);
    let payload = encoded.as_bytes();
    if payload.is_empty() {
        return Vec::new();
    }
    let pieces = payload.len().div_ceil(CHUNK_B64_BYTES);
    // Each piece is its own escape sequence, so a `Vec` that grows once to the total size rather than
    // one reallocation per piece.
    let mut out = Vec::with_capacity(payload.len() + 96 * pieces);
    for (piece_index, piece) in payload.chunks(CHUNK_B64_BYTES).enumerate() {
        let more = piece_index + 1 < pieces;
        if piece_index == 0 {
            // The first piece carries everything the terminal needs to allocate the image: its id and
            // its size, plus `f=32` for raw RGBA. Later pieces only say whether more is coming.
            out.extend(b"\x1b_Ga=t,q=2,f=32,i=");
            push_number(&mut out, id);
            out.extend(b",s=");
            push_number(&mut out, img.w);
            out.extend(b",v=");
            push_number(&mut out, img.h);
            out.extend(b",m=");
        } else {
            out.extend(b"\x1b_Gm=");
        }
        out.push(if more { b'1' } else { b'0' });
        out.push(b';');
        out.extend(piece);
        out.extend(ST);
    }
    out
}

/// Place the image `id` over `cols` columns and `rows` rows, at the cursor.
///
/// `U=1` makes it a virtual placement: nothing is drawn until the placeholder cells are printed,
/// which is what lets ratatui own the picture like any other text.
#[must_use]
pub fn place(id: u32, cols: u16, rows: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(48);
    out.extend(b"\x1b_Ga=p,U=1,q=2,i=");
    push_number(&mut out, id);
    out.extend(b",c=");
    push_number(&mut out, u32::from(cols));
    out.extend(b",r=");
    push_number(&mut out, u32::from(rows));
    out.extend(ST);
    out
}

/// Delete `id` from the screen and free its data too: kitty's uppercase `d=I` removes the image's
/// placements *and* forgets the transmitted pixels, so drawing it again afterwards needs a fresh
/// [`transmit`].
///
/// That is an exit verb, and this client only sends it on exit (`Placer::on_exit`, Task 9, before the
/// terminal is restored), where giving the data back is exactly what should happen. A change of size or
/// position mid-session is not a delete: the placer transmits the new pixels under the same id and
/// [`place`]s it again.
#[must_use]
pub fn delete(id: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend(b"\x1b_Ga=d,d=I,i=");
    push_number(&mut out, id);
    out.extend(b",q=2");
    out.extend(ST);
    out
}

/// The text one cell of `id`'s virtual placement is drawn as: the placeholder character plus the
/// marks for `(row, col)` inside the placement.
#[must_use]
pub fn placeholder(row: u16, col: u16) -> String {
    let mut out = String::with_capacity(PLACEHOLDER_CHAR.len_utf8() * 3);
    out.push(PLACEHOLDER_CHAR);
    out.push(diacritic(row));
    out.push(diacritic(col));
    out
}

/// The mark that encodes one row or column value.
///
/// `logo_dims` never asks for more than 24 of either, so reaching past the table means a caller that
/// ignored it; falling back to the first mark keeps the cell a placeholder rather than turning it into
/// a combining accent on whatever text was there before.
fn diacritic(value: u16) -> char {
    DIACRITICS
        .get(usize::from(value))
        .copied()
        .unwrap_or(DIACRITICS[0])
}

/// The foreground colour that carries `id` in a placeholder cell: its low three bytes.
#[must_use]
pub fn id_colour(id: u32) -> Color {
    Color::Rgb((id >> 16) as u8, (id >> 8) as u8, id as u8)
}

/// A number written as decimal digits into `out`.
///
/// The escape sequences are bytes and their fields are numbers, so the digits go straight into the
/// buffer instead of through a `String` per field: an image transmission is written once per placement
/// but still carries several numbers.
fn push_number(out: &mut Vec<u8>, value: u32) {
    // Digits come out least significant first, so they are written into a fixed buffer (ten digits is
    // the most a `u32` has) and copied over in reverse.
    let mut digits = [0u8; 10];
    let mut len = 0usize;
    let mut rest = value;
    loop {
        digits[len] = b'0' + (rest % 10) as u8;
        len += 1;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    out.extend(digits[..len].iter().rev());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `(m, chunk)` pairs of a transmission: each piece is an APC string between two `ST`s, and
    /// the only `;` in one is the separator before its base64 data.
    fn transmissions(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        let mut rest = bytes;
        while let Some(end) = rest.windows(ST.len()).position(|w| w == ST) {
            let part = &rest[..end];
            rest = &rest[end + ST.len()..];
            if part.is_empty() {
                continue;
            }
            let semi = part
                .iter()
                .position(|b| *b == b';')
                .expect("a transmission separates its header from its data with ';'");
            let flag = part
                .windows(2)
                .position(|w| w == b"m=")
                .expect("every transmission carries 'm='")
                + 2;
            out.push((part[flag], part[semi + 1..].to_vec()));
        }
        out
    }

    #[test]
    fn kitty_transmit_bytes() {
        // A 2 x 1 image, red then blue, fully opaque: eight bytes of raw RGBA, one piece.
        let img = Rgba {
            w: 2,
            h: 1,
            px: vec![255, 0, 0, 255, 0, 0, 255, 255],
        };
        assert_eq!(
            transmit(SMALL_ID, &img),
            b"\x1b_Ga=t,q=2,f=32,i=16478209,s=2,v=1,m=0;/wAA/wAA//8=\x1b\\".to_vec()
        );

        // 3000 x 1 is 12000 raw bytes: 16000 base64 characters, so four pieces with `m=1` on all but
        // the last, and no piece bigger than the chunk limit.
        let px: Vec<u8> = (0..12_000u32).map(|i| (i % 256) as u8).collect();
        let wide = Rgba { w: 3000, h: 1, px };
        let pieces = transmissions(&transmit(SMALL_ID, &wide));
        assert_eq!(pieces.len(), 4);
        assert_eq!(
            pieces.iter().map(|(m, _)| *m).collect::<Vec<_>>(),
            vec![b'1', b'1', b'1', b'0']
        );
        assert_eq!(
            pieces.iter().map(|(_, c)| c.len()).collect::<Vec<_>>(),
            vec![4096, 4096, 4096, 3712]
        );
        // Each piece is legal base64 on its own, and all four put the pixels back together.
        let joined: Vec<u8> = pieces
            .iter()
            .flat_map(|(_, chunk)| BASE64_STANDARD.decode(chunk).expect("a chunk decodes"))
            .collect();
        assert_eq!(joined, wide.px);
    }

    #[test]
    fn kitty_place_and_delete_bytes() {
        assert_eq!(
            place(LARGE_ID, 12, 10),
            b"\x1b_Ga=p,U=1,q=2,i=16478210,c=12,r=10\x1b\\".to_vec()
        );
        // The `d=I` in these bytes is the whole rule: exit only, and it frees the placements and the
        // transmitted data together.
        assert_eq!(
            delete(SMALL_ID),
            b"\x1b_Ga=d,d=I,i=16478209,q=2\x1b\\".to_vec()
        );
    }

    #[test]
    fn kitty_placeholder_cells() {
        // Row 0 is `U+0305` and column 2 is `U+030E`, which is kitty's own table saying value 0 is
        // the first mark and value 2 the third.
        assert_eq!(placeholder(0, 2), "\u{10EEEE}\u{0305}\u{030E}");
        assert_eq!(placeholder(1, 0), "\u{10EEEE}\u{030D}\u{0305}");
        // The table is kitty's `rowcolumn-diacritics.txt` in order, first 24 entries.
        assert_eq!(DIACRITICS.len(), 24);
        assert_eq!(
            &DIACRITICS[..6],
            [
                '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}'
            ]
        );
        assert_eq!(DIACRITICS[23], '\u{0369}');
        // The id travels in the foreground colour: its low three bytes.
        assert_eq!(id_colour(SMALL_ID), Color::Rgb(0xFB, 0x70, 0x01));
        assert_eq!(id_colour(LARGE_ID), Color::Rgb(0xFB, 0x70, 0x02));
    }
}
