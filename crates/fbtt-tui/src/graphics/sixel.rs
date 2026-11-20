//! Sixel: a DCS string carrying a palette and bands of six pixel rows.
//!
//! Sixel is the oldest of the three protocols here and the only one with no image id to keep, so it
//! gets the simplest treatment: quantize to at most 256 colours, then write the palette and one band at
//! a time. A terminal that answers DA1 with `4` understands this (foot, fbterm, xterm built with sixel
//! graphics), and nothing else in the plan needs it.
//!
//! Sixel has no alpha channel, so [`quantize`] turns each pixel's alpha into painted or not: a
//! pixel below half opaque paints nothing, and the stream's background flag (`P2=1`) leaves
//! whatever the terminal already has on those pixels; a painted pixel carries the colour it
//! composites over the canvas it is given at its own alpha, so edges stay smooth where the
//! terminal's ground is solid and at most a thin rim shows where it is not.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use super::image::{Rgba, composite};

/// The most colours a sixel palette holds.
const MAX_COLORS: usize = 256;

/// How many pixel rows a sixel band carries.
const BAND_ROWS: usize = 6;

/// The shortest run worth writing with `!`, and the longest count that field can hold.
const RUN_MIN: usize = 4;
const RUN_MAX: usize = 255;

/// `ST`: the two bytes that end a sixel string.
const ST: &str = "\x1b\\";

/// The least alpha, out of 255, that paints a pixel. Sixel carries no partial alpha: a pixel below
/// half opaque is read as whatever is behind it, and the format's only two choices are a solid
/// colour or nothing, so nothing wins and the terminal's own pixels show through.
const PAINT_ALPHA: u8 = 128;

/// An image in the form sixel is written from: a palette of at most 256 colours and one entry per
/// pixel naming what paints it, if anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Indexed {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
    /// The palette, in the order the encoder numbers it: entry `i` is `#i`.
    pub palette: Vec<[u8; 3]>,
    /// `w * h` entries, rows in order: `Some(i)` paints the pixel with palette entry `i`, and
    /// `None` paints nothing there, leaving whatever the terminal already has on that pixel.
    pub idx: Vec<Option<u8>>,
}

/// `img` reduced to at most 256 colours, with each pixel's alpha turned into painted or not.
///
/// A pixel below half opaque is left unpainted (`None` in `idx`), and the stream's background flag
/// keeps the terminal's own pixels there. A pixel at or above it paints the colour `composite`
/// makes of it over `canvas` at its own alpha, which is what keeps the mark's anti-aliased edges
/// exactly right on a solid ground. An unpainted pixel contributes no colour to anything: a palette
/// entry spent on a pixel that is never drawn is a wasted entry.
///
/// An image whose painted colours are no more than 256 keeps them exactly, in the order they first
/// appear - which is what a logo drawn from five theme colours deserves, and it costs one pass over
/// the pixels. Anything with more goes through NeuQuant (the algorithm `pngquant` uses) at its usual
/// study factor of 10, trained on the painted pixels only.
///
/// An image whose byte count disagrees with its dimensions quantizes to nothing: an `Indexed` whose
/// indices do not match its size would have `encode` read colours that are not there.
#[must_use]
pub fn quantize(img: &Rgba, canvas: [u8; 3]) -> Indexed {
    let Some(need) = usize::try_from(u64::from(img.w) * u64::from(img.h) * 4).ok() else {
        return Indexed::default();
    };
    if img.px.len() != need {
        return Indexed::default();
    }

    // What each pixel paints, in pixel order: `None` where there is nothing to paint. Both passes
    // below read this list, so the threshold and the compositing cannot drift between them.
    let painted: Vec<Option<[u8; 3]>> = img
        .px
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| paint(pixel, canvas))
        .collect();

    // One pass over the painted colours, numbering each one the first time it appears. A mark drawn
    // from a handful of theme colours keeps those exact values this way, which is what it deserves;
    // an image with more colours than a palette holds throws this away and goes through NeuQuant.
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut index_of: HashMap<[u8; 3], u8> = HashMap::new();
    let mut idx: Vec<Option<u8>> = Vec::with_capacity(painted.len());
    for entry in &painted {
        let Some(rgb) = *entry else {
            idx.push(None);
            continue;
        };
        let index = match index_of.entry(rgb) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                // Past 256 the number no longer fits a palette entry, but that only matters for the
                // result this function is about to throw away.
                let fresh = u8::try_from(palette.len()).unwrap_or(0);
                palette.push(rgb);
                *entry.insert(fresh)
            }
        };
        idx.push(Some(index));
    }
    if palette.len() <= MAX_COLORS {
        return Indexed {
            w: img.w,
            h: img.h,
            palette,
            idx,
        };
    }

    // More colours than a palette holds. `10` is the study factor `pngquant` uses too: every tenth
    // painted pixel trains the net well enough on an image this size. Unpainted pixels stay out of
    // the training set entirely, so the net spends all 256 of its entries on colours the stream
    // will actually draw.
    let painted_px: Vec<u8> = painted
        .iter()
        .filter_map(|entry| entry.map(|[r, g, b]| [r, g, b, 255]))
        .flatten()
        .collect();
    let net = color_quant::NeuQuant::new(10, MAX_COLORS, &painted_px);
    let palette: Vec<[u8; 3]> = (0..MAX_COLORS)
        .map(|i| {
            let [r, g, b, _a] = net.lookup(i).unwrap_or([0, 0, 0, 255]);
            [r, g, b]
        })
        .collect();
    let idx: Vec<Option<u8>> = painted
        .iter()
        .map(|entry| {
            entry.map(|[r, g, b]| u8::try_from(net.index_of(&[r, g, b, 255])).unwrap_or(0))
        })
        .collect();
    Indexed {
        w: img.w,
        h: img.h,
        palette,
        idx,
    }
}

