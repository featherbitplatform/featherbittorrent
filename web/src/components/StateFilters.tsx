/**
 * The state filters: what the table can be narrowed to, and how many rows each narrowing holds.
 *
 * The list is the Torrents prototype's sidebar: a section label, then one button per state with a
 * colour dot and a count in mono. The colours are the terminal's state colours (Downloading the
 * accent, Seeding green, Stalled amber, Errors red, everything else muted) and the counts follow
 * `row_state` exactly, which includes the part of the plan the prototype cannot show: Queued,
 * Metadata and Checking rows count under All only, because none of the five named buckets is
 * theirs.
 *
 * Below 768 px the whole sidebar stacks above the table as a scrolling row. That is the one layout
 * rule this component owns, and it lives in the classes on the `nav`: mobile-first, it is a row of
 * buttons with `overflow-x-auto` so six of them fit a phone, and `md:` turns it into the wide
 * column with its right-hand rule. The screen gives the table its bounded height, so the box under
 * it is what scrolls at every width. The active pill is the soft violet wash the prototype gives
 * the chosen filter; it never moves.
 *
 * Which row counts in which bucket is not this file's business - it lives in `filters.ts`, next
 * to the rule it copies from the terminal.
 *
 * @module components/StateFilters
 */
import type { FilterKey } from '../filters'

const FILTERS: { key: FilterKey; label: string; dot: string }[] = [
  { key: 'all', label: 'All', dot: 'bg-body' },
  { key: 'downloading', label: 'Downloading', dot: 'bg-violet-500' },
  { key: 'seeding', label: 'Seeding', dot: 'bg-seeding' },
  { key: 'stalled', label: 'Stalled', dot: 'bg-stalled' },
  { key: 'paused', label: 'Paused', dot: 'bg-muted' },
  { key: 'errors', label: 'Errors', dot: 'bg-error' },
]

export interface StateFiltersProps {
  counts: Record<FilterKey, number>
  active: FilterKey
  onChange(filter: FilterKey): void
}

export default function StateFilters({ counts, active, onChange }: StateFiltersProps) {
  return (
    <nav
      aria-label="Torrents"
      className={
        'flex shrink-0 flex-row items-center gap-1 overflow-x-auto border-b border-rule bg-sunken px-2 py-2 ' +
        'md:w-60 md:basis-auto md:flex-col md:gap-px md:border-r md:border-b-0 md:px-3 md:py-4'
      }
    >
      <span className="shrink-0 px-2.5 py-1.5 text-[11px] uppercase tracking-[0.06em] text-muted">
        Torrents
      </span>
      {FILTERS.map(({ key, label, dot }) => (
        <button
          key={key}
          type="button"
          aria-pressed={active === key}
          onClick={() => onChange(key)}
          className={
            'flex min-h-9 shrink-0 cursor-pointer items-center justify-between gap-2.5 rounded-control px-2.5 py-2 ' +
            'text-left text-[13px] transition-colors ' +
            (active === key ? 'bg-accent-soft text-strong' : 'text-body hover:bg-ink-700')
          }
        >
          <span className="flex items-center gap-2.5 whitespace-nowrap">
            <span aria-hidden="true" className={`h-2 w-2 flex-none rounded-full ${dot}`} />
            {label}
          </span>
          <span className="font-mono text-xs text-muted">{counts[key]}</span>
        </button>
      ))}
    </nav>
  )
}
