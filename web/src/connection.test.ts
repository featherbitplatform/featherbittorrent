/**
 * The connection: one endpoint, and the addresses every part of the page derives from it. The
 * browser's stays relative and cookie-borne; the desktop's is absolute and carries its token where a
 * header cannot go.
 *
 * @module connection.test
 */
import { expect, it } from 'vitest'
import { createConnection } from './connection'
import { ApiError } from './api/client'
import { SESSION_ENDED } from './session'

const BASE = 'http://127.0.0.1:53211'
const unused = (async () => {
  throw new Error('nothing here should reach the network')
}) as typeof fetch

it('bearer_events_url_carries_the_token', () => {
  const desktop = createConnection({ kind: 'bearer', base: `${BASE}/`, token: 'a+b/c' }, unused)
  expect(desktop.eventsUrl('categories=status'), 'the event stream went out without the token').toBe(
    `${BASE}/api/v1/events?categories=status&token=a%2Bb%2Fc`,
  )
  expect(desktop.url('/api/v1/torrents'), 'a bearer path was not made absolute').toBe(`${BASE}/api/v1/torrents`)

  const browser = createConnection({ kind: 'sameOrigin' }, unused)
  expect(browser.eventsUrl('categories=status'), 'the browser event stream changed').toBe(
    '/api/v1/events?categories=status',
  )
  expect(browser.url('/api/v1/torrents'), 'a browser path stopped being relative').toBe('/api/v1/torrents')
})

it('bearer_client_401_ends_the_session_through_the_connection', async () => {
  // The wiring this pins is `createConnection` itself: the client it builds reports its 401 to the
  // store it built, not to some other session. A desktop built from the parts by hand could wire
  // them to each other wrong and every screen would still call.
  const desktop = createConnection({ kind: 'bearer', base: BASE, token: 'launch-secret' }, async (input) => {
    if (String(input) === `${BASE}/api/v1/torrents`) {
      return new Response('{"error":{"code":"unauthorized","message":"the token is not valid"}}', {
        status: 401,
        headers: { 'content-type': 'application/json' },
      })
    }
    throw new Error(`nothing here answers ${String(input)}`)
  })
  const started = desktop.session.get()
  expect(started.phase === 'signedIn', 'a bearer connection did not start signed in with its token').toBe(true)

  await expect(desktop.api.torrents(), 'the 401 answer did not fail the call').rejects.toBeInstanceOf(ApiError)

  const after = desktop.session.get()
  expect(
    after.phase === 'signedOut' && after.notice === SESSION_ENDED,
    'the 401 from a bearer client call did not reach the session store through the connection',
  ).toBe(true)
  expect(desktop.session.token(), 'the ended session kept its token').toBeNull()
})
