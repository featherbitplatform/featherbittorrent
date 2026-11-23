# Web UI milestone 2b Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The browser can do everything with a featherbittorrent server that the TUI can, plus the per-torrent actions the server already offers. Every new screen works one-handed on a phone at 390 px.

**Architecture:** Mostly front-end work in `web/`, over fbt-api and fbtt-server routes that already exist. The calls are added to the typed `Api` client, and each feature is a component wired into the existing Torrents screen. One server addition: `POST /api/v1/fs/mkdir` in `crates/fbtt-server/src/routes/fs.rs`. Below 640 px the torrent list becomes cards, and a torrent opens full screen.

**Tech Stack:** React + TypeScript + Vite + Tailwind 4, vitest + Testing Library. Rust (axum) for the one route.

**Spec:** `docs/superpowers/specs/2026-11-23-web-ui-milestone-2b-design.md` (the binding parent spec is `docs/superpowers/specs/2026-11-21-web-ui-and-desktop-design.md`).

## Global Constraints

- Product copy: no emoji, no em-dashes. Remote text (names, paths, server messages) goes through `displaySafe` before it is drawn.
- Colours only from the Graphite tokens already in `web/src/theme.css`.
- Bundle: within the 300 KB gzipped budget (`npm --prefix web run size`). No new npm dependency.
- Every change request goes through the existing client `request()`, so it carries the session cookie and the `X-CSRF-Token` header. No hand-written fetch.
- Rates: entered and shown in KiB/s, sent in bytes per second. 0 means unlimited.
- Phone: every new screen is usable at 390 px wide. The breakpoint is 640 px (`max-[640px]:` / `min-[641px]:`), the one the player uses.
- Rust: the repo's CLAUDE.md rules (no unsafe, workspace dependencies only, tests never touch the network). Gate: `cargo fmt --all --check`, clippy `-D warnings`, `cargo test --workspace --all-features`, doc `-D warnings`, `cargo deny check`.
- Web gate: `npm --prefix web run lint`, `typecheck`, `test`, `build`, `size`.
- Every new test gets a mutation patch that plants exactly its bug, proved once with `prove-test.sh`, and the test passes when run alone.
- No server API change other than Task 1's route.

## Review Focus

1. **A folder name that tries to leave the allowed folders** (`..`, an absolute path, a separator, a symlink pointing outside): refused, and nothing is created. Pinned in Task 1.
2. **A file-priority change on a torrent with pad files:** the whole list goes back with the pads unchanged, so changing one file never re-prioritises a pad or shifts indexes. Pinned in Task 3.
3. **A setting typed as text, negative or huge (more than 2^53 KiB/s):** refused in the form before any request. 0 and empty both mean unlimited. Pinned in Task 6.
4. **Devices when the server runs with logins off:** the devices route answers 403 `forbidden`. The section shows the logins-off line, not an error banner. Pinned in Task 7.
5. **The browser back button on a phone after opening a torrent:** it returns to the list, and does not leave the app or sign out. Pinned in Task 9.

---

### Task 1: `POST /api/v1/fs/mkdir` (server)

**Files:**
- Modify: `crates/fbtt-server/src/routes/fs.rs` (the handler, beside `list`)
- Modify: `crates/fbtt-server/src/routes/mod.rs:855` (the route, behind the same auth as `/api/v1/fs/list`)
- Modify: `crates/fbtt-core/src/fs_list.rs` only if a shared helper is needed (prefer reusing `inside_lexically` and whatever `list_dirs` uses to resolve symlinks)
- Test: `crates/fbtt-server/tests/server.rs` (route tests, using the existing seeded-server helpers), plus unit tests in fs.rs for the name check
- Docs: `docs/server.md` route table (one row)

**Interfaces:**
- Produces: `POST /api/v1/fs/mkdir`, JSON body `{ "parent": string, "name": string }`. Answers `201 { "path": string }` (the created folder: parent joined with the name, spelled the way `/api/v1/fs/list` spells paths). Errors use the existing `RouteError` shape:
  - `400 invalid_request`: a bad name or a body that is not this shape;
  - `403 path_not_allowed`: the parent is outside the allowed roots, or resolves outside them through a symlink;
  - `404 not_found`: the parent does not exist;
  - `409 already_exists`: the folder already exists.

