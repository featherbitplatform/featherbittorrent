/**
 * The page's two live halves: the regions it speaks through, and the loop that keeps what it
 * speaks about fresh.
 *
 * An announcement lands when text appears inside a live region that was already on the page. A
 * message that mounts together with its region - or with a whole screen that arrives replacing
 * another - is news nobody hears, because the element carrying the news is itself brand new and
 * the screen reader had no reason to be watching it yet. So the page has exactly two regions,
 * mounted once in `App` outside every screen, and this module decides which words belong in which:
 * `polite` for what a person can read when it suits them (the "session ended" notice, a sign-out
 * that failed while the session was still live), `assertive` for what just happened to them (a
 * refused sign-in, a server that stopped answering).
 *
 * The screens keep their visible banners exactly where they are; what moved here is the
 * announcing. The mapping from a session state to a message lives in this file and is the only
 * writer of the two regions' text, and the announcer never replaces the nodes it was handed -
 * `the_live_regions_outlive_the_screens` is what keeps that promise.
 *
 * The second half is the refresh loop, `startLive`: the remote TUI's cadence (`crates/fbtt-core/
 * src/remote/mod.rs` and `backend.rs`) carried to the browser. The table is read every second,
 * an event on the server's stream asks early, hidden means ten seconds, and a server that stops
 * answering is waited out on the same doubling pause. Like the TUI, the event stream is a wake-up
 * signal and nothing else: its payload never becomes rows. Like the TUI too, a failed read keeps
 * the last table on the screen, and `deriveRows` spells each row's state with the TUI's own rule
 * (`crates/fbtt-core/src/remote/map.rs` and `engine/map.rs`), stalled clock included.
 * Everything a test needs to control - the clock, the scheduler, the page's visibility, the event
 * stream - arrives through `opts`.
 *
 * @module live
 */
import type { components } from './api/schema'
import type { TorrentInfo } from './api/client'
import type { SessionState, SessionStore } from './session'

/** The one line the unreachable screen says. Shared between the visible text and the announcer so the two can never drift apart. */
export const UNREACHABLE_COPY = 'Cannot reach the server.'

/** The two nodes the page announces through. App mounts them once; this module only ever writes text into them. */
export interface LiveRegions {
  polite: HTMLElement
  assertive: HTMLElement
}

/** The running announcer: the regions it writes, how to stop it, and how to make it repeat itself. */
export interface LiveAnnouncer {
  /** The polite region as it stands now. Exposed through getters so a test can hold the mount to its nodes. */
  polite: HTMLElement
  assertive: HTMLElement
  /** Stop following the store. The regions stay mounted; they just stop being written to. */
  stop(): void
  /** Say the assertive words again: blank them now, restore them a moment later, so the same sentence registers as news twice. */
  reannounce(): void
}

/** What each state has to say, and through which region it says it. */
export function messageFor(state: SessionState): { polite: string | null; assertive: string | null } {
  switch (state.phase) {
    case 'signedOut':
      // The notice is context a person reads when they get to it; the reason this attempt failed is about the attempt they just made.
      return { polite: state.notice, assertive: state.error }
    case 'signedIn':
      // The session is live; the only news here is a sign-out that failed inside it.
      return { polite: state.error, assertive: null }
    case 'unreachable':
      return { polite: null, assertive: UNREACHABLE_COPY }
    default:
      // Loading and open have nothing to announce.
      return { polite: null, assertive: null }
  }
}

/**
 * Mount the announcer: write the current words into the given regions, then follow the store and
 * write every later change into those same nodes. `injected` are App's own persistent nodes; the
 * announcer keeps writing into them for its whole life, which is the whole reason the regions are
 * hoisted out of the screens - a message that outlives a screen needs a region that outlives it too.
 */
export function createLiveAnnouncer(store: SessionStore, regions: LiveRegions): LiveAnnouncer {
  let timer: ReturnType<typeof setTimeout> | undefined

  function say(): void {
    const next = messageFor(store.get())
    regions.polite.textContent = next.polite ?? ''
    regions.assertive.textContent = next.assertive ?? ''
  }

  const unsubscribe = store.subscribe(() => {
    clearTimeout(timer)
    say()
  })
  say()

  return {
    get polite(): HTMLElement {
      return regions.polite
    },
    get assertive(): HTMLElement {
      return regions.assertive
    },
    stop: () => {
      clearTimeout(timer)
      unsubscribe()
    },
    reannounce: () => {
      // Blank now, restore on the next tick: screen readers compare against what the region held
      // a moment ago, and text that never left is not news.
      regions.assertive.textContent = ''
      clearTimeout(timer)
      timer = setTimeout(say, 60)
    },
  }
}

