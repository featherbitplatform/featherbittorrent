# featherbittorrent: web UI and desktop app (draft)

Status: **all owner questions answered (2026-11-21)**. Ready for the owner's final read, then the milestone 1 plan. Nothing here is built yet.

Tags, as in `2026-11-14-featherbittorrent-design.md`:
- **[owner]**: the owner said it.
- **[builder's call]**: proposed while writing this draft. Any of these can be changed in review.

Prototype of the screens: the "featherbittorrent web UI" canvas (Torrents, Add torrent, Sign in, Desktop app), built on the Graphite tokens in `../design-system-v2/`.

## 1. Intent

The owner, 2026-11-21: "we are going to have a featherbit version that also exposes the web UI (a full version and a headless version) and a Tauri desktop application". **[owner]**

The binding design already lists "the web UI, and the Tauri desktop builds that reuse it" as a later goal, with browser streaming when the web UI exists. **[owner, 2026-11-14]** fbt-api's control API, SSE event stream and streaming routes were built so a web UI could sit on them.

## 2. What gets built

**One web front end, two builds of the binary, and a desktop app.**

The two builds come from the owner, 2026-11-21: "For headless I meant only the terminal app without the embedded web ui". **[owner]**

| Deliverable | What it is |
|---|---|
| **Full** | The `featherbittorrent` binary with the web UI compiled in: the TUI and `serve` as today, and `serve` also serves the web UI at `/` beside the API. |
| **Headless** | The `featherbittorrent` binary exactly as it ships today, TUI and `serve`, with no web UI compiled in. Nothing of the front end is in it. |
| **Desktop** | A Tauri app with the same front end in a native window. It runs the engine in-process ("This computer"), or connects to a server like the remote TUI does. |

The headless build contains no front-end assets and changes nothing for existing clients. It does carry the browser-session routes and the `web_ui` setting, so one config file and one API work with both builds. **[builder's call]** In cargo terms the split is the `web-ui` feature: full builds turn it on, headless builds leave it off. **[builder's call]** Both builds ship in every release, for every platform in the 2026-11-14 table. **[owner, 2026-11-21]** How the packages are named is question 1.

**The web UI is on by default in a full build.** A full build's `serve` serves it unless it is turned off. **[owner, 2026-11-21]** Two ways to turn it off, either one enough:
- the config setting `server.web_ui: false`;
- the environment variable `FBTT_WEB_UI=0`. This one wins over the file, like `FBTT_CONFIG_DIR` and `FBTT_DATA_DIR` do.

The names `server.web_ui` and `FBTT_WEB_UI` are the builder's call. The headless build accepts the setting and ignores it, with one log line, so one config file works with either build.

## 2a. The local TUI can serve the web UI on request

Owner, 2026-11-21: "Not automatic, but you could have it serving if the user chooses it after the app has opened. In that case the TUI app stops updating the screen displaying a message it's serving UI on a specific host port and then you open the browser. The user can go back to TUI." **[owner]**

**What the owner decided.**
- The local TUI never opens a port by itself.
- The user asks for the web UI from the running TUI. The TUI then stops drawing its normal screen and shows one message: the web UI is being served at a given host and port, so open a browser there.
- From that message the user can go back to the TUI.

**The rest is the builder's call:**
- **Builds.** Only the full build has the action. The headless build has no key for it and no hint in the footer.
- **The key.** A single key in the TUI footer, for example `w` for web UI, listed in `?`. It works only with the local engine. On a remote connection the server already serves its own web UI, so the key is hidden there.
- **The address.** It binds `127.0.0.1` on a free port by default, so it is reachable only from this machine. Opening it to the LAN would be an explicit setting, never the default.
  - The message shows the full URL, including a one-time sign-in token, so another user on the same machine cannot use the port.
  - With a session cookie, the token is exchanged for that cookie on the first request.
- **Going back.** Going back to the TUI stops the server and closes the port. The engine and the torrents keep running throughout, because the server only serves the engine the TUI already runs. One library, one engine.
- **Leaving the message screen.** Quitting from it is the same as quitting the TUI: the torrents stop and resume on the next start, as today.

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
3. **A real media player for video torrents.**

   Owner, 2026-11-21: "Let's not forget the media player with subtitles and multi audio support for streaming video torrents". **[owner]**

   **What browsers do on their own.** Not enough:
   - Choosing between audio tracks (`HTMLMediaElement.audioTracks`) works only in Safari. Chrome, Edge (and so the desktop app's WebView2 on Windows) and Firefox do not expose it.
   - A browser never sees subtitles that are inside a Matroska or MP4 file. Only external WebVTT files given through `<track>` work.

   **Mechanism.** **[owner agreed, 2026-11-21]** The server remuxes, never transcodes:
   - **Audio.** For a video file, the server reads its container (Matroska/WebM and MP4). It serves fragmented MP4 that carries the video track and **one chosen audio track**, copied as they are. The player fetches it through Media Source Extensions, and choosing another language reloads the audio stream at the current position.
   - **Subtitles.** Text subtitles inside the file (SRT, ASS/SSA, mov_text) and next to it (`.srt`, `.vtt`, `.ass` in the torrent) are served as WebVTT. The player shows them as tracks it can choose. ASS styling becomes plain text.
   - **What cannot play.** Image subtitles (PGS, VobSub) are listed but not shown. A video or audio codec the browser cannot decode (often HEVC, AC-3, DTS) gets a clear message, plus "Download" and, on the desktop, "Open in mpv or VLC".
   - **During download.** Playback can start before the download finishes, through fbt-lib's deadline-driven reader, as streaming already does.
   - **Cost.** Remuxing is copying, so it is light enough for the Pi. No new native dependencies, and the parsers are pure Rust, like everything else.

   **The player itself:**
   - the Graphite look and keyboard control;
   - choosers for the audio track and the subtitles;
   - a remembered position per file.

   It lives in milestone 2, with browser streaming.

4. **Snappy and light, like the TUI.** **[owner: the 2026-11-14 goal applies]**
   - The torrent list refreshes every 1 s and is woken early by the SSE event stream, as the remote TUI does: fbt-api's events carry no transfer rates. It refreshes every 10 s while the page is hidden. That pause is the web UI's own addition.
   - The table renders only its visible rows, so hundreds of torrents scroll smoothly.
   - The embedded bundle stays small: a target of under 300 KB gzipped, excluding fonts. **[builder's call]**
   - The full build's server, on a Pi 2B, grows only by the static files it serves; the headless build is unchanged.
5. **The same Graphite look.** **[owner: design-system-v2 for every UI]**
   - Dark-first ink, violet as the only accent, Geist for text and Geist Mono for every real value.
   - Lucide icons, no emoji, no em-dashes.
   - Green for seeding, amber for stalled, cyan for the connection, red for errors, as in the TUI.
6. **Works at phone width**: the sidebar stacks, the toolbar wraps, and the table scrolls sideways. **[builder's call]**

## 4. Non-goals

- Search across indexing sites, and one UI on several servers at once. These stay "later" exactly as in the TUI design. **[owner, 2026-11-14]**
- Transcoding for the browser. Remuxing is in scope (goal 3); re-encoding is not. **[builder's call]**
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
  - For a remote server, the webview talks to that server directly, cross-origin. The server lists the desktop's origin in `cors_origins`, which fbt-api already has. **[owner, 2026-11-21]** A `SameSite=Strict` cookie cannot cross origins, so against a remote server the desktop app signs in with the existing bearer-token login, the TUI's. The cookie session is for pages the server serves itself. The page's `connect-src` must allow the saved servers' origins in the desktop build only. **[builder's call]**
- **Auth in a browser.**
  - User name and password, against the existing login routes.
  - The session is an HttpOnly cookie that survives a browser restart, but not a server restart: sessions live in the server's memory. It is `Secure` over https, `SameSite=Strict`, and paired with a CSRF token on every state-changing request. No token in script-readable storage. **[owner: cookie, 2026-11-21]**
  - Browser device keys are not in scope for now. **[owner, 2026-11-21]**
  - Behind the gateway, its `openid-connect` plugin covers browsers, per the 2026-11-15 decision. A gateway that forwards its own `Authorization` header upstream must run with featherbittorrent's logins off (the 2026-11-15 "gateway only" setup). With logins on, that header is checked as a featherbittorrent bearer token and never falls back to the cookie. `docs/web-ui.md` says so. **[builder's call]**
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
  - Password. The prototype's "browser device key" button is out of scope for now (question 6).
  - The plain-http warning, the same rule as the TUI's basic-auth warning.
- **Desktop.**
  - The connection switcher: This computer, saved servers, Add a server.
  - Settings.
  - Desktop-only choices: keep running in the tray, and handle magnet links and `.torrent` files.

## 7. Milestones (proposal)

1. **The web UI in the full build.**
   - `web/`, the `web-ui` feature, and the torrent table and detail pane with live updates.
   - Add, pause, resume and delete, and sign-in with a password.
   - CI builds and tests both builds. The release ships the full build beside the headless one.
2. **The rest of parity:**
   - the local TUI's "serve the web UI" action (section 2a);
   - file priorities;
   - settings;
   - devices;
   - browser streaming, with the media player: its audio-track and subtitle choosers, built on the server-side remux in goal 3.
3. **The desktop app:**
   - the Tauri crate, This computer and remote servers;
   - the tray, start at login, `.torrent` file associations and the `magnet:` link handler;
   - the release jobs.

## 8. Anti-goals

- **A heavy page:** a framework runtime or component library that blows the bundle target, refreshing faster than the remote TUI does, or one poll per component instead of one refresh loop.
- **A second client code path for the desktop.** It must use the same HTTP client as the browser.
- **Secrets in browser storage that survives the tab**, or a password on any URL.
- **A desktop "This computer" that ignores the library lock** and runs beside a TUI or `serve` on the same library.

## 9. Left to the builder

The component structure, state management (plain React, or a small store), the routing library, and the exact table virtualisation.

## 10. Questions for the owner

1. Answered: the full build is `featherbittorrent`, the terminal-only build is `featherbittorrent-headless`, and both ship in every release. **[owner, 2026-11-21]**
2. Answered: on by default in a full build, off through `server.web_ui: false` or `FBTT_WEB_UI=0` (section 2).
3. Answered: not automatically, but on request at runtime (section 2a). **[owner, 2026-11-21]**
4. Answered: the webview talks to a remote server directly, through CORS on the server (`cors_origins`, already in fbt-api). **[owner, 2026-11-21]**
5. Answered: an HttpOnly session cookie that survives a browser restart (not a server restart: sessions are in memory). It is `Secure` over https, `SameSite=Strict`, and paired with a CSRF token on every state-changing request. **[owner, 2026-11-21: "cookie could work"]**
6. Answered: not now. Browsers sign in with a password, or through the gateway's OpenID Connect. **[owner, 2026-11-21]**
7. Answered: the first desktop release has all of these: the tray, start at login, `.torrent` file associations, and the `magnet:` link handler. **[owner, 2026-11-21]**
8. Answered: the desktop app carries featherbittorrent's version and ships in the same release. **[owner, 2026-11-21]**
9. Answered: yes, the server-side remux in goal 3. **[owner, 2026-11-21: "Agreed"]**
