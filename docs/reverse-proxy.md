# Putting featherbittorrent behind a gateway or reverse proxy

The server speaks plain HTTP and holds no certificate: TLS termination is somebody else's job by
design. On any network you do not own, we recommend the [featherbit
gateway](https://github.com/featherbitplatform/gateway), or another API gateway or reverse proxy, in
front of `featherbittorrent serve` for three reasons at once: TLS, throttling, and further
authentication. Any plain reverse proxy answers the TLS part; the rest of this guide is written for
the featherbit gateway and translates directly to the others.

## Never buffer the stream

Two kinds of response must reach the client as it is produced, not collected first:

- `GET /api/v1/events`, the server-sent-events stream the TUI watches for updates; and
- file streams (`/api/v1/torrents/*/files/*/stream`), which serve byte ranges while pieces arrive.

A proxy that buffers them turns a live view into a hang: nothing completes, so nothing is ever sent.
Switch response buffering off for these paths (nginx: `proxy_buffering off;`, plus a long
`proxy_read_timeout`; Caddy: flush everything). The featherbit gateway needs no switch: it streams as
soon as the node after `upstream` does not read the body, which is the case when nothing else touches
the response. What to avoid inside a policy chain is any node that reads the whole body (a buffering
logger, `limit-conn`); `openid-connect`, `basic-auth` and `limit-count` do not touch the request body,
so they are stream-safe.

## Three ways to combine gateway login and featherbittorrent login

The server's own logins (users and device keys, [docs/server.md](server.md)) and a gateway's logins
are separate doors. You can run either door alone or both at once.

### 1. featherbittorrent login only: the gateway for TLS and rate limits

Keep `server.auth.enabled` on (it is on by default for any non-loopback bind) and let the gateway do
TLS and throttling without authenticating anybody itself. A minimal policy chain is
`client -> limit-count -> upstream -> client`, with the listener carrying the certificate in
`system.yaml`.

One thing to know: the server's own sign-in rate limits key on the TCP peer address, and the server
does not read `X-Forwarded-For`. Behind a proxy that makes one connection per backend request from its
own address, every visitor shares one login budget, and the gateway's `real-ip` plugin rewrites only
the gateway's own view of the client, not what the server sees. Put the effective throttle at the
gateway (`limit-count`) and treat the server's shared budget as the backstop it is.

### 2. Both doors: gateway login first, featherbittorrent login on top

Two credentials travel together in this setup and neither one stands in for the other: the gateway decides
whether this is somebody it knows, and only then does the server decide whether it is a user of *this*
library. `server.auth.enabled` stays on.

- **Browsers** sign in at the gateway: `openid-connect` with `bearer_only: false`, the interactive
  Authorization Code flow with PKCE (or `basic-auth`, or whatever else you run for people). This half is
  wired for the web UI featherbittorrent has not got yet, and it is worth being plain about what that means
  today: there is no browser client to serve. A browser pointed at `/api` gets nothing out of it - without a
  credential either door recognises it stops with a `401` at the gateway, and a browser that does get through
  (a bearer token the gateway accepts, say) still meets featherbittorrent's own sign-in on the other side,
  which a browser session is not an answer to: `401` again, this time from the server. What setup 2 serves
  today is the TUI.
- **The TUI** carries an API key the gateway reads before it looks at anything else, and then signs in to
  featherbittorrent as it always has. One `multi-auth` node accepts either credential on one route: its
  `key-auth` sub-plugin is tried first, and a request with no key there has only failed one attempt, so
  `openid-connect` gets its turn at the same door.

```yaml
# gateway.yaml (the example folder has the full version)
routes:
  - name: api
    match: { host: torrent.example.test, path: /api/* }
    policy: api-policy
  - name: web
    match: { host: torrent.example.test }
    policy: web-policy

policies:
  # The API route takes either credential: a TUI with its key, or a browser session's bearer token.
  - name: api-policy
    nodes:
      - { id: listener, type: listener }
      - { id: client, type: client }
      - id: doors
        type: multi-auth
        config:
          auth_plugins:
            - key-auth:
                keys: ["featherbit-tui-key"]
                header_name: x-api-key
            - openid-connect:
                discovery: https://auth.example.test/.well-known/openid-configuration
                bearer_only: true
                claim_validator:
                  issuer: { valid_issuers: ["https://auth.example.test"] }
      - id: backend
        type: upstream
        config:
          targets:
            - { host: featherbittorrent, port: 8090 }
    edges:
      - from: listener.out
        to: doors.in
      - from: doors.denied
        to: client.in
      - from: doors.success
        to: backend.in
      - from: backend.success
        to: client.in

  # Browsers get the interactive flow, which is the one that redirects to the portal and back.
  - name: web-policy
    nodes:
      - { id: listener, type: listener }
      - { id: client, type: client }
      - id: portal
        type: openid-connect
        config:
          discovery: https://auth.example.test/.well-known/openid-configuration
          bearer_only: false
          claim_validator:
            issuer: { valid_issuers: ["https://auth.example.test"] }
      - id: backend
        type: upstream
        config:
          targets:
            - { host: featherbittorrent, port: 8090 }
    edges:
      - from: listener.out
        to: portal.in
      - from: portal.denied
        to: client.in
      - from: portal.redirect
        to: client.in
      - from: portal.success
        to: backend.in
      - from: backend.success
        to: client.in
```

`multi-auth` runs its sub-plugins in the order they are listed and lets the request through on the first
success; `denied` (a mandatory port) is where a request that failed every one of them leaves, wired
straight to `client` so the prepared `401` reaches whoever knocked instead of continuing into the
upstream. `key-auth` compares what it finds against its `keys` list and reads it from `header_name`,
which defaults to `x-api-key` and is matched case-insensitively: whatever you put there is exactly what
goes in the connect screen's `Gateway key header` row. Neither node reads the request body, so the events
stream stays safe through this policy (see "Never buffer the stream").

On the TUI side, both halves are filled in on the connect screen (`c`): the header name, and the key under
`Gateway key`. The two go together - a header name with no key under it, or a key with no name to send it
under, is refused on the screen rather than dialled. A name this client needs for its own request is refused
there too (`Authorization`, `Proxy-Authorization`, `Content-Type`, `Content-Length`, `Host`, `Cookie`,
`Connection`, `Transfer-Encoding`, `TE`, `Upgrade`, and anything that is not a header name at all): the key is
put onto a request under whatever name it is given, replacing what was already there, so a key typed into
`Authorization` would leave the request carrying the gateway's key as its only proof.

The key then rides on every request this connection makes to that server's address - the sign-in call
included, because the gateway stands in front of that too - beside this client's own bearer token, so what the
server sees is an ordinary `password` (or device key) login whatever the gateway thought. That address is the
only place it goes: a `gateway private key` login whose token endpoint is somebody else's host posts that
endpoint the assertion it is there to check and nothing else, because a provider that has never seen this
client's server has nothing to check a key for that server against.

What happens to it afterwards is the same as what happens to a password: answer *remember in the system
keychain* once and the key is kept there under this server's address, spelled one way so that
`https://torrent.example.test` and `https://torrent.example.test/` are one entry
(`gateway-key:https://torrent.example.test`), while `ui.yaml` records only the header name and never the
value - a file somebody can `cat` says that a gateway stands in front of this server without saying what
opens it. Leave the header row empty and no such header is sent at all, which is right for every server
you reach directly.

With logins on there is no host-name guessing at all: every request has to carry a token, so
`server.allowed_hosts` is irrelevant in this setup.

### 3. Gateway only: featherbittorrent auth off

`server.auth.enabled: false` makes the server trust what reaches it, so it must be reachable only
through the gateway: bind it to loopback (same machine) or to an internal network the outside cannot
reach, and publish no port but the gateway's. Then two things go in `server.allowed_hosts`, because
with logins off the host-name check is the whole defense:

- the name the request arrives with. The featherbit gateway's `upstream` node always rewrites `Host`
  to its target's `host:port`, so that is what the server sees; names are compared after the port is
  taken off, so list the target host as a bare name (in the example below, `featherbittorrent`) and do
  not put a port on any entry - an entry that could never match is named in a startup warning line; and
- the browser-facing domain (`torrent.example.test`). A browser sends an `Origin` header naming the
  site it is on, the gateway forwards it unchanged, and the server checks it against the same list.

A warning that belongs here, not in a footnote: with logins off the server always accepts a literal
address as the host name, and no gateway sits in front of an address someone types straight into the
browser at the server's port. If the bind is reachable beyond loopback, firewall that port to the
gateway (and any machines you mean to share it with), or turn logins back on.

## Streaming to a player

`s` opens a file in mpv or VLC ([docs/streaming.md](streaming.md)), and a media player sends no headers:
the only credential such a request can carry is the token in its address, issued by
`POST /api/v1/stream-tokens` ([docs/server.md](server.md)).

- **Setup 1**, the gateway for TLS and rate limits without a login of its own: the address works through
  the gateway exactly as it is. The player arrives with a token the server issued, the gateway asks
  nothing of it, and the server checks that token against the one file the address names. Two things to
  keep in mind: a player re-requests its address on every seek, so a `limit-count` counts seeks rather
  than plays; and nothing in this route's policy chain may buffer or read the response, exactly as for
  the events stream above.
- **Setups where the gateway authenticates** (2 and 3): the player has no gateway credential to send, so
  an address on your public host would be turned away at the gateway before its token was ever looked
  at. For those setups the TUI relays these requests itself: it listens on `127.0.0.1`, and what the
  player is given is `http://127.0.0.1:<port>/relay/<token>`, an address on the machine the TUI runs.
  The relay adds the gateway's credential to the server address it forwards to, passes one `Range`
  header through, and copies back only what a player needs to seek. It sends the gateway's proof, never
  the server's session token. What reaches your gateway is an ordinary API request carrying that
  credential; no route or host rule of yours applies to the loopback address itself.
- With logins off (setup 3) the issued address carries no token at all, and it is the relay that supplies
  the gateway's credential; the server then sees the host name your `upstream` node rewrites the request
  to, as above.

A relayed request whose answer is not a status a player understands (`200`, `206`, `404` or `416`) comes
back as a `502`: the relay does not follow redirects, so a proxy that would answer these paths with a
redirect should be set to pass them through instead.

## The TUI's two gateway login methods

When the gateway is the door, the connect screen (`c`) has two methods for it:

- **`gateway private key`** is OAuth2 client credentials with a signed assertion (`private_key_jwt`):
  the TUI signs a short-lived JWT with a device key and posts it to the provider's token endpoint you
  enter as the token URL, then uses the bearer token it gets back against the gateway. The signing
  header names the key with its OpenSSH fingerprint (`SHA256:...`), and the assertion's audience is
  exactly the token URL you entered. Providers that document this method take RSA or ECDSA client keys
  and usually no Ed25519 at all, so make a gateway key as P-256 ECDSA:

  ```
  ssh-keygen -t ecdsa -b 256 -f ~/.ssh/featherbittorrent_gateway
  ssh-keygen -lf ~/.ssh/featherbittorrent_gateway.pub   # prints the SHA256 fingerprint
  ```

  Register the client with your provider: a client id, the token endpoint's URL as the token URL, and
  the public key as the client's JWK. With Authelia the registered `key_id` must equal the fingerprint
  line `ssh-keygen -lf` prints (`SHA256:<base64>`), because that is what the assertion's header names,
  and `token_endpoint_auth_signing_alg` must be `ES256`. Authelia 4.39 accepts the assertion exactly
  as the TUI sends it (`typ: JWT`, the token URL as audience) without any extra switch, with two
  details to get right. The public key goes in as a PEM naming the P-256 curve, and some
  `ssh-keygen` builds (macOS among them) export explicit curve parameters instead, which Authelia
  rejects; pipe the export through `openssl ec -pubin -param_enc named_curve -pubout`. And set the
  revocation, introspection and pushed-authorization endpoint auth methods to `private_key_jwt` as
  well, or Authelia defaults them to `client_secret_basic` and refuses a client with no secret. The
  example under [docs/examples/gateway-oidc/](examples/gateway-oidc/) shows a working registration,
  and Authelia's own reference for these keys is its
  [OpenID Connect clients configuration page](https://www.authelia.com/configuration/identity-providers/openid-connect/clients/).

- **`gateway password`** is plain HTTP Basic at the gateway (its `basic-auth` plugin, or any proxy's
  equivalent): a user name and password checked by the gateway, with featherbittorrent itself running
  auth-off. It is the quick door; the private key is the one that never puts a password on the wire.

In both cases the server URL you connect to is the gateway's HTTPS URL, and the server behind it runs
with `server.auth.enabled: false` as in setup 3.

## The featherbit gateway route shape

Two files: `system.yaml` for the listener and certificate (read once at startup), and `gateway.yaml`
for routes and policies (hot-reloaded). A route picks requests by host and path and names a policy;
a policy is a graph of nodes wired by edges, from a `listener` node to an `upstream` node, with every
auth node's extra ports wired back to `client` so rejections and redirects reach the browser:

```yaml
# gateway.yaml (the example folder has the full version)
routes:
  - name: api
    match: { host: torrent.example.test, path: /api/* }
    policy: api-policy
  - name: web
    match: { host: torrent.example.test }
    policy: web-policy

policies:
  - name: api-policy
    nodes:
      - { id: listener, type: listener }
      - { id: client, type: client }
      - id: throttle
        type: limit-count
        config: { count: 240, time_window: 60, rejected_code: 429 }
      - id: tokens
        type: openid-connect
        config:
          discovery: https://auth.example.test/.well-known/openid-configuration
          bearer_only: true
          claim_validator:
            issuer: { valid_issuers: ["https://auth.example.test"] }
      - id: backend
        type: upstream
        config:
          targets:
            - { host: featherbittorrent, port: 8090 }
    edges:
      - from: listener.out
        to: throttle.in
      - from: throttle.success
        to: tokens.in
      - from: throttle.limited
        to: client.in
      - from: tokens.denied
        to: client.in
      - from: tokens.redirect
        to: client.in
      - from: tokens.success
        to: backend.in
      - from: backend.success
        to: client.in
```

`bearer_only: true` validates a bearer token against the provider's JWKS (signature, expiry, issuer);
`bearer_only: false` runs the interactive Authorization Code flow with PKCE for browsers instead. The
two modes are separate nodes on separate routes, which is why the example splits `/api/*` from the
rest of the host. Routes that match on `host` need `http2.enabled: false` in `system.yaml` with the
gateway release the example was verified against: it reads the host name from the `Host` header
only, which HTTP/2 requests do not carry, so they would get `No route matched`.

A worked-out stack, with Authelia as the provider and `featherbittorrent serve` behind it, is
[docs/examples/gateway-oidc/](examples/gateway-oidc/).
