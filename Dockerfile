# featherbittorrent serve, as small as the workspace lets it be.
#
#   docker build -t featherbittorrent .
#   docker run --rm -v "$PWD/data:/data" -p 8090:8090 -p 6881:6881 -p 6881:6881/udp \
#     -e FBTT_CONFIG_DIR=/data/config -e FBTT_DATA_DIR=/data/library featherbittorrent serve
#
# The first run wants a `config.yaml` under FBTT_CONFIG_DIR with a `server:` section, or an
# interactive terminal for the wizard (docs/server.md). Downloads go wherever the config's
# download_dir points; in the example stack that is inside the /data volume.

# Bookworm (Debian 12) on both sides: the binary built here needs exactly the glibc the release
# archives promise (see .github/workflows/release.yml), and rust-toolchain.toml picks the stable
# toolchain out of the builder image.
FROM rust:1-bookworm AS build
WORKDIR /app

# The lockfile, manifest and toolchain file first: a crates-only change reuses the dependency
# layer instead of rebuilding all 400+ of them.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --locked --release -p featherbittorrent

FROM debian:12
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/featherbittorrent /usr/local/bin/featherbittorrent

EXPOSE 8090 6881 6881/udp
ENTRYPOINT ["/usr/local/bin/featherbittorrent"]
CMD ["serve"]
