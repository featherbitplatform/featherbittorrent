//! The mark, and the exact pixel size it is drawn at.
//!
//! The logo is only worth drawing if the terminal draws it 1:1, so this file answers two
//! questions and nothing else: how big should the image be for `rows` cells of this terminal's
//! cells ([`logo_dims`]), and what are those pixels ([`render`]). Both are pure arithmetic over
//! whole numbers - no floats, because a size worked out in floating point can land on the wrong
//! side of a cell boundary and every pixel of a crisp logo depends on that.
//!
//! [`mark`] decodes the PNG the crate ships. A build whose asset is broken is a test failure; at
//! run time it degrades to a 1 x 1 transparent image, which [`logo_dims`] refuses to size up, so
//! the worst a bad asset can do is leave the text title in place.

use super::caps::CellPx;

/// The shipped mark: `assets/featherbit-mark.png`, 511 x 853 RGBA.
const MARK_PNG: &[u8] = include_bytes!("../../assets/featherbit-mark.png");

/// An image in the form every encoder here wants: straight (not premultiplied) RGBA8, row major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
    /// `w * h * 4` bytes: red, green, blue and alpha for each pixel, rows in order.
    pub px: Vec<u8>,
}

/// The size the logo is drawn at for one terminal.
///
/// The image is `img_w x img_h` pixels, placed over `cols x rows` cells whose device pixel size is
/// `cell.w * scale` by `cell.h * scale`. The prepared bitmap is the whole cell area (`px_w x
/// px_h`) with the image centred on it, because a bitmap narrower than its placement is scaled by
/// the terminal and that is exactly what this module exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogoDims {
    /// Columns the logo is placed over.
    pub cols: u16,
    /// Rows the logo is placed over.
    pub rows: u16,
    /// `cols * cell.w * scale`: the width of the bitmap handed to the terminal.
    pub px_w: u32,
    /// `rows * cell.h * scale`: the height of the bitmap handed to the terminal.
    pub px_h: u32,
    /// The width of the mark inside that bitmap.
    pub img_w: u32,
    /// The height of the mark inside that bitmap.
    pub img_h: u32,
}

/// The mark, decoded to RGBA8.
#[must_use]
pub fn mark() -> Rgba {
    match decode_mark() {
        Some(img) => img,
        // A build whose asset does not decode has nothing to draw; 1 x 1 is small enough that
        // `logo_dims` refuses it, so the header keeps its text title instead of a broken image.
        None => Rgba {
            w: 1,
            h: 1,
            px: vec![0, 0, 0, 0],
        },
    }
}

/// The shipped PNG decoded to RGBA8, or `None` when it does not decode that way.
fn decode_mark() -> Option<Rgba> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(MARK_PNG));
    // `EXPAND` is what turns a palette, grayscale or interlaced source into plain 8-bit samples with
    // alpha, which is the one form the encoders here read.
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    // The mark this crate ships is 8-bit RGBA. Anything else would need a conversion nobody asked
    // for, so it counts as "this build has no logo" rather than as an image with the wrong samples.
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    Some(Rgba {
        w: info.width,
        h: info.height,
        px: buf[..info.buffer_size()].to_vec(),
    })
}

