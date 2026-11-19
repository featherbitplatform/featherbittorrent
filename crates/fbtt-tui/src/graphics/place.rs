//! The bytes the logo writes to the terminal, and when.
//!
//! Three protocols, three ways of keeping the same promise: the image is on the screen, and it stays
//! there while the text beside it moves. Kitty does it from inside the buffer - its placeholder cells
//! are ordinary text that ratatui diffs like anything else, and the pixels travel once per id and size
//! ([`Placer::before_draw`]). iTerm2 and sixel have no placeholders, so the view marks its cells as
//! skipped and the placer writes the image over them after the frame is drawn
//! ([`Placer::after_draw`]) - only when the placement is new, or when a placement that was there before
//! is gone and its cells must be repainted from the buffer itself.
//!
//! Everything here is a byte question, so it is tested without a terminal: a `Placer` given the same
//! placement on three frames answers with bytes only on the first.

use std::sync::Arc;

use ratatui::buffer::CellDiffOption;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Style};

use super::caps::{CellPx, Graphics, Protocol};
use super::image::{self};
pub(crate) use super::image::{LogoDims, Rgba};
use super::iterm2;
use super::kitty;
use super::sixel;
use crate::theme;

/// How tall the help screen's mark stands, tried from tallest first when it fits (decision 12).
pub const LARGE_ROWS: [u16; 3] = [10, 8, 6];

/// Which mark is placed: the header's three rows, or the help screen's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoSize {
    /// The header's mark: three rows tall.
    Small,
    /// The help screen's mark: `rows` tall, the tallest of [`LARGE_ROWS`] that fits.
    Large(u16),
}

/// One prepared mark: its size, the cells it is placed over, and the bytes that put it on the screen.
///
/// The payload is what [`Placer`] writes: the transmit-plus-place for kitty, the inline image for
/// iTerm2, the encoded sixel for sixel. It is built once per cell size and reused until that changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// What this mark is.
    pub size: LogoSize,
    /// The cells it is placed over, and the bitmap it was built at.
    pub dims: LogoDims,
    /// The bytes that put it on the screen, ready to write.
    pub payload: Arc<[u8]>,
}

/// The marks for one terminal: the header's if it sizes up, and the help screen's at each of
/// [`LARGE_ROWS`], tallest first.
///
/// A field that is `None` or empty is a size the terminal cannot draw - its cells are too small for the
/// mark to be more than a smudge - and the view simply keeps the text there.
#[derive(Debug, Clone)]
pub struct PreparedLogos {
    /// The terminal's verdict the marks were prepared for.
    pub graphics: Graphics,
    /// The header's three-row mark, when the terminal's cells can draw one.
    pub small: Option<Prepared>,
    /// The help screen's mark at each of [`LARGE_ROWS`], tallest first, the ones that fit.
    pub large: Vec<Prepared>,
}

impl PreparedLogos {
    /// The prepared mark `placement` draws, or `None` when this terminal does not have one.
    pub fn logo(&self, placement: &Placement) -> Option<&Prepared> {
        match placement.size {
            LogoSize::Small => self.small.as_ref(),
            LogoSize::Large(rows) => self
                .large
                .iter()
                .find(|prepared| prepared.dims.rows == rows),
        }
    }
}

/// The marks for `graphics`, ready to write.
///
/// CPU work: [`image::render`] area-averages a 511 x 853 source into every size, and the quantization an
/// iTerm2 or sixel mark needs walks the result once more. It is called on the blocking pool by the binary,
/// and again when a resize changes the cell size.
///
/// Every size is built against the terminal's own cells: `logo_dims` is asked with scale 1, because a
/// `CellPx` is already the device pixel size of one cell for every protocol. Passing the terminal's scale
/// as well would count it twice and hand the terminal a bitmap twice the size of the cells it is placed
/// over - the blurry mark this module exists to avoid.
#[must_use]
pub fn prepare(graphics: Graphics) -> PreparedLogos {
    let Some(cell) = graphics.cell else {
        // No cell pixel size means no logo (decision 11); `choose` already answered None for it, but a
        // hand-built `Graphics` may not have.
        return PreparedLogos {
            graphics,
            small: None,
            large: Vec::new(),
        };
    };
    let src = image::mark();
    let canvas = rgb(theme::INK_950);

    PreparedLogos {
        graphics,
        small: prepared(LogoSize::Small, 3, graphics, cell, &src, canvas),
        large: LARGE_ROWS
            .iter()
            .copied()
            .filter_map(|rows| prepared(LogoSize::Large(rows), rows, graphics, cell, &src, canvas))
            .collect(),
    }
}

/// One prepared mark for `size` at `rows` of `cell`-sized cells, or `None` when the mark would not size
/// up.
fn prepared(
    size: LogoSize,
    rows: u16,
    graphics: Graphics,
    cell: CellPx,
    src: &Rgba,
    canvas: [u8; 3],
) -> Option<Prepared> {
    let dims = image::logo_dims(rows, cell, 1, (src.w, src.h))?;
    let img = image::render(src, &dims, canvas);
    Some(Prepared {
        size,
        dims,
        payload: Arc::from(payload_for(graphics.protocol, &img, &dims)),
    })
}

