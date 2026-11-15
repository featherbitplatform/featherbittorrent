# Milestone 2: server, remote client, login, release 0.1.0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. The implementer step runs through the `local-implementer` skill.

**Goal:** `featherbittorrent serve` runs a headless server with password and device-key login, the TUI connects to it (or to the local engine) through the same screens, and featherbittorrent ships its first release, `0.1.0`.

**Architecture:** the engine lifecycle that `LocalBackend` owns today (restore, periodic and final saves, every state-file safety rule) moves into one `fbtt_core::engine::Engine` used by both the local backend and the server. The server is a new crate `fbtt-server`: fbt-api's `ApiServer` with `Auth::Custom` (our authenticator) and extra routes for login, devices and folder browsing (fbt-lib `v0.9.0`, plan `../fbt-lib/docs/superpowers/plans/2026-11-15-pluggable-auth-0.9.md`, must be released first). The TUI gets a `RemoteBackend` behind the existing `Client` interface, so every screen works unchanged against a server.

**Tech Stack:** as milestone 1, plus `fbt-api` (git tag `v0.9.0`), `axum` (the version fbt-api uses), `reqwest` (rustls with ring, json, stream), `argon2`, `ssh-key` (ed25519, encryption), `ssh-agent-lib` (client), `keyring`, `rpassword`, `getrandom`, `base64`, `tracing` + `tracing-subscriber`.

**Spec:** `docs/superpowers/specs/2026-11-14-featherbittorrent-design.md` (sections 6 and 5.2 matter most).

## Global Constraints

Everything in milestone 1's Global Constraints, plus:
- `fbt` and `fbt-api` both pinned to tag `v0.9.0` (same git source).
- Secrets: the server stores Argon2id PHC hashes only; the TUI stores a password only in the OS keychain (`keyring`, service `featherbittorrent`, account `<user>@<server url>`); private keys are only read from their path or used through ssh-agent, never copied. No secret in config files, logs, `Debug` output, process arguments or error messages.
- Ed25519 device keys are required and are the only key type in this milestone (RSA pulls the `rsa` crate, which has an open advisory).
- Authentication is on by default whenever the server binds to anything other than loopback; turning it off is the explicit `server.auth.enabled: false`.
- File-system access through the API is confined to `server.allowed_roots` (default: `[download_dir]`).
- The UI never waits on the network: every HTTP call runs on the backend task.

## Review Focus

1. Login brute force: more than 10 failed logins from one address within 5 minutes → `429` with `Retry-After`, for passwords and keys alike. Test in Task 3 (`login_failures_are_rate_limited`).
2. A signed challenge replayed, or a challenge for one key answered with another, is refused. Test in Task 3 (`challenge_is_single_use`, `challenge_is_bound_to_key`).
3. Folder browsing outside the roots: `..`, an absolute path elsewhere, a symlink pointing out of a root → `403`. Test in Task 3 (`fs_list_stays_in_roots`).
4. The server goes away mid-session: the TUI keeps the last snapshot, the header shows the connection as lost and reconnecting, keys still work, nothing freezes; it reconnects with backoff. Test in Task 6 (`server_restart_reconnects`).
5. A hostile or broken server: a 100 MiB response, garbage JSON, an SSE line with no end → bounded reads, one error notice, no panic. Tests in Task 6 (`hostile_server_is_bounded`).

---

### Task 1: Pin fbt-lib 0.9.0, extract `Engine`, config for the server

**Files:**
- Modify: `Cargo.toml` (fbt and fbt-api at `v0.9.0`), `crates/fbtt-core/src/config.rs`, `crates/fbtt-core/src/local.rs` (+ `local/state.rs`, `local/map.rs` move)
- Create: `crates/fbtt-core/src/engine.rs` (+ `engine/state.rs`, `engine/map.rs`), `crates/fbtt-core/src/ui_state.rs`

