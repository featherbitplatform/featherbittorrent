#!/usr/bin/env bash
# Tests the linux packaging decisions without building anything, on any OS, the way the release
# workflow's verify job runs it: dev/check-linux-binary.sh accepts excerpts of a real almalinux:9
# release build (the -ok fixtures), refuses a glibc symbol newer than its 2.34 floor or a NEEDED
# library off the allow list, each refusal one line naming its culprit, and is run at the boundary
# (GLIBC_2.4 and 2.34 accepted, 2.35 and 2.34.1 refused) so the version compare itself is tested, not
# just the two fixtures; dev/package-linux.sh maps X.Y.Z-pre to the tilde form deb and rpm need
# (--print-versions), holds the requirements rpm derives from the real binary to the allow list
# (--check-rpm-reqs), and refuses missing arguments, a version that is not X.Y.Z(-pre), and an arch
# other than x86_64 or aarch64, and a variant other than full or headless, all before it asks anything
# about the platform, which is why these tests run on macOS too; and dev/test-service-examples.sh passes,
# because the packages ship those examples as documentation. The fixtures are excerpts of objdump -T and objdump -p output for the
# binary dev/package-linux.sh builds in almalinux:9 (the failing copies are hand-edited from them), so
# they name no machine's paths. Nothing here contacts the network, builds anything, or installs
# anything.
# Usage: bash dev/test-packaging.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root" || exit 1

fail() { echo "FAIL: $*" >&2; exit 1; }

check_sh=dev/check-linux-binary.sh
package_sh=dev/package-linux.sh
fixtures=dev/fixtures

for file in "$check_sh" "$package_sh"; do
  [[ -f "$file" ]] || fail "missing $file"
done
for fixture in objdump-T-ok.txt objdump-T-new-glibc.txt objdump-p-ok.txt objdump-p-extra-lib.txt; do
  [[ -f "$fixtures/$fixture" ]] || fail "missing fixture $fixtures/$fixture"
done
echo 'ok: both scripts and all four fixtures are files'

# CHECK_OBJDUMP_T and CHECK_OBJDUMP_P replace the objdump output dev/check-linux-binary.sh reads, so
# these runs need neither objdump nor Linux. The binary argument is still required; in fixture mode it
# is never read, so the check script itself stands in for one. check_out holds the captured output for
# the assertions below.
check_out=''
run_check() { # <objdump -T fixture> <objdump -p fixture>; returns dev/check-linux-binary.sh's status
  local t="$1" p="$2" status=0 out
  out="$(CHECK_OBJDUMP_T="$fixtures/$t" CHECK_OBJDUMP_P="$fixtures/$p" bash "$check_sh" "$check_sh" 2>&1)" || status=$?
  check_out="$out"
  return "$status"
}

# 1. The real excerpts of the almalinux:9 binary the .deb and the .rpm ship: its newest symbol version
# is GLIBC_2.34, the floor of every supported system, and it asks for nothing off the allow list.
status=0
run_check objdump-T-ok.txt objdump-p-ok.txt || status=$?
[[ $status -eq 0 ]] || fail "check-linux-binary.sh refused its own ok fixtures (exit $status): $check_out"
echo 'ok: check-linux-binary.sh accepts the almalinux:9 excerpts'

# 2. A binary built on newer glibc: GLIBC_2.38 is newer than the 2.34 floor the packages promise, and
# the refusal has to name it, because that line is what a person reads when the release job fails.
status=0
run_check objdump-T-new-glibc.txt objdump-p-ok.txt || status=$?
[[ $status -eq 1 ]] || fail "check-linux-binary.sh exited $status on the new-glibc fixture, expected 1: $check_out"
grep -qF 'GLIBC_2.38' <<<"$check_out" || fail "the refusal did not name GLIBC_2.38: $check_out"
echo 'ok: check-linux-binary.sh refuses a GLIBC_2.38 symbol and names it'

