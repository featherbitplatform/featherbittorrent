/**
 * The typed API client: every call the page makes to the server beside it.
 *
 * The request and response shapes come from `schema.ts`, the file `npm run gen:api` generates from
 * the OpenAPI document the server serves, so the client and the API drift apart only if the
 * generated file is stale, and the CI drift step catches that. Two calls are not in that document
 * because they are this server's own routes rather than fbt-api's (`crates/fbtt-server/src/routes/
 * fs.rs`), and they are typed by hand for that reason: `roots` answers a JSON array of strings and
 * `list` answers `{ path, dirs }`.
 *
 * Three rules hold for every call, and each one is about the cookie rather than the JSON:
 * `credentials: 'same-origin'` rides the session cookie along; every method that changes something
 * carries `X-CSRF-Token`, because the browser volunteers the cookie on its own and the header is
 * what proves the change came from this page; and every path segment and query value goes through
 * `encodeURIComponent`, because a torrent id and a save path are inputs with their own ideas about
 * slashes and spaces.
 *
 * A `401` is the interesting failure. It means the cookie stopped meaning anything, so the client
 * hands it to `onUnauthorized` once and throws, and it does not retry: a call that was refused will
 * still be refused by the same cookie, and calling again would be how a lost session turns into a
 * request loop. "Once" is per lost session, not per client: the report flag is reset by any answer
 * that came in signed, so a session that ends again later is reported again.
 *
 * @module api/client
 */
import type { components, operations } from './schema'
import { session } from '../session'

/** One torrent, as the list and the detail screens will draw it. */
export type TorrentInfo = components['schemas']['TorrentInfo']
/** One file inside a torrent. */
export type FileInfo = components['schemas']['FileInfo']
/** One peer of a torrent. */
export type PeerInfo = components['schemas']['PeerInfo']
/** One tracker of a torrent, and what it last said about it. */
export type TrackerInfo = components['schemas']['TrackerInfo']
/** Which pieces of a torrent are on disk. */
export type PiecesInfo = components['schemas']['PiecesInfo']
/** The session at a glance: rates, limits, the number of torrents. */
export type SessionInfo = components['schemas']['SessionInfo']
/** The directories inside one path, as `GET /api/v1/fs/list` answers them. */
export interface DirectoryListing {
  path: string
  dirs: string[]
}

/**
 * A call the server refused. `status` and `code` are what a caller decides from (Task 7 closes the
 * detail pane on a `404`, for instance); `message` is the server's own words when it had any.
 */
export class ApiError extends Error {
  readonly status: number
  /** The machine-readable code from the error body, or null when the answer was not one of this API's own. */
  readonly code: string | null

  constructor(status: number, code: string | null, message: string) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.code = code
  }
}

/** The JSON body of an operation's 200 answer. */
type Json200<Op> = Op extends { responses: { 200: { content: { 'application/json': infer T } } } }
  ? T
  : never

/** The JSON body of an operation's 201 answer. */
type Json201<Op> = Op extends { responses: { 201: { content: { 'application/json': infer T } } } }
  ? T
  : never

/** Every call the page makes, named the way the screens will name it. The return types are read out of the generated document operation by operation, so a change in the API surface shows up here as a type error rather than as a screen that quietly trusts a stale alias. */
export interface Api {
  torrents(): Promise<Json200<operations['list_torrents']>>
  torrent(id: string): Promise<Json200<operations['get_torrent']>>
  files(id: string): Promise<Json200<operations['get_files']>>
  peers(id: string): Promise<Json200<operations['get_peers']>>
  trackers(id: string): Promise<Json200<operations['get_trackers']>>
  pieces(id: string): Promise<Json200<operations['get_pieces']>>
  session(): Promise<Json200<operations['get_session']>>
  pause(id: string): Promise<void>
  resume(id: string): Promise<void>
  remove(id: string, deleteFiles: boolean): Promise<void>
  addMagnet(
    magnet: string,
    savePath: string,
    paused: boolean,
  ): Promise<Json201<operations['add_torrent']>>
  addFile(
    bytes: ArrayBuffer,
    savePath: string,
    paused: boolean,
  ): Promise<Json201<operations['add_torrent']>>
  roots(): Promise<string[]>
  list(path: string): Promise<DirectoryListing>
}

const TORRENTS = '/api/v1/torrents'

/** The document's own JSON shape for a magnet add, named once so the method below says what it sends. */
type AddTorrentRequest = components['schemas']['AddTorrentRequest']

/**
 * The client over an injected `fetch`, a way to ask "what token should a change carry right now",
 * and the one action to take when a call finds the session gone. The tests hand it a fake and read
 * the requests back; the page hands it the browser's own.
 */
