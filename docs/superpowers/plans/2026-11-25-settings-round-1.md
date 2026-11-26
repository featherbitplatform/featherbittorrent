# Settings and Proxy, Round 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. The implementer step runs through the `parallel-implementers` skill (two lanes; Claude reviews every task).

**Goal:** Every surface (TUI, web UI, desktop app) can set the proxy, the Downloads settings and today's speed and queue settings without editing a file; every Torrents-section save applies to the running engine and is written to that engine's `config.yaml`; the web Settings page shows one section at a time and the TUI gets a full-screen Settings view.

**Architecture:** `fbtt-core` owns the data: the new `config.yaml` keys (`downloads:`, `proxy:`), a catch-all that keeps unknown keys, one `SettingsView` value that every surface reads, and the `downloads` automation (unfinished-folder move, watch folder, run a program) that both the local backend and the server spawn. `fbtt-server` gets product routes under `/api/v1/settings` that validate, write `config.yaml` atomically under one lock, then apply. The local and remote backends answer a new `Command::Settings`, so the TUI talks to its own engine and to a server the same way. Every call into fbt-lib's v0.13.0 proxy surface goes through one adapter file, `crates/fbtt-core/src/proxy/engine.rs`, so a renamed engine item is reconciled there only.

**Tech Stack:** Rust 2024 (1.99), fbt / fbt-api (v0.12.0, then v0.13.0), axum (through fbt-api), ratatui; React 19, Vite, Tailwind 4, vitest in `web/`; Tauri 2 and `keyring` in the desktop crate.

**Spec:** `docs/superpowers/specs/2026-11-25-settings-and-proxy-design.md` (round 1: sections 3 app side, 4, 5, 6, 7). Rounds 2 to 4 (section 8) are not in this plan.

## Engine interface (fbt-lib v0.13.0)

These names come from the fbt-lib plan `fbt-lib/docs/superpowers/plans/2026-11-25-proxy-runtime-0.13.md`
(Tasks 4 to 6). If the released tag differs, reconcile **in `crates/fbtt-core/src/proxy/engine.rs` only**
(Task B2 creates it); no other file names an fbt proxy item other than `fbt::ProxyConfig`,
`fbt::ProxyKind` and `fbt::NetworkSettings`.

```rust
// F1: one runtime call for the proxy and the switches section 3.3 turns off. A change closes
// connections made under the old proxy; nothing connects directly in between. Equal value: no-op.
#[non_exhaustive] pub struct NetworkSettings { pub proxy: ProxyConfig, pub dht_enabled: bool,
    pub enable_lsd: bool, pub enable_upnp: bool, pub enable_natpmp: bool, pub enable_incoming_tcp: bool,
    pub enable_incoming_utp: bool, pub enable_outgoing_utp: bool }
impl Settings { pub fn network(&self) -> NetworkSettings; pub fn set_network(&mut self, net: NetworkSettings); }
impl Session {
    pub async fn set_network_settings(&self, net: NetworkSettings) -> Result<(), SessionError>;
    pub fn network_settings(&self) -> NetworkSettings;
    pub fn proxy_state(&self) -> ProxyState;                                          // F5
}
// ProxyConfig gains `proxy_only: bool`: with a proxy set, the engine itself treats every bypass as off
// (incoming, LSD, UPnP, NAT-PMP; with HTTP also DHT and uTP) whatever the switches say. featherbittorrent
// always sets it with a proxy, and also turns the switches off itself (spec 3.3), so either one alone holds.
#[non_exhaustive] pub enum ProxyState { NotSet, Reachable, UdpNotRelayed, Unreachable { reason: String }, LoginRefused }
Event::ProxyStateChanged { state: ProxyState }   // EventMask::STATUS, only on change; fbt-api event kind `proxy_state_changed`
// F2: Session::new returns Ok when the proxy is down or does not relay UDP; proxy_state() says which.
// F4: no surface here. UDP tracker names go to the SOCKS5 relay by name; DHT bootstrap names are skipped under a proxy.
// UDP trackers under an HTTP proxy fail inside fbt-lib (no switch needed).
```

## Global Constraints

- Everything in the repo's `CLAUDE.md` holds: edition 2024, `rust-version = "1.99"`, `#![forbid(unsafe_code)]`; dependencies only in `[workspace.dependencies]` (this plan adds none); no source file headers; Conventional Commits with the crate as scope; no attribution trailers or session links in commits or PR bodies.
- **Tests never contact the internet or a real proxy.** Test `Settings` keep DHT, LSD, UPnP, NAT-PMP off and `listen_addrs` on `127.0.0.1:0`. A proxy in a test is a listener on `127.0.0.1:0` inside the test (a closed port for "unreachable", a listener that answers SOCKS5 auth failure for "login refused"), or no proxy at all with the adapter's health stubbed.
- Remote text through `fbt::display_safe(s, fbt::MAX_DISPLAY_CHARS)`: torrent names in messages, proxy reasons, file names from the watch folder.
- The UI thread never awaits the network or the disk: the TUI screen sends `Command`s and reads `Snapshot`s.
- Product copy: no emoji, no em-dashes. Exact strings fixed by this plan:
  - section titles `Downloads`, `Speed`, `Queue` (sub-heading inside Speed), `Proxy`, `Devices`, `Keyboard shortcuts`, `Window`, `Serve the web UI`, `Instances`; group titles `Torrents`, `Access`, `This app`; `Back to torrents`; `Jump to`;
  - field labels `Default folder`, `Unfinished folder`, `Start torrents paused`, `Watch folder`, `Run a program on completion`, `Download limit KiB/s`, `Upload limit KiB/s`, `Active downloads`, `Active seeds`, `Active total`, `Proxy type`, `None`, `HTTP`, `SOCKS5`, `Host`, `Port`, `User name`, `Password`;
  - `Save`, `Saved.`, `Set in the server's config file`, `You have unsaved changes in <Section>.`, `Save changes`, `Discard changes`;
  - `Can't reach the proxy` (followed by `: <reason>` when there is one), `Can't reach the proxy: it refused the login`, `This proxy does not relay UDP, so the DHT and UDP trackers are off`, `While a proxy is set, every connection goes through it. Incoming connections, port forwarding and local peer discovery are off, so you may see fewer peers.`, `An HTTP proxy carries TCP only, so the DHT, uTP and UDP trackers are off too.`, `saved` (the password field placeholder when one is stored), `Clear password`, `This system has no keychain, so the proxy password is kept in config.yaml.`;
  - refusals `Choose a folder inside the server's allowed folders.`, `The unfinished folder must be different from the default folder.`, `The watch folder must be different from the download folders.`, `Name the program by its full path.`, `No program at that path.`, `Name a program, not a batch file.` (Windows), `Enter the proxy's host name or address.`, `The port must be a number from 1 to 65535.`, `The user name and password can be at most 255 bytes each.`, `A SOCKS5 login needs both a user name and a password.`, `A proxy login needs a user name.`, `The user name cannot contain a colon.`, `Write the proxy setting as proxy: in lower case.`, `Write the proxy: section out in full; a YAML merge (<<) cannot carry it.`, `The proxy setting is not valid: <engine message>`, `This server is too old to change its settings here. Update it to change them.`
