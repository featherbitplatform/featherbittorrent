# Milestone 1: local client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this repo the implementer step runs through the `local-implementer` skill.

**Goal:** `featherbittorrent` runs as a complete local BitTorrent client in the terminal: it embeds the fbt engine, lists, adds, pauses, resumes and deletes torrents, changes file priorities and live limits, and resumes everything on the next start, in a repo that already carries the full featherbit CI and security gates.

**Architecture:** One Cargo workspace with three crates. `fbtt-core` holds config, paths, the library lock and the client interface: a `Client` is a command sender plus a `watch` of immutable snapshots, so the UI never awaits anything. `LocalBackend` implements it over `fbt::Session`; milestone 2 adds the remote backend behind the same interface. `fbtt-tui` is a pure model (keys in, actions out) plus ratatui rendering, and the `featherbittorrent` binary wires terminal, runtime and backend together.

**Tech Stack:** Rust 2024 (rust-version 1.99), tokio, `fbt` (git, tag `v0.8.0`, feature `serde`), ratatui + crossterm, clap, serde + `serde_norway` (YAML), `directories`.

**Spec:** `docs/superpowers/specs/2026-11-14-featherbittorrent-design.md`

**Milestone order (builder's call, spec section 9):**
1. This plan: scaffold and gates, local engine, the TUI over it. No release.
2. `serve`, the wizard, the remote backend over fbt-api plus SSE, authentication (basic and device keys, fbt-api authenticator decision), the folder picker routes, the connect screen and "reopen as last used", the release workflow. Release `0.1.0`: downloading is solid locally and through a server (spec goal 1).
3. Streaming to mpv/VLC (local loopback endpoint and server stream URLs), the image logo (kitty, iTerm2, sixel with a timed-out query).
4. Packages: `.deb` and `.rpm` (arm64, x86-64), Windows and macOS archives, autostart docs.

## Global Constraints

- Repository `github.com/featherbitplatform/featherbittorrent`, Apache-2.0; every source file needs no header, `LICENSE` and `NOTICE` at the root.
- Gitflow: work on `feature/m1-local-client` off `develop`; PR into `develop`; `main` holds tags only.
- Conventional Commits. No attribution of any kind: no `Co-Authored-By`, no `Claude-Session`, no session links, no "Generated with" footer, in commits or PR bodies.
- Rust edition 2024, `rust-version = "1.99"`, `unsafe_code = "forbid"`, clippy `-D warnings`, `cargo fmt`.
- Every dependency is declared once in `[workspace.dependencies]` and inherited with `.workspace = true`.
- `fbt = { git = "https://github.com/featherbitplatform/fbt-lib", tag = "v0.8.0", features = ["serde"] }`.
- Tests never contact the public internet: every test `Settings` sets `dht_enabled = false`, `enable_lsd = false`, `enable_upnp = false`, `enable_natpmp = false`, `listen_addrs` on `127.0.0.1:0`.
- Product copy (UI strings, docs, README): no emoji, no em-dashes. Naming other torrent clients is allowed.
- Remote text shown in the TUI (torrent names, file paths, tracker messages, peer client names, error strings) goes through `fbt::display_safe(s, fbt::MAX_DISPLAY_CHARS)` before it is drawn.
- Colours (Graphite tokens): canvas `#0b0b0e`, selected row `#151518`, rules `#313135`, muted text `#919199`, body text `#b6b6bf`, strong text `#f5f5f7`, accent `#863bff`, accent light `#a584ff`, seeding `#2bbb71`, stalled `#f2a635`, connection `#59c5f5`, error `#ed4a49`.
- The UI thread never awaits the network or the disk; redraws happen only when the snapshot or the model changed, at most 30 per second.

## Review Focus

1. A torrent name or file path full of bidi overrides, zero-width or control characters, or 10 000 characters long: the table shows a cleaned, cut name and keeps its column widths. Test in Task 5 (`hostile_names_render_cleanly`).
2. A terminal resized to 20x5 or 1x1: no panic, the view degrades to whatever fits. Test in Task 5 (`tiny_terminal_does_not_panic`).
3. A corrupt or truncated `session.state` or `config.yaml`: the app starts, reports the problem in one line, and keeps the bad file as `<name>.bad` instead of overwriting it silently. Tests in Task 2 (`bad_config_is_reported`) and Task 4 (`corrupt_state_is_kept_aside`).
4. A second `featherbittorrent` on the same library: it exits with a clear message instead of corrupting resume data. Test in Task 3 (`second_lock_is_refused`) and Task 7 (`lock_held_message`).
5. Adding a magnet or `.torrent` that is malformed, a path that does not exist, or a duplicate torrent: the dialog shows the error and the app keeps running. Tests in Task 4 (`add_errors_become_notices`) and Task 6 (`add_dialog_shows_error`).

---

### Task 1: Scaffold with CI and security gates

**Files:**
- Create: `Cargo.toml` (workspace), `rust-toolchain.toml` (channel `stable`), `rustfmt.toml` (copy fbt-lib's), `crates/fbtt-core/{Cargo.toml,src/lib.rs}`, `crates/fbtt-tui/{Cargo.toml,src/lib.rs}`, `crates/featherbittorrent/{Cargo.toml,src/main.rs}`, `crates/featherbittorrent/tests/cli.rs`
- Create: `.github/workflows/ci.yml`, `.github/workflows/security.yml`, `deny.toml`, `.gitleaks.toml`, `.grype.yaml`, `.semgrepignore`, `dev/sast.sh`, `dev/sast.ps1`
- Create: `CHANGELOG.md`, `CLAUDE.md`, `README.md`, `NOTICE`; Modify: `.gitignore` (add `/target`, `/sast-out`, keep `.superpowers/`). `LICENSE` exists: check it is Apache-2.0, else replace it.

**Interfaces:**
- Produces: crate names `fbtt-core`, `fbtt-tui`, binary `featherbittorrent`; workspace dependency table every later task adds to.

- [ ] **Step 1: Write the failing test** `crates/featherbittorrent/tests/cli.rs`:

```rust
#[test]
fn version_prints_name_and_version() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_featherbittorrent"))
        .arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(),
               format!("featherbittorrent {}", env!("CARGO_PKG_VERSION")));
}
```

- [ ] **Step 2: Run** `cargo test -p featherbittorrent --test cli`. Expected: FAIL (no workspace yet).
- [ ] **Step 3: Write the workspace.** Version `0.0.0` in `[workspace.package]` (the first release branch bumps it), lints as in fbt-lib's root `Cargo.toml` (`unsafe_code = "forbid"`, `missing_debug_implementations = "warn"`, clippy `all = "warn"`). `main.rs` uses clap derive with `#[command(name = "featherbittorrent", version)]`.
- [ ] **Step 4: Write the gates by adapting fbt-lib's files** (`../fbt-lib/.github/workflows/ci.yml`, `security.yml`, `deny.toml`, `.gitleaks.toml`, `.grype.yaml`, `.semgrepignore`, `dev/sast.sh`, `dev/sast.ps1`): fmt (Linux), clippy `--workspace --all-targets -- -D warnings` and `cargo test --workspace` on ubuntu, macos and windows; cargo-deny; semgrep, grype, gitleaks over the full history, weekly on `main`. Drop everything fbt-lib-specific (fuzz, interop, bench, pure-rust check). Every ignore keeps a one-line reason; `deny.toml` allows git sources from `github.com/featherbitplatform` only.
- [ ] **Step 5: Write the docs.** `CHANGELOG.md`: Keep a Changelog header as in fbt-lib plus an empty `## [Unreleased]`. `README.md`: what it is (spec section 1, two paragraphs), build and run commands, licence. `CLAUDE.md`: what this repo is, the binding spec path, the rules from Global Constraints, the commands (`cargo run -p featherbittorrent`, the full gate as CI runs it, `bash dev/sast.sh`), the debug environment variables Task 2 adds, and "Status: milestone 1 in progress".
- [ ] **Step 6: Run** `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check && cargo deny check`. Expected: all pass.
- [ ] **Step 7: Commit** `chore: scaffold workspace with ci and security gates`.

### Task 2: Paths and config

**Files:**
- Create: `crates/fbtt-core/src/paths.rs`, `crates/fbtt-core/src/config.rs`; Modify: `crates/fbtt-core/src/lib.rs`
- Test: unit tests in both files

**Interfaces:**
- Produces:
  - `pub struct Paths { pub config_dir: PathBuf, pub data_dir: PathBuf }`; `Paths::resolve() -> Result<Paths, ConfigError>` (env `FBTT_CONFIG_DIR` and `FBTT_DATA_DIR` win, else `directories::ProjectDirs::from("io", "featherbit", "featherbittorrent")`); `Paths::config_file(&self) -> PathBuf` (= `config.yaml`), `state_file` (= `session.state` in data dir), `lock_file` (= `library.lock` in data dir).
  - `#[serde(default)] pub struct Config { pub download_dir: PathBuf, pub listen_port: u16, pub limits: Limits, pub cache_size_mib: u64 }`, `pub struct Limits { pub download_kib: u64, pub upload_kib: u64, pub active_downloads: Option<usize>, pub active_seeds: Option<usize>, pub active_total: Option<usize> }` (0 KiB/s = unlimited; `None` = unlimited). Defaults: `download_dir` = the user's download dir (`directories::UserDirs`) or `data_dir/downloads`, `listen_port` 6881, rate limits 0/0, and the three active counts taken from `fbt::QueueSettings::default()` (never hardcoded), `cache_size_mib` 32.
  - `Config::load(path: &Path) -> Result<ConfigLoad, ConfigError>` where `pub enum ConfigLoad { Loaded(Config), Missing, Bad { config: Config, problem: String } }` (Bad: defaults are used, the file is renamed to `config.yaml.bad`, `problem` is one line). `Config::save(&self, path: &Path) -> Result<(), ConfigError>` writes a temp file in the same dir and renames it.
  - `Config::session_settings(&self) -> fbt::Settings`: `listen_addrs` `0.0.0.0:<port>` and `[::]:<port>`, `rate_limit` from KiB/s × 1024, `queue` from `Limits`, `cache_size` from MiB.
  - `pub enum ConfigError` (thiserror): `NoHome`, `Io { path: PathBuf, source: std::io::Error }`, `Write { path: PathBuf, source: std::io::Error }`.

- [ ] **Step 1: Write the failing tests:** `defaults_round_trip` (save then load gives `Loaded` equal to `Config::default()`), `missing_file_is_missing`, `partial_yaml_keeps_defaults` (`listen_port: 51413` only → other fields default), `bad_config_is_reported` (`"listen_port: [nope"` → `Bad`, problem non-empty, `config.yaml.bad` exists, `config.yaml` gone), `env_overrides_paths` (set both env vars in a temp dir; use a mutex since env is process-global), `limits_map_to_settings` (download_kib 100 → `settings.rate_limit.download == 102_400`; `active_total: Some(4)` → `settings.queue.active_limit == Some(4)`).
- [ ] **Step 2: Run** `cargo test -p fbtt-core`. Expected: FAIL.
- [ ] **Step 3: Implement** in `paths.rs` and `config.rs` as in Interfaces; add `serde`, `serde_norway`, `directories`, `thiserror` to the workspace.
- [ ] **Step 4: Run** `cargo test -p fbtt-core`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(core): config file and platform paths`.

### Task 3: Library lock

**Files:**
- Create: `crates/fbtt-core/src/lock.rs`

**Interfaces:**
- Produces: `pub struct LibraryLock` (holds the open `File`, unlocks on drop); `LibraryLock::acquire(path: &Path) -> Result<LibraryLock, LockError>`; `pub enum LockError { Held, Io(std::io::Error) }`. Use `std::fs::File::try_lock` (stable), creating parent dirs; no extra crate.

- [ ] **Step 1: Write the failing tests:** `second_lock_is_refused` (acquire twice on one path → second is `Err(LockError::Held)`), `lock_is_released_on_drop` (drop the first, acquire again → Ok).
- [ ] **Step 2: Run** `cargo test -p fbtt-core lock`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS on all three OSes in CI.
- [ ] **Step 5: Commit** `feat(core): one engine per library lock`.

### Task 4: Client interface and local backend

**Files:**
- Create: `crates/fbtt-core/src/client.rs` (types and `Client`), `crates/fbtt-core/src/local.rs` (`LocalBackend`), `crates/fbtt-core/tests/local.rs`

**Interfaces:**
- Consumes: `Config`, `Paths`, `LibraryLock` (Tasks 2-3).
- Produces (all in `fbtt_core::client`, re-exported at the crate root):
  - `pub struct TorrentId(pub String)`: lowercase hex of `InfoHashes::wire_hash()` (40 digits, same as fbt-api's path ids). `Eq + Hash + Ord + Clone`.
  - `pub struct TorrentRow { id, name, progress: f64, state: RowState, down_rate: u64, up_rate: u64, peers: u32, seeds: u32, size: u64, done: u64, save_path: PathBuf, error: Option<String>, queue_position: usize }`; `pub enum RowState { Metadata, Checking(f64), Queued, Downloading, Stalled, Seeding, Paused, Error }` (Stalled = Downloading with `down_rate == 0` and `progress < 1.0` for at least 30 s; the backend tracks when each torrent last had a non-zero rate).
  - `pub struct Totals { down_rate: u64, up_rate: u64, dht_nodes: Option<usize> }` (`nodes_v4 + nodes_v6`).
  - `pub struct LiveLimits { download_kib: u64, upload_kib: u64, active_downloads: Option<usize>, active_seeds: Option<usize>, active_total: Option<usize> }` (same fields as `config::Limits`; `From` both ways).
  - `pub enum Detail { Files(Vec<FileRow>), Peers(Vec<PeerRow>), Trackers(Vec<TrackerRow>), Pieces { have: Vec<bool> } }`; `FileRow { index, path: String, size, done, priority: FilePriority }`, `pub enum FilePriority { Skip, Low, Normal, High }`; `PeerRow { addr: String, client: String, down_rate, up_rate, progress: Option<f64> }` (progress may be `None` in M1); `TrackerRow { url, tier, status: String, seeders: Option<u32>, leechers: Option<u32> }`; `pub enum DetailTab { Files, Peers, Trackers, Pieces }`.
  - `pub struct Snapshot { pub connection: Connection, pub torrents: Vec<TorrentRow>, pub totals: Totals, pub limits: LiveLimits, pub detail: Option<(TorrentId, Detail)>, pub default_dir: PathBuf }`, `pub enum Connection { Local, Remote { name: String, secure: bool, auth: String } }` (M1 builds `Local` only). All types `Debug + Clone + PartialEq`.
  - `pub enum AddSource { Magnet(String), File(Vec<u8>) }`; `pub enum Command { Add { source: AddSource, dir: Option<PathBuf> }, Pause(TorrentId), Resume(TorrentId), Remove { id: TorrentId, delete_files: bool }, SetFilePriority { id: TorrentId, file: usize, priority: FilePriority }, SetLimits(LiveLimits), Watch(Option<(TorrentId, DetailTab)>), Shutdown }`.
  - `pub enum Notice { Info(String), Error(String) }`.
  - `pub struct Client { commands: mpsc::Sender<Command>, snapshot: watch::Receiver<Arc<Snapshot>>, notices: mpsc::Receiver<Notice> }` with `send(&self, c: Command) -> bool` (`try_send`; false if full or closed, never blocks), `snapshot(&self) -> Arc<Snapshot>`, `changed(&mut self) -> &mut watch::Receiver<Arc<Snapshot>>`, `try_notice(&mut self) -> Option<Notice>`, and `pub async fn notice(&mut self) -> Option<Notice>`. Command queue capacity 64.
  - `LocalBackend::start(config: Config, paths: &Paths, lock: LibraryLock) -> Result<(Client, JoinHandle<()>), fbt::SessionError>`: restores from `paths.state_file()` via `Session::restore`, else `Session::new`; spawns the backend task that owns the session and the lock. The task's `JoinHandle` resolves after `Shutdown` has saved.
  - `LocalBackend::start_with(settings: fbt::Settings, config: Config, paths: &Paths, lock: LibraryLock)` for tests (same, explicit settings).
- Backend loop rules:
  - publish a new snapshot only when it differs (`PartialEq`) from the last, checked every 500 ms and right after each command;
  - `Watch` sets which detail it refreshes on the same cadence (files, peers, trackers via the handle's async getters; pieces via `pieces()`);
  - `Add`: decode with `Magnet::parse` or `Metainfo::from_bytes`; dir defaults to `config.download_dir`; errors (decode, duplicate, session) become `Notice::Error` with `display_safe` text, success a `Notice::Info("Added <name>")`;
  - `SetLimits` calls `set_rate_limit` and `set_queue_settings` and writes the new limits into `config.yaml` (load, change `limits`, save);
  - every 300 s, `Session::save_state` to the state file (atomic: temp + rename); `Shutdown` calls `shutdown_and_save`, writes the file, then ends the task;
  - a state file that does not decode (`SessionState::from_bytes` error) is renamed to `session.state.bad`, the session starts empty and a `Notice::Error` says `Saved torrents could not be read; the file was kept as session.state.bad`;
  - torrents that fail to restore each produce one `Notice::Error`.

- [ ] **Step 1: Write the failing tests** in `crates/fbtt-core/tests/local.rs`, all with loopback-only settings and temp dirs, each test under a 60 s `tokio::time::timeout`:
  - `downloads_from_local_seed`: build a 1 MiB file, `CreateTorrent` it, seed it with a plain `fbt::Session` (the file already in place), `Command::Add { File(bytes) }` into the backend, connect them with `session.torrent(..).add_peer(seed_addr)` through a `#[doc(hidden)] pub fn session(&self)` test hook on a `LocalBackend` test-support handle (feature `test-support`, enabled in dev-dependencies), wait for a snapshot whose row has `state == RowState::Seeding` and `progress == 1.0`, compare file bytes.
  - `state_survives_restart`: add a magnet with a made-up hash, `Shutdown`, start again on the same paths, the snapshot lists that `TorrentId`.
  - `corrupt_state_is_kept_aside`: write `b"garbage"` as the state file, start, expect `Notice::Error` containing `session.state.bad`, file exists, empty torrent list.
  - `add_errors_become_notices`: `Magnet("magnet:?xt=nope")` → `Notice::Error`; the same valid magnet twice → second gives `Notice::Error`; backend still answers `Pause` afterwards.
  - `remove_with_files_deletes_them` and `remove_keeps_files`.
  - `set_limits_applies_and_persists`: `SetLimits` with download 100 → snapshot `limits.download_kib == 100`, `Config::load` shows it.
  - `set_file_priority_skip`: on a 2-file torrent, Skip file 1 → `Watch(Files)` detail shows `FilePriority::Skip`.
- [ ] **Step 2: Run** `cargo test -p fbtt-core --test local`. Expected: FAIL.
- [ ] **Step 3: Implement** `client.rs` then `local.rs`. Map `fbt::TorrentState` to `RowState` (Paused/Queued/Seeding/Error/DownloadingMetadata→Metadata/QueuedForChecking→Queued/CheckingFiles→Checking/Downloading→Downloading or Stalled). Keep each file under about 500 lines; split mapping helpers into `local/map.rs` if needed.
- [ ] **Step 4: Run** `cargo test -p fbtt-core`. Expected: PASS. Also run `downloads_from_local_seed` 10 times (`for i in $(seq 10); do cargo test -p fbtt-core --test local downloads_from_local_seed || break; done`) to catch flakiness.
- [ ] **Step 5: Commit** `feat(core): client interface and embedded engine backend`.

### Task 5: TUI model and main views

**Files:**
- Create: `crates/fbtt-tui/src/{model.rs,keys.rs,theme.rs,format.rs,view/mod.rs,view/header.rs,view/table.rs,view/detail.rs,view/footer.rs,view/minimal.rs}`

**Interfaces:**
- Consumes: `fbtt_core::{Snapshot, TorrentRow, RowState, Detail, DetailTab, Command, TorrentId}`.
- Produces:
  - `pub struct Model` with `pub fn new() -> Model`, `pub fn handle_key(&mut self, key: crossterm::event::KeyEvent, snap: &Snapshot) -> Vec<Command>`, `pub fn quit_requested(&self) -> bool`, `pub fn watch(&self) -> Option<(TorrentId, DetailTab)>` (what the detail pane needs; the binary sends `Command::Watch` when it changes).
  - `pub fn draw(frame: &mut ratatui::Frame, model: &Model, snap: &Snapshot)`.
  - Model state: `view: View { Full, Minimal }`, `selected: Option<TorrentId>` (selection follows the id, not the row index, across re-sorts), `sort: (SortKey, Descending)`, `SortKey { Name, Progress, Down, Up, Peers, State, Queue }` (default `Queue` ascending), `filter: String` (case-insensitive substring of the name), `detail_tab: DetailTab`, `modal: Option<Modal>` (Task 6 fills `Modal`).
  - `pub fn visible_rows<'a>(model: &Model, snap: &'a Snapshot) -> Vec<&'a TorrentRow>` (filtered and sorted).
- Key map (M1): `j`/`k`/Down/Up move, `g`/`G` first/last, `PageUp`/`PageDown`, `p` pause or resume selected, `m` toggles view, `Tab`/`Shift+Tab` next/previous detail tab, `o` cycles sort key, `O` reverses, `/` starts the filter (Task 6), `a` add, `d` delete, `S` settings, `?` help (Task 6), `q` quit (a second `q` or `y` confirms only when any torrent is downloading; otherwise quits at once). `c` and `s` are reserved for milestones 2 and 3 and not listed.
- Layout:
  - full view: header 3 lines (title `featherbittorrent` in strong text plus `env!("CARGO_PKG_VERSION")` muted; `Local` in the connection colour; `down <rate>  up <rate>  DHT <n> nodes  <n> torrents`), the table, a rule, the detail pane (tabs line plus content, 40 % of the remaining height, at least 6 lines when the terminal allows), a rule, the footer with the key hints in accent light.
  - minimal view: one header line (`featherbittorrent  down <rate> up <rate>`), then per torrent a 24-cell bar, percent, name, state.
  - the table renders only visible rows (ratatui `Table` with a `TableState` offset), progress as a bar of `█`/`░` in the state's colour, rates via `format::rate(bytes_per_sec) -> String` (`0 B/s`, `512 B/s`, `1.5 KiB/s`, `4.2 MiB/s`; one decimal from KiB up), sizes via `format::size`.
  - state colours: Downloading accent, Seeding green, Stalled amber, Error red, Paused/Queued/Metadata/Checking muted.

- [ ] **Step 1: Write the failing tests** (ratatui `TestBackend`, assertions on buffer text and cell colours):
  - `full_view_shows_rows_and_header` (two rows; header contains `featherbittorrent` and `Local`; rows show names, `62%`, `4.2 MiB/s`).
  - `selection_follows_id_after_resort` (select B, change sort, still B).
  - `filter_hides_rows` (filter `deb` keeps only `debian-...`).
  - `minimal_view_toggle` (`m` → no detail tabs line drawn, bars present).
  - `seeding_row_is_green` (the state cell's fg is `#2bbb71`).
  - `hostile_names_render_cleanly`: a name of `"evil\u{202e}gpj.exe" + "x".repeat(10_000)` → no `\u{202e}` in the buffer, the row fits its width.
  - `tiny_terminal_does_not_panic`: draw at 20x5 and 1x1.
  - `p_sends_pause_or_resume` (Downloading row → `Command::Pause(id)`, Paused row → `Command::Resume(id)`).
  - `rate_format` table test for `format::rate`.
- [ ] **Step 2: Run** `cargo test -p fbtt-tui`. Expected: FAIL.
- [ ] **Step 3: Implement.** Colours only from `theme.rs` constants named after the tokens (`INK_950`, `VIOLET_500`, ...). Names pass through `fbt::display_safe` in the view (format.rs `safe(s: &str) -> String`).
- [ ] **Step 4: Run** `cargo test -p fbtt-tui`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(tui): torrent table, detail pane and minimal view`.

### Task 6: Dialogs: add, delete, settings, filter, help, file priorities

**Files:**
- Create: `crates/fbtt-tui/src/modal/{mod.rs,add.rs,delete.rs,settings.rs,help.rs,input.rs}`; Modify: `model.rs`, `view/mod.rs`, `view/detail.rs`

**Interfaces:**
- Consumes: Task 5's `Model`, `handle_key`; `Command::{Add, Remove, SetLimits, SetFilePriority}`; `Snapshot::{limits, default_dir}`.
- Produces:
  - `pub enum Modal { Add(AddForm), Delete { id: TorrentId, name: String }, Settings(SettingsForm), Help, Filter }`.
  - `AddForm { source: Input, dir: Input, error: Option<String> }`: Enter on `source` accepts text starting with `magnet:` as `AddSource::Magnet`; anything else is a path, read on Enter with `std::fs::read` (the TUI machine's file system, spec 5.2) capped at 16 MiB; read errors set `error` and keep the dialog open. `dir` is prefilled with `snap.default_dir`; an edited `dir` is sent as `Some(dir)`. Tab switches fields, Esc closes.
  - `Delete`: `k` keep files → `Remove { delete_files: false }`, `D` (capital) delete files → `delete_files: true`, Esc cancels. Copy: `Remove "<name>"?  k keep files   D delete files   Esc cancel`.
  - `SettingsForm`: five numeric fields matching `LiveLimits` (`Download limit KiB/s`, `Upload limit KiB/s`, `Active downloads`, `Active seeds`, `Active total`), `0` or empty = unlimited; Enter applies → `Command::SetLimits`; non-numeric input marks the field red and does not send.
  - Filter: `/` opens a one-line input in the footer; typing updates `model.filter` live; Enter keeps it, Esc clears it.
  - Help: lists every key with its action, generated from the same key table the footer uses (one `const KEYS: &[(&str, &str)]` in `keys.rs`).
  - File priorities: on the Files tab, `Enter` focuses the file list, `j`/`k` move, `+`/`-` raise or lower priority (Skip < Low < Normal < High), each change sends `SetFilePriority`; Esc leaves the list.
  - Notices: the model keeps the last notice for 5 s (`Model::push_notice(Notice, now: Instant)`); the footer shows it (Error in red); an `Add` dialog that is open when an `Error` notice arrives shows it in `error` instead.

- [ ] **Step 1: Write the failing tests:** `add_magnet_sends_command`, `add_dialog_shows_error` (path that does not exist → `error` set, no command, dialog open), `add_file_over_cap_is_refused`, `delete_keep_and_delete` (both keys), `settings_rejects_text` and `settings_sends_limits`, `filter_live_and_escape`, `help_lists_every_key` (every entry of `KEYS` appears on the help screen), `file_priority_keys`, `notice_expires_after_5s`.
- [ ] **Step 2: Run** `cargo test -p fbtt-tui`. Expected: FAIL.
- [ ] **Step 3: Implement.** Dialogs draw centered over the main view with a `#313135` border and `#151518` background; inputs are one shared `Input` (text, cursor, insert, backspace, left/right, home/end).
- [ ] **Step 4: Run** `cargo test -p fbtt-tui`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(tui): add, delete, settings, filter and help dialogs`.

### Task 7: The binary: terminal, event loop, lock handling

**Files:**
- Modify: `crates/featherbittorrent/src/main.rs`; Create: `crates/featherbittorrent/src/{run.rs,terminal.rs}`, `crates/featherbittorrent/tests/cli.rs` (extend)

**Interfaces:**
- Consumes: everything above.
- Produces: `featherbittorrent` with no subcommand runs the local TUI. No `serve` subcommand yet (milestone 2).
- Behaviour:
  - order: `Paths::resolve`, `Config::load` (`Missing` → write defaults; `Bad` → keep going, show the problem as the first notice), `LibraryLock::acquire`, `LocalBackend::start`, then the terminal.
  - lock held → print `Another featherbittorrent is already using this library (<data dir>). Close it first.` to stderr, exit code 3, terminal untouched.
  - terminal: raw mode, alternate screen, hidden cursor; restored on every exit path including panics (a panic hook that restores, then calls the previous hook) and on `SIGTERM`/Ctrl-C (`tokio::signal`).
  - loop: `tokio::select!` over crossterm's `EventStream` (feature `event-stream`), `client.changed()`, `client.notice()` and a 33 ms frame tick that draws only when a dirty flag is set; never awaits a backend reply.
  - `Model::watch()` changes → `Command::Watch`.
  - quit: `Command::Shutdown`, a status line `Saving...` drawn once, await the backend `JoinHandle` (no timeout: losing resume data is worse than waiting), restore the terminal, exit 0.
  - debug env `FBTT_CONFIG_DIR`, `FBTT_DATA_DIR` (Task 2) are the only knobs; document them in `CLAUDE.md`.

- [ ] **Step 1: Write the failing test** `lock_held_message` in `tests/cli.rs`: hold `LibraryLock` on a temp data dir in the test process, run the binary with `FBTT_DATA_DIR`/`FBTT_CONFIG_DIR` pointing at temp dirs and stdin not a TTY, expect exit code 3 and stderr containing `already using this library`.
- [ ] **Step 2: Run** `cargo test -p featherbittorrent`. Expected: FAIL.
- [ ] **Step 3: Implement** `run.rs` (setup and loop) and `terminal.rs` (guard type restoring on `Drop`, panic hook).
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Manual check (controller, not the implementer):** in a scratch config dir run `cargo run -p featherbittorrent`, add a magnet for a public test torrent (only by the owner, outside CI), watch it download, quit, restart, see it resume; resize to tiny and back; check memory with 0 torrents stays under 30 MB RSS.
- [ ] **Step 6: Commit** `feat: local terminal client`.

### Task 8: Milestone wrap-up

**Files:**
- Modify: `CHANGELOG.md` (`## [Unreleased]` → Added lines for the local client, each ending in the PR link once known), `README.md` (usage: keys, config file location and fields, debug env vars), `CLAUDE.md` (status: milestone 1 done on `develop`, next milestone 2).

- [ ] **Step 1:** Write the docs. Config fields are documented from `Config` exactly (names, units, defaults).
- [ ] **Step 2: Run** the full gate (`cargo fmt --all --check`, clippy, `cargo test --workspace`, `cargo deny check`, `bash dev/sast.sh`). Expected: all pass, SAST clean or every finding ignored with a one-line reason.
- [ ] **Step 3: Commit** `docs: local client usage and changelog`. Open the PR `feature/m1-local-client` → `develop`; merge only when every CI check is green.
