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
  /** Saves the file at `url` (an absolute stream address on the instance shown) to the place `handle` names. Rejects with the line to show. */
  save(handle: string, url: string): Promise<void>
}

export const DesktopDownloadContext = createContext<DesktopDownload | null>(null)

/** The shell's save for the screen this is rendered in, or null in a browser. */
export function useDesktopDownload(): DesktopDownload | null {
  return useContext(DesktopDownloadContext)
}