# 3. A new shared library: the NEEDED allow list is what keeps [package.metadata.deb] depends true, so
# the refusal must name the library and say to move the allow list and the depends together.
status=0
run_check objdump-T-ok.txt objdump-p-extra-lib.txt || status=$?
[[ $status -eq 1 ]] || fail "check-linux-binary.sh exited $status on the extra-library fixture, expected 1: $check_out"
grep -qF 'libssl.so.3' <<<"$check_out" || fail "the refusal did not name libssl.so.3: $check_out"
grep -qF 'allow list' <<<"$check_out" || fail "the refusal did not mention the allow list: $check_out"
grep -qF 'depends' <<<"$check_out" || fail "the refusal did not mention the .deb depends: $check_out"
echo 'ok: check-linux-binary.sh refuses NEEDED libssl.so.3 and names it with both places to fix'

# 4. The version mapping deb and rpm need: RPM versions may not contain "-", and both package managers
# sort "~" before everything else, so "0.1.0-rc.1" has to come out as exactly "0.1.0~rc.1", twice. The
# output is compared byte for byte because on bash 3.2, which these scripts support, "${version//-/\~}"
# inside double quotes leaves a backslash behind, and one in a version string would poison both
# package managers.
versions_test() { # <version to map> <what each of the two printed lines must be>
  local v="$1" want_line="$2" status=0 out want
  out="$(bash "$package_sh" --print-versions "$v" 2>&1)" || status=$?
  [[ $status -eq 0 ]] || fail "package-linux.sh --print-versions $v exited $status: $out"
  want="$(printf '%s\n%s' "$want_line" "$want_line")"
  [[ "$out" == "$want" ]] || fail "package-linux.sh --print-versions $v printed '$out', expected two lines of '$want_line'"
  case "$out" in *\\*) fail "the mapping left a backslash in: $out" ;; esac
}
versions_test 0.1.0-rc.1 '0.1.0~rc.1'
versions_test 0.2.0 '0.2.0'
echo 'ok: package-linux.sh maps 0.1.0-rc.1 to 0.1.0~rc.1 for deb and rpm, leaves 0.2.0 alone, prints no backslash'

# 5. The glibc floor as a boundary, not just the two fixtures: "2.4" sorts above "2.34" when dotted
# versions compare as text, so only a numeric sort lets an older symbol pass, and a third number like
# GLIBC_2.34.1 has to lose to 2.34 on its own field. Each run feeds check-linux-binary.sh one versioned
# symbol through CHECK_OBJDUMP_T with the ok -p fixture beside it, so only the compare is under test.
tmp="$(mktemp -d)" || fail "mktemp failed"
trap 'rm -rf "$tmp"' EXIT
glibc_case() { # <symbol version> <exit to expect>; a refusal must name that version
  local ver="$1" want="$2" status=0 out
  printf '%s\n%s\n' 'DYNAMIC SYMBOL TABLE:' \
    "0000000000000000      DF *UND*  0000000000000000  GLIBC_$ver   some_symbol" \
    > "$tmp/objdump-T-$ver.txt"
  out="$(CHECK_OBJDUMP_T="$tmp/objdump-T-$ver.txt" CHECK_OBJDUMP_P="$fixtures/objdump-p-ok.txt" bash "$check_sh" "$check_sh" 2>&1)" || status=$?
  [[ $status -eq "$want" ]] || fail "check-linux-binary.sh exited $status on a lone GLIBC_$ver symbol, expected $want: $out"
  if [[ $want -eq 1 ]]; then
    grep -qF "GLIBC_$ver" <<<"$out" || fail "the refusal did not name GLIBC_$ver: $out"
  fi
}
glibc_case 2.4 0     # older than the floor, and lexicographically above 2.34; a numeric sort must pass it
glibc_case 2.34 0    # exactly the floor
glibc_case 2.35 1    # just above the floor
glibc_case 2.34.1 1  # a third number, newer than 2.34 only on its own field
echo 'ok: check-linux-binary.sh accepts GLIBC_2.4 and GLIBC_2.34, refuses GLIBC_2.35 and GLIBC_2.34.1 by name'

