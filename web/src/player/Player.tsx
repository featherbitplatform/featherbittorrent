/**
 * The player: the modal that plays one file of one torrent in the browser.
 *
 * It asks the server for the probe, decides what the browser will do with it (`chooseTracks`),
 * and then one of three things is on screen: the remuxed video fed by the engine with the
 * controls around it, the same file through the server's plain stream route when the browser
 * plays it on its own, or the one honest sentence about a codec the browser cannot decode, with
 * the file offered for download. The position the person left the file at is remembered the way
 * `positions.ts` says, and it is where the player starts when it is there.
 *
 * Every control is a real button with an `aria-label`, the seek bar is a range input in media
 * time, and the keyboard drives the player the way the plan spells it: Space and `k` for play
 * and pause, the arrows for time and volume, `f` for fullscreen, `m` for mute, `a` for the next
 * audio, `c` for the next subtitle, and Escape for the way out.
 *
 * @module player/Player
 */
import { useEffect, useMemo, useRef, useState } from 'react'
import type { KeyboardEvent, ReactNode } from 'react'
import {
  Download,
  Maximize,
  Pause,
  Play,
  RotateCcw,
  RotateCw,
  Volume2,
  VolumeX,
  X,
} from 'lucide-react'
import { api } from '../api/client'
import type { Api, Cue, MediaProbe, MediaTrack } from '../api/client'
import { displaySafe } from '../format'
import { useModalDialog } from '../modal'
import { mergeCues, nextWindow } from './cues'
import { createEngine } from './engine'
import type { Engine, EngineState } from './engine'
import { loadPosition, savePosition } from './positions'
import { chooseTracks } from './tracks'
import type { TrackChoice } from './tracks'
import { toMedia, toSource } from './time'

export interface PlayerProps {
  /** The torrent the file belongs to, addressed the way every other call addresses it. */
  id: string
  /** The file's index in the torrent. */
  file: number
  /** The file's name, shown over the video. It is remote text; `displaySafe` runs here. */
  name: string
  /** The client to read through. Defaults to the page's one so tests can hand in a fake. */
  client?: Api
  onClose(): void
}

/** A choice the menus list: Off first, then the tracks and the external files that can be shown. */
interface SubtitleOption {
  key: string
  text: string
  disabled?: boolean
}

/** The engine's word for a stream that has stopped: the line the player draws over the video. */
const WAITING_COPY = 'Waiting for data...'
/** The one sentence a browser that cannot decode the video gets, with the codec in it. */
function unplayableCopy(codec: string): string {
  return `This browser cannot play ${codec} video.`
}

/** The words a track is listed under: its name when it has one, its language when it does not. */
function trackLabel(track: MediaTrack): string {
  if (track.name !== null && track.name !== '') return track.name
  return track.language ?? 'Audio'
}

/** Whether a mime is one the browser's `SourceBuffer` will take. */
function remuxSupported(mime: string): boolean {
  try {
    return typeof MediaSource !== 'undefined' && MediaSource.isTypeSupported(mime)
  } catch {
    return false
  }
}

/** Whether the element itself plays a mime, for the native path. */
function nativeSupported(mime: string): boolean {
  const probe = document.createElement('video')
  return probe.canPlayType(mime) !== ''
}

/** The `lang` a `TextTrack` wants from a language, or an empty string when there is none. */
function langOf(language: string | null): string {
  return language ?? ''
}

/** The seconds one nudge of the arrows moves the playhead. */
const SEEK_STEP = 5
/** The fraction one nudge of the volume arrows moves the volume. */
const VOLUME_STEP = 0.1

