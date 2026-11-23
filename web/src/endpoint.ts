/**
 * Where the API lives, and how a call proves who it is: the one fact the client, the session store
 * and the connection all derive their addresses from.
 *
 * The browser build talks to the server that served the page (`sameOrigin`): relative paths, the
 * session cookie, a CSRF header on changes. The desktop app talks to an engine at an address of its
 * own (`bearer`): absolute URLs, an `Authorization: Bearer` header, and no cookie at all. `EventSource`
 * and a `<video src>` cannot set a header, so the routes behind them take the same token as a `token`
 * query parameter, and {@link withToken} is how it gets there. {@link isOwnUrl} is the check that
 * keeps that token on this endpoint's own server.
 *
 * This lives apart from `connection.ts` so the client and the session store can read it without
 * importing the module that builds them.
 *
 * @module endpoint
 */

export type Endpoint =
  /** The browser: cookie + CSRF, same origin as the page. */
  | { kind: 'sameOrigin' }
  /** The desktop: an absolute base like `http://127.0.0.1:53211`, and the token, or null when a sign-in is needed. */
  | { kind: 'bearer'; base: string; token: string | null }

/** The browser's endpoint, which is what every caller that names none gets. */
export const SAME_ORIGIN: Endpoint = { kind: 'sameOrigin' }

/** A path on this endpoint: unchanged for the browser, the base in front of it for the desktop. */
export function resolve(endpoint: Endpoint, path: string): string {
  if (endpoint.kind === 'sameOrigin') return path
  // An address that already names its scheme is somebody else's, not a path to prefix.
  if (isAbsolute(path)) return path
  return `${normaliseBase(endpoint.base)}${path}`
}

/**
 * An instance's base address in the one spelling every URL is built on: surrounding space and trailing
 * slashes dropped, so `http://host:1/` and `http://host:1` are one instance and `base + '/api/...'`
 * never makes `//api`. This computer's address from the shell and a saved instance's address both go
 * through here before a connection is made on them.
 */
export function normaliseBase(base: string): string {
  return base.trim().replace(/\/+$/, '')
}

/**
 * The URL with `token=` added, for the routes a header cannot reach. A URL that already carries a
 * token of its own (a stream token the server handed out) keeps it, and no token means no change.
 */
export function withToken(url: string, token: string | null): string {
  if (token === null || /[?&]token=/.test(url)) return url
  return `${url}${url.includes('?') ? '&' : '?'}token=${encodeURIComponent(token)}`
}

/** Whether a URL names its own origin: such a request carries no cookie of this page's. */
export function isAbsolute(url: string): boolean {
  return /^[a-z][a-z0-9+.-]*:/i.test(url)
}

/**
 * Whether a URL belongs to this endpoint and may carry its token: a relative path always does; an
 * absolute URL only when it sits on the endpoint's own base, at a path boundary - `http://host:1x`
 * is not this connection's `http://host:1`. A URL that names somebody else's server must not take
 * this session's token with it.
 */
export function isOwnUrl(endpoint: Endpoint, url: string): boolean {
  if (endpoint.kind !== 'bearer') return true
  if (!isAbsolute(url)) return true
  const base = normaliseBase(endpoint.base)
  if (base === '') return false
  if (url.slice(0, base.length).toLowerCase() !== base.toLowerCase()) return false
  const rest = url.slice(base.length)
  return rest === '' || rest.startsWith('/') || rest.startsWith('?') || rest.startsWith('#')
}
