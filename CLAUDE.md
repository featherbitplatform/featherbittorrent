# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

**featherbittorrent**: a BitTorrent client for the terminal, built on `fbt-lib` (`../fbt-lib`, released as `v0.10.0`). One binary, two roles: plain `featherbittorrent` runs a TUI that embeds the engine (or, from milestone 2, connects to one server), and `featherbittorrent serve` runs headless and is controlled over the network. Cargo workspace under `crates/`: `fbtt-core` (config, platform paths, the library lock, the client interface, the local backend over `fbt::Session`, the remote backend and its login methods), `fbtt-tui` (the model: keys in, actions out; plus ratatui rendering), `fbtt-server` (the HTTP server: fbt-api's routes plus this product's auth, device and fs routes, the Host guard when auth is off) and `featherbittorrent` (the binary that wires terminal, runtime, backends, the wizard and the `user`/`device` admin commands together). The screen side of a backend comes apart through `Client::split`: the command sender and the two receivers become fields the runtime owns, because a method on the client borrows the whole client and one `select!` branch cannot hold that borrow while another sends on it.

Status: milestone 1, the local client, is complete (branch `feature/m1-local-client`). Milestone 2, the server role (`serve`), the remote backend, logins (users and passwords, Ed25519 device keys, gateway private key and gateway password), the folder picker, the devices screen, the wizard and hand-off, `docs/server.md` and `docs/reverse-proxy.md` with a runnable gateway-Authelia example under `docs/examples/gateway-oidc/`, a root `Dockerfile` and the release workflow, is complete and carried the first release (`v0.1.0`). Every release bumps the workspace version on its own `release/X.Y.Z` branch. Milestone 4, packages and autostart docs, shipped in `v0.2.0`: the hidden `generate-assets` subcommand (man pages and shell completions written from the same `Command` as `--help`), `.deb` and `.rpm` for x86_64 and aarch64 plus Linux, macOS and Windows archives that carry the same files, the service examples under `packaging/`, the smoke scripts that check every artifact in a clean container or runner before anything is published, `docs/install.md` and `docs/autostart.md`; no package installs or enables a service, and removal keeps the user's config. Milestone 3, also in `v0.2.0`: streaming to mpv or VLC with `s` (a local path or a loopback stream, server stream tokens from `POST /api/v1/stream-tokens`, the loopback gateway relay), the `player:` settings, and the featherbit mark drawn as an image in the header and on the help screen (`logo: auto | off`, kitty graphics, iTerm2 inline images, sixel) with the terminal probe; `docs/streaming.md` describes it. Release `0.2.0` carried milestones 3 and 4, so the workspace version is `0.2.0`.

Binding design: `docs/superpowers/specs/2026-11-14-featherbittorrent-design.md`. Read it before any feature work; it records which decisions are the owner's and which are the builder's calls. The current plan is `docs/superpowers/plans/2026-11-19-milestone-3-streaming-and-logo.md`, and its Global Constraints are the rules below.

## Rules specific to this repo

