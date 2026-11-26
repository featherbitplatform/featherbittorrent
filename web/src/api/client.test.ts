/**
 * The API client, against a fake fetch: the rules these tests pin are the ones a live server makes
 * no cheap test for (a session that ends mid-use, a token on a change and no token on a read), and
 * none of them needs a process, a port, or the internet.
 *
 * The 401 tests pair the client with a real session store over the same fake, because the rule under
 * test is whose session a 401 is about: the client reports the generation its request was sent under
 * and the store decides whether that session is still current. A hand-written callback counting
 * notices could show "once" while proving nothing about a second session, so these tests read the
 * store's own states, and count its transitions where "once" is the claim.
 *
 * @module api/client.test
 */
import { expect, it } from 'vitest'
import { ApiError, createClient } from './client'
import { createSessionStore } from '../session'
import type { SessionStore, SignedOutState } from '../session'

interface Seen {
  url: string
  method: string
  headers: Headers
  body: BodyInit | null
  credentials: string | null
}

/** A fetch that records what was asked and answers with whatever `answer` says. */
function record(
  answer: (call: Seen) => Response | Promise<Response>,
): { seen: Seen[]; fetchFn: typeof fetch } {
  const seen: Seen[] = []
  const fetchFn = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const call: Seen = {
      url: String(input),
      method: init?.method ?? 'GET',
      headers: new Headers(init?.headers),
      body: init?.body ?? null,
      credentials: init?.credentials ?? null,
    }
    seen.push(call)
    return answer(call)
  }
  return { seen, fetchFn }
}

function json(status: number, body: string): Response {
  return new Response(body, { status, headers: { 'content-type': 'application/json' } })
}

const ID = 'a1b2c3d4e5f60718293a4b5c6d7e8f9012345678'
const SESSION_URL = '/api/v1/auth/web-session'
const UNAUTHORIZED = '{"error":{"code":"unauthorized","message":"the credentials are not valid"}}'
const SESSION_ENDED_COPY = 'Your session ended. Sign in again.'

/** A promise the test holds the end of, so an answer can arrive exactly when the test says. */
function held(): { promise: Promise<void>; release: () => void } {
  let release = (): void => {}
  const promise = new Promise<void>((resolve) => {
    release = resolve
  })
  return { promise, release }
}

/** The message the sign-in screen is holding, or a thrown line saying this is not the sign-in screen. */
function signedOut(store: SessionStore): SignedOutState {
  const state = store.get()
  if (state.phase !== 'signedOut') {
    throw new Error(`the page is in state ${state.phase}, not on the sign-in screen`)
  }
  return state
}

it('sends_the_csrf_token_on_changes_only', async () => {
  const { seen, fetchFn } = record(() => json(200, '[]'))
  let notices = 0
  const client = createClient(
    fetchFn,
    () => 'csrf-value',
    () => 0,
    () => {
      notices += 1
    },
  )

  await client.torrents()
  await client.pause(ID)
  await client.remove(ID, true)

  expect(seen.every((call) => call.credentials === 'same-origin'), 'a call did not send same-origin credentials').toBe(true)
  expect(seen[0].headers.get('x-csrf-token'), 'a GET carried the token a GET has no business sending').toBeNull()
  expect(seen[1].headers.get('x-csrf-token'), 'a change did not carry the CSRF token').toBe('csrf-value')
  expect(seen[2].headers.get('x-csrf-token'), 'a change did not carry the CSRF token').toBe('csrf-value')
  expect(notices, 'the page signed itself out over answers that arrived signed in').toBe(0)

  // A change with no session to prove sends no header at all, not an empty one.
  const headless = record(() => json(200, '[]'))
  await createClient(headless.fetchFn, () => null, () => 0, () => {}).pause(ID)
  expect(headless.seen[0].headers.has('x-csrf-token'), 'a change sent an empty token where there is none').toBe(false)
})