// ---------------------------------------------------------------------------
// The refresh loop
// ---------------------------------------------------------------------------

/** How the link to the server stands, as the header draws it: a retry pause of N seconds when it is lost. */
export type LinkState = { phase: 'live' } | { phase: 'lost'; retryIn: number }

/** What one torrent is doing, spelled the terminal's way (`crates/fbtt-tui/src/view/mod.rs`'s `state_label`), after the mapping both TUI backends share. */
export type RowState =
  | 'downloading'
  | 'seeding'
  | 'stalled'
  | 'metadata'
  | 'queued'
  | 'checking'
  | 'paused'
  | 'error'

/** One torrent as the table draws it: the server's row plus the state the page derived from it. */
export type TorrentRow = TorrentInfo & { rowState: RowState }

/** The pauses the terminal waits out a lost server with, in seconds: doubling to 30 and staying there (`BACKOFF` in `crates/fbtt-core/src/remote/mod.rs`). */
const BACKOFF_SECONDS = [1, 2, 4, 8, 16, 30] as const

function rungAt(rung: number): number {
  return BACKOFF_SECONDS[Math.min(rung, BACKOFF_SECONDS.length - 1)]
}

/** How often the table is read when nothing wakes it early: the terminal's `REFRESH`. */
const REFRESH_MS = 1000
/** The same read while the page is hidden. The web UI's own addition, as the plan calls it; the terminal's `SLOW_REFRESH` is a different thing (queue counts, not a pause). */
const SLOW_REFRESH_MS = 10_000
/** How long a download may take nothing before the table calls it stalled: the terminal's `STALL_AFTER`. Long enough that a slow tracker reply or a handshake is not "stalled". */
const STALL_AFTER_MS = 30_000

/** `EventSource.CLOSED`: the stream will not fix itself, which is a refused login or a server that will not take a stream. */
const STREAM_CLOSED = 2

/** The event names of this API: the `type` values the generated document gives `EventDto`. */
type EventTypeName = components['schemas']['EventDto']['type']

/** The `status` category exactly: every name `fbt-session` classifies as status (`Event::category` in its `event.rs` is what decides that). */
const STATUS_CATEGORY_EVENTS: EventTypeName[] = [
  'torrent_added',
  'torrent_removed',
  'state_changed',
  'metadata_received',
  'torrent_finished',
  'seed_limit_reached',
  'proxy_error',
]

/**
 * The stream events worth waking for: the `status` category exactly, plus `lagged`, which comes
 * whatever the categories, plus `message`, the type EventSource dispatches for frames with no
 * `event:` field. The names run through the generated document's types so a typo is a compile
 * error; an event name this list does not know wakes nobody until the next second's read, which is
 * why the stream stays a wake-up signal and never the source of rows.
 */
const WAKE_EVENT_TYPES: string[] = [
  ...STATUS_CATEGORY_EVENTS,
  // `lagged` comes whatever the categories; `message` is what EventSource dispatches for a frame
  // with no `event:` field at all. Neither is a status event; both still mean "something changed".
  'lagged',
  'message',
]

/** What `startLive` needs from its surroundings, all of it injected so the loop is testable without a browser, a port or a server. */
export interface LiveOptions {
  /** The clock, in milliseconds: the stalled clock and nothing else reads it. */
  now(): number
  /** Schedule one call. There is no cancel: a superseded call becomes a no-op by its own generation. */
  setTimeout(fn: () => void, ms: number): void
  /** Whether the page is visible: hidden is the ten-second cadence. */
  visibility(): 'visible' | 'hidden'
  /** Start watching whether the page is shown; the listener is called each time it *becomes* visible, and what this returns is called on `stop`. */
  onVisibilityChange(listener: () => void): () => void
  /** Open the event stream, or say this environment has none (then the seconds do the work alone). */
  eventSource(): WakeStream | null
}

