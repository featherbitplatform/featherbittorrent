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

# The lockfile, manifest and toolchain file first, and a stub source tree beside them: what cargo has to
# have to build 400+ dependencies is their manifests, not this workspace's code, so an empty `lib.rs` per
# crate is enough to compile every dependency once. Each crate's own manifest goes in its own directory
# because that is where the path dependencies say it lives, and a flattened copy of four of them into one
# folder would not be a workspace at all. The three test files named by a `[[test]]` section have to exist
# too - cargo reads a declared target's path when it reads the manifest - and an empty one is never built
# here, because those targets ask for the `test-support` feature this build does not turn on. What that
# costs in the log is a handful of "unused dependency" lines about the stubs, which stop at the next step.
# A crates-only change reuses this layer; the real sources go in after it, so editing code no longer costs
# a full dependency rebuild.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/fbtt-core/Cargo.toml crates/fbtt-core/
COPY crates/fbtt-server/Cargo.toml crates/fbtt-server/
COPY crates/fbtt-tui/Cargo.toml crates/fbtt-tui/
COPY crates/featherbittorrent/Cargo.toml crates/featherbittorrent/
RUN mkdir -p \
        crates/fbtt-core/src crates/fbtt-core/tests \
        crates/fbtt-server/src crates/fbtt-server/tests \
        crates/fbtt-tui/src \
        crates/featherbittorrent/src \
 && : > crates/fbtt-core/src/lib.rs \
 && : > crates/fbtt-core/tests/local.rs \
 && : > crates/fbtt-server/src/lib.rs \
 && : > crates/fbtt-server/tests/server.rs \
 && : > crates/fbtt-server/tests/remote.rs \
 && : > crates/fbtt-tui/src/lib.rs \
 && : > crates/featherbittorrent/src/main.rs \
 && cargo build --locked --release -p featherbittorrent

# The real sources, each one touched: cargo decides what to recompile by mtime, and a file copied out of
# the build context can carry an mtime older than the stub it replaces, which would leave the empty stub
# compiled into the image instead of the code.
COPY crates ./crates
RUN find crates -name '*.rs' -exec touch {} + \
 && cargo build --locked --release -p featherbittorrent

FROM debian:12
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/featherbittorrent /usr/local/bin/featherbittorrent

EXPOSE 8090 6881 6881/udp
ENTRYPOINT ["/usr/local/bin/featherbittorrent"]
CMD ["serve"]
