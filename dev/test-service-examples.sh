#!/usr/bin/env bash
# Checks the opt-in service examples under packaging/ and the guide that points at them: every file is
# there; the systemd system unit carries the least-privilege settings the guide promises (identity, paths,
# umask, sandbox, socket families) and runs as nobody but featherbittorrent; the user unit sets no User= and
# no FBTT_* directory, so a TUI on the same machine shares the library; the launchd plist is well formed when
# plutil exists; the PowerShell example declares the strict shell settings, registers without starting, wraps
# its cmd.exe command the way cmd wants it quoted, and parses and prints its own argument when pwsh exists;
# systemd-analyze verify reads the system unit with nothing to say when it and its prerequisites are present;
# docs/autostart.md has no em-dash and links every example that exists. Nothing here contacts the network,
# and nothing here asks a real service manager to start or enable anything.
# Usage: bash dev/test-service-examples.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root" || exit 1

system_unit=packaging/systemd/system/featherbittorrent.service
user_unit=packaging/systemd/user/featherbittorrent.service
agent_plist=packaging/launchd/io.featherbit.featherbittorrent.plist
task_ps1=packaging/windows/register-task.ps1
guide=docs/autostart.md

fail() { echo "FAIL: $*" >&2; exit 1; }
skip() { echo "skip: $*"; }

for file in "$system_unit" "$user_unit" "$agent_plist" "$task_ps1" packaging/README.md "$guide"; do
  [[ -f "$file" ]] || fail "missing $file"
done
echo "ok: the four examples, their README and the guide are all files"

# The system unit is what a Raspberry Pi or a home server copies, so the settings the guide tells
# someone to rely on have to actually be in the file.
for text in \
  'Type=exec' \
  'User=featherbittorrent' \
  'Group=featherbittorrent' \
  'Environment=FBTT_CONFIG_DIR=/var/lib/featherbittorrent/config' \
  'Environment=FBTT_DATA_DIR=/var/lib/featherbittorrent/library' \
  'ExecStart=/usr/bin/featherbittorrent serve' \
  'StandardInput=null' \
  'Restart=on-failure' \
  'RestartSec=10' \
  'TimeoutStopSec=90' \
  'StateDirectory=featherbittorrent' \
  'StateDirectoryMode=0700' \
  'NoNewPrivileges=yes' \
  'ProtectSystem=strict' \
  'ProtectHome=yes' \
  'PrivateTmp=yes' \
  'PrivateDevices=yes' \
  'UMask=0022' \
  'ProtectKernelTunables=true' \
  'ProtectControlGroups=true' \
  'RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX' \
  'ReadWritePaths=/var/lib/featherbittorrent' \
  '# RequiresMountsFor=/mnt/nas/torrents' \
  '# ReadWritePaths=/mnt/nas/torrents' \
  'WantedBy=multi-user.target'; do
  grep -qF -- "$text" "$system_unit" || fail "the system unit has no $text"
done
echo "ok: the system unit sets the paths, the sandbox and the stop grace the guide describes"

# One identity, and it is not root. A second User= line would mean the file disagrees with itself.
user_lines="$(grep -E '^User=' "$system_unit" || true)"
[[ "$user_lines" == 'User=featherbittorrent' ]] || fail "the system unit's User= lines are: $user_lines"
echo "ok: the system unit runs as featherbittorrent and never as root"

# A user service belongs to the person logged in: no identity of its own, and no library of its own
# either, because that is what lets the TUI on this machine find the running server and offer to connect.
for text in \
  'Type=exec' \
  'ExecStart=/usr/bin/featherbittorrent serve' \
  'StandardInput=null' \
  'Restart=on-failure' \
  'RestartSec=10' \
  'TimeoutStopSec=90' \
  'WantedBy=default.target'; do
  grep -qF -- "$text" "$user_unit" || fail "the user unit has no $text"
done
if grep -qE '^[[:space:]]*User=' "$user_unit"; then
  fail "the user unit sets User=: $(grep -nE '^[[:space:]]*User=' "$user_unit" | head -1)"
fi
if grep -qF 'FBTT_' "$user_unit"; then
  fail "the user unit names an FBTT_ directory: $(grep -nF 'FBTT_' "$user_unit" | head -1)"
fi
echo "ok: the user unit has no identity and no library of its own, so the TUI shares its library"

# The plist is XML that launchd reads, so the keys macOS acts on have to be spelled out.
for text in \
  '<string>io.featherbit.featherbittorrent</string>' \
  '<string>/usr/local/bin/featherbittorrent</string>' \
  '<string>serve</string>' \
  '<key>RunAtLoad</key>' \
  '<key>KeepAlive</key>' \
  '<key>SuccessfulExit</key>' \
  '<key>ExitTimeOut</key>' \
  '<key>StandardInPath</key>' \
  '<string>/dev/null</string>' \
  '<key>ProcessType</key>' \
  '<string>Background</string>'; do
  grep -qF -- "$text" "$agent_plist" || fail "the launchd agent has no $text"
done
echo "ok: the launchd agent names the binary, serve, KeepAlive and the 90 s stop grace"

