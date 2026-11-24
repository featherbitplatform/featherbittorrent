/**
 * The MediaSource engine: the one place the page feeds `SourceBuffer`s from the server.
 *
 * The server answers one HTTP stream per track, fragmented MP4, and the body opens with the init
 * segment (`ftyp` and `moov`) followed by the fragments, all in the one body. The engine appends
 * what it reads, one chunk at a time, in order, waiting for `updateend` between appends, and keeps
 * each buffer a bounded distance ahead of the playhead with the read gate from `buffering.ts`:
 * reads while the buffer is short, stops while it is not, and a `timeupdate` is the playhead
 * saying it moved, which is what lifts a stopped read.
 *
 * Every point of the file lives in the server's clock, source milliseconds, and the element lives
 * in media seconds: the two meet only at the edges, the fetch (`toSource`) and the element
 * (`toMedia`), with the probe's `time_offset_ms` between them. The engine is written against
 * `EngineDeps`, the small surface a real browser and `Player.test.tsx` both satisfy, so nothing
 * here reaches past the element and the source for a browser object.
 *
 * @module player/engine
 */
import { createReadGate, evictRanges } from './buffering'
import { toMedia, toSource } from './time'
import type { MediaTrack } from '../api/client'
import { isAbsolute } from '../endpoint'

/** The engine's state, as the player draws it. */
export type EngineState = 'loading' | 'waiting' | 'playing' | 'error'

/**
 * What the engine reaches for beyond the element: the network, the media source it will be, and
 * whether that source manages its own playback (a `ManagedMediaSource`, attached by `srcObject`
 * and with no object URL at all). The defaults are the page's own: `window.fetch`, then
 * `window.ManagedMediaSource` when the browser has one, else `window.MediaSource`.
 */
export interface EngineDeps {
  fetch: typeof fetch
  MediaSource: typeof MediaSource
  managed: boolean
  /**
   * The address of one track's body from a point of source time. Unset, it is the route on the
   * page's own origin; the player hands in the client's `mediaStreamUrl`, which for the desktop is
   * absolute and carries the token.
   */
  streamUrl?: (trackId: number, startMs: number) => string
}

/**
 * Why the engine stopped, in the engine's own terms: which track broke, what kind of thing broke
 * it, and whatever words the browser or the server offered.
 */
export interface EngineFailure {
  /** Which track failed, when the engine can tell: a stream's own HTTP answer, its SourceBuffer, its append. `null` for the element's own error. */
  role: 'video' | 'audio' | null
  /** `decode`: the element's MediaError or a SourceBuffer error; `server`: a non-OK HTTP answer other than 503/504; `buffer`: addSourceBuffer or appendBuffer threw. */
  kind: 'decode' | 'server' | 'buffer'
  /** The mime of the failing track (`video/mp4; codecs="..."`), when role is set. */
  mime: string | null
  /** The HTTP status, for kind `server`. */
  status: number | null
  /** The browser's or server's own words (MediaError.message, the exception's message, or the error body's `message` field when it parses as JSON), at most 300 characters, or null. */
  detail: string | null
}

/** The surface the player drives: seek, change the audio, read the state, go away. */
export interface Engine {
  /** Move the playhead to a point of source time. Outside the buffer, the streams restart there. */
  seek(sourceMs: number): void
  /**
   * Change the audio track: the old one is cut at the playhead, the new one is fetched there.
   * False when the engine cannot switch (no audio buffer yet, or no mime for the track), so the
   * player keeps showing the track that is still playing.
   */
  setAudio(track: MediaTrack): boolean
  /** The state the player draws. */
  state(): EngineState
  /**
   * Why it stopped, or null while it has not. The first failure is the one the player shows:
   * later ones are that failure's consequences and never overwrite it.
   */
  failure(): EngineFailure | null
  /** Stop everything: the fetches, the buffers, and the object URL the engine made, when it made one. */
  destroy(): void
  /** Subscribe to state changes. The engine does not hand back an unsubscribe; it dies with the player. */
  onState(cb: (s: EngineState) => void): void
}

interface Target {
  id: string
  file: number
}

/** The probe the engine maps source milliseconds and media seconds with. */
export interface EngineProbe {
  /** The shift every remuxed track carries, in source milliseconds. */
  offsetMs: number
  /** The file's duration in source milliseconds, when the probe knew it. */
  durationMs: number | null
  /** The tracks the player decided to play: the video first, the audio second, when there is one. */
  video: MediaTrack
  videoMime: string
  audio: MediaTrack | null
  audioMime: string | null
  /** What a track of this file's audio kind remuxes to, for a track the probe did not name. */
  mimeFor(track: MediaTrack): string | null
}

/** One stream the engine runs: a fetch it reads, a buffer it fills, and what its end means. */
interface Stream {
  track: MediaTrack
  buffer: SourceBuffer
  /** The mime this stream's buffer decodes, the one its failures are named by; updated with a `changeType`. */
  mime: string
  /** The chunk a quota refusal kept: it goes in before any later chunk, when the buffer has room. */
  heldChunk: ArrayBuffer | null
  /**
   * The timer that asks for the held chunk again. A held chunk at the playhead is one the element
   * is waiting on: it fires no `timeupdate` and the buffer no `updateend`, so a timer is the only
   * thing left to wake the read.
   */
  holdTimer: ReturnType<typeof setTimeout> | null
  /**
   * The far edge, in media seconds, of the range this body's last append grew: the range the body
   * feeds. Its end is where the body's data stops, whatever range the playhead sits in. Null until
   * the body's first append grows a range.
   */
  fedTo: number | null
  /**
   * The mime an audio switch changes the buffer to before the next body is read. It stays set
   * until the `changeType` has happened, so a seek that interrupts the switch hands it to the
   * restart, which fetches the new track rather than bringing the old one back.
   */
  pendingType: string | null
  /** True while the current body is one that begins the file's playback, or a restart of it. */
  isStart: boolean
  /** The read gate, one per stream: the two tracks of one file each keep their own answer. */
  gate: (bufferedAheadSeconds: number) => boolean
  abort: AbortController | null
  reader: ReadableStreamDefaultReader<Uint8Array> | null
  /** True while one `appendBuffer` is on its way to `updateend`. */
  appending: boolean
  /** True while the read loop is between reads, which is to say: running. */
  reading: boolean
  /** The point of source time the current body began at, from `X-Fbtt-Start-Ms`, or its start. */
  bodyStartMs: number
  /** True once the body has been read to its end at the end of the file: done, and not to be refetched. */
  atEnd: boolean
  /** How many times this body has stopped and been asked again; the delay follows it. */
  retries: number
  /** True while a retry is waiting on its timer: one timer per stream, never two. */
  retrying: boolean
  retryTimer: ReturnType<typeof setTimeout> | null
  /** What this stream is, for the engine's own bookkeeping: the video, or the audio. */
  role: 'video' | 'audio'
  /**
   * The generation the stream's read loop and its timers run under. It goes up on every abort,
   * seek and audio switch, and a loop that wakes from an `await` to find its generation gone
   * stops at once: the old stream must not touch the new one's reader or state.
   */
  generation: number
  /** The read pump: set on the stream once the function that closes over it is defined. */
  pump?: () => void
  /** Settled when the buffer stops updating after an `abort`; the next `beginStream` waits for it. */
  quiescent: Promise<void> | null
  /** The buffer's `error` listener, kept so `destroy` can take it back off the buffer. */
  errorListener: () => void
}

