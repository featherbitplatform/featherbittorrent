#!/usr/bin/env bash
# The release-safe CLI smoke, shared by every packaging smoke (dev/smoke-linux.sh, dev/package-macos.sh):
# one binary, one scratch home, and nothing that reaches outside the machine. Checks in order, each failure a
# clear line on stderr and exit 1: --version is exactly `featherbittorrent <version> (full)` or
# `featherbittorrent <version> (headless)`; the last argument names which of the two the artifact is, and
# when it is named the check compares the binary's own marker against it, so a full artifact holding the
# headless binary (or the other way around) fails here; --help names serve,
# user and device; serve --help shows none of the debug-only flags; serve --offline is refused with exit 2,
# which is what proves the artifact is a release build; then user set with a piped password and user list
# write and read back an argon2id login in the platform's own config directory, which holds the hash and not
# the password. HOME is the scratch home, and XDG_CONFIG_HOME, XDG_DATA_HOME, FBTT_CONFIG_DIR and
# FBTT_DATA_DIR are unset, so what runs here is the platform's real path resolution and nothing of the
# operator's library is touched. Every command reads its stdin from /dev/null, so nothing here can wait for a
# keyboard it was never meant to have. Prints one ok line per check, then `cli smoke ok: <version>` and, on the
# last line, `config: <path>`: the config file this smoke created, which dev/smoke-linux.sh checks survives
# removing the package. Windows twin: dev/smoke-cli.ps1.
# Usage: bash dev/smoke-cli.sh <binary> <version> <home> <full|headless>
#   <binary>  the binary to smoke: target/release/featherbittorrent, /usr/bin/featherbittorrent, ...
#   <version> what it has to print, as bash dev/release-version.sh resolves it
#   <home>    an existing scratch HOME, written to here and removed by the caller. Never the real one.
#   <variant> the build the artifact was made from, full or headless. It is required: check 1 compares
#             the binary's version marker against it exactly, so a full artifact holding the headless
#             binary (or the other way around) fails here rather than at install time, and a caller that
#             forgets the variant is refused with a usage line rather than guessed at.
set -euo pipefail

# The usage refusal is exit 2, the same code a missing or invalid argument gets in the packaging
# scripts: a smoke run that did not get the arguments it needs is a caller bug, not a bad artifact.
fail() { echo "FAIL: $*" >&2; exit 2; }

# Captured output goes into failure messages, and it can carry newlines: this keeps a failure to the one line
# it is meant to be, and caps the length so a chatty binary cannot flood a CI log.
one_line() { local text="${1//$'\n'/ }"; printf '%s' "${text:0:200}"; }

# Arguments first, before any platform or tool question, so a test can exercise the refusals anywhere.
[[ $# -eq 4 ]] || fail "usage: bash dev/smoke-cli.sh <binary> <version> <home> <full|headless>"
bin="$1"
version="$2"
home="$3"
variant="$4"
case "$variant" in
  full|headless) ;;
  *) fail "invalid variant '$variant' (expected full or headless)" ;;
esac
[[ -n "$bin" && -n "$version" && -n "$home" ]] || fail "an argument is empty (binary '$bin', version '$version', home '$home')"
[[ -f "$bin" && -x "$bin" ]] || fail "no executable binary at $bin"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || fail "'$version' is not a version"
[[ -d "$home" ]] || fail "no scratch home directory at $home"

# HOME is read by the binary rather than by this script, so it has to be absolute: a relative scratch home
# would become whatever directory the smoke happened to start in.
home="$(cd "$home" && pwd)" || fail "cannot enter the scratch home $home"

# Where the platform puts config.yaml when nothing overrides it, which is the file check 5 reads back. Linux
# joins only the application name to $HOME/.config (directories 6.0.0's lin.rs ignores the qualifier and the
# organization); macOS builds the bundle id io.featherbit.featherbittorrent under Application Support, and
# that one folder is the config and the data directory there. Both fall back to HOME once XDG_* is unset.
case "$(uname -s)" in
  Linux)  config_file="$home/.config/featherbittorrent/config.yaml" ;;
  Darwin) config_file="$home/Library/Application Support/io.featherbit.featherbittorrent/config.yaml" ;;
  *)      fail "this smoke runs on Linux or macOS (Windows has dev/smoke-cli.ps1); $(uname -s) is neither" ;;
esac

# The environment every check runs in: the scratch home, and none of the four variables that would send the
# library somewhere else. An override left set would smoke the wrong path and pass anyway.
scratch=(env -u XDG_CONFIG_HOME -u XDG_DATA_HOME -u FBTT_CONFIG_DIR -u FBTT_DATA_DIR "HOME=$home")

if [[ -e "$config_file" ]]; then
  fail "$config_file already exists: give the smoke a fresh scratch home"
fi

# Checks 1 to 3 compare what the binary prints on stdout; its stderr is left to the log, where a failure's own
# words are easier to read than mixed into the text being compared. The stdin they get is /dev/null, so running
# this smoke by hand from a terminal cannot let a command reach for the keyboard.

# 1. The version the artifact was built with, spelled the way the release page names it. The binary's
# command name is featherbittorrent in both builds (it is the package's binary name, which the package
# manager installs as /usr/bin/featherbittorrent whatever the package is called), so what distinguishes
# a full from a headless artifact is the build marker: (full) or (headless). The whole string is compared,
# so a full package holding the headless binary (or the other way around) fails here rather than at
# install time.
status=0; printed="$( "${scratch[@]}" "$bin" --version </dev/null )" || status=$?
[[ $status -eq 0 ]] || fail "--version exited $status (expected 0): $(one_line "$printed")"
# The whole string is compared: the command name, the version and the build marker, so a full artifact
# holding the headless binary (or the other way around) fails here rather than at install time.
expected="featherbittorrent $version ($variant)"
if [[ "$printed" != "$expected" ]]; then
  fail "--version printed '$(one_line "$printed")', expected '$expected'"
