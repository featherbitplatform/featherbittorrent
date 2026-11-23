#!/usr/bin/env bash
# Builds the desktop app as one universal .dmg, the macOS artifact the release page ships for it, then
# smokes that .dmg with dev/smoke-desktop-macos.sh. Runs on macOS only (hdiutil, codesign, plutil and lipo
# are Apple's own tools).
#
# The bundle comes from the Tauri bundler, `cargo tauri build --bundles app,dmg --target
# universal-apple-darwin`, with tauri-cli pinned at 2.12.6 (`cargo install --locked tauri-cli@2.12.6`;
# this script refuses any other version, so the bundler that writes a release is a reviewed one). The
# bundler builds both Apple architectures and joins them with lipo, writes Info.plist from
# crates/featherbittorrent-desktop/tauri.conf.json (the identifier, the magnet scheme from the deep-link
# plugin, the .torrent document type from bundle.fileAssociations, LSMinimumSystemVersion from
# bundle.macOS.minimumSystemVersion) and signs the app ad-hoc (bundle.macOS.signingIdentity "-"): Apple
# Silicon runs nothing without a signature, and the app is unsigned for distribution like ../terminal's,
# so Gatekeeper warns once. The 11.0 floor is the web UI's: the page is built for the WebKit of Safari 16.4
# and newer, which Big Sur is the oldest macOS to receive. Xcode is whatever xcode-select points at; the
# release workflow selects Xcode 26 or newer first, and the smoke refuses a binary built against an older
# SDK, as ../terminal does, because macOS draws the current window design only for such an app.
#
# The .dmg: in CI the bundler's own image is the artifact, and a failure fails the job. Locally (no $CI),
# hdiutil create fails with "Resource busy" on some newer macOS releases (seen on macOS 27); when the
# bundler's .dmg step fails but the .app was written, this script makes the image from that .app itself,
# with hdiutil and then `diskutil image create` as ../terminal's dev/package-macos.sh does. CI never falls
# back. The bundler runs with CI=true either way, which skips the Finder window layout step (AppleScript
# that needs a logged-in Finder and an automation permission), so both paths give the same plain image:
# the app and an Applications link.
#
# The front end must be built first (npm --prefix web ci && npm --prefix web run build): tauri.conf.json
# names web/dist as frontendDist and runs no build command of its own.
# Usage: bash dev/package-desktop-macos.sh <version>   e.g. bash dev/package-desktop-macos.sh 0.3.0
# Output: target/dist/featherbittorrent-desktop-<version>-macos-universal.dmg and its .sha256.
set -euo pipefail

refuse() { echo "package-desktop-macos.sh: $*" >&2; exit 2; }
fail() { echo "package-desktop-macos.sh: $*" >&2; exit 1; }

# Arguments first, before any platform or tool question, so the refusals can be tested on any OS.
[[ $# -eq 1 ]] || refuse "usage: bash dev/package-desktop-macos.sh <version>"
version="$1"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  refuse "invalid version '$version' (expected X.Y.Z or X.Y.Z-pre)"
fi

# Whether the caller is CI, read before CI=true is set for the bundler below.
in_ci="${CI:-}"

[[ "$(uname -s)" == "Darwin" ]] || fail "package-desktop-macos.sh runs on macOS only"
for tool in cargo hdiutil diskutil shasum plutil codesign lipo otool; do
  command -v "$tool" >/dev/null || fail "package-desktop-macos.sh needs $tool"
done
tauri_cli="$(cargo tauri --version 2>/dev/null)" \
  || fail "no tauri-cli: cargo install --locked tauri-cli@2.12.6"
[[ "$tauri_cli" == "tauri-cli 2.12.6" ]] \
  || fail "found $tauri_cli, the release bundler is tauri-cli 2.12.6: cargo install --locked tauri-cli@2.12.6"

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[[ -f web/dist/index.html ]] \
  || fail "web/dist is not built: npm --prefix web ci && npm --prefix web run build"

target="${CARGO_TARGET_DIR:-$root/target}"
bundle="$target/universal-apple-darwin/release/bundle"
dist="$root/target/dist"
name="featherbittorrent-desktop-${version}-macos-universal.dmg"
app="$bundle/macos/featherbittorrent.app"

# A stale .app or .dmg from an earlier run must not be mistaken for this run's.
rm -rf "${bundle:?}/macos" "${bundle:?}/dmg" "${dist:?}/$name" "${dist:?}/$name.sha256"
mkdir -p "$dist"

status=0
(cd crates/featherbittorrent-desktop &&
  CI=true cargo tauri build --bundles app,dmg --target universal-apple-darwin -- --locked) || status=$?

if [[ $status -eq 0 ]]; then
  dmgs=("$bundle"/dmg/*.dmg)
  [[ ${#dmgs[@]} -eq 1 && -f "${dmgs[0]}" ]] || fail "expected one .dmg under $bundle/dmg, found: $(ls "$bundle/dmg" 2>&1)"
  cp "${dmgs[0]}" "$dist/$name"
else
  [[ -z "$in_ci" ]] || fail "cargo tauri build failed (exit $status)"
  [[ -d "$app" ]] || fail "cargo tauri build failed (exit $status) before it wrote $app"
  echo "package-desktop-macos.sh: the bundler's .dmg step failed (exit $status); making the image from $app (local only)" >&2
  stage="$dist/stage-desktop-macos"
  rm -rf "$stage"
  mkdir -p "$stage"
  ditto "$app" "$stage/featherbittorrent.app"
  ln -s /Applications "$stage/Applications"
  hdiutil_err="$dist/hdiutil.err"
  if ! hdiutil create -volname featherbittorrent -srcfolder "$stage" -format UDZO "$dist/$name" >/dev/null 2>"$hdiutil_err"; then
    cat "$hdiutil_err" >&2
    echo "package-desktop-macos.sh: hdiutil create failed; falling back to diskutil image create (local only)" >&2
    rm -f "$dist/$name"
    diskutil image create from --format UDZO --volumeName featherbittorrent "$stage" "$dist/$name" >/dev/null \
      || fail "diskutil image create failed too"
  fi
  rm -f "$hdiutil_err"
  rm -rf "$stage"
fi

# shasum prints `hash  name` with two spaces, the shape sha256sum uses, so both read this file.
(cd "$dist" && shasum -a 256 "$name" > "$name.sha256") || fail "shasum failed in $dist"

bash dev/smoke-desktop-macos.sh "$dist" "$version"

echo "$dist/$name"
cat "$dist/$name.sha256"
