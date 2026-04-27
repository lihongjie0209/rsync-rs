# tests/run_regression.ps1
#
# Windows-friendly entry point for the regression suite.  Builds rsync-rs
# with cargo and runs the Python harness directly on the host.  Scenarios
# requiring a Unix wrapper (C↔Rust SSH-style transport) are auto-skipped on
# Windows because /usr/local/bin/wrapper is unavailable.
#
# Examples:
#     .\tests\run_regression.ps1                   # local-mode subset
#     .\tests\run_regression.ps1 --smoke           # fast smoke
#     .\tests\run_regression.ps1 -k symlink        # filter
#     .\tests\run_regression.ps1 -- --verbose      # pass-through to harness
#
# When running on Windows a few axes of the matrix are dropped:
#   - SSH-wrapper modes (C↔Rust)        → skipped: no `wrapper` executable
#   - Permission/symlink-mode tests     → harness already gates on capability
#

[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$HarnessArgs
)

$ErrorActionPreference = 'Stop'
$Root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $Root

Write-Host "→ cargo build --release" -ForegroundColor Cyan
cargo build --release --quiet
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$BinDir = Join-Path $Root 'target\release'
$env:PATH = "$BinDir;$env:PATH"

# Default: skip client/server modes that require POSIX wrapper.
if (-not $env:RSYNC_WRAPPER) {
    $env:RSYNC_WRAPPER = "$BinDir\wrapper-not-installed"
}

python -m tests.regress @HarnessArgs
exit $LASTEXITCODE