/** The part of `EventSource` the loop uses: event *names* and closure. The payload is never read. */
export interface WakeStream {
  addEventListener(type: string, listener: (event: { type: string }) => void): void
  close(): void
  readonly readyState: number
}

/** The running loop: the one way to stop it. */
export interface LiveHandle {
  stop(): void
}

/** The only call the loop makes. The screen hands it the page's client; a test hands it a fake that answers when the test says so. */
export interface RefreshClient {
  torrents(): Promise<TorrentInfo[]>
}

/** The server's snake_case state name as the row state the table draws, exactly `torrent_state` plus `row_state`'s non-stalled arms. A name this release does not know is a torrent that is running. */
function mappedRowState(name: string): RowState {
  switch (name) {
    case 'downloading_metadata':
      return 'metadata'
    case 'queued_for_checking':
    case 'queued':
      return 'queued'
    case 'checking_files':
      return 'checking'
    case 'seeding':
      return 'seeding'
    case 'paused':
      return 'paused'
    case 'error':
      return 'error'
    default:
      return 'downloading'
  }
}

/**
 * The table for every torrent the server listed, and the stalled clocks moved along to this look.
 *
 * The clock for one torrent runs only while that torrent is a download taking nothing; every other
 * moment resets it to `now`, so the stalled column measures how long a *running* download has been
 * getting nothing and not how long since it last moved in some wider sense (a torrent resumed after
 * an hour paused starts from zero). Ids no longer on the server lose their entries, and one first
 * seen starts its count now. `stalled` is updated in place, the way `crates/fbtt-core/src/remote/
 * map.rs` updates the same map, and returns the rows with `rowState` spelled the terminal's way.
 */
export function deriveRows(
  torrents: TorrentInfo[],
  stalled: Map<string, number>,
  now: number,
): TorrentRow[] {
  const next = new Map<string, number>()
  const rows = torrents.map((torrent) => {
    const mapped = mappedRowState(torrent.state)
    // Only downloads run the stalled clock, unknown names included: a torrent whose state is newer
    // than this mapping is a torrent that is running.
    const running = mapped === 'downloading'
    const zeroSince =
      running && torrent.download_rate === 0 ? (stalled.get(torrent.id) ?? now) : now
    next.set(torrent.id, zeroSince)
    let rowState = mapped
    if (
      running &&
      torrent.download_rate === 0 &&
      torrent.progress < 1 &&
      now - zeroSince >= STALL_AFTER_MS
    ) {
      // Running, taking nothing, and not about to be seeding: a person scanning the table needs to
      // see that rather than a bar stuck at 40 %.
      rowState = 'stalled'
    }
    return { ...torrent, rowState }
  })
  stalled.clear()
  for (const [id, at] of next) stalled.set(id, at)
  return rows
}

/**
 * Start the refresh loop: read the table at once, then every second, early on a stream event or
 * on the page becoming visible again (each coalesced to one extra read per read in flight, the
 * terminal's rule), every ten seconds while hidden, and on a doubling pause after a failure - 1,
 * 2, 4, 8, 16, then 30 seconds.
 *
 * A lost link is on its own schedule: while the loop is waiting out a backoff pause, wakes do not
 * apply - neither stream events nor the page coming back - because cutting in front of the ladder
 * is how a down server gets stampeded. The backoff timer decides when the next read goes out.
 *
 * `onRows` comes only from a good read, so a server that stops answering keeps the last table on
 * the screen, which is what the header's "Reconnecting in N s" is for; `onLink` says how the link
 * stands after every read. A `401` is not the loop's business: the client hands it to the session
 * store, the store ends the session, and the screen goes away - the stream never signs anybody
 * out, and neither does a lost round. The stream is closed on `stop`; a stream that reaches
 * `CLOSED` is reopened on its own copy of the same ladder, because a stream that will not open and
 * a table that will not read are different troubles.
 */