- `config.yaml`: no existing key is renamed, removed or given a new default. New keys only: `downloads.{unfinished_dir,start_paused,watch_dir,on_complete}` and `proxy.{type,host,port,username,password}` (`type: none | http | socks5`). Unknown keys survive a save; comments do not, and the docs say so.
- Run a program (owner's rule): settable from the TUI on its own engine and from the desktop app for This computer only (the request carries the launch secret as a bearer header). A server reads it only from `config.yaml`. The program gets the torrent's name and folder as two separate arguments through `std::process::Command`, never a shell; it is not waited for.
- `CHANGELOG.md`: each task that ships a user-visible change adds its line under `## [Unreleased]` ending in `([#NN](https://github.com/featherbitplatform/featherbittorrent/pull/NN))`; the PR number replaces `NN` when the PR exists.
- Docs guides live in `docs/*.md`; links out of `docs/` are absolute `https://github.com/featherbitplatform/featherbittorrent/blob/develop/...` URLs.

## Lanes and order

Two implementers, two lanes, different files at any one time.

| Task | Lane | Depends on |
|---|---|---|
| A1 Config: unknown keys, `downloads:` keys, `SettingsView` | A | none |
| A2 Downloads automation in `fbtt-core` | A | A1 |
| A3 Server product settings routes and persistence | A | A1, A2 |
| A4 Backends: `Command::Settings` local and remote | A | A3 |
| A5 Web: Settings page layout B and the Speed section | A | A3 |
| A6 Web: Downloads section and Add dialog defaults | A | A5 |
| A7 TUI: full-screen Settings | A | A4 |
| B1 Move fbt and fbt-api to v0.13.0 | B | the v0.13.0 tag exists |
| B2 Proxy config, derived engine settings, start without a proxy | B | B1, A1 |
| B3 Proxy apply at runtime: route and backends | B | B2, A3, A4 |
| B4 Desktop: proxy password in the keychain | B | B3 |
| B5 Proxy health on all three surfaces | B | B3, A5, A7 |
| B6 Proxy section in the web UI and the TUI, proxy docs | B | B4, B5, A6 |

A5 and A4 can run at the same time (web files vs Rust backend files); A6 and A7 likewise. Lane B starts B1 as soon as the tag exists and B2 once A1 is merged; B3 waits for A4.

## Review Focus

1. **A `proxy:` section the app cannot use** (a typo in `type`, `socks4`, an empty host, or a YAML error elsewhere in a file that has `proxy:`). Today an unparsable file is renamed to `.bad` and the run continues on defaults, which would connect directly: an address leak. Expected: the TUI and `serve` refuse to start with one line naming the problem; the desktop app shows its startup problem. Test: Task B2 `invalid_proxy_refuses_to_start` and `unparsable_file_with_a_proxy_key_refuses_to_start`.
2. **Changing the Default folder on a server whose `allowed_roots` is empty** (empty means "the download folder"): the next start would quietly narrow or move the allowed folders. Expected: the route pins `allowed_roots` to the roots this run serves before it writes the new `download_dir`. Test: Task A3 `changing_default_folder_pins_empty_roots`.
3. **Two saves at once, or a save racing a device add**, each reading and rewriting `config.yaml`: one would lose the other's change. Expected: one lock for every product write. Test: Task A3 `concurrent_section_saves_keep_both`.
4. **A watch folder receiving a file still being written, a file that is not a torrent, a torrent already added, or a `.torrent.added` name already taken.** Expected: no early read, `.invalid` with a reason, `.added` without a second copy, a free name chosen; never a deletion. Test: Task A2 `watch_folder_*` tests.
5. **A desktop app or TUI on 0.5 talking to a 0.4.x server** that has no `/api/v1/settings` (`404`). Expected: the Torrents sections say the server is too old; Speed keeps working through the engine routes as today; nothing crashes or spins. Test: Task A4 `old_server_settings_are_unsupported_and_limits_still_save`, Task A5 `old_server_shows_too_old_and_speed_still_saves`.

---

### Task A1: Config keeps unknown keys; the `downloads:` keys; `SettingsView`

**Files:**
- Modify: `crates/fbtt-core/src/config.rs` (`Config` ~380, `Default`/`with_download_dir` ~405-470, tests)
- Create: `crates/fbtt-core/src/settings.rs`
- Modify: `crates/fbtt-core/src/lib.rs` (module and re-exports)
- Create: `crates/fbtt-core/tests/fixtures/config-0.4.1.yaml` (a full 0.4.1 file: every key, a `server:` section with one user and one device)
- Modify: `docs/server.md` ("The config file": the `downloads:` keys, unknown keys kept, comments not kept), `README.md:145` (same correction)

**Interfaces:**
- Produces in `config.rs`:
  ```rust
  #[serde(default)]
  pub struct DownloadsConfig {                       // YAML `downloads:`
      #[serde(skip_serializing_if = "Option::is_none")] pub unfinished_dir: Option<PathBuf>,
      #[serde(skip_serializing_if = "std::ops::Not::not")] pub start_paused: bool,
      #[serde(skip_serializing_if = "Option::is_none")] pub watch_dir: Option<PathBuf>,
      #[serde(skip_serializing_if = "Option::is_none")] pub on_complete: Option<PathBuf>,
  }
  // Config gains:
  #[serde(skip_serializing_if = "DownloadsConfig::is_default")] pub downloads: DownloadsConfig,
  #[serde(flatten)] pub unknown_keys: BTreeMap<String, serde_norway::Value>,
  ```
  `Config::resolve` makes `unfinished_dir` and `watch_dir` absolute with `absolute_dir` (as `download_dir`); `on_complete` is left as written.
- Produces in `settings.rs` (Serialize + Deserialize, snake_case; this is also the wire shape of Task A3):
  ```rust
  pub struct SettingsView { pub speed: LiveLimits, pub downloads: DownloadsView }   // B3 adds `pub proxy: ProxyView`
  pub struct DownloadsView {
      pub default_dir: PathBuf, pub unfinished_dir: Option<PathBuf>, pub start_paused: bool,
      pub watch_dir: Option<PathBuf>, pub on_complete: OnCompleteView,
      pub watch_problem: Option<String>, pub on_complete_problem: Option<String>,   // latest one, display-safe
  }
  pub struct OnCompleteView { pub program: Option<PathBuf>, pub editable: bool }
  pub struct DownloadsChange {
      pub default_dir: PathBuf, pub unfinished_dir: Option<PathBuf>, pub start_paused: bool,
      pub watch_dir: Option<PathBuf>,
      #[serde(default, skip_serializing_if = "Option::is_none")] pub on_complete: Option<OnCompleteSet>,  // absent = keep
  }
  pub struct OnCompleteSet { pub program: Option<PathBuf> }                          // null = clear
  pub enum SettingsProblem { OutsideRoots, UnfinishedIsDefault, WatchIsDownloads, ProgramNotAbsolute, NoProgram, BatchFile, NotEditable }
  impl SettingsProblem { pub fn message(&self) -> &'static str }                     // exact refusals from Global Constraints; NotEditable = "Set in the server's config file"
  pub fn validate_downloads(change: &DownloadsChange, roots: Option<&[PathBuf]>) -> Result<(), SettingsProblem>
  impl DownloadsConfig { pub fn apply(&mut self, change: &DownloadsChange) }         // on_complete only when Some
  ```
  `LiveLimits` gains `Serialize, Deserialize`. `validate_downloads` checks: every folder absolute; `unfinished_dir != default_dir`; `watch_dir` differs from both; with `roots = Some(..)` each folder is inside one root (lexically, after `..` normalisation; the server adds its canonical check in A3); a program, when set, is absolute and an existing file, and on Windows not `.bat` or `.cmd` (case-insensitive).

- [ ] **Step 1: Write the failing tests** in `config.rs` tests and `settings.rs` tests:
  ```rust
  #[test] fn config_0_4_1_reads_unchanged() {        // fixture parses; every field equals what 0.4.1 read; downloads is default; unknown_keys empty
      let config: Config = serde_norway::from_str(FIXTURE_0_4_1).unwrap();
      assert_eq!(config.downloads, DownloadsConfig::default());
      assert!(config.unknown_keys.is_empty());
  }
  #[test] fn unknown_keys_survive_a_save() {           // file with `seeding: {ratio: 2}` and `future_key: 1`
      // load_for_edit, save, load_for_edit again
      assert_eq!(again.unknown_keys.get("future_key"), Some(&Value::Number(1.into())));
      assert!(matches!(again.unknown_keys.get("seeding"), Some(Value::Mapping(_))));
  }
  #[test] fn default_downloads_write_no_downloads_key() { assert!(!saved_text.contains("downloads")); }
  #[test] fn downloads_round_trip() { /* all four keys set, save, load: equal */ }
  #[test] fn resolve_roots_relative_unfinished_and_watch_dirs() { assert_eq!(c.downloads.watch_dir, Some(data_dir.join("watch"))); }
  #[test] fn validate_refuses_each_problem() {         // table: one case per SettingsProblem variant except NotEditable
      assert_eq!(validate_downloads(&same_as_default, None), Err(SettingsProblem::UnfinishedIsDefault));
      assert_eq!(validate_downloads(&outside, Some(&[root])), Err(SettingsProblem::OutsideRoots));
      assert_eq!(validate_downloads(&dotdot_escape, Some(&[root])), Err(SettingsProblem::OutsideRoots)); // root/../etc
  }
  #[cfg(windows)] #[test] fn validate_refuses_batch_files() { assert_eq!(.., Err(SettingsProblem::BatchFile)); }
  #[test] fn change_without_on_complete_keeps_it() { cfg.apply(&change_none); assert_eq!(cfg.on_complete, Some(old)); }
  #[test] fn settings_view_json_shape() {               // serde_json of a sample view
      assert_eq!(json["downloads"]["on_complete"]["editable"], false);
      assert_eq!(json["speed"]["download_kib"], 100);
  }
  ```
- [ ] **Step 2: Run** `cargo test -p fbtt-core --all-features --lib` → FAIL (no `downloads`, no `settings` module).
- [ ] **Step 3: Implement** the field, the flattened map (the pattern of `fbtt-desktop-core/src/settings.rs:152`), `DownloadsConfig`, `settings.rs`, the `LiveLimits` derives; update every `Config { .. }` literal.
- [ ] **Step 4: Run** `cargo test --workspace --all-features` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` → PASS.
- [ ] **Step 5: Docs.** `docs/server.md` "The config file": the four `downloads:` keys with one line each; "Keys this version does not know are kept when the app rewrites the file. Comments are not kept."; fix `README.md:145` the same way. CHANGELOG: "Saving settings keeps keys in `config.yaml` that this version does not know about."
- [ ] **Step 6: Commit** `feat(core): keep unknown config keys and add the downloads settings`.

### Task A2: Downloads automation (unfinished folder, watch folder, run a program, start paused)

**Files:**
- Create: `crates/fbtt-core/src/downloads/mod.rs` (`Automation`, `AutomationHandle`), `downloads/watch.rs` (the folder scan, pure decisions), `downloads/program.rs` (spawning)
- Modify: `crates/fbtt-core/src/local.rs` (`spawn` ~112 starts the automation; `add` ~431 honours `start_paused`), `crates/fbtt-core/src/lib.rs`
- Modify: `crates/fbtt-server/src/server.rs` (`build` ~200: start the automation on the engine's session, notices to the reports channel)
- Create: `crates/fbtt-core/tests/downloads.rs`; Create: `crates/fbtt-core/tests/support/mod.rs` (move `within`, `loopback`, `sample`, `write_file`, `torrent_for`, `wait_until_seeding`, `seeding_library` out of `tests/local.rs` and use them from both)
- Modify: `docs/server.md`, `docs/desktop.md` (what each `downloads:` key does)

**Interfaces:**
- Consumes: `DownloadsConfig`, `DownloadsView` (A1).
- Produces:
  ```rust
  pub struct Automation;
  impl Automation {
      /// Subscribes to `EventMask::STATUS` on `session`, scans `watch_dir` every WATCH_EVERY, runs the program.
      pub fn spawn(session: fbt::Session, default_dir: PathBuf, config: DownloadsConfig, editable: bool,
                   notices: mpsc::Sender<Notice>) -> AutomationHandle;
  }
  #[derive(Clone)] pub struct AutomationHandle { .. }
  impl AutomationHandle {
      pub fn update(&self, default_dir: PathBuf, config: DownloadsConfig);   // takes effect on the next event or scan
      pub fn problems(&self) -> (Option<String>, Option<String>);           // (watch, on_complete), latest each
  }
  pub const WATCH_EVERY: Duration = Duration::from_secs(2);
  pub const MAX_WATCHED_FILE: u64 = 16 * 1024 * 1024;
  pub(crate) fn settled(prev: Option<(u64, SystemTime)>, now: (u64, SystemTime)) -> bool;   // same size and mtime on two scans
  pub(crate) fn free_name(dir: &Path, file: &str, suffix: &str) -> PathBuf; // name.torrent.added, then name.torrent.added.2, .3 ... up to .999
  pub(crate) fn completion_args(name: &str, folder: &Path) -> [OsString; 2];
  ```
- Behaviour (builder's rulings, recorded in docs):
  - **Unfinished folder** applies to torrents saved in the Default folder: on `TorrentAdded` with `save_path == default_dir`, not yet finished, and `unfinished_dir` set, the torrent is moved there (`move_storage`, `MoveMode::FailIfExists`); on `TorrentFinished` with `save_path == unfinished_dir` it is moved to the current `default_dir`. A torrent added to a folder chosen in the Add dialog downloads in place. A failed move is a notice and the torrent stays where it is.
  - **Run a program** on `TorrentFinished`, after the unfinished move (if any) completes, with the final folder: `Command::new(program).args(completion_args(..))`, stdin, stdout and stderr null, spawned and reaped on a `std::thread` so no zombie remains; not waited for by the engine. A failure to start is `Notice::Error("Could not start the program run on completion: <reason>")` and is the `on_complete_problem`. One run per `TorrentFinished` event.
  - **Watch folder**: only top-level regular files whose name ends `.torrent` (case-insensitive), not hidden; read only when `settled`; larger than `MAX_WATCHED_FILE` or not a torrent → renamed with suffix `.invalid`, reason as a notice and as `watch_problem`; added (to `default_dir`, paused per `start_paused`) or already in the session → suffix `.added`. Never deleted.
  - **Start paused**: the local backend's `add` and the watch folder pass `paused: config.downloads.start_paused` to `AddTorrent`. (The web Add dialog sends its own choice; A6 defaults it.)
  - The server passes `editable = false` (it only reads the program from the file) and the local backend `true`; the desktop's This computer passes `true` (A3 wires `DesktopAccess`). `editable` only feeds `DownloadsView`; the automation runs whatever the config says.

- [ ] **Step 1: Write failing unit tests** in `downloads/watch.rs` and `program.rs`:
  ```rust
  #[test] fn settled_needs_two_equal_scans() { assert!(!settled(None, a)); assert!(settled(Some(a), a)); assert!(!settled(Some(a), grown)); }
  #[test] fn free_name_skips_taken_names() { /* x.torrent.added exists */ assert_eq!(free_name(d, "x.torrent", ".added"), d.join("x.torrent.added.2")); }
  #[test] fn completion_args_are_two_separate_arguments() {
      let args = completion_args("a b; $(rm -rf ~)", Path::new("/d l"));
      assert_eq!(args, [OsString::from("a b; $(rm -rf ~)"), OsString::from("/d l")]);
  }
  ```
- [ ] **Step 2: Write failing integration tests** in `tests/downloads.rs` (loopback seed and leecher as `tests/local.rs::downloads_from_local_seed` does; the program is `fbtt-fake-player`, whose log records its arguments):
  - `unfinished_then_moved_to_default_on_finish`: assert the file is under `unfinished_dir` while downloading and under `default_dir` after, and absent from `unfinished_dir`.
  - `chosen_folder_is_not_moved`: added with `dir: Some(other)` → stays in `other`.
  - `program_gets_name_and_final_folder`: fake-player log line's args `== [name, default_dir]` (after the move), exactly one line.
  - `program_that_does_not_exist_is_a_notice`: `Notice::Error` starting `Could not start the program run on completion` and `problems().1.is_some()`.
  - `watch_folder_adds_and_renames_to_added`: drop a `.torrent` → torrent appears, file is now `<name>.torrent.added`, original gone.
  - `watch_folder_waits_for_a_growing_file`: write half the bytes, assert not added after one scan, write the rest, assert added.
  - `watch_folder_marks_garbage_invalid`: `junk.torrent` with random bytes → `junk.torrent.invalid`, notice names `junk.torrent`, nothing added.
  - `watch_folder_duplicate_is_added_once`: same torrent twice under two names → one torrent in the session, both files `.added`.
  - `watch_folder_respects_start_paused`: `start_paused: true` → the row is `RowState::Paused`.
  - `local_add_respects_start_paused`.
  - `update_takes_effect_without_restart`: `handle.update(..)` with a new `watch_dir` → a file dropped there is added.
- [ ] **Step 3: Run** `cargo test -p fbtt-core --all-features --test downloads` and the unit tests → FAIL.
- [ ] **Step 4: Implement** the module; wire it into `local.rs` and `fbtt-server/src/server.rs` (`build`, after `engine.report_to`; keep the handle in `Shared` as `automation: AutomationHandle`).
- [ ] **Step 5: Run** `cargo test --workspace --all-features` and clippy → PASS.
- [ ] **Step 6: Docs and changelog.** `docs/server.md` and `docs/desktop.md`: each key, the `.added` and `.invalid` renames, the unfinished-folder rule (Default folder only), the program's two arguments and "never through a shell", and that a server reads the program only from `config.yaml` and needs a restart after it changes. CHANGELOG: "Unfinished folder, watch folder, start paused and run a program on completion (`downloads:` in `config.yaml`)."
- [ ] **Step 7: Commit** `feat(core): unfinished folder, watch folder and a program on completion`.

### Task A3: Server product settings routes, persisted to `config.yaml`

**Files:**
- Create: `crates/fbtt-server/src/routes/settings.rs`
- Modify: `crates/fbtt-server/src/routes/mod.rs` (`Shared`: rename `changing_devices` to `changing_config`, add `current: Mutex<Config>`, `automation`; add `Shared::write_config`; routes in `router` ~905), `routes/devices.rs` (`change` ~194 goes through `write_config`), `crates/fbtt-server/src/auth/mod.rs` (`carries_launch_secret`), `crates/fbtt-server/src/server.rs` (`DesktopAccess` gains nothing here; `build` passes `editable` = launch secret configured)
- Test: `crates/fbtt-server/tests/settings.rs`
- Modify: `docs/server.md` ("The API": the four routes), `docs/web-ui.md` (Settings now persists)

**Interfaces:**
- Consumes: `SettingsView`, `DownloadsChange`, `validate_downloads`, `SettingsProblem` (A1); `AutomationHandle` (A2).
- Produces routes (JSON, errors in the existing `{"error":{"code","message"}}` shape; behind `FbttAuth` and CSRF like every product route):
  - `GET /api/v1/settings` → `200 SettingsView`. `speed` from the live session (`engine::map::limits`); `downloads.on_complete.editable` = this request carries the launch secret.
  - `PUT /api/v1/settings/speed` body `LiveLimits` → `200 SettingsView`.
  - `PUT /api/v1/settings/downloads` body `DownloadsChange` → `200 SettingsView`; `400 invalid_setting` with `SettingsProblem::message()`; `403 outside_roots`; `403 set_in_config_file` when `on_complete` is present and the request does not carry the launch secret.
  - every PUT: `500 config_unreadable` when `config.yaml` cannot be read for edit (nothing written, nothing applied), as `devices.rs` does.
- Produces in `routes/mod.rs`: `async fn write_config(&self, change: impl FnOnce(&mut Config) + Send) -> Result<Config, RouteError>`: under `changing_config`, `Config::load_for_edit` (missing file → `base_config`), apply `change`, `save` (atomic temp-then-rename, as `Config::save` already is), store into `current`, return it. Order in every PUT: validate → `write_config` → apply to the engine (`set_rate_limit`, `set_queue_settings`, `automation.update`). A write failure applies nothing.
- Produces in `auth/mod.rs`: `pub(crate) fn carries_launch_secret(&self, headers: &HeaderMap) -> bool` (only `Authorization: Bearer <secret>`, constant-time; the query token never counts).
- Default folder rule: a `download_dir` change on a config whose file `server.allowed_roots` is empty first writes the roots this run serves (the given roots) into the file, so the allowed folders do not move.

- [ ] **Step 1: Write the failing tests** in `tests/settings.rs` (offline settings, scratch library, a user `alice`; cookie and bearer logins as in `tests/web_session.rs`):
  - `get_settings_reports_live_speed_and_downloads`: after `PUT /api/v1/session/limits` 100 KiB/s, `GET /api/v1/settings` → `speed.download_kib == 100`.
  - `put_speed_applies_and_survives_restart`: PUT speed 64/32 → engine `rate_limit` is 65536/32768; stop, start on the same library → `GET` shows 64/32.
  - `put_downloads_persists_and_keeps_users_devices_and_unknown_keys`: file has a user, a device and `future_key: 1` → after PUT, `load_for_edit` has all three unchanged.
  - `put_downloads_outside_roots_is_403` and `put_downloads_unfinished_equal_default_is_400` (message equals the Global Constraints string).
  - `run_a_program_from_a_password_session_is_403`: bearer session from `alice`, body with `on_complete: {program: <fake-player>}` → `403`, code `set_in_config_file`, file unchanged byte for byte.
  - `run_a_program_from_the_web_cookie_session_is_403`: same with cookie and CSRF.
  - `run_a_program_with_the_launch_secret_is_saved`: `start_desktop` (expose false) → `200`, file's `downloads.on_complete` is the path.
  - `exposed_desktop_password_user_cannot_set_the_program`: `start_desktop` with `expose: true`, `alice` → `403`; the launch secret → `200`.
  - `query_token_secret_does_not_count_as_launch_secret`: `?token=<secret>` on the PUT → `403` (or `401`), never `200`.
  - `get_reports_program_not_editable_without_the_secret`: `on_complete.editable == false` for `alice`, `true` with the secret; `program` shows the configured path in both.
  - `changing_default_folder_pins_empty_roots` (Review Focus 2): file with `allowed_roots: []`, change Default folder to a subfolder → file's `allowed_roots == [old_download_dir]`.
  - `concurrent_section_saves_keep_both` (Review Focus 3): speed PUT, downloads PUT and a device add started together → the file has all three.
  - `unreadable_config_is_500_and_applies_nothing`: file replaced by invalid YAML → `500`, the rate limit unchanged.
  - `engine_routes_still_work_and_do_not_persist`: `PUT /api/v1/session/limits` → `200`; file unchanged.
- [ ] **Step 2: Run** `cargo test -p fbtt-server --all-features --test settings` → FAIL.
- [ ] **Step 3: Implement** `routes/settings.rs`, `write_config`, the devices refactor, `carries_launch_secret`; the canonical inside-roots check reuses `fs`'s root comparison.
- [ ] **Step 4: Run** `cargo test --workspace --all-features` (including `--test openapi_snapshot`: product routes are not in fbt-api's document, so it must stay unchanged) and clippy → PASS.
- [ ] **Step 5: Docs and changelog.** `docs/server.md` "The API": the four routes, that saves write `config.yaml` atomically and keep unknown keys, that the engine's `/api/v1/session/limits` and `/queue` do not persist, and that the program is set only in the file. CHANGELOG (Fixed): "Speed limits and queue counts set from the web UI or the desktop app are now saved to the server's `config.yaml` and survive a restart."
- [ ] **Step 6: Commit** `feat(server): settings routes that apply and persist`.

### Task A4: Backends answer `Command::Settings`

**Files:**
- Modify: `crates/fbtt-core/src/client.rs` (`Command` ~371, `Snapshot` ~337), `crates/fbtt-core/src/local.rs` (`handle` ~387, `save_config` ~511 generalised), `crates/fbtt-core/src/remote/commands.rs`, `remote/backend.rs` (snapshot ~282), `remote/routes.rs` (paths), `crates/fbtt-core/src/lib.rs`
- Test: `crates/fbtt-core/tests/local.rs` (new tests), `crates/fbtt-core/src/remote/*` unit tests against a stub server as the existing remote tests do, `crates/fbtt-server/tests/remote.rs` (end to end against a real `Server`)

**Interfaces:**
- Consumes: `SettingsView`, `DownloadsChange`, `validate_downloads` (A1); the routes (A3); `AutomationHandle` (A2).
- Produces:
  ```rust
  pub enum SettingsCommand { Load, Save(SettingsChange) }
  pub enum SettingsChange { Speed(LiveLimits), Downloads(DownloadsChange) }        // B3 adds Proxy(ProxyChange)
  pub enum SettingsState { NotLoaded, Loaded(SettingsView), Unsupported }
  Command::Settings(SettingsCommand)
  Snapshot.settings: SettingsState
  ```
  Local: `Load` fills `Loaded` with `on_complete.editable = true`; `Save` validates with `roots = None`, applies (session, automation, `default_dir`), writes `config.yaml` through the generalised `save_config(change: impl FnOnce(&mut Config))`, publishes the new view and `Notice::Info("Saved.")`; a validation failure is `Notice::Error(problem.message())` and nothing changes. Remote: `Load` is `GET /api/v1/settings` (`404` → `Unsupported`); `Save` is the matching `PUT`, the answer becomes `Loaded`, then `Notice::Info("Saved.")`; an error answer is `Notice::Error(<message>)`. `Command::SetLimits` stays (the Speed path to an old server).

- [ ] **Step 1: Write failing tests:**
  - `local_settings_load_and_save_speed_persist`: `Save(Speed)` → `Snapshot.settings` speed updated, `Notice::Info("Saved.")`, file `limits` updated.
  - `local_downloads_save_changes_default_dir_for_new_adds`: after `Save(Downloads)` with a new default, `Snapshot.default_dir` is it and an `Add { dir: None }` lands there.
  - `local_program_is_editable`: `Loaded(v)` with `v.downloads.on_complete.editable == true`.
  - `local_invalid_downloads_is_an_error_and_changes_nothing`.
  - `remote_settings_round_trip` (in `fbtt-server/tests/remote.rs`): `Load` then `Save(Speed)` against a real server → `Loaded` with new values; the server's file updated.
  - `remote_program_is_not_editable`: `editable == false`; a `Save(Downloads)` from the remote backend never sends `on_complete` (assert the stub saw no `on_complete` key).
  - `old_server_settings_are_unsupported_and_limits_still_save` (Review Focus 5): stub answers `404` on `/api/v1/settings` → `Unsupported`; `SetLimits` still PUTs `/api/v1/session/limits`.
- [ ] **Step 2: Run** `cargo test -p fbtt-core --all-features` and `cargo test -p fbtt-server --all-features --test remote` → FAIL.
- [ ] **Step 3: Implement.** The remote `DownloadsChange` is built with `on_complete: None` always.
- [ ] **Step 4: Run** `cargo test --workspace --all-features` and clippy → PASS.
- [ ] **Step 5: Commit** `feat(core): settings commands for the local and remote backends`.

### Task A5: Web Settings page, layout B, and the Speed section

**Files:**
- Modify: `web/src/screens/Settings.tsx` (rewritten: list on the left, one section on the right)
- Create: `web/src/components/settings/sections.ts`, `SectionList.tsx`, `UnsavedPrompt.tsx`, `useSectionDirty.ts`, `SpeedSection.tsx` (+ `.test.tsx` each where listed)
- Delete: `web/src/components/settings/ServerSettings.tsx` and its test (SpeedSection replaces it and keeps its fallback)
- Modify: `web/src/api/client.ts` (`settings()`, `saveSpeed()`, `saveDownloads()` and the types, hand-written beside `devices()` since product routes are not in `openapi.json`)
- Modify: `web/src/desktopSettings.ts` (slot carries sections), `web/src/desktop/DesktopSettings.tsx` (split into `WindowSection`, `ServingSection`, `InstancesSection`; editable `Shortcuts` moves to the Access group), `web/src/desktop/DesktopRoot.tsx` (`askSettings` ids keep working), `web/src/App.tsx` (passes `section`)
- Modify: `docs/web-ui.md` (## Settings rewritten), `docs/desktop.md` (sections moved)

**Interfaces:**
- Consumes: the routes (A3).
- Produces in `sections.ts`:
  ```ts
  export type SectionId = 'downloads' | 'speed' | 'proxy' | 'devices' | 'shortcuts' | 'window' | 'serving' | 'instances'
  export type Group = 'Torrents' | 'Access' | 'This app'
  export interface SectionDef { id: SectionId; title: string; group: Group }
  export function visibleSections(opts: { desktop: boolean; built: ReadonlySet<SectionId> }): SectionDef[]  // order of spec section 6
  export function resolveSection(id: string | null, visible: SectionDef[]): SectionId                     // legacy ids below, else last used, else 'downloads'
  export const LEGACY_IDS: Record<string, SectionId> = {
    'settings-server': 'speed', 'settings-devices': 'devices', 'settings-shortcuts': 'shortcuts',
    'settings-desktop': 'window', 'settings-desktop-serving': 'serving', 'settings-desktop-instances': 'instances' }
  export const LAST_SECTION_KEY = 'fbtt.settings.section'   // localStorage, read and written in try/catch
  ```
  `built` holds the sections that exist: A5 ships `speed`, `devices`, `shortcuts`, the three desktop ones; A6 adds `downloads`; B6 adds `proxy`. Nothing unbuilt is listed.
- Produces in `useSectionDirty.ts`: `useSectionDirty(dirty: boolean, save: () => Promise<boolean>): void` registering the open section's state with the page; the page asks before switching section, pressing `Back to torrents`, or `beforeunload`.
- `UnsavedPrompt`: `You have unsaved changes in <Section>.` with `Save changes` and `Discard changes`; Escape closes it and stays.
- `SectionList`: a `nav` with one button per section under group headings, `aria-current="page"` on the open one, roving tabindex, ArrowUp/ArrowDown move and open, Home/End. Below the `sm` breakpoint a `<select aria-label="Jump to">` replaces it. On open, the section's `h2` takes focus.
- `DesktopSettingsSlot` becomes `{ sections: { id: 'window' | 'serving' | 'instances'; render(): ReactNode }[]; shortcuts: ReactNode; request; onOpened }`; `DESKTOP_SECTION`, `SERVING_SECTION`, `INSTANCES_SECTION` keep their values and resolve through `LEGACY_IDS`.
- `SpeedSection`: the five fields (rates, then the `Queue` sub-heading with the three counts) and one `Save` calling `saveSpeed`; on `404` from `settings()` it shows `This server is too old to change its settings here. Update it to change them.` and saves through the engine routes as `ServerSettings` did.

- [ ] **Step 1: Write failing tests:**
  - `sections.test.ts`: `visible_sections_browser_has_no_this_app_group`; `visible_sections_desktop_order` (`['speed','devices','shortcuts','window','serving','instances']` with A5's `built`); `legacy_ids_resolve` (`resolveSection('settings-desktop-serving', v) === 'serving'`); `unbuilt_sections_are_not_listed` (`!ids.includes('proxy')`); `last_section_survives_storage_throwing` (a `localStorage` that throws → `'downloads'` fallback, then first visible).
  - `Settings.test.tsx`: `shows_one_section_at_a_time` (only the Speed fields in the document); `arrow_keys_move_and_focus_the_heading` (`document.activeElement` is the `Devices` `h2`); `jump_to_select_on_phone_width`; `back_to_torrents_is_always_present`; `leaving_with_unsaved_changes_asks` (edit Speed, click Devices → prompt text, `Discard changes` → Devices shown, value reverted); `save_changes_from_the_prompt_saves_then_moves`; `opens_on_last_used_section`.
  - `SpeedSection.test.tsx`: `save_puts_speed_route` (fetch saw `PUT /api/v1/settings/speed` with `{download_kib: 64, ...}`); `old_server_shows_too_old_and_speed_still_saves` (Review Focus 5: `404` → the exact line; Save PUTs `/api/v1/session/limits`).
  - `DesktopSettings.test.tsx` / `DesktopRoot.test.tsx`: `manage_instances_opens_the_instances_section`; `serving_link_opens_serving`.
- [ ] **Step 2: Run** `npm --prefix web test` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `npm --prefix web test`, `npm --prefix web run lint`, `npm --prefix web run typecheck`, `npm --prefix web run build`, `npm --prefix web run size` → PASS; then `cargo test -p featherbittorrent-desktop` (the shell embeds `web/dist`).
- [ ] **Step 5: Docs and changelog.** `docs/web-ui.md` ## Settings: the groups, one section at a time, per-section Save, the unsaved-changes prompt, `Jump to` on a phone; `docs/desktop.md`: Window, Serve the web UI, Instances and Keyboard shortcuts as their own sections. CHANGELOG (Changed): "Settings shows one section at a time with a section list, in the web UI and the desktop app."
- [ ] **Step 6: Commit** `feat(web): settings page with one section at a time`.

### Task A6: Web Downloads section and the Add dialog's defaults

**Files:**
- Create: `web/src/components/settings/DownloadsSection.tsx`, `DownloadsSection.test.tsx`
- Modify: `web/src/components/settings/sections.ts` (`built` gains `downloads`), `web/src/components/AddTorrent.tsx` (~146: `startPaused` and the picked folder start from `settings()`), `AddTorrent.test.tsx`
- Modify: `docs/web-ui.md`, `docs/desktop.md`

**Interfaces:**
- Consumes: `client.settings()`, `client.saveDownloads(change)` (A5), `FolderPicker` (existing).
- `DownloadsSection`: `Default folder`, `Unfinished folder` (with an off switch), `Start torrents paused`, `Watch folder` (with an off switch), all folders chosen with `FolderPicker` on the server's roots; `Run a program on completion` is a text field only when `on_complete.editable`, otherwise the configured path (or `None`) and `Set in the server's config file`. `watch_problem` and `on_complete_problem` show under their fields. Save sends `on_complete` only when editable and changed.

- [ ] **Step 1: Write failing tests:**
  - `downloads_section_shows_set_in_the_servers_config_file_when_not_editable`: no input labelled `Run a program on completion`; text `Set in the server's config file` present.
  - `not_editable_save_never_sends_on_complete`: the PUT body has no `on_complete` key.
  - `editable_program_is_sent`: desktop This computer view (`editable: true`) → body `on_complete: {program: '/usr/bin/x'}`.
  - `refusal_message_is_shown`: `400` with `The unfinished folder must be different from the default folder.` → that text visible, fields kept.
  - `watch_problem_is_shown`.
  - `add_dialog_defaults_from_settings`: `start_paused: true` → the dialog's checkbox starts checked; the picked folder starts at `default_dir`; the user can still uncheck.
  - `add_dialog_on_old_server_keeps_todays_defaults`: `settings()` `404` → unchecked, no crash.
- [ ] **Step 2: Run** `npm --prefix web test` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the web gate of A5 Step 4 → PASS.
- [ ] **Step 5: Docs and changelog.** `docs/web-ui.md` and `docs/desktop.md`: the Downloads section, where the program is set on a server and on This computer. CHANGELOG: "Downloads settings in the web UI and the desktop app: default and unfinished folders, start paused, a watch folder, and a program to run on completion (This computer only)."
- [ ] **Step 6: Commit** `feat(web): downloads settings`.

### Task A7: TUI full-screen Settings

**Files:**
- Create: `crates/fbtt-tui/src/settings/mod.rs` (`SettingsScreen` state and keys), `crates/fbtt-tui/src/view/settings.rs` (drawing)
- Delete: `crates/fbtt-tui/src/modal/settings.rs` (and `Modal::Settings`, `modal/mod.rs:64,91,109,196`)
- Modify: `crates/fbtt-tui/src/model.rs` (`Model.settings: Option<SettingsScreen>`), `keys.rs:149` (`S` opens the screen and sends `Command::Settings(SettingsCommand::Load)`), `view/mod.rs` (`draw_with` draws the screen instead of the table while open), `lib.rs`
- Test: `crates/fbtt-tui/tests/settings.rs`; existing `tests/dialogs.rs` cases for the old dialog move here
- Modify: `README.md:99,145` (the `S` key and what saving does)

**Interfaces:**
- Consumes: `SettingsCommand`, `SettingsChange`, `SettingsState`, `SettingsView` (A4).
- Produces:
  ```rust
  pub enum TuiSection { Speed, Downloads }            // B6 adds Proxy; order Downloads, Speed, Proxy as on the web
  pub struct SettingsScreen { /* section, focus: List|Fields, per-section form, dirty, confirm_leave */ }
  impl SettingsScreen {
      pub fn new(state: &SettingsState) -> SettingsScreen;
      pub(crate) fn on_key(&mut self, key: KeyEvent, snap: &Snapshot) -> ScreenAnswer;
      pub(crate) fn refresh(&mut self, state: &SettingsState);   // refills only sections that are not dirty
  }
  pub(crate) enum ScreenAnswer { Stay, Close, Send(Command) }
  ```
- Keys (builder's call, shown in the screen's footer): Tab switches list and fields; Up/Down move in the list or between fields; Space toggles `Start torrents paused`; Enter on a field edits it, Ctrl+S or the `Save` row saves the section; Esc goes back, and with unsaved changes first asks `You have unsaved changes in <Section>.` with `s` Save changes, `d` Discard changes, Esc keep editing. Switching section with unsaved changes asks the same.
- `Run a program on completion` is a field when `on_complete.editable`, otherwise a line with the path and `Set in the server's config file`. `SettingsState::Unsupported` shows the too-old line and keeps Speed working through `Command::SetLimits`. Devices stays on its own key and modal.

