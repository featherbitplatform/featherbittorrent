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
 * Three states come from that one endpoint, and a fourth is this module's own:
 * `loading` while the first answer is on its way, `open` when logins are off and nobody has to sign
 * in, `signedIn` with the subject and the CSRF value, and `signedOut` when the cookie answers
 * nothing. The `401` that means signed-out is not an error, and the first one of those on page load
 * is not a lost session either: it is the ordinary starting point. The "session ended" notice exists
 * separately, because only the API client can discover a session that was working and stopped; that
 * is why `endSession` is its own door rather than a flag the `GET` could set.
 *
 * The states and the actions are a plain store (get / subscribe / refresh / signIn / signOut), so
 * the rules are testable without a DOM, and `useSession` is the React reading of that store rather
 * than a second implementation of it.
 *
 * @module session
 */
import { useEffect, useSyncExternalStore } from 'react'

/** The one endpoint every question about this session asks. It is this server's own, not part of the generated API document, which is why its shapes are written by hand here. */
const ENDPOINT = '/api/v1/auth/web-session'

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
  | SignedOutState
  | { phase: 'signedIn'; subject: string; csrf: string }

/** What a caller can ask of the session, and do to it. */
export interface SessionStore {
  /** The current state: a stable object until the next change, which is what `useSyncExternalStore` needs. */
  get(): SessionState
  /** Follow every later change. Returns the way to stop. */
  subscribe(listener: () => void): () => void
  /** Ask the server what this cookie is: `open`, `signedIn`, or `signedOut`. */
  refresh(): Promise<void>
  /** Try the password. True took, false shows why it did not. */
  signIn(user: string, password: string): Promise<boolean>
  /** End this browser's session. A `401` here means it was already gone, which is signed out too. */
  signOut(): Promise<void>
  /** The value every change must carry, or null when there is no session to prove. */
  csrf(): string | null
  /** Move to signed-out with the "session ended" notice, from whoever found out. */
  endSession(): void
}

/**
 * The store, over an injected `fetch` so the tests answer it themselves and nothing here needs a
 * running server. The states it walks are the rules; only the transport is a parameter.
 */
export function createSessionStore(fetchFn: typeof fetch): SessionStore {
  let state: SessionState = { phase: 'loading' }
  let token: string | null = null
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
      signedOut(SIGN_IN_FAILED)
      return
    }
    // The ordinary "nobody is signed in" answer: signed out, no error, and no notice about a
    // session that ended, because from the page's seat this one never started.
    if (response.status === 401) {
      signedOut(null)
      return
    }
    if (!response.ok) {
      signedOut(await message(response))
      return
    }
    const body = (await response.json()) as { auth?: boolean; subject?: unknown; csrf?: unknown }
    if (body.auth === false) {
      token = null
      set({ phase: 'open' })
      return
    }
    if (body.auth === true && typeof body.subject === 'string' && typeof body.csrf === 'string') {
      token = body.csrf
      set({ phase: 'signedIn', subject: body.subject, csrf: body.csrf })
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
        token = body.csrf
        set({ phase: 'signedIn', subject: body.subject, csrf: body.csrf })
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

  async function signOut(): Promise<void> {
    const given = token
    token = null
    let response: Response
    try {
      response = await fetchFn(ENDPOINT, {
        method: 'DELETE',
        headers: given === null ? {} : { 'x-csrf-token': given },
        credentials: 'same-origin',
      })
    } catch {
      // Nothing arrived and nothing can be asked of it again: the browser is on its own with the
      // cookie, and the server expires that session on its own schedule.
      set({ phase: 'signedOut', notice: null, error: null })
      return
    }
    // A `401` here is not a failure: it says the session was already gone, which is exactly what
    // signing out was asked to bring about.
    if (response.ok || response.status === 401) {
      set({ phase: 'signedOut', notice: null, error: null })
      return
    }
    set({ phase: 'signedOut', notice: null, error: await message(response) })
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
    endSession: () => signedOut(null, SESSION_ENDED),
  }
}

/** The store this page runs on: one browser, one cookie, one set of answers about it. */
export const session = createSessionStore(fetch)

/** The state the page renders, plus the two things a person can do about it. */
export function useSession(): {
  state: SessionState
  signIn: (user: string, password: string) => Promise<boolean>
  signOut: () => Promise<void>
} {
  const state = useSyncExternalStore(session.subscribe, session.get)
  // The server answers before anything about this cookie is known, so nothing is drawn as decided
  // until that answer has arrived.
  useEffect(() => {
    void session.refresh()
  }, [])
  return { state, signIn: session.signIn, signOut: session.signOut }
}
