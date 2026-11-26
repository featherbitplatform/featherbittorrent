# The web UI in the full build

The full build of featherbittorrent serves a web UI in the browser beside the API. `serve` answers the
page on the API's own origin, so a browser pointed at the server's address signs in, sees the live torrent
table, and can pause, resume, remove and add torrents the way the TUI does. The headless build carries no
front end and answers no page: it is the build a server that only the TUI drives wants.

`featherbittorrent --version` says which build you have: `featherbittorrent <version> (full)` carries the
web UI, `featherbittorrent <version> (headless)` does not. A headless build that finds `server.web_ui` set
in its config logs one line and ignores it, because there is nothing to serve.

## The off switch

A full build serves the web UI by default. To turn it off for a run, set `server.web_ui: false` in
`config.yaml`, or set the environment variable `FBTT_WEB_UI` to one of the off words: `0`, `false`, `off`
or `no`. The environment wins over the file, and the on words (`1`, `true`, `on`, `yes`) work the other
way, so an environment set to `1` keeps the UI on even when the config says `false`. A value the switch
does not understand is treated as no answer, and the config is in charge.

## Browser sign-in

The browser signs in with a user name and password, the same accounts the TUI's password method uses
([docs/server.md](server.md)). A good sign-in answers with an `HttpOnly` cookie named `fbtt_session`
(`SameSite=Strict`, `Path=/`, a 7-day `Max-Age`) and a CSRF token the page sends back as `X-CSRF-Token` on
every state-changing request. The cookie never leaves the browser, and a request that changes something
without its CSRF token is refused. Sign out revokes the session and clears the cookie.

The cookie's `Secure` flag follows `server.cookie_secure` in `config.yaml`:

- `auto` (the default): the cookie is `Secure` only when the request arrived over https, which behind a
  proxy means when the proxy sends `X-Forwarded-Proto: https` (or `Forwarded: ...proto=https`).
- `always`: the cookie is `Secure` whatever the request says.

A proxy that terminates TLS but does not send `X-Forwarded-Proto` leaves the server believing the request
came over plain http, and under `auto` the cookie is then issued without `Secure`. That is not a sign-in
loop, because a browser sends a non-`Secure` cookie over https too; the cost is that the token is then
free to travel over plain http to this host as well, where anyone who can see that hop can read it.
Behind such a proxy, set `server.cookie_secure: always` so the cookie carries `Secure` whatever the
request says and the token travels on https alone.

The session behind the cookie is the server's own, and it outlives nothing the cookie promises: the
server forgets it after an hour without use (`SESSION_TTL` in `crates/fbtt-server/src/auth/sessions.rs`)
and on every restart, whatever the cookie's 7-day `Max-Age` says. A page that arrives with a cookie
whose session has gone simply asks for the password again.

## Where to serve it

Browsers do not scope cookies by port: two sites on the same host name but different ports share a cookie.
A server that runs the web UI on a port of its own on a host that also runs another application is telling
that other application the name of a cookie it can read, because the browser hands both of them the
`fbtt_session` cookie for that host. Serve the web UI on a host name of its own (its own domain, or a
subdomain), not on another application's host name, so the cookie belongs to one thing only.

## Behind the gateway

The web UI signs in at the server, so a gateway that stands in front of it must not put a credential of
its own in the way. The gateway-only setup in [docs/reverse-proxy.md](reverse-proxy.md) (its setup 3) is
the one for the browser: `server.auth.enabled` off, the server reachable only through the gateway, and
`server.allowed_hosts` holding the name the gateway rewrites the request to and the browser-facing
domain.

Setup 1 in that guide, a gateway that does TLS and rate limits and asks nothing of its own, serves the
web UI too: the page, its sign-in and its API calls all pass a gateway that authenticates nobody, and
the sign-in goes on answering at the server. The gateway rewrites `Host`, so the browser-facing domain
has to be in `server.allowed_hosts`. A browser that reaches the server at the server's own name, with
no rewriting proxy between, needs nothing listed: the sign-in and the cookie's changes accept an
`Origin` naming the very name the request arrived at, and with logins on that is safe because the
credential for a session is the password, not the name.