# 6. The requirements rpm derives from the real binary, held to the allow list through --check-rpm-reqs,
# which reads stdin and asks nothing about the platform. The accepted list is verbatim what rpm -qpR
# printed for the almalinux:9 aarch64 build (task-4-report.md); libssl.so.3 is what a stray NEEDED
# would add, and it must be refused by name with both places to fix, as section 3 insists.
reqs_std="$(printf '%s\n' /bin/sh \
  'libc.so.6()(64bit)' 'libc.so.6(GLIBC_2.17)(64bit)' 'libc.so.6(GLIBC_2.18)(64bit)' \
  'libc.so.6(GLIBC_2.25)(64bit)' 'libc.so.6(GLIBC_2.27)(64bit)' 'libc.so.6(GLIBC_2.28)(64bit)' \
  'libc.so.6(GLIBC_2.29)(64bit)' 'libc.so.6(GLIBC_2.30)(64bit)' 'libc.so.6(GLIBC_2.32)(64bit)' \
  'libc.so.6(GLIBC_2.33)(64bit)' 'libc.so.6(GLIBC_2.34)(64bit)' \
  'libgcc_s.so.1()(64bit)' 'libgcc_s.so.1(GCC_3.0)(64bit)' 'libgcc_s.so.1(GCC_3.3)(64bit)' \
  'libgcc_s.so.1(GCC_4.2.0)(64bit)' 'libm.so.6()(64bit)' 'libm.so.6(GLIBC_2.29)(64bit)' \
  'rpmlib(CompressedFileNames) <= 3.0.4-1' 'rpmlib(FileDigests) <= 4.6.0-1' \
  'rpmlib(PayloadFilesHavePrefix) <= 4.0-1' 'rpmlib(PayloadIsZstd) <= 5.4.18-1' 'rtld(GNU_HASH)')"
status=0; reqs_out="$(bash "$package_sh" --check-rpm-reqs <<<"$reqs_std" 2>&1)" || status=$?
[[ $status -eq 0 ]] || fail "package-linux.sh --check-rpm-reqs refused the real build's requirements (exit $status): $reqs_out"
reqs_extra="$(printf '%s\nlibssl.so.3()(64bit)' "$reqs_std")"
status=0; reqs_out="$(bash "$package_sh" --check-rpm-reqs <<<"$reqs_extra" 2>&1)" || status=$?
# The refusal is exit 2, the same code every argument and content refusal in package-linux.sh takes:
# a list the declared dependencies do not cover is one of them.
[[ $status -eq 2 ]] || fail "package-linux.sh --check-rpm-reqs exited $status on a list holding libssl.so.3()(64bit), expected 2: $reqs_out"
grep -qF 'libssl.so.3()(64bit)' <<<"$reqs_out" || fail "the refusal did not name libssl.so.3()(64bit): $reqs_out"
grep -qF 'allow list' <<<"$reqs_out" || fail "the refusal did not mention the allow list: $reqs_out"
grep -qF 'depends' <<<"$reqs_out" || fail "the refusal did not mention the .deb depends: $reqs_out"
echo 'ok: package-linux.sh accepts the requirements of the real build and refuses libssl.so.3()(64bit) by name'

