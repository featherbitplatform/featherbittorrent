#!/usr/bin/env bash
# Checks a linux binary against what the packages promise about it, so [package.metadata.deb] depends
# and cargo-generate-rpm's automatic requirements cannot silently become wrong: no glibc symbol newer
# than GLIBC_2.34 (the floor of every supported system: RHEL 9 and Ubuntu 22.04; Raspberry Pi OS 12/13
# and Fedora are newer), and NEEDED entries only from the allow list below, which is exactly what the
# declared dependencies cover. Reads objdump -T and objdump -p on the binary itself. Each refusal is
# one line on stderr naming the culprit, and exit 1.
# Inputs: <binary>, or files standing in for the two objdump outputs through CHECK_OBJDUMP_T and
# CHECK_OBJDUMP_P (dev/test-packaging.sh feeds fixtures that way, on machines with no objdump).
# Output: one ok line on success; a refusal line and exit 1 on failure.
# Usage: bash dev/check-linux-binary.sh <binary>
set -euo pipefail

fail() { echo "check-linux-binary.sh: $*" >&2; exit 1; }

[[ $# -eq 1 ]] || fail "usage: bash dev/check-linux-binary.sh <binary>"
bin="$1"

# The newest glibc whose symbols the binary may use and still run where the packages install it.
max_glibc=2.34

# Every shared library the binary is allowed to link at runtime, matching [package.metadata.deb]
# depends: libc, libm, libgcc_s and the loader only (ring and rustls are static, and the keyring's
# Linux store is pure-Rust D-Bus). The libpthread/libdl/librt entries are the pre-2.34 stubs a binary
# built on an older glibc would ask for; nothing newer than 2.34 puts them in DT_NEEDED.
allowed_libs=(libc.so.6 libm.so.6 libgcc_s.so.1 libpthread.so.0 libdl.so.2 librt.so.1 ld-linux-x86-64.so.2 ld-linux-aarch64.so.1)

# objdump reads the binary unless a fixture stands in for its output; that is how the tests run this
# script on macOS, where there is no objdump and the fixtures are excerpts of a real build's.
if [[ -n "${CHECK_OBJDUMP_T:-}" ]]; then
  [[ -f "$CHECK_OBJDUMP_T" ]] || fail "no objdump -T fixture at $CHECK_OBJDUMP_T"
  t_out="$(cat "$CHECK_OBJDUMP_T")"
else
  command -v objdump >/dev/null 2>&1 || fail "needs objdump, or CHECK_OBJDUMP_T naming its output (see dev/test-packaging.sh)"
  [[ -f "$bin" ]] || fail "no binary at $bin"
  if ! t_out="$(objdump -T "$bin" 2>&1)"; then
    fail "objdump -T could not read $bin: $(head -1 <<<"$t_out")"
  fi
fi
if [[ -n "${CHECK_OBJDUMP_P:-}" ]]; then
  [[ -f "$CHECK_OBJDUMP_P" ]] || fail "no objdump -p fixture at $CHECK_OBJDUMP_P"
  p_out="$(cat "$CHECK_OBJDUMP_P")"
else
  command -v objdump >/dev/null 2>&1 || fail "needs objdump, or CHECK_OBJDUMP_P naming its output (see dev/test-packaging.sh)"
  [[ -f "$bin" ]] || fail "no binary at $bin"
  if ! p_out="$(objdump -p "$bin" 2>&1)"; then
    fail "objdump -p could not read $bin: $(head -1 <<<"$p_out")"
  fi
fi

# Versioned symbols carry the glibc they came from (GLIBC_2.17, GLIBC_2.34, rarely a third number
# like GLIBC_2.1.3). sort -t. with numeric key fields orders dotted versions on any POSIX sort; the
# last line of that order is the newest version asked for. A static binary names none, and needs no
# glibc at all, so an empty answer passes this check.
newest="$(grep -oE 'GLIBC_[0-9]+(\.[0-9]+)*' <<<"$t_out" | sed 's/^GLIBC_//' \
  | sort -u -t. -k1,1n -k2,2n -k3,3n | tail -1)" || true
if [[ -n "$newest" && "$newest" != "$max_glibc" ]]; then
  top="$(printf '%s\n%s\n' "$newest" "$max_glibc" | sort -u -t. -k1,1n -k2,2n -k3,3n | tail -1)"
  [[ "$top" != "$newest" ]] || fail "the binary needs GLIBC_$newest, newer than the GLIBC_$max_glibc floor the packages declare; build it on almalinux:9 (glibc 2.34)"
fi

# DT_NEEDED is what a loader really loads, so this is the list the .deb depends must match line for
# line. objdump -p prints it as "NEEDED <name>" lines in the Dynamic Section.
libs="$(grep -E '^[[:space:]]*NEEDED[[:space:]]+[^[:space:]]+' <<<"$p_out" | awk '{print $2}' | sort -u)" || true
[[ -n "$libs" ]] || fail "the objdump -p input lists no NEEDED entry; is this a static binary or not an ELF one?"
while IFS= read -r lib; do
  [[ -n "$lib" ]] || continue
  allowed=0
  for want in "${allowed_libs[@]}"; do
    [[ "$lib" == "$want" ]] && allowed=1
  done
  [[ $allowed -eq 1 ]] || fail "the binary needs $lib, which is not on the allow list: add it here and to [package.metadata.deb] depends together, so the .deb still installs what the loader will want"
done <<<"$libs"

echo "check-linux-binary.sh ok: newest glibc symbol version GLIBC_${newest:-none}, NEEDED only from the allow list"