- [ ] **Step 1: Write failing tests** in `tests/settings.rs` (model and `TestBackend` render, as `tests/views.rs` does):
  - `s_opens_full_screen_and_loads`: actions contain `Command::Settings(SettingsCommand::Load)`; the rendered buffer has the section list and no torrent table header.
  - `tab_moves_between_list_and_fields`.
  - `save_speed_sends_the_change`: edit download to `64`, Ctrl+S → `Command::Settings(Save(SettingsChange::Speed(LiveLimits { download_kib: 64, .. })))`.
  - `esc_with_unsaved_changes_asks_then_discards`.
  - `remote_settings_show_set_in_server_config_file_for_run_a_program`: `editable: false` → buffer contains `Set in the server's config file`; no editable field for it; the Downloads save carries `on_complete: None`.
  - `local_program_is_editable_and_sent`.
  - `unsupported_server_shows_too_old_and_speed_uses_set_limits`.
  - `refresh_does_not_overwrite_a_dirty_form`.
- [ ] **Step 2: Run** `cargo test -p fbtt-tui` → FAIL.
- [ ] **Step 3: Implement**, colours only from `theme.rs`.
- [ ] **Step 4: Run** `cargo test --workspace --all-features` and clippy → PASS.
- [ ] **Step 5: Docs and changelog.** `README.md`: `S` opens Settings (Downloads, Speed); saving applies and writes `config.yaml`, connected to a server it writes the server's. CHANGELOG (Changed): "Terminal client: `S` opens a full-screen Settings view with Downloads and Speed."
- [ ] **Step 6: Commit** `feat(tui): full-screen settings`.

