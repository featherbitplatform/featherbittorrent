#!/usr/bin/env bash
# Builds the featherbittorrent desktop app for Linux and packages it as .deb, .rpm and .tar.gz.
# Runs on Linux, natively for the given architecture (no cross-compiling), and must run inside the
# same Ubuntu 22.04 container that dev/check-linux-binary.sh names as the desktop floor: the webview
# stack (webkit2gtk 4.1, GTK 3, GLib, libsoup 3) comes from the distribution, so the container both
# builds against it and declares it. Release builds run there; on any other machine the script's own
# dev/check-linux-binary.sh --desktop run is what proves the floor, and building elsewhere is a
# choice, not something this script can see.
#
# Usage: bash dev/package-desktop-linux.sh <version> <x86_64|aarch64>
#   e.g. bash dev/package-desktop-linux.sh 0.3.0 aarch64
# Output: target/dist/featherbittorrent-desktop-<version>-linux-<arch>.{deb,rpm,tar.gz}, each with a
#         .sha256 sidecar, the same names and layout as dev/package-linux.sh uses for the CLI.
#
# The .deb and the .rpm come from cargo tauri build (tauri-cli, the version pinned by the release
# workflow: cargo install --locked tauri-cli@2.12.6). Their metadata lives in
# crates/featherbittorrent-desktop/tauri.conf.json: the package name is the kebab-cased productName
# (featherbittorrent-desktop, not featherbittorrent, so installing the desktop app never claims
# ownership of the CLI package's files), the .desktop file's MimeType= lines come from
# bundle.fileAssociations, and the runtime depends from bundle.linux.deb.depends and
# bundle.linux.rpm.depends. tauri's bundler writes no maintainer scripts and no scriptlets unless the
# config names them, adds no Conflicts, Replaces or Obsoletes unless the config names them, installs
# the binary to /usr/bin and the desktop file and icons to /usr/share; this script verifies each of
# those facts against the built artifacts rather than trusting the documentation about them.
#
# The .tar.gz is not a tauri bundle here: tauri's AppImage target needs a FUSE-capable host and its
# "tar.gz" is a raw binary, neither of which is what a user should get. This script packs the same
# cargo-built binary with the same .desktop file, icons and a README naming the runtime libraries,
# into an FHS tree under /usr/local (the CLI tarball drops a binary into whatever folder you unpack;
# this one unpacks onto a system root: tar -C / -xzf), and it carries no depends, so the README says
# in words what the packages declare in fields.
#
# No package installs or starts a service: the desktop binary is a login-session application. The
# check_no_service pass from dev/package-linux.sh runs over all three artifacts, the .deb control
# member may hold no maintainer script, and the .rpm may carry no scriptlet.
#
# Run it inside the Ubuntu 22.04 build container:
#
#   docker run --rm -v "$PWD:/src" -w /src \
#     -e CARGO_TARGET_DIR=/src/target/linux-desktop-docker \
#     -v fbt-desktop-cargo:/root/.cargo -v fbt-desktop-rustup:/root/.rustup \
#     ubuntu:22.04 bash -c '
#       set -euo pipefail
#       export DEBIAN_FRONTEND=noninteractive
#       apt-get update -qq
#       apt-get install -y -qq --no-install-recommends \
#         build-essential pkg-config file rpm cpio xz-utils curl ca-certificates gnupg \
#         libssl-dev libgtk-3-dev libwebkit2gtk-4.1-dev librsvg2-dev \
#         libsoup-3.0-dev libjavascriptcoregtk-4.1-dev
#       . "$HOME/.cargo/env"
#       cargo install --locked --target-dir /src/target/linux-desktop-docker tauri-cli@2.12.6
#       bash dev/package-desktop-linux.sh "$(bash dev/release-version.sh)" "$(uname -m)"'
#
# (The container needs no node: tauri.conf.json names web/dist as frontendDist and sets no
# beforeBuildCommand, so the build reads the web files the host already built.)
set -euo pipefail

