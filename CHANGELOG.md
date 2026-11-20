# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- The featherbit mark no longer shows the first letters of the text header through it when it appears after startup: the cells under a mark that has just appeared are rewritten blank before the image is drawn over them, so there is no old text left for the transparent image to let through. The help screen's mark now sits on the help box's own colour instead of a darker rectangle in the middle of the box. ([#17](https://github.com/featherbitplatform/featherbittorrent/pull/17))

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

[0.2.2]: https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.2.2
[0.2.1]: https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.2.1
[0.2.0]: https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.2.0
[0.1.0]: https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.1.0
