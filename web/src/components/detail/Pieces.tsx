/**
 * The Pieces tab: which pieces of the selected torrent are on disk.
 *
 * `have` is the bitfield as lowercase hex, piece 0 in the top bit of the first byte (BEP 3 order),
 * and this is a browser drawing a server's string, so the decode is defensive: a character that is
 * not hex makes the four bits it should describe "not had", a dangling half byte describes nothing,
 * and the number of pieces drawn never exceeds what the hex can actually describe. None of those
 * may throw - remote answers are allowed to be odd, not fatal.
 *
 * Up to 2,000 pieces draw as cells, the prototype's 10 px squares on a 2 px gap: had in the accent
 * violet (`--color-violet-500`), missing in `--color-ink-700`, the `#262629` the token file itself
 * names "the missing half of a piece grid". Past 2,000 the cells become one `<canvas>` of the same
 * squares, because 30,000 spans in the DOM is not a picture anybody can read faster than one
 * picture. The canvas reads its two colours from the same CSS tokens (`getComputedStyle`, with the
 * hexes as the fallback an environment without the stylesheet answers), and when the bitfield is so
 * long that not even one pixel per piece fits on a canvas, each cell stands for a run of pieces
 * and fills only when the whole run is had - the TUI's own rule (`pieces` in
 * `fbtt-tui/src/view/detail.rs`), where the exact count above the grid is the truth either way.
 * That count - "1,234 of 5,000 pieces" - rides above the grid in both modes and is the grid's
 * accessible name, so nobody has to count squares to learn what they show.
 *
 * A display with `devicePixelRatio` above 1 turns a canvas sized in CSS pixels into a blurry
 * half-scale bitmap, so the backing store is that many *device* pixels - `ctx.scale` maps drawing
 * coordinates back to the CSS size, which stays exactly `columns * 12` by `rows * 12`, and the
 * picture is sharp at any ratio. The columns come from an inner wrapper with no padding: the scroll
 * box's own `clientWidth` counts its `px-5` in, and columns bought with padding the picture cannot
 * use overflow the box sideways. And because the pane re-reads this tab once a second, everything
 * downstream of the answer is memoised on the two values it actually reads - the `have` string and
 * the `num_pieces` number, not the response object - so a poll that answers with an unchanged map
 * re-decodes nothing, rebuilds none of the 2,000 spans, and repaints no canvas.
 *
 * @module components/detail/Pieces
 */
import { useEffect, useMemo, useRef, useState } from 'react'
import type { PiecesInfo } from '../../api/client'

export interface PiecesProps {
  pieces: PiecesInfo
}

/** At most this many pieces draw as individual elements; past it they draw on one canvas. */
const PIECE_GRID_CAP = 2_000

/** A drawn piece square, and the gap after it: 12 px per cell on the canvas, matching the 10 px cells and 2 px gap of the grid mode. */
const PIECE_CELL = 10
const PIECE_STRIDE = 12

/** The canvas fallback for an environment that lays nothing out (jsdom): the width a pane draws at on a laptop. */
const FALLBACK_BOX_WIDTH = 960

/** Canvas sides browser engines refuse; a bitfield so long that even 1 pixel per piece overflows it falls back to runs per cell, the TUI's rule. The device-pixel backing store is capped at the same side. */
const MAX_CANVAS_SIDE = 65_000

/** A hex digit's value, or -1 for anything that is not one - including "out of string", whose charCodeAt is NaN and matches no branch. */
function hexValue(code: number): number {
  if (code >= 48 && code <= 57) return code - 48
  if (code >= 97 && code <= 102) return code - 87
  if (code >= 65 && code <= 70) return code - 55
  return -1
}

/** Piece `index` on disk, or "not had" for every way the answer can fall short of describing it. */
function pieceHad(have: string, index: number): boolean {
  const byte = index >> 3
  const hi = hexValue(have.charCodeAt(byte * 2))
  const lo = hexValue(have.charCodeAt(byte * 2 + 1))
  if (hi === -1 || lo === -1) return false
  return ((((hi << 4) | lo) >> (7 - (index & 7))) & 1) === 1
}

