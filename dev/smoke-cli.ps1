<#
.SYNOPSIS
    The release-safe CLI smoke: one binary, one scratch directory, no network, no server run.

.DESCRIPTION
    Windows twin of dev/smoke-cli.sh; keep the two in sync. Checks in order, each failure one clear line on
    stderr and exit 1: --version starts with `featherbittorrent <version>` (a build marker like ` (headless)`
    or ` (full)` may follow the version, so that check matches by prefix); --help names serve, user and
    device; serve --help shows none of the debug-only flags; serve --offline is refused with exit 2, which is
    what proves the artifact is a release build; then user set with a piped password and user list write and
    read back an argon2id login in a config file that holds the hash and not the password. Prints one ok line
    per check, then `cli smoke ok: <version>` and `config: <path>`, the config file this smoke created.

    Windows takes its folders from the known-folder APIs rather than from HOME or the XDG variables, so those
    would change nothing here; the two directories this product understands are set explicitly instead, to
    <Scratch>\config and <Scratch>\data. That is also what services and containers use (docs/autostart.md).
    Nothing of the operator's own library is read or written, and -Scratch is left in place for the caller to
    remove. Every command takes its stdin from an empty source - a pipeline that carries nothing, or the empty
    file serve --offline redirects - so the smoke can never wait on a keyboard it was not meant to have.

.EXAMPLE
    $scratch = New-Item -ItemType Directory -Force -Path ([IO.Path]::GetTempPath() + 'fbtt-smoke')
    ./dev/smoke-cli.ps1 -Binary target\release\featherbittorrent.exe -Version 0.1.0 -Scratch $scratch.FullName

    -Version is what bash dev/release-version.sh resolves (on Windows: the version in Cargo.toml). Give every
    run a fresh -Scratch directory: this refuses one whose config.yaml already exists.
#>
param(
    [Parameter(Mandatory)] [string] $Binary,
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $Scratch
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# PowerShell 7.3 turned this on by default, and with Stop it makes a native command's non-zero exit a
# terminating error before the check can say what it expected. This script judges exit codes itself, so the
# classic behaviour is wanted; on Windows PowerShell the line only defines an unused variable.
$PSNativeCommandUseErrorActionPreference = $false

function Write-Failure {
    # The one clear line a failure gets, then out. exit ends this script and the caller that ran it: nothing
    # downstream may publish an artifact that failed here.
    param([string] $Message)
    [Console]::Error.WriteLine("FAIL: $Message")
    exit 1
}

function Get-OneLineExcerpt {
    # The first 200 characters of captured output, on one line, for a failure message that has to stay one.
    param([string] $Text)
    $one = ($Text -replace '\r?\n', ' ')
    if ($one.Length -le 200) { return $one }
    return $one.Substring(0, 200)
}

# Arguments first, before anything is run, so the refusals can be tested anywhere.
if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) { Write-Failure "no binary at $Binary" }
$Binary = (Resolve-Path -LiteralPath $Binary).Path
if ($Version -notmatch '^\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?$') { Write-Failure "'$Version' is not a version" }
if (-not (Test-Path -LiteralPath $Scratch -PathType Container)) { Write-Failure "no scratch directory at $Scratch" }
$Scratch = (Resolve-Path -LiteralPath $Scratch).Path

$configDir = Join-Path $Scratch 'config'
$dataDir = Join-Path $Scratch 'data'
$configFile = Join-Path $configDir 'config.yaml'
$env:FBTT_CONFIG_DIR = $configDir
$env:FBTT_DATA_DIR = $dataDir
if (Test-Path -LiteralPath $configFile) { Write-Failure "$configFile already exists: give the smoke a fresh scratch directory" }

# 1. The version the artifact was built with, spelled the way the release page names it. A build marker
# may follow the version (` (headless)` or ` (full)`), so this check is a prefix one; the -Version argument
# is refused above unless it is plain version characters, none of which PowerShell's -like treats as a
# wildcard, so only the trailing `*` below patterns. -cnotlike keeps the comparison case-sensitive. stderr
# is left to the log: what is captured here is what the check compares. Checks 1 to 3 and user list take
# their stdin from a pipeline that carries nothing, which gives a native command an empty stdin and an
# immediate EOF (checked in PowerShell 7.4 against /bin/cat: no bytes read, prompt exit). serve --offline
# gets the same thing from the empty file it redirects, and user set is the one call given input of its
# own, the two password lines. Nothing here can wait on a terminal this smoke has no business reading.
$printed = [string]($null | & $Binary --version | Out-String)
if ($LASTEXITCODE -ne 0) { Write-Failure "--version exited $LASTEXITCODE (expected 0)" }
$printed = $printed.Trim()
if ($printed -cnotlike "featherbittorrent $Version*") {
    Write-Failure "--version printed '$(Get-OneLineExcerpt $printed)', expected output starting with 'featherbittorrent $Version'"
}
Write-Host "ok: --version starts with featherbittorrent $Version"