/// The image a protocol writes for a prepared mark: the transmit-plus-place for kitty, the inline image
/// for iTerm2, the encoded sixel for sixel.
fn payload_for(protocol: Protocol, img: &Rgba, dims: &LogoDims) -> Vec<u8> {
    match protocol {
        Protocol::None => Vec::new(),
        // The transmit carries the id the placement puts on the screen, and the placement is `U=1`: a
        // virtual one, drawn by the placeholder cells the view writes into the buffer.
        Protocol::Kitty => match dims.rows {
            3 => kitty::transmit(kitty::SMALL_ID, img)
                .into_iter()
                .chain(kitty::place(kitty::SMALL_ID, dims.cols, dims.rows))
                .collect(),
            _ => kitty::transmit(kitty::LARGE_ID, img)
                .into_iter()
                .chain(kitty::place(kitty::LARGE_ID, dims.cols, dims.rows))
                .collect(),
        },
        // The inline image fills the cells it is placed over, and the bitmap is exactly those cells'
        // device pixel size, so `preserveAspectRatio=0` draws it 1:1.
        Protocol::Iterm2 => iterm2::inline(&iterm2::encode_png(img), dims.cols, dims.rows),
        Protocol::Sixel => sixel::encode(&sixel::quantize(img)),
    }
}

/// The ink the theme's ground is, for the canvas a mark is composited over when the token it is
/// given is not an `Rgb` the theme would draw a canvas in. Derived from the theme's own token
/// rather than a hard-coded hex, so the two cannot drift.
const GROUND: [u8; 3] = ground_rgb(theme::INK_950);

const fn ground_rgb(color: Color) -> [u8; 3] {
    match color {
        Color::Rgb(r, g, b) => [r, g, b],
        // Unreachable while the theme keeps its tokens as `Rgb` (this is a `const`, so it cannot
        // call anything cleverer). Black is the fallback: it is not the ground's colour, it is
        // the one value a canvas can take when the theme names no `Rgb` for it.
        _ => [0, 0, 0],
    }
}

/// The RGB of one of the theme's tokens, for the canvas a mark is composited over.
///
/// The theme keeps every token as `Rgb`, so this is a match on the theme's own type: a token that
/// is not `Rgb` is not a ground this palette would draw a canvas in, and rather than invent a
/// colour for it, the canvas falls back to the ink the theme's ground is.
fn rgb(color: Color) -> [u8; 3] {
    match color {
        Color::Rgb(r, g, b) => [r, g, b],
        // A token the theme did not keep as `Rgb` is not a ground this palette draws a canvas in:
        // fall back to the ink the theme's ground is, rather than invent a colour.
        _ => GROUND,
    }
}

/// What the view draws for the marks this frame: the placeholders or skipped cells, and where each one
/// sits so [`Placer`] can write its bytes around them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Which mark this is.
    pub size: LogoSize,
    /// The cells it is placed over.
    pub rect: Rect,
    /// The bitmap the placement carries, so a frame can tell a moved image from a different one.
    pub dims: LogoDims,
    /// The prepared mark this is, so the bytes that put it on the screen come with it: the help
    /// screen's mark has three sizes that share one id, and the one the frame placed is the one
    /// whose pixels the terminal must hold.
    pub prepared: Option<Arc<Prepared>>,
}

impl Placement {
    /// The prepared mark this placement is, or `None` for a placement the view built without one.
    pub fn prepared(&self) -> Option<Arc<Prepared>> {
        self.prepared.clone()
    }
}

/// The marks to draw, or nothing.
///
/// The binary holds the prepared logos behind this: `LogoView::text()` while `prepare` is still running
/// on the blocking pool, the real one when it answers. Everything the view draws is a function of this
/// value, which is what makes `view::draw` and `view::draw_with` one pair of functions rather than two
/// renderers.
#[derive(Debug, Clone, Copy)]
pub struct LogoView<'a> {
    /// The prepared marks to draw, or nothing for the text title.
    pub logos: Option<&'a PreparedLogos>,
}

impl LogoView<'_> {
    /// The text title: no marks at all.
    pub const fn text() -> Self {
        LogoView { logos: None }
    }

    /// The cells of `placement` drawn the way the terminal's protocol needs them drawn, or `None` when
    /// the terminal has no mark for it.
    ///
    /// Kitty's placeholders are ordinary text, so they go through the buffer's cells and ratatui diffs
    /// them like anything else. The foreground is the image's id - the one colour on the screen that is
    /// not a theme token - and the symbol is the mark's character for the cell's row and column inside
    /// the placement. iTerm2 and sixel write their image over the cells the frame just drew, so the frame
    /// marks them as skipped: ratatui never writes a skipped cell, which is what keeps the frame's own
    /// bytes from shoving the image out of its cells.
    pub fn draw_into(
        &self,
        placement: &Placement,
        buffer: &mut ratatui::buffer::Buffer,
    ) -> Option<()> {
        let logos = self.logos?;
        let rect = placement.rect;
        for y in rect.top()..rect.bottom() {
            for x in rect.left()..rect.right() {
                let (row, col) = (y - rect.top(), x - rect.left());
                let cell = &mut buffer[(x, y)];
                match logos.graphics.protocol {
                    Protocol::Kitty => {
                        let id = id_for(placement.size);
                        let symbol = kitty::placeholder(row, col);
                        cell.set_symbol(&symbol).set_style(Style {
                            fg: Some(kitty::id_colour(id)),
                            ..Style::default()
                        });
                    }
                    Protocol::Iterm2 | Protocol::Sixel => {
                        // The cell is the canvas, and it is not written: the image goes over it. The
                        // style is left as the frame drew it - a `Reset` here would ask the terminal
                        // for a colour this palette never named - with only the canvas's ground
                        // named, since the ground is what shows while the image is being sent.
                        cell.set_symbol(" ")
                            .set_bg(theme::INK_950)
                            .set_diff_option(CellDiffOption::Skip);
                    }
                    Protocol::None => {}
                }
            }
        }
        Some(())
    }
}

