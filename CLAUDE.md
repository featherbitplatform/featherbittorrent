# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

**featherbittorrent**: a BitTorrent client for the terminal, built on `fbt-lib` (`../fbt-lib`, released as `v0.9.0`). One binary, two roles: plain `featherbittorrent` runs a TUI that embeds the engine (or, from milestone 2, connects to one server), and `featherbittorrent serve` runs headless and is controlled over the network. Cargo workspace under `crates/`: `fbtt-core` (config, platform paths, the library lock, the client interface and the local backend over `fbt::Session`), `fbtt-tui` (the model: keys in, actions out; plus ratatui rendering) and `featherbittorrent` (the binary that wires terminal, runtime and backend together). The screen side of a backend comes apart through `Client::split`: the command sender and the two receivers become fields the runtime owns, because a method on the client borrows the whole client and one `select!` branch cannot hold that borrow while another sends on it.

Status: milestone 1, the local client, is complete on `feature/m1-local-client`: the scaffold and gates, the config, the platform paths, the library lock, the local backend (the client interface plus the engine over `fbt::Session`) and the TUI over them. Next is milestone 2: `serve`, the remote client, authentication, and the first release (`0.1.0`). The workspace version (`0.0.0`) is bumped on the first `release/X.Y.Z` branch.

Binding design: `docs/superpowers/specs/2026-11-14-featherbittorrent-design.md`. Read it before any feature work; it records which decisions are the owner's and which are the builder's calls. The current plan is `docs/superpowers/plans/2026-11-14-milestone-1-local-client.md`, and its Global Constraints are the rules below.

## Rules specific to this repo

- **Rust edition 2024, `rust-version = "1.99"`, stable.** No `unsafe` anywhere: `#![forbid(unsafe_code)]` at the top of every crate and the same lint at the workspace.
- **Dependencies** are declared once in `[workspace.dependencies]` and inherited with `dep.workspace = true`; crates never pin their own versions, and nothing is added outside that table. The engine comes from fbt-lib as `fbt = { git = "https://github.com/featherbitplatform/fbt-lib", tag = "v0.9.0", features = ["serde"] }`, pinned by tag (cargo-deny enforces the tag); `fbt-api` joins the table at that same tag when the server crate starts using it.
- **Tests never contact the public internet**, the public DHT, a real router or a real proxy: every test `Settings` sets `dht_enabled = false`, `enable_lsd = false`, `enable_upnp = false`, `enable_natpmp = false`, and `listen_addrs` on `127.0.0.1:0`.
- **Remote text is display-safe.** Torrent names, file paths, tracker messages, peer client names and error strings go through `fbt::display_safe(s, fbt::MAX_DISPLAY_CHARS)` before they are drawn.
- **The UI thread never awaits the network or the disk.** It redraws from the latest snapshot, only when the snapshot or the model changed, at most 30 frames per second.
- **Colours** come only from the Graphite tokens in `fbtt-tui`'s `theme.rs`: canvas `#0b0b0e`, selected row `#151518`, rules `#313135`, muted text `#919199`, body text `#b6b6bf`, strong text `#f5f5f7`, accent `#863bff`, accent light `#a584ff`, seeding `#2bbb71`, stalled `#f2a635`, connection `#59c5f5`, error `#ed4a49`.
- **Product copy** (UI strings, docs, README): no emoji and no em-dashes. Naming other torrent clients is allowed, since interop and migration are part of the domain.
- **No source file headers.** `LICENSE` and `NOTICE` at the root cover everything.
- **Remote input never panics, hangs or allocates without bound.** A server, a peer or a torrent file must not take the TUI or the server down.
- **Cross-platform.** Builds and tests pass on macOS, Linux and Windows.

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

# Full gate, as CI runs it (fmt is Linux-only in CI; deny runs as cargo-deny-action)
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check

# Local security scans via Docker (semgrep, deny, grype, gitleaks); one tool:
# bash dev/sast.sh semgrep. Windows twin: ./dev/sast.ps1
bash dev/sast.sh

# Scratch config and data directories (added with the config code in Task 2). These
# are the only debug knobs: they win over the platform's config and data dirs.
FBTT_CONFIG_DIR=/tmp/fbtt-config FBTT_DATA_DIR=/tmp/fbtt-data cargo run -p featherbittorrent
```