**Interfaces:**
- Produces:
  - `pub struct Engine` owning `fbt::Session`, the state-file rules (temp + sync + rename, `writes_blocked`, `.bad`/`.bak` names, 300 s saves) and the library lock: `Engine::start(settings: fbt::Settings, paths: &Paths, lock: LibraryLock) -> Result<(Engine, Vec<Notice>), SessionError>`, `engine.session() -> &fbt::Session`, `engine.save_periodic(&mut self) -> Option<Notice>` (call every 300 s), `engine.shutdown(self) -> Result<(), String>` (final save or plain shutdown when blocked; drops the lock last). `LocalBackend` keeps its exact behaviour on top of it; every milestone-1 test passes unchanged.
  - `map.rs` (`TorrentStatus` → `TorrentRow`, stalled clock) becomes `pub(crate)` in `engine` so the server does not need it but the local backend keeps using it.
  - Config gains `#[serde(default)] pub server: ServerConfig { bind: SocketAddr (default 127.0.0.1:8090), allowed_roots: Vec<PathBuf> (empty = [download_dir]), auth: AuthConfig { enabled: Option<bool> (None = on unless bind is loopback), users: Vec<UserEntry { name, password_hash }>, devices: Vec<DeviceEntry { name, public_key }> } }`. `UserEntry`'s `Debug` prints the hash as `<hash>`. `ServerConfig::auth_on(&self) -> bool`. `Config::resolve` also makes `allowed_roots` absolute and fills the default.
  - `ui_state.rs`: `ui.yaml` in the config dir, `pub enum LastUsed { Local, Remote(RemoteTarget) }`, `pub struct RemoteTarget { url: String, login: LoginMethod, user: Option<String>, key_path: Option<PathBuf> }`, `pub enum LoginMethod { None, Password, KeyFile, Agent, GatewayKey, GatewayBasic }` (the gateway methods authenticate to a gateway in front of a server with auth off; `RemoteTarget` gains `token_url: Option<String>` and `client_id: Option<String>` for `GatewayKey`); `UiState::load(&Paths) -> UiState` (unreadable → default `Local`, never fatal), `save`.

- [ ] **Step 1: Failing tests:** `engine_round_trip` (start, add via session, shutdown, start again, torrent back), the milestone-1 `tests/local.rs` suite unchanged, `server_config_defaults` (loopback bind → `auth_on()` false; `0.0.0.0:8090` → true; explicit `enabled: false` → false), `user_debug_hides_hash`, `ui_state_round_trip`, `bad_ui_state_is_local`.
- [ ] **Step 2: Run** `cargo test --workspace --all-features`. Expected: new tests FAIL.
- [ ] **Step 3: Implement.** The extraction moves code; it does not rewrite the safety rules.
- [ ] **Step 4: Run** the full gate. Expected: PASS.
- [ ] **Step 5: Commit** `refactor(core): shared engine lifecycle and server config`.

### Task 2: Auth store and authenticator

**Files:** Create `crates/fbtt-server/{Cargo.toml,src/lib.rs,src/auth/mod.rs,src/auth/password.rs,src/auth/keys.rs,src/auth/sessions.rs,src/auth/limiter.rs}`

**Interfaces:**
- Consumes: `AuthConfig`, `UserEntry`, `DeviceEntry` (Task 1); `fbt_api::{Authenticator, AuthRequest, AuthRejection}` (fbt-lib 0.9.0).
- Produces:
  - `password::hash(password: &str) -> String` (Argon2id, `Argon2::default()` parameters, random salt, PHC string) and `password::verify(phc: &str, password: &str) -> bool` (constant-time via the argon2 crate; false on a malformed hash).
  - `keys::parse_device_key(text: &str) -> Result<ssh_key::PublicKey, KeyError>` (OpenSSH one-line format; Ed25519 only, else `KeyError::Unsupported`), `keys::fingerprint(&PublicKey) -> String` (SHA-256, OpenSSH style).
  - `pub const LOGIN_NAMESPACE: &str = "featherbittorrent-login";` signatures are `SshSig` over the raw nonce bytes with `HashAlg::Sha512`; `keys::verify_login(key: &PublicKey, nonce: &[u8], sig_pem: &str) -> bool`.
  - `sessions::Sessions`: `issue(subject: String) -> (String, Duration)` returns a 32-byte random token (`getrandom`), base64url without padding, valid 1 h, sliding (each successful use renews); stores only `SHA-256(token)`; at most 5 000 live sessions (the oldest expire first); `check(token) -> Option<String>`; `revoke(token)`.
  - `sessions::Challenges`: `issue(fingerprint: String) -> Vec<u8>` (32 random bytes, valid 60 s, single use, at most 1 000 pending, oldest evicted), `take(fingerprint, nonce) -> bool`.
  - `limiter::LoginLimiter`: per IP address, 10 failures in a sliding 5-minute window → blocked until the window clears; `check(ip) -> Result<(), u64 /*retry_after_secs*/>`, `failure(ip)`, `success(ip)` (clears).
  - `pub struct FbttAuth` (implements `fbt_api::Authenticator`): with auth off, allows everything; with auth on, allows `POST /api/v1/auth/challenge` and `POST /api/v1/auth/session` without a token, and everything else only with `Authorization: Bearer <session token>` (or `query_token` on the routes fbt-api offers it for) that `Sessions::check` accepts. `Debug` prints counts only.
