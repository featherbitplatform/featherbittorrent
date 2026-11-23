/**
 * The entry point: mounts {@link App} in `#root`, or, inside the desktop app, the desktop page around it.
 *
 * The desktop part is reached only through `import('./desktop')`, so a browser's first load never
 * fetches it or the Tauri API it uses. The check here is the same one `desktop/bridge.ts` makes
 * (`inDesktop`), written out because importing the bridge to ask would load what this file is keeping
 * out. `theme.css` is imported here rather than linked from `index.html` because the CSS has to go
 * through Vite for Tailwind to process it and for the fonts to be self-hosted.
 *
 * @module main
 */
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './theme.css'
import App from './App'

const root = createRoot(document.getElementById('root')!)

if ('__TAURI_INTERNALS__' in window) {
  void import('./desktop').then(({ DesktopRoot }) => {
    root.render(
      <StrictMode>
        <DesktopRoot />
      </StrictMode>,
    )
  })
} else {
  root.render(
    <StrictMode>
      <App />
    </StrictMode>,
  )
}
