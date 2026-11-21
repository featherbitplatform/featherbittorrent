#!/usr/bin/env bash
# Smoke-tests one Linux artifact in a clean container, which is what the release workflow's smoke jobs do
# before anything is published: check the artifact's .sha256, install it with no recommended or weak
# dependencies, look at where its files landed, and prove that no package installs or starts a service:
# nothing a service manager would pick up was installed or enabled, and no system account appeared. Then
# run the shared CLI smoke (dev/smoke-cli.sh) against the binary in front of us, ask systemd itself
# whether the example unit is valid on Debian images, then remove the package and assert that every file
# it owns is gone while the config.yaml the smoke wrote is still there, because removal owns nothing of
# the user's. The .tar.gz installs nothing, so it gets the checksum, the extraction, the same no-service
# grep over its file list and the CLI smoke. Ubuntu's and
# Fedora's container images skip documentation files by default (dpkg path-excludes, dnf tsflags=nodocs), so
# this script clears both before installing: a check about a man page or a doc file has to be about the
# package, not about how the image was shaved down for download.
# Usage: bash dev/smoke-linux.sh <deb|rpm|tar.gz> <dist-dir> <version> [full|headless]
# Run as root inside a throwaway container (it installs and removes system packages), e.g.
#   docker run --rm -v "$PWD:/src" -w /src debian:12 bash dev/smoke-linux.sh deb target/dist 0.1.0 full
#   docker run --rm -v "$PWD:/src" -w /src fedora:41 bash dev/smoke-linux.sh rpm target/dist 0.1.0 headless
#   docker run --rm -v "$PWD:/src" -w /src ubuntu:22.04 bash dev/smoke-linux.sh tar.gz target/dist 0.1.0 full
# The variant is the build the artifact was made from: full is the default and holds the web UI,
# headless does not. It changes the package and archive names this script looks for and the package
# it installs and removes, and the CLI smoke checks that the binary's own version string names that
# same build, so an artifact holding the other variant's binary fails before it is published.
# It refuses to run anywhere else: outside a container and outside CI, exactly like ../terminal's twin.
# Output: one line per check on stdout, then `smoke ok: <format> on <PRETTY_NAME> <arch>`. The first thing
# that is wrong is one clear line on stderr and exit 1. Nothing here contacts the internet beyond the
# container's own package manager, and nothing here runs `serve`.
set -euo pipefail

fail() { echo "smoke-linux.sh: $*" >&2; exit 1; }

need() { command -v "$1" >/dev/null || fail "this image has no $1"; }

# Arguments before any container or platform question, so a test can exercise the refusals on any OS (the
# rule every dev/ script here follows). The artifact is named by the machine's own arch, because the matrix
# runs each package on its native architecture.
[[ $# -eq 3 || $# -eq 4 ]] || fail "usage: bash dev/smoke-linux.sh <deb|rpm|tar.gz> <dist-dir> <version> [full|headless]"
format="$1"
dist="$2"
version="$3"
variant="${4:-full}"
case "$format" in
  deb|rpm|tar.gz) ;;
  *) fail "unknown format '$format' (expected deb, rpm or tar.gz)" ;;
esac
case "$variant" in
  full|headless) ;;
  *) fail "unknown variant '$variant' (expected full or headless)" ;;
esac
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] \
  || fail "invalid version '$version' (expected X.Y.Z or X.Y.Z-pre)"
arch="$(uname -m)"
# full is the release name; headless carries -headless in the artifact and package names.
if [[ "$variant" == headless ]]; then prefix="featherbittorrent-headless"; else prefix="featherbittorrent"; fi
case "$format" in
  tar.gz) name="${prefix}-${version}-${arch}-unknown-linux-gnu" ;;
  *)      name="${prefix}-${version}-linux-${arch}" ;;
esac
file="$dist/$name.$format"
[[ -f "$file" ]] || fail "no artifact at $file"
[[ -f "$file.sha256" ]] || fail "no checksum beside $file (expected $file.sha256)"

# The guard ../terminal/dev/smoke-linux.sh uses: this script installs and removes system packages, so it
# belongs in a throwaway container or on a CI runner, never on the machine that built the artifact.
if [[ ! -e /.dockerenv && ! -e /run/.containerenv && -z "${CI:-}" ]]; then
  fail "smoke-linux.sh installs and removes system packages: run it in a container or in CI"
fi

need sha256sum
need realpath
need tar
need getent
case "$format" in
  deb) need apt-get; need dpkg ;;
  rpm) need dnf; need rpm ;;
esac