- **Rust edition 2024, `rust-version = "1.99"`, stable.** No `unsafe` anywhere: `#![forbid(unsafe_code)]` at the top of every crate and the same lint at the workspace.
- **Dependencies** are declared once in `[workspace.dependencies]` and inherited with `dep.workspace = true`; crates never pin their own versions, and nothing is added outside that table. The engine comes from fbt-lib as `fbt = { git = "https://github.com/featherbitplatform/fbt-lib", tag = "v0.10.0", features = ["serde"] }`, pinned by tag (cargo-deny enforces the tag); `fbt-api` joins the table at that same tag when the server crate starts using it.
- **Tests never contact the public internet**, the public DHT, a real router or a real proxy: every test `Settings` sets `dht_enabled = false`, `enable_lsd = false`, `enable_upnp = false`, `enable_natpmp = false`, and `listen_addrs` on `127.0.0.1:0`.
- **Tests never need a real media player or an image-capable terminal.** The player a test starts is `fbtt-fake-player`: it appends the arguments it was given and the contents of the playlist it was handed to a log file, so "no secret on the player's command line" is a test rather than a manual check. No test runs mpv or VLC; no test reads a real tty.
- **Remote text is display-safe.** Torrent names, file paths, tracker messages, peer client names and error strings go through `fbt::display_safe(s, fbt::MAX_DISPLAY_CHARS)` before they are drawn.
- **The UI thread never awaits the network or the disk.** It redraws from the latest snapshot, only when the snapshot or the model changed, at most 30 frames per second.
- **Colours** come only from the Graphite tokens in `fbtt-tui`'s `theme.rs`: canvas `#0b0b0e`, selected row `#151518`, rules `#313135`, muted text `#919199`, body text `#b6b6bf`, strong text `#f5f5f7`, accent `#863bff`, accent light `#a584ff`, seeding `#2bbb71`, stalled `#f2a635`, connection `#59c5f5`, error `#ed4a49`.
- **Product copy** (UI strings, docs, README): no emoji and no em-dashes. Naming other torrent clients is allowed, since interop and migration are part of the domain.
- **No source file headers.** `LICENSE` and `NOTICE` at the root cover everything.
- **Remote input never panics, hangs or allocates without bound.** A server, a peer or a torrent file must not take the TUI or the server down.
- **Cross-platform.** Builds and tests pass on macOS, Linux and Windows.
- **Packages never install, enable or start a service** (the owner's rule): no maintainer scripts, nothing under `/lib/systemd`, `/usr/lib/systemd`, `/etc/systemd`, `/etc/init.d` or `/Library/Launch*`, and no service account created. The systemd, launchd and Task Scheduler examples live under `packaging/` and ship as documentation only: `/usr/share/doc/featherbittorrent/examples/` in the `.deb` and `.rpm`, `examples/` inside the archives. `check_no_service` in `dev/package-linux.sh` enforces it at build time and `dev/smoke-linux.sh` after an install, and a package owns nothing of the user's: `apt purge` and `dnf remove` leave the config and library where they are.
- **Linux artifacts need glibc 2.34 or newer**, built inside `almalinux:9` (RHEL 9's glibc, the oldest supported target: Ubuntu 22.04 and RHEL 9 up to Debian 13 and Raspberry Pi OS 13), so one binary serves the `.deb`, the `.rpm` and the `.tar.gz`. `dev/check-linux-binary.sh` refuses a binary that needs a glibc symbol newer than that floor or any `NEEDED` library outside `libc.so.6`, `libm.so.6`, `libgcc_s.so.1`, `libpthread.so.0`, `libdl.so.2`, `librt.so.1`, `ld-linux-x86-64.so.2` and `ld-linux-aarch64.so.1`; a new library goes into that allow list and into `[package.metadata.deb] depends` together.

## Conventions

- **Gitflow:** `main` holds tagged releases (`vX.Y.Z`) only; `develop` is the integration branch; work on `feature/<name>` off `develop` (milestone 1 works on `feature/m1-local-client`); `release/X.Y.Z` and `hotfix/<name>` as in the other featherbit repos.
- **Conventional Commits** with the crate as scope (`feat(core): ...`, `feat(tui): ...`, `ci: ...`, `chore: ...`).
- **No attribution of any kind:** no `Co-Authored-By` trailer, no `Claude-Session` line or session link in commits, no "Generated with" footer or session link in PR bodies.
- **Semantic versioning.** The workspace version is bumped on the `release/X.Y.Z` branch; the annotated `vX.Y.Z` tag on `main` triggers the release workflow.
- **Changelog:** `CHANGELOG.md` at the root (Keep a Changelog); user-visible changes go under `## [Unreleased]`, each ending in its PR link.
- **CI gates:** `cargo fmt --all --check` (Linux), `cargo clippy --workspace --all-targets --all-features -- -D warnings` and `cargo test --workspace --all-features` on Linux, macOS and Windows, `cargo doc` with warnings denied, cargo-deny, plus the security scans in `.github/workflows/security.yml` (semgrep, grype, gitleaks over the full history; also weekly on `main`). Their configs live at the repo root (`deny.toml`, `.semgrepignore`, `.gitleaks.toml`, `.grype.yaml`); every ignore carries a one-line reason. `--all-features` is what makes `fbtt-core`'s `local` test target run at all: it requires that crate's `test-support` feature, and a filtered-out test is a test nobody noticed. PRs merge only when every gate is green.

## Commands

Run `source "$HOME/.cargo/env"` first if cargo is not on PATH.

```
# The client: no subcommand runs the local TUI (it needs a real terminal; a second
# instance on a locked library prints one line and exits 3 without touching the tty)
cargo run -p featherbittorrent

# The same library with no screen: logs to stderr, one line per event, and the first
# line is exactly `listening on <addr>`. Ctrl-C or SIGTERM stops it. A config.yaml with
# a `server:` section means no wizard questions; without one it asks five (docs/server.md).
cargo run -p featherbittorrent -- serve

# Logins and devices in a server's config.yaml. A running server reads them when it
# starts, so restart it for a change to take effect. `device add` takes an
# `ssh-ed25519` public key line or a file holding one (stdin with no argument).
cargo run -p featherbittorrent -- user set alice
cargo run -p featherbittorrent -- user list
cargo run -p featherbittorrent -- user remove alice
cargo run -p featherbittorrent -- device add 'studio laptop' ~/.ssh/id_ed25519.pub
cargo run -p featherbittorrent -- device list
cargo run -p featherbittorrent -- device remove 'studio laptop'

# serve's two debug-build-only flags, hidden from help and compiled out of release
# builds: --stop-file PATH stops as soon as that file exists, and --offline keeps every
# socket on loopback with DHT, LSD, UPnP and NAT-PMP off. Tests and scripts use them.
cargo run -p featherbittorrent -- serve --offline

# The stand-in media player the play tests start instead of a real one: fbtt-fake-player --log FILE
# [--hold-ms N] [media...] appends one JSON line to FILE with every argument it was given and the
# contents of the playlist it was handed, then exits. No window, no codec, nothing over the network;
# it needs fbtt-core's test-support feature, and release archives build -p featherbittorrent only, so
# it is never shipped. Run it with no --log and it prints its usage line and exits 2.
cargo run -p fbtt-core --features test-support --bin fbtt-fake-player

# Release version helper (its test is `bash dev/test-release-version.sh`): it prints the
# workspace version and refuses a tag that does not match it; --notes prints that
# version's CHANGELOG.md section, which is what the release workflow publishes as notes.
bash dev/release-version.sh
bash dev/release-version.sh --notes

# The man pages and shell completions the packages install, written from the same Command as --help:
# <dir>/man holds one page per command, <dir>/completions the bash, zsh, fish and PowerShell files. The
# subcommand is hidden from --help and present in every build, because the packaging scripts make these
# by running the release binary itself.
cargo run -p featherbittorrent -- generate-assets /tmp/fbtt-assets

# Packaging checks that build nothing and ask nothing about the platform, so they run on any OS (the
# release workflow's verify job runs the first): dev/check-linux-binary.sh against objdump fixtures, the
# deb/rpm version mapping and package-linux.sh's argument refusals, then the service examples.
bash dev/test-packaging.sh
bash dev/test-service-examples.sh

# The release-safe CLI smoke: --version, --help, serve --help, serve --offline refused with exit 2 (that
# is what proves the artifact is a release build), then user set and user list against a scratch HOME.
# Build the binary first; Windows twin: ./dev/smoke-cli.ps1 -Binary <exe> -Version <v> -Scratch <dir>
cargo build --release -p featherbittorrent
bash dev/smoke-cli.sh target/release/featherbittorrent "$(bash dev/release-version.sh)" "$(mktemp -d)"

# The Linux packages (.deb, .rpm and .tar.gz, each beside its own .sha256), built on the glibc 2.34 floor.
# On Apple silicon this makes the aarch64 (Raspberry Pi) artifacts; the separate target dir keeps them
# apart from the macOS build. Needs Docker.
docker run --rm -v "$PWD:/src" -w /src -e CARGO_TARGET_DIR=/src/target/linux-docker almalinux:9 bash -c '
  dnf install -y -q gcc make git tar gzip binutils rpm cpio findutils &&
  curl --proto =https -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal &&
  . "$HOME/.cargo/env" &&
  cargo install --locked cargo-deb@3.8.0 cargo-generate-rpm@0.21.1 &&
  bash dev/package-linux.sh "$(bash dev/release-version.sh)" "$(uname -m)"'

# Then smoke each artifact in a clean container, as root: it checks the .sha256, installs with no
# recommends or weak deps, proves nothing was installed, enabled or started, removes the package and
# asserts the config it wrote survived. dev/smoke-linux.sh refuses to run outside a container or CI.
docker run --rm -v "$PWD:/src" -w /src debian:12 bash dev/smoke-linux.sh deb target/dist <version>
docker run --rm -v "$PWD:/src" -w /src fedora:41 bash dev/smoke-linux.sh rpm target/dist <version>
docker run --rm -v "$PWD:/src" -w /src ubuntu:22.04 bash dev/smoke-linux.sh tar.gz target/dist <version>

# The universal macOS archive, its .sha256 and the smoke on a fresh extraction (macOS only: lipo,
# codesign and plutil are Apple's own tools). Windows twin: ./dev/package-windows.ps1 -Version <v>
# -Target x86_64-pc-windows-msvc
bash dev/package-macos.sh "$(bash dev/release-version.sh)"

# Full gate, as CI runs it (fmt is Linux-only in CI; deny runs as cargo-deny-action)
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check

# Local security scans via Docker (semgrep, deny, grype, gitleaks); one tool:
# bash dev/sast.sh semgrep. Windows twin: ./dev/sast.ps1
bash dev/sast.sh

# Scratch config and data directories (added with the config code in Task 2). They win over the
# platform's config and data dirs, and they are what services and containers use as well: the systemd
# system unit and the LaunchDaemon variant keep a library under /var/lib/featherbittorrent with them,
# and the Dockerfile's documented run line passes them with -e (FBTT_CONFIG_DIR=/data/config and
# FBTT_DATA_DIR=/data/library); the image itself sets no ENV for either.
FBTT_CONFIG_DIR=/tmp/fbtt-config FBTT_DATA_DIR=/tmp/fbtt-data cargo run -p featherbittorrent
```
