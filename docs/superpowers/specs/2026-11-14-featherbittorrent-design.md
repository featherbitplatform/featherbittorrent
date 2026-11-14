# featherbittorrent: design

Status: written 2026-11-14 from the design conversation with the owner.

Decisions are tagged **[owner]** when the owner made them in the design conversation, and **[builder's call]** when they were made while writing this document. Builder's calls may be revisited by whoever plans a milestone. Owner decisions may not, without the owner.

## 1. Intent

featherbittorrent is a BitTorrent client built on `fbt-lib` (`../fbt-lib`, released to `v0.8.0`). It runs in one of two roles: **[owner]**

- a **server** (`featherbittorrent serve`): headless, downloads and seeds, and is controlled over the network;
- a **TUI** in the terminal, which either runs the engine itself on the same machine or connects to one server.

A typical setup is a Raspberry Pi at home running the server, saving to a NAS or another chosen storage. The owner controls it from a laptop's TUI over the LAN, or remotely through a reverse proxy. On the laptop itself, the TUI is a complete client with no daemon behind it. **[owner]**

Why: the owner has used uTorrent, Deluge and qBittorrent. He liked the Deluge and qBittorrent interfaces, a clear torrent list with a detail pane, and Deluge's split between a daemon and a thin client. He finds them dated, and wants something **snappier and with a smaller footprint**. **[owner]**

## 2. Goals

1. **Downloading torrents reliably is the core of the first release**. Everything else is built around it. **[owner]**
2. **One binary, two roles**: `featherbittorrent serve` for the server, and `featherbittorrent` for the TUI. **[builder's call; owner agreed with "single binary" in the playback]**
3. **A local TUI embeds the engine.** There is no separate daemon and no API server unless asked for. Quitting the TUI stops the torrents, which resume on the next start. **[owner]**
4. **A remote TUI** connects to exactly one server at a time. **[owner]** It watches progress, adds torrents, and pauses, resumes and deletes them. **[owner]**
5. **Streaming** from the TUI: a key opens the selected file in mpv or VLC on the TUI's machine. This works with the local engine and with a remote server. **[owner]**
6. **Snappy and light**:
   - keypresses feel instant, and redraws stay smooth with hundreds of torrents;
   - small memory and CPU use, so the server runs comfortably on a Raspberry Pi 2B. **[owner: "snappy, less footprint"; numbers are the builder's]**
7. **Graphite look** (`../design-system-v2/`): ink canvas, violet as the only accent, Geist Mono where the terminal font allows. **[owner, from the fbt-lib design]**

## 3. Non-goals and anti-goals

Non-goals (not built now, on the roadmap):

- **Search** across torrent indexing sites. **[owner: later]**
- **One TUI talking to several servers.** **[owner: later]**
- **The web UI, and the Tauri desktop builds** that reuse it. **[owner: later, per the fbt-lib design]** The server's API is designed so the web UI can be built on it. When the web UI exists, it also gets browser streaming.
- **Autostart.** Packages never install or enable a service. The docs explain how to set up systemd, launchd or a Windows service. **[owner]**
- **Read-only users or per-user permissions.** Every authenticated client may do everything. **[builder's call]**
- **A separate "in progress" folder** with automatic moves on completion. **[owner]**

Anti-goals (would make the work a failure even if it works):

- A local TUI that needs a separately started or hidden daemon. **[owner]**
- A heavy or sluggish UI: blocking redraws, keypresses that wait on the network, or memory growth with the number of torrents beyond what the engine needs.
- A blurry or garbled logo. In a terminal that cannot show images, the logo is not drawn at all (section 5.3). **[owner]**
- Passwords or private keys stored in plain text, or put on a media player's command line.
- Any remote input (a server, a peer, a torrent) crashing the TUI or the server.

## 4. Platforms and distribution **[owner]**

| Platform | Architectures | Distribution |
|---|---|---|
| Linux, Debian family (incl. Raspberry Pi OS 64-bit) | arm64, x86-64 | `.deb` |
| Linux, Fedora family | arm64, x86-64 | `.rpm` **[builder's call for the format]** |
| Windows | x86-64, arm64 | archive or installer **[builder's call]** |
| macOS | Apple Silicon (and Intel if the toolchain allows) | archive or `.dmg` **[builder's call]** |

The reference low end is a **Raspberry Pi 2 Model B v1.2**: Cortex-A53, 1 GB RAM, 100 Mbit Ethernet shared with USB, 64-bit OS. **[owner]** Downloads often land on network storage (NFS/SMB mounts). **[owner]** Defaults on such a host keep fbt-lib's write cache small. Network filesystems must work: sparse files may not exist there, and renames and moves may cross devices. **[builder's call]**

## 5. The TUI

### 5.1 Views **[owner, from the mockups]**

- **Full management view:**
  - a header with the connection (local, or the server name, scheme and auth method), total down and up rates, and the number of DHT nodes and torrents;
  - the torrent table: name, progress bar and percent, down and up rates, peers, state;
  - a detail pane for the selected torrent, with tabs for Files, Peers, Trackers and Pieces;
  - a key-hint footer.
- **Minimal mode:** only a progress bar per torrent, plus a one-line header, for a small split or a corner of the screen.
- `m` switches between the two views.

### 5.2 Interaction

- **Keys.** Every action has a single key, listed in the footer, and `?` opens a help screen. The mockup's keys are a starting point: `a` add, `d` delete, `p` pause or resume, `s` stream, `/` filter, `m` minimal, `c` connect, `?` help, `q` quit. **[owner agreed in the playback]**
- **Table.** It can be sorted by any column and filtered by typing `/`. **[owner agreed]**
- **Delete.** Deleting asks whether to keep or delete the files. **[owner agreed]**
- **File priorities.** They are changed from the detail pane. **[owner agreed]**
- **Colours.** Ink background, violet accent, green for seeding, amber for stalled, cyan for the connection, red for errors. **[owner agreed]**
- **Adding torrents.** You paste a magnet link or choose a `.torrent` file from the TUI machine's file system. When connected remotely, the TUI uploads the file to the server. **[owner]**
- **Destination folder.** Each server has a default download folder. Before a torrent is added, you can browse the server's allowed folders and pick another (section 6.5). **[owner]**
- **Where files live.** A torrent's files stay where they were downloaded: the default folder or the one chosen when adding. There is no separate "in progress" folder and no automatic move on completion. **[owner]**
- **Settings screen.** A small screen changes, while running, the global download and upload limits and the number of active torrents (downloads, seeds, total). It works the same on the local engine and on a remote server. Everything else is set in the YAML file only, in the first release. **[owner]**

### 5.3 Logo **[owner]**

The real featherbit mark (`../design-system-v2/assets/featherbit-mark.png`) is shown as an image wherever the terminal supports inline images: the header (small) and the splash, about or help screen (large). The supported protocols are kitty graphics, iTerm2 inline images and sixel, which covers kitty, Ghostty, WezTerm and iTerm2. If the terminal supports none of them, **no logo is drawn at all**: the header shows the text "featherbittorrent". Half-block and braille renderings were tried and rejected.

The featherbit terminal (`fbterm`) has no inline image support today, so it shows the text title. Adding kitty graphics to `fbterm` belongs on that project's roadmap, not this one. **[builder's note]**

Detecting the terminal's capability must never stall startup. A query that gets no answer within a short timeout counts as "not supported". **[builder's call]**

### 5.4 Streaming **[owner]**

- `s` on a file opens it in the configured player (mpv or VLC), which must be installed on the TUI's machine.
- **Local TUI:** the player gets the file's path if the file is complete. Otherwise it gets a stream URL from a small loopback streaming endpoint that the local TUI starts on demand. **[builder's call]**
- **Remote TUI:** the player gets a short-lived stream URL from the server, so the user's password or key never appears on the player's command line. **[owner agreed]**
- **Reads ahead of the download.** In both cases, reads that get ahead of the download wait for the data. fbt-lib's deadline-driven reader prioritises the pieces the player needs.

## 6. The server

### 6.1 Running **[owner]**

`featherbittorrent serve` runs in the foreground until stopped, logging to stderr. Nothing starts automatically. Docs show how to run it under systemd, launchd or as a Windows service.

### 6.2 Configuration **[owner]**

- **First-run wizard.** The first `serve` with no config file runs a terminal wizard. It asks for the download folder, the BitTorrent listen port, the API listen address, and whether authentication is on. With authentication on, it asks for a user name and password and/or authorized devices.
- **YAML file.** Everything the wizard writes goes into a YAML file, which can also be edited by hand.
- **TUI connect wizard.** The TUI has its own small wizard, or connect screen, for the server address and the login method.
- **Where the files live.** The config sits in the platform's config directory, and state such as resume data and session state in the platform's data directory. **[builder's call]**
- **Secrets.**
  - The server stores password **hashes**, never passwords.
  - The TUI stores a basic-auth password in the OS keychain (on Windows, DPAPI), never in its config, following `fbterm`'s rule. **[builder's call]**
  - Private keys are never copied or stored by featherbittorrent; they are only read from their path or used through ssh-agent.

### 6.3 Network access and authentication **[owner]**

- **Three ways to reach a server:**
  - the same machine, with the embedded engine and no network at all;
  - the LAN, directly;
  - remote, through a reverse proxy. The docs recommend the featherbit gateway (`../gateway`), which terminates TLS.
- **Authentication can be turned off,** for example on a trusted LAN; that is the user's choice. It is on by default whenever the API listens on anything other than loopback, and turning it off is an explicit setting. **[builder's call, consistent with fbt-api's rule]**
- **Two login methods:**
  - **Username and password** (HTTP basic auth).
  - **Device keys:**
    - The server keeps a list of authorized devices, each a name plus a public key, like SSH's `authorized_keys`.
    - A device is added by pasting its public key. That can be done from the server's command line, the YAML file, or a connected TUI.
    - The TUI signs in with the user's default SSH key (e.g. `~/.ssh/id_ed25519`) or another key the user chooses. Keys protected by a passphrase work through ssh-agent, or the TUI asks for the passphrase.
- **The mechanism is the builder's call, under these constraints:**
  - The key login is challenge-response: the server sends a nonce, the client signs it, and the server verifies the signature against the device list. A successful login yields a short-lived session token that the TUI uses for API calls and stream URLs.
  - Ed25519 is required. RSA and ECDSA are optional.
  - Basic auth over plain HTTP is allowed on a LAN, with a warning. Remote access should be HTTPS, via the proxy.
- **fbt-api integration.** `fbt-api` today offers only one bearer token or no auth. featherbittorrent's server needs its own authentication in front of the fbt-api routes. That may require a pluggable authenticator in `fbt-api`, a change to fbt-lib. Deciding and doing that is part of planning the first server milestone. **[builder's call]**

### 6.4 One library per machine **[builder's call]**

The local TUI and `featherbittorrent serve` on the same machine share one torrent library (config, resume data, session state, download folder). A lock file guarantees only one engine runs on it at a time: if a server already runs on the machine, the TUI offers to connect to it instead of starting its own engine. Plain `featherbittorrent` reopens the way it was last used (the local engine, or the last server it connected to); the connect screen (`c`) switches.

The reverse proxy in front of a remote server is assumed to terminate TLS only, without a login of its own, so stream URLs and the event stream pass through unchanged; the gateway setup guide says so. A proxy that adds its own authentication is not supported in the first release.

### 6.5 API

The server exposes fbt-api's control API, live events and streaming, plus the routes featherbittorrent needs itself:
- the login challenge and response;
- listing the allowed folders, and the directories under them, for the destination picker;
- managing authorized devices.

All file-system access from the API is confined to the configured download roots (fbt-api's `allowed_roots`). **[builder's call]**

## 7. Architecture **[builder's call, within the owner's decisions above]**

- **Language and libraries:** Rust (edition 2024, stable). The TUI uses ratatui on crossterm **[from the fbt-lib design: "ratatui TUI"]**. The engine is `fbt` from fbt-lib, used as a git dependency pinned by tag, and the server uses `fbt-api`.
- **One client interface for every screen.** The TUI talks to an engine through one trait-like client interface: commands, status snapshots and events. There are two implementations:
  - an in-process one over `fbt::Session`;
  - a remote one over HTTP, using fbt-api's JSON routes and the SSE event stream.

  Every screen works the same against either, so local and remote are one code path.
- **Responsiveness:**
  - The UI thread never waits on the network or the disk. It redraws from the latest snapshot at a capped frame rate, and only when something changed.
  - Remote updates come from the SSE stream.
  - Large tables render only their visible rows.
- **Workspace:** one Cargo workspace. Suggested crates are a core with the client interface and config, the TUI, the server, and the binary. This follows the shape of fbt-lib and `fbterm`.

## 8. Engineering standards **[owner]**

The same rules as the gateway and fbterm, from the first commit and for every change after it: Gitflow, Conventional Commits, semantic versioning with the same release flow, and the same CI/CD, security and code-quality gates. No release or merge skips them.

- **Repository:**
  - `github.com/featherbitplatform/featherbittorrent`, Apache-2.0. The repo exists, with an initial commit.
  - Its own `CLAUDE.md` is written with the scaffold.
- **Gitflow:** `main` holds tagged releases only and `develop` is the integration branch, with `feature/*`, `release/X.Y.Z` and `hotfix/*`. Releases follow the same flow as fbt-lib and `fbterm`.
- **Commits and versions:** Conventional Commits, semantic versioning (the version bump happens on the `release/X.Y.Z` branch, the annotated `vX.Y.Z` tag on `main` triggers the release workflow), and a `CHANGELOG.md` in Keep a Changelog format with PR links.
- **No attribution of any kind** in commits or PRs: no `Co-Authored-By`, no session links, no "Generated with" footer.
- **CI and security:** fmt, clippy `-D warnings`, and tests on Linux, macOS and Windows; cargo-deny; and the security workflow used in the other repos (semgrep, grype, gitleaks over the full history, also weekly on `main`), with a local `dev/sast.sh` that runs the same scans; every ignore carries a one-line reason. Pull requests merge only when every gate is green. Releases build every platform in section 4 and attach CycloneDX SBOMs.
- **Tests never contact the public internet.**
- **Product copy:** no emoji and no em-dashes. Naming other torrent clients is allowed, since interop and migration are part of the domain. **[builder's call]**

## 9. Left to the builder

- Milestone order and contents. The first release must make downloading solid locally and through a server, per goal 1.
- The exact key map, sorting defaults, and the layout of the detail tabs.
- The YAML schema, and the wizard's questions beyond those in section 6.2.
- The auth mechanism details within section 6.3, and whether fbt-api gets a pluggable authenticator.
- Packaging tools for `.deb`, `.rpm`, Windows and macOS.
- How the local TUI decides between handing the player a file path and a loopback stream URL.
