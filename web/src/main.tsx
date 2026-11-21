/**
 * The entry point: mounts {@link App} in `#root`.
 *
 * There is no router here yet and no session to resolve: Task 4 is the skeleton the screens are built
 * on, so this file does one thing, and `theme.css` is imported here rather than linked from
 * `index.html` because the CSS has to go through Vite for Tailwind to process it and for the fonts to
 * be self-hosted.
 *
 * @module main
 */
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './theme.css'
import App from './App'

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
