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
 * A `401` is the interesting failure. It means the cookie stopped meaning anything *for the call
 * that got it*, so the client hands it to `onUnauthorized` and throws, and it does not retry: a call
 * that was refused will still be refused by the same cookie, and calling again would be how a lost
 * session turns into a request loop. The client does not decide what to do about the `401` on its
 * own and does not keep any session flag of its own: it reports the *generation* of the session the
 * request was sent under, captured at send time beside the CSRF value, and the session store decides
 * whether that session is still the current one. A slow request under an old session that answers
 * `401` after the person signed in again is history, and the store drops it; a `401` on the first
 * call under a brand new session is not, and no once-ever flag would know the difference.
 *
 * All of that is the browser's `sameOrigin` endpoint. The desktop app hands in a `bearer` endpoint
 * instead, and the three rules change with it: every path is prefixed with the engine's base, every
 * call carries `Authorization: Bearer <token>` with `credentials: 'omit'` (there is no cookie to ride
 * along, so there is nothing for a CSRF header to prove), and the stream URLs come back absolute with
 * the token in the query, because a `<video src>` cannot send a header. The `401` path is the same.
 *
 * @module api/client
 */
import type { components, operations } from './schema'
import { displaySafe } from '../format'
import { UNREACHABLE_COPY } from '../live'
import { session } from '../session'
import { SAME_ORIGIN, isOwnUrl, resolve, withToken } from '../endpoint'
import type { Endpoint } from '../endpoint'

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

/** One paired device, as `GET /api/v1/auth/devices` lists it (this product's own route, not in the OpenAPI document). */
export interface DeviceRow {
  name: string
  fingerprint: string
}

/**
 * The speed and queue settings, as `GET /api/v1/settings` answers them under `speed` (this product's own
 * route, not in the OpenAPI document). A rate is in KiB/s, a count is a number, and `null` is unlimited.
 */
export interface SpeedSettings {
  download_kib: number | null
  upload_kib: number | null
  active_downloads: number | null
  active_seeds: number | null
  active_total: number | null
}

/** What a server says about running a program on completion: the program, and whether this page may set it. */
export interface OnCompleteState {
  program: string | null
  editable: boolean
}

/** The downloads settings, as `GET /api/v1/settings` answers them under `downloads`. */
export interface DownloadsSettings {
  default_dir: string
  unfinished_dir: string | null
  start_paused: boolean
  watch_dir: string | null
  on_complete: OnCompleteState
  watch_problem: string | null
  on_complete_problem: string | null
}

/** What `PUT /api/v1/settings/downloads` takes from this page. Never the program on completion: a server reads it only from its config file. */
export interface DownloadsInput {
  default_dir: string
  unfinished_dir: string | null
  start_paused: boolean
  watch_dir: string | null
}

/**
 * The program on completion, sent only on purpose: the desktop app's This computer view (the server says
 * `editable`) names it through this, and `program: null` means run nothing. A save that does not pass it
 * leaves the key out, which the server reads as "keep what the config has".
 */
export interface OnCompleteChange {
  program: string | null
}

/** The proxy section of the settings: only whether one is set is read here (`type` is `none` when there is none). */
export interface ProxySettings {
  type: string
}

/**
 * How the proxy is doing, as `GET /api/v1/settings/proxy/state` answers. The reason is display-safe
 * and never carries the proxy's address or login.
 */
export type ProxyHealth =
  | { state: 'off' | 'ok' | 'udp_not_relayed' | 'login_refused' }
  | { state: 'unreachable'; reason?: string }

/** The whole settings object, as every settings route answers it. A server older than the proxy setting has no `proxy`. */
export interface ProductSettings {
  speed: SpeedSettings
  downloads: DownloadsSettings
  proxy?: ProxySettings
}

/** The kind of track a file carries, as the media probe says it. */
export type MediaTrackKind = 'video' | 'audio' | 'subtitle'

/**
 * One track of a media file, field for field with the probe answer
 * (`GET /api/v1/media/{id}/{file}`, `crates/fbtt-server/src/routes/media.rs` at the milestone 2
 * plan): the wire names are the server's own, so the page keeps them as they arrive.
 */