export function createClient(
  fetchFn: typeof fetch,
  getCsrf: () => string | null,
  onUnauthorized: () => void,
): Api {
  // Whether the current loss of the session has been reported yet. Any answer that arrived signed
  // clears it, so the next one gets reported too.
  let reported = false

  async function request(
    url: string,
    method: 'GET' | 'POST' | 'DELETE',
    body?: BodyInit,
    contentType?: string,
  ): Promise<Response> {
    const headers = new Headers()
    if (contentType !== undefined) headers.set('content-type', contentType)
    if (method !== 'GET') {
      const token = getCsrf()
      if (token !== null) headers.set('x-csrf-token', token)
    }
    const response = await fetchFn(url, { method, headers, body, credentials: 'same-origin' })
    if (!response.ok) {
      if (response.status === 401 && !reported) {
        reported = true
        onUnauthorized()
      }
      throw await errorFrom(response)
    }
    reported = false
    return response
  }

  async function errorFrom(response: Response): Promise<ApiError> {
    let code: string | null = null
    let message = `the server answered ${response.status}`
    try {
      const body = (await response.json()) as Partial<components['schemas']['ErrorBody']>
      const detail = body?.error
      if (detail && typeof detail.message === 'string' && detail.message !== '') {
        message = detail.message
        if (typeof detail.code === 'string') code = detail.code
      }
    } catch {
      // An answer that is not JSON keeps the line above as its description.
    }
    return new ApiError(response.status, code, message)
  }

  async function json<T>(
    url: string,
    method: 'GET' | 'POST' | 'DELETE',
    body?: BodyInit,
    contentType?: string,
  ): Promise<T> {
    return (await (await request(url, method, body, contentType)).json()) as T
  }

  async function nothing(url: string, method: 'POST' | 'DELETE'): Promise<void> {
    await request(url, method)
  }

  /** One torrent, addressed: the id is remote text and a raw slash would be somebody else's path. */
  function torrentPath(id: string, suffix = ''): string {
    return `${TORRENTS}/${encodeURIComponent(id)}${suffix}`
  }

  /** The two add options that travel in the query string, encoded like everything else. */
  function addQuery(savePath: string, paused: boolean): string {
    return `?save_path=${encodeURIComponent(savePath)}&paused=${encodeURIComponent(String(paused))}`
  }

  return {
    torrents: () => json(TORRENTS, 'GET'),
    torrent: (id) => json(torrentPath(id), 'GET'),
    files: (id) => json(torrentPath(id, '/files'), 'GET'),
    peers: (id) => json(torrentPath(id, '/peers'), 'GET'),
    trackers: (id) => json(torrentPath(id, '/trackers'), 'GET'),
    pieces: (id) => json(torrentPath(id, '/pieces'), 'GET'),
    session: () => json('/api/v1/session', 'GET'),
    pause: (id) => nothing(torrentPath(id, '/pause'), 'POST'),
    resume: (id) => nothing(torrentPath(id, '/resume'), 'POST'),
    remove: (id, deleteFiles) =>
      nothing(
        `${torrentPath(id)}?delete_files=${encodeURIComponent(String(deleteFiles))}`,
        'DELETE',
      ),
    // A magnet's options travel in the body, and no query string is sent at all: the pinned server
    // refuses one on a JSON add outright ("a JSON add takes its options in the body, not the query
    // string", fbt-api `routes/control.rs`, at the tag Cargo.lock pins). The OpenAPI document lists
    // `save_path` and `paused` as query parameters because one operation serves both body types,
    // and for this one the body carries them.
    addMagnet: (magnet, savePath, paused) =>
      json(
        TORRENTS,
        'POST',
        JSON.stringify({ magnet, save_path: savePath, paused } satisfies AddTorrentRequest),
        'application/json',
      ),
    addFile: (bytes, savePath, paused) =>
      json(`${TORRENTS}${addQuery(savePath, paused)}`, 'POST', bytes, 'application/x-bittorrent'),
    roots: () => json('/api/v1/fs/roots', 'GET'),
    list: (path) => json(`/api/v1/fs/list?path=${encodeURIComponent(path)}`, 'GET'),
  }
}

/**
 * The client the page runs on: the browser's own `fetch`, and the session store as the source of
 * both the token and the one thing to do when a call finds the session gone. Tasks 6 and on import
 * this instance rather than building their own, so one cookie and one CSRF value are the whole story.
 */
export const api = createClient(fetch, () => session.csrf(), () => session.endSession())
