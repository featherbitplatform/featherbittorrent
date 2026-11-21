#!/usr/bin/env bash
# Builds featherbittorrent for Linux and packages it as .deb, .rpm and .tar.gz. Runs on Linux, natively
# for the given architecture (no cross-compiling). Release builds run in an almalinux:9 container: its
# glibc (2.34, RHEL 9's) is the oldest of the supported systems, so the binary runs on all of them, and
# dev/check-linux-binary.sh refuses one that asks for more. The man pages and completions come from the
# binary itself (`generate-assets`), gzipped here; the service examples ship as documentation only. No
# package installs or starts a service: no maintainer scripts and nothing a service manager would pick up,
# which check_no_service below enforces at build time. Needs cargo-deb and cargo-generate-rpm (the
# versions pinned in .github/workflows/release.yml).
# The variant is the build behind the packages: full (default) turns on the web-ui cargo feature and
# embeds the built web UI, and its package names are featherbittorrent; headless is the build without
# it, named featherbittorrent-headless. Both install /usr/bin/featherbittorrent and each declares that it
# conflicts with and replaces the other, so the package managers swap one for the other.
# Usage: bash dev/package-linux.sh <version> <x86_64|aarch64> [full|headless]
#   e.g. bash dev/package-linux.sh 0.1.0 aarch64 full
# Output: full, target/dist/featherbittorrent-<version>-linux-<arch>.{deb,rpm} and
#         target/dist/featherbittorrent-<version>-<arch>-unknown-linux-gnu.tar.gz, each with a .sha256;
#         headless, the same with -headless after featherbittorrent.
# Test hooks that need no build and ask nothing about the platform (dev/test-packaging.sh runs them on
# any OS): --print-versions <version> prints the deb version then the rpm version; --check-rpm-reqs
# reads requirement lines on stdin and refuses any the declared dependencies do not cover. Both refuse
# with one line like everything else here.
set -euo pipefail

fail() { echo "package-linux.sh: $*" >&2; exit 2; }

# Debian and RPM sort "~" before anything, so a pre-release precedes its release, and RPM versions may
# not contain "-", so both mappings replace the dash with a tilde. A variable holds the tilde because on
# bash 3.2, which these scripts support, "${version//-/\~}" keeps the backslash inside double quotes and
# "${version//-/'~'}" keeps the quotes; "$tilde" is a plain tilde on every bash.
tilde='~'

print_versions() { # <version>; prints the deb version, then the rpm version, one per line
  local v="$1"
  printf '%s\n%s\n' "${v//-/$tilde}" "${v//-/$tilde}"
}

check_version() { # <version>; refuses anything that is not X.Y.Z or X.Y.Z(-pre)
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] \
    || fail "invalid version '$1' (expected X.Y.Z or X.Y.Z-pre)"
}

check_rpm_reqs() { # rpm requirement lines on stdin; refuses any the declared dependencies do not cover
  local req
  while IFS= read -r req; do
    [[ -z "$req" ]] && continue
    grep -qE '^(libc\.so\.6|libm\.so\.6|libgcc_s\.so\.1|libpthread\.so\.0|libdl\.so\.2|librt\.so\.1|ld-linux-(x86-64|aarch64)\.so\.[0-9]|rtld|rpmlib|/bin/sh)' <<<"$req" \
      || fail "the .rpm requires '$req', which the declared dependencies do not cover: add it to [package.metadata.deb] depends and to dev/check-linux-binary.sh's allow list together"
  done
}

