/**
 * The browser objects `Player.test.tsx` stands in for, said once for all of it.
 *
 * The engine is written against `EngineDeps`, and the tests give it fakes that answer the routes
 * the way Task 6's routes answer them: the same JSON, the same headers, the same status codes.
 * The fakes are strict where a browser is strict: a `SourceBuffer` refuses work while it is
 * updating or its source is not open, a `MediaSource` takes no second buffer after media data has
 * arrived, an aborted fetch rejects with an `AbortError`, and a `currentTime` set on the element
 * fires `seeking` asynchronously, the way the element does once its metadata has loaded.
 *
 * The fakes record what they are told - the `appendBuffer`, `remove`, `abort` and `changeType`
 * calls, the `endOfStream` calls, the object URLs made and revoked - so a test reads the engine's
 * actions back rather than guessing at them. The fake bodies are streams a test can feed one
 * chunk at a time and end on demand, which is how a stream that ends early is a thing the test
 * does rather than a thing it waits for.
 *
 * @module player/testing
 */
import type { EngineDeps } from './engine'

/** What one fake `SourceBuffer` was told, in the order it was told it. */
export interface BufferLog {
  appends: ArrayBuffer[]
  removes: Array<[number, number]>
  aborts: number
  changeTypes: string[]
  /** The appends a one-shot `QuotaExceededError` refused, so a test can read the refusal back. */
  quotas: number
  /**
   * Every `appendBuffer`, `remove`, `abort` and `changeType` the buffer took, in the order it took
   * them, one shared log, so a test reads the order of different calls back, not only their counts.
   */
  ops: Array<'append' | 'remove' | 'abort' | 'changeType'>
}

/** A `TimeRanges` over a list of ranges, the shape the engine reads. */
function rangesOf(ranges: [number, number][]): TimeRanges {
  return {
    length: ranges.length,
    start: (i: number): number => ranges[i]?.[0] ?? 0,
    end: (i: number): number => ranges[i]?.[1] ?? 0,
  }
}

/**
 * A fake `SourceBuffer`: it records its calls, keeps its ranges, and fires `updateend` async.
 * Strict where a browser is strict: `appendBuffer` and `remove` throw `InvalidStateError` while
 * the buffer is updating, `changeType` is refused while updating, and `abort` stops the current
 * append without touching the ranges a real `abort` leaves in place.
 *
 * `abort` is the MSE spec's: on a buffer that is updating it sets `updating` to false at once and
 * queues `abort` and `updateend`, and the aborted update's own `updateend` never comes; on a
 * buffer that is not updating it does nothing at all, and no event fires.
 */
export class FakeSourceBuffer extends EventTarget {
  readonly mimeType: string
  readonly log: BufferLog = { appends: [], removes: [], aborts: 0, changeTypes: [], quotas: 0, ops: [] }
  /** The mime a successful `changeType` moves the buffer to, so a test can read the switch back. */
  currentMimeType: string
  private _buffered: [number, number][] = []
  private _updating = false
  private _updateTimer: ReturnType<typeof setTimeout> | null = null
  private _updateKind: 'append' | 'remove' | 'changeType' | null = null
  private appendBufferOriginal: (data: ArrayBuffer | ArrayBufferView) => void
  /** The owning source, for the `InvalidStateError` a buffer in a closed source throws. */
  owner: FakeMediaSource | null = null
  readyState: 'open' | 'ended' = 'open'

  constructor(mimeType: string) {
    super()
    this.mimeType = mimeType
    this.currentMimeType = mimeType
    this.appendBufferOriginal = (data: ArrayBuffer | ArrayBufferView): void => {
      FakeSourceBuffer.prototype.appendBuffer.call(this, data)
    }
  }

  get buffered(): TimeRanges {
    return rangesOf(this._buffered)
  }

  /** The ranges as pairs: the shape a test reads back, without the `TimeRanges` interface. */
  get ranges(): [number, number][] {
    return this._buffered.map(([s, e]) => [s, e] as [number, number])
  }

  get updating(): boolean {
    return this._updating
  }

