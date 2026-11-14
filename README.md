# featherbittorrent

featherbittorrent is a BitTorrent client built on [fbt-lib](https://github.com/featherbitplatform/fbt-lib), the from-scratch pure-Rust library that does the downloading and seeding. It runs in one of two roles: as a server (`featherbittorrent serve`), headless, downloading and seeding and controlled over the network; or as a TUI in the terminal that either runs the engine itself on the same machine or connects to one server. A typical setup is a Raspberry Pi at home running the server and saving to a NAS or another chosen storage, controlled from a laptop's TUI over the LAN or remotely through a reverse proxy. On the laptop itself the TUI is a complete client with no daemon behind it.

The clients that came before it (uTorrent, Deluge, qBittorrent) work, and their shape is the one people expect: a torrent list with a detail pane, a daemon and a thin client. featherbittorrent wants that shape snappier and lighter: keypresses that feel instant, redraws that stay smooth with hundreds of torrents, and memory and CPU use small enough for the server to run comfortably on a Raspberry Pi 2B.

Status: milestone 1 in progress, the local client. `serve`, the remote backend and streaming come with milestone 2. Nothing is released yet.

## Build and run

A stable Rust toolchain (edition 2024, `rust-version = 1.99`) is enough; `rust-toolchain.toml` picks it up with rustup.

```
cargo build --workspace
cargo run -p featherbittorrent
```

The binary today answers `--version` and says the terminal UI is not built yet: the TUI lands later in this milestone, along with `FBTT_CONFIG_DIR` and `FBTT_DATA_DIR` for pointing it at scratch directories.

## Gates

Every change runs the same gates locally and in CI:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
bash dev/sast.sh
```

CI runs fmt on Linux, clippy and tests on Linux, macOS and Windows, plus `cargo doc` with warnings denied. The security workflow runs semgrep, cargo-deny, grype and gitleaks over the full history, and weekly on `main`; `dev/sast.sh` (and `dev/sast.ps1` on Windows) runs the same scans locally through Docker. Tests never contact the public internet.

## Design

The binding design is [docs/superpowers/specs/2026-11-14-featherbittorrent-design.md](docs/superpowers/specs/2026-11-14-featherbittorrent-design.md). The plan for the current milestone sits next to it.

## License

Apache-2.0, see [LICENSE](LICENSE) and [NOTICE](NOTICE).
