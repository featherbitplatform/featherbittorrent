# Settings and proxy: design

Status: draft for the owner's approval, 2026-11-25. Builder check done; the owner answered its two open questions (sections 5 and 9).

## 1. Why

The owner went looking for a proxy setting and there was none. More generally, featherbittorrent exposes
only five engine settings on screen (download and upload limits, three queue counts); everything else is
config-file only or not exposed at all, while fbt-lib already supports most of what other clients offer.
Users of the app may want a proxy for two reasons: to hide their address, and to get out of a network that
only allows traffic through a proxy. Both are served by one rule (section 3).

Success: a user can set a proxy and the common download settings from any of the three surfaces (TUI,
web UI, desktop app) without editing a file, the Settings page is easy to move around in, and the rest of
the settings other clients have are designed now so later rounds only build them.

## 2. Scope

**This round (round 1):** the proxy (section 3), the Downloads settings (section 4), the new Settings page
in the web UI and desktop app (section 6), the new TUI Settings screen (section 7), and moving today's
settings into them (section 5).

**Designed now, built later** (section 8): round 2, speed schedule and seeding limits; round 3, connection
and privacy; round 4, advanced. The order is the builder's guess, most-used first; the owner did not
object.

**Non-goals:** RSS, search, mail notifications, HTTPS for the web UI, SOCKS4, a proxy for the desktop
app's own connection to a remote server, routing DHT or uTP through an HTTP proxy.

**Anti-goals** (would make this a failure even if it technically works):
- a proxy that is set but lets any connection go out directly (an address leak);
- a setting that silently does nothing (saved but not applied, or applied only after a restart nobody
  was told about);
- a web UI user being able to make the server run a program.

## 3. Proxy

### 3.1 The rule (owner's decision)

When a proxy is set, the app connects **only** through it. Nothing falls back to a direct connection.
When no proxy is set, the app behaves as today. There is no separate privacy or kill switch: setting a
proxy is the switch.

### 3.2 What it covers

- Types: **HTTP** (CONNECT) and **SOCKS5**, by host and port, with optional user name and password.
- Through the proxy: outgoing peer connections, HTTP(S) tracker announces and scrapes, web seeds, and
  magnet metadata (which arrives over peer connections).
- With SOCKS5, when the proxy relays UDP (UDP ASSOCIATE): uTP, the DHT and UDP trackers also go
  through it.
- **Name lookups count as connections** (builder's ruling, from the owner's rule): no host name is
  resolved from this machine while a proxy is set. TCP connections hand the name to the proxy (SOCKS5
  remote resolution, HTTP CONNECT by name). UDP tracker and DHT bootstrap names, which fbt-lib v0.12.0
  resolves locally even under SOCKS5, are resolved through the proxy (fbt-lib work item F4) or, where
  that cannot be done, are not used while a proxy is set.

### 3.3 What turns off while a proxy is set

Everything that cannot go through the proxy or would reveal the address:
- incoming connections (TCP and uTP), and with them UPnP and NAT-PMP port forwarding;
- local peer discovery (LSD);
- with an HTTP proxy, or a SOCKS5 proxy that does not relay UDP: the DHT, uTP and UDP trackers.