/// How big the logo should be for `rows` cells of `cell`-sized cells at `scale`, scaling the mark's
/// own `(src.0, src.1)` aspect ratio to fit.
///
/// `cell` is device pixels and `scale` is the factor that turns a cell into device pixels, so both are
/// only interesting together for a caller whose cells are in some other unit. A [`CellPx`] from
/// [`crate::graphics::caps::choose`] has already been converted - including iTerm2's points times
/// its own pixels-per-point factor - so pass `1` with it; passing
/// [`crate::graphics::caps::Graphics::scale`] here as well would count that factor twice and build
/// a bitmap larger than the cells it is placed over.
///
/// The height is the cell area's height, capped at the source's own height so a small terminal gets
/// a downsampled mark rather than an upscaled blurry one; the width follows the source's ratio. The
/// columns are whatever whole cells that width needs, and the bitmap then fills those cells exactly.
///
/// `None` when the mark would be too small to read (`img_w < 4`), wider than 24 columns or taller
/// than 24 rows - past that it is a splash screen rather than a header mark (decision 12) - and when
/// any input is zero, which sizes nothing.
#[must_use]
pub fn logo_dims(rows: u16, cell: CellPx, scale: u16, src: (u32, u32)) -> Option<LogoDims> {
    let (src_w, src_h) = src;
    // A logo past 24 rows or columns is a splash screen rather than a header mark (decision 12), and
    // a zero anywhere means there is nothing to size.
    if rows > 24 || src_w == 0 || src_h == 0 || cell.w == 0 || cell.h == 0 || scale == 0 {
        return None;
    }
    let cell_w = u32::from(cell.w).checked_mul(u32::from(scale))?;
    let cell_h = u32::from(cell.h).checked_mul(u32::from(scale))?;
    // The height is the whole cell area, capped at the source's own height: a shorter row count gets
    // a downsampled mark, never an upscaled one.
    let px_h = u32::from(rows).checked_mul(cell_h)?;
    let img_h = px_h.min(src_h);
    // The width follows the source's ratio, rounded to the nearest pixel rather than floored, so a
    // mark that is three quarters of a pixel wider than a whole number of cells does not lose that
    // column.
    let img_w = round_div(u64::from(img_h) * u64::from(src_w), u64::from(src_h))?;
    if img_w < 4 {
        return None;
    }
    // Whole cells, rounded up: the bitmap then fills them exactly instead of leaving a gap the
    // terminal would have to stretch the mark across.
    let cols =
        u32::try_from((u64::from(img_w) + u64::from(cell_w) - 1) / u64::from(cell_w)).ok()?;
    if cols > 24 {
        return None;
    }
    Some(LogoDims {
        cols: u16::try_from(cols).ok()?,
        rows,
        px_w: cols.checked_mul(cell_w)?,
        px_h,
        img_w,
        img_h,
    })
}

/// `n / d` rounded to the nearest whole number, or `None` when it does not fit a `u32`.
fn round_div(n: u64, d: u64) -> Option<u32> {
    // Twice the numerator plus one denominator, over twice the denominator: an exact half rounds up.
    // `d` came from a `u32`, so `d * 2` cannot overflow a `u64`.
    u32::try_from((n.checked_mul(2)?.checked_add(d)?) / (d * 2)).ok()
}

