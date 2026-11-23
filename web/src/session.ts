/**
 * The browser session: who this page speaks as, and the value it proves that with.
 *
 * The session token itself is never here. The server put it in a `HttpOnly` cookie a script cannot
 * read, so this module knows only what the two answers it makes tell it: whether logins are on at
 * all, who the cookie belongs to, and the CSRF value that belongs with it. That value lives in this
 * closure and nowhere else. `localStorage` and `sessionStorage` survive things a browser session
 * should not: a token copied into one of them outlives the server's expiry, sits readable by
 * anything that can run script on this origin, and goes stale the moment the cookie is cleared
 * somewhere else. A page that loses the CSRF value loses nothing a reload cannot get back, because
 * `GET /api/v1/auth/web-session` hands it out again from the cookie that is already trusted.
 *
 * Four states come from that one endpoint and one is this module's own:
 * `loading` while the first answer is on its way, `open` when logins are off and nobody has to sign
 * in, `signedIn` with the subject and the CSRF value, `signedOut` when the cookie answers nothing,
 * and `unreachable` when the question itself got no answer to read - the fetch did not complete or
 * the server answered with a failure. `unreachable` is deliberately not `signedOut` with an error:
 * nobody tried to sign in, and a screen that asks for a password when the server is simply not
 * answering tells the person the wrong story.
 *
 * The `401` that means signed-out is not an error, and the first one of those on page load is not a
 * lost session either: it is the ordinary starting point. The "session ended" notice exists
 * separately, because only the API client can discover a session that was working and stopped; that
 * is why `endSession` is its own door rather than a flag the `GET` could set.
 *
 * `endSession` takes the *generation* of the session the finding call was made under, and the store
 * keeps that number. It moves every time a session starts or ends: a successful sign-in, a load
 * that answers signed in, a sign-out that took, and the ending itself. That ties the notice to a
 * session instead of to whichever `401` happens to arrive last. A request sent under an old session
 * can answer `401` after the person has signed in again - slow ones always outrun a person - and an
 * answer about a session that is already over must not end the new one. A flag on the client cannot
 * say that: only this store knows which session is current.
 *
 * The states and the actions are a plain store (get / subscribe / refresh / signIn / signOut /
 * generation / endSession), so the rules are testable without a DOM, and `useSession` is the React
 * reading of that store rather than a second implementation of it.
 *
 * The desktop app has no cookie: it talks to an engine at an address of its own with a bearer token
 * (a `bearer` endpoint). Its store keeps the same states and the same generation rule, with two
 * differences. A token handed in at launch is a session from the start (`signedIn` as `desktop`,
 * nothing to ask the server); with none, the page starts on the sign-in screen and a sign-in is
 * `POST /api/v1/auth/session` with `Authorization: Basic`, whose answer's token lives in this closure
 * only, for the same reasons the CSRF value does. A `401` ends it exactly as it ends a cookie session.
 *
 * @module session
 */
import { useEffect, useSyncExternalStore } from 'react'
import { SAME_ORIGIN, resolve } from './endpoint'
import type { Endpoint } from './endpoint'

/** The one endpoint every question about this session asks. It is this server's own, not part of the generated API document, which is why its shapes are written by hand here. */
const ENDPOINT = '/api/v1/auth/web-session'

/** The desktop's sign-in: user and password in, a bearer token out. */
const BEARER_SIGN_IN = '/api/v1/auth/session'

/** Who a session started from a launch token is: the desktop app's own engine has no user name to show. */
export const DESKTOP_SUBJECT = 'desktop'

/**
 * The notice the Sign in screen shows when a call finds the session gone mid-use. The plan names
 * this string; only `endSession` puts it into the state, which is what keeps a plain page-load `401`
 * from claiming a session it never had.
 */
export const SESSION_ENDED = 'Your session ended. Sign in again.'

/** The sign-in answer for a `401`, in the page's words: the server's own copy is the API's lowercase problem text, not something to show a person reaching for the password again. */
const WRONG_CREDENTIALS = 'Wrong user name or password.'

/** The sign-in answer for a `429`. The server says the same thing as a retry-after number; this is what a person reads. */
const RATE_LIMITED = 'Too many attempts. Try again in a minute.'

/** What the screen says when the answer was neither of those and had no message of its own to show. */
const SIGN_IN_FAILED = 'Could not sign in. Try again.'

