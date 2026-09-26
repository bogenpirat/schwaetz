# Runs the same checks as CI: formatting, clippy (warnings are errors) and the test suite.
#
# Usage:  .\scripts\check.ps1 [-Fix]

param([switch]$Fix)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
. "$PSScriptRoot\dev-env.ps1"

function Step($name, [scriptblock]$cmd) {
    Write-Host "==> $name" -ForegroundColor Cyan
    & $cmd
    if ($LASTEXITCODE -ne 0) { throw "$name failed (exit $LASTEXITCODE)" }
}

if ($Fix) { Step 'cargo fmt' { cargo fmt --all } }
else { Step 'cargo fmt --check' { cargo fmt --all -- --check } }
Step 'cargo clippy' { cargo clippy --workspace --all-targets -- -D warnings }
Step 'cargo test' { cargo test --workspace }
Write-Host 'All checks passed.' -ForegroundColor Green
