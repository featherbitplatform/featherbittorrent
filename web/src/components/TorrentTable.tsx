/**
 * The torrent table: the toolbar that narrows it, the headers that order it, and the rows it can
 * afford to draw.
 *
 * The columns are the prototype's and the terminal's, in that shape: name, a bar and a percent,
 * down, up, the seed/peer split, and the state with its dot and colour. The percent beside the bar
 * and the width of the bar are the same number (`filledPercent`), the state's word and colour come
 * straight from `live.ts`'s reading of the TUI's rule, and names and error strings go through
 * `displaySafe` before they are drawn, because every one of them came from somewhere else. A name
 * the server left empty gets its hash, the way both TUI backends do it.
 *
 * Twenty rows render twenty rows; two thousand render a window of them. The box scrolls, the rows
 * below and above the window are replaced by two spacer rows sized to keep the scrollbar honest,
 * and the window moves with the scroll position. This is deliberately not a library: windowing a
 * `tbody` is one division and two spacer rows, and a library would cost more than the 2,000-row
 * table earns back. `viewportHeight` overrides the measured height for the same reason jsdom is a
 * poor place to measure anything: it has no layout.
 *
 * The toolbar filter narrows by name, case-insensitively, over the displayed name. The headers
 * sort: one click orders the column, the next reverses it, and the toolbar says which column is in
 * charge the way the prototype does. Sorting uses the same display-safe names the rows show, so
 * what a filter finds and what a header orders are readings of the same thing.
 *
 * @module components/TorrentTable
 */
import { useId, useEffect, useMemo, useRef, useState } from 'react'
import { ArrowDown, ArrowUp } from 'lucide-react'
import { displaySafe, filledPercent, formatPercent, formatRate } from '../format'
import type { RowState, TorrentRow } from '../live'

export interface TorrentTableProps {
  /** The rows the screen's filters left standing, in the order the server listed them. */
  rows: TorrentRow[]
  selectedId: string | null
  onPick?: (id: string) => void
  /** The scroll box's height in pixels. Given by tests because jsdom lays nothing out; measured from the box otherwise. */
  viewportHeight?: number
}

/** The height every row is drawn at and every window calculation counts in. `40px` is the prototype's row: 13px text with 10px above and below, rounded up to a whole that fits a 36px control. */
const ROW_HEIGHT = 40
/** Rows kept drawn above and below the window, so a scroll does not have to wait for them. */
const OVERSCAN = 6

type SortColumn = 'name' | 'progress' | 'down' | 'up' | 'peers' | 'state'

