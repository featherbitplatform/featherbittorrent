# featherbittorrent

featherbittorrent is a BitTorrent client built on [fbt-lib](https://github.com/featherbitplatform/fbt-lib), the from-scratch pure-Rust library that does the downloading and seeding. It runs in one of two roles: as a server (`featherbittorrent serve`), headless, downloading and seeding and controlled over the network; or as a TUI in the terminal that either runs the engine itself on the same machine or connects to one server. A typical setup is a Raspberry Pi at home running the server and saving to a NAS or another chosen storage, controlled from a laptop's TUI over the LAN or remotely through a reverse proxy. On the laptop itself the TUI is a complete client with no daemon behind it.

The documentation is on the site at <https://featherbitplatform.github.io/featherbittorrent/>; the same guides are in [docs/](docs/).

The clients that came before it (uTorrent, Deluge, qBittorrent) work, and their shape is the one people expect: a torrent list with a detail pane, a daemon and a thin client. featherbittorrent wants that shape snappier and lighter: keypresses that feel instant, redraws that stay smooth with hundreds of torrents, and memory and CPU use small enough for the server to run comfortably on a Raspberry Pi 2B.

Status: milestone 1, the local client, is complete: `featherbittorrent` in a terminal is a whole
client, the engine runs in that process, and there is nothing to install beside the binary. Milestone 2,
the server role (`serve`), the remote backend, authentication and streaming, shipped the first release,
[0.1.0](https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.1.0);
[docs/server.md](docs/server.md) and [docs/reverse-proxy.md](docs/reverse-proxy.md) describe it. Release
[0.2.0](https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.2.0) adds milestones 3 and 4, and
[0.2.1](https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.2.1),
[0.2.2](https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.2.2) and
[0.2.3](https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.2.3) fix what came after them,
[0.3.0](https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.3.0) is the first release with the
web UI,
[0.4.0](https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.4.0) is the first release with the
desktop app, and
[0.4.1](https://github.com/featherbitplatform/featherbittorrent/releases/tag/v0.4.1) fixes what came after it.
Milestone 3 streams a file to mpv or VLC with `s` and draws the featherbit mark as an image in terminals
that can show one ([docs/streaming.md](docs/streaming.md)). Milestone 4 ships packages: `.deb` and `.rpm` for x86_64 and aarch64, one universal macOS `.tar.gz` and Windows `.zip`
files, each carrying man pages, shell completions, the licenses and the docs. The web UI, released in 0.3.0,
puts a browser UI in the full build: a sign-in, the live torrent table, the detail pane, add, pause, resume
and remove, a media player that streams video with audio and subtitle choices, file priorities, queue moves,
recheck, reannounce, per-torrent limits, moving files, a Settings screen for the server's limits, device logins
and the keyboard shortcuts, and a phone layout, with the off switch in `server.web_ui` and `FBTT_WEB_UI`
([docs/web-ui.md](docs/web-ui.md)). Every release now ships two builds: the full build, `featherbittorrent`,
which serves the web UI, and the headless build, `featherbittorrent-headless`, which carries no front end and
changes nothing for the TUI. Every artifact is built and smoke tested in a clean container or runner before
anything is published, and the release workflow publishes them all on a `v*` tag. The desktop app,
`featherbittorrent-desktop`, released in 0.4.0: the same web UI in its own window, with an engine of its
own on this machine (`This computer`), instances to switch between, a tray, start at login, `magnet:` links
and `.torrent` files, and changeable keyboard shortcuts, shipped as a `.dmg`, an `.msi`, `.deb`, `.rpm` and
`.tar.gz` beside the CLI builds ([docs/desktop.md](docs/desktop.md)).

## Install

Downloads are on the [releases page](https://github.com/featherbitplatform/featherbittorrent/releases), each
file beside its own `.sha256`. Every release ships two builds: the full build, `featherbittorrent`, which
serves the web UI, and the headless build, `featherbittorrent-headless`, which carries no front end.
[docs/install.md](docs/install.md) names both, says which file is for your system, how to check it and where
installing puts its files. Installing never starts anything: no package installs or enables a service, none
creates an account, and nothing runs after the install. To have `serve` start at boot under systemd, launchd
or Windows Task Scheduler, [docs/autostart.md](docs/autostart.md) has the commands, one at a time, all of it
opt-in. The web UI's sign-in, the off switch and where to serve it are in
[docs/web-ui.md](docs/web-ui.md). The desktop app is a package of its own in the same releases: its files,
its install steps and everything you can turn on after them are in [docs/desktop.md](docs/desktop.md), and
the last section of [docs/install.md](docs/install.md) covers its files beside these.

## Build and run

A stable Rust toolchain (edition 2024, `rust-version = 1.99`) is enough; `rust-toolchain.toml` picks it up with rustup.

```
# The headless build, today's binary:
cargo build -p featherbittorrent
# The full build, which serves the web UI. Build the front end first, because the web-ui feature
# embeds web/dist and the build refuses to start without it:
npm --prefix web ci && npm --prefix web run build
cargo build -p featherbittorrent --features web-ui
```

The two builds print which one they are: `featherbittorrent --version` says `(full)` for the build that
serves the web UI and `(headless)` for the one that does not.

Run it from a terminal emulator: it takes the screen (raw mode plus the alternate screen) and gives
it back when it quits. `featherbittorrent serve` runs the library headless with its HTTP API, first run
asked through a short wizard; on any network you do not own, put the [featherbit gateway](https://github.com/featherbitplatform/gateway) or
another API gateway or reverse proxy in front of it for TLS, throttling and further authentication. The
server's own guide is [docs/server.md](docs/server.md), and the full build's web UI, its sign-in and its
off switch are in [docs/web-ui.md](docs/web-ui.md).

## Usage

### Keys

| Key | What it does |
| --- | --- |
| `j` | next torrent |
| `k` | previous torrent |
| `g` | first torrent |
| `G` | last torrent |
| `End` | last torrent |
| `Page Down` | down a page |
| `Page Up` | up a page |
| `Tab` | next detail tab |
| `Shift+Tab` | previous detail tab |
| `o` | sort by the next column |
| `O` | reverse the sort |
| `p` | pause/resume |
| `s` | stream in mpv or VLC |
| `/` | filter |
| `m` | minimal |
| `a` | add |
| `d` | delete |
| `S` | settings |
| `c` | connect |
| `K` | devices |
| `?` | help |
| `Enter` | focus the file list |
| `+` | raise the focused file's priority |
| `-` | lower the focused file's priority |
| `Ctrl+O` | pick a destination folder |
| `Esc` | close a dialog |
| `y` | confirm quit |
| `q` | quit |

The arrow keys do what `j` and `k` do, and Ctrl-C means the same as `q`. A key is an action, not a shortcut for whichever widget has focus: `p` stops or starts the torrent under the cursor wherever the cursor is. A dialog owns the keyboard while it is up, so a `q` typed into a magnet field adds a letter instead of quitting; `Esc` closes the dialog. `q` asks before leaving when something is still downloading: press `q` again or `y` to confirm, any other key to cancel.

### Streaming to a player

`s` opens a file in mpv or VLC while it is still downloading: the file under the cursor with the file list focused, otherwise the selected torrent's largest file. A file that is already whole on this machine plays by its own path; anything else is streamed, over loopback from the local engine, or from a server through a short-lived token that opens exactly that one file. The player is handed a private playlist file rather than the address, so the token never shows in a process list, and mpv and VLC are found by themselves unless `player:` in `config.yaml` names one or adds arguments. Tokens, gateways and what keeps playing after you quit are in [docs/streaming.md](docs/streaming.md).

The header and the help screen draw the featherbit mark as an image in terminals that speak the kitty graphics protocol, iTerm2 inline images or sixel, and keep the text title everywhere else; `logo: off` in `config.yaml` turns the image off ([docs/streaming.md](docs/streaming.md)).

### The config file

Nothing to set up before the first run. It writes its own defaults into `config.yaml` in the platform's config directory and downloads into the machine's Downloads folder, or into `<data dir>/downloads` on a system that has none.

| System | Config file | Data directory (`session.state`, `library.lock`) |
| --- | --- | --- |
| Linux | `~/.config/featherbittorrent/config.yaml`, or `$XDG_CONFIG_HOME/featherbittorrent/config.yaml` | `~/.local/share/featherbittorrent`, or `$XDG_DATA_HOME/featherbittorrent` |
| macOS | `~/Library/Application Support/io.featherbit.featherbittorrent/config.yaml` | the same folder |
| Windows | `%APPDATA%\featherbit\featherbittorrent\config\config.yaml` | `%APPDATA%\featherbit\featherbittorrent\data` |

The file is plain YAML, small enough to edit by hand. A file that names one setting keeps every other one at its default:

```yaml
download_dir: /Users/you/Downloads   # where torrent contents go
listen_port: 6881                    # both IPv4 and IPv6 listen on this port
limits:
  download_kib: 0                    # KiB/s, 0 is unlimited
  upload_kib: 0                      # KiB/s, 0 is unlimited
  active_downloads: 3                # torrents downloading at once
  active_seeds: 5                    # torrents seeding at once
  active_total: 500                  # torrents running at once, either kind
cache_size_mib: 32                   # the engine's write cache, in MiB
```

Rates are whole KiB/s and the cache whole MiB, because that is how people state them. A queue count left out of the file takes the number above (the engine's own); write `null` for no limit. Both address families listen on the one port, and `0` asks the operating system for one instead of claiming 6881; a run that gets no listener at all prints why it could not start and exits 1.

Changing limits in the settings dialog (`S`) applies them to the running session and writes them back to this file, so they are still there next run. Everything else in the file survives that save unchanged.

A `config.yaml` that is not valid YAML does not stop the app: it is moved aside as `config.yaml.bad`, the run starts on the defaults, and one line about what was wrong appears on screen. The state file follows the same rule. An unreadable `session.state` becomes `session.state.bad` rather than being overwritten, and resume data that cannot all be turned back into torrents is copied to `session.state.bak` first, so the missing ones are not lost to the next save.

A `server:` section in the same file is what `featherbittorrent serve` acts on: the address its API
listens on, which folders the API may name, which host names it answers when nobody signs in, and who
has to sign in. The TUI reads none of it; every field, default and rule is in
[docs/server.md](docs/server.md).

### Scratch directories

Two environment variables point the app at a different library, which is how you try it without touching your own:

```
FBTT_CONFIG_DIR=/tmp/fbtt-config FBTT_DATA_DIR=/tmp/fbtt-data cargo run -p featherbittorrent
```

Each one replaces one of the two directories above, and both together mean no home directory is needed at
all. That is also what services and containers use them for, not only experiments: the systemd system unit and
the LaunchDaemon variant in [docs/autostart.md](docs/autostart.md) keep a service's library under
`/var/lib/featherbittorrent`, and the `Dockerfile`'s run line points both directories at the `/data` volume. An
empty value counts as unset rather than as "use whatever directory the app happens to run in".

### Two instances on one library

One library is used by one run at a time: `library.lock` in the data directory says which. A second featherbittorrent pointed at the same library answers before it touches the terminal, so you can read the reason instead of watching a blank screen blink:

```
Another featherbittorrent is already using this library (/Users/you/Library/Application Support/io.featherbit.featherbittorrent). Close it first.
```

It exits with code 3 and changes nothing. Other failures print one line and exit 1; a run that quit cleanly exits 0. SIGTERM (and Ctrl-C where the terminal does not report it as a key) quits without asking, because a signal is nobody who can answer a confirmation.

When the busy run is `featherbittorrent serve` on this library and this TUI last used the library
locally, the second run asks instead of only failing: `A featherbittorrent server is running on this
library. Connect to it? y/n`. Answering `y` connects to that server rather than starting an engine,
and the sign-in you complete there is borrowed and never written down; answering `n` gives the exit-3
line above. The lock and the hand-off are spelled out in [docs/server.md](docs/server.md).

## Gates

Every change runs the same gates locally and in CI:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
bash dev/sast.sh
```

CI runs fmt on Linux, clippy and tests on Linux, macOS and Windows, where both pass `--all-features`: that is what makes fbtt-core's backend tests run at all. It also runs `cargo doc` with warnings denied. The security workflow runs semgrep, cargo-deny, grype and gitleaks over the full history, and weekly on `main`; `dev/sast.sh` (and `dev/sast.ps1` on Windows) runs the same scans locally through Docker. Tests never contact the public internet.

## Design

The binding design is [docs/superpowers/specs/2026-11-14-featherbittorrent-design.md](docs/superpowers/specs/2026-11-14-featherbittorrent-design.md). The plan for the current milestone sits next to it. A worked-out deployment, gateway and provider stack you can run is [docs/examples/gateway-oidc/](docs/examples/gateway-oidc/).

## License

Apache-2.0, see [LICENSE](LICENSE) and [NOTICE](NOTICE).