/** The answer when the sign-out request failed but the session it was meant to end did not: the cookie and the token are still good, so the page stays signed in and says what did not work. */
const SIGN_OUT_FAILED = 'Could not sign out. Try again.'

/** The screen a person signs in on, and the two messages it might have to carry. */
export type SignedOutState = {
  phase: 'signedOut'
  /** The "session ended" notice, or null when nobody lost a session to show. */
  notice: string | null
  /** Why the last sign-in did not take, or null. */
  error: string | null
}

/** Every state the page can be in, and what each one knows. */
export type SessionState =
  | { phase: 'loading' }
  | { phase: 'open' }
  /**
   * The status question got no readable answer: the fetch threw, or the server answered a failure.
   * Nobody is signed out and nobody failed to sign in - the server simply did not say - so the page
   * offers "Try again" rather than a password.
   */
  | { phase: 'unreachable' }
  | SignedOutState
  | {
      phase: 'signedIn'
      subject: string
      csrf: string
      /** Why the last sign-out attempt did not take, or null. The session is live; only the request to end it failed. */
      error: string | null
    }

/** What a caller can ask of the session, and do to it. */
export interface SessionStore {
  /** The current state: a stable object until the next change, which is what `useSyncExternalStore` needs. */
  get(): SessionState
  /** Follow every later change. Returns the way to stop. */
  subscribe(listener: () => void): () => void
  /** Ask the server what this cookie is: `open`, `signedIn`, `signedOut`, or `unreachable`. Only call it while the page is `loading` or `unreachable`: answers come back in whatever order the server sends them, and a refresh sent from a live session would let a late answer overwrite a newer sign-in. */
  refresh(): Promise<void>
  /** Try the password. True took, false shows why it did not. */
  signIn(user: string, password: string): Promise<boolean>
  /**
   * End this browser's session, and stay signed in unless the server says it ended: a `401` means
   * it was already gone (which is signed out too), but any other failure left the session alive,
   * and only this request to end it failed.
   */
  signOut(): Promise<void>
  /** The value every change must carry, or null when there is no session to prove. */
  csrf(): string | null
  /**
   * Which session is current. It moves every time one starts or ends, so a caller can say "the call
   * that got this `401` was made under session number N" and the store can tell whether that
   * session is still the one the page is running.
   */
  generation(): number
  /**
   * End the session a finding call was sent under. Does nothing when that generation is not the
   * current one (the answer came about a session that is already over) or when the page is already
   * signed out (there is nothing left to end, and a late `401` has no business rewriting the
   * sign-in screen's last message).
   */
  endSession(ended: number): void
  /** The bearer token calls carry right now, or null: always null for the browser's cookie session. */
  token(): string | null
}

/**
 * The store, over an injected `fetch` so the tests answer it themselves and nothing here needs a
 * running server. The states it walks are the rules; only the transport is a parameter.
 */
