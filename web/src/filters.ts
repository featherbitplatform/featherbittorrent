/**
 * Which narrowing a row belongs to, and how many rows each narrowing holds.
 *
 * The buckets are the prototype's sidebar buttons: All, Downloading, Seeding, Stalled, Paused,
 * Errors. The row states with no button of their own - Queued, Metadata, Checking - count under
 * All only, exactly as `row_state`'s mapping hands them to the terminal without naming a colour
 * for them (`crates/fbtt-tui/src/view/mod.rs`). This lives apart from the filter buttons so the
 * component file exports exactly one component and nothing else.
 *
 * @module filters
 */
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
