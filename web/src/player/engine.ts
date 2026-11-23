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
}

/** The surface the player drives: seek, change the audio, read the state, go away. */
export interface Engine {
  /** Move the playhead to a point of source time. Outside the buffer, the streams restart there. */
  seek(sourceMs: number): void
  /** Change the audio track: the old one is cut at the playhead, the new one is fetched there. */
  setAudio(track: MediaTrack): void
  /** The state the player draws. */
  state(): EngineState
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
  buffered: { length: number; start(i: number): number; end(i: number): number }
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
    fetch: window.fetch,
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
  // The point the engine itself put the playhead at, so the `seeking` that write fires is not
  // answered with a restart of the streams it was just asking for.
  let engineSeekTo: number | null = null
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
    }
    // One `updateend` ends the append the buffer was busy with, and it is also where a stopped
    // read finds out there is room to go again.
    stream.buffer.addEventListener('updateend', () => {
      stream.appending = false
      stream.pump?.()
    })
    streams.push(stream)
    // The pump closes over `readStream` and the rest, so it is handed to the stream once they are
    // all in scope: the `updateend` above and the `timeupdate` below both call it through this field.
    stream.pump = () => pump(stream)
    return stream
  }

  // The buffers are added when the source opens, not when the engine is made: a `SourceBuffer`
  // refused by a source that is not yet open is the browser's `InvalidStateError`, and the order
  // the decoder wants them in is the video first, the audio after.
  function openBuffers(): void {
    videoStream = addStream(probe.video, probe.videoMime, 'video')
    audioStream =
      probe.audio !== null && probe.audioMime !== null
        ? addStream(probe.audio, probe.audioMime, 'audio')
        : null
  }

  function streamUrl(trackId: number, startMs: number): string {
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
      engineSeekTo = mapTimes.toMedia(startMsLanded)
      video.currentTime = engineSeekTo
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
      // An audio switch that has not reached its `changeType` yet is finished here, before the
      // new track's first byte: the switch's own body, or a restart that interrupted it.
      if (stream.pendingType !== null) {
        const switched = await switchType(stream, stream.pendingType, mapTimes.toMedia(startMs), abort.signal)
        if (!switched) return
      }
      let response: Response
      try {
        response = await full.fetch(url, { signal: abort.signal, credentials: 'same-origin' })
      } catch (cause) {
        // The engine's own abort is not a failure: the stream is being replaced, and the old
        // fetch is the way the engine said so.
        if (destroyed || abort.signal.aborted || isAbort(cause)) return
        // A network error is a wait, not an end: the state, and its retries. A first fetch of
        // the video that fails re-runs the whole start, so the audio is asked for too.
        setState('waiting')
        if (stream.role === 'video') scheduleRetry(stream, () => restartAll(startMs))
        else scheduleRetry(stream)
        return
      }
      if (destroyed || abort.signal.aborted) return
      if (!response.ok) {
        // A 504 is a read timeout and a 503 a full set of streams: both are waits, not errors,
        // and a 503's `Retry-After` is the shortest the wait may be.
        if (response.status === 504 || response.status === 503) {
          const retryAfterMs = parseRetryAfterMs(response)
          setState('waiting')
          if (stream.role === 'video') {
            scheduleRetry(stream, () => restartAll(startMs), retryAfterMs ?? undefined)
          } else {
            scheduleRetry(stream, undefined, retryAfterMs ?? undefined)
          }
          return
        }
        setState('error')
        return
      }

      const header = response.headers?.get?.('x-fbtt-start-ms')
      const parsed = header === null || header === undefined ? Number.NaN : Number(header)
      const bodyStartMs = Number.isFinite(parsed) && parsed > 0 ? parsed : Math.max(0, startMs)
      stream.bodyStartMs = bodyStartMs
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
      if (Math.abs(time - target) < 1e-3) return
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

  function setAudio(track: MediaTrack): void {
    // The buffers are added when the source opens; a switch asked for before then has no buffer
    // to switch, and the track the probe chose is already the one the open will play.
    if (destroyed || audioStream === null || source.readyState !== 'open') return
    const mime = probe.mimeFor(track)
    if (mime === null) return
    // The audio track changes on the buffer it already has: a `SourceBuffer` cannot change its
    // mime in place, but it can `changeType`, and a second `addSourceBuffer` after media data has
    // arrived is the browser's `QuotaExceededError`. The new body's `beginStream` stops the old
    // fetch and `abort()`s the buffer, then cuts it from the playhead to the end and re-types it
    // (`switchType`), then asks for the new track at the playhead. The switch is pending until
    // the `changeType` has happened: a seek that interrupts it restarts the audio on the new
    // track, which is the one the player shows as chosen.
    const stream = audioStream
    stream.track = track
    stream.pendingType = mime
    beginStream(stream, mapTimes.toSource(video.currentTime))
  }

  function destroy(): void {
    if (destroyed) return
    destroyed = true
    for (const stream of streams) {
      abortStream(stream)
      try {
        stream.buffer.abort()
      } catch {
        // A buffer with nothing in it has nothing to clean.
      }
    }
    source.removeEventListener('sourceopen', onOpen)
    video.removeEventListener('timeupdate', onTimeUpdate)
    video.removeEventListener('seeking', onSeeking)
    if (objectUrl !== null) {
      URL.revokeObjectURL(objectUrl)
      objectUrl = null
    }
  }

  function onOpen(): void {
    if (probe.durationMs !== null) source.duration = mapTimes.toMedia(probe.durationMs)
    openBuffers()
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
    destroy,
    onState(cb: (s: EngineState) => void): void {
      listeners.push(cb)
    },
  }
}
