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
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react'
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
import { useConnection } from '../connection'
import { CANCELLED_COPY, useDesktopDownload } from '../desktopDownload'
import type { FileDownload } from '../desktopDownload'
import { isAbsolute } from '../endpoint'
import type { Api, Cue, MediaProbe, MediaTrack } from '../api/client'
import { displaySafe, formatBytes, formatPercent } from '../format'
import { matches } from '../keys'
import { MODAL_CENTRED, useModalDialog } from '../modal'
import { mergeCues, nextWindow } from './cues'
import { createEngine } from './engine'
import type { Engine, EngineFailure, EngineState } from './engine'
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
/** The line the player draws when the engine's state is `error`, beside the file it can still offer. */
const ERROR_COPY = 'Playback failed.'
/** The line shown beside the controls after the audio was dropped so the video could play. */
const AUDIO_RETRY_COPY = 'This audio track cannot be played here, so the video plays without sound.'
/** The line shown when a Windows system is missing the decoder HEVC needs. */
const HEVC_WINDOWS_COPY = 'On Windows, HEVC video needs the HEVC Video Extensions from the Microsoft Store.'
/** The one sentence a browser that cannot decode the video gets, with the codec in it. */
function unplayableCopy(codec: string): string {
  return `This browser cannot play ${codec} video.`
}
/**
 * The sentence for a file with no video track at all (an audio file, or a file the probe found no
 * tracks in): there is no video codec to name, and this player plays video files only.
 */
const NO_VIDEO_COPY = 'This file has no video track to play.'

/**
 * The error line for the engine's failure: the codec that could not be decoded, the status the
 * server answered, or the buffer the browser refused, each named in the copy's own words. With no
 * failure recorded (an engine that stopped for some other reason) it is today's bare line.
 */
function playbackFailureCopy(
  failure: EngineFailure | null,
  video: MediaTrack | null,
  audio: MediaTrack | null,
): string {
  if (failure === null) return ERROR_COPY
  if (failure.kind === 'server') return `Playback failed: the server answered ${failure.status}.`
  const track = failure.role === 'audio' ? audio : video
  const codec = displaySafe(track?.codec ?? 'unknown')
  if (failure.kind === 'buffer') {
    return `Playback failed: the browser refused the ${codec} ${failure.role ?? 'video'} stream.`
  }
  if (failure.role === 'audio') return `Playback failed: this system could not decode the ${codec} audio.`
  return `Playback failed: this system could not decode the ${codec} video.`
}

/**
 * The Audio menu's memory after a decode failure: the tracks that failed on this system (listed
 * not playable, never asked for again), and the track the person chose to play in their place -
 * null while the file plays without sound.
 */
type AudioRetry = { failed: string[]; selected: string | null }

/**
 * Whether the Windows HEVC line applies: a track named with an `hvc1` or `hev1` codec (the failing
 * one when the engine named one, the chosen video track otherwise), on Windows, where the decoder
 * is a paid extension and its absence is the commonest cause of a decode failure.
 */
function hevcWindowsHint(failure: EngineFailure | null, video: MediaTrack | null): boolean {
  if (!navigator.userAgent.includes('Windows')) return false
  // The purchase is the fix for a video this system was asked to decode: not for a dead audio
  // track, and not for a server that refused the stream. A hint about HEVC on an audio failure
  // sends the person shopping for the wrong thing.
  if (failure !== null) {
    if (failure.kind !== 'decode' && failure.kind !== 'buffer') return false
    if (failure.role === 'audio') return false
  }
  const isHevc = (codec: string): boolean => codec.startsWith('hvc1') || codec.startsWith('hev1')
  if (video !== null && video.mime_codecs.some(isHevc)) return true
  if (failure === null || failure.mime === null) return false
  const match = /codecs="([^"]*)"/.exec(failure.mime)
  const codecs = match !== null ? match[1] : failure.mime
  return codecs.split(',').some((entry) => isHevc(entry.trim()))
}

/** The words a track is listed under: its name when it has one, its language when it does not. The name is remote text, so it goes through `displaySafe` on the way out. */
function trackLabel(track: MediaTrack): string {
  if (track.name !== null && track.name !== '') return displaySafe(track.name)
  // The language is remote text too: a track with no name is listed under its language, and the
  // same run of `displaySafe` the name gets is the one it gets.
  return track.language !== null && track.language !== '' ? displaySafe(track.language) : 'Audio'
}

/**
 * Whether a mime is one the browser's `SourceBuffer` will take, checked against whichever source
 * constructor the browser offers: a `ManagedMediaSource` manages its own playback and is the one
 * to ask when it is present, and a `MediaSource` when it is not. The mime is checked on that same
 * constructor, because the answer is the constructor's.
 */