# What image this is: the Debian ones (the ones Raspberry Pi OS stands in for, no Pi image exists) are the
# only place systemd-analyze verify runs, and PRETTY_NAME names the machine in the ok line at the end.
[[ -r /etc/os-release ]] || fail "/etc/os-release is unreadable, so this image cannot be identified"
# shellcheck source=/dev/null
. /etc/os-release
distro_id="${ID:-}"
pretty_name="${PRETTY_NAME:-unknown}"

# 1. The checksum that ships beside the artifact, read from inside the dist directory: a .sha256 names the
# file rather than a path, which is what lets an artifact move between machines unchanged.
(cd "$dist" && sha256sum -c "$name.$format.sha256") >/dev/null \
  || fail "$(basename "$file") does not match $(basename "$file").sha256"
echo "ok: $(basename "$file") matches its sha256"

# Step 4, first half: the grep dev/package-linux.sh applies to the artifact at build time, now applied to
# what the package manager says it installed. No package installs or starts a service, so no path may land
# in /etc (packages own none) and none in a directory a service manager reads. Paths arrive absolute from
# dpkg -L and rpm -ql, and relative to the extraction from tar; a segment boundary at the start or after any
# slash matches either spelling, and the closing group accepts the end of the line as well as a slash, so a
# bare `/etc` or `/usr/lib/systemd` entry counts too. That last part is one thing stricter than the build
# time grep at the other end of this flow, which needs the trailing slash: a directory entry on its own is
# exactly what a package that shipped a unit or a conffile would leave behind among its listed files, and it
# is the case a person would otherwise have to notice by hand. `lib/systemd` covers `/usr/lib/systemd` as
# well as `/lib/systemd`, because the match starts at a slash.
check_no_service_paths() { # <what the list is of> <one path per line>
  local what="$1" files="$2" hit
  hit="$(grep -nE '(^|/)(etc|lib/systemd)(/|$)' <<<"$files" | head -1)" || true
  [[ -z "$hit" ]] || fail "$what names a path in /etc or in a unit directory, and no package may install one: $hit"
}

# Step 4, second half: the two things a file list cannot show. An enable leaves a link in a *.wants/
# directory, and a package that creates a service user is halfway to installing a service.
check_no_service_state() {
  local wants
  wants="$(ls /etc/systemd/system/*.wants/ 2>/dev/null || true)"
  if grep -qi featherbittorrent <<<"$wants"; then
    fail "something under /etc/systemd/system enabled featherbittorrent, and no package enables one: $(head -1 <<<"$wants")"
  fi
  if getent passwd featherbittorrent >/dev/null 2>&1; then
    fail "a featherbittorrent account exists, and no package may create one"
  fi
  echo 'ok: nothing installed or enabled a service, nothing landed in /etc, no system user was created'
}

# Where each distribution expects the shell's completions, which is the one place the two packages differ:
# Debian's zsh reads vendor-completions, Fedora's reads site-functions. [package.metadata] says the same.
case "$format" in
  deb) zsh_dir=/usr/share/zsh/vendor-completions ;;
  rpm) zsh_dir=/usr/share/zsh/site-functions ;;
  *)   zsh_dir='' ;;
esac

# The file entries of a package's own list, one per line: everything removal has to take away. Directories
# are left out on purpose, because the two formats own them differently: cargo-deb writes them into the .deb
# data tar as objects dpkg tracks and removes, while cargo-generate-rpm names no %dir, so dnf removes every
# file it owns and leaves the empty directories it created for them behind. That is RPM's ordinary behaviour
# for any package that does not list a directory, not a defect in ours, and step 7 says which of the two it
# is looking at. The man pages and completions keep the product's own name in both variants; the doc folder
# follows the package, so the headless package's files are asked for under its own name.
installed_file_list() { # <one path per line> -> the same paths, without the directories
  local p
  while IFS= read -r p; do
    if [[ -n "$p" && ! -d "$p" ]]; then printf '%s\n' "$p"; fi
  done <<<"$1"
}

