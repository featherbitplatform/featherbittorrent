/**
 * How the player saves a file in the desktop app: nothing in a browser, and inside the app a call to the
 * shell, which asks where to put the file and streams it there.
 *
 * In a browser the stream address is on the page's own origin and an `<a download>` saves it. In the
 * app the page's origin is not the engine's, and a webview ignores `download` on a link to another
 * origin: the click would take the whole window to the media address. The page hands the address to
 * the shell instead, which never holds the file in memory.
 *
 * It lives apart from `desktop/` so the player can read it without the browser build reaching for the
 * shell: the default is `null`, which is a browser, and only the desktop page provides a value.
 *
 * @module desktopDownload
 */
import { createContext, useContext } from 'react'

/**
 * Save the file at `url` (an absolute stream address on the instance the page is connected to) under a
 * name the person chooses, starting from `name`. Resolves `true` once saved and `false` when the person
 * cancelled; rejects with the line to show.
 */
export type SaveDownload = (url: string, name: string) => Promise<boolean>

export const DesktopDownloadContext = createContext<SaveDownload | null>(null)

/** The shell's save for the screen this is rendered in, or null in a browser. */
export function useDesktopDownload(): SaveDownload | null {
  return useContext(DesktopDownloadContext)
}
