/**
 * `chooseTracks` against the probes the plan names, with browser support faked the way the browsers
 * have it: Chrome plays everything the remux offers but not AC-3; HEVC is refused where the browser
 * refuses it; a WebM of VP9 plays through the element itself.
 *
 * @module player/tracks.test
 */
import { expect, it } from 'vitest'
import { chooseTracks } from './tracks'
import type { MediaProbe, MediaTrack } from '../api/client'

/** The probes the tests build from. */
function track(overrides: Partial<MediaTrack> & Pick<MediaTrack, 'id' | 'kind' | 'codec'>): MediaTrack {
  return {
    mime_codecs: [`${overrides.codec}.64000A`],
    language: null,
    name: null,
    default: false,
    forced: false,
    text: false,
    ...overrides,
  }
}

/** A Matroska the way the probe answers one, with a track list the test fills. */
function probe(overrides: Partial<Omit<MediaProbe, 'container'>> & { container?: MediaProbe['container'] }): MediaProbe {
  return {
    container: 'matroska',
    duration_ms: 4000,
    time_offset_ms: 1000,
    tracks: [],
    external_subtitles: [],
    ...overrides,
  }
}

/** What a Chrome-like browser takes: every remuxed mime except the AC-3 ones. */
const chromeSupports = (mime: string): boolean => !mime.includes('ac-3')
/** A browser that does not decode HEVC: it takes every remuxed mime but the HEVC ones. */
const noHevcSupports = (mime: string): boolean => !mime.includes('hvc1') && !mime.includes('hev1')
/** What a HEVC-blind browser plays natively: nothing. */
const canPlayNothing = (): boolean => false
/** What a browser plays natively: only a WebM. */
const canPlayWebm = (mime: string): boolean => mime === 'video/webm'

const h264Video = track({ id: 1, kind: 'video', codec: 'h264', mime_codecs: ['avc1.64000A'], default: true })
const ac3Audio = track({ id: 2, kind: 'audio', codec: 'ac-3', mime_codecs: ['ac-3'] })
const opusAudio = track({ id: 3, kind: 'audio', codec: 'opus', mime_codecs: ['opus'], default: true })
const mp3Audio = track({ id: 4, kind: 'audio', codec: 'mp3', mime_codecs: ['mp4a.40.29'] })

it('remuxes_with_the_audio_the_browser_cannot_play_as_null', () => {
  const choice = chooseTracks(probe({ tracks: [h264Video, ac3Audio] }), chromeSupports, canPlayNothing)
  expect(choice.kind, 'the remux was not chosen where the browser plays the video').toBe('remux')
  if (choice.kind !== 'remux') return
  expect(choice.videoMime, 'the video mime was not the remux form').toBe('video/mp4; codecs="avc1.64000A"')
  expect(choice.audio, 'an unplayable audio track was chosen to play').toBeNull()
  expect(choice.audioMime, 'an unplayable audio track was given a mime').toBeNull()
  expect(choice.audioOptions, 'the audio options were not listed').toEqual([{ track: ac3Audio, playable: false }])
})

it('refuses_a_video_codec_the_browser_cannot_remux', () => {
  const hevcVideo = track({ id: 1, kind: 'video', codec: 'hevc', mime_codecs: ['hvc1.1.6.L93.90'], default: true })
  const choice = chooseTracks(probe({ tracks: [hevcVideo, opusAudio] }), noHevcSupports, canPlayNothing)
  expect(choice, 'HEVC without HEVC support was not named unplayable').toEqual({ kind: 'unplayable', codec: 'hevc' })
})

it('plays_a_vp9_webm_natively', () => {
  const vp9 = track({ id: 1, kind: 'video', codec: 'vp9', mime_codecs: [], default: true })
  const webm = track({ id: 2, kind: 'audio', codec: 'opus', mime_codecs: ['opus'], default: true })
  const choice = chooseTracks(probe({ container: 'webm', tracks: [vp9, webm] }), chromeSupports, canPlayWebm)
  expect(choice, 'a VP9 WebM was not played natively').toEqual({ kind: 'native' })
})

it('refuses_a_webm_the_browser_cannot_play_natively', () => {
  const av1 = track({ id: 1, kind: 'video', codec: 'av01', mime_codecs: [], default: true })
  const choice = chooseTracks(probe({ container: 'webm', tracks: [av1] }), chromeSupports, canPlayWebm)
  expect(choice, 'a WebM outside the native codecs was not claimed native').toEqual({
    kind: 'unplayable',
    codec: 'av01',
  })
})

it('prefers_the_audio_track_the_person_chose', () => {
  const tracks = [h264Video, opusAudio, mp3Audio]
  const choice = chooseTracks(probe({ tracks }), chromeSupports, canPlayNothing, 4)
  if (choice.kind !== 'remux') throw new Error(`not a remux: ${JSON.stringify(choice)}`)
  expect(choice.audio?.id, 'the preferred track was not chosen').toBe(4)
  expect(choice.audioMime).toBe('audio/mp4; codecs="mp4a.40.29"')

  // The default is chosen when no preference was given, and the first playable one when there is no default either.
  const defaulted = chooseTracks(probe({ tracks }), chromeSupports, canPlayNothing)
  if (defaulted.kind !== 'remux') throw new Error(`not a remux: ${JSON.stringify(defaulted)}`)
  expect(defaulted.audio?.id, 'the default track was not chosen by default').toBe(3)

  const noDefault = chooseTracks(probe({ tracks: [h264Video, mp3Audio] }), chromeSupports, canPlayNothing)
  if (noDefault.kind !== 'remux') throw new Error(`not a remux: ${JSON.stringify(noDefault)}`)
  expect(noDefault.audio?.id, 'the first playable track was not chosen where there is no default').toBe(4)
})