- [ ] **Step 1: Write the failing tests**
  - `mkdir_creates_one_folder_inside_a_root`: POST with parent = the download folder and name `New Show`. Expect 201, `path` ends with `New Show`, and the folder exists on disk. A following `GET /api/v1/fs/list?path=<parent>` lists `New Show`.
  - `mkdir_refuses_names_that_are_not_one_component`: each of `""`, `"."`, `".."`, `"a/b"`, `"a\\b"`, `"/etc"`, `"x\u{0}y"`, `"x\ny"`, and a 256-byte name gets 400 `invalid_request`. Nothing new is created under the parent; compare the listing before and after.
  - `mkdir_refuses_a_parent_outside_the_roots`: parent = the temp dir's own parent gets 403 `path_not_allowed`.
  - `mkdir_does_not_follow_a_symlink_out` (`#[cfg(unix)]`): a symlink inside the root points to a folder outside it. Parent = that symlink gets 403, and nothing is created outside.
  - `mkdir_refuses_an_existing_folder`: the same name twice gives 201, then 409 `already_exists`.
  - `mkdir_needs_a_session_and_the_csrf_token` (logins on): with no session, 401. A browser session without `X-CSRF-Token`: refused, the same way the existing change routes refuse it.
- [ ] **Step 2:** `cargo test -p fbtt-server --all-features -- mkdir_`. Expected: FAIL (route missing).
- [ ] **Step 3: Implement `mkdir`.**
  - Validate the name as one component: the rules above, at most 255 bytes.
  - Check the parent lexically with `inside_lexically`. Then, on the blocking pool, resolve the parent (canonicalize) and check the resolved path is inside a root, the same way `list_dirs` guards listing.
  - Create with `std::fs::create_dir` (never `create_dir_all`) and map `AlreadyExists` to 409.
  - Log only the error kind, never the path.
- [ ] **Step 4:** the tests pass, then the Rust gate.
- [ ] **Step 5: Commit** `feat(server): create a folder inside the allowed folders`

### Task 2: The client calls

