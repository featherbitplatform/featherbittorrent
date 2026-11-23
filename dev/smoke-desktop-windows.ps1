<#
.SYNOPSIS
    Installs the desktop app's .msi, checks it, uninstalls it, and proves the user's settings survive.

.DESCRIPTION
    What the release workflow's desktop Windows smoke runs on a fresh runner, modelled on ../terminal's
    dev/smoke-windows.ps1: the .sha256 beside the .msi, a quiet per-machine install, then

    - the magnet URL protocol under HKLM\Software\Classes\magnet (the "URL Protocol" value and an open
      command that starts featherbittorrent-desktop.exe), which is also how this script finds the
      installed exe, so the install folder's name is the bundler's business;
    - `featherbittorrent-desktop --version` printing exactly `featherbittorrent-desktop <version>`. A
      release build is a GUI-subsystem program with no console, so its output is captured through a pipe
      (`& $exe --version | Out-String`), which also makes PowerShell wait for it to exit;
    - the .torrent association: HKCR\.torrent names a ProgId, and that ProgId has an open verb;
    - nothing that starts at boot or login: no service and no Run entry naming featherbittorrent;

    then a quiet uninstall, after which the exe, the protocol key and the association are gone and a
    settings file written where the app keeps it (%APPDATA%\featherbit\featherbittorrent-desktop\config\
    desktop.yaml, fbtt-desktop-core's DesktopPaths) is still there, unchanged.

    Run as administrator on a clean machine (a CI runner): it installs into Program Files and writes into
    the current user's real config folder, so it refuses to run unless $env:CI is set or that folder holds
    no desktop.yaml.

.EXAMPLE
    pwsh ./dev/smoke-desktop-windows.ps1 target/dist 0.3.0
#>
param(
    [string] $Dist,
    [string] $Version,
    [string] $Logs = (Join-Path ([System.IO.Path]::GetTempPath()) 'featherbittorrent-desktop-msi-logs')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

function Write-Refusal {
    param([string] $Message)
    [Console]::Error.WriteLine("smoke-desktop-windows.ps1: $Message")
    exit 2
}

# Arguments first, before any platform question, so the refusals are testable on any OS.
if (-not $Dist -or -not $Version) { Write-Refusal 'usage: pwsh dev/smoke-desktop-windows.ps1 <dist-dir> <version>' }
if ($Version -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?\z') {
    Write-Refusal "invalid version '$Version' (expected X.Y.Z or X.Y.Z-pre)"
}

if (-not $IsWindows) { throw 'smoke-desktop-windows.ps1 runs on Windows only' }

$name = "featherbittorrent-desktop-$Version-windows-x86_64.msi"
$msi = (Resolve-Path -LiteralPath (Join-Path $Dist $name)).Path

# The checksum file, in sha256sum's format.
$line = (Get-Content -Raw -LiteralPath (Join-Path $Dist "$name.sha256")).Trim()
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $msi).Hash.ToLowerInvariant()
if ($line -cne "$hash  $name") { throw "checksum file says '$line', the .msi hashes to $hash" }
Write-Output "ok: $name.sha256 matches"

$magnetKey = 'HKLM:\Software\Classes\magnet'
$torrentKey = 'Registry::HKEY_CLASSES_ROOT\.torrent'
# fbtt-desktop-core's DesktopPaths: ProjectDirs("io", "featherbit", "featherbittorrent-desktop").
$configDir = Join-Path $env:APPDATA 'featherbit\featherbittorrent-desktop\config'
$settings = Join-Path $configDir 'desktop.yaml'
$hadConfig = Test-Path -LiteralPath $settings
if ($hadConfig -and -not $env:CI) {
    throw "$settings exists; run this on a CI runner or a clean machine"
}
if (Test-Path $magnetKey) {
    throw "$magnetKey exists before the install: this machine (or runner image) already has a magnet: handler, so the check could not tell whether this .msi registered one. Run the smoke on a clean machine or a runner image without a magnet: handler."
}
New-Item -ItemType Directory -Force $Logs | Out-Null

# msiexec, quiet, with a verbose log; stops on any result but success.
function Invoke-Msiexec {
    param([string] $Action, [string] $Log)
    $logFile = Join-Path $Logs $Log
    $p = Start-Process -FilePath msiexec.exe -Wait -PassThru `
        -ArgumentList @($Action, "`"$msi`"", '/qn', '/norestart', '/l*v', "`"$logFile`"")
    if ($p.ExitCode -ne 0) {
        Get-Content $logFile -Tail 60
        throw "msiexec $Action failed with exit code $($p.ExitCode) (log: $logFile)"
    }
}

