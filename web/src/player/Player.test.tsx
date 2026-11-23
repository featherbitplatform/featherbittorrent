// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import Files from '../components/detail/Files'
import { createClient } from '../api/client'
import type { Api, FileInfo, MediaProbe, MediaTrack } from '../api/client'
import { createEngine } from './engine'
import type { EngineProbe } from './engine'
import { toSource } from './time'
import { installDialogShim } from '../dialog-shim'
import Player from './Player'
import {
  FakeBody,
  FakeMediaSource,
  FakeSourceBuffer,
  fakeDeps,
  fakeFetch,
  fakeVideo,
  makeVideoPlayable,
} from './testing'


installDialogShim()

afterEach(cleanup)

// jsdom's `URL.createObjectURL` takes a `Blob` and will not take a `MediaSource`, so the engine's
// object-URL path is stood in for: it makes a `blob:` url and remembers it so a test can read it back.
const objectUrls: string[] = []
let objectUrlCount = 0
const revokeObjectUrls: string[] = []
let createObjectUrlCalls = 0
beforeEach(() => {
  objectUrlCount = 0
  createObjectUrlCalls = 0
  objectUrls.length = 0
  revokeObjectUrls.length = 0
  // jsdom has no `MediaSource`, so the player's `chooseTracks` would answer `unplayable` for
  // everything. The stub is a `FakeMediaSource` with a browser-shaped `isTypeSupported`: it takes
  // the codecs a real browser takes (h264, aac, opus, vp8, vp9) and refuses the rest (hevc), which
  // is what the unplayable test needs. It is constructible, so the engine the player makes for a
  // `remux` choice can open it and feed it.
  class BrowserMediaSource extends FakeMediaSource {
    static isTypeSupported(mime: string): boolean {
      return /avc1|mp4a|opus|vp8|vp9/.test(mime)
    }
  }
  vi.stubGlobal('MediaSource', BrowserMediaSource)
  vi.stubGlobal('URL', {
    ...URL,
    createObjectURL: () => {
      createObjectUrlCalls += 1
      const url = `blob:fake/${objectUrlCount}`
      objectUrlCount += 1
      objectUrls.push(url)
      return url
    },
    revokeObjectURL: (url: string) => {
      revokeObjectUrls.push(url)
    },
  })
  // jsdom has no `VTTCue`, and its `addTextTrack` is not implemented: the player's subtitle path
  // builds cues and adds tracks, so both are stood in for with the minimal surface the player uses.
  class FakeVTTCue {
    startTime: number
    endTime: number
    text: string
    constructor(startTime: number, endTime: number, text: string) {
      this.startTime = startTime
      this.endTime = endTime
      this.text = text
    }
  }
  vi.stubGlobal('VTTCue', FakeVTTCue)
  // jsdom's `addTextTrack` is not implemented, and its `textTracks` list is not populated by a
  // working `addTextTrack`. The shim keeps the tracks on the element and exposes them through the
  // element's `textTracks` getter, so the player's reads of `video.textTracks` see them.
  const originalGetTextTracks = Object.getOwnPropertyDescriptor(
    HTMLMediaElement.prototype,
    'textTracks',
  )
  const tracksGetter =
    originalGetTextTracks?.get ??
    (() => {
      const list: { length: number } = { length: 0 }
      return list
    })
  HTMLMediaElement.prototype.addTextTrack = function (
    this: HTMLMediaElement,
    kind: TextTrackKind,
    label: string,
    language?: string,
  ): TextTrack {
    const element = this as unknown as { _tracks?: TextTrack[] }
    const tracks = element._tracks ?? []
    const held: unknown[] = []
    const track = {
      kind,
      label,
      language: language ?? '',
      mode: 'disabled' as TextTrackMode,
      cues: held,
      addCue(cue: unknown): void {
        held.push(cue)
      },
    } as unknown as TextTrack
    // The browser's `TextTrack.id` is a getter with no setter: writing it throws, the way a
    // module (strict mode) throws on a read-only property. The fake is at least as strict, so a
    // player that keys its tracks by `id` is caught here the way a browser would catch it.
    Object.defineProperty(track, 'id', {
      get(): string {
        return ''
      },
      configurable: true,
    })
    tracks.push(track)
    element._tracks = tracks
    return track
  }
  // Expose the tracks through the element's `textTracks` getter.
  if (originalGetTextTracks !== undefined) {
    Object.defineProperty(HTMLMediaElement.prototype, 'textTracks', {
      configurable: true,
      get(): { length: number } & Record<string, TextTrack> {
        const element = this as unknown as { _tracks?: TextTrack[] }
        const tracks = element._tracks ?? []
        const list = tracksGetter.call(this) as { length: number } & Record<string, TextTrack>
        for (let i = 0; i < tracks.length; i += 1) list[i] = tracks[i]
        list.length = tracks.length
        return list
      },
    })
  }
})
afterEach(() => {
  vi.unstubAllGlobals()
})

/** One file of the fixture's shape: the probe the tests build their assertions on. */
function track(over: Partial<MediaTrack> & { id: number; kind: MediaTrack['kind'] }): MediaTrack {
  return {
    codec: 'h264',
    mime_codecs: ['avc1.64000A'],
    language: null,
    name: null,
    default: false,
    forced: false,
    text: false,
    ...over,
  }
}

/** The probe the fixtures answer: a 4 s file with a 1 s offset, five tracks, and one external subtitle. */
const PROBE: MediaProbe = {
  container: 'matroska',
  duration_ms: 4000,
  time_offset_ms: 1000,
  tracks: [
    track({ id: 1, kind: 'video', codec: 'h264', mime_codecs: ['avc1.64000A'], default: true }),
    track({ id: 2, kind: 'audio', codec: 'aac', mime_codecs: ['mp4a.40.2'], language: 'eng', default: true }),
    track({ id: 3, kind: 'audio', codec: 'opus', mime_codecs: ['opus'], language: 'ita' }),
    track({ id: 4, kind: 'subtitle', codec: 'srt', language: 'eng', text: true }),
    track({ id: 5, kind: 'subtitle', codec: 'hdmv_pgs_subtitle', language: 'ita', text: false }),
  ],
  external_subtitles: [{ file: 3, name: 'movie.ita.srt', format: 'srt' }],
}

/** A client that answers the probe and the stream token, and records the calls it is given. */
function probeClient(probe: MediaProbe = PROBE, cues: import('../api/client').Cue[] = []): {
  client: import('../api/client').Api
  tokenCalls(): number
} {
  let tokenCalls = 0
  const client = {
    probe: async () => probe,
    cues: async () => ({ cues }),
    subtitleFile: async () => ({ cues }),
    streamToken: async () => {
      tokenCalls += 1
      return '/api/v1/stream/t?token=abc'
    },
    mediaStreamUrl: () => '/api/v1/media/x',
    torrents: async () => [],
    torrent: async () => ({}),
    files: async () => [],
    peers: async () => [],
    trackers: async () => [],
    pieces: async () => ({ num_pieces: 0, have: '' }),
    session: async () => ({}),
    pause: async () => {},
    resume: async () => {},
    remove: async () => {},
    addMagnet: async () => ({}),
    addFile: async () => ({}),
    roots: async () => [],
    list: async () => ({ path: '', dirs: [] }),
  } as unknown as import('../api/client').Api
  return { client, tokenCalls: () => tokenCalls }
}

/** Let the fakes' async work - the probe, the `sourceopen`, the `updateend`, the bodies - land. */
async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0))
  })
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0))
  })
}



const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms))

/** The audio mime the player would give a track, for the engine tests that switch the audio. */
function audioMimeFor(track: MediaTrack): string | null {
  for (const codec of track.mime_codecs) {
    if (/mp4a|opus/.test(codec)) return `audio/mp4; codecs="${codec}"`
  }
  return null
}

/** The probe the engine tests hand the engine: the fixture's video and English audio. */
function engineProbe(over: Partial<EngineProbe> = {}): EngineProbe {
  return {
    offsetMs: 1000,
    durationMs: 4000,
    video: PROBE.tracks[0] as MediaTrack,
    videoMime: 'video/mp4; codecs="avc1.64000A"',
    audio: PROBE.tracks[1] as MediaTrack,
    audioMime: 'audio/mp4; codecs="mp4a.40.2"',
    mimeFor: audioMimeFor,
    ...over,
  }
}

/** Set a fake buffer's ranges by hand, the way a test puts the buffer in a shape it needs. */
function setRanges(buffer: FakeSourceBuffer, ranges: [number, number][]): void {
  ;(buffer as unknown as { _buffered: [number, number][] })._buffered = ranges
}

/**
 * An engine over a fresh source, video element and fetch. The video's first fetch answers at 0
 * and every later one at `restartMs`, so a seek's restart lands where the test asked; each fetch
 * gets a fresh body, read back from `recorder.callsFor`.
 */
async function openEngine(over: Partial<EngineProbe> = {}, restartMs = 19_000) {
  const source = new FakeMediaSource()
  const recorder = fakeFetch()
  let videoCalls = 0
  recorder.answer('/tracks/1/stream', () => {
    videoCalls += 1
    return { startMs: videoCalls === 1 ? 0 : restartMs, body: () => new FakeBody() }
  })
  recorder.answer('/tracks/2/stream', () => ({ startMs: 0, body: () => new FakeBody() }))
  recorder.answer('/tracks/3/stream', () => ({ startMs: 0, body: () => new FakeBody() }))
  const video = fakeVideo()
  document.body.appendChild(video)
  const engine = createEngine(video, { id: 'a', file: 0 }, engineProbe(over), fakeDeps(source, recorder))
  source.open()
  await settle()
  const body = (track: number, index = -1): FakeBody => {
    const call = recorder.callsFor(`/tracks/${track}/stream`).at(index)
    if (call?.body === undefined || call.body === null) throw new Error(`no body for track ${track}`)
    return call.body
  }
  const close = (): void => {
    engine.destroy()
    document.body.removeChild(video)
  }
  return {
    source,
    recorder,
    video,
    engine,
    videoBuffer: source.activeSourceBuffers[0] as FakeSourceBuffer,
    audioBuffer: source.activeSourceBuffers[1] as FakeSourceBuffer,
    body,
    close,
  }
}

function fileInfo(index: number, path: string): FileInfo {
  return { index, path, len: 1_048_576, downloaded: 524_288, priority: 'normal', pad: false }
}

