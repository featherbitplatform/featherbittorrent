/**
 * The page's two live regions, against a fake store and even faker nodes.
 *
 * The rule under test is that an announcement needs a region that was already on the page when the
 * news arrives, so the regions must outlive every screen - the assertive one survives unreachable
 * arriving and leaving, the polite one survives the sign-in screen and the placeholder both. The
 * node identity is the whole claim, so this mounts the announcer exactly as `App` does (two nodes,
 * handed over once) and drives the real store through the two moves a screen reader would notice,
 * reading the text straight off the same nodes each time. The nodes here are plain objects because
 * the announcer only ever sets `textContent`; `App` hands it real ones.
 *
 * The refresh-loop half is tested on a hand-turned world: an injected clock, scheduler, visibility
 * and stream, so a pause is an assertion about a number and not about wall time, and "exactly one
 * extra refresh" is a count of calls. No test here sleeps, and none touches a server.
 *
 * @module live.test
 */
import { expect, it } from 'vitest'
import { createLiveAnnouncer, deriveRows, startLive, UNREACHABLE_COPY } from './live'
import type { LinkState, LiveOptions, TorrentRow, WakeStream } from './live'
import { createSessionStore } from './session'
import type { TorrentInfo } from './api/client'

/** A stand-in live region: all the announcer ever does to a node is set its textContent. */
const fakeRegion = (): HTMLElement => ({ textContent: '' }) as unknown as HTMLElement

function json(status: number, body: string): Response {
  return new Response(body, { status, headers: { 'content-type': 'application/json' } })
}

it('the_live_regions_outlive_the_screens', async () => {
  const polite = fakeRegion()
  const assertive = fakeRegion()
  // The status GET never answers (the page goes unreachable) while a sign-in POST works, so the
  // test can visit both ends of the moves a screen reader needs the regions to survive.
  const store = createSessionStore(async (_input, init) => {
    if ((init?.method ?? 'GET') === 'POST') return json(200, '{"subject": "alice", "csrf": "csrf-value"}')
    throw new TypeError('fetch failed')
  })
  const announcer = createLiveAnnouncer(store, { polite, assertive })

  expect(polite.textContent + assertive.textContent, 'the page had words before the server had any').toBe('')

  // loading to unreachable: the assertive region this page mounted at the start holds the new line.
  await store.refresh()
  expect(
    announcer.assertive === assertive && assertive.textContent === UNREACHABLE_COPY,
    'the unreachable message arrived in a fresh assertive region instead of the one already mounted',
  ).toBe(true)

  // back through a live session (which has nothing to announce)...
  expect(await store.signIn('alice', 'hunter2'), 'the sign-in did not take').toBe(true)
  expect(store.get().phase, 'the store never got signed in').toBe('signedIn')
  expect(assertive.textContent, 'a live session left the assertive region still shouting').toBe('')

  // ...and out of it by a 401 elsewhere: the polite region still mounted from the start holds the notice.
  store.endSession(store.generation())
  expect(
    announcer.polite === polite && polite.textContent === 'Your session ended. Sign in again.',
    'the ended-session notice arrived in a fresh polite region instead of the one already mounted',
  ).toBe(true)

  announcer.stop()
})

// ---------------------------------------------------------------------------
// The refresh loop, on an injected clock, scheduler, visibility and stream.
// ---------------------------------------------------------------------------

/** The fake stream: it records its instances, lets the test fire named events and fatal closes, and never reads or sends a payload the loop could parse. */
class FakeStream implements WakeStream {
  readonly listeners = new Map<string, ((event: { type: string }) => void)[]>()
  closed = false
  readyState = 1
  addEventListener(type: string, listener: (event: { type: string }) => void): void {
    const list = this.listeners.get(type) ?? []
    list.push(listener)
    this.listeners.set(type, list)
  }
  close(): void {
    this.closed = true
  }
  emit(type: string): void {
    for (const listener of this.listeners.get(type) ?? []) listener({ type })
  }
  /** What a refused 401 or a dead server leaves behind: a fatal error on a stream that is now CLOSED. */
  refuse(): void {
    this.readyState = 2
    this.emit('error')
  }
}

