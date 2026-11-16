# featherbittorrent behind the gateway with Authelia

One host, three containers, HTTPS on `https://torrent.example.test`. The featherbit gateway forwards
`/api/*` to the featherbittorrent server after checking a bearer token that the TUI buys from
Authelia on its own, and forwards everything else on the host to the same server after an
interactive browser login through Authelia. BitTorrent itself goes straight to the server container.

featherbittorrent has no web UI yet (it is a later milestone), so the browser door has nothing to
show today: after the login the server answers `/` with an empty `404`, and browsing to an API path
such as `/api/v1/torrents` gets the gateway's `401`, because the API door wants a bearer token, not
a browser session. The door is wired and tested now so the web UI drops in behind it unchanged.

| Container | Listens | Published | Role |
|---|---|---|---|
| `featherbittorrent` | 8090 inside the network | 6881 tcp/udp only | API (and the future web UI), logins off |
| `gateway` | 8443 | 443 | HTTPS, OIDC on both doors, rate limits |
| `authelia` | 9443 (TLS) | 9443 | portal plus the OIDC provider |

Nothing publishes the server's own port: with its logins off it answers only to the gateway, which
is what [docs/reverse-proxy.md](../../reverse-proxy.md) calls the gateway-only setup.

## What has to line up

- **Names.** `torrent.example.test` and `auth.example.test` must resolve to this machine for your
  browser and for any TUI you run yourself. On one machine a pair of `/etc/hosts` lines is enough.
  Inside the compose network the containers reach each other by service name; the gateway also has
  an `extra_hosts` entry so it can call Authelia under the same hostname your browser uses.
- **The issuer.** Authelia serves its own TLS on 9443, inside the network and on the host alike, so
  its issuer is exactly `https://auth.example.test:9443` for the browser and for the gateway's
  callouts, and nothing in front has to rewrite a forwarded header. The session cookie covers
  `example.test`, which spans both names.
- **The token endpoint.** Since 4.39 the OIDC endpoints sit under `/api/oidc/`, so the URL the TUI
  posts its assertion to is `https://auth.example.test:9443/api/oidc/token`. That is what you type
  into the connect screen, and it is also the audience the assertion names.
- **Certificates.** The demo pair is self-signed and covers both hostnames, so both
  openid-connect nodes set `ssl_verify: false`. Delete those two lines once the certificate is a
  real one. Note the caveat for the terminal below: featherbittorrent itself verifies TLS against
  the public root certificates and has no switch to look the other way.
- **HTTP/1.1 at the gateway.** `config/gateway/system.yaml` turns HTTP/2 off: the gateway release
  this was verified against matches a route's host against the `Host` header only, which HTTP/2
  requests do not carry.

## Prerequisites

- Docker with Compose.
- `openssl` and `ssh-keygen` (both ship with most systems).
- The gateway image `featherbit/featherbit`, or another tag you pin in `.env`.
- The featherbittorrent image, which compose builds from this repository's root `Dockerfile` on
  first `up --build`.

## One-time setup

Run every command from this directory.

1. **Names.** Point both hostnames at this machine, for example two lines in `/etc/hosts`:

   ```text
   127.0.0.1   torrent.example.test auth.example.test
   ```

2. **Secrets.** Compose expects five files under `secrets/` and two values in `.env`. None of them
   are committed (see `.gitignore`).

   ```sh
   mkdir -p secrets

   openssl rand -hex 32 > secrets/session_secret
   openssl rand -hex 32 > secrets/storage_encryption_key
   openssl rand -hex 32 > secrets/oidc_hmac_secret
   openssl genrsa -out secrets/oidc_jwks_key 2048

   WEB_CLIENT_SECRET="$(openssl rand -hex 32)"
   docker run --rm authelia/authelia:4.39.28 \
     authelia crypto hash generate pbkdf2 --variant sha512 --password "$WEB_CLIENT_SECRET" \
     | sed -n 's/^Digest: //p' > secrets/web_client_secret_digest

   printf 'WEB_CLIENT_SECRET=%s\n' "$WEB_CLIENT_SECRET" > .env
   printf 'GATEWAY_SESSION_SECRET=%s\n' "$(openssl rand -hex 32)" >> .env
   ```

   The web client's secret exists twice on purpose: the gateway reads the plaintext from `.env`,
   Authelia only ever sees its PBKDF2 digest in `secrets/web_client_secret_digest` (one line
   starting `$pbkdf2-sha512$`). `secrets/oidc_jwks_key` is the provider's RSA signing key in plain
   PEM; the template filter indents it into `configuration.yml` for us.

