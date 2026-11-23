/**
 * Which narrowing a row belongs to, and how many rows each narrowing holds.
 *
 * The buckets are the prototype's sidebar buttons: All, Downloading, Seeding, Stalled, Paused,
 * Errors. The row states with no button of their own - Queued, Metadata, Checking - count under
 * All only, exactly as `row_state`'s mapping hands them to the terminal without naming a colour
 * for them (`crates/fbtt-tui/src/view/mod.rs`). This lives apart from the filter buttons so the
 * component file exports exactly one component and nothing else.
 *
 * The text filter and the sort live here too: the screen owns them, and the desktop table and the
 * phone cards both arrange their rows through `arrangeRows`, so a query typed on one is the query
 * the other shows after a resize.
 *
 * @module filters
 */
import { displaySafe } from './format'
import type { RowState, TorrentRow } from './live'

/** The six narrowings the table offers. The row states with no bucket of their own live under All. */
export type FilterKey = 'all' | 'downloading' | 'seeding' | 'stalled' | 'paused' | 'errors'

/** Which bucket a row's state counts in, or null when only All will ever show it. */
export function filterBucket(rowState: RowState): FilterKey | null {
  switch (rowState) {
    case 'downloading':
    case 'seeding':
    case 'stalled':
    case 'paused':
      return rowState
    case 'error':
      return 'errors'
    default:
      return null
  }
}

/** Whether a row passes the chosen filter. */
export function matchesFilter(row: TorrentRow, filter: FilterKey): boolean {
  return filter === 'all' || filterBucket(row.rowState) === filter
}

/** How many rows each filter holds, counted over every row, whatever the screen currently shows. */
export function filterCounts(rows: TorrentRow[]): Record<FilterKey, number> {
  const counts: Record<FilterKey, number> = {
    all: rows.length,
    downloading: 0,
    seeding: 0,
    stalled: 0,
    paused: 0,
    errors: 0,
  }
  for (const row of rows) {
    const bucket = filterBucket(row.rowState)
    if (bucket !== null) counts[bucket] += 1
  }
  return counts
}

/** The columns a list can be ordered by, with the words the table header and the phone's sort choice show. */
export type SortColumn = 'name' | 'progress' | 'down' | 'up' | 'peers' | 'state'

export const SORT_COLUMNS: readonly { key: SortColumn; label: string }[] = [
  { key: 'name', label: 'Name' },
  { key: 'progress', label: 'Progress' },
  { key: 'down', label: 'Down' },
  { key: 'up', label: 'Up' },
  { key: 'peers', label: 'Peers' },
  { key: 'state', label: 'State' },
]

/** The order a list is in: a column and its direction, or null for the order the server listed. */
export type TorrentSort = { column: SortColumn; reverse: boolean } | null

/** The name as it is drawn: display-safe, and the hash when the server had no name to give (a magnet whose metadata has not arrived). */
export function rowName(row: TorrentRow): string {
  return displaySafe(row.name) || displaySafe(row.id)
}

/** The fraction a row's bar and percent carry. A torrent hash-checking reports its fraction in the state rather than in `progress`, which for one being checked over again says nothing useful. */
export function rowFraction(row: TorrentRow): number {
  return row.rowState === 'checking' ? (row.checking_progress ?? 0) : row.progress
}

/** The value a column sorts on, so the order and the rows' contents agree. */
function sortValue(row: TorrentRow, column: SortColumn): number | string {
  switch (column) {
    case 'name':
      return rowName(row).toLowerCase()
    case 'progress':
      return rowFraction(row)
    case 'down':
      return row.download_rate
    case 'up':
      return row.upload_rate
    case 'peers':
      return row.num_peers
    case 'state':
      return row.rowState
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

/** The rows whose display-safe name holds `query` (any case), in the order `sort` asks for. */
export function arrangeRows(rows: TorrentRow[], query: string, sort: TorrentSort): TorrentRow[] {
  const needle = query.trim().toLowerCase()
  const kept =
    needle === '' ? [...rows] : rows.filter((row) => rowName(row).toLowerCase().includes(needle))
  if (sort !== null) {
    const factor = sort.reverse ? -1 : 1
    kept.sort((a, b) => factor * compareRows(a, b, sort.column))
  }
  return kept
}