/// The id a placement's cells carry: the header's mark, or the help screen's.
fn id_for(size: LogoSize) -> u32 {
    match size {
        LogoSize::Small => kitty::SMALL_ID,
        LogoSize::Large(_) => kitty::LARGE_ID,
    }
}

/// What [`Placer::after_draw`] answers with.
///
/// `repaint` are the cells whose previous contents were an image rather than text: ratatui will not
/// write them again, because in its eyes they have not changed, so the image would stay on the screen as
/// a ghost. The binary copies those cells out of the just-drawn buffer and writes them to the backend.
/// `bytes` are the images to write, already positioned, in the order to write them.
#[derive(Debug, Clone, Default)]
pub struct AfterDraw {
    /// The cells to repaint from the drawn buffer, because the image that was on them is gone.
    pub repaint: Vec<Rect>,
    /// The image bytes to write, already positioned around the cursor.
    pub bytes: Vec<u8>,
}

/// The one writer of image bytes: what goes before a frame, what goes after, and what goes on the way
/// out.
///
/// It remembers one fact per frame - which placements were on the screen, on a screen of which size -
/// and nothing else: whether to write is a comparison between that and the placements of the next frame.
/// A changed cell size invalidates it outright, because a new cell size is a new bitmap, and a bitmap is
/// never resent for a placement that was already where it was.
#[derive(Debug)]
pub struct Placer {
    /// The protocol these bytes are for: the rules differ, but the bookkeeping does not.
    protocol: Protocol,
    /// The cell size the `sent` below was sent for: the same pixels under a new cell size are a new
    /// bitmap, so a change here is a change in `dims`.
    cell: Option<CellPx>,
    /// The payload last transmitted under `SMALL_ID`, for this cell size. The header's mark has one
    /// size - three rows - so one slot is the whole question.
    sent_small: Option<Arc<Prepared>>,
    /// The payload last transmitted under `LARGE_ID`, for this cell size. The help screen's mark has
    /// three sizes - ten, eight and six rows - and the one on the screen is whichever the frame
    /// placed, so what was sent last is what the next frame is compared against: a frame that
    /// places the six-row mark sends the six-row pixels, not the ten-row ones it sent before.
    sent_large: Option<Arc<Prepared>>,
    /// The placements the last frame put on the screen: the answer to "is this placement new?".
    last: Vec<Placed>,
    /// The screen `last` was placed on: a different one is a cleared screen for the protocols that write
    /// images at a cursor, and cleared screens do not keep images.
    last_screen: Option<Size>,
}

/// A placement and the image it carried, as the last frame left it on the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placed {
    /// The cells it covered.
    rect: Rect,
    /// The bitmap it was built from, so a different bitmap at the same rect is a new placement.
    dims: LogoDims,
}

impl Placer {
    /// A placer for `protocol`, remembering nothing: its first frame is all new.
    #[must_use]
    pub fn new(protocol: Protocol) -> Self {
        Placer {
            protocol,
            cell: None,
            sent_small: None,
            sent_large: None,
            last: Vec::new(),
            last_screen: None,
        }
    }

    /// The bytes to write before a frame is drawn.
    ///
    /// For kitty this is the transmission: the pixels of every mark this frame draws that the terminal
    /// does not already hold for this cell size, each followed by its placement. The terminal keeps
    /// them under their ids from here on, and the frame's own placeholder cells are what ratatui
    /// writes, so an image is never written once per frame.
    ///
    /// `placements` is what the frame can be expected to draw, which the caller knows only from the
    /// frame before: the placements the last frame put on the screen. The small mark is sent
    /// whatever they are; the help screen's large mark, whose three sizes share one id, is sent
    /// here only when the last frame placed it, so a frame that draws the same help again finds
    /// its pixels in place. A large mark the frame newly chooses (help just opened, or a size
    /// change) is not known until the frame is drawn, and is transmitted by
    /// [`Placer::after_draw`] in the same frame, right after the draw.
    ///
    /// For the other two protocols there is nothing to write before the frame: their image goes out
    /// after it, because it goes where the frame has decided to put it.
    pub fn before_draw(&mut self, logos: &PreparedLogos, placements: &[Placement]) -> Vec<u8> {
        // The cell size is remembered here even for the protocols that write nothing before the
        // frame, because a cell change is what resets `sent` when kitty answers later.
        if logos.graphics.protocol == self.protocol && self.cell != logos.graphics.cell {
            self.cell = logos.graphics.cell;
            self.sent_small = None;
            self.sent_large = None;
        }
        if self.protocol != Protocol::Kitty || logos.graphics.protocol != self.protocol {
            return Vec::new();
        }
        self.transmit(logos, placements)
    }