  private _refuseWhileBusy(): void {
    if (this._updating) {
      throw new DOMException('the buffer is updating', 'InvalidStateError')
    }
  }

  appendBuffer(data: ArrayBuffer | ArrayBufferView): void {
    this._refuseWhileBusy()
    this.owner?.reopenIfEnded()
    this.log.appends.push(data as ArrayBuffer)
    this.log.ops.push('append')
    // The fake buffer grows by a whole second per chunk: a test can reason about how far ahead
    // the buffer reaches without decoding what was appended.
    const last = this._buffered.at(-1)
    if (last !== undefined) last[1] += 1
    else this._buffered.push([0, 1])
    this._beginUpdate('append')
  }

  remove(start: number, end: number): void {
    this._refuseWhileBusy()
    const duration = this.owner?.duration ?? Number.POSITIVE_INFINITY
    if (!(start >= 0) || start > duration || !(end > start)) {
      throw new TypeError(`remove(${start}, ${end}) is out of range`)
    }
    this.owner?.reopenIfEnded()
    this.log.removes.push([start, end])
    this.log.ops.push('remove')
    this._buffered = this._buffered.filter(([s, e]) => e <= start || s >= end)
    this._beginUpdate('remove')
  }

  abort(): void {
    // The spec refuses an abort on a source that is not open, and during a range removal (the
    // removal runs to its own `updateend`).
    if (this.owner !== null && this.owner.readyState !== 'open') {
      throw new DOMException('the media source is not open', 'InvalidStateError')
    }
    if (this._updating && this._updateKind === 'remove') {
      throw new DOMException('the buffer is removing a range', 'InvalidStateError')
    }
    this.log.aborts += 1
    this.log.ops.push('abort')
    // A real `abort()` only stops the current update: the buffered ranges stay. On an idle buffer
    // it does nothing. On a busy one, `updating` is false when the call returns, the aborted
    // update never fires its own `updateend`, and `abort` then `updateend` are queued as a task.
    if (!this._updating) return
    if (this._updateTimer !== null) clearTimeout(this._updateTimer)
    this._updating = false
    this._updateTimer = null
    this._updateKind = null
    setTimeout(() => {
      this.dispatchEvent(new Event('abort'))
      this.dispatchEvent(new Event('updateend'))
    }, 0)
  }

  changeType(mime: string): void {
    this._refuseWhileBusy()
    this.owner?.reopenIfEnded()
    this.log.changeTypes.push(mime)
    this.log.ops.push('changeType')
    this.currentMimeType = mime
    this._beginUpdate('changeType')
  }

  private _beginUpdate(kind: 'append' | 'remove' | 'changeType'): void {
    this._updating = true
    this._updateKind = kind
    // A browser's `updateend` is a macrotask: the buffer is busy for a whole task, and a test
    // that appends and then appends again in the same task sees the `InvalidStateError` a browser
    // would throw, the way a missing wait for `updateend` shows up.
    this._updateTimer = setTimeout(() => {
      this._updateTimer = null
      this._updating = false
      this._updateKind = null
      this.dispatchEvent(new Event('updateend'))
    }, 0)
  }

  /**
   * A `QuotaExceededError` on the next `count` appends: the browser's quota is a property of the
   * buffer's contents, and a test drives the engine's quota branch by making the next appends
   * refuse, the way a full buffer would. With `count` of 2 the eviction's re-append is refused
   * too, which is the way a buffer that cannot make room for the chunk refuses it a second time.
   */
  makeNextAppendThrowQuota(count = 1): void {
    // Each refusal is recorded and the chunk is not appended; the following appends go through,
    // and the buffer is not left busy: in a browser the refused append never started an update.
    let left = count
    this.appendBuffer = (data: ArrayBuffer | ArrayBufferView): void => {
      if (left > 0) {
        left -= 1
        this.log.quotas += 1
        throw new DOMException('quota', 'QuotaExceededError')
      }
      this.appendBufferOriginal(data)
    }
  }
}

