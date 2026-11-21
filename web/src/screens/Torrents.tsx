/**
 * The Torrents screen: the workspace a signed-in person (or a server with logins off) lands on.
 *
 * The layout is the Torrents prototype at full width: the header strip, a sidebar of state filters
 * on the left, and the table with its toolbar beside it. The screen is exactly one viewport tall
 * (`h-dvh`), every flex ancestor of the table box carries `min-h-0`, and the box itself is the one
 * thing that scrolls: left an ancestor unbounded and the box would grow to its content instead of
 * scrolling, which is how a windowed table ends up showing fourteen rows and a mile of spacer.
 * Below 768 px the column layout stacks the filters above the table; that is the `md:` classes in
 * `StateFilters`, not a media query in JS. The table scrolls sideways in its box at every width,
 * the way the prototype draws it.
 *
 * The data comes from `startLive`: one second while the page is up, ten while it is hidden, early
 * on the event stream or when the page comes back into view, and on a doubling pause when the
 * server stops answering - and while it backs off, nothing cuts in front of the pause. A lost
 * server is the header pill's news, not the table's: the last rows stay while the loop backs off.
 * The engine version behind the header's tooltip and the DHT count ride the session row, re-read
 * every half minute and on mount - the version the header displays instead ships inside the bundle,
 * injected at build time - and none of it earns a place in the once-a-second round.
 *
 * The header's Add torrent button answers nothing yet: the dialog comes with the actions task. The
 * selected row is remembered here because the detail pane under the table reads through it, and
 * the pane's heartbeat is this screen's: `startLive` bumps `tick` once per live round, and the pane
 * re-reads its tab on that bump, so it shares the loop's cadence, its hidden-page pause and its
 * backoff instead of keeping a timer of its own.
 *
 * @module screens/Torrents
 */
import { useEffect, useMemo, useState } from 'react'
import Banner from '../components/Banner'
import DetailPane from '../components/DetailPane'
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
  // Bumped once per live refresh round. It is the pane's clock: the detail pane re-reads its tab
  // when this moves, so the pane's cadence, hidden-page pause and backoff are exactly the table's,
  // with no second timer anywhere near the same server.
  const [tick, setTick] = useState(0)

  useEffect(() => {
    const live = startLive(
      client,
      (nextRows) => {
        setRows(nextRows)
        setTick((n) => n + 1)
      },
      setLink,
      {
      now: () => Date.now(),
      setTimeout: (fn: () => void, ms: number) => {
        setTimeout(fn, ms)
      },
      visibility: () => (document.visibilityState === 'hidden' ? 'hidden' : 'visible'),
      // Coming back to a tab should not cost the ten-second wait: the loop reads the moment the
      // page shows itself again.
      onVisibilityChange: (listener: () => void) => {
        const handler = () => {
          if (document.visibilityState === 'visible') listener()
        }
        document.addEventListener('visibilitychange', handler)
        return () => {
          document.removeEventListener('visibilitychange', handler)
        }
      },
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
  const selectedName = useMemo(() => {
    if (selectedId === null) return null
    const row = rows.find((torrent) => torrent.id === selectedId)
    return row === undefined ? null : row.name
  }, [rows, selectedId])

  return (
    <div className="flex h-dvh min-h-0 flex-col overflow-hidden">
      <Header
        engineVersion={sessionInfo?.version ?? null}
        link={link}
        totalDown={totals.down}
        totalUp={totals.up}
        dhtNodes={sessionInfo?.dht_nodes ?? null}
        signedIn={signedIn}
        onSignOut={onSignOut}
      />
      {signOutError !== null && (
        <div className="shrink-0 px-5 pt-3">
          <Banner tone="danger">{signOutError}</Banner>
        </div>
      )}
      <div className="flex min-h-0 flex-1 flex-col md:flex-row">
        <StateFilters counts={counts} active={filter} onChange={setFilter} />
        <main className="flex min-h-0 min-w-0 flex-1 flex-col">
          <TorrentTable rows={shown} selectedId={selectedId} onPick={setSelectedId} />
          <DetailPane
            id={selectedId}
            client={client}
            tick={tick}
            name={selectedName}
            onDismiss={() => {
              setSelectedId(null)
            }}
            onRemoved={(goneId) => {
              // The pane closes itself; the selection goes with it, but only when this is still
              // the selected torrent - a late 404 about one nobody chose must not close a pane
              // that is busy describing another.
              setSelectedId((current) => (current === goneId ? null : current))
            }}
          />
        </main>
      </div>
    </div>
  )
}
