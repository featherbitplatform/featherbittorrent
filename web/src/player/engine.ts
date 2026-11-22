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
  onState(cb: (state: EngineState) => void): void
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
  /** The read gate, one per stream: the two tracks of one file each keep their own answer. */
  gate: (bufferedAheadSeconds: number) => boolean
  abort: AbortController | null
  reader: ReadableStreamDefaultReader<Uint8Array> | null
  /** True while one `appendBuffer` is on its way to `updateend`. */
  appending: boolean
  /** True while the read loop is between reads, which is to say: running. */
  reading: boolean
  /** The point of source time the current body begins at, from `X-Fbtt-Start-Ms`, or null for its start. */
  bodyStartMs: number | null
  /** True once the body has been read to its end. */
  bodyDone: boolean
  /** How many times this body has ended early and been asked again. */
  retries: number
  /** True while a retry is waiting on its timer: one timer per stream, never two. */
  retrying: boolean
  retryTimer: ReturnType<typeof setTimeout> | null
  /** The read pump: set on the stream once the function that closes over it is defined. */
  pump?: () => void
}

/** The shape the fake and the real `SourceBuffer` both give the engine. */
interface BufferLike extends EventTarget {
  buffered: { length: number; start(i: number): number; end(i: number): number }
  appendBuffer(data: ArrayBuffer | ArrayBufferView): void
  remove(start: number, end: number): void
  abort(): void
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
  return {
    fetch: window.fetch,
    MediaSource: mediaCtor,
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

  function setState(next: EngineState): void {
    if (next === state) return
    state = next
    for (const listener of listeners) listener(next)
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

  const videoStream = addStream(probe.video, probe.videoMime)
  const audioStream =
    probe.audio !== null && probe.audioMime !== null ? addStream(probe.audio, probe.audioMime) : null

  function addStream(track: MediaTrack, mime: string): Stream {
    // The source the engine holds is its own structural read; the buffer it returns is the same.
    const stream: Stream = {
      track,
      buffer: source.addSourceBuffer(mime) as unknown as Stream['buffer'],
      gate: createReadGate(),
      abort: null,
      reader: null,
      appending: false,
      reading: false,
      bodyStartMs: null,
      bodyDone: false,
      retries: 0,
      retrying: false,
      retryTimer: null,
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
    beginStream(primary, startMs, (bodyStartMs) => {
      // The playhead is the server's own word for where this body began, in source milliseconds.
      video.currentTime = mapTimes.toMedia(bodyStartMs ?? startMs)
      // The follow is asked for at the same point the primary landed on.
      if (follow !== null) beginStream(follow, bodyStartMs ?? startMs)
      if (state === 'loading') setState('playing')
    })
  }

  function beginStream(stream: Stream, startMs: number, onStarted?: (bodyStartMs: number | null) => void): void {
    if (destroyed) return
    abortStream(stream)
    clearRetry(stream)
    stream.bodyDone = false
    stream.bodyStartMs = null
    stream.retries = 0

    const abort = new AbortController()
    stream.abort = abort
    const url = streamUrl(stream.track.id, startMs)

    void (async () => {
      let response: Response
      try {
        response = await full.fetch(url, { signal: abort.signal, credentials: 'same-origin' })
      } catch {
        // Aborted is the engine's own word; a fetch that throws any other way is a network error,
        // and it gets the waiting state and its retries.
        if (destroyed || abort.signal.aborted) return
        setState('waiting')
        scheduleRetry(stream)
        return
      }
      if (destroyed || abort.signal.aborted) return
      if (!response.ok) {
        // A 504 is a read timeout: the data is there, the disk is not answering yet, and it is a
        // wait, not an error. Anything else is an answer the engine cannot act on.
        if (response.status === 504) {
          setState('waiting')
          scheduleRetry(stream)
          return
        }
        setState('error')
        return
      }

      const header = response.headers?.get?.('x-fbtt-start-ms')
      const parsed = header === null || header === undefined ? Number.NaN : Number(header)
      stream.bodyStartMs = Number.isFinite(parsed) && parsed > 0 ? parsed : null
      onStarted?.(stream.bodyStartMs)

      if (response.body === null) {
        stream.bodyDone = true
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
      if (destroyed) return
      if (stream.reader === null) return
      let result: ReadableStreamReadResult<Uint8Array>
      try {
        result = await stream.reader.read()
      } catch {
        // A body that dies mid-read is the same as one that ends early: waiting, with retries.
        if (destroyed || (stream.abort !== null && stream.abort.signal.aborted)) return
        setState('waiting')
        scheduleRetry(stream)
        return
      }
      if (stream.abort !== null && stream.abort.signal.aborted) return

      if (result.done) {
        stream.reader = null
        stream.reading = false
        stream.bodyDone = true
        settleEnd(stream)
        return
      }

      // One chunk at a time, in order, and a wait for `updateend` before the next: a
      // `SourceBuffer` is not a queue, and two appends in flight is an error at best.
      const chunk = result.value
      appendChunk(stream, chunk.buffer as ArrayBuffer, chunk.byteOffset, chunk.byteLength)
      if (destroyed || (stream.abort !== null && stream.abort.signal.aborted)) return
      await appendDone(stream)
    }
  }

  function appendChunk(stream: Stream, buffer: ArrayBuffer, byteOffset: number, byteLength: number): void {
    const data: ArrayBuffer = buffer.slice(byteOffset, byteOffset + byteLength)
    stream.appending = true
    try {
      stream.buffer.appendBuffer(data)
    } catch (cause) {
      stream.appending = false
      if (isQuotaError(cause)) {
        // The buffer is full: cut what lies far behind the playhead and try the chunk once more.
        // `evictRanges` can answer a range whose end is not past its start, and a `remove` of
        // nothing is a `remove` of nothing: those are skipped.
        const cut = evictRanges(ranges(stream.buffer.buffered), video.currentTime).filter(
          ([start, end]) => end > start,
        )
        for (const [start, end] of cut) {
          try {
            stream.buffer.remove(start, end)
          } catch {
            // A remove the buffer refuses is data it already let go.
          }
        }
        // The pending `remove`s settle on their own `updateend`, and the retry of the chunk waits
        // for that: an `appendBuffer` before the buffer is clean throws.
        stream.retryTimer = setTimeout(() => {
          stream.retryTimer = null
          if (destroyed) return
          stream.appending = true
          try {
            stream.buffer.appendBuffer(data)
          } catch {
            // Still full after one eviction: the waiting state and the retries take it from here.
            stream.appending = false
            setState('waiting')
            scheduleRetry(stream)
          }
        }, 0)
        return
      }
      throw cause
    }
  }

  /** A read loop that is stopped by its gate, woken by a `timeupdate` moving the playhead. */
  function pump(stream: Stream): void {
    if (destroyed || stream.reading || stream.appending || stream.reader === null) return
    // The gate answers from how far the buffer already reaches: a buffer a minute ahead of the
    // playhead is not one the person is watching, and its fetch stops.
    const ahead = bufferEnd(stream.buffer.buffered) - video.currentTime
    if (!stream.gate(ahead)) return
    stream.reading = true
    void readStream(stream)
  }

  function appendDone(stream: Stream): Promise<void> {
    return new Promise((resolve) => {
      const check = (): void => {
        if (!stream.appending) resolve()
        else stream.buffer.addEventListener('updateend', check, { once: true })
      }
      check()
    })
  }

  function settleEnd(stream: Stream): void {
    if (destroyed || endedFired) return
    const end = probe.durationMs === null ? null : mapTimes.toMedia(probe.durationMs)
    if (end !== null) {
      const allDone = streams.every((other) => other.bodyDone || other.retrying)
      const allReached = streams.every((other) => bufferEnd(other.buffer.buffered) >= end - END_WITHIN)
      if (allDone && allReached) {
        endedFired = true
        try {
          source.endOfStream()
        } catch {
          // A source that is already done is the best outcome the call could have.
        }
        return
      }
    }
    // The body ended but the file is not over, or the end cannot be measured: the stream stopped
    // early, and it waits to be asked again.
    setState('waiting')
    scheduleRetry(stream)
  }

  function scheduleRetry(stream: Stream): void {
    if (destroyed || stream.retrying) return
    stream.retrying = true
    const delay = stream.retries < RETRY_DELAYS.length ? RETRY_DELAYS[stream.retries] : RETRY_STEADY
    stream.retries += 1
    stream.retryTimer = setTimeout(() => {
      stream.retryTimer = null
      stream.retrying = false
      if (destroyed) return
      // The retry goes from the end of what the buffer already has, not from where the fetch
      // began: the data that made it in is kept, and the server resumes where the buffer stops.
      const fromMs = mapTimes.toSource(bufferEnd(stream.buffer.buffered))
      beginStream(stream, fromMs)
    }, delay)
  }

  function clearRetry(stream: Stream): void {
    if (stream.retryTimer !== null) {
      clearTimeout(stream.retryTimer)
      stream.retryTimer = null
    }
    stream.retrying = false
  }

  function abortStream(stream: Stream): void {
    clearRetry(stream)
    stream.reading = false
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
  }

  function restartAll(fromMs: number): void {
    // Every stream stops where it is and starts again at the same point of source time: the
    // buffers are cut, the fetches aborted, and the playhead is where the person put it.
    for (const stream of streams) {
      abortStream(stream)
      try {
        stream.buffer.abort()
      } catch {
        // Nothing in the buffer, or a buffer that is already clean: the seek takes it from here.
      }
    }
    if (audioStream !== null) {
      // The playhead follows the video; the audio is asked for at the same point it lands on.
      openAt(videoStream, audioStream, fromMs)
    } else {
      openAt(videoStream, null, fromMs)
    }
  }

  // A seek the buffer already covers costs nothing: the element plays from what it has. A seek
  // that does not is a restart of both streams at the point the element moved to.
  function onSeeking(): void {
    if (destroyed || endedFired) return
    const time = video.currentTime
    if (time < 0) return
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
    if (destroyed || audioStream === null) return
    const mime = probe.mimeFor(track)
    if (mime === null) return
    // The old audio is cut at the playhead, not at its start: what has played stays, what has not
    // goes, and the new track is asked for at the point the buffer now covers.
    abortStream(audioStream)
    const cutFrom = video.currentTime
    const buffered = audioStream.buffer.buffered
    for (let i = buffered.length - 1; i >= 0; i -= 1) {
      const start = buffered.start(i)
      const end = buffered.end(i)
      if (end <= cutFrom) continue
      try {
        audioStream.buffer.remove(Math.max(start, cutFrom), end)
      } catch {
        // A remove the buffer refuses is data it already let go.
      }
    }
    // The new track gets a new buffer: a `SourceBuffer` cannot change the mime it was made with.
    const old = audioStream
    const index = streams.indexOf(old)
    try {
      source.removeSourceBuffer(old.buffer)
    } catch {
      // A buffer that will not be removed is one that is already gone.
    }
    const fresh = addStream(track, mime)
    streams.splice(index, 1, fresh)
    beginStream(fresh, mapTimes.toSource(cutFrom))
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
    openAt(videoStream, audioStream, 0)
  }
  source.addEventListener('sourceopen', onOpen)
  video.addEventListener('timeupdate', onTimeUpdate)
  video.addEventListener('seeking', onSeeking)

  return {
    seek(sourceMs: number): void {
      if (destroyed || endedFired) return
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