# Anything set to start at boot or login that names this product: a service, or a Run entry.
function Assert-NoAutostart {
    $services = @(Get-CimInstance Win32_Service | Where-Object { "$($_.Name) $($_.PathName)" -match 'featherbittorrent' })
    if ($services.Count -gt 0) { throw "a service names featherbittorrent: $($services[0].Name)" }
    foreach ($run in @('HKLM:\Software\Microsoft\Windows\CurrentVersion\Run',
                       'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run',
                       'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run')) {
        if (-not (Test-Path $run)) { continue }
        $props = (Get-ItemProperty $run).PSObject.Properties | Where-Object { $_.Name -notlike 'PS*' }
        foreach ($p in $props) {
            if ("$($p.Name) $($p.Value)" -match 'featherbittorrent') { throw "$run holds '$($p.Name)': $($p.Value)" }
        }
    }
}

# 1. Install.
Invoke-Msiexec '/i' 'install.log'
Write-Output "ok: msiexec /i $name /qn"

# 2. The magnet protocol, and the exe it opens.
if (-not (Test-Path $magnetKey)) { throw "no $magnetKey after the install" }
$protocol = Get-ItemProperty $magnetKey
if ($null -eq $protocol.PSObject.Properties['URL Protocol']) { throw "$magnetKey has no 'URL Protocol' value" }
$command = (Get-ItemProperty "$magnetKey\shell\open\command").'(default)'
if ($command -notmatch '^"([^"]+)"') {
    throw "the magnet open command is '$command', expected it to start with a quoted exe path"
}
$raw = $Matches[1]
if (-not (Test-Path -LiteralPath $raw)) { throw "the magnet open command is '$command', which names $raw, which is not there" }
# The installer may write the 8.3 short path (C:\PROGRA~1\FEATHE~1\FEATHE~1.EXE). Get-Item and
# GetFullPath do not expand short names, so ask Win32 GetLongPathName directly.
Add-Type -Namespace Fbt -Name Native -MemberDefinition @'
[DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern uint GetLongPathName(string shortPath, System.Text.StringBuilder longPath, uint bufferSize);
'@
$buf = New-Object System.Text.StringBuilder 4096
if ([Fbt.Native]::GetLongPathName($raw, $buf, $buf.Capacity) -eq 0) { throw "could not expand $raw to its long path" }
$exe = $buf.ToString()
if ((Split-Path -Leaf $exe) -ne 'featherbittorrent-desktop.exe') {
    throw "the magnet open command is '$command'; expanded to '$exe', expected it to be featherbittorrent-desktop.exe"
}
$installDir = Split-Path -Parent $exe
Write-Output "ok: magnet opens $exe"

# 3. --version, through a pipe: the release exe has no console of its own.
$got = (& $exe --version | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $got -cne "featherbittorrent-desktop $Version") {
    throw "$exe --version printed '$got' (exit $LASTEXITCODE), expected 'featherbittorrent-desktop $Version'"
}
Write-Output "ok: --version printed '$got'"

# 4. The .torrent association.
if (-not (Test-Path -LiteralPath $torrentKey)) { throw 'no .torrent association after the install' }
$progId = (Get-ItemProperty -LiteralPath $torrentKey).'(default)'
if (-not $progId) { throw '.torrent names no ProgId' }
$verbKey = "Registry::HKEY_CLASSES_ROOT\$progId\shell\open"
if (-not (Test-Path -LiteralPath $verbKey)) { throw ".torrent's ProgId $progId has no open verb" }
Write-Output "ok: .torrent opens with $progId"

# 5. Nothing starts at boot or login.
Assert-NoAutostart
Write-Output 'ok: no service and no Run entry names featherbittorrent'

try {
    # 6. The user's settings, as the app would keep them, then the uninstall.
    New-Item -ItemType Directory -Force $configDir | Out-Null
    Set-Content -LiteralPath $settings -Value "# smoke test`n" -NoNewline
    $settingsHash = (Get-FileHash -LiteralPath $settings).Hash

    Invoke-Msiexec '/x' 'uninstall.log'
    Write-Output "ok: msiexec /x $name /qn"
    foreach ($gone in @($exe, $installDir)) {
        if (Test-Path -LiteralPath $gone) { throw "left behind by the uninstall: $gone" }
    }
    if (Test-Path $magnetKey) { throw "left behind by the uninstall: $magnetKey" }
    if (Test-Path -LiteralPath "Registry::HKEY_CLASSES_ROOT\$progId") { throw "left behind by the uninstall: the ProgId $progId" }
    Write-Output 'ok: the uninstall removed the exe, the magnet protocol and the .torrent ProgId'

    if (-not (Test-Path -LiteralPath $settings)) { throw "the uninstall removed $settings" }
    if ((Get-FileHash -LiteralPath $settings).Hash -ne $settingsHash) { throw "the uninstall changed $settings" }
    Write-Output "ok: $settings survived the uninstall"
} finally {
    if (-not $hadConfig) { Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath $settings }
}

Write-Output "smoke ok: $name on $((Get-CimInstance Win32_OperatingSystem).Caption)"