it('a_401_signs_out_once_and_does_not_retry', async () => {
  // Store and client over one fake: the load says signed in, every torrent call after it says 401.
  // The count that matters is the store's transitions, because "once" is about how many times the
  // page is moved, not about how many refusals arrive.
  const { seen, fetchFn } = record((call) =>
    call.url === SESSION_URL ? json(200, '{"auth": true, "subject": "alice", "csrf": "csrf-value"}') : json(401, UNAUTHORIZED),
  )
  const store = createSessionStore(fetchFn)
  await store.refresh()
  const client = createClient(fetchFn, store.csrf, store.generation, store.endSession)

  let moves = 0
  store.subscribe(() => {
    moves += 1
  })

  const settled = await Promise.allSettled([client.torrents(), client.torrents()])
  for (const outcome of settled) {
    expect(outcome.status === 'rejected' && outcome.reason instanceof ApiError, 'a call that got 401 did not throw an ApiError').toBe(true)
  }
  expect(seen, 'a 401 made the client call the server more than once per call').toHaveLength(3)
  expect(store.get().phase, 'a 401 did not sign the page out').toBe('signedOut')
  expect(signedOut(store).notice, 'the page did not say why it signed itself out').toBe(SESSION_ENDED_COPY)
  expect(moves, 'two 401s for one session moved the page more than once').toBe(1)

  // And a later 401 finds the page already signed out: it is refused, but it does not move the page
  // again or rewrite the words it is already showing.
  await expect(client.torrents(), 'a 401 after the sign-out was not a failure the caller could see').rejects.toBeInstanceOf(ApiError)
  expect(moves, 'a 401 after the page was already signed out moved it again').toBe(1)
  expect(signedOut(store).notice, 'a second 401 changed the notice the page was showing').toBe(SESSION_ENDED_COPY)
})

it('a_stale_401_does_not_end_a_new_session', async () => {
  // A call is in flight under session one; the person signs out and signs in again (session three);
  // the old call's 401 lands last. Slow answers outrun people, and an answer about a session that is
  // already over must not end the one the page is in now.
  const { promise, release } = held()
  const { fetchFn } = record((call) => {
    if (call.url === SESSION_URL) {
      if (call.method === 'GET') return json(200, '{"auth": true, "subject": "alice", "csrf": "csrf-one"}')
      if (call.method === 'POST') return json(200, '{"subject": "alice", "csrf": "csrf-two"}')
      return json(200, '{}')
    }
    return promise.then(() => json(401, UNAUTHORIZED))
  })
  const store = createSessionStore(fetchFn)
  await store.refresh()
  const client = createClient(fetchFn, store.csrf, store.generation, store.endSession)

  const slow = client.torrents()
  await store.signOut()
  expect(await store.signIn('alice', 'hunter2'), 'the second sign-in did not take').toBe(true)
  const fresh = store.get()
  expect(fresh.phase === 'signedIn' && fresh.csrf === 'csrf-two', 'the new session did not start as it should').toBe(true)

  release()
  await expect(slow, 'the stale 401 was not a failure the caller could see').rejects.toBeInstanceOf(ApiError)
  expect(store.get(), 'a stale 401 ended the new session').toBe(fresh)
})

it('a_401_right_after_signing_in_again_still_signs_out', async () => {
  // Session one ends by a 401; the person signs in again; the very next call is refused. A flag that
  // remembers reporting once would keep the page sitting signed in while every call fails, because a
  // new session is not the old one's second refusal.
  const { fetchFn } = record((call) => {
    if (call.url === SESSION_URL) {
      if (call.method === 'GET') return json(200, '{"auth": true, "subject": "alice", "csrf": "csrf-one"}')
      if (call.method === 'POST') return json(200, '{"subject": "alice", "csrf": "csrf-two"}')
      return json(200, '{}')
    }
    return json(401, UNAUTHORIZED)
  })
  const store = createSessionStore(fetchFn)
  await store.refresh()
  expect(store.get().phase, 'the first session did not start signed in').toBe('signedIn')
  const client = createClient(fetchFn, store.csrf, store.generation, store.endSession)

  await expect(client.torrents(), 'the first 401 was not a failure the caller could see').rejects.toBeInstanceOf(ApiError)
  expect(signedOut(store).notice, 'the 401 did not end the first session with a reason').toBe(SESSION_ENDED_COPY)

  expect(await store.signIn('alice', 'hunter2'), 'signing in again did not take').toBe(true)
  await expect(client.torrents(), 'the 401 after signing in again was not a failure the caller could see').rejects.toBeInstanceOf(ApiError)

  const after = store.get()
  expect(after.phase === 'signedOut' && after.notice === SESSION_ENDED_COPY, 'the first 401 after signing in again did not sign the page out').toBe(true)
})

