/**
 * The phone's torrent list: one card per torrent, the owner's option A.
 *
 * Each card is a single button (the whole card is the tap target, well over 44 px tall) holding the
 * name on one line with an ellipsis, a progress bar in the state colour, the state with its percent,
 * and the one speed that matters: the download rate while downloading, the upload rate while
 * seeding, the peer count when stalled. Everything else a torrent can be (paused, queued, errored)
 * shows no speed. Names are remote text and go through `displaySafe`.
 *
 * @module components/TorrentCards
 */
import { displaySafe, filledPercent, formatPercent, formatRate } from '../format'
import type { RowState, TorrentRow } from '../live'

export interface TorrentCardsProps {
  rows: TorrentRow[]
  /** The card that was tapped. */
  onPick(id: string): void
}

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

function fractionOf(row: TorrentRow): number {
  return row.rowState === 'checking' ? (row.checking_progress ?? 0) : row.progress
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

export default function TorrentCards({ rows, onPick }: TorrentCardsProps) {
  return (
    <ul className="m-0 flex min-h-0 flex-1 list-none flex-col gap-2 overflow-y-auto p-3">
      {rows.map((row) => {
        const name = displaySafe(row.name) || displaySafe(row.id)
        const fraction = fractionOf(row)
        const speed = speedOf(row)
        const label =
          row.rowState === 'seeding' ? 'seeding' : `${row.rowState} ${formatPercent(fraction)}`
        return (
          <li key={row.id}>
            <button
              type="button"
              data-card={row.id}
              onClick={() => {
                onPick(row.id)
              }}
              className="flex min-h-[44px] w-full cursor-pointer flex-col gap-1.5 rounded-xl border border-rule bg-ink-800 p-3 text-left"
            >
              <span className="truncate text-[13px] text-strong">{name}</span>
              <span
                role="progressbar"
                aria-label="Progress"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={filledPercent(fraction)}
                className="block h-1 w-full overflow-hidden rounded-full bg-rule"
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
    </ul>
  )
}
