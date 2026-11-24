#!/usr/bin/env bash
# Install one Linux desktop artifact, run it on the container's own tools, and show that the user's
# settings are not lost when the package goes away.
#
#   bash dev/smoke-desktop-linux.sh <deb|rpm|tar.gz> <package-dir> <version>
#
# The version is passed in rather than derived here: the artifact's own name and metadata are what is
# being proven, so the script must not read them to know what to look for.
#
# The container must already have the runtime the desktop needs: GTK 3, webkit2gtk 4.1, GLib, libsoup,
# pango, cairo and libdbus. The deb and rpm pull their declared runtime from the distribution, which
# is the point of installing them: an unsatisfiable Depends or Requires line fails the install. The
# tar.gz runs on the host's libraries, so its container is expected to have them installed already.
# xvfb and pkill come with the smoke test, not with the package, exactly like the CLI smoke test's
# libglib and D-Bus fallbacks.
#
# The installed .desktop file is checked because the whole reason the package exists is the magnet
# association: an installed file that does not declare x-scheme-handler/magnet and
# application/x-bittorrent is a desktop integration that was never wired up, and no binary check can
# see that. The icon is checked alongside it: a launcher whose Icon resolves to nothing is half the
# same bug.
#
# The settings file is written into a HOME of our own (smoke-home) rather than a system directory: a
# file the desktop binary itself creates, in a directory no package owns, is exactly the situation an
# uninstall can get wrong. The check needs the app to have run once, so the script starts it
# backgrounded under xvfb, waits for the file the app writes on first start (crates/
# featherbittorrent-desktop/src/local.rs writes config.yaml before the window opens), stops it, and
# removes the package around it. The .desktop file and the icon must go with the package; the user's
# config file must remain.
#
# Run it inside the distribution container it is about to install onto:
#
#   docker run --rm -v "$PWD:/src" -w /src debian:12 \
#     bash dev/smoke-desktop-linux.sh deb target/dist 0.3.0
#   docker run --rm -v "$PWD:/src" -w /src fedora:41 \
#     bash dev/smoke-desktop-linux.sh rpm target/dist 0.3.0
#   docker run --rm -v "$PWD:/src" -w /src ubuntu:22.04 \
#     bash dev/smoke-desktop-linux.sh tar.gz target/dist 0.3.0
#
# It refuses to run outside a container or CI for the same reason dev/smoke-linux.sh does: installing
# a package onto the machine editing the scripts is a side effect nobody asked for.
set -euo pipefail

pkg_kind="${1:-}"
pkg_dir="${2:-}"
version="${3:-}"

if [[ -z "$pkg_kind" || -z "$pkg_dir" || -z "$version" ]]; then
  echo "usage: smoke-desktop-linux.sh <deb|rpm|tar.gz> <package-dir> <version>" >&2
  exit 2
fi
if [[ ! -d "$pkg_dir" ]]; then
  echo "FAIL: no such package directory $pkg_dir" >&2
  exit 1
fi
case "$pkg_kind" in deb|rpm|tar.gz) ;; *)
  echo "FAIL: unknown package kind $pkg_kind (expected deb, rpm or tar.gz)" >&2
  exit 2 ;;
esac
command -v sha256sum >/dev/null || { echo "FAIL: sha256sum is missing" >&2; exit 1; }

name='featherbittorrent-desktop'
# The package path is resolved to an absolute one: apt-get reads a bare relative path as a package
# name ("target/dist/....deb" is not a file to it), and dnf behaves the same way.
pkg="$(cd "$pkg_dir" && pwd)/$name-$version-linux-$(uname -m).$pkg_kind"
[[ -f "$pkg" ]] || { echo "FAIL: no such package $pkg" >&2; exit 1; }

# The sidecar names the file beside it, so the check runs from inside the directory, exactly as
# dev/smoke-linux.sh does: a renamed artifact or a tampered file both fail sha256sum -c.
[[ -f "$pkg.sha256" ]] || { echo "FAIL: no checksum beside $pkg (expected $(basename "$pkg").sha256)" >&2; exit 1; }
(cd "$(dirname "$pkg")" && sha256sum -c "$(basename "$pkg").sha256") >/dev/null \
  || { echo "FAIL: $(basename "$pkg") does not match $(basename "$pkg").sha256" >&2; exit 1; }
echo "ok: $(basename "$pkg") matches its sha256"