# Step 3: what a package must have put where, checked twice on purpose: the path has to be in the package's
# own file list (so the package owns it), and it has to exist now (so the install really placed it). The man
# page is asked for as a file rather than through `man -w`, because minimal images carry no man-db.
check_installed() { # <what installed it> <one path per line, absolute>
  local what="$1" files="$2" bin_path p
  bin_path="$(command -v featherbittorrent || true)"
  [[ "$bin_path" == /usr/bin/featherbittorrent ]] \
    || fail "featherbittorrent is '$bin_path' after the $what install, expected /usr/bin/featherbittorrent"
  for p in "/usr/share/man/man1/featherbittorrent.1.gz" \
           /usr/share/bash-completion/completions/featherbittorrent \
           "$zsh_dir/_featherbittorrent" \
           /usr/share/fish/vendor_completions.d/featherbittorrent.fish \
           "/usr/share/doc/$prefix/examples/systemd/system/featherbittorrent.service"; do
    grep -qFx -- "$p" <<<"$files" || fail "the $what does not own $p"
    [[ -e "$p" ]] || fail "$p is not there after the $what install"
  done
  echo "ok: the $what installed the binary, the man page, the bash, zsh and fish completions and the example unit"
}

# The scratch HOME the CLI smoke writes into, and the config file it reports back. /root is this container's
# own, never a person's home, and the smoke refuses a home whose config already exists.
scratch_home=/root/smoke-home

work=''
cleanup() { if [[ -n "$work" ]]; then rm -rf "$work"; fi; }
trap cleanup EXIT

