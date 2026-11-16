# featherbittorrent behind the gateway with Authelia

One host, three containers, HTTPS on `https://torrent.example.test`. The web app and the API are
served by the featherbit gateway. It forwards the web UI to the featherbittorrent server after an
interactive login through Authelia, and it forwards `/api/*` to the same server after checking a
bearer token that the TUI buys from Authelia on its own. BitTorrent itself goes straight to the
server container.

| Container | Listens | Published | Role |
|---|---|---|---|
| `featherbittorrent` | 8090 inside the network | 6881 tcp/udp only | web app and API, logins off |
| `gateway` | 8443 | 443 | HTTPS, OIDC on both doors, rate limits |
| `authelia` | 9091 (TLS) | 9443 | portal plus the OIDC provider |

Nothing publishes the server's own port: with its logins off it answers only to the gateway, which
is what [docs/reverse-proxy.md](../../reverse-proxy.md) calls the gateway-only setup.

## What has to line up

- **Names.** `torrent.example.test` and `auth.example.test` must resolve to this machine for your
  browser and for any TUI you run yourself. On one machine a pair of `/etc/hosts` lines is enough.
  Inside the compose network the containers reach each other by service name; the gateway also has
  an `extra_hosts` entry so it can call Authelia under the same hostname your browser uses.
- **The issuer.** Authelia serves its own TLS on 9091, published as 9443, so its issuer is exactly
  `https://auth.example.test:9443` and nothing in front has to rewrite a forwarded header. The
  session cookie covers `example.test`, which spans both names.
- **The token endpoint.** Since 4.39 the OIDC endpoints sit under `/api/oidc/`, so the URL the TUI
  posts its assertion to is `https://auth.example.test:9443/api/oidc/token`. That is what you type
  into the connect screen, and it is also the audience the assertion names.
- **Certificates.** The demo pair is self-signed and covers both hostnames, so both
  openid-connect nodes set `ssl_verify: false`. Delete those two lines once the certificate is a
  real one. Note the caveat for the terminal below: featherbittorrent itself verifies TLS against
  the public root certificates and has no switch to look the other way.

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
   openssl rand -hex 32 > secrets/web_client_secret
   openssl genrsa -out secrets/oidc_jwks_key 2048

   printf 'WEB_CLIENT_SECRET=%s\n' "$(cat secrets/web_client_secret)" > .env
   printf 'GATEWAY_SESSION_SECRET=%s\n' "$(openssl rand -hex 32)" >> .env
   ```

   `secrets/web_client_secret` and `WEB_CLIENT_SECRET` hold the same value on purpose: Authelia
   reads the file, the gateway reads the variable. `secrets/oidc_jwks_key` is the provider's RSA
   signing key in plain PEM; the template filter indents it into `configuration.yml` for us.

3. **The TUI's gateway key.** The terminal signs its assertion with an ECDSA P-256 key (Authelia
   accepts RSA or ECDSA client keys here, not Ed25519):

   ```sh
   ssh-keygen -t ecdsa -b 256 -f ~/.ssh/featherbittorrent_gateway -N '' -C 'featherbittorrent gateway'
   ```

4. **Teach Authelia that key.** Two placeholders sit in
   `config/authelia/configuration.yml`:

   ```sh
   ssh-keygen -lf ~/.ssh/featherbittorrent_gateway.pub
   ssh-keygen -f ~/.ssh/featherbittorrent_gateway.pub -e -m pkcs8
   ```

   The first prints the fingerprint: paste everything from `SHA256:` up to the next space over
   `REPLACE_WITH_YOUR_FINGERPRINT`. The second prints the same key as a PEM public key: paste its
   body lines over `REPLACE_WITH_YOUR_PUBLIC_KEY_LINES`, keeping the indentation. It is a public
   key, so it is fine that it lives in a committed file.

5. **A password for Alice.** This one comes last because the hashing tool loads this directory's
   configuration and refuses to start while its placeholders are still unfilled:

   ```sh
   docker compose run --rm authelia authelia crypto hash generate argon2 --password 'a passphrase'
   ```

   Copy the `Digest:` line over `REPLACE_ME` in `config/authelia/users_database.yml`, quotes
   included.

## Run it

```sh
docker compose up --build -d
docker compose logs gateway authelia featherbittorrent
```

The first run compiles featherbittorrent, which takes a few minutes; later runs reuse the cached
image unless you pass `--build` after changing Rust code. The `certs` service generates the
self-signed pair into its own volume on the first run and reuses it afterwards.

## Log in from a browser

Open `https://torrent.example.test/`. With a self-signed certificate your browser will complain
first; accept it once per hostname (`auth.example.test` gets its own complaint when you are sent to
the portal). The gateway redirects to Authelia, you sign in as `alice` with the password from step
5, and you land on the web app. `/logout` ends the gateway session.

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
gateway key signed, which is awkward to sign by hand from a shell; `$TOKEN` below therefore stands
for any valid access token this provider issued for a client you can authenticate as:

```sh
curl -sk https://torrent.example.test/api/v1/torrents \
  -H "Authorization: Bearer $TOKEN"
```

The gateway validates the token against Authelia's JWKS (signature, expiry, issuer) before the
request reaches featherbittorrent, and rate limits it on top. The body is the JSON that
[docs/server.md](../../server.md) describes.

## Use your own certificate

Put a pair covering both hostnames into the `certs` volume and nothing else changes: the generator
service leaves files that already exist alone. Copy them in, then fix their ownership the same way
the generator does, because the gateway reads the private key as uid 65532:

```sh
docker compose up -d certs
docker compose cp cert.pem key.pem certs:/tls/
docker compose run --rm --entrypoint sh certs -c 'chown 65532:65532 /tls/*.pem; chmod 600 /tls/key.pem; chmod 644 /tls/cert.pem'
docker compose restart gateway authelia
```

If your certificate is issued for different hostnames, change the `hosts:` entries and
`valid_issuers` in `config/gateway/gateway.yaml`, `authelia_url`, the cookie domain and the redirect
URI in `config/authelia/configuration.yml`, and `server.allowed_hosts` in
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

- **The gateway returns 401 on `/api/*`.** The token was rejected. The usual causes are a mismatch
  between the token URL you typed and the issuer Authelia reports, `key_id` in
  `configuration.yml` not matching the fingerprint of the key you sign with, or the registered
  client missing `client_assertion_jwt_validation_header_allow_types`. Authelia's log names the
  exact reason.
- **The TUI says the certificate is not trusted.** Expected with the demo pair; see the note under
  the terminal section.
- **`Host '...' is not allowed` from featherbittorrent.** The server rejects a Host header that is
  neither loopback nor one of its `allowed_hosts`. Add the bare name (no port) to
  `server.allowed_hosts` in `config/featherbittorrent/config.yaml`.
- **Login bounces back to the portal forever.** The cookie domain has to cover both hostnames and
  `authelia_url` has to equal the URL you actually open; both are set for this example, so a change
  to one of them without the other is the usual cause.
- **Authelia will not start about a key.** `secrets/oidc_jwks_key` is not PEM, or someone edited
  the indentation around the `key:` line in `configuration.yml`. Regenerate the file; the filter
  does the indenting.
- **The portal says the client is unknown.** The connect screen's Client ID must match
  `client_id` in `configuration.yml` exactly, and its assertion must be signed by the registered
  key.