/// The pixels to hand a terminal: the mark area-averaged down to `img_w x img_h`, composited over
/// `canvas` (the theme's ink), centred on a `px_w x px_h` field of that same colour.
///
/// Downscaling averages whole source pixels by the exact fraction of each one the destination pixel
/// covers, in premultiplied alpha so a half-transparent edge darkens its neighbour the way it should
/// rather than ringing. The output is opaque: the terminal draws this on the canvas colour it already
/// has, and an alpha channel here would only invite whatever that terminal does with one.
#[must_use]
pub fn render(src: &Rgba, dims: &LogoDims, canvas: [u8; 3]) -> Rgba {
    let (px_w, px_h) = (dims.px_w, dims.px_h);
    let Some(len) = usize::try_from(u64::from(px_w) * u64::from(px_h) * 4).ok() else {
        return Rgba {
            w: px_w,
            h: px_h,
            px: Vec::new(),
        };
    };
    // The field first, entirely in the canvas colour: that is what reaches the edge beside and below
    // the mark once it is centred, and it is what makes every output pixel opaque.
    let mut px = vec![0u8; len];
    for pixel in px.as_chunks_mut::<4>().0 {
        pixel[0] = canvas[0];
        pixel[1] = canvas[1];
        pixel[2] = canvas[2];
        pixel[3] = 255;
    }

    // A `LogoDims` from `logo_dims` never exceeds its own cell area. Clamping costs nothing and keeps
    // a hand-built one - or an empty source - from drawing past the edge of the field.
    let img_w = dims.img_w.min(px_w);
    let img_h = dims.img_h.min(px_h);
    if img_w == 0 || img_h == 0 {
        return Rgba {
            w: px_w,
            h: px_h,
            px,
        };
    }
    let small = area_average(src, img_w, img_h);

    // Centred, with an odd leftover pixel going to the right and bottom rather than split in half.
    let off_x = usize::try_from((px_w - img_w) / 2).unwrap_or(0);
    let off_y = usize::try_from((px_h - img_h) / 2).unwrap_or(0);
    // A size that does not fit a `usize` cannot be indexed at all, and then it is not drawn.
    let (cols, rows) = (
        usize::try_from(img_w).unwrap_or(0),
        usize::try_from(img_h).unwrap_or(0),
    );
    let field_w = usize::try_from(px_w).unwrap_or(0);
    for y in 0..rows {
        // `area_average` returns exactly four bytes for each of these pixels.
        let from = y * cols * 4;
        let into_row = ((off_y + y) * field_w + off_x) * 4;
        for x in 0..cols {
            let at = from + x * 4;
            let into = into_row + x * 4;
            let alpha = u32::from(small[at + 3]);
            px[into] = composite(small[at], alpha, canvas[0]);
            px[into + 1] = composite(small[at + 1], alpha, canvas[1]);
            px[into + 2] = composite(small[at + 2], alpha, canvas[2]);
        }
    }
    Rgba {
        w: px_w,
        h: px_h,
        px,
    }
}

/// One channel of a pixel with `alpha` over an opaque `over`: the source's share plus the canvas's
/// share, rounded half up. With `alpha` 255 this returns the source channel unchanged, which is what
/// keeps an already-opaque mark exactly as it was; each product is at most 255 * 255 and their sum at
/// most twice that, so a `u32` holds it.
fn composite(c: u8, alpha: u32, over: u8) -> u8 {
    let mixed = u32::from(c) * alpha + u32::from(over) * (255 - alpha);
    // 255 is odd, so adding 127 before dividing rounds exactly half up.
    u8::try_from((mixed + 127) / 255).unwrap_or(u8::MAX)
}

/// `src` averaged to `w x h` by exact fractional coverage, in premultiplied alpha, returned as
/// straight RGBA.
fn area_average(src: &Rgba, w: u32, h: u32) -> Vec<u8> {
    let Some(out_len) = usize::try_from(u64::from(w) * u64::from(h) * 4).ok() else {
        return Vec::new();
    };
    let mut out = vec![0u8; out_len];
    let (sw, sh) = (src.w, src.h);
    let Some(src_len) = usize::try_from(u64::from(sw) * u64::from(sh) * 4).ok() else {
        return out;
    };
    // An image whose byte count disagrees with its dimensions has no pixels to average, and averaging
    // nothing is what keeps this from reading past the end of `px`.
    if w == 0 || h == 0 || sw == 0 || sh == 0 || src.px.len() != src_len {
        return out;
    }

    for dy in 0..h {
        // This output row's footprint in the source, with the denominator multiplied through so that
        // every coverage fraction below is a whole number: rows `[dy * sh / h, (dy + 1) * sh / h)`.
        let y0 = u64::from(dy) * u64::from(sh);
        let y1 = u64::from(dy + 1) * u64::from(sh);
        let row_from = span_start(y0, h);
        let row_to = span_end(y1, h, sh);
        let out_row = usize::try_from(dy).unwrap_or(0) * usize::try_from(w).unwrap_or(0);

        for dx in 0..w {
            let x0 = u64::from(dx) * u64::from(sw);
            let x1 = u64::from(dx + 1) * u64::from(sw);
            let col_from = span_start(x0, w);
            let col_to = span_end(x1, w, sw);

            // Premultiplied sums over the covered source pixels, each weighted by how much of it this
            // output pixel covers. A coverage is at most one source pixel's area in denominator units,
            // and the coverages of one output pixel sum to no more than the whole source, so every
            // term and every sum fits a `u64` for any image whose size fits a `u32`.
            let mut acc_r = 0u64;
            let mut acc_g = 0u64;
            let mut acc_b = 0u64;
            let mut acc_a = 0u64;
            let mut coverage = 0u64;

            for sy in row_from..row_to {
                let top = u64::from(sy) * u64::from(h);
                let bottom = u64::from(sy + 1) * u64::from(h);
                let cov_y = y1.min(bottom) - y0.max(top);
                for sx in col_from..col_to {
                    let left = u64::from(sx) * u64::from(w);
                    let right = u64::from(sx + 1) * u64::from(w);
                    let cov_x = x1.min(right) - x0.max(left);
                    let cov = cov_y * cov_x;
                    let at = (usize::try_from(u64::from(sy) * u64::from(sw) + u64::from(sx))
                        .unwrap_or(0))
                        * 4;
                    let alpha = u64::from(src.px[at + 3]);
                    acc_r += u64::from(src.px[at]) * alpha * cov;
                    acc_g += u64::from(src.px[at + 1]) * alpha * cov;
                    acc_b += u64::from(src.px[at + 2]) * alpha * cov;
                    acc_a += alpha * cov;
                    coverage += cov;
                }
            }

            let to = (out_row + usize::try_from(dx).unwrap_or(0)) * 4;
            // Un-premultiply by the averaged alpha: a colour weighted by how opaque it was, divided by
            // how much opacity there was. A fully transparent footprint has no colour to report.
            out[to] = straight(acc_r, acc_a);
            out[to + 1] = straight(acc_g, acc_a);
            out[to + 2] = straight(acc_b, acc_a);
            out[to + 3] = clamp_channel(div_rounded(acc_a, coverage));
        }
    }
    out
}