### Task B1: Move fbt and fbt-api to v0.13.0

**Files:**
- Modify: `Cargo.toml:58,61` (`tag = "v0.13.0"` on both), `Cargo.lock`, `CLAUDE.md:7,16` (the two `v0.12.0` mentions), `deny.toml` if it names the tag
- Modify: any call site v0.13.0 changed (compile errors decide; no behaviour change in this task)

**Interfaces:**
- Produces: the v0.13.0 API in the tree. Every later B task depends on this.

- [ ] **Step 1: Edit the pins and run** `cargo update -p fbt -p fbt-api` → `Cargo.lock` names `v0.13.0` for both (`grep -c 'tag=v0.13.0' Cargo.lock` ≥ 2, and no `v0.12.0`).
- [ ] **Step 2: Run** `cargo build --workspace --all-features`; fix only what v0.13.0 broke.
- [ ] **Step 3: Run** the full gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `cargo deny check`, `UPDATE_OPENAPI=1 cargo test -p fbtt-server --test openapi_snapshot` then `npm --prefix web run gen:api` if the document changed → PASS.
- [ ] **Step 4: Changelog.** "Built on fbt-lib 0.13.0."
- [ ] **Step 5: Commit** `chore: move fbt and fbt-api to v0.13.0`.

### Task B2: Proxy config, the engine settings it implies, and starting without the proxy