# The PowerShell example must register a task and leave it for the next boot: a line that begins with
# Start-ScheduledTask would mean it starts the server during registration. The how to start it lines are
# indented inside the printed help text, so they do not count.
# SC2016: these patterns are matched literally against PowerShell source, so their $ is PowerShell's.
# shellcheck disable=SC2016
for text in \
  "\$ErrorActionPreference = 'Stop'" \
  'Set-StrictMode -Version Latest' \
  '-AtStartup' \
  '-LogonType Password' \
  'Read-Host -AsSecureString' \
  'Test-Path' \
  '/d /c ""{0}" serve 2>>"{1}""' \
  'Register-ScheduledTask -InputObject $task -TaskName $TaskName -User $user -Password $password -Force'; do
  grep -qF -- "$text" "$task_ps1" || fail "register-task.ps1 has no $text"
done
if grep -nE '^Start-ScheduledTask' "$task_ps1"; then
  fail 'register-task.ps1 calls Start-ScheduledTask at the top level, so it would start the server now'
fi
echo 'ok: register-task.ps1 is strict, stores a logon, wraps the cmd command in quotes, and registers without starting'

if command -v plutil >/dev/null 2>&1; then
  lint="$(plutil -lint "$agent_plist" 2>&1)" || fail "plutil -lint failed: $lint"
  grep -q 'OK' <<<"$lint" || fail "plutil -lint did not say OK: $lint"
  echo "ok: plutil -lint accepts $agent_plist"
else
  skip 'plutil is not installed, so the plist was grepped but not linted (macOS has it)'
fi

if command -v pwsh >/dev/null 2>&1; then
  # Parse only: no cmdlet runs here, so nothing is registered and no task scheduler is touched.
  # SC2016: the $ inside this single-quoted block belongs to PowerShell, not to bash.
  # shellcheck disable=SC2016
  parse="$(FBTT_PS1="$root/$task_ps1" pwsh -NoProfile -Command '
    $problems = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($env:FBTT_PS1, [ref] $null, [ref] $problems)
    if ($problems.Count -gt 0) {
      $problems | ForEach-Object { $_.ToString() }
      exit 1
    }' 2>&1)" || fail "pwsh could not parse $task_ps1: $parse"
  echo 'ok: pwsh parses register-task.ps1'

  # cmd.exe strips the first and last quote of a /c command line that holds more than two, so the whole
  # command has to arrive wrapped in one extra pair. A path with spaces is what shows whether it does: ask
  # the script for its own argument (PrintArgument runs before any filesystem or task scheduler work) and
  # compare byte for byte.
  spaced_binary='C:\Program Files\featherbittorrent\featherbittorrent.exe'
  spaced_log='C:\Users\you\AppData\Local\featherbit\featherbittorrent\serve.log'
  want_argument="/d /c \"\"${spaced_binary}\" serve 2>>\"${spaced_log}\"\""
  got_argument="$(pwsh -NoProfile -File "$root/$task_ps1" -Binary "$spaced_binary" \
    -LogFile "$spaced_log" -PrintArgument 2>&1)" || fail "pwsh could not print the task argument: $got_argument"
  [[ "$got_argument" == "$want_argument" ]] || fail "register-task.ps1 builds '$got_argument', expected '$want_argument'"
  echo 'ok: pwsh builds the doubled-quote cmd argument for a path with spaces'
else
  skip 'pwsh is not installed, so register-task.ps1 was grepped but neither parsed nor asked for its argument (it is a Windows example)'
fi

if command -v systemd-analyze >/dev/null 2>&1; then
  # verify resolves ExecStart's program and the account to run as, so it only means something on a host
  # with both. The linux smoke runs it in a Debian container against the installed binary (Task 5).
  if [[ -x /usr/bin/featherbittorrent ]] && id -u featherbittorrent >/dev/null 2>&1; then
    verify="$(systemd-analyze verify "$system_unit" 2>&1)" || fail "systemd-analyze verify failed: $verify"
    [[ -z "${verify//[[:space:]]/}" ]] || fail "systemd-analyze verify had something to say: $verify"
    echo 'ok: systemd-analyze verify reads the system unit with nothing to say'
  else
    skip 'systemd-analyze verify needs /usr/bin/featherbittorrent and a featherbittorrent account, and this host has neither'
  fi
else
  skip 'systemd-analyze verify is not installed (the linux smoke runs it inside a Debian container)'
fi

# Product copy: no em-dash in the guide. The bytes are spelled out so this file carries none either.
em_dash=$'\xe2\x80\x94'
if grep -qF -- "$em_dash" "$guide"; then
  fail "the guide has an em-dash: $(grep -nF -- "$em_dash" "$guide" | head -1)"
fi
echo 'ok: docs/autostart.md has no em-dash'

for file in "$system_unit" "$user_unit" "$agent_plist" "$task_ps1"; do
  [[ -e "$file" ]] || continue
  grep -qF -- "$file" "$guide" || fail "the guide does not link $file"
done
grep -qF 'docs/autostart.md' packaging/README.md || fail 'packaging/README.md does not point at the guide'
echo 'ok: the guide links every example, and the examples README points back to it'

echo 'service examples: all checks passed'
