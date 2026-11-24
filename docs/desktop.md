# The desktop app

`featherbittorrent-desktop` is featherbittorrent in a window: the same web UI and the same API client a
browser uses, in an executable of its own beside the terminal client, plus the few things only a native app
can do. It runs the engine on this machine, it lives in the tray, it can start at login, it opens `magnet:`
links and `.torrent` files, its window draws its own title bar, it switches between instances, and its
keyboard shortcuts are yours to change. Nothing else is new: anything it shows that a browser also shows is
the web UI's own code ([docs/web-ui.md](web-ui.md)).

It carries the same version as the command line builds and ships in the same release, and it is its own
package: installing it does not replace `featherbittorrent` or `featherbittorrent-headless`, and it does not
share their library.

## Install

Every release puts these files on the [releases page](https://github.com/featherbitplatform/featherbittorrent/releases),
each beside its own `.sha256`, and [docs/install.md](install.md) says how to check one.

| System | The file |
| --- | --- |
| macOS 11 or newer, Apple Silicon or Intel | `featherbittorrent-desktop-<version>-macos-universal.dmg` |
| Windows, x86-64 | `featherbittorrent-desktop-<version>-windows-x86_64.msi` |
| Debian, Ubuntu or Raspberry Pi OS 64-bit | `featherbittorrent-desktop-<version>-linux-x86_64.deb`, or `-linux-aarch64.deb` |
| Fedora and family | `featherbittorrent-desktop-<version>-linux-x86_64.rpm`, or `-linux-aarch64.rpm` |
| Any other Linux with glibc 2.35 or newer | `featherbittorrent-desktop-<version>-linux-x86_64.tar.gz`, or `-linux-aarch64.tar.gz` |

Nothing here is signed with an Apple Developer ID or notarized, and the `.msi` is unsigned, the way this
project's other artifacts are. That costs one approved first run on macOS and one on Windows, below. The
app is installed the same way whatever the file: nothing is enabled, nothing is started, and nothing starts
at boot or login. What you can then choose to turn on, `Start at login`, is your setting, written when you
turn it on and taken away when you turn it off.

### macOS

Open the `.dmg` and drag `featherbittorrent.app` onto the `Applications` shortcut in the same window, then
eject the disk image. The app inside is named `featherbittorrent.app`, after the product, and the `.dmg`
around it is named for the package. The CLI stays an archive on macOS, so nothing here collides with it.

The first time you open it macOS says it cannot verify the app, because it is unsigned, and offers only
`Done` or `Move to Trash`. Choose `Done`, then approve the app once. Right-click it in `Applications`,
choose `Open`, and click `Open` in the dialog that follows. On macOS 15 and later that no longer skips the
check; open `System Settings`, then `Privacy & Security`, and click `Open Anyway` beside the line about
featherbittorrent, then `Open Anyway` again in the dialog after it. Either way the app opens normally from
then on, and `xattr -dr com.apple.quarantine "/Applications/featherbittorrent.app"` in a terminal clears
the check as well.

### Windows

Double-click the `.msi` and follow the installer. It installs for every user of the machine, so Windows asks
for administrator approval, and it adds a Start menu shortcut. The installer is unsigned, so SmartScreen may
show `Windows protected your PC`; choose `More info`, then `Run anyway`. Open the app from the Start menu.
The installer is also what registers the app for `magnet:` links and `.torrent` files.

### Debian, Ubuntu and family

```bash
sudo apt install ./featherbittorrent-desktop-<version>-linux-x86_64.deb
featherbittorrent-desktop --version
```

The `./` says "this file here", not "a package with that name from a repository". The package carries no
maintainer script, so nothing runs after the files are placed, and it conflicts with nothing the CLI ships:
it sits beside `featherbittorrent` and `featherbittorrent-headless`, which are a pair of their own. It puts
the binary in `/usr/bin` and a launcher in the applications menu, listed as `featherbittorrent`.

### Fedora and family

```bash
sudo dnf install ./featherbittorrent-desktop-<version>-linux-x86_64.rpm
featherbittorrent-desktop --version
```

Same: no scriptlets, so `rpm -qp --scripts` on the file prints nothing, and nothing is enabled or started.

### Any other Linux, the `.tar.gz`

```bash
sudo tar -C / -xzf featherbittorrent-desktop-<version>-linux-x86_64.tar.gz
featherbittorrent-desktop --version
```

The archive is a small system root, not a folder to unpack anywhere: extracted onto `/` it puts the binary in
`/usr/local/bin`, a launcher in `/usr/local/share/applications`, the icons under `/usr/local/share/icons` and
a `README-desktop.txt` beside the licenses in `/usr/local/share/doc/featherbittorrent-desktop`. The launcher
is listed as `featherbittorrent`, as it is from the `.deb` and the `.rpm`. The archive carries no dependency
metadata, so its binary needs the libraries in
[The Linux floor](#the-linux-floor) already installed, and the launcher is only a declaration: making the app
the one that answers `magnet:` links and `.torrent` files takes the two `xdg-mime` commands in
[Magnet links and torrent files](#magnet-links-and-torrent-files). The `.deb` and the `.rpm` declare the
libraries to your package manager and install a launcher with the same two types.

## This computer

`This computer` is the instance the app connects to when it opens, until you mark another one as the default:
torrents downloading on this machine, in an engine the app runs itself, in its own process. It is not a
featherbittorrent you started: a `serve` or a TUI on the same machine is a separate program on a separate
library, and you add it as an instance by its address like any other one.

Nothing about `This computer` is exposed by default. The engine opens no port to the network, and it serves
no web UI and no API to another machine. The window reaches it on a loopback port chosen when the app
launched, with a secret made when the app launched; another program on this machine can reach that port, but
it gets nothing from it without the secret.
[Serving the web UI from This computer](#serving-the-web-ui-from-this-computer) is the one switch that opens
a port at all.

One library is driven by one run at a time, as it is for the terminal client. Launching the app a second
time, by clicking its icon or a magnet link, is answered first: the running copy takes what was handed over,
shows its window, and the second launch exits without starting an engine. If something else already holds the
library, a second app pointed at the same directories through the two variables below, say, the window opens
with `featherbittorrent is already running.` and a `Quit` button, and quitting from there exits with code 3,
the terminal client's code for the same thing.

## Where the app's files live

The desktop app keeps its own config and data directories, beside but never shared with the terminal
client's. Its settings are `desktop.yaml`, in the config directory, and the engine `This computer` runs keeps
its own `config.yaml` beside it, so the config directory holds both. The data directory holds that engine's
`session.state` and `library.lock`. Downloads go to the machine's Downloads folder, or `<data dir>/downloads`
where there is none, exactly as for the terminal client.

| System | Config directory (`desktop.yaml`, `config.yaml`) | Data directory (`session.state`, `library.lock`) |
| --- | --- | --- |
| Linux | `~/.config/featherbittorrent-desktop`, or `$XDG_CONFIG_HOME/featherbittorrent-desktop` | `~/.local/share/featherbittorrent-desktop`, or `$XDG_DATA_HOME/featherbittorrent-desktop` |
| macOS | `~/Library/Application Support/io.featherbit.featherbittorrent-desktop` | the same folder |
| Windows | `%APPDATA%\featherbit\featherbittorrent-desktop\config` | `%APPDATA%\featherbit\featherbittorrent-desktop\data` |

Two variables point the app at another pair of directories, the way `FBTT_CONFIG_DIR` and `FBTT_DATA_DIR`
point the terminal client at one:

```bash
FBTT_DESKTOP_CONFIG_DIR=/tmp/fbtt-desktop-config FBTT_DESKTOP_DATA_DIR=/tmp/fbtt-desktop-library \
  featherbittorrent-desktop
```

An empty value counts as no value. The terminal client's two variables are not read here: pointing a service
or a container at another library does not move this one by accident, and a run that wants both moved sets
four variables.

`desktop.yaml` is short, plain YAML, and every setting in it has a default, so there is no first run to
answer questions about: no file is the defaults. It holds what you choose in the window, and nothing about
addresses, logins or folders; those are the library's own `server:` section, and serving follows it exactly
as `serve` follows it. A file that does not parse is shown as an error naming the file and what is wrong in
it, and nothing is written over it, because the app has no way of guessing what your bytes meant.

## Instances

The switcher in the title bar lists `This computer` first, then every instance you added, with a mark on the
one you are connected to. `Manage instances` opens the Instances section of Settings, where you add one,
remove one, and mark the default. Adding takes a name and an address: `http://192.168.1.20:8090` or
`https://bit.example` for a server on another machine, `http://127.0.0.1:8090` for a `serve` on this one,
which is another instance rather than `This computer`. An address is a scheme, a host and an optional port,
and nothing else: no path, no query, no user name in it. A name is at most 64 characters, it is unique
among the instances read without regard to case, and it is never `This computer`, which is the app's own
instance and is in the switcher already. An `https` address needs a certificate the operating system's certificate store trusts, which is what the app's
webview checks against, and there is no setting yet for trusting a private certificate authority.

The default is `This computer` until you mark another one, and the app connects to the default when it
starts. A default it cannot reach does not leave you with a stuck window: the app opens, says `Cannot reach
the server.`, and the switcher works so you can choose another. Removing the instance you had marked takes
the default back to `This computer`.

Switching is instant and it is total: the page is rebuilt against that instance, so the torrent table, the
settings and the folder picker are that instance's, and the torrents downloading on this machine are not
mixed in with the ones on the Pi.

### What a server on another machine has to allow

Two things about the other server have to be set before the desktop app can use it.

**Its logins have to be on.** The app gets a bearer token by signing in with a user name and a password, the
same accounts `featherbittorrent user set` writes. A server with logins off answers its API `403` to those
calls: with nobody signed in, the only credentials such a server has are the name a request says it came to
and the origin it says it came from, and a webview origin is neither. `server.auth.enabled` is on for every
bind that is not a loopback address, so a server you reach across a network already has logins on unless its
file says `enabled: false`.

**Your origin has to be in `server.cors_origins`.** The page runs inside the app's own webview, and a webview
sends the origin of the page on every call it makes to another address. The server answers that origin only
if it is on its list, and the list is empty by default. The desktop app's origin depends on the system it
runs on:

- `tauri://localhost` on macOS and Linux
- `http://tauri.localhost` on Windows

List the one for the system you use, or both of them if people on more than one system connect from the same
server. When a connection is refused for this reason the app shows the line with the origin for the system it
is running on, so you can copy it into the file:

```
Add this to the server's cors_origins: tauri://localhost
```

```yaml
server:
  bind: 0.0.0.0:8090
  cors_origins:
    - tauri://localhost
    - http://tauri.localhost
```

Spell each entry as a whole origin, scheme included. Listing an origin grants nothing by itself: it says this
webview may call, not that anyone who calls is signed in. Every key, every rule and what logins off means for
a name are in [docs/server.md](server.md).

## Saved passwords, and the keychain

A remote instance asks for a user name and a password the first time you connect to it. The sign-in is the
server's own password login, and the token it answers with lives in the page's memory: it is not a cookie, it
is not written to browser storage, and it is gone when the app closes.

Tick the `Save password` box on that form and the password itself goes into the operating system's keychain,
one entry per instance under the app's identifier `io.featherbit.featherbittorrent.desktop`. The app signs in
with it every time it connects to that instance, and you are asked nothing. Removing the instance removes its
keychain entry with it.

Where the system has no keychain, a Linux session with no Secret Service for example, nothing is saved and
the app says so beside the option instead of offering it: `This system has no keychain, so passwords are not
saved.` It asks for the password at every connect. It never falls back to a plain file, because a plain file
is where a password is read by whoever can read the file.

## The window

The app draws its own top bar: the instance switcher, the web UI's own header actions, and on Windows and
Linux its own minimize, maximize and close buttons. On macOS the native window buttons stay where they are
and sit on that bar. Drag an empty part of the bar to move the window, and double-click an empty part to
maximize or zoom. On Linux the setting `Use the system title bar` hands the title bar back to
your desktop, drawn buttons included, which is what you want on a desktop that refuses to move undecorated
windows. That setting exists on Linux only: Windows always draws its own bar and macOS always keeps its own
window buttons. The window has a minimum size of 900 by 600, and you resize it from its edges.

## Closing the window, and the tray

The first time you close the window the app asks, once: `What should closing the window do?`, with
`You can change this later in Settings.` under it and two buttons, `Close completely` and
`Keep running in the tray`. Your answer is saved and the question is never asked again. Settings' Desktop
section changes the answer later under `Close the window`.

Keeping running means the tray. Its menu is `Show featherbittorrent`, a line with the name of the instance the
app is connected to (a label, not a button), and `Quit`. On macOS and Windows clicking the tray icon shows the
window as well; on Linux the menu's `Show featherbittorrent` is the way, because Linux tray hosts send the
app no click. On macOS, clicking the Dock icon while the window is hidden brings it back too. Quitting from
the tray, from `Cmd+Q`, or by logging out or shutting down stops `This computer` cleanly: the last save runs,
and your torrents resume when the app runs next, the way the terminal client's do when you quit it.

Where the desktop offers no tray there is nothing to hide a window behind, so `Keep running in the tray` is
not offered, the first-close question is not asked, and closing the window quits: the app never hides its
window with no way back. macOS and Windows always have a tray. On Linux the app asks the session bus once at
startup whether a StatusNotifier host is running (the name `org.kde.StatusNotifierWatcher` has an owner), and
it also needs the AppIndicator library to draw the icon; with no host, no session bus or no library it runs
without a tray and says why on stderr. KDE has a host of its own. Stock GNOME draws no status icons, so it
needs the AppIndicator shell extension, which is what provides one. Neither the `.deb` nor the `.rpm`
declares the AppIndicator library as a dependency; [The Linux floor](#the-linux-floor) names it.

## Start at login

`Start at login` is off by default. Turning it on writes the entry your desktop uses for you, when you turn
it on, and removes it again when you turn it off: a LaunchAgent in your `~/Library/LaunchAgents` on macOS, a
value in your `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` key on Windows, an autostart file in
`~/.config/autostart` on Linux. No package installs anything that starts at boot or login, and none runs
anything at install time, so this setting is the only thing that makes this app start itself, and only
because you asked it to.

The entry launches the app with `--background`, so a start at login is quiet: the app connects to its default
instance and sits in the tray with no window, or with its window minimized where there is no tray.
`Show featherbittorrent` in the tray menu brings it back, and so does starting the app again, which hands the
running copy a window rather than starting a second engine.

The autostart guide for the server, [docs/autostart.md](autostart.md), is a different thing: it is about
running `serve` before anybody logs in, under systemd, launchd or Task Scheduler. This setting is about your
own session.

## Magnet links and torrent files

The `.dmg`, the `.msi`, the `.deb` and the `.rpm` register the app as the handler for `magnet:` links and
`.torrent` files. Opening either one shows the Add torrent page, and the desktop app's Add page has an
instance picker beside the folder picker: the instance already preselected is the one the app was connected
to, or the default when the app was not already running. Changing the instance reloads the folder picker with
that instance's allowed folders, and if that instance needs a sign-in the page asks for it right there. The
link or file stays on the page until you add it or press `Cancel`, so a sign-in that goes wrong does not
throw it away. A `.torrent` bigger than 10 MiB is refused, with the reason on the page.

Opening a link or a file while the app is already running does not start a second engine: the launch hands
what it was given to the running copy, brings its window to the front, and exits.

### Making the `.tar.gz` the default handler

The archive's launcher declares `x-scheme-handler/magnet` and `application/x-bittorrent`, but a declaration
is not a choice: say which application answers the two types, and refresh the desktop's cache of launchers
where it keeps one:

```bash
xdg-mime default featherbittorrent-desktop.desktop x-scheme-handler/magnet
xdg-mime default featherbittorrent-desktop.desktop application/x-bittorrent
sudo update-desktop-database /usr/local/share/applications
```

The launcher is `/usr/local/share/applications/featherbittorrent-desktop.desktop` once the archive is
extracted onto `/`, as [Install](#any-other-linux-the-targz) says. To keep everything in your home instead,
copy the binary, the launcher and the icons under `~/.local`, and change the launcher's `Exec` line to the
binary's new path, keeping the `%u`: that field is how a clicked link reaches the app.

## Serving the web UI from This computer

`Serve the web UI` is off by default, and turning it on is the only time this app opens a port to the
network. It puts `This computer`'s web UI and API on an address you choose, `0.0.0.0:8090` if you leave it
alone, so a phone on the same network opens the page you are looking at and signs in to the torrents
downloading on this machine.

Turning it on asks for a user name and a password, because a port other machines reach is a port with logins:
they become a login in the library's `config.yaml`, in `server.auth.users`, hashed with Argon2id rather than
written as you typed them, which is what `featherbittorrent user set` writes for a server. Everything else
stays the server's own rules in that same file: `server.allowed_hosts` for a name behind a proxy,
`server.allowed_roots` for the folders the API may name, `server.web_ui` and `server.cookie_secure` for what
the browser side does ([docs/server.md](server.md), [docs/web-ui.md](web-ui.md)). The section shows the
address the page is reachable at, and while the address listens on more than loopback it warns that passwords
travel unencrypted over `http`: for anything beyond your own network, put the page behind `https`.
If `config.yaml` is edited later to turn logins off (`server.auth.enabled: false`) or to remove every user,
the next start serves nothing: the window gets its engine as usual, and the section says why and turns the
setting off until you turn it on again with a user name and a password.

The app itself is unaffected either way: its window keeps reaching the engine on the private loopback port
with its own secret, port open or closed, and the sign-in you complete in a browser is the browser's own.
Turning the setting off closes the port again. Applying it restarts the engine, and the app reconnects to it;
the torrents are still there afterwards.

## Changing keyboard shortcuts

In the desktop app, Settings' Keyboard shortcuts section is editable, because keys are the way the terminal
client is driven and a window should not be worse. A row's `Change` waits for the next key you press and
records that for the row; `Escape` cancels. A combination another action in the same group already answers to
is refused, and the refusal names the action holding it: `Already used by <action>`. `Reset to defaults`
puts every row back to the keys this release ships with.

Your overrides are kept in `desktop.yaml` and applied to the one list of bindings the app works from, so the
player, the menus and the list in Settings all follow them, and what the list shows is the keys in effect.
The same list in the web UI, in a browser, stays read-only.

## The Linux floor

The desktop app's Linux files are built on Ubuntu 22.04, against its glibc, 2.35, and every release installs
and runs them in clean containers of Ubuntu 22.04 and Debian 12 (the `.deb`) and Fedora 41 (the `.rpm`), with
the `.tar.gz` on Ubuntu 22.04. Systems newer than those are expected to work and are not tested yet. RHEL 9 and
its rebuilds, AlmaLinux and Rocky, are not supported for the desktop app: Tauri 2 needs WebKitGTK 4.1
together with libsoup 3, and that pair is not what RHEL 9 ships as supported. The terminal client keeps its
own, lower floor, so `featherbittorrent` and `featherbittorrent-headless` still run on RHEL 9 and from
Ubuntu 22.04 up to Debian 13.

Both architectures the CLI builds for, `x86_64` and `aarch64`, are built for this app too. The `.deb` and the
`.rpm` declare what the binary links and your package manager pulls it in. The `.tar.gz` declares nothing, so
these have to be installed already:

| What it is | Debian and Ubuntu | Fedora |
| --- | --- | --- |
| WebKitGTK 4.1, the webview | `libwebkit2gtk-4.1-0` and `libjavascriptcoregtk-4.1-0` | `webkit2gtk4.1` |
| GTK 3 and the libraries under it | `libgtk-3-0`, `libgdk-pixbuf-2.0-0`, `libglib2.0-0`, `libpango-1.0-0`, `libcairo2`, `libcairo-gobject2` | `gtk3`, `gdk-pixbuf2`, `glib2`, `pango`, `cairo` |
| libsoup 3 | `libsoup-3.0-0` | `libsoup3` |
| D-Bus | `libdbus-1-3` | `dbus-libs` |

The tray's AppIndicator library, `libayatana-appindicator3-1` on Debian and Ubuntu and
`libayatana-appindicator-gtk3` on Fedora, is loaded when the tray is built rather than linked, so no package
here lists it. Install it with any of the packages if you want the tray; without it the app runs, has no tray
icon, and closing its window quits. On GNOME the tray also needs the AppIndicator shell extension, as above.