function remuxSupported(mime: string): boolean {
  const managed = (window as { ManagedMediaSource?: { isTypeSupported(m: string): boolean } })
    .ManagedMediaSource
  const source =
    (managed ?? (typeof MediaSource !== 'undefined' ? MediaSource : undefined)) as
      | { isTypeSupported(m: string): boolean }
      | undefined
  try {
    return source !== undefined && source.isTypeSupported(mime)
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
/** The actions whose keys the native video's own controls take when they have the focus. */
const NATIVE_CONTROL_ACTIONS: readonly string[] = ['Play or pause', 'Seek back', 'Seek forward', 'Volume up', 'Volume down']
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
  onMenuChange,
}: {
  label: string
  options: SubtitleOption[]
  selected: string
  onChoose(key: string): void
  onMenuChange(open: boolean): void
}) {
  const [open, setOpen] = useState(false)
  const buttonRef = useRef<HTMLButtonElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  const wrapperRef = useRef<HTMLDivElement>(null)
  // The latest report, for the listener below: it is added once per open, not once per render.
  const onMenuChangeRef = useRef(onMenuChange)
  useEffect(() => {
    onMenuChangeRef.current = onMenuChange
  }, [onMenuChange])

  function setOpenAndReport(next: boolean): void {
    setOpen(next)
    onMenuChange(next)
  }

  // An open menu takes the focus into its choices, the chosen one first, so the arrows move
  // through the list from where the person is; and a press anywhere outside the menu closes it,
  // the way a menu closes when a person moves on, without taking the focus back to the button.
  useEffect(() => {
    if (!open) return
    const list = listRef.current
    const target =
      list?.querySelector<HTMLElement>('[role="option"][aria-selected="true"]:not([disabled])') ??
      list?.querySelector<HTMLElement>('[role="option"]:not([disabled])') ??
      null
    target?.focus()
    const onPointerDown = (event: Event): void => {
      const wrapper = wrapperRef.current
      if (wrapper !== null && event.target instanceof Node && wrapper.contains(event.target)) return
      setOpen(false)
      onMenuChangeRef.current(false)
    }
    document.addEventListener('pointerdown', onPointerDown)
    return () => {
      document.removeEventListener('pointerdown', onPointerDown)
    }
  }, [open])

  function close(): void {
    setOpenAndReport(false)
    buttonRef.current?.focus()
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (!open) {
      if (matches('Menus', 'Open the menu', event)) {
        event.preventDefault()
        setOpenAndReport(true)
      }
      return
    }
    // Escape closes the menu even when it has no choice that can be taken.
    if (matches('Menus', 'Close the menu', event)) {
      event.preventDefault()
      close()
      return
    }
    const items = Array.from(
      listRef.current?.querySelectorAll<HTMLElement>('[role="option"]:not([disabled])') ?? [],
    )
    if (items.length === 0) return
    // The option the cursor is on is the one focused, not the one selected: the arrows move the
    // focus, and Enter takes the focused option, so a person can move and then choose.
    const focused = items.findIndex((item) => item === listRef.current?.querySelector<HTMLElement>(':focus'))
    const current = focused !== -1 ? focused : items.findIndex((item) => item.getAttribute('aria-selected') === 'true')
    if (matches('Menus', 'Choose the focused option', event)) {
      event.preventDefault()
      const key = items[current === -1 ? 0 : current]?.getAttribute('data-key')
      if (key !== null && key !== undefined) onChoose(key)
      close()
      return
    }
    let next: number
    if (matches('Menus', 'Next option', event)) next = (current + 1) % items.length
    else if (matches('Menus', 'Previous option', event)) next = (current - 1 + items.length) % items.length
    else if (matches('Menus', 'First option', event)) next = 0
    else if (matches('Menus', 'Last option', event)) next = items.length - 1
    else return
    event.preventDefault()
    items[next]?.focus()
  }

  return (
    <div
      ref={wrapperRef}
      className="relative"
      onKeyDown={onKeyDown}
      onBlur={(event) => {
        // Focus that leaves the menu (Tab past its last choice) closes it, and leaves the focus
        // where it went: an open menu nobody is in would keep the player's keys off.
        const next = event.relatedTarget
        if (!open || (next instanceof Node && wrapperRef.current?.contains(next) === true)) return
        setOpenAndReport(false)
      }}
    >
      <button
        ref={buttonRef}
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => (open ? close() : setOpenAndReport(true))}
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

/** What Download says where a file on another origin has nothing to save it with. */
const NO_DOWNLOAD_COPY = 'This file cannot be downloaded here.'

/** Whether `address` is on the page's own origin, where an `<a download>` saves rather than navigates. */
function sameOriginAddress(address: string): boolean {
  // A path is on the page's own origin by definition (the browser build's token route answers one).
  if (!isAbsolute(address) && !address.startsWith('//')) return true
  try {
    return new URL(address, window.location.href).origin === window.location.origin
  } catch {
    return false
  }
}

/** What watching downloads looks like with no shell: there is none, and nothing to watch. */
function noShellSubscribe(notify: () => void): () => void {
  void notify
  return () => {}
}

function noShellDownload(): FileDownload | null {
  return null
}

/**
 * The line that replaces the Download button while the shell writes the file: with a size
 * `Downloading 42% (1.2 GiB of 2.9 GiB)`, with none `Downloading 1.2 GiB`. The numbers come from
 * `formatPercent` and `formatBytes`, so they read the way every other size on the page does.
 */
function downloadProgressCopy(download: FileDownload): string {
  if (download.total !== null) {
    return `Downloading ${formatPercent(download.received / download.total)} (${formatBytes(download.received)} of ${formatBytes(download.total)})`
  }
  return `Downloading ${formatBytes(download.received)}`
}

/** The Download button, as the unplayable screen and the playback failure drew it. */
const OFFER_BUTTON =
  'flex h-10 cursor-pointer items-center gap-2 rounded-control bg-accent px-4 text-[13px] font-medium text-strong hover:bg-accent-hover disabled:cursor-default disabled:opacity-55'

/** The secondary weight, for the lesser action: the ruled outline the desktop page uses elsewhere. */
const CANCEL_BUTTON =
  'inline-flex h-10 cursor-pointer items-center justify-center rounded-control border border-rule bg-raised px-4 text-[13px] font-medium text-strong transition-colors hover:bg-ink-700'

/**
 * The player's Download offer. While a download of this file runs, the Download button is replaced
 * by a progress line, a bar, and a Cancel; the line and bar report what the shell has written.
 * After it ends the button is back, with `Saved.` or `Download cancelled.` beside it in the quiet
 * colour; those come from the download's final state, kept above the player, so they are there for
 * a person who closed the player during the download and came back. Any other ending is the
 * failure alert's, fed by the shell's own line.
 */
function DownloadOffer({
  download,
  disabled,
  onDownload,
  onCancel,
}: {
  download: FileDownload | null
  disabled: boolean
  onDownload(): void
  onCancel(): void
}) {
  const saving = download !== null && download.state === 'saving'
  return (
    <>
      {saving && download !== null && (
        <>
          <p role="status" aria-live="polite" className="max-w-sm text-center text-[13px] text-strong">
            {downloadProgressCopy(download)}
            <progress
              aria-label="Download progress"
              {...(download.total === null ? {} : { value: download.received, max: download.total })}
              className="mx-auto mt-2 block h-2 w-full max-w-sm accent-accent"
            />
          </p>
          <button type="button" aria-label="Cancel download" onClick={onCancel} className={CANCEL_BUTTON}>
            Cancel download
          </button>
        </>
      )}
      {!saving && (
        <>
          <button type="button" aria-label="Download" onClick={onDownload} disabled={disabled} className={OFFER_BUTTON}>
            <Download className="h-4 w-4" aria-hidden="true" />
            Download
          </button>
          {download?.state === 'saved' && (
            <p role="status" aria-live="polite" className="max-w-sm text-[13px] text-muted">
              Saved.
            </p>
          )}
          {download?.state === 'cancelled' && (
            <p role="status" aria-live="polite" className="max-w-sm text-[13px] text-muted">
              {CANCELLED_COPY}
            </p>
          )}
        </>
      )}
    </>
  )
}

export default function Player({ id, file, name, client: given, onClose }: PlayerProps) {
  const connection = useConnection()
  const client = given ?? connection.api
  const bearer = connection.endpoint.kind === 'bearer'
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
  // After an audio track this system cannot decode: which tracks failed (never asked for again)
  // and which track the person chose in their place, null while the file plays with no sound.
  // The notice above the controls belongs to the no-sound stretch of it.
  const [audioRetry, setAudioRetry] = useState<AudioRetry | null>(null)
  const [downloading, setDownloading] = useState(false)
  const [downloadProblem, setDownloadProblem] = useState<string | null>(null)
  const shell = useDesktopDownload()
  // The app's download of this file, kept by the desktop page above the player: closing the player
  // neither cancels it nor forgets it, and opening this file again shows the download where it
  // stands. A browser has no shell, so it has no download kept here either.
  const fileKey = `${id}:${file}`
  const fileDownload = useSyncExternalStore(
    shell === null ? noShellSubscribe : shell.subscribe,
    shell === null ? noShellDownload : () => shell.current(fileKey),
  )
  // The Seek bar's value while a pointer drags it, in source seconds, or null when it is not
  // being dragged. A drag shows where it is going and seeks once, on release: every seek outside
  // the buffer restarts the streams, and a drag writes dozens of values.
  const [scrub, setScrub] = useState<number | null>(null)
  const draggingRef = useRef(false)
  // True while a menu is open: the dialog's key handler ignores keys from inside it, so an
  // `ArrowUp` in the Audio listbox does not move the volume. Each menu keeps its own flag:
  // closing one while the other is open is the way a person works, and the handler has to
  // know either one is up.
  const [audioMenuOpen, setAudioMenuOpen] = useState(false)
  const [subtitleMenuOpen, setSubtitleMenuOpen] = useState(false)
  const menuOpen = audioMenuOpen || subtitleMenuOpen

  const videoRef = useRef<HTMLVideoElement | null>(null)
  const engineRef = useRef<Engine | null>(null)
  const dialogRef = useRef<HTMLDialogElement | null>(null)
  // The cues each track has already been given: a window fetched again for the same second must
  // not double the cues the `TextTrack` already shows.
  const cueKeysRef = useRef<Map<string, Set<string>>>(new Map())
  // The `TextTrack` each subtitle key is kept under: the browser's `TextTrack.id` is read-only,
  // so the key lives here, not on the track, and every lookup goes through the map.
  const subtitleTracksRef = useRef<Map<string, TextTrack>>(new Map())
  // The cue windows each embedded track has already been asked for.
  const fetchedRef = useRef<Map<number, [number, number][]>>(new Map())
  // The external files fetched once, by their index.
  const externalDoneRef = useRef<Set<number>>(new Set())
  // The position the probe's answer will start at, in source seconds, decided once per probe.
  const startRef = useRef(0)
  // The point, in source milliseconds, where an engine that failed on the audio hands the playhead
  // to the engine that replaces it; read once by the next engine effect, then cleared.
  const resumeMsRef = useRef<number | null>(null)
  // The audio track the menus chose, kept where the engine's state callback can read it fresh:
  // a menu switch does not re-run the engine effect, and a failure must be filed against the
  // track that was actually playing, not the one the probe preferred.
  const audioKeyRef = useRef<string | null>(null)
  useEffect(() => {
    audioKeyRef.current = audioKey
  }, [audioKey])
  // True once the open has asked the element to play: the open plays once, and later data or
  // events do not ask it again. Opening a file starts playback, so a second press is not needed;
  // an autoplay the browser refuses leaves the Play control showing, the person's own choice.
  const autoPlayedRef = useRef(false)

  const offsetMs = probe?.time_offset_ms ?? 0
  const durationMs = probe?.duration_ms ?? null
  const durationSourceSec = durationMs === null ? 0 : durationMs / 1000

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
  // effect because it reads `localStorage`, which is not a pure function of the probe. The
  // position and the duration are both in source seconds, the clock `positions` keeps, so the
  // comparison is the one the person thinks in.
  useEffect(() => {
    if (probe === null) return
    const remembered = loadPosition(id, file)
    const limit = probe.duration_ms === null ? null : probe.duration_ms / 1000
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

  // The native path's transport: the open asks the element to play once its data can play, the
  // way the remux path does, and a refused autoplay leaves the Play control (the element's own
  // `controls`) showing. The viewer's own play or pause (the element's `controls`) wins: once they
  // have said what they want, the autoplay never overrides it.
  useEffect(() => {
    if (choice?.kind !== 'native' || nativeSrc === null) return
    const video = videoRef.current
    if (video === null) return
    const onCanPlay = (): void => {
      if (autoPlayedRef.current) return
      autoPlayedRef.current = true
      void video.play().catch(() => {
        // An autoplay the browser refuses is not an error: the element's own Play control stays.
      })
    }
    // A play or pause the viewer makes through the element's controls is their word for what they
    // want: mark the open handled so the autoplay does not override it.
    const onPlay = (): void => {
      autoPlayedRef.current = true
    }
    const onPause = (): void => {
      autoPlayedRef.current = true
    }
    video.addEventListener('canplay', onCanPlay)
    video.addEventListener('play', onPlay)
    video.addEventListener('pause', onPause)
    return () => {
      video.removeEventListener('canplay', onCanPlay)
      video.removeEventListener('play', onPlay)
      video.removeEventListener('pause', onPause)
    }
  }, [choice, nativeSrc])

  // The engine, for the remux. It owns the source and the buffers; the element's transport events
  // are read here, and the position is saved on its clock.
  useEffect(() => {
    if (choice?.kind !== 'remux' || probe === null) return
    const video = videoRef.current
    if (video === null) return

    // The start is source seconds, the clock the position is kept in; the engine speaks source
    // milliseconds, so the one conversion is the seconds to milliseconds, not a media mapping. A
    // start after an audio failure is the point the failed engine handed over, already in the
    // engine's own clock.
    const resumeMs = resumeMsRef.current
    resumeMsRef.current = null
    const startMs = resumeMs !== null ? resumeMs : Math.round(startRef.current * 1000)
    // The audio the engine opens with: the probe's choice normally, the track the person chose
    // after another one failed, and nothing while the failed track stands replaced.
    const openedAudio = audioRetry === null
      ? choice.audio
      : audioRetry.selected === null
        ? null
        : choice.audioOptions.find((option) => String(option.track.id) === audioRetry.selected)?.track ?? null
    const audioMimeOf = (track: MediaTrack): string | null => {
      for (const codec of track.mime_codecs) {
        const mime = `audio/mp4; codecs="${codec}"`
        if (remuxSupported(mime)) return mime
      }
      return null
    }
    const engine = createEngine(
      video,
      { id, file },
      {
        offsetMs: probe.time_offset_ms,
        durationMs: probe.duration_ms,
        video: choice.video,
        videoMime: choice.videoMime,
        audio: openedAudio,
        audioMime: openedAudio === null ? null : audioMimeOf(openedAudio),
        mimeFor: audioMimeOf,
      },
      // The browser keeps the engine's own route on the page's origin; the desktop's engine lives at
      // another address, so the body comes from the client's absolute URL with the token in it.
      bearer ? { streamUrl: (track, ms) => client.mediaStreamUrl(id, file, track, ms) } : undefined,
    )
    engineRef.current = engine
    engine.onState((next: EngineState): void => {
      // Only the engine this effect opened speaks for the player. A callback from one it has
      // already replaced - an `error` from a SourceBuffer of a playback the person left - is
      // news about the past, and the past does not get to put its overlay back up.
      if (engineRef.current !== engine) return
      setState(next)
      // One audio track this system cannot decode is not a reason to lose the picture: open the
      // file once more at the point it stopped, without that track. A track that failed is never
      // asked for again; the Audio menu offers the others. A wait the server asked for (a 500
      // on the audio route) is the server's failure, not a codec one, and the overlay's own
      // line about the server is the word for it.
      if (next !== 'error') return
      if (choice.audio === null) return
      const failure = engine.failure()
      if (failure === null || failure.role !== 'audio') return
      if (failure.kind !== 'decode' && failure.kind !== 'buffer') return
      const deadKey = audioRetry === null
        ? (audioKeyRef.current ?? String(choice.audio.id))
        : audioRetry.selected
      if (deadKey === null) return
      if (audioRetry !== null && audioRetry.failed.includes(deadKey)) return
      const at = videoRef.current
      const playhead = at !== null ? Math.round(toSource(at.currentTime, probe.time_offset_ms)) : 0
      // A playhead that never moved is a video that never landed: the point to come back to is
      // the start this open aimed at, not the zero the element is sitting at.
      resumeMsRef.current = playhead > 0 ? playhead : startMs
      // Moving picture stays moving: the reload pauses the element, so the play intent goes to
      // the next open with the position.
      rememberPlaying()
      setAudioRetry({
        failed: audioRetry === null ? [deadKey] : [...audioRetry.failed, deadKey],
        selected: null,
      })
      // The retry is under way, so the old failure comes off the screen now - its Download and
      // its words belong to a playback that has already been replaced, not to the one opening.
      setState('loading')
    })
    engine.seek(startMs)

    // A play or pause the viewer makes (Space or `k`, or the Play control) is their word for what
    // they want: mark the open handled so the autoplay on the first `canplay` does not override it.
    const onPlay = (): void => {
      autoPlayedRef.current = true
      setPaused(false)
    }
    const onPause = (): void => {
      autoPlayedRef.current = true
      setPaused(true)
    }
    // The display is in source time, the clock the probe answers and the person thinks in: the
    // element's media time mapped back through the offset.
    const onTime = (): void => setTime(toSource(video.currentTime, probe.time_offset_ms))
    const onVolume = (): void => {
      setVolume(video.volume)
      setMuted(video.muted)
    }
    // The open's own word for "there is data to play": the element can play. The ask happens here,
    // once, so a `canplay` that fires for later data does not ask a second time.
    const onCanPlay = (): void => {
      // The element may have reloaded since the last word heard - a reopen replaces its `src`,
      // and a reload pauses without firing `pause` - so the control reads the element as it is
      // now, not the last state it was reported.
      setPaused(video.paused)
      if (autoPlayedRef.current) return
      autoPlayedRef.current = true
      void video.play().catch(() => {
        // An autoplay the browser refuses (its policy, or the element's) is not an error: the
        // Play control stays showing, and the person's own press is the one that plays.
      })
    }
    video.addEventListener('play', onPlay)
    video.addEventListener('pause', onPause)
    video.addEventListener('timeupdate', onTime)
    // A seek (a key, a nudge, the engine's own landing) moves the shown time at once, not when
    // the data for it arrives.
    video.addEventListener('seeking', onTime)
    video.addEventListener('volumechange', onVolume)
    video.addEventListener('canplay', onCanPlay)

    // The position is saved every five seconds, and on the way out. It is remembered in source
    // seconds, the clock `positions` keeps: the duration is converted to the same clock, so the
    // "finished within" clear at the end of the file can see it.
    const save = (): void =>
      savePosition(id, file, toSource(video.currentTime, probe.time_offset_ms) / 1000, probe.duration_ms / 1000)
    const saveTimer = setInterval(save, 5_000)

    return () => {
      clearInterval(saveTimer)
      video.removeEventListener('play', onPlay)
      video.removeEventListener('pause', onPause)
      video.removeEventListener('timeupdate', onTime)
      video.removeEventListener('seeking', onTime)
      video.removeEventListener('volumechange', onVolume)
      video.removeEventListener('canplay', onCanPlay)
      save()
      engine.destroy()
      engineRef.current = null
    }
  }, [choice, probe, bearer, client, id, file, audioRetry])

  function trackLanguage(trackId: number, probeNow: MediaProbe): string | null {
    return probeNow.tracks.find((track) => track.id === trackId)?.language ?? null
  }

  /** The label a subtitle `TextTrack` is kept under: its human label, not the internal key. */
  function subtitleLabel(key: string): string {
    if (key.startsWith('track:')) {
      const trackId = Number(key.slice('track:'.length))
      const track = probe?.tracks.find((entry) => entry.id === trackId)
      if (track !== undefined) return trackLabel(track)
    }
    if (key.startsWith('external:')) {
      const index = Number(key.slice('external:'.length))
      const entry = probe?.external_subtitles.find((item) => item.file === index)
      if (entry !== undefined) return displaySafe(entry.name)
    }
    return key
  }

  function addCues(key: string, cues: Cue[], language: string | null): void {
    const video = videoRef.current
    if (video === null) return
    const label = subtitleLabel(key)
    // The track is kept under its own key, not its label: two tracks named "English" are two
    // tracks, and the key is what tells them apart. The key lives in the map, not on the
    // `TextTrack`, whose `id` is read-only in a browser.
    let found = subtitleTracksRef.current.get(key) ?? null
    if (found === null) {
      found = video.addTextTrack('subtitles', label, langOf(language))
      subtitleTracksRef.current.set(key, found)
    }
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
    applySubtitleModes()
  }

  // The chosen subtitle is the one shown and the rest are not, or none are when the choice is
  // `Off`: a `TextTrack` that is not `showing` shows nothing, and `Off` is the one that turns
  // them all off rather than leaving the last chosen one up.
  function applySubtitleModes(): void {
    const video = videoRef.current
    if (video === null) return
    // Every track the player made is in the map; the chosen key's track is the one shown, and
    // the rest are not. A key that is not the chosen one, including `off`, is a track not shown.
    for (const [key, track] of subtitleTracksRef.current) {
      track.mode = key === subtitleKey ? 'showing' : 'disabled'
    }
  }

  useEffect(() => {
    applySubtitleModes()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [subtitleKey])

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
      out.push({ key: `external:${entry.file}`, text: displaySafe(entry.name) })
    }
    return out
  }

  const waiting = choice?.kind === 'remux' && state === 'waiting'
  const failed = choice?.kind === 'remux' && state === 'error'
  // The player's own reopen (the audio-less retry, a replacement track) replaces the engine and
  // its `src`, and a reload pauses the element: the waiting line covers that loading too, so a
  // reopen says the picture is coming back instead of holding a stale word for playing.
  const reopening = choice?.kind === 'remux' && state === 'loading' && audioRetry !== null
  const videoTrack = choice?.kind === 'remux' ? choice.video : null
  const audioTrack = choice?.kind === 'remux' ? choice.audio : null
  // The audio the file plays now: the probe's choice, or the live switch's, and after a failure
  // the track chosen in its place - null while the failed track stands replaced with silence.
  const playingAudioKey = audioRetry === null
    ? (audioKey ?? (choice?.kind === 'remux' && choice.audio !== null ? String(choice.audio.id) : null))
    : audioRetry.selected
  // Why the engine stopped, read from the engine that stopped: the overlay's words come from it.
  const engineFailure = failed ? engineRef.current?.failure() ?? null : null
  const errorCopy = playbackFailureCopy(engineFailure, videoTrack, audioTrack)
  const errorDetail = engineFailure !== null && engineFailure.detail !== null ? displaySafe(engineFailure.detail) : null
  const showHevcHint = failed && hevcWindowsHint(engineFailure, videoTrack)
  // The no-sound notice belongs to the stretch between a track failing and another one being
  // chosen: while a replacement plays, the sound is back and the notice has said its piece.
  const audioNotice = audioRetry !== null && audioRetry.selected === null && audioTrack !== null

  function togglePlay(): void {
    const video = videoRef.current
    if (video === null || choice?.kind !== 'remux') return
    if (video.paused) void video.play()
    else video.pause()
  }

  function nudge(deltaSec: number): void {
    const video = videoRef.current
    if (video === null || choice?.kind !== 'remux' || probe === null) return
    // The nudge is in the source seconds the display shows: move in source time, then set the
    // element from there, clamped to the file's duration in source milliseconds.
    const sourceMs = toSource(video.currentTime, probe.time_offset_ms)
    const nextMs = Math.min(Math.max(0, sourceMs + deltaSec * 1000), probe.duration_ms ?? Infinity)
    video.currentTime = toMedia(nextMs, probe.time_offset_ms)
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
    // `!= null`, not `!== null`: a browser without the Fullscreen API (iPhone Safari) has no
    // `fullscreenElement` at all, and reads it as `undefined`.
    if (document.fullscreenElement != null) {
      if (typeof document.exitFullscreen === 'function') void document.exitFullscreen().catch(() => {})
      return
    }
    if (typeof root.requestFullscreen === 'function') {
      void root.requestFullscreen().catch(() => {
        // A refused fullscreen leaves the player as it was.
      })
      return
    }
    // iPhone Safari has no `requestFullscreen` on elements: the video's own fullscreen player is
    // the one fullscreen there is.
    const video = videoRef.current as (HTMLVideoElement & { webkitEnterFullscreen?: () => void }) | null
    if (typeof video?.webkitEnterFullscreen === 'function') {
      try {
        video.webkitEnterFullscreen()
      } catch {
        // Before the video has its metadata, Safari refuses with `InvalidStateError`.
      }
    }
  }

  /** Move the playhead to a point of the Seek bar, in source seconds. */
  function commitSeek(sourceSeconds: number): void {
    const video = videoRef.current
    if (video === null) return
    // The bar is in source time: the value is source seconds, mapped to the element.
    video.currentTime = toMedia(sourceSeconds * 1000, probe?.time_offset_ms ?? 0)
    // The bar stays where it was put: a seek outside the buffer fires no `timeupdate` until the
    // data arrives, and the old time would pull it back meanwhile.
    setTime(Math.round(sourceSeconds * 1000))
  }

  /**
   * A pointer went down on the Seek bar: the values it writes are shown, not sought, until it
   * comes up (or the browser takes it away), and the release seeks once to where it stopped.
   */
  function startDrag(input: HTMLInputElement): void {
    draggingRef.current = true
    const finish = (): void => {
      window.removeEventListener('pointerup', finish)
      window.removeEventListener('pointercancel', finish)
      if (!draggingRef.current) return
      draggingRef.current = false
      commitSeek(Number(input.value))
      setScrub(null)
    }
    window.addEventListener('pointerup', finish)
    window.addEventListener('pointercancel', finish)
  }

  /** Hand the point the playhead stands at, in source milliseconds, to the next engine. */
  function rememberPlayhead(): void {
    const at = videoRef.current
    if (at !== null && probe !== null) {
      resumeMsRef.current = Math.round(toSource(at.currentTime, probe.time_offset_ms))
    }
  }

  /**
   * The picture was moving: hand that word to the engine about to replace this one. A new
   * engine sets a new `src`, and a reload pauses the element - silently, with no `pause` the
   * player could hear - so without the hand-over the next open's first `canplay` is already
   * marked handled and the person is left with a frozen picture under a Pause control.
   */
  function rememberPlaying(): void {
    const at = videoRef.current
    if (at !== null && !at.paused) autoPlayedRef.current = false
  }

  function nextAudio(): void {
    if (choice?.kind !== 'remux') return
    // A track that failed on this system is not a next track: it is listed not playable in the
    // menu, and cycling must not walk back onto it.
    const playable = choice.audioOptions.filter(
      (option) => option.playable && !(audioRetry?.failed.includes(String(option.track.id)) ?? false),
    )
    if (playable.length === 0) return
    // Cycle from the track that is playing now, not from the track the probe preferred: a
    // person who chose the Italian audio gets the English next, not the Italian again.
    const index = playable.findIndex((option) => String(option.track.id) === playingAudioKey)
    const next = playable[(index + 1) % playable.length].track
    if (audioRetry !== null) {
      // After a failure the switch is a fresh open on the new track, from where the playhead
      // stands now: the engine that lost the last track has no audio buffer to switch. The
      // reopen pauses the element and replaces the picture, so the play intent and the state
      // both move to the new open with the position.
      rememberPlayhead()
      rememberPlaying()
      setAudioRetry({ failed: audioRetry.failed, selected: String(next.id) })
      setState('loading')
      return
    }
    // The engine says whether it switched: a refused switch keeps the track that still plays.
    if (engineRef.current?.setAudio(next) === true) setAudioKey(String(next.id))
  }

  function nextSubtitle(): void {
    const options = subtitleOptions().filter((option) => option.disabled !== true)
    if (options.length === 0) return
    const index = options.findIndex((option) => option.key === subtitleKey)
    setSubtitleKey(options[(index + 1) % options.length].key)
  }

  function chooseAudio(key: string): void {
    if (choice?.kind !== 'remux' || probe === null) return
    const found = choice.audioOptions.find((option) => String(option.track.id) === key)
    if (found === undefined || !found.playable) return
    if (audioRetry !== null) {
      // After a decode failure the switch is a fresh open on the chosen track, from where the
      // playhead stands now: the engine that lost the last track has no audio buffer to switch,
      // and a track that failed is never asked for again. The reopen pauses the element and
      // replaces the picture, so the play intent and the state both move to the new open with
      // the position.
      if (audioRetry.failed.includes(key) || audioRetry.selected === key) return
      rememberPlayhead()
      rememberPlaying()
      setAudioRetry({ failed: audioRetry.failed, selected: key })
      setState('loading')
      return
    }
    if (engineRef.current?.setAudio(found.track) === true) setAudioKey(key)
  }

  async function download(): Promise<void> {
    // One download per file at a time: the store's `saving` covers a download that started before
    // this player opened, and `downloading` the moment between the click and the store's entry.
    if (downloading || fileDownload?.state === 'saving') return
    setDownloading(true)
    try {
      setDownloadProblem(null)
      // In the app the stream address is on the engine's origin, not the page's: a link to it is not a
      // download (the webview ignores `download` there and navigates the page itself to the media), so
      // the shell saves it. Where to save is asked first, and the token only after: it is good for 60
      // seconds, and a person reading a save dialog can take longer than that. `run` keeps the store
      // entry fresh (progress for the screen, the ending for whoever opens the player next) and
      // swallows a cancel; any other failure gets rethrown, and its line goes to the alert below.
      if (shell !== null) {
        const handle = await shell.choose(name)
        if (handle === null) return
        await shell.run(fileKey, handle, await client.streamToken(id, file))
        return
      }
      // The token is fetched at the click, not at the open: it is good for 60 seconds, and the
      // click is the moment the person is actually about to take the file. Only a same-origin
      // address is clicked; a browser has no shell to save another one with.
      const path = await client.streamToken(id, file)
      if (!sameOriginAddress(path)) {
        setDownloadProblem(NO_DOWNLOAD_COPY)
        return
      }
      const anchor = document.createElement('a')
      anchor.href = path
      anchor.download = name
      document.body.appendChild(anchor)
      anchor.click()
      anchor.remove()
    } catch (problem) {
      setDownloadProblem(problem instanceof Error ? problem.message : String(problem))
    } finally {
      setDownloading(false)
    }
  }

  function cancelFileDownload(): void {
    if (shell !== null && fileDownload !== null) {
      shell.cancel(fileDownload.handle).catch(() => {
        // Nothing else to do: the save fails or finishes on its own, and the store shows that.
      })
    }
  }

  const { ref: modalRef, onKeyDown } = useModalDialog(onClose)
  useEffect(() => {
    dialogRef.current = modalRef.current
  })

  function onDialogKey(event: KeyboardEvent): void {
    const target = event.target
    // A key a menu already answered (the ArrowDown that opens it, the arrows that move in it) is
    // the menu's; and while a menu is open, every key is the menu's, so an `ArrowUp` there never
    // also moves the volume.
    if (event.defaultPrevented || menuOpen) return
    // The native video's own controls take Space and the arrows when they have the focus: the
    // player must not take the keys away from them.
    if (
      target instanceof HTMLVideoElement &&
      target.controls &&
      NATIVE_CONTROL_ACTIONS.some((action) => matches('Player', action, event))
    ) {
      return
    }
    // A key from inside a range input or an open menu is the menu's, not the player's: an
    // `ArrowUp` on a listbox option moves the cursor, and the same key here would move the
    // volume. The menu's `role` on its items and its listbox names it either way.
    if (
      target instanceof HTMLElement &&
      (target.tagName === 'INPUT' ||
        target.getAttribute('role') === 'listbox' ||
        target.getAttribute('role') === 'option' ||
        (menuOpen && target.closest('[role="listbox"]') !== null))
    ) {
      return
    }
    const is = (action: string): boolean => matches('Player', action, event)
    if (is('Play or pause')) {
      event.preventDefault()
      togglePlay()
    } else if (is('Seek back')) {
      event.preventDefault()
      nudge(-SEEK_STEP)
    } else if (is('Seek forward')) {
      event.preventDefault()
      nudge(SEEK_STEP)
    } else if (is('Volume up')) {
      event.preventDefault()
      nudgeVolume(VOLUME_STEP)
    } else if (is('Volume down')) {
      event.preventDefault()
      nudgeVolume(-VOLUME_STEP)
    } else if (is('Full screen')) {
      event.preventDefault()
      toggleFullscreen()
    } else if (is('Mute or unmute')) {
      event.preventDefault()
      if (videoRef.current !== null) videoRef.current.muted = !videoRef.current.muted
    } else if (is('Next audio track')) {
      event.preventDefault()
      nextAudio()
    } else if (is('Next subtitle track')) {
      event.preventDefault()
      nextSubtitle()
    }
  }

  const audioOptions: SubtitleOption[] =
    choice?.kind === 'remux'
      ? choice.audioOptions.map((option) => {
          // A track whose buffer failed on this system is listed the way a codec the browser
          // refuses is: named, marked not playable, and never asked for again.
          const dead = !option.playable ||
            (audioRetry?.failed.includes(String(option.track.id)) ?? false)
          return {
            key: String(option.track.id),
            text: trackLabel(option.track) + (dead ? ': not playable here' : ''),
            disabled: dead,
          }
        })
      : []
  const subtitleChoices = subtitleOptions()
  // The Seek bar's position: where a drag is, or where the playhead is.
  const shownSec = scrub ?? time / 1000

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
      className={`${MODAL_CENTRED} flex h-[min(700px,calc(100dvh-48px))] w-[min(960px,calc(100vw-48px))] flex-col overflow-hidden rounded-overlay border border-rule bg-canvas text-body max-[640px]:h-dvh max-[640px]:w-full max-[640px]:rounded-none max-[640px]:border-0 max-[640px]:max-w-none max-[640px]:max-h-none max-[640px]:m-0`}
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
          <p className="max-w-sm text-[14px] text-strong">
            {probe.tracks.some((entry) => entry.kind === 'video')
              ? unplayableCopy(displaySafe(choice.codec))
              : NO_VIDEO_COPY}
          </p>
          <DownloadOffer
            download={fileDownload}
            disabled={downloading}
            onDownload={() => {
              void download()
            }}
            onCancel={cancelFileDownload}
          />
          {downloadProblem !== null && (
            <p role="alert" className="max-w-sm text-[13px] text-error">
              {displaySafe(downloadProblem)}
            </p>
          )}
        </div>
      )}

      {probe !== null && choice?.kind === 'native' && (
        <div className="flex min-h-0 flex-1 items-center justify-center bg-black">
          {/* `playsInline`: without it iPhone Safari takes the video into its own fullscreen
              player on `play()`. */}
          <video ref={videoRef} src={nativeSrc ?? undefined} controls playsInline className="max-h-full max-w-full" />
        </div>
      )}

      {probe !== null && choice?.kind === 'remux' && (
        <>
          <div className="relative flex min-h-0 flex-1 items-center justify-center bg-black">
            {/* `playsInline`: without it iPhone Safari takes the video into its own fullscreen
                player on `play()`, past the controls, the menus and the subtitles drawn here. */}
            <video ref={videoRef} playsInline className="h-full w-full max-h-full max-w-full object-contain" />
            {(waiting || reopening) && (
              <div className="absolute inset-0 flex items-center justify-center">
                <span className="rounded-card bg-canvas/80 px-4 py-2 text-[13px] text-strong">{WAITING_COPY}</span>
              </div>
            )}
            {failed && (
              <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 px-6 text-center">
                <span className="max-w-md rounded-card bg-canvas/80 px-4 py-2 text-[13px] text-strong">
                  {errorCopy}
                </span>
                {errorDetail !== null && <span className="max-w-md text-[12px] text-muted">{errorDetail}</span>}
                {showHevcHint && <span className="max-w-md text-[12px] text-muted">{HEVC_WINDOWS_COPY}</span>}
                {/* A video that will not play here is still the person's file: the Download offer
                    the unplayable screen gives stays reachable from the failure. */}
                <DownloadOffer
                  download={fileDownload}
                  disabled={downloading}
                  onDownload={() => {
                    void download()
                  }}
                  onCancel={cancelFileDownload}
                />
                {downloadProblem !== null && (
                  <p role="alert" className="max-w-sm text-[13px] text-error">
                    {displaySafe(downloadProblem)}
                  </p>
                )}
              </div>
            )}
          </div>
          {audioNotice && (
            <div role="status" aria-live="polite" className="border-t border-ink-700 px-4 py-2 text-[13px] text-muted">
              {AUDIO_RETRY_COPY}
            </div>
          )}
          <div className="flex flex-wrap items-center gap-x-2 gap-y-2 border-t border-ink-700 px-3 py-2.5">
            {/* The Seek bar gets its own full-width row on a phone, above the other controls:
                sharing the row with the buttons and the volume slider squeezed it to ~30 px, so
                it could not be used. The time text and the buttons stay on the controls row. */}
            <input
              type="range"
              aria-label="Seek"
              min={0}
              max={durationSourceSec}
              step={0.1}
              value={shownSec}
              aria-valuetext={`${clock(shownSec)} of ${clock(durationSourceSec)}`}
              onPointerDown={(event) => startDrag(event.currentTarget)}
              onChange={(event) => {
                const value = Number(event.target.value)
                // A drag shows the value and seeks on release; a key step seeks at once.
                if (draggingRef.current) setScrub(value)
                else commitSeek(value)
              }}
              className="h-1.5 w-full shrink-0 min-[641px]:w-auto min-[641px]:min-w-0 min-[641px]:shrink flex-1 cursor-pointer appearance-none rounded-full bg-ink-600 accent-accent max-[640px]:order-first max-[640px]:flex-none"
            />
            <span className="font-mono text-[12px] text-strong">
              {clock(shownSec)} / {clock(durationSourceSec)}
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
              className="h-1.5 w-20 min-[420px]:w-24 max-[640px]:hidden cursor-pointer appearance-none rounded-full bg-ink-600 accent-accent"
            />
            {audioOptions.length > 0 && (
              <ChoiceMenu
                label="Audio"
                options={audioOptions}
                selected={playingAudioKey ?? ''}
                onChoose={chooseAudio}
                onMenuChange={setAudioMenuOpen}
              />
            )}
            {subtitleChoices.length > 0 && (
              <ChoiceMenu
                label="Subtitles"
                options={subtitleChoices}
                selected={subtitleKey}
                onChoose={setSubtitleKey}
                onMenuChange={setSubtitleMenuOpen}
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