fail() { echo "package-desktop-linux.sh: $*" >&2; exit 2; }

# RPM versions may not contain "-", and RPM sorts "~" before anything, so a prerelease maps to a
# tilde the way dev/package-linux.sh does. A variable holds it because on bash 3.2 the substitution
# "${version//-/\~}" keeps the backslash; "$tilde" is a plain tilde on every bash. (tauri's own
# bundler applies the same rule inside rpm.rs, so for a plain release the two agree exactly.)
tilde='~'

check_version() { # <version>; refuses anything that is not X.Y.Z or X.Y.Z(-pre)
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] \
    || fail "invalid version '$1' (expected X.Y.Z or X.Y.Z-pre)"
}

# Arguments first, before any platform or tool question, mirroring dev/package-linux.sh.
[[ $# -eq 2 ]] || fail "usage: bash dev/package-desktop-linux.sh <version> <x86_64|aarch64>"
version="$1"
arch="$2"
check_version "$version"
case "$arch" in
  x86_64|aarch64) ;;
  *) fail "invalid arch '$arch' (expected x86_64 or aarch64)" ;;
esac

[[ "$(uname -s)" == "Linux" ]] || fail "package-desktop-linux.sh runs on Linux only"
[[ "$(uname -m)" == "$arch" ]] || fail "this machine is $(uname -m), not $arch (no cross-compiling)"
for tool in cargo ar cpio rpm2cpio rpm objdump gzip tar sha256sum; do
  command -v "$tool" >/dev/null || fail "package-desktop-linux.sh needs $tool"
done
command -v cargo-tauri >/dev/null || cargo tauri --version >/dev/null 2>&1 \
  || fail "cargo tauri is missing: cargo install --locked tauri-cli@2.12.6"

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
target="${CARGO_TARGET_DIR:-$root/target}"
dist="$root/target/dist"
name='featherbittorrent-desktop'
pkg="${name}-${version}-linux-${arch}"
conf="crates/featherbittorrent-desktop/tauri.conf.json"
# tauri's deb member names follow Debian's arch spelling; the artifact names follow uname -m, like
# every other artifact in target/dist.
case "$arch" in x86_64) deb_arch=amd64 ;; aarch64) deb_arch=arm64 ;; *) deb_arch="$arch" ;; esac

# The web assets are the app's front end: cargo tauri build refuses an empty frontendDist on its own,
# but the failure there ("the frontend dist directory is empty") is cryptic next to the one command
# that fills it, so check it here with the reason attached.
[[ -f web/dist/index.html ]] || fail "web/dist is missing: run npm --prefix web ci && npm --prefix web run build first"

