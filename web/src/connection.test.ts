/**
 * The connection: one endpoint, and the addresses every part of the page derives from it. The
 * browser's stays relative and cookie-borne; the desktop's is absolute and carries its token where a
 * header cannot go.
 *
 * @module connection.test
 */
import { expect, it } from 'vitest'
import { createConnection } from './connection'

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