The Proxy section says this in plain words next to the fields, so fewer peers is not a surprise. When a
proxy is set, the settings these override (round 3's DHT, LSD, UPnP toggles) show as "off while a proxy
is set" rather than their stored value; the stored value comes back when the proxy is removed.

### 3.4 A proxy that does not work

- A proxy that cannot be reached, refuses the login or refuses a connection puts a visible state on the
  instance ("Can't reach the proxy" with the proxy's own reason, display-safe), in the TUI status line,
  the web UI header area and the desktop app. It links (web, desktop) or points (TUI) to the Proxy
  section. Torrents stay as they are and resume on their own once the proxy works again.
- A SOCKS5 proxy that does not relay UDP is not an error: the app carries on over TCP only (3.3) and the
  Proxy section says "This proxy does not relay UDP, so the DHT and UDP trackers are off". Today fbt-lib's
  `Session::new` fails when the UDP association fails (fbt-lib work item F2).
- A proxy that is down when the engine starts does not stop the engine: the library opens, the screens
  work, nothing connects, and the instance shows "Can't reach the proxy" until it comes back (F2).
- Health comes from the engine: fbt-lib reports a session-level proxy state (reachable, unreachable with
  a reason, login refused, UDP not relayed) and re-establishes the UDP association by itself when the
  proxy returns (F5). featherbittorrent shows that state; it does not probe the proxy on its own.

### 3.5 Storage

- Config key `proxy:` in `config.yaml` with `type: http | socks5`, `host`, `port`, `username`,
  `password` (all optional except type, host and port). Absent or `type: none` means no proxy.
- The password: in the desktop app (This computer) it goes in the system keychain, like instance
  passwords today, and `config.yaml` holds no password. For the CLI and a server it lives in
  `config.yaml` (which is already the place for secrets such as the user list); the file's permissions
  stay as today. The web UI never receives the stored password back: the field shows "saved" and can be
  replaced or cleared.

### 3.6 Applying it

A changed proxy takes effect without the user restarting anything, through an fbt-lib runtime setter
(work item F1). Existing peer and tracker connections made under the old setting are closed; no
connection may be made directly in between.

### 3.7 Downgrading

A 0.4.x binary reading a `config.yaml` that has `proxy:` ignores the key and connects directly, and can
drop the key when it saves. Old binaries cannot be changed, so this is accepted and documented: the
changelog entry and `docs/` say that going back to 0.4.x removes the proxy (builder's ruling).

## 4. Downloads (round 1)

| Setting | Meaning | Notes |
|---|---|---|
| Default folder | Where new torrents go | Exists as `download_dir`, today config-file or wizard only. Applies to new torrents only. On a server the picker is limited to `server.allowed_roots`, as now. |
| Unfinished folder | Optional. Torrents download there and move to their final folder when complete | Uses the existing move (`move_storage`). Off by default. Same `allowed_roots` rule. |
| Start torrents paused | Default for new torrents | The Add dialog's own choice still overrides it per torrent. |
| Watch folder | `.torrent` files dropped there are added automatically | Off by default. Added files go to the default folder. After adding, the file is renamed to `<name>.torrent.added`, never deleted, so it is not added twice. A file that fails to parse is renamed `<name>.torrent.invalid` and the reason shows in the app's messages. Partially written files must not be read too early (builder's call how: wait for the size to settle, or similar). On a server, the watch folder must be inside `allowed_roots`. |
| Run a program on completion | Runs a program with the torrent's name and folder when a download finishes | Owner's rule: settable from the TUI on its own engine and from the desktop app for This computer; a server takes it **only from its config file**, never from the web UI or a remote TUI, which show "set in the server's config file" instead. The program gets the name and folder as separate arguments, never through a shell. It runs once per completion, is not waited for, and its failure to start is reported as a message. |

## 5. Today's settings move in

Everything settable today stays settable and moves into the new screens:
- Speed: download and upload limits (from the old Server section and the TUI dialog).
- Queue counts (active downloads, active seeds, active total): a **Queue** part of the Speed section,
  so round 2 does not have to move them again (builder's call on the exact placement).
- Devices, Keyboard shortcuts, and the desktop sections (Window, Serve the web UI, Instances) as their
  own entries.

**Where saved settings go (owner's decision):** in `config.yaml`. Saving a Torrents section from any
surface (the web UI, the desktop app, a TUI, connected to a server or not) applies it to the running
engine and writes it to that engine's `config.yaml`, so it survives a restart. This fixes today's gap
where a limit set from the web UI is lost on restart. The rewrite keeps every key it does not know;
comments a person typed into the file are not kept, and the docs say so. The server rewrites the file
atomically (write a temporary file, then rename) and never touches the user list or device keys except
through their own routes. These saves go through new product routes in `fbtt-server`; the engine's own
routes (`/api/v1/session/limits`, `/queue`) stay for API users but do not persist.

No setting is lost, renamed in `config.yaml` or given a new default by this work. `config.yaml` written
by 0.4.x reads unchanged, and a file written by the new version keeps the keys it does not know about
(as `desktop.yaml` already does), so an older binary reading it does not lose them.

## 6. The Settings page (web UI and desktop app)

Owner's choice: **one section at a time** (layout B of the mockups), like qBittorrent's Options.

- A list of sections on the left; selecting one shows only that section on the right. "Back to torrents"
  stays fixed at the top. The page never needs scrolling back up to reach either.
- Groups and order:
  - **Torrents:** Downloads, Speed, Proxy (round 1); Seeding (round 2); Connection, Privacy (round 3);
    Advanced (round 4). These are the engine's settings and belong to the instance being viewed:
    switching from This computer to a server shows the server's settings.
  - **Access:** Devices, Keyboard shortcuts.
  - **This app** (desktop app only): Window, Serve the web UI, Instances.
- Sections of later rounds do not appear until they are built: no greyed-out placeholders.
- Each section has its own Save. Leaving a section or the page with unsaved changes asks to keep or
  discard them.
- The page opens on the last section used. Links can open a given section (the "Can't reach the proxy"
  state opens Proxy; the existing links to the desktop sections keep working).
- Phone width: the list becomes a "Jump to" menu at the top.
- Keyboard: the list is navigable with the arrow keys; the selected section's heading takes focus when it
  opens.
- A setting the user cannot change from here (run a program on a server) shows its state and where it is
  set, not an input.

## 7. The TUI Settings screen

- `S` opens a full-screen Settings view replacing today's dialog: section list on the left, fields on the
  right, Tab moves between the two, Esc goes back (asking about unsaved changes).
- Same Torrents sections as the web page, same per-section Save rule, same "set in the server's config
  file" for run-a-program when connected to a server.
- Devices stays where it is today (its own modal). Desktop-only sections are not in the TUI.
- Connected to a server, it reads and writes the server's settings over the API; on its own engine, it
  writes `config.yaml` and applies to the running engine.

## 8. Later rounds (designed now)

Same placement rules as round 1: engine settings in the Torrents group, everything on all three
surfaces and in `config.yaml`, applied without a restart.

- **Round 2, Speed and Seeding.**
  - Alternative speed limits and a schedule (days, from and to), plus a manual "use alternative limits"
    toggle in the main screens.
  - Seeding: stop at a ratio and/or after a seeding time (fbt-lib `seed_ratio_limit`,
    `seed_time_limit`, event `SeedLimitReached`); the action when reached: pause, or remove (keeping the
    files).
- **Round 3, Connection and Privacy.**
  - Connection: listen port (today config-file or wizard only), random port on start, UPnP and NAT-PMP,
    connection limits global and per torrent, encryption mode (forced, preferred, disabled).
  - Privacy: DHT, PEX and LSD toggles; an IP blocklist (fbt-api already has `/api/v1/session/ip-filter`
    and banned peers), from a file or a URL; binding to a network interface so traffic only leaves
    through it (for VPN users).
- **Round 4, Advanced.** Disk cache size (today config-file only), the TUI player program and arguments
  (today config-file only), recheck on completion, pre-allocation.

## 9. Engine (fbt-lib) work this needs

**The owner approved F1 to F5 (2026-11-25)**, shipped as fbt-lib **v0.13.0** inside round 1;
featherbittorrent moves its pin to that tag. Items:
- **F1, runtime proxy and network settings.** Today only rate limits, queue settings, peer-class limits
  and the IP filter can change at runtime; the proxy is fixed when the session starts. Add a runtime
  setter for the proxy and for the switches 3.3 turns off (3.6). Round 3's toggles reuse it.
- **F2, start without the proxy's UDP, or without the proxy.** `Session::new` fails when the SOCKS5 UDP
  association fails, and cannot tell "does not relay UDP" from "unreachable". It should start in both
  cases, with UDP features off and nothing connecting directly, and say which case it is (3.4).
- **F3, no-bypass check.** fbt-lib documents what bypasses a proxy (`proxy_bypasses()`: LSD, UPnP,
  NAT-PMP, DHT under HTTP, incoming). featherbittorrent turns those off itself (3.3). A test proves no
  socket other than the proxy's is opened to a non-loopback address while a proxy is set. The test lives
  in fbt-lib (sockets) and featherbittorrent (the settings it derives from `proxy:`).
- **F4, names through the proxy.** UDP tracker and DHT bootstrap host names resolved through the proxy,
  or those features not used under a proxy (3.2).
- **F5, proxy health.** A session-level proxy state event (reachable, unreachable with reason, login
  refused, UDP not relayed), and re-establishing the UDP association when the proxy comes back (3.4).

## 10. Constraints

All of the repository's rules apply (CLAUDE.md), in particular: tests never contact a real proxy or the
internet (a test proxy runs on loopback inside the test); remote text is display-safe; the UI thread
never awaits the network; product copy has no emoji and no em-dashes; cross-platform.

## 11. Left to the builder

Exact field labels and help text, the API shape for reading and writing settings (featherbittorrent's own
server routes may carry the product settings; the engine's existing routes stay), how the watch folder
waits for complete files, how the "unsaved changes" prompt looks, keyboard details in the TUI screen, and
the order of the work inside round 1.