    /// The pixels a kitty frame owes the terminal: every mark the frame will show that the terminal
    /// does not already hold under its id, in the order the screen will show them - the header's
    /// first, the help screen's after. The small mark is transmitted whenever the terminal can draw
    /// one, because the frame's placeholders for it reference the id before the frame is drawn, and
    /// the placements this frame places are not known until it is drawn: a frame that does not place
    /// the mark this time still needs its pixels held for the next one that does. A new cell size is
    /// a new bitmap under the same id, so the first frame after one transmits everything again.
    fn transmit(&mut self, logos: &PreparedLogos, placements: &[Placement]) -> Vec<u8> {
        let mut out = Vec::new();
        // The header's mark: transmitted whenever the terminal can draw it, so the placeholders the
        // frame is about to write find their pixels already in place.
        let small = logos.small.clone();
        let large = placements
            .iter()
            .find(|placement| matches!(placement.size, LogoSize::Large(_)))
            .and_then(|placement| logos.logo(placement).cloned());
        for prepared in [small, large].into_iter().flatten() {
            let slot = match prepared.size {
                LogoSize::Small => &mut self.sent_small,
                LogoSize::Large(_) => &mut self.sent_large,
            };
            if slot.as_ref().map(|sent| &sent.payload) != Some(&prepared.payload) {
                out.extend_from_slice(&prepared.payload);
                *slot = Some(Arc::new(prepared));
            }
        }
        out
    }

    /// The bytes to write after a frame is drawn, and the cells the frame no longer covers.
    ///
    /// For kitty this is the pixels of a mark [`Placer::before_draw`] could not know about: the help
    /// screen's large mark when this frame newly places it or places another size than the last
    /// frame did. The placeholder cells the frame wrote are the placement, and the terminal draws
    /// them as soon as it holds the pixels, so the transmit goes out in the same flush. Pixels
    /// already sent are not sent again, so a steady frame writes nothing here.
    ///
    /// For iTerm2 and sixel, an image is written for every placement that is new - a different rect, a
    /// different image, or a screen of a different size, which is the one ratatui cleared - and the
    /// placements that were there before and are not now come back as `repaint`: the image that was on
    /// those cells must be taken off them by writing the cells back, because ratatui will not write them
    /// again. The bytes are wrapped in a cursor save and restore, positioned at the placement's top-left,
    /// so nothing the frame just drew is moved.
    pub fn after_draw(
        &mut self,
        placements: &[Placement],
        screen: Size,
        logos: &PreparedLogos,
    ) -> AfterDraw {
        if logos.graphics.protocol != self.protocol || self.protocol == Protocol::Kitty {
            // Kitty keeps no placement bookkeeping: its placeholders are ordinary cells. What it
            // may still owe is the pixels of a large mark this frame has just chosen. The frame
            // still tells the placer what the screen is now, so a next frame of a different
            // protocol knows the screen changed.
            self.last_screen = Some(screen);
            self.last.clear();
            let bytes = if logos.graphics.protocol == self.protocol {
                self.transmit(logos, placements)
            } else {
                Vec::new()
            };
            return AfterDraw {
                repaint: Vec::new(),
                bytes,
            };
        }

        // A screen of a different size is a cleared screen: ratatui has already redrawn every cell of
        // it, so no cells are owed a repaint from the just-drawn buffer.
        let screen_changed = self.last_screen != Some(screen);

        // The previous placements that are gone now: their cells still hold their image on the screen, so
        // they are repainted from the drawn buffer instead of left as ghosts. Each rect is clipped to the
        // screen it is painted on, because a rect from a larger screen is not a rect of this one, and the
        // cells beyond this screen's edge are not cells the drawn buffer holds.
        let mut repaint = Vec::new();
        if !screen_changed {
            let screen_rect = Rect::new(0, 0, screen.width, screen.height);
            for placed in &self.last {
                if !placements
                    .iter()
                    .any(|p| p.rect == placed.rect && p.dims == placed.dims)
                {
                    let clipped = placed.rect.intersection(screen_rect);
                    if !clipped.is_empty() {
                        repaint.push(clipped);
                    }
                }
            }
        }

        let mut bytes = Vec::new();
        for placement in placements {
            // A placement is new when its rect, its image or the screen itself has changed since the last
            // frame: any of the three has put the pixels somewhere the terminal does not have them.
            let new = self.last_screen != Some(screen)
                || !self
                    .last
                    .iter()
                    .any(|placed| placed.rect == placement.rect && placed.dims == placement.dims);
            if new {
                bytes.extend_from_slice(&wrapped_at(placement.rect, logos, placement));
            }
        }

        self.last = placements
            .iter()
            .map(|p| Placed {
                rect: p.rect,
                dims: p.dims,
            })
            .collect();
        self.last_screen = Some(screen);
        AfterDraw { repaint, bytes }
    }

