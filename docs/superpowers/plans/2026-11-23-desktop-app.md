# Desktop App (Milestone 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A separate Tauri 2 executable, `featherbittorrent-desktop`, that shows the existing web UI in a native window with its own title bar, runs a private engine ("This computer"), switches between saved instances, lives in the tray, starts at login, opens `magnet:` links and `.torrent` files, lets the user change keyboard shortcuts, and ships as `.dmg`, `.msi`, `.deb`, `.rpm` and `.tar.gz`.

**Architecture:** The page stays `web/dist`; it learns to talk to an API at any base address with a bearer token (one client code path for browser and desktop). `fbtt-server` gains `server.cors_origins` and a desktop access mode: loopback only, no static routes, no logins of its own, one per-launch secret. A pure-Rust crate `fbtt-desktop-core` holds everything testable without a webview (paths, settings file, instances, launch secret, argument handoff); the Tauri crate `featherbittorrent-desktop` is the thin shell (window, tray, plugins, commands) over it. Desktop-only React code lives in `web/src/desktop/` and is loaded with a dynamic `import()` only inside the shell.

**Tech Stack:** Rust 2024 (1.99), Tauri 2 with `tauri-plugin-single-instance`, `tauri-plugin-deep-link`, `tauri-plugin-autostart`, `keyring` 3; React/Vite/Tailwind 4 and vitest in `web/`; the Tauri CLI bundler for `.dmg`, `.msi`, `.deb`, `.rpm`.

**Spec:** `docs/superpowers/specs/2026-11-23-desktop-app-design.md` (approved 2026-11-23), building on `docs/superpowers/specs/2026-11-21-web-ui-and-desktop-design.md`.

## Global Constraints