export function createSessionStore(fetchFn: typeof fetch, endpoint: Endpoint = SAME_ORIGIN): SessionStore {
  if (endpoint.kind === 'bearer') return createBearerStore(fetchFn, endpoint)
  let state: SessionState = { phase: 'loading' }
  let token: string | null = null
  // Which session the page is in the middle of. Every start and every end of a session moves it,
  // which is how an answer about a session already over is recognized as stale.
  let generation = 0
  const listeners = new Set<() => void>()
  // The first status question is on its way while React mounts the tree twice in development; two
  // callers share one answer rather than asking the server twice about one cookie.
  let refreshing: Promise<void> | null = null

  function set(next: SessionState): void {
    state = next
    for (const listener of listeners) listener()
  }

  /** Signed out, with the CSRF value dropped with it: there is no session left to prove. */
  function signedOut(error: string | null, notice: string | null = null): void {
    token = null
    set({ phase: 'signedOut', notice, error })
  }

  /** A session this store just learned is live: a new generation, with nothing carried over from the last one. */
  function signedIn(subject: string, csrf: string): void {
    token = csrf
    generation += 1
    set({ phase: 'signedIn', subject, csrf, error: null })
  }

  /** The server's own words, when the answer carries any; the page's, when it does not. */
  async function message(response: Response): Promise<string> {
    try {
      const body = (await response.json()) as { error?: { message?: unknown } }
      const detail = body?.error?.message
      if (typeof detail === 'string' && detail !== '') return detail
    } catch {
      // An answer that is not JSON has no words of its own to borrow.
    }
    return SIGN_IN_FAILED
  }

  async function status(): Promise<void> {
    let response: Response
    try {
      response = await fetchFn(ENDPOINT, { method: 'GET', credentials: 'same-origin' })
    } catch {
      set({ phase: 'unreachable' })
      return
    }
    // The ordinary "nobody is signed in" answer: signed out, no error, and no notice about a
    // session that ended, because from the page's seat this one never started.
    if (response.status === 401) {
      signedOut(null)
      return
    }
    // Anything else that is not an answer at all - a 5xx, a proxy error page, a 403 from a guard -
    // says the server did not answer this question, not that anybody failed to sign in.
    if (!response.ok) {
      set({ phase: 'unreachable' })
      return
    }
    const body = (await response.json()) as { auth?: boolean; subject?: unknown; csrf?: unknown }
    if (body.auth === false) {
      token = null
      set({ phase: 'open' })
      return
    }
    if (body.auth === true && typeof body.subject === 'string' && typeof body.csrf === 'string') {
      signedIn(body.subject, body.csrf)
      return
    }
    signedOut(SIGN_IN_FAILED)
  }

  async function signIn(user: string, password: string): Promise<boolean> {
    const notice = state.phase === 'signedOut' ? state.notice : null
    let response: Response
    try {
      response = await fetchFn(ENDPOINT, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ user, password }),
        credentials: 'same-origin',
      })
    } catch {
      set({ phase: 'signedOut', notice, error: SIGN_IN_FAILED })
      return false
    }
    if (response.ok) {
      const body = (await response.json()) as { subject?: unknown; csrf?: unknown }
      if (typeof body.subject === 'string' && typeof body.csrf === 'string') {
        signedIn(body.subject, body.csrf)
        return true
      }
      token = null
      set({ phase: 'signedOut', notice, error: SIGN_IN_FAILED })
      return false
    }
    // The two answers a person can act on get the page's own words; everything else is what the
    // server said, which is the most this page knows about a failure it did not expect.
    const error =
      response.status === 401
        ? WRONG_CREDENTIALS
        : response.status === 429
          ? RATE_LIMITED
          : await message(response)
    token = null
    set({ phase: 'signedOut', notice, error })
    return false
  }

  function signOutFailed(): void {
    const was = state
    if (was.phase !== 'signedIn') {
      // There is no session this request was meant to end anymore - a `401` elsewhere probably
      // ended it while the DELETE was in flight - and a sign-in screen that nobody is typing into
      // has nothing to be told about a request to end a session that is already over.
      return
    }
    // The server kept the session and the cookie kept working; only this browser's request to end
    // them failed. Staying signed in is the truth, and it keeps the next change sendable.
    set({ ...was, error: SIGN_OUT_FAILED })
  }

  async function signOut(): Promise<void> {
    if (state.phase !== 'signedIn') return
    const given = token
    let response: Response
    try {
      response = await fetchFn(ENDPOINT, {
        method: 'DELETE',
        headers: given === null ? {} : { 'x-csrf-token': given },
        credentials: 'same-origin',
      })
    } catch {
      signOutFailed()
      return
    }
    // A `401` here is not a failure: it says the session was already gone, which is exactly what
    // signing out was asked to bring about. Only a session that the server agrees is over ends a
    // generation and clears the token - and only while this store is still in that session. If a
    // `401` from elsewhere ended it mid-flight, the ending, and its notice, already happened.
    if (response.ok || response.status === 401) {
      if (state.phase !== 'signedIn') return
      generation += 1
      token = null
      set({ phase: 'signedOut', notice: null, error: null })
      return
    }
    signOutFailed()
  }

  return {
    get: () => state,
    subscribe: (listener) => {
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
      }
    },
    refresh: () => {
      refreshing ??= status().finally(() => {
        refreshing = null
      })
      return refreshing
    },
    signIn,
    signOut,
    csrf: () => token,
    generation: () => generation,
    endSession: (ended) => {
      // Not the session this page is in anymore, or nothing left to end: either way this `401` is
      // history talking, and history does not move the page or rewrite its messages.
      if (ended !== generation || state.phase === 'signedOut') return
      generation += 1
      signedOut(null, SESSION_ENDED)
    },
    token: () => null,
  }
}

/** `user:password` as HTTP Basic wants it: the UTF-8 bytes, base64. `btoa` alone refuses anything past Latin-1. */
function basic(user: string, password: string): string {
  let binary = ''
  for (const byte of new TextEncoder().encode(`${user}:${password}`)) binary += String.fromCharCode(byte)
  return btoa(binary)
}

