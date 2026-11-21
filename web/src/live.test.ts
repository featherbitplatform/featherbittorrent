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
 * @module live.test
 */
import { expect, it } from 'vitest'
import { createLiveAnnouncer, UNREACHABLE_COPY } from './live'
import { createSessionStore } from './session'

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