- Everything in the repo's `CLAUDE.md` holds: edition 2024, `rust-version = "1.99"`, `#![forbid(unsafe_code)]` in every crate including both new ones; dependencies only in `[workspace.dependencies]`; tests never touch the public internet (test `Settings` with DHT, LSD, UPnP, NAT-PMP off and `listen_addrs` on `127.0.0.1:0`); remote text through `fbt::display_safe`; no emoji and no em-dashes in product copy; no source file headers; Conventional Commits with the crate as scope; no attribution trailers or session links anywhere.
- Tauri **2**. Every Tauri crate and plugin is pinned to an exact version in the workspace table, the newest 2.x on crates.io when Task 5 starts; `tauri-cli` is installed in CI and release at that same exact version (`cargo install --locked tauri-cli@<x.y.z>`).
- If a Tauri macro (`generate_context!`, `generate_handler!`, `#[tauri::command]`) expands to code that `forbid(unsafe_code)` refuses, the implementer stops with `BLOCKED` and says which macro; no lint is loosened.
- "One client code path": torrents, files, settings, devices and streaming always go through `web/src/api/client.ts`. Tauri commands exist only for: the instance list, the keychain, connecting, window and title bar, tray, start at login, desktop settings, shortcut overrides, handed-over links and files, and the This computer serving switch.
- Nothing listens beyond `127.0.0.1` unless the user turns on "Serve the web UI" (spec 2.3).
- No secret in a plain file or in browser storage: bearer tokens live in JS memory; saved passwords only in the OS keychain; the launch secret only in process memory.
- Desktop webview origins, verbatim: `tauri://localhost` (macOS, Linux) and `http://tauri.localhost` (Windows).
- Desktop library directories: `ProjectDirs::from("io", "featherbit", "featherbittorrent-desktop")`; overrides `FBTT_DESKTOP_CONFIG_DIR` and `FBTT_DESKTOP_DATA_DIR` (never the CLI's `FBTT_CONFIG_DIR`/`FBTT_DATA_DIR`).
- Product copy fixed by this plan (exact strings): `This computer`; `Close completely`; `Keep running in the tray`; `What should closing the window do?`; `You can change this later in Settings.`; `Start at login`; `Use the system title bar`; `Serve the web UI`; `Save password`; `This system has no keychain, so passwords are not saved.`; `Add this to the server's cors_origins: ` followed by the origin.
- The bundle budget stays 300 KB gzipped JS+CSS (`npm --prefix web run size`), desktop chunk included.
- Linux desktop artifacts: built on `ubuntu:22.04`, smoke tested on `ubuntu:22.04`, `debian:12` and `fedora:41`; the CLI's `almalinux:9` build and allow list are unchanged.

## Review Focus

1. **The default instance is down at startup.** The app must still open, show "Cannot reach the server." with the instance switcher usable, never a blank or stuck window. Test: Task 7 `default_instance_unreachable_still_shows_switcher`.
2. **Another local program hits This computer's port.** Without the launch secret every route, including `/api/v1/events` and media streams, answers `401`, and a static path answers `404`. Test: Task 1 `desktop_mode_refuses_requests_without_the_launch_secret`.
3. **A saved instance is removed or its default flag points nowhere** (removed while connected, settings file edited by hand, upgrade from a newer file). The app falls back to This computer and rewrites nothing it cannot read. Test: Task 2 `missing_default_falls_back_to_this_computer` and `unknown_fields_are_kept_on_save`.
4. **A handed-over link arrives while the target needs a sign-in, or the sign-in fails.** The link stays on the Add page until added or dismissed; it is never dropped silently. Test: Task 9 `pending_link_survives_failed_sign_in`.
5. **Shortcut overrides that no longer fit** (an action renamed in an upgrade, an empty key, a key used twice in one group from a hand-edited file). Unknown actions are ignored, invalid entries dropped, defaults used. Test: Task 4 `stale_and_invalid_overrides_fall_back_to_defaults`.

---

### Task 1: Server: `cors_origins` and the desktop access mode

**Files:**
- Modify: `crates/fbtt-core/src/config.rs` (`ServerConfig`)
- Modify: `crates/fbtt-server/src/server.rs`, `crates/fbtt-server/src/auth/mod.rs`, `crates/fbtt-server/src/lib.rs`
- Test: `crates/fbtt-server/tests/desktop_access.rs`, `crates/fbtt-server/tests/cors.rs`
- Modify: `docs/server.md` (the new key)

**Interfaces:**
- Produces: `ServerConfig.cors_origins: Vec<String>` (YAML key `server.cors_origins`, default empty, `#[serde(default)]`), passed to `ApiConfig.cors_origins`.
- Produces: `pub struct DesktopAccess { pub launch_secret: String, pub origins: Vec<String>, pub expose: bool }` and `Server::start_desktop(settings: fbt::Settings, config: Config, paths: &Paths, lock: LibraryLock, access: DesktopAccess) -> Result<Server, ServerError>`, re-exported from `fbtt_server`.
- Behaviour of `start_desktop`: `cors_origins` = `access.origins` plus the config's own; with `expose == false` the bind is `127.0.0.1:0` whatever the config says, no static web routes, the login routes answer `404`, and only the launch secret is accepted (as `Authorization: Bearer` or as the query token on the routes that already take one). With `expose == true` the config's `server` section applies exactly as `Server::start` (bind, logins, `allowed_hosts`, web UI if the `web-ui` feature is on) and the launch secret is accepted in addition.
- The secret is compared in constant time with `subtle`'s `ct_eq`, as `auth/mod.rs` already does for the CSRF value.

- [ ] **Step 1: Write the failing tests** in `tests/desktop_access.rs`, each on an offline test `Settings`:
  - `desktop_mode_binds_loopback_only`: config asks `0.0.0.0:9999`, `expose: false` → `local_addr().ip()` is `127.0.0.1` and the port is not 9999.
  - `desktop_mode_refuses_requests_without_the_launch_secret`: `GET /api/v1/torrents`, `GET /api/v1/events`, `GET /api/v1/media/x/0/tracks/0/stream` without a credential → `401`; with a wrong bearer → `401`; `GET /` and `GET /assets/x.js` → `404`.
  - `desktop_mode_accepts_the_secret_as_bearer_and_query_token`: bearer → `200` on `/api/v1/torrents`; `?token=<secret>` on `/api/v1/events` → `200`.
  - `desktop_mode_has_no_login_routes`: `POST /api/v1/auth/session` → `404`.
  - `desktop_mode_answers_the_desktop_origin_preflight`: `OPTIONS /api/v1/torrents` with `Origin: tauri://localhost` and `Access-Control-Request-Method: GET` → `access-control-allow-origin: tauri://localhost`; with `Origin: http://evil.example` → no allow-origin header.
  - `exposed_desktop_mode_requires_logins_and_keeps_the_secret`: config with one user, `expose: true` → bind from config, password login works, the secret still works.
- [ ] **Step 2: Write the failing test** `tests/cors.rs::configured_cors_origin_can_sign_in_with_a_password`: `server.cors_origins: [tauri://localhost]`, a user; preflight then `POST /api/v1/auth/session` with Basic credentials and `Origin: tauri://localhost` → `200` with a token, and a bearer `GET /api/v1/torrents` with that origin → `200`.
- [ ] **Step 3: Run** `cargo test -p fbtt-server --all-features --test desktop_access --test cors` → FAIL (no `start_desktop`, no field).
- [ ] **Step 4: Implement** the field, `DesktopAccess`, `Server::start_desktop` (sharing `build`), and the launch-secret check in `FbttAuth` (a field `launch_secret: Option<String>` consulted before the session store; it never expires and is never issued by a route).
- [ ] **Step 5: Run** the two test files and then `cargo test --workspace --all-features` → PASS. Add `cors_origins` to `docs/server.md` with the two desktop origins.
- [ ] **Step 6: Commit** `feat(server): cors_origins and the desktop access mode`.

### Task 2: `fbtt-desktop-core`: paths, settings, instances, secret, handoff

**Files:**
- Create: `crates/fbtt-desktop-core/Cargo.toml`, `src/lib.rs`, `src/paths.rs`, `src/settings.rs`, `src/instances.rs`, `src/secret.rs`, `src/handoff.rs`
- Modify: root `Cargo.toml` (table entry `fbtt-desktop-core = { path = "crates/fbtt-desktop-core" }`)

**Interfaces:**
- Produces: `DesktopPaths::resolve() -> Result<DesktopPaths, ConfigError>` and `resolve_with(look_up)` (same contract as `fbtt_core::Paths`, keys and project in Global Constraints); `fn library(&self) -> fbtt_core::Paths` (the This computer library: `config_dir`/`data_dir` of the desktop dirs); `fn settings_file(&self) -> PathBuf` = `<config_dir>/desktop.yaml`.
- Produces in `settings.rs`:
  ```rust
  pub enum CloseAction { Ask, Quit, Tray }                // YAML: ask | quit | tray, default ask
  pub struct ServeSettings { pub enabled: bool }          // bind, users etc. live in the library's config.yaml `server:` section
  pub struct DesktopSettings {
      pub close: CloseAction,
      pub start_at_login: bool,                           // default false
      pub system_title_bar: bool,                         // default false
      pub default_instance: String,                       // default "local"
      pub instances: Vec<Instance>,                       // remote ones only; This computer is implicit
      pub shortcuts: Vec<ShortcutOverride>,               // { group, action, keys: Vec<String> }
      pub serve: ServeSettings,
      // plus a flattened map of unknown keys, kept on save
  }
  impl DesktopSettings { pub fn load(path: &Path) -> Result<DesktopSettings, SettingsError>; pub fn save(&self, path: &Path) -> Result<(), SettingsError> }
  ```
  A missing file is `Default`; a file that does not parse is an error the shell shows, and the file is not overwritten. Save writes to a temp file and renames.
- Produces in `instances.rs`: `pub struct Instance { pub id: String, pub name: String, pub url: String, pub save_password: bool, pub user: Option<String> }`; `pub const LOCAL_ID: &str = "local"`; `fn add(&mut self, name, url) -> Result<Instance, InstanceError>` (URL must be `http` or `https` with a host, no path beyond `/`, no query, no credentials; ids are random URL-safe strings; names trimmed, non-empty, at most 64 chars, unique case-insensitively, never `This computer`); `fn remove(&mut self, id)` (also resets `default_instance` to `local` when it pointed there); `fn set_default(&mut self, id) -> Result<(), InstanceError>`; `fn default_or_local(&self) -> &str`.
- Produces in `secret.rs`: `pub fn launch_secret() -> String` (32 random bytes, base64url no padding, from the OS RNG already in the tree).
- Produces in `handoff.rs`: `pub enum Opened { Magnet(String), TorrentFile(PathBuf) }` and `pub fn opened_from_args(args: &[OsString]) -> Vec<Opened>` (skips argv[0] and flags; accepts `magnet:` URIs case-insensitively and existing paths ending `.torrent` case-insensitively, including `file://` URLs; at most 16 items; anything else ignored).

- [ ] **Step 1: Write failing unit tests** in each module: `env_overrides_win_and_empty_counts_as_unset`, `desktop_dirs_differ_from_the_cli_dirs`, `missing_file_is_default_settings`, `unparsable_file_is_an_error_and_is_not_overwritten`, `unknown_fields_are_kept_on_save`, `close_action_round_trips_as_ask_quit_tray`, `add_refuses_bad_urls` (table: `ftp://x`, `http://`, `http://u:p@h`, `http://h/path`, `http://h/?q`, `""`), `add_refuses_duplicate_and_reserved_names`, `missing_default_falls_back_to_this_computer` (default id not in the list → `default_or_local()` is `local`), `remove_resets_the_default`, `secret_is_43_chars_and_differs_each_call`, `args_pick_magnets_and_torrent_files` (mixed case, `file://` URL, a non-existent `.torrent` ignored, 20 magnets capped at 16).
- [ ] **Step 2: Run** `cargo test -p fbtt-desktop-core` → FAIL.
- [ ] **Step 3: Implement** the modules (serde with `serde_norway`, as the CLI config does).
- [ ] **Step 4: Run** `cargo test -p fbtt-desktop-core` and `cargo clippy -p fbtt-desktop-core --all-targets -- -D warnings` → PASS.
- [ ] **Step 5: Commit** `feat(desktop-core): paths, settings, instances, launch secret and handoff`.

### Task 3: Web: one client for any address and any login

**Files:**
- Create: `web/src/connection.ts`, `web/src/connection.test.ts`
- Modify: `web/src/api/client.ts`, `web/src/session.ts`, `web/src/App.tsx`, `web/src/screens/Torrents.tsx` (events URL), `web/src/player/Player.tsx` and `web/src/player/engine.ts` (stream URLs and fetch credentials), the components that default `client = api`
- Test: `web/src/api/client.test.ts`, `web/src/session.test.ts`, `web/src/App.test.tsx`

**Interfaces:**
- Produces in `connection.ts`:
  ```ts
  export type Endpoint =
    | { kind: 'sameOrigin' }                                       // the browser: cookie + CSRF, today
    | { kind: 'bearer'; base: string; token: string | null }       // desktop: base like 'http://127.0.0.1:53211', token or null when a sign-in is needed
  export interface Connection {
    endpoint: Endpoint
    api: Api
    session: SessionStore
    url(path: string): string                 // base + path for bearer, path unchanged for sameOrigin
    eventsUrl(query: string): string          // adds `token=` for bearer
  }
  export function createConnection(endpoint: Endpoint, fetchFn?: typeof fetch): Connection
  export const ConnectionContext: React.Context<Connection>
  export function useConnection(): Connection
  ```
- `createClient` gains an `Endpoint` parameter: for `bearer` it prefixes `base`, sends `Authorization: Bearer <token>` and `credentials: 'omit'`, never sends `x-csrf-token`; `sameOrigin` behaves exactly as today. `mediaStreamUrl` and the stream-token path come back absolute through `url()` for bearer.
- `createSessionStore(fetchFn, endpoint)`: for `bearer` with a token the state is `signedIn` (subject `desktop`, no sign-out button) ; with `token: null` it is `signedOut` and `signIn(user, password)` posts `POST <base>/api/v1/auth/session` with `Authorization: Basic` and keeps the answer's `token` in memory only; a `401` later goes back to `signedOut` with `SESSION_ENDED`. It never writes `localStorage`, `sessionStorage` or cookies.
- The module-level `api` and `session` stay as the `sameOrigin` connection so the browser build changes nothing; `App` renders inside `ConnectionContext.Provider` and screens read `useConnection()` where they read the singletons today.

- [ ] **Step 1: Write failing tests:** `bearer_client_prefixes_base_and_sends_the_token`, `bearer_client_omits_credentials_and_csrf`, `same_origin_client_is_unchanged` (the existing client tests keep passing untouched), `bearer_events_url_carries_the_token`, `bearer_stream_paths_are_absolute`, `bearer_sign_in_uses_basic_and_keeps_the_token_in_memory` (assert storage APIs untouched via spies), `bearer_401_returns_to_sign_in_with_session_ended`, `app_reads_the_connection_from_context` (App under a bearer provider calls the fake base).
- [ ] **Step 2: Run** `npm --prefix web test` → the new tests FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `npm --prefix web test`, `npm --prefix web run lint`, `npm --prefix web run typecheck`, `npm --prefix web run build`, `npm --prefix web run size` → PASS.
- [ ] **Step 5: Commit** `feat(web): a connection for any address and bearer logins`.

### Task 4: Web: shortcut overrides

**Files:**
- Modify: `web/src/keys.ts`, `web/src/keys.test.ts`, `web/src/components/settings/Shortcuts.tsx`, `Shortcuts.test.tsx`

**Interfaces:**
- Produces in `keys.ts`: `export interface ShortcutOverride { where: KeyBinding['where']; action: string; keys: string[] }`; `export function setOverrides(overrides: ShortcutOverride[]): void` (applies them; `matches` and `effectiveBindings()` follow at once); `export function effectiveBindings(): readonly KeyBinding[]`; `export function conflictFor(where, action, key): string | null` (the action in the same group already holding `key`, else null); `KEY_BINDINGS` stays the defaults.
- `Shortcuts` gains `editable?: boolean` and `onChange?: (overrides: ShortcutOverride[]) => void`. Editable rows show `Change` (captures the next key press; Escape cancels the capture), a refusal line `Already used by <action>` on conflict, and one `Reset to defaults` button. Not editable: today's read-only list, unchanged.

- [ ] **Step 1: Write failing tests:** `override_changes_what_matches_answers`, `effective_bindings_show_the_override`, `conflict_names_the_action_in_the_same_group`, `same_key_in_another_group_is_allowed`, `stale_and_invalid_overrides_fall_back_to_defaults` (unknown action, empty keys, duplicate key within a group → ignored), `read_only_list_has_no_change_buttons`, `editable_list_captures_a_key_and_reports_overrides`, `reset_restores_defaults_and_reports_empty`.
- [ ] **Step 2: Run** `npm --prefix web test` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the web gate as in Task 3 Step 4 → PASS.
- [ ] **Step 5: Commit** `feat(web): changeable shortcut bindings`.

### Task 5: The Tauri shell and This computer

**Files:**
- Create: `crates/featherbittorrent-desktop/` with `Cargo.toml` (`[package] name = "featherbittorrent-desktop"`, binary of the same name, `build = "build.rs"`), `build.rs` (`tauri_build::build()`), `tauri.conf.json`, `capabilities/default.json`, `icons/` (generated from the existing mark with `cargo tauri icon` from the repo's logo source), `src/main.rs`, `src/local.rs`, `src/commands.rs`
- Create: `web/src/desktop/bridge.ts`, `web/src/desktop/index.ts`, `web/src/desktop/bridge.test.ts`
- Modify: `web/src/main.tsx` (desktop detection and lazy import), `web/package.json` (`@tauri-apps/api` at an exact 2.x version)
- Modify: root `Cargo.toml` (tauri, tauri-build, plugins, keyring entries), `deny.toml` (any new license with a one-line reason), `.github/workflows/ci.yml` (Linux installs `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libayatana-appindicator3-dev librsvg2-dev`; the workspace gate stays `--workspace --all-targets --all-features`)

**Interfaces:**
- `tauri.conf.json`: `productName` `featherbittorrent`, `identifier` `io.featherbit.featherbittorrent.desktop`, `version` read from the workspace (the build fails if they differ), `build.frontendDist` `../../web/dist`, one window `main`, min size 900x600, `app.security.csp` with `default-src 'self'; connect-src 'self' ipc: http://ipc.localhost http: https:; media-src 'self' http: https: blob:; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'`.
- `local.rs`: `pub struct ThisComputer { server: fbtt_server::Server, secret: String }`; `pub async fn start(paths: &DesktopPaths, serve: bool) -> Result<ThisComputer, String>` (acquires the library lock, loads or writes the default config, calls `Server::start_desktop` with `launch_secret()`, the desktop origin for this OS, `expose = serve`); `pub fn endpoint(&self) -> (String /*base*/, String /*secret*/)`; `pub async fn stop(self)`. A lock refusal shows a native message `featherbittorrent is already running.` and exits 3, matching the TUI.
- `commands.rs` (first set; later tasks add theirs): `#[tauri::command] fn desktop_info() -> DesktopInfo { version, platform: "macos" | "windows" | "linux" }`; `#[tauri::command] async fn connect(id: String) -> Result<EndpointDto, String>` returning `{ kind: "bearer", base, token }` (`token` is the secret for `local`; Task 7 fills remote ids).
- `web/src/desktop/bridge.ts`: `export function inDesktop(): boolean` (true when `window.__TAURI_INTERNALS__` exists); `export async function connect(id: string): Promise<Endpoint>`; `export async function desktopInfo(): Promise<{ version: string; platform: 'macos' | 'windows' | 'linux' }>`. Only `web/src/desktop/*` imports `@tauri-apps/api`; `main.tsx` reaches it only through `import('./desktop')`, so the browser's first load does not fetch it.
- Startup: the shell starts This computer, opens the window; the page asks `connect(defaultId)` (default from Task 7; `local` until then) and renders `App` inside the resulting `Connection`.
- The secret is delivered by the `connect` command rather than an initialization script (spec 2.3 left the mechanism to the builder): one path for every instance, and the secret never sits in a page global.

- [ ] **Step 1: Write failing tests:** Rust `local::tests::start_serves_loopback_with_a_fresh_secret` (start on a temp `DesktopPaths` with offline settings, bearer request to `endpoint()` answers `200`, no secret → `401`, a second `start` on the same paths is refused by the lock); `commands::tests::connect_local_returns_the_secret`; web `bridge.test.ts`: `in_desktop_is_false_in_a_browser`, `connect_maps_the_command_answer_to_a_bearer_endpoint` (mock `@tauri-apps/api/core` `invoke`).
- [ ] **Step 2: Run** `cargo test -p featherbittorrent-desktop` and `npm --prefix web test` → FAIL.
- [ ] **Step 3: Implement.** Keep `main.rs` to wiring; everything with logic sits in `local.rs`, `commands.rs` or `fbtt-desktop-core`.
- [ ] **Step 4: Run** the full gate (`cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `cargo deny check`, the web gate) → PASS. Then `npm --prefix web run build && cargo run -p featherbittorrent-desktop`: the window opens on This computer's empty torrent list; adding a magnet works.
- [ ] **Step 5: Commit** `feat(desktop): the Tauri shell running This computer`.

### Task 6: The title bar and window chrome

**Files:**
- Create: `web/src/desktop/TitleBar.tsx`, `TitleBar.test.tsx`; `crates/featherbittorrent-desktop/src/window.rs`
- Modify: `crates/featherbittorrent-desktop/src/main.rs`, `tauri.conf.json`, `capabilities/default.json`, `web/src/components/Header.tsx` (renders inside the title bar in the desktop)

**Interfaces:**
- `window.rs`: `pub fn apply_chrome(window: &WebviewWindow, platform: Platform, system_title_bar: bool)`. macOS: `TitleBarStyle::Overlay`, hidden title, native traffic lights kept. Windows and Linux: `decorations(false)` unless `system_title_bar` is true (then the system's decorations and no drawn controls).
- `TitleBar` props: `{ platform: 'macos' | 'windows' | 'linux'; systemTitleBar: boolean; instanceSwitcher: ReactNode; actions: ReactNode }`. Layout: on macOS a 78px left inset for the traffic lights; elsewhere drawn Minimize, Maximize/Restore and Close buttons (Lucide icons) on the right that call the window API. Empty bar space carries `data-tauri-drag-region` (drag, and double-click maximize or zoom). With `systemTitleBar` it renders only the switcher and actions as a toolbar.
- Setting `system_title_bar` is in Settings (Task 10 places the switch; this task adds the command `set_system_title_bar(on: bool)` which saves and reapplies).

- [ ] **Step 1: Write failing tests:** `mac_bar_leaves_room_for_the_traffic_lights_and_draws_no_controls`, `windows_and_linux_bars_draw_three_controls`, `system_title_bar_draws_no_controls`, `empty_space_is_a_drag_region` (attribute present on the spacer, absent on buttons), `control_buttons_call_the_window_api` (mocked `getCurrentWindow`).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** the full gate → PASS; manual check on macOS: drag, double-click zoom, traffic lights aligned, resize from edges.
- [ ] **Step 5: Commit** `feat(desktop): own title bar and window chrome`.

### Task 7: Instances: list, switch, default, sign-in, keychain

**Files:**
- Create: `crates/featherbittorrent-desktop/src/keychain.rs`, `src/instances.rs`; `web/src/desktop/Instances.tsx`, `InstanceSwitcher.tsx`, tests beside them
- Modify: `commands.rs`, `web/src/desktop/bridge.ts`, `web/src/screens/SignIn.tsx` (a `Save password` checkbox when told the shell can save)

**Interfaces:**
- `keychain.rs`: `pub trait Keychain { fn get(&self, id: &str) -> Option<String>; fn set(&self, id: &str, password: &str) -> Result<(), String>; fn delete(&self, id: &str); fn available(&self) -> bool }`, `SystemKeychain` over `keyring` (service `io.featherbit.featherbittorrent.desktop`, account = instance id), `MemoryKeychain` for tests. `available()` probes once at startup; on Linux without a Secret Service it is false.
- Commands: `list_instances() -> InstancesDto { default: String, items: [{ id, name, url, saved_password: bool }], keychain: bool }` (This computer first, id `local`, name `This computer`); `add_instance(name, url)`; `remove_instance(id)` (also deletes its keychain entry); `set_default_instance(id)`; `connect(id)` for remote ids returns `{ kind: "bearer", base: url, token: null }` plus, when a password is saved, signs in from the shell with `POST /api/v1/auth/session` and returns the token; `save_password(id, user, password)` / `forget_password(id)`.
- When a request from the page fails before any HTTP status (a CORS refusal looks like a network error), the page asks the shell `probe_instance(id) -> { reachable: bool, cors_ok: bool }` (the shell sends `GET <url>/api/v1/session` with no `Origin`: any HTTP status means reachable; then an `OPTIONS` preflight with this system's desktop origin: `cors_ok` is whether `access-control-allow-origin` names it) and, when reachable but not allowed, shows `Add this to the server's cors_origins: <origin>`.
- Switching re-creates the `Connection` and remounts `App` under a key of the instance id; the switcher sits in the title bar and lists every instance with a mark on the connected one and a `Manage instances` entry that opens the `Instances` section of Settings.

- [ ] **Step 1: Write failing tests:** Rust `remove_deletes_the_keychain_entry`, `connect_remote_with_saved_password_returns_a_token` (a test `fbtt-server` with one user on loopback), `connect_remote_without_password_needs_sign_in`, `no_keychain_means_no_save` ; web `switcher_lists_this_computer_first`, `switching_remounts_with_a_new_connection`, `cors_refusal_shows_the_origin_to_add`, `default_instance_unreachable_still_shows_switcher`, `save_password_hidden_without_keychain` (shows `This system has no keychain, so passwords are not saved.`).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** the full gate → PASS; manual: add a `serve --offline` instance on another port with `cors_origins: [tauri://localhost]`, sign in, switch back and forth, make it default, restart the app.
- [ ] **Step 5: Commit** `feat(desktop): instances, switching and saved passwords`.

### Task 8: Closing, the tray and start at login

**Files:**
- Create: `crates/featherbittorrent-desktop/src/tray.rs`, `src/lifecycle.rs`; `web/src/desktop/CloseChoice.tsx`, test
- Modify: `main.rs`, `commands.rs`, `Cargo.toml` (autostart plugin)

**Interfaces:**
- `lifecycle.rs`: `pub fn on_close_requested(settings: &DesktopSettings, tray_available: bool) -> CloseOutcome` with `CloseOutcome { Quit, HideToTray, AskPage }`: `Ask` + tray → `AskPage` (prevent close, emit `close-requested` to the page); `Ask` without tray → `Quit`; `Tray` without tray → `Quit`; `Quit` → `Quit`. Pure function, unit tested.
- The page's `CloseChoice` dialog (the existing modal component) shows `What should closing the window do?`, `You can change this later in Settings.`, buttons `Close completely` and `Keep running in the tray`; the choice is saved with `set_close_action("quit" | "tray")` and then applied.
- `tray.rs`: menu items `Show featherbittorrent`, a disabled line with the connected instance's name, `Quit`. Left click shows the window. Tray availability is what the tray builder reports; when it fails the app records `tray_available = false`.
- Quit: stop This computer (`ThisComputer::stop`, which runs the final save), then exit.
- Start at login: `set_start_at_login(on: bool)` uses `tauri-plugin-autostart` with the argument `--background`; a launch with `--background` creates the window hidden when the tray exists and minimized when it does not.

- [ ] **Step 1: Write failing tests:** the four `on_close_requested` cases; `background_flag_hides_or_minimizes` (pure function `initial_window_state(args, tray_available)`); web `close_choice_saves_and_applies_each_answer`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** the full gate → PASS; manual on macOS: first close asks once, tray menu works, Quit stops torrents and they resume, start at login creates and removes the LaunchAgent.
- [ ] **Step 5: Commit** `feat(desktop): closing, tray and start at login`.

### Task 9: `magnet:` links and `.torrent` files

**Files:**
- Create: `crates/featherbittorrent-desktop/src/opened.rs`; `web/src/desktop/OpenedQueue.ts`, test
- Modify: `main.rs` (single-instance plugin registered first, deep-link plugin), `tauri.conf.json` (`plugins.deep-link.desktop.schemes: ["magnet"]`, `bundle.fileAssociations` for `torrent` with MIME `application/x-bittorrent`), `web/src/components/AddTorrent.tsx` (optional `instancePicker` slot and an initial magnet or file)

**Interfaces:**
- `opened.rs`: collects `Opened` from the first launch's args (`opened_from_args`), from the single-instance callback (the second launch's args, then shows and focuses the window), and from deep-link and macOS open-file events; emits `opened` to the page with `{ kind: "magnet", uri } | { kind: "file", name, bytes_b64 }` (the shell reads a `.torrent` of at most 10 MiB; larger is refused with a visible error). Items arriving before the page is ready are queued in the shell and fetched with `take_opened()` on page start.
- `OpenedQueue.ts`: a FIFO the page drains one item at a time into the Add page. The Add page shows the instance picker (preselected: the connected instance) beside the folder picker; changing it builds that instance's `Connection`, reloads the folder picker, and shows the sign-in form in place when that instance needs one. The item leaves the queue only on a successful add or an explicit `Cancel`.

- [ ] **Step 1: Write failing tests:** Rust `second_launch_args_are_forwarded_not_started` (pure handler over a fake emitter), `oversized_torrent_is_refused`; web `opened_magnet_opens_add_with_the_connected_instance`, `changing_instance_reloads_folders`, `pending_link_survives_failed_sign_in`, `cancel_drops_only_the_current_item`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** the full gate → PASS; manual on macOS with a built `.app` (associations need a bundle): open a magnet from a browser with the app closed and with it open; double-click a `.torrent`.
- [ ] **Step 5: Commit** `feat(desktop): open magnet links and torrent files`.

### Task 10: Desktop settings: serving the web UI, shortcuts, chrome

**Files:**
- Create: `web/src/desktop/DesktopSettings.tsx`, test
- Modify: `web/src/screens/Settings.tsx` (a `desktopSections` slot rendered only in the desktop), `commands.rs`, `local.rs`, `crates/featherbittorrent-desktop/Cargo.toml` (`fbtt-server` with the `web-ui` feature)

**Interfaces:**
- Settings in the desktop gains a `Desktop` section: `Close the window` (`Close completely` / `Keep running in the tray`, the second hidden without a tray), `Start at login`, `Use the system title bar` (Windows and Linux only), `Serve the web UI`, and the editable shortcut list (Task 4's `Shortcuts editable`), plus Task 7's `Instances`.
- Shortcut overrides: `get_shortcuts() -> ShortcutOverride[]`, `set_shortcuts(list)`; the page calls `setOverrides` at start and on every change.
- Serving: `set_serving({ enabled, bind, user, password })`. Turning it on requires a user name and a password (the password is hashed with the server's own hash and written to the desktop library's `config.yaml` `server.auth.users`); `bind` defaults to `0.0.0.0:8090`. Applying restarts This computer through `ThisComputer::stop` then `start(paths, true)`; the page reconnects to `local`. The section shows the address the web UI is reachable at, and `Off` by default.

- [ ] **Step 1: Write failing tests:** Rust `serving_requires_a_password`, `serving_restart_keeps_torrents` (add a torrent, toggle, it is still listed), `serving_on_answers_the_web_ui_on_the_chosen_bind` (offline test settings, loopback bind); web `desktop_section_only_in_desktop`, `tray_choice_hidden_without_tray`, `system_title_bar_switch_only_on_windows_and_linux`, `shortcut_changes_are_saved_and_applied`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** the full gate → PASS; manual: turn serving on, open the web UI from a phone on the LAN, sign in, turn it off and confirm the port is closed.
- [ ] **Step 5: Commit** `feat(desktop): desktop settings, serving the web UI and shortcut editing`.

### Task 11: Packages and the release workflow

**Files:**
- Create: `dev/package-desktop-linux.sh`, `dev/smoke-desktop-linux.sh`, `dev/package-desktop-macos.sh`, `dev/package-desktop-windows.ps1`, `dev/smoke-desktop-windows.ps1`, `dev/fixtures/desktop-needed-*.txt`
- Modify: `dev/check-linux-binary.sh` (a `--desktop` allow list: the CLI list plus the webview, GTK, GLib, libsoup, cairo, pango, gdk-pixbuf, appindicator and their direct `NEEDED` libraries as the built binary reports them; glibc floor 2.35), `dev/test-packaging.sh`, `.github/workflows/release.yml`, `crates/featherbittorrent-desktop/tauri.conf.json` (bundle targets, `.deb` and `.rpm` `depends`)

**Interfaces:**
- Artifacts, each with a `.sha256` beside it: `featherbittorrent-desktop-<v>-macos-universal.dmg`, `featherbittorrent-desktop-<v>-windows-x86_64.msi`, `featherbittorrent-desktop-<v>-linux-<arch>.deb`, `.rpm`, `.tar.gz` for `x86_64` and `aarch64`. Built with `cargo tauri build --bundles dmg|msi|deb|rpm` (macOS universal via `--target universal-apple-darwin`); the `.tar.gz` holds the binary, the icon, a `.desktop` file and `README-desktop.txt`, made by `package-desktop-linux.sh`.
- Package rules: no maintainer scripts, nothing that starts at boot or login, removal keeps `~/.config/featherbittorrent-desktop` and the library; the package installs beside `featherbittorrent` or `featherbittorrent-headless` with no `Conflicts`, `Replaces` or `Obsoletes` on them. `check_no_service` runs on the desktop packages too.
- Smoke: Linux in `ubuntu:22.04`, `debian:12` (deb), `fedora:41` (rpm), `ubuntu:22.04` (tar.gz) checks the sha256, installs, runs `featherbittorrent-desktop --version` under `xvfb-run` and expects `featherbittorrent-desktop <v>`, checks the `.desktop` file declares `x-scheme-handler/magnet` and `application/x-bittorrent`, removes, and asserts a written settings file survived. Windows: install the `.msi` quietly, `--version`, the `magnet` URL protocol key exists, uninstall. macOS: mount the `.dmg`, `--version` from the `.app`, `CFBundleURLSchemes` contains `magnet`, `CFBundleDocumentTypes` lists `torrent`.
- `--version` is handled before Tauri starts any window, so it works headless.

- [ ] **Step 1: Write failing checks:** `dev/test-packaging.sh` cases `desktop_allow_list_accepts_the_fixture`, `desktop_allow_list_refuses_an_unknown_library`, `cli_allow_list_unchanged` (the CLI fixture still refused for a GTK library).
- [ ] **Step 2: Run** `bash dev/test-packaging.sh` → FAIL. **Step 3: Implement** the scripts and release jobs (the release refuses to publish when any desktop smoke fails, like the CLI jobs). **Step 4: Run** `bash dev/test-packaging.sh`, build the macOS `.dmg` locally with `bash dev/package-desktop-macos.sh "$(bash dev/release-version.sh)"`, and the Linux artifacts in Docker as in `CLAUDE.md` but with `ubuntu:22.04`; smoke them → PASS. CI on the PR runs the Windows job.
- [ ] **Step 5: Commit** `ci: desktop packages and smoke tests`.

### Task 12: Docs and the hardware checks

**Files:**
- Create: `docs/desktop.md`
- Modify: `README.md`, `CHANGELOG.md` (`## [Unreleased]`), `CLAUDE.md` (workspace crates, commands, the desktop Linux floor, the hardware checks CI cannot cover), `docs/install.md`, `docs/web-ui.md` (the desktop as a client, `cors_origins`)

**Interfaces:**
- `docs/desktop.md` covers: install per system (unsigned: the macOS right-click Open and the Windows SmartScreen note, as the terminal's docs say), This computer and where its library lives, instances and `cors_origins` with both origins, saved passwords and the keychain, closing and the tray (GNOME needs the AppIndicator extension), start at login, links and files (and registering by hand for the `.tar.gz`: the `.desktop` file and `xdg-mime default`), serving the web UI, changing shortcuts, the Linux floor.
- `CLAUDE.md` hardware checks list: tray on GNOME with and without AppIndicator, KDE tray, Windows edge resize and snap, MSE playback in WebView2, WKWebView and WebKitGTK, magnet handoff from Firefox and Chrome on each system.

- [ ] **Step 1: Write** the docs. **Step 2: Run** the docs job's checks (`.github/workflows/ci.yml` `docs`) locally and grep the new files for em-dashes and emoji → none.
- [ ] **Step 3: Commit** `docs: the desktop app`.

---

## After the last task

A real-app check on macOS with the built `.app`: This computer add, pause, delete, stream a video; a remote `serve --offline` instance; switching; first-close dialog; tray; start at login; magnet from a browser with the app closed and open; `.torrent` double-click; shortcut change applied in the player; serving the web UI to a phone. Findings go back to the owning task's implementer.
