#!/usr/bin/env bash
# Tests the linux packaging decisions without building anything, on any OS, the way the release
# workflow's verify job runs it: dev/check-linux-binary.sh accepts excerpts of a real almalinux:9
# release build (the -ok fixtures) and refuses a glibc symbol newer than its 2.34 floor or a NEEDED
# library off the allow list, each refusal one line naming its culprit; dev/package-linux.sh refuses
# missing arguments, a version that is not X.Y.Z(-pre), and an arch other than x86_64 or aarch64, all
# before it asks anything about the platform, which is why these tests run on macOS too; and
# dev/test-service-examples.sh passes, because the packages ship those examples as documentation. The
# fixtures are excerpts of objdump -T and objdump -p output for the binary dev/package-linux.sh builds
# in almalinux:9 (the failing copies are hand-edited from them), so they name no machine's paths.
# Nothing here contacts the network, builds anything, or installs anything.
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

# 4. dev/package-linux.sh validates its arguments before any platform or tool question. None of these
# refusals may mention Linux, because here they run on macOS and still have to mean the same thing.
refusal() { # <what the message must say> <package-linux.sh argument>...
  local want="$1"; shift
  local status=0 out
  out="$(bash "$package_sh" "$@" 2>&1)" || status=$?
  [[ $status -eq 1 ]] || fail "package-linux.sh $* exited $status, expected 1: $out"
  grep -qF -- "$want" <<<"$out" || fail "package-linux.sh $* did not say '$want': $out"
  # The two refusal lines that live after the argument checks. Seeing either would mean the script
  # asked about the platform before its arguments, which is what dev/test-packaging.sh tests against.
  if grep -qE 'runs on Linux only|no cross-compiling' <<<"$out"; then
    fail "package-linux.sh $* asked about the platform before its arguments: $out"
  fi
}
refusal usage
refusal 'invalid version' 0.1 x86_64
refusal 'invalid version' 1.2.3/4 x86_64
refusal 'invalid arch' 0.1.0 armv7
echo 'ok: package-linux.sh refuses bad arguments with one line each, before any platform check'

# 5. The service examples the packages ship as documentation (Review Focus 1 starts there).
status=0; examples_out="$(bash dev/test-service-examples.sh 2>&1)" || status=$?
[[ $status -eq 0 ]] || fail "dev/test-service-examples.sh failed (exit $status): $(tail -3 <<<"$examples_out")"
echo 'ok: dev/test-service-examples.sh passes'

echo 'packaging: all checks passed'
