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
 * nobody asked about, and a way to close: that clears the selection up in the screen. The tabs
 * follow the WAI-ARIA tabs pattern for real: roving tabindex, Arrow Left/Right wrapping, Home and
 * End, one panel whose `aria-labelledby` names the selected tab.
 *
 * The pane takes its own share of the screen - 40 % of it, never less than 220 px, never more
 * than 60 % - so a tall list of torrents cannot squeeze it to its own tab strip while the table,
 * `flex-1 min-h-0` above it, scrolls inside what is left.
 *
 * Reading follows the page's own clock, not a timer of its own: the screen bumps `tick` once per
 * live round of `startLive`, and the pane re-reads the tab it is showing on that bump - one second
 * while the page is up, ten while it is hidden, paused and backed off exactly when the server is,
 * because the pane only ever reads when a list round just succeeded. A tick while a read of the
 * same torrent and tab is still in flight starts nothing and supersedes nothing: an answer that
 * takes longer than the tick (a big torrent's pieces, a slow link) always arrives and paints. The
 * sequence that discards history moves only when a genuinely new question is asked - a different
 * torrent, a different tab.
 *
 * A torrent that is known but whose metadata has not arrived (`409 no_metadata`, magnets waiting
 * for their file list) is a wait, not a failure: the pane says "Waiting for the torrent's
 * metadata." in the muted text, no error colour, and keeps asking on the ticks until the tab
 * fills itself in. A failure that is not a `404` keeps the last good data under a small inline
 * note; a `401` is the client's and the session store's business, not the pane's. A `404` is the
 * pane closing: the torrent was removed somewhere else, so the pane says "That torrent was
 * removed." through the page's polite region - the region outlives the pane that raised the news -
 * clears the selection through `onRemoved`, and never sends another detail request for that id.
 *
 * @module components/DetailPane
 */
import { useEffect, useRef, useState } from 'react'
import { X } from 'lucide-react'
import Files from './detail/Files'
import Player from '../player/Player'
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

/** The id every tab's `aria-controls` names, and the panel carries: there is exactly one panel, and it shows whichever tab is chosen. */
const PANEL_ID = 'detail-panel'

/** The server's own copy is its lowercase problem text; this is what a person reads while the magnets fill themselves in. */
const METADATA_WAITING = "Waiting for the torrent's metadata."

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

/** The last thing that stopped this key from drawing, keyed so a tab switch or a new torrent does not inherit the old one's note. `waiting` is the metadata pause: the same slot, the muted colour instead of the error one. */
interface PaneNote {
  requestId: string
  tab: Tab
  tone: 'error' | 'waiting'
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
  const [note, setNote] = useState<PaneNote | null>(null)
  // The id a `404` closed this pane over. It is state, not a ref behind the render, so the render
  // sees it: the pane draws nothing for that id and reads nothing for it, while the pane lives,
  // because a torrent that is gone does not un-remove itself between one second and the next.
  const [closedId, setClosedId] = useState<string | null>(null)
  // The file the player is open on, or nothing. The player is mounted only while it is open, the
  // way every dialog here is, and it unmounts itself through `onClose` the way they do.
  const [playing, setPlaying] = useState<{ file: number; name: string } | null>(null)

  // Which request is still current. The counter moves only when a genuinely new question is
  // asked - a new torrent, a new tab, or a new poll of a question already answered. A tick while
  // a read of the same (torrent, tab) is in flight neither starts a second read nor supersedes
  // the one running, so an answer slower than the tick always arrives and paints.
  const seqRef = useRef(0)
  // The read currently in flight, as the (torrent, tab) it belongs to and the sequence it must
  // still match to paint. Cleared the moment its answer wins.
  const inflightRef = useRef<{ requestId: string; tab: Tab; seq: number } | null>(null)
  // The pane is unmounted: nothing late may paint, announce, or blame the screen any more.
  const stoppedRef = useRef(false)
  useEffect(() => {
    stoppedRef.current = false
    return () => {
      stoppedRef.current = true
    }
  }, [])

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
    const running = inflightRef.current
    if (running !== null && running.requestId === requestId && running.tab === requestTab) {
      // A tick, not a new question: the read already in flight is the answer this tick wants too.
      // Starting a second one would only make the next round discard the answer a second earlier.
      return
    }
    const seq = (seqRef.current += 1)
    const record = { requestId, tab: requestTab, seq }
    inflightRef.current = record

    async function run(): Promise<void> {
      try {
        const data = await FETCHERS[requestTab](client, requestId)
        if (stoppedRef.current || seqRef.current !== seq) return
        if (inflightRef.current === record) inflightRef.current = null
        setPayload({ requestId, tab: requestTab, data })
        setNote(null)
      } catch (cause) {
        if (stoppedRef.current || seqRef.current !== seq) return
        if (inflightRef.current === record) inflightRef.current = null
        if (cause instanceof ApiError && cause.status === 404) {
          // The torrent was removed elsewhere. Close the pane, say so once through the polite
          // region that outlives this component, and never ask for this id again.
          setClosedId(requestId)
          onNoticeRef.current(TORRENT_REMOVED_COPY)
          onRemovedRef.current?.(requestId)
          setPayload(null)
          setNote(null)
          return
        }
        if (cause instanceof ApiError && cause.code === 'no_metadata') {
          // Metadata that has not arrived yet is a wait, not a failure: muted words, no error
          // colour, and the ticks keep asking until the tab fills itself in.
          setNote({ requestId, tab: requestTab, tone: 'waiting', message: METADATA_WAITING })
          return
        }
        // Any other failure: keep the last good data (the payload stays) and say what did not work.
        const raw = cause instanceof Error ? cause.message : String(cause)
        setNote({ requestId, tab: requestTab, tone: 'error', message: displaySafe(raw) })
      }
    }
    void run()
  }, [client, id, tab, tick, closedId])

  if (id === null || id === closedId) return null

  const nameText = name === null || name === undefined ? '' : displaySafe(name)
  const liveNote = note !== null && note.requestId === id && note.tab === tab ? note : null
  // Only the answer that belongs to the torrent and tab on screen right now may paint: an answer
  // won for another torrent is not shown under this one even while the new read is still in flight.
  const live =
    payload !== null && payload.requestId === id && payload.tab === tab ? payload : null

  function moveTabs(event: {
    key: string
    preventDefault(): void
    currentTarget: HTMLDivElement
    target: EventTarget
  }): void {
    // Only the tabs themselves answer the arrows. The Close button sits inside the tablist too, and
    // arrows on a focused button belong to that button; switching tabs behind its back would make the
    // arrow keys leave it wherever they found it.
    const target = event.target
    if (!(target instanceof HTMLElement) || target.getAttribute('role') !== 'tab') return
    const order = TABS.map((item) => item.key)
    const current = order.indexOf(tab)
    let next: Tab | null = null
    if (event.key === 'ArrowRight') next = order[(current + 1) % order.length]
    else if (event.key === 'ArrowLeft') next = order[(current - 1 + order.length) % order.length]
    else if (event.key === 'Home') next = order[0]
    else if (event.key === 'End') next = order[order.length - 1]
    if (next === null) return
    event.preventDefault()
    setTab(next)
    // Roving tabindex: the tab that took the choice takes the focus too, so the arrows read as
    // one motion through the strip, the way `Tab` walks the TUI's tabs line.
    const button = event.currentTarget.querySelector(`[id="detail-tab-${next}"]`)
    if (button !== null) (button as HTMLElement).focus()
  }

  return (
    <section
      aria-label="Details"
      className="flex min-h-0 shrink-0 flex-col overflow-hidden border-t border-rule bg-sunken h-[40%] max-h-[60%] min-h-[220px]"
    >
      <div
        role="tablist"
        aria-label="Detail tabs"
        onKeyDown={moveTabs}
        className="flex flex-wrap items-center gap-1 border-b border-ink-700 px-4 py-2"
      >
        {TABS.map((item) => (
          <button
            key={item.key}
            type="button"
            role="tab"
            id={`detail-tab-${item.key}`}
            aria-controls={PANEL_ID}
            aria-selected={tab === item.key}
            tabIndex={tab === item.key ? 0 : -1}
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
        id={PANEL_ID}
        aria-labelledby={`detail-tab-${tab}`}
        className="flex min-h-0 min-w-0 flex-1 flex-col"
      >
        {liveNote !== null && (
          <p
            className={`shrink-0 px-5 pt-2 font-mono text-[12px] ${liveNote.tone === 'waiting' ? 'text-muted' : 'text-error'}`}
          >
            {liveNote.message}
          </p>
        )}
        {live === null && liveNote === null && (
          <p className="shrink-0 px-5 py-3 text-[12px] text-muted">Loading...</p>
        )}
        {live !== null && tab === 'files' && (
          <Files
            key={id}
            files={live.data as FileInfo[]}
            viewportHeight={viewportHeight}
            id={id}
            client={client}
            onPlay={(index) => {
              const file = (live.data as FileInfo[]).find((entry) => entry.index === index)
              if (file !== undefined) setPlaying({ file: index, name: file.path })
            }}
          />
        )}
        {live !== null && tab === 'peers' && (
          <Peers peers={live.data as PeerInfo[]} viewportHeight={viewportHeight} />
        )}
        {live !== null && tab === 'trackers' && (
          <Trackers trackers={live.data as TrackerInfo[]} viewportHeight={viewportHeight} />
        )}
        {live !== null && tab === 'pieces' && <Pieces pieces={live.data as PiecesInfo} />}
      </div>

      {playing !== null && (
        <Player
          key={`${id}:${playing.file}`}
          id={id}
          file={playing.file}
          name={playing.name}
          client={client}
          onClose={() => {
            setPlaying(null)
          }}
        />
      )}
    </section>
  )
}
