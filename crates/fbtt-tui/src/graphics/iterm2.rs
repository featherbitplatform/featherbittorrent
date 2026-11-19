//! iTerm2's inline images: one OSC 1337 string carrying a whole PNG.
//!
//! Unlike kitty, this protocol has no placeholders - the image is written into the screen at the
//! cursor - so the placer (Task 9) owns it completely and sends it once per placement. The payload is
//! a PNG rather than raw RGBA because that is what the protocol takes, and `preserveAspectRatio=0`
//! with `width` and `height` in cells says "fill exactly this many cells", which combined with a
//! bitmap built at those cells' device pixel size means no rescaling.
//!
//! The arguments are assembled here rather than interpolated straight into the payload: a 195 KiB
//! base64 string is not something to format twice while it is being copied.

use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;

use super::image::Rgba;

/// `BEL`: the terminator iTerm2 accepts for an OSC, and the one this client always writes.
const BEL: u8 = 0x07;

/// The image `png` inline, filling `cols` columns and `rows` rows, ending at the cursor line so the
/// text drawn beside it is not pushed around.
///
/// `size` is the byte count of the payload, which iTerm2 uses for its progress indicator; an empty
/// PNG is a payload of nothing, which draws nothing.
#[must_use]
pub fn inline(png: &[u8], cols: u16, rows: u16) -> Vec<u8> {
    let payload = BASE64_STANDARD.encode(png);
    // The arguments are one short string and the payload is the only large thing, so this copies the
    // bytes once each rather than formatting them together.
    let head = format!(
        "\x1b]1337;File=inline=1;size={};width={cols};height={rows};preserveAspectRatio=0;\
         doNotMoveCursor=1:",
        png.len()
    );
    let mut out = Vec::with_capacity(head.len() + payload.len() + 1);
    out.extend(head.as_bytes());
    out.extend(payload.as_bytes());
    out.push(BEL);
    out
}

/// `img` as a PNG (RGBA8, `png`'s own default compression).
///
/// An encoder that refuses - a zero-sized image, an error from a `Vec` that cannot fail - comes back
/// as no bytes at all, which the caller treats as "no logo" rather than as a partial image.
#[must_use]
pub fn encode_png(img: &Rgba) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, img.w, img.h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // `finish` is what writes the IEND chunk and flushes: a stream without it is a truncated PNG, and
    // iTerm2 would be waiting for the rest of it.
    let written = encoder.write_header().and_then(|mut writer| {
        writer.write_image_data(&img.px)?;
        writer.finish()
    });
    match written {
        Ok(()) => out,
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test-only PNG reader: the payload has to decode back to the pixels that went in.
    fn decode_png(png: &[u8]) -> Option<Rgba> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().ok()?;
        let size = reader.output_buffer_size()?;
        let mut buf = vec![0; size];
        let info = reader.next_frame(&mut buf).ok()?;
        if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
            return None;
        }
        Some(Rgba {
            w: info.width,
            h: info.height,
            px: buf[..info.buffer_size()].to_vec(),
        })
    }

    #[test]
    fn iterm2_bytes() {
        let img = Rgba {
            w: 3,
            h: 2,
            px: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255, 0, 0, 0, 255,
                255, 0, 255, 255,
            ],
        };
        let png_bytes = encode_png(&img);
        assert!(!png_bytes.is_empty(), "the image encodes");

        let bytes = inline(&png_bytes, 4, 3);
        let prefix = format!(
            "\x1b]1337;File=inline=1;size={};width=4;height=3;preserveAspectRatio=0;\
             doNotMoveCursor=1:",
            png_bytes.len()
        );
        assert!(
            bytes.starts_with(prefix.as_bytes()),
            "the arguments come before the payload: {}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(
            *bytes.last().expect("a sequence is not empty"),
            0x07,
            "BEL ends it"
        );

        // The payload is the PNG itself in base64, between the last `:` and the `BEL`.
        let text = String::from_utf8(bytes.clone()).expect("the sequence is ASCII plus base64");
        let colon = text
            .find(':')
            .expect("':' separates the arguments from the payload");
        let decoded = BASE64_STANDARD
            .decode(&text[colon + 1..text.len() - 1])
            .expect("the payload is base64");
        assert_eq!(decoded, png_bytes);

        // And that PNG is the image that went in, pixel for pixel.
        let back = decode_png(&decoded).expect("the payload decodes as an RGBA8 PNG");
        assert_eq!((back.w, back.h), (3, 2));
        assert_eq!(back.px, img.px);
    }
}
