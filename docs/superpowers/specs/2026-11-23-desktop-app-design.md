# featherbittorrent desktop app (milestone 3)

Status: draft for the owner's read (2026-11-23). Builds on `2026-11-21-web-ui-and-desktop-design.md` (sections 2, 5 and 7, milestone 3) and changes it where noted in section 9.

Tags:
- **[owner]**: the owner said it.
- **[builder's call]**: proposed while writing this draft. Any of these can be changed in review.

## 1. Intent

The owner, 2026-11-23: "I have chosen tauri because it's a tiny wrapper on what we already have with (hopefully) some slight differences when it comes to OS integrations". **[owner]**

Day to day the app is used to check progress, pause, delete, change settings and stream, which is what the web UI already does. **[owner]** The desktop app is that same web UI in a native window. Its only differences are what a native app does better: running the engine on this computer, a tray, start at login, opening `magnet:` links and `.torrent` files, a window that draws its own title bar, switching between instances, and changeable keyboard shortcuts.

## 2. Goals

### 2.1 One more executable, the same UI
- The desktop app is its own executable, separate from the `featherbittorrent` binary. The TUI stays in `featherbittorrent` (full or headless). **[owner]**
- The window shows the same `web/dist` the full build serves, with the same API client. Desktop-only parts (title bar, instance switcher, desktop settings, shortcut editing) are React components shown only when running inside the desktop shell. **[owner: thin wrapper; builder's call: how]**
- It carries featherbittorrent's version and ships in the same release. **[owner, 2026-11-21]**

### 2.2 Instances
- **This computer** is the instance the app connects to by default: torrents downloading on this machine, in an engine the app runs itself. **[owner]**
- The user can add more instances (another featherbittorrent `serve`, for example the Pi at home, or one running on this same machine), each by its address. **[owner]**
- The user switches between instances while the app is open. **[owner]**
- The user can mark one instance as the default; the app connects to it at startup. Until changed, the default is This computer. **[owner]**
- A featherbittorrent on the same machine (TUI or `serve`) is never This computer: it is just another instance, added by address. **[owner: "if a user wants to connect to another localhost instance of featherbittorrent he can do that but that's another instance"]**
- The switcher lives in the app's own title bar. **[builder's call]**
- Remote instances sign in like the TUI's remote client, with the existing bearer-token login (a `SameSite=Strict` cookie cannot cross origins; 2026-11-21 spec, section 5). A saved password or token is kept in the operating system's keychain, never in a plain file. **[builder's call]**
- A remote instance must list the desktop app's origin in its `cors_origins` (fbt-api already has it). The app says so, with the exact value to add, when a connection is refused for that reason. **[owner, 2026-11-21: CORS; builder's call: the message]**

### 2.3 This computer
- The app runs the engine in-process, with a library of its own, in the app's own config and data directories, separate from the TUI's. **[owner: separate executable, other local copies are other instances; builder's call: directories]** The library lock still applies: two copies of the desktop app on one library is refused, as for the TUI.
- **Nothing is exposed by default.** This computer's engine opens no port to the network and does not serve the web UI or API to other machines. **[owner]**
- How the window reaches its own engine without a network port is left to the builder, with one rule from the 2026-11-21 spec: the page uses the same HTTP client as the browser, never a second client code path. The expected way is an API bound to `127.0.0.1` on a random port, with a per-launch secret only the window knows and the web UI's static routes off, so nothing else on the machine or the network can use it. **[builder's call]**
- **Optionally exposed.** A desktop setting turns on serving the web UI and API from This computer, so the laptop's torrents can be reached from a phone or another machine. When on, it follows the same rules as `serve`: logins, `allowed_hosts`, allowed folders, the bind address chosen in Settings. Off by default. **[owner: optional; builder's call: same rules as serve]**

### 2.4 Window and title bar
- No OS title bar decoration; the app draws its own, like featherbit terminal. **[owner, 2026-11-23]**
  - macOS: the title strip is transparent and hidden, the native window buttons stay and sit on the app's own top bar; empty bar space drags the window and a double-click zooms. **[builder's call, following fbterm]**
  - Windows and Linux: no OS decorations; drawn window controls in the app's bar; drag, double-click to maximize and restore, resize from the edges. On Linux a setting falls back to the system title bar, and the app falls back by itself once when the desktop refuses to move the window (fbterm's rule). **[builder's call, following fbterm]**
- The top bar holds: the window controls, the instance switcher, and the web UI's own header actions (Add torrent, Settings). **[builder's call]**

### 2.5 Closing and the tray
- A setting chooses what closing the window does: quit completely, or keep running in the tray. **[owner]**
- The first time the window is closed, an in-app dialog asks which, and saves the answer to that setting. It never asks again; the setting can be changed later. **[owner]**
- With the tray in use, the tray menu has Show and Quit, and shows the instance connected to. **[builder's call]**
- Quitting stops This computer's torrents cleanly; they resume on the next start, like the TUI. **[builder's call]**

