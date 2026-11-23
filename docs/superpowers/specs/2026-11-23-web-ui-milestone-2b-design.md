# featherbittorrent web UI, milestone 2b: the browser does everything the server can

Status: draft for the owner's read (2026-11-23). Builds on `2026-11-21-web-ui-and-desktop-design.md` (milestone 2b in its section 7) and changes that list as described below.

Tags:
- **[owner]**: the owner said it.
- **[builder's call]**: proposed while writing this draft. Any of these can be changed in review.

## 1. Intent

The owner, 2026-11-23: "reaching the server through a TUI requires you to have another binary on the local machine, a webui is reachable anywhere a browser is available, even a smartphone". **[owner]**

After milestone 2b, a person with only a browser can do everything with a featherbittorrent server that the TUI can do, plus the per-torrent actions the server already offers and no client offers yet. A phone is a first-class target: every screen added in 2b works one-handed at 390 px wide. The 390 px check from milestone 2's player is the bar for every new screen. **[owner, intent; builder's call, the 390 px bar]**

## 2. Goals

### 2.1 File priorities
In the detail pane's Files tab, each file's priority can be changed: skip, low, normal, high. Today the tab only shows the priority as a word.
- The server route exists: `PUT /api/v1/torrents/{id}/file-priorities` takes the whole list, pad files included. The page reads the current list, changes one entry and sends the whole list back, as the TUI does (`crates/fbtt-core/src/remote/commands.rs`). **[builder's call]**
- The control is large enough to tap on a phone. **[builder's call]**
- Pad files are not shown and keep their priority. **[builder's call]**

### 2.2 Per-torrent actions (new: no client offers these yet)
The owner, asked whether 2b should also bring in server features neither the TUI nor the web UI has: "why can't we do these through webui?" **[owner]** Each one uses an fbt-api route that already exists:
- **Queue:** move up, move down, to top, to bottom (`/torrents/{id}/queue-position`).
- **Recheck** the data (`/torrents/{id}/recheck`).
- **Reannounce** to trackers (`/torrents/{id}/reannounce`).
- **This torrent's own limits:** download and upload rate (`/torrents/{id}/limits`). 0 means unlimited.
- **Move its files** to another folder the server allows (`/torrents/{id}/move`), chosen with the same folder picker as Add torrent.

They live with the torrent, not in Settings. On desktop, next to Pause and Remove for the selected torrent. On a phone, in the torrent's full-screen detail view (section 2.5). **[owner: "everything else is ok"; placement builder's call]**

How each one behaves, all **[builder's call]** after the builder check:
- **Queue.** The API takes an absolute position. Up and down send the torrent's `queue_position` minus or plus one; to top sends 0; to bottom sends a position past the end, which the engine treats as the bottom. The queue controls show only for a torrent that has a queue position (an auto-managed torrent); others show no queue controls.
- **Per-torrent limits.** The server has no way to read them back: `TorrentInfo` carries no limit fields. The form opens empty, says it sets a new limit, and 0 removes the limit. It does not pretend to know the current value.
- **Move.** Always the default mode (`fail_if_exists`), so nothing is overwritten. The folder picker offers only the server's allowed roots, as Add torrent does. While the move runs, the torrent shows `Moving...`; a refusal (409 or 422) shows in the existing banner with the server's reason, through `displaySafe`.

The exact request and answer shapes come from fbt-api's OpenAPI document at the pinned tag (`web/src/api/openapi.json`). If a route turns out to be missing or different there, the plan says so and that action is dropped rather than adding server code. **[builder's call]**

### 2.3 The Settings screen
One Settings screen, opened from the header next to Sign out. **[owner: "a settings screen where you could also see the keymaps and shortcuts"]** Three sections:

1. **Server:** the five settings the TUI's `S` screen has. **[owner, parity]**
   - download rate limit;
   - upload rate limit;
   - active downloads;
   - active seeds;
   - active torrents in total.
   0 means unlimited. Rates are entered and shown in KiB/s, as in the TUI, and sent in bytes per second as the API takes them. Routes: `PUT /api/v1/session/limits`, `GET` and `PUT /api/v1/session/queue`. The current rate limits are read from `GET /api/v1/session` (`download_limit`, `upload_limit`), as the TUI reads them from its snapshot. The queue settings are read, changed and written back as a whole object, so fields this screen does not show are kept. Saving applies them at once. **[builder's call: units, sources and save behaviour]**
2. **Devices:** the device logins (Ed25519 keys that sign in without a password), as on the TUI's `K` screen. **[owner, parity]**
   - List: name and key fingerprint.
   - Add: a name and a pasted `ssh-ed25519` public key line. The fingerprint is the server's: it is shown from the server's answer once the device is saved, and in the list. The browser does not compute it, because that would need key parsing in the page and Web Crypto, which a phone on plain HTTP does not have.
   - Remove: with a confirmation.
   - A change is saved on the server in its `config.yaml`, as the TUI's devices screen does, and the section says so in one line.
   - When the server runs with logins off, the section says so instead of showing an empty list. The plan finds the status and error code the devices route answers in that case and keys the line on it.
   Routes: `GET`/`POST /api/v1/auth/devices`, `DELETE /api/v1/auth/devices/{name}`. **[builder's call: fingerprint from the server, the saved-on-server line, the logins-off line]**
3. **Keyboard shortcuts:** a read-only list of every key the web UI understands, grouped by where it works (the player; the torrent list; dialogs). On a phone the section is still shown, with a note that it is for a keyboard. **[owner: see them; read-only in the web UI]** Changing shortcuts belongs to the desktop app, milestone 3. **[owner, 2026-11-23: "we can make it changable in the desktop app"]**

The list of keys is defined in one place in the code and the screen renders that list, so the screen cannot drift from what the keys do. Today the keys live inside each component's handler (the player, the torrent list, the detail tabs); moving them into one shared list that the handlers read is part of this work, not extra scope. **[builder's call]**

### 2.4 Phone layout of the torrent list: cards
Below a breakpoint of 640 px (the one the player uses), the torrent list is shown as cards, one per torrent. **[owner, 2026-11-23: chose "A. Cards" from three mockups]**
- Each card: the name (one line, cut with an ellipsis), a progress bar in the state's colour, the state and its percentage, and the one speed that matters (download speed while downloading, upload speed while seeding, peer count when stalled).
- The state filters become a row of chips above the cards; the text filter and sort stay available. **[builder's call]**
- Above 640 px the desktop table is unchanged.
- Colours follow the Graphite tokens already used.

### 2.5 The torrent on a phone: full-screen detail
Tapping a card opens that torrent full screen: its actions (pause or resume, remove, the section 2.2 actions) at the top, then the tabs (Files, Peers, Trackers, Pieces). Back returns to the list. Opening the view pushes a browser history entry, so the browser's back button and Android's back gesture return to the list; no routing library is added for this. **[builder's call; the mockup showed long-press for actions, dropped because long-press is hard to discover]**

## 3. Non-goals

- **The local TUI's "serve the web UI" action** (section 2a of the 2026-11-21 spec). The owner, 2026-11-23: "leave the serve web ui feature for now". **[owner]** It leaves milestone 2b's list; it is not dropped from the product.
- **Changing keyboard shortcuts** in the web UI (desktop app, milestone 3). **[owner]**
- **Anything a browser cannot do safely or at all:** reading key files or ssh-agent for device-key sign-in, launching mpv or VLC, switching between servers (the desktop app's job, milestone 3), the TUI's terminal-only keys and its minimal view. **[builder's call]**
- **Server API changes.** 2b is front-end work over routes that exist. If one is missing, the feature is dropped from 2b and recorded, not built on the server. **[builder's call]**
- **Search across torrent sites** stays parked (2026-11-21 spec, section 2b). **[owner, 2026-11-21]**

## 4. Constraints

- Everything in `CLAUDE.md` holds: product copy without emoji or em-dashes; remote text through `displaySafe`; Graphite tokens only.
- The bundle stays inside the 300 KB gzipped budget; no new dependency without a reason in the plan. **[builder's call]**
- Every change the browser makes uses the existing session cookie and CSRF token. Device add and remove are security-sensitive: they get the same review tier as the server's auth code. **[builder's call]**
- Each feature ends with a real-browser check in Chrome at 1280 and 390 px, as milestone 2 did, because that check found the bugs the unit tests' fakes hid. **[builder's call]**

## 5. Left to the builder

- Exact control styles (select, segmented control or stepper) for priorities and settings, within the design system.
- Error copy for refused changes (a 409 or 422 from the server), following the existing banner pattern.
- Whether the five server settings save field by field or with one Save button.
- The order of tasks in the plan.

## 6. Change to the 2026-11-21 spec

Its section 7, milestone 2b, becomes: file priorities; the per-torrent actions of section 2.2 above; the Settings screen (server settings, devices, keyboard shortcuts); the phone card layout. The "local TUI serves the web UI" action moves out of 2b, to a later milestone the owner has not set.
