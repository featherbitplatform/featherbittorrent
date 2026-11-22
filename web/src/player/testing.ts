/**
 * The browser objects `Player.test.tsx` stands in for, said once for all of it.
 *
 * The engine is written against `EngineDeps`, and the tests give it fakes that answer the routes
 * the way Task 6's routes answer them: the same JSON, the same headers, the same status codes.
 * The fakes record what they are told - the `appendBuffer`, `remove` and `abort` calls, the
 * `endOfStream` calls, the object URLs made and revoked - so a test reads the engine's actions
 * back rather than guessing at them. The fake bodies are streams a test can feed one chunk at a
 * time and end on demand, which is how a stream that ends early is a thing the test does rather
 * than a thing it waits for.
 *
 * @module player/testing
 */
import type { EngineDeps } from './engine'

/** What one fake `SourceBuffer` was told, in the order it was told it. */
export interface BufferLog {
  appends: ArrayBuffer[]
  removes: Array<[number, number]>
  aborts: number
}

/** A `TimeRanges` over a list of ranges, the shape the engine reads. */
function rangesOf(ranges: [number, number][]): TimeRanges {
  return {
    length: ranges.length,
    start: (i: number): number => ranges[i]?.[0] ?? 0,
    end: (i: number): number => ranges[i]?.[1] ?? 0,
  }
}

/** A fake `SourceBuffer`: it records its calls, keeps its ranges, and fires `updateend` async. */
export class FakeSourceBuffer extends EventTarget {
  readonly mimeType: string
  readonly log: BufferLog = { appends: [], removes: [], aborts: 0 }
  private _buffered: [number, number][] = []
  private _updating = false
  readyState: 'open' | 'ended' = 'open'

  constructor(mimeType: string) {
    super()
    this.mimeType = mimeType
  }

  get buffered(): TimeRanges {
    return rangesOf(this._buffered)
  }

  get updating(): boolean {
    return this._updating
  }

  appendBuffer(data: ArrayBuffer | ArrayBufferView): void {
    this.log.appends.push(data as ArrayBuffer)
    // The fake buffer grows by a whole second per chunk: a test can reason about how far ahead
    // the buffer reaches without decoding what was appended.
    const last = this._buffered.at(-1)
    if (last !== undefined) last[1] += 1
    else this._buffered.push([0, 1])
    this._beginUpdate()
  }

  remove(start: number, end: number): void {
    this.log.removes.push([start, end])
    this._buffered = this._buffered.filter(([s, e]) => e <= start || s >= end)
    this._beginUpdate()
  }

  abort(): void {
    this.log.aborts += 1
    this._buffered = []
    this._beginUpdate()
  }

  private _beginUpdate(): void {
    this._updating = true
    queueMicrotask(() => {
      this._updating = false
      this.dispatchEvent(new Event('updateend'))
    })
  }
}

/** A fake media source: it opens async, records its buffers, and counts its `endOfStream` calls. */
export class FakeMediaSource extends EventTarget {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  static isTypeSupported = (_mime: string): boolean => true
  readonly activeSourceBuffers: FakeSourceBuffer[] = []
  readonly createdUrls: string[] = []
  private _duration = 0
  private _open = false
  endOfStreamCalls: string[] = []

  get duration(): number {
    return this._duration
  }

  set duration(value: number) {
    this._duration = value
  }

  get readyState(): string {
    return this._open ? 'open' : 'closed'
  }

  addSourceBuffer(mime: string): FakeSourceBuffer {
    const buffer = new FakeSourceBuffer(mime)
    this.activeSourceBuffers.push(buffer)
    return buffer
  }

  removeSourceBuffer(buffer: FakeSourceBuffer): void {
    const index = this.activeSourceBuffers.indexOf(buffer)
    if (index !== -1) this.activeSourceBuffers.splice(index, 1)
  }

  endOfStream(mode?: string): void {
    this.endOfStreamCalls.push(mode ?? 'none')
  }

  /** The test's side of the element's `load`: the source says it is open. */
  open(): void {
    if (this._open) return
    this._open = true
    queueMicrotask(() => this.dispatchEvent(new Event('sourceopen')))
  }
}

/** What one fake `fetch` answers for a stream: its status, its start, and a body to feed. */
export interface StreamAnswer {
  status?: number
  /** The `X-Fbtt-Start-Ms` header, in source milliseconds. */
  startMs?: number
  /** A body to feed, built per call. When absent, the body is empty and ends at once. */
  body?: () => FakeBody
}

/** What the fake `fetch` records for every call it answers. */
export interface FetchCall {
  url: string
  signal: AbortSignal
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

/** A body a test feeds one chunk at a time and ends on demand. */
export class FakeBody {
  private pending: Uint8Array[] = []
  private ended = false
  private reader: ReadableStreamDefaultReader<Uint8Array> | null = null

  getReader(): ReadableStreamDefaultReader<Uint8Array> {
    if (this.reader !== null) return this.reader
    this.reader = {
      read: async (): Promise<ReadableStreamReadResult<Uint8Array>> => {
        if (this.pending.length > 0) return { done: false, value: this.pending.shift() as Uint8Array }
        if (this.ended) return { done: true, value: undefined }
        // A body that has neither a chunk nor an end is one the test is still deciding on; the
        // reader waits for the next `push` or `end` by returning a promise the test resolves.
        return new Promise<ReadableStreamReadResult<Uint8Array>>((resolve) => {
          this.waiters.push(resolve)
        })
      },
      cancel: async (): Promise<void> => {
        this.ended = true
      },
      releaseLock: (): void => {
        this.reader = null
      },
    } as ReadableStreamDefaultReader<Uint8Array>
    return this.reader
  }

  private waiters: Array<(r: ReadableStreamReadResult<Uint8Array>) => void> = []

  /** Feed the next chunk the body will give. A reader already waiting takes it at once. */
  push(chunk: Uint8Array | string): void {
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
    this.ended = true
    while (this.waiters.length > 0) {
      this.waiters.shift()?.({ done: true, value: undefined })
    }
  }
}

/**
 * The fake `fetch` the tests hand the engine and the client: it answers the named urls with the
 * named answers, records every call with its `AbortSignal` and its body, and gives an unnamed url
 * a JSON 200 with an empty body by default.
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

  const fetch = async (
    input: RequestInfo | URL,
    init?: RequestInit,
  ): Promise<Response> => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
    const signal = init?.signal ?? new AbortController().signal
    const answer = resolve(url)
    const status = answer.status ?? 200
    const headers = new Headers()
    if (answer.startMs !== undefined) headers.set('x-fbtt-start-ms', String(answer.startMs))
    const body = status === 200 && answer.body !== undefined ? answer.body() : null
    calls.push({ url, signal, body })
    const stream =
      body === null
        ? null
        : new ReadableStream<Uint8Array>({
            start(controller) {
              // The chunks and the end are fed by the test through the `FakeBody`; this stream
              // only hands the engine a reader that reads from that body.
              const reader = body.getReader()
              void (async () => {
                for (;;) {
                  const result = await reader.read()
                  if (result.done) {
                    controller.close()
                    return
                  }
                  controller.enqueue(result.value)
                }
              })().catch(() => {
                controller.error(new Error('the fake body ended badly'))
              })
            },
            cancel() {
              body.getReader().cancel().catch(() => {
                // A body that will not cancel is a body that is already gone.
              })
            },
          })
    return new Response(stream, { status, headers })
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

/** A video element's side of the engine's contract, as a test builds it. */
export function fakeVideo(): HTMLVideoElement {
  const video = document.createElement('video')
  makeVideoPlayable(video)
  return video
}