/// What one pixel paints, if it paints anything: nothing below [`PAINT_ALPHA`], and above it the
/// colour the pixel composites over `canvas` at its own alpha. That composite is the closest a
/// format with no partial alpha gets to the pixel's own transparency: it is exactly what the pixel
/// would show on a ground of `canvas`, painted as one solid colour.
fn paint(pixel: &[u8; 4], canvas: [u8; 3]) -> Option<[u8; 3]> {
    if pixel[3] < PAINT_ALPHA {
        return None;
    }
    let alpha = u32::from(pixel[3]);
    Some([
        composite(pixel[0], alpha, canvas[0]),
        composite(pixel[1], alpha, canvas[1]),
        composite(pixel[2], alpha, canvas[2]),
    ])
}

/// `img` as a sixel stream: the header, the palette, then one band of six pixel rows at a time.
///
/// Within a band each colour present gets the bits of the columns it paints as sixel characters (`63`
/// plus the bits, bit 0 being the band's top row), four or more identical characters in a row collapse
/// to `!<count><char>`, colours are separated by `$` and bands by `-`. The palette is written in full at
/// the start because a sixel stream has no way to say "the colours you already know". A pixel whose
/// entry is `None` is no colour's bit in any band - like an index past the end of `idx`, it writes
/// nothing - so a hand-built `Indexed` cannot read out of bounds and a fully transparent image
/// encodes to just the header and `ST`.
#[must_use]
pub fn encode(img: &Indexed) -> Vec<u8> {
    // Everything written here is ASCII, so a `String` is just a buffer that grows itself.
    let mut out = String::new();
    // The header: sixel graphics with no background colour specified, and the image's own size so the
    // terminal knows how much of it to draw.
    out.push_str(&format!("\x1bP0;1;0q\"1;1;{};{}", img.w, img.h));
    for (i, rgb) in img.palette.iter().enumerate() {
        // `2` is the direct-RGB spelling of a palette entry; channels are percentages, because that is
        // all this format carries.
        out.push_str(&format!(
            "#{};2;{};{};{}",
            i,
            percent(rgb[0]),
            percent(rgb[1]),
            percent(rgb[2])
        ));
    }

    let width = usize::try_from(img.w).unwrap_or(0);
    let bands = usize::try_from(u64::from(img.h).div_ceil(BAND_ROWS as u64)).unwrap_or(0);
    let mut masks: Vec<u8> = vec![0u8; width];
    for band in 0..bands {
        let rows = usize::try_from(
            img.h
                .saturating_sub(u32::try_from(band * BAND_ROWS).unwrap_or(u32::MAX))
                .min(BAND_ROWS as u32),
        )
        .unwrap_or(0);
        // Colours in palette order, each with the columns it paints in this band. `written` is what
        // keeps the `$` a separator rather than a terminator.
        let mut written = false;
        for i in 0..img.palette.len() {
            let colour = u8::try_from(i).unwrap_or(u8::MAX);
            masks[..width].fill(0);
            let mut present = false;
            for row in 0..rows {
                let base = (band * BAND_ROWS + row) * width;
                // One bit per row of the band: bit 0 is its top row, which is how a sixel character
                // stacks vertically down the screen.
                let bit = 1u8 << row;
                for (col, mask) in masks.iter_mut().enumerate().take(width) {
                    // `None` matches no colour: an unpainted pixel sets no bit in any band.
                    if img.idx.get(base + col).copied().flatten() == Some(colour) {
                        *mask |= bit;
                        present = true;
                    }
                }
            }
            if !present {
                continue;
            }
            if written {
                out.push('$');
            }
            written = true;
            out.push_str(&format!("#{i}"));
            push_runs(&mut out, &masks[..width]);
        }
        if band + 1 < bands {
            out.push('-');
        }
    }

    out.push_str(ST);
    out.into_bytes()
}