- Mechanism: all of this is in memory, behind `std::sync::Mutex` (short critical sections, no `.await` inside); Argon2 verification is slow and runs on the blocking pool in the login route (Task 3), never inside `authorize`.

- [ ] **Step 1: Failing tests:** `hash_and_verify` (right password true, wrong false, malformed hash false), `rsa_device_key_is_unsupported`, `ed25519_round_trip_signature` (generate a key with `ssh_key::PrivateKey::random`, sign the nonce with `LOGIN_NAMESPACE` and `Sha512`, `verify_login` true; wrong nonce false; other key false), `session_expires_and_slides` (inject a clock: `Sessions::with_clock`), `session_cap_evicts_oldest`, `challenge_single_use_and_expiry`, `limiter_blocks_after_10_failures`, `authorizer_routes` (auth off allows all; on: login routes open, others need a valid bearer, a bogus bearer is 401).
- [ ] **Step 2: Run** `cargo test -p fbtt-server`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the full gate. Expected: PASS.
- [ ] **Step 5: Commit** `feat(server): password, device key and session auth`.

### Task 3: Server routes and `Server`

**Files:** Create `crates/fbtt-server/src/{server.rs,routes/mod.rs,routes/login.rs,routes/devices.rs,routes/fs.rs}`, `crates/fbtt-server/tests/server.rs`

