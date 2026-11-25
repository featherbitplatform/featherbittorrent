# Running `featherbittorrent serve`

`serve` runs the library and its HTTP API with no screen. Everything it has to say goes to stderr as
logs, one line per event; the first line is exactly `listening on <address>`, which is what a script
or a container healthcheck waits for. Ctrl-C and SIGTERM both mean stop: the API closes, the last
save runs, and the library lock is released. Exit codes are the same as everywhere else in this
product: 0 stopped cleanly, 1 something went wrong (the line says what), 2 clap could not read the
command line, and 3 another featherbittorrent holds the library.

The API speaks plain HTTP. This server holds no certificate; TLS, throttling and any further
authentication on an untrusted network belong to a gateway or reverse proxy in front of it, as
[docs/reverse-proxy.md](reverse-proxy.md) explains. On the connecting side the same care runs the
other way: an address that starts with `https` is verified against the public root certificates, and
there is no setting yet for trusting a private certificate authority of your own.

To have `serve` running before anybody logs in, or restarting itself after a crash, put it under a service
manager: [docs/autostart.md](autostart.md) walks through systemd (a system unit and a user one), launchd and
Windows Task Scheduler one command at a time. Nothing this project ships installs or enables any of them; the
example files ship as documentation, in `/usr/share/doc/featherbittorrent/examples/` inside the Linux packages
and under `examples/` inside the archives, and the guide names the directory to copy each one to.

## The first run: the wizard

`serve` asks these before anything is on disk, and writes the answers to `config.yaml`:

```
Download folder [/home/you/Downloads]:
BitTorrent port [6881]:
API address [127.0.0.1:8090]:
Require username and password for non-local devices? [no]:
User name (empty for none):
```

An empty answer takes the bracketed default, which is what this library already does, so taking every
default changes nothing an existing run would not have done anyway. A bad answer is said out loud and
asked again, up to three times; then `serve` prints the complaint it last heard and exits 1 without
writing anything. Name a user and its password comes next (`Password [hidden]:`, then `Repeat password
[hidden]:`); leave the name empty and the wizard says so instead: `No login yet. Create one with:
featherbittorrent user set`.

The last question is asked whatever the answers above were, logins off included: `Add a device key now?
[no]:`. Say yes and paste an `ssh-ed25519 ... [comment]` line; the comment becomes the device's name when
it is usable, and when it is not (too long, or something invisible in it) you are asked for a name of your
own. Answering `no`, or pasting nothing, adds no device key.

The wizard runs only when there is no `config.yaml`, when the file has no `server:` section (one
written by the TUI never has), or when its bytes could not be read. A config that parses and names a
server is used as it stands. The library lock comes first, before any of these questions: while another
run holds the library, `serve` neither asks anything nor writes anything, and exits 3 with the usual
"already using this library" line.

## The config file

Everything below is what `config.yaml` accepts; every field has a default, so naming one setting keeps
the rest as they are. Rates are whole KiB/s and the cache whole MiB because that is how people state
them. A file that cannot be parsed is moved aside as `config.yaml.bad` and the run starts on defaults,
with the problem logged after the first line; a file that cannot be read at all stops the run (exit 1).

Keys this version does not know are kept when the app rewrites the file. Comments are not kept. So a
setting a newer version wrote into this file survives a save made by this one, and a note you left
beside a setting is gone the first time the app saves it - keep your own copy of an annotated file, or
keep the annotations somewhere else.