describe('the Files tab', () => {
  it('opens from the Files tab', () => {
    const files = [fileInfo(0, 'movie.mkv'), fileInfo(1, 'notes.txt')]
    let played = -1
    render(
      <Files
        files={files}
        viewportHeight={200}
        id="a"
        onPlay={(index) => {
          played = index
        }}
      />,
    )
    const mkvRow = screen.getByText('movie.mkv').closest('tr')
    const txtRow = screen.getByText('notes.txt').closest('tr')
    const mkvButton = mkvRow?.querySelector('button[aria-label="Play"]')
    expect(mkvButton !== null, 'a .mkv row has no Play button').toBe(true)
    expect(txtRow?.querySelector('button[aria-label="Play"]') === null, 'a .txt row has a Play button').toBe(true)
    fireEvent.click(mkvButton as HTMLElement)
    expect(played === 0, 'clicking Play did not open the player on the right file').toBe(true)
  })
})

describe('the player', () => {
  it('lists audio and subtitle choices', async () => {
    const { client } = probeClient()
    render(<Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />)
    await settle()

    const audio = screen.getByRole('button', { name: 'Audio' })
    fireEvent.click(audio)
    await settle()
    const audioOptions = screen.getAllByRole('option')
    const audioTexts = audioOptions.map((option) => option.textContent ?? '')
    expect(
      audioTexts.some((text) => text.startsWith('eng')) &&
        audioTexts.some((text) => text.startsWith('ita')),
      'the Audio menu did not list both the English and the Italian tracks',
    ).toBe(true)
    const listbox = screen.getByRole('listbox', { name: 'Audio' })
    fireEvent.keyDown(listbox, { key: 'Escape' })
    await settle()

    const subtitles = screen.getByRole('button', { name: 'Subtitles' })
    fireEvent.click(subtitles)
    await settle()
    const listbox2 = screen.getByRole('listbox', { name: 'Subtitles' })
    const subOptions = Array.from(listbox2.querySelectorAll('[role="option"]'))
    const subTexts = subOptions.map((option) => option.textContent ?? '')
    expect(subTexts.includes('Off'), 'the Subtitles menu has no Off entry').toBe(true)
    expect(
      subTexts.some((text) => text.startsWith('eng')) &&
        subTexts.some((text) => text.startsWith('ita')) &&
        subTexts.includes('movie.ita.srt'),
      'the Subtitles menu did not list the text tracks and the external file',
    ).toBe(true)
    const pgs = subOptions.find((option) => (option as HTMLButtonElement).disabled === true)
    expect(pgs !== undefined, 'the PGS track is not listed disabled').toBe(true)
    expect(
      (pgs?.textContent ?? '').includes('Image subtitles cannot be shown here.'),
      'the PGS track does not say why it cannot be shown',
    ).toBe(true)
  })

  it('unplayable shows the message and Download', async () => {
    const unplayable: MediaProbe = {
      ...PROBE,
      tracks: [track({ id: 1, kind: 'video', codec: 'hevc', mime_codecs: ['hvc1.1.6.L93.B8'] })],
    }
    // The player's real `streamToken`, over the fake `fetch`: the probe is answered by the
    // test's client, and the `Download` click asks the token route through the same fake, which
    // the test reads back. The token route is answered with the path the anchor follows.
    const recorder = fakeFetch()
    recorder.answer('/api/v1/stream-tokens', {
      status: 200,
      json: { path: '/api/v1/stream/x?token=abc' },
    })
    const client: Api = {
      ...(createClient(recorder.fetch, () => null, () => 0, () => {}) as Api),
      probe: async () => unplayable,
      cues: async () => ({ cues: [] }),
      subtitleFile: async () => ({ cues: [] }),
    }
    render(<Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />)
    await settle()

    expect(
      screen.getByText('This browser cannot play hevc video.') !== null,
      'the unplayable player did not say which codec it cannot play',
    ).toBe(true)
    const tokensBefore = recorder.callsFor('/api/v1/stream-tokens').length
    expect(tokensBefore, 'the token was fetched before the click').toBe(0)
    // The path the token route answers is the one the `<a download>` follows: a temporary anchor
    // is made, pointed at it, clicked, and removed, so the test reads it off the click.
    let followed = ''
    const originalClick = HTMLAnchorElement.prototype.click
    ;(HTMLAnchorElement.prototype as unknown as { click(): void }).click = function (this: HTMLAnchorElement) {
      followed = this.href
      return originalClick.call(this)
    }
    fireEvent.click(screen.getByRole('button', { name: 'Download' }))
    await settle()
    ;(HTMLAnchorElement.prototype as unknown as { click(): void }).click = originalClick
    // Exactly one `POST /api/v1/stream-tokens`, and the returned path is the one followed.
    const tokenCalls = recorder.callsFor('/api/v1/stream-tokens')
    expect(tokenCalls.length, 'Download did not ask for a stream token').toBe(1)
    expect(tokenCalls[0]?.method, 'the token was not asked with a POST').toBe('POST')
    expect(
      followed.includes('/api/v1/stream/x?token=abc'),
      `the anchor did not follow the returned path (got ${followed})`,
    ).toBe(true)
  })

  it('managed media source attaches by srcObject', async () => {
    // With `managed: true`, the source is attached by `srcObject`, `disableRemotePlayback` is
    // set, and no object URL is made. With `managed: false`, the source is an object URL.
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    recorder.answer('/api/v1/media/', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: null,
        audioMime: null,
        mimeFor: () => null,
      },
      fakeDeps(source, recorder, true),
    )
    source.open()
    await settle()

    expect(
      (video.srcObject as unknown) === source,
      'a managed source is not attached by srcObject',
    ).toBe(true)
    expect(video.disableRemotePlayback === true, 'a managed source did not disable remote playback').toBe(true)
    expect(video.src === '', 'a managed source made an object URL').toBe(true)
    expect(createObjectUrlCalls, 'a managed source made an object URL').toBe(0)
    engine.destroy()
    document.body.removeChild(video)

    // The plain source, for contrast: an object URL, no `srcObject`.
    const source2 = new FakeMediaSource()
    const recorder2 = fakeFetch()
    recorder2.answer('/api/v1/media/', { startMs: 0, body: () => new FakeBody() })
    const video2 = fakeVideo()
    document.body.appendChild(video2)
    const engine2 = createEngine(
      video2,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: null,
        audioMime: null,
        mimeFor: () => null,
      },
      fakeDeps(source2, recorder2, false),
    )
    source2.open()
    await settle()
    expect(video2.src.startsWith('blob:'), 'a plain source is not attached by an object URL').toBe(true)
    // jsdom's `srcObject` is `undefined` rather than `null` when nothing is attached, so the check
    // is that it is not the source.
    expect(
      (video2.srcObject as unknown) !== source2,
      'a plain source was attached by srcObject',
    ).toBe(true)
    expect(createObjectUrlCalls === 1, 'a plain source did not make an object URL').toBe(true)
    engine2.destroy()
    document.body.removeChild(video2)
  })

  it('duration and end of stream', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    const videoBody = new FakeBody()
    const audioBody = new FakeBody()
    recorder.answer('/tracks/1/stream', { startMs: 0, body: () => videoBody })
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => audioBody })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()

    // The duration is the probe's, mapped: the remuxed tracks are shifted forward by the offset,
    // so 4000 ms of source time is 5 s of media time (the offset's 1 s of room at the front, then
    // the 4 s of content).
    expect(source.duration === 5, 'the source did not take the probe duration').toBe(true)

    // Feed the two bodies enough that the buffers reach the end, then end them: the source is
    // told once. The playhead stays at the start: the end is measured on the range each body
    // fed, not on where the playhead happens to be.
    for (let i = 0; i < 40; i += 1) {
      videoBody.push(new Uint8Array([1]))
      audioBody.push(new Uint8Array([1]))
      await settle()
    }
    videoBody.end()
    audioBody.end()
    await settle()
    expect(
      source.endOfStreamCalls.length === 1,
      `endOfStream was called ${source.endOfStreamCalls.length} times`,
    ).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('start header moves the playhead', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    const videoBody = new FakeBody()
    recorder.answer('/tracks/1/stream', { startMs: 2000, body: () => videoBody })
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()

    // The server's `X-Fbtt-Start-Ms: 2000` is where the playhead lands: the remuxed tracks are
    // shifted forward by the 1000 ms offset, so 2000 ms of source time is 3 s of media time.
    expect(video.currentTime === 3, `the playhead did not move to the start header (got ${video.currentTime})`).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('a stream that ends early shows Waiting for data and retries', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    const firstBody = new FakeBody()
    // The first body answers, then ends after its init segment: one chunk, then done.
    recorder.answer('/tracks/1/stream', () => ({ startMs: 0, body: () => firstBody }))
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()
    // The init segment lands, then the body ends.
    firstBody.push(new Uint8Array([1, 2, 3]))
    firstBody.end()
    await settle()

    // The engine is waiting, and it will be asked again in a second.
    expect(engine.state(), 'an early end did not move the engine to waiting').toBe('waiting')
    const before = recorder.callsFor('/tracks/1/stream').length
    // No fetch before the first retry: ~900 ms is just under the one-second delay, so the
    // engine is still waiting on its timer.
    await new Promise((r) => setTimeout(r, 900))
    await settle()
    expect(
      recorder.callsFor('/tracks/1/stream').length === before,
      'the engine fetched before the first retry',
    ).toBe(true)
    // The first retry is one second out: the cadence is one, two, four, then five each.
    await new Promise((r) => setTimeout(r, 300))
    await settle()
    let after = recorder.callsFor('/tracks/1/stream').length
    expect(after - before === 1, 'the early end was not retried after a second').toBe(true)
    // The retry goes from the end of what the buffer has, not from the start. The buffer has
    // one media second, and with the 1000 ms offset that is source time 0: the offset's room
    // at the front has no source time, so the retry asks for the start of the file.
    const retry = recorder.callsFor('/tracks/1/stream')[after - 1]
    expect(retry.url.includes('start_ms=0'), `the retry did not start at the buffered end (${retry.url})`).toBe(true)

    // The first retry's body ends the same way, and the count persists, so the second retry
    // is two seconds out, not one.
    const firstRetryBody = recorder.callsFor('/tracks/1/stream')[after - 1].body
    firstRetryBody?.end()
    await settle()
    const beforeSecond = recorder.callsFor('/tracks/1/stream').length
    // No fetch before the second retry: ~1.9 s is just under the two-second delay.
    await new Promise((r) => setTimeout(r, 1_900))
    await settle()
    expect(
      recorder.callsFor('/tracks/1/stream').length === beforeSecond,
      'the engine fetched before the second retry',
    ).toBe(true)
    await new Promise((r) => setTimeout(r, 300))
    await settle()
    after = recorder.callsFor('/tracks/1/stream').length
    expect(after - beforeSecond === 1, 'the second retry was not two seconds out').toBe(true)

    // The second retry's first append lands: the data is arriving, and the state is the
    // engine's own, and the append is what the next `waiting` to `playing` turn would come from.
    const retryBody = recorder.callsFor('/tracks/1/stream')[after - 1].body
    retryBody?.push(new Uint8Array([9]))
    await settle()
    expect(engine.state(), 'the engine kept running after the retry appended').toBe('playing')

    engine.destroy()
    document.body.removeChild(video)
  })

  it('keyboard', async () => {
    const recorder = fakeFetch()
    // The Player's engine reads `window.fetch` (it is given no deps), so the test's fake fetch is
    // put on `window` for the duration of the test: the engine's audio-switch fetch lands in the
    // recorder the test reads back.
    vi.stubGlobal('fetch', recorder.fetch)
    const { client } = probeClient()
    let closeCalls = 0
    const { container } = render(
      <Player id="a" file={0} name="movie.mkv" client={client} onClose={() => { closeCalls += 1 }} />,
    )
    await settle()

    const dialog = container.querySelector('dialog') as HTMLDialogElement
    const video = container.querySelector('video') as HTMLVideoElement
    // jsdom's `play` and `pause` do not move the `paused` flag, so the test drives them the way a
    // browser does.
    makeVideoPlayable(video)

    // Space toggles the play state.
    expect(video.paused, 'the video was not paused at the start').toBe(true)
    fireEvent.keyDown(dialog, { key: ' ' })
    await settle()
    expect(video.paused === false, 'Space did not play').toBe(true)
    fireEvent.keyDown(dialog, { key: ' ' })
    await settle()
    expect(video.paused === true, 'Space did not pause').toBe(true)

    // `a` takes the next playable audio: the engine asks for its stream, with the other track id.
    const audioCallsBefore = recorder.callsFor('/tracks/3/stream').length
    fireEvent.keyDown(dialog, { key: 'a' })
    await settle()
    const audioCallsAfter = recorder.callsFor('/tracks/3/stream').length
    expect(
      audioCallsAfter - audioCallsBefore === 1,
      '`a` did not ask for the next audio track',
    ).toBe(true)

    // Escape closes: the dialog's `cancel` is answered as `onClose`, and the prevention is what
    // keeps the element from closing itself under the state. The test drives the `cancel` the
    // browser fires on Escape and reads that the handler reached, prevented it, and closed.
    const cancel = new Event('cancel', { bubbles: true, cancelable: true })
    dialog.dispatchEvent(cancel)
    expect(
      cancel.defaultPrevented,
      'Escape did not reach the dialog close handler and prevent the element closing itself',
    ).toBe(true)
    expect(closeCalls === 1, 'Escape did not close through onClose').toBe(true)
  })

  it('closing the audio menu keeps the subtitle menu open', async () => {
    const recorder = fakeFetch()
    vi.stubGlobal('fetch', recorder.fetch)
    const { client } = probeClient()
    const { container } = render(
      <Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />,
    )
    await settle()
    const dialog = container.querySelector('dialog') as HTMLElement
    // The menu buttons are labelled by their content, not by an `aria-label`.
    const buttons = Array.from(dialog.querySelectorAll('button'))
    const audioButton = buttons.find((b) => b.textContent === 'Audio') as HTMLButtonElement
    const subtitleButton = buttons.find((b) => b.textContent === 'Subtitles') as HTMLButtonElement
    expect(audioButton !== undefined, 'the audio menu was not found').toBe(true)
    expect(subtitleButton !== undefined, 'the subtitle menu was not found').toBe(true)

    // Open both menus: each keeps its own flag, so both are open at once.
    fireEvent.click(audioButton)
    await settle()
    fireEvent.click(subtitleButton)
    await settle()
    expect(
      audioButton.getAttribute('aria-expanded') === 'true',
      'the audio menu was not open',
    ).toBe(true)
    expect(
      subtitleButton.getAttribute('aria-expanded') === 'true',
      'the subtitle menu was not open',
    ).toBe(true)

    // Close the audio menu: the subtitle menu stays open, because each menu keeps its own flag.
    fireEvent.click(audioButton)
    await settle()
    expect(
      audioButton.getAttribute('aria-expanded') === 'false',
      'the audio menu was not closed',
    ).toBe(true)
    expect(
      subtitleButton.getAttribute('aria-expanded') === 'true',
      'closing the audio menu closed the subtitle menu too: the menus share one flag',
    ).toBe(true)
  })

  it('a seek restarts once', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    // The first fetch answers at 0; the restart answers at 1500, above the 1000 the seek
    // requested, so the engine moves the playhead to toMedia(1500) = 2.5 and the self-seek
    // guard must ignore the `seeking` that write fires.
    let callCount = 0
    recorder.answer('/tracks/1/stream', () => {
      callCount += 1
      return { startMs: callCount === 1 ? 0 : 1500, body: () => new FakeBody() }
    })
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()
    // The start has landed, and the playhead is at the body's start.
    const callsAtStart = recorder.callsFor('/tracks/1/stream').length
    expect(callsAtStart, 'the start was not asked for').toBe(1)
    // One chunk lands, and the buffer covers the start.
    const startBody = recorder.callsFor('/tracks/1/stream')[0]?.body
    startBody?.push(new Uint8Array([1]))
    await settle()

    // A seek the buffer does not cover restarts the streams once. The element's `seeking` fires
    // (the fake fires it on the `currentTime` write), and the engine restarts. The restart
    // answers at 1500, above the 1000 requested, so the engine moves the playhead to 2.5 and
    // that move fires another `seeking` - the one the guard must ignore, not answer with a
    // second restart.
    video.currentTime = 2
    await settle()
    const callsAfterSeek = recorder.callsFor('/tracks/1/stream').length
    expect(
      callsAfterSeek - callsAtStart === 1,
      `a seek restarted the stream more than once (${callsAfterSeek - callsAtStart})`,
    ).toBe(true)
    // The playhead landed where the body began: toMedia(1500, 1000) = 2.5.
    expect(
      Math.abs(video.currentTime - 2.5) < 1e-3,
      `the playhead did not land at the body's start (got ${video.currentTime})`,
    ).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('an aborted stream does not touch the new one', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    // Each fetch of the video track gets a fresh body that holds: the test can abort one fetch
    // and read that the old read loop did not run as the new one's.
    recorder.answer('/tracks/1/stream', () => ({ startMs: 0, body: () => new FakeBody() }))
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()
    // A chunk lands, then a seek aborts the in-flight fetch and starts a new one. The old read
    // loop, which was waiting on the aborted body, must not say the file ended or touch the new
    // stream's reader.
    const firstBody = recorder.callsFor('/tracks/1/stream')[0]?.body
    firstBody?.push(new Uint8Array([1]))
    await settle()
    // Fake time from before the seek: any retry the old loop schedules when it wakes is on the
    // fake clock, and the advance below runs it.
    vi.useFakeTimers()
    try {
      video.currentTime = 2
      await vi.advanceTimersByTimeAsync(10)
      const callsAfterSeek = recorder.callsFor('/tracks/1/stream').length
      expect(callsAfterSeek, 'the seek did not restart the stream').toBe(2)
      // The new body feeds a chunk: the data is arriving, and the engine goes back to playing.
      const newBody = recorder.callsFor('/tracks/1/stream').at(-1)?.body
      newBody?.push(new Uint8Array([1]))
      await vi.advanceTimersByTimeAsync(10)
      // Past the retry cadence (one, two, four seconds): the old loop must not have said the
      // file stopped and scheduled a retry, at any of its delays.
      await vi.advanceTimersByTimeAsync(8_000)
      expect(engine.state(), 'the old loop said the file stopped').toBe('playing')
      expect(
        recorder.callsFor('/tracks/1/stream').length - callsAfterSeek === 0,
        'the old loop scheduled a retry fetch',
      ).toBe(true)
    } finally {
      vi.useRealTimers()
    }
    expect(source.endOfStreamCalls.length, 'an abort ended the stream').toBe(0)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('the default fetch is called on the window', async () => {
    // The engine's default deps take `window.fetch` itself. The browser's `fetch` refuses a call
    // it is not invoked as `window.fetch` (an `Illegal invocation`), so the recorder below stands
    // in for `window.fetch` the way the browser is strict: it throws when its `this` is not the
    // `window`. The engine makes no deps at all - the default path - and the stream request must
    // still get out, and the state must not sit in the `waiting` the unbound call would have left.
    const source = new FakeMediaSource()
    const calls: string[] = []
    // The recorder answers like the rest of the fakes: a 200 with the stream's start header and
    // a fresh body for each call, the way the rest of the fakes build one. It is strict where
    // the browser is strict: it throws when its `this` is not the `window`, which is what an
    // unbound `window.fetch` call gets.
    const strictWindowFetch = function (this: unknown, input: RequestInfo | URL): Promise<unknown> {
      if (this !== window) throw new TypeError('Illegal invocation')
      calls.push(typeof input === 'string' ? input : input instanceof URL ? input.href : input.url)
      const body = new FakeBody()
      const headers = new Headers({ 'x-fbtt-start-ms': '0' })
      const reader = body.getReader()
      const response = {
        ok: true,
        status: 200,
        headers,
        body: { getReader: () => reader },
      }
      return Promise.resolve(response)
    } as unknown as typeof fetch
    const originalFetch = window.fetch
    window.fetch = strictWindowFetch
    try {
      const video = fakeVideo()
      document.body.appendChild(video)
      const engine = createEngine(
        video,
        { id: 'a', file: 0 },
        {
          offsetMs: 1000,
          durationMs: 4000,
          video: PROBE.tracks[0] as MediaTrack,
          videoMime: 'video/mp4; codecs="avc1.64000A"',
          audio: null,
          audioMime: null,
          mimeFor: () => null,
        },
        // No `fetch` in the deps: the default path, which reads `window.fetch`. The `MediaSource`
        // and `managed` are given only to keep the fake source in reach; the `fetch` stays the
        // browser's.
        {
          MediaSource: fakeDeps(source, fakeFetch()).MediaSource,
          managed: false,
        },
      )
      source.open()
      await settle()
      await settle()
      // One more for the append's `updateend`: the engine reads the body's end after it has put
      // the chunk in, and that end is what settles the engine's state.
      await settle()
      expect(
        calls.some((url) => url.includes('/tracks/1/stream')),
        'the default path never asked for a stream (an unbound fetch throws instead)',
      ).toBe(true)
      // The data has landed, and a data-arrived state is the one the person sees: `loading`
      // before the first byte, `playing` with data, and never the `waiting` the unbound call
      // would have left.
      expect(
        engine.state() !== 'waiting',
        `the unbound fetch was taken for a network wait (state ${engine.state()})`,
      ).toBe(true)
      engine.destroy()
      document.body.removeChild(video)
    } finally {
      window.fetch = originalFetch
    }
  })

  it('opening the player starts playback', async () => {
    const recorder = fakeFetch()
    vi.stubGlobal('fetch', recorder.fetch)
    // The stream the engine reads: one body, held open, so the test can feed data after it lands.
    const videoBody = new FakeBody()
    recorder.answer('/tracks/1/stream', () => ({ startMs: 0, body: () => videoBody }))
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const { client } = probeClient()
    const { container } = render(
      <Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />,
    )
    await settle()
    const video = container.querySelector('video') as HTMLVideoElement
    // The element the test reads `play()` back from: the open asks it once, and the ask is
    // counted here.
    let playCalls = 0
    video.play = (): Promise<void> => {
      playCalls += 1
      return Promise.resolve()
    }

    // The first data arrives: the engine appends it, the element can play, and the open's one
    // play happens.
    videoBody.push(new Uint8Array([1]))
    await settle()
    video.dispatchEvent(new Event('canplay'))
    await settle()
    expect(playCalls, 'the open did not start playback').toBe(1)

    // Later data does not ask again: the open plays once, not once per chunk.
    videoBody.push(new Uint8Array([2]))
    await settle()
    await settle()
    expect(playCalls, 'later data asked for playback again').toBe(1)

    // `canplay` on later data does not ask either: the element saying it can play is not the
    // open asking to play a second time.
    video.dispatchEvent(new Event('canplay'))
    await settle()
    expect(playCalls, 'a later canplay asked for playback again').toBe(1)
  })

  it('a pause before the first data is kept', async () => {
    const recorder = fakeFetch()
    vi.stubGlobal('fetch', recorder.fetch)
    const videoBody = new FakeBody()
    recorder.answer('/tracks/1/stream', () => ({ startMs: 0, body: () => videoBody }))
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const { client } = probeClient()
    const { container } = render(
      <Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />,
    )
    await settle()
    const video = container.querySelector('video') as HTMLVideoElement
    let playCalls = 0
    video.play = (): Promise<void> => {
      playCalls += 1
      return Promise.resolve()
    }

    // The viewer pauses before any data arrives: that is their word for what they want, and it
    // must be kept.
    video.dispatchEvent(new Event('pause'))
    await settle()

    // Data arrives and the element can play: the autoplay must not override the viewer's pause.
    videoBody.push(new Uint8Array([1]))
    await settle()
    video.dispatchEvent(new Event('canplay'))
    await settle()
    await settle()
    expect(playCalls, 'the autoplay overrode the viewer\'s pause').toBe(0)
  })

  it('a refused autoplay leaves the player paused', async () => {
    const recorder = fakeFetch()
    vi.stubGlobal('fetch', recorder.fetch)
    const videoBody = new FakeBody()
    recorder.answer('/tracks/1/stream', () => ({ startMs: 0, body: () => videoBody }))
    recorder.answer('/tracks/2/stream', { status: 500 })
    const { client } = probeClient()
    const { container } = render(
      <Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />,
    )
    await settle()
    const video = container.querySelector('video') as HTMLVideoElement
    // The browser refuses the autoplay: `play()` rejects, the way an autoplay policy does.
    video.play = (): Promise<void> => Promise.reject(new Error('NotAllowedError'))

    // Data arrives and the element can play: the autoplay is refused.
    videoBody.push(new Uint8Array([1]))
    await settle()
    video.dispatchEvent(new Event('canplay'))
    await settle()
    await settle()
    // The refusal is not an error: the Play control shows, so the person can play when they
    // choose. The autoplay's own error state is not one a person sees (the element's `play`
    // promise is caught), so no unhandled rejection leaks into the test either.
    expect(
      video.paused === true,
      'a refused autoplay did not leave the player paused',
    ).toBe(true)
    expect(
      screen.queryByRole('button', { name: 'Play' }) !== null,
      'the Play control did not show after a refused autoplay',
    ).toBe(true)
  })

  it('reading pauses at the gate', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    const videoBody = new FakeBody()
    recorder.answer('/tracks/1/stream', { startMs: 0, body: () => videoBody })
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()
    // Feed the buffer a long way ahead of the playhead: past the gate's stop line, the read must
    // stop rather than keep asking for chunks it has already buffered a minute of. The buffer is
    // one media second per chunk, so 130 chunks is 130 seconds ahead. Each settle lets the
    // append's `updateend` land the way a browser's does: a macrotask, not a microtask.
    for (let i = 0; i < 130; i += 1) {
      videoBody.push(new Uint8Array([1]))
      await settle()
    }
    // The read is stopped: the body has chunks held, and the buffer is far ahead of the playhead.
    const bufferAhead = (source.activeSourceBuffers[0]?.buffered.end(0) ?? 0) - video.currentTime
    expect(bufferAhead >= 120, 'the buffer is not far ahead of the playhead').toBe(true)
    // One more chunk is held, not appended: the gate said stop, and the read waited for a
    // `timeupdate` to lift it.
    const appendsBeforeGate = source.activeSourceBuffers[0]?.log.appends.length ?? 0
    videoBody.push(new Uint8Array([1]))
    await settle()
    const appendsAfterGate = source.activeSourceBuffers[0]?.log.appends.length ?? 0
    expect(
      appendsAfterGate === appendsBeforeGate,
      'the gate did not stop the read: a chunk was appended past the stop line',
    ).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('quota evicts and retries the same chunk in order', async () => {
    // The engine drives the quota path: a buffer that throws QuotaExceededError on the second
    // append, the engine evicts the ranges behind the playhead, and retries the same chunk (A)
    // before the next one (B). The appended sequence is A, A, B - the retried A is before B.
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    const videoBody = new FakeBody()
    recorder.answer('/tracks/1/stream', { startMs: 0, body: () => videoBody })
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 120_000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()
    const buffer = source.activeSourceBuffers[0]
    expect(buffer !== undefined, 'the video buffer was not made').toBe(true)
    // Feed chunks so the buffer runs to media 100: the playhead at 70 sits inside it, and the
    // 60 seconds behind it (media 0 to 10) lie past the eviction's keep line.
    for (let i = 0; i < 100; i += 1) {
      videoBody.push(new Uint8Array([1]))
      await settle()
    }
    // Move the playhead to media 70: the range [0, 10] lies 60 seconds behind, past the keep
    // line, so the eviction has something to cut, and the playhead stays inside the buffer.
    video.currentTime = 70
    await settle()
    // The next chunk (A) will hit the quota: the buffer is full, and the next two appends are
    // refused, the way a buffer that cannot make room for a chunk refuses it a second time,
    // even after the eviction has cut what it can.
    buffer.makeNextAppendThrowQuota(2)
    videoBody.push(new Uint8Array([1]))
    await settle()
    // The refused A is not appended, and the eviction cut the ranges behind the playhead.
    expect(buffer.log.appends.length === 100, 'the refused chunk was appended').toBe(true)
    expect(buffer.log.removes.length > 0, 'the eviction did not remove any range').toBe(true)
    expect(buffer.log.quotas === 2, 'the eviction did not make room for the chunk').toBe(true)
    // B follows in the body, and it does not land: the held A is not appended before the read
    // is woken, so B is not reordered ahead of the retried A. B is a different size than A, so
    // the order of the appends is readable back from the log.
    videoBody.push(new Uint8Array([1, 2]))
    await settle()
    expect(buffer.log.appends.length === 100, 'the chunk after the held one was appended first').toBe(true)
    // The playhead advances a second (a `timeupdate`, not a seek: 71 is still inside the
    // buffer), and the read is woken: the held A goes in, before B, the way a browser's
    // buffer makes room as the playhead moves.
    video.currentTime = 71
    video.dispatchEvent(new Event('timeupdate'))
    await settle()
    // The held A (1 byte) went in before B (2 bytes): the order of the appends is the order
    // the decoder wants it in, and the retried A is not reordered behind the data that follows.
    expect(buffer.log.appends.length === 102, 'the held chunk and the one after it were not both appended').toBe(true)
    expect(buffer.log.appends[100]?.byteLength === 1, 'the retried A did not go in before B').toBe(true)
    expect(buffer.log.appends[101]?.byteLength === 2, 'the chunk after the held one did not go in after A').toBe(true)
    // The state is not an error: the chunk that did not fit is not dropped, and the engine is
    // not in the permanent `error` state.
    expect(
      engine.state() !== 'error',
      'a quota error became a permanent error',
    ).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('a busy server is a retry, not an error', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    // The server is busy: a 503 with `Retry-After: 3`, the exact shape Task 6's routes give.
    // Three seconds is longer than the first retry's one, so the header is what sets the wait.
    recorder.answer('/tracks/1/stream', () => ({ status: 503, headers: { 'Retry-After': '3' } }))
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()
    // A 503 is a wait, not an error: the engine is waiting, and it is not the permanent `error`.
    expect(engine.state(), 'a 503 was treated as a permanent error').toBe('waiting')
    expect(engine.state() === 'error', 'a 503 became an error').toBe(false)
    // No refetch before the Retry-After: the 503's `Retry-After: 3` is the minimum delay, and
    // ~2.9 s is just under it, so the engine is still waiting.
    const before = recorder.callsFor('/tracks/1/stream').length
    expect(before, 'the initial fetch did not happen').toBe(1)
    await new Promise((r) => setTimeout(r, 2_850))
    await settle()
    expect(
      recorder.callsFor('/tracks/1/stream').length === before,
      'the engine refetched before the Retry-After',
    ).toBe(true)
    // The refetch is at 3000 ms: the Retry-After's three seconds, the minimum delay.
    await new Promise((r) => setTimeout(r, 400))
    await settle()
    const after = recorder.callsFor('/tracks/1/stream').length
    expect(
      after - before === 1,
      `the engine did not refetch after the Retry-After (${after - before})`,
    ).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('changing audio reuses the buffer', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    recorder.answer('/tracks/1/stream', { startMs: 0, body: () => new FakeBody() })
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    recorder.answer('/tracks/3/stream', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: (track: MediaTrack): string | null => {
          for (const codec of track.mime_codecs) {
            const mime = `audio/mp4; codecs="${codec}"`
            if (/mp4a|opus/.test(mime)) return mime
          }
          return null
        },
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()
    const buffersAtStart = source.activeSourceBuffers.length
    const audioBuffer = source.activeSourceBuffers[1]
    // The start is all microtasks (the open, the two fetches), so the audio body is out after
    // one settle; each chunk lands in one `updateend`, a macrotask a settle covers.
    const body = recorder.callsFor('/tracks/2/stream')[0]?.body
    expect(body !== undefined && body !== null, 'the audio stream was not fetched').toBe(true)
    // Two chunks: the audio buffer runs to media 2, and the playhead at 1 is inside it, so the
    // switch has data ahead of the playhead to remove and a point the new track fetches from.
    body?.push(new Uint8Array([1]))
    await settle()
    body?.push(new Uint8Array([1]))
    await settle()
    expect(audioBuffer.ranges, 'the two audio chunks did not land before the switch').toEqual([[0, 2]])
    // Switch the audio: the engine reuses the audio buffer with `changeType`, not a new one.
    const opsBefore = audioBuffer.log.ops.length
    engine.setAudio(PROBE.tracks[2] as MediaTrack)
    // The abort, the remove, the `changeType` and the fetch: two `updateend`s, and a settle
    // more than that for margin.
    await settle()
    await settle()
    await settle()
    expect(
      source.activeSourceBuffers.length === buffersAtStart,
      'changing audio added a second source buffer',
    ).toBe(true)
    expect(
      audioBuffer !== undefined && audioBuffer.log.changeTypes.length === 1,
      'changing audio did not change the buffer type',
    ).toBe(true)
    // The buffer is aborted first, then cut, then re-typed: one ordered log of all three calls,
    // so an abort that comes late (or only from somewhere else) is read back as out of order.
    expect(
      audioBuffer.log.ops.slice(opsBefore, opsBefore + 3),
      'changing audio did not abort, then remove, then changeType the buffer',
    ).toEqual(['abort', 'remove', 'changeType'])
    // The old audio ranges were removed from the playhead to the end.
    expect(
      audioBuffer !== undefined && audioBuffer.log.removes.length >= 1,
      'changing audio did not remove the old audio ranges',
    ).toBe(true)
    // The new track is fetched at toSource(currentTime), through the switch's own
    // `beginStream`: one call for track 3, with a `start_ms` the engine asks for.
    expect(
      recorder.callsFor('/tracks/3/stream').length === 1,
      'changing audio did not ask for the new track',
    ).toBe(true)
    const newAudioCall = recorder.callsFor('/tracks/3/stream')[0]
    // The new track is fetched exactly at `toSource(currentTime)`: the point the playhead is
    // at, mapped into source time, is where the new track's data begins.
    const expectedStartMs = toSource(video.currentTime, 1000)
    expect(
      newAudioCall !== undefined &&
        newAudioCall.url.includes(`start_ms=${expectedStartMs}`),
      `the new audio track was not asked for at toSource(currentTime) (got ${newAudioCall?.url}, wanted start_ms=${expectedStartMs})`,
    ).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('the remembered position survives the open', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    recorder.answer('/tracks/1/stream', { startMs: 0, body: () => new FakeBody() })
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    // The remembered position, in source milliseconds, is where the engine will start.
    const rememberedMs = 3000
    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    // The player asks to start at the remembered position before the source opens.
    engine.seek(rememberedMs)
    source.open()
    await settle()
    // The engine started there: the first fetch is at the remembered point, not at 0.
    const firstCall = recorder.callsFor('/tracks/1/stream')[0]
    expect(
      firstCall !== undefined && firstCall.url.includes(`start_ms=${rememberedMs}`),
      `the engine did not start at the remembered position (${firstCall?.url})`,
    ).toBe(true)
    // The playhead is at the remembered position: toMedia(3000, 1000) = 4.
    expect(
      Math.abs(video.currentTime - 4) < 1e-3,
      `the playhead did not land at the remembered position (got ${video.currentTime})`,
    ).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('subtitles Off hides every track', async () => {
    const recorder = fakeFetch()
    vi.stubGlobal('fetch', recorder.fetch)
    // The chosen subtitle has cues, so a `TextTrack` is made and shown.
    const { client } = probeClient(
      PROBE,
      [{ start_ms: 0, end_ms: 1000, text: 'hello' }],
    )
    const { container } = render(
      <Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />,
    )
    await settle()
    const video = container.querySelector('video') as HTMLVideoElement

    // Choose a subtitle: the first `c` takes the first track, and its cues make a `TextTrack`
    // that is shown.
    fireEvent.keyDown(container.querySelector('dialog') as HTMLElement, { key: 'c' })
    await settle()
    const showingBefore = Array.from(video.textTracks).filter((t) => t.mode === 'showing').length
    expect(showingBefore >= 1, 'the chosen subtitle was not showing').toBe(true)

    // Cycle to Off: the choices are Off, the text track, and the external file, so two more
    // `c` presses reach Off, and every track is disabled.
    for (let i = 0; i < 2; i += 1) {
      fireEvent.keyDown(container.querySelector('dialog') as HTMLElement, { key: 'c' })
      await settle()
    }
    const showingAfter = Array.from(video.textTracks).filter((t) => t.mode === 'showing').length
    expect(showingAfter === 0, 'Off left a subtitle track showing').toBe(true)
  })

  it('a chosen subtitle shows its cues', async () => {
    const recorder = fakeFetch()
    vi.stubGlobal('fetch', recorder.fetch)
    // The chosen subtitle's window answers with two cues: the track the player makes for the key
    // must be the one shown, and the cues it is given must be on it. The fake `TextTrack` keeps
    // the browser's strictness: its `id` is a getter with no setter, so a player that writes it
    // is caught here the way a browser would catch it.
    const { client } = probeClient(
      PROBE,
      [
        { start_ms: 1000, end_ms: 2000, text: 'hello' },
        { start_ms: 2000, end_ms: 3000, text: 'world' },
      ],
    )
    const { container } = render(
      <Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />,
    )
    await settle()
    const video = container.querySelector('video') as HTMLVideoElement

    // Choose the first subtitle track: one `c` takes it, and its cues arrive from the window.
    fireEvent.keyDown(container.querySelector('dialog') as HTMLElement, { key: 'c' })
    await settle()
    const tracks = Array.from(video.textTracks)
    expect(tracks.length >= 1, 'the chosen subtitle made no track').toBe(true)
    const shown = tracks.filter((track) => track.mode === 'showing')
    expect(shown.length === 1, 'a chosen subtitle did not show its track').toBe(true)
    const shownCues = (shown[0] as unknown as { cues: unknown[] }).cues
    const texts = shownCues.map((cue) => (cue as { text: string }).text)
    expect(
      texts.includes('hello') && texts.includes('world'),
      `the chosen subtitle did not hold its cues (got ${JSON.stringify(texts)})`,
    ).toBe(true)
  })

  it('a subtitle language without a name is shown through displaySafe', async () => {
    const recorder = fakeFetch()
    vi.stubGlobal('fetch', recorder.fetch)
    // A text track with no name and a hostile language: the label is the language, and
    // `displaySafe` is what turns `<img src=x onerror=alert(1)>` into inert text.
    const probe: MediaProbe = {
      ...PROBE,
      tracks: PROBE.tracks.map((t) =>
        t.id === 4
          ? { ...t, name: null, language: 'eng\u202e.png' }
          : t,
      ),
    }
    const { client } = probeClient(probe)
    render(<Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />)
    await settle()
    const subtitles = screen.getByRole('button', { name: 'Subtitles' })
    fireEvent.click(subtitles)
    await settle()
    const listbox = screen.getByRole('listbox', { name: 'Subtitles' })
    const subTexts = Array.from(listbox.querySelectorAll('[role="option"]')).map(
      (option) => option.textContent ?? '',
    )
    // The language's invisible bidi override is what `displaySafe` removes: a label still
    // carrying it was listed raw, without the safe pass.
    const hostile = subTexts.find((text) => text.includes('\u202e'))
    expect(
      hostile === undefined,
      `the hostile language was listed raw: the label skipped displaySafe (${JSON.stringify(subTexts)})`,
    ).toBe(true)
  })

  it('two subtitle tracks with the same label stay separate', async () => {
    const recorder = fakeFetch()
    vi.stubGlobal('fetch', recorder.fetch)
    // Two text tracks, both with the language `eng` and no name: `trackLabel` falls back to the
    // language, so both `TextTrack`s are made with the label `eng`. The player keys the tracks
    // by their own key, so each keeps its own cues.
    const probe: MediaProbe = {
      ...PROBE,
      tracks: PROBE.tracks.map((t) =>
        t.id === 4 || t.id === 5
          ? { ...t, language: 'eng', text: true, codec: 'srt' }
          : t,
      ),
    }
    const { client } = probeClient(
      probe,
      [
        { start_ms: 0, end_ms: 1000, text: 'first' },
        { start_ms: 1000, end_ms: 2000, text: 'second' },
      ],
    )
    const { container } = render(
      <Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />,
    )
    await settle()
    const video = container.querySelector('video') as HTMLVideoElement
    const dialog = container.querySelector('dialog') as HTMLElement

    // The choices are Off, the two text tracks, and the external file, so one `c` takes the
    // first track and two more `c` presses take the second.
    fireEvent.keyDown(dialog, { key: 'c' })
    await settle()
    const tracksAfterFirst = Array.from(video.textTracks).length
    expect(tracksAfterFirst >= 1, 'the first track was not made').toBe(true)

    fireEvent.keyDown(dialog, { key: 'c' })
    await settle()
    fireEvent.keyDown(dialog, { key: 'c' })
    await settle()
    const tracks = Array.from(video.textTracks)
    expect(
      tracks.length >= 2,
      `the two tracks were merged into one (label lookup: ${tracks.map((t) => t.label).join(', ')})`,
    ).toBe(true)
    const byLabel = new Map<string, number>()
    for (const t of tracks) byLabel.set(t.label, (byLabel.get(t.label) ?? 0) + 1)
    expect(
      (byLabel.get('eng') ?? 0) >= 2,
      'two tracks are not kept under the same label',
    ).toBe(true)
  })

  it('a position is saved and restored', async () => {
    // The player saves the position in source seconds and restores it on reopen. Play to a
    // position, unmount, reopen the same file, and the restart fetch and the playhead are at
    // the saved position. The probe is a 120 s file so the position is above the 10 s minimum.
    const longProbe: MediaProbe = { ...PROBE, duration_ms: 120_000 }
    // Clear any position a previous run left: the test saves and restores its own.
    localStorage.removeItem('fbtt.position.a.0')
    const recorder = fakeFetch()
    vi.stubGlobal('fetch', recorder.fetch)
    recorder.answer('/tracks/1/stream', { startMs: 0, body: () => new FakeBody() })
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const { client } = probeClient(longProbe)
    const { container, unmount } = render(
      <Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />,
    )
    await settle()
    const video = container.querySelector('video') as HTMLVideoElement
    // Play to a position: source 12000 ms, which is media 13 s. The position is 12 source
    // seconds, above the 10-second minimum `positions` requires.
    video.currentTime = 13
    // Unmount: the player's cleanup calls `save()`, which stores the position.
    unmount()
    // The position is in localStorage, in source seconds.
    const stored = localStorage.getItem('fbtt.position.a.0')
    expect(stored !== null, 'the position was not saved').toBe(true)
    const storedSeconds = Number(stored)
    // Source 12000 ms is 12 source seconds.
    expect(
      Math.abs(storedSeconds - 12) < 0.5,
      `the position was not saved in source seconds (got ${storedSeconds})`,
    ).toBe(true)
    // Reopen: the position is still in localStorage, and the engine starts the file. The
    // player re-reads the position when the source opens, and the restart fetch is at the
    // saved point.
    const recorder2 = fakeFetch()
    vi.stubGlobal('fetch', recorder2.fetch)
    recorder2.answer('/tracks/1/stream', { startMs: 0, body: () => new FakeBody() })
    recorder2.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const { client: client2 } = probeClient(longProbe)
    const { container: container2 } = render(
      <Player id="a" file={0} name="movie.mkv" client={client2} onClose={() => {} } />,
    )
    await settle()
    await settle()
    // The position is still in localStorage: the reopen did not clear it.
    expect(
      localStorage.getItem('fbtt.position.a.0') === '12',
      'the reopen cleared the stored position',
    ).toBe(true)
    // The engine started the file at the saved point: the reopen fetch is at source 12000 ms,
    // the position the player re-read from `localStorage`, and the playhead is at the matching
    // media time (12000 ms of source plus the 1000 ms offset is 13 s of media).
    const reopenUrl = recorder2.callsFor('/tracks/1/stream')[0]?.url ?? ''
    expect(
      reopenUrl.includes('start_ms=12000'),
      `the reopen did not fetch from the saved position (got ${reopenUrl})`,
    ).toBe(true)
    expect(
      Math.abs((container2.querySelector('video') as HTMLVideoElement).currentTime - 13) < 0.5,
      `the playhead is not at the saved position's media time`,
    ).toBe(true)
    // Clear the stored position.
    localStorage.removeItem('fbtt.position.a.0')
  })

  it('a finished file starts from the beginning', async () => {
    // A file the person watched to within 30 s of the end is done: its position is cleared,
    // and a reopen starts at the beginning, not where the person left off. The probe is a
    // 120 s file, so a position at 95 s is within the 30 s `FINISHED_WITHIN` and above the
    // 10 s minimum that would otherwise keep it.
    const probe: MediaProbe = { ...PROBE, duration_ms: 120_000 }
    localStorage.removeItem('fbtt.position.a.0')
    const recorder = fakeFetch()
    vi.stubGlobal('fetch', recorder.fetch)
    recorder.answer('/tracks/1/stream', { startMs: 0, body: () => new FakeBody() })
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const { client } = probeClient(probe)
    const { container, unmount } = render(
      <Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {} } />,
    )
    await settle()
    const video = container.querySelector('video') as HTMLVideoElement
    // Play to 96 s of media: source 95000 ms (96 s of media minus the 1 s offset), which is 95
    // source seconds, within 30 s of the 120 s end.
    video.currentTime = 96
    unmount()
    // The position was within 30 s of the end, so it was cleared: nothing is stored.
    expect(
      localStorage.getItem('fbtt.position.a.0') === null,
      'a position within 30 s of the end was not cleared',
    ).toBe(true)
    // Reopen: the file starts from the beginning, not from the cleared position.
    const recorder2 = fakeFetch()
    vi.stubGlobal('fetch', recorder2.fetch)
    recorder2.answer('/tracks/1/stream', { startMs: 0, body: () => new FakeBody() })
    recorder2.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const { client: client2 } = probeClient(probe)
    render(<Player id="a" file={0} name="movie.mkv" client={client2} onClose={() => {} } />)
    await settle()
    await settle()
    // Exactly one reopen fetch, and it is from the start.
    const reopenCalls = recorder2.callsFor('/tracks/1/stream')
    expect(reopenCalls.length, 'the reopen did not fetch the video exactly once').toBe(1)
    const reopenUrl = reopenCalls[0]?.url ?? ''
    expect(
      reopenUrl.includes('start_ms=0'),
      `a finished file did not start from the beginning (got ${reopenUrl})`,
    ).toBe(true)
    localStorage.removeItem('fbtt.position.a.0')
  })

  it('a seek after the end restarts', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    const videoBody = new FakeBody()
    const audioBody = new FakeBody()
    recorder.answer('/tracks/1/stream', () => ({ startMs: 0, body: () => videoBody }))
    recorder.answer('/tracks/2/stream', () => ({ startMs: 0, body: () => audioBody }))
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()
    // Feed both bodies to the end: the source ends.
    for (let i = 0; i < 40; i += 1) {
      videoBody.push(new Uint8Array([1]))
      audioBody.push(new Uint8Array([1]))
      await settle()
    }
    videoBody.end()
    audioBody.end()
    await settle()
    expect(
      source.endOfStreamCalls.length === 1,
      'the source did not end',
    ).toBe(true)
    const callsAtEnd = recorder.callsFor('/tracks/1/stream').length
    // Seek into an evicted range: the ended state is cleared and the streams restart.
    video.currentTime = 2
    await settle()
    const callsAfterSeek = recorder.callsFor('/tracks/1/stream').length
    expect(
      callsAfterSeek - callsAtEnd === 1,
      `a seek after the end did not restart the stream (${callsAfterSeek - callsAtEnd})`,
    ).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('seeking back far opens the read gate', async () => {
    // A buffer far ahead of the playhead, in a range the playhead does not sit in: with the
    // gate measuring the range the playhead sits in (0 when it sits in none), a `timeupdate`
    // that moves the playhead out of the far range lifts the read. Before the fix, "ahead" was
    // measured to the far end of all ranges, so the far range kept the gate shut even after
    // the playhead had left it, and the read stayed stopped.
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    const videoBody = new FakeBody()
    recorder.answer('/tracks/1/stream', () => ({ startMs: 0, body: () => videoBody }))
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()
    const buf = source.activeSourceBuffers[0]
    expect(buf !== undefined, 'no source buffer').toBe(true)
    // One chunk: the buffer is [0, 1], the playhead at 0, the gate open, the read running.
    videoBody.push(new Uint8Array([1]))
    await settle()
    // The buffer runs far ahead and the playhead sits in it: [0, 131], the playhead at media 1
    // (the element's start, `toMedia(0)`), 130 seconds ahead, past the gate's stop line. The
    // read is held at the gate.
    ;(buf as unknown as { _buffered: [number, number][] })._buffered = [[0, 131]]
    // The read already waiting takes this chunk (the gate answers before a read, not after),
    // and the gate, measured after it lands, says stop.
    videoBody.push(new Uint8Array([1]))
    await settle()
    // The next chunk is held: counted before the push, so the count can tell.
    const appendsAtGate = buf?.log.appends.length ?? 0
    videoBody.push(new Uint8Array([1]))
    await settle()
    expect(
      (buf?.log.appends.length ?? 0) === appendsAtGate,
      'the gate did not hold the read over a far-ahead range',
    ).toBe(true)
    // The playhead leaves the range into the gap behind it: the buffer is [2, 132], and the
    // playhead at media 1 sits in no range. The far end of everything buffered is still 131
    // ahead, but the range the playhead sits in is none, so the gate re-measures to 0 and lifts.
    ;(buf as unknown as { _buffered: [number, number][] })._buffered = [[2, 132]]
    videoBody.push(new Uint8Array([1]))
    const appendsBefore = buf?.log.appends.length ?? 0
    // The `timeupdate` is the element's word for the move, and it is what wakes the read: the
    // loop, blocked at the gate, re-measures on its next read and takes the held chunk.
    video.dispatchEvent(new Event('timeupdate'))
    for (let i = 0; i < 4; i += 1) {
      videoBody.push(new Uint8Array([1]))
      await settle()
    }
    const appendsAfter = buf?.log.appends.length ?? 0
    expect(
      appendsAfter > appendsBefore,
      'the gate stayed shut over a range the playhead does not sit in: the read did not resume',
    ).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('a retry after seeking back fills the gap', async () => {
    // A buffer with a far range the body did not feed: when the body ends early, the retry goes
    // from the end of the range the body fed (or the body's own start, when that range is
    // gone), not from the far end of everything buffered, which would start past the gap and
    // never fill it.
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    const firstBody = new FakeBody()
    recorder.answer('/tracks/1/stream', () => ({ startMs: 0, body: () => firstBody }))
    recorder.answer('/tracks/2/stream', { startMs: 0, body: () => new FakeBody() })
    const video = fakeVideo()
    document.body.appendChild(video)

    const engine = createEngine(
      video,
      { id: 'a', file: 0 },
      {
        offsetMs: 1000,
        durationMs: 4000,
        video: PROBE.tracks[0] as MediaTrack,
        videoMime: 'video/mp4; codecs="avc1.64000A"',
        audio: PROBE.tracks[1] as MediaTrack,
        audioMime: 'audio/mp4; codecs="mp4a.40.2"',
        mimeFor: () => null,
      },
      fakeDeps(source, recorder),
    )
    source.open()
    await settle()
    const buf = source.activeSourceBuffers[0]
    expect(buf !== undefined, 'no source buffer').toBe(true)
    // The body ends after its init segment: one chunk, then done. The buffer is far ahead of
    // the playhead, in a range the playhead does not sit in.
    firstBody.push(new Uint8Array([1]))
    await settle()
    ;(buf as unknown as { _buffered: [number, number][] })._buffered = [[5, 200]]
    // The playhead is at media 1, outside [5, 200]: it sits in no range.
    firstBody.end()
    await settle()
    // The body ended early: the engine is waiting, and it schedules a retry. The retry goes
    // from the body's own start (its range is gone), not from the far end of the buffer (200).
    expect(engine.state(), 'an early end was not a wait').toBe('waiting')
    // Advance past the first retry delay (1 s): the retry fetch is at the body's start, not
    // past the far range.
    const callsBefore = recorder.callsFor('/tracks/1/stream').length
    await new Promise((r) => setTimeout(r, 1_100))
    await settle()
    const retryCall = recorder.callsFor('/tracks/1/stream').at(-1)
    expect(
      recorder.callsFor('/tracks/1/stream').length - callsBefore === 1,
      'the early end did not schedule a retry',
    ).toBe(true)
    // The retry is from the body's own start (source 0), not from the far end of the buffer
    // (which would be source 199000, past the playhead's gap).
    expect(
      retryCall !== undefined && retryCall.url.includes('start_ms=0'),
      `the retry did not start from the body's own range (got ${retryCall?.url})`,
    ).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('a far range does not end the stream', async () => {
    // The audio has reached the end of the file. The video body fed one second and ends; a far
    // range it did not feed runs past the file's end. That range is not the video's data, so
    // the video is not at its end, and the source is not ended.
    const t = await openEngine()
    for (let i = 0; i < 6; i += 1) {
      t.body(2).push(new Uint8Array([1]))
      await settle()
    }
    t.body(2).end()
    await settle()
    t.body(1).push(new Uint8Array([1]))
    await settle()
    setRanges(t.videoBuffer, [[5, 200]])
    t.body(1).end()
    await settle()
    expect(t.source.endOfStreamCalls.length, 'a far range ended the stream').toBe(0)
    expect(t.engine.state(), 'an early end was not a wait').toBe('waiting')
    t.close()
  })

  it('a seek drops the held chunk', async () => {
    // A chunk the buffer refused for quota is held, then a seek restarts the stream. The held
    // chunk is the old position's: it must never be appended, and the new body must be read.
    const t = await openEngine({ durationMs: 120_000 })
    t.body(1).push(new Uint8Array([1]))
    await settle()
    t.videoBuffer.makeNextAppendThrowQuota(2)
    t.body(1).push(new Uint8Array([7, 7, 7]))
    await settle()
    expect(t.videoBuffer.log.quotas, 'the chunk was not refused twice').toBe(2)
    expect(t.videoBuffer.log.appends.length, 'the refused chunk was appended').toBe(1)
    t.video.currentTime = 20
    await settle()
    expect(t.recorder.callsFor('/tracks/1/stream').length, 'the seek did not restart').toBe(2)
    t.body(1).push(new Uint8Array([1]))
    await settle()
    expect(t.videoBuffer.log.appends.length, 'the new body was not read').toBe(2)
    // Past the hold timer too: the old chunk never goes in.
    await sleep(1_100)
    await settle()
    expect(
      t.videoBuffer.log.appends.some((chunk) => chunk.byteLength === 3),
      'the old position\'s held chunk was appended into the new stream',
    ).toBe(false)
    t.close()
  })

  it('a held chunk is offered again on a timer', async () => {
    // The first chunk after a seek is refused for quota and held. It is the data at the
    // playhead, so the element fires no `timeupdate` and the buffer no `updateend`: only the
    // engine's own timer can offer it again.
    const t = await openEngine({ durationMs: 120_000 })
    for (let i = 0; i < 3; i += 1) {
      t.body(1).push(new Uint8Array([1]))
      await settle()
    }
    t.video.currentTime = 20
    await settle()
    t.videoBuffer.makeNextAppendThrowQuota(2)
    t.body(1).push(new Uint8Array([1]))
    await settle()
    expect(t.videoBuffer.log.quotas, 'the chunk was not refused twice').toBe(2)
    expect(t.videoBuffer.log.appends.length, 'the refused chunk was appended').toBe(3)
    await sleep(1_100)
    await settle()
    expect(t.videoBuffer.log.appends.length, 'the held chunk was never offered again').toBe(4)
    t.close()
  })

  it('quota evicts the ranges away from the playhead', async () => {
    // After a seek to media 20, the buffer holds [0, 3]: not behind the playhead by the 60 s the
    // keep line needs, but a range the playhead is not in and the body does not feed. A full
    // buffer gives it up, so the chunk at the playhead fits.
    const t = await openEngine({ durationMs: 120_000 })
    for (let i = 0; i < 3; i += 1) {
      t.body(1).push(new Uint8Array([1]))
      await settle()
    }
    t.video.currentTime = 20
    await settle()
    t.videoBuffer.makeNextAppendThrowQuota(1)
    t.body(1).push(new Uint8Array([1]))
    await settle()
    expect(t.videoBuffer.log.removes, 'the range away from the playhead was not evicted').toEqual([[0, 3]])
    expect(t.videoBuffer.log.appends.length, 'the chunk did not go in after the eviction').toBe(4)
    t.close()
  })

  it('the end is measured on the range the body fed', async () => {
    // The playhead sits in an earlier range, [0.5, 2]; the video body feeds a later one that
    // runs past the file's end. When both bodies end, the file is over: the source is ended
    // once, and nothing waits or refetches.
    const t = await openEngine()
    t.body(1).push(new Uint8Array([1]))
    await settle()
    setRanges(t.videoBuffer, [[0.5, 2], [3, 4]])
    for (let i = 0; i < 3; i += 1) {
      t.body(1).push(new Uint8Array([1]))
      await settle()
    }
    for (let i = 0; i < 6; i += 1) {
      t.body(2).push(new Uint8Array([1]))
      await settle()
    }
    t.body(1).end()
    t.body(2).end()
    await settle()
    expect(t.video.currentTime, 'the playhead moved').toBe(1)
    expect(t.source.endOfStreamCalls.length, 'the end was not measured on the range the body fed').toBe(1)
    expect(t.engine.state(), 'a body that reached the end was taken for an early stop').toBe('playing')
    t.close()
  })

  it('a retry resumes from the range the body fed', async () => {
    // The playhead sits in [0.5, 2]; the video body fed [3, 5] and stops early. The retry goes
    // on from 5 (source 4000), not from the playhead's range end (source 1000), which would
    // fetch again what the buffer already holds.
    const t = await openEngine({ durationMs: 120_000 })
    t.body(1).push(new Uint8Array([1]))
    await settle()
    setRanges(t.videoBuffer, [[0.5, 2], [3, 4]])
    t.body(1).push(new Uint8Array([1]))
    await settle()
    t.body(1).end()
    await settle()
    expect(t.engine.state(), 'an early end was not a wait').toBe('waiting')
    await sleep(1_100)
    await settle()
    const calls = t.recorder.callsFor('/tracks/1/stream')
    expect(calls.length, 'the early end was not retried').toBe(2)
    expect(calls[1]?.url, 'the retry did not resume from the range the body fed').toContain('start_ms=4000')
    t.close()
  })

  it('a range just past the playhead holds the gate', async () => {
    // The playhead is at media 1.000 and the buffered range starts at 1.020: the first fragment
    // seldom starts on the exact millisecond. The playhead is in that range, so a range 130 s
    // ahead of it shuts the gate.
    const t = await openEngine()
    t.body(1).push(new Uint8Array([1]))
    await settle()
    setRanges(t.videoBuffer, [[1.02, 131]])
    // The read already waiting takes this chunk; the gate, measured after it, says stop.
    t.body(1).push(new Uint8Array([1]))
    await settle()
    const appendsAtGate = t.videoBuffer.log.appends.length
    t.body(1).push(new Uint8Array([1]))
    await settle()
    expect(
      t.videoBuffer.log.appends.length,
      "a range starting just past the playhead did not count as the playhead's: the gate stayed open",
    ).toBe(appendsAtGate)
    t.close()
  })

  it('an error after a seek stops the streams again', async () => {
    // The audio route fails: an error, and the video stream is stopped. A seek starts both again,
    // the audio fails again, and the new video stream must be stopped again too.
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    recorder.answer('/tracks/1/stream', () => ({ startMs: 0, body: () => new FakeBody() }))
    recorder.answer('/tracks/2/stream', () => ({ status: 500 }))
    const video = fakeVideo()
    document.body.appendChild(video)
    const engine = createEngine(video, { id: 'a', file: 0 }, engineProbe(), fakeDeps(source, recorder))
    source.open()
    await settle()
    expect(engine.state(), 'a 500 was not an error').toBe('error')
    expect(recorder.callsFor('/tracks/1/stream')[0]?.signal.aborted, 'the first error did not stop the video').toBe(true)
    video.currentTime = 3
    await settle()
    const videoCalls = recorder.callsFor('/tracks/1/stream')
    expect(videoCalls.length, 'the seek did not restart the video').toBe(2)
    expect(recorder.callsFor('/tracks/2/stream').length, 'the seek did not restart the audio').toBe(2)
    expect(engine.state(), 'the second 500 was not an error').toBe('error')
    expect(videoCalls[1]?.signal.aborted, 'the second error did not stop the video stream').toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('a seek during an audio switch keeps the new track', async () => {
    // The person picks the Italian audio and seeks before the switch has re-typed the buffer.
    // The player shows the Italian track as chosen, so the restart must fetch it, with the
    // buffer re-typed for it, and must not bring the English track back.
    const t = await openEngine({ durationMs: 120_000 })
    for (let i = 0; i < 2; i += 1) {
      t.body(2).push(new Uint8Array([1]))
      await settle()
    }
    t.engine.setAudio(PROBE.tracks[2] as MediaTrack)
    t.video.currentTime = 20
    await settle()
    await settle()
    await settle()
    // The switch's own fetch went out and the seek aborted it; the restart fetches the new
    // track again, at the seek, and that fetch is the live one.
    const italian = t.recorder.callsFor('/tracks/3/stream')
    expect(italian.length, 'the restart did not fetch the new audio track').toBe(2)
    expect(italian[0]?.signal.aborted, 'the interrupted switch fetch was not aborted').toBe(true)
    expect(italian[1]?.url, 'the new track was not fetched at the seek').toContain('start_ms=19000')
    expect(italian[1]?.signal.aborted, 'the restart fetch of the new track was stopped').toBe(false)
    expect(t.recorder.callsFor('/tracks/2/stream').length, 'the restart brought the old track back').toBe(1)
    expect(t.audioBuffer.log.changeTypes, 'the buffer was not re-typed for the new track').toEqual([
      'audio/mp4; codecs="opus"',
    ])
    t.close()
  })

  it('a held chunk that hits quota again stays ahead of the next', async () => {
    // A (3 bytes) is held for quota and B (2 bytes) waits in the body. When the timer offers A
    // again, the buffer refuses it once more and evicts a range to make room; the `updateend` of
    // that eviction must not start a read of B in the middle of A's turn. A goes in, then B,
    // once each, and nothing is an error.
    const t = await openEngine({ durationMs: 120_000 })
    for (let i = 0; i < 3; i += 1) {
      t.body(1).push(new Uint8Array([1]))
      await settle()
    }
    t.video.currentTime = 20
    await settle()
    t.videoBuffer.makeNextAppendThrowQuota(2)
    t.body(1).push(new Uint8Array([7, 7, 7]))
    await settle()
    t.body(1).push(new Uint8Array([8, 8]))
    await settle()
    expect(t.videoBuffer.log.appends.length, 'A or B went in while A was held').toBe(3)
    // A range away from the playhead for the second eviction to cut, and one more refusal.
    setRanges(t.videoBuffer, [[50, 60]])
    t.videoBuffer.makeNextAppendThrowQuota(1)
    const opsBefore = t.videoBuffer.log.ops.length
    await sleep(1_100)
    await settle()
    await settle()
    expect(t.videoBuffer.log.removes.at(-1), 'the second eviction did not run').toEqual([50, 60])
    const sizes = t.videoBuffer.log.appends.slice(3).map((chunk) => chunk.byteLength)
    expect(sizes, 'A did not go in before B, once each').toEqual([3, 2])
    expect(
      t.videoBuffer.log.ops.slice(opsBefore),
      'B was read while A was being evicted for',
    ).toEqual(['remove', 'append', 'append'])
    expect(t.engine.state(), 'the held re-append ended in an error').not.toBe('error')
    t.close()
  })

  it('a seek back after the end keeps one buffer per track', async () => {
    // Both bodies reach the end and the source is ended. A seek back into data the buffer does
    // not hold restarts the streams, and their first appends reopen the source, which fires
    // `sourceopen` again. The engine must not set itself up a second time.
    const errors: unknown[] = []
    const onError = (event: ErrorEvent): void => {
      errors.push(event.error ?? event.message)
    }
    window.addEventListener('error', onError)
    try {
      const t = await openEngine({}, 1_000)
      for (let i = 0; i < 6; i += 1) {
        t.body(1).push(new Uint8Array([1]))
        t.body(2).push(new Uint8Array([1]))
        await settle()
      }
      t.body(1).end()
      t.body(2).end()
      await settle()
      expect(t.source.readyState, 'the source did not end').toBe('ended')
      // The front of the file is evicted: the seek to media 2 lands in no buffered range.
      setRanges(t.videoBuffer, [[4, 6]])
      setRanges(t.audioBuffer, [[4, 6]])
      const appendsBefore = t.videoBuffer.log.appends.length
      t.video.currentTime = 2
      await settle()
      expect(t.recorder.callsFor('/tracks/1/stream').length, 'the seek did not restart').toBe(2)
      t.body(1).push(new Uint8Array([1]))
      t.body(2).push(new Uint8Array([1]))
      await settle()
      await settle()
      expect(t.source.readyState, 'the append did not reopen the source').toBe('open')
      expect(t.source.activeSourceBuffers.length, 'the reopen added buffers').toBe(2)
      expect(t.videoBuffer.log.appends.length, 'the restart did not append').toBe(appendsBefore + 1)
      expect(errors, 'the second sourceopen threw').toEqual([])
      expect(t.engine.state(), 'the restart after the end is an error').not.toBe('error')
      t.close()
    } finally {
      window.removeEventListener('error', onError)
    }
  })

  it('switching audio after the end plays the new track to the end', async () => {
    // The file has ended. The person picks another audio track: the engine switches (the
    // switch reopens the source), reads the new track to the end, and ends the source again.
    const t = await openEngine()
    for (let i = 0; i < 6; i += 1) {
      t.body(1).push(new Uint8Array([1]))
      t.body(2).push(new Uint8Array([1]))
      await settle()
    }
    t.body(1).end()
    t.body(2).end()
    await settle()
    expect(t.source.readyState, 'the source did not end').toBe('ended')
    expect(t.engine.setAudio(PROBE.tracks[2] as MediaTrack), 'the switch after the end was refused').toBe(true)
    await settle()
    await settle()
    await settle()
    expect(t.recorder.callsFor('/tracks/3/stream').length, 'the new track was not fetched').toBe(1)
    expect(t.audioBuffer.log.changeTypes, 'the buffer was not re-typed').toEqual(['audio/mp4; codecs="opus"'])
    for (let i = 0; i < 6; i += 1) {
      t.body(3).push(new Uint8Array([1]))
      await settle()
    }
    t.body(3).end()
    await settle()
    expect(t.source.endOfStreamCalls.length, 'the new track did not end the source again').toBe(2)
    t.close()
  })

  it('switching audio after an error restarts both streams', async () => {
    // The English audio route fails: an error, and every stream is stopped. Picking the Italian
    // track is a full restart: the video is fetched again with it, and the state leaves the error.
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    recorder.answer('/tracks/1/stream', () => ({ startMs: 0, body: () => new FakeBody() }))
    recorder.answer('/tracks/2/stream', () => ({ status: 500 }))
    recorder.answer('/tracks/3/stream', () => ({ startMs: 0, body: () => new FakeBody() }))
    const video = fakeVideo()
    document.body.appendChild(video)
    const engine = createEngine(video, { id: 'a', file: 0 }, engineProbe(), fakeDeps(source, recorder))
    source.open()
    await settle()
    expect(engine.state(), 'a 500 was not an error').toBe('error')
    expect(engine.setAudio(PROBE.tracks[2] as MediaTrack), 'the switch after an error was refused').toBe(true)
    await settle()
    await settle()
    await settle()
    const videoCalls = recorder.callsFor('/tracks/1/stream')
    expect(videoCalls.length, 'the switch did not restart the video').toBe(2)
    expect(videoCalls[1]?.signal.aborted, 'the restarted video was stopped').toBe(false)
    expect(recorder.callsFor('/tracks/3/stream').length, 'the new track was not fetched').toBe(1)
    expect(engine.state(), 'the switch left the engine in the error').toBe('playing')
    engine.destroy()
    document.body.removeChild(video)
  })

  it('a later audio start keeps the old audio up to it', async () => {
    // The audio buffer holds [0, 5] and the playhead is at 1. The new track's body answers
    // `X-Fbtt-Start-Ms: 2500` (media 3.5), later than asked: the old audio is cut from 3.5, so
    // the playhead keeps the old track up to the new data rather than a gap.
    const t = await openEngine({ durationMs: 120_000 })
    t.recorder.answer('/tracks/3/stream', () => ({ startMs: 2500, body: () => new FakeBody() }))
    for (let i = 0; i < 5; i += 1) {
      t.body(2).push(new Uint8Array([1]))
      await settle()
    }
    t.engine.setAudio(PROBE.tracks[2] as MediaTrack)
    await settle()
    await settle()
    await settle()
    expect(t.audioBuffer.log.removes, 'the old audio was not cut where the new body starts').toEqual([[3.5, 5]])
    expect(t.audioBuffer.log.changeTypes.length, 'the buffer was not re-typed').toBe(1)
    t.close()
  })

  it('a refused audio switch keeps the selection', async () => {
    // A source that never opens: the engine has no audio buffer, so it refuses the switch, and
    // the Audio menu must not show the Italian track as chosen while the English one plays.
    class NeverOpens extends FakeMediaSource {
      static isTypeSupported(mime: string): boolean {
        return /avc1|mp4a|opus/.test(mime)
      }
      open(): void {}
    }
    vi.stubGlobal('MediaSource', NeverOpens)
    vi.stubGlobal('fetch', fakeFetch().fetch)
    const { client } = probeClient()
    const { container } = render(<Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />)
    await settle()
    fireEvent.keyDown(container.querySelector('dialog') as HTMLElement, { key: 'a' })
    await settle()
    fireEvent.click(screen.getByRole('button', { name: 'Audio' }))
    await settle()
    const chosen = screen
      .getAllByRole('option')
      .filter((option) => option.getAttribute('aria-selected') === 'true')
      .map((option) => option.textContent ?? '')
    expect(
      chosen.some((text) => text.startsWith('ita')),
      `a refused switch showed the new track as chosen (${JSON.stringify(chosen)})`,
    ).toBe(false)
  })
})

describe('the final review fixes', () => {
  it('a failed resume keeps the playhead', async () => {
    // The video body stops early; its resume gets a 504, the normal answer of a slow torrent.
    // The next try must only ask again from where the buffer ends: a restart of the whole start
    // would move the playhead to the far end of the buffered range and skip what lies between.
    const t = await openEngine({ durationMs: 120_000 })
    for (let i = 0; i < 2; i += 1) {
      t.body(1).push(new Uint8Array([1]))
      await settle()
    }
    expect(t.video.currentTime, 'the start did not land at media 1').toBe(1)
    let busy = true
    t.recorder.answer('/tracks/1/stream', () =>
      busy ? { status: 504 } : { startMs: 1000, body: () => new FakeBody() },
    )
    vi.useFakeTimers()
    try {
      t.body(1).end()
      await vi.advanceTimersByTimeAsync(1_100)
      expect(t.recorder.callsFor('/tracks/1/stream').length, 'the early end was not retried').toBe(2)
      busy = false
      await vi.advanceTimersByTimeAsync(2_100)
      const calls = t.recorder.callsFor('/tracks/1/stream')
      expect(calls.length, 'the failed resume was not asked again').toBe(3)
      expect(calls[2]?.url, 'the resume did not ask from the buffered end').toContain('start_ms=1000')
      expect(t.video.currentTime, 'the failed resume moved the playhead').toBe(1)
      expect(t.recorder.callsFor('/tracks/2/stream').length, 'the failed resume restarted the audio').toBe(1)
    } finally {
      vi.useRealTimers()
    }
    t.close()
  })

  it('a busy answer after a seek fetches the new position', async () => {
    // The seek's audio follow-up gets a 503. Its retry must ask for the new position, not resume
    // the range the old body fed at the old position.
    const t = await openEngine({ durationMs: 120_000 })
    for (let i = 0; i < 2; i += 1) {
      t.body(2).push(new Uint8Array([1]))
      await settle()
    }
    let busy = true
    t.recorder.answer('/tracks/2/stream', () =>
      busy ? { status: 503 } : { startMs: 19_000, body: () => new FakeBody() },
    )
    t.video.currentTime = 20
    await settle()
    const asked = t.recorder.callsFor('/tracks/2/stream')
    expect(asked.length, 'the seek did not ask for the audio').toBe(2)
    expect(asked[1]?.url, 'the seek did not ask for the audio at the seek').toContain('start_ms=19000')
    busy = false
    await sleep(1_100)
    await settle()
    const calls = t.recorder.callsFor('/tracks/2/stream')
    expect(calls.length, 'the busy answer was not retried').toBe(3)
    expect(calls[2]?.url, 'the retry went back to the old position').toContain('start_ms=19000')
    t.close()
  })

  it('a busy answer during an audio switch starts at the playhead', async () => {
    // The new audio track's first body gets a 503. The buffer still holds the old track to media
    // 3; the retry must ask for the new track at the playhead, not at the old track's range end,
    // which would leave the old language playing up to it.
    const t = await openEngine({ durationMs: 120_000 })
    for (let i = 0; i < 3; i += 1) {
      t.body(2).push(new Uint8Array([1]))
      await settle()
    }
    let busy = true
    t.recorder.answer('/tracks/3/stream', () =>
      busy ? { status: 503 } : { startMs: 0, body: () => new FakeBody() },
    )
    expect(t.engine.setAudio(PROBE.tracks[2] as MediaTrack), 'the switch was refused').toBe(true)
    await settle()
    expect(t.recorder.callsFor('/tracks/3/stream').length, 'the switch did not ask for the new track').toBe(1)
    busy = false
    await sleep(1_100)
    await settle()
    await settle()
    const calls = t.recorder.callsFor('/tracks/3/stream')
    expect(calls.length, 'the busy answer was not retried').toBe(2)
    expect(calls[1]?.url, 'the retry of the switch did not start at the playhead').toContain('start_ms=0')
    expect(t.audioBuffer.log.removes, 'the old track was not cut at the playhead').toEqual([[1, 3]])
    t.close()
  })

})
