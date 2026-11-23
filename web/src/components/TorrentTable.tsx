/**
 * The torrent table: the toolbar that narrows it, the headers that order it, and the rows it can
 * afford to draw.
 *
 * The columns are the prototype's and the terminal's, in that shape: name, a bar and a percent,
 * down, up, the seed/peer split, and the state with its dot and colour. The percent beside the bar
 * and the width of the bar are the same number (`filledPercent`), the state's word and colour come
 * straight from `live.ts`'s reading of the TUI's rule, and names and error strings go through
 * `displaySafe` before they are drawn, because every one of them came from somewhere else. A name
 * the server left empty gets its hash, the way both TUI backends do it. Each header shares its
 * column's alignment - the number headers sit right with the rates, the rest sit left with their
 * cells - because alignment is a property of the column, said once in `COLUMNS`, and never the
 * browser's default of centring a header over cells that are not centred.
 *
 * Twenty rows render twenty rows; two thousand render a window of them. The box scrolls, the rows
 * below and above the window are replaced by two spacer rows sized to keep the scrollbar honest,
 * and the window moves with the scroll position minus the sticky header's height. This is
 * deliberately not a library: windowing a `tbody` is one division and two spacer rows, and a
 * library would cost more than the 2,000-row table earns back. `viewportHeight` overrides the
 * measured height for the same reason jsdom is a poor place to measure anything: it has no layout.
 * In a real browser the box's size is watched with a `ResizeObserver` (the `resize` event is the
 * fallback where one is missing), so a banner appearing, the sidebar wrapping, or the phone
 * turning sideways all move the window before the next read rather than after the next reload.
 *
 * Every row is exactly `ROW_HEIGHT` tall, because the spacer arithmetic only tells the truth if it
 * can multiply a constant - and in a real browser it is the cells that make it so, not the row:
 * `border-separate` with zero spacing so nothing collapses or adds between cells, the rule drawn as
 * an inset shadow so it costs no height, and every cell `box-border` with the row height as its own
 * height and `leading-5` so one line of text is exactly 20 px. Padding 10, text 20, padding 10 is
 * then exactly 40: nothing a cell holds can exceed its declared 40, and a row is as tall as its
 * tallest cell, whole pixels at any zoom. The state cell's dot-and-word span is a block-level 20px
 * flex box for that reason - an inline box with an empty first child synthesises a baseline taller
 * than the text line, and that is how 40 quietly becomes 40.5. Every cell `truncate` with the full
 * text in a `title` keeps it to that one line - a 255-character error is dots, not a wrapped
 * paragraph.
 *
 * Rows are keyboard citizens, not click-only targets: each is focusable, Enter and Space pick it,
 * Arrow Up and Down move the selection through the current order, and the drawn window follows
 * the chosen row until the reader scrolls away - the pin releases at the first real scroll. The
 * focus indicator draws ONE ring around the whole row, as inset shadow bands on the row's own
 * cells: Chrome renders an `outline` (or a `border`) on a `tr` around every cell of the row, which
 * is why the ring is bands on cells and why the selected row's accent edge rides the first cell
 * alone - either one drawn per-cell is a violet bar between the columns. The sort starts where the
 * server left it; one click orders a column, the next reverses it, the third hands it back to
 * server order, and the toolbar says which of those the rows are in. Sorting and filtering both
 * read the same display-safe names the rows show.
 *
 * @module components/TorrentTable
 */