| Field | Default | What it means |
| --- | --- | --- |
| `download_dir` | the machine's Downloads folder, or `<data dir>/downloads` where there is none | Where torrent contents go. A relative value like `dl` means below the library directory and is made absolute at startup. |
| `listen_port` | `6881` | The BitTorrent port, both address families on the one port. `0` asks the operating system for one. |
| `limits.download_kib` | `0` | Download rate in KiB/s; 0 is unlimited. |
| `limits.upload_kib` | `0` | Upload rate in KiB/s; 0 is unlimited. |
| `limits.active_downloads` | the engine's own count | Torrents downloading at once; `null` for no limit. |
| `limits.active_seeds` | the engine's own count | Torrents seeding at once; `null` for no limit. |
| `limits.active_total` | the engine's own count | Torrents running at once, either kind; `null` for no limit. |
| `cache_size_mib` | `32` | The engine's write cache, in MiB. |
| `downloads.unfinished_dir` | the download folder | Where a torrent's files live until it is whole, so a partial torrent is not mixed in with finished ones. A relative value means below the library directory, as `download_dir` does. It must be a different folder from `download_dir`. |
| `downloads.start_paused` | `false` | Whether a new torrent starts paused: nothing arrives until you start it. |
| `downloads.watch_dir` | none | A folder whose `.torrent` files are added as they appear. A relative value means below the library directory. It must be a different folder from `download_dir` and from `downloads.unfinished_dir`. |
| `downloads.on_complete` | none | A program to start when a torrent finishes, named by its whole path: a relative value or a bare name is not started, refused with a warning line, because which file either one means depends on where the server was started. It is handed the torrent's name and its folder as two separate arguments, and the app does not wait for it. |
| `server.bind` | `127.0.0.1:8090` | The address and port the API listens on, as `<ip>:<port>`. `0.0.0.0:8090` serves every interface this machine has. |
| `server.allowed_roots` | the download folder alone | The directories the API may name in a request: save paths, move targets, folders to browse through `/api/v1/fs/roots` and `/api/v1/fs/list`. An empty list means the download folder alone; entries are made absolute. |
| `server.allowed_hosts` | empty | Names this server answers, beside `localhost` and any literal address. With logins off these are the whole host-name defence, and every request's name is checked against them. With logins on they still decide one thing, the web UI: a browser that reaches the server at the server's own name needs nothing listed (its `Origin` is the name the request arrived at, and the sign-in and the cookie's changes accept that), but behind a proxy that rewrites `Host`, which the featherbit gateway does, the public name must be listed or the web UI's sign-in is refused. Spell each entry a bare name (`bit.local`, not `https://bit.local:8090`). At startup, an entry no request could ever carry is named in a warning line. |
| `server.auth.enabled` | on unless `server.bind` is a loopback address | Whether requests must carry a session token. Turning it off on a non-loopback bind takes an explicit `false`. |
| `server.auth.users` | empty | The accounts a password logs in to, as `{ name, password_hash }` pairs. The hash is Argon2id; the password never appears in the file. Empty means no password logins at all. |
| `server.auth.devices` | empty | Device keys that log in by signing a challenge instead of typing a password: `{ name, public_key }` pairs, the key in the one-line OpenSSH format. Only Ed25519 answers a featherbittorrent challenge; any other kind is refused at startup with a line naming it. Empty means none. |
| `server.web_ui` | on | Whether `serve` also serves the browser UI. Only `false` turns it off; a value the file does not name is on, and `FBTT_WEB_UI` in the environment replaces the file's answer when it holds a word the switch understands (`0`, `false`, `off`, `no`, or the on words `1`, `true`, `on`, `yes`). This key decides nothing in a headless build, which has no UI to serve: it accepts the key, ignores it, and logs one line when it is set. Which build you have is what `featherbittorrent --version` says. See [docs/web-ui.md](web-ui.md). |
| `server.cookie_secure` | `auto` | When the browser session cookie carries the `Secure` flag. `auto` adds it when the request arrived over https (which behind a proxy means when the proxy sends `X-Forwarded-Proto: https`); `always` adds it whatever the request says. A proxy that terminates TLS but does not send `X-Forwarded-Proto` needs `always`. See [docs/web-ui.md](web-ui.md). |
| `server.cors_origins` | empty | Origins of clients served from somewhere else that may call this API, answered by its CORS layer. This is for the featherbittorrent desktop app connecting to this server: list `tauri://localhost` (macOS and Linux) and `http://tauri.localhost` (Windows). Write each entry as a whole origin, scheme included. Listing an origin grants nothing by itself: a client from it still signs in with a password or a device key like any other, and the web UI's cookie session keeps its own origin rule. Empty means no cross-origin caller at all. |

### What the `downloads:` keys do

Two of these move a torrent's files, one starts a program, and none of them deletes anything. Everything
they run into is one warning line naming the torrent or file and what went wrong, and none of it fails a
download.

**`downloads.unfinished_dir`** is where a torrent works, and it gives the torrent up as soon as it is
whole: the engine is told to move the files to `download_dir` on the last piece. It moves the folder
rather than copying it, which is why a 200 GiB download changes folders as cheaply as a 200 MiB one and
leaves nothing behind in the folder it came from. Two cases move anything at all: a torrent still arriving
and saving in `download_dir` goes to the unfinished folder, and a torrent whole and sitting in the
unfinished folder comes back. A torrent you put somewhere else when you added it, a folder picked in the
Add dialog or a `save_path` named in the request, downloads there and stays there, because you chose it,
and a torrent whole in some other folder is left alone. A refusal of a move comes in two kinds. The engine
is still checking that torrent's data, which is ordinary just after an add: it is asked again at the next
look, within two seconds, and on until the move goes through. Anything else - a target that is somebody
else's file, a folder it cannot move into - is one warning line said once, and the files stay where they
are until the settings change, the torrent goes, or the torrent ends up somewhere else; after a restart
the asking starts fresh. A torrent that is whole runs its completion program even when its move failed,
for the folder the files are really in, because a finished torrent does not go without its program over a
move that could not be done.

What a run can ask of the torrents the library already holds, within its first two seconds: a torrent
still arriving into the download folder moves into the unfinished one, and a torrent whole and sitting in
the folder the setting names today comes back - so a torrent that finished while this server was stopped,
whole in that folder, is handed to `download_dir` and gets its completion program run then, once, because
it was never reported finished. An older unfinished folder is not fetched from: a torrent already working
there, or already whole there, stays there, because the setting says where unfinished torrents belong from
now on, not that everything ever filed there should be moved.

**`downloads.start_paused`** decides what a torrent this run brings in by itself does: on, it waits for you
to start it. Torrents the watch folder adds obey it, and so do the terminal client's own adds. An add over
the API is not covered by it. `POST /api/v1/torrents` starts whatever it adds unless the request itself
says otherwise: its `paused` field, in the JSON body or in the query string, means add it paused, and
leaving it out means start it.

**`downloads.watch_dir`** is looked at every two seconds, the first look within two seconds of the run
starting. Only its own files are candidates: a regular file sitting directly in it, whose name ends
`.torrent` in either case, and not one whose name starts with `.` (which is how the `._show.torrent` macOS
writes beside a downloaded file hides itself). A candidate is read only after two consecutive looks have
seen the same size and the same modification time, so a `.torrent` somebody is still copying in is not
added half-written; one that has stopped growing is added within about four seconds of arriving. Then it is
added and renamed `<name>.torrent.added`, or it is refused and gets the `.invalid` rename with a line
saying why: the file is bigger than 16 MiB, which is decided from its size alone without reading a byte of
it; the name answers to a different file than the two looks agreed on - a link or somebody else's file
swapped in between the look and the read, and the folder reads the file it looked at or nothing at all; a
file that grew past 16 MiB between those looks is judged by its size at the read, not by the size they
saw; its bytes are not a torrent; or the engine will not take it. What is added goes to `download_dir`,
paused or not per `start_paused`. A `.torrent` for a torrent the library already has puts nothing new in
the library: the file gets its `.added` name like any other, and nothing is said about it. Nothing is
copied and nothing is deleted, so what you put in that folder stays yours under a different name, and the
rename is also what keeps a file from being added a second time after a restart. A marker name that is
already taken comes back numbered, `<name>.torrent.added.2`, `.3` and on to `.999`, and a folder holding
all of those - or one that will not take the rename for any other reason - leaves the file where it is with
its one line said once, rather than replacing somebody's file or reading it again every two seconds for the
same answer. A watch folder that is not there, or that goes away while the server runs, is
named once rather than every two seconds; it is named again if it comes back and goes away again, and a
look that works clears the reason: what there is to say about this folder is what went wrong the last time
it was read.

**`downloads.on_complete`** starts a program when a torrent finishes, after the unfinished folder has given
it up, so the folder the program is handed is where the files ended up. It is started with two arguments
and nothing else: the torrent's name, and that folder. No shell is involved, so nothing is re-parsed on the
way and there is nothing to quote: a name carrying a space, a `;` or `$(...)` arrives as one argument and
cannot become a command. Its standard input, output and error are the null device, so it has nowhere to
print and nobody to answer, and the app does not wait for it to exit: one torrent finishing does not hold
up another, and a program that never returns is no trouble for the library. One finish is one run, and a
whole torrent whose move failed still gets its program, for the folder the files are really in. Arriving
whole is not finishing: a torrent the watch folder or the API brings in already whole and left where you
put it never finished here, and gets nothing for the add - a program is run for a finish, and for whole
files the unfinished folder hands back. A program this run will not start is one warning line beginning
`Could not start the program run on completion`, naming the program and the reason, and it is not retried.
The name must be the whole path to the program: a relative path means below whatever directory the server
was started in, and a bare name is looked up on `PATH`, which on a service is not the `PATH` of the person
who wrote the line, so anything that is not a whole path is refused before anything is tried. On Windows a
name ending `.bat` or `.cmd` - trailing dots and spaces count, capitalisation makes no difference - is
refused the same way, because Windows starts those through `cmd.exe`, where arguments are re-parsed and a
torrent's own name stops being safely one argument. The program runs on the machine this server is on, as
whoever this server runs as, with the server's own environment and the directory the server was started in
as its own: variables the server holds are the program's, and a path the program opens relatively is
opened against that directory. The torrent's name arrives as the first argument, and BitTorrent names can
begin with `-`, which to a program that takes options reads like one; programs written for this should
stop reading options at `--`. The two folder
keys and `download_dir` name places on that machine too, and the watch folder is scanned there: a folder
set on a LAN server is a folder on that server's disk, not on the machine you are browsing from.

A server reads `on_complete` from `config.yaml` and nowhere else. It belongs to the machine the program
would run on, so nothing over the API changes it, and a program you change in the file takes effect when
the server is restarted, as a new or changed user does. The desktop app's `This computer` follows these
same four keys in the `config.yaml` the app keeps for its own engine: see
[desktop.md](desktop.md).

A LAN server that anyone on the network signs in to:

```yaml
server:
  bind: 0.0.0.0:8090
  auth:
    users:
      - name: alice
        password_hash: '$argon2id$v=19$m=65536,t=3,p=4$...'
    devices:
      - name: laptop
        public_key: ssh-ed25519 AAAAC3NzaC1lZDI1NTE5... laptop
```

`featherbittorrent user set` and `device add` write these entries for you; the file above is what they
produce, shown so you can see it before editing by hand. Note that a non-loopback bind with password
logins prints one warning line at startup: this server speaks plain HTTP, so passwords cross the
network unencrypted unless something in front terminates TLS.

## Users and devices

```
featherbittorrent user set [name]      # create or replace one login; asks for the name when left out,
                                       # then the password twice (hidden on a terminal)
featherbittorrent user remove <name>
featherbittorrent user list            # name and password hash per line
featherbittorrent device add <name> [key]   # key: an ssh-ed25519 line, a file holding one, or stdin
featherbittorrent device remove <name>
featherbittorrent device list          # name and SHA256 fingerprint per line
```

These commands edit `config.yaml` where it already is; they do not start or stop a running server. A
running server reads logins from that file when it starts, so restart it for a new or changed user to
take effect. Devices are the exception: a signed-in client's devices screen (`K`) adds and removes them
through the API while the server runs, which is immediate. A device key names a machine that may sign
in without a password: the TUI signs the server's challenge with the matching private key (see the
connect screen's `key file` and `ssh-agent` methods). Fingerprints are safe to print; only the hash
beside a user name is a secret, and nothing prints it except `user list`, which you chose to run.

## Login rules

- **On by default away from loopback.** Left unnamed, `server.auth.enabled` is on for any bind that
  is not a loopback address: a port on loopback is already reachable only by someone sitting at this
  machine, who owns the library anyway. A non-loopback bind with logins left off must say so
  explicitly (`enabled: false`).
- **With logins on**, every request under `/api/v1/` carries the session token except `GET /api/v1/openapi.json`
  and CORS preflights; `GET /api/v1/events` and a file stream may put it in `?token=` instead of the
  header. Sign-in is `POST /api/v1/auth/challenge` then `POST /api/v1/auth/session`, for both methods:
  a password body, or a challenge signature for a device key. A session token lives an hour and the
  hour slides with each use; a challenge lives a minute. Failed sign-ins are rate limited per client
  address, keyed on the TCP peer, which behind a proxy means every visitor shares one budget (the
  reverse-proxy guide says what to do about that).
- **With logins off**, the server has no credential to say who a request came from. It then refuses any
  request whose `Host` is not `localhost`, a literal address, or one of `server.allowed_hosts`, and it
  checks an `Origin` header against the same list: that is what stands between a hostile web page and
  this library when nobody signs in (a DNS-rebound page cannot name an address, and cannot be trusted
  with the name either). Behind a gateway that rewrites `Host`, list both the name the gateway sends
  upstream and the browser-facing domain.
- A device key registered for gateway use is not a login here: gateway logins are settled by the
  gateway itself, and this server then runs with auth off. See the reverse-proxy guide.

## The API

The routes are [fbt-lib](https://github.com/featherbitplatform/fbt-lib)'s `fbt-api` surface (torrents,
files, trackers, peers, queue moves, the `/api/v1/events` server-sent-events stream, per-file streams,
and the OpenAPI document at `GET /api/v1/openapi.json`, which describes them all) plus what this
product adds:

| Route | What it is for |
| --- | --- |
| `POST /api/v1/auth/challenge` | First half of sign-in: asks a password body or answers a device-key challenge with the nonce to sign. |
| `POST /api/v1/auth/session` / `DELETE` | Turns a signed challenge or a good password into a session token; DELETE drops the token. |
| `GET /api/v1/auth/devices`, `POST`, `DELETE /api/v1/auth/devices/<name>` | A signed-in client manages device logins over the API, so the TUI's devices screen (`K`) works against a remote server. |
| `GET /api/v1/fs/roots`, `GET /api/v1/fs/list` | Browse inside `server.allowed_roots` while adding a torrent: the folder picker (`Ctrl+O`) and the add dialog read this instead of the client's own disk, which is not where the server saves. |
| `POST /api/v1/fs/mkdir` | Creates one folder, `{"parent": ..., "name": ...}`, directly inside a folder within `server.allowed_roots` (the web UI's folder picker, `New folder`). The name is one path component of at most 255 bytes that reads exactly as it is shown (no invisible characters, no leading, trailing or doubled spaces), does not start with a dot (the listing hides those), contains none of `< > : " | ? *`, does not end in a dot or a space and is not a Windows device name such as `CON`, `LPT1` or `CONIN$`; the parent is checked after links are resolved, so a link that leads out is refused. Answers `201 {"path": ...}`, or `400` for a bad name, `403` outside the allowed folders, `404` for a missing parent and `409` when the name is taken. Never creates intermediate folders. |
| `POST /api/v1/stream-tokens` | Asks for an address to stream one file from, for a media player that cannot send headers; its own section below. |

### A stream address

A player cannot carry a session token, so this route puts the credential in the address it hands out.
The request names a torrent by either of its hashes in either case (the way fbt-api's own routes take an
id) and a file by its index:

```
POST /api/v1/stream-tokens    { "torrent": "<40 or 64 hex digits>", "file": 0 }

201 Created                   { "path": "/api/v1/torrents/<40 hex>/files/0/stream?token=<t>",
                                "expires_in": 60 }
```

The id in the answer is always the torrent's wire hash in 40 lowercase hex digits, whatever spelling
named it, and the index is plain decimal: that string is what the token is scoped to, byte for byte, so
re-spelling it would hand out an address its own token could not open. `expires_in` is how long the
token may go before somebody uses it. The body is capped at 8 KiB (a bigger one is `413`), because with
logins off this route answers whoever reaches the address and a megabyte of JSON naming one file is not
a request worth buffering. A torrent this library does not have, an index past the end of its list, or
an index naming a pad file gets `404`; a torrent whose metadata has not arrived gets fbt-api's own
`409 no_metadata`, which is what the stream route itself answers for the same torrent; anything that is
not 40 or 64 hex digits gets `400 invalid_id`. All in this API's error shape.

The token's life: it must be used within 60 s of being issued; after that it stays alive while a request
arrives at least every 30 minutes, since a player re-requests its address on every seek and a paused
film must survive; and never beyond 8 hours from the issue. At most 64 are live on one server; once 64
are live, issuing a new token evicts the least recently used one, and tokens that have already expired
are dropped first so they do not count against 64. Only a hash of each token is stored, a token opens
exactly the one file its address names, and log lines name the subject, the torrent and the file but
never the token.

With logins on, a request for the file-stream route may carry that token in `?token=` instead of a
session token: the guard tries a query token as a stream token first, only on the two methods of that
route, only when no `Authorization` header is present, and then as a session token. A stream token is
never accepted as a bearer header, and never on `GET /api/v1/events`. It is not the session either:
signing out (`DELETE /api/v1/auth/session`) does not end a stream that was already handed out.

With logins off there is no credential to hand out: the answer's `path` carries nothing on it and
nothing is stored, because the host-name guard that stands in for a login has already accepted this
request, and the stream route accepts it on the same terms. See [docs/streaming.md](streaming.md) for
what a client does with these addresses, and [docs/reverse-proxy.md](reverse-proxy.md) for playing
through a gateway.

## One library, one run: the lock and the hand-off

The library directory holds `library.lock`, and one run drives it at a time. `serve` takes the lock
before asking any question; a second featherbittorrent pointed at the same library exits with code 3
and the line naming the busy library, changing nothing.

A plain TUI run whose library is held by a running server does not just fail. When `ui.yaml` points
at this library and the config names a server address, the TUI asks, before taking over the terminal:

```
A featherbittorrent server is running on this library. Connect to it? y/n
```

- `y` connects to that server over loopback instead of starting an engine. The connect screen comes up
  prefilled because a connection may need a login this run cannot guess, and the sign-in you complete
  there is borrowed from the running server: it is never written into `ui.yaml`, so the next plain run
  does not wake up pointed at somebody else's login. This is how one person on one machine runs the
  server headless and drives it with the TUI without configuring anything.
- `n` (or a declined offer) leaves with the busy-library line and exit 3, terminal untouched.
- A run whose `ui.yaml` points at some other server asks nothing of this library: those files belong
  to wherever that server runs.

What a normal remote connection remembers lives in `ui.yaml` beside `config.yaml`: which backend the
last run used (`last_used: Local`, or `Remote:` with `url`, `login`, and for that method what it needs
- `user`, `key_path`, a gateway's `token_url` and `client_id`, plus `gateway_key_header`, the name of the
header a gateway in front of that server reads its API key from). The secret half is not in that file: a
connect password lives in the system keychain under its user name, and a gateway key beside it under the
server's address (see [docs/reverse-proxy.md](reverse-proxy.md)'s setup 2). Bad bytes in `ui.yaml` answer
"the local engine", like a missing file does, and are kept aside as `ui.yaml.bad`.

## Debug-only switches

`serve --stop-file <path>` (stop when that file exists) and `serve --offline` (every socket on
loopback: no DHT, LSD, UPnP or NAT-PMP) exist only in debug builds. A release binary does not accept
them at all; they are what this repository's own tests drive a real server with.
