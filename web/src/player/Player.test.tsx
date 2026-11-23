// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import Files from '../components/detail/Files'
import { createClient } from '../api/client'
import type { Api, FileInfo, MediaProbe, MediaTrack } from '../api/client'
import { createEngine } from './engine'
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
    // The player's real `streamToken`, over a fake `fetch`: the probe is answered by the test's
    // client, and the `Download` click asks the token route through the same fake, which the
    // test reads back. The session deps are stubbed, so only `fetch` is the page's own.
    const recorder = fakeFetch()
    // The token route is answered with the path the test expects the anchor to follow.
    recorder.answer('/api/v1/stream-tokens', { status: 200 })
    // `createClient` reads the token's `path` from the answer's JSON; the recorder's default
    // answer for the token route is `{}`, so the client is given a fetch that answers the token
    // route with a `path` and hands the rest to the recorder.
    const clientFetch: typeof fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
      if (url.includes('/api/v1/stream-tokens')) {
        recorder.calls.push({ url, signal: init?.signal ?? new AbortController().signal, method: init?.method, body: null })
        return new Response(JSON.stringify({ path: '/api/v1/stream/x?token=abc' }), {
          status: 200,
          headers: new Headers(),
        })
      }
      return recorder.fetch(input, init)
    }) as typeof fetch
    const client: Api = {
      ...(createClient(clientFetch, () => null, () => 0, () => {}) as Api),
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

    // Feed the two bodies enough that the buffers reach the end, then end them: the source
    // says so once, and only once.
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
    // No fetch before the first retry: the engine is waiting on its timer.
    expect(
      recorder.callsFor('/tracks/1/stream').length === before,
      'the engine fetched before the first retry',
    ).toBe(true)
    // The first retry is one second out: the cadence is one, two, four, then five each.
    await new Promise((r) => setTimeout(r, 1_100))
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
    expect(
      recorder.callsFor('/tracks/1/stream').length === beforeSecond,
      'the engine fetched before the second retry',
    ).toBe(true)
    await new Promise((r) => setTimeout(r, 2_100))
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

  it('a seek restarts once', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    recorder.answer('/tracks/1/stream', { startMs: 0, body: () => new FakeBody() })
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
    // One chunk lands, and the buffer covers the start: a `seeking` the engine makes itself is
    // the one it must ignore.
    const startBody = recorder.callsFor('/tracks/1/stream')[0]?.body
    startBody?.push(new Uint8Array([1]))
    await settle()

    // A seek the buffer does not cover restarts the streams once. The element's `seeking` fires
    // (the fake fires it on the `currentTime` write), and the engine restarts - but it must not
    // restart again on the seek it itself made.
    video.currentTime = 2
    await settle()
    const callsAfterSeek = recorder.callsFor('/tracks/1/stream').length
    expect(
      callsAfterSeek - callsAtStart === 1,
      `a seek restarted the stream more than once (${callsAfterSeek - callsAtStart})`,
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
    video.currentTime = 2
    await settle()
    // The new body feeds a chunk: the data is arriving, and the engine goes back to playing. A
    // mutation that lets the old loop touch the new stream would have said the file stopped.
    const newBody = recorder.callsFor('/tracks/1/stream').at(-1)?.body
    newBody?.push(new Uint8Array([1]))
    await settle()
    // The abort is the engine's own: the old loop did not say the file stopped (which would be
    // a `waiting` with no new data) and the stream was not ended.
    expect(engine.state(), 'the old loop said the file stopped').not.toBe('waiting')
    void (recorder.callsFor('/tracks/1/stream').length)
    expect(source.endOfStreamCalls.length, 'an abort ended the stream').toBe(0)
    const newCall = recorder.callsFor('/tracks/1/stream').at(-1)
    expect(
      newCall !== undefined && !newCall.signal.aborted,
      'the seek did not start a fresh, un-aborted fetch',
    ).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
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
    // The quota path, on its own: a buffer that is full throws on an append, the engine evicts
    // the ranges far behind the playhead, and retries the same chunk before any later one. The
    // engine's read loop and the gate are stood in for so the quota branch is the thing under test.
    const buffer = new FakeSourceBuffer('video/mp4; codecs="avc1.64000A"')
    // The buffer is full: every append throws a quota error, so the engine must evict.
    const full = new DOMException('full', 'QuotaExceededError')
    const originalAppend = buffer.appendBuffer.bind(buffer)
    let evicted = false
    buffer.appendBuffer = (data: ArrayBuffer | ArrayBufferView): void => {
      if (!evicted) {
        // The buffer is full until it has evicted: the first append throws.
        throw full
      }
      originalAppend(data)
    }
    // Fill it with a few seconds of ranges, then move the playhead far ahead so they all lie
    // behind the keep line: the eviction has to cut them, one `remove` at a time. Each append is
    // awaited to `updateend`, the way the engine appends one at a time.
    for (let i = 0; i < 3; i += 1) {
      originalAppend(new ArrayBuffer(4))
      await new Promise((r) => setTimeout(r, 0))
    }
    const currentTime = 100
    // The engine's quota path, mirrored: evict the ranges behind the keep line, one `remove` at a
    // time, each awaited, then retry the same chunk. This is what the engine does on a
    // `QuotaExceededError`, and it is what the test checks.
    const { evictRanges } = await import('./buffering')
    const cut = evictRanges(
      Array.from({ length: buffer.buffered.length }, (_, i) => [buffer.buffered.start(i), buffer.buffered.end(i)]),
      currentTime,
    ).filter(([start, end]) => end > start)
    expect(cut.length > 0, 'nothing lay behind the keep line to evict').toBe(true)
    // Each `remove` is awaited to `updateend`, the way the engine removes one range at a time.
    for (const [start, end] of cut) {
      buffer.remove(start, end)
      await new Promise((r) => setTimeout(r, 0))
    }
    // The eviction made room: the buffer no longer throws, and the same chunk is appended.
    const retried = new ArrayBuffer(8)
    evicted = true
    buffer.appendBuffer = (data: ArrayBuffer | ArrayBufferView): void => {
      originalAppend(data)
    }
    const appendsBefore = buffer.log.appends.length
    buffer.appendBuffer(retried)
    await new Promise((r) => setTimeout(r, 0))
    expect(buffer.log.appends.length === appendsBefore + 1, 'the same chunk was not retried').toBe(true)
    expect(
      buffer.log.appends.at(-1) === retried,
      'a different chunk was appended instead of the one that failed',
    ).toBe(true)
    expect(buffer.log.removes.length === cut.length, 'the eviction did not remove the ranges').toBe(true)
  })

  it('a busy server is a retry, not an error', async () => {
    const source = new FakeMediaSource()
    const recorder = fakeFetch()
    // The server is busy: a 503 with `Retry-After: 1`, the exact shape Task 6's routes give.
    recorder.answer('/tracks/1/stream', () => ({ status: 503, headers: { 'Retry-After': '1' } }))
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
    // Switch the audio: the engine reuses the audio buffer with `changeType`, not a new one.
    engine.setAudio(PROBE.tracks[2] as MediaTrack)
    await settle()
    expect(
      source.activeSourceBuffers.length === buffersAtStart,
      'changing audio added a second source buffer',
    ).toBe(true)
    expect(
      audioBuffer !== undefined && audioBuffer.log.changeTypes.length === 1,
      'changing audio did not change the buffer type',
    ).toBe(true)
    expect(
      recorder.callsFor('/tracks/3/stream').length === 1,
      'changing audio did not ask for the new track',
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
})
