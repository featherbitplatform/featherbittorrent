# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Desktop app: Open in player in the player hands the file to VLC or mpv, for files the window cannot play itself (HEVC without the Windows extension, AC-3, DTS). It works while the file is still downloading. ([#37](https://github.com/featherbitplatform/featherbittorrent/pull/37))

### Changed

- Playing a file (the web player, the desktop app's Open in player, or s in the terminal client) now raises it to High priority and downloads its pieces in order, so playback can start sooner; the torrent's other files keep their priorities. Built on fbt-lib 0.12.0. ([#38](https://github.com/featherbitplatform/featherbittorrent/pull/38))

### Fixed

- Remote client: the snapshot that says the link is lost - or that it came back - is published before the notice about it, so the two are seen together instead of the line arriving against the state from before.
- Desktop app: pressing Add torrent while Settings was open left the window unresponsive. It now goes back to the torrent list and opens the Add dialog there. ([#35](https://github.com/featherbitplatform/featherbittorrent/pull/35))
- Player: a video that fails to play now says why (the codec and the system's own message) instead of only "Playback failed.", hints at the HEVC Video Extensions on Windows, and keeps playing without sound when only the audio track cannot be decoded. ([#36](https://github.com/featherbitplatform/featherbittorrent/pull/36))

## [0.4.0] - 2026-11-24

### Added

- The desktop app, `featherbittorrent-desktop`: the web UI in a native window (Tauri 2) beside the terminal client, as its own package in the same release. It runs an engine of its own on this machine, `This computer`, with its own settings and library; it draws its own title bar with an instance switcher, and switches between `This computer` and remote instances added by address. A remote instance asks for a user name and a password once, and `Save password` keeps the password in the system keychain. The first close asks whether closing quits or keeps the app running in the tray, and the answer can be changed in Settings; where there is no tray, closing quits. `Start at login` is off until you turn it on. `magnet:` links and `.torrent` files open its Add page, with an instance picker, and a second launch hands them to the copy already running. `Serve the web UI` puts the web UI on an address of your choice for other machines, behind a login it asks you to create. Releases carry a `.dmg` for macOS (universal), an `.msi` for Windows (x86-64) and a `.deb`, an `.rpm` and a `.tar.gz` for Linux (x86-64 and aarch64), each with a `.sha256`; none of them is signed, and none installs or starts a service. The desktop app's Linux floor is glibc 2.35 (Ubuntu 22.04, Debian 12, Fedora 41), higher than the command line builds', and it needs WebKitGTK 4.1 and libsoup 3. Documented in `docs/desktop.md`. ([#30](https://github.com/featherbitplatform/featherbittorrent/pull/30))
- `server.cors_origins`: the origins of clients served from somewhere else that may call a server's API, empty by default. The desktop app connecting to a remote server needs its own origin listed, `tauri://localhost` on macOS and Linux and `http://tauri.localhost` on Windows. Listing an origin signs nobody in. Beside it, the server has a desktop access mode for the engine the desktop app runs itself: it accepts a launch secret made when the app starts, and exposes the configured `server:` section to the network only when `Serve the web UI` is on. Documented in `docs/server.md`. ([#30](https://github.com/featherbitplatform/featherbittorrent/pull/30))
- The web client signs in with a bearer token to any address, not only to the origin it was served from, which is how the desktop app reaches a remote server: the token is kept in memory, with no cookie and no CSRF header, and signing out ends the session on the server. The keyboard shortcuts can be changed in the desktop app (`Change` on a row, `Already used by <action>` for a key another action in the group holds, `Reset to defaults`), and the list stays read-only in a browser. ([#30](https://github.com/featherbitplatform/featherbittorrent/pull/30))

### Changed

- On macOS the desktop app keeps its Dock icon hidden when it starts in the background (at login, or with `--background`): only its menu bar icon shows. Showing the window, from the tray, a second launch, or a `magnet:` link or `.torrent` file opened with the app, puts the Dock icon back, and closing the window to the tray takes it away again. Where there is no tray the Dock icon never hides. Documented in `docs/desktop.md`. ([#32](https://github.com/featherbitplatform/featherbittorrent/pull/32))

### Fixed

- Dialogs (close, delete, add torrent, limits, move files, remove device and the player) now open centred in the window instead of in its top-left corner. This affected the web UI released in `0.3.0`. ([#30](https://github.com/featherbitplatform/featherbittorrent/pull/30))

## [0.3.0] - 2026-11-23

### Added

- A web UI in the browser, served by the full build's `serve` on the API's own origin: sign in with a user name and password (an `HttpOnly` cookie session with a CSRF token on every change), a live torrent table with filters and sorting, the detail pane (files, peers, trackers, pieces), pause, resume, remove keeping or deleting the files, and add a torrent by magnet link or `.torrent` file into a folder the server allows. The page refreshes on a one-second tick and wakes early on the server's event stream. Every release now ships two builds: the full build, `featherbittorrent`, which serves the web UI, and the headless build, `featherbittorrent-headless`, which carries no front end and changes nothing for the TUI. On Linux the two are packages that install the same binary and remove the other when they are installed: the `.deb` says so with `Conflicts` and `Replaces`, and the `.rpm` says so with `Obsoletes`. A full build's `server.web_ui: false` (or `FBTT_WEB_UI=0`) turns the UI off for a run. `featherbittorrent --version` says which build you have, and the off switch and browser sign-in are documented in `docs/web-ui.md`. ([#20](https://github.com/featherbitplatform/featherbittorrent/pull/20))

- A media player in the browser for video files: a `Play` button on each `.mkv`, `.mp4`, `.m4v`, `.webm` or `.mov` row of the Files tab opens it, and playback starts while the torrent is still downloading. It has seeking, an `Audio` menu to switch between audio tracks, a `Subtitles` menu for text subtitles inside the file and subtitle files beside it, fullscreen, and keys for play and pause, seeking, volume, mute, audio, subtitles and fullscreen. It remembers where you stopped in each file and starts there the next time. When the browser cannot play the video track, the player shows `This browser cannot play <codec> video.` and a `Download` button; a video-container file with no video track (for example an audio-only `.mkv`) is not played and gets the same `Download` button. Audio files such as `.mp3` and `.flac` get no `Play` button. Documented in `docs/web-ui.md`. ([#25](https://github.com/featherbitplatform/featherbittorrent/pull/25))

- The web UI's second milestone. The Files tab's `Priority` select sets a file to `skip`, `low`, `normal` or `high`. A `More` menu on each torrent has `Move up`, `Move down`, `Move to top` and `Move to bottom` for a queued torrent, `Recheck`, `Reannounce`, `Limits...` for a per-torrent download and upload limit, and `Move files...`, which never overwrites. The folder picker has a `New folder` button that makes one folder at a time and refuses unsafe names. A `Settings` screen, opened from the header, edits the server's rate limits and queue counts, lists, adds and removes devices by their `ssh-ed25519` public key (a private key is refused and never sent), and shows every keyboard shortcut. At 640 px wide or less the torrent list is cards, and a tapped torrent opens full screen with the browser's back returning to the list. Documented in `docs/web-ui.md`. ([#27](https://github.com/featherbitplatform/featherbittorrent/pull/27))

### Security

- The `url` library moves to 2.5.9 for RUSTSEC-2026-0592, where a URL with thousands of nested `blob:` prefixes could overflow the stack when its origin was taken. ([#24](https://github.com/featherbitplatform/featherbittorrent/pull/24))

## [0.2.3] - 2026-11-21

### Fixed

- The featherbit mark no longer shows the first letters of the text header through it when it appears after startup: the cells under a mark that has just appeared are rewritten blank before the image is drawn over them, so there is no old text left for the transparent image to let through. The help screen's mark now sits on the help box's own colour instead of a darker rectangle in the middle of the box. ([#17](https://github.com/featherbitplatform/featherbittorrent/pull/17))
- On Windows, a session listening on port 0 keeps its UDP socket (DHT, uTP, UDP trackers) on its TCP listen port. Separately, on every platform, a DHT node whose bootstrap ended with fewer than 8 routing-table entries now keeps retrying until its table holds 8. Both come with fbt-lib `v0.11.1`. ([#18](https://github.com/featherbitplatform/featherbittorrent/pull/18))

## [0.2.2] - 2026-11-20

### Fixed

- On Windows, the featherbit mark is now drawn as an image in terminals that support sixel and report their cell size (fbterm 0.11 or newer, for one), where before every Windows terminal got the text title. ([#13](https://github.com/featherbitplatform/featherbittorrent/pull/13))
- The terminal probe now runs in raw mode on macOS and Linux. In cooked mode the terminal's replies could not be read and were echoed onto the screen, so the text title showed. ([#13](https://github.com/featherbitplatform/featherbittorrent/pull/13))
- The featherbit mark is now drawn with a transparent background, so a terminal with a translucent or blurred background shows through around it instead of a dark box. ([#14](https://github.com/featherbitplatform/featherbittorrent/pull/14))

## [0.2.1] - 2026-11-20

### Fixed

- A client of `featherbittorrent serve` that sends a request body too slowly can no longer hold a connection forever: a body must arrive within 20 seconds, plus one second for every 500 bytes received, or the connection is closed, with a `408` answer when one can still be sent. The login and stream-token routes this server adds answer that `408` too, so a slow body gets the same answer from every route. This comes with fbt-lib `v0.11.0`. ([#10](https://github.com/featherbitplatform/featherbittorrent/pull/10))
- Logging in to a server with a body over the size limit now gets the 413 answer instead of a reset connection (seen as `ConnectionAborted` on Windows): both login routes read the refused body up to a bound before answering, as the stream-token route already did. ([#9](https://github.com/featherbitplatform/featherbittorrent/pull/9))
- A refused request clears only the session or gateway token it was sent with, so a late refusal of an older request no longer throws away a token a newer login just stored. ([#9](https://github.com/featherbitplatform/featherbittorrent/pull/9))
- With the image logo in a kitty terminal, the `Saving...` screen no longer writes any image bytes, and a zoom with the help screen open sends the large mark once per frame instead of twice. ([#9](https://github.com/featherbitplatform/featherbittorrent/pull/9))
- The no-metadata notice starts lower-case, like the other notices. ([#9](https://github.com/featherbitplatform/featherbittorrent/pull/9))

## [0.2.0] - 2026-11-20

### Added

- Press `s` to open a file in mpv or VLC while it downloads: the file under the cursor with the file list focused, otherwise the selected torrent's largest file. A file that is already whole plays by its path; anything else streams, over loopback from the local engine or from a server through a short-lived token scoped to that one file (`POST /api/v1/stream-tokens`). Behind a gateway that checks a credential of its own, the stream goes through a loopback relay that adds it. ([#6](https://github.com/featherbitplatform/featherbittorrent/pull/6))
- `player:` in `config.yaml`: `player.program` (`auto`, `mpv`, `vlc`, or a program name or path) and `player.args` for extra arguments put before the media. `auto` finds mpv, then VLC: `PATH` first, then where each is usually installed. ([#6](https://github.com/featherbitplatform/featherbittorrent/pull/6))
- The featherbit mark drawn as an image in the header and on the help screen in terminals that speak the kitty graphics protocol, iTerm2 inline images or sixel; the text title stays everywhere else, and `logo: off` in `config.yaml` turns the image off. ([#6](https://github.com/featherbitplatform/featherbittorrent/pull/6))
- `.deb` and `.rpm` packages for x86_64 and aarch64: Debian, Ubuntu and Raspberry Pi OS 64-bit take the `.deb`, Fedora and the RHEL 9 family the `.rpm`. Each carries gzipped man pages for every command, bash, zsh and fish completions, `LICENSE` and `NOTICE` and the docs, all generated from the same command definition as `--help` so they cannot drift from the flags, and installs nothing that starts by itself: no maintainer script, no service installed or enabled, no account created. ([#5](https://github.com/featherbitplatform/featherbittorrent/pull/5))
- The Windows `.zip` and the macOS and Linux `.tar.gz` archives now carry those completions, the docs and the service examples beside the binary, where 0.1.0's held only the binary with `LICENSE`, `NOTICE` and `README.md`. ([#5](https://github.com/featherbitplatform/featherbittorrent/pull/5))
- `docs/autostart.md` shows how to run `serve` at boot under systemd (a system unit and a user one), launchd or Windows Task Scheduler, with the example files for each under `packaging/`. Nothing installs or enables any of them: they ship as documentation, in `/usr/share/doc/featherbittorrent/examples/` inside the Linux packages and under `examples/` inside the archives. ([#5](https://github.com/featherbitplatform/featherbittorrent/pull/5))
- A `.sha256` sits beside every file on the release page, in `sha256sum`'s format, so a download is checked before it is run (`sha256sum -c`, or `shasum -a 256 -c` on macOS). ([#5](https://github.com/featherbitplatform/featherbittorrent/pull/5))

### Changed

- The engine is fbt-lib `v0.10.0`: a custom authenticator now sees the request path as it was received, which is what lets a stream token open exactly one file. ([#6](https://github.com/featherbitplatform/featherbittorrent/pull/6))
- Linux binaries now need glibc 2.34 or newer rather than 2.36: they are built inside an AlmaLinux 9 container instead of Debian 12, so the same binary runs on Ubuntu 22.04 and the RHEL 9 family as it did on Debian 12, Debian 13 and Raspberry Pi OS 12 and 13. ([#5](https://github.com/featherbitplatform/featherbittorrent/pull/5))

## [0.1.0] - 2026-11-16

### Added

- featherbittorrent is now a working local BitTorrent client: run `featherbittorrent` in a terminal and the engine runs inside that process, downloading and seeding with no daemon to install or keep running. ([#1](https://github.com/featherbitplatform/featherbittorrent/pull/1))
- Add a torrent from a magnet link or a `.torrent` file (`a`), choose the folder its files go to, pause and resume it (`p`), forget it keeping or deleting the files on disk (`d`), and change what gets downloaded of individual files (`+` and `-`). ([#1](https://github.com/featherbitplatform/featherbittorrent/pull/1))
- A torrent list with a detail pane (files, peers, trackers, pieces), sorting by column (`o`, `O`), filtering (`/`), a minimal one-line-per-torrent view (`m`), and download, upload and queue limits you can change while the app runs (`S`). ([#1](https://github.com/featherbitplatform/featherbittorrent/pull/1))
- The library comes back when you start it again: torrents, their files and priorities are restored from the state file in the data directory, which is also written every five minutes in case the run ends badly. ([#1](https://github.com/featherbitplatform/featherbittorrent/pull/1))
- `config.yaml` in the platform's config directory sets the download folder, listen port, rates, queue counts and cache size; a file that does not parse is kept aside as `config.yaml.bad` and the run starts on this library's defaults instead, while one that cannot be read at all stops the start with an error (exit 1). ([#1](https://github.com/featherbitplatform/featherbittorrent/pull/1))
- One library serves one run at a time: a second featherbittorrent pointed at the same data directory prints that the library is already in use, naming its directory, leaves the terminal alone and exits with code 3. ([#1](https://github.com/featherbitplatform/featherbittorrent/pull/1))
- `featherbittorrent serve` runs the library headless with fbt-api's HTTP API: a short wizard on the first run, logs to stderr starting with `listening on <address>`, a clean stop on Ctrl-C or SIGTERM, and a `server:` section of `config.yaml` (bind address, allowed roots, allowed host names, auth) documented field by field in `docs/server.md`. A TUI whose library a running server holds offers to connect to it instead of failing. ([#2](https://github.com/featherbitplatform/featherbittorrent/pull/2))
- Sign in to a server with a user name and password (Argon2id hashes, rate limited per client address) or with an Ed25519 device key that signs a challenge; sessions are bearer tokens that live an hour and slide with use. Logins are on by default for any non-loopback bind, and with them off the server answers only host names it recognizes, which is what keeps a rebound web page out. ([#2](https://github.com/featherbitplatform/featherbittorrent/pull/2))
- `featherbittorrent user set|remove|list` and `device add|remove|list` edit logins from the terminal, a signed-in client manages devices over the API (the TUI's `K` screen), and the connect screen (`c`) offers no login, password, key file, ssh-agent, gateway private key and gateway password. ([#2](https://github.com/featherbitplatform/featherbittorrent/pull/2))
- Pick a destination folder when adding or moving a torrent (`Ctrl+O`): the picker browses the server's disk through the API inside `server.allowed_roots`, so it lists folders where the files will actually land. ([#2](https://github.com/featherbitplatform/featherbittorrent/pull/2))
- `docs/reverse-proxy.md` covers running the server behind the featherbit gateway or another reverse proxy, three ways to combine gateway logins with featherbittorrent's own (TLS and rate limits only; gateway OIDC first; gateway only with auth off), the TUI's private-key and gateway-password methods, and why `/api/v1/events` and file streams must never be buffered; `docs/examples/gateway-oidc/` is a runnable stack of gateway, Authelia and server. ([#2](https://github.com/featherbitplatform/featherbittorrent/pull/2))
- The release workflow publishes archives on a `v*` tag (and refuses any other tag): a universal macOS `.tar.gz`, Linux `.tar.gz` for x86_64 and aarch64 built inside Debian 12 so they run on Raspberry Pi OS 12, Windows `.zip` for x86_64 and aarch64, each holding the binary beside `LICENSE`, `NOTICE` and `README.md`, plus a CycloneDX SBOM and release notes taken from this file. ([#2](https://github.com/featherbitplatform/featherbittorrent/pull/2))

[0.4.0]: https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.4.0
[0.3.0]: https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.3.0
[0.2.3]: https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.2.3
[0.2.2]: https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.2.2
[0.2.1]: https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.2.1
[0.2.0]: https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.2.0
[0.1.0]: https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.1.0
