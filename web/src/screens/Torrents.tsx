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
 * The text filter and the sort are this screen's too, not the table's: the phone's cards show the
 * same two above them, and a query typed at one width is the query at the other. On a phone the
 * full view is modal: while it is open the header, the banners and the list behind it are `inert`,
 * and closing it gives the focus back to the card that opened it. An action's refusal is drawn in
 * the full view as well as in the banner, and any later action the server accepts clears it.
 * `hidden` is App's word that Settings covers this screen: the loop then reads at its hidden-page
 * cadence, and wakes the moment the screen is shown again.
 *
 * @module screens/Torrents
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import AddTorrent from '../components/AddTorrent'
import Banner from '../components/Banner'
import Button from '../components/Button'
import TorrentActions from '../components/TorrentActions'
import ConfirmDelete from '../components/ConfirmDelete'
import DetailPane from '../components/DetailPane'
import Header from '../components/Header'
import StateFilters from '../components/StateFilters'
import TorrentCards from '../components/TorrentCards'
import TorrentFullView from '../components/TorrentFullView'
import TorrentTable from '../components/TorrentTable'
import { errorMessage } from '../api/client'
import { useConnection } from '../connection'
import type { Api, SessionInfo } from '../api/client'
import { announcePolite, startLive } from '../live'
import type { LinkState, LiveHandle, TorrentRow } from '../live'
import { filterCounts, matchesFilter } from '../filters'
import type { FilterKey, TorrentSort } from '../filters'

export interface TorrentsProps {
  /** Whether there is a session to end: a server with logins off has none. */
  signedIn: boolean
  /** Why the last sign-out attempt did not take, or null. The session is still live in that case, and the table keeps running under the notice. */
  signOutError: string | null
  onSignOut(): void
  /** Opens the Settings screen, which replaces this one. */
  onSettings(): void
  /** Whether another screen (Settings) covers this one: the live loop slows to its hidden-page cadence. */
  hidden?: boolean
  /** The client to read through. Defaults to the connection's so tests can hand in a fake. */
  client?: Api
}

/** The stream this screen wakes on: the `status` category, and nothing else (the payload is never read; see `live.ts`). */
const EVENTS_QUERY = 'categories=status'

/** How often the session row is re-read for the header's version and DHT count. It is decoration, not the live table; the refresh model puts the once-a-second cadence on the list. */
const SESSION_POLL_MS = 30_000

/** Whether the viewport is a phone: `matchMedia('(max-width: 640px)')`, followed through its `change` event. An environment without `matchMedia` (a test jsdom) is the desktop. */
const PHONE_QUERY = '(max-width: 640px)'

function usePhone(): boolean {
  const [phone, setPhone] = useState(
    () => typeof window.matchMedia === 'function' && window.matchMedia(PHONE_QUERY).matches,
  )
  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return
    const query = window.matchMedia(PHONE_QUERY)
    const onChange = (): void => {
      setPhone(query.matches)
    }
    onChange()
    query.addEventListener('change', onChange)
    return () => {
      query.removeEventListener('change', onChange)
    }
  }, [])
  return phone
}