/**
 * A fake media source: it opens async, records its buffers, and counts its `endOfStream` calls.
 * Strict where a browser is strict: `addSourceBuffer` throws `InvalidStateError` while the source
 * is not open, and `QuotaExceededError` once any buffer has had an append, the behaviour
 * Chromium keeps - no new buffer after media data has arrived.
 */
export class FakeMediaSource extends EventTarget {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  static isTypeSupported = (_mime: string): boolean => true
  readonly activeSourceBuffers: FakeSourceBuffer[] = []
  readonly createdUrls: string[] = []
  private _duration = 0
  private _state: 'closed' | 'open' | 'ended' = 'closed'
  endOfStreamCalls: string[] = []

  get duration(): number {
    return this._duration
  }

  set duration(value: number) {
    this._duration = value
  }

  get readyState(): string {
    return this._state
  }

  /**
   * The MSE spec's step for an append or a remove on an ended source: the source is `open`
   * again at once, and `sourceopen` is queued as a task, so a page that set itself up in
   * `sourceopen` sees that event a second time.
   */
  reopenIfEnded(): void {
    if (this._state !== 'ended') return
    this._state = 'open'
    setTimeout(() => this.dispatchEvent(new Event('sourceopen')), 0)
  }

  private _anyAppend(): boolean {
    return this.activeSourceBuffers.some((buffer) => buffer.log.appends.length > 0)
  }

  addSourceBuffer(mime: string): FakeSourceBuffer {
    if (this.readyState !== 'open') {
      throw new DOMException('the media source is not open', 'InvalidStateError')
    }
    if (this._anyAppend()) {
      throw new DOMException('no new source buffer after media data has arrived', 'QuotaExceededError')
    }
    const buffer = new FakeSourceBuffer(mime)
    buffer.owner = this
    this.activeSourceBuffers.push(buffer)
    return buffer
  }

  removeSourceBuffer(buffer: FakeSourceBuffer): void {
    const index = this.activeSourceBuffers.indexOf(buffer)
    if (index !== -1) this.activeSourceBuffers.splice(index, 1)
  }

  endOfStream(mode?: string): void {
    // The spec refuses an end on a source that is not open or a buffer that is updating.
    if (this._state !== 'open') {
      throw new DOMException('the media source is not open', 'InvalidStateError')
    }
    if (this.activeSourceBuffers.some((buffer) => buffer.updating)) {
      throw new DOMException('a source buffer is updating', 'InvalidStateError')
    }
    this.endOfStreamCalls.push(mode ?? 'none')
    this._state = 'ended'
  }

  /** The test's side of the element's `load`: the source says it is open. */
  open(): void {
    if (this._state !== 'closed') return
    // A browser stays `closed` until the async `sourceopen`: the `readyState` flips in the same
    // task that fires the event, not before it, so an `addSourceBuffer` in between is the
    // `InvalidStateError` a real element would throw.
    queueMicrotask(() => {
      this._state = 'open'
      this.dispatchEvent(new Event('sourceopen'))
    })
  }
}

/** What one fake `fetch` answers for a stream: its status, its start, and a body to feed. */
export interface StreamAnswer {
  status?: number
  /** The `X-Fbtt-Start-Ms` header, in source milliseconds. */
  startMs?: number
  /** A body to feed, built per call. When absent, the body is empty and ends at once. */
  body?: () => FakeBody
  /** Extra headers, for a `Retry-After` the engine honours as a minimum delay. */
  headers?: Record<string, string>
  /** A JSON body the answer carries, for the routes that answer JSON rather than a stream. */
  json?: unknown
}

/** What the fake `fetch` records for every call it answers. */
export interface FetchCall {
  url: string
  signal: AbortSignal
  method?: string
  /** The body the call was given, when the answer had one. */
  body: FakeBody | null
}

export interface FetchRecorder {
  fetch: typeof fetch
  /** Every call, in the order it was made. */
  calls: FetchCall[]
  /** Name an answer for a url, by an `includes` match. The last match wins. */
  answer(urlPart: string, answer: StreamAnswer | (() => StreamAnswer)): void
  /** The calls whose url contains the part. */
  callsFor(part: string): FetchCall[]
  /** The answers a url would get, for the assertions that read the engine's questions back. */
  answersFor(prefix: string): FetchCall[]
}