# The depends the packages declare, read out of the config rather than retyped here: the script then
# compares them against what the artifacts actually carry, so the config, the artifact and the check
# all have to agree.
config_depends() { # <deb|rpm>; prints one package name per line from bundle.linux.<kind>.depends
  # The section key must be "deb": or "rpm": inside the "linux" block (a colon after the quoted
  # name), or the "deb" and "rpm" entries of bundle.targets would open the section early and the
  # wrong depends array would be read for rpm.
  awk -v sect="\"$1\":" '
    index($0, "\"linux\"") { inlinux = 1 }
    inlinux && index($0, sect) { insec = 1 }
    insec && index($0, "\"depends\"") { inarr = 1; next }
    inarr && /\]/ { inarr = 0; insec = 0; next }
    inarr {
      gsub(/["[:space:]]/, "")
      sub(/,$/, "")
      if ($0 != "") print
    }
  ' "$conf"
}

mkdir -p "$dist"
stage="$dist/stage-desktop-${arch}"
rm -rf "$stage" "$dist/$pkg.deb" "$dist/$pkg.rpm" "$dist/$pkg.tar.gz" \
  "$dist/$pkg.deb.sha256" "$dist/$pkg.rpm.sha256" "$dist/$pkg.tar.gz.sha256"

# The build: cargo tauri build runs cargo build --release for the crate and then the bundler for the
# formats named. --locked goes after -- so it reaches cargo, keeping the dependency set of a release
# build exactly the one Cargo.lock names.
cargo tauri build --bundles deb,rpm -- --locked

bin="$target/release/featherbittorrent-desktop"
[[ -x "$bin" ]] || fail "cargo tauri build produced no $bin"
if ! printed="$("$bin" --version)"; then fail "$bin --version failed to run"; fi
# The whole string, like the CLI script: the desktop binary prints "<name> <version>" with no variant
# marker, and a package built from the wrong tree is refused here rather than published.
[[ "$printed" == "$name $version" ]] || fail "--version printed '$printed', expected '$name $version'"
bash dev/check-linux-binary.sh --desktop "$bin"

# The bundler writes bundle/deb/<name>_<version>_<debarch>.deb and
# bundle/rpm/<name>-<version>-1.<arch>.rpm (its own naming, from tauri-bundler's debian.rs and
# rpm.rs); target/dist carries the release naming the smoke tests and the release workflow expect, so
# the two are copied here rather than renamed in place, and exactly-one-of each is required.
mapfile -t built_debs < <(find "$target/release/bundle/deb" -maxdepth 1 -name '*.deb' 2>/dev/null | sort)
mapfile -t built_rpms < <(find "$target/release/bundle/rpm" -maxdepth 1 -name '*.rpm' 2>/dev/null | sort)
[[ ${#built_debs[@]} -eq 1 ]] || fail "expected exactly one .deb under $target/release/bundle/deb, found ${#built_debs[@]}"
[[ ${#built_rpms[@]} -eq 1 ]] || fail "expected exactly one .rpm under $target/release/bundle/rpm, found ${#built_rpms[@]}"
deb="${built_debs[0]}"
rpm="${built_rpms[0]}"
install -m644 "$deb" "$dist/$pkg.deb"
install -m644 "$rpm" "$dist/$pkg.rpm"

# --- the .deb, read with ar and tar (no dpkg-deb needed, the same path dev/package-linux.sh uses).
if ! members="$(ar t "$dist/$pkg.deb" 2>&1)"; then fail "ar could not read $dist/$pkg.deb: $(head -1 <<<"$members")"; fi
data="$(grep '^data\.tar' <<<"$members" | head -1)" || true
control="$(grep '^control\.tar' <<<"$members" | head -1)" || true
[[ -n "$data" && -n "$control" ]] || fail "the .deb has no data.tar or control.tar member (ar listed: $(tr '\n' ' ' <<<"$members"))"
mkdir -p "$stage/deb"
ar p "$dist/$pkg.deb" "$data" > "$stage/$data"
case "$data" in
  *.gz) data_opt=(--gzip) ;; *.xz) data_opt=(--xz) ;; *.zst) data_opt=(--zstd) ;; *) data_opt=() ;;
esac
if ! deb_files="$(tar ${data_opt[@]+"${data_opt[@]}"} -tf "$stage/$data" 2>&1)"; then
  fail "tar could not list the .deb data member $data: $(head -1 <<<"$deb_files")"
fi
# tauri's data.tar.gz stores members as "usr/..." without the ./ prefix a GNU tar -cf would add, so
# the matches below are written without it; and the 128x128@2x png is 256 pixels wide, so the
# bundler installs it as hicolor 256x256@2 (the retina spelling of 128 CSS px), like this:
#   32x32   <- icons/32x32.png
#   128x128 <- icons/128x128.png
#   256x256@2 <- icons/128x128@2x.png
want_desktop=(
  "usr/bin/$name"
  "usr/share/applications/$name.desktop"
  "usr/share/icons/hicolor/32x32/apps/$name.png"
  "usr/share/icons/hicolor/128x128/apps/$name.png"
  "usr/share/icons/hicolor/256x256@2/apps/$name.png"
)
for f in "${want_desktop[@]}"; do
  grep -qx "$f" <<<"$deb_files" || fail "the .deb is missing /$f"
done

ar p "$dist/$pkg.deb" "$control" > "$stage/$control"
if ! control_files="$(tar --gzip -tf "$stage/$control" 2>&1)"; then
  fail "tar could not list the .deb control member $control: $(head -1 <<<"$control_files")"
fi
while IFS= read -r entry; do
  [[ -z "$entry" ]] && continue
  case "${entry#./}" in
    control|md5sums) ;;
    *) fail "the .deb control member holds $entry, which is a maintainer script or its data, and no package may run one" ;;
  esac
done <<<"$control_files"
deb_control="$(tar --gzip -xOf "$stage/$control" ./control 2>/dev/null || tar --gzip -xOf "$stage/$control" control)"

deb_pkg="$(grep -E '^Package: ' <<<"$deb_control" | head -1 | sed 's/^Package: //' || true)"
deb_ver="$(grep -E '^Version: ' <<<"$deb_control" | head -1 | sed 's/^Version: //' || true)"
deb_arc="$(grep -E '^Architecture: ' <<<"$deb_control" | head -1 | sed 's/^Architecture: //' || true)"
[[ "$deb_pkg" == "$name" ]] || fail "the .deb's control says 'Package: $deb_pkg', expected 'Package: $name': installing the desktop app must never take over the CLI package's name"
[[ "$deb_ver" == "$version" ]] || fail "the .deb's control says 'Version: $deb_ver', expected '$version'"
[[ "$deb_arc" == "$deb_arch" ]] || fail "the .deb's control says 'Architecture: $deb_arc', expected '$deb_arch'"
# tauri's bundler writes Conflicts:/Replaces:/Breaks: only what the config declares, and the desktop
# package declares none: it installs into /usr/bin and /usr/share paths the CLI package does not use
# (its binary is featherbittorrent, this one featherbittorrent-desktop), so the two coexist and the
# control file must not say otherwise.
for field in Conflicts Replaces Breaks; do
  if grep -q "^${field}: " <<<"$deb_control"; then
    fail "the .deb's control declares ${field}, which it must not: the desktop and CLI packages own disjoint paths and coexist"
  fi
done
# The Depends line must carry exactly what bundle.linux.deb.depends names, as a set: tauri-cli's
# rust.rs (1431) appends its own libwebkit2gtk-4.1-0 and libgtk-3-0 without checking the config list
# first, so the artifact repeats those two (dpkg treats a repeated dependency as satisfied by the same
# install) while anything else that drifted - a dropped entry, a renamed one - shows up as a set
# difference here rather than as a broken install three distributions later. The config keeps the two
# entries anyway: the Depends the user reads should say the whole truth even though tauri would add
# them if the config stayed silent.
deb_depends="$(grep -E '^Depends: ' <<<"$deb_control" | head -1 | sed 's/^Depends: //' || true)"
conf_deb="$(config_depends deb | sort -u)"
art_deb="$(tr ',' '\n' <<<"${deb_depends:- }" | sed 's/^ *//;s/ *$//' | grep -v '^$' | sort -u)" || true
if [[ "$conf_deb" != "$art_deb" ]]; then
  fail "the .deb's Depends ($(tr '\n' ' ' <<<"$art_deb")) differs from $conf's bundle.linux.deb.depends ($(tr '\n' ' ' <<<"$conf_deb"))"
fi
echo "ok: the .deb is $deb_pkg $deb_ver $deb_arc with exactly the configured Depends"

# The .desktop file as packed: magnet first and torrent second in MimeType=, which is the association
# the whole package exists to register. (smoke-desktop runs the same grep against the installed copy;
# catching it here fails the build instead of a rollout.)
deb_desktop="$(tar ${data_opt[@]+"${data_opt[@]}"} -xOf "$stage/$data" "usr/share/applications/$name.desktop" 2>/dev/null)"
[[ -n "$deb_desktop" ]] || fail "the .deb's data member has no readable ./usr/share/applications/$name.desktop"
grep '^MimeType=' <<<"$deb_desktop" | sed 's/^MimeType=//' | tr ';' '\n' | grep -qx 'x-scheme-handler/magnet' \
  || fail "the .deb's $name.desktop does not declare x-scheme-handler/magnet"
grep '^MimeType=' <<<"$deb_desktop" | sed 's/^MimeType=//' | tr ';' '\n' | grep -qx 'application/x-bittorrent' \
  || fail "the .deb's $name.desktop does not declare application/x-bittorrent"

# --- the .rpm, read with rpm (the build container carries it through apt).
if ! rpm_files="$(rpm -qlp "$dist/$pkg.rpm" 2>&1)"; then fail "rpm -qlp could not read $dist/$pkg.rpm: $(head -1 <<<"$rpm_files")"; fi
for f in "${want_desktop[@]}"; do
  grep -qx "/$f" <<<"$rpm_files" || fail "the .rpm is missing /$f"
done
rpm_meta="$(rpm -qp --queryformat '%{NAME} %{VERSION} %{ARCH}' "$dist/$pkg.rpm" 2>&1)" || fail "rpm -qp could not read $dist/$pkg.rpm"
read -r rpm_name rpm_ver rpm_arc <<<"$rpm_meta"
[[ "$rpm_name" == "$name" ]] || fail "the .rpm is named '$rpm_name', expected '$name'"
# tauri's rpm version is the raw version with any prerelease rendered RPM-legal ("-" becomes "~" and
# dots, per rpm.rs's to_rpm_version); for a plain X.Y.Z release the two strings are identical, and a
# future rc build changes the rpm copy only.
rpm_ver_want="${version//-/$tilde}"
[[ "$rpm_ver" == "$rpm_ver_want" ]] || fail "the .rpm declares version '$rpm_ver', expected '$rpm_ver_want'"
[[ "$rpm_arc" == "$arch" ]] || fail "the .rpm declares architecture '$rpm_arc', expected '$arch'"
if ! rpm_scripts="$(rpm -qp --scripts "$dist/$pkg.rpm" 2>&1)"; then fail "rpm -qp --scripts could not read $dist/$pkg.rpm: $(head -1 <<<"$rpm_scripts")"; fi
[[ -z "${rpm_scripts//[[:space:]]/}" ]] || fail "the .rpm carries scriptlets, and no package installs or starts a service: $(head -1 <<<"$rpm_scripts")"
if ! rpm_conflicts="$(rpm -qp --conflicts "$dist/$pkg.rpm" 2>&1 | tr -d '[:space:]')"; then fail "rpm -qp --conflicts failed"; fi
if ! rpm_obsoletes="$(rpm -qp --obsoletes "$dist/$pkg.rpm" 2>&1 | tr -d '[:space:]')"; then fail "rpm -qp --obsoletes failed"; fi
[[ -z "$rpm_conflicts" ]] || fail "the .rpm carries Conflicts ($(tr '\n' ' ' <<<"$rpm_conflicts")), which it must not: it coexists with the CLI packages"
[[ -z "$rpm_obsoletes" ]] || fail "the .rpm carries Obsoletes ($(tr '\n' ' ' <<<"$rpm_obsoletes")), which would remove the CLI package when this one installs"
# The Requires line must cover every declared package and every library the loader actually needs;
# rpm auto-discovers the library sonames from the payload, and this is the same allow list
# dev/check-linux-binary.sh --desktop enforces on the binary, so a link that slipped in without a
# depends entry fails the build here too. (Keep the two lists in step.)
if ! rpm_reqs="$(rpm -qpR "$dist/$pkg.rpm" 2>&1)"; then fail "rpm -qpR could not read $dist/$pkg.rpm: $(head -1 <<<"$rpm_reqs")"; fi
conf_rpm="$(config_depends rpm)"
# A requirement is covered when it is one of the declared packages (that is what declaring means),
# when it is metadata rpm always adds (rpmlib capabilities, the interpreter of a script it does not
# have: there are none), or when it is a library soname on the same allow list the desktop branch of
# dev/check-linux-binary.sh enforces on the binary. tauri-cli injects libwebkit2gtk-4.1.so.0 and
# libgtk-3.so.0 requires itself (rust.rs), and rpm's own file deps add the rest - a soname that
# slipped in without a depends entry fails here too. (Keep the soname set in step with
# check-linux-binary.sh's desktop allow list.)
while IFS= read -r req; do
  [[ -z "$req" ]] && continue
  grep -qxF "$req" <<<"$conf_rpm" && continue
  grep -qE '^(/usr/bin/env|/bin/sh|rpmlib|rtld)' <<<"$req" && continue
  grep -qE '^(libc\.so|libm\.so|libgcc_s\.so|ld-linux|libgtk-3\.so|libgdk-3\.so|libgdk_pixbuf-2\.0\.so|libgobject-2\.0\.so|libglib-2\.0\.so|libgio-2\.0\.so|libpango-1\.0\.so|libpangocairo-1\.0\.so|libcairo\.so|libcairo-gobject\.so|libdbus-1\.so|libwebkit2gtk-4\.1\.so|libjavascriptcoregtk-4\.1\.so|libsoup-3\.0\.so)' <<<"$req" \
    || fail "the .rpm requires '$req', which the declared dependencies do not cover: add its package to $conf's bundle.linux.rpm.depends and, if the binary really gained the library, to dev/check-linux-binary.sh's desktop allow list too"
done <<<"$rpm_reqs"
while IFS= read -r dep; do
  [[ -z "$dep" ]] && continue
  grep -qx "$dep" <<<"$rpm_reqs" || fail "$conf declares rpm depends '$dep' but the built .rpm does not require it"
done <<<"$conf_rpm"
echo "ok: the .rpm is $rpm_name $rpm_ver $rpm_arc with no scriptlets, conflicts or obsoletes, and requires cover the allow list"

# --- the .tar.gz, packed by this script (tauri's own Linux tar bundle is a raw binary with no desktop
# integration). The tree is a system root: unpack it with tar -C / -xzf and everything lands where
# the .deb would have put it, under /usr/local instead of /usr.
top="$stage/tar"
rm -rf "$top"
install -Dm755 "$bin" "$top/usr/local/bin/$name"
install -Dm644 "crates/featherbittorrent-desktop/icons/128x128.png" "$top/usr/local/share/icons/hicolor/128x128/apps/$name.png"
install -Dm644 "crates/featherbittorrent-desktop/icons/128x128@2x.png" "$top/usr/local/share/icons/hicolor/256x256@2/apps/$name.png"
install -Dm644 LICENSE "$top/usr/local/share/doc/$name/LICENSE"
install -Dm644 NOTICE "$top/usr/local/share/doc/$name/NOTICE"

# The desktop file the tarball ships: same shape tauri's template generates (Exec and Icon name the
# binary and the hicolor icon by name), with the MimeType= line spelled out, trailing semicolon
# included, because the whole point of the file is the two associations the packages declare.
# mkdir first: install -D creates the directories for its own target, cat creates only the file.
mkdir -p "$top/usr/local/share/applications"
cat > "$top/usr/local/share/applications/$name.desktop" <<EOF
[Desktop Entry]
Categories=Network;FileTransfer;
Comment=featherbittorrent desktop app: the web UI in a native window, with This computer's engine behind it
Exec=/usr/local/bin/$name \%u
Icon=$name
Name=featherbittorrent desktop
StartupWMClass=$name
Terminal=false
Type=Application
MimeType=x-scheme-handler/magnet;application/x-bittorrent;
EOF

{
  echo "$name $version (Linux $arch tarball)"
  echo "Version: $version"
  echo
  echo "Install onto a system root:"
  echo "  tar -C / -xzf $(basename "$dist")/${pkg}.tar.gz"
  echo "Then /usr/local/bin/$name is on PATH and the launcher at"
  echo "/usr/local/share/applications/$name.desktop registers magnet: and .torrent links."
  echo
  echo "This tarball carries no dependency metadata, so the runtime libraries the binary links must"
  echo "already be on the host (the .deb and .rpm in this release declare them to dpkg and dnf):"
  echo
  echo "  Ubuntu 22.04 and Debian 12 (apt-get install -y):"
  echo "    libgtk-3-0 libgdk-pixbuf-2.0-0 libglib2.0-0 libpango-1.0-0 libcairo2 libcairo-gobject2"
  echo "    libdbus-1-3 libwebkit2gtk-4.1-0 libjavascriptcoregtk-4.1-0 libsoup-3.0-0"
  echo "    (libc6 and libgcc-s1 cover the rest, and every system has them)"
  echo
  echo "  Fedora (dnf install -y):"
  echo "    gtk3 glib2 gdk-pixbuf2 pango cairo dbus-libs webkit2gtk4.1 libsoup3"
  echo "    (glibc and libgcc cover the rest)"
  echo
  echo "Settings live in ~/.config/$name and outlive every install and uninstall; the app writes"
  echo "config.yaml there on first start and reads it back after."
} > "$top/usr/local/share/doc/$name/README-desktop.txt"

tar -C "$top" --owner=0 --group=0 --numeric-owner --sort=name -czf "$dist/$pkg.tar.gz" usr
if ! tar_files="$(tar -tzf "$dist/$pkg.tar.gz" 2>&1)"; then fail "tar could not list $dist/$pkg.tar.gz: $(head -1 <<<"$tar_files")"; fi
want_tar=(
  usr/local/bin/"$name"
  usr/local/share/applications/"$name".desktop
  usr/local/share/icons/hicolor/128x128/apps/"$name".png
  usr/local/share/icons/hicolor/256x256@2/apps/"$name".png
  usr/local/share/doc/"$name"/README-desktop.txt
  usr/local/share/doc/"$name"/LICENSE
  usr/local/share/doc/"$name"/NOTICE
)
for f in "${want_tar[@]}"; do
  grep -qx "$f" <<<"$tar_files" || fail "the tarball is missing $f"
done
tar -xOf "$dist/$pkg.tar.gz" "usr/local/share/applications/$name.desktop" \
  | sed 's/^MimeType=//' | tr ';' '\n' | grep -qx 'x-scheme-handler/magnet' || fail "the tarball's desktop file does not declare magnet"
tar -xOf "$dist/$pkg.tar.gz" "usr/local/share/applications/$name.desktop" \
  | sed 's/^MimeType=//' | tr ';' '\n' | grep -qx 'application/x-bittorrent' || fail "the tarball's desktop file does not declare application/x-bittorrent"

# --- no package installs or starts a service (the same rule dev/package-linux.sh enforces for the CLI
# packages: nothing a unit manager or a login session would pick up, no maintainer scripts, no
# scriptlets; the desktop app starts when a person clicks it and from nothing else).
check_no_service() { # <what the list is of> <one path per line>
  local what="$1" files="$2" hit
  hit="$(grep -nE '(^|/)etc/|(^|/)lib/systemd/' <<<"$files" | head -1)" || true
  [[ -z "$hit" ]] || fail "$what ships a path in a unit or init directory, and no package may ship one: $hit"
}
check_no_service "the .deb" "$deb_files"
check_no_service "the .rpm" "$rpm_files"
check_no_service "the tarball" "$tar_files"

# --- every binary every package installs, checked as the loader will load it: the copy inside each
# artifact is extracted and run through dev/check-linux-binary.sh --desktop, so a bundler that
# stripped, swapped or corrupted the binary while packaging is caught with the artifact open rather
# than three distributions later. One edit is intentional: tauri patches three bytes of the binary,
# the constant "TAURI_BUNDLE_TYPE_VAR_XXX" - UNK out of the box, DEB in the .deb, RPM in the .rpm -
# so the identity check requires that tag to name the format it lives in and then compares the rest
# of the file against the release binary byte for byte.
release_tag="$(LC_ALL=C grep -ao 'TAURI_BUNDLE_TYPE_VAR_[A-Z][A-Z][A-Z]' "$bin" | head -1)" || true
for kind in deb rpm tar.gz; do
  probe="$stage/probe-$kind"
  rm -rf "$probe"
  mkdir -p "$probe"
  case "$kind" in
    deb) tar ${data_opt[@]+"${data_opt[@]}"} -C "$probe" -xf "$stage/$data" "usr/bin/$name" ;;
    rpm)
      # Ubuntu's rpm2cpio exits 1 after a complete dump: its compression fallback chain probes with
      # pipelines that fail on purpose. Dump to a file, prove the payload lists the binary, extract;
      # the byte-for-byte cmp below is what actually vouches for the copy.
      rpm2cpio "$dist/$pkg.rpm" > "$stage/payload.cpio" || true
      cpio -it --quiet < "$stage/payload.cpio" | grep -qx "./usr/bin/$name" \
        || fail "the .rpm payload does not contain ./usr/bin/$name"
      ( cd "$probe" && cpio -idm --quiet < "$stage/payload.cpio" ) ;;
    tar.gz) tar -C "$probe" -xzf "$dist/$pkg.tar.gz" "usr/local/bin/$name" ;;
  esac
  probe_bin="$probe/usr/bin/$name"
  [[ "$kind" == tar.gz ]] && probe_bin="$probe/usr/local/bin/$name"
  [[ -f "$probe_bin" ]] || fail "could not extract the installed binary from the $kind to check it"
  chmod 755 "$probe_bin"
  bash dev/check-linux-binary.sh --desktop "$probe_bin"
  case "$kind" in
    deb) want_tag='TAURI_BUNDLE_TYPE_VAR_DEB' ;;
    rpm) want_tag='TAURI_BUNDLE_TYPE_VAR_RPM' ;;
    tar.gz) want_tag='TAURI_BUNDLE_TYPE_VAR_UNK' ;;
  esac
  tag_line="$(LC_ALL=C grep -abo 'TAURI_BUNDLE_TYPE_VAR_[A-Z][A-Z][A-Z]' "$probe_bin" | head -1)" || true
  probe_tag="$(LC_ALL=C grep -ao 'TAURI_BUNDLE_TYPE_VAR_[A-Z][A-Z][A-Z]' "$probe_bin" | head -1)" || true
  [[ -n "$tag_line" && -n "$release_tag" ]] || fail "no TAURI_BUNDLE_TYPE_VAR tag in the extracted binary: tauri stopped patching, or patched somewhere else; update this check rather than trusting it"
  [[ "$probe_tag" == "$want_tag" ]] || fail "the binary inside the $kind declares bundle type '${probe_tag#*VAR_}', expected the tag for $kind"
  tag_at=$(( ${tag_line%%:*} + 22 ))
  printf '%s' "${release_tag: -3}" | dd of="$probe_bin" bs=1 seek="$tag_at" conv=notrunc status=none
  cmp -s "$bin" "$probe_bin" || fail "the binary inside the $kind differs from the one cargo built in a byte that is not the bundle-type tag"
done
echo "ok: every artifact carries the checked binary, identical to the release build except tauri's per-format bundle-type tag"

rm -rf "$stage"

(cd "$dist" && for f in "$pkg.deb" "$pkg.rpm" "$pkg.tar.gz"; do sha256sum "$f" > "$f.sha256"; done)
for f in "$pkg.deb" "$pkg.rpm" "$pkg.tar.gz"; do
  echo "$dist/$f"
  cat "$dist/$f.sha256"
done
