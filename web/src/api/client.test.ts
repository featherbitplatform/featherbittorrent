/**
 * The API client, against a fake fetch: the rules these tests pin are the ones a live server makes
 * no cheap test for (a session that ends mid-use, a token on a change and no token on a read), and
 * none of them needs a process, a port, or the internet.
 *
 * @module api/client.test
 */
import { expect, it } from 'vitest'
import { ApiError, createClient } from './client'

interface Seen {
  url: string
  method: string
  headers: Headers
  body: BodyInit | null
  credentials: string | null
}

/** A fetch that records what was asked and answers with whatever `answer` says. */
function record(answer: (call: Seen) => Response): { seen: Seen[]; fetchFn: typeof fetch } {
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
const UNAUTHORIZED = '{"error":{"code":"unauthorized","message":"the credentials are not valid"}}'

it('sends_the_csrf_token_on_changes_only', async () => {
  const { seen, fetchFn } = record(() => json(200, '[]'))
  let notices = 0
  const client = createClient(fetchFn, () => 'csrf-value', () => {
    notices += 1
  })

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
  await createClient(headless.fetchFn, () => null, () => {}).pause(ID)
  expect(headless.seen[0].headers.has('x-csrf-token'), 'a change sent an empty token where there is none').toBe(false)
})

it('a_401_signs_out_once_and_does_not_retry', async () => {
  const { seen, fetchFn } = record(() => json(401, UNAUTHORIZED))
  let notices = 0
  const client = createClient(fetchFn, () => 'csrf-value', () => {
    notices += 1
  })

  await expect(client.torrents(), 'a call that got 401 did not throw an ApiError').rejects.toBeInstanceOf(ApiError)
  await expect(client.torrents(), 'the second 401 was not a failure the caller could see').rejects.toBeInstanceOf(ApiError)

  expect(notices, 'a 401 did not sign the page out').toBe(1)
  expect(seen, 'a 401 made the client call the server more than once per call').toHaveLength(2)

  // An answer that arrived signed in re-arms the report: the next ended session is a second ended
  // session, and the page has to be moved again.
  const phases = [401, 200, 401]
  let phase = 0
  const later = record(() => {
    const status = phases[phase] ?? 401
    phase += 1
    return status === 200 ? json(200, '[]') : json(401, UNAUTHORIZED)
  })
  let noticesAgain = 0
  const client2 = createClient(later.fetchFn, () => 'csrf-value', () => {
    noticesAgain += 1
  })
  await expect(client2.torrents(), 'the next ended session was not reported at all').rejects.toBeInstanceOf(ApiError)
  expect(noticesAgain, 'a session that ended twice in a row signed the page out once').toBe(1)
  await expect(client2.torrents(), 'a call that arrived signed in did not succeed').resolves.toEqual([])
  await expect(client2.torrents(), 'the session that ended a second time was not refused').rejects.toBeInstanceOf(ApiError)
  expect(noticesAgain, 'a session that ended again after working did not sign the page out again').toBe(2)
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
  const client = createClient(fetchFn, () => 'csrf-value', () => {})
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
