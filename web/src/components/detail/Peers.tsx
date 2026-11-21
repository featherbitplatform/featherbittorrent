/**
 * The Peers tab: who the selected torrent is talking to, and what moves with each of them.
 *
 * Address, client, and the two rates, as the prototype lays them - the TUI's own peer list with
 * its per-column-shape ladder dropped, because a web pane scrolls sideways instead of dropping
 * columns to fit. A peer that has not introduced itself has no client name, and the list shows
 * blank for it: a dash would be the peer making a claim it never made. Addresses and client names
 * are remote text and go through `displaySafe`. Rates use the same formatter as the table above.
 *
 * The windowed 28 px rows are shared with the other pane lists (`window.ts`): a popular torrent
 * can hold hundreds of peers at once and the pane draws a window of them, not all of them.
 *
 * @module components/detail/Peers
 */
import type { PeerInfo } from '../../api/client'
import { displaySafe, formatRate } from '../../format'
import { DETAIL_ROW_HEIGHT, DETAIL_ROW_RULE, useRowWindow } from './window'

export interface PeersProps {
  peers: PeerInfo[]
  /** The scroll box's height in pixels. Given by tests because jsdom lays nothing out. */
  viewportHeight?: number
}

export default function Peers({ peers, viewportHeight }: PeersProps) {
  const { boxRef, onScroll, start, end, padTop, padBottom } = useRowWindow(
    peers.length,
    viewportHeight,
  )

  return (
    <div ref={boxRef} onScroll={onScroll} className="max-h-56 overflow-auto">
      <table className="w-full min-w-[560px] border-separate border-spacing-0">
        <caption className="sr-only">Peers of the selected torrent</caption>
        <thead>
          <tr className="text-[12px] text-muted">
            <th scope="col" className="px-5 py-2 text-left font-medium">
              Address
            </th>
            <th scope="col" className="px-3 py-2 text-left font-medium">
              Client
            </th>
            <th scope="col" className="px-3 py-2 text-right font-medium">
              <span aria-hidden="true">{'\u2193'}</span>
              <span className="sr-only">Down</span>
            </th>
            <th scope="col" className="px-5 py-2 text-right font-medium">
              <span aria-hidden="true">{'\u2191'}</span>
              <span className="sr-only">Up</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {padTop > 0 && (
            <tr data-spacer="top" aria-hidden="true" style={{ height: padTop }}>
              <td colSpan={4} />
            </tr>
          )}
          {peers.slice(start, end).map((peer) => {
            const addr = displaySafe(peer.addr)
            const client = peer.client === null || peer.client === undefined ? '' : displaySafe(peer.client)
            return (
              <tr key={peer.addr} data-row={peer.addr} style={{ height: DETAIL_ROW_HEIGHT }}>
                <td
                  className={`box-border h-7 max-w-[300px] truncate px-5 font-mono text-[12px] leading-4 text-body ${DETAIL_ROW_RULE}`}
                  title={addr}
                >
                  {addr}
                </td>
                <td
                  className={`box-border h-7 max-w-[220px] truncate px-3 font-mono text-[12px] leading-4 text-body ${DETAIL_ROW_RULE}`}
                  title={client}
                >
                  {client}
                </td>
                <td
                  className={`box-border h-7 truncate px-3 text-right font-mono text-[12px] leading-4 text-body ${DETAIL_ROW_RULE}`}
                >
                  {formatRate(peer.download_rate)}
                </td>
                <td
                  className={`box-border h-7 truncate px-5 text-right font-mono text-[12px] leading-4 text-body ${DETAIL_ROW_RULE}`}
                >
                  {formatRate(peer.upload_rate)}
                </td>
              </tr>
            )
          })}
          {peers.length === 0 && (
            <tr>
              <td colSpan={4} className="px-5 py-3 text-[12px] text-muted">
                No peers.
              </td>
            </tr>
          )}
          {padBottom > 0 && (
            <tr data-spacer="bottom" aria-hidden="true" style={{ height: padBottom }}>
              <td colSpan={4} />
            </tr>
          )}
        </tbody>
      </table>
    </div>
  )
}
