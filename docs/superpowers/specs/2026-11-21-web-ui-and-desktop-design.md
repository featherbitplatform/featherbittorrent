# featherbittorrent: web UI and desktop app (draft)

Status: **draft for the owner's review**, written 2026-11-21 overnight from one sentence of the owner's. Nothing here is built yet. The section 10 questions need answers before a plan is written.

Tags, as in `2026-11-14-featherbittorrent-design.md`:
- **[owner]**: the owner said it.
- **[builder's call]**: proposed while writing this draft. Any of these can be changed in review.

Prototype of the screens: the "featherbittorrent web UI" canvas (Torrents, Add torrent, Sign in, Desktop app), built on the Graphite tokens in `../design-system-v2/`.

## 1. Intent

The owner, 2026-11-21: "we are going to have a featherbit version that also exposes the web UI (a full version and a headless version) and a Tauri desktop application". **[owner]**

The binding design already lists "the web UI, and the Tauri desktop builds that reuse it" as a later goal, with browser streaming when the web UI exists. **[owner, 2026-11-14]** fbt-api's control API, SSE event stream and streaming routes were built so a web UI could sit on them.

## 2. What gets built

**One web front end** serves three ways of running it. **[builder's call]**

| Shell | What runs | Who it is for |
|---|---|---|
| **Headless** | `featherbittorrent serve` with the web UI served at `/` beside the API. No terminal UI. | A Raspberry Pi or a NAS, used from any browser. |
| **Full** | The `featherbittorrent` binary, both roles. The TUI and `serve`, each able to expose the web UI. | A machine where someone wants both the terminal and a browser. |
| **Desktop** | A Tauri app with the same front end in a native window. It runs the engine in-process ("This computer"), or connects to a server like the remote TUI does. | Desktop users who want neither a terminal nor a browser tab. |

My reading of "a full version and a headless version" is two builds of the same binary:
- **full:** TUI plus web UI, every feature;
- **headless:** server plus web UI, with no TUI code compiled in, for the smallest footprint on a Pi.

This is a guess. Question 1.

## 3. Goals

1. **Feature parity with the TUI.** **[builder's call]** The web UI has every TUI screen:
   - the torrent table with state filters, sorting and a filter box;
   - the detail pane with Files, Peers, Trackers and Pieces;
   - add torrent, by magnet or `.torrent` upload, with the allowed-folder picker and file selection;
   - pause, resume and delete, with delete asking whether to keep or remove the files;
   - file priorities;
   - the settings screen: limits and active torrents;
   - devices;
   - the connect screen, on the desktop.
2. **Browser streaming.** **[owner, 2026-11-14: "When the web UI exists, it also gets browser streaming"]** A file plays in an HTML5 `<video>` through a short-lived stream token (`POST /api/v1/stream-tokens`, already built for the TUI). If the browser cannot play the container or codec (mkv and HEVC on most browsers), the UI says so and offers the download link instead. No transcoding. **[builder's call]**
3. **Snappy and light, like the TUI.** **[owner: the 2026-11-14 goal applies]**
   - Live updates come from the SSE stream; nothing polls.
   - The table renders only its visible rows, so hundreds of torrents scroll smoothly.
   - The embedded bundle stays small: a target of under 300 KB gzipped, excluding fonts. **[builder's call]**
   - The headless server's memory on a Pi 2B grows only by the static files it serves.
4. **The same Graphite look.** **[owner: design-system-v2 for every UI]**
   - Dark-first ink, violet as the only accent, Geist for text and Geist Mono for every real value.
   - Lucide icons, no emoji, no em-dashes.
   - Green for seeding, amber for stalled, cyan for the connection, red for errors, as in the TUI.
5. **Works at phone width**: the sidebar stacks, the toolbar wraps, and the table scrolls sideways. **[builder's call]**

## 4. Non-goals

- Search across indexing sites, and one UI on several servers at once. These stay "later" exactly as in the TUI design. **[owner, 2026-11-14]**
- Transcoding for the browser. **[builder's call]**
- Read-only users or per-user permissions: every signed-in client may do everything. **[builder's call, as in the TUI design]**

## 5. Architecture **[builder's call, within the decisions above]**

- **Front end.**
  - React, Vite, TypeScript, Tailwind, `@fontsource-variable/geist` and `geist-mono`, and `lucide-react`: the gateway's admin UI stack (`../gateway/ui`), so both products share tooling and look.
  - It lives in this repo under `web/`.
  - It talks only HTTP: fbt-api's JSON routes, the `/api/v1/events` SSE stream and the streaming route, plus featherbittorrent's own routes (`/api/v1/auth/*`, `/api/v1/fs/*`, `/api/v1/stream-tokens`).
  - It has no state of its own beyond the session token and view preferences.
- **Served from the binary.**
  - `fbtt-server` embeds the built `web/dist` with `rust-embed`, as the gateway does, behind a cargo feature `web-ui`.
  - With the feature off, nothing of the front end is compiled in.
  - The static routes sit outside `/api/v1/`, so fbt-api's path rules are untouched.
  - The web UI is served from the API's own origin, so no CORS is needed.
- **The desktop app.**
  - A new crate `featherbittorrent-desktop` (Tauri 2) loads the same `web/dist`.
  - For "This computer", it starts the engine in-process, with the same library lock the TUI uses. It also starts `fbtt-server` on `127.0.0.1` with a random per-launch token, and the webview talks HTTP to it. There is one client code path, the browser's; no Tauri-specific API calls; and the TUI's local-library rules (one engine per library, reopen the last used) apply unchanged.
  - For a remote server, the webview talks to that server directly. That needs the desktop's origin in the server's `cors_origins`, which fbt-api already has. Question 4.
- **Auth in a browser.**
  - User name and password, against the existing login routes.
  - The session token is kept in memory and `sessionStorage`, never `localStorage`. Question 5.
  - Device keys: a browser can hold a WebCrypto Ed25519 key that cannot be exported, kept in IndexedDB. It becomes an authorized device once approved, from a signed-in session or `featherbittorrent device add`. The challenge-response is the TUI's. Question 6.
  - Behind the gateway, its `openid-connect` plugin already covers browsers, per the 2026-11-15 decision.
- **Builds and CI.**
  - Node, pinned, builds `web/` before cargo in CI and in the release workflow.
  - New gates: `npm audit` (as fbterm's `website/` has), eslint and typecheck, and vitest for the client code.
  - The desktop app gets its own release jobs: `.dmg`, `.msi` and `.AppImage`/`.deb`, unsigned until signing is decided, like fbterm.

## 6. Screens (in the prototype)

- **Torrents.**
  - Sidebar: state filters with counts, and the current limits.
  - Header: the connection (name, scheme, login method), total rates, DHT nodes and Add torrent.
  - The table, with toolbar actions for the selection.
  - The detail pane, with the four tabs. Files offers "Play in browser".
- **Add torrent.**
  - Magnet or file.
  - The destination tree, limited to the allowed folders.
  - File selection with the selected size, and "Start paused".
- **Sign in.**
  - Password and browser device key.
  - The plain-http warning, the same rule as the TUI's basic-auth warning.
- **Desktop.**
  - The connection switcher: This computer, saved servers, Add a server.
  - Settings.
  - Desktop-only choices: keep running in the tray, and handle magnet links and `.torrent` files.

## 7. Milestones (proposal)

1. **Web UI on the headless server.**
   - `web/`, the `web-ui` feature, and the torrent table and detail pane with live updates.
   - Add, pause, resume and delete, and sign-in with a password.
   - CI and release with the front end built in.
2. **The rest of parity:**
   - file priorities;
   - settings;
   - devices;
   - browser device keys;
   - browser streaming.
3. **The desktop app:**
   - the Tauri crate, This computer and remote servers;
   - the tray and file associations;
   - the release jobs.
4. **The full and headless build split:** the feature matrix and packaging for both (question 1).

## 8. Anti-goals

- **A heavy page:** a framework runtime or component library that blows the bundle target, or polling where the SSE stream exists.
- **A second client code path for the desktop.** It must use the same HTTP client as the browser.
- **Secrets in browser storage that survives the tab**, or a password on any URL.
- **A desktop "This computer" that ignores the library lock** and runs beside a TUI or `serve` on the same library.

## 9. Left to the builder

The component structure, state management (plain React, or a small store), the routing library, and the exact table virtualisation.

## 10. Questions for the owner

1. **"A full version and a headless version".** Is it the two builds in section 2 (full = TUI + web UI, headless = server + web UI, no TUI), or something else, for example the web UI only in the desktop app and the headless server?
2. **Should `serve` expose the web UI by default?** Or only when it's turned on, with a `server.web_ui: true` setting or a wizard question?
3. **Should the local TUI also serve the web UI** for its embedded engine, for example on `127.0.0.1` so a browser on the same machine can watch it? That means the TUI starts a server, which the 2026-11-14 design avoided unless asked for.
4. **Desktop to a remote server:** should the webview talk to it directly (it needs CORS on the server), or go through a small Rust proxy inside the app (no server change, but a second path)?
5. **Sessions in the browser:** a token in `sessionStorage` (sign in again per tab or after a restart), or an HttpOnly cookie that survives a restart (it needs CSRF protection)?
6. **Browser device keys:** do you want them in milestone 2, or is password plus the gateway's OpenID Connect enough for browsers?
7. **Desktop scope:** tray, start at login, and magnet and `.torrent` file associations. Are all three wanted in the first desktop release?
8. **Versioning:** does the desktop app ship with the same version number as the binary, in the same release, or on its own schedule?