**Files:**
- Create: `crates/fbtt-core/src/proxy/mod.rs` (product types, validation, derived settings), `crates/fbtt-core/src/proxy/engine.rs` (the adapter: the only file naming v0.13.0 proxy items)
- Modify: `crates/fbtt-core/src/config.rs` (`Config.proxy`, `session_settings` ~609, `load` ~482 and `read_and_parse` ~652 for the fail-closed rules, `ConfigError`), `crates/fbtt-core/src/lib.rs`
- Modify: `crates/featherbittorrent/src/*` where a `ConfigError` is turned into the startup line (one more variant, same handling), `crates/featherbittorrent-desktop/src/local.rs` (`library_config` ~129: the same refusal becomes the startup problem)
- Test: unit tests in `proxy/mod.rs`, `config.rs`; `crates/fbtt-core/tests/proxy.rs`

**Interfaces:**
- Consumes: the names in "Engine interface (fbt-lib v0.13.0)".
- Produces in `proxy/mod.rs`:
  ```rust
  #[derive(Serialize, Deserialize)] #[serde(rename_all = "lowercase")]
  pub enum ProxyType { None, Http, Socks5 }
  pub struct ProxySetting {                              // YAML `proxy:`; Debug prints the password as "<redacted>"
      #[serde(rename = "type")] pub kind: ProxyType, pub host: String, pub port: u16,
      #[serde(skip_serializing_if = "Option::is_none")] pub username: Option<String>,
      #[serde(skip_serializing_if = "Option::is_none")] pub password: Option<String>,
  }
  pub enum ProxyProblem { NoHost, BadPort, TooLong, UnknownType(String), IncompleteLogin, LoginNeedsUser, UsernameColon, KeyCase, MergeKey, Engine(String) }
  impl ProxyProblem { pub fn message(&self) -> String }  // the Global Constraints refusals; UnknownType: "Unknown proxy type \"<t>\"; use none, http or socks5." (the word is quoted and display-safe, so an empty one reads as "")
  impl ProxySetting {
      pub fn validate(&self) -> Result<(), ProxyProblem>;    // host 1..=253 chars (one surrounding pair of brackets stripped first), no whitespace, no "://"; port 1..=65535; username and password <= 255 bytes; a SOCKS5 login is both halves or neither; an HTTP user name carries no ":"; the engine's own to_engine().validate() last
      pub fn is_set(&self) -> bool;                           // kind != None, and an empty type naming no host is not set
      pub fn to_engine(&self) -> fbt::ProxyConfig;            // proxy_peers, proxy_trackers, proxy_hostnames and proxy_only all true; the host loses its brackets; an empty login is no login
  }
  pub fn network_for(proxy: Option<&ProxySetting>, base: &fbt::Settings) -> fbt::NetworkSettings;
  pub fn apply_to_settings(settings: &mut fbt::Settings, proxy: Option<&ProxySetting>);
  ```
  `network_for` / `apply_to_settings` with a proxy set: `enable_incoming_tcp`, `enable_incoming_utp`, `enable_upnp`, `enable_natpmp`, `enable_lsd` off; with `Http` also `dht_enabled` and `enable_outgoing_utp` off; with `Socks5` those two keep `base`'s values (fbt-lib turns them off itself if UDP is not relayed, F2). With no proxy: `base`'s switches unchanged, and the engine's default proxy (not whatever `base` carried).