function deferred<T>(): {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (reason?: unknown) => void
} {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

/** One torrent as the list answers it, with everything the loop ignores filled in. */
function torrent(over: Partial<TorrentInfo> & Pick<TorrentInfo, 'id'>): TorrentInfo {
  return {
    name: over.name ?? over.id,
    progress: 0.4,
    download_rate: 0,
    upload_rate: 0,
    num_peers: 0,
    num_seeds: 0,
    state: 'downloading',
    active_time: 0,
    auto_managed: true,
    has_metadata: true,
    is_seed: false,
    layers_missing: 0,
    need_save_resume: false,
    num_web_seeds: 0,
    queue_position: 0,
    save_path: '/downloads',
    seeding_time: 0,
    total_downloaded: 0,
    total_uploaded: 0,
    total_wanted: 100,
    total_wasted: 0,
    wanted_done: 40,
    ...over,
  }
}

/**
 * The whole world `startLive` sees, on a clock the test turns by hand. Nothing real is timed:
 * the scheduler is a list the test drains, so "exactly one extra refresh" is a count and a
 * pending timer is a count, and no test here sleeps at all.
 */
function world() {
  let nowMs = 1_000
  let hidden = false
  let seq = 0
  const timers: { at: number; seq: number; fn: () => void }[] = []
  const streams: { stream: FakeStream; at: number }[] = []
  const visibilityListeners: (() => void)[] = []
  const injected: LiveOptions = {
    now: () => nowMs,
    setTimeout: (fn: () => void, ms: number) => {
      timers.push({ at: nowMs + ms, seq: seq++, fn })
    },
    visibility: () => (hidden ? 'hidden' : 'visible'),
    onVisibilityChange: (listener: () => void) => {
      visibilityListeners.push(listener)
      return () => {
        const at = visibilityListeners.indexOf(listener)
        if (at !== -1) visibilityListeners.splice(at, 1)
      }
    },
    eventSource: () => {
      const stream = new FakeStream()
      streams.push({ stream, at: nowMs })
      return stream
    },
  }
  // Let the real microtask (and promise) queue drain between an action and its assertions.
  const settle = () =>
    new Promise<void>((resolve) => {
      globalThis.setTimeout(resolve, 0)
    })
  async function advance(ms: number): Promise<void> {
    const target = nowMs + ms
    for (;;) {
      const due = timers
        .filter((timer) => timer.at <= target)
        .sort((a, b) => a.at - b.at || a.seq - b.seq)[0]
      if (due === undefined) break
      timers.splice(timers.indexOf(due), 1)
      nowMs = Math.max(nowMs, due.at)
      due.fn()
      await settle()
    }
    nowMs = target
  }
  return {
    injected,
    streams,
    settle,
    advance,
    hide: () => {
      hidden = true
    },
    show: () => {
      // The browser only says "it changed"; the injector is what calls the listener, and only
      // on the hidden-to-visible edge, the way `document` fires `visibilitychange`.
      const wasHidden = hidden
      hidden = false
      if (wasHidden) {
        for (const listener of [...visibilityListeners]) listener()
      }
    },
  }
}

/** A client whose every read is a promise the test hands out when it wants it answered. */
function roundRobinClient(): {
  answers: ReturnType<typeof deferred<TorrentInfo[]>>[]
  client: { torrents(): Promise<TorrentInfo[]> }
} {
  const answers: ReturnType<typeof deferred<TorrentInfo[]>>[] = []
  const client = {
    torrents: () => {
      const answer = deferred<TorrentInfo[]>()
      answers.push(answer)
      return answer.promise
    },
  }
  return { answers, client }
}

it('refreshes_every_second_and_wakes_on_events', async () => {
  const w = world()
  const { answers, client } = roundRobinClient()
  const rowsSeen: TorrentRow[][] = []
  const links: LinkState[] = []
  const live = startLive(client, (rows) => rowsSeen.push(rows), (link) => links.push(link), w.injected)
  await w.settle()

  expect(answers.length, 'startLive did not read the table at once').toBe(1)
  expect(w.streams.length, 'the loop did not open the event stream').toBe(1)

  // The second read goes out one second after the first came back.
  answers[0].resolve([torrent({ id: 'a' })])
  await w.settle()
  expect(links.map((link) => link.phase), 'the first round did not report a live link').toEqual(['live'])
  expect(rowsSeen.length, 'the first read did not deliver its rows').toBe(1)
  await w.advance(999)
  expect(answers.length, 'the table was read again before a second had passed').toBe(1)
  await w.advance(1)
  expect(answers.length, 'the table was not read again after a second').toBe(2)

  // Three events land while the second read is in flight: they buy one extra read after it, not three.
  w.streams[0].stream.emit('state_changed')
  w.streams[0].stream.emit('torrent_added')
  w.streams[0].stream.emit('torrent_removed')
  answers[1].resolve([])
  await w.settle()
  await w.advance(0)
  expect(answers.length, 'events were not coalesced').toBe(3)

  // And the cadence picks back up where the extra read left it: one second later, not two.
  answers[2].resolve([])
  await w.settle()
  await w.advance(1_000)
  expect(answers.length, 'the extra read broke the second cadence').toBe(4)

  live.stop()
  expect(w.streams[0].stream.closed, 'the stream stayed open after the screen went away').toBe(true)
  answers[3].resolve([])
  await w.advance(60_000)
  expect(answers.length, 'a stopped loop kept reading').toBe(4)
})

it('slows_to_ten_seconds_while_hidden', async () => {
  const w = world()
  const { answers, client } = roundRobinClient()
  const live = startLive(client, () => {}, () => {}, w.injected)
  await w.settle()

  answers[0].resolve([])
  await w.settle()
  await w.advance(1_000)
  expect(answers.length, 'the visible page did not read at its second').toBe(2)

  // The read that was in flight when the page hid finishes visible-late: the read after it waits ten seconds.
  w.hide()
  answers[1].resolve([])
  await w.settle()
  await w.advance(9_999)
  expect(answers.length, 'the hidden page read again before its ten seconds').toBe(2)
  await w.advance(1)
  expect(answers.length, 'the hidden page did not read at ten seconds').toBe(3)

  // The page coming back into view wakes the loop at once: the read in flight finishes, the wake
  // buys one extra read straight after it, and the second cadence follows from there.
  w.show()
  answers[2].resolve([])
  await w.settle()
  await w.advance(1_000)
  expect(answers.length, 'the visible page stayed on the slow cadence').toBe(4)

  live.stop()
})

it('becoming_visible_refreshes_now', async () => {
  const w = world()
  const { answers, client } = roundRobinClient()
  const live = startLive(client, () => {}, () => {}, w.injected)
  await w.settle()
  answers[0].resolve([])
  await w.settle()

  // Hide the page while a read is in flight: the cadence slows to ten seconds, and two seconds
  // into the wait there is nothing new yet.
  await w.advance(1_000)
  w.hide()
  answers[1].resolve([])
  await w.settle()
  await w.advance(2_000)
  expect(answers.length, 'the hidden page read early').toBe(2)

  // Coming back does not wait for the slow round: the listener wakes the loop, superseding the
  // pending ten-second timer with a read right now.
  w.show()
  await w.advance(0)
  expect(answers.length, 'becoming visible waited for the slow timer').toBe(3)

  // And a wake that lands while a read is already in flight is coalesced exactly like an event:
  // one extra read after the round in flight, not one per wake.
  answers[2].resolve([])
  await w.settle()
  await w.advance(1_000) // the next second's read goes out and is still in flight
  w.hide()
  w.show()
  answers[3].resolve([])
  await w.settle()
  await w.advance(0)
  expect(answers.length, 'becoming visible while reading bought more than one extra read').toBe(5)

  live.stop()
})

it('a_lost_server_keeps_the_rows_and_backs_off', async () => {
  const w = world()
  const { answers, client } = roundRobinClient()
  const rowsSeen: TorrentRow[][] = []
  const links: LinkState[] = []
  const live = startLive(client, (rows) => rowsSeen.push(rows), (link) => links.push(link), w.injected)
  await w.settle()

  answers[0].resolve([torrent({ id: 'keep-me', name: 'Sintel (2010) 4K' })])
  await w.settle()
  expect(rowsSeen.length, 'the first read did not deliver its rows').toBe(1)

  // Failures walk the TUI's ladder: 1, 2, 4, 8, 16, 30, and 30 again rather than doubling on forever.
  const ladder = [1, 2, 4, 8, 16, 30, 30]
  for (let attempt = 0; attempt < ladder.length; attempt += 1) {
    await w.advance(attempt === 0 ? 1_000 : ladder[attempt - 1] * 1_000)
    answers[attempt + 1].reject(new Error('server gone'))
    await w.settle()
    const last = links[links.length - 1]
    expect(
      last.phase === 'lost' && last.retryIn === ladder[attempt],
      `failure ${attempt + 1} should retry in ${ladder[attempt]} s on the 1, 2, 4, 8, 16, 30, 30 ladder`,
    ).toBe(true)
  }

  // While a pause is running the backoff owns the next read: neither a stream event nor the page
  // coming back into view cuts in front of the ladder.
  const beforeWake = answers.length
  w.streams[w.streams.length - 1].stream.emit('state_changed')
  w.hide()
  w.show()
  await w.settle()
  await w.advance(ladder[6] * 1_000 - 1)
  expect(answers.length, 'a wake cut in front of the backoff pause').toBe(beforeWake)
  await w.advance(1)
  expect(answers.length, 'the backoff pause ended without a read going out').toBe(beforeWake + 1)

  // Through all of it the screen still shows the last good table: a failed round says nothing to onRows.
  expect(rowsSeen.length, 'a failed refresh dropped the rows the page already had').toBe(1)
  expect(rowsSeen[0][0].name, 'the rows that stayed are not the rows that arrived').toBe('Sintel (2010) 4K')

  // A good round is the ladder's reset: live at once, and the next failure starts from one second again.
  answers[8].resolve([torrent({ id: 'keep-me', name: 'Sintel (2010) 4K' }), torrent({ id: 'new' })])
  await w.settle()
  expect(links[links.length - 1].phase, 'a good round did not bring the link back').toBe('live')
  expect(rowsSeen.length, 'the recovery did not deliver its rows').toBe(2)

  await w.advance(1_000)
  answers[9].reject(new Error('server gone again'))
  await w.settle()
  const after = links[links.length - 1]
  expect(
    after.phase === 'lost' && after.retryIn === 1,
    'the retry ladder did not start over from one second after a good round',
  ).toBe(true)

  live.stop()
})

it('a_closed_event_stream_is_reopened_with_backoff', async () => {
  const w = world()
  const { answers, client } = roundRobinClient()
  const live = startLive(client, () => {}, () => {}, w.injected)
  await w.settle()
  answers[0].resolve([])
  await w.settle()

  // The ladder the stream keeps on its own rung, and the times it reopened at.
  const ladder = [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]
  let opens = 1
  for (const pause of ladder) {
    w.streams[w.streams.length - 1].stream.refuse()
    expect(
      w.streams[w.streams.length - 1].stream.closed,
      'the closed stream was left open while the loop waited to reopen it',
    ).toBe(true)
    await w.advance(pause)
    opens += 1
    expect(w.streams.length, `the closed stream was not reopened with backoff after ${pause / 1_000} s`).toBe(
      opens,
    )
    const created = w.streams[w.streams.length - 1].at
    const previous = w.streams[w.streams.length - 2].at
    expect(created - previous, `the reopen after ${pause / 1_000} s came at the wrong pause`).toBe(pause)
  }

  // A stream that opened again starts its ladder over: the reopen after a good long life is one second, not thirty.
  w.streams[w.streams.length - 1].stream.emit('open')
  w.streams[w.streams.length - 1].stream.refuse()
  await w.advance(1_000)
  opens += 1
  expect(w.streams.length, 'a stream that opened again did not reset its reopen ladder').toBe(opens)
  expect(
    w.streams[w.streams.length - 1].at - w.streams[w.streams.length - 2].at,
    'a stream that opened again did not start its reopen ladder over from one second',
  ).toBe(1_000)

  live.stop()
  const lastStream = w.streams[w.streams.length - 1].stream
  expect(lastStream.closed, 'unmounting did not close the event stream').toBe(true)
  lastStream.refuse()
  await w.advance(60_000)
  expect(w.streams.length, 'the stream reopened after the screen went away').toBe(opens)
})

it('rows_get_the_tuis_state_rule', () => {
  const stalled = new Map<string, number>()
  const first = deriveRows([torrent({ id: 'a', download_rate: 0 })], stalled, 1_000)
  expect(first[0].rowState, 'a download first seen at zero rate is already stalled').toBe('downloading')

  const soon = deriveRows([torrent({ id: 'a', download_rate: 0 })], stalled, 30_999)
  expect(soon[0].rowState, '29.999 quiet seconds are not stalled yet').toBe('downloading')
  const late = deriveRows([torrent({ id: 'a', download_rate: 0 })], stalled, 31_000)
  expect(late[0].rowState, 'thirty quiet seconds of a running download are stalled').toBe('stalled')

  // Bytes reset the clock: the count runs again from the moment bytes arrived, not from where it
  // was, so 29.999 quiet seconds after that are still a downloading row.
  deriveRows([torrent({ id: 'a', download_rate: 4_096 })], stalled, 32_000)
  const resumedSoon = deriveRows([torrent({ id: 'a', download_rate: 0 })], stalled, 61_999)
  expect(resumedSoon[0].rowState, 'the stalled clock did not restart when bytes came back').toBe(
    'downloading',
  )
  const resumedLate = deriveRows([torrent({ id: 'a', download_rate: 0 })], stalled, 62_000)
  expect(resumedLate[0].rowState, 'thirty quiet seconds after bytes stopped should be stalled').toBe(
    'stalled',
  )

  // At 100 % everything is about to be seeding: it does not belong in the stalled column.
  const done = deriveRows([torrent({ id: 'b', download_rate: 0, progress: 1 })], stalled, 200_000)
  expect(done[0].rowState, 'a finished download with no rate is not stalled').toBe('downloading')

  // A state name newer than this mapping is a torrent that is running, stalled rule included.
  const unknown = deriveRows([torrent({ id: 'c', state: 'warming_caches' })], stalled, 1_000)
  expect(unknown[0].rowState, 'an unknown state is not a running torrent').toBe('downloading')
  const unknownLater = deriveRows([torrent({ id: 'c', state: 'warming_caches' })], stalled, 31_000)
  expect(unknownLater[0].rowState, 'an unknown state does not run the stalled clock').toBe('stalled')

  // The name mapping itself, straight from `torrent_state`.
  const names: [string, string][] = [
    ['downloading_metadata', 'metadata'],
    ['queued_for_checking', 'queued'],
    ['queued', 'queued'],
    ['checking_files', 'checking'],
    ['downloading', 'downloading'],
    ['seeding', 'seeding'],
    ['paused', 'paused'],
    ['error', 'error'],
  ]
  const clock = new Map<string, number>()
  const rows = deriveRows(names.map(([state], i) => torrent({ id: `n${i}`, state })), clock, 1_000)
  names.forEach(([state, want], i) => {
    expect(rows[i].rowState, `state ${state} should read as ${want}`).toBe(want)
  })

  // A torrent no longer on the server loses its clock entry; a torrent first seen starts now.
  const kept = new Map<string, number>()
  deriveRows([torrent({ id: 'gone' }), torrent({ id: 'here' })], kept, 1_000)
  deriveRows([torrent({ id: 'here' })], kept, 5_000)
  expect(kept.size, 'a removed torrent kept its stalled clock').toBe(1)
  expect(kept.has('gone'), 'the removed torrent is still in the clock').toBe(false)
})
