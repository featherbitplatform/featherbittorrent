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

## Streaming in the browser

Streaming and playing video in the browser are provided by Playing video below.

## Playing video

## Playing video

The web UI plays video and audio files in the browser.

- Supported **video containers** are Matroska, WebM and MP4; supported **video codecs** 
are H.264, HEVC and AV1.
- Supported **audio codecs** are AAC, Opus, FLAC, MP3, AC-3 and E-AC-3. Each audio 
codec plays where the browser can decode it.

### Starting playback

A `Play` button appears on rows in the Files tab whose path ends in `.mkv`, `.mp4`, 
`.m4v`, `.webm` or `.mov` (case-insensitive). Clicking it opens the player modal that 
shows the video, a seek bar, play controls and the menus.

VP8/VP9 video in a WebM container plays natively, without audio or subtitle choice.

### The player

- `Audio`: change between playable audio tracks while video plays.
- `Subtitles`: choose or turn off text tracks (embedded or as separate SRT/ASS/VTT 
files). Image-based subtitles like PGS or VobSub are shown disabled with the message 
`Image subtitles cannot be shown here.`

### Keys

`Space`, `k` play or pause; `Left`, `Right` move 5 seconds; `Up`, `Down` change 
volume by 0.1; `m` mutes; `a` selects the next playable audio track; `c` cycles 
through subtitles including Off; `f` enters fullscreen; `Escape` closes.

### Waiting state and partial download

When playback reaches a part of the file that has not arrived yet, the player shows 
`Waiting for data...` and keeps trying again, from the end of what it already has, 
until the data is there. Playback can start even while the torrent is still 
downloading.

### Fallback

For a file in an unsupported format, Download saves the file to your computer.
