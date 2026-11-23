/**
 * The session store, against a fake fetch: which states the endpoint's answers move it through, and
 * what the sign-in screen is shown when an answer is a refusal - and, just as much, which answers
 * are *not* a refusal, so a server that said nothing does not end up blamed on a password nobody
 * typed. No DOM, no server, no internet.
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

/** A promise the test holds the end of, so an answer can arrive exactly when the test says. */
function held(): { promise: Promise<void>; release: () => void } {
  let release = (): void => {}
  const promise = new Promise<void>((resolve) => {
    release = resolve
  })
  return { promise, release }
}

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

it('an_unreachable_server_is_not_a_sign_in_failure', async () => {
  // Two ways the status question gets no readable answer: the fetch never completed, and the server
  // answered its own failure. Neither means anybody failed to sign in - nobody tried - so the store
  // says "unreachable" and the page offers to ask again instead of asking for a password.
  const store = createSessionStore(async () => {
    throw new TypeError('fetch failed')
  })
  await store.refresh()
  expect(store.get(), 'a server that did not answer became a sign-in failure').toEqual({
    phase: 'unreachable',
  })

  const answering = createSessionStore(async () => json(500, 'upstream exploded'))
  await answering.refresh()
  expect(answering.get(), 'a server that answered its own failure became a sign-in failure').toEqual({
    phase: 'unreachable',
  })
})

it('a_failed_sign_out_after_the_session_ended_says_nothing', async () => {
  // The person pressed Sign out; while the DELETE was in flight a call elsewhere found the session
  // gone and ended it properly, notice and all. The DELETE then failed - but it was trying to end a
  // session that no longer exists, and there is nothing left to complain about on a sign-in screen
  // that is already explaining how the session ended.
  const { promise, release } = held()
  const store = createSessionStore(async (_input, init) => {
    if ((init?.method ?? 'GET') === 'DELETE') return promise.then(() => json(500, 'boom'))
    return json(200, '{"auth": true, "subject": "alice", "csrf": "csrf-gone"}')
  })
  await store.refresh()
  const mine = store.generation()

  const leaving = store.signOut()
  store.endSession(mine)
  release()
  await leaving

  const state = store.get()
  expect(
    state.phase === 'signedOut' && state.notice === 'Your session ended. Sign in again.' && state.error === null,
    'a sign-out that arrived after its session ended went looking for a screen to complain on',
  ).toBe(true)
})