/** A run of pieces is had only when every one of them is, the TUI's rule for a cell that stands for more than one. */
function runHad(have: string, start: number, length: number): boolean {
  for (let i = start; i < start + length; i += 1) {
    if (!pieceHad(have, i)) return false
  }
  return true
}

/** A piece count a person reads with commas, the way the prototype's counts are grouped. */
function group(count: number): string {
  return count.toLocaleString('en-US')
}

export default function Pieces({ pieces }: PiecesProps) {
  // What the answer actually describes, decoded once. `have` is remote text: a server could hand
  // back a short, odd or dirty string, and none of it may throw or draw past itself. The memo keys
  // on the two primitives the decode actually reads, not on the response object: the pane re-reads
  // this tab every second, and a fresh object holding the same `have` and `num_pieces` must not
  // re-decode, re-render the grid, or repaint the canvas.
  const decoded = useMemo(() => {
    const have = typeof pieces.have === 'string' ? pieces.have : ''
    const describedBits = Math.floor(have.length / 2) * 8
    const declared = Number.isFinite(pieces.num_pieces)
      ? Math.max(0, Math.floor(pieces.num_pieces))
      : 0
    const total = Math.min(declared, describedBits)
    let had = 0
    for (let i = 0; i < total; i += 1) {
      if (pieceHad(have, i)) had += 1
    }
    return { have, total, had }
  }, [pieces.have, pieces.num_pieces])
  const { have, total, had } = decoded

  const onCanvas = total > PIECE_GRID_CAP

  // How many cells fit across, and how many device pixels a cell needs: the wrapper's real width in
  // a browser, the laptop width it would have where nothing measures (jsdom), and the display's
  // pixel ratio. The wrapper - not the scroll box - is what gets measured, because `clientWidth`
  // includes the box's own `px-5` and columns bought with padding the picture cannot use overflow
  // the box sideways. The canvas is built at this many columns and scrolls if the box got narrower
  // since. The ratio rides in state (it is not an event source of its own): browser zoom fires
  // `resize`, and a zoom or a window move changes the wrapper's box, so either a `ResizeObserver`
  // callback or the `resize` fallback re-measures both numbers together.
  const gridRef = useRef<HTMLDivElement>(null)
  const [metrics, setMetrics] = useState({ width: 0, ratio: 1 })
  useEffect(() => {
    const grid = gridRef.current
    if (grid === null) return
    const measure = () => {
      const width = grid.clientWidth
      const ratio = window.devicePixelRatio ?? 1
      setMetrics((current) =>
        current.width === width && current.ratio === ratio ? current : { width, ratio },
      )
    }
    measure()
    if (typeof ResizeObserver === 'function') {
      const observer = new ResizeObserver(measure)
      observer.observe(grid)
      window.addEventListener('resize', measure)
      return () => {
        observer.disconnect()
        window.removeEventListener('resize', measure)
      }
    }
    window.addEventListener('resize', measure)
    return () => {
      window.removeEventListener('resize', measure)
    }
  }, [])

  const maxCols = Math.floor(MAX_CANVAS_SIDE / PIECE_STRIDE)
  const measured = metrics.width === 0 ? FALLBACK_BOX_WIDTH : metrics.width
  const columns = Math.max(1, Math.min(Math.floor(measured / PIECE_STRIDE), maxCols))
  const maxRows = Math.max(1, Math.floor(MAX_CANVAS_SIDE / PIECE_STRIDE))
  // One piece per cell whenever that fits a canvas; otherwise each cell stands for a run, exactly
  // as many pieces as it takes so the canvas still has no more than one side a browser accepts.
  const perCell = Math.max(1, Math.ceil(total / (columns * maxRows)))
  const cells = Math.ceil(total / perCell)
  const rows = Math.max(1, Math.ceil(cells / columns))

  const canvasRef = useRef<HTMLCanvasElement>(null)
  useEffect(() => {
    const canvas = canvasRef.current
    if (!onCanvas || canvas === null) return
    // The CSS size stays `columns * 12` by `rows * 12`; the backing store is that many device
    // pixels, shrunk if even that overflows what engines accept, and the context scales from CSS
    // pixels into backing pixels so one square stays one square at ratio 1, 2, or 3.5. Assigning
    // `width` or `height` resets the canvas - transform included - so every redraw (a resize, a
    // ratio change, a new answer) scales from a clean start, never twice.
    const cssWidth = columns * PIECE_STRIDE
    const cssHeight = rows * PIECE_STRIDE
    const ratio = Math.max(1, metrics.ratio || 1)
    const scale = Math.min(ratio, MAX_CANVAS_SIDE / cssWidth, MAX_CANVAS_SIDE / cssHeight)
    canvas.width = Math.round(cssWidth * scale)
    canvas.height = Math.round(cssHeight * scale)
    const ctx = canvas.getContext('2d')
    if (ctx === null) return
    ctx.scale(canvas.width / cssWidth, canvas.height / cssHeight)
    // The two colours are the page's tokens; the hexes are the same values for an environment
    // (a test) that has no stylesheet loaded, where getComputedStyle answers the empty string.
    const style = getComputedStyle(canvas)
    const token = (name: string, fallback: string): string => {
      const value = style.getPropertyValue(name).trim()
      return value === '' ? fallback : value
    }
    const haveColor = token('--color-violet-500', '#863bff')
    const missingColor = token('--color-ink-700', '#262629')
    for (let cell = 0; cell < cells; cell += 1) {
      const startCell = cell * perCell
      // The last run stops at the described end of the bitfield: spare bits of a final byte are
      // zero, and zero beyond the torrent's own piece count is not a piece this cell stands for.
      const length = Math.min(perCell, total - startCell)
      ctx.fillStyle = runHad(have, startCell, length) ? haveColor : missingColor
      ctx.fillRect((cell % columns) * PIECE_STRIDE, Math.floor(cell / columns) * PIECE_STRIDE, PIECE_CELL, PIECE_CELL)
    }
  }, [onCanvas, have, total, cells, perCell, columns, rows, metrics.ratio])

  // The grid's spans, keyed on the same primitives as the decode: an unchanged map hands back the
  // same array of the same elements, and React has nothing to reconcile on the tick's re-render.
  const squares = useMemo(
    () =>
      Array.from({ length: total }, (_, i) => (
        <span
          key={i}
          data-piece={i}
          className={`h-2.5 w-2.5 rounded-[2px] ${pieceHad(have, i) ? 'bg-violet-500' : 'bg-ink-700'}`}
        />
      )),
    [have, total],
  )

  const label = `${group(had)} of ${group(total)} pieces`

  return (
    <div className="min-h-0 flex-1 overflow-auto px-5 py-3">
      <div className="mb-2 font-mono text-[12px] text-muted">{label}</div>
      {/* The measured wrapper: no padding of its own, so its `clientWidth` is exactly the room a
          canvas may fill, not that room plus the scroll box's `px-5`. It is outside the `total > 0`
          branch so the measurement is wired even before a metadata-less torrent has pieces to draw. */}
      <div ref={gridRef}>
        {total > 0 &&
          (onCanvas ? (
            <canvas
              ref={canvasRef}
              aria-label={label}
              className="block"
              style={{
                width: columns * PIECE_STRIDE,
                height: rows * PIECE_STRIDE,
                imageRendering: 'pixelated',
              }}
            />
          ) : (
            <div role="img" aria-label={label} className="flex flex-wrap gap-0.5">
              {squares}
            </div>
          ))}
      </div>
    </div>
  )
}