it('a_late_401_leaves_the_failed_sign_in_alone', async () => {
  // Nobody was signed in when this call went out, and the answer arrives after the person has tried
  // a password and failed. The refusal stands as a failure of its own, but it has no business
  // rewriting the sign-in screen with a session that never started here.
  const { promise, release } = held()
  const { fetchFn } = record((call) =>
    call.url === SESSION_URL ? json(401, UNAUTHORIZED) : promise.then(() => json(401, UNAUTHORIZED)),
  )
  const store = createSessionStore(fetchFn)
  const client = createClient(fetchFn, store.csrf, store.generation, store.endSession)

  const late = client.torrents()
  await store.refresh()
  expect(await store.signIn('alice', 'nope'), 'a wrong password was treated as signed in').toBe(false)

  release()
  await expect(late, 'the late 401 was not a failure the caller could see').rejects.toBeInstanceOf(ApiError)
  expect(signedOut(store).error, 'a late 401 rewrote the reason the last sign-in failed').toBe('Wrong user name or password.')
  expect(signedOut(store).notice, 'a late 401 gave the sign-in screen a session that never started').toBeNull()
})

it('a_failed_sign_out_keeps_the_session', async () => {
  // The person pressed Sign out and the server said 500. The cookie and the session it belongs to
  // survived that - only the request to end them failed - so the page stays signed in, says the sign
  // out did not work, and keeps proving the next change with the same token.
  const { seen, fetchFn } = record((call) => {
    if (call.url === SESSION_URL) {
      if (call.method === 'DELETE') return json(500, '{"error":{"code":"internal_error","message":"something broke"}}')
      return json(200, '{"auth": true, "subject": "alice", "csrf": "csrf-keep"}')
    }
    return new Response(null, { status: 204 })
  })
  const store = createSessionStore(fetchFn)
  await store.refresh()
  const client = createClient(fetchFn, store.csrf, store.generation, store.endSession)

  await store.signOut()
  const state = store.get()
  expect(
    state.phase === 'signedIn' && state.subject === 'alice' && state.csrf === 'csrf-keep' && state.error === 'Could not sign out. Try again.',
    'a failed sign-out logged the person out anyway',
  ).toBe(true)

  await client.pause(ID)
  const change = seen[seen.length - 1]
  if (!change) throw new Error('no call was made')
  expect(change.headers.get('x-csrf-token'), 'the change after a failed sign-out did not carry the session token').toBe('csrf-keep')
})

it('encodes_path_segments_and_query_values', async () => {
  const { seen, fetchFn } = record((call) => {
    if (call.method === 'DELETE') return new Response(null, { status: 204 })
    if (call.method === 'POST') return json(201, '{"name":"added"}')
    if (call.url.startsWith('/api/v1/fs/')) {
      return json(200, call.url.startsWith('/api/v1/fs/list') ? '{"path":"/tmp","dirs":[]}' : '[]')
    }
    if (call.url.includes('/pieces') || call.url === '/api/v1/session') return json(200, '{}')
    return json(200, '[]')
  })
  const client = createClient(fetchFn, () => 'csrf-value', () => 0, () => {})
  const last = (): Seen => {
    const call = seen[seen.length - 1]
    if (!call) throw new Error('no call was made')
    return call
  }

  await client.torrent('a b/c')
  expect(last().url, 'a path segment went out without its encoding').toBe('/api/v1/torrents/a%20b%2Fc')
  await client.pause('a b/c')
  expect(last().url, 'a change path went out without its encoding').toBe('/api/v1/torrents/a%20b%2Fc/pause')

  await client.remove(ID, true)
  expect(last().url, 'a remove with its files did not say delete_files=true').toBe(
    `/api/v1/torrents/${ID}?delete_files=true`,
  )
  await client.remove(ID, false)
  expect(last().url, 'a remove that keeps its files did not say delete_files=false').toBe(
    `/api/v1/torrents/${ID}?delete_files=false`,
  )

  const bytes = new Uint8Array([0x64, 0x38, 0x61, 0x6e, 0x6e, 0x65]).buffer
  await client.addMagnet('magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567', '/Downloads/TV Shows', true)
  expect(
    last().url,
    'a JSON add carried options in the query string, where the pinned server refuses them',
  ).toBe('/api/v1/torrents')
  expect(last().headers.get('content-type'), 'a JSON add did not say it was JSON').toBe('application/json')
  expect(
    JSON.parse(String(last().body)),
    'the magnet body was not the shape the API document describes',
  ).toEqual({
    magnet: 'magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567',
    save_path: '/Downloads/TV Shows',
    paused: true,
  })

  await client.addFile(bytes, '/Save/Path', false)
  expect(last().url, 'a file add did not carry its options as encoded query values').toBe(
    '/api/v1/torrents?save_path=%2FSave%2FPath&paused=false',
  )
  expect(last().headers.get('content-type'), 'a file add did not say what it was').toBe(
    'application/x-bittorrent',
  )
  expect(last().body, 'the .torrent bytes were not sent as they were given').toBe(bytes)

  await client.list('/one two/three')
  expect(last().url, 'the folder path was not encoded').toBe('/api/v1/fs/list?path=%2Fone%20two%2Fthree')
  await client.roots()
  expect(last().url, 'the roots call went somewhere else').toBe('/api/v1/fs/roots')
})

