/**
 * The session store, against a fake fetch: which states the endpoint's answers move it through, and
 * what the sign-in screen is shown when an answer is a refusal. No DOM, no server, no internet.
 *
 * @module session.test
 */
import { expect, it } from 'vitest'
import { createSessionStore } from './session'
import type { SessionStore } from './session'

function json(status: number, body: string): Response {
  return new Response(body, { status, headers: { 'content-type': 'application/json' } })
}

const UNAUTHORIZED = '{"error":{"code":"unauthorized","message":"the credentials are not valid"}}'

/** The message the screen is holding, or a thrown line saying this is not the sign-in screen. */
function shown(store: SessionStore): { notice: string | null; error: string | null } {
  const state = store.get()
  if (state.phase !== 'signedOut') {
    throw new Error(`the page is in state ${state.phase}, not on the sign-in screen`)
  }
  return state
}

it('logins_off_skips_sign_in', async () => {
  const store = createSessionStore(async () => json(200, '{"auth": false}'))
  expect(store.get().phase, 'the page claimed a state before the server answered').toBe('loading')

  await store.refresh()

  expect(store.get(), 'logins off did not take the page straight past sign-in').toEqual({ phase: 'open' })
  expect(store.csrf(), 'logins off put a CSRF token in the page for nobody').toBeNull()
})

it('sign_in_failure_shows_the_server_message', async () => {
  // The answers the pinned server actually sends, in the order this test asks them: the load-time
  // status, then three refusals. The two the person can act on are shown in the page's words, and
  // the one that is not (a 403 from the origin guard) is shown in the server's own.
  const answers = [
    () => json(401, UNAUTHORIZED),
    () => json(401, UNAUTHORIZED),
    () => json(429, '{"error":{"code":"too_many_requests","message":"too many requests, retry later"}}'),
    () => json(403, '{"error":{"code":"forbidden","message":"the request came from an origin this server does not answer"}}'),
  ]
  let asked = 0
  const store = createSessionStore(async () => {
    const answer = answers[asked]
    if (!answer) throw new Error('the fake server ran out of answers')
    asked += 1
    return answer()
  })

  await store.refresh()
  expect(shown(store).notice, 'the first 401 on the page was announced as an ended session').toBeNull()

  expect(await store.signIn('alice', 'nope'), 'a wrong password was treated as signed in').toBe(false)
  expect(shown(store).error, 'a wrong password did not ask for a new one').toBe('Wrong user name or password.')

  expect(await store.signIn('alice', 'nope'), 'a spent attempt budget was treated as signed in').toBe(false)
  expect(shown(store).error, 'a spent attempt budget did not say to wait a minute').toBe(
    'Too many attempts. Try again in a minute.',
  )

  expect(await store.signIn('alice', 'nope'), 'a 403 was treated as signed in').toBe(false)
  expect(shown(store).error, 'the sign-in screen did not pass on the server\'s own message').toBe(
    'the request came from an origin this server does not answer',
  )
})