- Produces in `proxy/engine.rs`: `pub(crate) async fn apply_now(session: &fbt::Session, proxy: Option<&ProxySetting>, base: &fbt::Settings) -> Result<(), String>`, `pub fn health(session: &fbt::Session) -> ProxyHealth`, `pub fn health_from_event(event: &fbt::Event) -> Option<ProxyHealth>`.
- Produces: `#[serde(tag = "state", rename_all = "snake_case")] pub enum ProxyHealth { Off, Ok, Unreachable { reason: String }, LoginRefused, UdpNotRelayed }` (reason display-safe), with `pub fn headline(&self) -> Option<String>` returning the two `Can't reach the proxy` strings or `None`.
- `Config.proxy: Option<ProxySetting>` (`skip_serializing_if = "Option::is_none"`). `Config::session_settings` calls `apply_to_settings`. Fail-closed loading: `Reading::Parsed` with an invalid `proxy:` → `ConfigError::Proxy(String)` from `load` and `Err` from `load_for_edit`; a parsed file with no `proxy:` section but a top-level `proxy:` key that read as null (or a section whose indentation was lost) → the same `NoHost` refusal; `Reading::Unparsable` whose bytes name a proxy anywhere (a `proxy` substring on any non-comment line, ASCII case-insensitive, after a leading BOM is dropped) → `ConfigError::Proxy("<file> cannot be read and names a proxy, so nothing will connect until it is fixed: <parse error>")`, and the file is **not** renamed. `ProxySetting.kind` deserialises from a string so `socks4` reaches validation instead of failing the whole parse.