it('the_media_calls_ask_where_the_routes_answer', async () => {
  const { seen, fetchFn } = record((call) =>
    call.method === 'POST'
      ? json(201, '{"path":"/api/v1/torrents/a1/files/0/stream?token=t","expires_in":60}')
      : json(200, '{"cues":[{"start_ms":0,"end_ms":1000,"text":"hi"}]}'),
  )
  const client = createClient(fetchFn, () => 'csrf-value', () => 0, () => {})
  const last = (): Seen => {
    const call = seen[seen.length - 1]
    if (!call) throw new Error('no call was made')
    return call
  }

  await client.probe(ID, 2)
  expect(last().url, 'the probe did not ask the media route').toBe(`/api/v1/media/${ID}/2`)
  expect(last().headers.has('x-csrf-token'), 'a probe, a read, carried the token a read has no business sending').toBe(false)

  await client.cues(ID, 2, 4, 1000, 61000)
  expect(last().url, 'the cue window was not asked with its bounds encoded').toBe(
    `/api/v1/media/${ID}/2/tracks/4/cues?from_ms=1000&to_ms=61000`,
  )

  await client.subtitleFile(ID, 2, 7)
  expect(last().url, 'the subtitle file was not asked by its index').toBe(`/api/v1/media/${ID}/2/subtitles/7`)

  expect(client.mediaStreamUrl(ID, 2, 1, 2000), 'the stream url was not the route the stream answers on').toBe(
    `/api/v1/media/${ID}/2/tracks/1/stream?start_ms=2000`,
  )

  const token = await client.streamToken(ID, 0)
  expect(token, 'the stream token call returned the path the answer names').toBe(
    '/api/v1/torrents/a1/files/0/stream?token=t',
  )
  const tokenCall = last()
  expect(tokenCall.url, 'the token was not asked at the token route').toBe('/api/v1/stream-tokens')
  expect(tokenCall.method, 'the token was not asked with a POST').toBe('POST')
  expect(tokenCall.headers.get('x-csrf-token'), 'the token request did not carry the CSRF token').toBe('csrf-value')
  expect(tokenCall.headers.get('content-type'), 'the token request did not say it was JSON').toBe('application/json')
  expect(JSON.parse(String(tokenCall.body)), 'the token body was not the shape the route takes').toEqual({
    torrent: ID,
    file: 0,
  })
})

it('sends_whole_non_negative_milliseconds', async () => {
  const { seen, fetchFn } = record(() => json(200, '{"cues":[]}'))
  const client = createClient(fetchFn, () => 'csrf-value', () => 0, () => {})
  const last = (): Seen => {
    const call = seen[seen.length - 1]
    if (!call) throw new Error('no call was made')
    return call
  }

  await client.cues(ID, 2, 4, -5, 61000.9)
  expect(last().url, 'a cue bound was not sent as a whole non-negative millisecond').toBe(
    `/api/v1/media/${ID}/2/tracks/4/cues?from_ms=0&to_ms=61000`,
  )

  expect(
    client.mediaStreamUrl(ID, 2, 1, -3.7),
    'a negative stream start was not clamped to the first millisecond',
  ).toBe(`/api/v1/media/${ID}/2/tracks/1/stream?start_ms=0`)
  expect(
    client.mediaStreamUrl(ID, 2, 1, 2000.4),
    'a fractional stream start was not sent whole',
  ).toBe(`/api/v1/media/${ID}/2/tracks/1/stream?start_ms=2000`)
  expect(
    client.mediaStreamUrl(ID, 2, 1, Number.NaN),
    'a non-finite stream start was not sent as a number the route would refuse',
  ).toBe(`/api/v1/media/${ID}/2/tracks/1/stream?start_ms=0`)
})