import { useId, useEffect, useMemo, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { ArrowDown, ArrowUp } from 'lucide-react'
import { displaySafe, filledPercent, formatPercent, formatRate } from '../format'
import type { RowState, TorrentRow } from '../live'
import { matches } from '../keys'

export interface TorrentTableProps {
  /** The rows the screen's filters left standing, in the order the server listed them. */
  rows: TorrentRow[]
  selectedId: string | null
  onPick?: (id: string) => void
  /** What the screen puts at the right end of the toolbar: the actions that work on the row that is chosen. A slot rather than six more props because the actions belong to the screen that owns the selection and the dialogs: the table's job is to hold the row at the same height the filter field has, at the same end of the row the prototype drew them, and to know nothing about pause or delete. */
  actions?: ReactNode
  /** The scroll box's height in pixels. Given by tests because jsdom lays nothing out; measured from the box otherwise. */
  viewportHeight?: number
}

/** The height every row is drawn at and every window calculation counts in. `40px` is the prototype's row: 13px text with 10px above and below, rounded up to a whole that fits a 36px control. */
const ROW_HEIGHT = 40
/** Rows kept drawn above and below the window, so a scroll does not have to wait for them. */
const OVERSCAN = 6

/** The rule between rows, drawn inside the row's own 40px so rows are exactly `ROW_HEIGHT` tall. */
const ROW_RULE = 'shadow-[inset_0_-1px_0_var(--color-ink-750)]'
/** The chosen row's accent edge - the bar the prototype drew at the row's left - on the first cell only. The same inset on every cell is the violet bar between the columns: a `tr` holds cells side by side, so anything painted per-cell repeats once per column. */
const ROW_EDGE_SELECTED =
  'shadow-[inset_0_-1px_0_var(--color-ink-750),inset_2px_0_0_var(--color-accent-light)]'
/** The focus ring: ONE ring around the whole row, drawn as inset shadow bands on the row's own cells - the top and bottom band on every cell, the left band on the first, the right band on the last - while the row (`tr.group`) has keyboard focus. A `border` or an `outline` on a `tr` cannot do this job in Chrome: both render around every cell of the row, which is exactly the per-cell violet bar this replaces. The ring replaces the row rule for the moment it is up - the accent band sits where the rule sat - which is what keeps one box-shadow declaration in charge per cell instead of two competing ones. */
const RING_BAND =
  'group-focus-visible:shadow-[inset_0_2px_0_var(--color-accent-light),inset_0_-2px_0_var(--color-accent-light)]'
const RING_EDGE_LEFT =
  'group-focus-visible:shadow-[inset_0_2px_0_var(--color-accent-light),inset_0_-2px_0_var(--color-accent-light),inset_2px_0_0_var(--color-accent-light)]'
const RING_EDGE_RIGHT =
  'group-focus-visible:shadow-[inset_0_2px_0_var(--color-accent-light),inset_0_-2px_0_var(--color-accent-light),inset_-2px_0_0_var(--color-accent-light)]'

type SortColumn = 'name' | 'progress' | 'down' | 'up' | 'peers' | 'state'

/** The columns with the alignment their header and their cells share. A `th` left to itself centres, so the alignment is said once here and taken from the same token by both. */
const COLUMNS: { key: SortColumn; label: string; className: string }[] = [
  { key: 'name', label: 'Name', className: 'px-5 text-left' },
  { key: 'progress', label: 'Progress', className: 'w-[220px] px-3 text-left' },
  { key: 'down', label: 'Down', className: 'px-3 text-right' },
  { key: 'up', label: 'Up', className: 'px-3 text-right' },
  { key: 'peers', label: 'Peers', className: 'px-3 text-right' },
  { key: 'state', label: 'State', className: 'px-5 text-left' },
]

/** One cell's shadows: the row rule (the chosen row's accent edge, first cell only), plus the cell's share of the row's focus ring. */
function cellRules(index: number, selected: boolean): string {
  const base = index === 0 && selected ? ROW_EDGE_SELECTED : ROW_RULE
  const ring =
    index === 0 ? RING_EDGE_LEFT : index === COLUMNS.length - 1 ? RING_EDGE_RIGHT : RING_BAND
  return `${base} ${ring}`
}

/** The one-word state labels, the terminal's (`state_label` in `fbtt-tui/src/view/mod.rs`). */
const STATE_LABEL: Record<RowState, string> = {
  downloading: 'downloading',
  seeding: 'seeding',
  stalled: 'stalled',
  metadata: 'metadata',
  queued: 'queued',
  checking: 'checking',
  paused: 'paused',
  error: 'error',
}

/** The state colours, the terminal's (`state_colour` in `fbtt-tui/src/theme.rs`) read through the tokens: the accent for running work, green for finished, amber for nothing arriving, red for a failure, muted for everything stopped or waiting. */
const STATE_TEXT: Record<RowState, string> = {
  downloading: 'text-violet-500',
  seeding: 'text-seeding',
  stalled: 'text-stalled',
  metadata: 'text-muted',
  queued: 'text-muted',
  checking: 'text-muted',
  paused: 'text-muted',
  error: 'text-error',
}

const BAR_BG: Record<RowState, string> = {
  downloading: 'bg-violet-500',
  seeding: 'bg-seeding',
  stalled: 'bg-stalled',
  metadata: 'bg-muted',
  queued: 'bg-muted',
  checking: 'bg-muted',
  paused: 'bg-muted',
  error: 'bg-error',
}

/** The fraction a row's bar and percent carry. A torrent hash-checking reports its fraction in the state rather than in `progress`, which for one being checked over again says nothing useful. */
function fractionOf(row: TorrentRow): number {
  return row.rowState === 'checking' ? (row.checking_progress ?? 0) : row.progress
}

/** The name as it is drawn: display-safe, and the hash when the server had no name to give (a magnet whose metadata has not arrived). */
function displayName(row: TorrentRow): string {
  return displaySafe(row.name) || displaySafe(row.id)
}

/** The state column's whole text, dots and all: what the cell shows when nothing is truncated, and what its `title` always holds. */
function stateText(row: TorrentRow): string {
  return row.rowState === 'error' && row.error !== null && row.error !== undefined
    ? `error: ${displaySafe(row.error)}`
    : STATE_LABEL[row.rowState]
}

/** The value a column sorts on, so the header's order and the rows' contents agree. */
function sortValue(row: TorrentRow, column: SortColumn): number | string {
  switch (column) {
    case 'name':
      return displayName(row).toLowerCase()
    case 'progress':
      return fractionOf(row)
    case 'down':
      return row.download_rate
    case 'up':
      return row.upload_rate
    case 'peers':
      return row.num_peers
    case 'state':
      return STATE_LABEL[row.rowState]
  }
}

function compareRows(a: TorrentRow, b: TorrentRow, column: SortColumn): number {
  const x = sortValue(a, column)
  const y = sortValue(b, column)
  if (typeof x === 'number' && typeof y === 'number') return x - y
  const xs = String(x)
  const ys = String(y)
  if (xs === ys) return 0
  return xs < ys ? -1 : 1
}

export default function TorrentTable({
  rows,
  selectedId,
  onPick,
  actions,
  viewportHeight,
}: TorrentTableProps) {
  const filterId = useId()
  const [query, setQuery] = useState('')
  const [sort, setSort] = useState<{ column: SortColumn; reverse: boolean } | null>(null)

  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase()
    const kept =
      needle === ''
        ? [...rows]
        : rows.filter((row) => displayName(row).toLowerCase().includes(needle))
    if (sort !== null) {
      const factor = sort.reverse ? -1 : 1
      kept.sort((a, b) => factor * compareRows(a, b, sort.column))
    }
    return kept
  }, [rows, query, sort])

  // The window. The height is measured from the scroll box in a real browser; `viewportHeight`
  // overrides it (jsdom measures nothing, and the virtualisation test names the height itself).
  // The measurement is watched with a ResizeObserver so banners, wrapping, and rotation all
  // change it: the window must follow the box the reader is looking at, not the box that existed
  // when the screen first mounted. `resize` is the fallback where no observer exists (jsdom).
  const boxRef = useRef<HTMLDivElement>(null)
  const headerRef = useRef<HTMLTableSectionElement>(null)
  const [measured, setMeasured] = useState(0)
  const [headerHeight, setHeaderHeight] = useState(0)
  const [scrollY, setScrollY] = useState(0)
  // The row the view is pinned to: set by whichever event selects a row (the first selection this
  // screen was handed included) and released by any real scroll, which reports where the box
  // actually is. The pin is what lets an arrow that lands off screen move the window in the same
  // render that makes the selection.
  const [pinToRow, setPinToRow] = useState<string | null>(selectedId)
  useEffect(() => {
    const box = boxRef.current
    if (box === null) return
    const measure = () => {
      setMeasured(box.clientHeight)
      setHeaderHeight(headerRef.current?.offsetHeight ?? 0)
    }
    measure()
    if (typeof ResizeObserver === 'function') {
      const observer = new ResizeObserver(measure)
      observer.observe(box)
      if (headerRef.current !== null) observer.observe(headerRef.current)
      return () => {
        observer.disconnect()
      }
    }
    window.addEventListener('resize', measure)
    return () => {
      window.removeEventListener('resize', measure)
    }
  }, [])

  const height = viewportHeight ?? measured
  const total = shown.length
  const windowSize = Math.ceil(height / ROW_HEIGHT) + 2 * OVERSCAN

  // Where the box is looking: its real `scrollTop`, unless a selection this very moment pinned the
  // view to a row outside it - then the nearest scroll position that keeps that row on screen
  // under the sticky header. A browser's own scroll event replaces the pin with the real number
  // the instant the box actually moves.
  let effectiveScroll = scrollY
  if (pinToRow !== null) {
    const pinned = shown.findIndex((row) => row.id === pinToRow)
    if (pinned !== -1) {
      const top = pinned * ROW_HEIGHT
      const lowest = Math.max(0, top + ROW_HEIGHT - height)
      const highest = Math.max(0, top - headerHeight)
      effectiveScroll = lowest <= highest ? Math.min(highest, Math.max(lowest, scrollY)) : lowest
    }
  }

  // The sticky header covers the first `headerHeight` pixels of whatever the box is scrolled to,
  // so the rows begin one header below it, not at it.
  const start = Math.max(
    0,
    Math.min(
      Math.floor(Math.max(0, effectiveScroll - headerHeight) / ROW_HEIGHT) - OVERSCAN,
      total - windowSize,
    ),
  )
  const end = Math.min(total, start + windowSize)
  const visible = shown.slice(start, end)
  const padTop = start * ROW_HEIGHT
  const padBottom = (total - end) * ROW_HEIGHT

  // The document's side of the two-way street, in one effect that only writes to the DOM: make
  // the box's real position agree with where the drawn window is, and hand the focus to the
  // selected row - with `preventScroll`, because the box's position is this component's business:
  // the browser's own "scroll it into view" nudge would fight the explicit `scrollTop` above and
  // the sticky header over it. State never moves here - it moves through this file's event handlers.
  //
  // The scroll is also restored when the box is measured again after being empty: a table hidden with `display:none`
  // (behind the Settings screen) reports height 0 and the browser may reset its `scrollTop`, and
  // nothing else would put it back when it is shown.
  useEffect(() => {
    const box = boxRef.current
    if (box !== null && box.scrollTop !== effectiveScroll) box.scrollTop = effectiveScroll
  }, [effectiveScroll])

  // ...and when the box comes back from nothing. Only 0 to something counts: a phone's address bar
  // resizes the box in the middle of a scroll, and restoring then would fight the reader's finger.
  const lastMeasured = useRef(0)
  useEffect(() => {
    const was = lastMeasured.current
    lastMeasured.current = measured
    const box = boxRef.current
    if (was === 0 && measured > 0 && box !== null && box.scrollTop !== effectiveScroll) {
      box.scrollTop = effectiveScroll
    }
  }, [measured, effectiveScroll])

  useEffect(() => {
    const box = boxRef.current
    if (selectedId === null) return
    const row = box?.querySelector<HTMLElement>(`[data-row="${CSS.escape(selectedId)}"]`)
    if (row !== null && row !== undefined && row !== document.activeElement) {
      row.focus({ preventScroll: true })
    }
  }, [selectedId, effectiveScroll])

  function pick(id: string): void {
    if (onPick === undefined) return
    // The row the reader chose is the row the view keeps on screen, until they scroll away.
    setPinToRow(id)
    onPick(id)
  }

  function moveSelection(delta: number): void {
    if (onPick === undefined || shown.length === 0) return
    const current = shown.findIndex((row) => row.id === selectedId)
    const next =
      current === -1
        ? delta > 0
          ? 0
          : shown.length - 1
        : Math.min(shown.length - 1, Math.max(0, current + delta))
    pick(shown[next].id)
  }

  const sortedLabel =
    sort === null ? null : COLUMNS.find((column) => column.key === sort.column)?.label.toLowerCase()

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex flex-wrap items-center gap-3 border-b border-rule px-5 py-3">
        <label htmlFor={filterId} className="sr-only">
          Filter torrents
        </label>
        <input
          id={filterId}
          type="search"
          placeholder="Filter by name"
          value={query}
          onChange={(event) => {
            setQuery(event.target.value)
          }}
          className="box-border h-9 min-w-0 basis-60 max-w-90 rounded-control border border-rule bg-sunken px-3 text-[13px] text-strong outline-none focus-visible:border-accent-light focus-visible:ring-2 focus-visible:ring-accent-soft"
        />
        <span className="text-[13px] text-muted">
          {sort === null ? (
            'In server order'
          ) : (
            <>
              Sorted by <span className="text-body">{sortedLabel}</span>
              {sort.reverse ? ' (reverse)' : ''}
            </>
          )}
        </span>
        {actions !== undefined && actions !== null && (
          <div className="ml-auto flex flex-wrap items-center gap-2">{actions}</div>
        )}
      </div>

      <div
        ref={boxRef}
        onScroll={(event) => {
          // A scroll is the box telling us where it really is: that truth replaces any pin, and
          // the pin lets go the moment it has done its job.
          setScrollY(event.currentTarget.scrollTop)
          setPinToRow(null)
        }}
        className="min-h-0 flex-1 overflow-auto"
      >
        <table className="w-full min-w-[760px] border-separate border-spacing-0 text-[13px]">
          <caption className="sr-only">Torrents</caption>
          <thead ref={headerRef}>
            <tr className="text-[12px] text-muted">
              {COLUMNS.map((column) => (
                <th
                  key={column.key}
                  scope="col"
                  aria-sort={
                    sort !== null && sort.column === column.key
                      ? sort.reverse
                        ? 'descending'
                        : 'ascending'
                      : 'none'
                  }
                  className={`sticky top-0 z-10 whitespace-nowrap border-b border-rule bg-canvas py-2.5 font-medium ${column.className}`}
                >
                  <button
                    type="button"
                    onClick={() => {
                      setSort((current) =>
                        current !== null && current.column === column.key
                          ? current.reverse
                            ? null
                            : { column: column.key, reverse: true }
                          : { column: column.key, reverse: false },
                      )
                    }}
                    className="inline-flex cursor-pointer items-center gap-1 hover:text-body"
                  >
                    {column.label}
                    {sort !== null && sort.column === column.key &&
                      (sort.reverse ? (
                        <ArrowDown className="h-3.5 w-3.5" aria-hidden="true" />
                      ) : (
                        <ArrowUp className="h-3.5 w-3.5" aria-hidden="true" />
                      ))}
                  </button>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {padTop > 0 && (
              <tr data-spacer="top" aria-hidden="true" style={{ height: padTop }}>
                <td colSpan={COLUMNS.length} />
              </tr>
            )}
            {visible.map((row) => {
              const name = displayName(row)
              const fraction = fractionOf(row)
              const filled = filledPercent(fraction)
              const selected = row.id === selectedId
              const ruleFor = (index: number): string => cellRules(index, selected)
              return (
                <tr
                  key={row.id}
                  data-row={row.id}
                  tabIndex={0}
                  aria-selected={selected ? true : undefined}
                  onClick={onPick === undefined ? undefined : () => pick(row.id)}
                  onKeyDown={(event) => {
                    if (matches('Torrent list', 'Select the focused torrent', event)) {
                      event.preventDefault()
                      pick(row.id)
                    } else if (matches('Torrent list', 'Move to the next torrent', event)) {
                      event.preventDefault()
                      moveSelection(1)
                    } else if (matches('Torrent list', 'Move to the previous torrent', event)) {
                      event.preventDefault()
                      moveSelection(-1)
                    }
                  }}
                  className={
                    // `group`: the ring's bands live on the cells but switch on with the row's own
                    // `:focus-visible`. `outline-none` keeps Chrome's own per-cell default focus
                    // draw-off; the bands below are the ring.
                    'group outline-none ' +
                    (onPick !== undefined ? 'cursor-pointer hover:bg-ink-700 ' : '') +
                    (selected ? 'bg-accent-soft' : '')
                  }
                  style={{ height: ROW_HEIGHT }}
                >
                  <td
                    className={`box-border h-10 leading-5 max-w-[420px] truncate px-5 py-2.5 text-body ${ruleFor(0)}`}
                    title={name}
                  >
                    {name}
                  </td>
                  <td
                    className={`box-border h-10 leading-5 overflow-hidden px-3 py-2.5 ${ruleFor(1)}`}
                  >
                    <div className="flex items-center gap-2.5">
                      <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-ink-700">
                        <div
                          className={`h-1.5 rounded-full ${BAR_BG[row.rowState]}`}
                          style={{ width: `${filled}%` }}
                        />
                      </div>
                      <span className="w-11 text-right font-mono text-xs text-body">
                        {formatPercent(fraction)}
                      </span>
                    </div>
                  </td>
                  <td
                    className={`box-border h-10 leading-5 truncate px-3 py-2.5 text-right font-mono text-xs text-body ${ruleFor(2)}`}
                  >
                    {formatRate(row.download_rate)}
                  </td>
                  <td
                    className={`box-border h-10 leading-5 truncate px-3 py-2.5 text-right font-mono text-xs text-body ${ruleFor(3)}`}
                  >
                    {formatRate(row.upload_rate)}
                  </td>
                  <td
                    className={`box-border h-10 leading-5 truncate px-3 py-2.5 text-right font-mono text-xs text-body ${ruleFor(4)}`}
                  >
                    {row.num_seeds} / {row.num_peers}
                  </td>
                  <td
                    className={`box-border h-10 leading-5 max-w-[190px] truncate px-5 py-2.5 ${ruleFor(5)}`}
                    title={stateText(row)}
                  >
                    {/* Block-level flex, deliberately not inline-flex: an inline box whose first child is the empty dot synthesises a baseline that enlarges the cell's line box past 20 px, and the row follows the tallest cell. */}
                    <span className={`flex h-5 items-center gap-2 ${STATE_TEXT[row.rowState]}`}>
                      <span
                        aria-hidden="true"
                        className={`h-1.75 w-1.75 flex-none rounded-full ${BAR_BG[row.rowState]}`}
                      />
                      {stateText(row)}
                    </span>
                  </td>
                </tr>
              )
            })}
            {total === 0 && (
              <tr>
                <td colSpan={COLUMNS.length} className="px-5 py-4 text-muted">
                  {query === '' ? 'No torrents.' : 'No torrents match.'}
                </td>
              </tr>
            )}
            {padBottom > 0 && (
              <tr data-spacer="bottom" aria-hidden="true" style={{ height: padBottom }}>
                <td colSpan={COLUMNS.length} />
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  )
}