const COLUMNS: { key: SortColumn; label: string; className: string }[] = [
  { key: 'name', label: 'Name', className: 'px-5' },
  { key: 'progress', label: 'Progress', className: 'w-[220px] px-3' },
  { key: 'down', label: 'Down', className: 'px-3 text-right' },
  { key: 'up', label: 'Up', className: 'px-3 text-right' },
  { key: 'peers', label: 'Peers', className: 'px-3 text-right' },
  { key: 'state', label: 'State', className: 'px-5' },
]

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
  viewportHeight,
}: TorrentTableProps) {
  const filterId = useId()
  const [query, setQuery] = useState('')
  const [sort, setSort] = useState<{ column: SortColumn; reverse: boolean }>({
    column: 'progress',
    reverse: false,
  })

  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase()
    const kept =
      needle === ''
        ? [...rows]
        : rows.filter((row) => displayName(row).toLowerCase().includes(needle))
    const factor = sort.reverse ? -1 : 1
    kept.sort((a, b) => factor * compareRows(a, b, sort.column))
    return kept
  }, [rows, query, sort])

  // The window. The height is measured from the scroll box in a real browser; `viewportHeight`
  // overrides it (jsdom measures nothing, and the virtualisation test names the height itself).
  const boxRef = useRef<HTMLDivElement>(null)
  const [measured, setMeasured] = useState(0)
  const [scrollY, setScrollY] = useState(0)
  useEffect(() => {
    const box = boxRef.current
    if (box === null) return
    const measure = () => {
      setMeasured(box.clientHeight)
    }
    measure()
    window.addEventListener('resize', measure)
    return () => {
      window.removeEventListener('resize', measure)
    }
  }, [])

  const height = viewportHeight ?? measured
  const total = shown.length
  const windowSize = Math.ceil(height / ROW_HEIGHT) + 2 * OVERSCAN
  const start = Math.max(0, Math.min(Math.floor(scrollY / ROW_HEIGHT) - OVERSCAN, total - windowSize))
  const end = Math.min(total, start + windowSize)
  const visible = shown.slice(start, end)
  const padTop = start * ROW_HEIGHT
  const padBottom = (total - end) * ROW_HEIGHT

  const sortedLabel = COLUMNS.find((column) => column.key === sort.column)?.label.toLowerCase()

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
          Sorted by <span className="text-body">{sortedLabel}</span>
          {sort.reverse && ' (reverse)'}
        </span>
      </div>

      <div
        ref={boxRef}
        onScroll={(event) => {
          setScrollY(event.currentTarget.scrollTop)
        }}
        className="min-h-0 flex-1 overflow-auto"
      >
        <table className="w-full min-w-[760px] border-collapse text-[13px]">
          <caption className="sr-only">Torrents</caption>
          <thead>
            <tr className="text-[12px] text-muted">
              {COLUMNS.map((column) => (
                <th
                  key={column.key}
                  scope="col"
                  aria-sort={
                    sort.column === column.key
                      ? sort.reverse
                        ? 'descending'
                        : 'ascending'
                      : 'none'
                  }
                  className={`sticky top-0 z-10 border-b border-rule bg-canvas py-2.5 font-medium ${column.className}`}
                >
                  <button
                    type="button"
                    onClick={() => {
                      setSort((current) =>
                        current.column === column.key
                          ? { column: column.key, reverse: !current.reverse }
                          : { column: column.key, reverse: false },
                      )
                    }}
                    className="inline-flex cursor-pointer items-center gap-1 hover:text-body"
                  >
                    {column.label}
                    {sort.column === column.key &&
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
              return (
                <tr
                  key={row.id}
                  data-row={row.id}
                  onClick={onPick === undefined ? undefined : () => onPick(row.id)}
                  className={
                    'border-t border-ink-750 ' +
                    (onPick !== undefined ? 'cursor-pointer hover:bg-ink-700 ' : '') +
                    (row.id === selectedId
                      ? 'bg-accent-soft shadow-[inset_2px_0_0_var(--color-accent-light)]'
                      : '')
                  }
                  style={{ height: ROW_HEIGHT }}
                >
                  <td className="max-w-[420px] truncate px-5 py-2.5 text-body" title={name}>
                    {name}
                  </td>
                  <td className="px-3 py-2.5">
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
                  <td className="px-3 py-2.5 text-right font-mono text-xs text-body">
                    {formatRate(row.download_rate)}
                  </td>
                  <td className="px-3 py-2.5 text-right font-mono text-xs text-body">
                    {formatRate(row.upload_rate)}
                  </td>
                  <td className="px-3 py-2.5 text-right font-mono text-xs text-body">
                    {row.num_seeds} / {row.num_peers}
                  </td>
                  <td className="px-5 py-2.5">
                    <span className={`inline-flex items-center gap-2 ${STATE_TEXT[row.rowState]}`}>
                      <span
                        aria-hidden="true"
                        className={`h-1.75 w-1.75 flex-none rounded-full ${BAR_BG[row.rowState]}`}
                      />
                      {row.rowState === 'error' && row.error !== null && row.error !== undefined
                        ? `error: ${displaySafe(row.error)}`
                        : STATE_LABEL[row.rowState]}
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
