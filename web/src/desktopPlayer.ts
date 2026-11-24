/**
 * How the player hands a file to the system's media player: nothing in a browser, and inside the app a
 * call to the shell, which starts VLC or mpv on the file.
 *
 * The webview only plays what the system's decoders take. The shell's players bring their own, and they
 * read the stream address from a private playlist rather than a command line, so a stream token is not
 * left in a process listing. Like `desktopDownload`, this lives apart from `desktop/` so the player can
 * read it without the browser build reaching for the shell: the default is `null`, which is a browser,
 * and only the desktop page provides a value.
 *
 * @module desktopPlayer
 */
import { createContext, useContext } from 'react'

/** The shell's start of a system player on one file. */
export interface DesktopPlayer {
  /** Starts the system's media player on the file at `url` (an absolute stream address on the instance shown). Resolves the line saying what was opened; rejects with the line to show. */
  open(url: string, title: string): Promise<string>
}

export const DesktopPlayerContext = createContext<DesktopPlayer | null>(null)

/** The shell's player for the screen this is rendered in, or null in a browser. */
export function useDesktopPlayer(): DesktopPlayer | null {
  return useContext(DesktopPlayerContext)
}