**Files:**
- Modify: `web/src/api/client.ts` (`Api` interface and `createClient`; widen `request()`'s method union to include `'PUT'`, with the CSRF header on PUT like the other non-GET methods)
- Test: `web/src/api/client.test.ts`

**Interfaces:**
- Produces (on `Api`; types from `operations`/`components` in `./schema` where they exist):
  - `setFilePriorities(id: string, priorities: Array<'skip' | 'low' | 'normal' | 'high'>): Promise<void>`: PUT `/api/v1/torrents/{id}/file-priorities` `{ priorities }`.
  - `setQueuePosition(id: string, position: number): Promise<void>`: PUT `.../queue-position` `{ position }`.
  - `recheck(id: string): Promise<void>`: POST `.../recheck`.
  - `reannounce(id: string): Promise<void>`: POST `.../reannounce`.
  - `setTorrentLimits(id: string, downloadBps: number | null, uploadBps: number | null): Promise<void>`: PUT `.../limits` `{ download, upload }`.
  - `moveStorage(id: string, path: string): Promise<void>`: POST `.../move` `{ path, mode: 'fail_if_exists' }`.
  - `setSessionLimits(downloadBps: number | null, uploadBps: number | null): Promise<void>`: PUT `/api/v1/session/limits`.
  - `queueSettings(): Promise<components['schemas']['QueueSettingsDto']>` and `setQueueSettings(settings: components['schemas']['QueueSettingsDto']): Promise<void>`: GET and PUT `/api/v1/session/queue`.
  - `devices(): Promise<DeviceRow[]>`, `addDevice(name: string, publicKey: string): Promise<DeviceRow>` (POST `{ name, public_key }`, 201), and `removeDevice(name: string): Promise<void>` (DELETE `/api/v1/auth/devices/{encodeURIComponent(name)}`). `export interface DeviceRow { name: string; fingerprint: string }`.
  - `mkdir(parent: string, name: string): Promise<string>`: POST `/api/v1/fs/mkdir`, returns `path`.

- [ ] **Step 1: Write the failing tests.** One test per call, `each call sends its method, path and body`: method, URL, JSON body, `content-type: application/json`, and the CSRF header present on every non-GET. Also `a device name is escaped in the path`: the name `a b/c` gives `/api/v1/auth/devices/a%20b%2Fc`. Also `an error answer keeps its code`: a 403 `{ error: { code: 'forbidden' } }` from `devices()` rejects with an `ApiError` whose `code === 'forbidden'`.
- [ ] **Step 2:** `npm --prefix web test -- client`. FAIL.
- [ ] **Step 3:** Implement with the existing `json()`/`nothing()` helpers. Add a small `sendJson(url, method, body)` helper only if `nothing()` cannot carry a body.
- [ ] **Step 4:** PASS, then the web gate.
- [ ] **Step 5: Commit** `feat(web): the client calls for priorities, torrent actions, settings, devices and folders`

### Task 3: File priorities in the Files tab

**Files:**
- Modify: `web/src/components/detail/Files.tsx` (the priority cell, around :111)
- Test: `web/src/components/DetailPane.test.tsx` (where the Files tab is tested today)

**Interfaces:**
- Consumes: `Api.files(id)`, `Api.setFilePriorities(id, priorities)` (Task 2).
- Produces: nothing other tasks use.

- [ ] **Step 1: Write the failing tests**
  - `a file's priority can be changed`: a torrent with three files. Choosing `skip` on file 1 sends `setFilePriorities(id, [p0, 'skip', p2])` with the others unchanged, and the cell shows `skip` after the next files refresh.
  - `pad files keep their priority`: the files include a pad at index 1, which is not shown. Changing file 2 sends a list of the same length with the pad's priority unchanged at index 1.
  - `an unknown priority is sent as normal`: a file reported `unknown` is sent as `normal` when another file changes, because requests have no `unknown` (PriorityDto).
  - `a refused change shows the server's reason`: a 409 shows the existing banner with the message through `displaySafe`, and the cell keeps the old value.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Replace the word with a native `<select>` (labelled `Priority of <file name>`), which works one-handed on a phone. Before sending, re-read the list with `files(id)` (read-modify-write, the TUI's rule in `crates/fbtt-core/src/remote/commands.rs:89-116`).
- [ ] **Step 4:** PASS, then the web gate.
- [ ] **Step 5: Commit** `feat(web): change a file's priority`

### Task 4: Per-torrent actions

**Files:**
- Create: `web/src/components/TorrentActions.tsx` (one menu of the actions, used on desktop and in the phone view of Task 9)
- Create: `web/src/components/TorrentLimits.tsx` (the limits dialog)
- Modify: `web/src/screens/Torrents.tsx` (place `TorrentActions` beside Pause/Resume/Remove for the selected torrent)
- Modify: `web/src/components/FolderPicker.tsx` only to be reused for Move (it already takes a starting folder; add nothing else here)
- Test: `web/src/components/TorrentActions.test.tsx`

**Interfaces:**
- Consumes: Task 2's `setQueuePosition`, `recheck`, `reannounce`, `setTorrentLimits`, `moveStorage`; `Api.roots/list`; and the torrent's `queue_position`, `auto_managed` and `state` from `TorrentInfo`.
- Produces: `export function TorrentActions(props: { torrent: TorrentInfo; client: Api; onError(message: string): void }): JSX.Element`.

- [ ] **Step 1: Write the failing tests**
  - `queue moves send absolute positions`: with `queue_position` 3: `Move up` sends 2, `Move down` 4, `Move to top` 0, `Move to bottom` 2147483647.
  - `queue controls show only for a queued torrent`: with `auto_managed` false or `queue_position` -1, there are no queue items.
  - `recheck and reannounce call the server`: each item calls its method once.
  - `torrent limits open empty and send bytes per second`: the dialog's two fields start empty, with the text `Sets a new limit. 0 removes it.`. Entering `500` and `0` sends `setTorrentLimits(id, 512000, null)`.
  - `move uses the picker and never overwrites`: choosing a folder sends `moveStorage(id, '<folder>')`. The client test already pins `mode: 'fail_if_exists'`. While it is pending the torrent shows `Moving...`. A 409 shows the reason in the banner.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implement. Desktop: one `More` menu button. Each item is at least 44 px high on a phone.
- [ ] **Step 4:** PASS, then the web gate.
- [ ] **Step 5: Commit** `feat(web): queue, recheck, reannounce, limits and move for a torrent`

### Task 5: `New folder` in the folder picker

**Files:**
- Modify: `web/src/components/FolderPicker.tsx`
- Test: `web/src/components/AddTorrent.test.tsx` (the picker is tested through Add torrent today)

**Interfaces:**
- Consumes: Task 1's route through Task 2's `Api.mkdir(parent, name)`.

- [ ] **Step 1: Write the failing tests**
  - `new folder creates and opens it`: `New folder`, type `Season 2`, then Create. This calls `mkdir('<shown folder>', 'Season 2')`, then the picker shows the returned path and lists it.
  - `a refused name stays in the form with the reason`: a 400 or 409 keeps the dialog open, shows the server's message through `displaySafe`, and the folder shown does not change.
  - `an empty name cannot be sent`: Create is disabled while the trimmed name is empty.
- [ ] **Step 2:** FAIL. **Step 3:** Implement: an inline text field, not a second modal. **Step 4:** PASS, then the web gate.
- [ ] **Step 5: Commit** `feat(web): create a folder in the folder picker`

### Task 6: The Settings screen and its Server section

**Files:**
- Create: `web/src/screens/Settings.tsx` (the screen, three sections; Devices and Shortcuts are filled by Tasks 7 and 8)
- Create: `web/src/components/settings/ServerSettings.tsx`
- Modify: `web/src/components/Header.tsx` (a `Settings` button beside `Sign out`)
- Modify: `web/src/App.tsx` (show Settings instead of Torrents while it is open; `Back to torrents` returns)
- Test: `web/src/components/settings/ServerSettings.test.tsx`, plus one App test for opening and leaving Settings

**Interfaces:**
- Consumes: `Api.session()` (`download_limit`, `upload_limit` in bytes per second), `Api.queueSettings()`, `Api.setSessionLimits`, `Api.setQueueSettings`.
- Produces: `export function Settings(props: { client: Api; onBack(): void }): JSX.Element`, with slots for `<Devices>` (Task 7) and `<Shortcuts>` (Task 8).

- [ ] **Step 1: Write the failing tests**
  - `settings open with the server's values`: with session limits 1048576 and 0 and queue `active_downloads` 3, `active_seeds` 5, `active_limit` 500, the fields show `1024`, `0`, `3`, `5` and `500`.
  - `save sends rates in bytes and keeps unknown queue fields`: change download to `2048` and active seeds to `8`, then Save. That sends `setSessionLimits(2097152, null)` and `setQueueSettings` with every field of the object it read, only `active_seeds` changed.
  - `a bad value is refused in the form`: `-1`, `abc` and `9007199254740993` each show an inline error and send nothing.
  - `empty means unlimited`: an emptied field saves as 0 (null for rates).
- [ ] **Step 2:** FAIL. **Step 3:** One Save button for the section (the spec leaves this to the builder; this plan picks one button so a half-typed value is never applied). **Step 4:** PASS, then the web gate.
- [ ] **Step 5: Commit** `feat(web): a Settings screen with the server's limits and queue`

### Task 7: Devices in Settings

**Files:**
- Create: `web/src/components/settings/Devices.tsx`
- Modify: `web/src/screens/Settings.tsx` (mount it)
- Test: `web/src/components/settings/Devices.test.tsx`

**Interfaces:**
- Consumes: Task 2's `devices`, `addDevice`, `removeDevice` and `DeviceRow`.

- [ ] **Step 1: Write the failing tests**
  - `devices are listed with their fingerprints`.
  - `adding a device shows the server's fingerprint`: name plus a pasted `ssh-ed25519 AAAA... comment` line, then Add, calls `addDevice(name, line)`. The new row shows the fingerprint from the 201 answer. The page computes no fingerprint itself.
  - `removing asks first`: `Remove` opens a confirmation, and only `Remove device` calls `removeDevice(name)`.
  - `logins off shows a line, not an error`: `devices()` rejects with 403 code `forbidden`. The section shows `Device logins need the server's logins on.` and no banner.
  - `changes are saved on the server`: the section shows `Changes are saved in the server's config.yaml.`
  - `a refused key shows the server's reason`: a 400 from `addDevice` shows the message through `displaySafe` and keeps the form filled.
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS, then the web gate.
- [ ] **Step 5: Commit** `feat(web): manage device logins in Settings`

### Task 8: One key list, and the Shortcuts section

**Files:**
- Create: `web/src/keys.ts`: `export interface KeyBinding { keys: string[]; action: string; where: 'Player' | 'Torrent list' | 'Detail tabs' | 'Dialogs' }` and `export const KEY_BINDINGS: readonly KeyBinding[]`
- Modify: `web/src/player/Player.tsx` (the key handler), `web/src/components/TorrentTable.tsx` (:419-422), `web/src/components/DetailPane.tsx` (:229-232): each handler decides from `KEY_BINDINGS` (for example a `matches(where, action, event)` helper in `keys.ts`) rather than from its own literals
- Create: `web/src/components/settings/Shortcuts.tsx`; modify `web/src/screens/Settings.tsx` to mount it
- Test: `web/src/keys.test.ts`, `web/src/components/settings/Shortcuts.test.tsx`; the existing Player, TorrentTable and DetailPane key tests must still pass unchanged

**Interfaces:**
- Produces: `KEY_BINDINGS`, `matches(where: KeyBinding['where'], action: string, event: KeyboardEvent | React.KeyboardEvent): boolean`.

- [ ] **Step 1: Write the failing tests**
  - `every handler's key is in the list`: for each binding, dispatching its key in its component triggers its action. Write one table-driven test per `where`.
  - `the screen lists every binding`: Shortcuts renders one row per `KEY_BINDINGS` entry, grouped by `where`.
  - `on a phone the section says it is for a keyboard`: at a 390 px viewport (a mocked `matchMedia('(max-width: 640px)')`), the note `For a keyboard.` is shown.
- [ ] **Step 2:** FAIL. **Step 3:** Move the literals into `KEY_BINDINGS` without changing any key's behaviour. **Step 4:** PASS, then all existing key tests, then the web gate.
- [ ] **Step 5: Commit** `feat(web): one list of keys, shown in Settings`

### Task 9: Phone layout: cards and the full-screen torrent

**Files:**
- Create: `web/src/components/TorrentCards.tsx` (the card list)
- Create: `web/src/components/TorrentFullView.tsx` (actions, then `DetailPane`'s tabs, full screen)
- Modify: `web/src/screens/Torrents.tsx` (below 640 px: cards instead of the table, filters as chips; a selected torrent opens the full view)
- Test: `web/src/components/TorrentCards.test.tsx`, `web/src/screens/Torrents.test.tsx`

**Interfaces:**
- Consumes: `TorrentActions` (Task 4), `DetailPane`, the existing filters (`web/src/filters.ts`) and `format.ts`.

- [ ] **Step 1: Write the failing tests**
  - `a card shows name, progress, state and the speed that matters`: downloading shows the download rate, seeding the upload rate, stalled the peer count (`0 peers`). The name goes through `displaySafe`.
  - `the phone shows cards and the desktop the table`: with `matchMedia('(max-width: 640px)')` true, cards; false, the table.
  - `tapping a card opens the full view and pushes history`: `history.pushState` is called once, and the view shows the actions and the four tabs.
  - `back returns to the list`: a `popstate` closes the full view without signing out or reloading.
  - `filters become chips`: each state filter is a button with its count, as in `StateFilters`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement with the Graphite tokens; cards per the mockup the owner chose (`.superpowers/brainstorm/7745-1795446671/content/phone-list.html`, option A). **Step 4:** PASS, then the web gate.
- [ ] **Step 5: Commit** `feat(web): torrent cards and a full-screen torrent on a phone`

### Task 10: Docs, CHANGELOG and the real-browser check

**Files:**
- Modify: `docs/web-ui.md` (a section per feature: priorities, torrent actions, Settings with devices and shortcuts, the phone layout, New folder), `docs/server.md` (if Task 1 did not already add the route row), `CHANGELOG.md` (one Added entry ending in `([#TBD](https://github.com/featherbitplatform/featherbittorrent/pull/TBD))`)

- [ ] **Step 1:** Write the docs from the code. Copy every label verbatim from the components.
- [ ] **Step 2 (controller):** The real-browser check in Chrome through Playwright, against a full build's `serve --offline` with logins on, reached by a host name, at 1280 and 390 px:
  - change a file's priority, and see the server's file list follow it;
  - queue up and down, recheck, reannounce, set and clear a torrent limit, move a torrent's files into a folder made with `New folder`;
  - Settings: change and save the limits, and the server's `GET /api/v1/session` shows them; add a device with a real `ssh-ed25519` key, see its fingerprint, then remove it;
  - phone: cards, open a torrent, browser back returns to the list;
  - no console errors other than the known pre-sign-in 401.
- [ ] **Step 3:** The full gate (web and Rust).
- [ ] **Step 4: Commit** `docs: the web UI's settings, torrent actions and phone layout`