Setup 2 does not serve the web UI in this release. Its `/api/*` policy opens only to a gateway API key
or a bearer token, and the page sends neither, so every call the page makes, its own sign-in included,
is answered `401` at the gateway. For the browser that means setup 1 or setup 3.

A gateway that forwards its own `Authorization` header to featherbittorrent must run with
featherbittorrent's logins off. The server reads any `Authorization` header as its own credential, so a
gateway that sends the browser's bearer token onward would have the server try to check it as a login
and fail; the gateway-only setup is the one where the two do not collide. Setups 1 and 2 keep the TUI's
bearer and device-key logins working through the gateway. The web UI never signs in at the gateway:
where it is served, in setups 1 and 3, it signs in at the server.

## In the desktop app

The desktop app shows this same page, from its own bundle rather than from a server, in a native window: the
same React code and the same API client, with the desktop-only parts drawn onto it (its own title bar with
the instance switcher, the desktop settings, the editable shortcut list, the instance picker on the Add page)
loaded only when the page is running inside that window. A browser loads the same files and shows none of
them, and none of them costs it a byte more.

What the page does through the API it does the same way there. Only how it reaches the API differs, and only
in the credential. For `This computer`, the engine the app runs itself, the window calls a loopback address
with a secret made when the app launched. For an instance on another machine it signs in with a user name and
a password and keeps the bearer token that answers in memory: no cookie, no `X-CSRF-Token`, nothing written to
browser storage, so the cookie rules above do not apply to it and the session ends when the app closes.

A remote server has to allow that in two ways: its logins on, and the desktop app's origin in
`server.cors_origins`. A webview sends the origin of its own page on every call it makes to another address,
and the desktop app's is `tauri://localhost` on macOS and Linux and `http://tauri.localhost` on Windows. A
server with logins off answers those calls `403`, because with nobody signed in the name a request carries and
the origin it carries are the only credentials that server has. [docs/desktop.md](desktop.md) has both rules,
and what the app shows when a connection is refused for its origin.

`Serve the web UI` in the desktop app's settings puts this page on an address of its own for other machines,
where a phone signs in with the cookie session described above, while the app itself keeps using its private
loopback address. Video plays through the system's own webview, so where that webview cannot play a codec the
player shows the same `This browser cannot play` line and `Download` button it shows in a browser. In the
app, `Download` opens the system's save dialog and the app writes the file where you choose; the window
stays on the app.

## Playing media

The web UI plays video files in the browser, with no media player to install. Files that show a
`Play` button in the Files tab are those whose path ends with `.mkv`, `.mp4`, `.m4v`, `.webm` or
`.mov` (case-insensitive); padding files have no `Play` button. Pressing the button opens the
player modal and starts playback immediately; a torrent's pieces can still be downloading when
playback begins.

Pressing `Play` also puts that file first: it goes to High priority and its pieces download in order, so
playback can start sooner, while the torrent's other files keep their priorities; the `Priority` column
of the Files tab shows it, and changing it there changes it back.

### How a file is played

The player tries one of three ways in the order listed below:

1. **Native**: a WebM file whose video track is VP8 or VP9, when the browser also reports it can
   play WebM video. The player does not show its own menus or draw its own controls; the browser's
   built-in controls are on screen.

2. **Remuxed**: every other file whose video the browser can play. The server repackages its
   tracks, and the player's own controls, menus and keys are used.

3. **Unplayable**: when the browser cannot play any codec of the video track, the player shows the
   sentence `This browser cannot play <codec> video.`, naming the video track's codec, and a
   `Download` button. A video-container file with no video track at all (for example an
   audio-only `.mkv`) is not played either: the player shows `This file has no video track to
   play.` and the same `Download` button. Audio files such as `.mp3` and `.flac` get no `Play`
   button.

