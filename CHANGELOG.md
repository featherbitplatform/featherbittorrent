# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- featherbittorrent is now a working local BitTorrent client: run `featherbittorrent` in a terminal and the engine runs inside that process, downloading and seeding with no daemon to install or keep running. ([#PR](https://github.com/featherbitplatform/featherbittorrent/pull/PR))
- Add a torrent from a magnet link or a `.torrent` file (`a`), choose the folder its files go to, pause and resume it (`p`), forget it keeping or deleting the files on disk (`d`), and change what gets downloaded of individual files (`+` and `-`). ([#PR](https://github.com/featherbitplatform/featherbittorrent/pull/PR))
- A torrent list with a detail pane (files, peers, trackers, pieces), sorting by column (`o`, `O`), filtering (`/`), a minimal one-line-per-torrent view (`m`), and download, upload and queue limits you can change while the app runs (`S`). ([#PR](https://github.com/featherbitplatform/featherbittorrent/pull/PR))
- The library comes back when you start it again: torrents, their files and priorities are restored from the state file in the data directory, which is also written every five minutes in case the run ends badly. ([#PR](https://github.com/featherbitplatform/featherbittorrent/pull/PR))
- `config.yaml` in the platform's config directory sets the download folder, listen port, rates, queue counts and cache size; a file that does not parse is kept aside as `config.yaml.bad` and the run starts on this library's defaults instead, while one that cannot be read at all stops the start with an error (exit 1). ([#PR](https://github.com/featherbitplatform/featherbittorrent/pull/PR))
- One library serves one run at a time: a second featherbittorrent pointed at the same data directory prints that the library is already in use, naming its directory, leaves the terminal alone and exits with code 3. ([#PR](https://github.com/featherbitplatform/featherbittorrent/pull/PR))