/** The time a person is looking at, `1:23`: whole minutes and zero-padded seconds. */
function clock(seconds: number): string {
  const whole = Number.isFinite(seconds) ? Math.max(0, Math.floor(seconds)) : 0
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, '0')}`
}

/**
 * The player's menu: a button that opens a `listbox`, and the listbox is keyboard-navigable,
 * the arrows moving the cursor, Enter and Space choosing, Escape closing and returning the focus
 * to the button. The items are real buttons, so Tab walks them in order.
 */
function ChoiceMenu({
  label,
  options,
  selected,
  onChoose,
}: {
  label: string
  options: SubtitleOption[]
  selected: string
  onChoose(key: string): void
}) {
  const [open, setOpen] = useState(false)
  const buttonRef = useRef<HTMLButtonElement>(null)
  const listRef = useRef<HTMLDivElement>(null)

  function close(): void {
    setOpen(false)
    buttonRef.current?.focus()
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (!open) {
      if (event.key === 'ArrowDown' || event.key === 'Enter' || event.key === ' ') {
        event.preventDefault()
        setOpen(true)
      }
      return
    }
    const items = Array.from(
      listRef.current?.querySelectorAll<HTMLElement>('[role="option"]:not([disabled])') ?? [],
    )
    if (items.length === 0) return
    const current = items.findIndex((item) => item.getAttribute('aria-selected') === 'true')
    if (event.key === 'Escape') {
      event.preventDefault()
      close()
      return
    }
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault()
      const key = items[current === -1 ? 0 : current]?.getAttribute('data-key')
      if (key !== null && key !== undefined) onChoose(key)
      close()
      return
    }
    let next: number
    if (event.key === 'ArrowDown') next = (current + 1) % items.length
    else if (event.key === 'ArrowUp') next = (current - 1 + items.length) % items.length
    else if (event.key === 'Home') next = 0
    else if (event.key === 'End') next = items.length - 1
    else return
    event.preventDefault()
    items[next]?.focus()
  }

  return (
    <div className="relative" onKeyDown={onKeyDown}>
      <button
        ref={buttonRef}
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => (open ? close() : setOpen(true))}
        className={`flex h-8 cursor-pointer items-center rounded-control px-2.5 text-[13px] ${
          open ? 'bg-accent-soft text-violet-300' : 'bg-transparent text-body hover:text-strong'
        }`}
      >
        {label}
      </button>
      {open && (
        <div
          ref={listRef}
          role="listbox"
          aria-label={label}
          className="absolute bottom-full z-10 mb-1 max-h-72 min-w-44 overflow-auto rounded-card border border-rule bg-raised p-1 shadow-[0_16px_40px_rgba(0,0,0,0.5)]"
        >
          {options.map((option) => (
            <button
              key={option.key}
              type="button"
              role="option"
              data-key={option.key}
              aria-selected={option.key === selected}
              disabled={option.disabled === true}
              tabIndex={0}
              onClick={() => {
                if (option.disabled === true) return
                onChoose(option.key)
                close()
              }}
              className={`block w-full cursor-pointer rounded-control px-2.5 py-1.5 text-left text-[13px] ${
                option.disabled === true
                  ? 'cursor-default text-muted'
                  : option.key === selected
                    ? 'bg-accent-soft text-violet-300'
                    : 'text-body hover:bg-ink-700 hover:text-strong'
              }`}
            >
              {option.text}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

/** The little icon button every control is built from. */
function IconButton({
  label,
  onClick,
  children,
}: {
  label: string
  onClick(): void
  children: ReactNode
}) {
  return (
    <button
      type="button"
      aria-label={label}
      onClick={onClick}
      className="flex h-8 w-8 cursor-pointer items-center justify-center rounded-control text-body hover:bg-ink-700 hover:text-strong"
    >
      {children}
    </button>
  )
}

export default function Player({ id, file, name, client = api, onClose }: PlayerProps) {
  const [probe, setProbe] = useState<MediaProbe | null>(null)
  const [probeError, setProbeError] = useState<string | null>(null)
  const [state, setState] = useState<EngineState>('loading')
  const [paused, setPaused] = useState(true)
  const [muted, setMuted] = useState(false)
  const [volume, setVolume] = useState(1)
  const [time, setTime] = useState(0)
  const [audioKey, setAudioKey] = useState<string | null>(null)
  const [subtitleKey, setSubtitleKey] = useState('off')
  const [nativeSrc, setNativeSrc] = useState<string | null>(null)
  const [downloading, setDownloading] = useState(false)

  const videoRef = useRef<HTMLVideoElement | null>(null)
  const engineRef = useRef<Engine | null>(null)
  const dialogRef = useRef<HTMLDialogElement | null>(null)
  // The cues each track has already been given: a window fetched again for the same second must
  // not double the cues the `TextTrack` already shows.
  const cueKeysRef = useRef<Map<string, Set<string>>>(new Map())
  // The cue windows each embedded track has already been asked for.
  const fetchedRef = useRef<Map<number, [number, number][]>>(new Map())
  // The external files fetched once, by their index.
  const externalDoneRef = useRef<Set<number>>(new Set())
  // The position the probe's answer will start at, in media seconds, decided once per probe.
  const startRef = useRef(0)

  const offsetMs = probe?.time_offset_ms ?? 0
  const durationMs = probe?.duration_ms ?? null
  const durationSec = durationMs === null ? 0 : toMedia(durationMs, offsetMs)

  // The probe, once: the player is opened for one file and asks for it once.
  useEffect(() => {
    let cancelled = false
    void (async () => {
      try {
        const answer = await client.probe(id, file)
        if (!cancelled) setProbe(answer)
      } catch (cause) {
        if (!cancelled) setProbeError(displaySafe(cause instanceof Error ? cause.message : 'the probe failed'))
      }
    })()
    return () => {
      cancelled = true
    }
  }, [client, id, file])

  // The decision, derived from the probe: what the browser will do with this file. It is a memo,
  // not an effect, because it is a pure function of the probe and the browser's own answer.
  const choice = useMemo<TrackChoice | null>(
    () => (probe === null ? null : chooseTracks(probe, remuxSupported, nativeSupported)),
    [probe],
  )

  // Where to start, when the probe lands: the remembered position, or the beginning. It is an
  // effect because it reads `localStorage`, which is not a pure function of the probe.
  useEffect(() => {
    if (probe === null) return
    const remembered = loadPosition(id, file)
    const limit = probe.duration_ms === null ? null : toMedia(probe.duration_ms, probe.time_offset_ms)
    startRef.current =
      remembered !== null && (limit === null || remembered < limit) ? remembered : 0
  }, [probe, id, file])

  // The native stream's token, fetched when the player opens the file through it. A token is good
  // for 60 seconds and the player opens the file once, so the token and the file arrive together.
  useEffect(() => {
    if (choice?.kind !== 'native') return
    let cancelled = false
    void (async () => {
      try {
        const path = await client.streamToken(id, file)
        if (!cancelled) setNativeSrc(path)
      } catch {
        // The token failed: the native video simply does not play, and the failure is the server's.
      }
    })()
    return () => {
      cancelled = true
    }
  }, [choice, client, id, file])

  // The engine, for the remux. It owns the source and the buffers; the element's transport events
  // are read here, and the position is saved on its clock.
  useEffect(() => {
    if (choice?.kind !== 'remux' || probe === null) return
    const video = videoRef.current
    if (video === null) return

    const start = startRef.current
    const engine = createEngine(
      video,
      { id, file },
      {
        offsetMs: probe.time_offset_ms,
        durationMs: probe.duration_ms,
        video: choice.video,
        videoMime: choice.videoMime,
        audio: choice.audio,
        audioMime: choice.audioMime,
        mimeFor: (track: MediaTrack): string | null => {
          for (const codec of track.mime_codecs) {
            const mime = `audio/mp4; codecs="${codec}"`
            if (remuxSupported(mime)) return mime
          }
          return null
        },
      },
    )
    engineRef.current = engine
    engine.onState(setState)
    engine.seek(toSource(start, probe.time_offset_ms))

    const onPlay = (): void => setPaused(false)
    const onPause = (): void => setPaused(true)
    const onTime = (): void => setTime(video.currentTime)
    const onVolume = (): void => {
      setVolume(video.volume)
      setMuted(video.muted)
    }
    video.addEventListener('play', onPlay)
    video.addEventListener('pause', onPause)
    video.addEventListener('timeupdate', onTime)
    video.addEventListener('volumechange', onVolume)

    // The position is saved every five seconds, and on the way out.
    const save = (): void => savePosition(id, file, video.currentTime, probe.duration_ms / 1000)
    const saveTimer = setInterval(save, 5_000)

    return () => {
      clearInterval(saveTimer)
      video.removeEventListener('play', onPlay)
      video.removeEventListener('pause', onPause)
      video.removeEventListener('timeupdate', onTime)
      video.removeEventListener('volumechange', onVolume)
      save()
      engine.destroy()
      engineRef.current = null
    }
  }, [choice, probe, id, file])

  function trackLanguage(trackId: number, probeNow: MediaProbe): string | null {
    return probeNow.tracks.find((track) => track.id === trackId)?.language ?? null
  }

  function addCues(key: string, cues: Cue[], language: string | null): void {
    const video = videoRef.current
    if (video === null) return
    const label = key
    let found: TextTrack | null = null
    for (let i = 0; i < video.textTracks.length; i += 1) {
      const candidate = video.textTracks[i]
      if (candidate !== undefined && candidate.label === label) {
        found = candidate
        break
      }
    }
    if (found === null) found = video.addTextTrack('subtitles', label, langOf(language))
    const seen = cueKeysRef.current.get(key) ?? new Set<string>()
    cueKeysRef.current.set(key, seen)
    for (const cue of mergeCues(seen, cues)) {
      try {
        // The lib's `TextTrackCueList` has no `addCue` (it is on `TextTrack`), so the cue is
        // added through the `TextTrack` itself, the `addCue` the WebVTT engine gives.
        ;(found as TextTrack & { addCue(cue: TextTrackCue): void }).addCue(
          new VTTCue(toMedia(cue.start_ms, offsetMs), toMedia(cue.end_ms, offsetMs), cue.text),
        )
      } catch {
        // A cue the `TextTrack` will not take is not a cue the player stops over.
      }
    }
    // The track that is chosen is the one shown, and the rest are not.
    for (let i = 0; i < video.textTracks.length; i += 1) {
      const candidate = video.textTracks[i]
      if (candidate === undefined) continue
      candidate.mode = candidate.label === subtitleKey ? 'showing' : 'disabled'
    }
  }

  // The subtitles: the embedded text track is fetched by window as the playhead moves, and the
  // chosen external file is fetched once.
  useEffect(() => {
    if (choice?.kind !== 'remux' || probe === null) return
    const video = videoRef.current
    const selected = subtitleKey
    if (selected === 'off') return

    if (selected.startsWith('external:')) {
      const index = Number(selected.slice('external:'.length))
      if (externalDoneRef.current.has(index)) return
      externalDoneRef.current.add(index)
      void (async () => {
        try {
          const answer = await client.subtitleFile(id, file, index)
          addCues(selected, answer.cues, null)
        } catch {
          // A subtitle that will not fetch is a subtitle that is not shown.
        }
      })()
      return
    }

    if (selected.startsWith('track:')) {
      const trackId = Number(selected.slice('track:'.length))
      const currentMs = toSource(video?.currentTime ?? 0, offsetMs)
      const fetched = fetchedRef.current.get(trackId) ?? []
      const window_ = nextWindow(currentMs, fetched)
      if (window_ === null) return
      if (fetched.some(([a, b]) => a === window_[0] && b === window_[1])) return
      fetched.push(window_)
      fetchedRef.current.set(trackId, fetched)
      const language = trackLanguage(trackId, probe)
      void (async () => {
        try {
          const answer = await client.cues(id, file, trackId, window_[0], window_[1])
          addCues(selected, answer.cues, language)
        } catch {
          // A window that will not fetch is a window that is not shown.
        }
      })()
    }
    // The effect re-runs on every state change, which is the player's clock for moving windows:
    // a `timeupdate` changes the state's reading of where the playhead is, and the next window
    // is asked for then.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [choice, probe, subtitleKey, client, id, file, offsetMs, state, time])

  function subtitleOptions(): SubtitleOption[] {
    if (probe === null) return []
    const out: SubtitleOption[] = [{ key: 'off', text: 'Off' }]
    for (const track of probe.tracks) {
      if (track.kind !== 'subtitle') continue
      if (!track.text) {
        // An image subtitle cannot be shown by the browser's WebVTT engine, and it is listed as
        // saying so rather than dropped: the choice exists, it is not one this page can take.
        out.push({ key: `track:${track.id}`, text: `${trackLabel(track)}: Image subtitles cannot be shown here.`, disabled: true })
      } else {
        out.push({ key: `track:${track.id}`, text: trackLabel(track) })
      }
    }
    for (const entry of probe.external_subtitles) {
      out.push({ key: `external:${entry.file}`, text: entry.name })
    }
    return out
  }

  const waiting = choice?.kind === 'remux' && state === 'waiting'

  function togglePlay(): void {
    const video = videoRef.current
    if (video === null || choice?.kind !== 'remux') return
    if (video.paused) void video.play()
    else video.pause()
  }

  function nudge(deltaSec: number): void {
    const video = videoRef.current
    if (video === null || choice?.kind !== 'remux') return
    video.currentTime = Math.min(Math.max(0, video.currentTime + deltaSec), durationSec || Infinity)
  }

  function nudgeVolume(delta: number): void {
    const video = videoRef.current
    if (video === null) return
    video.volume = Math.min(1, Math.max(0, video.volume + delta))
    if (video.volume > 0 && video.muted) video.muted = false
  }

  function toggleFullscreen(): void {
    const root = dialogRef.current
    if (root === null) return
    if (document.fullscreenElement !== null) void document.exitFullscreen()
    else root.requestFullscreen?.()
  }

  function nextAudio(): void {
    if (choice?.kind !== 'remux') return
    const playable = choice.audioOptions.filter((option) => option.playable)
    if (playable.length === 0) return
    const current = choice.audio?.id
    const index = playable.findIndex((option) => option.track.id === current)
    const next = playable[(index + 1) % playable.length].track
    engineRef.current?.setAudio(next)
    setAudioKey(String(next.id))
  }

  function nextSubtitle(): void {
    const options = subtitleOptions().filter((option) => option.disabled !== true)
    if (options.length === 0) return
    const index = options.findIndex((option) => option.key === subtitleKey)
    setSubtitleKey(options[(index + 1) % options.length].key)
  }

  function chooseAudio(key: string): void {
    if (choice?.kind !== 'remux') return
    const found = choice.audioOptions.find((option) => String(option.track.id) === key)
    if (found === undefined || !found.playable) return
    engineRef.current?.setAudio(found.track)
    setAudioKey(key)
  }

  async function download(): Promise<void> {
    if (downloading) return
    setDownloading(true)
    try {
      // The token is fetched at the click, not at the open: it is good for 60 seconds, and the
      // click is the moment the person is actually about to take the file.
      const path = await client.streamToken(id, file)
      const anchor = document.createElement('a')
      anchor.href = path
      anchor.download = name
      document.body.appendChild(anchor)
      anchor.click()
      anchor.remove()
    } finally {
      setDownloading(false)
    }
  }

  const { ref: modalRef, onKeyDown } = useModalDialog(onClose)
  useEffect(() => {
    dialogRef.current = modalRef.current
  })

  function onDialogKey(event: KeyboardEvent): void {
    const target = event.target
    if (
      target instanceof HTMLElement &&
      (target.tagName === 'INPUT' || target.getAttribute('role') === 'listbox')
    ) {
      return
    }
    switch (event.key) {
      case ' ':
      case 'k':
        event.preventDefault()
        togglePlay()
        return
      case 'ArrowLeft':
        event.preventDefault()
        nudge(-SEEK_STEP)
        return
      case 'ArrowRight':
        event.preventDefault()
        nudge(SEEK_STEP)
        return
      case 'ArrowUp':
        event.preventDefault()
        nudgeVolume(VOLUME_STEP)
        return
      case 'ArrowDown':
        event.preventDefault()
        nudgeVolume(-VOLUME_STEP)
        return
      case 'f':
        event.preventDefault()
        toggleFullscreen()
        return
      case 'm':
        event.preventDefault()
        if (videoRef.current !== null) videoRef.current.muted = !videoRef.current.muted
        return
      case 'a':
        event.preventDefault()
        nextAudio()
        return
      case 'c':
        event.preventDefault()
        nextSubtitle()
        return
      default:
        return
    }
  }

  const audioOptions: SubtitleOption[] =
    choice?.kind === 'remux'
      ? choice.audioOptions.map((option) => ({
          key: String(option.track.id),
          text: trackLabel(option.track) + (option.playable ? '' : ': not playable here'),
          disabled: !option.playable,
        }))
      : []
  const subtitleChoices = subtitleOptions()

  return (
    <dialog
      ref={(node) => {
        modalRef.current = node
        dialogRef.current = node
      }}
      aria-label={`Play ${displaySafe(name)}`}
      onKeyDown={(event) => {
        onDialogKey(event)
        onKeyDown(event)
      }}
      className="flex h-[min(700px,calc(100dvh-48px))] w-[min(960px,calc(100vw-48px))] flex-col overflow-hidden rounded-overlay border border-rule bg-canvas text-body"
    >
      <div className="flex items-center justify-between gap-3 border-b border-ink-700 px-4 py-2.5">
        <span className="min-w-0 truncate text-[13px] text-strong">{displaySafe(name)}</span>
        <button
          type="button"
          aria-label="Close player"
          onClick={onClose}
          className="flex h-8 w-8 flex-none cursor-pointer items-center justify-center rounded-control text-muted hover:bg-ink-700 hover:text-strong"
        >
          <X className="h-4 w-4" aria-hidden="true" />
        </button>
      </div>

      {probeError !== null && (
        <div className="flex flex-1 items-center justify-center px-6 text-center">
          <p className="text-[13px] text-error">{probeError}</p>
        </div>
      )}

      {probeError === null && probe === null && (
        <div className="flex flex-1 items-center justify-center text-[13px] text-muted">Loading...</div>
      )}

      {probe !== null && choice?.kind === 'unplayable' && (
        <div className="flex flex-1 flex-col items-center justify-center gap-4 px-6 text-center">
          <p className="max-w-sm text-[14px] text-strong">{unplayableCopy(choice.codec)}</p>
          <button
            type="button"
            aria-label="Download"
            onClick={() => {
              void download()
            }}
            className="flex h-10 cursor-pointer items-center gap-2 rounded-control bg-accent px-4 text-[13px] font-medium text-strong hover:bg-accent-hover disabled:cursor-default disabled:opacity-55"
            disabled={downloading}
          >
            <Download className="h-4 w-4" aria-hidden="true" />
            Download
          </button>
        </div>
      )}

      {probe !== null && choice?.kind === 'native' && (
        <div className="flex min-h-0 flex-1 items-center justify-center bg-black">
          <video ref={videoRef} src={nativeSrc ?? undefined} controls className="max-h-full max-w-full" />
        </div>
      )}

      {probe !== null && choice?.kind === 'remux' && (
        <>
          <div className="relative flex min-h-0 flex-1 items-center justify-center bg-black">
            <video ref={videoRef} className="max-h-full max-w-full" />
            {waiting && (
              <div className="absolute inset-0 flex items-center justify-center">
                <span className="rounded-card bg-canvas/80 px-4 py-2 text-[13px] text-strong">{WAITING_COPY}</span>
              </div>
            )}
          </div>
          <div className="flex flex-wrap items-center gap-x-2 gap-y-2 border-t border-ink-700 px-3 py-2.5">
            <input
              type="range"
              aria-label="Seek"
              min={0}
              max={durationSec || 0}
              step={0.1}
              value={time}
              onChange={(event) => {
                const video = videoRef.current
                if (video !== null) video.currentTime = Number(event.target.value)
              }}
              className="h-1.5 min-w-40 flex-1 cursor-pointer appearance-none rounded-full bg-ink-600 accent-accent"
            />
            <span className="font-mono text-[12px] text-strong">
              {clock(time)} / {clock(durationSec)}
            </span>
            <IconButton label={paused ? 'Play' : 'Pause'} onClick={togglePlay}>
              {paused ? <Play className="h-4 w-4" aria-hidden="true" /> : <Pause className="h-4 w-4" aria-hidden="true" />}
            </IconButton>
            <IconButton label="Back 5 seconds" onClick={() => nudge(-SEEK_STEP)}>
              <RotateCcw className="h-4 w-4" aria-hidden="true" />
            </IconButton>
            <IconButton label="Forward 5 seconds" onClick={() => nudge(SEEK_STEP)}>
              <RotateCw className="h-4 w-4" aria-hidden="true" />
            </IconButton>
            <IconButton
              label={muted ? 'Unmute' : 'Mute'}
              onClick={() => {
                const video = videoRef.current
                if (video !== null) video.muted = !video.muted
              }}
            >
              {muted ? <VolumeX className="h-4 w-4" aria-hidden="true" /> : <Volume2 className="h-4 w-4" aria-hidden="true" />}
            </IconButton>
            <input
              type="range"
              aria-label="Volume"
              min={0}
              max={1}
              step={0.05}
              value={muted ? 0 : volume}
              onChange={(event) => {
                const video = videoRef.current
                if (video === null) return
                video.volume = Number(event.target.value)
                if (video.volume > 0 && video.muted) video.muted = false
              }}
              className="h-1.5 w-24 cursor-pointer appearance-none rounded-full bg-ink-600 accent-accent"
            />
            {audioOptions.length > 0 && (
              <ChoiceMenu label="Audio" options={audioOptions} selected={audioKey ?? ''} onChoose={chooseAudio} />
            )}
            {subtitleChoices.length > 0 && (
              <ChoiceMenu
                label="Subtitles"
                options={subtitleChoices}
                selected={subtitleKey}
                onChoose={setSubtitleKey}
              />
            )}
            <IconButton label="Fullscreen" onClick={toggleFullscreen}>
              <Maximize className="h-4 w-4" aria-hidden="true" />
            </IconButton>
          </div>
        </>
      )}
    </dialog>
  )
}