files=''
installed_files=''
cli_bin=''
case "$format" in
  deb)
    # 2. A minimal install: no recommends, so a library or a helper the package does not declare cannot
    # arrive and make the artifact look healthier than it is. DEBIAN_FRONTEND keeps apt from asking.
    export DEBIAN_FRONTEND=noninteractive
    # Ubuntu's container images ship a dpkg config that throws away every man page, doc file and
    # translation to keep the image small. dpkg still lists them, so a check that asks for one would fail on
    # the image's download policy instead of on the package. A real Ubuntu system keeps them, so the excludes
    # come out first. Matched by content because the file name has changed between image versions; Debian's
    # images carry no such file, and nothing else in these containers does either.
    for cfg in /etc/dpkg/dpkg.cfg.d/*; do
      if [[ -f "$cfg" ]] && grep -q '^path-exclude=' "$cfg"; then rm -f "$cfg"; fi
    done
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends "$(realpath "$file")"
    files="$(dpkg -L "$prefix")"
    check_installed '.deb' "$files"
    check_no_service_paths "dpkg -L $prefix" "$files"
    installed_files="$(installed_file_list "$files")"
    cli_bin=/usr/bin/featherbittorrent
    ;;
  rpm)
    # install_weak_deps=False is the RPM reading of "no recommends". tsflags= clears the nodocs flag the
    # Fedora and Rocky container images set in /etc/dnf/dnf.conf, which would skip every %doc file: the man
    # pages, the docs and the example units are what this smoke checks, and a real system installs them.
    dnf install -y -q --setopt=install_weak_deps=False --setopt=tsflags= "$(realpath "$file")"
    files="$(rpm -ql "$prefix")"
    check_installed '.rpm' "$files"
    check_no_service_paths "rpm -ql $prefix" "$files"
    installed_files="$(installed_file_list "$files")"
    cli_bin=/usr/bin/featherbittorrent
    ;;
  tar.gz)
    # The archive installs nothing anywhere: it is unpacked into a scratch directory and run from there, so
    # the only thing to prove about it here is that its own tree holds no unit or /etc path.
    work="$(mktemp -d)"
    tar -C "$work" -xzf "$file"
    cli_bin="$work/$name/featherbittorrent"
    [[ -x "$cli_bin" ]] || fail "the tarball has no executable $name/featherbittorrent at its top"
    check_no_service_paths 'the tarball listing' "$(tar -tzf "$file")"
    echo 'ok: the tarball unpacked with the binary at its top and no unit or /etc path in it'
    ;;
esac
check_no_service_state

# 5. The shared CLI smoke, run against whatever this format put in front of us: --version checked
# exactly, whole string, against this run's version and build marker (` (full)` or ` (headless)` on the
# product's own name in front of it), the help texts, serve --offline refused with exit 2 (the
# release-build proof) and a login written and read back through the platform's own paths. Its last two
# lines are the version and the config file it created; the config path is kept for step 7. Output goes
# to a file first so a failure still shows every line it printed.
rm -rf "$scratch_home"
mkdir -p "$scratch_home"
cli_log="$(mktemp)" || fail "could not make a temporary file for dev/smoke-cli.sh's output"
status=0
bash "$(dirname "$0")/smoke-cli.sh" "$cli_bin" "$version" "$scratch_home" "$variant" >"$cli_log" 2>&1 || status=$?
cat "$cli_log"
[[ $status -eq 0 ]] || fail "dev/smoke-cli.sh exited $status against $cli_bin (everything it printed is above)"
config_path="$(sed -n 's/^config: //p' "$cli_log" | tail -1)"
[[ -n "$config_path" ]] || fail "dev/smoke-cli.sh printed no 'config: <path>' line to follow"
[[ -f "$config_path" ]] || fail "the config dev/smoke-cli.sh reported is not a file: $config_path"
rm -f "$cli_log"

# 6. On the Debian images, systemd itself has to accept the example unit, because that is the file a person
# copies into /etc/systemd/system to run serve at boot; no package does that by itself. man-db comes with
# systemd rather than as a Recommends:
# verify resolves the unit's Documentation=man: entry by running man, and without it the check would fail on
# an unindexed page instead of on anything about the unit (task-2-report.md concern 1). The account the unit
# names is created only for this check and removed right after it; the package created none, as step 4 saw.
if [[ "$distro_id" == debian && "$format" != tar.gz ]]; then
  apt-get install -y -qq --no-install-recommends systemd man-db
  useradd --system featherbittorrent
  unit=/usr/share/doc/featherbittorrent/examples/systemd/system/featherbittorrent.service
  status=0
  verify_out="$(systemd-analyze verify "$unit" 2>&1)" || status=$?
  userdel featherbittorrent
  [[ $status -eq 0 ]] || fail "systemd-analyze verify $unit exited $status: $(head -1 <<<"$verify_out")"
  [[ -z "${verify_out//[[:space:]]/}" ]] \
    || fail "systemd-analyze verify $unit printed something, and the example unit has to pass as shipped: $(head -1 <<<"$verify_out")"
  echo 'ok: systemd-analyze verify accepts the example unit and says nothing'
fi

# 7. Removal. purge rather than remove for the .deb, because it is the stronger case: it also drops
# conffiles, so if the package had dared to put a file in /etc this would show it. dnf has one form. What
# must survive is the user's own config under the scratch home, which the package never owned; what must
# be gone is every file the package did own, the list captured at step 3 rather than a handful of paths
# anyone remembered.
case "$format" in
  deb) apt-get purge -y -qq "$prefix" ;;
  rpm) dnf remove -y -q "$prefix" ;;
esac
if [[ "$format" != tar.gz ]]; then
  left=''
  while IFS= read -r p; do
    if [[ -n "$p" ]] && [[ -e "$p" ]]; then left="$left $p"; fi
  done <<<"$installed_files"
  [[ -z "$left" ]] || fail "removal left files it owns behind:$left"
  if [[ "$format" == deb && -e "/usr/share/doc/$prefix" ]]; then
    fail "purge left /usr/share/doc/$prefix behind, and the .deb owns that directory"
  fi
  [[ -f "$config_path" ]] \
    || fail "removal took away the user's config at $config_path, which the package never owned"
  if ! grep -qF 'smoke' "$config_path"; then
    fail "$config_path no longer names the smoke user, so removal rewrote what it never owned: $(head -1 "$config_path")"
  fi
  echo "ok: $format removed, every file it owned is gone (including /usr/bin/featherbittorrent), $config_path is still there with the smoke user in it"
  if [[ "$format" == rpm ]] && command -v find >/dev/null; then
    # The directories stay, and only for the .rpm: cargo-generate-rpm lists files and no %dir, so dnf has
    # nothing to remove them by. Empty, unnamed by any package, harmless to a reinstall. Recorded rather than
    # asserted away, because asking RPM to delete what it never claimed would be asking the wrong thing of it.
    # Each directory is asked about only if it is there now: find exits 1 for a path that is not there, and
    # with pipefail that status lands on the assignment, where set -e ends the script without printing
    # anything and without the smoke ok line at the end. Today both exist; the day the .rpm owns its
    # directories (the change this note argues for) or stops shipping the licenses tree, one of them is
    # gone, so the list comes from what is present rather than from two remembered names. A find that fails
    # on a directory that *is* there still says so, because that one is worth stopping for.
    empty_dirs=''
    for d in "/usr/share/doc/$prefix" /usr/share/licenses/featherbittorrent; do
      if [[ -d "$d" ]]; then
        status=0
        listed="$(find "$d" -type d | sort)" || status=$?
        [[ $status -eq 0 ]] || fail "find could not list $d after the removal, exit $status"
        empty_dirs="$empty_dirs$(tr '\n' ' ' <<<"$listed")"
      fi
    done
    if [[ -n "${empty_dirs//[[:space:]]/}" ]]; then
      echo "note: dnf left the empty directories it created but does not own: $empty_dirs"
    fi
  fi
fi

# The line the release workflow greps for, naming the format, the image and the architecture it ran on.
echo "smoke ok: $format on $pretty_name $arch"