arch="$(uname -m)"
# shellcheck source=/dev/null
pretty="$(. /etc/os-release; echo "${PRETTY_NAME:-unknown}")"

# The guard dev/smoke-linux.sh uses: this script installs and removes system packages, so it belongs
# in a throwaway container or on a CI runner, never on the machine that built the artifact.
if [[ ! -e /.dockerenv && ! -e /run/.containerenv && -z "${CI:-}" ]]; then
  echo "FAIL: smoke-desktop-linux.sh installs and removes system packages: run it in a container or in CI" >&2
  exit 1
fi

pkg_installed='no'
pkg_removed='no'
apt_get_install() {
  if [[ "${FBT_SMOKE_APT_UPDATED:-}" != 1 ]]; then
    DEBIAN_FRONTEND=noninteractive apt-get update -qq
    FBT_SMOKE_APT_UPDATED=1
  fi
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "$@"
}

# apt-get installs a .deb by path and resolves its Depends line itself, which is the check: the
# declared runtime is pulled from the distribution, with no Recommends, exactly as an unattended
# machine would do it. xvfb and procps are ours, not the package's.
install_the_package() {
  case "$pkg_kind" in
    # xauth is only a Recommends of xvfb on Debian, and xvfb-run refuses to start without it.
    deb) apt_get_install xvfb xauth procps "$pkg" >/dev/null ;;
    rpm)
      dnf -y -q install xorg-x11-server-Xvfb procps-ng >/dev/null
      dnf -y -q --setopt=install_weak_deps=False install "$pkg" >/dev/null ;;
    tar.gz)
      apt_get_install xvfb xauth procps libgtk-3-0 libwebkit2gtk-4.1-0 >/dev/null
      tar -C / -xzf "$pkg" ;;
  esac
}

remove_the_package() {
  case "$pkg_kind" in
    deb) DEBIAN_FRONTEND=noninteractive dpkg --purge "$name" >/dev/null ;;
    rpm) dnf -y -q remove "$name" >/dev/null ;;
    tar.gz)
      for f in "$bin_abs" "$desktop_file" "$icon_abs" "$icon2x_abs" "/usr/local/share/doc/$name/README-desktop.txt"; do
        [[ -n "$f" ]] && rm -f "$f"
      done
      rmdir -p --ignore-fail-on-non-empty \
        /usr/local/share/icons/hicolor/256x256@2/apps \
        /usr/local/share/icons/hicolor/128x128/apps \
        /usr/local/share/doc/"$name" 2>/dev/null || true
      ;;
  esac
}

if ! install_the_package; then
  echo "FAIL: installing $(basename "$pkg") on $pretty $arch failed: the declared runtime dependencies are not satisfiable there"
  exit 1
fi
pkg_installed='yes'

# Where the package put things. deb and rpm follow the FHS (/usr/bin, /usr/share); the tar.gz installs
# under /usr/local. Finding the binary through PATH rather than hardcoding either root is the honest
# check: if a future bundler moves the binary, the smoke test follows it, and a package that installs
# nothing runnable fails here instead of passing by accident.
bin_abs="$(command -v "$name" 2>/dev/null || true)"
[[ -n "$bin_abs" ]] || { echo "FAIL: $(basename "$pkg") installed but $name is not on PATH; the package owns no runnable binary" >&2; exit 1; }
desktop_file=''
for d in /usr/share/applications /usr/local/share/applications; do
  [[ -f "$d/$name.desktop" ]] && desktop_file="$d/$name.desktop"
done
icon_abs=''
icon2x_abs=''
for d in /usr/share/icons/hicolor/128x128/apps /usr/local/share/icons/hicolor/128x128/apps; do
  [[ -f "$d/$name.png" ]] && icon_abs="$d/$name.png"
done
# The @2 copy: tauri installs the 256-pixel png as hicolor 256x256@2 (the retina spelling of 128
# logical px), and the tarball ships it in the same directory name.
for d in /usr/share/icons/hicolor/256x256@2/apps /usr/local/share/icons/hicolor/256x256@2/apps; do
  [[ -f "$d/$name.png" ]] && icon2x_abs="$d/$name.png"
done

