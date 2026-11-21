/**
 * The Torrents screen: the workspace a signed-in person (or a server with logins off) lands on.
 *
 * The layout is the Torrents prototype at full width: the header strip, a sidebar of state filters
 * on the left, and the table with its toolbar beside it. Below 768 px the flex row wraps and the
 * sidebar becomes a scrolling row above the table; that is one wrapper `flex-wrap` and the
 * `max-md:` classes in `StateFilters`, not a media query in JS. The table scrolls sideways in its
 * own box at every width, the way the prototype draws it.
 *
 * The data comes from `startLive`: one second while the page is up, ten while it is hidden, early
 * on the event stream, and on a doubling pause when the server stops answering. A lost server is
 * the header pill's news, not the table's: the last rows stay while the loop backs off. The
 * version and the DHT count ride the session row, re-read every half minute and on mount; they are
 * header decoration, and they do not earn a place in the once-a-second round.
 *
 * The header's Add torrent button answers nothing yet: the dialog comes with the actions task. The
 * selected row is remembered here because the detail pane that will use it mounts beside this
 * screen in the next task.
 *
 * @module screens/Torrents
 */
import { useEffect, useMemo, useState } from 'react'
import Banner from '../components/Banner'
import Header from '../components/Header'
import StateFilters from '../components/StateFilters'
import TorrentTable from '../components/TorrentTable'
import { api } from '../api/client'
import type { Api, SessionInfo } from '../api/client'
import { startLive } from '../live'
import type { LinkState, TorrentRow } from '../live'
import { filterCounts, matchesFilter } from '../filters'
import type { FilterKey } from '../filters'

export interface TorrentsProps {
  /** Whether there is a session to end: a server with logins off has none. */
  signedIn: boolean
  /** Why the last sign-out attempt did not take, or null. The session is still live in that case, and the table keeps running under the notice. */
  signOutError: string | null
  onSignOut(): void
  /** The client to read through. Defaults to the page's one so tests can hand in a fake. */
  client?: Api
}

/** The stream this screen wakes on: the `status` category, and nothing else (the payload is never read; see `live.ts`). */
const EVENTS_URL = '/api/v1/events?categories=status'

/** How often the session row is re-read for the header's version and DHT count. It is decoration, not the live table; the refresh model puts the once-a-second cadence on the list. */
const SESSION_POLL_MS = 30_000

export default function Torrents({
  signedIn,
  signOutError,
  onSignOut,
  client = api,
}: TorrentsProps) {
  const [rows, setRows] = useState<TorrentRow[]>([])
  const [link, setLink] = useState<LinkState>({ phase: 'live' })
  const [filter, setFilter] = useState<FilterKey>('all')
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [sessionInfo, setSessionInfo] = useState<SessionInfo | null>(null)

  useEffect(() => {
    const live = startLive(client, setRows, setLink, {
      now: () => Date.now(),
      setTimeout: (fn: () => void, ms: number) => {
        setTimeout(fn, ms)
      },
      visibility: () => (document.visibilityState === 'hidden' ? 'hidden' : 'visible'),
      // The cookie authenticates the stream; it is same-origin, so there is nothing to add to it.
      // An environment without EventSource (a test jsdom) simply gets no stream.
      eventSource: () => (typeof EventSource === 'function' ? new EventSource(EVENTS_URL) : null),
    })
    return live.stop
  }, [client])

  useEffect(() => {
    let stopped = false
    let timer: ReturnType<typeof setTimeout> | undefined
    async function poll(): Promise<void> {
      try {
        const info = await client.session()
        if (!stopped) setSessionInfo(info)
      } catch {
        // The header keeps what it last knew; the next poll may know better.
      }
      if (!stopped) timer = setTimeout(poll, SESSION_POLL_MS)
    }
    void poll()
    return () => {
      stopped = true
      if (timer !== undefined) clearTimeout(timer)
    }
  }, [client])

  const shown = useMemo(() => rows.filter((row) => matchesFilter(row, filter)), [rows, filter])
  const counts = useMemo(() => filterCounts(rows), [rows])
  const totals = useMemo(() => {
    let down = 0
    let up = 0
    for (const row of rows) {
      down += row.download_rate
      up += row.upload_rate
    }
    return { down, up }
  }, [rows])

  return (
    <div className="flex min-h-dvh flex-col">
      <Header
        version={sessionInfo?.version ?? null}
        link={link}
        totalDown={totals.down}
        totalUp={totals.up}
        dhtNodes={sessionInfo?.dht_nodes ?? null}
        signedIn={signedIn}
        onSignOut={onSignOut}
      />
      {signOutError !== null && (
        <div className="px-5 pt-3">
          <Banner tone="danger">{signOutError}</Banner>
        </div>
      )}
      <div className="flex min-h-0 flex-1 flex-wrap">
        <StateFilters counts={counts} active={filter} onChange={setFilter} />
        <main className="flex min-h-0 min-w-0 flex-1 flex-col">
          <TorrentTable rows={shown} selectedId={selectedId} onPick={setSelectedId} />
        </main>
      </div>
    </div>
  )
}