it('a_stream_token_answer_without_a_path_is_an_error', async () => {
  const { fetchFn } = record((call) =>
    call.method === 'POST' ? json(201, '{"expires_in":60}') : json(200, '[]'),
  )
  const client = createClient(fetchFn, () => 'csrf-value', () => 0, () => {})
  await expect(
    client.streamToken(ID, 0),
    'a token answer without a path was not a failure the caller could see',
  ).rejects.toThrow()
})

/** One client over a fake that answers every call with `status` and `body`, carrying a CSRF token. */
function clientFor(status = 200, body = '{}') {
  const { seen, fetchFn } = record(() => (status === 204 ? new Response(null, { status }) : json(status, body)))
  return { seen, client: createClient(fetchFn, () => 'tok', () => 0, () => {}) }
}

it('each call sends its method, path and body', async () => {
  const device = '{"name":"phone","fingerprint":"ab:cd"}'
  const cases: Array<{
    run: (c: ReturnType<typeof createClient>) => Promise<unknown>
    method: string
    url: string
    body: unknown
    answer?: string
    status?: number
  }> = [
    { run: (c) => c.setFilePriorities(ID, ['skip', 'high']), method: 'PUT', url: `/api/v1/torrents/${ID}/file-priorities`, body: { priorities: ['skip', 'high'] }, status: 204, answer: '' },
    { run: (c) => c.setQueuePosition(ID, 3), method: 'PUT', url: `/api/v1/torrents/${ID}/queue-position`, body: { position: 3 }, status: 204, answer: '' },
    { run: (c) => c.recheck(ID), method: 'POST', url: `/api/v1/torrents/${ID}/recheck`, body: undefined, status: 204, answer: '' },
    { run: (c) => c.reannounce(ID), method: 'POST', url: `/api/v1/torrents/${ID}/reannounce`, body: undefined, status: 204, answer: '' },
    { run: (c) => c.setTorrentLimits(ID, 100, null), method: 'PUT', url: `/api/v1/torrents/${ID}/limits`, body: { download: 100, upload: null }, status: 204, answer: '' },
    { run: (c) => c.moveStorage(ID, '/data/x'), method: 'POST', url: `/api/v1/torrents/${ID}/move`, body: { path: '/data/x', mode: 'fail_if_exists' }, status: 204, answer: '' },
    { run: (c) => c.setSessionLimits(null, 5), method: 'PUT', url: '/api/v1/session/limits', body: { download: null, upload: 5 }, status: 204, answer: '' },
    { run: (c) => c.queueSettings(), method: 'GET', url: '/api/v1/session/queue', body: undefined, answer: '{"active_downloads":1}' },
    { run: (c) => c.setQueueSettings({ active_downloads: 2, dont_count_slow_torrents: true, inactive_down_rate: 1, inactive_up_rate: 1, interval: 30, startup_grace: 60 }), method: 'PUT', url: '/api/v1/session/queue', body: { active_downloads: 2, dont_count_slow_torrents: true, inactive_down_rate: 1, inactive_up_rate: 1, interval: 30, startup_grace: 60 }, status: 204, answer: '' },
    { run: (c) => c.devices(), method: 'GET', url: '/api/v1/auth/devices', body: undefined, answer: `[${device}]` },
    { run: (c) => c.addDevice('phone', 'KEY'), method: 'POST', url: '/api/v1/auth/devices', body: { name: 'phone', public_key: 'KEY' }, status: 201, answer: device },
    { run: (c) => c.removeDevice('phone'), method: 'DELETE', url: '/api/v1/auth/devices/phone', body: undefined, status: 204, answer: '' },
    { run: (c) => c.mkdir('/data', 'new'), method: 'POST', url: '/api/v1/fs/mkdir', body: { parent: '/data', name: 'new' }, status: 201, answer: '{"path":"/data/new"}' },
  ]
  for (const k of cases) {
    const { seen, client } = clientFor(k.status ?? 200, k.answer ?? '{}')
    await k.run(client)
    const call = seen[0]
    const label = `${k.method} ${k.url}`
    expect(call.method, `${label}: wrong method`).toBe(k.method)
    expect(call.url, `${label}: wrong url`).toBe(k.url)
    if (k.body === undefined) {
      expect(call.body, `${label}: sent a body`).toBeNull()
    } else {
      expect(JSON.parse(String(call.body)), `${label}: wrong body`).toEqual(k.body)
      expect(call.headers.get('content-type'), `${label}: no json content type`).toBe('application/json')
    }
    if (k.method !== 'GET') expect(call.headers.get('x-csrf-token'), `${label}: no csrf header`).toBe('tok')
  }
  const { client } = clientFor(201, '{"path":"/data/new"}')
  expect(await client.mkdir('/data', 'new')).toBe('/data/new')
})

