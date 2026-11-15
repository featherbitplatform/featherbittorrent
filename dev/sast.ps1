<#
.SYNOPSIS
    Local security scans: the same scanners and thresholds as
    .github/workflows/security.yml, run via Docker (plus native cargo-deny).

.DESCRIPTION
    Windows twin of dev/sast.sh; keep the two in sync. Every tool reads its
    config from the repo root (deny.toml, .semgrepignore, .gitleaks.toml,
    .grype.yaml), so local runs and CI can't drift.
    High/critical findings fail; lower severities are reported.

.EXAMPLE
    ./dev/sast.ps1                 # every scan
    ./dev/sast.ps1 semgrep         # a single tool
    ./dev/sast.ps1 semgrep deny    # any subset

    Targets: semgrep deny grype gitleaks all
#>
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Targets = @('all')
)

$ErrorActionPreference = 'Continue'
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

# Tool images, the same as the gateway's dev/sast.ps1. Bump deliberately.
$SemgrepImage  = 'semgrep/semgrep:latest'
$GrypeImage    = 'anchore/grype:latest'
$GitleaksImage = 'zricethezav/gitleaks:latest'

# Semgrep registry rulesets: keep in sync with the semgrep job in security.yml.
$SemgrepRulesets = @('p/default', 'p/rust')

if ($Targets -contains 'all') {
    $Targets = @('semgrep', 'deny', 'grype', 'gitleaks')
}

$Results = [ordered]@{}

function Invoke-Scan {
    param([string]$Name, [scriptblock]$Body)
    Write-Host ""
    Write-Host "=== $Name ===" -ForegroundColor Cyan
    & $Body
    $Results[$Name] = ($LASTEXITCODE -eq 0)
}

foreach ($target in $Targets) {
    switch ($target) {

        'semgrep' {
            Invoke-Scan 'semgrep' {
                $configs = $SemgrepRulesets | ForEach-Object { @('--config', $_) }
                docker run --rm -v "${RepoRoot}:/src:ro" -w /src $SemgrepImage `
                    semgrep scan @configs --severity ERROR --error --metrics=off
            }
        }

        'deny' {
            Invoke-Scan 'cargo-deny' {
                if (-not (Get-Command cargo-deny -ErrorAction SilentlyContinue)) {
                    Write-Host 'cargo-deny is not installed; run: cargo install cargo-deny --locked' -ForegroundColor Yellow
                    cmd /c 'exit 1'
                    return
                }
                cargo deny --manifest-path "$RepoRoot/Cargo.toml" --all-features check
            }
        }

        'grype' {
            Invoke-Scan 'grype (filesystem)' {
                # Named volume caches the vulnerability DB between runs.
                docker run --rm -v "${RepoRoot}:/src:ro" `
                    -v featherbittorrent-grype-db:/db -e GRYPE_DB_CACHE_DIR=/db `
                    $GrypeImage dir:/src -c /src/.grype.yaml
            }
        }

        'gitleaks' {
            # Full history, as CI scans it. The git directory is mounted on its
            # own so this also works from a linked worktree (.git is a file).
            $gitDir = (git -C $RepoRoot rev-parse --path-format=absolute --git-common-dir)
            Invoke-Scan 'gitleaks (history)' {
                docker run --rm -v "${RepoRoot}:/src:ro" -v "${gitDir}:/repo:ro" $GitleaksImage `
                    git /repo --config /src/.gitleaks.toml --no-banner --redact
            }.GetNewClosure()
            # The working tree too, so files not committed yet are covered.
            Invoke-Scan 'gitleaks (working tree)' {
                docker run --rm -v "${RepoRoot}:/src:ro" $GitleaksImage `
                    dir /src --config /src/.gitleaks.toml --no-banner --redact
            }
        }

        default {
            Write-Host "Unknown target '$target'. Targets: semgrep deny grype gitleaks all" -ForegroundColor Red
            exit 2
        }
    }
}

Write-Host ""
Write-Host '=== Summary ===' -ForegroundColor Cyan
$failed = $false
foreach ($entry in $Results.GetEnumerator()) {
    if ($entry.Value) {
        Write-Host ("  PASS  {0}" -f $entry.Key) -ForegroundColor Green
    } else {
        Write-Host ("  FAIL  {0}" -f $entry.Key) -ForegroundColor Red
        $failed = $true
    }
}
if ($failed) { exit 1 }
