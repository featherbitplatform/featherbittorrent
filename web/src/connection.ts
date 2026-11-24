/**
 * The connection the page runs on: where the API is, the client that calls it, and the session that
 * says who those calls speak as.
 *
 * The browser build has one, the `sameOrigin` connection built from the module-level `api` and
 * `session`, and that is the context's default, so a page rendered with no provider is exactly the
 * page it always was. The desktop app wraps `App` in a provider with a `bearer` connection instead:
 * the same screens then call an engine at its own address with a token. Screens read
 * {@link useConnection} wherever they used to import the singletons, which is what keeps one client
 * code path for both.
 *
 * @module connection
 */
import { createContext, useContext } from 'react'
import { api, createClient } from './api/client'
import type { Api } from './api/client'
import { resolve, SAME_ORIGIN, withToken } from './endpoint'
import type { Endpoint } from './endpoint'
import { createSessionStore, session } from './session'
import type { SessionStore } from './session'

export type { Endpoint } from './endpoint'

/** The event stream's route, before its query. */
const EVENTS = '/api/v1/events'

export interface Connection {
  endpoint: Endpoint
  api: Api
  session: SessionStore
  /** `base + path` for bearer, the path unchanged for sameOrigin. */
  url(path: string): string
  /** The event stream's URL with `query`; for bearer it carries `token=`, since `EventSource` cannot set a header. */
  eventsUrl(query: string): string
}

function assemble(endpoint: Endpoint, client: Api, store: SessionStore): Connection {
  return {
    endpoint,
    api: client,
    session: store,
    url: (path) => resolve(endpoint, path),
    eventsUrl: (query) => {
      const url = resolve(endpoint, query === '' ? EVENTS : `${EVENTS}?${query}`)
      return endpoint.kind === 'bearer' ? withToken(url, store.token()) : url
    },
  }
}

/** A connection of its own: a session store and a client over one `fetch`, for one endpoint. */
export function createConnection(endpoint: Endpoint, fetchFn: typeof fetch = fetch): Connection {
  const store = createSessionStore(fetchFn, endpoint)
  const client = createClient(
    fetchFn,
    store.csrf,
    store.generation,
    store.endSession,
    endpoint,
    store.token,
  )
  return assemble(endpoint, client, store)
}

/** The browser's connection: the page's own origin, over the singletons the browser build has always used. */
export const browserConnection: Connection = assemble(SAME_ORIGIN, api, session)

export const ConnectionContext = createContext<Connection>(browserConnection)

/** The connection the page is rendered under: the browser's, unless a provider says otherwise. */
export function useConnection(): Connection {
  return useContext(ConnectionContext)
}
