---
slug: /
sidebar_position: 1
---

# featherbittorrent

featherbittorrent is a BitTorrent client built on [fbt-lib](https://github.com/featherbitplatform/fbt-lib), the from-scratch pure-Rust library that does the downloading and seeding. It wants the shape people know from uTorrent, Deluge and qBittorrent, a torrent list with a detail pane, a daemon and a thin client, but snappier and lighter: keypresses that feel instant, redraws that stay smooth with hundreds of torrents, and memory and CPU use small enough for the server to run comfortably on a Raspberry Pi 2B.

## Three ways to run it

- **In a terminal.** Plain `featherbittorrent` is a whole client with the engine running in the same process, and nothing to install beside the binary. It can also connect to one server. Pressing `s` on a file streams it to mpv or VLC while it downloads ([Streaming](streaming.md)).
- **As a server with a web UI.** `featherbittorrent serve` runs the library headless with its HTTP API, controlled over the network by the TUI or, in the full build, by a browser. A typical setup is a Raspberry Pi at home saving to a NAS, controlled from a laptop over the LAN or through a reverse proxy. See [Server](server.md), [Web UI](web-ui.md) and [Reverse proxy](reverse-proxy.md).
- **As a desktop app.** `featherbittorrent-desktop` is the same web UI in its own window, with an engine of its own on this machine, instances to switch between, a tray, start at login, `magnet:` links and `.torrent` files, and changeable keyboard shortcuts. See [Desktop app](desktop.md).

## Where to start

- [Install](install.md): which file to download for your system, how to check it and where installing puts its files. Every release ships a full build, which serves the web UI, and a headless build, which carries no front end. Installing never starts anything.
- [Desktop app](desktop.md): the desktop package, its install steps and everything you can turn on after them.
- [Web UI](web-ui.md): the browser UI of the full build, its sign-in and its off switch.
- [Server](server.md): `featherbittorrent serve`, its settings and its logins.
- [Reverse proxy](reverse-proxy.md): putting the server behind the featherbit gateway or another reverse proxy for TLS, throttling and further authentication.
- [Streaming](streaming.md): playing a file in mpv or VLC while it downloads.
- [Autostart](autostart.md): running `serve` at boot under systemd, launchd or Windows Task Scheduler, all of it opt-in.

Downloads are on the [releases page](https://github.com/featherbitplatform/featherbittorrent/releases), and what changed in each one is in the [changelog](/changelog).