export function startLive(
  client: RefreshClient,
  onRows: (rows: TorrentRow[]) => void,
  onLink: (link: LinkState) => void,
  opts: LiveOptions,
): LiveHandle {
  // Where each torrent's stalled count has been running from, by id.
  const stalled = new Map<string, number>()
  let stopped = false
  // The one permit for whatever the stream said while a read was in flight: this is the difference
  // between a stream that keeps a screen fresh and a stream that makes the page re-read per event.
  let permit = false
  let inFlight = false
  // Which rung of the ladder the next failure waits out; a good read takes it back to the bottom.
  let rung = 0
  // Whether the link is lost right now: while it is, the backoff timer owns the next read and wakes do not apply.
  let linkLost = false
  // Two schedules this loop cannot cancel (`opts` has no clearTimeout), so each scheduled call
  // carries the generation it was scheduled under and a stale call is a no-op.
  let refreshGeneration = 0
  let streamGeneration = 0
  let streamRung = 0
  let stream: WakeStream | null = null

  function scheduleRefresh(ms: number): void {
    const generation = (refreshGeneration += 1)
    opts.setTimeout(() => {
      if (!stopped && generation === refreshGeneration) void tick()
    }, ms)
  }

  function wake(): void {
    // While the link is lost the backoff timer owns the next read: events and a page turning
    // visible come back to the queue rather than cutting in front of the ladder.
    if (stopped || linkLost) return
    if (inFlight) {
      // Coalesced: one extra read after the one in flight, however many events arrived meanwhile.
      permit = true
      return
    }
    scheduleRefresh(0)
  }

  async function tick(): Promise<void> {
    if (stopped) return
    // This round supersedes any read waiting on a timer.
    refreshGeneration += 1
    inFlight = true
    let ok = false
    try {
      const torrents = await client.torrents()
      ok = true
      if (!stopped) onRows(deriveRows(torrents, stalled, opts.now()))
    } catch {
      // A read that failed is a table that stays: `onRows` is only ever said by a good round. A
      // `401` has already been reported to the session store by the client itself, and the store
      // takes this screen away; the catch here only keeps the loop alive long enough to notice.
    }
    inFlight = false
    if (stopped) return
    let delay: number
    if (ok) {
      rung = 0
      linkLost = false
      onLink({ phase: 'live' })
      delay = permit ? 0 : opts.visibility() === 'hidden' ? SLOW_REFRESH_MS : REFRESH_MS
    } else {
      // The ladder owns the next read from here: wake() will not answer the stream or the page
      // until a round succeeds and reports the link live again.
      linkLost = true
      const pause = rungAt(rung)
      rung = Math.min(rung + 1, BACKOFF_SECONDS.length - 1)
      onLink({ phase: 'lost', retryIn: pause })
      delay = pause * 1000
    }
    permit = false
    scheduleRefresh(delay)
  }

  function openStream(): void {
    const source = opts.eventSource()
    if (source === null || stopped) return
    stream = source
    for (const type of WAKE_EVENT_TYPES) source.addEventListener(type, wake)
    source.addEventListener('open', () => {
      // It opened: the next trouble starts at the bottom of the ladder.
      streamRung = 0
    })
    source.addEventListener('error', () => {
      // `CONNECTING` is the browser's own reconnect, which needs nothing added. `CLOSED` is fatal -
      // a 401 or a server that refuses the stream - and it does not fix itself: dial again on the
      // ladder, on this stream's own rung so it never makes the table wait longer than it has to.
      if (source.readyState !== STREAM_CLOSED || stopped) return
      source.close()
      stream = null
      const pause = rungAt(streamRung)
      streamRung = Math.min(streamRung + 1, BACKOFF_SECONDS.length - 1)
      const generation = (streamGeneration += 1)
      opts.setTimeout(() => {
        if (!stopped && generation === streamGeneration) openStream()
      }, pause * 1000)
    })
  }

  // Coming back to the page must not wait out the slow cadence: becoming visible wakes the loop
  // exactly the way a stream event does - extra read after the one in flight, and not at all
  // while the link is lost and the backoff owns the next read.
  const stopWatchingVisibility = opts.onVisibilityChange(() => {
    if (!stopped) wake()
  })

  openStream()
  void tick()

  return {
    stop: () => {
      stopped = true
      stopWatchingVisibility()
      stream?.close()
      stream = null
      // Scheduled calls outlive `stop` (nothing cancels them); they check `stopped` and do nothing.
    },
  }
}
