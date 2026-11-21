import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

/**
 * The build of the browser UI.
 *
 * The built app is served by `fbtt-server` itself, beside the API and on the API's own origin, so a
 * served page and an API call are the same host and no CORS is involved. That is also why nothing here
 * rewrites the paths it emits: `fbtt-server` serves `/`, `/favicon.png` and `/assets/{*path}` and
 * nothing else, so the default absolute `/assets/` is what the router answers.
 *
 * The dev server is the one place that needs a proxy: it runs on its own port, and the API it talks to
 * is a `featherbittorrent serve` started beside it. `8090` is that server's default bind. The front
 * end routes with the URL hash, so no history fallback is configured in either place.
 */
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:8090',
    },
  },
})