# The hooks dispatch here, ahead of every platform and tool question. --print-versions checks the
# version exactly as package mode does, so a bad one is refused the same way.
if [[ "${1:-}" == "--print-versions" ]]; then
  [[ $# -eq 2 ]] || fail "usage: bash dev/package-linux.sh --print-versions <version>"
  check_version "$2"
  print_versions "$2"
  exit 0
fi
if [[ "${1:-}" == "--check-rpm-reqs" ]]; then
  [[ $# -eq 1 ]] || fail "usage: bash dev/package-linux.sh --check-rpm-reqs (requirement lines on stdin)"
  check_rpm_reqs
  echo 'package-linux.sh ok: every rpm requirement is covered by the declared dependencies'
  exit 0
fi

# Arguments first, before any platform or tool question, so dev/test-packaging.sh can exercise the
# refusals on any OS.
[[ $# -eq 2 || $# -eq 3 ]] || fail "usage: bash dev/package-linux.sh <version> <x86_64|aarch64> [full|headless]"
version="$1"
arch="$2"
variant="${3:-full}"
check_version "$version"
case "$arch" in
  x86_64|aarch64) ;;
  *) fail "invalid arch '$arch' (expected x86_64 or aarch64)" ;;
esac
case "$variant" in
  full|headless) ;;
  *) fail "invalid variant '$variant' (expected full or headless)" ;;
esac

[[ "$(uname -s)" == "Linux" ]] || fail "package-linux.sh runs on Linux only"
[[ "$(uname -m)" == "$arch" ]] || fail "this machine is $(uname -m), not $arch (no cross-compiling)"
for tool in cargo-deb cargo-generate-rpm objdump ar rpm gzip sha256sum; do
  command -v "$tool" >/dev/null || fail "package-linux.sh needs $tool"
done

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
target="${CARGO_TARGET_DIR:-$root/target}"
dist="$root/target/dist"
# full is the release name and the default; headless carries -headless in every artifact name.
if [[ "$variant" == headless ]]; then prefix="featherbittorrent-headless"; else prefix="featherbittorrent"; fi
pkg="${prefix}-${version}-linux-${arch}"
triple="${arch}-unknown-linux-gnu"
targz="${prefix}-${version}-${triple}"
stage="$dist/stage-linux-$arch"

# The same mapping the --print-versions hook prints (see print_versions for why a variable holds "~").
{ read -r deb_version; read -r rpm_version; } < <(print_versions "$version")

# The feature decides which build the packages hold; the variant must match what it prints, whole
# string, so a headless package built from the wrong tree is refused here rather than published.
build_features=()
if [[ "$variant" == full ]]; then build_features=(--features web-ui); fi
cargo build --locked --release -p featherbittorrent "${build_features[@]}"
bin="$target/release/featherbittorrent"
if ! printed="$("$bin" --version)"; then fail "$bin --version failed to run"; fi
# The binary's command name is featherbittorrent in both builds (it is the package's binary, which the
# package manager installs as /usr/bin/featherbittorrent whatever the package is called); what marks
# the build is the trailing (full) or (headless). The whole string is compared, so a package built
# from the wrong tree is refused here rather than published.
[[ "$printed" == "featherbittorrent $version ($variant)" ]] || fail "--version printed '$printed', expected 'featherbittorrent $version ($variant)'"
bash dev/check-linux-binary.sh "$bin"

# The man pages and completions are generated by the release binary itself, from the same clap Command
# as --help, so they cannot drift from the flags. Debian policy wants man pages compressed, so gzip
# runs here rather than inside cargo-deb, and the tarball ships the same compressed pages.
rm -rf "$target/release/pkg"
"$bin" generate-assets "$target/release/pkg" >/dev/null || fail "generate-assets failed for $bin"
pages=("$target"/release/pkg/man/*.1)
[[ -e "${pages[0]}" ]] || fail "generate-assets wrote no man pages under $target/release/pkg/man"
for page in "${pages[@]}"; do
  gzip -9n "$page"
done

mkdir -p "$dist"
rm -rf "$stage" \
  "$dist/$pkg.deb" "$dist/$pkg.rpm" "$dist/$targz.tar.gz" \
  "$dist/$pkg.deb.sha256" "$dist/$pkg.rpm.sha256" "$dist/$targz.tar.gz.sha256"

# The packages ship the binary exactly as cargo built it (--no-strip), the same file in all three.
# The headless name and the conflicts with and replace of the full package (and the other way
# around) live in the [package.metadata.deb.variants.headless] and
# [package.metadata.generate-rpm.variants.headless] sections of crates/featherbittorrent/Cargo.toml;
# --variant selects them. The full variant is the base metadata, so the full build passes no
# --variant and gets the conflicts from the base sections that both tools merge from there.
deb_variant_args=()
rpm_variant_args=()
if [[ "$variant" == headless ]]; then
  deb_variant_args=(--variant headless)
  rpm_variant_args=(--variant headless)
fi
cargo deb -p featherbittorrent --no-build --no-strip --deb-version "$deb_version" \
  "${deb_variant_args[@]}" --output "$dist/$pkg.deb"
cargo generate-rpm -p crates/featherbittorrent --target-dir "$target" \
  --set-metadata "version = \"$rpm_version\"" "${rpm_variant_args[@]}" --output "$dist/$pkg.rpm"

# Tarball: the binary at the top of one folder (the 0.1.0 layout), with the licenses, the docs and the
# examples that are documentation only. The flags make the archive reproducible for a given build.
top="$stage/$targz"
install -Dm755 "$bin" "$top/featherbittorrent"
install -Dm644 LICENSE "$top/LICENSE"
install -Dm644 NOTICE "$top/NOTICE"
install -Dm644 README.md "$top/README.md"
cp -R "$target/release/pkg/man" "$top/man"
cp -R "$target/release/pkg/completions" "$top/completions"
install -Dm644 docs/autostart.md "$top/docs/autostart.md"
install -Dm644 docs/server.md "$top/docs/server.md"
install -Dm644 packaging/systemd/system/featherbittorrent.service "$top/examples/systemd/system/featherbittorrent.service"
install -Dm644 packaging/systemd/user/featherbittorrent.service "$top/examples/systemd/user/featherbittorrent.service"
tar -C "$stage" --owner=0 --group=0 --numeric-owner --sort=name -czf "$dist/$targz.tar.gz" "$targz"

# Verify contents. No dpkg on the build image: read the .deb's data member with ar and tar. mkdir
# first, because the members land in $stage before anything else creates it.
mkdir -p "$stage"
# The man pages, completions and examples keep the product's own name: generate-assets writes them
# from the binary's command name, which is featherbittorrent in both variants. Only the doc folder
# follows the package, so the headless package does not ship into the full package's documentation.
want_common=(
  usr/bin/featherbittorrent
  usr/share/man/man1/featherbittorrent.1.gz
  usr/share/man/man1/featherbittorrent-serve.1.gz
  usr/share/bash-completion/completions/featherbittorrent
  usr/share/fish/vendor_completions.d/featherbittorrent.fish
  "usr/share/doc/$prefix/autostart.md"
  "usr/share/doc/$prefix/examples/systemd/system/featherbittorrent.service"
  "usr/share/doc/$prefix/examples/systemd/user/featherbittorrent.service"
)
# Debian lets zsh own vendor-completions, Fedora's zsh reads site-functions; the metadata says so and
# this list has to agree with it, or a renamed path would install nowhere.
want_deb=("${want_common[@]}" usr/share/zsh/vendor-completions/_featherbittorrent)
if ! members="$(ar t "$dist/$pkg.deb" 2>&1)"; then fail "ar could not read $dist/$pkg.deb: $members"; fi
data="$(grep '^data\.tar' <<<"$members" | head -1)" || true
[[ -n "$data" ]] || fail "the .deb has no data.tar member (ar listed: $(tr '\n' ' ' <<<"$members"))"
control="$(grep '^control\.tar' <<<"$members" | head -1)" || true
[[ -n "$control" ]] || fail "the .deb has no control.tar member (ar listed: $(tr '\n' ' ' <<<"$members"))"
ar p "$dist/$pkg.deb" "$data" > "$stage/$data"
if ! deb_files="$(tar -tf "$stage/$data" 2>&1)"; then fail "tar could not list the .deb data member $data: $(head -1 <<<"$deb_files")"; fi
for f in "${want_deb[@]}"; do
  grep -qx "\./$f" <<<"$deb_files" || fail "the .deb is missing /$f"
done
if ! rpm_files="$(rpm -qlp "$dist/$pkg.rpm" 2>&1)"; then fail "rpm -qlp could not read $dist/$pkg.rpm: $(head -1 <<<"$rpm_files")"; fi
want_rpm=("${want_common[@]}" usr/share/zsh/site-functions/_featherbittorrent)
for f in "${want_rpm[@]}"; do
  grep -qx "/$f" <<<"$rpm_files" || fail "the .rpm is missing /$f"
done
if ! tar_files="$(tar -tzf "$dist/$targz.tar.gz" 2>&1)"; then fail "tar could not list $dist/$targz.tar.gz: $(head -1 <<<"$tar_files")"; fi
want_tar=(
  featherbittorrent LICENSE NOTICE README.md
  man/featherbittorrent.1.gz man/featherbittorrent-serve.1.gz
  completions/featherbittorrent.bash completions/_featherbittorrent completions/featherbittorrent.fish
  docs/autostart.md docs/server.md
  examples/systemd/system/featherbittorrent.service examples/systemd/user/featherbittorrent.service
)
for f in "${want_tar[@]}"; do
  grep -qx "$targz/$f" <<<"$tar_files" || fail "the tarball is missing $f"
done

# No package installs or starts a service. Nothing may sit where a unit or init drop-in would be picked
# up, the .deb control member may hold no maintainer script, and the .rpm may carry no scriptlet. The
# example units live under usr/share/doc/<package>/examples/ instead.
check_no_service() { # <what the list is of> <one path per line>
  local what="$1" files="$2" hit
  hit="$(grep -nE '(^|/)etc/|(^|/)lib/systemd/' <<<"$files" | head -1)" || true
  [[ -z "$hit" ]] || fail "$what ships a path in a unit or init directory, and no package may ship one: $hit"
}
check_no_service "the .deb" "$deb_files"
check_no_service "the .rpm" "$rpm_files"
check_no_service "the tarball" "$tar_files"

ar p "$dist/$pkg.deb" "$control" > "$stage/$control"
if ! control_files="$(tar -tf "$stage/$control" 2>&1)"; then fail "tar could not list the .deb control member $control: $(head -1 <<<"$control_files")"; fi
while IFS= read -r entry; do
  [[ -z "$entry" ]] && continue
  case "${entry#./}" in
    control|md5sums) ;;
    *) fail "the .deb control member holds $entry, which is a maintainer script or its data, and packages run none" ;;
  esac
done <<<"$control_files"
if ! scripts="$(rpm -qp --scripts "$dist/$pkg.rpm" 2>&1)"; then fail "rpm -qp --scripts could not read $dist/$pkg.rpm: $(head -1 <<<"$scripts")"; fi
[[ -z "${scripts//[[:space:]]/}" ]] || fail "the .rpm carries scriptlets, and no package installs or starts a service: $(head -1 <<<"$scripts")"

# The requirements cargo-generate-rpm derived from the binary, printed for the release log and held to
# the same allow list dev/check-linux-binary.sh greps: they come from the binary's own DT_NEEDED, so a
# library that slipped in without anyone updating [package.metadata.deb] depends is caught here too.
if ! reqs="$(rpm -qpR "$dist/$pkg.rpm" 2>&1)"; then fail "rpm -qpR could not read $dist/$pkg.rpm: $(head -1 <<<"$reqs")"; fi
echo "rpm requirements:"
printf '%s\n' "$reqs"
check_rpm_reqs <<<"$reqs"

rm -rf "$stage"

(cd "$dist" && for f in "$pkg.deb" "$pkg.rpm" "$targz.tar.gz"; do sha256sum "$f" > "$f.sha256"; done)
for f in "$pkg.deb" "$pkg.rpm" "$targz.tar.gz"; do
  echo "$dist/$f"
  cat "$dist/$f.sha256"
done
