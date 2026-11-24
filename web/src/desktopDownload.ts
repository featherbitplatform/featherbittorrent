/**
 * How the player saves a file in the desktop app: nothing in a browser, and inside the app a call to the
 * shell, which asks where to put the file and streams it there.
 *
 * In a browser the stream address is on the page's own origin and an `<a download>` saves it. In the
 * app the page's origin is not the engine's, and a webview ignores `download` on a link to another
 * origin: the click would take the whole window to the media address. The shell saves it instead, and
 * never holds the file in memory.
 *
 * It lives apart from `desktop/` so the player can read it without the browser build reaching for the
 * shell: the default is `null`, which is a browser, and only the desktop page provides a value.
 * Besides the shell's three calls, the value carries the downloads the app is running: one per file,
 * keyed by `${torrentId}:${fileIndex}`, kept above the player so closing it neither cancels a download
 * nor forgets it, and reopening the player on the same file shows the download where it stands.
 *
 * @module desktopDownload
 */
import { createContext, useContext } from 'react'

/**
 * The shell's save, in two steps, so the stream address's short-lived token is fetched only once the
 * person has chosen where the file goes: a token fetched first could run out while the dialog is open.
 */
export interface DesktopDownload {
  /** Asks where to save a file called `name`. Resolves an opaque handle for the place, or null when the person cancelled; the path stays in the shell. */
  choose(name: string): Promise<string | null>
  /** Saves the file at `url` (an absolute stream address on the instance shown) to the place `handle` names. `onProgress` hears what the shell has written as it goes. Rejects with the line to show. */
  save(handle: string, url: string, onProgress: (progress: DownloadProgress) => void): Promise<void>
  /** Asks the shell to stop the download running under `handle`. */
  cancel(handle: string): Promise<void>
  /** The download of file `key` (`${torrentId}:${fileIndex}`) as of now, or null when the app has none. Read it beside {@link DesktopDownload.subscribe}: every change calls the listener. */
  current(key: string): FileDownload | null
  /** Calls `changed` whenever any file's download changes; answers the function that stops listening. */
  subscribe(changed: () => void): () => void
  /** Marks file `key` as having its save dialog open: while that stands, no other player opened on the file can start a second save of it. */
  choosing(key: string): void
  /** Takes the record of file `key` down: the dialog was answered with no place, or its failure was dismissed. */
  forget(key: string): void
  /** Runs the download of file `key`: saves the file at `url` to the place `handle` names and keeps {@link DesktopDownload.current} fresh while it runs. `instanceId` is the instance the save streams from — the store does not know it otherwise, and the shell's save needs it. Resolves when the file is saved or the download was cancelled, and rejects with the shell's line on any other failure — keeping a `failed` record with that line, for the next player opened on the file to show. */
  run(key: string, handle: string, url: string, instanceId?: string): Promise<void>
}

/** One report from the shell while it writes: `received` bytes so far, and `total`, the file's size, null when the answer brought no length to report against. */
export interface DownloadProgress {
  received: number
  total: number | null
}

/**
 * One file's download, kept above the player: the shell's `handle`, how far the writing has
 * gotten, and `state`: `choosing` while the save dialog is open, `saving` while bytes arrive, and
 * `saved`, `cancelled` or `failed` once the download ends. A `failed` download keeps the shell's
 * line in `line`, for the next player opened on the file to show.
 */
export interface FileDownload {
  handle: string
  received: number
  total: number | null
  state: 'choosing' | 'saving' | 'saved' | 'cancelled' | 'failed'
  /** The shell's line, kept for a `failed` download. */
  line?: string
}

/** The line the shell rejects a cancelled download with, and the line the player then shows. */
export const CANCELLED_COPY = 'Download cancelled.'

/**
 * Builds the context value for one instance connection: `choose`, `save`, and `cancel` are the
 * shell's own as the bridge spells them, and beside them the store of running downloads the player
 * reads. {@link DesktopDownload.run} is the store's side of a save: it keeps
 * {@link DesktopDownload.current} fresh from the progress reports and settles the download when the
 * shell is done.
 */
export function createDownloads(
  choose: (name: string) => Promise<string | null>,
  save: (
    handle: string,
    url: string,
    onProgress: (progress: DownloadProgress) => void,
    instanceId?: string,
  ) => Promise<void>,
  cancel: (handle: string) => Promise<void>,
): DesktopDownload {
  const files = new Map<string, FileDownload>()
  const listeners = new Set<() => void>()
  const changed = (): void => {
    for (const notify of listeners) notify()
  }
  const put = (key: string, download: FileDownload | null): void => {
    if (download === null) files.delete(key)
    else files.set(key, download)
    changed()
  }
  const settle = (key: string, state: 'saved' | 'cancelled'): void => {
    const was = files.get(key)
    if (was !== undefined) put(key, { ...was, state })
  }
  return {
    choose,
    save,
    cancel,
    current: (key) => files.get(key) ?? null,
    subscribe(notify: () => void): () => void {
      listeners.add(notify)
      return () => {
        listeners.delete(notify)
      }
    },
    choosing(key: string): void {
      put(key, { handle: '', received: 0, total: null, state: 'choosing' })
    },
    forget(key: string): void {
      put(key, null)
    },
    async run(key: string, handle: string, url: string, instanceId?: string): Promise<void> {
      put(key, { handle, received: 0, total: null, state: 'saving' })
      try {
        await save(handle, url, (progress) => {
          const was = files.get(key)
          if (was !== undefined && was.state === 'saving') {
            put(key, { ...was, received: progress.received, total: progress.total })
          }
        }, instanceId)
        settle(key, 'saved')
      } catch (problem) {
        const line = problem instanceof Error ? problem.message : String(problem)
        if (line === CANCELLED_COPY) {
          settle(key, 'cancelled')
          return
        }
        // The failure stays on the record with the shell's line: a player that opens on this
        // file later shows it, and only dismissing it or starting again takes it down.
        const was = files.get(key)
        put(key, {
          handle: was?.handle ?? handle,
          received: was?.received ?? 0,
          total: was?.total ?? null,
          state: 'failed',
          line,
        })
        throw problem
      }
    },
  }
}

/**
 * The store's records re-keyed per instance: `current`, `run`, `choosing` and `forget` take the
 * page's file key and add `instanceId`, and `run` hands it to the shell's save too. The view is
 * built for the instance the page shows at the time, so switching the instance neither drops
 * another instance's running download nor shows it as the new instance's own. The rest passes
 * straight through.
 */
export function scopedDownloads(store: DesktopDownload, instanceId: string): DesktopDownload {
  const at = (key: string): string => `${instanceId}:${key}`
  return {
    choose: (name) => store.choose(name),
    save: (handle, url, onProgress) => store.save(handle, url, onProgress),
    cancel: (handle) => store.cancel(handle),
    current: (key) => store.current(at(key)),
    subscribe: (changed) => store.subscribe(changed),
    choosing: (key) => store.choosing(at(key)),
    forget: (key) => store.forget(at(key)),
    run: (key, handle, url) => store.run(at(key), handle, url, instanceId),
  }
}

export const DesktopDownloadContext = createContext<DesktopDownload | null>(null)

/** The shell's save for the screen this is rendered in, or null in a browser. */
export function useDesktopDownload(): DesktopDownload | null {
  return useContext(DesktopDownloadContext)
}