3. **The TUI's gateway key.** The terminal signs its assertion with an ECDSA P-256 key (Authelia
   accepts RSA or ECDSA client keys here, not Ed25519):

   ```sh
   ssh-keygen -t ecdsa -b 256 -f ~/.ssh/featherbittorrent_gateway -N '' -C 'featherbittorrent gateway'
   ```

4. **Teach Authelia that key.** Two placeholders sit in `config/authelia/configuration.yml`:

   ```sh
   ssh-keygen -lf ~/.ssh/featherbittorrent_gateway.pub
   ssh-keygen -e -m pkcs8 -f ~/.ssh/featherbittorrent_gateway.pub \
     | openssl ec -pubin -param_enc named_curve -pubout
   ```

   The first prints a line like `256 SHA256:0r/1zfy... featherbittorrent gateway (ECDSA)`. The
   placeholder already starts with `SHA256:`, so replace only `REPLACE_WITH_YOUR_FINGERPRINT` with
   the characters after `SHA256:` up to the next space; the result reads
   `key_id: 'SHA256:0r/1zfy...'`. The second prints the same key as a PEM public key: paste its
   body lines (two of them, between the `BEGIN` and `END` markers) over
   `REPLACE_WITH_YOUR_PUBLIC_KEY_LINES`, keeping the indentation. The `openssl ec` step matters:
   some `ssh-keygen` builds (macOS among them) write the curve out as explicit parameters, which
   Authelia rejects with `x509: invalid ECDSA parameters`; `-param_enc named_curve` rewrites it as
   the named P-256 curve. It is a public key, so it is fine that it lives in a committed file.

5. **A password for Alice.** Hash it with Authelia's own tool:

   ```sh
   docker run --rm authelia/authelia:4.39.28 \
     authelia crypto hash generate argon2 --password 'a passphrase'
   ```

   Copy the value after `Digest: ` over `REPLACE_WITH_THE_DIGEST` in
   `config/authelia/users_database.yml`, inside the quotes.

## Run it

```sh
docker compose up --build -d
docker compose ps
docker compose logs gateway authelia featherbittorrent
```

The first run compiles featherbittorrent, which takes a few minutes; later runs reuse the cached
image unless you pass `--build` after changing Rust code. The `certs` service generates the
self-signed pair into its own volume on the first run and reuses it afterwards. The gateway starts
once Authelia reports healthy. featherbittorrent's log should end with
`featherbittorrent is serving address=0.0.0.0:8090 auth=false`.

## Log in from a browser

Open `https://torrent.example.test/`. With a self-signed certificate your browser will complain
first; accept it once per hostname (`auth.example.test` gets its own complaint when you are sent to
the portal). The gateway redirects to Authelia, you sign in as `alice` with the password from step
5, confirm the consent screen, and Authelia sends you back through `/oidc/callback` with a gateway
session cookie. What you see next is featherbittorrent itself: an empty `404` for `/`, since there
is no web UI yet (see the top of this page). `/logout` ends the gateway session.

## Log in from a terminal

Run featherbittorrent anywhere it can reach those names, with scratch directories so it does not
touch your real ones, and press `c` to open the connect screen:

```sh
mkdir -p /tmp/featherbittorrent-demo/config /tmp/featherbittorrent-demo/data
FBTT_CONFIG_DIR=/tmp/featherbittorrent-demo/config \
  FBTT_DATA_DIR=/tmp/featherbittorrent-demo/data \
  cargo run -p featherbittorrent
```

Fill it in as follows, then save it so the next start goes straight in:

| Field | Value |
|---|---|
| URL | `https://torrent.example.test` |
| Login | gateway private key |
| Token URL | `https://auth.example.test:9443/api/oidc/token` |
| Client ID | `featherbittorrent-tui` |
| The agent switch, and a key path | leave the agent switch off and name `~/.ssh/featherbittorrent_gateway`, or turn it on with that key loaded in ssh-agent (the `.pub` beside a named path is what tells the client which of the agent's keys to use) |

**This step needs a real certificate.** The TUI verifies TLS against the public root certificates
and has no option to skip that, so with the self-signed demo pair it stops at the certificate
instead of logging in. See "Use your own certificate" below; browser logins are unaffected.

## API calls

The TUI never shows you its token: it buys one when it needs one and replaces it before it expires.
What it sends to the provider is a `client_credentials` request authenticated by an assertion the
gateway key signed (an ES256 JWT whose header names the key's `SHA256:` fingerprint as `kid` and
whose audience is the token URL), which is awkward to sign by hand from a shell; `$TOKEN` below
therefore stands for an access token this provider issued for `featherbittorrent-tui`:

```sh
curl -sk https://torrent.example.test/api/v1/torrents \
  -H "Authorization: Bearer $TOKEN"
```

The gateway validates the token against Authelia's JWKS (signature, expiry, issuer) before the
request reaches featherbittorrent, and rate limits it on top. Without a token the answer is the
gateway's `401`; with one it is the JSON that [docs/server.md](../../server.md) describes (`[]` on a
fresh library).