# 7. dev/package-linux.sh validates its arguments before any platform or tool question. None of these
# refusals may mention Linux, because here they run on macOS and still have to mean the same thing.
# 7a. package-linux.sh validates its arguments before any platform or tool question. None of these
# refusals may mention Linux, because here they run on macOS and still have to mean the same thing.
# The variant argument is optional (full is the default), so a version-and-arch with no variant is a
# complete command, and one that names a third variant is refused with exit 2 as the script's own
# usage line says.
refusal() { # <what the message must say> <package-linux.sh argument>...
  local want="$1"; shift
  local status=0 out
  out="$(bash "$package_sh" "$@" 2>&1)" || status=$?
  [[ $status -eq 2 ]] || fail "package-linux.sh $* exited $status, expected 2: $out"
  grep -qF -- "$want" <<<"$out" || fail "package-linux.sh $* did not say '$want': $out"
  # The two refusal lines that live after the argument checks. Seeing either would mean the script
  # asked about the platform before its arguments, which is what dev/test-packaging.sh tests against.
  if grep -qE 'runs on Linux only|no cross-compiling' <<<"$out"; then
    fail "package-linux.sh $* asked about the platform before its arguments: $out"
  fi
}
refusal 'usage'
refusal 'invalid version' 0.1 x86_64
refusal 'invalid version' 1.2.3/4 x86_64
refusal 'invalid version' --print-versions 0.1
refusal 'invalid arch' 0.1.0 armv7
refusal 'invalid variant' 0.1.0 x86_64 both
echo 'ok: package-linux.sh refuses bad arguments with one line each, before any platform check'

# 7b. The variant refusal is exit 2, and the message must name the variant as the argument that was
# refused rather than the version or the arch: the exit code is the gate the release workflow relies on,
# and the line is what a person reads when a matrix cell names a spelling the script does not accept.
status=0; out="$(bash "$package_sh" 0.1.0 x86_64 both 2>&1)" || status=$?
[[ $status -eq 2 ]] || fail "package-linux.sh 0.1.0 x86_64 both exited $status, expected 2: $out"
grep -qF 'invalid variant' <<<"$out" || fail "the variant refusal did not name the variant: $out"
grep -qF 'expected full or headless' <<<"$out" || fail "the variant refusal did not name the two it accepts: $out"
# A valid variant passes the argument checks: the run gets as far as the platform question, which is the
# next line, so on a non-Linux host the refusal that shows up is the platform's and says nothing about
# the variant. On Linux the run goes further and asks for the arch to match, which is also nothing about
# the variant.
status=0; out="$(bash "$package_sh" 0.1.0 x86_64 full 2>&1)" || status=$?
if grep -qF 'invalid variant' <<<"$out"; then
  fail "a valid variant was refused as one: $out"
fi
echo 'ok: package-linux.sh refuses an unknown variant with exit 2 and accepts a valid one'

# 7c. The package metadata is what makes the two builds replace each other on install, and it is the one
# thing that is not exercised by the argument refusals above, so it is checked offline here through the
# same hooks dev/package-linux.sh runs against the real packages: --check-deb-control and
# --check-rpm-metadata. The hooks call the real check functions with fixture text from files, so a change
# to the rule in dev/package-linux.sh is a change tested here.
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$tmp" "$fixture_dir"' EXIT

# A full .deb: named featherbittorrent, conflicts with and replaces featherbittorrent-headless.
cat > "$fixture_dir/full-deb-control" <<'FIXTURE'
Package: featherbittorrent
Version: 0.2.2-1
Architecture: amd64
Conflicts: featherbittorrent-headless
Replaces: featherbittorrent-headless
FIXTURE
bash "$package_sh" --check-deb-control full "$fixture_dir/full-deb-control" >/dev/null \
  || fail 'the full .deb fixture was refused by --check-deb-control'
# The headless .deb: named featherbittorrent-headless, the conflicts the other way around.
cat > "$fixture_dir/headless-deb-control" <<'FIXTURE'
Package: featherbittorrent-headless
Version: 0.2.2-1
Architecture: amd64
Conflicts: featherbittorrent
Replaces: featherbittorrent
FIXTURE
bash "$package_sh" --check-deb-control headless "$fixture_dir/headless-deb-control" >/dev/null \
  || fail 'the headless .deb fixture was refused by --check-deb-control'