/** The `AbortError` a browser raises when a fetch, or a read of its body, is aborted. */
function abortError(): DOMException {
  return new DOMException('aborted', 'AbortError')
}

/** A body a test feeds one chunk at a time and ends on demand. */
export class FakeBody {
  private pending: Uint8Array[] = []
  private ended = false
  private reader: ReadableStreamDefaultReader<Uint8Array> | null = null
  /** Set once the body's `AbortSignal` has fired; a `read` after that rejects with the `AbortError`. */
  private aborted = false

  /** A fresh fetch of the same body clears an earlier fetch's abort and its end: the body is read
   * again, the way a browser's fetch gives a fresh body for the same stream. */
  resetAbort(): void {
    this.aborted = false
    this.ended = false
  }

  getReader(): ReadableStreamDefaultReader<Uint8Array> {
    // A body is read through the one reader its fetch gave it; a later `getReader` is the same
    // reader, the way the browser's body reader is reused.
    if (this.reader !== null) return this.reader
    this.reader = {
      read: async (): Promise<ReadableStreamReadResult<Uint8Array>> => {
        // A `read` that has not been answered yet, after the signal fired, rejects with the
        // `AbortError`, the way a browser's body read does, rather than returning the next chunk.
        if (this.aborted) throw abortError()
        // A chunk that was fed before the reader asked for it is the next read, even if the body
        // was once cancelled: the `cancel` is the old reader's end, and a fresh fetch of the same
        // body clears the abort, so the chunk is the new read's.
        if (this.pending.length > 0) return { done: false, value: this.pending.shift() as Uint8Array }
        if (this.ended) return { done: true, value: undefined }
        // A body that has neither a chunk nor an end is one the test is still deciding on; the
        // reader waits for the next `push`, `end` or abort by returning a promise the test resolves.
        return new Promise<ReadableStreamReadResult<Uint8Array>>((resolve, reject) => {
          this.waiters.push((value) => resolve(value))
          this.abortWaiters.push((error) => reject(error))
        })
      },
      cancel: async (): Promise<void> => {
        // A browser's `reader.cancel()` settles the pending `read` with `{done: true}`: the body
        // is closed, not failed. The pending reads get the end, the way the browser's do. The
        // `ended` is not set: a fresh fetch of the same body clears the abort and reads on, and
        // the `cancel` is the old reader's end, not the body's.
        while (this.waiters.length > 0) {
          this.waiters.shift()?.({ done: true, value: undefined })
        }
      },
      releaseLock: (): void => {
        this.reader = null
      },
    } as ReadableStreamDefaultReader<Uint8Array>
    return this.reader
  }

  private waiters: Array<(r: ReadableStreamReadResult<Uint8Array>) => void> = []
  private abortWaiters: Array<(e: DOMException) => void> = []

  /** The signal's side of a pending `read`: the `AbortError` the browser would raise. */
  fireAbort(): void {
    this.aborted = true
    const error = abortError()
    while (this.abortWaiters.length > 0) {
      this.abortWaiters.shift()?.(error)
    }
  }

  /** Feed the next chunk the body will give. A reader already waiting takes it at once. */
  push(chunk: Uint8Array | string): void {
    if (this.aborted) return
    const bytes = typeof chunk === 'string' ? new TextEncoder().encode(chunk) : chunk
    const waiter = this.waiters.shift()
    if (waiter !== undefined) {
      waiter({ done: false, value: bytes })
      return
    }
    this.pending.push(bytes)
  }

  /** End the body: the next `read`, and any reader already waiting, is `done`. */
  end(): void {
    if (this.aborted) return
    this.ended = true
    while (this.waiters.length > 0) {
      this.waiters.shift()?.({ done: true, value: undefined })
    }
  }
}

/**
 * A `Response` the engine reads the way a browser's `fetch` answer is read: `status`, `ok`, and
 * the `body` it `getReader()`s. It is not a real `Response` because a `Response` over a
 * `ReadableStream` would hand back the stream's own reader, and that reader swallows the
 * `AbortError` the engine must see at the `read()`. Instead the body is the fake's own reader, the
 * one that rejects with the `AbortError` once the signal fires.
 */