it('a device name is escaped in the path', async () => {
  const { seen, client } = clientFor(204, '')
  await client.removeDevice('a b/c')
  expect(seen[0].url).toBe('/api/v1/auth/devices/a%20b%2Fc')
})

it('an error answer keeps its code', async () => {
  const { client } = clientFor(403, '{"error":{"code":"forbidden","message":"no"}}')
  const failure = await client.devices().catch((e: unknown) => e)
  expect(failure).toBeInstanceOf(ApiError)
  expect((failure as ApiError).code).toBe('forbidden')
})

it('a mkdir answer without a path is an error', async () => {
  const { client } = clientFor(201, '{}')
  await expect(client.mkdir('/data', 'new')).rejects.toThrow()
})

const BASE = 'http://127.0.0.1:53211'

it('bearer_client_prefixes_base_and_sends_the_token', async () => {
  const { seen, fetchFn } = record(() => json(200, '[]'))
  const client = createClient(fetchFn, () => null, () => 0, () => {}, { kind: 'bearer', base: BASE, token: 'desk-token' })

  await client.torrents()
  await client.pause(ID)

  expect(seen[0].url, 'a bearer read went to the page origin instead of the engine').toBe(`${BASE}/api/v1/torrents`)
  expect(seen[1].url, 'a bearer change went to the page origin instead of the engine').toBe(`${BASE}/api/v1/torrents/${ID}/pause`)
  expect(
    seen.every((call) => call.headers.get('authorization') === 'Bearer desk-token'),
    'a bearer call went out without its token',
  ).toBe(true)
})

it('bearer_client_omits_credentials_and_csrf', async () => {
  const { seen, fetchFn } = record(() => json(200, '[]'))
  const client = createClient(fetchFn, () => 'csrf-value', () => 0, () => {}, { kind: 'bearer', base: `${BASE}/`, token: 'desk-token' })

  await client.torrents()
  await client.pause(ID)
  await client.remove(ID, false)

  expect(seen[0].url, 'a trailing slash on the base doubled up in the path').toBe(`${BASE}/api/v1/torrents`)
  expect(seen.every((call) => call.credentials === 'omit'), 'a bearer call rode cookies along').toBe(true)
  expect(seen.some((call) => call.headers.has('x-csrf-token')), 'a bearer call carried a CSRF token').toBe(false)
})

it('same_origin_client_is_unchanged', async () => {
  const { seen, fetchFn } = record(() => json(200, '[]'))
  const client = createClient(fetchFn, () => 'csrf-value', () => 0, () => {}, { kind: 'sameOrigin' })

  await client.torrents()
  await client.pause(ID)

  expect(seen[0].url, 'the browser client stopped using relative paths').toBe('/api/v1/torrents')
  expect(seen.every((call) => call.credentials === 'same-origin'), 'the browser client dropped its cookie').toBe(true)
  expect(seen[1].headers.get('x-csrf-token'), 'the browser client dropped the CSRF token').toBe('csrf-value')
  expect(seen.some((call) => call.headers.has('authorization')), 'the browser client sent an Authorization header').toBe(false)
  expect(
    client.mediaStreamUrl(ID, 0, 1, 1500),
    'the browser stream path changed',
  ).toBe(`/api/v1/media/${ID}/0/tracks/1/stream?start_ms=1500`)
})