/** The shape the fake and the real `SourceBuffer` both give the engine. */
interface BufferLike extends EventTarget {
  buffered: { length: number; start(i: number): number; end(i: number): number }
  appendBuffer(data: ArrayBuffer | ArrayBufferView): void
  remove(start: number, end: number): void
  abort(): void
  changeType(mime: string): void
}

/** The shape the fake and the real media source both give the engine. */
interface SourceLike extends EventTarget {
  readyState: string
  duration: number
  addSourceBuffer(mime: string): BufferLike
  removeSourceBuffer(buffer: BufferLike): void
  endOfStream(mode?: string): void
}

/** The element the engine drives, as the engine reads it: every field it touches. */
interface VideoLike extends EventTarget {
  src?: string
  srcObject?: unknown
  disableRemotePlayback?: boolean
  currentTime: number
  /** `HAVE_NOTHING` (0) until the element has its metadata; a write then fires no `seeking`. */
  readyState: number
  buffered: { length: number; start(i: number): number; end(i: number): number }
  /** The element's own answer for why playback broke (`MediaError`), set when its `error` fires. */
  error?: { message?: string } | null
}

/**
 * The delays a body that stops gets before it is asked again: one second, two, four, and every
 * five seconds after that, the cadence the plan fixes for a stream that ends early.
 */
const RETRY_DELAYS = [1_000, 2_000, 4_000] as const
/** After the first three, every retry waits this long. */
const RETRY_STEADY = 5_000
/** How close to the end of the file a buffered second counts as the end. */
const END_WITHIN = 0.5
/**
 * How far outside a range a point may sit and still count as in it. The first fragment of a body
 * rarely starts on the exact millisecond the playhead was put at: a playhead at 1.000 and a range
 * that starts at 1.020 is the playhead in that range.
 */
const IN_RANGE_WITHIN = 0.1
/** How long a chunk the buffer refused for quota waits before it is offered again. */
const HOLD_RETRY_MS = 1_000

/**
 * The engine's two edges for a point of the file, said once so the fetch and the element cannot
 * drift apart on which clock they are reading.
 */
const map = (probe: EngineProbe) => ({
  toMedia: (sourceMs: number): number => toMedia(sourceMs, probe.offsetMs),
  toSource: (mediaSeconds: number): number => toSource(mediaSeconds, probe.offsetMs),
})

function defaultDeps(): EngineDeps {
  // The managed source, when the browser has one: it manages its own playback and is attached by
  // `srcObject`. The lib has no type for it, so it is read through a structural read. A browser
  // without either (a test jsdom) gets a constructor that throws on call, which is the honest
  // answer for "there is no media source here".
  const managedCtor = (window as { ManagedMediaSource?: new () => unknown }).ManagedMediaSource
  const mediaCtor =
    (managedCtor ?? (window as { MediaSource?: new () => unknown }).MediaSource ??
      ((): unknown => {
        throw new Error('no MediaSource in this browser')
      })) as typeof MediaSource
  // The browser's own source: the element's `load` (an object URL on a video) is what opens it,
  // and it does so asynchronously, firing `sourceopen`. A fake source has no element to load it,
  // so the engine opens one it makes itself, the way the element's load would.
  const openSource = (raw: unknown): void => {
    const open = (raw as { open?: () => void }).open
    if (typeof open === 'function') open.call(raw)
  }
  const Ctor: typeof MediaSource = function (this: unknown) {
    const instance = new mediaCtor()
    openSource(instance)
    return instance
  } as unknown as typeof MediaSource
  return {
    // The browser's `fetch` refuses a call it is not invoked as `window.fetch` (an `Illegal
    // invocation`), so the engine keeps the `window` as its `this` rather than handing the method
    // out unbound.
    fetch: (input, init) => window.fetch(input, init),
    MediaSource: Ctor,
    managed: managedCtor !== undefined,
  }
}

/** The far edge of a buffer's ranges, or 0 for an empty one. */
function bufferEnd(
  buffered: { length: number; start(i: number): number; end(i: number): number },
): number {
  let end = 0
  for (let i = 0; i < buffered.length; i += 1) end = Math.max(end, buffered.end(i))
  return end
}

/** Whether a point lies in a range, give or take `IN_RANGE_WITHIN` at either edge. */
function inRange(start: number, end: number, point: number): boolean {
  return start - IN_RANGE_WITHIN <= point && point <= end + IN_RANGE_WITHIN
}

/** The far edge of the range a point sits in, or 0 when it sits in none. */
function rangeEndAt(
  buffered: { length: number; start(i: number): number; end(i: number): number },
  point: number,
): number {
  let end = 0
  for (let i = 0; i < buffered.length; i += 1) {
    const endI = buffered.end(i)
    if (inRange(buffered.start(i), endI, point)) end = Math.max(end, endI)
  }
  return end
}

/**
 * How far the buffer runs ahead of the playhead, measured on the range the playhead sits in, or
 * 0 when it sits in none: a range an hour ahead is not one the person is watching, and the gate
 * must not keep it shut over a range the playhead has moved out of.
 */
function aheadOfPlayhead(
  buffered: { length: number; start(i: number): number; end(i: number): number },
  currentTime: number,
): number {
  let ahead = 0
  for (let i = 0; i < buffered.length; i += 1) {
    const start = buffered.start(i)
    const end = buffered.end(i)
    if (inRange(start, end, currentTime)) ahead = Math.max(ahead, end - currentTime)
  }
  return ahead
}

/** A `SourceBuffer`'s ranges as pairs, the shape `evictRanges` reads. */
function ranges(
  buffered: { length: number; start(i: number): number; end(i: number): number },
): [number, number][] {
  const out: [number, number][] = []
  for (let i = 0; i < buffered.length; i += 1) out.push([buffered.start(i), buffered.end(i)])
  return out
}

function isQuotaError(cause: unknown): boolean {
  return (
    cause instanceof DOMException &&
    (cause.name === 'QuotaExceededError' || cause.name === 'NS_ERROR_DOM_QUOTA_REACHED')
  )
}

/** The engine's own abort: an `AbortError` is the engine asking the stream to stop, not a failure. */
function isAbort(cause: unknown): boolean {
  return cause instanceof DOMException && cause.name === 'AbortError'
}

/** Whether the stream's fetch has been aborted: the engine's own word for "this stream is gone". */
function isAborted(stream: Stream): boolean {
  return stream.abort !== null && stream.abort.signal.aborted
}

