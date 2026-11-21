import { readFileSync } from 'node:fs'
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
 *
 * The header must say which featherbittorrent this is, and `GET /session` cannot say it: that
 * answer carries the engine library's version. So the workspace version is read out of the root
 * `Cargo.toml` at build time - a regex over the `[workspace.package]` section, no TOML dependency -
 * and handed to the app as `__APP_VERSION__` through Vite's `define`, typed in
 * `src/app-version.d.ts`.
 */

/** The `version` under `[workspace.package]` in the workspace root's `Cargo.toml`. Other `version` assignments live further down that file, so the search starts at the section header. */
function workspaceVersion(): string {
  const manifest = readFileSync(new URL('../Cargo.toml', import.meta.url), 'utf8')
  const match = manifest
    .slice(manifest.indexOf('[workspace.package]'))
    .match(/^version = "([^"]+)"/m)
  if (match === null || match[1] === undefined) {
    throw new Error('no version line in the [workspace.package] section of the root Cargo.toml')
  }
  return match[1]
}

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(workspaceVersion()),
  },
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:8090',
    },
  },
})
