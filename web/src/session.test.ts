// @vitest-environment jsdom
/**
 * The session store, against a fake fetch: which states the endpoint's answers move it through, and
 * what the sign-in screen is shown when an answer is a refusal - and, just as much, which answers
 * are *not* a refusal, so a server that said nothing does not end up blamed on a password nobody
 * typed. No DOM, no server, no internet.
 *
 * @module session.test
 */
import { expect, it, vi } from 'vitest'
import { SESSION_ENDED, createSessionStore } from './session'
import type { SessionStore } from './session'
import { createClient } from './api/client'

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

const BASE = 'http://127.0.0.1:53211'

it('bearer_sign_in_uses_basic_and_keeps_the_token_in_memory', async () => {
  const setItem = vi.spyOn(Storage.prototype, 'setItem')
  const cookie = vi.spyOn(document, 'cookie', 'set')
  const seen: Array<{ url: string; init: RequestInit | undefined }> = []
  const store = createSessionStore(async (input, init) => {
    seen.push({ url: String(input), init })
    return json(200, '{"token":"tok-1","expires_in":3600,"subject":"alice"}')
  }, { kind: 'bearer', base: BASE, token: null })

  expect(shown(store), 'a bearer connection with no token did not start on the sign-in screen').toEqual({
    phase: 'signedOut',
    notice: null,
    error: null,
  })
  expect(await store.signIn('alice', 'pässword'), 'a good password did not sign in').toBe(true)

  expect(seen.length, 'sign-in asked the server more than once').toBe(1)
  expect(seen[0].url, 'sign-in did not go to the engine').toBe(`${BASE}/api/v1/auth/session`)
  expect(seen[0].init?.method, 'sign-in was not a POST').toBe('POST')
  expect(seen[0].init?.credentials, 'sign-in rode cookies along').toBe('omit')
  const auth = new Headers(seen[0].init?.headers).get('authorization')
  const expected = btoa(String.fromCharCode(...new TextEncoder().encode('alice:pässword')))
  expect(auth, 'sign-in did not use Basic with the UTF-8 user and password').toBe(`Basic ${expected}`)
  const state = store.get()
  expect(state.phase === 'signedIn' && state.subject === 'alice', 'the answer subject was not the session').toBe(true)
  expect(store.token(), 'the token the server handed out was not kept').toBe('tok-1')
  expect(store.csrf(), 'a bearer session claimed a CSRF value').toBeNull()
  expect(setItem, 'the token was written to web storage').not.toHaveBeenCalled()
  expect(cookie, 'the token was written to a cookie').not.toHaveBeenCalled()
  setItem.mockRestore()
  cookie.mockRestore()
})

it('bearer_401_returns_to_sign_in_with_session_ended', async () => {
  const store = createSessionStore(async () => json(401, UNAUTHORIZED), { kind: 'bearer', base: BASE, token: 'launch-secret' })
  const state = store.get()
  expect(state.phase === 'signedIn' && state.subject === 'desktop', 'a bearer token did not start signed in as desktop').toBe(true)
  await store.refresh()
  expect(store.get().phase, 'refresh moved a desktop session that nothing ended').toBe('signedIn')

  store.endSession(store.generation())

  expect(shown(store), 'a 401 did not return to sign-in with the ended notice').toEqual({
    phase: 'signedOut',
    notice: SESSION_ENDED,
    error: null,
  })
  expect(store.token(), 'the ended token was kept').toBeNull()
})

it('bearer_sign_out_deletes_the_session_with_the_token', async () => {
  const seen: Array<{ url: string; init: RequestInit | undefined }> = []
  const store = createSessionStore(async (input, init) => {
    seen.push({ url: String(input), init })
    if (init?.method === 'DELETE') return new Response(null, { status: 204 })
    return json(200, '{"token":"tok-1","expires_in":3600,"subject":"alice"}')
  }, { kind: 'bearer', base: BASE, token: null })
  await store.signIn('alice', 'password')

  await store.signOut()

  const del = seen[1]
  expect(del?.init?.method, 'sign-out did not DELETE the session').toBe('DELETE')
  expect(del?.url, 'sign-out did not go to the engine').toBe(`${BASE}/api/v1/auth/session`)
  expect(new Headers(del?.init?.headers).get('authorization'), 'sign-out did not prove which session to end').toBe(
    'Bearer tok-1',
  )
  expect(del?.init?.credentials, 'sign-out rode cookies along').toBe('omit')
  expect(shown(store), 'a sign-out the server took did not land on sign-in').toEqual({
    phase: 'signedOut',
    notice: null,
    error: null,
  })
  expect(store.token(), 'the ended token was kept').toBeNull()
})

it('bearer_sign_out_failure_keeps_the_session', async () => {
  const seen: Array<{ url: string; auth: string | null }> = []
  const fetchFn = (async (input: RequestInfo | URL, init?: RequestInit) => {
    seen.push({ url: String(input), auth: new Headers(init?.headers).get('authorization') })
    if (init?.method === 'DELETE') return json(500, 'boom')
    if (String(input).endsWith('/auth/session')) return json(200, '{"token":"tok-1","expires_in":3600,"subject":"alice"}')
    return json(200, '[]')
  }) as typeof fetch
  const store = createSessionStore(fetchFn, { kind: 'bearer', base: BASE, token: null })
  await store.signIn('alice', 'password')

  await store.signOut()

  const state = store.get()
  expect(
    state.phase === 'signedIn' && state.subject === 'alice' && state.error === 'Could not sign out. Try again.',
    'a refused sign-out did not keep the session with its error',
  ).toBe(true)
  expect(store.token(), 'a refused sign-out dropped a token the server still honours').toBe('tok-1')
  const client = createClient(fetchFn, store.csrf, store.generation, store.endSession, { kind: 'bearer', base: BASE, token: null }, store.token)
  await client.torrents()
  expect(seen[seen.length - 1].auth, 'the next call went out without the kept token').toBe('Bearer tok-1')
})
