# Streaming a file to mpv or VLC

`s` opens a file in a media player while the torrent is still downloading. It works from the local
engine and from a server, on the machine the TUI runs on: the player is started there, whatever
machine is doing the downloading.

## What `s` plays

- With the file list focused (after `Enter`), `s` plays the file under the cursor.
- In the torrent table, `s` plays the largest file of the selected torrent. Pad files are skipped; on
  a tie the earlier file wins. The common case is one video plus small extras.
- Nothing selected sends nothing. A torrent still fetching its metadata gets one line on the footer,
  `The torrent has no metadata yet`, and nothing else happens.

## Where the bytes come from

**A file that is already whole plays by its path.** The rule is: the torrent is seeding, this file's
priority is not "skip", every byte of it is verified, and it exists on disk at exactly that length.
Then the player gets the absolute path and streams nothing.

Everything else plays through a stream address. A piece can be verified and still sitting in the write
cache, and a skipped file can live inside another file's bytes; a path would point at data that is not
there yet, while the stream route reads both correctly and waits for pieces as it goes.

From the local engine the address is a private HTTP server on `127.0.0.1`, started at an OS-picked port
by the first `s` that needs one and kept for the run. It serves one route: the file-stream route, and
only with `GET` or `HEAD`. In `Host` it answers only its own `127.0.0.1:<port>` and `localhost:<port>`.
Every address carries a token scoped to the one file it names. Eight streams play at once; a ninth gets
`503`. A paused torrent whose data is missing still opens, with one extra line saying
`The torrent is paused: playback stops where the download does`.

## Playing from a server

A remote TUI asks `POST /api/v1/stream-tokens` (see [docs/server.md](server.md)) and gets the same
kind of address on the server: `/api/v1/torrents/<id>/files/<index>/stream?token=<t>`. The token rules
are the same there and on the local endpoint:

- A token must be used within 60 s of being issued, or it is gone. This bounds an address nobody ever
  opened.
- After that it stays alive while a request arrives at least every 30 minutes. A paused film survives;
  a film somebody stopped watching ends by itself.
- Never longer than 8 hours from the issue, however much it is used.

A player re-requests its address on every seek, so a single-use token cannot work: that is why keeping
it alive is the player's seeking. At most 64 tokens are live per server; when the store fills, the least
recently used token goes, never one a player is still on. Only a hash of each token is stored, and a
token opens exactly one file: another file, another spelling of the same file, or any other route is
refused. With logins off on the server there is no credential to hand out, and the address comes back
with nothing on it (see [docs/server.md](server.md)).

## Why the player gets a playlist file

The player is started with the path of a small M3U file, never with the stream address itself:

```
mpv --no-terminal --playlist=<data dir>/play/<random>.m3u
```

While a token lives it is a credential, and on every platform this product runs on another user can
read another process's arguments (`ps`). A playlist path names nothing secret. The file holds three
lines, the middle one being the address; its folder is `<data dir>/play`, made `0700` so only this user
can see the names in it, and each file is `0600`, readable only by this user (the two modes are Unix:
other platforms have no such bits). Its name is random, so two plays of one torrent are two files and
nothing about a name says which torrent it came from.

The file goes when the player exits, or 30 s after the launch, whichever comes first: 30 s is longer
than any player takes to read three lines and shorter than a pause at the start of a film. A run that
died before its sweep leaves files behind only until the next run sweeps playlists older than 12 h.

A file played by its path needs none of this: its path carries no secret, so it is passed directly.

## The gateway relay

A media player sends no headers. So when the TUI reaches its server through a gateway that checks a
credential of its own (a gateway key header or a gateway password, see
[docs/reverse-proxy.md](reverse-proxy.md)), an address on the server is something the player cannot
open: the gateway turns it away before the token is ever looked at.

For those setups the TUI starts a loopback relay and the player gets its address instead:
`http://127.0.0.1:<port>/relay/<token>`. The relay forwards `GET` and `HEAD` to the server's own stream
address with the gateway's credential added, and hands the answer back. What it will accept is bounded:

- It listens on loopback only, and in `Host` it answers only its own `127.0.0.1:<port>` and
  `localhost:<port>`.
- Its token opens exactly one server address, with the same 60 s, 30 min and 8 h life as a stream
  token, and at most 16 of them are live.
- Eight streams play at once; the ninth request gets `503` with `Retry-After: 1`.
- One `Range` header of at most 256 printable ASCII bytes is passed through, and only a single value;
  anything else reaches the server as no range, and the file is served whole. The player's other headers
  go nowhere.
- The server must answer the head of its response within 30 s, or the relay answers `504`. A body may
  run as long as the player keeps asking for it.