/** The desktop's store: a bearer token in memory, from the launch or from a sign-in. */
function createBearerStore(
  fetchFn: typeof fetch,
  endpoint: Extract<Endpoint, { kind: 'bearer' }>,
): SessionStore {
  let token = endpoint.token
  let generation = token === null ? 0 : 1
  let state: SessionState =
    token === null
      ? { phase: 'signedOut', notice: null, error: null }
      : { phase: 'signedIn', subject: DESKTOP_SUBJECT, csrf: '', error: null }
  const listeners = new Set<() => void>()

  function set(next: SessionState): void {
    state = next
    for (const listener of listeners) listener()
  }

  function signedOut(notice: string | null, error: string | null): void {
    token = null
    set({ phase: 'signedOut', notice, error })
  }

  async function signIn(user: string, password: string): Promise<boolean> {
    const notice = state.phase === 'signedOut' ? state.notice : null
    let response: Response
    try {
      response = await fetchFn(resolve(endpoint, BEARER_SIGN_IN), {
        method: 'POST',
        headers: { authorization: `Basic ${basic(user, password)}` },
        credentials: 'omit',
      })
    } catch {
      signedOut(notice, SIGN_IN_FAILED)
      return false
    }
    if (response.ok) {
      let body: { token?: unknown; subject?: unknown } = {}
      try {
        body = (await response.json()) as typeof body
      } catch {
        // Not JSON: no token in it, which the check below says.
      }
      if (typeof body.token === 'string' && typeof body.subject === 'string') {
        token = body.token
        generation += 1
        set({ phase: 'signedIn', subject: body.subject, csrf: '', error: null })
        return true
      }
      signedOut(notice, SIGN_IN_FAILED)
      return false
    }
    let error = SIGN_IN_FAILED
    if (response.status === 401) error = WRONG_CREDENTIALS
    else if (response.status === 429) error = RATE_LIMITED
    else {
      try {
        const body = (await response.json()) as { error?: { message?: unknown } }
        const detail = body?.error?.message
        if (typeof detail === 'string' && detail !== '') error = detail
      } catch {
        // No words of its own to borrow.
      }
    }
    signedOut(notice, error)
    return false
  }

  return {
    get: () => state,
    subscribe: (listener) => {
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
      }
    },
    // Nothing to ask: a launch token is a session and no token is the sign-in screen, both known
    // before any answer could arrive.
    refresh: async () => {},
    signIn,
    // The same rule as the cookie store: the server ends the session, because the token has already
    // travelled in query strings and forgetting it here would leave it good until it expires. A 2xx
    // or a 401 means it is over; anything else left it alive, so the page stays signed in and says so.
    signOut: async () => {
      if (state.phase !== 'signedIn' || token === null) return
      const given = token
      const mine = generation
      let ended: boolean
      try {
        const response = await fetchFn(resolve(endpoint, BEARER_SIGN_IN), {
          method: 'DELETE',
          headers: { authorization: `Bearer ${given}` },
          credentials: 'omit',
        })
        ended = response.ok || response.status === 401
      } catch {
        ended = false
      }
      // A `401` elsewhere may have ended this session while the DELETE was in flight: that ending,
      // and its notice, already happened.
      if (generation !== mine || state.phase !== 'signedIn') return
      if (ended) {
        generation += 1
        signedOut(null, null)
        return
      }
      set({ ...state, error: SIGN_OUT_FAILED })
    },
    csrf: () => null,
    generation: () => generation,
    endSession: (ended) => {
      if (ended !== generation || state.phase === 'signedOut') return
      generation += 1
      signedOut(SESSION_ENDED, null)
    },
    token: () => token,
  }
}

/** The store this page runs on: one browser, one cookie, one set of answers about it. */
export const session = createSessionStore(fetch)

/** The state the page renders, plus the things a person can do about it. */
export function useSession(store: SessionStore = session): {
  state: SessionState
  signIn: (user: string, password: string) => Promise<boolean>
  signOut: () => Promise<void>
  refresh: () => Promise<void>
} {
  const state = useSyncExternalStore(store.subscribe, store.get)
  // The server answers before anything about this cookie is known, so nothing is drawn as decided
  // until that answer has arrived.
  useEffect(() => {
    void store.refresh()
  }, [store])
  return {
    state,
    signIn: store.signIn,
    signOut: store.signOut,
    refresh: () => store.refresh(),
  }
}