    /// A change the frame cannot see: a new cell size.
    ///
    /// The bitmap built for the old cells is not the bitmap the new cells want, so everything sent is
    /// forgotten; the next frame sends what it draws again. The placements are forgotten too, for the
    /// same reason, which is also what repaints a screen the terminal cleared while it was resizing.
    pub fn invalidate(&mut self) {
        self.sent_small = None;
        self.sent_large = None;
        self.last.clear();
        self.last_screen = None;
    }

    /// The bytes to write before the terminal is put back.
    ///
    /// Kitty's images live in the terminal after this process is gone, and the placeholder cells this
    /// run drew will not be drawn again, so the ids are deleted on the way out - both of them, because
    /// which one was shown is not worth remembering for the exit line. The other protocols write nothing
    /// on exit: their images are the cells the screen is about to leave, and those go with it.
    #[must_use]
    pub fn on_exit(&self) -> Vec<u8> {
        if self.protocol != Protocol::Kitty {
            return Vec::new();
        }
        let mut out = Vec::new();
        out.extend_from_slice(&kitty::delete(kitty::SMALL_ID));
        out.extend_from_slice(&kitty::delete(kitty::LARGE_ID));
        out
    }
}

/// The image bytes to write at `rect`'s top-left, wrapped so the cursor is where it was when this
/// process began the frame and where the terminal puts it back.
///
/// The cursor goes to `(x, y)` - the placement's top-left, one-based - the image is written, and the
/// cursor is restored. Nothing else moves, which is what keeps a placement from shoving the text the
/// frame just drew out of its cells.
fn wrapped_at(rect: Rect, logos: &PreparedLogos, placement: &Placement) -> Vec<u8> {
    let Some(payload) = logos
        .logo(placement)
        .map(|prepared| prepared.payload.as_ref())
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(payload.len() + 32);
    out.extend_from_slice(b"\x1b7");
    out.extend_from_slice(format!("\x1b[{};{}H", rect.y + 1, rect.x + 1).as_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\x1b8");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal with a protocol, cells of `cell`, scale 1.
    fn graphics(protocol: Protocol, cell: CellPx) -> Graphics {
        Graphics {
            protocol,
            cell: Some(cell),
            scale: 1,
        }
    }

    /// The header's placement over a 100 x 30 screen: the mark's cols at the left of the three header
    /// rows, which is where the view draws it.
    fn small_placement(logos: &PreparedLogos) -> Placement {
        let small = logos
            .small
            .as_ref()
            .expect("this terminal can draw the header's mark")
            .clone();
        Placement {
            size: LogoSize::Small,
            rect: Rect::new(0, 0, small.dims.cols, 3),
            dims: small.dims,
            prepared: Some(Arc::new(small)),
        }
    }

    /// The screen the placements above sit on.
    const SCREEN: Size = Size::new(100, 30);

    /// Sixel, 8 x 16 cells: the case where an image is written over skipped cells.
    fn sixel_logos() -> PreparedLogos {
        prepare(graphics(Protocol::Sixel, CellPx { w: 8, h: 16 }))
    }

    /// The same terminal with its cells doubled: a new bitmap for every mark it has.
    fn sixel_logos_big() -> PreparedLogos {
        prepare(graphics(Protocol::Sixel, CellPx { w: 16, h: 34 }))
    }

    fn iterm2_logos() -> PreparedLogos {
        prepare(graphics(Protocol::Iterm2, CellPx { w: 16, h: 34 }))
    }

    fn kitty_logos() -> PreparedLogos {
        prepare(graphics(Protocol::Kitty, CellPx { w: 8, h: 16 }))
    }

    /// The iTerm2 case: a terminal that reports its cells in points, at its own pixels-per-point.
    fn iterm2_scaled() -> Graphics {
        Graphics {
            protocol: Protocol::Iterm2,
            // 8.5 x 17 points at scale 2: the device pixels the terminal draws, which `choose`
            // has already multiplied in before it hands the verdict over.
            cell: Some(CellPx { w: 17, h: 34 }),
            scale: 2,
        }
    }

    /// A `PreparedLogos` whose `Protocol::None` terminal draws no marks at all: the text title's case.
    fn none_logos() -> PreparedLogos {
        // A `None` terminal is one that never proved a protocol: `Graphics::none()` has no cell size
        // either, which is what `choose` returns for it (decision 11).
        prepare(Graphics::none())
    }

    #[test]
    fn iterm2_logo_is_device_pixels() {
        // The terminal reports its cells in points and its own pixels-per-point: 17 points at scale 2 is
        // the 34 device pixels its cells draw, which `choose` has already multiplied in. The scale is
        // 2 on the verdict - a scale-1 terminal would not catch a `prepare` that multiplied it in again.
        let logos = prepare(iterm2_scaled());
        let Some(cell) = logos.graphics.cell else {
            panic!("the cell size is there");
        };
        assert_eq!(
            logos.graphics.scale, 2,
            "the terminal's own scale, which `choose` has applied"
        );
        let small = logos.small.as_ref().expect("the header's mark");
        // The bitmap is the cells it is placed over, in device pixels: cols by the cell's width, rows by
        // its height. Passing the scale into `logo_dims` as well would double these.
        assert_eq!(
            small.dims.px_w,
            u32::from(small.dims.cols) * u32::from(cell.w),
            "the bitmap's width is its columns in device pixels"
        );
        assert_eq!(
            small.dims.px_h,
            u32::from(small.dims.rows) * u32::from(cell.h),
            "the bitmap's height is its rows in device pixels"
        );
        assert_eq!(
            cell,
            CellPx { w: 17, h: 34 },
            "the cells the logo is built for"
        );
        // And a help mark: the same rule at ten rows.
        let large = logos.large.first().expect("ten rows fits");
        assert_eq!(
            large.dims.px_w,
            u32::from(large.dims.cols) * u32::from(cell.w)
        );
        assert_eq!(
            large.dims.px_h,
            u32::from(large.dims.rows) * u32::from(cell.h)
        );
        // The regression the scale-1 verdict could not catch: passing the scale into `logo_dims` as
        // well would count it twice and build a bitmap wider than its cells - which this terminal
        // would rescale, and which is the blurry mark the whole module exists to avoid.
        let src = image::mark();
        let doubled = image::logo_dims(3, cell, logos.graphics.scale, (src.w, src.h));
        let doubled = doubled.expect("the doubled size still fits a header");
        assert_ne!(
            small.dims.px_w, doubled.px_w,
            "a scale-2 cell doubled by the scale again is a different width, so the prepared size \
             must not have been made that way"
        );
    }

    #[test]
    fn prepare_is_empty_for_none() {
        let logos = none_logos();
        assert!(logos.small.is_none());
        assert!(logos.large.is_empty());
    }

    #[test]
    fn prepare_sizes_up_for_an_image_terminal() {
        let logos = sixel_logos();
        let small = logos.small.as_ref().expect("the header's mark sizes up");
        // 8 x 16 cells, three rows: 48 pixels tall, and the mark's ratio decides the width.
        assert_eq!(small.dims.rows, 3);
        assert_eq!(small.dims.px_h, 48);
        assert!(small.dims.cols > 0 && small.dims.cols <= 24);
        // Ten, eight and six rows all fit a screen this tall.
        assert_eq!(
            logos.large.iter().map(|p| p.dims.rows).collect::<Vec<_>>(),
            vec![10, 8, 6]
        );
    }

    #[test]
    fn placer_sends_once() {
        let logos = sixel_logos();
        let mut placer = Placer::new(Protocol::Sixel);
        let mut frame = || {
            let placement = small_placement(&logos);
            placer.after_draw(std::slice::from_ref(&placement), SCREEN, &logos)
        };

        let first = frame();
        assert!(!first.bytes.is_empty(), "the first frame writes the image");
        let second = frame();
        assert!(
            second.bytes.is_empty(),
            "the same placement on the second frame writes nothing"
        );
        let third = frame();
        assert!(
            third.bytes.is_empty(),
            "the same placement on the third frame writes nothing"
        );
        assert!(first.repaint.is_empty());
        assert!(second.repaint.is_empty());
        assert!(third.repaint.is_empty());
    }

    #[test]
    fn placer_resends_after_resize() {
        let small = sixel_logos();
        let big = sixel_logos_big();
        let mut placer = Placer::new(Protocol::Sixel);
        let first = small_placement(&small);
        let answered = placer.after_draw(std::slice::from_ref(&first), SCREEN, &small);
        assert!(
            !answered.bytes.is_empty(),
            "the first frame writes the image"
        );

        // A resize with a new cell size: the terminal cleared the screen, and the bitmap this cell size
        // wants is a new one.
        placer.invalidate();
        let placement = small_placement(&big);
        let resized = placer.after_draw(std::slice::from_ref(&placement), SCREEN, &big);
        assert!(
            !resized.bytes.is_empty(),
            "a new cell size writes the image again"
        );
    }

    #[test]
    fn placer_resends_when_rect_moves() {
        let logos = sixel_logos();
        let mut placer = Placer::new(Protocol::Sixel);
        let first = small_placement(&logos);
        let answered = placer.after_draw(std::slice::from_ref(&first), SCREEN, &logos);
        assert!(!answered.bytes.is_empty());

        // The same image at a different rect: the pixels are somewhere else, so they go again.
        let moved = Placement {
            size: LogoSize::Small,
            rect: Rect::new(2, 0, first.rect.width, first.rect.height),
            dims: first.dims,
            prepared: first.prepared.clone(),
        };
        let moved_answered = placer.after_draw(std::slice::from_ref(&moved), SCREEN, &logos);
        assert!(
            !moved_answered.bytes.is_empty(),
            "a moved placement writes the image again"
        );
    }

    #[test]
    fn placer_repaints_removed() {
        let logos = sixel_logos();
        let mut placer = Placer::new(Protocol::Sixel);
        let first = small_placement(&logos);
        let answered = placer.after_draw(std::slice::from_ref(&first), SCREEN, &logos);
        assert!(!answered.bytes.is_empty(), "the first frame placed it");

        // The placement is gone - a dialog opened over it, the help screen came down - and the image is
        // still on the cells it was placed over. Those cells are the repaint: the frame just drawn has
        // the text that belongs there, and ratatui will not write it again on its own.
        let removed = placer.after_draw(&[], SCREEN, &logos);
        assert_eq!(removed.repaint, vec![first.rect]);
        assert!(
            removed.bytes.is_empty(),
            "nothing is placed this frame, so nothing is written"
        );
    }

    /// The help screen's placement at `rows` of a 100-column box: the widest bitmap the mark has,
    /// centred, is where the view draws it.
    fn large_placement(logos: &PreparedLogos, rows: u16) -> Placement {
        let prepared = logos
            .large
            .iter()
            .find(|prepared| prepared.dims.rows == rows)
            .expect("the help mark at these rows")
            .clone();
        let x = 100u16.saturating_sub(prepared.dims.cols) / 2;
        Placement {
            size: LogoSize::Large(rows),
            rect: Rect::new(x, 4, prepared.dims.cols, rows),
            dims: prepared.dims,
            prepared: Some(Arc::new(prepared)),
        }
    }

    #[test]
    fn kitty_transmits_before_first_frame_and_after_cell_change() {
        let logos = kitty_logos();
        let mut placer = Placer::new(Protocol::Kitty);

        // The first frame transmits the mark it draws: the header's.
        let small = small_placement(&logos);
        let first = placer.before_draw(&logos, std::slice::from_ref(&small));
        assert!(!first.is_empty(), "the first frame transmits");
        let second = placer.before_draw(&logos, std::slice::from_ref(&small));
        assert!(
            second.is_empty(),
            "the second frame transmits nothing: the pixels are already there"
        );

        // A cell change is a new bitmap under the same id.
        let big = prepare(graphics(Protocol::Kitty, CellPx { w: 16, h: 34 }));
        let moved = small_placement(&big);
        let resized = placer.before_draw(&big, std::slice::from_ref(&moved));
        assert!(
            !resized.is_empty(),
            "a new cell size transmits the marks again"
        );
        assert!(
            placer
                .before_draw(&big, std::slice::from_ref(&moved))
                .is_empty(),
            "and only once"
        );
    }

    #[test]
    fn kitty_resends_the_large_mark_when_its_size_changes() {
        let logos = kitty_logos();
        let mut placer = Placer::new(Protocol::Kitty);
        // The small mark's payload, transmitted first: the header's mark is transmitted whenever
        // the terminal can draw it, before the help screen's large mark.
        let small_payload = logos
            .small
            .as_ref()
            .expect("this terminal draws the header's mark")
            .payload
            .clone();

        // A frame with the help screen open at the ten-row mark: the small mark and the ten-row
        // mark go out.
        let ten = large_placement(&logos, 10);
        let first = placer.before_draw(&logos, std::slice::from_ref(&ten));
        assert!(!first.is_empty(), "the frame transmits");
        let ten_expected = logos
            .large
            .iter()
            .find(|prepared| prepared.dims.rows == 10)
            .expect("the ten-row bitmap")
            .payload
            .clone();
        assert!(
            first.len() >= small_payload.len() + ten_expected.len(),
            "the transmit holds both the small and the ten-row marks"
        );
        assert_eq!(
            &first[small_payload.len()..][..ten_expected.len()],
            &ten_expected[..],
            "the ten-row pixels, not another size's"
        );
        // The same frame again: nothing.
        let again = placer.before_draw(&logos, std::slice::from_ref(&ten));
        assert!(again.is_empty(), "the same size does not resend");

        // The terminal shrank, and the help screen's mark is the six-row one now. The id is the
        // same, but the bitmap under it must be the six-row one - a terminal that kept the ten-row
        // pixels would crop them to six rows.
        let six = large_placement(&logos, 6);
        let changed = placer.before_draw(&logos, std::slice::from_ref(&six));
        let six_expected = logos
            .large
            .iter()
            .find(|prepared| prepared.dims.rows == 6)
            .expect("the six-row bitmap")
            .payload
            .clone();
        // The small mark is already sent, so only the six-row mark goes out.
        assert_eq!(
            &changed[..],
            &six_expected[..],
            "a different size of the same mark transmits that size's pixels"
        );
        let again = placer.before_draw(&logos, std::slice::from_ref(&six));
        assert!(again.is_empty(), "and only once, for the size it is now");
    }

    #[test]
    fn kitty_on_exit_deletes_both_ids() {
        let mut placer = Placer::new(Protocol::Kitty);
        let logos = kitty_logos();
        let small = small_placement(&logos);
        placer.before_draw(&logos, std::slice::from_ref(&small));
        let exit = placer.on_exit();
        // Both ids, small first: which one the screen showed is not the exit's question.
        assert_eq!(
            exit,
            kitty::delete(kitty::SMALL_ID)
                .into_iter()
                .chain(kitty::delete(kitty::LARGE_ID))
                .collect::<Vec<_>>()
        );

        // The other protocols have nothing to delete: their images are the cells the screen leaves.
        assert!(Placer::new(Protocol::Sixel).on_exit().is_empty());
        assert!(Placer::new(Protocol::Iterm2).on_exit().is_empty());
        assert!(Placer::new(Protocol::None).on_exit().is_empty());
    }

    #[test]
    fn a_shrunk_screen_needs_no_repaint_outside_it() {
        let logos = sixel_logos();
        let mut placer = Placer::new(Protocol::Sixel);

        // A frame on the big screen: the header's mark at the top-left.
        let first = small_placement(&logos);
        let answered = placer.after_draw(std::slice::from_ref(&first), SCREEN, &logos);
        assert!(
            !answered.bytes.is_empty(),
            "the first frame writes the image"
        );

        // The terminal shrank to 60 x 20: the screen is cleared, the buffer is 60 x 20, and the
        // mark's old rect (cols x 3 at 0, 0) still sits inside it, so nothing is owed a repaint -
        // ratatui has already redrawn every cell of the smaller screen. The placement is new on the
        // new screen, so the image goes out once more.
        let shrunk = Size::new(60, 20);
        let placement = small_placement(&logos);
        let shrunken = placer.after_draw(std::slice::from_ref(&placement), shrunk, &logos);
        assert!(
            shrunken.repaint.is_empty(),
            "a cleared screen owes no repaint: {shrunken:?}"
        );
        assert!(
            !shrunken.bytes.is_empty(),
            "the mark is placed on the new screen, so the image goes out for it"
        );

        // The other half of the rule: a placement that was on this screen and is gone now, with
        // the screen still the same, is repainted from the drawn buffer.
        let mut placer = Placer::new(Protocol::Sixel);
        let first = small_placement(&logos);
        let _ = placer.after_draw(std::slice::from_ref(&first), SCREEN, &logos);
        let removed = placer.after_draw(&[], SCREEN, &logos);
        assert_eq!(removed.repaint, vec![first.rect]);
    }

    #[test]
    fn a_shrunk_screen_does_not_panic_on_a_removed_placement() {
        // The old bug, directly: a placement from a 100 x 42 frame is gone on a 60 x 20 frame,
        // and the cells beyond 60 x 20 are not cells the drawn buffer holds.
        let logos = sixel_logos();
        let mut placer = Placer::new(Protocol::Sixel);
        let first = Placement {
            size: LogoSize::Small,
            rect: Rect::new(0, 0, 80, 3),
            dims: logos.small.as_ref().expect("the header's mark").dims,
            prepared: None,
        };
        let big = Size::new(100, 42);
        let _ = placer.after_draw(std::slice::from_ref(&first), big, &logos);
        let shrunk = Size::new(60, 20);
        let removed = placer.after_draw(&[], shrunk, &logos);
        // The screen changed, so no repaint at all: ratatui has redrawn every cell of it, and no
        // rect from the old screen is indexed into the new one's buffer.
        assert!(removed.repaint.is_empty());
        assert!(removed.bytes.is_empty());
    }
    #[test]
    fn iterm2_bytes_are_wrapped_in_cursor_save() {
        let logos = iterm2_logos();
        let mut placer = Placer::new(Protocol::Iterm2);
        let first = small_placement(&logos);
        let answered = placer.after_draw(std::slice::from_ref(&first), SCREEN, &logos);
        assert!(
            !answered.bytes.is_empty(),
            "the first frame writes the image"
        );

        let bytes = &answered.bytes;
        assert!(
            bytes.starts_with(b"\x1b7\x1b[1;1H"),
            "the cursor is saved and the image starts at the placement's top-left"
        );
        assert!(
            bytes.ends_with(b"\x1b8"),
            "the cursor is restored after the image"
        );
    }

    #[test]
    fn before_draw_is_empty_for_the_after_draw_protocols() {
        // iTerm2 and sixel write nothing before the frame: their image goes out after it.
        let mut placer = Placer::new(Protocol::Sixel);
        let logos = sixel_logos();
        let small = small_placement(&logos);
        assert!(
            placer
                .before_draw(&logos, std::slice::from_ref(&small))
                .is_empty()
        );
        let mut placer = Placer::new(Protocol::Iterm2);
        let logos = iterm2_logos();
        let small = small_placement(&logos);
        assert!(
            placer
                .before_draw(&logos, std::slice::from_ref(&small))
                .is_empty()
        );
    }

    #[test]
    fn draw_into_marks_cells_the_way_the_protocol_wants() {
        use ratatui::buffer::Buffer;

        let logos = sixel_logos();
        let view = LogoView {
            logos: Some(&logos),
        };
        let placement = small_placement(&logos);
        let mut buffer = Buffer::empty(placement.rect);
        view.draw_into(&placement, &mut buffer)
            .expect("the mark draws");
        assert!(
            buffer[(0, 0)].diff_option == CellDiffOption::Skip,
            "an image protocol skips its cells, so the frame never writes over the image"
        );

        // The text title draws nothing at all.
        let mut buffer = Buffer::empty(placement.rect);
        assert!(
            LogoView::text()
                .draw_into(&placement, &mut buffer)
                .is_none()
        );
    }
}
