/**
 * The Trackers tab: what the last announce to each tracker did.
 *
 * The wording and the colours are the TUI's (`trackers_table` in `fbtt-tui/src/view/detail.rs`):
 * `ok` is what a tracker that accepted the announce says, and anything else is that tracker's own
 * reason, drawn in the error colour. The message column is the swarm its last reply reported,
 * `seeders/leechers`, and a count it never reported is a dash rather than a zero - 0 would be a
 * claim the tracker never made. URLs and error text are remote and go through `displaySafe`. The
 * dot beside each status is the prototype's, colouring a healthy tracker the seeding green.
 *
 * @module components/detail/Trackers
 */
import type { TrackerInfo } from '../../api/client'
import { displaySafe } from '../../format'
import { DETAIL_ROW_HEIGHT, DETAIL_ROW_RULE, useRowWindow } from './window'

export interface TrackersProps {
  trackers: TrackerInfo[]
  /** The scroll box's height in pixels. Given by tests because jsdom lays nothing out. */
  viewportHeight?: number
}

/** A count a tracker may not have reported: a dash rather than a zero, exactly the TUI's `count`. */
function count(value: number | null | undefined): string {
  return value === null || value === undefined ? '-' : String(value)
}

export default function Trackers({ trackers, viewportHeight }: TrackersProps) {
  const { boxRef, onScroll, start, end, padTop, padBottom } = useRowWindow(
    trackers.length,
    viewportHeight,
  )

  return (
    <div ref={boxRef} onScroll={onScroll} className="min-h-0 flex-1 overflow-auto">
      <table className="w-full min-w-[560px] border-separate border-spacing-0">
        <caption className="sr-only">Trackers of the selected torrent</caption>
        <thead>
          <tr className="text-[12px] text-muted">
            <th scope="col" className="px-5 py-2 text-left font-medium">
              Tracker
            </th>
            <th scope="col" className="px-3 py-2 text-left font-medium">
              Status
            </th>
            <th scope="col" className="px-5 py-2 text-right font-medium">
              Seeds/leechers
            </th>
          </tr>
        </thead>
        <tbody>
          {padTop > 0 && (
            <tr data-spacer="top" aria-hidden="true" style={{ height: padTop }}>
              <td colSpan={3} />
            </tr>
          )}
          {trackers.slice(start, end).map((tracker) => {
            const lastError = tracker.last_error ?? null
            const status = lastError === null ? 'ok' : displaySafe(lastError)
            const message = `${count(tracker.seeders)}/${count(tracker.leechers)}`
            const url = displaySafe(tracker.url)
            return (
              <tr key={`${tracker.tier}:${tracker.url}`} data-row={url} style={{ height: DETAIL_ROW_HEIGHT }}>
                <td
                  className={`box-border h-7 max-w-[360px] truncate px-5 font-mono text-[12px] leading-4 text-body ${DETAIL_ROW_RULE}`}
                  title={url}
                >
                  {url}
                </td>
                <td className={`box-border h-7 px-3 text-[12px] leading-4 ${DETAIL_ROW_RULE}`}>
                  {/* Block-level flex, not inline-flex: the empty dot in an inline box synthesises a
                      taller line box than the row, the trap the table above the pane already fell into. */}
                  <span
                    className={`flex h-4 items-center gap-2 ${lastError === null ? 'text-body' : 'text-error'}`}
                  >
                    <span
                      aria-hidden="true"
                      className={`h-1.5 w-1.5 flex-none rounded-full ${lastError === null ? 'bg-seeding' : 'bg-error'}`}
                    />
                    <span className="min-w-0 truncate font-mono">{status}</span>
                  </span>
                </td>
                <td
                  className={`box-border h-7 truncate px-5 text-right font-mono text-[12px] leading-4 text-muted ${DETAIL_ROW_RULE}`}
                >
                  {message}
                </td>
              </tr>
            )
          })}
          {trackers.length === 0 && (
            <tr>
              <td colSpan={3} className="px-5 py-3 text-[12px] text-muted">
                No trackers.
              </td>
            </tr>
          )}
          {padBottom > 0 && (
            <tr data-spacer="bottom" aria-hidden="true" style={{ height: padBottom }}>
              <td colSpan={3} />
            </tr>
          )}
        </tbody>
      </table>
    </div>
  )
}