### 2.6 Start at login
- A setting "Start at login", off by default. **[owner: user's choice; builder's call: off by default]**
- When it starts at login, the app starts quietly in the tray with no window, connected to the default instance. **[owner]**

### 2.7 `magnet:` links and `.torrent` files
- The app registers as the handler for `magnet:` links and opens `.torrent` files. **[owner, 2026-11-21]**
- Opening one shows the Add torrent page, which in the desktop app gains an **instance picker** beside the folder picker. **[owner]**
- The preselected instance is the one connected to when the app was already open. **[owner]** When the app was not running, it starts, connects to the default instance, and preselects that. **[builder's call]**
- Changing the instance reloads the folder picker with that instance's allowed folders. If the chosen instance needs a sign-in, the page asks for it in place. **[builder's call]**
- A second launch of the app (which is how most systems deliver a link or file) hands the link or file to the running copy and exits; it never starts a second engine. **[builder's call]**

### 2.8 Changeable keyboard shortcuts
- In the desktop app, Settings' Keyboard shortcuts section can change the keys. **[owner: part of this milestone]** The web UI in a browser keeps the list read-only.
- The bindings stay one list (`web/src/keys.ts`); the desktop app stores the user's overrides and applies them to that list, so every handler and the Settings list follow. A key already bound in the same group is refused with the name of the action holding it. A Reset puts back the defaults. **[builder's call]**

### 2.9 Packages
- The same three systems and formats as featherbit terminal, from the desktop app's first release: macOS `.dmg`, Windows `.msi`, Linux `.deb`, `.rpm` and `.tar.gz`. Unsigned, like the terminal. **[owner]**
- Each artifact is smoke tested before it is published, like the rest of the release (install, start, `--version` or equivalent, remove keeps the user's data). **[builder's call]**
- Linux packages follow the repository's glibc floor and dependency rules where the webview allows; the webview's system libraries (WebKitGTK) become declared dependencies. **[builder's call]**

## 3. Non-goals

- A second UI: anything the desktop app shows that the web UI also shows is the web UI's code. **[owner]**
- The TUI in the desktop app, or the desktop app sharing the TUI's library. **[owner]**
- Finding torrents from inside the app (spec 2026-11-21, section 2b): still parked. **[owner, 2026-11-21]**
- The local TUI's "serve the web UI" action: moved out of 2b, not part of this milestone. **[owner, 2026-11-23]**
- Signing and auto-update. **[builder's call, following fbterm]**

## 4. Anti-goals

- A port open to the network by default.
- A second client code path in the page (Tauri-only calls for anything the browser can do through the API).
- A secret in a plain file or in browser storage that outlives the app.
- Starting a second engine on the same library.

## 5. Constraints

- Everything in the repo's `CLAUDE.md`: no `unsafe` in our crates, dependencies only in the workspace table, no emoji or em-dashes in product copy, remote text display-safe, cross-platform builds and tests.
- Tauri 2. **[owner: Tauri; builder's call: version 2, the current major]**
- The bundle and the web gate as today; the desktop shell must not grow `web/dist` for browser users (desktop-only components load only in the desktop shell). **[builder's call]**

## 6. Known risks for the builder

- **Media playback in the system webviews.** The player uses Media Source Extensions (or ManagedMediaSource). WebView2 (Windows) and WKWebView (macOS) support them; WebKitGTK on Linux depends on its GStreamer build. If a webview cannot play, the player's existing "This browser cannot play" path with `Download` applies. Check each system's webview in the real-app check. **[builder's call]**
- **Webview differences** (WebKit versus Chromium) for anything the browser check covered only in Chrome.

## 7. Left to the builder

- The crate and directory layout (for example `crates/featherbittorrent-desktop` with the Tauri project), and how the shell and the page tell each other they are in the desktop app.
- The local transport for This computer, within section 2.3's rules.
- The exact Settings layout for the desktop-only settings (closing, start at login, serving the web UI, instances, shortcuts).
- How the release workflow builds and smoke tests the desktop artifacts.

## 8. Checks

Each feature ends with a real-app check on macOS (the owner's machine), plus whatever CI can run on Windows and Linux; the hardware checks CI cannot cover are listed in `CLAUDE.md`, as fbterm does. **[builder's call]**

## 9. Changes to the 2026-11-21 spec

- Section 5 said This computer uses "the same library lock the TUI uses" and the TUI's local-library rules: it now has a library of its own (the lock rules still apply to it). **[owner, 2026-11-23]**
- Section 5 said the desktop release jobs make `.dmg`, `.msi` and `.AppImage`/`.deb`: they now make the terminal's formats, `.dmg`, `.msi`, `.deb`, `.rpm` and `.tar.gz`. **[owner, 2026-11-23]**
- Section 5's loopback API with a per-launch token stays as the expected way for the window to reach This computer (section 2.3 here), with the web UI's static routes off.
