# Web UI milestone 1: the web UI in the full build. Implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A full build of `featherbittorrent` whose `serve` serves a browser UI. The UI shows:
- a password sign-in;
- the live torrent table with filters and sorting;
- the detail pane: Files, Peers, Trackers and Pieces;
- pause, resume and delete;
- adding a torrent by magnet link or `.torrent` upload into an allowed folder.

The headless build carries no front-end assets and changes nothing for existing clients. It does gain the browser-session routes and the `web_ui` setting. Every release ships both builds.

**Architecture:**
- **The front end.** A React app in `web/`, built with Vite. It is embedded into `fbtt-server` with `rust-embed` behind a cargo feature `web-ui`, and served on the API's own origin.
- **Sign-in.** The browser signs in through a new cookie session, with a CSRF token on every state-changing request. The existing bearer-token paths for the TUI are unchanged.
- **Live data.** The page refreshes the torrent list every second and wakes early on the SSE event stream, exactly like the remote TUI backend.

**Tech stack:**
- **Server:** Rust edition 2024, axum, and `rust-embed` (new).
- **Front end:** React 19, Vite 8, TypeScript 6, Tailwind 4, `@fontsource-variable/geist` and `geist-mono`, `lucide-react` 1, and vitest 5, at the versions in `../gateway/ui/package.json`.
- **API types:** `openapi-typescript` for the types taken from fbt-api's OpenAPI document.