class FakeResponse {
  readonly status: number
  private readonly bodyReader: ReadableStreamDefaultReader<Uint8Array> | null
  private readonly bodyHeaders: Headers
  private readonly bodyText: string

  constructor(
    bodyReader: ReadableStreamDefaultReader<Uint8Array> | null,
    status: number,
    bodyHeaders: Headers,
    bodyText = '',
  ) {
    this.bodyReader = bodyReader
    this.status = status
    this.bodyHeaders = bodyHeaders
    this.bodyText = bodyText
  }

  get ok(): boolean {
    return this.status >= 200 && this.status < 300
  }

  get headers(): Headers {
    return this.bodyHeaders
  }

  get body(): ReadableStream<Uint8Array> | null {
    if (this.bodyReader === null) return null
    // The shape `getReader` needs: it returns the reader, and a second call hands back the same
    // locked reader, the way the browser's does.
    const reader = this.bodyReader
    return {
      getReader: (): ReadableStreamDefaultReader<Uint8Array> => reader,
    } as unknown as ReadableStream<Uint8Array>
  }

  json(): Promise<unknown> {
    return Promise.resolve(JSON.parse(this.bodyText))
  }

  text(): Promise<string> {
    return Promise.resolve(this.bodyText)
  }
}

/**
 * The fake `fetch` the tests hand the engine and the client: it answers the named urls with the
 * named answers, records every call with its `AbortSignal` and its body, and gives an unnamed url
 * a JSON 200 with an empty body by default.
 *
 * It honours `AbortSignal` the way a browser does: aborting rejects a fetch that is still waiting
 * on its answer, or fires the `AbortError` the body already handed out will raise on its next
 * `read()`, so a read pending when the signal fires rejects with it. The 503 and 504 answers
 * carry the exact bodies Task 6's routes give.
 */
export function fakeFetch(): FetchRecorder {
  const calls: FetchCall[] = []
  const answers: Array<{ prefix: string; answer: StreamAnswer | (() => StreamAnswer) }> = []

  function resolve(url: string): StreamAnswer {
    for (let i = answers.length - 1; i >= 0; i -= 1) {
      // A `includes` match, not a `startsWith`: the test names the route by its tail
      // (`/tracks/1/stream`), and the full url carries the torrent and file ahead of it.
      if (url.includes(answers[i].prefix)) {
        const entry = answers[i].answer
        return typeof entry === 'function' ? entry() : entry
      }
    }
    return { status: 200 }
  }

  const fetch = (
    input: RequestInfo | URL,
    init?: RequestInit,
  ): Promise<Response> => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
    const signal = init?.signal ?? new AbortController().signal
    const answer = resolve(url)
    const status = answer.status ?? 200
    const headers = new Headers()
    if (answer.startMs !== undefined) headers.set('x-fbtt-start-ms', String(answer.startMs))
    for (const [name, value] of Object.entries(answer.headers ?? {})) headers.set(name, value)

    const body = status === 200 && answer.body !== undefined ? answer.body() : null
    calls.push({ url, signal, method: init?.method, body })

    // A 504 or 503 answers with the body its route names, not a stream.
    if (status === 504 || status === 503) {
      const code = status === 504 ? 'read_timeout' : 'too_many_media_streams'
      const text = JSON.stringify({ error: code })
      const response = new FakeResponse(null, status, headers, text) as unknown as Response
      return Promise.resolve(response)
    }

    // The engine reads the body it is given. When the signal fires, the body rejects its pending
    // reads with the `AbortError` the browser would raise. The `Response` is stood in for so the
    // body the engine reads is the fake's own reader: a `Response` over a `ReadableStream` hands
    // back a `ReadableStream`'s reader, which would swallow the `AbortError` the engine must see
    // at the `read()`.
    let bodyReader: ReadableStreamDefaultReader<Uint8Array> | null = null
    if (body !== null) {
      // A fresh body for this fetch: any abort from an earlier fetch of the same body is gone, so
      // this fetch's signal is the only one that can reject its reads.
      body.resetAbort()
      bodyReader = body.getReader()
      if (signal.aborted) body.fireAbort()
      else signal.addEventListener('abort', () => body.fireAbort(), { once: true })
    }

    const defaultText =
      answer.json !== undefined
        ? JSON.stringify(answer.json)
        : status === 200 && answer.body === undefined
          ? JSON.stringify({})
          : ''
    const response: Response =
      bodyReader === null
        ? (new FakeResponse(null, status, headers, defaultText) as unknown as Response)
        : (new FakeResponse(bodyReader, status, headers) as unknown as Response)

    // An abort before the answer lands rejects the fetch with the `AbortError`.
    if (signal.aborted) {
      return Promise.reject(abortError())
    }
    return new Promise<Response>((resolveFetch, rejectFetch) => {
      const onAbort = (): void => rejectFetch(abortError())
      signal.addEventListener('abort', onAbort, { once: true })
      Promise.resolve(response).then(
        (ok) => {
          signal.removeEventListener('abort', onAbort)
          resolveFetch(ok)
        },
        (cause: unknown) => {
          signal.removeEventListener('abort', onAbort)
          rejectFetch(cause)
        },
      )
    })
  }
  // The engine and the client both call `fetch` as the browser's own; this is that shape.
  const fetchFn = fetch as unknown as typeof fetch

  return {
    fetch: fetchFn,
    calls,
    answer(urlPart: string, answer: StreamAnswer | (() => StreamAnswer)): void {
      answers.push({ prefix: urlPart, answer })
    },
    callsFor(part: string): FetchCall[] {
      return calls.filter((call) => call.url.includes(part))
    },
    answersFor(part: string): FetchCall[] {
      return calls.filter((call) => call.url.includes(part))
    },
  }
}