- [ ] **Step 1: Write failing unit tests:**
  ```rust
  #[test] fn proxy_round_trips_and_absent_writes_nothing() { /* type socks5 + all fields */ assert!(!default_saved.contains("proxy")); }
  #[test] fn debug_hides_the_password() { assert!(!format!("{p:?}").contains("hunter2")); }
  #[test] fn validate_table() { /* "" -> NoHost; "http://h" -> NoHost; port 0 -> BadPort; 256-byte password -> TooLong */ }
  #[test] fn socks5_turns_off_incoming_and_discovery() {
      let s = config_with(socks5).session_settings();
      assert!(!s.enable_incoming_tcp && !s.enable_incoming_utp && !s.enable_lsd && !s.enable_upnp && !s.enable_natpmp);
      assert_eq!(s.proxy.kind, fbt::ProxyKind::Socks5);
      assert!(s.proxy.proxy_peers && s.proxy.proxy_trackers && s.proxy.proxy_hostnames);
  }
  #[test] fn http_also_turns_off_dht_and_utp() { assert!(!s.dht_enabled && !s.enable_outgoing_utp); }
  #[test] fn no_proxy_is_todays_settings() { assert_eq!(Config::default().session_settings(), before_this_change()); } // compare field by field with a v0.12-era copy
  #[test] fn invalid_proxy_refuses_to_start() { assert!(matches!(Config::load(&paths), Err(ConfigError::Proxy(_)))); }       // Review Focus 1
  #[test] fn unknown_proxy_type_refuses_to_start() { /* type: socks4 */ }
  #[test] fn unparsable_file_with_a_proxy_key_refuses_to_start() { /* `proxy:` + a tab-indent error; file still there, no .bad */ }
  #[test] fn unparsable_file_without_proxy_is_still_set_aside() { /* today's .bad behaviour unchanged */ }
  ```
- [ ] **Step 2: Write failing integration tests** in `tests/proxy.rs` (loopback only):
  - `engine_starts_when_the_proxy_is_down` (F2): proxy `127.0.0.1:<closed port>` → `LocalBackend::start_for_test` is `Ok`, snapshot arrives, `health()` is `Unreachable { .. }` within the deadline.
  - `no_direct_connection_while_proxy_is_set` (F3, featherbittorrent side): a loopback seed listening on `127.0.0.1:<p>`, its address added as a peer to a backend whose proxy is a closed loopback port → after 3 s the seed's session has seen no incoming peer (`peers().is_empty()`).
  - `login_refused_is_reported`: a loopback listener that answers SOCKS5 greeting with username/password method and auth status `0x01` → `health()` is `LoginRefused`.
- [ ] **Step 3: Run** `cargo test -p fbtt-core --all-features` → FAIL.
- [ ] **Step 4: Implement**; the startup paths of the TUI, `serve` and the desktop app print or show the `ConfigError::Proxy` line and do not start the engine.
- [ ] **Step 5: Run** `cargo test --workspace --all-features` and clippy → PASS.
- [ ] **Step 6: Commit** `feat(core): proxy setting that turns off what would bypass it`.

### Task B3: Proxy applied at runtime: route and backends

**Files:**
- Modify: `crates/fbtt-core/src/settings.rs` (`SettingsView.proxy`, `ProxyChange`), `crates/fbtt-core/src/client.rs` (`SettingsChange::Proxy`), `local.rs`, `remote/commands.rs`
- Modify: `crates/fbtt-server/src/routes/settings.rs` (`PUT /api/v1/settings/proxy`), `routes/mod.rs` (`Shared.base_settings: fbt::Settings` kept for `network_for`)
- Test: `crates/fbtt-server/tests/settings.rs`, `crates/fbtt-core/tests/proxy.rs`

**Interfaces:**
- Consumes: `ProxySetting`, `apply_now`, `ProxyProblem` (B2); `write_config` (A3); `SettingsCommand` (A4).
- Produces:
  ```rust
  pub struct ProxyView { #[serde(rename = "type")] pub kind: ProxyType, pub host: String, pub port: u16,
                         pub username: Option<String>, pub password_saved: bool,
                         pub password_storage: PasswordStorage }        // never carries the password
  #[serde(rename_all = "snake_case")] pub enum PasswordStorage { ConfigFile, Keychain, Unavailable }
  pub struct ProxyChange { #[serde(rename = "type")] pub kind: ProxyType, pub host: String, pub port: u16,
                           pub username: Option<String>, pub password: PasswordChange }
  #[serde(rename_all = "snake_case")] pub enum PasswordChange { Keep, Clear, Set(String) }   // JSON: "keep" | "clear" | {"set": "..."}
  SettingsChange::Proxy(ProxyChange)
  ```
  `PUT /api/v1/settings/proxy` → validate (`400 invalid_setting` with `ProxyProblem::message()`) → `write_config` (`kind: None` removes the `proxy:` key) → `apply_now` → `200 SettingsView`. The local backend does the same through `save_config`. `password_storage` is `ConfigFile` for the CLI and a server (B4 sets `Keychain`/`Unavailable` on This computer).

- [ ] **Step 1: Write failing tests:**
  - `get_never_returns_the_password`: file has `password: hunter2` → response body text does not contain `hunter2`; `password_saved == true`.
  - `put_proxy_keep_preserves_the_stored_password`, `put_proxy_clear_removes_it`, `put_proxy_set_replaces_it` (file checked each time).
  - `put_proxy_none_removes_the_key_and_restores_switches`: after `None`, the file has no `proxy:`; `apply_now` was called with `None` (adapter test double records it).
  - `put_proxy_invalid_is_400_and_nothing_changes`.
  - `local_proxy_change_applies_without_restart` (`tests/proxy.rs`): set a closed-port proxy at runtime → `health()` moves from `Off` to `Unreachable` without restarting the backend; then `None` → `Off`.
  - `remote_proxy_save_round_trip` (`fbtt-server/tests/remote.rs`).
