// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import Files from '../components/detail/Files'
import type { FileInfo, MediaProbe, MediaTrack } from '../api/client'
import { createEngine } from './engine'
import { installDialogShim } from '../dialog-shim'
import Player from './Player'
import {
  FakeBody,
  FakeMediaSource,
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
beforeEach(() => {
  objectUrlCount = 0
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
      const url = `blob:fake/${objectUrlCount}`
      objectUrlCount += 1
      objectUrls.push(url)
      return url
    },
    revokeObjectURL: (url: string) => {
      revokeObjectUrls.push(url)
    },
  })
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
function probeClient(probe: MediaProbe = PROBE): {
  client: import('../api/client').Api
  tokenCalls(): number
} {
  let tokenCalls = 0
  const client = {
    probe: async () => probe,
    cues: async () => ({ cues: [] }),
    subtitleFile: async () => ({ cues: [] }),
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

/** Flush the microtask queue without a timer: for the fake-timer test, where a `setTimeout`
 * would sit faked and never resolve. */
async function flushMicrotasks(): Promise<void> {
  await act(async () => {
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()
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
    const { client, tokenCalls } = probeClient(unplayable)
    render(<Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />)
    await settle()

    expect(
      screen.getByText('This browser cannot play hevc video.') !== null,
      'the unplayable player did not say which codec it cannot play',
    ).toBe(true)
    expect(tokenCalls(), 'the token was fetched before the click').toBe(0)
    fireEvent.click(screen.getByRole('button', { name: 'Download' }))
    await settle()
    expect(tokenCalls(), 'Download did not ask for a stream token').toBe(1)
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

    // The duration is the probe's, mapped: 4000 ms of source time is 3 s of media time.
    expect(source.duration === 3, 'the source did not take the probe duration').toBe(true)

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

    // The server's `X-Fbtt-Start-Ms: 2000` is where the playhead lands: 2000 ms of source time
    // is 1 s of media time, and the offset is 1000, so the element reads 1. The test's offset
    // makes the body's 2000 the element's 1.
    expect(video.currentTime === 1, `the playhead did not move to the start header (got ${video.currentTime})`).toBe(true)
    engine.destroy()
    document.body.removeChild(video)
  })

  it('a stream that ends early shows Waiting for data and retries', async () => {
    vi.useFakeTimers()
    try {
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
      await flushMicrotasks()
      // The init segment lands, then the body ends.
      firstBody.push(new Uint8Array([1, 2, 3]))
      firstBody.end()
      await flushMicrotasks()

      // The engine is waiting, and it will be asked again in a second.
      expect(engine.state(), 'an early end did not move the engine to waiting').toBe('waiting')
      const before = recorder.callsFor('/tracks/1/stream').length
      await vi.advanceTimersByTimeAsync(1_000)
      await flushMicrotasks()
      const after = recorder.callsFor('/tracks/1/stream').length
      expect(after - before === 1, 'the early end was not retried after a second').toBe(true)
      // The retry goes from the end of what the buffer has, not from the start. The buffer has one
      // media second, which is source time 2000 ms with the 1000 ms offset.
      const retry = recorder.callsFor('/tracks/1/stream')[after - 1]
      expect(retry.url.includes('start_ms=2000'), `the retry did not start at the buffered end (${retry.url})`).toBe(true)

      engine.destroy()
      document.body.removeChild(video)
    } finally {
      vi.useRealTimers()
    }
  })

  it('keyboard', async () => {
    const recorder = fakeFetch()
    // The Player's engine reads `window.fetch` (it is given no deps), so the test's fake fetch is
    // put on `window` for the duration of the test: the engine's audio-switch fetch lands in the
    // recorder the test reads back.
    vi.stubGlobal('fetch', recorder.fetch)
    const { client } = probeClient()
    const { container } = render(
      <Player id="a" file={0} name="movie.mkv" client={client} onClose={() => {}} />,
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
    // browser fires on Escape and reads that the handler reached and prevented it.
    const cancel = new Event('cancel', { bubbles: true, cancelable: true })
    dialog.dispatchEvent(cancel)
    expect(
      cancel.defaultPrevented,
      'Escape did not reach the dialog close handler and prevent the element closing itself',
    ).toBe(true)
  })
})
