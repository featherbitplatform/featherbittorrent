/**
 * The pane under the table: what the selected torrent is made of, one tab at a time.
 *
 * It is the detail pane of `crates/fbtt-tui/src/view/detail.rs` carried to the browser: the same
 * four tabs in the same order, the same file/peer/tracker columns and their rules (pad files
 * hidden, `ok` or the tracker's own reason for the status, `seeders/leechers` with a dash for what
 * was never reported), the same piece picture with the exact count above it - and read-only, as
 * milestone 1 promises: nothing here sends a request that changes a torrent, and the "Play in
 * browser" button the prototype drew waits for the milestone that can act on it. The tab strip
 * carries the torrent it is describing on its right, so the pane never shows a list for a torrent
 * nobody asked about, and a way to close: that clears the selection up in the screen.
 *
 * Reading follows the page's own clock, not a timer of its own: the screen bumps `tick` once per
 * live round of `startLive`, and the pane re-reads the tab it is showing on that bump - one second
 * while the page is up, ten while it is hidden, paused and backed off exactly when the server is,
 * because the pane only ever reads when a list round just succeeded. Switching tabs or torrents
 * fetches at once, and only the tab being shown is ever fetched.
 *
 * Answers are guarded against becoming history mid-flight: each request is checked against a
 * counter the next request - or the unmount, or any change of torrent or tab - moves before the
 * answer lands, so a slow reply about a torrent nobody is looking at any more never paints. A
 * failure that is not a `404` keeps the last good data under a small inline error; a `401` is the
 * client's and the session store's business, not the pane's. A `404` is the pane closing: the
 * torrent was removed somewhere else, so the pane says "That torrent was removed." through the
 * page's polite region - the region outlives the pane that raised the news - clears the selection
 * through `onRemoved`, and never sends another detail request for that id.
 *
 * @module components/DetailPane
 */
import { useEffect, useRef, useState } from 'react'
import { X } from 'lucide-react'
import Files from './detail/Files'
import Pieces from './detail/Pieces'
import Trackers from './detail/Trackers'
import Peers from './detail/Peers'
import { ApiError } from '../api/client'
import type { Api, FileInfo, PeerInfo, PiecesInfo, TrackerInfo } from '../api/client'
import { displaySafe } from '../format'
import { announcePolite, TORRENT_REMOVED_COPY } from '../live'

export interface DetailPaneProps {
  /** The torrent being described, or null when nothing is selected and there is no pane. */
  id: string | null
  /** The client to read through. */
  client: Api
  /** The page's heartbeat: bumped by the screen once per live refresh round. The pane reads the tab it is showing on each bump, so it rides the loop's cadence - and its pause when the page is hidden - instead of keeping a timer of its own. */
  tick?: number
  /** The selected torrent's name, drawn right-aligned on the tabs line. It is remote text; `displaySafe` runs here. */
  name?: string | null
  /** The close button: clears the selection up in the screen. */
  onDismiss?: () => void
  /** The pane closed itself over a `404`: the screen should stop selecting that torrent. */
  onRemoved?: (id: string) => void
  /** Where the removal notice is announced. Defaults to the page's polite live region (`live.ts`). */
  onNotice?: (message: string) => void
  /** The lists' scroll-box height, for the tests: jsdom lays nothing out. */
  viewportHeight?: number
}

/** The tabs, in the TUI's order and with the TUI's words. */
type Tab = 'files' | 'peers' | 'trackers' | 'pieces'

const TABS: { key: Tab; label: string }[] = [
  { key: 'files', label: 'Files' },
  { key: 'peers', label: 'Peers' },
  { key: 'trackers', label: 'Trackers' },
  { key: 'pieces', label: 'Pieces' },
]

/** The one call per tab. Only the tab being shown is ever asked - a pane that fetched all four on every tick would quadruple the traffic for data nobody is looking at. */
const FETCHERS: Record<Tab, (client: Api, id: string) => Promise<unknown>> = {
  files: (client, id) => client.files(id),
  peers: (client, id) => client.peers(id),
  trackers: (client, id) => client.trackers(id),
  pieces: (client, id) => client.pieces(id),
}

/** The last answer that won its request. `data` is the tab's shape; each render arm checks `tab` and reads it as such. */
interface Payload {
  requestId: string
  tab: Tab
  data: unknown
}

/** The last failure that won its request, keyed so a tab switch or a new torrent does not keep showing the old one's error. */
interface PaneError {
  requestId: string
  tab: Tab
  message: string
}