# The version the artifact declares must be the version passed in, printed by the binary itself and
# repeated in the package's own metadata. A package built for 0.3.0 that installs a 0.4.0 binary is a
# misfiled artifact; the desktop binary prints "<name> <version>" with no variant marker.
got_version="$("$bin_abs" --version 2>&1 | head -1)"
[[ "$got_version" == "$name $version" ]] || { echo "FAIL: $(basename "$pkg") installed a binary reporting \"$got_version\", expected \"$name $version\"" >&2; exit 1; }
echo "ok: $(basename "$pkg") installed $name $version ($bin_abs) on $pretty $arch"

case "$pkg_kind" in
  deb) meta="$(dpkg-query -W -f='${Version}' "$name" 2>/dev/null || true)" ;;
  rpm) meta="$(rpm -q --qf '%{VERSION}' "$name" 2>/dev/null || true)" ;;
  tar.gz) meta="$(grep -oE 'Version: [0-9][0-9A-Za-z.~-]*' /usr/local/share/doc/"$name"/README-desktop.txt 2>/dev/null | head -1 | cut -d' ' -f2)" ;;
esac
if [[ -z "$meta" ]]; then
  echo "FAIL: $(basename "$pkg") carries no version metadata; dpkg/rpm metadata or README-desktop.txt is missing" >&2
  exit 1
fi
if [[ "$meta" != "$version" ]]; then
  echo "FAIL: $(basename "$pkg") declares version $meta, expected $version" >&2
  exit 1
fi
echo "ok: $(basename "$pkg") declares version $meta in its own metadata"

# The binary must run under a virtual display: webkit2gtk constructs its webview against a display and
# a running display server, so --version alone (which exits before any display is opened) does not
# prove the install is good. Printing the version through xvfb-run exercises the same loader path a
# double-click takes, with no window manager wanted.
if ! command -v xvfb-run >/dev/null; then
  echo "FAIL: xvfb-run is missing, cannot run the GUI on $pretty" >&2
  exit 1
fi
out="$(xvfb-run -a "$bin_abs" --version 2>&1 | head -1)"
[[ "$out" == "$name $version" ]] || { echo "FAIL: $bin_abs under xvfb printed \"$out\", expected \"$name $version\"" >&2; exit 1; }
echo "ok: $name runs on the virtual X server on $pretty $arch"

# The desktop file is the magnet + torrent association, and the icon is what makes the launcher
# findable. A .desktop without MimeType= means the package was never taught why it exists.
[[ -n "$desktop_file" ]] || { echo "FAIL: $(basename "$pkg") installed no $name.desktop under /usr/share/applications or /usr/local/share/applications" >&2; exit 1; }
grep -q '^Exec=' "$desktop_file" || { echo "FAIL: $desktop_file has no Exec line" >&2; exit 1; }
# A launcher without a URL field code starts the app but never tells it what was clicked: a magnet
# link or .torrent file handed to the desktop would open the window and drop the link on the floor.
if ! grep -qE '^Exec=[^[:space:]]+[[:space:]]+%[uU]([[:space:]]|$)' "$desktop_file"; then
  echo "FAIL: $desktop_file's Exec line has no %u or %U field code, so clicked links never reach the app" >&2
  exit 1
fi
if ! grep '^MimeType=' "$desktop_file" | sed 's/^MimeType=//' | tr ';' '\n' | grep -qx 'x-scheme-handler/magnet'; then
  echo "FAIL: $desktop_file does not declare x-scheme-handler/magnet in MimeType=" >&2
  exit 1
fi
if ! grep '^MimeType=' "$desktop_file" | sed 's/^MimeType=//' | tr ';' '\n' | grep -qx 'application/x-bittorrent'; then
  echo "FAIL: $desktop_file does not declare application/x-bittorrent in MimeType=" >&2
  exit 1
fi
# The menu label is the product's name on every system and from every format: the .deb, the .rpm and
# the tarball all list featherbittorrent, as macOS and Windows do.
if [[ "$(grep '^Name=' "$desktop_file")" != 'Name=featherbittorrent' ]]; then
  echo "FAIL: $desktop_file lists the launcher as \"$(grep '^Name=' "$desktop_file" | sed 's/^Name=//')\", expected \"featherbittorrent\"" >&2
  exit 1
fi
if ! grep -q '^Icon=' "$desktop_file"; then
  echo "FAIL: $desktop_file has no Icon line" >&2
  exit 1
fi
[[ -n "$icon_abs" ]] || { echo "FAIL: $(basename "$pkg") installed no 128x128 $name.png in the hicolor icon tree; $desktop_file's Icon resolves to nothing" >&2; exit 1; }
echo "ok: $(basename "$pkg") installs $desktop_file declaring magnet and torrent, with the icon at $icon_abs"