fi
echo "ok: --version names the build: $(one_line "$printed")"

# 2. Help that names what the binary can do. Word boundaries, because a help text about a "server" is not one
# that mentions the serve subcommand.
status=0; help="$( "${scratch[@]}" "$bin" --help </dev/null )" || status=$?
[[ $status -eq 0 ]] || fail "--help exited $status (expected 0): $(one_line "$help")"
for word in serve user device; do
  grep -qwF -- "$word" <<<"$help" || fail "--help does not mention $word: $(one_line "$help")"
done
echo 'ok: --help exits 0 and mentions serve, user and device'

# 3. The debug-only switches are hidden from help in every build, so seeing them here would mean the release
# build leaked flags a person never asked for (see ServeDebug in crates/featherbittorrent/src/serve.rs).
status=0; serve_help="$( "${scratch[@]}" "$bin" serve --help </dev/null )" || status=$?
[[ $status -eq 0 ]] || fail "serve --help exited $status (expected 0): $(one_line "$serve_help")"
for word in stop-file offline; do
  if grep -qiwF -- "$word" <<<"$serve_help"; then
    fail "serve --help mentions $word, which a release build does not have: $(one_line "$serve_help")"
  fi
done
echo 'ok: serve --help exits 0 and shows no debug-only flag'

# 4. The flag itself, not only its absence from help: clap refuses an argument the binary does not define, so
# exit 2 is the proof that this artifact was compiled without debug assertions. A build that accepts the flag
# answers the wizard's questions and then serves, so the run is bounded at 10 s by whichever guard this host
# has: `timeout`, or `gtimeout` (the name Homebrew's coreutils installs under, because it will not overwrite a
# system one), or this script's own watchdog. All three answer 124 for a run that had to be cut short, which is
# what check 4 reports. The output goes to a file rather than into the command substitution: the guard ends the
# run mid-write, clap writes the refusal on stderr so both streams are wanted here, and a file keeps what was
# written without this script waiting on a writer that has gone away.
guard=''
if command -v timeout >/dev/null 2>&1; then
  guard='timeout'
elif command -v gtimeout >/dev/null 2>&1; then
  guard='gtimeout'
fi

served_file="$(mktemp)" || fail "could not make a temporary file for serve --offline's output"
status=0
if [[ -n "$guard" ]]; then
  "$guard" 10 "${scratch[@]}" "$bin" serve --offline </dev/null >"$served_file" 2>&1 || status=$?
else
  # The watchdog: the same budget, polled in one second steps so that a release build is answered as soon as it
  # has refused the flag and only a build that accepted it is cut short. kill -0 asks whether the process this
  # script started is still there; bash reaps a finished child by itself, so "gone" means finished and wait
  # then hands back the code check 4 wants. This one process is the watchdog's own to kill.
  "${scratch[@]}" "$bin" serve --offline </dev/null >"$served_file" 2>&1 &
  served_pid=$!
  waited=0
  while (( waited < 10 )); do
    if ! kill -0 "$served_pid" 2>/dev/null; then break; fi
    sleep 1
    waited=$((waited + 1))
  done
  if kill -0 "$served_pid" 2>/dev/null; then
    kill "$served_pid" 2>/dev/null || true
    wait "$served_pid" 2>/dev/null || true
    status=124
  else
    wait "$served_pid" || status=$?
  fi
fi
served="$(cat "$served_file")" || fail "could not read $served_file"
rm -f "$served_file"
if [[ $status -ne 2 ]]; then
  fail "serve --offline exited $status, expected 2 (clap refusing a flag a release build does not define); it printed: $(one_line "$served")"
fi
echo 'ok: serve --offline exits 2, so this is a release build'

# 5. A login, written and read back through the platform's own paths. The password goes in on stdin as two
# lines because user set reads it with rpassword only when stdin really is a terminal (see admin_command in
# crates/featherbittorrent/src/main.rs). This is the one command here that gets input of its own; every other
# one has /dev/null for stdin, so none of them can reach a terminal.
status=0; user_set="$(printf 'smoke-pass-1\nsmoke-pass-1\n' | "${scratch[@]}" "$bin" user set smoke 2>&1)" || status=$?
[[ $status -eq 0 ]] || fail "user set smoke exited $status (expected 0): $(one_line "$user_set")"
status=0; users="$( "${scratch[@]}" "$bin" user list </dev/null 2>&1 )" || status=$?
[[ $status -eq 0 ]] || fail "user list exited $status (expected 0): $(one_line "$users")"
# SC2016: the $ in that pattern is regex syntax (a literal dollar sign), not bash.
# shellcheck disable=SC2016
grep -qE '^smoke  \$argon2id\$' <<<"$users" || fail "user list printed no argon2id hash for smoke: $(one_line "$users")"
echo 'ok: user set and user list round-trip a login through the library lock'

# The file itself: created where the platform says it goes, and holding the hash rather than what was typed.
[[ -f "$config_file" ]] || fail "user set wrote no config at $config_file (user list said: $(one_line "$users"))"
if grep -qF 'smoke-pass-1' "$config_file"; then
  fail "the config holds the password in the clear: $(grep -nF -- 'smoke-pass-1' "$config_file" | head -1)"
fi
echo "ok: $config_file exists and holds no password"

# The last two lines are what the smokes above this one read: the version, and the config file. A package owns
# only the files it installed, so `apt-get purge` and `dnf remove` leave this one exactly where it is.
echo "cli smoke ok: $version"
echo "config: $config_file"
