/**
 * Which tracks the page will play, and which it will not.
 *
 * The browser is the only authority on what it decodes, so the decision is made against two
 * questions the page asks it: `supported(mime)` for the remuxed tracks (can a `SourceBuffer` take
 * this mime) and `canPlay(mime)` for the native ones (can the element play this container). A remux
 * is one HTTP stream per track, so the video and the audio are decided separately, and an audio the
 * browser cannot play is listed, not dropped: the chooser shows it as unavailable rather than hiding
 * the choice that exists (plan ruling 3: H.264, HEVC and AV1 video; AAC, Opus, FLAC, MP3, AC-3 and
 * E-AC-3 audio, each playing where the browser decodes it).
 *
 * WebM with VP8 or VP9 plays natively through the existing stream route, and that is the whole of
 * its story: no audio choice, no embedded subtitles, and no remux at all. Anything else the browser
 * cannot remux is not played here; the screen says so and offers the file for download.
 *
 * @module player/tracks
 */
import type { MediaProbe, MediaTrack } from '../api/client'

/** What the player will do with a probed file, decided once when it opens. */
export type TrackChoice =
  | {
      kind: 'remux'
      video: MediaTrack
      videoMime: string
      audio: MediaTrack | null
      audioMime: string | null
      /** Every audio track, in order, with whether this browser decodes it. */
      audioOptions: { track: MediaTrack; playable: boolean }[]
    }
  | { kind: 'native' }
  | { kind: 'unplayable'; codec: string }

/** The codecs a WebM plays natively. */
const NATIVE_CODECS = ['vp8', 'vp9'] as const

/**
 * The first of a track's `mime_codecs` the browser takes, or null when it takes none: the remuxed
 * mime is named in the server's own order, and the first one the browser accepts is the one to ask
 * for.
 */
function remuxMime(
  track: MediaTrack,
  supported: (mime: string) => boolean,
): string | null {
  const prefix = track.kind === 'video' ? 'video/mp4; codecs="': 'audio/mp4; codecs="'
  for (const codec of track.mime_codecs) {
    const mime = `${prefix}${codec}"`
    if (supported(mime)) return mime
  }
  return null
}

/** Whether a WebM track plays through the element itself, unremuxed. */
function nativeVideo(probe: MediaProbe, canPlay: (mime: string) => boolean): MediaTrack | null {
  if (probe.container !== 'webm' || !canPlay('video/webm')) return null
  const video = probe.tracks.find((track) => track.kind === 'video')
  if (video === undefined || !NATIVE_CODECS.includes(video.codec as (typeof NATIVE_CODECS)[number])) {
    return null
  }
  return video
}

/**
 * The decision for a probed file. `preferredAudio` is the track the person chose last time, and it
 * wins when the browser still plays it.
 */
export function chooseTracks(
  probe: MediaProbe,
  supported: (mime: string) => boolean,
  canPlay: (mime: string) => boolean,
  preferredAudio?: number,
): TrackChoice {
  if (nativeVideo(probe, canPlay) !== null) return { kind: 'native' }

  const video = probe.tracks.find((track) => track.kind === 'video')
  if (video === undefined) {
    return { kind: 'unplayable', codec: 'video' }
  }

  const videoMime = remuxMime(video, supported)
  if (videoMime === null) {
    return { kind: 'unplayable', codec: video.codec }
  }

  const audios = probe.tracks.filter((track) => track.kind === 'audio')
  const audioOptions = audios.map((track) => ({ track, playable: remuxMime(track, supported) !== null }))

  const pick = (id: number | undefined): MediaTrack | null => {
    if (id === undefined) return null
    const option = audioOptions.find((entry) => entry.track.id === id)
    return option?.playable === true ? option.track : null
  }

  const audio =
    pick(preferredAudio) ??
    pick(audios.find((track) => track.default)?.id) ??
    audioOptions.find((entry) => entry.playable)?.track ??
    null
  const audioMime = audio === null ? null : remuxMime(audio, supported)

  return { kind: 'remux', video, videoMime, audio, audioMime, audioOptions }
}
