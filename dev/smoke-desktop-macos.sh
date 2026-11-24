#!/usr/bin/env bash
# Smokes the desktop app's macOS .dmg the way a person receives it: the .sha256 beside it, the image
# itself (hdiutil verify, mounted read-only), the one app inside it, and what that app tells the system
# about itself. The app's Info.plist must carry the bundle identifier io.featherbit.featherbittorrent.desktop,
# register the magnet URL scheme (CFBundleURLSchemes) and declare the .torrent document type
# (CFBundleDocumentTypes); those three are what make a magnet link or a .torrent open in this app, and no
# binary check can see them. The binary must hold arm64 and x86_64, be built against the macOS 26 SDK or
# newer in both slices (macOS draws the current window design only for such an app, the same guard as
# ../terminal's dev/macos-sdk.sh), and answer --version with exactly `featherbittorrent-desktop <version>`
# before Tauri opens anything, so the run needs no display and no login session. The app's ad-hoc
# signature must verify, and the bundle carries no LaunchAgent, LaunchDaemon or login item: no package
# starts anything at boot or login. Nothing is installed, copied to /Applications or left mounted.
# dev/package-desktop-macos.sh runs this on the image it has just written.
# Usage: bash dev/smoke-desktop-macos.sh <dist-dir> <version>   e.g. bash dev/smoke-desktop-macos.sh target/dist 0.3.0
set -euo pipefail

refuse() { echo "smoke-desktop-macos.sh: $*" >&2; exit 2; }
fail() { echo "FAIL: smoke-desktop-macos.sh: $*" >&2; exit 1; }