/// The sixel percentage for one 8-bit channel: `round(v * 100 / 255)`, which is all this format can
/// express. 255 becomes 100 and 0 stays 0.
fn percent(v: u8) -> u32 {
    (u32::from(v) * 100 + 127) / 255
}

/// `masks` as sixel characters, with runs of [`RUN_MIN`] or more written `!<count><char>`.
///
/// The count field holds at most [`RUN_MAX`], so a longer run is written as several of them; the short
/// tail that leaves goes out as plain characters, because `!1`, `!2` and `!3` save nothing over
/// repeating the character.
fn push_runs(out: &mut String, masks: &[u8]) {
    let mut at = 0usize;
    while at < masks.len() {
        let value = masks[at];
        let mut end = at + 1;
        while end < masks.len() && masks[end] == value {
            end += 1;
        }
        let run = end - at;
        // `63` plus the bits: `?` for no rows of this colour, `~` for all six.
        let ch = char::from(63 + value);
        if run < RUN_MIN {
            for _ in 0..run {
                out.push(ch);
            }
            at = end;
            continue;
        }
        let mut left = run;
        while left > RUN_MAX {
            out.push_str("!255");
            out.push(ch);
            left -= RUN_MAX;
        }
        if left >= RUN_MIN {
            out.push('!');
            out.push_str(&left.to_string());
            out.push(ch);
        } else {
            for _ in 0..left {
                out.push(ch);
            }
        }
        at = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::caps::CellPx;
    use crate::graphics::image::{logo_dims, mark, render};

    /// The canvas sixel paints against: `theme::INK_950`, `#0b0b0e`.
    const CANVAS: [u8; 3] = [0x0b, 0x0b, 0x0e];

    /// A vertical strip: `top` pixels of red above `bottom` pixels of blue, one column wide.
    fn strip(top: usize, bottom: usize) -> Rgba {
        let mut px = Vec::new();
        for _ in 0..top {
            px.extend([255, 0, 0, 255]);
        }
        for _ in 0..bottom {
            px.extend([0, 0, 255, 255]);
        }
        Rgba {
            w: 1,
            h: u32::try_from(top + bottom).expect("a test strip fits"),
            px,
        }
    }

    #[test]
    fn sixel_two_colours() {
        // One column, six rows: the red half is bits 0..2 of the band's only column (`63 + 7` is `F`)
        // and the blue half is bits 3..5 (`63 + 56` is `w`). Red appears first, so it is `#0`.
        let img = strip(3, 3);
        assert_eq!(
            encode(&quantize(&img, CANVAS)),
            b"\x1bP0;1;0q\"1;1;1;6#0;2;100;0;0#1;2;0;0;100#0F$#1w\x1b\\".to_vec()
        );
    }

    #[test]
    fn sixel_runs() {
        // Eight columns of one colour across a full band: the same character eight times in a row is
        // written as `!8` plus that character, which is all six rows set (`63 + 63` is `~`).
        let img = Rgba {
            w: 8,
            h: 6,
            px: [255u8, 0, 0, 255].repeat(48),
        };
        let out = encode(&quantize(&img, CANVAS));
        assert!(
            out.windows(5).any(|w| w == b"#0!8~"),
            "a run of eight is written as `!8`: {}",
            String::from_utf8_lossy(&out)
        );
    }

    #[test]
    fn sixel_two_bands() {
        // Twelve rows is two bands, and bands are separated - not terminated - by one `-`.
        let img = strip(6, 6);
        let out = encode(&quantize(&img, CANVAS));
        assert_eq!(
            out.iter().filter(|b| **b == b'-').count(),
            1,
            "two bands are separated by one '-': {}",
            String::from_utf8_lossy(&out)
        );
        assert!(out.starts_with(b"\x1bP0;1;0q\"1;1;1;12"));
        assert!(out.ends_with(b"\x1b\\"));
    }

    #[test]
    fn sixel_leaves_transparent_pixels_unpainted() {
        // 2 x 1: one opaque red pixel and one fully transparent. Sixel cannot carry partial alpha, so
        // the transparent one must be painted by no colour at all: the header asks for background
        // transparency, and what shows there is the terminal's own.
        let img = Rgba {
            w: 2,
            h: 1,
            px: vec![255, 0, 0, 255, 200, 200, 200, 0],
        };
        let q = quantize(&img, CANVAS);
        assert_eq!(
            q.palette,
            vec![[255, 0, 0]],
            "a transparent pixel was painted"
        );
        assert_eq!(
            q.idx,
            vec![Some(0), None],
            "a transparent pixel was painted"
        );
        // One band, one row: the red column carries bit 0 of the mask (`63 + 1` is `@`) and the
        // transparent column carries no bit in the only colour's band at all (`63` is `?`).
        assert_eq!(
            encode(&q),
            b"\x1bP0;1;0q\"1;1;2;1#0;2;100;0;0#0@?\x1b\\".to_vec(),
            "a transparent pixel was painted"
        );
        // A fully transparent image still encodes, validly: the header, no colours, then `ST`.
        let ghost = Rgba {
            w: 2,
            h: 1,
            px: vec![200, 200, 200, 0, 10, 20, 30, 0],
        };
        assert_eq!(
            encode(&quantize(&ghost, CANVAS)),
            b"\x1bP0;1;0q\"1;1;2;1\x1b\\".to_vec(),
            "a fully transparent image must still encode"
        );
    }

    #[test]
    fn sixel_paints_a_mostly_opaque_edge_over_the_canvas() {
        // A pixel at alpha 200 is mostly opaque: the eye reads it as a colour, not as the ground, so
        // sixel paints it with the colour `composite` makes of it over the canvas at that alpha.
        // composite(c, 200, over) is `(c * 200 + over * 55 + 127) / 255`: 159, 81, 42.
        let img = Rgba {
            w: 1,
            h: 1,
            px: vec![200, 100, 50, 200],
        };
        let q = quantize(&img, CANVAS);
        assert_eq!(
            q.palette,
            vec![[159, 81, 42]],
            "a mostly opaque pixel was not painted over the canvas"
        );
        assert_eq!(
            q.idx,
            vec![Some(0)],
            "a mostly opaque pixel was left unpainted"
        );
    }

    #[test]
    fn quantize_keeps_small_palettes_exact() {
        // Three painted colours keep their exact values, in the order they first appeared, and every
        // painted pixel points at its own entry.
        let img = Rgba {
            w: 4,
            h: 1,
            px: vec![
                10, 20, 30, 255, 40, 50, 60, 255, 10, 20, 30, 255, 70, 80, 90, 255,
            ],
        };
        let q = quantize(&img, CANVAS);
        assert_eq!((q.w, q.h), (4, 1));
        assert_eq!(q.palette, vec![[10, 20, 30], [40, 50, 60], [70, 80, 90]]);
        assert_eq!(q.idx, vec![Some(0), Some(1), Some(0), Some(2)]);
        // An alpha-0 pixel paints nothing and takes no palette entry: what shows there is the
        // terminal's own, not a colour in the stream.
        let faded = Rgba {
            w: 2,
            h: 1,
            px: vec![10, 20, 30, 0, 10, 20, 30, 255],
        };
        let q = quantize(&faded, CANVAS);
        assert_eq!(q.palette, vec![[10, 20, 30]]);
        assert_eq!(q.idx, vec![None, Some(0)]);
        // Exactly 256 painted colours is still the exact palette, and an index is its entry's number.
        let mut px = Vec::new();
        for i in 0..=255u8 {
            px.extend([i, 0, 0, 255]);
        }
        let many = Rgba { w: 256, h: 1, px };
        let q = quantize(&many, CANVAS);
        assert_eq!(q.palette.len(), 256);
        assert_eq!(q.palette[255], [255, 0, 0]);
        assert_eq!(q.idx, (0..=255u8).map(Some).collect::<Vec<Option<u8>>>());
    }

    #[test]
    fn quantize_caps_at_256() {
        // The mark at the help screen's twelve rows has far more colours than a palette allows: the
        // anti-aliased edges and the gradient are thousands of them.
        let src = mark();
        let dims =
            logo_dims(12, CellPx { w: 8, h: 16 }, 1, (src.w, src.h)).expect("twelve rows fits");
        let img = render(&src, &dims);
        assert!(
            img.w * img.h > 256,
            "the rendered mark has room for many colours"
        );
        let q = quantize(&img, CANVAS);
        assert_eq!(
            q.palette.len(),
            MAX_COLORS,
            "a crowded mark goes through NeuQuant, which returns the whole palette"
        );
        assert_eq!(
            q.idx.len(),
            usize::try_from(img.w * img.h).expect("a logo fits")
        );
        assert!(
            q.idx
                .iter()
                .all(|entry| entry.is_none_or(|i| usize::from(i) < q.palette.len()))
        );
    }
}
