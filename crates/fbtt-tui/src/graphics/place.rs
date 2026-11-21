//! The bytes the logo writes to the terminal, and when.
//!
//! Three protocols, three ways of keeping the same promise: the image is on the screen, and it stays
//! there while the text beside it moves. Kitty does it from inside the buffer - its placeholder cells
//! are ordinary text that ratatui diffs like anything else, and the pixels travel once per id and size
//! ([`Placer::before_draw`]). iTerm2 and sixel have no placeholders, so the view marks its cells as
//! skipped and the placer writes the image over them after the frame is drawn
//! ([`Placer::after_draw`]) - only when the placement is new, and then over the frame's own cells
//! written there first, or when a placement that was there before is gone and its cells must be
//! repainted from the buffer itself.
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
    // Sixel has no alpha: `quantize` turns each pixel's alpha into a colour composited over the
    // canvas it is given, so a mark's anti-aliased edges are the colour of the ground it was prepared
    // for. Each mark is prepared against the ground it is actually drawn on. The header's mark stands
    // on the canvas every frame fills the screen with (`INK_950`). The help screen's mark stands
    // inside the help box, which is filled with its own surface, `INK_850` (`modal::draw_box`), and
    // one canvas for both would carry the wrong ground's colour in one of their edges.
    let header_ground = rgb(theme::INK_950);
    let help_ground = rgb(theme::INK_850);

    PreparedLogos {
        graphics,
        small: prepared(LogoSize::Small, 3, graphics, cell, &src, header_ground),
        large: LARGE_ROWS
            .iter()
            .copied()
            .filter_map(|rows| {
                prepared(
                    LogoSize::Large(rows),
                    rows,
                    graphics,
                    cell,
                    &src,
                    help_ground,
                )
            })
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
    let img = image::render(src, &dims);
    Some(Prepared {
        size,
        dims,
        payload: Arc::from(payload_for(graphics.protocol, &img, &dims, canvas)),
    })
}

/// The image a protocol writes for a prepared mark: the transmit-plus-place for kitty, the inline
/// image for iTerm2, the encoded sixel for sixel, painted against `canvas`.
fn payload_for(protocol: Protocol, img: &Rgba, dims: &LogoDims, canvas: [u8; 3]) -> Vec<u8> {
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
        Protocol::Sixel => sixel::encode(&sixel::quantize(img, canvas)),
    }
}

/// The ink the theme's ground is, for the canvas sixel paints against when the token it is given is
/// not an `Rgb` the theme would draw a canvas in. Derived from the theme's own token rather than a
/// hard-coded hex, so the two cannot drift.
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

