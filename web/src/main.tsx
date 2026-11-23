/**
 * The entry point: mounts {@link App} in `#root`, or, inside the desktop app, the desktop page around it.
 *
 * The desktop part is reached only through `import('./desktop')`, so a browser's first load never
 * fetches it or the Tauri API it uses. The check here is the same one `desktop/bridge.ts` makes
 * (`inDesktop`), written out because importing the bridge to ask would load what this file is keeping
 * out. That import can fail - a release that replaced the file under an open window - and the window
 * then says so in one line and logs the reason, rather than staying blank. `renderDesktop` takes its
 * loader as a parameter for the same reason the rest of the page takes its `fetch`: so a test can
 * hand it one that fails. `theme.css` is imported here rather than linked from `index.html` because
 * the CSS has to go through Vite for Tailwind to process it and for the fonts to be self-hosted.
 *
 * @module main
 */
import { StrictMode } from 'react'
import type { ComponentType } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import './theme.css'
import App from './App'

/** What a person is told when the desktop part of the page is gone, and what to do about it. */
const DESKTOP_LOAD_FAILED = 'The desktop part of the page did not load. Restart featherbittorrent.'

/**
 * Mount the desktop page on `root`, fetching its chunk only here and only inside the shell. A load
 * that fails is logged and answered with one line, because a window that shows nothing at all says
 * nothing.
 */
export async function renderDesktop(
  root: Root,
  load: () => Promise<{ DesktopRoot: ComponentType }> = () => import('./desktop'),
): Promise<void> {
  try {
    const { DesktopRoot } = await load()
    root.render(
      <StrictMode>
        <DesktopRoot />
      </StrictMode>,
    )
  } catch (problem) {
    console.error('the desktop chunk did not load', problem)
    root.render(
      <main className="flex min-h-dvh flex-col items-center justify-center">
        <p role="alert" className="text-base font-medium text-strong">
          {DESKTOP_LOAD_FAILED}
        </p>
      </main>,
    )
  }
}

const root = createRoot(document.getElementById('root')!)

if ('__TAURI_INTERNALS__' in window) {
  void renderDesktop(root)
} else {
  root.render(
    <StrictMode>
      <App />
    </StrictMode>,
  )
}
