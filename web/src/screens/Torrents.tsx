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
 * The header's Add torrent button and the toolbar over the table are the two ways into the server's
 * write side. The toolbar belongs to the chosen row: Remove goes with any choice, Pause only with a
 * row that is not already paused, Resume only with one that says it is paused, and all three go quiet while a request is on the wire so nobody
 * presses a fourth time at something already happening. Removal is a question rather than a click
 * (`ConfirmDelete`), and Add is a dialog of its own (`AddTorrent`). Each of them makes its own call
 * and reports its own failure where it happened - inside the dialog for a dialog, in the banner above
 * the filters for the toolbar - and every one the server accepted wakes the refresh loop, which is the
 * difference between a row that changes while the person is looking at it and one that changes a
 * second later.
 *
 * The selected row is remembered here because the detail pane under the table reads through it, and
 * the pane's heartbeat is this screen's: `startLive` bumps `tick` once per live round, and the pane
 * re-reads its tab on that bump, so it shares the loop's cadence, its hidden-page pause and its
 * backoff instead of keeping a timer of its own.
 *
 * @module screens/Torrents
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import AddTorrent from '../components/AddTorrent'
import Banner from '../components/Banner'
import Button from '../components/Button'
import ConfirmDelete from '../components/ConfirmDelete'
import DetailPane from '../components/DetailPane'
import Header from '../components/Header'
import StateFilters from '../components/StateFilters'
import TorrentTable from '../components/TorrentTable'
import { api, errorMessage } from '../api/client'
import type { Api, SessionInfo } from '../api/client'
import { announcePolite, startLive } from '../live'
import type { LinkState, LiveHandle, TorrentRow } from '../live'
import { filterCounts, matchesFilter } from '../filters'
import type { FilterKey } from '../filters'

export interface TorrentsProps {
  /** Whether there is a session to end: a server with logins off has none. */
  signedIn: boolean
  /** Why the last sign-out attempt did not take, or null. The session is still live in that case, and the table keeps running under the notice. */
  signOutError: string | null
  onSignOut(): void
  /** Opens the Settings screen, which replaces this one. */
  onSettings(): void
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
  onSettings,
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
  // The dialogs the toolbar and the header open, and the toolbar's own failure. An action's error is
  // not the sign-out notice's slot: the two can be true at once and they mean different things.
  const [addOpen, setAddOpen] = useState(false)
  const [confirming, setConfirming] = useState<{ id: string; name: string } | null>(null)
  const [actionError, setActionError] = useState<string | null>(null)
  // True while an action is on the wire: a toolbar that stayed live during a pause is a toolbar that
  // lets the person press it four more times.
  const [busy, setBusy] = useState(false)
  const liveRef = useRef<LiveHandle | null>(null)

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
    // The handle stays reachable because the toolbar needs it after the effect that started the loop
    // is long gone: the read a pause earns happens on a click, not on a mount.
    liveRef.current = live
    return () => {
      liveRef.current = null
      live.stop()
    }
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
  // The chosen row, read from every row rather than from the filtered ones: a pause can take a torrent
  // out of the filter that is showing while the person is still looking at it in the pane beside the
  // table, and a toolbar that went quiet then would be refusing to act on a torrent that is chosen.
  const selectedRow = useMemo(
    () => rows.find((torrent: TorrentRow) => torrent.id === selectedId) ?? null,
    [rows, selectedId],
  )
  const selectedName = selectedRow?.name ?? null

  /** Ask the refresh loop for the table again, the way a stream event does. */
  const wake = useCallback((): void => {
    liveRef.current?.wake()
  }, [])

  async function act(kind: 'pause' | 'resume'): Promise<void> {
    const target = selectedRow
    if (target === null || busy) return
    setBusy(true)
    setActionError(null)
    try {
      if (kind === 'pause') await client.pause(target.id)
      else await client.resume(target.id)
      // The server said yes: the row should look different before the person looks away from it.
      wake()
    } catch (cause) {
      // The toolbar's own failure, in the banner above the filters and through the polite region, so
      // it reaches somebody whose hands are on the keyboard and somebody who is not looking.
      const why = errorMessage(cause)
      setActionError(why)
      announcePolite(why)
    } finally {
      setBusy(false)
    }
  }

  const toolbar = (
    <>
      <Button
        size="sm"
        variant="secondary"
        disabled={selectedRow === null || selectedRow.rowState === 'paused' || busy}
        onClick={() => {
          void act('pause')
        }}
      >
        Pause
      </Button>
      <Button
        size="sm"
        variant="secondary"
        disabled={selectedRow === null || selectedRow.rowState !== 'paused' || busy}
        onClick={() => {
          void act('resume')
        }}
      >
        Resume
      </Button>
      <Button
        size="sm"
        variant="danger"
        disabled={selectedRow === null || busy}
        onClick={() => {
          if (selectedRow !== null) setConfirming({ id: selectedRow.id, name: selectedRow.name })
        }}
      >
        Remove
      </Button>
    </>
  )

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
        onSettings={onSettings}
        onAdd={() => {
          setAddOpen(true)
        }}
      />
      {(signOutError !== null || actionError !== null) && (
        <div className="flex shrink-0 flex-col gap-2 px-5 pt-3">
          {signOutError !== null && <Banner tone="danger">{signOutError}</Banner>}
          {actionError !== null && <Banner tone="danger">{actionError}</Banner>}
        </div>
      )}
      <div className="flex min-h-0 flex-1 flex-col md:flex-row">
        <StateFilters counts={counts} active={filter} onChange={setFilter} />
        <main className="flex min-h-0 min-w-0 flex-1 flex-col">
          <TorrentTable
            rows={shown}
            selectedId={selectedId}
            onPick={setSelectedId}
            actions={toolbar}
          />
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

      {addOpen && (
        <AddTorrent
          client={client}
          onAdded={() => {
            // The new torrent is on the server now: the wake is what puts it in the table. The
            // dialog closes itself and says so through the polite region.
            wake()
          }}
          onClose={() => {
            setAddOpen(false)
          }}
        />
      )}
      {confirming !== null && (
        <ConfirmDelete
          key={confirming.id}
          id={confirming.id}
          name={confirming.name}
          client={client}
          onClose={() => {
            setConfirming(null)
          }}
          onRemoved={(goneId) => {
            setSelectedId((current) => (current === goneId ? null : current))
            wake()
          }}
        />
      )}
    </div>
  )
}