# 2. Help that names what the binary can do, with word boundaries: a help text about a "server" is not one
# that mentions the serve subcommand.
$helpText = [string]($null | & $Binary --help | Out-String)
if ($LASTEXITCODE -ne 0) { Write-Failure "--help exited $LASTEXITCODE (expected 0)" }
foreach ($word in @('serve', 'user', 'device')) {
    if ($helpText -notmatch "\b$word\b") { Write-Failure "--help does not mention $word" }
}
Write-Host 'ok: --help exits 0 and mentions serve, user and device'

# 3. The debug-only switches are hidden from help in every build, so seeing them here would mean the release
# build leaked flags a person never asked for (see ServeDebug in crates/featherbittorrent/src/serve.rs).
$serveHelpText = [string]($null | & $Binary serve --help | Out-String)
if ($LASTEXITCODE -ne 0) { Write-Failure "serve --help exited $LASTEXITCODE (expected 0)" }
if ($serveHelpText -match '\b(stop-file|offline)\b') {
    Write-Failure 'serve --help mentions stop-file or offline, which a release build does not have'
}
Write-Host 'ok: serve --help exits 0 and shows no debug-only flag'

# 4. The flag itself, not only its absence from help: clap refuses an argument the binary does not define, so
# exit 2 is the proof that this artifact was compiled without debug assertions. stdin comes from an empty
# file rather than the console, because a build that accepts the flag answers the wizard's questions and then
# serves; -Wait would sit there until it stops, which is why this script runs against release builds (the
# bash twin carries a 10 s timeout for exactly that reason, being run against debug builds too).
$emptyInput = Join-Path $Scratch 'stdin-empty.txt'
$stdoutFile = Join-Path $Scratch 'serve-offline-out.txt'
$stderrFile = Join-Path $Scratch 'serve-offline-err.txt'
foreach ($file in @($emptyInput, $stdoutFile, $stderrFile)) {
    if (Test-Path -LiteralPath $file) { Remove-Item -LiteralPath $file -Force }
}
New-Item -ItemType File -Path $emptyInput -Force | Out-Null
$offline = Start-Process -FilePath $Binary -ArgumentList @('serve', '--offline') `
    -RedirectStandardInput $emptyInput -RedirectStandardOutput $stdoutFile -RedirectStandardError $stderrFile `
    -NoNewWindow -Wait -PassThru
if ($offline.ExitCode -ne 2) {
    $served = [string](Get-Content -LiteralPath $stdoutFile -Raw) + ' ' +
              [string](Get-Content -LiteralPath $stderrFile -Raw)
    Write-Failure ("serve --offline exited $($offline.ExitCode), expected 2 (clap refusing a flag a release " +
        "build does not define); it printed: $(Get-OneLineExcerpt $served)")
}
Write-Host 'ok: serve --offline exits 2, so this is a release build'

# 5. A login, written and read back through the platform's own paths. The password arrives on stdin as two
# lines because user set reads it with rpassword only when stdin really is a terminal (see admin_command in
# crates/featherbittorrent/src/main.rs), which is also why nothing here may run attached to one.
$userSet = [string]((@('smoke-pass-1', 'smoke-pass-1') | & $Binary user set smoke) -join "`n")
if ($LASTEXITCODE -ne 0) { Write-Failure "user set smoke exited $LASTEXITCODE (expected 0): $(Get-OneLineExcerpt $userSet)" }
$users = [string]($null | & $Binary user list | Out-String)
if ($LASTEXITCODE -ne 0) { Write-Failure "user list exited $LASTEXITCODE (expected 0): $(Get-OneLineExcerpt $users)" }
if ($users -notmatch '(?m)^smoke  \$argon2id\$') {
    Write-Failure "user list printed no argon2id hash for smoke: $(Get-OneLineExcerpt $users)"
}
Write-Host 'ok: user set and user list round-trip a login through the library lock'

# The file itself: created where the platform says it goes, and holding the hash rather than what was typed.
if (-not (Test-Path -LiteralPath $configFile -PathType Leaf)) { Write-Failure "user set wrote no config at $configFile" }
$written = [string](Get-Content -LiteralPath $configFile -Raw)
if ($written.Contains('smoke-pass-1')) { Write-Failure "the config holds the password in the clear: $configFile" }
Write-Host "ok: $configFile exists and holds no password"

Write-Host "cli smoke ok: $Version"
Write-Host "config: $configFile"