# The desktop file installs an application, not a service: nothing may start at boot or login. The
# package must own no unit files, the autostart dir must stay empty, and no boot- or login-time wants
# directory may reference it.
bad_units="$(systemctl list-unit-files 2>/dev/null | grep -i featherbittorrent || true)"
if [[ -n "$bad_units" ]]; then
  echo "FAIL: the desktop package owns systemd units, which would start at boot: $bad_units" >&2
  exit 1
fi
if [[ -e /etc/systemd/system/"$name".service ]]; then
  echo "FAIL: the desktop package installed /etc/systemd/system/$name.service" >&2
  exit 1
fi
if [[ -e /etc/xdg/autostart/"$name".desktop ]]; then
  echo "FAIL: the desktop package installed /etc/xdg/autostart/$name.desktop, which starts it at login" >&2
  exit 1
fi
wants_listing="$(ls -1 /etc/systemd/system/multi-user.target.wants /etc/systemd/system/default.target.wants \
  /run/systemd/system/user@1000.service.d 2>/dev/null || true)"
if grep -qi featherbittorrent <<<"$wants_listing"; then
  echo "FAIL: a boot or login wants directory references $name, which would start it without anyone asking: $(grep -i featherbittorrent <<<"$wants_listing" | head -1)" >&2
  exit 1
fi
echo "ok: nothing the desktop package installs starts at boot or login"

# The settings-survival proof: HOME becomes a directory of our own so the run cannot touch a real
# user's config, the app starts backgrounded under xvfb, the wait is for the config file the app
# writes on first start (before any window opens), and the kill is SIGTERM, the same quit path a
# window close takes. pkill -f matches command lines containing the binary's absolute path, which
# are the app and its xvfb-run parent (and nothing else): -x would match the 15-char truncated comm
# (featherbittorrent-de), true of every featherbittorrent process, and a bare name would hit any
# unrelated command that merely mentions it.
export HOME=/root/smoke-home
mkdir -p "$HOME"
config_file="$HOME/.config/$name/config.yaml"
rm -rf "$HOME/.config"
xvfb-run -a "$bin_abs" >/dev/null 2>&1 &
app_job=$!
appeared='no'
for _ in $(seq 1 30); do
  if [[ -f "$config_file" ]]; then
    appeared='yes'
    break
  fi
  if ! kill -0 "$app_job" 2>/dev/null; then
    break
  fi
  sleep 1
done
pkill -TERM -f "$bin_abs" 2>/dev/null || true
kill -TERM "$app_job" 2>/dev/null || true
wait "$app_job" 2>/dev/null || true
if [[ "$appeared" != yes ]]; then
  echo "FAIL: the app never wrote $config_file within 30s; a settings file an uninstall could destroy is the whole proof" >&2
  exit 1
fi
echo "ok: $name wrote its settings file $config_file under HOME=$HOME"

remove_the_package
pkg_removed='yes'
if [[ -f "$config_file" ]]; then
  echo "ok: $config_file survived removing $(basename "$pkg"); the user's settings are not package state"
else
  echo "FAIL: $config_file vanished when $(basename "$pkg") was removed; the package is owning, or wiping, the user's settings" >&2
  exit 1
fi
# What the package *does* own must go with it: a leftover binary or launcher starts a ghost at
# login or keeps a dead magnet handler registered.
for leftover in "$bin_abs" "$desktop_file" "$icon_abs"; do
  if [[ -e "$leftover" ]]; then
    echo "FAIL: $leftover survived removing $(basename "$pkg"); the package's own files must go with it" >&2
    exit 1
  fi
done

if [[ "$pkg_kind" == deb ]]; then
  status="$(dpkg-query -W -f='${Status}' "$name" 2>/dev/null || true)"
  if [[ "$status" == *"ok installed"* ]]; then
    echo "FAIL: dpkg still reports $name installed after purge (status: $status); the removal did not take" >&2
    exit 1
  fi
  echo "ok: dpkg reports $name purged (status: ${status:-not-installed})"
fi

echo "smoke ok: $pkg_kind on $pretty $arch; settings file $([[ -f "$config_file" ]] && echo preserved || echo lost), package installed=$pkg_installed removed=$pkg_removed"