/** How long a failure's `detail` may be: an explanation, not a dump. */
const DETAIL_MAX_CHARS = 300

/** The browser's or server's own words, capped, with an empty one made into null. */
function capDetail(message: string | null): string | null {
  if (message === null || message === '') return null
  return message.length > DETAIL_MAX_CHARS ? message.slice(0, DETAIL_MAX_CHARS) : message
}

/** The browser's or exception's own words, capped, or null when it has none. */
function messageOf(cause: unknown): string | null {
  // Read the `message` without assuming `cause instanceof Error`: the browser's refusals are
  // `DOMException`s, which are Error-like whether or not a given runtime puts them on Error's
  // chain, and their words are exactly what the player must show.
  const raw = cause instanceof Error
    ? cause.message
    : typeof cause === 'object' && cause !== null && 'message' in cause
      ? (cause as { message: unknown }).message
      : null
  return capDetail(typeof raw === 'string' ? raw : null)
}

/** How many bytes of an error body are worth reading: the message is a sentence, not a dump. */
const ERROR_BODY_CAP_BYTES = 4_096
/** How long an error body may take to say it: a server that keeps streaming is answered, not followed. */
const ERROR_BODY_TIMEOUT_MS = 2_000

/**
 * The server's own words for a refused stream: the error body is small JSON
 * (`{ "error": ..., "message": ... }`). It is read through the body's own reader, capped at
 * `ERROR_BODY_CAP_BYTES` bytes and `ERROR_BODY_TIMEOUT_MS` milliseconds - whichever comes first,
 * so a large or never-ending body cannot hold the player in `loading` or grow memory without
 * end. JSON gives its `message` field; a body that is not JSON is its own words, capped. The
 * reader is cancelled afterwards, and its abort listener does not outlive the read.
 */
async function errorDetail(response: Response, signal: AbortSignal): Promise<string | null> {
  if (response.body === null) return null
  const reader = response.body.getReader()
  const gaveUp = Symbol('the read stopped early')
  let timerId: ReturnType<typeof setTimeout> | null = null
  // The abort listener's undo, set the moment the listener is put on; the `finally` below always
  // runs it, so the listener never outlives the read it was racing.
  let stopAbort: () => void = (): void => {}
  const timeout = new Promise<typeof gaveUp>((resolve) => {
    timerId = setTimeout(() => resolve(gaveUp), ERROR_BODY_TIMEOUT_MS)
    const onAbort = (): void => resolve(gaveUp)
    signal.addEventListener('abort', onAbort, { once: true })
    stopAbort = (): void => signal.removeEventListener('abort', onAbort)
  })
  try {
    const decoder = new TextDecoder()
    let text = ''
    let size = 0
    for (;;) {
      const raced = await Promise.race([reader.read(), timeout])
      if (raced === gaveUp) return null
      if (raced.done === true) break
      text += decoder.decode(raced.value, { stream: true })
      size += raced.value.byteLength
      if (size >= ERROR_BODY_CAP_BYTES) {
        text = text.slice(0, ERROR_BODY_CAP_BYTES)
        break
      }
    }
    let parsed: unknown
    try {
      parsed = JSON.parse(text)
    } catch {
      // Not JSON. An HTML page (a proxy or a load balancer answering in the server's place) is
      // not an explanation a person can use - the status line already says the server refused -
      // so it says nothing. Any other body is the server's own words.
      if (text.trim().startsWith('<')) return null
      return capDetail(text)
    }
    const message = parsed !== null && typeof parsed === 'object'
      ? (parsed as { message?: unknown }).message
      : undefined
    return capDetail(typeof message === 'string' ? message : null)
  } finally {
    if (timerId !== null) clearTimeout(timerId)
    stopAbort()
    // The read is over however it ended: close the body, and any read still pending on it.
    // A stream already closed says so, and that is the same done.
    try {
      void Promise.resolve(reader.cancel()).catch(() => {})
    } catch {
      // A reader that will not answer `cancel` is already closed.
    }
  }
}

/**
 * Feed one video element from the server's per-track streams.
 *
 * The engine owns the source's life: it opens it, adds the buffers in the order the decoder wants
 * them (the video first, the audio after, so a seek the audio does not have is clamped by the
 * video, which is the track the picture comes from), and it ends the stream when the file does.
 * What it does not own is the element's transport: `paused`, `muted` and `volume` are the
 * player's, and the keyboard's, and they are never the engine's.
 */