## Use your own certificate

Put a pair covering both hostnames into the `certs` volume and nothing else changes: the generator
service leaves files that already exist alone. Copy them in, then fix their ownership the same way
the generator does, because the gateway reads the private key as uid 65532:

```sh
docker compose up -d certs
docker compose cp cert.pem certs:/tls/
docker compose cp key.pem certs:/tls/
docker compose run --rm --entrypoint sh certs -c 'chown 65532:65532 /tls/*.pem; chmod 600 /tls/key.pem; chmod 644 /tls/cert.pem'
docker compose restart gateway authelia
```

`docker compose cp` takes one source per run, which is why the pair goes in twice.

If your certificate is issued for different hostnames, change each route's `match.host` and the
`valid_issuers` in `config/gateway/gateway.yaml`, `authelia_url`, the cookie domain, the
`access_control` rule, the TUI client's `audience` and the redirect URI in
`config/authelia/configuration.yml`, and `server.allowed_hosts` in
`config/featherbittorrent/config.yaml`. Then delete the two `ssl_verify` lines.

## Changing something else

| Want to | Do |
|---|---|
| Pin the gateway image | Set `FEATHERBIT_TAG` in `.env`, or replace `${FEATHERBIT_TAG:-latest}` in `compose.yaml`. |
| Use a published featherbittorrent image | Replace the `build:` block under the `featherbittorrent` service with `image: <your image>`. |
| Publish the server's own port | Add `- "8090:8090"` to the `featherbittorrent` service. Only do this on a machine where nothing else can reach that port, since its logins are off. |
| Move BitTorrent off 6881 | Change the two `6881` entries in `compose.yaml` and `listen_port` in `config/featherbittorrent/config.yaml`. |
| Adjust rate limits | Change `count` and `time_window` on both limit-count nodes in `config/gateway/gateway.yaml`. That file is hot-reloaded: no restart needed. |

## Troubleshooting

- **Authelia exits at startup.** `docker compose logs authelia` names each problem. The usual ones:
  `x509: invalid ECDSA parameters` means the client key was pasted without the `openssl ec` step
  of step 4; a placeholder still in `configuration.yml` or `users_database.yml` fails the same way.
- **The gateway returns `404` with `No route matched`.** The route matches on the host name: open
  `https://torrent.example.test/` by that name, and keep `http2.enabled: false` in `system.yaml`
  (see "What has to line up").
- **The gateway returns `401` on `/api/*`.** The token was rejected or missing. The usual causes
  are a mismatch between the token URL you typed and the issuer Authelia reports, or `key_id` in
  `configuration.yml` not matching the fingerprint of the key you sign with (Authelia's log then
  names the client and the reason).
- **The gateway returns `502` with `discovery fetch failed`.** The gateway could not reach
  `https://auth.example.test:9443`: Authelia is not up, or its listen port and the published port
  no longer agree.
- **The TUI says the certificate is not trusted.** Expected with the demo pair; see the note under
  the terminal section.
- **`403` with `the authenticator rejected this request` from featherbittorrent.** With logins off
  the server rejects a Host (or Origin) that is neither loopback nor one of its `allowed_hosts`.
  Add the bare name (no port) to `server.allowed_hosts` in `config/featherbittorrent/config.yaml`.
- **Login bounces back to the portal forever.** The cookie domain has to cover both hostnames and
  `authelia_url` has to equal the URL you actually open; both are set for this example, so a change
  to one of them without the other is the usual cause.
- **Authelia will not start about the signing key.** `secrets/oidc_jwks_key` is not PEM, or someone
  edited the indentation around the `key:` line in `configuration.yml`. Regenerate the file; the
  filter does the indenting.
- **The portal says the client is unknown.** The connect screen's Client ID must match
  `client_id` in `configuration.yml` exactly, and its assertion must be signed by the registered
  key.