export default function Torrents({
  signedIn,
  signOutError,
  onSignOut,
  onSettings,
  hidden = false,
  client: given,
}: TorrentsProps) {
  const connection = useConnection()
  const client = given ?? connection.api
  const [rows, setRows] = useState<TorrentRow[]>([])
  const [link, setLink] = useState<LinkState>({ phase: 'live' })
  const [filter, setFilter] = useState<FilterKey>('all')
  const [selectedId, setSelectedId] = useState<string | null>(null)
  // The text filter and the sort, shared by the desktop table and the phone cards.
  const [query, setQuery] = useState('')
  const [sort, setSort] = useState<TorrentSort>(null)
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
  // The torrents whose files are being moved: kept here, not in the menu, so the indicator survives choosing another row.
  const [movingIds, setMovingIds] = useState<ReadonlySet<string>>(new Set())
  const liveRef = useRef<LiveHandle | null>(null)
  const phone = usePhone()
  // Whether the full-screen torrent owns a history entry: opened once with `pushState`, closed by
  // the browser's back (a `popstate`) or by `history.back()`, so the history stays balanced and
  // neither path ever leaves the page.
  const pushedRef = useRef(false)
  // Numbers the entries the full view pushes. A popstate that lands on one of them is a stale
  // answer to an earlier `history.back()` (a card was opened before it arrived), not the person
  // going back to the list, so it must not close the view that is open now.
  const tokenRef = useRef(0)
  // The card the full view was opened from, which takes the focus back when the view closes.
  const openerRef = useRef<string | null>(null)
  // Read by the loop's `visibility` on every round, so it never needs restarting for Settings.
  const hiddenRef = useRef(hidden)

  useEffect(() => {
    const onPop = (event: PopStateEvent): void => {
      if (!pushedRef.current) return
      // A stale answer to either screen's earlier `history.back()` (the full view's or Settings').
      const landed = event.state as { fbttFull?: number; fbttSettings?: number } | null
      if (landed?.fbttFull !== undefined || landed?.fbttSettings !== undefined) return
      pushedRef.current = false
      setSelectedId(null)
    }
    window.addEventListener('popstate', onPop)
    return () => {
      window.removeEventListener('popstate', onPop)
    }
  }, [])

  // Leaving the phone layout, losing the session or losing the torrent all strand the entry the
  // full view pushed: each consumes it once and resets the ref, so the next pick pushes again.
  const consumeEntry = useCallback((): void => {
    if (!pushedRef.current) return
    pushedRef.current = false
    window.history.back()
  }, [])

  useEffect(() => {
    if (!phone) consumeEntry()
  }, [phone, consumeEntry])

  useEffect(() => consumeEntry, [consumeEntry])

  /** Pick a card: select it and open it full screen on its own history entry. */
  function openFull(id: string): void {
    if (!pushedRef.current) {
      tokenRef.current += 1
      window.history.pushState({ fbttFull: tokenRef.current }, '')
      pushedRef.current = true
    }
    openerRef.current = id
    // The last refusal belongs to the torrent it was about, not to the one being opened now.
    setActionError(null)
    setSelectedId(id)
  }

  /** Close the full view: through the history when it owns an entry, so the entry is consumed. */
  function closeFull(): void {
    // The ref and the selection go first: a second tap on Back (or Close) arriving before the
    // next render must not call `history.back()` again and walk out of the app.
    const owned = pushedRef.current
    pushedRef.current = false
    setSelectedId(null)
    if (owned) window.history.back()
  }

  /** The torrent went away while open: clear the selection and give the history entry back. */
  function goneFull(goneId: string): void {
    setSelectedId((current) => (current === goneId ? null : current))
    if (pushedRef.current) {
      pushedRef.current = false
      window.history.back()
    }
  }

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
      visibility: () =>
        document.visibilityState === 'hidden' || hiddenRef.current ? 'hidden' : 'visible',
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
      // The browser's cookie authenticates the stream as it is; the desktop's URL carries its token,
      // because EventSource cannot set a header. An environment without EventSource (a test jsdom)
      // simply gets no stream.
      eventSource: () =>
        typeof EventSource === 'function' ? new EventSource(connection.eventsUrl(EVENTS_QUERY)) : null,
    })
    // The handle stays reachable because the toolbar needs it after the effect that started the loop
    // is long gone: the read a pause earns happens on a click, not on a mount.
    liveRef.current = live
    return () => {
      liveRef.current = null
      live.stop()
    }
  }, [client, connection])

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

  // The chosen torrent left the list while the full view owned a history entry.
  useEffect(() => {
    if (selectedRow === null && pushedRef.current) {
      setSelectedId(null)
      consumeEntry()
    }
  }, [selectedRow, consumeEntry])

  /** Ask the refresh loop for the table again, the way a stream event does. */
  const wake = useCallback((): void => {
    liveRef.current?.wake()
  }, [])

  // Settings covering the screen is a hidden page to the loop; coming back reads at once, the way
  // a tab coming back into view does.
  useEffect(() => {
    const was = hiddenRef.current
    hiddenRef.current = hidden
    if (was && !hidden) wake()
  }, [hidden, wake])

  const fullOpen = phone && selectedRow !== null
  // Closing the full view hands the focus back to the card it was opened from. When that card is
  // gone (removed, or the window grew into the table) the focus still needs somewhere to land: the
  // selected row on the desktop, else the name filter, never `body`.
  const wasFull = useRef(false)
  useEffect(() => {
    if (wasFull.current && !fullOpen && openerRef.current !== null) {
      const card = document.querySelector<HTMLElement>(
        `[data-card="${CSS.escape(openerRef.current)}"]`,
      )
      const row =
        !phone && selectedId !== null
          ? document.querySelector<HTMLElement>(`[data-row="${CSS.escape(selectedId)}"]`)
          : null
      ;(card ?? row ?? document.querySelector<HTMLElement>('[data-torrent-filter]'))?.focus()
      openerRef.current = null
    }
    wasFull.current = fullOpen
  }, [fullOpen, phone, selectedId])

  /** An action the server accepted: an older refusal is no longer the news. */
  const clearError = useCallback((): void => {
    setActionError(null)
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
      setActionError(null)
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

  const setMoving = (moverId: string, on: boolean): void => {
    setMovingIds((current) => {
      const next = new Set(current)
      if (on) next.add(moverId)
      else next.delete(moverId)
      return next
    })
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
      {selectedRow !== null && (
        <TorrentActions
          key={selectedRow.id}
          torrent={selectedRow}
          client={client}
          moving={movingIds.has(selectedRow.id)}
          onMoving={(on) => {
            setMoving(selectedRow.id, on)
          }}
          onError={(why) => {
            setActionError(why)
            announcePolite(why)
          }}
          onDone={clearError}
        />
      )}
    </>
  )

  return (
    <div className="flex h-dvh min-h-0 flex-col overflow-hidden">
      <Header
        inert={fullOpen}
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
        <div inert={fullOpen} className="flex shrink-0 flex-col gap-2 px-5 pt-3">
          {signOutError !== null && <Banner tone="danger">{signOutError}</Banner>}
          {actionError !== null && <Banner tone="danger">{actionError}</Banner>}
        </div>
      )}
      <div inert={fullOpen} className="flex min-h-0 flex-1 flex-col md:flex-row">
        <StateFilters counts={counts} active={filter} onChange={setFilter} />
        <main className="flex min-h-0 min-w-0 flex-1 flex-col">
          {phone ? (
            <TorrentCards
              rows={shown}
              onPick={openFull}
              query={query}
              onQueryChange={setQuery}
              sort={sort}
              onSortChange={setSort}
            />
          ) : (
            <>
              <TorrentTable
                rows={shown}
                selectedId={selectedId}
                onPick={setSelectedId}
                actions={toolbar}
                query={query}
                onQueryChange={setQuery}
                sort={sort}
                onSortChange={setSort}
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
            </>
          )}
        </main>
      </div>

      {fullOpen && selectedRow !== null && (
        <TorrentFullView
          torrent={selectedRow}
          client={client}
          tick={tick}
          busy={busy}
          onBack={closeFull}
          onAct={(kind) => {
            void act(kind)
          }}
          onRemove={() => {
            setConfirming({ id: selectedRow.id, name: selectedRow.name })
          }}
          moving={movingIds.has(selectedRow.id)}
          onMoving={(on) => {
            setMoving(selectedRow.id, on)
          }}
          onError={(why) => {
            setActionError(why)
            announcePolite(why)
          }}
          onDone={clearError}
          error={actionError}
          onRemoved={goneFull}
        />
      )}
      {addOpen && (
        <AddTorrent
          client={client}
          onAdded={() => {
            // The new torrent is on the server now: the wake is what puts it in the table. The
            // dialog closes itself and says so through the polite region.
            setActionError(null)
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
            setActionError(null)
            goneFull(goneId)
            wake()
          }}
        />
      )}
    </div>
  )
}