### Controls

The remuxed player shows the following controls by their exact labels:

- `Seek`: a range input that moves the playhead to any point in the file. While you drag it, it
  shows where you are going, and the playhead moves once, when you let go.
- `Play` and `Pause`: a toggle that starts and stops playback.
- `Back 5 seconds` and `Forward 5 seconds`: icon buttons that move the playhead 5 seconds back and
  forward.
- `Mute` and `Unmute`: an icon button that toggles the audio on and off.
- `Volume`: a slider that sets the volume from quietest to loudest.
- `Audio`: a menu that lists the audio tracks; a track the browser will not play is listed
  disabled with the suffix `: not playable here`.
- `Subtitles`: a menu that lists the available subtitles; `Off` turns subtitles off, and image
  subtitles appear disabled with the text `<name>: Image subtitles cannot be shown here.`
- `Fullscreen`: an icon button that toggles fullscreen mode. On an iPhone, which has no
  fullscreen for the page, it opens the video in the browser's own fullscreen player; there, the
  player's own subtitles and menus are not shown. Before the video has loaded, the iPhone
  refuses it and the button does nothing.
- `Close player`: a button that closes the modal and stops playback.

`Audio` appears whenever the file has at least one audio track. `Subtitles` always appears and
always includes `Off`.

### Keys

`Space` or `k` toggles play and pause; `ArrowLeft` or `ArrowRight` moves the playhead 5 seconds
back or forward; `ArrowUp` or `ArrowDown` changes the volume by 0.1; `m` toggles mute; `a` selects
the next playable audio track; `c` cycles through subtitles including `Off`; `f` toggles
fullscreen; `Escape` closes the player.

Play and pause, the arrow-key seek, `a` and `c` work only for a remuxed file; the volume keys,
`m` and `f` also work for a native file.

Keys do nothing while an input, such as the `Seek` or `Volume` slider, has focus, except `Escape`,
which still closes the player. While the `Audio` or `Subtitles` menu is open, the keys are the
menu's: the arrows move through its choices, `Enter` or `Space` picks one, and `Escape` closes
only the menu, not the player. A press outside an open menu closes it too. For a native file, the
browser's own controls keep `Space` and the arrow keys when they have focus.

### Remembered position

For a remuxed file, the position is saved every 5 seconds and when the player closes, once
playback is at least 10 seconds into the file. It is cleared when the position is within 30
seconds of the end of the file. When you reopen the file, playback starts at the saved position,
provided it is before the end of the file or the file's length is unknown.

### Waiting and errors

- `Waiting for data...`: the part of the file you are watching has not arrived, the server is
  busy or the connection dropped; the player retries by itself.

- `Playback failed.`: the engine could not continue; changing the audio track restarts playback.

### Known limits

Seeking into the last part of a file that has no cue index (a live recording, for example) can give
an empty stream, so the player shows `Waiting for data...` there.

## File priorities

The Files tab has a `Priority` column. Each row's priority is a select with `skip`, `low`, `normal`
and `high`. `skip` means the file is not downloaded.

## Torrent actions

The `More` menu beside Pause and Remove holds the less common actions for the selected torrent:

- `Move up`, `Move down`, `Move to top` and `Move to bottom` change the torrent's place in the queue.
  They show only for a torrent the queue manages; a torrent that is not in the queue has no place to
  move.
- `Recheck` checks the files on disk against the torrent again.
- `Reannounce` asks the trackers for peers now.
- `Limits...` opens a dialog with a download limit and an upload limit in KiB/s. Both limits are set
  together, and 0 means unlimited.
- `Move files...` opens a folder picker and moves the torrent's files into the folder you choose. A
  move never overwrites: if the destination already holds a file of the same name, the server refuses
  the move and the page shows its reason. `Moving...` shows beside the menu while a move runs.

