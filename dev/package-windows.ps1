<#
.SYNOPSIS
    Builds featherbittorrent for this machine's Windows architecture and packages it as a .zip.

.DESCRIPTION
    Windows twin of dev/package-macos.sh and dev/package-linux.sh: the artifact for one architecture,
    built with cargo build --locked --release, staged, zipped, hashed, and then smoked from a fresh
    extraction rather than from the build folder. Nothing here signs anything (Windows runs what it is
    given; Authenticode is a later milestone), and nothing installs or registers anything: the task
    script in the archive is documentation, like every other example this product ships.

    The zip holds one folder named featherbittorrent-<version>-<target>, with the binary at its top beside
    LICENSE, NOTICE and README.md, the PowerShell completion the archive's own shell needs, the docs and
    examples\windows\register-task.ps1. Man pages are not in it: Windows has no man, and the pages ship in
    the macOS and Linux archives. <version>.sha256 beside the zip is in sha256sum's format (lowercase hex,
    two spaces, file name), so `shasum -a 256 -c`, `sha256sum -c` and Get-FileHash all read it.

    The last steps are the checks: the zip's own file list, register-task.ps1 parsed by PowerShell's parser
    (Review Focus 5: the example is valid, not just present), then dev/smoke-cli.ps1 on the extracted exe.

.EXAMPLE
    ./dev/package-windows.ps1 -Version 0.1.0 -Target x86_64-pc-windows-msvc

    Run it with PowerShell 7 (`pwsh`), the shell .github/workflows/release.yml uses. The build is native, so an
    ARM machine produces the aarch64 zip and an Intel machine the x86_64 one, and this refuses the other rather
    than naming an x86-64 binary after ARM. -Version is what bash dev/release-version.sh resolves; the smoke at
    the end compares it to what the packaged binary prints.

.EXAMPLE
    ./dev/package-windows.ps1 0.1.0 aarch64-pc-windows-msvc

    The same thing positionally, which is how .github/workflows/release.yml calls it.
#>
param(
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $Target
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# PowerShell 7.3 turned this on by default, and with Stop it makes a native command's non-zero exit a
# terminating error before this script can say what it expected. The exit codes are judged here, so the
# classic behaviour is wanted; on Windows PowerShell the line only defines an unused variable.
$PSNativeCommandUseErrorActionPreference = $false

function Write-Failure {
    # The one clear line a failure gets, then out: an artifact that failed its own checks must not be
    # uploaded, so nothing downstream may run after this.
    param([string] $Message)
    [Console]::Error.WriteLine("package-windows.ps1: $Message")
    exit 1
}

function Invoke-Native {
    # A native command with its output left on the console, which is where cargo's progress belongs, and a
    # non-zero exit code turned into our own failure line. stdout and stderr are not redirected: piping a
    # native command's output while ErrorActionPreference is Stop is how PowerShell turns ordinary progress
    # into a NativeCommandError.
    param([string] $File, [string[]] $Arguments, [string] $What)
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) { Write-Failure "$What failed with exit code $LASTEXITCODE" }
}

function Copy-StagedFile {
    # One file into the stage folder, with a failure that names what is missing. The folders come from
    # New-Item below, so this only has to care about the source.
    param([string] $Source, [string] $Destination)
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        Write-Failure "the archive ships $Source, and it is not in the repository"
    }
    Copy-Item -LiteralPath $Source -Destination $Destination -Force
}