export default function DetailPane({
  id,
  client,
  tick,
  name,
  onDismiss,
  onRemoved,
  onNotice,
  viewportHeight,
}: DetailPaneProps) {
  const [tab, setTab] = useState<Tab>('files')
  const [payload, setPayload] = useState<Payload | null>(null)
  const [error, setError] = useState<PaneError | null>(null)
  // The id a `404` closed this pane over. It is state, not a ref behind the render, so the render
  // sees it: the pane draws nothing for that id and reads nothing for it, while the pane lives,
  // because a torrent that is gone does not un-remove itself between one second and the next.
  const [closedId, setClosedId] = useState<string | null>(null)

  // Which request is still current. Every new one bumps it, and the cleanup that ends a request
  // (a tab switch, a torrent switch, an unmount) bumps it too, so a late answer for a superseded
  // request is history, and history does not paint - not for the wrong torrent, not for the wrong
  // tab, not over a fresher answer to the same one.
  const seqRef = useRef(0)

  // The screen's callbacks live in refs so the request effect below keys on nothing but what a
  // read actually depends on - the client, the torrent, the tab, the tick, and whether this
  // torrent is the one a 404 already closed the pane over - and not on whether the screen chose
  // to inline a new arrow this render.
  const onRemovedRef = useRef(onRemoved)
  const onNoticeRef = useRef(onNotice ?? announcePolite)
  useEffect(() => {
    onRemovedRef.current = onRemoved
    onNoticeRef.current = onNotice ?? announcePolite
  })

  useEffect(() => {
    if (id === null || id === closedId) return
    const requestId = id
    const requestTab = tab
    const seq = (seqRef.current += 1)

    async function run(): Promise<void> {
      try {
        const data = await FETCHERS[requestTab](client, requestId)
        if (seqRef.current !== seq) return
        setPayload({ requestId, tab: requestTab, data })
        setError(null)
      } catch (cause) {
        if (seqRef.current !== seq) return
        if (cause instanceof ApiError && cause.status === 404) {
          // The torrent was removed elsewhere. Close the pane, say so once through the polite
          // region that outlives this component, and never ask for this id again.
          setClosedId(requestId)
          onNoticeRef.current(TORRENT_REMOVED_COPY)
          onRemovedRef.current?.(requestId)
          setPayload(null)
          setError(null)
          return
        }
        // Any other failure: keep the last good data (the payload stays) and say what did not work.
        const raw = cause instanceof Error ? cause.message : String(cause)
        setError({ requestId, tab: requestTab, message: displaySafe(raw) })
      }
    }
    void run()

    return () => {
      // This request ends as far as this component is concerned; anything that supersedes it has
      // already moved the counter its answer will be checked against.
      seqRef.current += 1
    }
  }, [client, id, tab, tick, closedId])

  if (id === null || id === closedId) return null

  const nameText = name === null || name === undefined ? '' : displaySafe(name)
  const errorText =
    error !== null && error.requestId === id && error.tab === tab ? error.message : null
  // Only the answer that belongs to the torrent and tab on screen right now may paint: an answer
  // won for another torrent is not shown under this one even while the new read is still in flight.
  const live =
    payload !== null && payload.requestId === id && payload.tab === tab ? payload : null

  return (
    <section aria-label="Details" className="shrink-0 border-t border-rule bg-sunken">
      <div className="flex flex-wrap items-center gap-1 border-b border-ink-700 px-4 py-2">
        <div role="tablist" aria-label="Detail tabs" className="flex flex-wrap gap-1">
          {TABS.map((item) => (
            <button
              key={item.key}
              type="button"
              role="tab"
              id={`detail-tab-${item.key}`}
              aria-controls={`detail-panel-${item.key}`}
              aria-selected={tab === item.key}
              onClick={() => {
                setTab(item.key)
              }}
              className={`h-8 cursor-pointer rounded-control px-3 text-[13px] ${
                tab === item.key
                  ? 'bg-accent-soft text-violet-300'
                  : 'bg-transparent text-body hover:text-strong'
              }`}
            >
              {item.label}
            </button>
          ))}
        </div>
        <span
          className="ml-auto min-w-0 max-w-[420px] truncate text-[13px] text-muted"
          title={nameText}
        >
          {nameText}
        </span>
        <button
          type="button"
          aria-label="Close details"
          onClick={onDismiss}
          className="flex h-8 w-8 cursor-pointer items-center justify-center rounded-control text-muted hover:bg-ink-750 hover:text-strong"
        >
          <X className="h-4 w-4" aria-hidden="true" />
        </button>
      </div>

      <div
        role="tabpanel"
        id={`detail-panel-${tab}`}
        aria-labelledby={`detail-tab-${tab}`}
        className="min-w-0"
      >
        {errorText !== null && (
          <p className="px-5 pt-2 font-mono text-[12px] text-error">{errorText}</p>
        )}
        {live === null && errorText === null && (
          <p className="px-5 py-3 text-[12px] text-muted">Loading...</p>
        )}
        {live !== null && tab === 'files' && (
          <Files files={live.data as FileInfo[]} viewportHeight={viewportHeight} />
        )}
        {live !== null && tab === 'peers' && (
          <Peers peers={live.data as PeerInfo[]} viewportHeight={viewportHeight} />
        )}
        {live !== null && tab === 'trackers' && (
          <Trackers trackers={live.data as TrackerInfo[]} viewportHeight={viewportHeight} />
        )}
        {live !== null && tab === 'pieces' && <Pieces pieces={live.data as PiecesInfo} />}
      </div>
    </section>
  )
}