- Only statuses a player understands come back as they are: `200`, `206`, `404` and `416`. Everything
  else becomes a `502`. What is copied back beside the status is only what a player needs to seek:
  `Content-Type`, `Content-Length`, `Content-Range`, `Accept-Ranges` and `Content-Disposition`.
- It sends the gateway's credential, never the server's session token.

A target that signs straight to the server gets the server's own address, with no relay in between.

## Choosing the player

`player:` in `config.yaml`:

```yaml
player:
  program: auto        # auto (default), mpv, vlc, or a program name or path
  args: []             # extra arguments, put before the media
```

`auto` tries mpv first and VLC second. Everything on `PATH` is tried before anything found at an
install path, because `PATH` is where a package manager puts a player somebody installed on purpose.
The install paths it then tries:

| System | Paths tried after `PATH` |
| --- | --- |
| macOS | `/Applications/mpv.app/Contents/MacOS/mpv`, `/Applications/VLC.app/Contents/MacOS/VLC`, and the same two under `~/Applications` |
| Windows | `%ProgramFiles%\mpv\mpv.exe`, `%ProgramFiles%\VideoLAN\VLC\vlc.exe`, and VLC's 32-bit install in `%ProgramFiles(x86)%` |
| Linux | none: that is what `PATH` and a package manager are for |

`program: mpv` or `program: vlc` looks for that one player by the same rules. Anything else is taken as
it is spelled: a path that exists, otherwise a name on `PATH`. A program that is not there is an error
naming it. With nothing found at all the line says what to do:

```
No media player found. Install mpv or VLC, or set player.program in config.yaml.
```

`args` are put before the media, at most 32 of them and at most 1024 bytes each, because they end up on
a process's argument line. This product adds one option of its own to each player it knows: mpv gets
`--no-terminal` (this program is the terminal already), takes a playlist through `--playlist=`, and gets
a `--` before a plain path so that a file named `-weird.mkv` arrives as a name; VLC gets
`--play-and-exit`, so it is not left holding an empty window. An unnamed player gets its configured
arguments and the media, and nothing else.

The player starts with its own stdin, stdout and stderr closed (a player writing to this terminal would
fight the screen for it), in its own process group on Unix, and with no console window on Windows. A
Ctrl-C at the TUI is not delivered to the film. The footer then says one line about what was started,
`Opening <title> in mpv`. If nothing could be started, the footer says why instead, and no player runs.

## What happens on quit

- A file played by its path needs nothing: the player keeps the file open.
- Local stream addresses end with the run: the loopback endpoint stops before the engine does, so a
  film playing from the local engine stops where the TUI did.
- A stream from a server keeps playing after the TUI quits, until its token idles out (30 minutes
  without a request) or reaches its 8 h cap. Signing out of the server does not stop it either: the
  token is not the session.
- A relayed stream ends with the TUI: the relay goes when the connection does, and every body it was
  carrying ends with it.
- The player itself is never killed by quitting.

One thing to know about a slow player. VLC on macOS can take a moment to open, and the playlist file is
removed when this run quits. If you quit right after pressing `s`, the player may not have read the
playlist yet and then fails to open it. Give it a second before quitting.

## The logo

`logo: auto | off` at the top level of `config.yaml`. `auto` is the default.

With `auto` the TUI asks the terminal once, before the first frame, whether it can draw images. A
terminal that answers gets the featherbit mark as an image in the header and on the help screen; every
other terminal keeps the text title, exactly as before. Three ways to prove it, tried in this order and
the first match wins:

- **kitty graphics.** The terminal answers the protocol's query and names itself as one that can place
  an image with ordinary cells. The image is sent once and those cells stand in for it, so a redraw does
  not flicker.
- **iTerm2 inline images.** The terminal answers iTerm2's cell-size report, or names itself.
- **sixel.** The terminal lists four among its device attributes, which is how a terminal says it takes
  sixel. A terminal that answers the kitty query without naming itself lands here too.

The mark is built at exactly the pixel size of the cells it covers, so the terminal never scales it: no
cell size known means no logo, because a scaled mark is a blurry one. In the header it sits at the left,
three rows tall, with the three text lines beside it, and only when the header has those three rows. On
the help screen it stands centred above the key list, ten rows tall, or eight, or six, whichever fits.
The minimal view has no logo, and a dialog opened over the header takes the mark away for that frame.

The probe is skipped, and the text title stays, inside tmux and GNU screen (their answers belong to the
multiplexer, which rewrites or swallows them), with `TERM=dumb`, when stdin is not a terminal, on
Windows, and with `logo: off`. A terminal that never answers costs 300 ms of startup; once its answer
has started arriving the wait extends to one second.