/// The RGB of one of the theme's tokens, for the canvas sixel paints against.
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
                        // The cell is the ground the frame drew, and it is not written: the image goes
                        // over it. Its style is left exactly as the frame left it - the header's
                        // canvas, the help box's own surface - because the image is transparent and
                        // whatever is on the cell shows around the mark. Naming a ground here would
                        // paint a rectangle of it wherever a mark stands, and a `Reset` would ask the
                        // terminal for a colour this palette never named. That the cells hold the text
                        // of the frame before this one, rather than this frame's blank, is the placer's
                        // question, not the cell's: a new placement's rect is a repaint rect (see
                        // [`Placer::after_draw`]).
                        cell.set_symbol(" ").set_diff_option(CellDiffOption::Skip);
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
/// `repaint` are the cells ratatui's own diff leaves alone and the terminal must not be left holding
/// what it last saw there: the cells of a placement that is gone now, which still carry the image that
/// was on them, and the cells of a placement this frame puts on the screen for the first time under an
/// image written over skipped cells, where whatever the frame before drew is still on the terminal and
/// shows through a transparent image. The binary copies those cells out of the just-drawn buffer and
/// writes them to the backend before the image bytes, so an image lands on the frame's own cells and
/// never on the text under them.
/// `bytes` are the images to write, already positioned, in the order to write them.
#[derive(Debug, Clone, Default)]
pub struct AfterDraw {
    /// The cells to repaint from the drawn buffer, because ratatui does not write them: the ones a
    /// removed image left behind, and the ones a new image is about to go over.
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
    /// The terminal cleared its image cache since the last frame: a logos swap after a cell-size
    /// change (a resize never does this - the bitmaps it has sent are still the right size for a
    /// screen whose cells did not move). The previous frame's large size is no longer the one this
    /// frame draws, so it is not pre-sent before the draw - the size the frame picks is sent once,
    /// by the step that knows it.
    cleared: bool,
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
            cleared: false,
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
    /// whatever they are, because they are the previous frame's, and this frame's own placeholders
    /// for the small mark reference the id before the frame is drawn. The `Saving...` screen never
    /// calls this: it draws only its centred line, so it owes no pixels before the draw. The help
    /// screen's large mark, whose three
    /// sizes share one id, is sent here at the size the last frame placed it, when the terminal
    /// has not cleared its cache: a large mark the frame newly chooses (help just opened, or a
    /// different size after a zoom) is not known until the frame is drawn, and is transmitted by
    /// [`Placer::after_draw`] in the same frame, right after the draw. After a clear the previous
    /// frame's large size is no longer the one drawn, so it is not pre-sent here: pre-sending it
    /// would put two bitmaps of the same id out in one frame, and the second one - the one the
    /// placeholders reference - would win.
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
        // The small mark goes out whatever the placements are: they are the previous frame's, and
        // this frame's placeholders for the small mark reference its id before the frame is drawn.
        // The large mark is pre-sent only when the terminal has not cleared its cache: after a
        // clear, the previous frame's large size is no longer the one this frame draws, so it is
        // not pre-sent - the size the frame picks is sent once, after the draw, by the step that
        // knows it. The flag is cleared here, so the next frame pre-sends the large mark again.
        let large = !self.cleared;
        self.cleared = false;
        self.transmit(logos, placements, true, large)
    }

    /// The pixels a kitty frame owes the terminal: every mark the frame will show that the terminal
    /// does not already hold under its id, in the order the screen will show them - the header's
    /// first, the help screen's after. A new cell size is a new bitmap under the same id, so the
    /// first frame after one transmits everything again.
    ///
    /// `small` and `large` name the marks this frame owes pixels for, and only those. The small
    /// mark is sent when `small` is true, whatever the placements are: they are the previous
    /// frame's, so this frame's placeholders for the small mark reference the id before the frame
    /// is drawn. The `Saving...` screen never calls [`Placer::before_draw`]. The large mark is
    /// sent when `large` is true, at the size of the large placement among `placements` - the
    /// previous frame's size, when this is a [`Placer::before_draw`], or this frame's own, when it
    /// is a [`Placer::after_draw`]. After the draw the small mark is owed to nobody (its frame
    /// paid for it before the draw, or places it not at all), and a size the frame already sent in
    /// this frame is not sent a second time (see [`Placer::after_draw`]).
    fn transmit(
        &mut self,
        logos: &PreparedLogos,
        placements: &[Placement],
        small: bool,
        large: bool,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let small = small.then(|| logos.small.clone()).flatten();
        let large = large
            .then(|| {
                placements
                    .iter()
                    .find(|placement| matches!(placement.size, LogoSize::Large(_)))
                    .and_then(|placement| logos.logo(placement).cloned())
            })
            .flatten();
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
    /// The small mark is never sent here: its frame paid for it before the draw. A size the frame
    /// already sent in this frame's [`Placer::before_draw`] is not a new choice either: a zoom with
    /// the help open changes the large mark's size while a fresh bitmap is in flight, and the frame
    /// that picks the new size is the one that sends it, once. After a clear, the previous frame's
    /// large size was not pre-sent, so the size this frame picks is the large mark's only transmit
    /// in the frame, and it goes out here.
    ///
    /// For iTerm2 and sixel, an image is written for every placement that is new - a different rect, a
    /// different image, or a screen of a different size, which is the one ratatui cleared - and that
    /// placement's cells come back as `repaint` with it: ratatui never writes a skipped cell, not even
    /// on a screen it just cleared, so the image would otherwise land on whatever the frame before drew
    /// there and let that text show through it. The placements that were there before and are not now
    /// come back as `repaint` too: the image that was on those cells must be taken off them by writing
    /// the cells back, because ratatui will not write them again. The binary writes the repaint cells
    /// out of the drawn buffer before the image bytes, so an image always lands on this frame's cells.
    /// The bytes are wrapped in a cursor save and restore, positioned at the placement's top-left, so
    /// nothing the frame just drew is moved.
    pub fn after_draw(
        &mut self,
        placements: &[Placement],
        screen: Size,
        logos: &PreparedLogos,
    ) -> AfterDraw {
        if logos.graphics.protocol != self.protocol || self.protocol == Protocol::Kitty {
            // Kitty keeps no placement bookkeeping: its placeholders are ordinary cells. What it
            // may still owe is the pixels of a large mark this frame has just chosen - not the
            // small one, whose frame either placed it (and paid for it before the draw) or owes
            // none at all. The frame still tells the placer what the screen is now, so a next
            // frame of a different protocol knows the screen changed.
            self.last_screen = Some(screen);
            self.last.clear();
            let bytes = if logos.graphics.protocol == self.protocol {
                // The large mark, when the frame placed one: a size the frame just chose is owed
                // its pixels once, here; the payload slot marks it sent, so a frame that already
                // transmitted this size in its [`Placer::before_draw`] sends nothing.
                self.transmit(logos, placements, false, true)
            } else {
                Vec::new()
            };
            return AfterDraw {
                repaint: Vec::new(),
                bytes,
            };
        }

        // A screen of a different size is a cleared screen: ratatui has already redrawn every cell it
        // writes itself, so none of those are owed a repaint from the just-drawn buffer. The cells it
        // skips are owed one either way, which is what the new placements below add.
        let screen_changed = self.last_screen != Some(screen);
        // Every rect below is painted on this screen, and a rect from a larger screen is not a rect of
        // it: the cells beyond this screen's edge are not cells the drawn buffer holds.
        let screen_rect = Rect::new(0, 0, screen.width, screen.height);

        // The previous placements that are gone now: their cells still hold their image on the screen, so
        // they are repainted from the drawn buffer instead of left as ghosts. Each rect is clipped to the
        // screen it is painted on, because a rect from a larger screen is not a rect of this one, and the
        // cells beyond this screen's edge are not cells the drawn buffer holds.
        let mut repaint = Vec::new();
        if !screen_changed {
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

        // An image that goes over skipped cells is put where the cursor is set, and ratatui never wrote
        // those cells on this frame - not even on a screen it just cleared - so they still hold what the
        // frame before left there: at startup, the text title, showing through the transparent mark.
        // A new placement's cells are repaint cells too, so the binary writes this frame's cells there
        // first and the image on top of them. Kitty is not in this group at all (it returns above): its
        // placeholders are ordinary cells, which the diff writes.
        let image_over_skipped_cells = matches!(self.protocol, Protocol::Sixel | Protocol::Iterm2);
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
                if image_over_skipped_cells {
                    let clipped = placement.rect.intersection(screen_rect);
                    if !clipped.is_empty() {
                        repaint.push(clipped);
                    }
                }
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
        self.cleared = true;
    }

    /// The marks this placer still remembers as sent, the way a frame can see them: the payloads
    /// held under each id, by payload identity, and the large mark's rows. It exists for tests: a
    /// production caller asks for bytes, not for the bookkeeping behind them.
    #[doc(hidden)]
    pub fn sent(&self) -> (Option<&[u8]>, Option<u16>) {
        (
            self.sent_small
                .as_ref()
                .map(|prepared| prepared.payload.as_ref()),
            self.sent_large.as_ref().map(|prepared| prepared.dims.rows),
        )
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

    /// The two payloads `prepared`'s bitmap could have been written from: the same pixels quantized
    /// against each ground, in the order `prepare` builds them.
    fn sixel_payloads_over_each_ground(src: &Rgba, prepared: &Prepared) -> (Vec<u8>, Vec<u8>) {
        let img = image::render(src, &prepared.dims);
        (
            sixel::encode(&sixel::quantize(&img, rgb(theme::INK_950))),
            sixel::encode(&sixel::quantize(&img, rgb(theme::INK_850))),
        )
    }

    /// Sixel carries no alpha, so `quantize` paints a pixel the colour `composite` makes of it over
    /// the canvas at that pixel's own alpha: a mark's anti-aliased edges are the colour of the ground
    /// it was prepared for. The header's mark stands on the canvas every frame fills the screen with
    /// and every help mark inside the help box, so each is prepared against the ground it sits on.
    ///
    /// How this observes the ground: the bitmap each size was painted from is rebuilt and encoded
    /// once against each ground, and the two encodings are compared with the bytes the terminal gets.
    /// For this mark the two are never the same, and a size without a semi-transparent pixel fails
    /// that guard instead of passing quietly, so a payload that is one ground's bytes is that
    /// ground's choice and not an accident.
    #[test]
    fn each_mark_is_painted_over_the_ground_it_sits_on() {
        let src = image::mark();
        let logos = prepare(graphics(Protocol::Sixel, CellPx { w: 8, h: 16 }));

        let small = logos.small.as_ref().expect("the header's mark");
        let (on_canvas, on_box) = sixel_payloads_over_each_ground(&src, small);
        assert!(
            on_canvas != on_box,
            "the header's mark has a pixel whose colour the ground decides, or this test could not \
             tell the two grounds apart"
        );
        assert_eq!(
            &small.payload[..],
            &on_canvas[..],
            "the header's mark is not painted over the canvas it stands on"
        );
        assert_ne!(
            &small.payload[..],
            &on_box[..],
            "and it is not painted over the help box's ground"
        );

        for large in &logos.large {
            let (on_canvas, on_box) = sixel_payloads_over_each_ground(&src, large);
            assert!(
                on_canvas != on_box,
                "the help mark at {} rows has a pixel whose colour the ground decides, or this \
                 test could not tell the two grounds apart",
                large.dims.rows
            );
            assert_eq!(
                &large.payload[..],
                &on_box[..],
                "the help mark at {} rows is not painted over the help box's ground",
                large.dims.rows
            );
            assert_ne!(
                &large.payload[..],
                &on_canvas[..],
                "and the help mark at {} rows is not painted over the canvas",
                large.dims.rows
            );
        }
    }

    #[test]
    fn placer_sends_once() {
        let logos = sixel_logos();
        let placement = small_placement(&logos);
        let mut placer = Placer::new(Protocol::Sixel);
        let mut frame = || placer.after_draw(std::slice::from_ref(&placement), SCREEN, &logos);

        let first = frame();
        assert!(!first.bytes.is_empty(), "the first frame writes the image");
        assert_eq!(
            first.repaint,
            vec![placement.rect],
            "the frame that places the mark repaints the cells its image goes over"
        );
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
        assert!(
            second.repaint.is_empty(),
            "a placement already on the screen owes no cells a second time"
        );
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

    /// The startup case, in order: the frames before the logos answer draw the text title, and the
    /// frame that first places the mark puts its image over cells ratatui never writes. Whatever the
    /// text frames left on them is still on the terminal, and a transparent mark lets it show through.
    /// The frame's own cells have to be written there first, then the image on top.
    #[test]
    fn a_new_sixel_placement_repaints_its_cells_first() {
        let logos = sixel_logos();
        let mut placer = Placer::new(Protocol::Sixel);

        // The text title's frame: no placement, so nothing to write and nothing to repaint.
        let text = placer.after_draw(&[], SCREEN, &logos);
        assert!(
            text.bytes.is_empty() && text.repaint.is_empty(),
            "a frame that places nothing writes nothing: {text:?}"
        );

        // The logos answer on the next frame: the mark goes over cells this frame marked skipped, and
        // the frame before it drew `featherbittorrent` and the rates on them.
        let placement = small_placement(&logos);
        let drawn = placer.after_draw(std::slice::from_ref(&placement), SCREEN, &logos);
        assert_eq!(
            drawn.repaint,
            vec![placement.rect],
            "a new sixel placement did not repaint the cells under it"
        );
        assert!(
            !drawn.bytes.is_empty(),
            "the new placement still writes its image"
        );

        // The next frame is the same one: neither half of the answer goes out twice.
        let steady = placer.after_draw(std::slice::from_ref(&placement), SCREEN, &logos);
        assert!(
            steady.repaint.is_empty() && steady.bytes.is_empty(),
            "a mark already on the screen is written once, not twice: repaint {:?}, {} bytes",
            steady.repaint,
            steady.bytes.len()
        );
    }

    /// The same rule for the other protocol that has no placeholder: iTerm2 writes its image over the
    /// cells the frame skipped, so a new placement owes those cells a repaint before the image goes on
    /// them.
    #[test]
    fn a_new_iterm2_placement_repaints_its_cells_first() {
        let logos = iterm2_logos();
        let mut placer = Placer::new(Protocol::Iterm2);

        // The text title's frame: no placement, so nothing to write and nothing to repaint.
        let text = placer.after_draw(&[], SCREEN, &logos);
        assert!(
            text.bytes.is_empty() && text.repaint.is_empty(),
            "a frame that places nothing writes nothing: {text:?}"
        );

        // The logos answer on the next frame, over the same cells the text frames filled.
        let placement = small_placement(&logos);
        let drawn = placer.after_draw(std::slice::from_ref(&placement), SCREEN, &logos);
        assert_eq!(
            drawn.repaint,
            vec![placement.rect],
            "a new iterm2 placement did not repaint the cells under it"
        );
        assert!(
            !drawn.bytes.is_empty(),
            "the new placement still writes its image"
        );

        // The next frame is the same one: neither half of the answer goes out twice.
        let steady = placer.after_draw(std::slice::from_ref(&placement), SCREEN, &logos);
        assert!(
            steady.repaint.is_empty() && steady.bytes.is_empty(),
            "a mark already on the screen is written once, not twice: repaint {:?}, {} bytes",
            steady.repaint,
            steady.bytes.len()
        );
    }

    /// The same question on a screen the frame has not seen before, which is the case the skipped
    /// cells are never let off: a screen of another size is a cleared screen, and ratatui has redrawn
    /// every cell of it that it writes itself, but these are cells it skips. Guarding the new
    /// placement's cells with "did the screen change" would leave the old text under the mark here,
    /// and no other test would notice.
    #[test]
    fn a_new_sixel_placement_on_a_new_screen_repaints_its_cells() {
        let logos = sixel_logos();
        let placement = small_placement(&logos);
        let mut placer = Placer::new(Protocol::Sixel);

        // The mark on this screen, so the frame after it has something to be compared against. Only
        // its image is asserted here, so the failure below names the changed screen and nothing else.
        let placed = placer.after_draw(std::slice::from_ref(&placement), SCREEN, &logos);
        assert!(
            !placed.bytes.is_empty(),
            "the frame that places the mark writes its image"
        );

        // The terminal was resized to 60 x 20 and cleared, and the mark is placed on the new screen.
        // Its cells are inside this screen, so they are owed whole.
        let other = Size::new(60, 20);
        let resized = placer.after_draw(std::slice::from_ref(&placement), other, &logos);
        assert_eq!(
            resized.repaint,
            vec![
                placement
                    .rect
                    .intersection(Rect::new(0, 0, other.width, other.height))
            ],
            "a new placement on a changed screen did not repaint its cells: {:?}",
            resized.repaint
        );
        assert!(
            !resized.bytes.is_empty(),
            "and the mark is written on the new screen too"
        );

        // The other way a placement becomes new on a screen this size: a logos swap, which clears the
        // terminal's image cache and what the placer remembers of the screen. Same answer owed.
        placer.invalidate();
        let swapped = placer.after_draw(std::slice::from_ref(&placement), SCREEN, &logos);
        assert_eq!(
            swapped.repaint,
            vec![placement.rect],
            "a new placement on a screen the placer forgot did not repaint its cells: {:?}",
            swapped.repaint
        );
        assert!(
            !swapped.bytes.is_empty(),
            "and the mark is written after that clear too"
        );
    }

    /// Kitty keeps its image inside the buffer: its placeholder cells are ordinary cells the diff
    /// writes like any other text, so a new kitty placement owes no cells a repaint at all. The rects
    /// this frame's skipped cells need are the image protocols' question alone.
    #[test]
    fn a_kitty_placement_needs_no_repaint() {
        let logos = kitty_logos();
        let mut placer = Placer::new(Protocol::Kitty);
        let text = placer.after_draw(&[], SCREEN, &logos);
        assert!(
            text.repaint.is_empty(),
            "a frame that places nothing repaints nothing: {text:?}"
        );

        let placement = small_placement(&logos);
        let drawn = placer.after_draw(std::slice::from_ref(&placement), SCREEN, &logos);
        assert!(
            !drawn.repaint.contains(&placement.rect),
            "a new kitty placement's cells are written by the diff, not owed a repaint: {:?}",
            drawn.repaint
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

        // The terminal shrank to 60 x 20: the screen is cleared, the buffer is 60 x 20, and no cell
        // ratatui writes itself is owed a repaint - it has already redrawn every one of them. The mark's
        // cells are owed one even here, because ratatui skips those and never writes them, and they are
        // clipped to the screen they are painted on: the mark stands on columns 0..80 of the frame it was
        // placed on, and columns past 60 are not cells the drawn buffer holds. The image goes out too,
        // because this frame places the mark on a screen that has not had it.
        let shrunk = Size::new(60, 20);
        let wide = Placement {
            size: LogoSize::Small,
            rect: Rect::new(0, 0, 80, 3),
            dims: first.dims,
            prepared: first.prepared.clone(),
        };
        let shrunken = placer.after_draw(std::slice::from_ref(&wide), shrunk, &logos);
        assert_eq!(
            shrunken.repaint,
            vec![Rect::new(0, 0, 60, 3)],
            "the mark's cells are repainted, clipped to the screen they are painted on: {:?}",
            shrunken.repaint
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

    /// The help screen's mark stands inside the help box, on the surface the box is filled with, and
    /// its cells keep it. Forcing the canvas colour under a transparent image paints a darker
    /// rectangle in the middle of the box, and in a terminal whose own background is that colour, a
    /// hole in the box.
    #[test]
    fn the_help_mark_keeps_the_help_box_ground() {
        use ratatui::buffer::Buffer;

        let logos = sixel_logos();
        let view = LogoView {
            logos: Some(&logos),
        };
        let placement = large_placement(&logos, 10);

        // The help box's surface, cell for cell, the way `modal::draw_box` fills the box the mark
        // stands at the top of.
        let mut buffer = Buffer::empty(Rect::new(0, 0, SCREEN.width, SCREEN.height));
        for y in 0..SCREEN.height {
            for x in 0..SCREEN.width {
                buffer[(x, y)].set_bg(theme::INK_850);
            }
        }

        view.draw_into(&placement, &mut buffer)
            .expect("the help mark draws");
        for y in placement.rect.top()..placement.rect.bottom() {
            for x in placement.rect.left()..placement.rect.right() {
                let cell = &buffer[(x, y)];
                assert_eq!(
                    cell.bg,
                    theme::INK_850,
                    "the help mark forced the canvas colour under it at column {x}, row {y}"
                );
                assert_eq!(
                    cell.symbol(),
                    " ",
                    "the mark's own cells stay blank: the image goes over them"
                );
            }
        }
    }

    /// A `Saving...` frame is a frame with no placements at all: it writes no kitty bytes.
    /// A frame that places nothing cannot owe pixels to the terminal, after the draw or
    /// before it, and a frame that places the small mark pays for it before the draw, not
    /// after.
    #[test]
    fn the_saving_frame_writes_no_kitty_bytes() {
        let logos = kitty_logos();
        let mut placer = Placer::new(Protocol::Kitty);
        let small = small_placement(&logos);
        // The small mark's id, carried by the transmit's own opening: how one image's bytes
        // are told from another's.
        let small_id = format!("\x1b_Ga=t,q=2,f=32,i={}", kitty::SMALL_ID);
        let owes_small = |bytes: &[u8]| {
            bytes
                .windows(small_id.len())
                .any(|w| w == small_id.as_bytes())
        };

        // A frame that places the header's mark transmits it before the draw: the
        // placeholders the frame is about to write reference its id.
        let first = placer.before_draw(&logos, std::slice::from_ref(&small));
        assert!(
            !first.is_empty(),
            "the first frame transmits the mark it places"
        );
        assert!(
            owes_small(&first),
            "the first frame transmits the small mark, under its own id"
        );

        // The `Saving...` screen: no placements at all. Nothing is owed, before or after
        // the draw, because the small mark was already sent by the first frame.
        let saving = placer.before_draw(&logos, &[]);
        assert!(
            saving.is_empty(),
            "a frame with no placements writes no pixels before the draw"
        );
        let after = placer.after_draw(&[], SCREEN, &logos);
        assert!(
            after.bytes.is_empty(),
            "a frame with no placements writes no pixels after the draw"
        );
        assert!(after.repaint.is_empty(), "kitty repaints no cells");

        // And a frame that places the small mark pays for it before the draw, not after:
        // the first frame transmitted it, so this one only writes its placeholders.
        let steady = placer.before_draw(&logos, std::slice::from_ref(&small));
        assert!(
            steady.is_empty(),
            "a frame that places the mark finds its pixels already held, and sends nothing"
        );
        let steady_after = placer.after_draw(std::slice::from_ref(&small), SCREEN, &logos);
        assert!(
            steady_after.bytes.is_empty(),
            "the small mark is never sent after the draw"
        );

        // The terminal clears its image cache: a logos swap after a cell-size change. A
        // resize never does this - the bitmaps it has sent are still the right size for a
        // screen whose cells did not move. The placer forgets what it sent, and the next
        // frame's after-draw must not owe the small mark either: the small mark is paid
        // for before the draw, by the frame's own placeholders, never after.
        placer.invalidate();
        let drawn_after = placer.after_draw(std::slice::from_ref(&small), SCREEN, &logos);
        assert!(
            drawn_after.bytes.is_empty(),
            "the small mark is never sent after the draw, even after a clear"
        );
        assert!(
            !owes_small(&drawn_after.bytes),
            "and no small-mark id in the bytes after a clear either"
        );

        // The `Saving...` screen after a clear: no placements at all. The after-draw owes no
        // pixels, because no placeholders reference an id and the small mark is never sent after
        // the draw.
        placer.invalidate();
        let saving_after = placer.after_draw(&[], SCREEN, &logos);
        assert!(
            saving_after.bytes.is_empty(),
            "the saving frame after a clear writes no pixels after the draw"
        );
        assert!(
            !owes_small(&saving_after.bytes),
            "and no small-mark id in the saving frame's after-draw bytes either"
        );
    }

    /// A zoom with the help open, in the frame that answers it: the terminal holds the large
    /// mark's pixels once and only once. The fresh bitmap goes out in the same frame as the
    /// placeholders that reference it (after the draw, because only the drawn frame knows the
    /// size it picks), so a size the frame already transmitted in this frame is not sent a
    /// second time.
    #[test]
    fn a_zoom_with_help_open_sends_the_large_mark_once() {
        let small = kitty_logos(); // 8 x 16 cells
        let big = prepare(graphics(Protocol::Kitty, CellPx { w: 16, h: 34 })); // a zoom
        let mut placer = Placer::new(Protocol::Kitty);
        // The count of the transmit of `id` in `bytes`: its own opening, which carries the
        // id, is what one image's pixels are told from another's by.
        let transmit_of =
            |id: u32| -> Vec<u8> { format!("\x1b_Ga=t,q=2,f=32,i={id},").into_bytes() };
        let (small_needle, large_needle) =
            (transmit_of(kitty::SMALL_ID), transmit_of(kitty::LARGE_ID));
        let counts = |bytes: &[u8]| -> (u32, u32) {
            (
                bytes
                    .windows(small_needle.len())
                    .filter(|w| *w == small_needle.as_slice())
                    .count() as u32,
                bytes
                    .windows(large_needle.len())
                    .filter(|w| *w == large_needle.as_slice())
                    .count() as u32,
            )
        };

        // The help is open at the ten-row mark, at the small cell size: the ten-row bitmap
        // goes out before the draw, because the placeholders the frame is about to write
        // reference its id.
        let ten = large_placement(&small, 10);
        let before = placer.before_draw(&small, std::slice::from_ref(&ten));
        let after = placer.after_draw(std::slice::from_ref(&ten), SCREEN, &small);
        assert_eq!(
            counts(&before),
            (1, 1),
            "the first help frame transmits both marks"
        );
        assert!(
            after.bytes.is_empty(),
            "a size the frame already sent before the draw is not sent again after it"
        );

        // The zoom: a new cell size is a new bitmap under the same id, and the terminal
        // cleared the screen, so the placer forgets what it sent. `before_draw` gets the
        // placements of the *previous* frame - the ten-row mark, at the old screen size -
        // and the view, on the bigger cells, picks the six-row mark for this frame. The
        // large mark must go out exactly once, by the step that knows this frame's size:
        // the old size is no longer the one drawn, so it is not pre-sent before the draw.
        placer.invalidate();
        let ten_big = large_placement(&big, 10);
        let six_big = large_placement(&big, 6);
        let before = placer.before_draw(&big, std::slice::from_ref(&ten_big));
        let after = placer.after_draw(std::slice::from_ref(&six_big), SCREEN, &big);
        let (before_small, before_large) = counts(&before);
        let (after_small, after_large) = counts(&after.bytes);
        assert_eq!(
            before_small + after_small,
            1,
            "the zoom frame transmits the small mark once"
        );
        assert_eq!(
            before_large + after_large,
            1,
            "large mark sent twice in one frame: the old size was pre-sent before the draw and the new size after it"
        );

        // The frame after the zoom is steady: the view still places the six-row mark, and
        // the pixels are held, so nothing goes out before or after the draw.
        let six_big = large_placement(&big, 6);
        let before = placer.before_draw(&big, std::slice::from_ref(&six_big));
        let after = placer.after_draw(std::slice::from_ref(&six_big), SCREEN, &big);
        assert!(
            before.is_empty() && after.bytes.is_empty(),
            "a steady frame sends nothing: the pixels are held"
        );
    }
}
