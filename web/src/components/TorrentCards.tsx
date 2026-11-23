/**
 * The phone's torrent list: one card per torrent, the owner's option A.
 *
 * Each card is a single button (the whole card is the tap target, well over 44 px tall) holding the
 * name on one line with an ellipsis, a progress bar in the state colour, the state with its percent,
 * and the one speed that matters: the download rate while downloading, the upload rate while
 * seeding, the peer count when stalled. Everything else a torrent can be (paused, queued, errored)
 * shows no speed. Names are remote text and go through `displaySafe`.
 *
 * Above the cards sit the text filter and the sort, the table's own two (spec 2.4): the screen owns
 * them, so a query typed here is still there when the window grows into the table. Fields are 16 px
 * so a phone does not zoom the page when one takes the focus. The cards are windowed with the
 * detail lists' `useRowWindow` at a fixed `CARD_HEIGHT`, so a live tick over 500 torrents redraws
 * the twenty or so a phone can show, not all of them.
 *
 * @module components/TorrentCards
 */
import { useId, useMemo, useState } from 'react'
import { useRowWindow } from './detail/window'
import { filledPercent, formatPercent, formatRate } from '../format'
import { arrangeRows, rowFraction as fractionOf, rowName, SORT_COLUMNS } from '../filters'
import type { SortColumn, TorrentSort } from '../filters'
import type { RowState, TorrentRow } from '../live'

export interface TorrentCardsProps {
  rows: TorrentRow[]
  /** The card that was tapped. */
  onPick(id: string): void
  /** The text filter, when the screen owns it; absent, the cards keep their own. */
  query?: string
  onQueryChange?: (query: string) => void
  /** The sort, when the screen owns it; absent, the cards keep their own. */
  sort?: TorrentSort
  onSortChange?: (sort: TorrentSort) => void
  /** The scroll box's height in pixels. Given by tests because jsdom lays nothing out. */
  viewportHeight?: number
}

/** Every card's slot: an 80 px card and the 8 px gap under it, both in px (the card is the slot's full content height), so a larger root font cannot make a card overflow its slot. The window maths counts in this. */
const CARD_HEIGHT = 88

const FIELD =
  'box-border min-h-11 rounded-control border border-rule bg-sunken px-3 text-base text-strong outline-none focus-visible:border-accent-light focus-visible:ring-2 focus-visible:ring-accent-soft'

const STATE_TEXT: Record<RowState, string> = {
  downloading: 'text-violet-400',
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

/** The speed that matters for this state, or null when the state has none. */
function speedOf(row: TorrentRow): string | null {
  switch (row.rowState) {
    case 'downloading':
      return `${formatRate(row.download_rate)} down`
    case 'seeding':
      return `${formatRate(row.upload_rate)} up`
    case 'stalled':
      return `${String(row.num_peers)} peers`
    default:
      return null
  }
}

export default function TorrentCards({
  rows,
  onPick,
  query: queryProp,
  onQueryChange,
  sort: sortProp,
  onSortChange,
  viewportHeight,
}: TorrentCardsProps) {
  const filterId = useId()
  const sortId = useId()
  const [ownQuery, setOwnQuery] = useState('')
  const [ownSort, setOwnSort] = useState<TorrentSort>(null)
  const query = queryProp ?? ownQuery
  const setQuery = onQueryChange ?? setOwnQuery
  const sort = sortProp === undefined ? ownSort : sortProp
  const setSort = onSortChange ?? setOwnSort

  const shown = useMemo(() => arrangeRows(rows, query, sort), [rows, query, sort])
  const { boxRef, onScroll, start, end, padTop, padBottom } = useRowWindow(
    shown.length,
    viewportHeight,
    CARD_HEIGHT,
  )

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-rule px-3 py-2">
        <label htmlFor={filterId} className="sr-only">
          Filter torrents
        </label>
        <input
          id={filterId}
          data-torrent-filter
          type="search"
          placeholder="Filter by name"
          value={query}
          onChange={(event) => {
            setQuery(event.target.value)
          }}
          className={`${FIELD} min-w-0 flex-1 basis-40`}
        />
        <label htmlFor={sortId} className="sr-only">
          Sort torrents
        </label>
        <select
          id={sortId}
          value={sort === null ? '' : sort.column}
          onChange={(event) => {
            const column = event.target.value
            setSort(
              column === ''
                ? null
                : { column: column as SortColumn, reverse: sort?.reverse ?? false },
            )
          }}
          className={`${FIELD} cursor-pointer`}
        >
          <option value="">Server order</option>
          {SORT_COLUMNS.map((column) => (
            <option key={column.key} value={column.key}>
              {column.label}
            </option>
          ))}
        </select>
        <button
          type="button"
          aria-pressed={sort?.reverse ?? false}
          disabled={sort === null}
          onClick={() => {
            if (sort !== null) setSort({ column: sort.column, reverse: !sort.reverse })
          }}
          className="box-border min-h-11 cursor-pointer rounded-control border border-rule px-3 text-[13px] text-body aria-pressed:border-accent-light aria-pressed:text-strong disabled:cursor-default disabled:opacity-55"
        >
          Reverse
        </button>
      </div>
      <div ref={boxRef} onScroll={onScroll} className="min-h-0 flex-1 overflow-y-auto px-3 pt-3">
        <ul className="m-0 list-none p-0">
          {padTop > 0 && <li aria-hidden="true" data-spacer="top" style={{ height: padTop }} />}
          {shown.slice(start, end).map((row) => {
            const name = rowName(row)
            const fraction = fractionOf(row)
            const speed = speedOf(row)
            const label = `${row.rowState} ${formatPercent(fraction)}`
            return (
              <li key={row.id} className="box-border pb-2" style={{ height: CARD_HEIGHT }}>
                <button
                  type="button"
                  data-card={row.id}
                  onClick={() => {
                    onPick(row.id)
                  }}
                  className="box-border flex h-full w-full cursor-pointer flex-col justify-center gap-1.5 overflow-hidden rounded-xl border border-rule bg-ink-800 p-3 text-left"
                >
                  <span className="truncate text-[13px] text-strong">{name}</span>
                  <span
                    role="progressbar"
                    aria-label="Progress"
                    aria-valuemin={0}
                    aria-valuemax={100}
                    aria-valuenow={filledPercent(fraction)}
                    className="block h-1 w-full shrink-0 overflow-hidden rounded-full bg-rule"
                  >
                    <span
                      className={`block h-full ${BAR_BG[row.rowState]}`}
                      style={{ width: `${String(filledPercent(fraction))}%` }}
                    />
                  </span>
                  <span className="flex justify-between gap-2 text-xs">
                    <span className={STATE_TEXT[row.rowState]}>{label}</span>
                    {speed !== null && <span className="font-mono text-muted">{speed}</span>}
                  </span>
                </button>
              </li>
            )
          })}
          {shown.length === 0 && (
            <li className="py-2 text-[13px] text-muted">
              {query.trim() === '' ? 'No torrents.' : 'No torrents match.'}
            </li>
          )}
          {padBottom > 0 && (
            <li aria-hidden="true" data-spacer="bottom" style={{ height: padBottom }} />
          )}
        </ul>
      </div>
    </div>
  )
}