**Interfaces:**
- Consumes: Task 1's `Engine`, `Config`; Task 2's auth pieces; `fbt_api::ApiServer::start_with`.
- Produces:
  - `Server::start(config: Config, paths: &Paths, lock: LibraryLock) -> Result<Server, ServerError>`: starts the `Engine`, builds `ApiConfig` (bind from config, `allowed_roots` from config, `Auth::Custom(FbttAuth)`), serves fbt-api plus the extra routes; runs the 300 s periodic save on its own task; `server.local_addr()`, `server.shutdown(self) -> Result<(), String>` (API first, then the engine's final save).
  - Routes (all JSON, errors as fbt-api's error body shape):
    - `POST /api/v1/auth/challenge` `{ "public_key": "<openssh line>" }` → `{ "nonce": "<base64>", "expires_in": 60 }`. Always answers (an unknown key gets a nonce that can never verify), so it does not reveal which keys are authorised.
    - `POST /api/v1/auth/session` with either `Authorization: Basic <user:password>` or `{ "public_key", "nonce", "signature": "<SSHSIG armored>" }` → `{ "token", "expires_in", "subject" }`; failures → 401 and a limiter failure; limited → 429 with `Retry-After`.
    - `DELETE /api/v1/auth/session` → revokes the caller's token.
    - `GET /api/v1/auth/devices` → `[{ name, fingerprint }]`; `POST /api/v1/auth/devices` `{ name, public_key }` (name 1-64 printable chars, unique; key parsed and Ed25519) → 201, saved to `config.yaml`; `DELETE /api/v1/auth/devices/{name}` → 204, saved.
    - `GET /api/v1/fs/roots` → `[path]`; `GET /api/v1/fs/list?path=<absolute>` → `{ path, dirs: [name] }` (directories only, sorted, at most 2 000 entries, names through `display_safe` and hidden ones skipped). The path must canonicalize to a location inside a canonicalized root, else 403.
  - Server log lines through `tracing` (logins with subject and peer address, never secrets). At start, with password users, a non-loopback bind and auth on, log a warning that passwords cross the network unencrypted unless a TLS reverse proxy is in front.
- [ ] **Step 1: Failing tests** in `tests/server.rs` (server on `127.0.0.1:0`, loopback-only engine settings via a test-support constructor `Server::start_with_settings`, `reqwest` as the client): `password_login_then_api` (Basic → token → `GET /api/v1/torrents` 200; without token 401), `key_login_then_api`, `challenge_is_single_use`, `challenge_is_bound_to_key`, `login_failures_are_rate_limited`, `devices_add_list_remove_persist` (config.yaml reflects it), `fs_list_stays_in_roots` (inside OK; `..`, `/`, a symlink out of the root → 403), `auth_off_allows_all`, `logout_revokes`.
- [ ] **Step 2: Run** `cargo test -p fbtt-server --all-features`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the full gate. Expected: PASS.
- [ ] **Step 5: Commit** `feat(server): login, devices and folder routes`.

### Task 4: `serve`, the wizard, `user` and `device` commands

**Files:** Modify `crates/featherbittorrent/src/main.rs`; Create `crates/featherbittorrent/src/{serve.rs,wizard.rs,admin.rs}`, tests in `crates/featherbittorrent/tests/serve.rs`

**Interfaces:**
- Consumes: `Server`, `Config`, `password::hash`, `keys::parse_device_key`.
- Produces (clap subcommands):
  - `featherbittorrent serve`: resolves paths and config; if `config.yaml` is missing or has no `server` section, runs the wizard first; takes the library lock (held → `Another featherbittorrent is already using this library (<data dir>).`, exit 3); logs to stderr (`tracing-subscriber`, `RUST_LOG` honoured, default `info`); stops cleanly on Ctrl-C and SIGTERM (`Server::shutdown`), exit 0, or 1 with the save error.
  - Wizard (plain prompts on stdin/stdout, defaults in brackets, Enter accepts): download folder `[<default>]`, BitTorrent port `[6881]`, API address `[127.0.0.1:8090]`, authentication `[yes]` if the address is not loopback else `[no]`; with auth on: a user name (empty skips) and password twice (`rpassword` when stdin is a terminal, a plain line otherwise), and "Add a device key now? [no]" (paste an OpenSSH public key line). Writes `config.yaml` and prints its path and `featherbittorrent device add` usage.
  - `featherbittorrent user set <name>` (asks the password twice), `user remove <name>`, `user list`; `device add <name> <key line or path to .pub>`, `device remove <name>`, `device list` (name and fingerprint). They edit `config.yaml` and take the library lock first; if it is held: `The library is in use (a server or TUI is running). Stop it first, or add the device from a connected TUI.` exit 3.
- [ ] **Step 1: Failing tests:** `wizard_writes_config` (feed stdin lines; the YAML holds a `$argon2id$` hash and not the password), `user_set_and_list`, `device_add_rejects_rsa`, `admin_refuses_while_locked`, `serve_stops_cleanly` (start `serve` on a scratch library with `listen_port: 0` and `server.bind: 127.0.0.1:0`; it prints `listening on <addr>` as its first log line; the test creates the file named by the hidden debug-only flag `--stop-file <path>` (`cfg(debug_assertions)`), which the server checks every second and treats exactly like SIGTERM; assert exit 0 and that `session.state` exists). The real signal path is covered by the controller's manual check.
- [ ] **Step 2: Run** `cargo test -p featherbittorrent`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the full gate. Expected: PASS.
- [ ] **Step 5: Commit** `feat: serve command, setup wizard, user and device commands`.

### Task 5: Client interface additions and the local folder listing

**Files:** Modify `crates/fbtt-core/src/client.rs`, `crates/fbtt-core/src/local.rs`

**Interfaces:**
- Produces:
  - `Connection::Remote { name: String, secure: bool, auth: String, state: LinkState }` and `pub enum LinkState { Connecting, Connected, Lost { retry_in_secs: u64 } }`.
  - `Command::ListDirs(Option<PathBuf>)` (None = the roots) → `Snapshot.dirs: Option<DirListing>`, `pub struct DirListing { path: Option<PathBuf>, roots: Vec<PathBuf>, dirs: Vec<String>, error: Option<String> }`. The local backend answers from the file system on the blocking pool, roots = `[download_dir]` plus the user's home, same caps as the server route (2 000 entries, hidden skipped, `display_safe` names).
  - `Command::Devices(DeviceCommand)` with `DeviceCommand::{List, Add { name, public_key }, Remove(String)}` → `Snapshot.devices: Option<Vec<DeviceRow { name, fingerprint }>>`; the local backend answers `Notice::Error("Devices are managed on a server")`.
- [ ] **Step 1: Failing tests:** `local_list_dirs_roots_and_child`, `local_list_dirs_error_is_in_listing`, `local_devices_is_a_notice`.
- [ ] **Step 2-4:** run, implement, run the full gate.
- [ ] **Step 5: Commit** `feat(core): folder listing and connection state in the client interface`.

### Task 6: `RemoteBackend`

**Files:** Create `crates/fbtt-core/src/remote/{mod.rs,http.rs,sse.rs,login.rs,map.rs}`, `crates/fbtt-server/tests/remote.rs` (the end-to-end tests live in the server crate so they can start a `Server`)

**Interfaces:**
- Consumes: Task 5's types; fbt-api's routes and DTOs (read `../fbt-lib/crates/fbt-api/src/routes/*.rs` and `dto.rs` at tag `v0.9.0`; map every `Command` to its route there); Task 3's login routes.
- Produces: `RemoteBackend::start(target: RemoteTarget, secret: Secret) -> Client` where `pub enum Secret { None, Password(Zeroizing<String>), KeyPassphrase(Option<Zeroizing<String>>) }`; it never fails synchronously: connection problems show up as `LinkState` and notices.
- Mechanism (decided):
  - one backend task owns a `reqwest::Client` (rustls with ring, 10 s connect timeout, 30 s request timeout except streams);
  - login: Basic → token; or challenge → sign → session. Key file: `ssh_key::PrivateKey::read_openssh_file`, decrypt with the passphrase if encrypted, `sign(LOGIN_NAMESPACE, HashAlg::Sha512, &nonce)`. Agent: `ssh-agent-lib` client on `SSH_AUTH_SOCK` (Unix) or the OpenSSH agent pipe `\\.\pipe\openssh-ssh-agent` (Windows); find the identity matching the key path's `.pub` (or the first Ed25519 identity), sign `SshSig::signed_data(LOGIN_NAMESPACE, HashAlg::Sha512, &nonce)`, wrap with `SshSig::new`. A 401 on any call → log in again once, then `LinkState::Lost`;
  - updates: `GET /api/v1/events` (SSE) wakes an immediate refresh; `GET /api/v1/torrents` every 1 s and the detail route of the watched tab every 1 s give rates and progress; publish only on change like the local backend;
  - SSE parser: bytes stream, lines capped at 1 MiB (a longer line ends the stream with a notice), `event:`/`data:` fields, blank line dispatches; reconnect on end;
  - every response body is read with a 16 MiB cap (`.bytes_stream()` plus a counter), then `serde_json`; parse errors → one notice, the last good snapshot stays;
  - lost connection: keep the last snapshot, `LinkState::Lost { retry_in_secs }`, backoff 1, 2, 4, 8, 16, 30 s (cap), reset on success;
  - `Add` with `AddSource::File` uploads the bytes as fbt-api's add route expects; `dir` must be a server path (the TUI picks it with `ListDirs`);
  - Gateway methods (owner, 2026-11-15); no featherbittorrent login call is made (the server runs with auth off behind the gateway):
    - `GatewayKey`: OAuth2 client credentials with `private_key_jwt` (RFC 7523) at `token_url`: build a JWT assertion (`iss` = `sub` = `client_id`, `aud` = `token_url`, `jti` random, `exp` = now + 60 s, header `alg` `EdDSA` for an Ed25519 key, `kid` = the key fingerprint), sign it with the device key (file or ssh-agent, same code as the device login), POST `grant_type=client_credentials&client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer&client_assertion=<jwt>`, read `access_token` and `expires_in`, send `Authorization: Bearer <token>` on every request (SSE included), fetch a new token 30 s before expiry or after a 401.
    - `GatewayBasic`: `Authorization: Basic` on every request.
    - A 401/403 that persists after one refresh → `LinkState::Lost` plus a notice naming the gateway.
    - Tests: `gateway_key_gets_and_uses_token` (an axum test token endpoint verifying the assertion's signature with the device public key and claims, and an API that requires the issued bearer), `gateway_token_refreshed_before_expiry`, `gateway_basic_sent_on_every_request`.
  - `Shutdown` revokes the session (`DELETE /api/v1/auth/session`, 2 s timeout) and ends the task.
- [ ] **Step 1: Failing tests** in `crates/fbtt-server/tests/remote.rs`: `remote_password_end_to_end` (server + remote client; add a `.torrent` via the client, connect the server's torrent to a loopback seed through fbt-api's add-peer route called directly from the test, wait for `Seeding` in the remote snapshot), `remote_key_file_end_to_end` (passphrase-protected Ed25519 key written to a temp file), `server_restart_reconnects` (stop the server, `LinkState::Lost`, start it again on the same port, back to `Connected`, rows back), `hostile_server_is_bounded` (an axum test server returning a 100 MiB body, garbage JSON, and an SSE stream of one endless line: each ends in a notice, the client keeps running and answers `Shutdown`), `wrong_password_is_a_notice`.
- [ ] **Step 2: Run** `cargo test -p fbtt-server --all-features --test remote`. Expected: FAIL.
- [ ] **Step 3: Implement.** Keep each file under about 400 lines.
- [ ] **Step 4: Run** the full gate. Expected: PASS.
- [ ] **Step 5: Commit** `feat(core): remote backend over the server api`.

### Task 7: TUI: connect screen, folder picker, devices, remote header

**Files:** Create `crates/fbtt-tui/src/modal/{connect.rs,folders.rs,devices.rs}`; Modify `keys.rs`, `model.rs`, `view/header.rs`, `modal/add.rs`

**Interfaces:**
- Consumes: Task 5's `Connection`, `LinkState`, `DirListing`, `DeviceRow`, `Command::{ListDirs, Devices}`; `fbtt_core::ui_state::{RemoteTarget, LoginMethod}`.
- Produces:
  - `c` opens the connect screen: target `Local engine` or `Server`; for a server: URL (`http://` or `https://`), login method (cycle with Left/Right: none, password, key file, ssh-agent, gateway private key, gateway password; the private-key method asks for the provider's token URL, the client id and the key path or ssh-agent), user, password (masked, `Remember in the system keychain` toggle), key path (default `~/.ssh/id_ed25519`). Enter → `Action::Connect(ConnectRequest)` (a new return channel: `handle_key` returns `Vec<Action>` where `pub enum Action { Command(Command), Connect(ConnectRequest), Quit }`; update every call site and test). Password over `http://` to a non-loopback host shows `Password sent without encryption: use https through a reverse proxy.` and needs a second Enter.
  - Add dialog: the `Save to` field gets `Ctrl+O`/`Tab` into a folder picker (`folders.rs`) driven by `ListDirs`: Enter opens a folder, Backspace goes up, `s` selects the current folder; it works the same locally and remotely.
  - `K` opens Devices (remote only): list, `a` add (name + pasted key), `x` remove (confirm `y`).
  - Header: `connected to <name>` in the connection colour with `https`/`http` and the login method; `connecting...`, `connection lost, retrying in <n>s` in amber.
  - Passphrase prompt: when the key file is encrypted and no agent is used, a masked prompt before connecting.
- [ ] **Step 1: Failing tests:** `connect_screen_builds_request`, `http_password_warning_needs_second_enter`, `folder_picker_navigates_and_selects`, `devices_add_and_remove_send_commands`, `header_shows_lost_link`, plus the milestone-1 suites updated to `Action`.
- [ ] **Step 2-4:** run, implement, run the full gate.
- [ ] **Step 5: Commit** `feat(tui): connect screen, folder picker and devices`.

### Task 8: Binary: switching backends, last used, local server hand-off, keychain

**Files:** Modify `crates/featherbittorrent/src/run.rs`; Create `crates/featherbittorrent/src/{backend.rs,secrets.rs}`

**Interfaces:**
- Consumes: everything above.
- Behaviour:
  - Start: `ui.yaml` says `Remote(target)` → start `RemoteBackend` (no engine, no lock; password from the keychain, or ask on the connect screen when missing). `Local` (default) → take the lock; if `Held` and the config has a `server` section, show `A featherbittorrent server is running on this library. Connect to it? y/n` and on `y` connect to `http://<server.bind>` (loopback) with the connect screen prefilled; on `n` or with no server section, exit 3 with the milestone-1 message.
  - `Action::Connect`: send `Shutdown` to the current backend, await it (the local engine saves and frees the lock), start the new one, save `ui.yaml`. Switching to `Local engine` takes the lock; held → error notice, stay disconnected from nothing (keep the previous backend if it is still running, i.e. do the lock check before shutting the current one down).
  - `secrets.rs`: `keyring` get/set/delete under service `featherbittorrent`, account `<user>@<url>`; a keychain that is missing or locked (`keyring::Error`) → notice `The system keychain is not available; the password will be asked each time.` and nothing is stored.
- [ ] **Step 1: Failing tests:** `ui_state_decides_start` (unit test over a pure `fn initial_target(ui: &UiState, lock: Result<..>, config: &Config) -> Start`), `lock_held_offers_local_server` (pure function), `switch_checks_lock_first` (pure function over a fake), and the existing CLI tests.
- [ ] **Step 2-4:** run, implement, run the full gate.
- [ ] **Step 5: Manual check (controller):** pty run: `serve` in one process on a scratch library with a user, the TUI on another scratch config connects with a password, adds a magnet, sees it live, switches to the local engine (a second scratch library) and back, quits; then the lock hand-off on one library.
- [ ] **Step 6: Commit** `feat: connect to servers, remember the last connection`.

### Task 9: Release workflow and docs

**Files:** Create `.github/workflows/release.yml`, `dev/release-version.sh`, `dev/test-release-version.sh`, `docs/server.md`, `docs/reverse-proxy.md`; Modify `README.md`, `CHANGELOG.md`, `CLAUDE.md`

- Release workflow (adapt `../fbt-lib/.github/workflows/release.yml` and `../terminal/.github/workflows/release.yml`, and `dev/release-version.sh` from fbt-lib): on a `v*` tag, refuse a tag that is not `v<workspace version>`, build `featherbittorrent` for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (native arm runner), `x86_64-apple-darwin` + `aarch64-apple-darwin` (universal binary with `lipo`), `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`; archives `featherbittorrent-<version>-<target>.tar.gz` (zip on Windows) with `LICENSE`, `NOTICE`, `README.md`; CycloneDX SBOM via syft (pinned as in fbt-lib); GitHub release with the changelog section as notes. `.deb`/`.rpm` are milestone 4.
- `docs/server.md`: running `serve`, the wizard, the YAML (every field, from the code), users and devices, auth on/off rules, the lock and the TUI hand-off.
- `docs/reverse-proxy.md` (owner, 2026-11-15: on untrusted networks we recommend the featherbit gateway or another API gateway, for throttling and further authentication): three setups, featherbittorrent login only (gateway for TLS and rate limits), both, and gateway only (featherbittorrent auth off, bound to loopback or an internal network, `server.allowed_hosts` listing the gateway's domain). An example `docs/examples/gateway-oidc/compose.yaml` with the gateway in front of `featherbittorrent serve`, `openid-connect` in bearer mode (JWKS) for the TUI's access tokens and the interactive mode for browsers, and `limit-count`. The OpenID Connect provider must support client credentials with `private_key_jwt` and Ed25519 (EdDSA) client keys, and be light enough for a Raspberry Pi 2B: pick and justify one from its documentation (memory use, `private_key_jwt`, EdDSA); if none supports EdDSA there, record that and fall back to P-256 (ES256) device keys for this method. The guide shows how to register a device's public key as a client. Also: putting the server behind the featherbit gateway (read `../gateway/website/docs` for the route config shape): TLS at the proxy, no proxy login, no response buffering for `/api/v1/events` and streams.
- [ ] Steps: write, `bash dev/test-release-version.sh`, `actionlint` if available, full gate, commit `ci: release workflow` and `docs: server and reverse proxy guides`.

### Task 10: Release 0.1.0 (controller)

- [ ] PR `feature/m2-server` → `develop`, merge on green. `release/0.1.0`: bump the workspace version to `0.1.0`, `## [0.1.0] - <date>` in `CHANGELOG.md` (milestones 1 and 2), PR into `main`, merge on green, annotated tag `v0.1.0`, watch the release workflow, merge `main` back into `develop`.