**Spec:** `docs/superpowers/specs/2026-11-21-web-ui-and-desktop-design.md`. It is binding here: sections 2, 3, 5 and 7 (milestone 1). Milestone 2 (parity, the media player, the TUI's serve-on-request) and milestone 3 (the desktop app) are **not** in this plan.

## Global Constraints

Everything in `CLAUDE.md` applies, plus these:

- **Builds.**
  - Full is `featherbittorrent`, built with `--features web-ui`.
  - Headless is `featherbittorrent-headless`, built without the feature, and is today's binary.
  - Both ship in every release. **[owner]**
- **Off switch.** A full build's `serve` serves the web UI unless `server.web_ui: false` is in the config or `FBTT_WEB_UI=0` is in the environment. The environment wins over the file. A headless build accepts `server.web_ui`, ignores it, and logs one line when it is set. **[owner]**
- **Browser sessions.**
  - An HttpOnly cookie `fbtt_session`, with `SameSite=Strict` and `Path=/`.
  - It is `Secure` when the request came over https (`X-Forwarded-Proto: https`).
  - `Max-Age` is the session's lifetime.
  - Every state-changing request (any method but GET, HEAD and OPTIONS) authenticated by the cookie must carry `X-CSRF-Token` equal to that session's CSRF token. The comparison is constant-time.
  - No session token is ever readable by script. **[owner: cookie]**
- **Unchanged paths.** Bearer headers, query tokens and stream tokens keep working exactly as now for the TUI and players. A request with an `Authorization` header never falls back to the cookie.
- **Refresh model.** As the remote TUI does (`crates/fbtt-core/src/remote/mod.rs`):
  - `GET /api/v1/torrents` every 1 s;
  - an early refresh when `/api/v1/events` sends anything, coalesced to one extra refresh per refresh in flight;
  - 10 s while the page is hidden (`document.visibilityState`). This pause is the web UI's own addition: the TUI's `SLOW_REFRESH` is a different thing.

  fbt-api's events carry no rates, which is why the list is refreshed. The spec says the same (section 3).
- **Bundle budget.** The built JS plus CSS is at most 300 KB gzipped, excluding fonts. A script checks it in CI.
- **Look.** Graphite:
  - colours only from these tokens: ink `#0b0b0e` `#101013` `#151518` `#1f1f22` `#262629` `#313135` `#54545c`; text `#919199` `#b6b6bf` `#d0d0d9` `#f5f5f7`; violet UI fill `#7c49e3`, hover `#8656ee`, light `#a584ff`, text `#bea8ff`; soft `rgba(165,132,255,0.14)`; seeding `#2bbb71`, stalled `#f2a635`, connection `#59c5f5`, error `#ed4a49`;
  - Geist for text and Geist Mono for every real value (sizes, rates, paths, hashes);
  - radius 6 for controls, 8 for cards, 12 for overlays;
  - Lucide icons only.
  - The prototype canvas "featherbittorrent web UI" (Torrents, Add torrent, Sign in) is the visual reference.
- **Copy and attribution.** No emoji and no em-dashes in any UI string or doc. No attribution in commits or PRs.
- **Remote text.** It is shown as text nodes only, never through `dangerouslySetInnerHTML`. Torrent names, paths, tracker messages and peer clients are remote input.
- **Tests offline.** Tests never contact the internet, and front-end tests never need a running server: the API client is tested against a fake `fetch`.

## Review Focus

1. **Expired session mid-use.** A `401` from any call returns the page to Sign in with "Your session ended. Sign in again.", and never loops on retries. The test is in Task 5.
2. **Server unreachable** (laptop asleep, server restarted). The table keeps its last rows, the header shows the connection as lost, and refreshing resumes on its own with a pause that doubles: 1, 2, 4, 8, 16, then 30 s. The test is in Task 6.
3. **A torrent removed elsewhere while it is selected.** The detail pane closes and does not keep asking for a torrent that answers `404`. The test is in Task 7.
4. **Large tables.** 2,000 torrents render only their visible rows (fewer than 100 row elements in the DOM). The test is in Task 6.
5. **Logins off.** On a server with `auth` off, `GET /api/v1/auth/web-session` answers `{"auth": false}`, and the page goes straight to the torrents with no sign-in screen. The tests are in Tasks 2 and 5.

---

### Task 1: The `web-ui` switch: config, environment, features and version string

**Files:**
- Modify:
  - `crates/fbtt-core/src/config.rs`: `ServerConfig` (around line 191) and its `Default` (around line 219);
  - every struct literal of `ServerConfig` in tests: `crates/fbtt-server/src/auth/mod.rs` tests (around lines 454-470), `crates/fbtt-server/tests/server.rs:138`, `crates/fbtt-server/tests/remote.rs:176` and `crates/featherbittorrent/tests/serve.rs:156`. Add `..Default::default()` or the new fields, and search for any others;
  - `crates/featherbittorrent/src/serve.rs`: the env override, applied before `Server::start_with_settings` (around line 149);
  - `crates/featherbittorrent/src/main.rs`: the version string;
  - the root `Cargo.toml` (`rust-embed`, `mime_guess` and `subtle` in `[workspace.dependencies]`, unused until later tasks), and `crates/fbtt-server/Cargo.toml` and `crates/featherbittorrent/Cargo.toml` (the features);
  - `dev/smoke-cli.sh`, `dev/smoke-cli.ps1` and `dev/smoke-linux.sh`: `--version` is matched by prefix, and the header comments say so.
- Test: unit tests in `config.rs` and `serve.rs`.

**Interfaces:**
- Produces:
  - in `fbtt_core::config`:
    - `ServerConfig::web_ui: Option<bool>`, serde default `None`, not written when `None`;
    - `ServerConfig::cookie_secure: CookieSecure`, with `#[serde(rename_all = "lowercase")] enum CookieSecure { #[default] Auto, Always }`, not written when `Auto`;
    - `pub fn web_ui_env_override(env: &dyn Fn(&str) -> Option<String>) -> Option<bool>`, which reads `FBTT_WEB_UI`. `0`, `false`, `off` and `no` mean off; `1`, `true`, `on` and `yes` mean on, case-insensitive; anything else is `None`;
  - in `featherbittorrent`'s `serve.rs`: before starting, `if let Some(v) = web_ui_env_override(&|k| std::env::var(k).ok()) { server.web_ui = Some(v) }`. The process environment is read only there, so no test ever sets it;
  - `ServerConfig::web_ui_on(&self) -> bool`, which is `self.web_ui != Some(false)`. The server asks only this;
  - cargo feature `web-ui` on `fbtt-server`, and on `featherbittorrent` as `web-ui = ["fbtt-server/web-ui"]`;
  - `featherbittorrent --version` prints `featherbittorrent <v> (full)` or `featherbittorrent <v> (headless)`.

- [ ] **Step 1: Write the failing tests:**
  - `web_ui_is_on_by_default`;
  - `the_config_turns_the_web_ui_off`;
  - `the_environment_override_parses_its_words`: every accepted word both ways, plus `maybe` giving `None`;
  - `the_environment_wins_over_the_config`: the serve-side helper applied to `Some(true)` with `FBTT_WEB_UI=0`, through an injected env closure;
  - `web_ui_and_cookie_secure_round_trip_in_yaml`: `server: {web_ui: false, cookie_secure: always}` parses and writes back. A config without them writes neither key.
- [ ] **Step 2: Run** `cargo test -p fbtt-core -p featherbittorrent`. Expect a compile failure.
- [ ] **Step 3: Implement.** In a build without `web-ui`, `serve` logs once: `server.web_ui is set, but this is the headless build: there is no web UI to serve`, when `web_ui` is `Some(_)` after the override.
- [ ] **Step 4: Run** `cargo test --workspace --all-features` and `cargo build -p featherbittorrent --features web-ui`. Expect a pass.
- [ ] **Step 5: Commit** `feat(server): the web-ui switch in the config, the environment and the build`.

### Task 2: Browser sessions: the cookie and the CSRF token

**Files:**
- Modify:
  - `crates/fbtt-server/src/routes/login.rs`: split the password check out of `password_login` (around line 269);
  - `crates/fbtt-server/src/auth/mod.rs`: `LOGIN_ROUTES` (around line 64), `authorize`, and a CSRF helper;
  - `crates/fbtt-server/src/routes/mod.rs`: the routes, under `DefaultBodyLimit::max(login::MAX_LOGIN_BODY)` like the other login routes.
- Create: `crates/fbtt-server/src/routes/web_session.rs`.
- Test: `crates/fbtt-server/tests/web_session.rs`, plus unit tests in `auth/mod.rs`.

**Interfaces:**
- Consumes: today's password login. It reads `Authorization: Basic` (see `credential`, around line 214), calls `limiter().reserve(peer)` before verifying and `limiter().success(peer)` after, and issues a bearer token.
- Produces:
  - **The shared check.** In `login.rs`, `pub(super) async fn verify_password(shared: &Shared, peer: SocketAddr, user: &str, password: &str) -> Result<String /*subject*/, RouteError>`. It does the limiter reserve, the verify (with the dummy hash for unknown names, as now) and the limiter success. `password_login` becomes the existing bearer wrapper around it, its answers unchanged, and the existing login tests pass untouched.
  - **The CSRF value.** In `auth/mod.rs`, `pub(crate) fn csrf_for(token: &str) -> String`, which is base64url (no padding) of SHA-256 of `b"fbtt-csrf\0"` followed by the token's bytes. Nothing new is stored. Deriving it never renews the session.
  - **`POST /api/v1/auth/web-session`.**
    - It is added to `LOGIN_ROUTES`. GET and DELETE are not.
    - It requires `Content-Type: application/json`; anything else gets `415`.
    - When an `Origin` header is present, it must pass `origin_is_allowed` (around line 169) against the server's allowed hosts; otherwise `403`.
    - Body: `{"user": string, "password": string}`, read with `read_capped` under `login::MAX_LOGIN_BODY`.
    - On success, `200 {"subject", "csrf", "expires_in"}` with `Set-Cookie: fbtt_session=<token>; HttpOnly; SameSite=Strict; Path=/; Max-Age=604800` and `; Secure` per the rule below.
    - Errors as the password login gives them: `401` and `429`.
  - **`GET /api/v1/auth/web-session`.**
    - With a valid cookie: `200 {"auth": true, "subject", "csrf"}`.
    - With logins off: `200 {"auth": false}`.
    - Otherwise `401`.
    - Always `Cache-Control: no-store`.
  - **`DELETE /api/v1/auth/web-session`.** It needs the CSRF header like any change. It revokes the session and answers `204` with `Set-Cookie: fbtt_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0`.
  - **`Secure`.**
    - Added when `cookie_secure: always`.
    - Under `auto`, added when `X-Forwarded-Proto: https` or `Forwarded: ...proto=https` is on the request.
    - `docs/web-ui.md` (Task 9) tells people behind a proxy that does not send either header to set `always`.
  - **The cookie's lifetime.** It lasts 7 days, and the server's own session expiry decides. A session the server has dropped answers `401`, and the page signs in again.
  - **`authorize`.** It changes only where there is no `Authorization` header **and** no `query_token`. Today's `match` keeps both of those paths exactly. Only then:
    - read `fbtt_session` from `Cookie` and check it with `Sessions::check`;
    - for any method but GET, HEAD and OPTIONS, also require `x-csrf-token` to equal `csrf_for(token)` (constant-time, through `subtle`), and an `Origin`, when present, to pass `origin_is_allowed`.

- [ ] **Step 1: Write the failing tests** against a `Server` with logins on and one user, as `tests/server.rs` builds one:
  - `a_browser_signs_in_with_a_cookie`: the cookie flags are as above, and `GET /api/v1/torrents` with only the cookie gets `200`;
  - `a_cookie_request_that_changes_something_needs_the_csrf_token`: with no header, `401`, message "a cookie call without its CSRF token was let through"; with the right header it gets past auth; with another session's token, `401`;
  - `a_bearer_header_never_falls_back_to_the_cookie`, and `a_query_token_never_falls_back_to_the_cookie` (on `/api/v1/events`);
  - `sign_out_ends_the_cookie_session`;
  - `the_secure_flag`: under `auto`, absent with no forwarded proto, present with `X-Forwarded-Proto: https` and with `Forwarded: for=1.2.3.4;proto=https`; under `always`, present;
  - `web_session_says_when_logins_are_off`;
  - `web_sign_in_shares_the_login_limiter`: the failures that trip the bearer login's limiter give `429` here too;
  - `web_sign_in_refuses_other_origins_and_bodies`: a foreign `Origin` gets `403`, message "a sign-in from another origin was accepted"; `Content-Type: text/plain` gets `415`.
- [ ] **Step 2: Run** `cargo test -p fbtt-server --test web_session`. Expect a failure.
- [ ] **Step 3: Implement** as specified.
- [ ] **Step 4: Run** `cargo test -p fbtt-server`. Expect a pass. The existing login tests must pass unchanged.
- [ ] **Step 5: Mutation-prove** two tests, using `bash ~/.claude/skills/local-implementer/scripts/prove-test.sh --expect '<message>' <patch> cargo test -p fbtt-server --test web_session <test>`:
  - the CSRF test, with a patch that drops the CSRF comparison;
  - the origin test, with a patch that drops the Origin check on the POST.
- [ ] **Step 6: Commit** `feat(server): browser sessions in an HttpOnly cookie with a CSRF token`.

### Task 3: The OpenAPI document the front end is typed from

**Files:**
- Create:
  - `web/src/api/openapi.json`, generated;
  - `crates/fbtt-server/tests/openapi_snapshot.rs`.

**Interfaces:**
- Produces: `web/src/api/openapi.json`, the exact document `GET /api/v1/openapi.json` serves, pretty-printed with a trailing newline.

- [ ] **Step 1: Write the test** `the_web_ui_has_the_current_api_document`.
  - It starts a server, as the other tests do, and fetches `/api/v1/openapi.json`.
  - It compares the response, pretty-printed, with the file.
  - With `UPDATE_OPENAPI=1` it rewrites the file instead.
  - On a mismatch the message is "web/src/api/openapi.json is out of date: run UPDATE_OPENAPI=1 cargo test -p fbtt-server --test openapi_snapshot".
- [ ] **Step 2:** Run it once with `UPDATE_OPENAPI=1`, then without. Expect a pass.
- [ ] **Step 3: Commit** `test(server): keep the web UI's copy of the API document current`.

### Task 4: The front end skeleton, embedded and served

**Files:**
- Create:
  - `web/package.json`, `web/package-lock.json`, `web/vite.config.ts`, `web/tsconfig.json`, `web/eslint.config.js`, `web/index.html`;
  - `web/src/main.tsx`, `web/src/App.tsx`, `web/src/theme.css` (the tokens above as CSS custom properties, `body` on `#0b0b0e`);
  - `web/src/api/schema.ts`, generated by `openapi-typescript src/api/openapi.json -o src/api/schema.ts` through the npm script `gen:api`;
  - `web/public/favicon.png`, copied from `../design-system-v2/assets/featherbit-mark.png`;
  - `web/scripts/check-bundle-size.mjs`.
- Create: `crates/fbtt-server/build.rs`.
- Extend: `crates/fbtt-server/src/web.rs` (static serving).
- Modify: `crates/fbtt-server/src/routes/mod.rs` and `auth/mod.rs` (the static routes and their auth).
- Modify: `.github/workflows/ci.yml`.
- Modify: `.gitignore` (`web/node_modules`, `web/dist`).

**Interfaces:**
- Produces:
  - static routes `GET`/`HEAD` `/` and `/assets/{*path}`, plus `/favicon.png`, from `fbtt_server::web::router()`. `server.rs`'s `build` merges them into `routes::router(...)` only with `cfg(feature = "web-ui")` and `config.server.web_ui_on()`. `server.rs` therefore joins this task's files;
  - the npm scripts `dev`, `build`, `lint`, `typecheck`, `test`, `gen:api` and `size`.

- [ ] **Step 1: Write the failing tests** in `crates/fbtt-server/tests/web_ui.rs`, `#![cfg(feature = "web-ui")]`:
  - `the_page_is_served_without_a_session`: `GET /` with logins on and no credentials gives `200 text/html`, containing `<div id="root">`.
  - `the_page_carries_the_security_headers`: exactly:
    - `Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; style-src-attr 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; font-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'`. `style-src-attr` is there for React `style=` on progress bars and virtualised rows;
    - `X-Frame-Options: DENY`;
    - `X-Content-Type-Options: nosniff`;
    - `Referrer-Policy: no-referrer`;
    - `Cache-Control: no-store` on `/`, and `public, max-age=31536000, immutable` on `/assets/*`.
  - `an_unknown_asset_is_a_404`: `/assets/nope.js` gives `404`.
  - `the_off_switch_removes_the_page`: with a config `web_ui: false`, `GET /` gives `404`, and the API still answers. The environment's part is Task 1's test.
  - `the_headers_hold_on_head_and_404_too`.
  - `logins_off_still_check_the_host`: with logins off, `/` obeys the same host guard as the API.

  Never set the process environment in a test.
- [ ] **Step 2: Run** `cargo test -p fbtt-server --features web-ui --test web_ui`. Expect a failure.
- [ ] **Step 3: Implement.**
  - **The front end.** Scaffold `web/` with the gateway's versions:
    - pin exact versions in `package.json`;
    - commit the lockfile;
    - `engines: {"node": ">=22"}`;
    - hash routing: no server fallback is needed.
  - **`build.rs`.**
    - Emit `cargo:rerun-if-changed=../../web/dist`.
    - With `CARGO_FEATURE_WEB_UI` set and `../../web/dist/index.html` missing, fail with: `the web-ui feature embeds web/dist, which is missing: run npm --prefix web ci && npm --prefix web run build`.
  - **Serving.**
    - `#[derive(rust_embed::Embed)] #[folder = "../../web/dist/"]`, with the MIME type from `mime_guess`.
    - `authorize` lets GET and HEAD on exactly the templates `/`, `/favicon.png` and `/assets/{*path}` through with logins on, compared by template equality as `login_route` compares. With logins off it runs `guard_local`.
  - **`check-bundle-size.mjs`.** It gzips every `dist/assets/*.js` and `*.css` and fails above 300 KB in total.
  - **CI.**
    - Every `check` job gets `actions/setup-node` (node 22, npm cache on `web/package-lock.json`) and `npm --prefix web ci && npm --prefix web run build` before cargo.
    - A new job `web` runs `npm ci`, `npm run lint`, `npm run typecheck`, `npm test` (vitest with `--passWithNoTests` until Task 5 adds tests), `npm run size` and `npm audit --audit-level=high`.
    - A step checks that `src/api/schema.ts` is up to date: `npm run gen:api && git diff --exit-code src/api/schema.ts`.
- [ ] **Step 4: Run** the Rust tests above, `npm --prefix web run lint`, `typecheck`, `test` and `size`. Expect them all to pass.
- [ ] **Step 5: Commit** `feat(web): the web UI skeleton, embedded behind the web-ui feature`.

### Task 5: API client, session and Sign in

**Files:**
- Create:
  - `web/src/api/client.ts`;
  - `web/src/session.ts`;
  - `web/src/screens/SignIn.tsx`;
  - `web/src/components/` (`Button`, `TextField`, `Banner`), used by later tasks.
- Test: `web/src/api/client.test.ts` and `web/src/session.test.ts`.

**Interfaces:**
- Produces:
  - `createClient(fetchFn: typeof fetch, getCsrf: () => string | null, onUnauthorized: () => void)`, returning typed methods built from `schema.ts`:
    - `torrents()`, `torrent(id)`, `files(id)`, `peers(id)`, `trackers(id)`, `pieces(id)`, `session()`;
    - `pause(id)`, `resume(id)`, `remove(id, deleteFiles: boolean)`;
    - `addMagnet(magnet, savePath, paused)`, `addFile(bytes: ArrayBuffer, savePath, paused)`;
    - `roots()`, `list(path)`.
  - `useSession()` with the states `loading | signedOut | signedIn{subject, csrf} | open` (logins off), and the actions `signIn(user, password)` and `signOut()`.
  - Every call uses `credentials: 'same-origin'`, and sends `X-CSRF-Token` on non-GET.
  - A `401` from any call runs `onUnauthorized` once, which moves the session to `signedOut` with the notice "Your session ended. Sign in again.".

- [ ] **Step 1: Write the failing tests:**
  - `sends_the_csrf_token_on_changes_only`;
  - `a_401_signs_out_once_and_does_not_retry`: two calls answering `401` give one `onUnauthorized` call. Message: "a 401 did not sign the page out".
  - `logins_off_skips_sign_in`: the GET answering `{"auth": false}` gives the state `open`.
  - `sign_in_failure_shows_the_server_message`: a `401` from the POST shows "Wrong user name or password." and a `429` shows "Too many attempts. Try again in a minute.".
- [ ] **Step 2: Run** `npm --prefix web test`. Expect a failure.
- [ ] **Step 3: Implement.**
  - The Sign in screen is the prototype's, with its password form only: no device-key button.
  - It shows the plain-http warning when `location.protocol === 'http:'` and the host is not `localhost`, `127.0.0.1` or `[::1]`: "This page is not encrypted: the password travels in the clear. Use https through a reverse proxy beyond your LAN."
- [ ] **Step 4: Run.** Expect a pass. Prove `a_401_signs_out_once_and_does_not_retry` with a patch that drops the `onUnauthorized` call: `bash ~/.claude/skills/local-implementer/scripts/prove-test.sh --expect 'a 401 did not sign the page out' <patch> npm --prefix web test -- client`.
- [ ] **Step 5: Commit** `feat(web): the API client, the browser session and Sign in`.

### Task 6: Torrents: header, filters, table and live refresh

**Files:**
- Create:
  - `web/src/live.ts` (the refresh loop);
  - `web/src/screens/Torrents.tsx`;
  - `web/src/components/TorrentTable.tsx`, `StateFilters.tsx`, `Header.tsx`;
  - `web/src/format.ts` (sizes, rates, percent).
- Test: `web/src/live.test.ts`, `web/src/format.test.ts` and `web/src/components/TorrentTable.test.tsx` (with `@testing-library/react` and `jsdom` as devDependencies).

**Interfaces:**
- Consumes: Task 5's client.
- Produces:
  - `startLive(client, onRows, onLink, opts: {now, setTimeout, visibility, eventSource})`, all injected for tests;
  - `LinkState` = `live | lost{retryIn}`;
  - `formatBytes`, `formatRate` and `formatPercent`. They match the TUI's units: `B`, `KB`, `MB`, `GB` with one decimal, rates with a `/s` suffix.

- [ ] **Step 1: Write the failing tests.**
  - `refreshes_every_second_and_wakes_on_events`: fake timers. One event while a refresh is in flight gives exactly one extra refresh after it. Message: "events were not coalesced".
  - `slows_to_ten_seconds_while_hidden`.
  - `a_lost_server_keeps_the_rows_and_backs_off`:
    - failures give the `LinkState.lost` retry pauses 1, 2, 4, 8, 16, 30 and 30 s;
    - the last rows stay;
    - a success gives `live`.
  - `two_thousand_rows_render_under_a_hundred`: the table with 2,000 rows in a 900 px viewport has fewer than 100 `tr` elements. Message: "the table rendered every row".
  - `sorts_by_any_column_and_filters_by_name`: clicking a header sorts and clicking again reverses. The filter box matches names case-insensitively.
  - `format_units`: `formatBytes(5_800_000_000)` is `5.8 GB` and `formatRate(850_000)` is `850.0 KB/s`. Check the TUI's own formatter in `crates/fbtt-tui` for its exact rules and copy them; the TUI is the reference.
- [ ] **Step 2: Run.** Expect a failure.
- [ ] **Step 3: Implement.**
  - **The screen.** The prototype's Torrents screen:
    - the header: the mark, `featherbittorrent`, the version from `session()`, the connection pill (cyan, live; red with "Reconnecting in N s" when lost), the total down and up rates summed from the rows, the DHT nodes from `session()`, and Add torrent;
    - the state filters with counts: All, Downloading, Seeding, Stalled, Paused, Errors, using the TUI's state colours;
    - the table: name, progress bar and percent, down, up, peers and state.
  - **Virtualisation.** Windowing by row height. Write it in-house; no virtualisation library.
  - **Phone width.** Below 768 px the state filters stack above the table as a scrolling row, the toolbar wraps, and the table scrolls sideways in its own box. Test `the_layout_stacks_at_phone_width` checks the sidebar's class at a 390 px viewport.
  - **Events.** Subscribe with `new EventSource('/api/v1/events?categories=status')` (the cookie authenticates it, same-origin), and close it when the screen unmounts.
    - When it reaches `readyState === CLOSED`, which a `401` or a refused stream causes, reopen it on the same 1, 2, 4, 8, 16, 30 s ladder.
    - Never sign out from the event stream: the 1 s refresh's `401` does that.
    - Test `a_closed_event_stream_is_reopened_with_backoff`.
- [ ] **Step 4: Run.** Expect a pass. Prove the coalescing test and the row-count test with one patch each:
  - the first refreshes once per event;
  - the second renders every row.
- [ ] **Step 5: Commit** `feat(web): the torrent table with live refresh`.

### Task 7: The detail pane

**Files:**
- Create: `web/src/components/DetailPane.tsx` and `web/src/components/detail/{Files,Peers,Trackers,Pieces}.tsx`.
- Test: `web/src/components/DetailPane.test.tsx`.

**Interfaces:**
- Consumes: the client's `files`, `peers`, `trackers` and `pieces`, and the selected torrent id from Task 6.
- Produces: `<DetailPane id={string | null} client={…} />`.

- [ ] **Step 1: Write the failing tests:**
  - `loads_the_selected_tab_only`: switching tabs fetches only that tab's data. The selected tab refreshes on the same 1 s tick.
  - `a_removed_torrent_closes_the_pane`: a `404` closes the pane, with the notice "That torrent was removed." and no further requests. Message: "the pane kept asking for a removed torrent".
  - `files_show_size_progress_and_priority`: read only in this milestone. Priorities are edited in milestone 2.
- [ ] **Step 2: Run.** Expect a failure.
- [ ] **Step 3: Implement** to the prototype's detail pane:
  - **Files:** path, size, done and priority, but no "Play in browser" in this milestone.
  - **Peers:** address, client, down and up.
  - **Trackers:** URL, status and message.
  - **Pieces:** the grid, with have in violet and missing in `#262629`. With more than 2,000 pieces, draw it on one `<canvas>`.
- [ ] **Step 4: Run.** Expect a pass. Prove `a_removed_torrent_closes_the_pane` with a patch that ignores the `404`.
- [ ] **Step 5: Commit** `feat(web): the detail pane`.

### Task 8: Pause, resume, delete and Add torrent

**Files:**
- Create: `web/src/components/AddTorrent.tsx`, `web/src/components/FolderPicker.tsx` and `web/src/components/ConfirmDelete.tsx`.
- Modify: `web/src/screens/Torrents.tsx` (the toolbar).
- Test: `web/src/components/AddTorrent.test.tsx` and `web/src/components/ConfirmDelete.test.tsx`.

**Interfaces:**
- Consumes the client's `pause`, `resume`, `remove`, `addMagnet`, `addFile`, `roots` and `list`. Adding uses:
  - `POST /api/v1/torrents`, JSON `{"magnet", "save_path", "paused"}`;
  - or the same route with `Content-Type: application/x-bittorrent`, the raw file as the body, and `save_path` and `paused` in the query.

- [ ] **Step 1: Write the failing tests:**
  - `delete_asks_whether_to_keep_the_files`:
    - "Remove and keep files" sends `delete_files=false`;
    - "Remove and delete files" sends `true`;
    - Escape cancels with no request.
  - `add_uses_the_default_folder_and_only_allowed_ones`: the picker starts at the first root from `roots()`, lists only what `list()` returns, and sends the chosen path as `save_path`.
  - `a_torrent_file_is_uploaded_as_is`: a dropped `.torrent` is sent as `application/x-bittorrent` with its exact bytes.
  - `an_add_error_keeps_the_dialog_open`: the server's error message is shown, and the dialog keeps its fields.
- [ ] **Step 2: Run.** Expect a failure.
- [ ] **Step 3: Implement** to the prototype's Add torrent dialog, with:
  - magnet or file;
  - the folder tree;
  - "Start paused".

  There is no file selection in this milestone: it comes with the priorities in milestone 2. The dialog is a real `<dialog>`, focus is trapped, and Escape closes it.
- [ ] **Step 4: Run.** Expect a pass. Prove `delete_asks_whether_to_keep_the_files` with a patch that always sends `true`.
- [ ] **Step 5: Commit** `feat(web): pause, resume, delete and add torrents`.

### Task 9: Two builds in every release, docs and changelog

**Files:**
- Modify:
  - `dev/package-linux.sh`, `dev/package-macos.sh`, `dev/package-windows.ps1`;
  - `dev/smoke-linux.sh`, `dev/smoke-cli.sh`, `dev/smoke-cli.ps1`;
  - `dev/test-packaging.sh`;
  - `.github/workflows/release.yml`;
  - `Dockerfile`;
  - the package metadata in `crates/featherbittorrent/Cargo.toml` (`[package.metadata.deb]` and `generate-rpm`).
- Create: `docs/web-ui.md`.
- Modify: `docs/install.md`, `docs/server.md` (the `web_ui` key and `FBTT_WEB_UI`), `README.md`, `CHANGELOG.md`, `CLAUDE.md` (status, commands: `npm --prefix web ...`, the two builds).

**Interfaces:**
- **Packaging scripts.** Each takes a new last argument `full|headless` (default `full`).
  - `full` builds `--features web-ui` and names everything `featherbittorrent-<v>-...`.
  - `headless` builds without it and names everything `featherbittorrent-headless-<v>-...`.
- **Linux packages.** The `.deb` and `.rpm` package names are `featherbittorrent` and `featherbittorrent-headless`. Both install the binary `/usr/bin/featherbittorrent`, and each declares that it conflicts with and replaces the other.
- **Release workflow.** A `variant: [full, headless]` matrix axis on the linux, macos and windows jobs and on the smoke jobs. The full jobs build `web/` first.
- **Smoke checks.** `--version` must end with `(full)` or `(headless)` to match the package.

- [ ] **Step 1: Extend** `dev/test-packaging.sh`. Its version-mapping and argument-refusal checks cover the new argument: an unknown variant is refused with exit 2.
- [ ] **Step 2: Run** `bash dev/test-packaging.sh`. Expect a failure.
- [ ] **Step 3: Implement.**
  - The scripts, the metadata (a second deb/rpm metadata set for the headless name, or the name passed on the command line; choose the simpler one that `cargo-deb` and `cargo-generate-rpm` support, and say which in the report).
  - The release matrix.
  - The Dockerfile builds the full variant and installs node for the web build.
  - The docs:
    - `docs/web-ui.md` covers:
      - what it is, and the off switch;
      - browser sign-in, including `cookie_secure: always` behind a proxy that does not send the scheme;
      - behind the gateway: a gateway that forwards its own `Authorization` header needs featherbittorrent's logins off, the 2026-11-15 gateway-only setup;
      - that streaming in the browser comes later;
    - the install docs name both packages.
  - The CHANGELOG `### Added` entry, ending with this PR's link.
- [ ] **Step 4: Run.**
  - `bash dev/test-packaging.sh` and `bash dev/test-service-examples.sh` must pass.
  - Build both variants locally with `cargo build --release -p featherbittorrent` and `--features web-ui`, then run `dev/smoke-cli.sh` against each.
  - Start the release workflow on the branch (`workflow_dispatch`) and check all of its jobs pass.
- [ ] **Step 5: Commit** `ci: ship the full and the headless build in every release`.

## After the last task

The controller drives the full build end to end:
1. Start `featherbittorrent serve --offline` with a scratch config: a user, logins on, an allowed root.
2. Open the page in a real browser through the controller's own browser tooling, or let the owner do it.
3. Sign in, add a magnet, pause, resume, delete with keep, and open each detail tab.
4. Sign out.

Then comes the whole-branch review, and one PR to `develop`.
