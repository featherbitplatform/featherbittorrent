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
- per-file streams (`.../stream`), which serve byte ranges while pieces arrive.

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

The browser-facing door (or any visitor that should pass a portal login) gets the gateway's own
authentication, for example `openid-connect` in interactive mode against an OIDC provider; signed-in
API clients then still sign in to featherbittorrent itself, because `server.auth.enabled` stays on. The
TUI does not care what the gateway thinks: it carries its own session token through the gateway, which
is where `limit-count` and TLS come free. With logins on there is no host-name guessing at all: every
request has to carry a token, so `server.allowed_hosts` is irrelevant in this setup.

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
  and `token_endpoint_auth_signing_alg` must be `ES256`. Two more settings are needed because this
  client sends the older assertion shape: Authelia 4.39 defaults to RFC 10070, which requires the
  issuer identifier rather than the endpoint URL as the audience, so a provider fronting
  featherbittorrent wants `enable_client_assertion_legacy_audience: true`; and it defaults a client to
  accepting only the `client.authentication.jwt` header type, so ours (`typ: JWT`, the value RFC 7523
  has always allowed) needs `client_assertion_jwt_validation_header_allow_types` listing both. The
  example under [docs/examples/gateway-oidc/](examples/gateway-oidc/) shows a working registration with
  all four, and Authelia's own reference for these keys is its
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
rest of the host.

A worked-out stack, with Authelia as the provider and `featherbittorrent serve` behind it, is
[docs/examples/gateway-oidc/](examples/gateway-oidc/).