it('bearer_stream_paths_are_absolute', async () => {
  const { fetchFn } = record((call) =>
    call.url.endsWith('/stream-tokens') && call.body !== null && String(call.body).includes('"file":1')
      ? json(200, '{"path":"/api/v1/stream/abc?token=st-1"}')
      : json(200, '{"path":"/api/v1/stream/abc"}'),
  )
  const client = createClient(fetchFn, () => null, () => 0, () => {}, { kind: 'bearer', base: BASE, token: 'desk token' })

  expect(await client.streamToken(ID, 1), 'a stream token path stayed relative, or lost its own token').toBe(
    `${BASE}/api/v1/stream/abc?token=st-1`,
  )
  expect(await client.streamToken(ID, 0), 'a stream path with no token of its own did not get the bearer one').toBe(
    `${BASE}/api/v1/stream/abc?token=desk%20token`,
  )
  expect(client.mediaStreamUrl(ID, 0, 1, 1500), 'the remux stream URL is not absolute and authorized').toBe(
    `${BASE}/api/v1/media/${ID}/0/tracks/1/stream?start_ms=1500&token=desk%20token`,
  )
})

it('bearer_token_is_not_added_to_a_foreign_absolute_stream_url', async () => {
  // The stream path the token answer names is not always under the engine's own base: an answer can
  // point at somebody else's server, and that server has no business seeing this session's token. A
  // base-name lookalike is somebody else too.
  const paths = [
    'https://cdn.example.test/stream/abc',
    `${BASE}1/stream/lookalike`,
    `${BASE}/stream/onbase`,
    '/stream/relative',
  ]
  let asked = 0
  const { fetchFn } = record(() => json(200, `{"path":"${paths[asked++]}"}`))
  const client = createClient(fetchFn, () => null, () => 0, () => {}, { kind: 'bearer', base: BASE, token: 'desk-token' })

  expect(await client.streamToken(ID, 0), 'the bearer token went to somebody else\'s server').toBe(
    'https://cdn.example.test/stream/abc',
  )
  expect(await client.streamToken(ID, 1), 'the bearer token went to a base-name lookalike').toBe(
    `${BASE}1/stream/lookalike`,
  )
  expect(await client.streamToken(ID, 2), 'a stream on the engine\'s own base lost its token').toBe(
    `${BASE}/stream/onbase?token=desk-token`,
  )
  expect(await client.streamToken(ID, 3), 'a relative stream path lost its token').toBe(
    `${BASE}/stream/relative?token=desk-token`,
  )
})

const SETTINGS_ANSWER = JSON.stringify({
  speed: { download_kib: null, upload_kib: null, active_downloads: null, active_seeds: null, active_total: null },
  downloads: {
    default_dir: '/data',
    unfinished_dir: null,
    start_paused: false,
    watch_dir: null,
    on_complete: { program: null, editable: false },
    watch_problem: null,
    on_complete_problem: null,
  },
})

it('the settings routes use the product paths and carry the csrf token on a change', async () => {
  const speed = { download_kib: 64, upload_kib: null, active_downloads: 3, active_seeds: null, active_total: 8 }
  const read = clientFor(200, SETTINGS_ANSWER)
  expect((await read.client.settings()).downloads.default_dir).toBe('/data')
  expect([read.seen[0].method, read.seen[0].url]).toEqual(['GET', '/api/v1/settings'])

  const put = clientFor(200, SETTINGS_ANSWER)
  await put.client.saveSpeed(speed)
  expect([put.seen[0].method, put.seen[0].url]).toEqual(['PUT', '/api/v1/settings/speed'])
  expect(JSON.parse(String(put.seen[0].body))).toEqual(speed)
  expect(put.seen[0].headers.get('x-csrf-token')).toBe('tok')
})

it('save_downloads_never_sends_on_complete', async () => {
  const { seen, client } = clientFor(200, SETTINGS_ANSWER)
  const input = {
    default_dir: '/data',
    unfinished_dir: '/data/part',
    start_paused: true,
    watch_dir: null,
    // A caller that holds the whole object must not be able to send the program through this page.
    on_complete: { program: '/bin/evil', editable: true },
  }
  await client.saveDownloads(input)
  expect([seen[0].method, seen[0].url]).toEqual(['PUT', '/api/v1/settings/downloads'])
  const sent = JSON.parse(String(seen[0].body)) as Record<string, unknown>
  expect(sent).toEqual({ default_dir: '/data', unfinished_dir: '/data/part', start_paused: true, watch_dir: null })
  expect('on_complete' in sent).toBe(false)
})