- [ ] **Step 2: Run** `cargo test -p fbtt-server --all-features --test settings --test remote` and `cargo test -p fbtt-core --all-features --test proxy` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace --all-features` and clippy → PASS.
- [ ] **Step 5: Commit** `feat(server): proxy setting applied without a restart`.

### Task B4: Desktop: the proxy password in the keychain

**Files:**
- Modify: `crates/fbtt-server/src/server.rs` (`DesktopAccess` ~89 gains `proxy_password: Option<Arc<dyn ProxyPasswordStore>>`; its `Debug` keeps `finish_non_exhaustive`), `crates/fbtt-server/src/routes/settings.rs` (store instead of file when present), `crates/fbtt-server/src/lib.rs` (re-export)
- Modify: `crates/featherbittorrent-desktop/src/keychain.rs` (an adapter implementing the trait on the existing store, account `proxy:this-computer` under `SERVICE`), `crates/featherbittorrent-desktop/src/local.rs` (`start_with` ~93: fill the password before `settings(&config)`; move a file password into the keychain)
- Test: `crates/fbtt-server/tests/desktop_access.rs`, `crates/featherbittorrent-desktop/src/local.rs` tests (with the in-memory keychain at `keychain.rs:123`)

**Interfaces:**
- Consumes: `PasswordChange`, `PasswordStorage` (B3).
- Produces in `fbtt-server`:
  ```rust
  pub trait ProxyPasswordStore: Send + Sync {
      fn get(&self) -> Option<String>;
      fn set(&self, password: &str) -> Result<(), String>;
      fn clear(&self);
      fn available(&self) -> bool;
  }
  ```
  With a store that is `available`: `PasswordChange::Set` goes to `set`, `Clear` to `clear`, and the file's `proxy.password` is removed; `password_saved = get().is_some()`; `password_storage = Keychain`. With no keychain (`available() == false`) the password is kept in `config.yaml` as for the CLI (spec 3.5: keychain where one exists, the config file otherwise) and `password_storage = Unavailable`, which the Proxy section explains with `This system has no keychain, so the proxy password is kept in config.yaml.`
- Desktop start: if the file has `proxy.password` and the keychain is available, it is moved into the keychain and removed from the file (one `Config::save`); the engine settings get the keychain's password.

- [ ] **Step 1: Write failing tests:**
  - `desktop_proxy_password_goes_to_the_store_not_the_file`: launch secret, `{"set":"hunter2"}` → in-memory store holds it; `config.yaml` text has no `hunter2` and no `password:`.
  - `desktop_without_keychain_keeps_the_password_in_the_file`: store unavailable → `200`, the file has the password, `password_storage == "unavailable"`.
  - `desktop_start_moves_a_file_password_into_the_keychain`: file has `password: hunter2` → after start the store has it and the file does not.
  - `desktop_start_uses_the_keychain_password`: the engine settings' `proxy.password == Some("hunter2")` (through the `settings` closure seam `start_with` already has).
  - `cli_server_keeps_the_password_in_the_file`: no store → file has it (B3 behaviour unchanged).
- [ ] **Step 2: Run** `cargo test -p fbtt-server --all-features --test desktop_access` and `cargo test -p featherbittorrent-desktop` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace --all-features` and clippy → PASS.
- [ ] **Step 5: Commit** `feat(desktop): proxy password in the system keychain`.

### Task B5: Proxy health on the TUI, the web UI and the desktop app

**Files:**
- Modify: `crates/fbtt-core/src/client.rs` (`Snapshot.proxy: ProxyHealth`), `local.rs` (subscribe through `health_from_event`), `remote/backend.rs` (poll `GET /api/v1/settings/proxy/state` every 5 s; `404` → `Off`)
- Modify: `crates/fbtt-server/src/routes/settings.rs` (`GET /api/v1/settings/proxy/state` → `200 ProxyHealth`)
- Modify: `crates/fbtt-tui/src/view/header.rs` (`connection_line` ~47: the headline in the error colour, followed by `  S: Proxy`)
- Create: `web/src/components/ProxyBanner.tsx`, `ProxyBanner.test.tsx`; Modify: `web/src/components/Header.tsx`, `web/src/api/client.ts` (`proxyState()`), `web/src/App.tsx` (opens Settings at `proxy`)
- Test: `crates/fbtt-tui/tests/views.rs`, `crates/fbtt-server/tests/settings.rs`, `crates/fbtt-core/tests/proxy.rs`

**Interfaces:**
- Consumes: `ProxyHealth`, `health`, `health_from_event` (B2); the Settings page's `section` prop and `resolveSection` (A5).
- `ProxyBanner`: polls `proxyState()` every 5 s while the page is visible and the settings say a proxy is set; for `unreachable`/`login_refused` shows `ProxyHealth`'s headline as a button that opens Settings at `proxy`; `udp_not_relayed`, `ok` and `off` show nothing here. The desktop app gets it through the same web header.

- [ ] **Step 1: Write failing tests:**
  - `proxy_state_route_reports_unreachable` (closed loopback port) and `reason_is_display_safe` (a reason with `\x1b[31m` comes back without the escape).
  - `local_snapshot_carries_proxy_health`.
  - `remote_polls_proxy_state_and_old_server_is_off`.
  - TUI `header_shows_cant_reach_the_proxy`: buffer contains `Can't reach the proxy: ` and `S: Proxy`; `header_shows_nothing_for_udp_not_relayed`.
  - web `banner_shows_headline_and_opens_proxy_section` (click → `onOpenSettings('proxy')`), `banner_hidden_when_ok`, `banner_stops_polling_when_no_proxy`.
- [ ] **Step 2: Run** the three crates' tests and `npm --prefix web test` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace --all-features`, clippy and the web gate of A5 Step 4 → PASS.
- [ ] **Step 5: Commit** `feat: show when the proxy cannot be reached`.

### Task B6: Proxy section in the web UI and the TUI; proxy docs

**Files:**
- Create: `web/src/components/settings/ProxySection.tsx`, `ProxySection.test.tsx`; Modify: `sections.ts` (`built` gains `proxy`), `web/src/api/client.ts` (`saveProxy`)
- Modify: `crates/fbtt-tui/src/settings/mod.rs` (`TuiSection::Proxy`), `view/settings.rs`; Test: `crates/fbtt-tui/tests/settings.rs`
- Modify: `docs/web-ui.md`, `docs/desktop.md`, `docs/server.md`, `README.md`, `CHANGELOG.md`

**Interfaces:**
- Consumes: `ProxyView`, `ProxyChange`, `PasswordChange`, `PasswordStorage` (B3, B4); `ProxyHealth` (B5).
- Both surfaces: `Proxy type` (`None`, `HTTP`, `SOCKS5`), `Host`, `Port`, `User name`, `Password` (placeholder `saved` when `password_saved`, a `Clear password` action, and the no-keychain line under it when `password_storage == Unavailable`), the two help paragraphs from Global Constraints (the HTTP one only for HTTP), `This proxy does not relay UDP, so the DHT and UDP trackers are off` when health is `udp_not_relayed`, and the current headline when unreachable. An untouched password field saves as `"keep"`.

- [ ] **Step 1: Write failing tests:**
  - web `untouched_password_sends_keep`, `typed_password_sends_set`, `clear_sends_clear`, `password_is_never_prefilled` (the input's value is `''`), `http_shows_tcp_only_line`, `no_keychain_shows_kept_in_config_line`, `udp_not_relayed_line_shown`, `invalid_port_shows_refusal`.
  - TUI `proxy_section_saves_change` (`Command::Settings(Save(SettingsChange::Proxy(ProxyChange { password: PasswordChange::Keep, .. })))`), `proxy_section_shows_udp_line`.
- [ ] **Step 2: Run** `cargo test -p fbtt-tui` and `npm --prefix web test` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the full gate (`cargo fmt --all --check`, clippy, `cargo test --workspace --all-features`, `cargo deny check`, the web gate) and `cd website && npm ci && npm run build` (docs links) → PASS.
- [ ] **Step 5: Docs and changelog.** A `## Proxy` section in `docs/server.md` (the `proxy:` keys, the rule that a set proxy is the only way out, what turns off, the fail-closed start, the password in the file for the CLI and a server), `docs/desktop.md` (the keychain; with no keychain the password stays in `config.yaml`), `docs/web-ui.md` (the section and the banner), `README.md` (the TUI section). The downgrade note in `docs/server.md` and the CHANGELOG line: "Going back to 0.4.x removes the proxy: 0.4.x ignores `proxy:` and connects directly, and may drop the key when it saves." CHANGELOG (Added): "Proxy settings (HTTP and SOCKS5, with optional login) in the terminal client, the web UI and the desktop app. While a proxy is set, every connection goes through it."
- [ ] **Step 6: Commit** `feat: proxy settings on every surface`.