# A .deb with no Replaces line is refused: exit 2, message names Replaces.
cat > "$fixture_dir/noreplaces-deb-control" <<'FIXTURE'
Package: featherbittorrent
Conflicts: featherbittorrent-headless
FIXTURE
status=0; out="$(bash "$package_sh" --check-deb-control full "$fixture_dir/noreplaces-deb-control" 2>&1)" || status=$?
[[ $status -eq 2 ]] || fail "--check-deb-control (no Replaces) exited $status, expected 2: $out"
grep -qF 'Replaces' <<<"$out" || fail "the no-Replaces refusal did not name the field: $out"
# A .deb whose Conflicts names the wrong package is refused: exit 2, message names Conflicts.
cat > "$fixture_dir/wrongconflicts-deb-control" <<'FIXTURE'
Package: featherbittorrent
Conflicts: something-else
Replaces: featherbittorrent-headless
FIXTURE
status=0; out="$(bash "$package_sh" --check-deb-control full "$fixture_dir/wrongconflicts-deb-control" 2>&1)" || status=$?
[[ $status -eq 2 ]] || fail "--check-deb-control (wrong Conflicts) exited $status, expected 2: $out"
grep -qF 'Conflicts' <<<"$out" || fail "the wrong-Conflicts refusal did not name the field: $out"

# RPM metadata fixtures.
# A full .rpm: named featherbittorrent, obsoletes featherbittorrent-headless, no conflicts.
printf 'featherbittorrent-headless\n' > "$fixture_dir/full-rpm-obsoletes"
printf '' > "$fixture_dir/full-rpm-conflicts"
bash "$package_sh" --check-rpm-metadata full featherbittorrent "$fixture_dir/full-rpm-obsoletes" "$fixture_dir/full-rpm-conflicts" >/dev/null \
  || fail 'the full .rpm fixture was refused by --check-rpm-metadata'
# The headless .rpm: named featherbittorrent-headless, the obsoletes the other way around, no conflicts.
printf 'featherbittorrent\n' > "$fixture_dir/headless-rpm-obsoletes"
printf '' > "$fixture_dir/headless-rpm-conflicts"
bash "$package_sh" --check-rpm-metadata headless featherbittorrent-headless "$fixture_dir/headless-rpm-obsoletes" "$fixture_dir/headless-rpm-conflicts" >/dev/null \
  || fail 'the headless .rpm fixture was refused by --check-rpm-metadata'
# An .rpm that carries a conflict is refused: exit 2, message names Conflicts.
printf 'featherbittorrent-headless\n' > "$fixture_dir/conflict-rpm-obsoletes"
printf 'featherbittorrent-headless\n' > "$fixture_dir/conflict-rpm-conflicts"
status=0; out="$(bash "$package_sh" --check-rpm-metadata full featherbittorrent "$fixture_dir/conflict-rpm-obsoletes" "$fixture_dir/conflict-rpm-conflicts" 2>&1)" || status=$?
[[ $status -eq 2 ]] || fail "--check-rpm-metadata (conflict) exited $status, expected 2: $out"
grep -qF 'Conflicts' <<<"$out" || fail "the conflict refusal did not name the field: $out"
# An .rpm that does not obsolete the other package is refused: exit 2, message names obsoletes.
printf '' > "$fixture_dir/empty-rpm-obsoletes"
status=0; out="$(bash "$package_sh" --check-rpm-metadata full featherbittorrent "$fixture_dir/empty-rpm-obsoletes" "$fixture_dir/full-rpm-conflicts" 2>&1)" || status=$?
[[ $status -eq 2 ]] || fail "--check-rpm-metadata (no obsoletes) exited $status, expected 2: $out"
grep -qiF 'obsoletes' <<<"$out" || fail "the no-obsoletes refusal did not name the field: $out"
echo 'ok: the .deb and .rpm metadata checks accept the right packages and refuse the wrong ones'

# 8. The service examples the packages ship as documentation: no package installs, enables or starts a
# service, so each example is a file a person copies where their own service manager looks.
status=0; examples_out="$(bash dev/test-service-examples.sh 2>&1)" || status=$?
[[ $status -eq 0 ]] || fail "dev/test-service-examples.sh failed (exit $status): $(tail -3 <<<"$examples_out")"
echo 'ok: dev/test-service-examples.sh passes'

echo 'packaging: all checks passed'