export interface MediaTrack {
  id: number
  kind: MediaTrackKind
  codec: string
  /** The codec strings the remux answers in `video/mp4; codecs="..."` or `audio/mp4; codecs="..."`. */
  mime_codecs: string[]
  language: string | null
  name: string | null
  default: boolean
  forced: boolean
  /** True for a text subtitle; image subtitles cannot be shown by the browser's WebVTT engine. */
  text: boolean
}

/** A subtitle file of the same torrent, listed beside the tracks that live inside the video. */
export interface ExternalSubtitle {
  file: number
  name: string
  format: string
}

/** The probe answer for one file: its container, its duration, its tracks, and the subtitle files beside it. */
export interface MediaProbe {
  container: 'matroska' | 'webm' | 'mp4'
  duration_ms: number
  /** The shift every remuxed track carries, in source milliseconds. */
  time_offset_ms: number
  tracks: MediaTrack[]
  external_subtitles: ExternalSubtitle[]
}

/** One subtitle cue, the wire shape of the cue routes' `{"cues":[...]}` body. */
export interface Cue {
  start_ms: number
  end_ms: number
  text: string
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

/**
 * The sentence to show a person about a call that did not take.
 *
 * An `ApiError` already carries the server's own words, and those are the ones worth reading: "that
 * torrent is already in the list" tells a person what to do next in a way "the server answered 409"
 * does not. Anything else that came out of a call is a failure with no answer behind it - no
 * response, no body, nothing the server said - and the one thing that is true of all of them is the
 * line the unreachable screen already uses, so it is shared rather than paraphrased here.
 *
 * Every branch goes through `displaySafe`: an error message is remote text like any other, and a
 * server that echoes something back can put control characters in it.
 */
export function errorMessage(cause: unknown): string {
  if (cause instanceof ApiError) return displaySafe(cause.message)
  return UNREACHABLE_COPY
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
  probe(id: string, file: number): Promise<MediaProbe>
  cues(
    id: string,
    file: number,
    track: number,
    fromMs: number,
    toMs: number,
  ): Promise<{ cues: Cue[] }>
  subtitleFile(id: string, file: number, sub: number): Promise<{ cues: Cue[] }>
  streamToken(id: string, file: number): Promise<string>
  mediaStreamUrl(id: string, file: number, track: number, startMs: number): string
  setFilePriorities(id: string, priorities: Array<components['schemas']['PriorityDto']>): Promise<void>
  setQueuePosition(id: string, position: number): Promise<void>
  recheck(id: string): Promise<void>
  reannounce(id: string): Promise<void>
  setTorrentLimits(id: string, downloadBps: number | null, uploadBps: number | null): Promise<void>
  moveStorage(id: string, path: string): Promise<void>
  setSessionLimits(downloadBps: number | null, uploadBps: number | null): Promise<void>
  queueSettings(): Promise<components['schemas']['QueueSettingsDto']>
  setQueueSettings(settings: components['schemas']['QueueSettingsDto']): Promise<void>
  settings(): Promise<ProductSettings>
  proxyState(): Promise<ProxyHealth>
  saveSpeed(speed: SpeedSettings): Promise<ProductSettings>
  saveDownloads(downloads: DownloadsInput, options?: { onComplete?: OnCompleteChange }): Promise<ProductSettings>
  devices(): Promise<DeviceRow[]>
  addDevice(name: string, publicKey: string): Promise<DeviceRow>
  removeDevice(name: string): Promise<void>
  mkdir(parent: string, name: string): Promise<string>
}

const TORRENTS = '/api/v1/torrents'
const MEDIA = '/api/v1/media'
const STREAM_TOKENS = '/api/v1/stream-tokens'
const SETTINGS = '/api/v1/settings'
const DEVICES = '/api/v1/auth/devices'

/**
 * A time the query string carries, in whole source milliseconds: the wire names them as non-negative
 * integers, so a fractional, negative, or non-finite value is clamped to the whole number it means
 * rather than sent as a number the route would refuse.
 */
function wholeMs(value: number): number {
  return Number.isFinite(value) ? Math.max(0, Math.floor(value)) : 0
}

/** The document's own JSON shape for a magnet add, named once so the method below says what it sends. */
type AddTorrentRequest = components['schemas']['AddTorrentRequest']

/**
 * The client over an injected `fetch`, a way to ask "what token should a change carry right now", a
 * way to ask "which session is the page in right now", and the one action to take when a call finds
 * the session gone, told with the generation that call was sent under. The tests hand it a fake and
 * read the requests back; the page hands it the browser's own and the session store's own answers.
 * `endpoint` says where the API is (the page's own origin unless told otherwise), and `getToken` is
 * the bearer token right now, which for a desktop that signs in changes after the client is built.
 */
export function createClient(
  fetchFn: typeof fetch,
  getCsrf: () => string | null,
  getGeneration: () => number,
  onUnauthorized: (generation: number) => void,
  endpoint: Endpoint = SAME_ORIGIN,
  getToken: () => string | null = () => (endpoint.kind === 'bearer' ? endpoint.token : null),
): Api {
  const bearer = endpoint.kind === 'bearer'

  /** A stream address the browser opens on its own (a `<video src>`, a download): absolute and carrying the token for the desktop. The token goes only to this connection's own server: an answer that names somebody else's keeps it to itself. */
  function streamAddress(path: string): string {
    if (!bearer) return path
    const url = resolve(endpoint, path)
    return isOwnUrl(endpoint, url) ? withToken(url, getToken()) : url
  }

  async function request(
    url: string,
    method: 'GET' | 'POST' | 'PUT' | 'DELETE',
    body?: BodyInit,
    contentType?: string,
  ): Promise<Response> {
    const headers = new Headers()
    if (contentType !== undefined) headers.set('content-type', contentType)
    if (bearer) {
      const token = getToken()
      if (token !== null) headers.set('authorization', `Bearer ${token}`)
    } else if (method !== 'GET') {
      const token = getCsrf()
      if (token !== null) headers.set('x-csrf-token', token)
    }
    // Captured *before* the request goes out, not when the answer lands: an answer about a `401`
    // must name the session the call left home under, because by the time a slow one arrives the
    // person may already be signed in again.
    const generation = getGeneration()
    const response = await fetchFn(resolve(endpoint, url), {
      method,
      headers,
      body,
      credentials: bearer ? 'omit' : 'same-origin',
    })
    if (!response.ok) {
      if (response.status === 401) onUnauthorized(generation)
      throw await errorFrom(response)
    }
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
    method: 'GET' | 'POST' | 'PUT' | 'DELETE',
    body?: BodyInit,
    contentType?: string,
  ): Promise<T> {
    return (await (await request(url, method, body, contentType)).json()) as T
  }

  async function nothing(url: string, method: 'POST' | 'PUT' | 'DELETE'): Promise<void> {
    await request(url, method)
  }

  /** A change that carries a JSON body and answers with nothing worth reading. */
  async function sendJson(url: string, method: 'POST' | 'PUT', body: unknown): Promise<void> {
    await request(url, method, JSON.stringify(body), 'application/json')
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
    probe: (id, file) => json(`${MEDIA}/${encodeURIComponent(id)}/${encodeURIComponent(file)}`, 'GET'),
    cues: (id, file, track, fromMs, toMs) =>
      json(
        `${MEDIA}/${encodeURIComponent(id)}/${encodeURIComponent(file)}/tracks/${encodeURIComponent(track)}/cues` +
          `?from_ms=${encodeURIComponent(wholeMs(fromMs))}&to_ms=${encodeURIComponent(wholeMs(toMs))}`,
        'GET',
      ),
    subtitleFile: (id, file, sub) =>
      json(`${MEDIA}/${encodeURIComponent(id)}/${encodeURIComponent(file)}/subtitles/${encodeURIComponent(sub)}`, 'GET'),
    // A change: the token travels with the CSRF header. What the caller keeps is the path the
    // answer names; it already carries `?token=` when a token came with it, and nothing else to add.
    streamToken: async (id, file) => {
      const body = JSON.stringify({ torrent: id, file } satisfies { torrent: string; file: number })
      const answer = (await json(STREAM_TOKENS, 'POST', body, 'application/json')) as { path?: string }
      if (typeof answer.path !== 'string') {
        throw new Error('the stream token answer had no path')
      }
      return streamAddress(answer.path)
    },
    mediaStreamUrl: (id, file, track, startMs) =>
      streamAddress(
        `${MEDIA}/${encodeURIComponent(id)}/${encodeURIComponent(file)}/tracks/${encodeURIComponent(track)}/stream?start_ms=${encodeURIComponent(wholeMs(startMs))}`,
      ),
    setFilePriorities: (id, priorities) =>
      sendJson(torrentPath(id, '/file-priorities'), 'PUT', { priorities } satisfies components['schemas']['FilePrioritiesRequest']),
    setQueuePosition: (id, position) =>
      sendJson(torrentPath(id, '/queue-position'), 'PUT', { position } satisfies components['schemas']['QueuePositionRequest']),
    recheck: (id) => nothing(torrentPath(id, '/recheck'), 'POST'),
    reannounce: (id) => nothing(torrentPath(id, '/reannounce'), 'POST'),
    setTorrentLimits: (id, downloadBps, uploadBps) =>
      sendJson(torrentPath(id, '/limits'), 'PUT', { download: downloadBps, upload: uploadBps } satisfies components['schemas']['LimitsRequest']),
    moveStorage: (id, path) =>
      sendJson(torrentPath(id, '/move'), 'POST', { path, mode: 'fail_if_exists' } satisfies components['schemas']['MoveRequest']),
    setSessionLimits: (downloadBps, uploadBps) =>
      sendJson('/api/v1/session/limits', 'PUT', { download: downloadBps, upload: uploadBps } satisfies components['schemas']['LimitsRequest']),
    queueSettings: () => json('/api/v1/session/queue', 'GET'),
    setQueueSettings: (settings) => sendJson('/api/v1/session/queue', 'PUT', settings),
    settings: () => json(SETTINGS, 'GET'),
    proxyState: () => json(`${SETTINGS}/proxy/state`, 'GET'),
    saveSpeed: (speed) =>
      json(
        `${SETTINGS}/speed`,
        'PUT',
        JSON.stringify({
          download_kib: speed.download_kib,
          upload_kib: speed.upload_kib,
          active_downloads: speed.active_downloads,
          active_seeds: speed.active_seeds,
          active_total: speed.active_total,
        }),
        'application/json',
      ),
    // Picked field by field: the program on completion is never taken from the caller's object, even
    // when it carries one. It goes only through the explicit `onComplete` option.
    saveDownloads: (downloads, options) =>
      json(
        `${SETTINGS}/downloads`,
        'PUT',
        JSON.stringify({
          default_dir: downloads.default_dir,
          unfinished_dir: downloads.unfinished_dir,
          start_paused: downloads.start_paused,
          watch_dir: downloads.watch_dir,
          ...(options?.onComplete === undefined ? {} : { on_complete: { program: options.onComplete.program } }),
        }),
        'application/json',
      ),
    devices: () => json(DEVICES, 'GET'),
    addDevice: (name, publicKey) =>
      json(DEVICES, 'POST', JSON.stringify({ name, public_key: publicKey }), 'application/json'),
    removeDevice: (name) => nothing(`${DEVICES}/${encodeURIComponent(name)}`, 'DELETE'),
    mkdir: async (parent, name) => {
      const answer = (await json('/api/v1/fs/mkdir', 'POST', JSON.stringify({ parent, name }), 'application/json')) as { path?: string }
      if (typeof answer.path !== 'string') throw new Error('the mkdir answer had no path')
      return answer.path
    },
  }
}

/**
 * The client the page runs on: the browser's own `fetch`, and the session store as the source of
 * both the token and the one thing to do when a call finds the session gone. Tasks 6 and on import
 * this instance rather than building their own, so one cookie and one CSRF value are the whole story.
 */
export const api = createClient(
  fetch,
  () => session.csrf(),
  session.generation,
  session.endSession,
)