function New-WorkDir {
    # A scratch directory of our own, named after a GUID so two runs on one machine never meet. The caller
    # removes it, whatever happens.
    $path = Join-Path ([System.IO.Path]::GetTempPath()) ("featherbittorrent-package-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -LiteralPath $path | Out-Null
    return $path
}

# Arguments first, before any platform or tool question, so the refusals can be tested anywhere. The two
# architectures are the two the release page ships; a zip is not universal, and this machine builds natively.
if ($Version -notmatch '^\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?$') {
    Write-Failure "invalid version '$Version' (expected X.Y.Z or X.Y.Z-pre)"
}
$ArchitectureOfTarget = @{
    'x86_64-pc-windows-msvc'  = 'AMD64'
    'aarch64-pc-windows-msvc' = 'ARM64'
}
if (-not $ArchitectureOfTarget.ContainsKey($Target)) {
    Write-Failure "-Target must be x86_64-pc-windows-msvc or aarch64-pc-windows-msvc (got '$Target')"
}

if (-not $IsWindows) { Write-Failure 'package-windows.ps1 runs on Windows only' }
$architecture = $env:PROCESSOR_ARCHITECTURE
if ($architecture -ne $ArchitectureOfTarget[$Target]) {
    Write-Failure "this machine is $architecture, not the architecture $Target targets (no cross-compiling)"
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { Write-Failure 'package-windows.ps1 needs cargo on PATH' }

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $root
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$dist = Join-Path $root 'target\dist'
$name = "featherbittorrent-$Version-$Target"
$zip = Join-Path $dist "${name}.zip"
$hashPath = "${zip}.sha256"
$exe = Join-Path $targetDir 'release\featherbittorrent.exe'
$stage = Join-Path $dist "stage-windows-$Target"
$top = Join-Path $stage $name

# The scratch directory every temporary file goes into, removed at the end whether the run passed or not.
$work = New-WorkDir
try {
    Invoke-Native cargo @('build', '--locked', '--release', '-p', 'featherbittorrent') 'cargo build'
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { Write-Failure "cargo did not write $exe" }

    # The man pages and completions, from the same clap Command as --help. They are generated by the release
    # binary itself, so they cannot drift from the flags; only the PowerShell completion is staged below.
    $assets = Join-Path $work 'assets'
    Invoke-Native $exe @('generate-assets', $assets) 'generate-assets'
    $psCompletion = Join-Path $assets 'completions\_featherbittorrent.ps1'
    if (-not (Test-Path -LiteralPath $psCompletion -PathType Leaf)) {
        Write-Failure "generate-assets wrote no PowerShell completion at $psCompletion"
    }

    # The stage folder whose name becomes the one folder inside the zip. Everything below it is what a
    # person unpacks, so the layout is checked from the finished archive rather than from here.
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    Remove-Item -LiteralPath $zip, $hashPath -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
    foreach ($dir in @($top, (Join-Path $top 'completions'), (Join-Path $top 'docs'), (Join-Path $top 'examples\windows'))) {
        New-Item -ItemType Directory -LiteralPath $dir | Out-Null
    }
    Copy-Item -LiteralPath $exe -Destination (Join-Path $top 'featherbittorrent.exe') -Force
    foreach ($file in @('LICENSE', 'NOTICE', 'README.md')) {
        Copy-StagedFile -Source (Join-Path $root $file) -Destination (Join-Path $top $file)
    }
    Copy-StagedFile -Source $psCompletion -Destination (Join-Path $top 'completions\_featherbittorrent.ps1')
    foreach ($doc in @('docs\autostart.md', 'docs\server.md')) {
        Copy-StagedFile -Source (Join-Path $root $doc) -Destination (Join-Path $top $doc)
    }
    # The install guide is written by the task that documents the packages; ship it when it is there.
    if (Test-Path -LiteralPath (Join-Path $root 'docs\install.md') -PathType Leaf) {
        Copy-Item -LiteralPath (Join-Path $root 'docs\install.md') `
            -Destination (Join-Path $top 'docs\install.md') -Force
    }
    Copy-StagedFile -Source (Join-Path $root 'packaging\windows\register-task.ps1') `
        -Destination (Join-Path $top 'examples\windows\register-task.ps1')

    # The folder itself is what gets compressed, so the zip's only top-level entry is $name: the same layout
    # the release page has always had, and the one docs/install.md unpacks. -Force overwrites a zip of an
    # earlier run.
    Compress-Archive -LiteralPath $top -DestinationPath $zip -Force

    # Same format as sha256sum: lower-case hash, two spaces, file name, LF; no BOM. The name in it is relative
    # to target\dist, which is where the check is run from.
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLowerInvariant()
    [System.IO.File]::WriteAllText($hashPath, "$hash  ${name}.zip`n", [System.Text.UTF8Encoding]::new($false))

    # What a person unpacks, read back out of the finished archive. Entry names are compared with / as the
    # separator because a zip written by Compress-Archive is not promised to use one or the other.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($zip)
    try {
        $entries = @($archive.Entries | ForEach-Object { $_.FullName -replace '\\', '/' })
    } finally {
        $archive.Dispose()
    }
    $want = @(
        "$name/featherbittorrent.exe", "$name/LICENSE", "$name/NOTICE", "$name/README.md",
        "$name/completions/_featherbittorrent.ps1", "$name/docs/autostart.md", "$name/docs/server.md",
        "$name/examples/windows/register-task.ps1"
    )
    if (Test-Path -LiteralPath (Join-Path $root 'docs\install.md') -PathType Leaf) {
        $want += "$name/docs/install.md"
    }
    foreach ($file in $want) {
        if ($entries -notcontains $file) { Write-Failure "the zip is missing $file (it lists $($entries.Count) entries)" }
    }
    Write-Host "ok: the zip lists every file it ships ($($entries.Count) entries)"

    # A fresh extraction, the same thing docs/install.md tells a person to do, and the binary the smoke runs.
    $extract = Join-Path $work 'archive'
    [System.IO.Compression.ZipFile]::ExtractToDirectory($zip, $extract)
    $taskScript = Join-Path $extract "$name\examples\windows\register-task.ps1"
    if (-not (Test-Path -LiteralPath $taskScript -PathType Leaf)) { Write-Failure "the zip extracted no $taskScript" }

    # Review Focus 5: the example parses. A quote wrong in the cmd.exe argument would only show up on the
    # machine that registers the task, which may be nobody's until a person follows docs/autostart.md.
    $parseTokens = $null
    $parseErrors = $null
    [System.Management.Automation.Language.Parser]::ParseFile($taskScript, [ref] $parseTokens, [ref] $parseErrors) | Out-Null
    if ($parseErrors.Count -gt 0) {
        $first = $parseErrors[0]
        Write-Failure "examples\windows\register-task.ps1 does not parse: line $($first.Extent.StartLineNumber): $($first.Message)"
    }
    Write-Host 'ok: register-task.ps1 parses'

    # The same smoke every other artifact passes through, on the binary this archive shipped. It is what
    # checks the version, so a mismatch here means the artifact and the release disagree.
    $scratch = Join-Path $work 'smoke-home'
    New-Item -ItemType Directory -LiteralPath $scratch | Out-Null
    & (Join-Path $PSScriptRoot 'smoke-cli.ps1') -Binary (Join-Path $extract "$name\featherbittorrent.exe") `
        -Version $Version -Scratch $scratch
    if ($LASTEXITCODE -ne 0) { Write-Failure "dev/smoke-cli.ps1 failed with exit code $LASTEXITCODE" }

    Remove-Item -LiteralPath $stage -Recurse -Force
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output $zip
Get-Content -LiteralPath $hashPath