/// The first source row or column a footprint that starts at `n / d` overlaps.
fn span_start(n: u64, d: u32) -> u32 {
    u32::try_from(n / u64::from(d)).unwrap_or(u32::MAX)
}

/// One past the last source row or column a footprint that ends at `n / d` overlaps, clamped to what
/// the image has. Rounding up is what makes a footprint that stops partway into a row still cover it.
fn span_end(n: u64, d: u32, bound: u32) -> u32 {
    u32::try_from((n + u64::from(d) - 1) / u64::from(d))
        .unwrap_or(u32::MAX)
        .min(bound)
}

/// The straight colour a premultiplied sum stands for: the mean of `c * a` over the mean of `a`,
/// which is what averaging in premultiplied alpha means. No opacity means no colour to report.
fn straight(acc_ca: u64, acc_a: u64) -> u8 {
    if acc_a == 0 {
        return 0;
    }
    clamp_channel(div_rounded(acc_ca, acc_a))
}

/// `n / d` rounded to the nearest whole number, or 0 for a denominator of 0.
fn div_rounded(n: u64, d: u64) -> u64 {
    if d == 0 {
        return 0;
    }
    (n + d / 2) / d
}

/// A value clamped to what one channel holds.
fn clamp_channel(v: u64) -> u8 {
    u8::try_from(v).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canvas the logo is composited over: `theme::INK_950`, `#0b0b0e`.
    const CANVAS: [u8; 3] = [0x0b, 0x0b, 0x0e];

    /// A source with varying colour and varying alpha, so a render that forgets one of them shows.
    fn swatch(w: u32, h: u32) -> Rgba {
        let mut px = Vec::with_capacity(usize::try_from(w * h * 4).expect("a test swatch fits"));
        for y in 0..h {
            for x in 0..w {
                px.push(((x * 7 + y * 13) % 256) as u8);
                px.push(((x * 31 + y * 17) % 256) as u8);
                px.push(((x + y * 5) % 256) as u8);
                px.push(((x * 60 + y * 90) % 256) as u8);
            }
        }
        Rgba { w, h, px }
    }

    #[test]
    fn logo_pixels_match_cells() {
        let cell = CellPx { w: 8, h: 16 };
        // The header's three rows in a terminal with 8 x 16 cells and no scale.
        let dims = logo_dims(3, cell, 1, (511, 853)).expect("three rows fits");
        assert_eq!((dims.img_h, dims.img_w), (48, 29));
        assert_eq!((dims.cols, dims.px_w, dims.px_h), (4, 32, 48));
        // Ten rows in a terminal whose cells are twice that big in device pixels: the mark fills what
        // it is placed over instead of being scaled by the terminal.
        let big = logo_dims(10, cell, 2, (511, 853)).expect("ten rows fits");
        assert_eq!((big.px_h, big.img_h, big.img_w), (320, 320, 192));
        assert_eq!((big.cols, big.px_w), (12, 192));
        // More rows than a header mark is worth (decision 12 refuses this), columns past 24 for the
        // same reason, and a mark too small to place at all.
        assert!(logo_dims(25, cell, 1, (511, 853)).is_none());
        assert!(logo_dims(24, CellPx { w: 2, h: 4 }, 1, (511, 853)).is_none());
        assert!(logo_dims(1, CellPx { w: 2, h: 4 }, 1, (511, 853)).is_none());
        // Nothing to scale from, and nothing to scale by.
        assert!(logo_dims(3, cell, 1, (0, 0)).is_none());
        assert!(logo_dims(3, CellPx { w: 0, h: 0 }, 1, (511, 853)).is_none());
        assert!(logo_dims(3, cell, 0, (511, 853)).is_none());
    }

    #[test]
    fn render_is_exact_size_and_opaque() {
        // 30 x 120 into three rows of 8 x 16 cells: the mark is 12 x 48 inside a 16 x 48 field, so it
        // is centred with two canvas columns either side.
        let src = swatch(30, 120);
        let dims =
            logo_dims(3, CellPx { w: 8, h: 16 }, 1, (src.w, src.h)).expect("three rows fits");
        assert_eq!((dims.img_w, dims.px_w), (12, 16));
        let img = render(&src, &dims, CANVAS);
        assert_eq!((img.w, img.h), (dims.px_w, dims.px_h));
        assert_eq!(
            img.px.len(),
            usize::try_from(dims.px_w * dims.px_h * 4).expect("a logo fits")
        );
        for pixel in img.px.as_chunks::<4>().0 {
            assert_eq!(pixel[3], 255, "the composite is opaque");
        }
        // The centring: the canvas reaches the edge of the field beside the mark.
        assert_eq!(&img.px[..4], &[0x0b, 0x0b, 0x0e, 255]);
    }

    #[test]
    fn area_average() {
        // A 2 x 2 checkerboard of black and white is 128 grey when averaged to one pixel: the mean of
        // two 0s and two 255s, rounded half up.
        let board = Rgba {
            w: 2,
            h: 2,
            px: vec![
                0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
            ],
        };
        assert_eq!(super::area_average(&board, 1, 1), vec![128, 128, 128, 255]);
        // A uniform colour averaged to any size is that same colour everywhere, which is what keeps a
        // downsampled flat edge flat instead of gaining a darker outline.
        let violet = [134u8, 59, 255, 255];
        let flat = Rgba {
            w: 4,
            h: 4,
            px: [violet; 16].concat(),
        };
        assert_eq!(
            super::area_average(&flat, 2, 2),
            vec![
                134, 59, 255, 255, 134, 59, 255, 255, 134, 59, 255, 255, 134, 59, 255, 255
            ]
        );
    }

    #[test]
    fn mark_decodes() {
        let src = decode_mark().expect("the shipped mark decodes");
        assert_eq!((src.w, src.h), (511, 853));
        assert_eq!(src.px.len(), 511 * 853 * 4);
        // `mark` hands out the same pixels; only a build whose asset is broken gets the stand-in.
        let via_mark = mark();
        assert_eq!((via_mark.w, via_mark.h), (511, 853));
        assert_eq!(via_mark.px, src.px);
    }
}