# Arguments first, before any platform or tool question, so the refusals can be tested on any OS.
[[ $# -eq 2 ]] || refuse "usage: bash dev/smoke-desktop-macos.sh <dist-dir> <version>"
dist="$1"
version="$2"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  refuse "invalid version '$version' (expected X.Y.Z or X.Y.Z-pre)"
fi

[[ "$(uname -s)" == "Darwin" ]] || fail "smoke-desktop-macos.sh runs on macOS only"
for tool in hdiutil plutil codesign lipo otool shasum perl; do
  command -v "$tool" >/dev/null || fail "smoke-desktop-macos.sh needs $tool"
done
[[ -d "$dist" ]] || fail "no such directory $dist"

name="featherbittorrent-desktop-${version}-macos-universal.dmg"
[[ -f "$dist/$name" ]] || fail "no $name in $dist"
[[ -f "$dist/$name.sha256" ]] || fail "no $name.sha256 in $dist"
# The sidecar names the file relative to its own folder, as sha256sum -c expects.
if ! sum_out="$( (cd "$dist" && shasum -a 256 -c "$name.sha256") 2>&1 )"; then
  fail "the .sha256 does not verify: $sum_out"
fi
echo "ok: shasum -a 256 -c accepts $name.sha256"

hdiutil verify "$dist/$name" >/dev/null 2>&1 || fail "hdiutil verify refuses $name"
echo "ok: hdiutil verify accepts $name"

mnt="$(mktemp -d)" || fail "could not make a mount point"
cleanup() { hdiutil detach "$mnt" -quiet 2>/dev/null || true; rmdir "$mnt" 2>/dev/null || true; }
trap cleanup EXIT
hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mnt" "$dist/$name" >/dev/null \
  || fail "hdiutil attach could not mount $name"

# One app, named as the desktop design says: the .dmg holds featherbittorrent.app.
apps=("$mnt"/*.app)
[[ ${#apps[@]} -eq 1 && -d "${apps[0]}" ]] || fail "expected exactly one .app in $name, found: $(ls "$mnt")"
app="${apps[0]}"
[[ "$(basename "$app")" == "featherbittorrent.app" ]] \
  || fail "the image holds $(basename "$app"), expected featherbittorrent.app"
[[ -L "$mnt/Applications" ]] || fail "the image has no Applications link to drag the app onto"
echo "ok: the image holds featherbittorrent.app and the Applications link"

plist="$app/Contents/Info.plist"
plutil -lint "$plist" >/dev/null || fail "plutil -lint refuses $plist"
value() { plutil -extract "$1" raw -o - "$plist" 2>/dev/null; }

id="$(value CFBundleIdentifier)" || fail "Info.plist has no CFBundleIdentifier"
[[ "$id" == "io.featherbit.featherbittorrent.desktop" ]] \
  || fail "CFBundleIdentifier is '$id', expected io.featherbit.featherbittorrent.desktop"
echo "ok: CFBundleIdentifier is $id"

# Every CFBundleURLSchemes entry of every CFBundleURLTypes entry, by index: plutil alone, no jq.
schemes=()
i=0
while plutil -extract "CFBundleURLTypes.$i" xml1 -o /dev/null "$plist" 2>/dev/null; do
  j=0
  while s="$(value "CFBundleURLTypes.$i.CFBundleURLSchemes.$j")"; do
    schemes+=("$s"); j=$((j + 1))
  done
  i=$((i + 1))
done
printf '%s\n' ${schemes[@]+"${schemes[@]}"} | grep -qx magnet \
  || fail "CFBundleURLSchemes does not list magnet (found: ${schemes[*]:-nothing})"
echo "ok: CFBundleURLSchemes lists magnet"

exts=()
i=0
while plutil -extract "CFBundleDocumentTypes.$i" xml1 -o /dev/null "$plist" 2>/dev/null; do
  j=0
  while e="$(value "CFBundleDocumentTypes.$i.CFBundleTypeExtensions.$j")"; do
    exts+=("$e"); j=$((j + 1))
  done
  i=$((i + 1))
done
printf '%s\n' ${exts[@]+"${exts[@]}"} | grep -qx torrent \
  || fail "CFBundleDocumentTypes does not list the torrent extension (found: ${exts[*]:-nothing})"
echo "ok: CFBundleDocumentTypes lists torrent"

for dir in Library/LaunchAgents Library/LaunchDaemons Library/LoginItems; do
  [[ ! -e "$app/Contents/$dir" ]] || fail "the app carries Contents/$dir: nothing may start at boot or login"
done
echo "ok: the app carries no LaunchAgent, LaunchDaemon or login item"

exe_name="$(value CFBundleExecutable)" || fail "Info.plist has no CFBundleExecutable"
exe="$app/Contents/MacOS/$exe_name"
[[ -x "$exe" ]] || fail "CFBundleExecutable names $exe_name, and $exe is not an executable"

archs="$(lipo -archs "$exe")" || fail "lipo -archs could not read $exe"
if ! grep -qw arm64 <<<"$archs" || ! grep -qw x86_64 <<<"$archs"; then
  fail "$exe holds '$archs', expected both arm64 and x86_64"
fi
echo "ok: $exe_name holds arm64 and x86_64"

# One "sdk X.Y" line per slice, from each LC_BUILD_VERSION load command.
sdks="$(otool -l "$exe" | awk '/LC_BUILD_VERSION/ {b=1} b && $1 == "sdk" {print $2; b=0}')"
[[ -n "$sdks" ]] || fail "no SDK version recorded in $exe"
while read -r sdk; do
  if (( ${sdk%%.*} < 26 )); then
    fail "$exe_name was built against macOS SDK $sdk; macOS 26 or newer is needed for the current window design (build with Xcode 26 or newer)"
  fi
done <<<"$sdks"
echo "ok: every slice was built against macOS SDK 26 or newer ($(tr '\n' ' ' <<<"$sdks" | sed 's/ $//'))"

sign_out="$(codesign --verify --deep --strict "$app" 2>&1)" || fail "codesign --verify refuses the app: $sign_out"
echo "ok: codesign --verify --deep --strict accepts the app"

# perl's alarm is the timeout macOS has without coreutils: a binary that opened a window instead of
# answering would otherwise hang the job.
got="$(perl -e 'alarm 20; exec @ARGV' "$exe" --version 2>&1)" || fail "$exe_name --version failed: $got"
[[ "$got" == "featherbittorrent-desktop $version" ]] \
  || fail "$exe_name --version printed '$got', expected 'featherbittorrent-desktop $version'"
echo "ok: $exe_name --version printed '$got'"

echo "smoke ok: $name on macOS $(sw_vers -productVersion)"