A refusal from `Limits...` is shown inside the Limits dialog. A refusal from the other actions, or from a
move, is shown in the page's banner, and on a phone also in the torrent's full-screen view, under its
actions. Either way it is the server's own words. The next action the server accepts clears it.

From the keyboard, `↓`, `Enter` or `Space` on `More` opens the menu, the arrows, `Home` and `End` move
through it, and `Escape` closes it. `Tab` leaves the menu and closes it, as does moving the focus
anywhere else.

## New folder

The folder picker, used when adding a torrent and when moving files, has a `New folder` button. It asks
for a `Folder name` and makes that folder inside the folder you have selected, then lists it. It never
creates parent folders, and a name that already exists is refused. The server also refuses these names:

- an empty name, `.` and `..`;
- a name with a slash or a backslash;
- a name with one of `<`, `>`, `:`, `"`, `|`, `?` or `*`;
- a name that starts with a dot;
- a name with control or invisible characters, or with leading, trailing or repeated spaces;
- a name that ends in a dot or a space;
- a Windows device name such as `CON`, `NUL`, `COM1` or `LPT1`, with or without an extension;
- a name over 255 bytes.

## Settings

The `Settings` button in the header opens the Settings screen; `Back to torrents` returns to the list, and
so does the browser's back button or a phone's back gesture. It has three sections.

### Server

The five server settings are `Download limit KiB/s`, `Upload limit KiB/s`, `Active downloads`,
`Active seeds` and `Active total`. The rates are in KiB/s, and 0 or an empty field means unlimited.
`Save` applies them to the running server and writes them to the server's `config.yaml`, so they are
there again the next time it starts. A folder or a switch set here is saved the same way and kept
between runs.

### Devices

Devices are the SSH keys the TUI signs in with. To add one, give it a `Name`, paste an `ssh-ed25519`
public key line into `Public key` and press `Add`; the new device is listed with its fingerprint. A
private key is refused in the page and never sent. Each device row has a `Remove` button, which asks
for confirmation in a `Remove device` dialog; a removed device can no longer sign in. Changes are saved in the server's `config.yaml`. With logins off, the section
says `Device logins need the server's logins on.` instead.

### Keyboard shortcuts

A read-only list of every shortcut, in groups. In the desktop app the same section is editable: `Change` on a
row waits for the next key you press, a key another action in the group holds is refused with `Already used by
<action>`, and `Reset to defaults` puts the shipped keys back, as [docs/desktop.md](desktop.md) describes.

**Player**

- Play or pause: `Space`, `K`
- Seek back: `←`
- Seek forward: `→`
- Volume up: `↑`
- Volume down: `↓`
- Full screen: `F`
- Mute or unmute: `M`
- Next audio track: `A`
- Next subtitle track: `C`

**Torrent list**

- Select the focused torrent: `Enter`, `Space`
- Move to the next torrent: `↓`
- Move to the previous torrent: `↑`

**Detail tabs**

- Next tab: `→`
- Previous tab: `←`
- First tab: `Home`
- Last tab: `End`

**Menus**

- Open the menu: `↓`, `Enter`, `Space`
- Choose the focused option: `Enter`, `Space`
- Next option: `↓`
- Previous option: `↑`
- First option: `Home`
- Last option: `End`
- Close the menu: `Escape`

**Dialogs**

- Close the dialog: `Escape`

## On a phone

At 640 px wide or less the torrent list is a list of cards. The state filters are a row of chips above
the cards, and under them are the same name filter and sort as the table: a sort choice (`Server order` or
a column) and a `Reverse` button. The filter and the sort are kept when the window is resized between the
cards and the table.

Tapping a card opens that torrent full screen, and the browser's back returns to the list. While it is
open, nothing behind it can be reached; closing it puts the focus back on the card you opened.