export function createEngine(video: VideoLike, target: Target, probe: EngineProbe, deps?: Partial<EngineDeps>): Engine {
  const full = { ...defaultDeps(), ...deps }
  const mapTimes = map(probe)

  // The state the player draws, and who to tell it to. A state that repeats is not news.
  let state: EngineState = 'loading'
  const listeners: Array<(s: EngineState) => void> = []
  const streams: Stream[] = []
  const rawSource = new full.MediaSource()
  const source = rawSource as unknown as SourceLike
  let objectUrl: string | null = null
  let destroyed = false
  let endedFired = false
  // The source is not open yet when `createEngine` runs, and the player may ask to start away from
  // the beginning (the remembered position) before it opens: the start the `onOpen` will act on.
  let pendingStartMs: number | null = null
  let errorFired = false
  // The reason the engine stopped, kept for the player to say. The first failure is the cause:
  // every later one is its consequence, and it does not overwrite the record.
  let recorded: EngineFailure | null = null
  // Whether the buffers exist: an `addSourceBuffer` that threw leaves the engine with none, and
  // a restart then has no video stream to fetch.
  let buffersOpen = false
  function recordFailure(next: EngineFailure): void {
    if (recorded === null) recorded = next
  }
  // The point the engine itself put the playhead at, so the `seeking` that write fires is not
  // answered with a restart of the streams it was just asking for.
  let engineSeekTo: number | null = null
  // The video stream's generation when the engine made that write, or null when the element had
  // no metadata yet. A write before metadata fires no `seeking`: it becomes the element's default
  // start position, and the element seeks there (firing `seeking` at that point) once its
  // metadata arrives, however much data the body has fed by then. So a point written before
  // metadata is kept until that seek; one written after it names the engine's own move only while
  // the body that made it is still the live one.
  let engineSeekGeneration: number | null = null
  let videoStream: Stream
  let audioStream: Stream | null

  function setState(next: EngineState): void {
    if (next === state) return
    state = next
    for (const listener of listeners) listener(next)
    // An `error` is the end of the streams: a buffer that will not take data, and a fetch the
    // server will not answer, are both stops the engine cannot work around, and a read that
    // keeps running after the player shows the error is a cost, not a courtesy.
    if (next === 'error' && !errorFired) {
      errorFired = true
      for (const stream of streams) abortStream(stream)
    }
  }

  // Attach the element to the source: an object URL for a plain `MediaSource`, a `srcObject` for a
  // managed one, which manages its own playback and therefore has no object URL at all.
  if (full.managed) {
    video.disableRemotePlayback = true
    video.srcObject = rawSource
  } else {
    objectUrl = URL.createObjectURL(rawSource)
    video.src = objectUrl
  }

  function addStream(track: MediaTrack, mime: string, role: 'video' | 'audio'): Stream {
    // The source the engine holds is its own structural read; the buffer it returns is the same.
    const stream: Stream = {
      track,
      mime,
      buffer: source.addSourceBuffer(mime) as unknown as Stream['buffer'],
      heldChunk: null,
      holdTimer: null,
      fedTo: null,
      pendingType: null,
      isStart: false,
      gate: createReadGate(),
      abort: null,
      reader: null,
      appending: false,
      reading: false,
      bodyStartMs: 0,
      atEnd: false,
      retries: 0,
      retrying: false,
      retryTimer: null,
      role,
      generation: 0,
      quiescent: null,
      errorListener: (): void => {},
    }
    // One `updateend` ends the append the buffer was busy with, and it is also where a stopped
    // read finds out there is room to go again.
    stream.buffer.addEventListener('updateend', () => {
      stream.appending = false
      stream.pump?.()
    })
    // A buffer's own `error` is the MSE's word for a stream its decoder gave up on. It does not
    // throw anywhere the engine can catch it: the failed append still gets its `updateend`, so
    // without this listener a broken track would look like a finished one. An event already
    // queued when the engine is torn down still reaches this listener after `destroy`, and a
    // dead engine has no failure to report and no player left to tell.
    const onBufferError = (): void => {
      if (destroyed) return
      recordFailure({ role: stream.role, kind: 'decode', mime: stream.mime, status: null, detail: null })
      setState('error')
    }
    stream.errorListener = onBufferError
    stream.buffer.addEventListener('error', onBufferError)
    streams.push(stream)
    // The pump closes over `readStream` and the rest, so it is handed to the stream once they are
    // all in scope: the `updateend` above and the `timeupdate` below both call it through this field.
    stream.pump = () => pump(stream)
    return stream
  }

  // The buffers are added when the source opens, not when the engine is made: a `SourceBuffer`
  // refused by a source that is not yet open is the browser's `InvalidStateError`, and the order
  // the decoder wants them in is the video first, the audio after. A buffer the source refuses
  // outright is the end of playback, not a thing to retry: the engine says so instead of sitting
  // in `loading` with no stream to fetch.
  function openBuffers(): void {
    try {
      videoStream = addStream(probe.video, probe.videoMime, 'video')
    } catch (cause) {
      recordFailure({
        role: 'video',
        kind: 'buffer',
        mime: probe.videoMime,
        status: null,
        detail: messageOf(cause),
      })
      setState('error')
      return
    }
    buffersOpen = true
    if (probe.audio === null || probe.audioMime === null) {
      audioStream = null
      return
    }
    try {
      audioStream = addStream(probe.audio, probe.audioMime, 'audio')
    } catch (cause) {
      audioStream = null
      recordFailure({
        role: 'audio',
        kind: 'buffer',
        mime: probe.audioMime,
        status: null,
        detail: messageOf(cause),
      })
      setState('error')
    }
  }

  function streamUrl(trackId: number, startMs: number): string {
    if (full.streamUrl !== undefined) return full.streamUrl(trackId, startMs)
    const ms = Math.max(0, Math.floor(startMs))
    return `/api/v1/media/${encodeURIComponent(target.id)}/${encodeURIComponent(target.file)}` +
      `/tracks/${encodeURIComponent(trackId)}/stream?start_ms=${ms}`
  }

  /**
   * Open one body at a point of source time. `primary` is the one the playhead follows: its
   * answer's `X-Fbtt-Start-Ms` is where the playhead lands, and `follow` is the one asked for at
   * that same point, so the two tracks begin at the same place rather than drifting apart from
   * their first fragment.
   */
  function openAt(primary: Stream, follow: Stream | null, startMs: number): void {
    if (destroyed) return
    const stream = primary
    beginStream(primary, startMs, (bodyStartMs, requestedMs) => {
      // The playhead lands where the later of the two points is: the person asked for one, the
      // server's body began at the other, and the body's data covers from its own start, so the
      // playhead must not sit before the data. The move is the engine's own, and the `seeking` it
      // fires is the one the guard below answers with no restart: `engineSeekTo` names the media
      // point the engine put the playhead at, in the element's clock, and the event that matches
      // it is ignored and cleared. A retry after a seek back out of a far range is not a first
      // fetch: the playhead stays where the person put it, and only the data moves.
      const startMsLanded = Math.max(requestedMs, bodyStartMs)
      if (stream.role !== 'video' || !stream.isStart) return
      stream.isStart = false
      const landed = mapTimes.toMedia(startMsLanded)
      const beforeMetadata = video.readyState === 0
      // A start at 0 before metadata is the element's own default: no seek ever comes for it, so
      // there is nothing to arm.
      engineSeekTo = beforeMetadata && landed === 0 ? null : landed
      engineSeekGeneration = beforeMetadata ? null : stream.generation
      video.currentTime = landed
      // The follow is asked for at the same point the primary landed on.
      if (follow !== null) beginStream(follow, bodyStartMs)
      if (state === 'loading') setState('playing')
    })
  }

  function beginStream(
    stream: Stream,
    startMs: number,
    onStarted?: (bodyStartMs: number, requestedMs: number) => void,
  ): void {
    if (destroyed) return
    // A first fetch is a start, and a restart is a start: the playhead lands at the body's
    // start either way. What the start is not is a retry, and the flag is set after the abort
    // below, which would clear it, so a retry of a body that begins again is not mistaken for
    // a start.
    abortStream(stream)
    clearRetry(stream)
    stream.atEnd = false
    stream.isStart = true
    stream.fedTo = null
    // The body this call opens begins here until its answer says otherwise: a body that fails
    // before it answers (a 503, a 504, the network) must not leave the last body's start in
    // place, or its retry resumes from where the old body was, not from where this one was asked.
    stream.bodyStartMs = Math.max(0, startMs)

    const abort = new AbortController()
    stream.abort = abort
    const url = streamUrl(stream.track.id, startMs)
    // The abort above may have left the buffer mid-update: settle it before the first append,
    // the way a browser's `abort` + `updateend` sequence does.
    const quiescent = stream.quiescent
    stream.quiescent = null

    void (async () => {
      if (quiescent !== null) await quiescent
      if (destroyed || abort.signal.aborted) return
      let response: Response
      try {
        // A body on the page's own origin rides the cookie; one at another address (the desktop's
        // engine) has its token in the URL and no cookie of this page's to send.
        response = await full.fetch(url, {
          signal: abort.signal,
          credentials: isAbsolute(url) ? 'omit' : 'same-origin',
        })
      } catch (cause) {
        // The engine's own abort is not a failure: the stream is being replaced, and the old
        // fetch is the way the engine said so.
        if (destroyed || abort.signal.aborted || isAbort(cause)) return
        // A network error is a wait, not an end: the state, and its retries. A start of the
        // video that fails re-runs the whole start, so the audio is asked for too. A resume (a
        // retry, which has no `onStarted`) only asks again: a restart would move the playhead to
        // the far end of what is buffered and skip everything in between.
        setState('waiting')
        if (stream.role === 'video' && onStarted !== undefined) {
          scheduleRetry(stream, () => restartAll(startMs))
        } else {
          scheduleRetry(stream)
        }
        return
      }
      if (destroyed || abort.signal.aborted) return
      if (!response.ok) {
        // A 504 is a read timeout and a 503 a full set of streams: both are waits, not errors,
        // and a 503's `Retry-After` is the shortest the wait may be.
        if (response.status === 504 || response.status === 503) {
          const retryAfterMs = parseRetryAfterMs(response)
          setState('waiting')
          // The same rule as the network error: only a start is re-run whole.
          if (stream.role === 'video' && onStarted !== undefined) {
            scheduleRetry(stream, () => restartAll(startMs), retryAfterMs ?? undefined)
          } else {
            scheduleRetry(stream, undefined, retryAfterMs ?? undefined)
          }
          return
        }
        // A refusal (404, 415, 500) is this file's answer, not a wait: name the status and take
        // the server's own message from the small JSON body it answers with.
        const detail = await errorDetail(response, abort.signal)
        if (destroyed || abort.signal.aborted) return
        recordFailure({
          role: stream.role,
          kind: 'server',
          mime: stream.mime,
          status: response.status,
          detail,
        })
        setState('error')
        return
      }

      const header = response.headers?.get?.('x-fbtt-start-ms')
      const parsed = header === null || header === undefined ? Number.NaN : Number(header)
      const bodyStartMs = Number.isFinite(parsed) && parsed > 0 ? parsed : Math.max(0, startMs)
      stream.bodyStartMs = bodyStartMs
      // An audio switch that has not reached its `changeType` yet is finished here, before the
      // new track's first byte: the switch's own body, or a restart that interrupted it. The cut
      // starts where the new body's data starts, never before it: a body that begins later than
      // asked leaves the old track playing up to it, not a gap at the playhead.
      if (stream.pendingType !== null) {
        const from = mapTimes.toMedia(Math.max(startMs, bodyStartMs))
        const switched = await switchType(stream, stream.pendingType, from, abort.signal)
        if (!switched) return
      }
      onStarted?.(bodyStartMs, startMs)

      if (response.body === null) {
        stream.reader = null
        settleEnd(stream)
        return
      }
      stream.reader = response.body.getReader()
      stream.reading = true
      readStream(stream)
    })()
  }

  async function readStream(stream: Stream): Promise<void> {
    for (;;) {
      // The generation is the stream's identity: a loop that wakes to find it changed is the old
      // stream, and it stops at once, never touching the new stream's reader or state.
      if (destroyed) return
      if (stream.reader === null) return
      // A chunk the buffer refused for quota is the one that goes in when there is room: the body
      // is held at its end, and the re-append is what the next `timeupdate` or `updateend` takes,
      // before any later chunk of the body.
      if (stream.heldChunk !== null) {
        // The held chunk goes in only when the read is woken by the playhead moving (a
        // `timeupdate`) or the buffer making room (an `updateend`), not in the read loop:
        // re-appending it here would spin on a buffer that is still full.
        stream.reading = false
        return
      }
      const generation = stream.generation
      // The gate answers before each read: a buffer a minute ahead of the playhead is not one
      // the person is watching, and its fetch stops until a `timeupdate` says the playhead moved.
      // The measure is the range the playhead sits in, not the far end of everything buffered:
      // a range far ahead that the playhead has left behind does not keep the gate shut.
      const ahead = aheadOfPlayhead(stream.buffer.buffered, video.currentTime)
      if (!stream.gate(ahead)) {
        stream.reading = false
        return
      }
      let result: ReadableStreamReadResult<Uint8Array>
      try {
        result = await stream.reader.read()
      } catch (cause) {
        // The engine's own abort is not the body ending: the stream is being replaced, and the
        // old loop must not say the file stopped.
        if (destroyed || isAborted(stream) || isAbort(cause) || stream.generation !== generation) return
        setState('waiting')
        scheduleRetry(stream)
        return
      }
      if (stream.generation !== generation) return
      if (result.done) {
        // A `done` after the engine aborted is the abort, not an end: the stream is being replaced.
        if (isAborted(stream)) return
        stream.reader = null
        stream.reading = false
        settleEnd(stream)
        return
      }

      // One chunk at a time, in order, and a wait for `updateend` before the next: a
      // `SourceBuffer` is not a queue, and two appends in flight is an error at best.
      const chunk = result.value
      const data: ArrayBuffer = (chunk.buffer as ArrayBuffer).slice(chunk.byteOffset, chunk.byteOffset + chunk.byteLength)
      const appended = await appendChunk(stream, data, generation)
      if (stream.generation !== generation) return
      if (appended) {
        // An append that landed while the engine was waiting is the data arriving: the state
        // goes back to playing, and the retry count starts again from the first stop.
        if (state === 'waiting') setState('playing')
        stream.retries = 0
      }
    }
  }

  /**
   * Record the append that threw. When the element already has its own error, the throw is that
   * failure's echo: an append after a dead decoder is the browser's `InvalidStateError`, and the
   * decode failure with the element's message is the real cause, not the append. Otherwise the
   * buffer refused the data for a reason the engine cannot answer.
   */
  function recordAppendFailure(stream: Stream, cause: unknown): void {
    const elementError = video.error ?? null
    if (elementError !== null) {
      recordFailure({
        role: stream.role,
        kind: 'decode',
        mime: stream.mime,
        status: null,
        detail: capDetail(elementError.message ?? null),
      })
      return
    }
    recordFailure({
      role: stream.role,
      kind: 'buffer',
      mime: stream.mime,
      status: null,
      detail: messageOf(cause),
    })
  }

  /**
   * Append one chunk, and if the buffer is full, run the eviction ranges one at a time, each
   * awaited to `updateend`, and retry the same chunk before any later one. A chunk that still
   * does not fit stops the read at the gate rather than throwing.
   */
  async function appendChunk(
    stream: Stream,
    data: ArrayBuffer,
    generation: number,
  ): Promise<boolean> {
    try {
      const before = ranges(stream.buffer.buffered)
      stream.appending = true
      stream.buffer.appendBuffer(data)
      if (stream.generation !== generation) {
        stream.appending = false
        return false
      }
      await appendDone(stream, generation)
      stream.appending = false
      if (stream.generation !== generation) return false
      noteFed(stream, before)
      return true
    } catch (cause) {
      if (stream.generation !== generation) return false
      stream.appending = false
      // A quota error is a buffer that is full, and the eviction below makes room. Any other
      // error is the buffer saying no for a reason the engine cannot answer, and it is the
      // player's `error`, not a silent drop of the chunk.
      if (!isQuotaError(cause)) {
        recordAppendFailure(stream, cause)
        setState('error')
        return false
      }
      // The buffer is full: cut what lies far behind the playhead, and every range that holds
      // neither the playhead nor the body's own data, one range at a time, each `remove` awaited
      // to its `updateend`, and a `remove` of nothing is skipped.
      const cut = evictionCut(stream)
      for (const [start, end] of cut) {
        if (stream.generation !== generation) return false
        // The buffer may still be finishing the update that ended in the quota error: a `remove`
        // while it is updating is the browser's `InvalidStateError`, so the wait comes first.
        await waitNotUpdating(stream.buffer, () => stream.generation !== generation)
        if (stream.generation !== generation) return false
        try {
          stream.buffer.remove(start, end)
        } catch {
          // A remove the buffer refuses is data it already let go.
        }
        await appendDone(stream, generation)
      }
      // The same chunk, again, before any later one: the eviction made room for it, and the
      // order of the buffer is the order the decoder wants it in.
      try {
        if (stream.generation !== generation) return false
        await waitNotUpdating(stream.buffer, () => stream.generation !== generation)
        if (stream.generation !== generation) return false
        const before = ranges(stream.buffer.buffered)
        stream.appending = true
        stream.buffer.appendBuffer(data)
        if (stream.generation !== generation) return false
        await appendDone(stream, generation)
        stream.appending = false
        if (stream.generation !== generation) return false
        noteFed(stream, before)
        return true
      } catch (cause) {
        // Still full after one eviction. A quota is the buffer saying there is no room yet: the
        // chunk is kept, the read stops, and the same chunk goes in when the buffer makes room
        // (the next `timeupdate` or `updateend` after the playhead has moved, or the hold timer
        // when neither comes). Any other error is the buffer saying no for a reason the engine
        // cannot answer, and it is the player's `error`, not a silent drop of the chunk.
        stream.appending = false
        if (stream.generation !== generation) return false
        if (isQuotaError(cause)) {
          stream.heldChunk = data
          armHold(stream, generation)
          return false
        }
        recordAppendFailure(stream, cause)
        setState('error')
        return false
      }
    }
  }

  /**
   * After an append, the range it grew is the range the body feeds: the end that is new, or that
   * moved, is where the body's data stops now. An append that grew no range (an init segment)
   * leaves the last answer in place.
   */
  function noteFed(stream: Stream, before: [number, number][]): void {
    let grown: number | null = null
    for (const [, end] of ranges(stream.buffer.buffered)) {
      if (!before.some(([, was]) => Math.abs(was - end) < 1e-6)) grown = Math.max(grown ?? end, end)
    }
    if (grown !== null) stream.fedTo = grown
  }

  /**
   * The far edge of the range the body feeds: the range its last append grew, or before any, the
   * range its start falls in. 0 when the body has no data in the buffer.
   */
  function fedRangeEnd(stream: Stream): number {
    const buffered = stream.buffer.buffered
    if (stream.fedTo !== null) {
      const end = rangeEndAt(buffered, stream.fedTo)
      if (end > 0) return end
    }
    return rangeEndAt(buffered, mapTimes.toMedia(stream.bodyStartMs))
  }

  /**
   * What a full buffer gives up: every range that holds neither the playhead nor the body's own
   * data, whole, and of the ranges that do, what lies far behind the playhead. A buffer full of
   * ranges the playhead is not in would otherwise refuse the chunk the playhead is waiting on
   * for good.
   */
  function evictionCut(stream: Stream): [number, number][] {
    const playhead = video.currentTime
    const fed = stream.fedTo ?? mapTimes.toMedia(stream.bodyStartMs)
    const cut: [number, number][] = []
    for (const [start, end] of ranges(stream.buffer.buffered)) {
      if (inRange(start, end, playhead) || inRange(start, end, fed)) {
        cut.push(...evictRanges([[start, end]], playhead))
      } else {
        cut.push([start, end])
      }
    }
    return cut.filter(([start, end]) => end > start)
  }

  /** Offer the held chunk again after a while, whether or not the element says anything. */
  function armHold(stream: Stream, generation: number): void {
    if (stream.holdTimer !== null) clearTimeout(stream.holdTimer)
    stream.holdTimer = setTimeout(() => {
      stream.holdTimer = null
      if (destroyed || stream.generation !== generation) return
      pump(stream)
    }, HOLD_RETRY_MS)
  }

  /** Drop the held chunk and its timer: the body it came from is gone. */
  function dropHeld(stream: Stream): void {
    stream.heldChunk = null
    if (stream.holdTimer !== null) {
      clearTimeout(stream.holdTimer)
      stream.holdTimer = null
    }
  }

  /**
   * Change an audio buffer's type for a new track: cut what it holds from `from` to the end, then
   * `changeType`, each step awaited until the buffer is not updating. False when the body the
   * switch was for went away first; the switch stays pending, and the next body finishes it.
   */
  async function switchType(stream: Stream, mime: string, from: number, signal: AbortSignal): Promise<boolean> {
    const gone = (): boolean => destroyed || signal.aborted
    const end = bufferEnd(stream.buffer.buffered)
    if (end > from) {
      await waitNotUpdating(stream.buffer, gone)
      if (gone()) return false
      try {
        stream.buffer.remove(Math.max(from, 0), end)
      } catch {
        // A remove the buffer refuses is data it already let go.
      }
    }
    await waitNotUpdating(stream.buffer, gone)
    if (gone()) return false
    try {
      stream.buffer.changeType(mime)
    } catch {
      // A buffer that will not change its type is one that is already gone.
    }
    // The stream now carries this track: its failures are named by the new mime.
    stream.mime = mime
    stream.pendingType = null
    await waitNotUpdating(stream.buffer, gone)
    return !gone()
  }

  /** A read loop that is stopped by its gate, woken by a `timeupdate` moving the playhead. */
  function pump(stream: Stream): void {
    if (destroyed || stream.reading || stream.appending) return
    if (stream.reader === null && stream.heldChunk === null) return
    // A held chunk goes in before any later data, and the read does not resume until it has:
    // the buffer made room when the playhead moved, and the order of the buffer is the order
    // the decoder wants it in.
    if (stream.heldChunk !== null) {
      const held = stream.heldChunk
      dropHeld(stream)
      // The re-append is the read loop's turn: `reading` stays true until it is over, so an
      // `updateend` from its own eviction does not start a read of the next chunk in the middle
      // of it. The read resumes from here, after the held chunk is in.
      stream.reading = true
      void (async () => {
        // A seek that arrives while the held chunk is being asked for restarts the stream, and
        // the held chunk is the old stream's: it is dropped, and the new stream reads its own.
        // The new stream owns `reading` then, so the old turn leaves it alone.
        const generation = stream.generation
        const appended = await appendChunk(stream, held, generation)
        if (stream.generation !== generation) return
        if (!appended) {
          // Held again (its timer is armed), or the buffer refused it for good (the `error`).
          stream.reading = false
          return
        }
        if (state === 'waiting') setState('playing')
        stream.retries = 0
        if (stream.reader === null) {
          stream.reading = false
          return
        }
        void readStream(stream)
      })()
      return
    }
    // The same measure the read loop uses: the range the playhead sits in, or none.
    const ahead = aheadOfPlayhead(stream.buffer.buffered, video.currentTime)
    if (!stream.gate(ahead)) return
    stream.reading = true
    void readStream(stream)
  }

  /**
   * Wait until the buffer is not updating: the browser's word that an append, remove or type
   * change has landed, after which the next one is legal. A stream that goes away while the wait
   * runs (a seek, a switch, a destroy) ends it: the caller is the old stream and has no buffer to
   * keep working.
   */
  function waitNotUpdating(buffer: Stream['buffer'], isGone: () => boolean): Promise<void> {
    return new Promise((resolve) => {
      const check = (): void => {
        if (isGone()) {
          resolve()
          return
        }
        if (!buffer.updating) resolve()
        else buffer.addEventListener('updateend', check, { once: true })
      }
      check()
    })
  }

  /** Wait for the buffer to stop updating after an append, and none the stream no longer is. */
  function appendDone(stream: Stream, generation: number): Promise<void> {
    return waitNotUpdating(stream.buffer, () => stream.generation !== generation)
  }

  /** A body that has been read to its end: is the file over, or does the stream wait to be asked again? */
  function settleEnd(stream: Stream): void {
    if (destroyed || endedFired) return
    const end = probe.durationMs === null ? null : mapTimes.toMedia(probe.durationMs)
    if (end !== null) {
      // The end is reached when the range this body fed runs to it, not when any range does: a
      // far range that covers the file's end is not this body's data, and the range the playhead
      // sits in may be an earlier one the body has long since left behind.
      const reached = fedRangeEnd(stream) >= end - END_WITHIN
      if (reached) {
        // This body ended at the end of the file: it is done, and not to be refetched.
        stream.atEnd = true
        if (streams.every((other) => other.atEnd)) {
          endedFired = true
          try {
            source.endOfStream()
          } catch {
            // A source that is already done is the best outcome the call could have.
          }
        }
        return
      }
    }
    // The body ended but the file is not over, or the end cannot be measured: the stream stopped
    // early, and it waits to be asked again.
    setState('waiting')
    scheduleRetry(stream)
  }

  /**
   * Ask again after a stop. The delay follows the retry count, which persists across stops and
   * resets only on a successful append, so the cadence is one, two, four, and then five seconds
   * each. A `minimumMs` (a 503's `Retry-After`) lengthens the wait, never shortens it.
   */
  function scheduleRetry(
    stream: Stream,
    action?: () => void,
    minimumMs?: number,
  ): void {
    if (destroyed || stream.retrying) return
    stream.retrying = true
    let delay = stream.retries < RETRY_DELAYS.length ? RETRY_DELAYS[stream.retries] : RETRY_STEADY
    if (minimumMs !== undefined) delay = Math.max(delay, minimumMs)
    stream.retries += 1
    stream.retryTimer = setTimeout(() => {
      stream.retryTimer = null
      stream.retrying = false
      if (destroyed) return
      if (action !== undefined) {
        action()
        return
      }
      // The retry goes from the end of the range the body fed, not from where the fetch began:
      // the data that made it in is kept, and the server resumes where that range stops. A
      // range an hour ahead is not this body's, and the retry must not start past it; the range
      // the playhead sits in may be one the body already left, and refetching it is waste. A
      // body with no data in the buffer resumes from its own start.
      //
      // An audio switch that has not reached its `changeType` is the exception: the buffer still
      // holds the old track, and its range is not the new track's data. Resuming from its end
      // would leave the old language playing up to it, so the new track is asked for at the
      // playhead, or at the body's own start when that is later.
      if (stream.pendingType !== null) {
        beginStream(stream, Math.max(stream.bodyStartMs, mapTimes.toSource(video.currentTime)))
        return
      }
      const rangeEnd = fedRangeEnd(stream)
      const fromMs = rangeEnd > 0 ? mapTimes.toSource(rangeEnd) : stream.bodyStartMs
      beginStream(stream, fromMs)
    }, delay)
  }

  /** The `Retry-After` seconds a 503 answers with, or null when the header is not one. */
  function parseRetryAfterMs(response: Response): number | null {
    const value = response.headers?.get?.('retry-after')
    if (value === null || value === undefined) return null
    const seconds = Number(value)
    return Number.isFinite(seconds) && seconds >= 0 ? seconds * 1000 : null
  }

  function clearRetry(stream: Stream): void {
    if (stream.retryTimer !== null) {
      clearTimeout(stream.retryTimer)
      stream.retryTimer = null
    }
    stream.retrying = false
  }

  /**
   * Bump the stream's generation and stop it: the old loops and timers find it gone. The buffer
   * is `abort()`ed, the way a browser's `SourceBuffer.abort` stops an in-flight update, and the
   * wait for it to stop updating is what makes the new body's first append legal: an append while
   * the old update is still running is the browser's `InvalidStateError`. The wait is handed to
   * the stream, and the next `beginStream` settles it before it reads.
   */
  function abortStream(stream: Stream): void {
    stream.generation += 1
    stream.isStart = false
    clearRetry(stream)
    // A chunk held for quota belongs to the body being stopped: appended into the next body, it
    // would be the old position's (or the old track's) data in the middle of the new one.
    dropHeld(stream)
    stream.reading = false
    stream.appending = false
    if (stream.reader !== null) {
      const reader = stream.reader
      stream.reader = null
      // A `cancel` puts the reader away without a final `read`: the loop is gone, and so is the body.
      void reader.cancel().catch(() => {
        // A body that will not cancel is a body that is already gone.
      })
    }
    if (stream.abort !== null) {
      stream.abort.abort()
      stream.abort = null
    }
    // An `abort` is legal while the buffer is updating, and it keeps the buffered ranges: it
    // stops the current update.
    const gone = stream.generation
    try {
      stream.buffer.abort()
    } catch {
      // A buffer the source already took away has nothing to stop.
    }
    stream.quiescent = waitNotUpdating(stream.buffer, () => destroyed || stream.generation !== gone)
  }

  function restartAll(fromMs: number): void {
    // A stream that was never opened has nothing to restart: a refused buffer is itself the
    // error, and there is no video stream to fetch again.
    if (!buffersOpen) return
    // A restart after an error is a fresh start: the streams run again, so an error in them must
    // stop them again, and the state is the start's, not the old error.
    errorFired = false
    if (state === 'error') setState('loading')
    // Every stream stops where it is and starts again at the same point of source time: the
    // fetches abort (and the generation bumps), and the playhead is where the person put it.
    for (const stream of streams) {
      abortStream(stream)
    }
    if (audioStream !== null) {
      // The playhead follows the video; the audio is asked for at the same point it lands on.
      openAt(videoStream, audioStream, fromMs)
    } else {
      openAt(videoStream, null, fromMs)
    }
  }

  // A seek the buffer already covers costs nothing: the element plays from what it has. A seek
  // that does not is a restart of both streams at the point the element moved to. The `seeking`
  // the engine's own `currentTime` write fires is answered with a restart, not a second one:
  // `engineSeekTo` names that point, and the event that matches it is the engine's, so it is
  // ignored and cleared.
  function onSeeking(): void {
    if (destroyed) return
    const time = video.currentTime
    if (time < 0) return
    // The file ended, and the person asked to go back: the ended state is the file's word for
    // "nothing more to fetch", and a seek past it is the person's word for "there is". The
    // streams restart at the point the element moved to, and the end is un-fired: the next time
    // the playhead reaches the true end, `settleEnd` fires it again.
    if (endedFired) {
      endedFired = false
      for (const stream of streams) stream.atEnd = false
      restartAll(mapTimes.toSource(time))
      return
    }
    if (engineSeekTo !== null) {
      const target = engineSeekTo
      engineSeekTo = null
      // A point written before metadata is the element's own seek at metadata, whatever the body
      // has fed since; one written after it is the engine's own move while that body is live.
      // Otherwise a person seeking to the same point gets the coverage check like any seek.
      const own =
        engineSeekGeneration === null ||
        (videoStream !== undefined && videoStream.generation === engineSeekGeneration)
      if (own && Math.abs(time - target) < 1e-3) return
    }
    const covered = video.buffered.length > 0 &&
      [...Array(video.buffered.length).keys()].some(
        (i) => video.buffered.start(i) <= time && time <= video.buffered.end(i),
      )
    if (covered) return
    restartAll(mapTimes.toSource(time))
  }

  function onTimeUpdate(): void {
    if (destroyed) return
    // A `timeupdate` is the playhead saying where it is, and it is what lifts a read the gate
    // stopped: the buffer that was an hour ahead of a paused playhead is not one ahead of it now.
    for (const stream of streams) pump(stream)
  }

  // The element's own `error`: the decoder giving up mid-stream, the playback dying for a reason
  // outside any stream. Its `MediaError.message` is the system's own words; and when an append
  // has already recorded which track broke, the message fills in that record rather than being
  // recorded over it, because the throw was this failure's echo.
  function onElementError(): void {
    const message = capDetail(video.error?.message ?? null)
    if (recorded === null) {
      recorded = { role: null, kind: 'decode', mime: null, status: null, detail: message }
    } else if (recorded.role !== null && recorded.detail === null && message !== null) {
      // The track that broke is the better answer than "something broke", and the message is the
      // part that was missing from it.
      recorded = { ...recorded, detail: message }
    }
    setState('error')
  }

  function setAudio(track: MediaTrack): boolean {
    // The buffers are added when the source opens; a switch asked for before then has no buffer
    // to switch, and the track the probe chose is already the one the open will play. An ended
    // source is a source with buffers: the switch's remove or append reopens it.
    if (destroyed || audioStream === null) return false
    if (source.readyState !== 'open' && source.readyState !== 'ended') return false
    const mime = probe.mimeFor(track)
    if (mime === null) return false
    // The audio track changes on the buffer it already has: a `SourceBuffer` cannot change its
    // mime in place, but it can `changeType`, and a second `addSourceBuffer` after media data has
    // arrived is the browser's `QuotaExceededError`. The new body's `beginStream` stops the old
    // fetch and `abort()`s the buffer, then cuts it from the new body's start to the end and
    // re-types it (`switchType`), then reads the new track. The switch is pending until the
    // `changeType` has happened: a seek that interrupts it restarts the audio on the new track,
    // which is the one the player shows as chosen.
    const stream = audioStream
    stream.track = track
    stream.pendingType = mime
    const fromMs = mapTimes.toSource(video.currentTime)
    // After an error every stream is stopped: the switch is a full restart, which re-arms the
    // error's stop and fetches the new track along with the video.
    if (state === 'error') {
      restartAll(fromMs)
      return true
    }
    // After the end, the new track is read to the end again: the video keeps its end, and the
    // source is ended again when the new audio reaches it.
    if (endedFired) endedFired = false
    beginStream(stream, fromMs)
    return true
  }

  function destroy(): void {
    if (destroyed) return
    destroyed = true
    for (const stream of streams) {
      abortStream(stream)
      // A `sourcebufferended`/`error` the buffer had already queued still finds this listener
      // after the teardown: take it off, and the dead engine reports nothing it does not own.
      stream.buffer.removeEventListener('error', stream.errorListener)
      try {
        stream.buffer.abort()
      } catch {
        // A buffer with nothing in it has nothing to clean.
      }
    }
    source.removeEventListener('sourceopen', onOpen)
    video.removeEventListener('timeupdate', onTimeUpdate)
    video.removeEventListener('seeking', onSeeking)
    video.removeEventListener('error', onElementError)
    if (objectUrl !== null) {
      URL.revokeObjectURL(objectUrl)
      objectUrl = null
    }
  }

  // `sourceopen` fires again whenever an append or a remove reopens an ended source (a seek back
  // after the end, an audio switch after it): the setup is the first open's alone, and a second
  // `addSourceBuffer` after media data is the browser's `QuotaExceededError`.
  let setUp = false
  function onOpen(): void {
    if (setUp) return
    setUp = true
    if (probe.durationMs !== null) source.duration = mapTimes.toMedia(probe.durationMs)
    openBuffers()
    // A buffer the source refused is the end: the failure is recorded, the state is `error`, and
    // starting a fetch into a stopped engine would only run against it.
    if (!buffersOpen || recorded !== null) return
    // The start is where the player asked to begin, when it did before the source opened; that is
    // the remembered position, and it is where the streams will be asked for.
    const startMs = pendingStartMs ?? 0
    pendingStartMs = null
    if (audioStream !== null) {
      openAt(videoStream, audioStream, startMs)
    } else {
      openAt(videoStream, null, startMs)
    }
  }
  source.addEventListener('sourceopen', onOpen)
  video.addEventListener('timeupdate', onTimeUpdate)
  video.addEventListener('seeking', onSeeking)
  video.addEventListener('error', onElementError)

  return {
    seek(sourceMs: number): void {
      if (destroyed) return
      // Before the source opens there is no stream to restart: the start is held for `onOpen`.
      if (source.readyState !== 'open') {
        pendingStartMs = sourceMs
        return
      }
      video.currentTime = mapTimes.toMedia(sourceMs)
    },
    setAudio,
    state: () => state,
    failure: (): EngineFailure | null => recorded,
    destroy,
    onState(cb: (s: EngineState) => void): void {
      listeners.push(cb)
    },
  }
}