/**
 * The `EngineDeps` a test hands the engine: a fake media source and a fake `fetch`. The
 * `MediaSource` is a constructor that returns the given source, so the engine's `new
 * MediaSource()` is the same instance the test opens and reads back.
 */
export function fakeDeps(source: FakeMediaSource, recorder: FetchRecorder, managed = false): EngineDeps {
  // A constructor that returns the given source: `new` hands back the object the constructor
  // returns, so the engine's instance is the one the test opens and reads back.
  const Ctor = function (this: unknown) {
    return source
  } as unknown as typeof MediaSource
  return {
    fetch: recorder.fetch,
    MediaSource: Ctor,
    managed,
  }
}

/**
 * Make a video element's play state drivable in a test: jsdom's `play` and `pause` are not
 * implemented, and `paused` is getter-only in the lib, so a test that drives the play state gets a
 * stored flag and ones that move it, the way a browser's do.
 */
export function makeVideoPlayable(video: HTMLVideoElement): void {
  let isPaused = video.paused
  const setPaused = (value: boolean): void => {
    isPaused = value
  }
  Object.defineProperty(video, 'paused', {
    get: () => isPaused,
    set: setPaused,
    configurable: true,
  })
  video.play = () => {
    setPaused(false)
    return Promise.resolve()
  }
  video.pause = () => {
    setPaused(true)
  }
}

/**
 * A video element's side of the engine's contract, as a test builds it.
 *
 * It behaves the way a browser's element does for the one field the engine writes: setting
 * `currentTime` fires `seeking` asynchronously, once the element has loaded its metadata. jsdom's
 * `currentTime` is a plain field, so it is stood in for with a setter that fires the event.
 */
export function fakeVideo(): HTMLVideoElement {
  const video = document.createElement('video')
  makeVideoPlayable(video)
  let current = 0
  Object.defineProperty(video, 'currentTime', {
    get: (): number => current,
    set: (value: number): void => {
      current = value
      // A browser fires `seeking` asynchronously once metadata is loaded; the element here has no
      // metadata to wait for, so the event goes out on the next microtask, the way the browser's
      // does relative to the write.
      queueMicrotask(() => video.dispatchEvent(new Event('seeking')))
    },
    configurable: true,
  })
  return video
}
