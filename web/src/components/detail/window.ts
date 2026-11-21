/**
 * The pane lists' window, said once for all three lists.
 *
 * A detail pane reads like the torrent table above it but lives in a much shorter box, so it gets
 * the same treatment in the pane's own measurements: every row is exactly `DETAIL_ROW_HEIGHT` tall
 * (the prototype's pane row - 12 px text with 8 px above and below, rounded up to a whole), the
 * drawn rows are a window moved by the box's scroll position, and two spacer rows keep the
 * scrollbar honest. This is the `TorrentTable` approach shrunk to the pane, not a library: one
 * division and two spacers, with the header left unsticky because the whole list scrolls and the
 * window maths must not owe anything to a header it cannot measure twice.
 *
 * The height is the scroll box's own `clientHeight`, watched with a `ResizeObserver` (`resize`
 * where one is missing), and `viewportHeight` overrides it because jsdom lays nothing out. With
 * nothing measured at all - jsdom, or a box that has not resized yet - the window assumes eight
 * rows, 224 px, the height a pane list has under a laptop-height pane, so it draws the rows a box
 * that size would have shown rather than none. The boxes themselves are `min-h-0 flex-1` inside
 * the pane's own fixed share of the screen, so in a browser this fallback is only ever what an
 * unmeasured box pretends it is.
 *
 * @module components/detail/window
 */
import { useEffect, useRef, useState } from 'react'
import type { RefObject } from 'react'

/** The height every pane row is drawn at and every window calculation counts in. */
export const DETAIL_ROW_HEIGHT = 28

/** The rule between pane rows, drawn as an inset shadow inside each cell - the table above the pane draws its 40 px rows the same way, so nothing adds height to these 28 px ones. */
export const DETAIL_ROW_RULE = 'shadow-[inset_0_-1px_0_var(--color-ink-750)]'

/** Rows kept drawn above and below the window, so a scroll does not have to wait for them. */
const OVERSCAN = 6

/** The rows the window assumes when nothing can be measured: 224 px, the height a pane list has under a laptop-height pane. */
const FALLBACK_ROWS = 8

export interface RowWindow {
  /** The scroll box's ref, handed to the list component's own div. */
  boxRef: RefObject<HTMLDivElement | null>
  /** The box's `onScroll`: a scroll is the box saying where it really is. */
  onScroll(event: { currentTarget: HTMLDivElement }): void
  start: number
  end: number
  padTop: number
  padBottom: number
}

/**
 * Which rows of a `count`-row list the box is showing, and the spacers around them. The pane's
 * three lists call this and slice their own arrays with the result.
 */
export function useRowWindow(count: number, viewportHeight?: number): RowWindow {
  const boxRef = useRef<HTMLDivElement>(null)
  const [scrollY, setScrollY] = useState(0)
  const [measured, setMeasured] = useState(0)

  useEffect(() => {
    const box = boxRef.current
    if (box === null) return
    const measure = () => {
      setMeasured(box.clientHeight)
    }
    measure()
    if (typeof ResizeObserver === 'function') {
      const observer = new ResizeObserver(measure)
      observer.observe(box)
      return () => {
        observer.disconnect()
      }
    }
    window.addEventListener('resize', measure)
    return () => {
      window.removeEventListener('resize', measure)
    }
  }, [])

  const height = viewportHeight ?? (measured === 0 ? FALLBACK_ROWS * DETAIL_ROW_HEIGHT : measured)
  const windowSize = Math.ceil(height / DETAIL_ROW_HEIGHT) + 2 * OVERSCAN
  const start = Math.max(
    0,
    Math.min(Math.floor(scrollY / DETAIL_ROW_HEIGHT) - OVERSCAN, count - windowSize),
  )
  const end = Math.min(count, start + windowSize)

  return {
    boxRef,
    onScroll: (event) => {
      setScrollY(event.currentTarget.scrollTop)
    },
    start,
    end,
    padTop: start * DETAIL_ROW_HEIGHT,
    padBottom: (count - end) * DETAIL_ROW_HEIGHT,
  }
}
