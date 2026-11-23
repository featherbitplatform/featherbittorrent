<#
.SYNOPSIS
    Builds the desktop app as a Windows .msi for x86_64.

.DESCRIPTION
    The installer comes from the Tauri bundler, `cargo tauri build --bundles msi --target
    x86_64-pc-windows-msvc`, with tauri-cli pinned at 2.12.6 (`cargo install --locked tauri-cli@2.12.6`);
    this script refuses any other version. That pin is also what pins WiX: tauri-cli 2.12.6's bundler
    (tauri-bundler 2.10.6) uses WiX Toolset 3.14.1, which it fetches from the wixtoolset/wix3 release
    wix3141rtm and refuses unless its SHA-256 is
    6ac824e1642d6f7277d0ed7ea09411a508f6116ba6fae0aa5f2c7daa2ff43d31, into its own tools folder. A runner's
    preinstalled WiX is not used, so an image update cannot change the toolset under a release; ../terminal
    pins the same WiX 3.14.1 for its .msi.

    The install is per machine, into Program Files, and registers the magnet URL protocol (from the
    deep-link plugin's schemes) and the .torrent association (bundle.fileAssociations) in
    crates/featherbittorrent-desktop/tauri.conf.json. It installs no service, no Run key and no scheduled
    task: start at login is the app's own setting, written by the app at runtime. Nothing is signed
    (Authenticode is a later milestone, as for the CLI zips).

    The UpgradeCode is fixed in tauri.conf.json (bundle.windows.wix.upgradeCode) and checked here against
    $UpgradeCode below. It must NEVER change: Windows Installer finds the installed copy to upgrade by it,
    and a new one would install a second copy beside the first instead of replacing it.

    The .msi lands in target\dist as featherbittorrent-desktop-<version>-windows-x86_64.msi with a .sha256
    in sha256sum's format beside it. dev/smoke-desktop-windows.ps1 installs and checks it; the release
    workflow runs that on a fresh runner.

    The front end must be built first (npm --prefix web ci; npm --prefix web run build): tauri.conf.json
    names web/dist as frontendDist and runs no build command of its own.

.EXAMPLE
    ./dev/package-desktop-windows.ps1 -Version 0.3.0

.DESCRIPTION
    An .msi carries a numeric version (major.minor.patch.build), and the bundler refuses a prerelease
    identifier that is not a plain number, so a release tag like v0.4.0-rc.1 would fail here and, through
    the publish job's needs, hold back every other artifact. A version with a prerelease is therefore given
    to the bundler as X.Y.Z.N through bundle.windows.wix.version (a --config file made for this build, so
    tauri.conf.json is never edited): N is the last numeric identifier of the prerelease (rc.1 is 1, beta
    is 0), and build metadata after + is dropped. A plain X.Y.Z passes through unchanged. Only the
    installer's internal version changes: the .msi's file name keeps the real version, and the installed
    exe still prints the workspace version. Windows Installer compares the first three fields only, and the
    bundler's MajorUpgrade allows same-version upgrades, so 0.4.0 still replaces 0.4.0-rc.1.

.EXAMPLE
    ./dev/package-desktop-windows.ps1 -Version 0.4.0-rc.1 -PrintWixVersion

    Prints 0.4.0.1, the version the .msi would carry, and exits: the mapping on its own, before any
    platform or tool question, which is what dev/test-packaging.sh checks.
#>
param(
    [string] $Version,
    [switch] $PrintWixVersion
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# The exit codes of native commands are judged here, not turned into terminating errors.
$PSNativeCommandUseErrorActionPreference = $false

# The one UpgradeCode this product's .msi ever has. Never change it (see the description above).
$UpgradeCode = '97828bd2-4932-4d8b-bbb3-84b05ea345e4'
$TauriCli = 'tauri-cli 2.12.6'
$Target = 'x86_64-pc-windows-msvc'

function Write-Refusal {
    # An argument refusal: exit 2, before any platform or tool question, so it is testable on any OS.
    param([string] $Message)
    [Console]::Error.WriteLine("package-desktop-windows.ps1: $Message")
    exit 2
}

function Write-Failure {
    param([string] $Message)
    [Console]::Error.WriteLine("package-desktop-windows.ps1: $Message")
    exit 1
}

if (-not $Version) { Write-Refusal 'usage: ./dev/package-desktop-windows.ps1 -Version <X.Y.Z>' }
if ($Version -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?\z') {
    Write-Refusal "invalid version '$Version' (expected X.Y.Z or X.Y.Z-pre)"
}

function ConvertTo-WixVersion {
    # X.Y.Z stays X.Y.Z; X.Y.Z-pre becomes X.Y.Z.N with N the last numeric identifier of pre, or 0.
    param([string] $SemVer)
    $core, $rest = ($SemVer -split '\+', 2)[0] -split '-', 2
    if (-not $rest) { return $core }
    $numbers = @($rest -split '\.' | Where-Object { $_ -cmatch '^[0-9]+\z' })
    $build = if ($numbers.Count -gt 0) { [long] $numbers[-1] } else { 0 }
    if ($build -gt 65535) { Write-Refusal "the prerelease number $build in '$SemVer' is over 65535, the most an .msi version field holds" }
    return "$core.$build"
}

$wixVersion = ConvertTo-WixVersion $Version
if ($PrintWixVersion) {
    Write-Output $wixVersion
    exit 0
}

if (-not $IsWindows) { Write-Failure 'package-desktop-windows.ps1 runs on Windows only' }

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$crate = Join-Path $root 'crates\featherbittorrent-desktop'

$conf = Get-Content -Raw (Join-Path $crate 'tauri.conf.json') | ConvertFrom-Json
$confCode = "$($conf.bundle.windows.wix.upgradeCode)"
if ($confCode -ne $UpgradeCode) {
    Write-Failure "tauri.conf.json's bundle.windows.wix.upgradeCode is '$confCode', expected $UpgradeCode; it must never change"
}

if (-not (Test-Path -LiteralPath (Join-Path $root 'web\dist\index.html'))) {
    Write-Failure 'web/dist is not built: npm --prefix web ci; npm --prefix web run build'
}

$found = & cargo tauri --version 2>$null
if ($LASTEXITCODE -ne 0) { Write-Failure "no tauri-cli: cargo install --locked tauri-cli@2.12.6" }
if ("$found".Trim() -ne $TauriCli) {
    Write-Failure "found '$found', the release bundler is $TauriCli`: cargo install --locked tauri-cli@2.12.6"
}

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$msiDir = Join-Path $targetDir "$Target\release\bundle\msi"
$dist = Join-Path $root 'target\dist'
$name = "featherbittorrent-desktop-$Version-windows-x86_64.msi"
$out = Join-Path $dist $name
$hashPath = "$out.sha256"

# A stale .msi from an earlier run must not be mistaken for this run's.
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $msiDir, $out, $hashPath
New-Item -ItemType Directory -Force $dist | Out-Null

# The .msi's numeric version, for this build only (see the description).
$wixConfig = Join-Path $dist 'desktop-wix-version.json'
$wixJson = @{ bundle = @{ windows = @{ wix = @{ version = $wixVersion } } } } | ConvertTo-Json -Depth 5 -Compress
[System.IO.File]::WriteAllText($wixConfig, $wixJson, [System.Text.UTF8Encoding]::new($false))
Write-Output "the .msi carries version $wixVersion"

Push-Location $crate
try {
    & cargo tauri build --bundles msi --target $Target --ci --config $wixConfig -- --locked
    if ($LASTEXITCODE -ne 0) { Write-Failure "cargo tauri build failed with exit code $LASTEXITCODE" }
} finally {
    Pop-Location
    Remove-Item -Force -ErrorAction SilentlyContinue $wixConfig
}

$msis = @(Get-ChildItem -LiteralPath $msiDir -Filter '*.msi' -File -ErrorAction SilentlyContinue)
if ($msis.Count -ne 1) { Write-Failure "expected one .msi under $msiDir, found $($msis.Count)" }
Copy-Item -LiteralPath $msis[0].FullName -Destination $out -Force

# Same format as sha256sum: lower-case hash, two spaces, file name, LF; no BOM.
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $out).Hash.ToLowerInvariant()
[System.IO.File]::WriteAllText($hashPath, "$hash  $name`n", [System.Text.UTF8Encoding]::new($false))

Write-Output $out
Get-Content -Raw $hashPath
